//! Reading a file closely enough to say whether this server will play it.
//!
//! [`probe`](crate::probe) answers "what is this", which is what the library
//! screen shows and what an upload records. This answers a narrower and
//! stricter question: **is this one of the three things the server accepts**
//! (as the owner narrowed it on 2026-09-23 — HEVC 8-bit, HEVC 10-bit,
//! HAP Alpha, and nothing else). The contract itself is
//! [`ovs_types::media::PlaybackProfile`]; this is the reading it is checked
//! against.
//!
//! # Why it costs a pass over the file
//!
//! Two of the contract's clauses cannot be read from a header. **How far apart
//! the keyframes are** is a property of the packets, and a container that
//! declares a GOP size is declaring what an encoder was asked for rather than
//! what it wrote. So this demuxes the whole file once — no decoding, just
//! packet flags — which on the reference machine is a few hundred megabytes a
//! second and is paid once, at ingest, against a file that has just been
//! written to disk anyway.
//!
//! The alternative was trusting the header, and the failure mode of trusting it
//! is a clip that seeks like treacle on the night because its real keyframe
//! interval was ten seconds and nobody looked.

use std::path::Path;

use ovs_types::media::ProbedVideo;

use crate::ProbeError;

/// Reads a file as the playback contract sees it.
///
/// # Errors
///
/// The same reasons [`crate::probe`] fails: unreadable, no video, no decoder
/// for what is in it.
#[cfg(unix)]
#[allow(unsafe_code)]
pub fn profile_of(path: &Path) -> Result<ProbedVideo, ProbeError> {
    use ffmpeg_next as ffmpeg;

    ffmpeg::init().map_err(|error| ProbeError::Unreadable {
        path: path.to_path_buf(),
        reason: error.to_string(),
    })?;

    let mut input = ffmpeg::format::input(&path).map_err(|error| ProbeError::Unreadable {
        path: path.to_path_buf(),
        reason: error.to_string(),
    })?;

    let (index, parameters, average, nominal, milliseconds) = {
        let stream = input
            .streams()
            .best(ffmpeg::media::Type::Video)
            .ok_or_else(|| ProbeError::NoVideo {
                path: path.to_path_buf(),
            })?;
        (
            stream.index(),
            stream.parameters(),
            as_f64(stream.avg_frame_rate()),
            as_f64(stream.rate()),
            // The same reading `probe` takes, which asks the stream first and
            // the container second: a small file often carries one and not the
            // other.
            crate::duration_ms(&input, &stream),
        )
    };

    let context =
        ffmpeg::codec::context::Context::from_parameters(parameters.clone()).map_err(|error| {
            ProbeError::Unreadable {
                path: path.to_path_buf(),
                reason: error.to_string(),
            }
        })?;

    let codec_id = context.id();
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

    // Two fields with no accessor in the binding. `field_order` is what says
    // whether a file is interlaced, and `bit_rate` is what the ceiling is
    // checked against; both are plain integers in a struct the binding hands
    // out a pointer to.
    //
    // SAFETY: `parameters` owns the allocation and outlives this borrow, and
    // `as_ptr` is documented to return it.
    let (declared_bits, field_order) = unsafe {
        let raw = parameters.as_ptr();
        (
            u64::try_from((*raw).bit_rate).unwrap_or(0),
            (*raw).field_order,
        )
    };

    // A container that will not say. The file's own size over its own duration
    // is the honest fallback and it errs high, because it counts the container
    // overhead the codec did not write — which is the right direction for a
    // ceiling. **A bitrate that could not be read must not become a file that
    // escapes the cap**, which is what returning zero here would be.
    let bits_per_second = if declared_bits > 0 {
        declared_bits
    } else {
        measured_bits(path, milliseconds)
    };

    let (mut keyframe_interval, frames) = keyframe_interval(&mut input, index);
    // A file of one frame has no interval to measure. Saying "one" rather than
    // "zero" keeps it inside the contract instead of failing a clause that does
    // not apply to it.
    if frames <= 1 {
        keyframe_interval = 1;
    }

    Ok(ProbedVideo {
        codec: crate::codec_name(codec_id),
        pixel_format: pixel_format_name(video.format()),
        has_alpha: crate::has_alpha(video.format()),
        primaries: video.color_primaries().name().unwrap_or("").to_owned(),
        transfer: video
            .color_transfer_characteristic()
            .name()
            .unwrap_or("")
            .to_owned(),
        matrix: video.color_space().name().unwrap_or("").to_owned(),
        range: video.color_range().name().unwrap_or("").to_owned(),
        frame_rate: average,
        nominal_frame_rate: nominal,
        keyframe_interval,
        // The decoder's own answer rather than an inference from the packet
        // order: a file whose every frame happens to arrive in order can still
        // be coded with reordering, and what matters is what the decoder has to
        // be prepared for.
        has_b_frames: video.has_b_frames(),
        sample_aspect: sample_aspect(video.aspect_ratio()),
        // Neither progressive nor unspecified. Unknown is not a claim of
        // interlacing: an untagged file is progressive until it says otherwise,
        // which is what every tool assumes of one.
        interlaced: !matches!(
            field_order,
            ffmpeg::ffi::AVFieldOrder::AV_FIELD_PROGRESSIVE
                | ffmpeg::ffi::AVFieldOrder::AV_FIELD_UNKNOWN
        ),
        bits_per_second,
    })
}

