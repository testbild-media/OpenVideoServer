//! Asks the card which plane actually drives which panel, by driving them.
//!
//! ```text
//! sudo systemctl stop ovs-renderer
//! ovs-planes
//! sudo systemctl start ovs-renderer
//! ```
//!
//! # Why this exists
//!
//! A display is driven through a *plane*, and Vulkan offers two ways to ask
//! which planes reach which displays. Both lied on the machine this was written
//! for: `vkGetDisplayPlaneSupportedDisplaysKHR` said every plane could drive
//! every display, and `vkGetDisplayPlaneCapabilitiesKHR` reported identical
//! extents for all of them. What actually happened was that one plane worked,
//! one presented a picture onto the *other* panel, and one refused to present at
//! all with `ERROR_UNKNOWN` — and which was which moved about with the order
//! outputs were built in.
//!
//! Three evenings of black panels went into finding that out from symptoms. This
//! finds it out by asking, in about a minute.
//!
//! # What it does
//!
//! For every display, for every plane the driver says can drive it, this builds
//! exactly what the renderer builds — the same `VulkanDisplayBackend`, the same
//! surface, the same swapchain — fills it with one flat colour and presents.
//! Then it says whether that worked, and holds the picture long enough for
//! somebody standing in front of the panels to say **which** one lit up.
//!
//! The colour names the plane, so a person can read the answer off the wall:
//!
//! ```text
//!   plane 0  red     plane 1  green    plane 2  blue     plane 3  yellow
//! ```
//!
//! A combination that presents onto the wrong panel is the one no program can
//! detect, and the only reason a human is in this loop.
//!
//! # It needs the card to itself
//!
//! Direct display mode takes the display, and the renderer has it while it is
//! running. Stop it first; this says so and exits if it cannot open a display.

use std::sync::Arc;
use std::time::Duration;

use ovs_renderer::backend::{FrameRequest, OutputBackend};
use ovs_renderer::scene::{Quad, Scene};
use ovs_renderer::vulkan::display::Display;
use ovs_renderer::vulkan::present::VulkanDisplayBackend;
use ovs_renderer::vulkan::VulkanContext;
use ovs_types::config::DisplayMode;

/// How long each working combination is held on the panel.
///
/// Long enough to look up from the keyboard, walk round the rack and read it.
const HOLD: Duration = Duration::from_secs(5);

/// How many frames are presented before the picture is called good.
///
/// The first queues while the mode is still being set and the second returns
/// once the panel is scanning out, so a single present proves nothing. Ten is
/// well past that and still a sixth of a second.
const FRAMES: u64 = 10;

/// What each plane is painted, so the answer can be read off the panel.
const COLOURS: [(&str, [f32; 4]); 6] = [
    ("red", [1.0, 0.1, 0.1, 1.0]),
    ("green", [0.1, 1.0, 0.1, 1.0]),
    ("blue", [0.2, 0.3, 1.0, 1.0]),
    ("yellow", [1.0, 0.9, 0.1, 1.0]),
    ("magenta", [1.0, 0.1, 0.9, 1.0]),
    ("cyan", [0.1, 0.9, 1.0, 1.0]),
];

