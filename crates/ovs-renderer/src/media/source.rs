//! Where frames come from.
//!
//! Everything above this — the playhead, residency, the render loop — is
//! written against "a source that can be asked for a frame" and knows nothing
//! about codecs. That is the whole point of the trait: a ProRes
//! decoder, a HAP decoder and a still are three implementations of one idea, and
//! the scheduler above them is written, tested and measured once.
//!
//! A still is a source with one frame. Treating it as a special case would mean
//! two paths through the renderer for "what is on this layer", and the one that
//! runs less often is the one that breaks.
//!
//! # Frames are shared, not copied
//!
//! A 1080p frame is eight megabytes. It is decoded once, put behind an [`Arc`]
//! and handed around — the render thread holding the frame it drew while the
//! decoder works on the next one costs a pointer, not a copy.

use std::sync::Arc;

use ovs_types::geometry::SizePx;

use crate::media::still::{DecodedImage, MediaError};
use crate::media::timeline::SourceLength;

/// One frame, ready to become a texture.
#[derive(Debug, Clone)]
pub struct Frame {
    /// Which frame of the source this is.
    pub index: u32,
    /// The pixels.
    pub image: Arc<DecodedImage>,
}

impl Frame {
    /// Wraps a decoded image as the frame at an index.
    #[must_use]
    pub fn new(index: u32, image: DecodedImage) -> Self {
        Self {
            index,
            image: Arc::new(image),
        }
    }

    /// The frame's dimensions.
    #[must_use]
    pub fn size(&self) -> SizePx {
        self.image.size()
    }
}

/// Anything that can be asked for a frame.
///
/// `Send` because decoding happens on a worker thread: at 60 Hz a frame is 16.7
/// ms, and a decode on the render path is a dropped frame every time.
pub trait FrameSource: Send {
    /// How long the source is and how many frames it holds.
    fn length(&self) -> SourceLength;

    /// Returns one frame.
    ///
    /// Indices are clamped to the source rather than refused: a scheduler that
    /// asked for frame 100 of a 100-frame clip has made a rounding error at the
    /// end of a pass, and holding the last frame for one tick is the right
    /// answer to it.
    ///
    /// # Errors
    ///
    /// Returns [`MediaError`] when the frame cannot be produced. A source that
    /// fails once is assumed to keep failing: the layer drops to its
    /// placeholder and says why, exactly as a missing file already does.
    fn frame(&mut self, index: u32) -> Result<Frame, MediaError>;

    /// How many bits a sample of this source's material carries.
    ///
    /// Eight unless a source says otherwise, which is the honest answer for a
    /// still, for a block-compressed texture and for most clips. A ten-bit
    /// source overrides it — and the *layout* those ten bits are stored in stays
    /// inside the renderer, because an operator asking what is playing is asking
    /// how many bits and not which end of a `u16` they sit at.
    fn decode_depth(&self) -> ovs_types::depth::DecodeDepth {
        ovs_types::depth::DecodeDepth::Eight
    }

    /// How many frames a player should keep decoded ahead of this source.
    ///
    /// The ring is what a jump is paid out of, and the only jump a clip makes
    /// on its own is the one at its loop point — which is not an edge case, it
    /// is what a clip in a show does all evening. **A loop may not show a still
    /// frame**, so the ring has to be deep enough to cover the seek back to the
    /// start while it happens.
    ///
    /// The default suits a source whose frames all cost about the same. A
    /// source that is fast in a straight line and slow to jump asks for more:
    /// a decoder on the card produces a frame in two milliseconds and takes two
    /// hundred and twenty to seek, which is thirteen frames at sixty a second.
    ///
    /// Frames rather than milliseconds, because that is what the ring holds.
    /// Memory is the cost: one 1080p frame is about three megabytes.
    fn lead_frames(&self) -> u32 {
        crate::media::player::FRAMES_AHEAD
    }
}

/// A source with exactly one frame.
///
/// What a still is, once it has been decoded. Every still in the system goes
/// through the same scheduler as every clip, which is what stops the
/// single-frame path from quietly rotting.
#[derive(Debug)]
pub struct StillSource {
    frame: Frame,
}

impl StillSource {
    /// Wraps a decoded image as a one-frame source.
    #[must_use]
    pub fn new(image: DecodedImage) -> Self {
        Self {
            frame: Frame::new(0, image),
        }
    }
}

impl FrameSource for StillSource {
    fn length(&self) -> SourceLength {
        // No duration. A still has no timeline, so nothing above it has to
        // decide what "halfway through a photograph" means.
        SourceLength {
            duration_ms: 0,
            frame_count: 1,
        }
    }

    fn frame(&mut self, _index: u32) -> Result<Frame, MediaError> {
        Ok(self.frame.clone())
    }
}

/// How many frames a synthetic source takes to cycle its hue.
const SYNTHETIC_CYCLE: u32 = 60;

/// A source that makes its own frames.
///
/// Not a stand-in for a decoder: it is the implementation that lets the
/// scheduler, the residency and the render loop be tested and measured on a
/// machine with no codec at all, and it stays useful after real decoding lands.
///
/// Every frame carries its own index in its first pixel, so a test — or a person
/// looking at a wall — can say exactly which frame is being shown rather than
/// "something is moving".
#[derive(Debug)]
pub struct SyntheticSource {
    size: SizePx,
    length: SourceLength,
}

impl SyntheticSource {
    /// Builds a source of `frame_count` frames lasting `duration_ms`.
    #[must_use]
    pub fn new(size: SizePx, duration_ms: u64, frame_count: u32) -> Self {
        Self {
            size,
            length: SourceLength {
                duration_ms,
                frame_count: frame_count.max(1),
            },
        }
    }

