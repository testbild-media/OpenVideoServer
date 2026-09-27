//! Receiving from a real sender on a real network.
//!
//! The unit tests beside the receiver prove the de-interleave against bytes
//! this program wrote itself, which is worth having and proves nothing about
//! whether a picture can be got off the network at all. That needs a sender, so
//! these skip — out loud, with the reason — on a machine that has no NDI
//! runtime or no sender to listen to.
//!
//! # What is proven here, and what was proven elsewhere
//!
//! Here: that a source can be found, connected to by name alone, and delivers
//! frames in the layout this build asks for; that the picture is held between
//! frames rather than dropped; and that a receiver stops when it is dropped.
//!
//! Not here: the colour. Which matrix NDI's YUV means cannot be established
//! from a sender whose content is unknown, so it was measured separately
//! against the SDK's own RGB conversion — BT.709 limited range, at a mean error
//! of 1.15 of 255 where the next candidate gave 3.10. The module documentation
//! on `ovs_renderer::ndi::receiver` records that measurement. What this file
//! checks is that a received frame is *labelled* with what was measured, so a
//! later edit cannot quietly change it.

use std::time::{Duration, Instant};

use ovs_renderer::media::ColourMatrix;
use ovs_renderer::ndi::{Ndi, NdiReceiver, Picture, Sender};

/// How long to look for a sender before deciding the network is quiet.
const DISCOVERY_MS: u32 = 5_000;

/// How long to wait for a first frame.
///
/// Connecting took 0.1 s on the reference network, so this is generous by two
/// orders of magnitude. A test that fails because a switch was busy teaches
/// nobody anything.
const FIRST_FRAME: Duration = Duration::from_secs(10);

/// The runtime and a source, or a printed reason and `None`.
fn a_sender() -> Option<(&'static Ndi, String)> {
    let ndi = match Ndi::load() {
        Ok(ndi) => ndi,
        Err(error) => {
            println!("skipped: {error}");
            return None;
        }
    };
    let sources = ndi.sources(DISCOVERY_MS);
    let Some(first) = sources.first() else {
        println!("skipped: the NDI runtime is here but nothing is sending on this network");
        return None;
    };
    println!("found {} source(s), using {first:?}", sources.len());
    Some((ndi, first.clone()))
}

