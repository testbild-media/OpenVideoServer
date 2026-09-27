//! Main 10, end to end, on whatever this machine has.
//!
//! The owner's verification list of 2026-09-23. Every item here is about the
//! *decoder* half of the ten-bit path: that a ten-bit clip is decoded at ten
//! bits, that no `swscale` conversion runs behind it, that the two storage
//! layouts are told apart, and that range and matrix come off the file rather
//! than out of a default. The card half — textures, the shader, mixed depths in
//! one composition — is in `vulkan::offscreen`'s own tests, because it needs a
//! device and these do not.
//!
//! # Why these fixtures are generated
//!
//! The committed fixtures are 64x64 and 7 KB. Main 10 at 1080p60 and 4K60 is
//! not committable, and the owner's decision was that synthetic material is
//! enough to begin with (real Main 10 off a camera is a later, separate test).
//! So these ask `ffmpeg` for what they need and **skip** where there is none —
//! the same choice `media::hap`'s real-file test makes. A skipped test says so
//! on stdout; it does not pass quietly.

#![cfg(unix)]

use std::path::{Path, PathBuf};

use ovs_renderer::media::{ColourMatrix, FrameSource, SampleDepth, VideoSource};

/// Where generated fixtures go, one directory a shape so they can be reused.
fn scratch(name: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!("ovs-ten-bit-{name}"));
    let _ = std::fs::create_dir_all(&directory);
    directory
}

/// Writes one clip with this machine's `ffmpeg`, or answers `None`.
///
/// `arguments` are everything between the source and the output file, so a
/// caller says only what it cares about. The source is always `testsrc2`, which
/// has saturated primaries in it — a gradient would pass a test that a wrong
/// matrix would fail.
fn encoded(name: &str, size: &str, rate: u32, seconds: u32, arguments: &[&str]) -> Option<PathBuf> {
    let clip = scratch(name).join("clip.mp4");
    if clip.exists() {
        return Some(clip);
    }

    let source = format!("testsrc2=size={size}:rate={rate}:duration={seconds}");
    let made = std::process::Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
        ])
        .arg(&source)
        .args(arguments)
        .arg(&clip)
        .status();

    match made {
        Ok(status) if status.success() => Some(clip),
        _ => {
            println!("skipped: this machine's ffmpeg would not write {name}");
            None
        }
    }
}

/// A Main 10 clip: HEVC, `yuv420p10le`, narrow range, BT.709.
fn main10(name: &str, size: &str, rate: u32, seconds: u32) -> Option<PathBuf> {
    encoded(
        name,
        size,
        rate,
        seconds,
        &[
            "-c:v",
            "libx265",
            "-pix_fmt",
            "yuv420p10le",
            "-preset",
            "ultrafast",
            "-x265-params",
            "log-level=none",
            "-color_range",
            "tv",
            "-colorspace",
            "bt709",
            "-color_primaries",
            "bt709",
            "-color_trc",
            "bt709",
            "-tag:v",
            "hvc1",
        ],
    )
}

/// What one source says about itself, as one line for a failure message.
fn described(source: &VideoSource) -> String {
    format!(
        "{} {:?} full_range={} {:?} conversion={:?}",
        source.decoder_name(),
        source.depth(),
        source.full_range(),
        source.matrix(),
        source.conversion()
    )
}

#[test]
fn a_main_ten_clip_is_decoded_at_ten_bits_with_nothing_converting_it() {
    // Item one and item fourteen of the list, which are the same test read from
    // two ends: `yuv420p10le` software decode, and no `swscale` 10-to-8 fallback
    // in the native path. If a conversion were running the picture would still
    // be right — that is what made the old behaviour easy to miss — so the
    // assertion is on the absence of the conversion, not on the colour.
    let Some(clip) = main10("software-1080p", "320x240", 25, 1) else {
        return;
    };

    let source = VideoSource::open_on_processor(&clip).expect("the fixture opens");
    assert_eq!(
        source.depth(),
        SampleDepth::TenLow,
        "a software decoder puts the ten bits at the bottom of each element: {}",
        described(&source)
    );
    assert_eq!(
        source.conversion(),
        None,
        "ten-bit material must reach the card as it is: {}",
        described(&source)
    );
}

