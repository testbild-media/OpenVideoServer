//! The picture on each generator's tile, drawn once and committed.
//!
//! ```text
//! cargo run -p ovs-renderer --bin ovs-generator-pictures
//! cargo run -p ovs-renderer --bin ovs-generator-pictures -- somewhere/else
//! ```
//!
//! # Why files rather than a request
//!
//! A tile wants a picture of what the generator is, so that an operator can
//! see a list of them without reading it. There are three ways to get one, and
//! two of them are wrong.
//!
//! Drawing it in the client is wrong because what a gradient looks like is the
//! renderer's business: a second implementation in another language, kept in
//! step by hand, would eventually show a picture of something the wall does not
//! draw — and the first time they disagreed, the operator would be the one who
//! found out.
//!
//! Asking the server for it is wrong because it is a show machine. Ten
//! generators at a composition size, drawn on the processor because somebody
//! opened a list of names, is work a wall pays for to tell the operator
//! something that cannot change between runs. The owner's words, 2026-10-15:
//! "keine Resourcen verschwenden, deshalb statische Vorschaubilder".
//!
//! So: the renderer's own picture, drawn here, committed beside the client, and
//! free at the moment somebody looks at it.
//!
//! # What is in them
//!
//! Each generator's own output from its published defaults, with nothing added
//! and nothing composed on top — except two, which are marks instead, because
//! their own output is a flat white field and two identical white tiles tell
//! an operator nothing. See [`mark_of`].
//!
//! **Drawn at the size a show runs at and averaged down**, rather than drawn
//! small. Half of these generators are sized in pixels — a shape is 256 across,
//! text is 108 tall, a grid is ruled every 64 — and on a canvas 320 wide each
//! of those would be one object filling the frame, which is a picture of the
//! parameter and not of the generator. At 1920 they are what they are, and
//! shrinking keeps them that: the tile is a miniature of the wall.
//!
//! Which frame comes from [`GeneratorSource::tick_worth_a_picture`], which
//! explains itself.
//!
//! # One file a published generator
//!
//! `ovs.noise.png`: the type and nothing else, because there is one of each
//! (owner, 2026-10-18: "Es gibt kein Invert V2.. nirgendwo").
//!
//! The retired types are here too. A source list offers neither
//! the clock nor the countdown (the owner, 2026-10-12: "Was sowieso dann weg
//! kann und mir keinen Mehrwert bringt: Generator Countdown, Generator
//! Clock"), and a show saved before that still has them on a pad, where they
//! still draw. Two files, and nothing in the browser.
//!
//! A client resolves a picture by name, so a generator with none shows what it
//! showed before these existed rather than a wrong picture.
//!
//! # Keeping them true
//!
//! The test at the bottom draws every generator the catalogue offers and
//! compares it with the committed file. Change what a generator draws and that
//! test says so, and says to run this. It is the only thing between a
//! committed picture and a tile that lies about what a pad will do.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use ovs_renderer::media::{DecodedImage, FrameSource, GeneratorSource};
use ovs_types::config::slot::ParameterMap;
use ovs_types::geometry::SizePx;

/// The size the pictures are drawn at, which is the size a show runs at.
const WALL: SizePx = SizePx::new(1920, 1080);

/// How much the drawn picture is shrunk by.
///
/// 1920 and 1080 are both whole multiples of it, so every pixel of the tile is
/// the average of exactly six by six of the wall and no resampling filter has
/// to be chosen, tuned or explained.
const SHRINK: u32 = 6;

/// The picture on a tile: 320 by 180.
///
/// A tile is between 92 and 130 pixels across and may be on a screen with two
/// device pixels to each of those, so this is oversampled rather than exact —
/// and the same file is the picture on a pad, which is smaller again.
const TILE: SizePx = SizePx::new(WALL.width / SHRINK, WALL.height / SHRINK);

/// The seed every picture is drawn with.
///
/// Any fixed number. It decides which field of noise this is, one is as
/// representative as another, and what matters is that it is the same one when
/// the test draws it again.
const SEED: u64 = 1;

