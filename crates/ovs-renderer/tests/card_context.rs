//! What one more clip on the card costs to keep open.
//!
//! The renderer's card decoders used to be FFmpeg's `*_cuvid`, and each of them
//! made a CUDA context of its own — host memory for every clip held open, and on
//! the reference server's driver a context that was not all given back when the
//! clip closed: 2 MiB left behind for every H.265 clip opened. Since 0.3.12
//! every clip decodes into one context the whole process shares.
//!
//! Measured with this test in the WSL on the development machine (RTX 3060 Ti),
//! three runs each: **86,417–86,425 KiB** a clip through `h264_cuvid`, which
//! fails it, and **6,799–6,801 KiB** through `h264_nvdec`, which passes. The
//! line is at 32 MiB, a long way from both.
//!
//! The leak itself shows only on that driver, so it cannot be a test. What can
//! is the cause: a clip that brings a context of its own costs a context's worth
//! of memory to keep open, and one that shares costs its decoder and nothing
//! else. This keeps several open and holds the cost of each one below what a
//! context costs.
//!
//! Alone in its file because it reads the process's resident memory, and every
//! other test in a binary runs beside it on threads of the same process.

#![cfg(unix)]

use ovs_renderer::media::VideoSource;

/// Sixty-four pixels of H.264, which every NVIDIA card decodes — HEVC's
/// smallest picture on the card is 144 pixels, and the committed HEVC fixture
/// is smaller than that.
const CLIP: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/media/colours-h264.mp4");

/// How many more clips are opened beside the first.
const MORE: u64 = 4;

/// What this process holds in memory, in KiB.
fn resident_kib() -> u64 {
    let statm = std::fs::read_to_string("/proc/self/statm").expect("statm is readable");
    let pages: u64 = statm
        .split_whitespace()
        .nth(1)
        .and_then(|field| field.parse().ok())
        .expect("statm has a resident field");
    pages * 4
}

#[test]
fn a_clip_on_the_card_brings_no_cuda_context_of_its_own() {
    let first = VideoSource::open(CLIP).expect("the fixture opens");
    let processor = VideoSource::open_on_processor(CLIP).expect("the fixture opens");
    // Said out loud rather than assumed: with no card both sides are the
    // processor, and a memory figure for that says nothing about the card.
    if first.decoder_name() == processor.decoder_name() {
        println!(
            "skipped: no hardware decoder here, both sides are {}",
            processor.decoder_name()
        );
        return;
    }
    drop(processor);

    let before = resident_kib();
    let more: Vec<VideoSource> = (0..MORE)
        .map(|_| VideoSource::open(CLIP).expect("the fixture opens again"))
        .collect();
    let after = resident_kib();

    for clip in &more {
        assert_eq!(
            clip.decoder_name(),
            first.decoder_name(),
            "every clip is on the card"
        );
    }
    let each = after.saturating_sub(before) / MORE;
    println!(
        "{}: {} KiB for each clip opened beside the first",
        first.decoder_name(),
        each
    );
    assert!(
        each < 32 * 1024,
        "each clip on the card costs {each} KiB to keep open — with a CUDA \
         context of its own it was about 86,000, sharing one about 6,800"
    );
}