#[test]
fn an_eight_bit_clip_still_takes_the_eight_bit_path() {
    // The other half of the owner's instruction, and the one a ten-bit change
    // is most likely to break: eight-bit source, eight-bit path, no widening
    // into a sixteen-bit representation for the sake of having one path.
    let Some(clip) = encoded(
        "software-eight",
        "320x240",
        25,
        1,
        &[
            "-c:v",
            "libx265",
            "-pix_fmt",
            "yuv420p",
            "-preset",
            "ultrafast",
            "-x265-params",
            "log-level=none",
            "-tag:v",
            "hvc1",
        ],
    ) else {
        return;
    };

    let source = VideoSource::open_on_processor(&clip).expect("the fixture opens");
    assert_eq!(
        source.depth(),
        SampleDepth::Eight,
        "eight bits stays eight bits: {}",
        described(&source)
    );
    assert_eq!(source.conversion(), None, "{}", described(&source));
}

#[test]
fn the_card_hands_back_the_other_layout_and_it_is_not_confused_with_the_first() {
    // Item two: P010 off NVDEC. The two layouts are sixty-four times apart and
    // there is no arithmetic that makes them equal, so what this checks is that
    // the same file decoded two ways is described two ways — `TenHigh` off the
    // card, `TenLow` off the processor — and still needs no conversion either
    // way.
    //
    // Skipped out loud where there is no card. Comparing software against
    // software and reporting green is the failure this avoids.
    let Some(clip) = main10("card-1080p", "320x240", 25, 1) else {
        return;
    };

    let on_card = VideoSource::open(&clip).expect("the fixture opens");
    let on_processor = VideoSource::open_on_processor(&clip).expect("the fixture opens");
    if on_card.decoder_name() == on_processor.decoder_name() {
        println!(
            "skipped: no hardware decoder here, both sides are {}",
            on_processor.decoder_name()
        );
        return;
    }

    assert_eq!(
        on_card.depth(),
        SampleDepth::TenHigh,
        "the card's P010 has its ten bits at the top: {}",
        described(&on_card)
    );
    assert_eq!(
        on_processor.depth(),
        SampleDepth::TenLow,
        "the processor's yuv420p10le has them at the bottom: {}",
        described(&on_processor)
    );
    assert_ne!(
        on_card.depth(),
        on_processor.depth(),
        "these two must never collapse into one depth"
    );
    assert_eq!(on_card.conversion(), None, "{}", described(&on_card));
}

#[test]
fn a_clip_decoded_both_ways_is_the_same_ten_bit_picture() {
    // The layouts differ; the picture must not. This is the test that would
    // have caught a shift of six bits in either direction, which is the mistake
    // the two layouts invite.
    let Some(clip) = main10("both-ways", "320x240", 25, 1) else {
        return;
    };

    let mut on_card = VideoSource::open(&clip).expect("opens");
    let mut on_processor = VideoSource::open_on_processor(&clip).expect("opens");
    if on_card.decoder_name() == on_processor.decoder_name() {
        println!("skipped: no hardware decoder here");
        return;
    }

    for index in [0, 5, 12] {
        let card = on_card.frame(index).expect("a frame from the card");
        let processor = on_processor
            .frame(index)
            .expect("a frame from the processor");
        let (left, right) = (centre(&card), centre(&processor));
        let apart = [
            i32::from(left.0) - i32::from(right.0),
            i32::from(left.1) - i32::from(right.1),
            i32::from(left.2) - i32::from(right.2),
        ];
        assert!(
            apart.iter().all(|channel| channel.abs() <= 8),
            "frame {index}: the card gave {left:?} and the processor {right:?} — \
             a shift of six bits would show here"
        );
    }
}

#[test]
fn range_and_matrix_come_off_the_file_rather_than_out_of_a_default() {
    // Items four and five: limited against full range, and BT.709 against
    // BT.2020. Four clips, each tagged one way, each read back as what it says
    // it is. A default that happened to be right for three of them would fail
    // on the fourth.
    let cases: [(&str, &str, &str, bool, ColourMatrix); 4] = [
        ("tv-709", "tv", "bt709", false, ColourMatrix::Bt709),
        ("pc-709", "pc", "bt709", true, ColourMatrix::Bt709),
        ("tv-2020", "tv", "bt2020nc", false, ColourMatrix::Bt2020),
        ("pc-2020", "pc", "bt2020nc", true, ColourMatrix::Bt2020),
    ];

    for (name, range, space, full_range, matrix) in cases {
        let Some(clip) = encoded(
            name,
            "320x240",
            25,
            1,
            &[
                "-c:v",
                "libx265",
                "-pix_fmt",
                "yuv420p10le",
                "-preset",
                "ultrafast",
                "-x265-params",
                "log-level=none",
                "-color_range",
                range,
                "-colorspace",
                space,
                "-color_primaries",
                if space == "bt709" { "bt709" } else { "bt2020" },
                "-color_trc",
                if space == "bt709" {
                    "bt709"
                } else {
                    "bt2020-10"
                },
                "-tag:v",
                "hvc1",
            ],
        ) else {
            return;
        };

        let source = VideoSource::open_on_processor(&clip).expect("opens");
        assert_eq!(
            source.full_range(),
            full_range,
            "{name}: the range it was tagged with: {}",
            described(&source)
        );
        assert_eq!(
            source.matrix(),
            matrix,
            "{name}: the matrix it was tagged with — BT.2020 read as BT.709 is a \
             hue error of several degrees in saturated reds: {}",
            described(&source)
        );
        assert_eq!(source.conversion(), None, "{name}: {}", described(&source));
    }
}

