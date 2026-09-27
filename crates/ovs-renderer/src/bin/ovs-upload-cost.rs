//! What it costs to put a decoded frame on the card, and to draw it.
//!
//! ```text
//! ovs-upload-cost                    one layer, 1080p, five hundred frames
//! ovs-upload-cost --layers 3         three, as the reference workload is
//! ovs-upload-cost --layers 3 --size 3840x2160
//! ovs-upload-cost --layers 3 --readback      with somebody watching a preview
//! ovs-upload-cost --layers 3 --upload-once   what a generator costs: no new frames
//! ovs-upload-cost --layers 3 --effects ovs.blur,ovs.tint    with a chain on each
//! ```
//!
//! # Why this exists
//!
//! The wall was measured running at forty frames a second with three clips on
//! stage and fifty-nine with three generators, at the same nineteen per cent of
//! the machine, and the decoders were found to be the *opposite* way round: the
//! clip three decoders keep up with is the one that ruins the wall, and the one
//! they cannot keep up with is the one it survives (R2).
//!
//! That points at the upload — a decoder that succeeds is a decoder that asks
//! for one — but pointing is not measuring. This measures it, by doing exactly
//! what the render thread does and nothing else: take a decoded picture per
//! layer, hand it to the compositor, draw the composition, and time the two
//! separately.
//!
//! No server, no display, no decoding, no clips. Just a Vulkan device, some
//! pixels, and a clock. That is what makes it runnable on the reference server,
//! where the question has to be answered and where none of the rest exists.
//!
//! # What is being measured, exactly
//!
//! The pictures are built once and handed over again every frame. That is not a
//! shortcut: the compositor stages whatever it is given without looking at it,
//! so the same picture costs exactly what a different one costs, and building a
//! new one inside the loop would time the building instead. Nothing is
//! allocated in the timed section.
//!
//! `upload` is queueing every layer's picture and flushing them: the copy into
//! mapped memory, on this thread, which is the thing under suspicion. `compose`
//! is the draw. Together they are two of the three figures the renderer now
//! reports as `FrameTimings`, and a frame at sixty has 16 667 microseconds for
//! all of it plus presenting.

#[cfg(not(unix))]
fn main() {
    eprintln!("ovs-upload-cost needs a Vulkan device, and is only built on unix");
    std::process::exit(2);
}

#[cfg(unix)]
fn main() -> std::process::ExitCode {
    unix::run()
}

#[cfg(unix)]
mod unix {
    use std::process::ExitCode;
    use std::time::{Duration, Instant};

    use ovs_renderer::backend::{Compositor, FrameRequest};
    use ovs_renderer::media::DecodedImage;
    use ovs_renderer::scene::{Quad, Scene, SourceRect, TextureKey, TextureRef};
    use ovs_renderer::vulkan::{VulkanError, VulkanOffscreenBackend};
    use ovs_types::config::{BlendMode, DisplayMode, EffectInstance, Route};
    use ovs_types::geometry::{FrameRate, SizePx};
    use ovs_types::id::LayerId;

    const USAGE: &str = "usage: ovs-upload-cost [--layers N] [--frames N] [--size WxH] \
[--readback] [--upload-once] [--effects TYPE[,TYPE...]]";

