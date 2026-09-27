//! What a media file actually is.
//!
//! The server has been taking a client's word for it. An upload declares its
//! codec, its size, its duration and its hash; the hash and the byte count are
//! checked, a still is opened and measured, and a video's claims are believed
//! because there was nothing on this side that could read one.
//!
//! That is a real hole. A show stores a media item, and what the operator
//! sees in the library — 1920x1080, twelve seconds, `ProRes` 422 — is whatever
//! was typed into a request. A wrong size is a clip that plays at the wrong
//! scale; a wrong duration is a cue that ends early; a file that is not `ProRes`
//! at all is a decoder failing in front of an audience rather than an upload
//! failing in an office.
//!
//! # What this does not do
//!
//! It does not transcode, and it does not judge whether a file is *good*.
//! [`probe`] opens the container, reads the first video stream's parameters and
//! closes it, which costs a seek and a header read — small enough to run inside
//! the request that registers an upload. [`thumbnail`] decodes exactly one
//! frame, which costs a seek and one picture.
//!
//! # Unix only, for the same reason as the decoder
//!
//! `FFmpeg` is a build dependency and the machine this runs on is Debian. A
//! Windows development machine builds the rest of the workspace without it.
//! Where there is no `FFmpeg`, [`probe`] says so rather than guessing: a caller
//! that cannot probe is told, and decides for itself whether to accept the
//! declaration (see [`ProbeError::Unavailable`]).

use std::path::{Path, PathBuf};

use ovs_types::geometry::{FrameRate, SizePx};

pub mod profile;

pub use profile::profile_of;

/// The largest side this will report.
///
/// The same ceiling the still decoder uses: 16384 is the smallest maximum image
/// dimension a conformant Vulkan implementation must support, so anything above
/// it could not become a texture on some device this might be deployed to.
/// Refusing it here means one clear message rather than a device-dependent
/// failure when a slot is triggered.
pub const MAX_MEDIA_SIDE: u32 = 16_384;

/// What one file turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    /// The codec's short name, as `FFmpeg` knows it: `prores`, `hap`, `png`.
    ///
    /// Compared against what was declared rather than mapped to a profile here:
    /// what counts as an acceptable profile is the core's rule, and this is
    /// only the reading.
    pub codec: String,
    /// The container's short name, for example `mov,mp4,m4a,3gp,3g2,mj2`.
    pub container: String,
    /// Pixel dimensions of the first video stream.
    pub size: SizePx,
    /// How long the file is, when the container says.
    ///
    /// `None` for a still, and for a container that carries no duration. It is
    /// not computed by decoding: a caller that needs an exact frame count for a
    /// file that does not declare one has to decode it, which is the decoder's
    /// job and not this.
    pub duration_ms: Option<u64>,
    /// The stream's average frame rate, when it declares one.
    pub rate: Option<FrameRate>,
    /// How many frames the container claims, when it claims any.
    pub frames: Option<u64>,
    /// Whether the pixel format carries an alpha channel.
    ///
    /// What decides whether a clip can be keyed over another, and the one thing
    /// an operator most often believes about a file that is not true of it.
    pub has_alpha: bool,
}