fn main() -> ExitCode {
    let into = std::env::args()
        .nth(1)
        .map_or_else(pictures_directory, PathBuf::from);

    if let Err(error) = std::fs::create_dir_all(&into) {
        eprintln!("{}: {error}", into.display());
        return ExitCode::from(1);
    }

    for manifest in &ovs_types::config::generators::PUBLISHED {
        let named = picture_name(manifest.type_id);
        let Some(picture) = picture_of(manifest.type_id) else {
            eprintln!("{named}: published and cannot be drawn");
            return ExitCode::from(1);
        };
        let Some(png) = encoded(&picture) else {
            eprintln!("{named}: could not be encoded");
            return ExitCode::from(1);
        };

        let path = into.join(&named);
        if let Err(error) = std::fs::write(&path, &png) {
            eprintln!("{}: {error}", path.display());
            return ExitCode::from(1);
        }

        println!(
            "{named:<26} {}x{}  {:>6} bytes",
            TILE.width,
            TILE.height,
            png.len()
        );
    }

    println!("into {}", into.display());
    ExitCode::SUCCESS
}

/// What one generator's picture is called.
fn picture_name(type_id: &str) -> String {
    format!("{type_id}.png")
}

/// Where the client keeps them.
///
/// From this crate's own place in the tree, so the tool can be run from
/// anywhere and still write where the client will look.
fn pictures_directory() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../clients/desktop/assets/generators")
}

/// One generator's tile-sized picture, from its published defaults.
///
/// `None` for a type this build does not publish.
///
/// Two of them are a mark instead of their own output: see [`mark_of`].
fn picture_of(type_id: &str) -> Option<DecodedImage> {
    if let Some(mark) = mark_of(type_id) {
        return shrunk(&mark);
    }
    let mut source = GeneratorSource::new(type_id, &ParameterMap::new(), WALL)?.with_seed(SEED);
    let frame = source.frame(source.tick_worth_a_picture()).ok()?;
    shrunk(&frame.image)
}

/// The two tiles that carry a mark rather than the generator's own picture.
///
/// **The owner's decision, 2026-10-16**: "Solid color bitte keine weiße Fläche
/// nehmen sondern rote Fläche mit einem Füll Symbol. Bei Strobe eine weiße
/// Fläche mit einem Blitz Symbol."
///
/// Every other picture here is what the generator draws, and these two were as
/// well — which is exactly the problem. A solid colour at its published default
/// is a white field and a strobe's lit frame is a white field, so the two tiles
/// were the same picture and neither said anything. A mark is a drawing rather
/// than a rendering and the tile is the one place that is right: nothing on the
/// wall changes, and a pad holding one still shows this, because what an
/// operator needs from a pad is to know which of the two it is.
///
/// Drawn at the wall's size and shrunk like everything else, so the edges are
/// averaged down rather than drawn with a smoothing rule of their own.
fn mark_of(type_id: &str) -> Option<DecodedImage> {
    // A red that reads as red at ninety pixels without being the signal red
    // the interface uses for a fault, and a white field for the strobe because
    // that is the colour it flashes.
    let (field, ink): ([u8; 4], [u8; 4]) = match type_id {
        "ovs.solid_colour" => ([224, 48, 48, 255], [255, 255, 255, 255]),
        "ovs.strobe" => ([255, 255, 255, 255], [24, 24, 28, 255]),
        _ => return None,
    };

    let mut pixels = vec![0u8; (WALL.width as usize) * (WALL.height as usize) * 4];
    for pixel in pixels.as_chunks_mut::<4>().0 {
        *pixel = field;
    }

    let centre = [WALL.width as f32 / 2.0, WALL.height as f32 / 2.0];
    if type_id == "ovs.strobe" {
        let bolt = f32::from(WALL.height as u16) * 0.62;
        fill(&mut pixels, &placed(BOLT, centre, bolt), ink);
    } else {
        let bucket = f32::from(WALL.height as u16) * 0.54;
        for shape in [BUCKET_BODY, BUCKET_RIM, BUCKET_HANDLE, DROP] {
            fill(&mut pixels, &placed(shape, centre, bucket), ink);
        }
    }

    DecodedImage::new(pixels, WALL)
}