    pub fn run() -> ExitCode {
        let mut layers = 1usize;
        let mut frames = 500u32;
        let mut size = SizePx::new(1920, 1080);
        let mut readback = false;
        let mut upload_once = false;
        let mut chain: Vec<EffectInstance> = Vec::new();

        let mut args = std::env::args().skip(1);
        while let Some(argument) = args.next() {
            match argument.as_str() {
                "--layers" => layers = parse(args.next().as_deref(), "--layers"),
                "--frames" => frames = parse(args.next().as_deref(), "--frames"),
                "--size" => {
                    let Some(value) = args.next() else {
                        eprintln!("--size needs a value");
                        return ExitCode::from(2);
                    };
                    let Some(parsed) = parse_size(&value) else {
                        eprintln!("--size wants something like 1920x1080, not {value}");
                        return ExitCode::from(2);
                    };
                    size = parsed;
                }
                "--effects" => {
                    let Some(value) = args.next() else {
                        eprintln!("--effects needs a value");
                        return ExitCode::from(2);
                    };
                    match effects_of(&value) {
                        Ok(built) => chain = built,
                        Err(unknown) => {
                            eprintln!("no published effect is called {unknown}");
                            eprintln!("published: {}", published_names().join(", "));
                            return ExitCode::from(2);
                        }
                    }
                }
                "--readback" => readback = true,
                "--upload-once" => upload_once = true,
                "--help" | "-h" => {
                    println!("{USAGE}");
                    return ExitCode::SUCCESS;
                }
                other => {
                    eprintln!("unknown option {other}\n{USAGE}");
                    return ExitCode::from(2);
                }
            }
        }
        let layers = layers.max(1);

        let mode = DisplayMode::new(size, FrameRate::FPS_60);
        let mut canvas = match VulkanOffscreenBackend::new("upload-cost", mode) {
            Ok(canvas) => canvas,
            Err(VulkanError::NoLoader(reason)) => {
                eprintln!("no Vulkan loader here: {reason}");
                return ExitCode::from(1);
            }
            Err(VulkanError::NoDevice(reason)) => {
                eprintln!("no usable device here: {reason}");
                return ExitCode::from(1);
            }
            Err(error) => {
                eprintln!("{error}");
                return ExitCode::from(1);
            }
        };

        // Off unless asked for. A show nobody is watching does not copy its
        // composition back off the card, and a measurement that always did
        // would be measuring the preview rather than the show.
        canvas.set_readback(readback);

        println!(
            "{layers} layer(s) of {}x{}, {frames} frames",
            size.width, size.height
        );
        println!(
            "{}",
            if upload_once {
                "uploading once, as a generator does: the composition has nothing new to copy"
            } else {
                "uploading every layer every frame, as a playing clip does"
            }
        );
        if chain.is_empty() {
            println!("no effects, so every layer goes straight into the composition");
        } else {
            // Named rather than counted, because the passes do not cost the
            // same: a blur is two passes over the whole layer and a tint is
            // arithmetic on the pixel that was already being written.
            println!(
                "a chain of {} on every layer: {}",
                chain.len(),
                chain
                    .iter()
                    .map(|instance| instance.type_id.as_str())
                    .collect::<Vec<_>>()
                    .join(" then ")
            );
        }
        println!(
            "{}",
            if readback {
                "with the composition copied back, as it is when somebody is watching"
            } else {
                "with nothing watching, so the composition stays on the card"
            }
        );
        // Which card, because this machine has two and a figure from the wrong
        // one is worse than no figure — and what its bus is doing, because a
        // slow copy on a link running below its capability is a fact about the
        // machine and not about the renderer. The test machine was found
        // negotiated at 2.5 GT/s against a card that supports 16, which is most
        // of why a copy there runs at under a gigabyte a second.
        println!("on {}", canvas.context().summary().name);
        if let Some(link) = pcie_link() {
            println!("{link}");
        }
        println!(
            "a frame at sixty is {} us",
            FrameRate::FPS_60.frame_duration_nanos().unwrap_or(0) / 1_000
        );
        println!();

        // One picture per layer, built here and handed over every frame. The
        // compositor stages what it is given without comparing it to what it
        // holds, so re-using a picture costs what a fresh one costs — and
        // building a fresh one inside the loop would put an eight-megabyte
        // allocation into the figure this is trying to read.
        let bytes = size.width as usize * size.height as usize * 4;
        let mut pictures: Vec<(LayerId, DecodedImage)> = Vec::with_capacity(layers);
        for index in 0..layers {
            let mut pixels = vec![0u8; bytes];
            // Something other than black, and different per layer, so a driver
            // cannot decide the whole thing is one colour.
            for (offset, byte) in pixels.iter_mut().enumerate() {
                *byte = ((offset / 4 + index * 37) % 251) as u8;
            }
            let Some(image) = DecodedImage::new(pixels, size) else {
                eprintln!(
                    "a picture of {}x{} is not a picture",
                    size.width, size.height
                );
                return ExitCode::from(1);
            };
            pictures.push((LayerId::new(), image));
        }

        let scene = scene_of(pictures.iter().map(|(layer_id, _)| *layer_id), &chain);

        // What the render thread hands the compositor: a key and a picture per
        // layer, the pictures behind an `Arc` because several threads read them.
        let staged: Vec<(TextureKey, std::sync::Arc<DecodedImage>)> = pictures
            .into_iter()
            .map(|(layer_id, image)| (TextureKey::Layer(layer_id), std::sync::Arc::new(image)))
            .collect();

        let mut upload = Duration::ZERO;
        let mut compose = Duration::ZERO;
        let mut worst_upload = Duration::ZERO;
        let mut worst_compose = Duration::ZERO;

        for frame in 0..frames {
            // Uploading only on the first frame is what a generator does: it
            // is drawn once and the composition has nothing to copy after that.
            // The difference between the two runs is the whole question, because
            // the card-side copy of a changed texture is recorded into the
            // command buffer that `compose` submits and then waits for.
            let began = Instant::now();
            if !upload_once || frame == 0 {
                // All of them at once, which is what the render thread does:
                // the copies are independent and one core does not fill the bus.
                for (key, image) in &staged {
                    if let Err(error) = canvas.queue_texture(*key, std::sync::Arc::clone(image)) {
                        eprintln!("{error}");
                        return ExitCode::from(1);
                    }
                }
                if let Err(error) = canvas.flush_textures() {
                    eprintln!("{error}");
                    return ExitCode::from(1);
                }
            }
            let uploaded = began.elapsed();

            let began = Instant::now();
            if let Err(error) = canvas.compose(
                FrameRequest {
                    index: frame.into(),
                },
                &scene,
            ) {
                eprintln!("{error}");
                return ExitCode::from(1);
            }
            let composed = began.elapsed();

            upload += uploaded;
            compose += composed;
            worst_upload = worst_upload.max(uploaded);
            worst_compose = worst_compose.max(composed);
        }

        let runs = u32::max(frames, 1);
        let mean_upload = upload / runs;
        let mean_compose = compose / runs;

        report("upload", mean_upload, worst_upload, layers);
        report("compose", mean_compose, worst_compose, layers);
        println!();
        println!(
            "together {} us a frame, which is {}% of one at sixty",
            (mean_upload + mean_compose).as_micros(),
            (mean_upload + mean_compose).as_micros() * 100 / 16_667
        );

        ExitCode::SUCCESS
    }