fn main() -> std::process::ExitCode {
    let filter = tracing_subscriber::EnvFilter::try_from_env("OVS_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    match probe() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn probe() -> Result<(), Box<dyn std::error::Error>> {
    let context = Arc::new(VulkanContext::open("ovs-planes", None)?);
    println!("{}", context.summary().name);

    if !context.has_display() {
        return Err("this Vulkan has no display extensions: nothing can be presented to".into());
    }

    let displays = context.displays()?;
    if displays.is_empty() {
        return Err("no connectors: nothing is plugged in, or this GPU drives none".into());
    }

    let planes = context.display_planes()?;
    println!(
        "{} connector(s), {} plane(s). Each working combination is held for {} seconds.",
        displays.len(),
        planes.len(),
        HOLD.as_secs()
    );
    println!("Watch the panels and write down which one lights, and in what colour.\n");

    let mut worked = Vec::new();
    let mut refused = Vec::new();

    for display in &displays {
        let Some(mode) = display.preferred_mode() else {
            println!("{}: offers no mode at all, skipped", display.label());
            continue;
        };

        for plane in &planes {
            if !plane.supports.contains(&display.handle) {
                continue;
            }

            let (colour_name, colour) = COLOURS[plane.index as usize % COLOURS.len()];
            print!(
                "{} via plane {} at {}x{} — {colour_name} … ",
                display.label(),
                plane.index,
                mode.size.width,
                mode.size.height
            );
            use std::io::Write as _;
            let _ = std::io::stdout().flush();

            match show(&context, display, mode, plane.index, colour) {
                Ok(()) => {
                    println!("presented");
                    worked.push((display.label(), plane.index, colour_name));
                }
                Err(error) => {
                    println!("refused: {error}");
                    refused.push((display.label(), plane.index, error));
                }
            }
        }
    }

    println!("\n--- what the driver accepted ---");
    if worked.is_empty() {
        println!("  nothing at all");
    }
    for (display, plane, colour) in &worked {
        println!("  {display} via plane {plane} ({colour})");
    }
    if !refused.is_empty() {
        println!("--- what it refused ---");
        for (display, plane, error) in &refused {
            println!("  {display} via plane {plane}: {error}");
        }
    }
    println!(
        "\nA combination listed as accepted may still have lit the *other* panel.\n\
         That is the one thing this cannot see, and the reason for the colours."
    );

    drm_side(&context, &displays);

    if displays.len() >= 2 {
        both(&context, &displays, &planes)?;
        as_the_renderer_does(&context, &displays)?;
        with_a_rebuild(&context, &displays)?;
        while_one_is_running(&context, &displays)?;
        regrouped(&context, &displays)?;
    }

    Ok(())
}

/// Asks the DRM side which display each kernel connector is, and tries to take it.
///
/// Two questions at once. `vkAcquireDrmDisplayEXT` refused every display that
/// came out of the ordinary enumeration; `vkGetDrmDisplayEXT` is the other way
/// in, and it names a display by the connector id the kernel publishes in
/// `/sys/class/drm`. If the handle it returns matches one this already knows,
/// that is also a connector mapping that needs no EDID names — which matters,
/// because Vulkan calls this machine's panels `DP-4` and `HDMI-0` where the
/// kernel calls them `eDP-1` and `HDMI-A-1`.
fn drm_side(context: &Arc<VulkanContext>, displays: &[Display]) {
    println!("\n=== the DRM side ===");

    let connectors = kernel_connectors();
    if connectors.is_empty() {
        println!("  no connector ids in /sys/class/drm: nothing to ask about");
        return;
    }

    // Master first: taking a display from the kernel is a master's privilege,
    // and this is the one thing left that has never been asked for.
    match context.become_drm_master() {
        Ok(()) => println!("  this process is now DRM master of the card"),
        Err(error) => println!("  could not become DRM master: {error}"),
    }

    for (name, id) in connectors {
        print!("  {name} (connector {id}): ");
        match context.drm_display(id) {
            Ok(handle) => {
                let known = displays
                    .iter()
                    .find(|display| display.handle == handle)
                    .map_or_else(
                        || "not one this renderer enumerated".to_owned(),
                        Display::label,
                    );
                print!("{known}; ");
                match context.acquire_drm_display_for_test(handle) {
                    Ok(()) => println!("taken from the kernel"),
                    Err(error) => println!("could not be taken: {error}"),
                }
            }
            Err(error) => println!("no Vulkan display for it: {error}"),
        }
    }
}

/// The connected connectors the kernel publishes, as name and id.
fn kernel_connectors() -> Vec<(String, u32)> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir("/sys/class/drm") else {
        return found;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        // `card0-eDP-1`, and not `card0` itself or `renderD128`.
        let Some((_, connector)) = name.split_once('-') else {
            continue;
        };
        if std::fs::read_to_string(path.join("status"))
            .unwrap_or_default()
            .trim()
            != "connected"
        {
            continue;
        }
        let Ok(id) = std::fs::read_to_string(path.join("connector_id"))
            .unwrap_or_default()
            .trim()
            .parse::<u32>()
        else {
            continue;
        };
        found.push((connector.to_owned(), id));
    }

    found.sort();
    found
}