/// Why a file could not be read.
#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    /// This build has no `FFmpeg`, so nothing here can read a file.
    ///
    /// Distinct from every other variant on purpose: it says nothing about the
    /// file. A caller that gets this has learnt nothing and should say so
    /// rather than treating it as a bad file.
    #[error("this build cannot read media files")]
    Unavailable,
    /// The container could not be opened.
    #[error("{path} could not be opened: {reason}")]
    Unreadable {
        /// Which file.
        path: PathBuf,
        /// What the library said.
        reason: String,
    },
    /// There is no video stream in it.
    #[error("{path} has no video in it")]
    NoVideo {
        /// Which file.
        path: PathBuf,
    },
    /// The file is readable and this build has no decoder for what is in it.
    ///
    /// Separate from [`ProbeError::Unreadable`], which is what `FFmpeg` says when
    /// asked for a decoder it does not have: "Decoder not found", with nothing
    /// about the file. The server accepts whatever it can decode, so
    /// this is the one refusal an operator is likely to meet, and it has to say
    /// what the file holds — that is the sentence that tells them what to
    /// convert it from.
    #[error("{path} holds {codec}, and this build has no decoder for it")]
    NoDecoder {
        /// Which file.
        path: PathBuf,
        /// The codec's short name, as `FFmpeg` knows it.
        codec: String,
    },
    /// The video stream is a size nothing here could show.
    #[error("{path} is {width}x{height}, and the largest that can be shown is {MAX_MEDIA_SIDE}")]
    TooLarge {
        /// Which file.
        path: PathBuf,
        /// Its width.
        width: u32,
        /// Its height.
        height: u32,
    },
}

/// One frame of a clip, as tightly packed RGBA.
#[derive(Clone, PartialEq, Eq)]
pub struct Thumbnail {
    /// What size it came out at.
    pub size: SizePx,
    /// The pixels, row by row from the top.
    pub pixels: Vec<u8>,
}

impl std::fmt::Debug for Thumbnail {
    /// Prints the size, never the pixels.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Thumbnail")
            .field("size", &self.size)
            .field("bytes", &self.pixels.len())
            .finish()
    }
}

/// Takes one picture of a clip, no larger than `max_side` on either edge.
///
/// # Which frame
///
/// A little way in rather than the first one. The first frame of a show clip is
/// very often black — a fade from nothing is how most of them start — and a
/// library of black rectangles tells an operator nothing. A tenth of the way
/// through is past the fade and still unmistakably the same clip.
///
/// A file that does not say how long it is gets its first frame, because there
/// is nothing to take a tenth of.
///
/// # Errors
///
/// Returns [`ProbeError::Unavailable`] where this build has no `FFmpeg`, and
/// otherwise the reason the file could not be read.
#[cfg(unix)]
pub fn thumbnail(path: &Path, max_side: u32) -> Result<Thumbnail, ProbeError> {
    use ffmpeg_next as ffmpeg;

    let read = probe(path)?;

    ffmpeg::init().map_err(|error| ProbeError::Unreadable {
        path: path.to_path_buf(),
        reason: error.to_string(),
    })?;

    let mut input = ffmpeg::format::input(&path).map_err(|error| ProbeError::Unreadable {
        path: path.to_path_buf(),
        reason: error.to_string(),
    })?;

    let stream_index = input
        .streams()
        .best(ffmpeg::media::Type::Video)
        .map(|stream| stream.index())
        .ok_or_else(|| ProbeError::NoVideo {
            path: path.to_path_buf(),
        })?;

    let parameters = input
        .stream(stream_index)
        .ok_or_else(|| ProbeError::NoVideo {
            path: path.to_path_buf(),
        })?
        .parameters();

    let context =
        ffmpeg::codec::context::Context::from_parameters(parameters).map_err(|error| {
            ProbeError::Unreadable {
                path: path.to_path_buf(),
                reason: error.to_string(),
            }
        })?;

    let mut decoder = context
        .decoder()
        .video()
        .map_err(|error| ProbeError::Unreadable {
            path: path.to_path_buf(),
            reason: error.to_string(),
        })?;

    // A tenth of the way in, in FFmpeg's own time base. A seek that fails is not
    // a failure: what it costs is the first frame instead of a better one.
    if let Some(duration_ms) = read.duration_ms {
        let target = i64::try_from(duration_ms / 10).unwrap_or(0)
            * i64::from(ffmpeg::ffi::AV_TIME_BASE)
            / 1000;
        let _ = input.seek(target, ..target.saturating_add(1));
    }

    let size = fit(read.size, max_side);
    let mut scaler = ffmpeg::software::scaling::Context::get(
        decoder.format(),
        decoder.width(),
        decoder.height(),
        ffmpeg::format::Pixel::RGBA,
        size.width,
        size.height,
        ffmpeg::software::scaling::Flags::BILINEAR,
    )
    .map_err(|error| ProbeError::Unreadable {
        path: path.to_path_buf(),
        reason: error.to_string(),
    })?;

    let mut raw = ffmpeg::util::frame::Video::empty();
    for (stream, packet) in input.packets() {
        if stream.index() != stream_index {
            continue;
        }
        if decoder.send_packet(&packet).is_err() {
            continue;
        }
        if decoder.receive_frame(&mut raw).is_err() {
            continue;
        }

        let mut picture = ffmpeg::util::frame::Video::empty();
        scaler
            .run(&raw, &mut picture)
            .map_err(|error| ProbeError::Unreadable {
                path: path.to_path_buf(),
                reason: error.to_string(),
            })?;

        return Ok(Thumbnail {
            size,
            pixels: packed(&picture, size),
        });
    }

    Err(ProbeError::NoVideo {
        path: path.to_path_buf(),
    })
}

