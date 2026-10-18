//! The picture on each effect's tile, drawn once and committed.
//!
//! ```text
//! cargo run -p ovs-renderer --bin ovs-effect-pictures
//! cargo run -p ovs-renderer --bin ovs-effect-pictures -- somewhere/else
//! ```
//!
//! # Why an effect's tile is harder than a generator's
//!
//! A generator draws the same thing wherever it is, so its tile is simply what
//! it draws. An effect draws **whatever is under it**, so a tile can only ever
//! be "this effect, on something". Pick the wrong something and the tile is a
//! picture of that clip rather than of the effect.
//!
//! So the something is the house's own reference: the service pattern, at the
//! size a show runs at. It has what an effect has to be judged on — saturated
//! colour, a black-level ramp, hard edges, and lettering — and it is drawn by
//! committed code, so the picture is the same next time.
//!
//! # And at a setting, not at its default
//!
//! Every published effect is neutral at its published defaults: that is the
//! promise the effect list makes, and the renderer proves it by drawing each
//! one and comparing. A tile drawn at the defaults would therefore be the
//! reference picture, once for every effect.
//!
//! Each one is drawn at a setting that shows what it does instead. Those
//! settings are **mine, and they are for the tile alone** — nothing a show ever
//! sees — so they are written out below with the reason each was picked.
//!
//! # On the card, which is where effects live
//!
//! An effect is a fragment shader, so this needs a device. CI has none; the
//! test below skips there, as every other GPU test in this crate does, and the
//! pictures are committed so that a client always has them whatever built it.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use ovs_renderer::backend::{Compositor, FrameRequest};
use ovs_renderer::media::DecodedImage;
use ovs_renderer::pattern::ServicePattern;
use ovs_renderer::scene::{Quad, Scene, SourceRect, TextureRef};
use ovs_renderer::vulkan::{VulkanError, VulkanOffscreenBackend};
use ovs_types::config::slot::{ParameterMap, ParameterValue};
use ovs_types::config::{BlendMode, DisplayMode, EffectInstance, Route};
use ovs_types::geometry::{FrameRate, SizePx};
use ovs_types::id::{LayerId, MediaItemId};

/// The size the reference is drawn at, which is the size a show runs at.
const WALL: SizePx = SizePx::new(1920, 1080);

/// The picture on a tile: the same 320 by 180 a generator's is.
const TILE: SizePx = SizePx::new(320, 180);

/// What the reference pattern's caption calls itself.
///
/// Not `COMPOSITION`, which is what a real surface says: this picture is not a
/// surface and a tile claiming to be one would be a small lie repeated on
/// every tile.
const REFERENCE: &str = "EFFECT";

/// How far into the pattern's own run the reference is taken.
///
/// Two seconds at sixty, which is `GeneratorSource::tick_worth_a_picture` for
/// the same pattern — so an effect's tile and the test pattern's tile are the
/// same moment of the same picture, and the difference between them is the
/// effect and nothing else.
const REFERENCE_SECONDS: f64 = 2.0;

fn main() -> ExitCode {
    let into = std::env::args()
        .nth(1)
        .map_or_else(pictures_directory, PathBuf::from);

    if let Err(error) = std::fs::create_dir_all(&into) {
        eprintln!("{}: {error}", into.display());
        return ExitCode::from(1);
    }

    let mut canvas = match canvas() {
        Ok(canvas) => canvas,
        Err(reason) => {
            eprintln!("{reason}");
            return ExitCode::from(1);
        }
    };

    for manifest in &ovs_types::config::EFFECTS {
        let named = picture_name(manifest.type_id);
        let Some(picture) = picture_of(&mut canvas, manifest.type_id) else {
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
            "{named:<28} {}x{}  {:>6} bytes",
            TILE.width,
            TILE.height,
            png.len()
        );
    }

    println!("into {}", into.display());
    ExitCode::SUCCESS
}

/// What one effect's picture is called.
fn picture_name(type_id: &str) -> String {
    format!("{type_id}.png")
}

/// Where the client keeps them.
fn pictures_directory() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../clients/desktop/assets/effects")
}

/// A device to draw on, with the reference and the mask already on it.
fn canvas() -> Result<Canvas, String> {
    let mode = DisplayMode::new(WALL, FrameRate::FPS_60);
    let mut backend = match VulkanOffscreenBackend::new("effect-pictures", mode) {
        Ok(backend) => backend,
        Err(VulkanError::NoLoader(reason)) => {
            return Err(format!("no Vulkan loader here: {reason}"))
        }
        Err(VulkanError::NoDevice(reason)) => {
            return Err(format!("no usable device here: {reason}"))
        }
        Err(error) => return Err(error.to_string()),
    };

    let reference = MediaItemId::new();
    let mask = MediaItemId::new();
    backend
        .set_media_texture(reference, &reference_picture())
        .map_err(|error| format!("the reference picture: {error}"))?;
    backend
        .set_media_texture(
            mask,
            &mask_picture().ok_or("the mask picture cannot be drawn")?,
        )
        .map_err(|error| format!("the mask picture: {error}"))?;

    Ok(Canvas {
        backend,
        reference,
        mask,
    })
}

