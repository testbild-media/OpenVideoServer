//! Decoding real files.
//!
//! Two fixtures of the same thirty frames: ten red, ten green, ten blue, 64x64
//! at 25 fps. One is ProRes 422 Proxy, which is all-intra; the other is H.264
//! with a fifteen-frame GOP, which is not. Both are committed rather than
//! generated, because a test that needs `ffmpeg` on the path is a test that
//! does not run.
//!
//! Solid colours are the point. A frame that comes back red is a frame that was
//! decoded, its planes packed the right way up and its colour converted — and
//! the frame it came back for is a seek that landed where it was asked to.
//!
//! The second fixture is here because the server plays what it is given
//! and long-GOP media seeks differently from all-intra. Every test in
//! this file that only used the first was testing the easy half.

#![cfg(unix)]

use ovs_renderer::media::{FrameSource, VideoSource};

/// Where the committed clips are.
const CLIP: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/media/colours.mov");
const LONG_GOP: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/media/colours-h264.mp4");

#[test]
fn a_clip_decoded_on_the_card_is_the_same_picture_as_one_decoded_off_it() {
    // NVDEC hands frames back with Cb and Cr interleaved in one plane and the
    // processor hands them back in two, and the split that puts that right is
    // the kind of code that is either exactly right or visibly wrong: the two
    // the wrong way round turns a red frame blue, and every frame of every clip
    // goes through it.
    //
    // Skipped rather than failed on a machine with no card, because that is the
    // development machine and half the test machines a build runs on.
    let mut on_card = VideoSource::open(LONG_GOP).expect("the fixture opens");
    let mut on_processor = VideoSource::open_on_processor(LONG_GOP).expect("the fixture opens");

    // Said out loud rather than assumed. The fall back to the processor is
    // silent by design, so without this a machine with no card would run this
    // test comparing software against software and report it as green.
    if on_card.decoder_name() == on_processor.decoder_name() {
        println!(
            "skipped: this machine has no hardware decoder, so both sides are {}",
            on_processor.decoder_name()
        );
        return;
    }

    for index in [0, 5, 12, 20, 29] {
        let card = on_card.frame(index).expect("a frame from the card");
        let processor = on_processor
            .frame(index)
            .expect("a frame from the processor");

        assert_eq!(
            card.image.size(),
            processor.image.size(),
            "frame {index} came back a different size"
        );

        let (card_rgb, processor_rgb) = (centre(&card), centre(&processor));
        let difference = [
            i32::from(card_rgb.0) - i32::from(processor_rgb.0),
            i32::from(card_rgb.1) - i32::from(processor_rgb.1),
            i32::from(card_rgb.2) - i32::from(processor_rgb.2),
        ];
        assert!(
            difference.iter().all(|channel| channel.abs() <= 8),
            "frame {index}: the card gave {card_rgb:?} and the processor {processor_rgb:?}"
        );
    }
}

/// The centre pixel of a frame, as RGB.
///
/// Through the picture's own accessor rather than off its bytes: a decoded
/// frame is three planes of Y'CbCr for most codecs now, and what this test is
/// asking is what colour the frame is rather than what is in its first plane.
fn centre(frame: &ovs_renderer::media::Frame) -> (u8, u8, u8) {
    let size = frame.size();
    let [red, green, blue, _] = frame.image.pixel(size.width / 2, size.height / 2);
    (red, green, blue)
}

/// Whether a pixel is the colour it was encoded as.
///
/// Not an exact comparison: a fixture went through subsampled Y'CbCr on the way
/// in and comes back through the same arithmetic the shader does, and neither
/// trip is lossless. A generous tolerance still tells red from green, which is
/// what this is asking.
#[track_caller]
fn assert_colour(actual: (u8, u8, u8), expected: (u8, u8, u8), what: &str) {
    let close = |left: u8, right: u8| u16::from(left).abs_diff(u16::from(right)) <= 24;
    assert!(
        close(actual.0, expected.0) && close(actual.1, expected.1) && close(actual.2, expected.2),
        "{what}: expected {expected:?}, got {actual:?}"
    );
}

const RED: (u8, u8, u8) = (255, 0, 0);
const GREEN: (u8, u8, u8) = (0, 255, 0);
const BLUE: (u8, u8, u8) = (0, 0, 255);

#[test]
fn a_clip_says_what_it_is_before_a_frame_is_asked_for() {
    let clip = VideoSource::open(CLIP).expect("the fixture opens");

    assert_eq!(clip.size().width, 64);
    assert_eq!(clip.size().height, 64);
    assert_eq!(clip.length().frame_count, 30);
    assert_eq!(clip.length().duration_ms, 1200, "30 frames at 25 fps");
    assert_eq!(
        clip.rate(),
        ovs_types::geometry::FrameRate {
            numerator: 25,
            denominator: 1
        }
    );
}

#[test]
fn the_frame_that_comes_out_is_the_frame_that_was_asked_for() {
    let mut clip = VideoSource::open(CLIP).expect("the fixture opens");

    assert_colour(centre(&clip.frame(5).expect("frame 5")), RED, "frame 5");
    assert_colour(
        centre(&clip.frame(15).expect("frame 15")),
        GREEN,
        "frame 15",
    );
    assert_colour(centre(&clip.frame(25).expect("frame 25")), BLUE, "frame 25");
}