/// Takes one picture of a clip.
///
/// # Errors
///
/// Always [`ProbeError::Unavailable`] here: this build has no `FFmpeg`.
#[cfg(not(unix))]
pub fn thumbnail(path: &Path, max_side: u32) -> Result<Thumbnail, ProbeError> {
    let _ = (path, max_side);
    Err(ProbeError::Unavailable)
}

/// Takes several pictures spread across a clip, earliest first.
///
/// # Where they come from
///
/// The clip is divided into `count` parts and one frame is taken from the end
/// of each: a tenth of the way in, two tenths, and so on. The owner asked for
/// exactly that division (2026-09-23) — "bei einem 20s Video alle 2s ein Frame,
/// bei nur 10s jede sekunde" — and it has the property that matters here: the
/// **first** of them is the frame [`thumbnail`] already takes, so the still
/// picture of a clip and the first frame of its moving preview are the same
/// picture rather than two that nearly agree.
///
/// Past the opening fade is why it starts a tenth in rather than at zero. The
/// first frame of a show clip is very often black, and a library of black
/// rectangles tells an operator nothing.
///
/// # What comes back
///
/// As many as could be read, in order, and never more than `count`. A file too
/// short to seek about in comes back with fewer, which is honest: a preview of
/// three frames is a preview, and refusing to make one because the clip is
/// brief would leave the library with nothing for it.
///
/// # Errors
///
/// Returns [`ProbeError::Unavailable`] where this build has no `FFmpeg`,
/// [`ProbeError::NoVideo`] when nothing could be read at all, and otherwise the
/// reason the file could not be opened.
#[cfg(unix)]
pub fn frames(path: &Path, count: u32, max_side: u32) -> Result<Vec<Thumbnail>, ProbeError> {
    use ffmpeg_next as ffmpeg;

    let read = probe(path)?;
    let wanted = count.max(1);

    ffmpeg::init().map_err(|error| ProbeError::Unreadable {
        path: path.to_path_buf(),
        reason: error.to_string(),
    })?;

    let size = fit(read.size, max_side);
    let mut taken: Vec<Thumbnail> = Vec::with_capacity(wanted as usize);

    // A clip that does not say how long it is cannot be divided, so it gets one
    // picture and that is the preview: a strip of ten identical frames would be
    // a lie told ten times.
    let Some(duration_ms) = read.duration_ms else {
        return Ok(vec![thumbnail(path, max_side)?]);
    };

    for step in 1..=wanted {
        let at_ms = duration_ms.saturating_mul(u64::from(step)) / u64::from(wanted).max(1);
        match frame_at(path, at_ms, size) {
            Ok(picture) => taken.push(picture),
            // One frame that could not be read is not a reason to have no
            // preview. A clip whose last tenth is unseekable still has nine.
            Err(ProbeError::NoVideo { .. }) => {}
            Err(error) => return Err(error),
        }
    }

    if taken.is_empty() {
        return Err(ProbeError::NoVideo {
            path: path.to_path_buf(),
        });
    }
    Ok(taken)
}