    /// Prints one measurement, and what it costs per layer.
    fn report(what: &str, mean: Duration, worst: Duration, layers: usize) {
        println!(
            "{what:<9} mean {:>7} us   worst {:>7} us   {:>6} us a layer",
            mean.as_micros(),
            worst.as_micros(),
            mean.as_micros() / layers as u128,
        );
    }

    /// The published effects, by the name `--effects` takes.
    fn published_names() -> Vec<&'static str> {
        ovs_types::config::effects::PUBLISHED
            .iter()
            .map(|manifest| manifest.type_id)
            .collect()
    }

    /// Turns `ovs.blur,ovs.tint` into a chain, or names the one it cannot.
    ///
    /// At the manifest's own defaults. A blur at radius zero would be a pass
    /// that costs what a pass costs and changes nothing, which is a fine thing
    /// to measure and a terrible thing to measure by accident — so the values
    /// come from the manifest rather than from here, exactly as they do for a
    /// real chain.
    fn effects_of(value: &str) -> Result<Vec<EffectInstance>, String> {
        let mut chain = Vec::new();
        for name in value.split(',').map(str::trim).filter(|n| !n.is_empty()) {
            let Some(manifest) = ovs_types::config::effects::PUBLISHED
                .iter()
                .find(|manifest| manifest.type_id == name)
            else {
                return Err(name.to_owned());
            };
            chain.push(EffectInstance::new(manifest.type_id, manifest.version));
        }
        Ok(chain)
    }

    /// A scene of `layers` quads, each covering the canvas and sampling its own
    /// layer's texture — which is what a stack of full-frame layers is.
    ///
    /// Every layer carries the same chain, which is the shape the acceptance
    /// asks about: a comparable effect load across the stack rather than one
    /// layer doing all the work.
    fn scene_of(layers: impl Iterator<Item = LayerId>, chain: &[EffectInstance]) -> Scene {
        let mut passes = Vec::new();
        let mut scene = Scene::new();
        for layer_id in layers {
            if !chain.is_empty() {
                passes.clear();
                ovs_renderer::effects::passes_into(chain, &mut passes);
                scene.set_effects(layer_id, &passes);
            }
            scene.push(Quad {
                left: -1.0,
                bottom: -1.0,
                right: 1.0,
                top: 1.0,
                colour: [1.0, 1.0, 1.0, 1.0],
                rotation_deg: 0.0,
                anchor: ovs_types::geometry::AnchorPoint::CENTRE,
                layer_id: Some(layer_id),
                source: SourceRect::WHOLE,
                source_turn: ovs_renderer::SourceTurn::NONE,
                texture: TextureRef::Frame(layer_id),
                route: Route::A,
                blend: BlendMode::Normal,
            });
        }
        scene
    }

    /// What the graphics card's bus is negotiated at, when Linux will say.
    ///
    /// Read from sysfs rather than from Vulkan, which has no notion of a link
    /// speed, and read for whichever device is the first non-integrated display
    /// controller — good enough for a machine with one card, and honest about
    /// saying nothing when it is not sure.
    fn pcie_link() -> Option<String> {
        let mut devices: Vec<std::path::PathBuf> = std::fs::read_dir("/sys/bus/pci/devices")
            .ok()?
            .filter_map(|entry| Some(entry.ok()?.path()))
            .filter(|path| {
                // Class 0x030000 is a VGA-compatible controller; 0x030200 is a
                // 3D controller, which is what a laptop's discrete card is.
                std::fs::read_to_string(path.join("class"))
                    .is_ok_and(|class| class.starts_with("0x0300") || class.starts_with("0x0302"))
            })
            .collect();
        devices.sort();

        let device = devices.into_iter().find(|path| {
            std::fs::read_to_string(path.join("max_link_speed"))
                .is_ok_and(|speed| !speed.trim().is_empty() && speed.trim() != "Unknown")
        })?;

        let current = std::fs::read_to_string(device.join("current_link_speed")).ok()?;
        let max = std::fs::read_to_string(device.join("max_link_speed")).ok()?;
        let width = std::fs::read_to_string(device.join("current_link_width")).ok()?;
        let (current, max, width) = (current.trim(), max.trim(), width.trim());

        if current == max {
            Some(format!(
                "its bus is at {current}, x{width}, which is all the card can do"
            ))
        } else {
            Some(format!(
                "its bus is at {current}, x{width} — the card can do {max}, so a copy here is slower than the card is"
            ))
        }
    }

    fn parse_size(value: &str) -> Option<SizePx> {
        let (width, height) = value.split_once(['x', 'X'])?;
        Some(SizePx::new(width.parse().ok()?, height.parse().ok()?))
    }

    fn parse<T: std::str::FromStr>(value: Option<&str>, what: &str) -> T {
        let Some(value) = value else {
            eprintln!("{what} needs a value");
            std::process::exit(2);
        };
        match value.parse() {
            Ok(parsed) => parsed,
            Err(_) => {
                eprintln!("{what} does not understand {value}");
                std::process::exit(2);
            }
        }
    }
}