#[test]
fn main_ten_at_a_shows_size_and_rate_decodes_without_converting() {
    // Items six and seven: 1080p60 and 4K60 Main 10. Not a measurement — that
    // is `ovs-decode-cost`'s job and needs more frames than a test should take
    // — but the sizes matter here for a different reason. 4K is where a stride
    // stops matching a width, and a plane copied by width rather than by stride
    // is a sheared picture at 3840 and a correct one at 320.
    for (name, size, rate) in [("hd60", "1920x1080", 60), ("uhd60", "3840x2160", 60)] {
        let Some(clip) = main10(name, size, rate, 1) else {
            return;
        };

        let mut source = VideoSource::open_on_processor(&clip).expect("opens");
        assert_eq!(source.depth(), SampleDepth::TenLow, "{name}");
        assert_eq!(source.conversion(), None, "{name}: {}", described(&source));

        let frame = source.frame(0).expect("a frame");
        let expected: Vec<u32> = size
            .split('x')
            .map(|side| side.parse().expect("a number"))
            .collect();
        assert_eq!(frame.size().width, expected[0], "{name}");
        assert_eq!(frame.size().height, expected[1], "{name}");
        // Two bytes a luma sample, tightly packed. A plane copied by width
        // instead of by stride would be short here rather than sheared, which
        // is the failure worth catching in a test rather than on a wall.
        assert_eq!(
            frame.image.pixels().len(),
            (expected[0] * expected[1] * 2) as usize,
            "{name}: ten-bit luma is two bytes a sample"
        );
    }
}

#[test]
fn several_main_ten_streams_decode_at_once() {
    // Item eight. Not a throughput measurement: what this asks is whether four
    // ten-bit decoders can exist at the same time and each give back its own
    // frames, which is the thing that fails when a decoder holds a global or a
    // card runs out of sessions. NVENC-class hardware has a session limit and
    // it is the kind of limit that appears as a fifth layer going black.
    let Some(clip) = main10("parallel", "640x360", 30, 1) else {
        return;
    };

    let mut sources: Vec<VideoSource> = (0..4)
        .map(|_| VideoSource::open(&clip).expect("opens"))
        .collect();

    for (index, source) in sources.iter_mut().enumerate() {
        assert!(
            source.depth().is_ten_bit(),
            "stream {index} lost its depth: {}",
            described(source)
        );
        let frame = source.frame(2).expect("a frame from every stream");
        assert_eq!(frame.size().width, 640, "stream {index}");
    }
}

fn centre(frame: &ovs_renderer::media::Frame) -> (u8, u8, u8) {
    let size = frame.size();
    let [red, green, blue, _] = frame.image.pixel(size.width / 2, size.height / 2);
    (red, green, blue)
}

/// Removes every fixture this file generated.
///
/// Not a test so much as a tidy-up with a name: the fixtures are reused across
/// runs on purpose, because 4K60 Main 10 takes a while to encode, and something
/// has to be able to clear them when a shape changes.
#[test]
#[ignore = "run with --ignored to clear the generated fixtures"]
fn clear_the_generated_fixtures() {
    for name in [
        "software-1080p",
        "software-eight",
        "card-1080p",
        "both-ways",
        "tv-709",
        "pc-709",
        "tv-2020",
        "pc-2020",
        "hd60",
        "uhd60",
        "parallel",
    ] {
        let directory = std::env::temp_dir().join(format!("ovs-ten-bit-{name}"));
        if Path::new(&directory).exists() {
            let _ = std::fs::remove_dir_all(&directory);
            println!("cleared {}", directory.display());
        }
    }
}