/// Waits for the receiver to have a frame, or gives up.
fn wait_for_a_frame(receiver: &NdiReceiver) -> bool {
    let deadline = Instant::now() + FIRST_FRAME;
    while Instant::now() < deadline {
        if receiver.arrived() > 0 {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

#[test]
fn a_sender_on_the_network_arrives_in_the_layout_this_build_asks_for() {
    let Some((ndi, name)) = a_sender() else {
        return;
    };
    println!("runtime {}", ndi.version());

    let receiver = NdiReceiver::connect(ndi, &name);
    assert!(
        wait_for_a_frame(&receiver),
        "connected to {name:?} but no frame arrived in {FIRST_FRAME:?}"
    );

    let frame = receiver.frame().expect("a frame that arrived can be read");
    let size = frame.size();
    assert!(size.width > 0 && size.height > 0, "a frame with no pixels");

    let report = receiver.report();
    println!(
        "  {}x{} — {} frame(s), declared {:?}",
        size.width, size.height, report.arrived, report.declared_rate
    );
    assert!(
        report.connected,
        "frames arrived but the receiver says it is not connected"
    );
    assert_eq!(
        report.unusable, 0,
        "the sender sent a layout this build cannot read"
    );
    assert!(
        report.declared_rate.is_some(),
        "a frame arrived without the sender's declared rate being recorded"
    );

    match frame.chroma() {
        Some(chroma) => {
            // The measured settings. An edit that changes any of these changes
            // what every network source looks like on the wall, which is the
            // kind of thing that should not be possible to do quietly.
            assert_eq!(chroma.subsample_x(), 2, "UYVY is 4:2:2 across");
            assert_eq!(chroma.subsample_y(), 1, "UYVY is not subsampled down");
            assert!(!chroma.full_range(), "NDI's eight-bit YUV is limited range");
            // At every raster size. The height used to decide, from the
            // convention rather than from a measurement, and the measurement
            // disagreed everywhere it was taken.
            assert_eq!(
                chroma.matrix(),
                ColourMatrix::Bt709,
                "the measured matrix, whatever the size"
            );
            println!("  planar 4:2:2, {:?}, limited range", chroma.matrix());
        }
        None => {
            // The other half of what was asked for: a sender with an alpha
            // channel gives RGBA, which is already the renderer's own layout.
            assert_eq!(
                frame.pixels().len(),
                size.width as usize * size.height as usize * 4,
                "an RGBA frame has four bytes a pixel"
            );
            println!("  RGBA, so this sender has an alpha channel");
        }
    }
}

#[test]
fn the_picture_is_held_between_frames() {
    // The whole reason a receiver keeps its last frame. A still sender emits
    // one frame a second — measured at 999 ms apart — so a layer that showed
    // only what arrived in the current pass would be black fifty-nine sixtieths
    // of the time. This holds whichever sender is up: a fast one simply has
    // more frames in the window.
    let Some((ndi, name)) = a_sender() else {
        return;
    };
    let receiver = NdiReceiver::connect(ndi, &name);
    assert!(wait_for_a_frame(&receiver), "no first frame from {name:?}");

    let first = receiver.arrived();
    let mut looks = 0_u32;
    let until = Instant::now() + Duration::from_secs(2);
    while Instant::now() < until {
        assert!(
            receiver.frame().is_some(),
            "the receiver lost its picture between frames"
        );
        looks += 1;
        std::thread::sleep(Duration::from_millis(10));
    }
    println!(
        "  held across {looks} looks while {} frame(s) arrived",
        receiver.arrived() - first
    );
    assert!(looks > 100, "the loop did not actually run");
}

#[test]
fn dropping_a_receiver_stops_it_promptly() {
    // A show being closed drops every receiver, and a changeover does that
    // repeatedly. The wait is bounded by the capture timeout, which is the
    // reason that timeout is a tenth of a second rather than a comfortable few.
    let Some((ndi, name)) = a_sender() else {
        return;
    };
    let receiver = NdiReceiver::connect(ndi, &name);
    assert!(wait_for_a_frame(&receiver), "no first frame from {name:?}");

    let started = Instant::now();
    drop(receiver);
    let took = started.elapsed();
    println!("  dropped in {took:?}");
    assert!(
        took < Duration::from_secs(1),
        "dropping a receiver took {took:?}, which a changeover cannot afford"
    );
}

/// The side of the picture the loop test sends.
const SIDE: u32 = 64;

/// How far a channel may move on the way out and back.
///
/// **NDI's wire format is lossy**, whatever layout the receiver is handed. A
/// saturated blue quadrant measured 210 where 224 went out — fourteen units, and
/// blue is the channel that loses most of them because Cb carries the largest
/// coefficient. So a round trip is not bit-exact, and this number is not a
/// fidelity claim about it.
///
/// What it is: loose enough for the codec and nowhere near loose enough to let a
/// structural fault through. A swapped channel or a flipped picture puts a
/// primary where its opposite belongs, which is out by more than two hundred.
const TOLERANCE: i32 = 24;

/// Red, green, blue and white, one per quadrant, all fully opaque.
fn quadrants() -> Vec<u8> {
    let mut pixels = Vec::with_capacity((SIDE * SIDE * 4) as usize);
    for y in 0..SIDE {
        for x in 0..SIDE {
            let colour = match (x < SIDE / 2, y < SIDE / 2) {
                (true, true) => [0xE0, 0x10, 0x10, 0xFF],
                (false, true) => [0x10, 0xE0, 0x10, 0xFF],
                (true, false) => [0x10, 0x10, 0xE0, 0xFF],
                (false, false) => [0xE0, 0xE0, 0xE0, 0xFF],
            };
            pixels.extend_from_slice(&colour);
        }
    }
    pixels
}

/// The closed loop: this renderer's sender into this renderer's receiver.
///
/// The strongest check available without a second machine, and stronger than one
/// with a second machine would be, because it looks at the picture rather than at
/// whether something arrived. Four flat quadrants go out over the real network
/// and come back, and their colours are compared where they land.
///
/// # What it proves
///
/// The channel order, because red and blue differ by more than two hundred and a
/// swap cannot hide inside the tolerance. The row order, because a flip puts the
/// bottom quadrants on top. The stride and the size, because either being wrong
/// moves every sample point. That the alpha survives, because an opaque picture
/// coming back transparent is a layer nobody can see. And the naming, because the
/// sender is found through discovery rather than by assuming what it is called.
///
/// And the colour matrix and the range, end to end. This did not used to reach
/// them: the sender published an alpha channel, an alpha-bearing source is
/// received as RGBA, and nothing on that path applies a matrix at all. Now that
/// nothing publishes alpha — the owner's decision, not a side effect — an opaque
/// sender comes back as 4:2:2 and every colour here makes the whole round trip
/// through the coefficients.
///
/// Flat quadrants rather than a gradient, sampled at their centres, so chroma
/// subsampling has nothing to do at the sample points.
#[test]
fn a_picture_sent_on_the_network_comes_back_the_same_picture() {
    let ndi = match Ndi::load() {
        Ok(ndi) => ndi,
        Err(error) => {
            println!("skipped: {error}");
            return;
        }
    };

    // Named after this process, so two runs on one machine — or a run beside the
    // installed service — cannot receive each other's pictures and pass.
    let stream = format!("ovs-loop-{}", std::process::id());
    let sender = Sender::start(ndi, &stream);

    // Offered continuously: a sender publishes nothing until it has a frame, and
    // the receiver has to connect before one is worth keeping. Sending the same
    // picture over and over is what a still source does anyway.
    let sending = std::thread::spawn({
        let stream = stream.clone();
        move || {
            let mut spare = Some(quadrants());
            let until = Instant::now() + Duration::from_secs(20);
            while Instant::now() < until {
                let pixels = spare.take().unwrap_or_else(quadrants);
                spare = sender.offer(Picture {
                    size: ovs_types::geometry::SizePx::new(SIDE, SIDE),
                    rate_numerator: 60,
                    rate_denominator: 1,
                    pixels,
                });
                if spare.is_none() {
                    spare = Some(quadrants());
                }
                std::thread::sleep(Duration::from_millis(16));
            }
            let report = sender.report();
            println!(
                "  sent {} of {} offered ({} superseded, {} malformed) as {stream:?}",
                report.sent, report.offered, report.superseded, report.malformed
            );
            assert_eq!(report.malformed, 0, "a frame this test built was refused");
            assert!(report.sent > 0, "nothing reached the network");
        }
    });

    // Found by discovery rather than assumed, which also proves the naming: the
    // runtime puts this machine in front of what was asked for.
    let mut announced = None;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && announced.is_none() {
        announced = ndi
            .sources(1_000)
            .into_iter()
            .find(|name| name.contains(&stream));
    }
    let Some(announced) = announced else {
        panic!("this machine could not see its own sender {stream:?} on the network");
    };
    println!("  asked for {stream:?}, the network announces {announced:?}");
    assert!(
        announced.ends_with(&format!("({stream})")),
        "the machine name belongs in front and the asked-for name in brackets"
    );

    let receiver = NdiReceiver::connect(ndi, &announced);
    assert!(
        wait_for_a_frame(&receiver),
        "connected to this machine's own sender and no frame came back"
    );
    let frame = receiver.frame().expect("a frame that arrived can be read");

    assert_eq!(frame.size().width, SIDE, "the width came back different");
    assert_eq!(frame.size().height, SIDE, "the height came back different");
    println!(
        "  received {}x{} as {}",
        frame.size().width,
        frame.size().height,
        if frame.chroma().is_some() {
            "planar 4:2:2"
        } else {
            "RGBA"
        }
    );

    // Well inside each quadrant, so chroma subsampling has nothing to do here.
    let quarter = SIDE / 4;
    // Typed, so both sides of the comparison below are pixel values and the
    // literals cannot quietly become something wider.
    let cases: [(&str, u32, u32, [u8; 3]); 4] = [
        ("top left, red", quarter, quarter, [0xE0, 0x10, 0x10]),
        (
            "top right, green",
            SIDE - quarter,
            quarter,
            [0x10, 0xE0, 0x10],
        ),
        (
            "bottom left, blue",
            quarter,
            SIDE - quarter,
            [0x10, 0x10, 0xE0],
        ),
        (
            "bottom right, white",
            SIDE - quarter,
            SIDE - quarter,
            [0xE0, 0xE0, 0xE0],
        ),
    ];
    for (where_, x, y, wanted) in cases {
        let got = frame.pixel(x, y);
        for (channel, (got, wanted)) in got.iter().zip(wanted.iter()).enumerate() {
            let drift = i32::from(*got) - i32::from(*wanted);
            assert!(
                drift.abs() <= TOLERANCE,
                "{where_}: channel {channel} came back {got} where {wanted} went out, \
                 which is {drift} — out by more than a round trip through 4:2:2 costs"
            );
        }
        assert_eq!(
            got[3], 0xFF,
            "{where_}: an opaque picture came back transparent"
        );
    }

    sending.join().expect("the sending thread");
}