/// Takes several pictures spread across a clip.
///
/// # Errors
///
/// Always [`ProbeError::Unavailable`] here: this build has no `FFmpeg`.
#[cfg(not(unix))]
pub fn frames(path: &Path, count: u32, max_side: u32) -> Result<Vec<Thumbnail>, ProbeError> {
    let _ = (path, count, max_side);
    Err(ProbeError::Unavailable)
}

/// One frame from a given point in a clip, scaled to `size`.
///
/// Opened afresh for each: seeking a decoder backwards and forwards through one
/// open context is where this kind of code usually goes wrong, and ten opens of
/// a file that is already in the page cache is not the expensive part of an
/// upload.
#[cfg(unix)]
fn frame_at(path: &Path, at_ms: u64, size: SizePx) -> Result<Thumbnail, ProbeError> {
    use ffmpeg_next as ffmpeg;

    let mut input = ffmpeg::format::input(&path).map_err(|error| ProbeError::Unreadable {
        path: path.to_path_buf(),
        reason: error.to_string(),
    })?;

    let stream_index = input
        .streams()
        .best(ffmpeg::media::Type::Video)
        .map(|stream| stream.index())
        .ok_or_else(|| ProbeError::NoVideo {
            path: path.to_path_buf(),
        })?;

    let parameters = input
        .stream(stream_index)
        .ok_or_else(|| ProbeError::NoVideo {
            path: path.to_path_buf(),
        })?
        .parameters();

    let context =
        ffmpeg::codec::context::Context::from_parameters(parameters).map_err(|error| {
            ProbeError::Unreadable {
                path: path.to_path_buf(),
                reason: error.to_string(),
            }
        })?;

    let mut decoder = context
        .decoder()
        .video()
        .map_err(|error| ProbeError::Unreadable {
            path: path.to_path_buf(),
            reason: error.to_string(),
        })?;

    // A seek that fails is not a failure: what it costs is an earlier frame
    // than the one asked for.
    let target = i64::try_from(at_ms).unwrap_or(0) * i64::from(ffmpeg::ffi::AV_TIME_BASE) / 1000;
    let _ = input.seek(target, ..target.saturating_add(1));

    let mut scaler = ffmpeg::software::scaling::Context::get(
        decoder.format(),
        decoder.width(),
        decoder.height(),
        ffmpeg::format::Pixel::RGBA,
        size.width,
        size.height,
        ffmpeg::software::scaling::Flags::BILINEAR,
    )
    .map_err(|error| ProbeError::Unreadable {
        path: path.to_path_buf(),
        reason: error.to_string(),
    })?;

    let mut raw = ffmpeg::util::frame::Video::empty();
    for (stream, packet) in input.packets() {
        if stream.index() != stream_index {
            continue;
        }
        if decoder.send_packet(&packet).is_err() {
            continue;
        }
        if decoder.receive_frame(&mut raw).is_err() {
            continue;
        }

        let mut picture = ffmpeg::util::frame::Video::empty();
        scaler
            .run(&raw, &mut picture)
            .map_err(|error| ProbeError::Unreadable {
                path: path.to_path_buf(),
                reason: error.to_string(),
            })?;

        return Ok(Thumbnail {
            size,
            pixels: packed(&picture, size),
        });
    }

    Err(ProbeError::NoVideo {
        path: path.to_path_buf(),
    })
}

/// The size a picture comes out at: the surface's own proportions, inside a
/// square of `max_side`, and never larger than the surface itself.
#[must_use]
pub fn fit(surface: SizePx, max_side: u32) -> SizePx {
    let side = u64::from(max_side.max(1));
    let width = u64::from(surface.width.max(1));
    let height = u64::from(surface.height.max(1));

    if width <= side && height <= side {
        return SizePx::new(surface.width.max(1), surface.height.max(1));
    }

    // Whichever edge runs out of room first decides the scale, in integers so
    // the answer cannot depend on how a float rounded.
    if width >= height {
        SizePx::new(max_side.max(1), ((height * side / width).max(1)) as u32)
    } else {
        SizePx::new(((width * side / height).max(1)) as u32, max_side.max(1))
    }
}