/// Drives two displays at the same time, which is the arrangement that fails.
///
/// **The case the first half cannot reach.** One at a time, every plane drove
/// every display; the renderer only ever went wrong with two outputs alive
/// together — both pictures on one panel, or one refusing to present at all.
/// This builds both, on a named pair of planes, and says what the driver did.
fn both(
    context: &Arc<VulkanContext>,
    displays: &[Display],
    planes: &[ovs_renderer::vulkan::display::PlaneReport],
) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n=== two at once ===");
    println!("The first display is painted red, the second green. Say which panel shows what.\n");

    let first = &displays[0];
    let second = &displays[1];

    // A handful of pairings rather than all of them: what matters is whether
    // *any* pair works, and twelve combinations is five minutes of somebody
    // standing in a rack.
    for (left, right) in [(0_u32, 1_u32), (1, 0), (0, 2), (2, 0), (1, 2)] {
        if !planes.iter().any(|plane| plane.index == left)
            || !planes.iter().any(|plane| plane.index == right)
        {
            continue;
        }

        println!(
            "{} on plane {left} (red)  +  {} on plane {right} (green)",
            first.label(),
            second.label()
        );

        match pair(context, first, second, left, right) {
            Ok(()) => println!("   both presented"),
            Err(error) => println!("   {error}"),
        }
    }

    Ok(())
}

/// Builds both outputs, presents to both, holds, and drops both.
fn pair(
    context: &Arc<VulkanContext>,
    first: &Display,
    second: &Display,
    left: u32,
    right: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    let (Some(first_mode), Some(second_mode)) = (first.preferred_mode(), second.preferred_mode())
    else {
        return Err("one of these displays offers no mode".into());
    };

    let mut one = VulkanDisplayBackend::on_plane(
        Arc::clone(context),
        first.label(),
        first,
        first_mode,
        DisplayMode {
            size: first_mode.size,
            refresh: first_mode.refresh(),
        },
        left,
    )
    .map_err(|error| format!("the first output could not be built: {error}"))?;

    // **Built while the first is alive**, which is the whole point: the renderer
    // never failed at one output and never succeeded at two.
    let mut two = VulkanDisplayBackend::on_plane(
        Arc::clone(context),
        second.label(),
        second,
        second_mode,
        DisplayMode {
            size: second_mode.size,
            refresh: second_mode.refresh(),
        },
        right,
    )
    .map_err(|error| format!("the second output could not be built: {error}"))?;

    let red = flat([1.0, 0.1, 0.1, 1.0]);
    let green = flat([0.1, 1.0, 0.1, 1.0]);

    for index in 0..FRAMES {
        one.present(FrameRequest { index }, &red)
            .map_err(|error| format!("the first output stopped presenting: {error}"))?;
        two.present(FrameRequest { index }, &green)
            .map_err(|error| format!("the second output stopped presenting: {error}"))?;
        std::thread::sleep(Duration::from_millis(16));
    }

    std::thread::sleep(HOLD);
    drop(two);
    drop(one);
    Ok(())
}

/// Builds two outputs the way the renderer builds them, and says what it chose.
///
/// **No plane is named.** The renderer asks `display_plane_for`, which reads the
/// driver's account of which plane is already on which display and its own
/// ledger of what it has handed out. Naming planes is what the phase above does,
/// and it proved that planes 0 and 1 drive two panels at once — while the
/// renderer, choosing for itself, put both pictures on one.
fn as_the_renderer_does(
    context: &Arc<VulkanContext>,
    displays: &[Display],
) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n=== as the renderer chooses ===");
    println!("The first display is red, the second green. Say which panel shows what.\n");

    let mut held: Vec<VulkanDisplayBackend> = Vec::new();
    let colours = [[1.0, 0.1, 0.1, 1.0], [0.1, 1.0, 0.1, 1.0]];

    for (index, display) in displays.iter().enumerate().take(2) {
        let Some(mode) = display.preferred_mode() else {
            continue;
        };
        // `on`, not `on_plane`: the chooser is the thing being tested.
        match VulkanDisplayBackend::on(
            Arc::clone(context),
            display.label(),
            display,
            mode,
            DisplayMode {
                size: mode.size,
                refresh: mode.refresh(),
            },
        ) {
            Ok(backend) => {
                println!(
                    "  {} came up ({})",
                    display.label(),
                    if index == 0 { "red" } else { "green" }
                );
                held.push(backend);
            }
            Err(error) => println!("  {} could not be built: {error}", display.label()),
        }
    }

    if held.len() < 2 {
        println!("  fewer than two outputs: nothing to see");
        return Ok(());
    }

    for index in 0..FRAMES {
        for (which, backend) in held.iter_mut().enumerate() {
            let scene = flat(colours[which]);
            if let Err(error) = backend.present(FrameRequest { index }, &scene) {
                println!("  output {which} stopped presenting: {error}");
            }
        }
        std::thread::sleep(Duration::from_millis(16));
    }

    std::thread::sleep(HOLD);
    drop(held);
    Ok(())
}

