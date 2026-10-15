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
//! # Why a pool, and not a thread a band
//!
//! The first version of this started a thread per band per frame and divided
//! the rows equally between them. It was **measured on the machine and it was
//! not good enough**: the noise field went from 25,532 us to 13,836 — not the
//! fivefold the development machine had promised — and the service pattern got
//! *worse*, from 4,158 us to 5,870.
//!
//! Two reasons, and one answer to both.
//!
//! **The cores are not equal.** PC05's processor is a hybrid: fast cores and
//! efficiency cores, and `available_parallelism` counts them all the same.
//! Eight equal bands means the bands that land on an efficiency core finish
//! long after the others, and a frame is not done until the slowest band is —
//! so the gain stops at about twice however many cores there are.
//!
//! **Starting a thread is not free.** The service pattern draws in two passes,
//! so it was starting sixteen threads a frame to save four milliseconds of
//! work, and on that machine the threads cost more than the work did.
//!
//! So: **one pool, started once, with more chunks than threads.** A thread that
//! finishes early takes the next chunk, which is what makes unequal cores
//! sort themselves out without anybody measuring them; and no thread is ever
//! started on a frame path. The pool is rayon's, sized here rather than left
//! at its default, because the default is every core in the machine and the
//! decoders need some of them.
//!
//! # The rule a chunk has to keep
//!
//! A chunk draws the rows it was given and nothing else, and is told where
//! those rows sit in the whole picture. The result must not depend on how the
//! rows were divided — otherwise the wall shows seams where the threads met,
//! and a seam that only appears on a machine with a particular number of cores
//! is the worst kind of fault to be sent a photograph of.
//!
//! That rule is what [`in_rows`] exists for: it takes the number of chunks as
//! an argument so a test can force two, three and more against the one-chunk
//! picture. A test that asked the machine would compare one chunk with one
//! chunk on a two-core runner and prove nothing.

use std::sync::OnceLock;

use rayon::prelude::*;
use rayon::{ThreadPool, ThreadPoolBuilder};

/// How many threads draw a picture.
///
/// The same arithmetic the changeover pool uses, and for the same reason: two
/// cores are left alone whatever the machine has, because one of them is the
/// render thread's own caller and the others belong to the decoders. Capped at
/// eight, because a media server is not a benchmark — the decoders, the
/// encoder and the console all want a core, and a generator that drew in two
/// milliseconds by starving a decoder would have moved the fault rather than
/// fixed it.
pub(crate) fn threads() -> usize {
    std::thread::available_parallelism()
        .map_or(1, |cores| cores.get().saturating_sub(2).clamp(1, 8))
}

/// How many chunks one picture is cut into, for that many threads.
///
/// Four times the threads rather than one each. Equal shares are the wrong
/// answer on a processor whose cores are not equal: the frame ends when the
/// slowest share does. With more chunks than threads a core that is finished
/// takes another one, and the fast cores end up doing more of the picture
/// without anybody having to know which cores are fast.
///
/// One frame of 1920x1080 coloured noise, thirty frames, eight threads, on the
/// sixteen-core development machine:
///
/// | chunks | a frame |
/// |---|---|
/// | 1 | 23.32 ms |
/// | 4 | 6.31 |
/// | 8 | 4.57 |
/// | 16 | 4.51 |
/// | **32** | **3.86** |
/// | 64 | 3.86 |
///
/// Flat past thirty-two, which is four a thread. A call that has nothing to
/// draw costs **2.7 microseconds**, so the service pattern's two passes are
/// paid for several hundred times over — the version of this that started
/// threads instead cost that machine more than the drawing did.
const CHUNKS_A_THREAD: usize = 4;

/// The pool every picture is drawn by, started once.
///
/// Named, so the next profile of the renderer says `ovs-draw` rather than a
/// number — reading a profile on that machine is already hard enough.
fn pool() -> Option<&'static ThreadPool> {
    static POOL: OnceLock<Option<ThreadPool>> = OnceLock::new();
    POOL.get_or_init(|| {
        ThreadPoolBuilder::new()
            .num_threads(threads())
            .thread_name(|index| format!("ovs-draw-{index}"))
            .build()
            .map_err(|error| {
                // Not a reason to stop: drawing in one thread is slow, and a
                // wall that is slow is still a wall.
                tracing::warn!(%error, "no drawing pool; generators will draw in one thread");
                error
            })
            .ok()
    })
    .as_ref()
}

/// Draws a picture in horizontal chunks of rows, across the pool.
///
/// `width` is in pixels and every picture here is four bytes to one. `draw` is
/// handed a chunk and the index of its first row in the whole picture.
pub(crate) fn in_bands(
    pixels: &mut [u8],
    width: usize,
    draw: impl Fn(&mut [u8], usize) + Send + Sync,
) {
    in_rows(pixels, width, threads() * CHUNKS_A_THREAD, draw);
}

/// The same, with the number of chunks stated rather than worked out.
///
/// What [`in_bands`] is built from, and what a test uses — see the note on the
/// module about why the count has to be an argument.
pub(crate) fn in_rows(
    pixels: &mut [u8],
    width: usize,
    chunks: usize,
    draw: impl Fn(&mut [u8], usize) + Send + Sync,
) {
    let rows = pixels.len().checked_div(width * 4).unwrap_or(0);
    if rows == 0 {
        // A layer sized to nothing. The drawer is not called at all rather
        // than called with an empty chunk, because "draw these rows" with no
        // rows is a question every drawer would have to answer separately.
        return;
    }

    let chunks = chunks.clamp(1, rows);
    let Some(pool) = pool().filter(|_| chunks > 1) else {
        draw(pixels, 0);
        return;
    };

    let per_chunk = rows.div_ceil(chunks);
    let bytes = per_chunk * width * 4;
    pool.install(|| {
        pixels
            .par_chunks_mut(bytes)
            .enumerate()
            .for_each(|(index, chunk)| draw(chunk, index * per_chunk));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_row_is_drawn_exactly_once_however_it_is_divided() {
        // The property the pictures depend on, checked directly rather than
        // through a generator: each row is visited once, by the chunk that
        // holds it, and told the right index.
        for width in [1_usize, 7, 64] {
            for height in [1_usize, 2, 5, 16, 17] {
                for count in [1_usize, 2, 3, 8, height + 3] {
                    let mut pixels = vec![0_u8; width * height * 4];
                    in_rows(&mut pixels, width, count, |chunk, first_row| {
                        for (row, line) in chunk.chunks_mut(width * 4).enumerate() {
                            // The row's own index, written into every pixel of
                            // it, so a chunk that wrote the wrong rows or was
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
                            "{width}x{height} in {count} chunks: row {row}"
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
    fn the_thread_count_leaves_the_machine_something_to_run_on() {
        assert!(threads() >= 1);
        assert!(threads() <= 8);
    }
}