/// The device, and what is on it.
struct Canvas {
    backend: VulkanOffscreenBackend,
    /// The picture every effect is shown on.
    reference: MediaItemId,
    /// The picture the mask effect cuts with. Nothing else reads it.
    mask: MediaItemId,
}

/// The service pattern, as every tile's backdrop.
fn reference_picture() -> DecodedImage {
    let mut pattern = ServicePattern::new(WALL, FrameRate::FPS_60, REFERENCE);
    pattern.draw(REFERENCE_SECONDS).clone()
}

/// A white ellipse on nothing, for the one effect that needs a second picture.
///
/// Drawn by the shape generator rather than built here: a mask is a picture
/// from the library, and the nearest thing this build has to one it can make
/// is the generator whose whole job is a shape.
fn mask_picture() -> Option<DecodedImage> {
    use ovs_renderer::media::{FrameSource, GeneratorSource};

    let parameters = ParameterMap::from([
        ("width_px".to_owned(), ParameterValue::Float(1200.0)),
        ("height_px".to_owned(), ParameterValue::Float(900.0)),
    ]);
    let mut source = GeneratorSource::new("ovs.shape", &parameters, WALL)?;
    Some(source.frame(0).ok()?.image.as_ref().clone())
}

/// How many frames the echo's tile is drawn over.
///
/// **Mine, for the tile alone.** An echo is a picture of what moved, so a
/// still reference draws it as the reference: the tile has to be the end of a
/// move. Five jumps of the picture at a third of the wall's width each, every
/// one covering part of the one before: at a sixth of the size, small steps
/// smeared into a blur, and these read as copies stacked behind each other,
/// each one darker than the one in front.
const ECHO_FRAMES: u32 = 5;

/// How wide and tall the moving picture is on the echo's tile, in the wall's
/// own coordinates, where the wall is two across.
const ECHO_PICTURE: f32 = 0.7;

/// How many places the shake's tile shows the picture thrown to.
///
/// **Mine, for the tile alone.** A shake is a movement, and one still of it is
/// only a crop. So its tile throws the picture to five places in turn, one
/// step of the published speed apart, with an echo after it — the device the
/// echo's own tile uses — so the newest place is in front and the ones before
/// it fade behind. The picture is drawn smaller than the wall, as the echo's
/// is, because a trail behind a full-frame picture has nowhere to be seen.
const SHAKE_FRAMES: u32 = 5;

/// When the shake's tile starts, on the clock the passes read.
///
/// **Mine**: any whole number of the shake's steps would do, and this one
/// throws the picture to five places that are far enough apart to read.
const SHAKE_FROM_SECONDS: f32 = 3.0;

/// One effect's tile-sized picture, at the setting that shows what it does.
fn picture_of(canvas: &mut Canvas, type_id: &str) -> Option<DecodedImage> {
    let mut chain = vec![EffectInstance {
        parameters: settings(type_id, canvas.mask),
        ..EffectInstance::new(type_id, 1)
    }];

    // Every effect but two is a function of the frame it is handed, and is
    // drawn once over the whole wall. The echo is drawn as a move, ending on
    // the right, so the tile is the picture with what it just was behind it;
    // the shake is drawn as five places, with an echo behind them.
    let moves = type_id == "ovs.echo";
    let shakes = type_id == "ovs.shake";
    if shakes {
        chain.push(EffectInstance {
            parameters: settings("ovs.echo", canvas.mask),
            ..EffectInstance::new("ovs.echo", 1)
        });
    }
    let frames = if moves {
        ECHO_FRAMES
    } else if shakes {
        SHAKE_FRAMES
    } else {
        1
    };

    let mut passes = Vec::new();
    ovs_renderer::effects::passes_into(&chain, &mut passes);

    let layer = LayerId::new();
    for step in 0..frames {
        let (left, right, bottom, top) = if moves {
            let travelled = step as f32 / (frames - 1) as f32;
            let centre = -0.6 + 1.2 * travelled;
            let half = ECHO_PICTURE / 2.0;
            (centre - half, centre + half, -half, half)
        } else if shakes {
            let half = ECHO_PICTURE / 2.0;
            (-half, half, -half, half)
        } else {
            (-1.0, 1.0, -1.0, 1.0)
        };
        if shakes {
            let hz = ovs_types::config::effects::SHAKE_V1.number(&chain[0].parameters, "hz") as f32;
            canvas
                .backend
                .set_effect_time(SHAKE_FROM_SECONDS + step as f32 / hz);
        }

        let mut scene = Scene::new();
        scene.set_effects(layer, &passes);
        scene.push(Quad {
            left,
            bottom,
            right,
            top,
            colour: [1.0, 1.0, 1.0, 1.0],
            rotation_deg: 0.0,
            anchor: ovs_types::geometry::AnchorPoint::CENTRE,
            layer_id: Some(layer),
            source: SourceRect::WHOLE,
            source_turn: ovs_renderer::SourceTurn::NONE,
            texture: TextureRef::Media(canvas.reference),
            route: Route::A,
            blend: BlendMode::Normal,
        });

        canvas
            .backend
            .compose(FrameRequest { index: 0 }, &scene)
            .ok()?;
    }
    if shakes {
        // Back to the moment every other tile is drawn at, whichever comes
        // next.
        canvas.backend.set_effect_time(0.0);
    }

    // Scaled by the card on the way back rather than averaged here: this is
    // the same path a preview takes, so what a tile shows is what a watcher
    // would see of the same frame.
    let mut pixels = Vec::new();
    canvas.backend.read_scaled(TILE, &mut pixels).ok()?;
    DecodedImage::new(pixels, TILE)
}

