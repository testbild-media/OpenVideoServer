//! Reads a file as the playback contract sees it, and says what is wrong with it.
//!
//! ```text
//! cargo run -p ovs-media --example profile-probe -- clip.mp4
//! ```
//!
//! What ingest will do to a file, before handing it to ingest. It prints the
//! reading [`ovs_media::profile_of`] takes and then asks the same three
//! questions the server asks — HEVC 8-bit, HEVC 10-bit, HAP Alpha — so that a
//! file which is going to be refused is refused here, at a prompt, with the
//! list of what to change.
//!
//! The keyframe interval is the interesting line. It cannot be read from a
//! header, it is the one clause that costs real time on the night, and it is
//! the one that most "already HEVC" files fail.

/// Reads the file named on the command line and reports on it.
fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: profile-probe <file>");
        std::process::exit(2);
    };

    let video = match ovs_media::profile_of(std::path::Path::new(&path)) {
        Ok(video) => video,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };

    // A bitrate large enough to lose precision in an f64 is a petabit a second,
    // and this is a number being printed.
    #[allow(clippy::cast_precision_loss)]
    let mbit = video.bits_per_second as f64 / 1_000_000.0;

    println!("{path}");
    println!("  codec            {}", video.codec);
    println!("  pixel format     {}", video.pixel_format);
    println!("  alpha            {}", video.has_alpha);
    println!("  primaries        {:?}", video.primaries);
    println!("  transfer         {:?}", video.transfer);
    println!("  matrix           {:?}", video.matrix);
    println!("  range            {:?}", video.range);
    println!(
        "  frame rate       {:.3} average, {:.3} nominal",
        video.frame_rate, video.nominal_frame_rate
    );
    println!(
        "  keyframes        every {} frames at most",
        video.keyframe_interval
    );
    println!("  b-frames         {}", video.has_b_frames);
    println!(
        "  sample aspect    {}:{}",
        video.sample_aspect.0, video.sample_aspect.1
    );
    println!("  interlaced       {}", video.interlaced);
    // Enough digits that a small file does not print as zero, which reads as
    // "could not be read" and is the one thing this must not be mistaken for.
    println!(
        "  bitrate          {mbit:.3} Mbit/s ({} bit/s)",
        video.bits_per_second
    );

    for depth in [
        ovs_types::media::BitDepth::Eight,
        ovs_types::media::BitDepth::Ten,
    ] {
        report(&video, ovs_types::media::PlaybackProfile::Hevc { depth });
    }
    report(&video, ovs_types::media::PlaybackProfile::HapAlpha);
}

/// Says whether one reading meets one profile, and what is wrong if it does not.
fn report(video: &ovs_types::media::ProbedVideo, profile: ovs_types::media::PlaybackProfile) {
    let faults = ovs_types::media::faults(video, profile);
    println!();
    if faults.is_empty() {
        println!("as {profile:?}: accepted");
        return;
    }
    println!("as {profile:?}: {} fault(s)", faults.len());
    for fault in faults {
        println!(
            "  {}: wanted {}, found {}",
            fault.what, fault.wanted, fault.found
        );
    }
}