/// Rebuilds one display the way `adopt` does, then brings the other one up.
///
/// **The one thing the renderer does that nothing above does.** `adopt` builds
/// the replacement before releasing what it replaces, so for a moment two
/// surfaces exist on one display. That is deliberate — a failed build then
/// leaves the wall as it was — and it is the only difference left between a
/// renderer that puts both pictures on one panel and a probe that does not.
fn with_a_rebuild(
    context: &Arc<VulkanContext>,
    displays: &[Display],
) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n=== with a rebuild, as `adopt` does it ===");
    println!("The first display is red, the second green. Say which panel shows what.\n");

    let first = &displays[0];
    let second = &displays[1];
    let (Some(first_mode), Some(second_mode)) = (first.preferred_mode(), second.preferred_mode())
    else {
        return Err("one of these displays offers no mode".into());
    };

    let build = |display: &Display, mode: ovs_renderer::vulkan::display::DisplayMode| {
        VulkanDisplayBackend::on(
            Arc::clone(context),
            display.label(),
            display,
            mode,
            DisplayMode {
                size: mode.size,
                refresh: mode.refresh(),
            },
        )
    };

    let old = build(first, first_mode).map_err(|error| format!("the first build: {error}"))?;
    println!("  {} is up", first.label());

    // **The handover**: the replacement is built while the old one still holds
    // its display and its plane, and only then is the old one let go.
    let mut replacement =
        build(first, first_mode).map_err(|error| format!("the rebuild: {error}"))?;
    println!(
        "  {} was rebuilt while the first was still alive",
        first.label()
    );
    drop(old);
    println!("  the first is released");

    let mut other = build(second, second_mode).map_err(|error| format!("the second: {error}"))?;
    println!("  {} is up", second.label());

    let red = flat([1.0, 0.1, 0.1, 1.0]);
    let green = flat([0.1, 1.0, 0.1, 1.0]);
    for index in 0..FRAMES {
        if let Err(error) = replacement.present(FrameRequest { index }, &red) {
            println!("  the rebuilt output stopped presenting: {error}");
        }
        if let Err(error) = other.present(FrameRequest { index }, &green) {
            println!("  the second output stopped presenting: {error}");
        }
        std::thread::sleep(Duration::from_millis(16));
    }

    std::thread::sleep(HOLD);
    Ok(())
}

/// Brings the second display up while the first is already presenting.
///
/// **The last difference.** Every phase above builds both outputs before showing
/// anything, and every one of them works. The renderer is a loop: it presents to
/// what it has, and a second output is created between two frames of that loop,
/// because `follow_console` and `follow_configuration` run once a second inside
/// `tick`'s own rhythm.
fn while_one_is_running(
    context: &Arc<VulkanContext>,
    displays: &[Display],
) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n=== the second comes up while the first is presenting ===");
    println!("The first display is red, the second green. Say which panel shows what.\n");

    let first = &displays[0];
    let second = &displays[1];
    let (Some(first_mode), Some(second_mode)) = (first.preferred_mode(), second.preferred_mode())
    else {
        return Err("one of these displays offers no mode".into());
    };

    let mut one = VulkanDisplayBackend::on(
        Arc::clone(context),
        first.label(),
        first,
        first_mode,
        DisplayMode {
            size: first_mode.size,
            refresh: first_mode.refresh(),
        },
    )
    .map_err(|error| format!("the first output: {error}"))?;
    println!("  {} is up and presenting", first.label());

    let red = flat([1.0, 0.1, 0.1, 1.0]);
    let green = flat([0.1, 1.0, 0.1, 1.0]);
    let mut two: Option<VulkanDisplayBackend> = None;

    // Two seconds of frames, and the second output is built in the middle of
    // them — not before them.
    for index in 0..120_u64 {
        if let Err(error) = one.present(FrameRequest { index }, &red) {
            println!("  the first output stopped presenting: {error}");
        }

        if index == 60 {
            match VulkanDisplayBackend::on(
                Arc::clone(context),
                second.label(),
                second,
                second_mode,
                DisplayMode {
                    size: second_mode.size,
                    refresh: second_mode.refresh(),
                },
            ) {
                Ok(backend) => {
                    println!("  {} was built mid-flight", second.label());
                    two = Some(backend);
                }
                Err(error) => println!("  the second output could not be built: {error}"),
            }
        }

        if let Some(backend) = two.as_mut() {
            if let Err(error) = backend.present(FrameRequest { index }, &green) {
                println!("  the second output stopped presenting: {error}");
            }
        }

        std::thread::sleep(Duration::from_millis(16));
    }

    std::thread::sleep(HOLD);
    Ok(())
}

