//! Playing a HAP clip, which is playing a file that is already textures.
//!
//! The simplest source in this renderer, and it is simple for a structural
//! reason rather than by luck. Every other clip needs a decoder, and a decoder
//! is a pipeline: packets go in, frames come out later, a seek costs a flush,
//! and a ring exists to hide the gap. HAP has none of that. Every packet is
//! one frame, every frame stands alone, and a seek is a seek.
//!
//! So there is no decoder here, no flush, no lead to keep, and no conversion.
//! FFmpeg demuxes the container and nothing else; what comes out of it goes to
//! the card as it is.
//!
//! # What it costs, against the same picture as a decoded clip
//!
//! For one 3840x2160 frame:
//!
//! | | processor | over the bus |
//! |---|---|---|
//! | H.265 through NVDEC | decode, then a copy back to host | 12.4 MB |
//! | Hap Alpha | one Snappy decompression | **8.3 MB** |
//!
//! And the Hap figure is what reaches the *card*; there is no host copy on the
//! way, because there is nothing to convert.

use std::path::{Path, PathBuf};

use ffmpeg_next as ffmpeg;
use ovs_types::geometry::SizePx;

use super::hap::{self, BlockFormat};
use super::source::{Frame, FrameSource};
use super::still::DecodedImage;
use super::timeline::SourceLength;
use super::MediaError;

/// How far ahead a HAP clip is read.
///
/// Four, the same as a clip on the processor, and for the opposite reason. A
/// decoder is read ahead of because catching up after a jump is expensive;
/// this is read ahead of because reading a file is, and four frames of a 4K
/// Hap Alpha clip is 33 MB of buffer. Deeper would buy nothing: there is no
/// pipeline here to keep full.
const FRAMES_AHEAD: u32 = 4;

/// A clip whose frames are already textures.
pub struct HapSource {
    path: PathBuf,
    input: ffmpeg::format::context::Input,
    stream_index: usize,
    time_base: ffmpeg::Rational,
    rate: ffmpeg::Rational,
    start_pts: i64,
    size: SizePx,
    length: SourceLength,
    /// The blocks of the frame being read, reused between frames.
    blocks: Vec<u8>,
    /// The frame handed over last, for the common case of being asked twice.
    last: Option<Frame>,
    /// Which frame the next packet will be, when that is known.
    next_index: Option<u32>,
}

impl HapSource {
    /// Opens a HAP clip.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError`] when the file cannot be read, holds no video,
    /// holds video that is not HAP, or holds HAP this build will not sample —
    /// `YCoCg_DXT5`, whose blocks are DXT5 but whose channels are not colour.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, MediaError> {
        let path = path.as_ref().to_path_buf();
        let input = ffmpeg::format::input(&path).map_err(|error| undecodable(&path, &error))?;

        let stream = input
            .streams()
            .best(ffmpeg::media::Type::Video)
            .ok_or_else(|| MediaError::Undecodable {
                path: path.clone(),
                reason: "there is no video in this file".to_owned(),
            })?;

        let parameters = stream.parameters();
        if parameters.id() != ffmpeg::codec::Id::HAP {
            return Err(MediaError::Undecodable {
                path: path.clone(),
                reason: "this is not a HAP clip".to_owned(),
            });
        }

        let stream_index = stream.index();
        let time_base = stream.time_base();
        let start_pts = match stream.start_time() {
            i64::MIN => 0,
            start => start,
        };
        let rate = super::video::rate_of(&stream);
        let frames = super::video::frame_count(&stream, rate);

        // Read off the stream rather than off a decoder, because there is no
        // decoder. A HAP stream carries its size in the container like any
        // other.
        let decoder_context = ffmpeg::codec::context::Context::from_parameters(parameters)
            .map_err(|error| undecodable(&path, &error))?;
        let video = decoder_context
            .decoder()
            .video()
            .map_err(|error| undecodable(&path, &error))?;
        let size = SizePx::new(video.width(), video.height());
        drop(video);

        let duration_ms = super::video::duration_ms(frames, rate);