/// Copies a scaled frame out of its padded rows.
///
/// A decoded frame's rows are padded to whatever the library found convenient,
/// so the bytes are not contiguous and cannot be handed on as they are.
#[cfg(unix)]
fn packed(picture: &ffmpeg_next::util::frame::Video, size: SizePx) -> Vec<u8> {
    let width = size.width as usize * 4;
    let stride = picture.stride(0);
    let data = picture.data(0);

    let mut pixels = Vec::with_capacity(width * size.height as usize);
    for row in 0..size.height as usize {
        let start = row * stride;
        let end = start + width;
        if end <= data.len() {
            pixels.extend_from_slice(&data[start..end]);
        }
    }
    pixels
}

/// Reads what a file is.
///
/// # Errors
///
/// Returns [`ProbeError::Unavailable`] where this build has no `FFmpeg`, and
/// otherwise the reason the file could not be read.
#[cfg(unix)]
pub fn probe(path: &Path) -> Result<Probe, ProbeError> {
    use ffmpeg_next as ffmpeg;

    // Idempotent, and cheap after the first call. Done here rather than asked of
    // the caller because a probe that needed initialising first would be a rule
    // somebody eventually forgets.
    ffmpeg::init().map_err(|error| ProbeError::Unreadable {
        path: path.to_path_buf(),
        reason: error.to_string(),
    })?;

    let input = ffmpeg::format::input(&path).map_err(|error| ProbeError::Unreadable {
        path: path.to_path_buf(),
        reason: error.to_string(),
    })?;

    let stream = input
        .streams()
        .best(ffmpeg::media::Type::Video)
        .ok_or_else(|| ProbeError::NoVideo {
            path: path.to_path_buf(),
        })?;

    let parameters = stream.parameters();
    let context =
        ffmpeg::codec::context::Context::from_parameters(parameters).map_err(|error| {
            ProbeError::Unreadable {
                path: path.to_path_buf(),
                reason: error.to_string(),
            }
        })?;

    let codec_id = context.id();
    // Asked before the decoder is built, so that a codec this build was not
    // compiled with is reported as itself rather than as "Decoder not found".
    if ffmpeg::codec::decoder::find(codec_id).is_none() {
        return Err(ProbeError::NoDecoder {
            path: path.to_path_buf(),
            codec: format!("{codec_id:?}").to_lowercase(),
        });
    }
    let video = context
        .decoder()
        .video()
        .map_err(|error| ProbeError::Unreadable {
            path: path.to_path_buf(),
            reason: error.to_string(),
        })?;

    let width = video.width();
    let height = video.height();
    if width == 0 || height == 0 || width > MAX_MEDIA_SIDE || height > MAX_MEDIA_SIDE {
        return Err(ProbeError::TooLarge {
            path: path.to_path_buf(),
            width,
            height,
        });
    }

    Ok(Probe {
        codec: codec_name(codec_id),
        container: input.format().name().to_owned(),
        size: SizePx::new(width, height),
        duration_ms: duration_ms(&input, &stream),
        rate: rate_of(stream.avg_frame_rate()).or_else(|| rate_of(stream.rate())),
        frames: (stream.frames() > 0).then(|| stream.frames() as u64),
        has_alpha: has_alpha(video.format()),
    })
}

/// Reads what a file is.
///
/// # Errors
///
/// Always [`ProbeError::Unavailable`] here: this build has no `FFmpeg`.
#[cfg(not(unix))]
pub fn probe(path: &Path) -> Result<Probe, ProbeError> {
    let _ = path;
    Err(ProbeError::Unavailable)
}