/// What each effect is set to for its own tile, and why.
///
/// **None of this reaches a show.** A manifest's defaults are neutral by
/// promise; these are the settings that make the difference visible in a
/// picture 320 pixels wide, picked by eye and kept here so that the next
/// person can see what was picked rather than infer it from a PNG.
fn settings(type_id: &str, mask: MediaItemId) -> ParameterMap {
    let number = |key: &str, value: f64| (key.to_owned(), ParameterValue::Float(value));
    let flag = |key: &str, value: bool| (key.to_owned(), ParameterValue::Bool(value));
    let choice =
        |key: &str, value: &str| (key.to_owned(), ParameterValue::Choice(value.to_owned()));

    match type_id {
        // **The colour taken out and the contrast pushed**, rather than a
        // grade in the same direction the test card already goes: a more
        // saturated test card looks like a test card, and the tile has to say
        // "this one changes the grade".
        // And its white point, hard over to the warm end, so the tile shows the
        // whole of what it does.
        "ovs.colour_correct" => ParameterMap::from([
            number("contrast", 1.6),
            number("saturation", 0.3),
            number("temperature_k", 2600.0),
        ]),
        // The house orange, fully mixed, brightness kept: the tile has to show
        // a tint rather than a wash.
        "ovs.tint" => ParameterMap::from([
            (
                "colour".to_owned(),
                ParameterValue::Color([1.0, 0.6, 0.0, 1.0]),
            ),
            number("amount", 1.0),
            flag("preserve_luma", true),
        ]),
        // Six blocks across the tile: fewer and it is a flat field, more and it
        // is the picture again.
        "ovs.pixelate" => ParameterMap::from([number("block_px", 56.0)]),
        // Enough to soften the lettering without losing the bars.
        "ovs.blur" => ParameterMap::from([number("radius_px", 28.0), number("amount", 1.0)]),
        "ovs.invert" => ParameterMap::from([flag("on", true)]),
        // All the way in, in a colour the pattern does not use, and three
        // pixels wide so the outlines of the bars and the lettering survive
        // being shrunk to a sixth: the tile has to say "only the outlines" at
        // ninety-two pixels across, where a one-pixel line is gone.
        "ovs.edges" => {
            let colour = (
                "colour".to_owned(),
                ParameterValue::Color([0.2, 0.9, 1.0, 1.0]),
            );
            ParameterMap::from([
                number("amount", 1.0),
                number("threshold", 0.12),
                number("width_px", 3.0),
                colour,
            ])
        }
        // A fifth of a second, which at sixty keeps about five eighths of a
        // copy each frame: the four behind the newest come out at roughly
        // sixty, forty, twenty-five and fifteen per cent, so the fade is the
        // thing the eye reads. Longer and they are all nearly as bright as the
        // newest, which says "copies" and not "fading".
        "ovs.echo" => ParameterMap::from([number("length_s", 0.2), choice("mode", "behind")]),
        // All the way, at the published speed: see `SHAKE_FRAMES` for how a
        // movement is drawn as a still.
        "ovs.shake" => ParameterMap::from([number("amount", 1.0)]),
        // A wide frame off the sides and a narrow one off the top, so the tile
        // shows both that it cuts and that the cut is transparent.
        "ovs.crop" => ParameterMap::from([
            number("left_px", 260.0),
            number("right_px", 260.0),
            number("top_px", 120.0),
            number("bottom_px", 120.0),
        ]),
        // Hard on, so the tear and the noise are both in the frame.
        "ovs.tv_glitch" => ParameterMap::from([number("amount", 0.85)]),
        // The ellipse, multiplied in: the shape of the mask is the point.
        "ovs.mask" => ParameterMap::from([
            ("image".to_owned(), ParameterValue::Text(mask.to_string())),
            choice("mode", "multiply"),
            number("amount", 1.0),
        ]),
        // One axis only. Both at once is a kaleidoscope and reads as a
        // different effect at this size.
        "ovs.mirror" => ParameterMap::from([choice("horizontal", "left")]),
        // Magenta, because it is the one hue the pattern has least of.
        "ovs.colorize" => ParameterMap::from([
            number("hue", 310.0),
            number("saturation", 1.0),
            number("amount", 1.0),
        ]),
        // Red up and blue down: a cast that cannot be mistaken for a grade.
        "ovs.rgb_correct" => ParameterMap::from([number("gain_r", 1.8), number("gain_b", 0.45)]),
        // Far enough apart to see three edges where there was one. At a
        // sixth of the size, twenty-eight pixels of separation came back as
        // four and read as a soft edge rather than as a split.
        "ovs.rgb_split" => {
            ParameterMap::from([number("offset_px", 72.0), number("angle_deg", 0.0)])
        }
        // An effect nobody has written a setting for yet draws at its defaults,
        // which is honest: its tile is the reference picture, and that says
        // "this one is neutral until you touch it".
        _ => ParameterMap::new(),
    }
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
    const RUN_IT: &str = "run `cargo run -p ovs-renderer --bin ovs-effect-pictures`";

    #[test]
    fn every_published_effect_has_a_committed_picture() {
        // The half that runs everywhere, CI included: a client resolves a tile
        // by name, so a published effect with no file is a tile that falls back
        // to its own name for ever without anybody noticing.
        for manifest in &ovs_types::config::EFFECTS {
            let named = picture_name(manifest.type_id);
            let path = pictures_directory().join(&named);
            let opened = image::open(&path)
                .unwrap_or_else(|error| panic!("{}: {error} — {RUN_IT}", path.display()));
            assert_eq!(
                (opened.width(), opened.height()),
                (TILE.width, TILE.height),
                "{named} is committed at another size — {RUN_IT}"
            );
        }
    }

    #[test]
    fn nothing_is_committed_that_no_effect_asks_for() {
        let wanted: std::collections::BTreeSet<String> = ovs_types::config::EFFECTS
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
    fn each_picture_is_the_effect_rather_than_the_reference() {
        // **The half that needs a card**, so it stands aside on a machine
        // without one, as every GPU test in this crate does. What it holds
        // down is the thing a committed picture cannot say for itself: that
        // each tile is its own effect at the setting written above, and not
        // the backdrop they are all drawn on.
        // No `one_gpu_at_a_time` here: that lock lives in the library's own
        // tests and is private to them, and it would not help anyway — a bin's
        // tests are a separate process, so the two could only be serialised by
        // something outside both. One more small offscreen device on a card
        // with eight gigabytes is a cost this can carry.
        let Ok(mut canvas) = canvas() else {
            return;
        };

        let mut seen: Vec<(String, Vec<u8>)> = Vec::new();
        for manifest in &ovs_types::config::EFFECTS {
            let named = picture_name(manifest.type_id);
            let drawn = picture_of(&mut canvas, manifest.type_id)
                .unwrap_or_else(|| panic!("{named} cannot be drawn"));
            let committed = image::open(pictures_directory().join(&named))
                .unwrap_or_else(|error| panic!("{named}: {error} — {RUN_IT}"))
                .to_rgba8()
                .into_raw();

            // Two levels, for the same reason the generators' pictures allow
            // it: a card's scaler and a shader's arithmetic are not obliged to
            // agree to the last bit between drivers.
            let worst = committed
                .iter()
                .zip(drawn.pixels())
                .map(|(committed, drawn)| committed.abs_diff(*drawn))
                .max()
                .unwrap_or(0);
            assert!(
                worst <= 2,
                "{named} draws something else than the committed picture, out by {worst} — {RUN_IT}"
            );

            seen.push((named, drawn.pixels().to_vec()));
        }

        // And no two effects are the same picture, which is what would happen
        // if a setting above stopped reaching its effect.
        for (at, (named, pixels)) in seen.iter().enumerate() {
            for (other_name, other) in seen.iter().skip(at + 1) {
                assert_ne!(
                    pixels, other,
                    "{named} and {other_name} are the same picture"
                );
            }
        }
    }
}