    /// Returns the colour frame `index` is painted in.
    ///
    /// The first pixel is the index itself — low byte in red, high byte in
    /// green — so a frame can be identified exactly. The rest is a colour that
    /// moves, so that a person watching can see motion without counting.
    #[must_use]
    pub fn colour_of(index: u32) -> [u8; 4] {
        let phase = (index % SYNTHETIC_CYCLE) as f32 / SYNTHETIC_CYCLE as f32;
        let wheel = phase * 6.0;
        let step = wheel as u32 % 6;
        let rising = ((wheel - wheel.floor()) * 255.0) as u8;
        let falling = 255 - rising;
        match step {
            0 => [255, rising, 0, 255],
            1 => [falling, 255, 0, 255],
            2 => [0, 255, rising, 255],
            3 => [0, falling, 255, 255],
            4 => [rising, 0, 255, 255],
            _ => [255, 0, falling, 255],
        }
    }

    /// Returns the frame index stamped into a frame's first pixel.
    #[must_use]
    pub fn index_in(image: &DecodedImage) -> Option<u32> {
        let pixels = image.pixels();
        let low = *pixels.first()?;
        let high = *pixels.get(1)?;
        Some(u32::from(high) << 8 | u32::from(low))
    }
}

impl FrameSource for SyntheticSource {
    fn length(&self) -> SourceLength {
        self.length
    }

    fn frame(&mut self, index: u32) -> Result<Frame, MediaError> {
        let index = index.min(self.length.frame_count - 1);
        let colour = Self::colour_of(index);

        let pixel_count = self.size.width as usize * self.size.height as usize;
        let mut pixels = Vec::with_capacity(pixel_count * 4);
        for _ in 0..pixel_count {
            pixels.extend_from_slice(&colour);
        }

        // The index, in the first pixel. Written after the fill so it survives
        // whatever the colour was.
        if pixels.len() >= 2 {
            pixels[0] = (index & 0xFF) as u8;
            pixels[1] = ((index >> 8) & 0xFF) as u8;
        }

        let image = DecodedImage::new(pixels, self.size)
            .expect("a synthetic frame is exactly as large as it says");
        Ok(Frame::new(index, image))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size() -> SizePx {
        SizePx::new(4, 2)
    }

    #[test]
    fn a_still_is_a_source_with_one_frame() {
        // Stills and clips go through one scheduler. Two paths through "what is
        // on this layer" would mean the one that runs less often is the one
        // that breaks.
        let image = DecodedImage::new(vec![7; 4 * 2 * 4], size()).expect("an image");
        let mut source = StillSource::new(image);

        assert_eq!(source.length().frame_count, 1);
        assert_eq!(source.length().duration_ms, 0, "a still has no timeline");
        assert_eq!(source.frame(0).expect("a frame").index, 0);
    }

    #[test]
    fn a_still_answers_the_same_frame_whatever_is_asked_for() {
        // A layer holding a still still has a playhead running over it, and a
        // rounding error at the end of a pass must not put it off stage.
        let image = DecodedImage::new(vec![7; 4 * 2 * 4], size()).expect("an image");
        let mut source = StillSource::new(image);

        let first = source.frame(0).expect("a frame");
        let later = source.frame(500).expect("a frame");
        assert_eq!(first.image.pixels(), later.image.pixels());
    }

    #[test]
    fn a_synthetic_frame_says_which_frame_it_is() {
        // What makes the synthetic source worth having: a test can assert what
        // is on the wall, not merely that something is.
        let mut source = SyntheticSource::new(size(), 4_000, 100);

        for index in [0_u32, 1, 37, 99] {
            let frame = source.frame(index).expect("a frame");
            assert_eq!(frame.index, index);
            assert_eq!(SyntheticSource::index_in(&frame.image), Some(index));
        }
    }

    #[test]
    fn a_frame_past_the_end_is_the_last_frame() {
        // A scheduler asking for frame 100 of a hundred has made a rounding
        // error at the end of a pass. Holding the last frame for one tick is
        // the right answer; failing is not.
        let mut source = SyntheticSource::new(size(), 4_000, 100);
        let frame = source.frame(1_000).expect("a frame");
        assert_eq!(frame.index, 99);
    }

    #[test]
    fn frames_are_the_size_they_claim() {
        let mut source = SyntheticSource::new(SizePx::new(16, 9), 1_000, 10);
        let frame = source.frame(3).expect("a frame");
        assert_eq!(frame.size(), SizePx::new(16, 9));
        assert_eq!(frame.image.pixels().len(), 16 * 9 * 4);
    }

    #[test]
    fn successive_frames_look_different() {
        // A source whose frames were identical would make every test above it
        // pass whether or not anything advanced.
        let mut source = SyntheticSource::new(SizePx::new(8, 8), 1_000, 60);
        let first = source.frame(0).expect("a frame");
        let later = source.frame(20).expect("a frame");

        // Past the index stamp, which is only two bytes of the first pixel.
        assert_ne!(&first.image.pixels()[4..8], &later.image.pixels()[4..8]);
    }

    #[test]
    fn a_source_with_no_frames_still_has_one() {
        // Nothing above this can divide by a frame count of zero.
        let source = SyntheticSource::new(size(), 0, 0);
        assert_eq!(source.length().frame_count, 1);
    }

    #[test]
    fn a_frame_is_shared_rather_than_copied() {
        // Eight megabytes at 1080p. The render thread holding the frame it drew
        // while the decoder works on the next one has to cost a pointer.
        let mut source = SyntheticSource::new(SizePx::new(64, 64), 1_000, 10);
        let frame = source.frame(1).expect("a frame");
        let second = frame.clone();
        assert!(Arc::ptr_eq(&frame.image, &second.image));
    }
}