/// Whether this build can read a file at all.
///
/// For a caller that has to decide what to do about a video *before* it has one
/// to look at — the difference between "we checked and it is wrong" and "we
/// could not check".
#[must_use]
pub const fn available() -> bool {
    cfg!(unix)
}

/// The codecs worth telling a client about, of the ones this build can decode.
///
/// Not every decoder `FFmpeg` was compiled with — that is hundreds of names,
/// most of them for formats nobody has had since 1998, and a list that long
/// says nothing. These are the ones an operator might actually bring to a show,
/// filtered by whether this build really has them, so the answer is a fact
/// about the binary rather than a hopeful list in the source.
///
/// Empty on a build with no `FFmpeg`, which a client should read as "this
/// server cannot say" rather than "this server plays nothing".
#[must_use]
pub fn playable_codecs() -> Vec<String> {
    WORTH_SAYING
        .iter()
        .filter(|name| can_decode(name))
        .map(|name| (*name).to_owned())
        .collect()
}

/// The names [`playable_codecs`] asks about, in the order it reports them.
///
/// Roughly: what a show is made of first, then what an operator's camera or
/// phone produced, then the editing formats a clip arrives in when somebody
/// exported it from a timeline. `hap` is in the list because the owner named
/// it and because a build without it should say so.
const WORTH_SAYING: &[&str] = &[
    "h264",
    "hevc",
    "av1",
    "vp9",
    "prores",
    "hap",
    "dnxhd",
    "mpeg2video",
    "mpeg1video",
    "mpeg4",
    "vc1",
    "mjpeg",
    "cfhd",
    "png",
    "jpeg2000",
];

/// Whether this build has a decoder for a codec, by `FFmpeg`'s short name.
///
/// # Why a caller would ask
///
/// To answer a question before there is a file: a client deciding whether to
/// convert, a diagnostics screen listing what the server plays. Anything that
/// *has* a file should probe it instead, because the name is not the whole
/// answer — a decoder can exist and still refuse a particular profile of it.
#[cfg(unix)]
#[must_use]
pub fn can_decode(codec: &str) -> bool {
    ffmpeg_next::codec::decoder::find_by_name(codec).is_some()
}

/// Whether this build has a decoder for a codec, by `FFmpeg`'s short name.
///
/// Always false here: this build has no `FFmpeg`.
#[cfg(not(unix))]
#[must_use]
pub fn can_decode(codec: &str) -> bool {
    let _ = codec;
    false
}

/// The codec's short name.
#[cfg(unix)]
pub(crate) fn codec_name(id: ffmpeg_next::codec::Id) -> String {
    // `Id::name()` is not in the binding, so this goes through the descriptor,
    // which is where FFmpeg keeps the short name every other tool prints.
    ffmpeg_next::codec::decoder::find(id).map_or_else(
        || format!("{id:?}").to_lowercase(),
        |codec| codec.name().to_owned(),
    )
}

/// How long the file is, preferring the stream's own answer.
///
/// A container's duration covers everything in it, including audio that runs
/// past the picture; the video stream's own is what a cue should be built on.
#[cfg(unix)]
pub(crate) fn duration_ms(
    input: &ffmpeg_next::format::context::Input,
    stream: &ffmpeg_next::format::stream::Stream<'_>,
) -> Option<u64> {
    use ffmpeg_next as ffmpeg;

    let base = stream.time_base();
    let ticks = stream.duration();
    if ticks > 0 && base.denominator() > 0 {
        let numerator = i128::from(ticks) * i128::from(base.numerator()) * 1000;
        let denominator = i128::from(base.denominator());
        return u64::try_from(numerator / denominator).ok();
    }

    // The container's answer, which is in FFmpeg's own time base rather than the
    // stream's.
    let whole = input.duration();
    if whole > 0 {
        let milliseconds = i128::from(whole) * 1000 / i128::from(ffmpeg::ffi::AV_TIME_BASE);
        return u64::try_from(milliseconds).ok();
    }

    None
}

