//! Sharing a picture's rows out between threads.
//!
//! The one thing in this renderer that still makes every pixel on the
//! processor is a generator, and on 2026-10-15 that was measured as the only
//! thing the wall was waiting for. One source at a time on PC05, wall at
//! 1920x1080 at 59.94, reading the render thread's own `playback_us`:
//!
//! | what was live | a pass |
//! |---|---|
//! | nothing | 2 us |
//! | a 3840x2160 clip, decoded on the card | **6 us** |
//! | `ovs.test_pattern` | 4,158 us |
//! | `ovs.noise` | **25,532 us** |
//!
//! A 4K clip costs six microseconds and a generator costs twenty-five
//! milliseconds, on a machine with twenty cores, in one thread. Everything
//! here exists to stop that being true.
//!
//! # The rule a band has to keep
//!
//! A band draws the rows it was given and nothing else, and is told where
//! those rows sit in the whole picture. The result must not depend on how many
//! bands there were — otherwise the wall shows seams where the threads met,
//! and a seam that only appears on a machine with a particular number of cores
//! is the worst kind of fault to be sent a photograph of.
//!
//! That rule is what [`in_rows`] exists for: it takes the number of bands as
//! an argument so a test can force two, three and more against the one-band
//! picture. A test that asked the machine would compare one band with one band
//! on a two-core runner and prove nothing.

/// How many threads a picture is drawn across.
///
/// The same arithmetic the changeover pool uses, and for the same reason: two
/// cores are left alone whatever the machine has, because one of them is the
/// render thread's own caller and the others belong to the decoders.
///
/// **Eight is where this stops, and it is a choice rather than a limit.** One
/// frame of 1920x1080 coloured noise, drawn twenty times, on the sixteen-core
/// development machine:
///
/// | bands | a frame |
/// |---|---|
/// | 1 | 23.11 ms |
/// | 2 | 11.84 |
/// | 4 | 6.28 |
/// | **8** | **4.91** |
/// | 12 | 4.08 |
/// | 16 | 3.48 |
///
/// Sixteen is a third quicker again and takes every core in the machine to do
/// it. A media server is not a benchmark: the decoders, the encoder and the
/// console all want a core, and a generator that drew in two milliseconds by
/// starving a decoder would have moved the fault rather than fixed it. Eight
/// buys the part of the curve that is nearly free.
pub(crate) fn bands() -> usize {
    std::thread::available_parallelism()
        .map_or(1, |cores| cores.get().saturating_sub(2).clamp(1, 8))
}

/// Draws a picture in horizontal bands, one thread a band.
///
/// `width` is in pixels and every picture here is four bytes to one. `draw` is
/// handed a band and the index of its first row in the whole picture.
///
/// One band draws in place, with no thread started at all — which is the
/// answer on a machine with two cores.
pub(crate) fn in_bands(pixels: &mut [u8], width: usize, draw: impl Fn(&mut [u8], usize) + Sync) {
    in_rows(pixels, width, bands(), draw);
}

/// The same, with the number of bands stated rather than asked of the machine.
///
/// What [`in_bands`] is built from, and what a test uses — see the note on the
/// module about why the count has to be an argument.
///
/// Threads are started for each call rather than kept in a pool. At eight
/// bands and sixty passes a second that is about a fifth of a millisecond a
/// frame against the twenty it saves, and a pool would be state to own, drain
/// and shut down in a module that currently owns nothing. Worth revisiting if
/// a profile ever shows the spawning rather than the drawing.
pub(crate) fn in_rows(
    pixels: &mut [u8],
    width: usize,
    bands: usize,
    draw: impl Fn(&mut [u8], usize) + Sync,
) {
    let rows = pixels.len().checked_div(width * 4).unwrap_or(0);
    if rows == 0 {
        // A layer sized to nothing. The drawer is not called at all rather
        // than called with an empty band, because "draw these rows" with no
        // rows is a question every drawer would have to answer separately.
        return;
    }

    let bands = bands.clamp(1, rows);
    if bands <= 1 {
        draw(pixels, 0);
        return;
    }

    let per_band = rows.div_ceil(bands);
    let bytes = per_band * width * 4;
    std::thread::scope(|scope| {
        for (index, band) in pixels.chunks_mut(bytes).enumerate() {
            let draw = &draw;
            scope.spawn(move || draw(band, index * per_band));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_row_is_drawn_exactly_once_however_many_bands_there_are() {
        // The property the pictures depend on, checked directly rather than
        // through a generator: each row is visited once, by the band that
        // holds it, and told the right index.
        for width in [1_usize, 7, 64] {
            for height in [1_usize, 2, 5, 16, 17] {
                for count in [1_usize, 2, 3, 8, height + 3] {
                    let mut pixels = vec![0_u8; width * height * 4];
                    in_rows(&mut pixels, width, count, |band, first_row| {
                        for (row, line) in band.chunks_mut(width * 4).enumerate() {
                            // The row's own index, written into every pixel of
                            // it, so a band that wrote the wrong rows or was
                            // told the wrong offset cannot agree.
                            let mark = u8::try_from((first_row + row) % 251).unwrap_or(0);
                            line.fill(mark);
                        }
                    });

                    for row in 0..height {
                        let mark = u8::try_from(row % 251).unwrap_or(0);
                        let at = row * width * 4;
                        assert!(
                            pixels[at..at + width * 4].iter().all(|&byte| byte == mark),
                            "{width}x{height} in {count} bands: row {row}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_picture_with_no_rows_draws_nothing_and_does_not_panic() {
        // A layer sized to nothing is a configuration mistake, not a crash.
        let mut nothing: Vec<u8> = Vec::new();
        in_rows(&mut nothing, 0, 4, |_, _| {
            panic!("there is nothing to draw")
        });
        in_rows(&mut nothing, 16, 4, |_, _| {
            panic!("there is nothing to draw")
        });
    }

    #[test]
    fn the_band_count_is_at_least_one_whatever_the_machine_says() {
        assert!(bands() >= 1);
        assert!(bands() <= 8);
    }
}