        Ok(Self {
            path,
            input,
            stream_index,
            time_base,
            rate,
            start_pts,
            size,
            length: SourceLength {
                frame_count: frames.max(1),
                duration_ms,
            },
            blocks: Vec::new(),
            last: None,
            next_index: None,
        })
    }

    /// Which frame a presentation stamp is.
    fn index_of(&self, pts: i64) -> u32 {
        let elapsed = pts.saturating_sub(self.start_pts);
        let numerator = i128::from(elapsed)
            * i128::from(self.time_base.numerator())
            * i128::from(self.rate.numerator());
        let denominator =
            i128::from(self.time_base.denominator()) * i128::from(self.rate.denominator().max(1));
        if denominator == 0 {
            return 0;
        }
        let index = (numerator + denominator / 2) / denominator;
        u32::try_from(index.max(0)).unwrap_or(u32::MAX)
    }

    /// Moves the reader to the frame at or before `index`.
    ///
    /// No flush and no walking forward afterwards, because every HAP frame is
    /// a keyframe: the seek lands on the frame itself. That is the whole of
    /// why this path has no ring to refill and no cost to hide.
    fn seek(&mut self, index: u32) -> Result<(), MediaError> {
        let micros = i128::from(index) * 1_000_000 * i128::from(self.rate.denominator().max(1))
            / i128::from(self.rate.numerator().max(1));
        let offset =
            i128::from(self.start_pts) * i128::from(self.time_base.numerator()) * 1_000_000
                / i128::from(self.time_base.denominator().max(1));
        let target = i64::try_from(micros + offset).unwrap_or(i64::MAX);

        self.input
            .seek(target, ..target)
            .map_err(|error| undecodable(&self.path, &error))?;
        self.next_index = None;
        Ok(())
    }

    /// Reads the next packet of this clip's stream, as blocks.
    ///
    /// Returns the frame's index and what format its blocks are, or `None` at
    /// the end of the file.
    fn next_frame(&mut self) -> Result<Option<(u32, BlockFormat)>, MediaError> {
        loop {
            let Some((stream, packet)) = self.input.packets().next() else {
                return Ok(None);
            };
            if stream.index() != self.stream_index {
                continue;
            }
            let Some(data) = packet.data() else {
                continue;
            };

            let index = packet
                .pts()
                .or_else(|| packet.dts())
                .map_or_else(|| self.next_index.unwrap_or(0), |pts| self.index_of(pts));

            let format = hap::read_frame(data, self.size.width, self.size.height, &mut self.blocks)
                .map_err(|error| MediaError::Undecodable {
                    path: self.path.clone(),
                    reason: error.to_string(),
                })?;
            return Ok(Some((index, format)));
        }
    }
}

impl FrameSource for HapSource {
    fn length(&self) -> SourceLength {
        self.length
    }

    fn lead_frames(&self) -> u32 {
        FRAMES_AHEAD
    }

    fn frame(&mut self, index: u32) -> Result<Frame, MediaError> {
        let wanted = index.min(self.length.frame_count.saturating_sub(1));

        if let Some(last) = &self.last {
            if last.index == wanted {
                return Ok(last.clone());
            }
        }

        // A jump costs a seek and one packet, so there is no window here worth
        // walking forward through: anything that is not the very next frame is
        // cheaper to seek to.
        let sequential = self.next_index == Some(wanted);
        if !sequential {
            self.seek(wanted)?;
        }

        loop {
            let Some((index, format)) = self.next_frame()? else {
                return self.last.clone().ok_or_else(|| MediaError::Undecodable {
                    path: self.path.clone(),
                    reason: format!("the clip ended before frame {wanted}"),
                });
            };
            self.next_index = Some(index.saturating_add(1));

            if index >= wanted {
                let picture =
                    DecodedImage::compressed(std::mem::take(&mut self.blocks), self.size, format)
                        .ok_or_else(|| MediaError::Undecodable {
                        path: self.path.clone(),
                        reason: format!(
                            "frame {index} is not a {}x{} {format:?} texture",
                            self.size.width, self.size.height
                        ),
                    })?;

                let frame = Frame::new(index, picture);
                self.last = Some(frame.clone());
                return Ok(frame);
            }
        }
    }
}

/// A file that will not open, said the way the rest of this module says it.
fn undecodable(path: &Path, error: &impl std::fmt::Display) -> MediaError {
    MediaError::Undecodable {
        path: path.to_path_buf(),
        reason: error.to_string(),
    }
}