/// A rational as a frame rate, when it is one.
#[cfg(unix)]
fn rate_of(rational: ffmpeg_next::Rational) -> Option<FrameRate> {
    let numerator = u32::try_from(rational.numerator()).ok()?;
    let denominator = u32::try_from(rational.denominator()).ok()?;
    (numerator > 0 && denominator > 0).then(|| FrameRate::new(numerator, denominator))
}

/// Whether a pixel format carries alpha.
///
/// Asked of `FFmpeg`'s own descriptor rather than matched against a list of
/// format names. The list is long and grows, and a missing entry would report an
/// alpha clip as opaque — which an operator finds out about when a key comes out
/// as a black rectangle in front of an audience.
///
/// The binding exposes the descriptor but not its flags, so this reads the one
/// bit `FFmpeg` itself tests. The pointer comes from `av_pix_fmt_desc_get`, which
/// returns an entry in a static table compiled into the library: it outlives the
/// process and is never written.
#[cfg(unix)]
#[allow(unsafe_code)]
pub(crate) fn has_alpha(format: ffmpeg_next::format::Pixel) -> bool {
    /// The bit `FFmpeg` sets on a format that carries alpha. Declared signed in
    /// the header and stored unsigned in the struct, which is the library's
    /// business rather than a question about the value.
    const ALPHA: u64 = ffmpeg_next::ffi::AV_PIX_FMT_FLAG_ALPHA as u64;

    let Some(descriptor) = format.descriptor() else {
        return false;
    };

    // SAFETY: `descriptor` came from `av_pix_fmt_desc_get`, which returns a
    // pointer into `FFmpeg`'s static descriptor table or null; `descriptor()`
    // already turned null into `None`.
    let flags = unsafe { (*descriptor.as_ptr()).flags };

    flags & ALPHA != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_build_without_ffmpeg_says_so_rather_than_guessing() {
        // The distinction that matters to a caller: "we looked and it is wrong"
        // is a refusal, "we could not look" is not.
        assert_eq!(available(), cfg!(unix));
    }

    #[cfg(not(unix))]
    #[test]
    fn nothing_can_be_read_where_there_is_nothing_to_read_it_with() {
        let error = probe(Path::new("anything.mov")).expect_err("no FFmpeg here");
        assert!(matches!(error, ProbeError::Unavailable));
    }

    #[cfg(not(unix))]
    #[test]
    fn a_build_with_no_ffmpeg_claims_no_codecs() {
        // Empty means "this server cannot say", which is the truth here. It
        // must not be read as "plays nothing", and nothing here can stop a
        // caller reading it that way except saying so where both are written.
        assert!(playable_codecs().is_empty());
        assert!(!can_decode("h264"));
    }

    #[cfg(unix)]
    #[test]
    fn what_the_server_says_it_plays_it_can_really_decode() {
        let codecs = playable_codecs();

        // The whole point of filtering the list: every name in the answer was
        // asked of this binary, not copied out of the source.
        for codec in &codecs {
            assert!(can_decode(codec), "{codec} was claimed and is not there");
        }

        // The ones the owner named. If a build ever loses one of these the
        // seven-layer target goes with it, so it is worth failing
        // here rather than finding out on a wall.
        for wanted in ["h264", "hevc", "prores"] {
            assert!(
                codecs.iter().any(|codec| codec == wanted),
                "this build cannot decode {wanted}, which it is supposed to play"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_codec_nobody_has_is_not_claimed() {
        assert!(!can_decode("no such codec"));
        assert!(!can_decode(""));
    }

    #[test]
    fn a_picture_keeps_the_proportions_of_what_it_is_a_picture_of() {
        // A library of squashed thumbnails is a library an operator cannot read
        // at a glance, which is the only thing a thumbnail is for.
        assert_eq!(fit(SizePx::new(1920, 1080), 320), SizePx::new(320, 180));
        assert_eq!(fit(SizePx::new(1080, 1920), 320), SizePx::new(180, 320));
        assert_eq!(
            fit(SizePx::new(64, 64), 320),
            SizePx::new(64, 64),
            "never larger"
        );
        assert_eq!(
            fit(SizePx::new(4000, 4), 320),
            SizePx::new(320, 1),
            "never nothing"
        );
    }

    #[cfg(unix)]
    mod files {
        use super::*;

        /// The `ProRes` clip the renderer's tests decode.
        ///
        /// Shared rather than copied: two fixtures of the same clip drift, and
        /// the whole point of this test is that what the server reads agrees
        /// with what the renderer plays.
        fn colours() -> std::path::PathBuf {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../ovs-renderer/tests/media/colours.mov")
        }

        #[test]
        fn ten_pictures_of_a_clip_start_where_the_still_does() {
            // The owner asked for ten frames across the whole clip (2026-09-23),
            // and for a pad to show the first of them when nothing is pointing
            // at it. That only reads as one thing if the still on the shelf and
            // the first frame of the strip are the same picture — otherwise a
            // pad changes appearance the moment the pointer arrives and again
            // when it leaves, for no reason an operator can see.
            let taken = frames(&colours(), 10, 32).expect("the fixture opens");
            assert_eq!(taken.len(), 10, "one per tenth of the clip");

            let still = thumbnail(&colours(), 32).expect("the fixture opens");
            assert_eq!(
                taken.first().expect("the first").pixels,
                still.pixels,
                "the first frame of the strip is the still"
            );

            // And they are not all the same picture, which is the other half of
            // the promise: this fixture changes colour as it runs.
            let first = &taken.first().expect("the first").pixels;
            assert!(
                taken.iter().any(|frame| &frame.pixels != first),
                "a preview of ten identical frames is not a preview"
            );

            for frame in &taken {
                assert_eq!(frame.size, still.size, "every frame at one size");
            }
        }

        #[test]
        fn a_clip_says_what_it_is() {
            // The same readings the renderer's own tests take of this file. If
            // these two ever disagree, the library shows an operator one thing
            // and the wall shows another.
            let probe = probe(&colours()).expect("the fixture opens");

            assert_eq!(probe.codec, "prores", "the fixture is ProRes");
            assert_eq!(probe.size, SizePx::new(64, 64));
            assert_eq!(probe.duration_ms, Some(1200), "30 frames at 25 fps");
            assert_eq!(probe.rate, Some(FrameRate::new(25, 1)));
            assert_eq!(probe.frames, Some(30));
            assert!(!probe.has_alpha, "422 Proxy carries no alpha");
        }

        #[test]
        fn a_picture_of_a_clip_is_a_picture_of_that_clip() {
            let picture = thumbnail(&colours(), 32).expect("the fixture opens");

            assert_eq!(picture.size, SizePx::new(32, 32));
            assert_eq!(
                picture.pixels.len(),
                32 * 32 * 4,
                "tightly packed RGBA, with the padding the decoder used removed"
            );

            // The fixture is ten frames of red, then green, then blue. A tenth
            // of the way in is inside the red, and whatever else is true of it,
            // it is not black — which is the thing a library full of first
            // frames would be.
            let lit = picture
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|pixel| pixel[0] > 32)
                .count();
            assert!(lit > 512, "{lit} pixels of the 1024 carry any red at all");
        }

        #[test]
        fn a_file_that_is_not_there_is_not_a_file_that_is_wrong() {
            // Both are refusals, but an operator reading the reason needs to
            // know which one happened.
            let error = probe(Path::new("/nonexistent/nothing.mov")).expect_err("no such file");
            assert!(matches!(error, ProbeError::Unreadable { .. }), "{error}");
        }

        #[test]
        fn something_that_is_not_media_at_all_is_refused() {
            let error = probe(Path::new(file!())).expect_err("this source file is not a clip");
            assert!(
                matches!(
                    error,
                    ProbeError::Unreadable { .. } | ProbeError::NoVideo { .. }
                ),
                "{error}"
            );
        }
    }
}