/// A lightning bolt, in a box one unit across centred on nothing.
///
/// The shape everything that flashes is drawn as, from a fire exit to a weather
/// map: two strokes offset about a kink. Written out rather than computed so
/// that what it looks like is readable here.
const BOLT: &[[f32; 2]] = &[
    [0.12, -0.52],
    [-0.40, 0.04],
    [-0.08, 0.04],
    [-0.24, 0.52],
    [0.36, -0.08],
    [0.02, -0.08],
];

/// The bucket's body: a trapezoid, narrower at the bottom, tipped to the right.
const BUCKET_BODY: &[[f32; 2]] = &[[-0.40, -0.26], [0.30, -0.42], [0.44, 0.22], [-0.14, 0.38]];

/// Its mouth, which is what makes the body read as a bucket and not a tub.
const BUCKET_RIM: &[[f32; 2]] = &[[-0.44, -0.30], [0.32, -0.48], [0.36, -0.33], [-0.40, -0.15]];

/// The handle, as a band over the mouth.
const BUCKET_HANDLE: &[[f32; 2]] = &[
    [-0.34, -0.34],
    [-0.20, -0.56],
    [0.16, -0.64],
    [0.30, -0.50],
    [0.22, -0.47],
    [0.12, -0.56],
    [-0.14, -0.50],
    [-0.24, -0.33],
];

/// The drop coming out of it, which is the half that says *fill*.
///
/// A teardrop: a point above and a round bottom, as a ring of points so that
/// one polygon routine draws everything here.
const DROP: &[[f32; 2]] = &[
    [0.56, 0.08],
    [0.42, 0.34],
    [0.44, 0.44],
    [0.50, 0.50],
    [0.62, 0.50],
    [0.68, 0.44],
    [0.70, 0.34],
];

/// Puts a shape of unit size where it belongs on the canvas.
fn placed(shape: &[[f32; 2]], centre: [f32; 2], size: f32) -> Vec<[f32; 2]> {
    shape
        .iter()
        .map(|point| [centre[0] + point[0] * size, centre[1] + point[1] * size])
        .collect()
}

/// Fills a polygon, hard-edged, by crossings on each row's centre line.
///
/// No smoothing: this draws at 1920 by 1080 and the picture is averaged
/// six-to-one afterwards, which is thirty-six samples a pixel and a better
/// edge than a rule here would give.
fn fill(pixels: &mut [u8], points: &[[f32; 2]], colour: [u8; 4]) {
    let top = points.iter().fold(f32::MAX, |low, p| low.min(p[1]));
    let bottom = points.iter().fold(f32::MIN, |high, p| high.max(p[1]));
    let first = top.max(0.0) as u32;
    let last = (bottom.ceil().max(0.0) as u32).min(WALL.height);

    let mut crossings: Vec<f32> = Vec::with_capacity(8);
    for row in first..last {
        let line = row as f32 + 0.5;
        crossings.clear();
        for (at, from) in points.iter().enumerate() {
            let to = points[(at + 1) % points.len()];
            if (from[1] <= line) != (to[1] <= line) {
                let along = (line - from[1]) / (to[1] - from[1]);
                crossings.push(from[0] + along * (to[0] - from[0]));
            }
        }
        crossings.sort_by(|one, other| one.partial_cmp(other).unwrap_or(Ordering::Equal));

        for span in crossings.as_chunks::<2>().0 {
            let from = (span[0].max(0.0) as u32).min(WALL.width);
            let to = (span[1].ceil().max(0.0) as u32).min(WALL.width);
            for column in from..to {
                let at = ((row * WALL.width + column) * 4) as usize;
                if let Some(pixel) = pixels.get_mut(at..at + 4) {
                    pixel.copy_from_slice(&colour);
                }
            }
        }
    }
}