/// Reads a file as the playback contract sees it.
///
/// # Errors
///
/// Always [`ProbeError::Unavailable`] here: this build has no `FFmpeg`.
#[cfg(not(unix))]
pub fn profile_of(path: &Path) -> Result<ProbedVideo, ProbeError> {
    let _ = path;
    Err(ProbeError::Unavailable)
}

/// The longest run of frames between two keyframes, and how many frames there
/// were.
///
/// Counts the *gap*, which is what a seek pays: a file whose keyframes are at
/// 0, 60 and 120 has an interval of 60, and one that is keyframe-only has an
/// interval of 1. A stream with no keyframe at all — which a broken file can
/// be — reports its whole length, which fails the contract as it should.
#[cfg(unix)]
fn keyframe_interval(input: &mut ffmpeg_next::format::context::Input, stream: usize) -> (u32, u64) {
    let mut frames: u64 = 0;
    let mut since: u32 = 0;
    let mut longest: u32 = 0;
    let mut seen_key = false;

    for (packet_stream, packet) in input.packets() {
        if packet_stream.index() != stream {
            continue;
        }
        frames += 1;
        if packet.is_key() {
            longest = longest.max(since);
            since = 1;
            seen_key = true;
        } else {
            since = since.saturating_add(1);
        }
    }
    longest = longest.max(since);

    if !seen_key {
        return (u32::try_from(frames).unwrap_or(u32::MAX).max(1), frames);
    }
    (longest.max(1), frames)
}

/// The file's own size over its own duration, in bits a second.
///
/// Zero only when the duration is unknown, which for a video means the file is
/// unusual enough that the contract's other clauses will have something to say
/// about it too.
#[cfg(unix)]
fn measured_bits(path: &Path, milliseconds: Option<u64>) -> u64 {
    let Ok(bytes) = std::fs::metadata(path).map(|data| data.len()) else {
        return 0;
    };
    let Some(milliseconds) = milliseconds.filter(|value| *value > 0) else {
        return 0;
    };
    #[allow(clippy::cast_precision_loss)]
    let seconds = milliseconds as f64 / 1_000.0;
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        clippy::cast_sign_loss
    )]
    let bits = (bytes as f64 * 8.0 / seconds).round();
    if bits.is_finite() && bits >= 0.0 {
        bits as u64
    } else {
        0
    }
}

/// `FFmpeg`'s own name for a pixel format, or empty for one it will not name.
#[cfg(unix)]
fn pixel_format_name(format: ffmpeg_next::format::Pixel) -> String {
    if format == ffmpeg_next::format::Pixel::None {
        return String::new();
    }
    // The descriptor is where FFmpeg keeps the short name every other tool
    // prints. A format with no descriptor is one this build does not know, and
    // its debug spelling is better than an empty string, which would read as
    // "untagged" to the contract.
    format.descriptor().map_or_else(
        || format!("{format:?}").to_lowercase(),
        |descriptor| descriptor.name().to_owned(),
    )
}

/// A sample aspect as a fraction, with "unspecified" read as square.
///
/// A container that says nothing writes 0/1, and square is what every tool
/// assumes of one. Reporting 0:1 instead would fail the contract for a file
/// that is perfectly square.
#[cfg(unix)]
fn sample_aspect(ratio: ffmpeg_next::Rational) -> (u32, u32) {
    let (numerator, denominator) = (ratio.numerator(), ratio.denominator());
    if numerator <= 0 || denominator <= 0 {
        return (1, 1);
    }
    (numerator.unsigned_abs(), denominator.unsigned_abs())
}

/// A rate as a plain number, and zero for one a container will not give.
#[cfg(unix)]
fn as_f64(rate: ffmpeg_next::Rational) -> f64 {
    if rate.denominator() == 0 {
        return 0.0;
    }
    f64::from(rate.numerator()) / f64::from(rate.denominator())
}