#[test]
fn it_plays_forwards_one_frame_at_a_time() {
    // The common case, and the one that must not seek: sixty times a second,
    // the frame after the last one.
    let mut clip = VideoSource::open(CLIP).expect("the fixture opens");

    for index in 0..30 {
        let frame = clip.frame(index).expect("every frame decodes");
        assert_eq!(frame.index, index, "the frames arrive in order");

        let expected = match index {
            0..=9 => RED,
            10..=19 => GREEN,
            _ => BLUE,
        };
        assert_colour(centre(&frame), expected, &format!("frame {index}"));
    }
}

#[test]
fn it_plays_backwards_just_as_well() {
    // Not a pathological case: reverse is a playback direction an operator
    // chooses, and every frame of an all-intra clip costs the same
    // whichever way the playhead is moving.
    let mut clip = VideoSource::open(CLIP).expect("the fixture opens");

    for index in (0..30).rev() {
        let frame = clip.frame(index).expect("every frame decodes");
        assert_eq!(frame.index, index, "a seek lands where it was sent");

        let expected = match index {
            0..=9 => RED,
            10..=19 => GREEN,
            _ => BLUE,
        };
        assert_colour(centre(&frame), expected, &format!("frame {index}"));
    }
}

#[test]
fn asking_for_the_same_frame_twice_decodes_once() {
    // A fader held still asks for the same frame for as long as it is held.
    let mut clip = VideoSource::open(CLIP).expect("the fixture opens");

    let first = clip.frame(7).expect("frame 7");
    let again = clip.frame(7).expect("frame 7 again");

    assert!(
        std::sync::Arc::ptr_eq(&first.image, &again.image),
        "the second ask must be the same pixels, not another decode"
    );
}

#[test]
fn a_frame_past_the_end_holds_the_last_one() {
    // What a scheduler that rounded up at the end of a pass asks for. Clamping
    // is the documented behaviour of the trait; failing would drop a layer to
    // its placeholder for one tick at the end of every loop.
    let mut clip = VideoSource::open(CLIP).expect("the fixture opens");

    let frame = clip.frame(9_999).expect("the last frame");

    assert_eq!(frame.index, 29);
    assert_colour(centre(&frame), BLUE, "the last frame");
}

#[test]
fn a_file_that_is_not_a_clip_says_so_rather_than_panicking() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let path = directory.path().join("notes.txt");
    std::fs::write(&path, b"this is not a clip").expect("writes");

    let error = VideoSource::open(&path).expect_err("must not open");

    assert!(
        error.to_string().contains("notes.txt"),
        "the message names the file: {error}"
    );
}

#[test]
fn a_file_that_is_not_there_says_which_one() {
    let error = VideoSource::open("/nonexistent/clip.mov").expect_err("must not open");

    assert!(error.to_string().contains("clip.mov"), "{error}");
}

#[test]
fn a_long_gop_clip_plays_forwards_one_frame_at_a_time() {
    // The same walk as the ProRes fixture, on a codec where frame n is not a
    // keyframe. A decoder that seeks when it should run forward shows up here
    // as the wrong colour, and on a wall as a clip that stutters.
    let mut clip = VideoSource::open(LONG_GOP).expect("the fixture opens");

    for (index, expected, what) in [
        (0, RED, "the first frame"),
        (5, RED, "still red"),
        (12, GREEN, "past the first keyframe"),
        (13, GREEN, "and the next"),
        (25, BLUE, "past the second"),
        (29, BLUE, "the last frame"),
    ] {
        let frame = clip.frame(index).expect("decodes");
        assert_eq!(frame.index, index, "{what}");
        assert_colour(centre(&frame), expected, what);
    }
}

#[test]
fn a_long_gop_clip_seeks_backwards_to_the_frame_it_was_asked_for() {
    // The expensive direction, and the one the owner was told would stutter.
    // Stuttering is allowed; landing on the wrong frame is not.
    let mut clip = VideoSource::open(LONG_GOP).expect("the fixture opens");

    for (index, expected) in [(25, BLUE), (5, RED), (28, BLUE), (15, GREEN), (2, RED)] {
        let frame = clip.frame(index).expect("decodes");
        assert_eq!(frame.index, index, "seeking to {index}");
        assert_colour(centre(&frame), expected, &format!("frame {index}"));
    }
}

#[test]
fn a_long_gop_clip_is_decoded_as_planes_rather_than_converted() {
    // The change, at the one place it can be seen from the
    // outside: an H.264 frame arrives as three planes of Y'CbCr, which is 2.7
    // times less to copy than the RGBA it used to be turned into.
    let mut clip = VideoSource::open(LONG_GOP).expect("the fixture opens");
    let frame = clip.frame(0).expect("decodes");

    let chroma = frame
        .image
        .chroma()
        .expect("an H.264 frame is planar, not RGBA");
    assert_eq!(
        (chroma.subsample_x(), chroma.subsample_y()),
        (2, 2),
        "4:2:0"
    );
    assert_eq!(
        frame.image.byte_size(),
        64 * 64 * 3 / 2,
        "against {} as RGBA",
        64 * 64 * 4
    );
}