/// Averages each six-by-six block of a picture into one pixel.
///
/// # Why the alpha is weighted
///
/// Two of these generators draw on a transparent background, and a flat
/// average of straight alpha darkens every edge: the pixels outside the shape
/// are transparent *black*, and averaging their colour in is averaging in
/// black. Weighting each pixel's colour by its own alpha — a premultiplied
/// average, undone afterwards — gives the edge the shape's colour at the
/// shape's coverage, which is what the compositor does with it on the wall.
///
/// `None` if the picture is not a whole number of blocks across, or is in the
/// other channel order: both are callers' mistakes rather than situations.
fn shrunk(image: &DecodedImage) -> Option<DecodedImage> {
    let from = image.size();
    if !from.width.is_multiple_of(SHRINK) || !from.height.is_multiple_of(SHRINK) || image.is_bgra()
    {
        return None;
    }

    let to = SizePx::new(from.width / SHRINK, from.height / SHRINK);
    let source = image.pixels();
    let block = (SHRINK * SHRINK) as f32;
    let mut pixels = Vec::with_capacity((to.width as usize) * (to.height as usize) * 4);

    for row in 0..to.height {
        for column in 0..to.width {
            let mut weighted = [0.0f32; 3];
            let mut alpha = 0.0f32;
            for y in 0..SHRINK {
                for x in 0..SHRINK {
                    let at = (((row * SHRINK + y) * from.width + column * SHRINK + x) * 4) as usize;
                    let taken = source.get(at..at + 4)?;
                    let coverage = f32::from(taken[3]);
                    for (channel, sum) in weighted.iter_mut().enumerate() {
                        *sum += f32::from(taken[channel]) * coverage;
                    }
                    alpha += coverage;
                }
            }

            for sum in weighted {
                pixels.push(rounded(if alpha > 0.0 { sum / alpha } else { 0.0 }));
            }
            pixels.push(rounded(alpha / block));
        }
    }

    DecodedImage::new(pixels, to)
}

/// The nearest byte to an average.
fn rounded(value: f32) -> u8 {
    value.clamp(0.0, 255.0).round() as u8
}