/// Presents on one display, then rebuilds the whole set and presents to both.
///
/// **The cure phase five argues for.** A display created while another is
/// presenting takes that other one's picture onto its panel; every phase that
/// built all the outputs before presenting anything worked. So the display
/// outputs are one group: a change to the group releases all of it and builds
/// all of it, and nothing presents in between.
fn regrouped(
    context: &Arc<VulkanContext>,
    displays: &[Display],
) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n=== released and rebuilt as one group ===");
    println!("The first display is red, the second green. Say which panel shows what.\n");

    let first = &displays[0];
    let second = &displays[1];
    let (Some(first_mode), Some(second_mode)) = (first.preferred_mode(), second.preferred_mode())
    else {
        return Err("one of these displays offers no mode".into());
    };

    let build = |display: &Display, mode: ovs_renderer::vulkan::display::DisplayMode| {
        VulkanDisplayBackend::on(
            Arc::clone(context),
            display.label(),
            display,
            mode,
            DisplayMode {
                size: mode.size,
                refresh: mode.refresh(),
            },
        )
    };

    let red = flat([1.0, 0.1, 0.1, 1.0]);
    let green = flat([0.1, 1.0, 0.1, 1.0]);

    // One output, presenting for a while — the state the renderer is in when a
    // System screen is switched on.
    let mut alone = build(first, first_mode).map_err(|error| format!("the first: {error}"))?;
    for index in 0..60_u64 {
        let _ = alone.present(FrameRequest { index }, &red);
        std::thread::sleep(Duration::from_millis(16));
    }
    println!("  {} has been presenting on its own", first.label());

    // **Everything goes, then everything comes back.** Not "add one to what is
    // running", which is what breaks it.
    drop(alone);
    println!("  every display output released");

    let mut one = build(first, first_mode).map_err(|error| format!("the first again: {error}"))?;
    let mut two = build(second, second_mode).map_err(|error| format!("the second: {error}"))?;
    println!("  both built from scratch, with nothing presenting in between");

    for index in 0..FRAMES {
        let _ = one.present(FrameRequest { index }, &red);
        let _ = two.present(FrameRequest { index }, &green);
        std::thread::sleep(Duration::from_millis(16));
    }

    std::thread::sleep(HOLD);
    Ok(())
}

/// One quad over the whole canvas, in one colour.
fn flat(colour: [f32; 4]) -> Scene {
    let mut scene = Scene::new();
    let mut quad = Quad::whole_canvas();
    quad.texture = ovs_renderer::scene::TextureRef::None;
    quad.colour = colour;
    scene.push(quad);
    scene
}

/// Builds one output on one plane, fills it with one colour, and holds it.
fn show(
    context: &Arc<VulkanContext>,
    display: &Display,
    display_mode: ovs_renderer::vulkan::display::DisplayMode,
    plane: u32,
    colour: [f32; 4],
) -> Result<(), Box<dyn std::error::Error>> {
    let wanted = DisplayMode {
        size: display_mode.size,
        refresh: display_mode.refresh(),
    };

    // The same call the renderer makes, on the plane this pass is asking about.
    let mut backend = VulkanDisplayBackend::on_plane(
        Arc::clone(context),
        display.label(),
        display,
        display_mode,
        wanted,
        plane,
    )?;

    // One quad over the whole canvas, in a flat colour: no texture, no
    // composition, nothing that could fail for a reason other than the plane.
    let mut scene = Scene::new();
    let mut quad = Quad::whole_canvas();
    quad.texture = ovs_renderer::scene::TextureRef::None;
    quad.colour = colour;
    scene.push(quad);

    for index in 0..FRAMES {
        backend.present(FrameRequest { index }, &scene)?;
        std::thread::sleep(Duration::from_millis(16));
    }

    std::thread::sleep(HOLD);
    // Dropped here, which releases the display and the plane before the next
    // combination asks for one.
    drop(backend);
    Ok(())
}