/// A picture as the bytes of a PNG.
fn encoded(picture: &DecodedImage) -> Option<Vec<u8>> {
    let size = picture.size();
    let buffer = image::RgbaImage::from_raw(size.width, size.height, picture.pixels().to_vec())?;
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgba8(buffer)
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .ok()?;
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What to do when a picture is not what this build draws.
    const RUN_IT: &str = "run `cargo run -p ovs-renderer --bin ovs-generator-pictures`";

    /// The committed picture of one published manifest, as bytes of RGBA.
    fn committed(named: &str) -> Vec<u8> {
        let path = pictures_directory().join(named);
        let opened = image::open(&path)
            .unwrap_or_else(|error| panic!("{}: {error} — {RUN_IT}", path.display()));
        opened.to_rgba8().into_raw()
    }

    #[test]
    fn every_published_generator_has_a_committed_picture_of_what_it_draws() {
        // The whole guard. A generator whose drawing changes, or a new one with
        // no picture at all, is caught here rather than by an operator dragging
        // a tile onto a pad and getting something else.
        for manifest in &ovs_types::config::generators::PUBLISHED {
            let named = picture_name(manifest.type_id);
            let drawn =
                picture_of(manifest.type_id).unwrap_or_else(|| panic!("{named} cannot be drawn"));
            assert_eq!(drawn.size(), TILE, "{named}");

            let committed = committed(&named);
            assert_eq!(
                committed.len(),
                drawn.pixels().len(),
                "{named} is committed at another size — {RUN_IT}"
            );

            // Two levels rather than nothing, because these are floating-point
            // pictures: a generator reaches a pixel through sines, square roots
            // and a lattice, and the last bit of a channel is not a promise two
            // machines have to keep. A picture that is actually of something
            // else is out by far more than this.
            let worst = committed
                .iter()
                .zip(drawn.pixels())
                .map(|(committed, drawn)| committed.abs_diff(*drawn))
                .max()
                .unwrap_or(0);
            assert!(
                worst <= 2,
                "{named} draws a different picture than the one committed, out by {worst} — {RUN_IT}"
            );
        }
    }

    #[test]
    fn nothing_is_committed_that_no_manifest_asks_for() {
        // The other direction, and the one that catches a generator being
        // removed: its picture stays in the directory, nothing draws it, and
        // the guard above never looks at it. A client would not ask for it
        // either — this is about not shipping a picture of something that no
        // longer exists.
        let wanted: std::collections::BTreeSet<String> = ovs_types::config::generators::PUBLISHED
            .iter()
            .map(|manifest| picture_name(manifest.type_id))
            .collect();

        let directory = pictures_directory();
        let listed = std::fs::read_dir(&directory)
            .unwrap_or_else(|error| panic!("{}: {error}", directory.display()));
        for entry in listed {
            let found = entry.expect("a directory entry").file_name();
            let found = found.to_string_lossy().to_string();
            assert!(
                wanted.contains(&found),
                "{found} is committed and nothing publishes it — delete it"
            );
        }
    }

    #[test]
    fn no_picture_is_one_flat_field() {
        // A placeholder that is one flat colour says nothing, and two of these
        // were exactly that until the marks were drawn: a solid colour at its
        // default is a white field, and so is a strobe's lit frame, so the two
        // tiles were the same picture. Every one of them carries something now.
        for manifest in &ovs_types::config::generators::PUBLISHED {
            let picture = picture_of(manifest.type_id)
                .unwrap_or_else(|| panic!("{} cannot be drawn", manifest.type_id));

            let colours: std::collections::BTreeSet<[u8; 4]> = picture
                .pixels()
                .as_chunks::<4>()
                .0
                .iter()
                .copied()
                .collect();
            assert!(
                colours.len() > 1,
                "{} is one flat colour, which says nothing on a tile ({} colours)",
                manifest.type_id,
                colours.len()
            );
        }
    }

    #[test]
    fn shrinking_takes_the_colour_of_a_corner_rather_than_fading_it() {
        // The premultiplied average, in the one case where getting it wrong is
        // visible: a block that is one quarter opaque white and three quarters
        // nothing. Weighted, that is white at a quarter coverage; flat, it is
        // dark grey at a quarter coverage, and every edge in the shape and text
        // pictures would have a dirty rim.
        let mut pixels = vec![0u8; (SHRINK * SHRINK * 4) as usize];
        for pixel in pixels
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .take((SHRINK * SHRINK / 4) as usize)
        {
            *pixel = [255, 255, 255, 255];
        }

        let whole =
            DecodedImage::new(pixels, SizePx::new(SHRINK, SHRINK)).expect("a square of pixels");
        let one = shrunk(&whole).expect("one pixel");

        assert_eq!(one.size(), SizePx::new(1, 1));
        assert_eq!(one.pixels(), &[255, 255, 255, 64]);
    }

    #[test]
    fn the_two_marked_tiles_are_told_apart_at_a_glance() {
        // The whole reason they are marks (owner, 2026-10-16). Whatever else
        // happens to them, these two must not go back to being the same white
        // field: the fields differ, and each carries ink over it.
        let solid = picture_of("ovs.solid_colour").expect("drawn");
        let strobe = picture_of("ovs.strobe").expect("drawn");

        assert_eq!(solid.pixel(2, 2), [224, 48, 48, 255], "a red field");
        assert_eq!(strobe.pixel(2, 2), [255, 255, 255, 255], "and a white one");

        // And a mark on each, which the middle column runs through. Measured as
        // distance from the field rather than as darkness, because one mark is
        // white on red and the other is near-black on white.
        for (named, picture) in [("solid colour", &solid), ("strobe", &strobe)] {
            let field = picture.pixel(2, 2);
            let furthest = (0..TILE.height)
                .map(|row| picture.pixel(TILE.width / 2, row))
                .map(|pixel| {
                    (0..3)
                        .map(|channel| field[channel].abs_diff(pixel[channel]))
                        .max()
                        .unwrap_or(0)
                })
                .max()
                .unwrap_or(0);
            assert!(
                furthest > 60,
                "{named} carries no mark down its middle: {furthest} away from its field"
            );
        }
    }
}
