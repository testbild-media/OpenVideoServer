//! `ovs-renderer` — the OpenVideoServer renderer.
//!
//! Two threads and nothing else on the hot path:
//!
//! - **the render thread**, which presents frames and never blocks on anything
//!   but the display;
//! - **the control thread**, which accepts a connection from the core and feeds
//!   the shared state the render thread reads.
//!
//! The renderer listens; the core connects. That is why `ovsd` stopping does not
//! stop the show.
//!
//! | Variable | Default | Meaning |
//! |---|---|---|
//! | `OVS_RENDERER_BIND` | `127.0.0.1:7421` | control channel address |
//! | `OVS_RENDERER_OUTPUTS` | `1` | outputs to drive |
//! | `OVS_RENDERER_BACKEND` | `headless` | `headless`, `vulkan` or `display` |
//! | `OVS_RENDERER_DEVICE` | *(best available)* | GPU name or index to use |
//! | `OVS_RENDERER_CONNECTOR` | *(the first found)* | connectors to drive, comma separated |
//! | `OVS_RENDERER_MODE` | *(the panel's own)* | mode to force, `1920x1080@60` |
//! | `OVS_LOG` | `info` | log filter |
//!
//! `headless` renders nowhere and `vulkan` renders to an offscreen canvas; both
//! are for machines with nothing to present on, and for the tests. `display`
//! takes a physical connector and puts frames on it, which is what a server in a
//! rack does.
//!
//! `ovs-renderer --displays` prints the physical connectors this machine's GPU
//! can drive, with their mode lists. It is the first thing to run when
//! commissioning a server: a connector that is not listed is one no show can
//! be mapped onto, and the reason is usually a cable.

use std::net::TcpListener;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ovs_renderer::backend::{
    BackendError, Compositor, CompositorFactory, HeadlessBackend, HeadlessCompositor, OutputBackend,
};
use ovs_renderer::control;
use ovs_renderer::ndi::{Discovery, Ndi};
use ovs_renderer::render_loop::RenderLoop;
use ovs_renderer::show_state::SharedShowState;
use ovs_renderer::vulkan::display::Display;
use ovs_renderer::vulkan::present::VulkanDisplayBackend;
use ovs_renderer::vulkan::{VulkanContext, VulkanOffscreenBackend};
use ovs_types::config::DisplayMode;
use ovs_types::geometry::{FrameRate, SizePx};

const DEFAULT_BIND: &str = "127.0.0.1:7421";

/// How long the render thread sleeps at most before checking the time again.
///
/// A ceiling, not a period. Sleeping exactly until the next deadline would mean
/// never noticing a shutdown request between frames.
const MAX_SLEEP: Duration = Duration::from_millis(5);

/// How often a picture of each output is taken.
///
/// Matches what the control thread sends, so a preview is neither taken and
/// dropped nor sent twice.
const PREVIEW_INTERVAL: Duration = Duration::from_millis(200);

fn main() -> ExitCode {
    let filter = tracing_subscriber::EnvFilter::try_from_env("OVS_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    // **Before anything else it might say.** Every build of 0.2.0 carries the
    // same version, so this is the only thing that tells one from another —
    // and the question it answers, "is the fix I just made in this", cost two
    // installs on 2026-10-10 because nothing could answer it.
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        commit = ovs_types::BUILD_COMMIT,
        "ovs-renderer"
    );

    // **Anything unrecognised is refused.** These used to be two `any()` scans
    // with everything else falling through to starting a renderer — so a
    // misspelt flag, or the right flag on a build older than it, starts a
    // second renderer beside the running one and says `Address already in use`.
    // That names a port and not the one thing that is wrong (owner,
    // 2026-10-10, running `--ndi-sources` against the installed package).
    let mut asked: Option<&'static str> = None;
    for argument in std::env::args().skip(1) {
        match argument.as_str() {
            "--displays" => asked = Some("--displays"),
            "--ndi-sources" => asked = Some("--ndi-sources"),
            other => {
                eprintln!("ovs-renderer: no such option: {other}");
                eprintln!();
                eprintln!("  --displays      what this machine is plugged into");
                eprintln!("  --ndi-sources   what NDI it can see on the network");
                eprintln!();
                eprintln!("With none of them it runs the renderer.");
                return ExitCode::FAILURE;
            }
        }
    }

    match asked {
        Some("--displays") => {
            return match list_displays() {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    tracing::error!("{error}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("--ndi-sources") => return list_ndi_sources(),
        _ => {}
    }

    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!("{error}");
            ExitCode::FAILURE
        }
    }
}

/// Builds outputs that drive real connectors.
///
/// Which connectors carry a show is **the core's decision**, sent as connector
/// roles and set by an operator in the client. This waits for
/// them, because a renderer that started driving connectors first would have to
/// guess, and the wrong guess is a show on the Service Display and a console
/// nowhere.
///
/// `OVS_RENDERER_CONNECTOR` overrides the roles for a machine being commissioned
/// with no client to hand. A connector it names that this machine is not driving
/// is a hard failure; the alternative is a show on the wrong wall.
///
/// With neither — no roles and nothing named — this drives **nothing** and says
/// so. An unassigned socket staying dark is the safe direction.
fn build_display_outputs(
    context: Arc<VulkanContext>,
    state: &Arc<SharedShowState>,
) -> Result<Stage, Box<dyn std::error::Error>> {
    if !context.has_display() {
        return Err(
            "this machine's Vulkan has no display extensions: nothing can be presented to".into(),
        );
    }

    let displays = context.displays()?;
    let gpu = context.summary().pci_address.clone().unwrap_or_default();

    // Tell the core what is here, whatever is decided about it. A client cannot
    // offer an operator a list of sockets to assign without this.
    state.set_connectors(connectors_of(&displays, &gpu));

    if displays.is_empty() {
        // Not a failure. A machine whose only panel is the Service Display has
        // no show outputs and is working perfectly; so is one whose processors
        // are not powered up yet. A renderer that exited here would be restarted
        // by systemd every second for the rest of the evening, and the core
        // would report it unreachable rather than idle (house rule §17).
        tracing::warn!(
            device = %context.summary().name,
            "this GPU is driving no connectors: nothing to present on, so nothing will be"
        );
    }

    let named = wanted_connectors();
    let chosen: Vec<&ovs_renderer::vulkan::display::Display> = if named.is_empty() {
        let roles = wait_for_connector_roles(state);
        let chosen = ovs_renderer::vulkan::display::media_outputs(&displays, &gpu, &roles);
        if chosen.is_empty() {
            tracing::warn!(
                connectors = display_names(&displays),
                "no connector on this machine is assigned as a media output; nothing will be \
                 driven until one is (set it in the client, or name one with \
                 OVS_RENDERER_CONNECTOR)"
            );
        }
        chosen
    } else {
        let mut chosen = Vec::with_capacity(named.len());
        for name in &named {
            let found = displays
                .iter()
                .find(|display| display.connector.eq_ignore_ascii_case(name))
                .ok_or_else(|| {
                    format!(
                        "no connector called {name:?}; this machine is driving: {}",
                        display_names(&displays)
                    )
                })?;
            chosen.push(found);
        }
        chosen
    };

    let forced = wanted_mode();
    let mut outputs: Vec<Box<dyn OutputBackend>> = Vec::with_capacity(chosen.len());

    for (index, display) in chosen.iter().enumerate() {
        let display_mode = match forced {
            Some((size, refresh)) => display.mode_for(size, refresh).ok_or_else(|| {
                format!(
                    "{} does not offer {}x{} at {:.3} Hz; it offers {}",
                    display.label(),
                    size.width,
                    size.height,
                    refresh.as_f64(),
                    mode_list(display)
                )
            })?,
            None => display
                .preferred_mode()
                .ok_or_else(|| format!("{} offers no modes at all", display.label()))?,
        };

        let name = if display.connector.is_empty() {
            format!("Output {}", index + 1)
        } else {
            display.label()
        };

        outputs.push(Box::new(VulkanDisplayBackend::on(
            Arc::clone(&context),
            name,
            display,
            display_mode,
            DisplayMode::new(display_mode.size, display_mode.refresh()),
        )?));
    }

    // The composition is made at whatever size the show asks for, on the same
    // device the outputs present from — an image rendered on one device cannot
    // be sampled by another, which is why there is one device for the process.
    let refresh = outputs
        .first()
        .map_or(FrameRate::FPS_60, |output| output.mode().refresh);

    // What a rebuild needs when the configuration changes what the hardware
    // should be doing, and what is running now so that a show asking for
    // what is already on the wall changes nothing.
    let stage = DisplayStage {
        context: Arc::clone(&context),
        // Filled by the first decision, a second from now.
        roles: std::sync::Arc::new(std::collections::BTreeMap::new()),
        displays,
        gpu_pci_address: gpu,
        running: outputs
            .iter()
            .filter_map(|output| {
                output
                    .identity()
                    .and_then(|identity| identity.connector)
                    .map(|connector| (connector, output.mode()))
            })
            .collect(),
        refused: std::collections::BTreeMap::new(),
        plugged: plugged_in(),
    };

    let factory = move |size: SizePx| {
        VulkanOffscreenBackend::on(
            Arc::clone(&context),
            "Composition",
            DisplayMode::new(size, refresh),
        )
        .map(|canvas| Box::new(canvas) as Box<dyn Compositor>)
        .map_err(|error| BackendError::UploadFailed {
            name: "the composition".to_owned(),
            reason: error.to_string(),
        })
    };

    let device = Arc::clone(&stage.context);
    Ok((Box::new(factory), outputs, Some(stage), Some(device)))
}

/// Rebuilds the physical outputs when the configuration asks for something else.
///
/// **What a show configures is what the connector runs at.** An output that
/// says 1920x1080 at 60 gets that mode set on its connector; it is not composed
/// at that size and scaled onto whatever the panel happened to come up in. The
/// difference is visible on a wall, and it is the difference between a mapping
/// an operator can trust and one that is approximately right.
///
/// A mode the panel does not offer is a fault, by name, with that output's pieces
/// dark — never the nearest thing instead. And a show asking for what is
/// already running changes nothing at all, because setting a mode takes a wall
/// dark for a second and doing that for no reason is worse than doing nothing.
fn wanted_panels(
    stage: &mut DisplayStage,
    state: &Arc<SharedShowState>,
) -> std::collections::BTreeMap<String, ovs_types::config::DisplayMode> {
    // **Asked again, not remembered — but only when something moved.** The
    // displays used to be enumerated once at startup, so changing a monitor
    // afterwards changed nothing: the client went on offering the modes of
    // whatever had been plugged in when the renderer started, and this went on
    // holding that thing's handles.
    //
    // Asking costs 95 ms on this driver, measured, which is most of six frames
    // at sixty — so asking every second cost five or six dropped frames a
    // second on an idle machine. The kernel answers "has anything moved" for
    // nothing, so that is the question asked every pass and Vulkan is asked
    // only when the answer changes (owner, 2026-09-25).
    let plugged = plugged_in();
    if plugged != stage.plugged {
        stage.plugged = plugged;
        match stage.context.displays() {
            Ok(found) => {
                let before = connectors_of(&stage.displays, &stage.gpu_pci_address);
                let after = connectors_of(&found, &stage.gpu_pci_address);
                stage.displays = found;
                if !same_connectors(&before, &after) {
                    tracing::info!(
                        was = before.len(),
                        now = after.len(),
                        "what this machine is wired to has changed"
                    );
                    state.set_connectors(after);
                    // Whatever is running was built against what was there
                    // before, so it is built again rather than left on a handle
                    // that may belong to something now unplugged.
                    stage.running.clear();
                    // And a refusal belonged to the panel that has just gone.
                    // The one plugged in now may well offer the mode this show
                    // asks for, and `attainable_now` would otherwise never let
                    // it be tried again.
                    stage.refused.clear();
                }
            }
            // Not fatal: the last reading is still the best answer there is,
            // and a renderer that stopped driving its walls because one query
            // failed would be worse than one working from a moment ago.
            Err(error) => {
                tracing::warn!("could not read this machine's connectors again: {error}");
            }
        }
    }

    let show = state.load();
    // Kept on the stage so the build half sees exactly the roles this decision
    // was made from, rather than reading them again a moment later.
    stage.roles = state.connector_roles();
    let available = ovs_renderer::vulkan::display::media_outputs(
        &stage.displays,
        &stage.gpu_pci_address,
        &stage.roles,
    );

    // What the show asks of each connector this machine is allowed to drive.
    let names: Vec<String> = available
        .iter()
        .map(|display| display.connector.clone())
        .collect();
    let wanted = wanted_modes(show.show.as_deref(), &stage.gpu_pci_address, &names);

    // **Exactly what the show asks for, and nothing else.**
    //
    // Every allowed connector used to be added here with whatever it was
    // running, on the reasoning that "a show that names one wall must not take
    // the other one dark". That was right while roles were assigned by hand:
    // `available` was then the set of sockets somebody had blessed, and a show
    // mentioning one of them had no business taking the rest dark.
    //
    // A role is derived from the show now, so `available` *is* what the show
    // uses, and the only thing this could add back was the output an operator
    // had just deleted — left running, and drawing the whole composition,
    // because a wall the show says nothing about has no pieces to cut it with
    // (owner, 2026-09-25: "läuft alles weiter statt dass das display aus geht",
    // and the picture "war die composition wieder fullscreen").
    //
    // It drove a **disabled** output too. One still holds its socket, so that
    // nothing takes it between two cues; holding is not driving, and an entry
    // added here could not tell the two apart.

    wanted
}

/// Builds the walls the show asks for, on the connectors it names.
///
/// **Only ever called with every display output already released.** Creating one
/// while another is presenting moves that other one's picture onto the new
/// panel; see `RenderLoop::release_displays`.
fn build_panels(
    stage: &mut DisplayStage,
    wanted: &std::collections::BTreeMap<String, ovs_types::config::DisplayMode>,
) -> (
    Vec<Box<dyn OutputBackend>>,
    std::collections::BTreeMap<String, ovs_types::config::DisplayMode>,
) {
    let roles = stage.roles.clone();
    let available = ovs_renderer::vulkan::display::media_outputs(
        &stage.displays,
        &stage.gpu_pci_address,
        &roles,
    );

    let mut outputs: Vec<Box<dyn OutputBackend>> = Vec::with_capacity(wanted.len());
    let mut built = std::collections::BTreeMap::new();

    for (connector, mode) in wanted {
        let Some(display) = available
            .iter()
            .find(|display| display.connector.eq_ignore_ascii_case(connector))
        else {
            continue;
        };

        // What the panel advertised, and failing that what the driver will
        // build. A display with no EDID advertises a handful of fallback
        // timings and nothing else, and a wall that is genuinely 1920x1080 is
        // then unreachable through the list.
        let display_mode = match display.mode_for(mode.size, mode.refresh) {
            Some(found) => found,
            None => match stage.context.force_mode(display, mode.size, mode.refresh) {
                Ok(forced) => {
                    // Built out here for the same reason the error below is:
                    // inside the macro `display` is a field shorthand and stops
                    // meaning the panel.
                    let offers = mode_list(display);
                    tracing::warn!(
                        connector = %connector,
                        wanted = format!(
                            "{}x{}@{:.3}",
                            mode.size.width,
                            mode.size.height,
                            mode.refresh.as_f64()
                        ),
                        offers,
                        "this panel does not advertise the mode the show asks for; the driver \
                         built it anyway"
                    );
                    forced
                }
                Err(error) => {
                    // Once per connector and mode. Said again when either
                    // changes, because then it is news rather than the same
                    // sentence for the two thousandth time.
                    if stage.refused.get(connector) == Some(mode) {
                        continue;
                    }
                    stage.refused.insert(connector.clone(), *mode);
                    // Built out here, not in the macro: `display` is a field
                    // shorthand inside it and would stop meaning the panel.
                    let offers = mode_list(display);
                    tracing::error!(
                        connector = %connector,
                        wanted = format!(
                            "{}x{}@{:.3}",
                            mode.size.width,
                            mode.size.height,
                            mode.refresh.as_f64()
                        ),
                        offers,
                        "this panel does not offer the mode the show asks for and the driver \
                         would not build it ({error}); it stays dark rather than running \
                         something nobody asked for. Measured on NVIDIA 550: this driver \
                         builds every mode it listed and refuses every mode it did not, so \
                         one that is not in the offers is forced at the kernel instead \
                         (video=CONNECTOR:WIDTHxHEIGHT@HZe — see deploy/README.md), not \
                         asked for here"
                    );
                    continue;
                }
            },
        };

        match VulkanDisplayBackend::on(
            Arc::clone(&stage.context),
            // The same label the first build gave it: the panel and the socket.
            // A client listing outputs should not see one of them renamed
            // because a show changed a mode.
            display.label(),
            display,
            display_mode,
            *mode,
        ) {
            Ok(backend) => {
                built.insert(connector.clone(), *mode);
                // It works now, so the next time it does not is news.
                stage.refused.remove(connector);
                outputs.push(Box::new(backend));
            }
            Err(error) => {
                tracing::error!(connector = %connector, "cannot drive this connector: {error}");
            }
        }
    }

    (outputs, built)
}

/// How long to wait for the core to say what each connector is for.
///
/// The core connects within a second on a machine where both are running. This
/// is generous enough for a slow boot and short enough that a renderer started
/// on its own — during commissioning, or with no core at all — comes up and says
/// what it found rather than appearing to hang.
const ROLES_TIMEOUT: Duration = Duration::from_secs(15);

/// Waits for the core to send the connector roles.
fn wait_for_connector_roles(
    state: &Arc<SharedShowState>,
) -> std::collections::BTreeMap<String, ovs_types::config::ConnectorRole> {
    let deadline = Instant::now() + ROLES_TIMEOUT;
    let mut said = false;

    while Instant::now() < deadline {
        let roles = state.connector_roles();
        if !roles.is_empty() {
            return (*roles).clone();
        }
        if !said {
            said = true;
            tracing::info!("waiting for the core to say which connectors carry a show");
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    tracing::warn!(
        "the core did not say which connectors carry a show within {}s",
        ROLES_TIMEOUT.as_secs()
    );
    std::collections::BTreeMap::new()
}

/// Lists a display's modes, for an error an operator has to act on.
fn mode_list(display: &ovs_renderer::vulkan::display::Display) -> String {
    let mut modes: Vec<String> = display
        .modes
        .iter()
        .map(|mode| {
            format!(
                "{}x{}@{:.3}",
                mode.size.width,
                mode.size.height,
                f64::from(mode.refresh_millihertz) / 1000.0
            )
        })
        .collect();
    modes.sort();
    modes.dedup();
    modes.join(", ")
}

/// Returns the connectors an operator named, in the order they named them.
fn wanted_connectors() -> Vec<String> {
    std::env::var("OVS_RENDERER_CONNECTOR")
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Returns the mode an operator forced, as `1920x1080@60` or `1920x1080@59.94`.
fn wanted_mode() -> Option<(SizePx, FrameRate)> {
    let value = std::env::var("OVS_RENDERER_MODE").ok()?;
    let (size, rate) = value.split_once('@')?;
    let (width, height) = size.split_once('x')?;

    let hertz: f64 = rate.trim().parse().ok()?;
    // Back to a rational the way a driver reports it, so `59.94` finds the mode
    // a panel calls 59 940 mHz rather than missing it by rounding.
    let millihertz = (hertz * 1000.0).round() as u32;

    Some((
        SizePx::new(width.trim().parse().ok()?, height.trim().parse().ok()?),
        FrameRate::new(millihertz, 1000),
    ))
}

/// Lists what a machine is driving, for an error that has to be acted on.
fn display_names(displays: &[ovs_renderer::vulkan::display::Display]) -> String {
    displays
        .iter()
        .map(|display| {
            if display.connector.is_empty() {
                format!("display {}", display.index)
            } else {
                display.label()
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Returns the GPU an operator named, if they named one.
fn wanted_device() -> Option<String> {
    std::env::var("OVS_RENDERER_DEVICE")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// Prints the displays the chosen GPU is wired to.
///
/// Reports rather than decides. Which connector carries a show is server
/// configuration and is never inferred from what happens to be plugged in
///; this is how an operator finds out what the choices are.
/// What this machine can see on the network, asked once and printed.
///
/// **The question that had no way of being asked.** The client shows an empty
/// list; the service says nothing unless it found something; and between them
/// there was nothing that would say whether a machine was looking and finding
/// nothing, or not looking. A camera on one machine's Studio Monitor and an
/// empty list on the server is two facts about two machines and no fact about
/// the one that matters (owner, 2026-10-10).
///
/// Run as the renderer's own user, because what a service can see and what a
/// login shell can see are not always the same thing:
///
/// ```text
/// sudo -u ovs-renderer /usr/bin/ovs-renderer --ndi-sources
/// ```
fn list_ndi_sources() -> ExitCode {
    // Long enough for a sender that answers slowly, short enough to be a
    // command somebody runs rather than a thing they wait for. Discovery
    // answers in well under a second on a healthy network; this is five.
    const PATIENCE_MS: u32 = 5_000;

    // Before the load, not after it: the run where somebody most wants to know
    // which build they are holding is the one that fails.
    println!(
        "ovs-renderer {} ({})",
        env!("CARGO_PKG_VERSION"),
        ovs_types::BUILD_COMMIT
    );

    let ndi = match ovs_renderer::ndi::Ndi::load() {
        Ok(ndi) => ndi,
        Err(error) => {
            println!("{error}");
            println!();
            println!("deploy/README.md says where the runtime comes from and where to put it.");
            return ExitCode::FAILURE;
        }
    };

    println!("the NDI runtime is here");
    println!("looking for {} ms", PATIENCE_MS);

    let sources = ndi.sources(PATIENCE_MS);
    if sources.is_empty() {
        println!();
        println!("nothing answered.");
        println!();
        println!("Discovery is mDNS, so a sender answers only if it is on a network this");
        println!("machine can hear multicast on, and only if it is in a group this machine");
        println!("looks at. Things worth knowing, in the order they go wrong:");
        println!();
        println!("  - whether the sender is on this machine's own subnet;");
        println!("  - whether it sends in the default NDI group, which is all this looks at;");
        println!("  - whether anything between them forwards multicast.");
        return ExitCode::FAILURE;
    }

    println!();
    for name in &sources {
        println!("  {name}");
    }
    println!();
    println!(
        "{} source(s). A server that is running will offer these.",
        sources.len()
    );
    ExitCode::SUCCESS
}

fn list_displays() -> Result<(), Box<dyn std::error::Error>> {
    println!(
        "ovs-renderer {} ({})",
        env!("CARGO_PKG_VERSION"),
        ovs_types::BUILD_COMMIT
    );
    let context = VulkanContext::open("ovs-renderer", wanted_device().as_deref())?;
    let summary = context.summary();
    println!("{} ({})", summary.name, summary.kind);

    if !context.has_display() {
        println!("  this Vulkan has no display extensions: nothing can be presented to");
        return Ok(());
    }

    let displays = context.displays()?;
    if displays.is_empty() {
        println!("  no connectors: nothing is plugged in, or this GPU drives none");
        return Ok(());
    }

    for display in &displays {
        let name = if display.connector.is_empty() {
            format!("display {}", display.index)
        } else {
            display.label()
        };
        let physical = display.physical_size_mm.map_or_else(
            || "size unknown".to_owned(),
            |(width, height)| format!("{width}x{height} mm"),
        );
        println!(
            "  {name}: {}x{} native, {physical}, {} modes, handle {:?}",
            display.native_size.width,
            display.native_size.height,
            display.modes.len(),
            display.handle
        );

        // Highest first: the one an operator is most likely to want is the one
        // they should not have to scroll for.
        let mut modes = display.modes.clone();
        modes.sort_by_key(|mode| {
            std::cmp::Reverse((
                u64::from(mode.size.width) * u64::from(mode.size.height),
                mode.refresh_millihertz,
            ))
        });
        for mode in modes.iter().take(12) {
            println!(
                "      {:>5}x{:<5} {:>9.3} Hz",
                mode.size.width,
                mode.size.height,
                f64::from(mode.refresh_millihertz) / 1000.0
            );
        }
        if modes.len() > 12 {
            println!("      … and {} more", modes.len() - 12);
        }
    }

    // **The planes, because a connector is not what a surface is created on.**
    //
    // A display is driven through a plane, and two outputs that came up on two
    // different planes at two different modes still put both pictures on one
    // panel — which nothing here could have shown, because nothing here ever
    // mentioned a plane. What matters is which display each one is *on*: planes
    // compose onto a display, so two planes on the same one are two pictures on
    // the same panel, whatever mode each was created with.
    match context.display_planes() {
        Ok(planes) => {
            println!("  planes: {}", planes.len());
            let name_of = |handle| {
                displays
                    .iter()
                    .find(|display| display.handle == handle)
                    .map_or_else(|| "unknown".to_owned(), Display::label)
            };
            for plane in planes {
                let current = plane.current.map_or_else(
                    || "free".to_owned(),
                    |handle| format!("{} (stack {})", name_of(handle), plane.current_stack_index),
                );
                let supports: Vec<String> = plane.supports.into_iter().map(name_of).collect();
                println!(
                    "    plane {:<2} on {current}; can drive: {}",
                    plane.index,
                    if supports.is_empty() {
                        "nothing".to_owned()
                    } else {
                        supports.join(", ")
                    }
                );

                // And the precise question, per display: what can this plane do
                // with that display's own preferred mode? A plane that reports
                // a zero extent for one of them cannot put a picture there,
                // whatever the coarse list above says.
                for display in &displays {
                    let Some(mode) = display.preferred_mode() else {
                        continue;
                    };
                    match context.display_plane_fit(mode.handle, plane.index) {
                        Ok(fit) => println!(
                            "        with {}: {}x{} max, {}x{} min{}",
                            display.label(),
                            fit.max.0,
                            fit.max.1,
                            fit.min.0,
                            fit.min.1,
                            if fit.fits(mode.size) {
                                ""
                            } else {
                                "   <- cannot show that display's own mode"
                            }
                        ),
                        Err(error) => {
                            println!("        with {}: refused ({error})", display.label())
                        }
                    }
                }
            }
        }
        Err(error) => println!("  the planes could not be read: {error}"),
    }

    Ok(())
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let bind = std::env::var("OVS_RENDERER_BIND").unwrap_or_else(|_| DEFAULT_BIND.to_owned());
    let output_count: usize = std::env::var("OVS_RENDERER_OUTPUTS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1);

    let mode = DisplayMode::new(SizePx::new(1920, 1080), FrameRate::FPS_60);
    let backend = std::env::var("OVS_RENDERER_BACKEND").unwrap_or_else(|_| "headless".to_owned());

    let state = Arc::new(SharedShowState::new());
    let running = Arc::new(AtomicBool::new(true));

    let listener = TcpListener::bind(&bind)?;
    tracing::info!(%bind, "renderer listening for the core");

    // The control thread starts before the outputs are built, not after. Which
    // connectors carry a show is the core's to say, and the only
    // way to be told is to be listening — a renderer that decided first would
    // have to guess, and the wrong guess puts a show on the Service Display.
    let control_state = Arc::clone(&state);
    let control_running = Arc::clone(&running);
    let control_thread = std::thread::Builder::new()
        .name("ovs-control".to_owned())
        .spawn(move || {
            if let Err(error) = control::serve(&listener, &control_state, &control_running) {
                tracing::error!("control thread stopped: {error}");
            }
            // The control thread ending means the core asked the renderer to
            // stop. Nothing else here ends a show.
            control_running.store(false, Ordering::Relaxed);
        })?;

    // What is on the network, for as long as this runs. Started here so the
    // first answers are arriving while the outputs are being built, and so a
    // core that connects early is told about an empty network rather than left
    // with nothing to tell a client.
    //
    // A machine with no NDI runtime says so once, here, and loses network
    // sources and nothing else. The runtime is installed by the operator rather
    // than carried by the package — `ovs_renderer::ndi` says why at length — so
    // its absence is an ordinary state of a machine and not a fault.
    let ndi_runtime = match Ndi::load() {
        Ok(ndi) => Some(ndi),
        Err(error) => {
            tracing::info!("{error}; no network sources or outputs will be offered");
            None
        }
    };
    let _discovery = ndi_runtime.map(|ndi| {
        let sources = Arc::clone(&state);
        Discovery::start(ndi, move |found| {
            sources.set_ndi_sources(found);
        })
    });

    let (make_compositor, outputs, stage, device) =
        build_outputs(&backend, output_count, mode, &state)?;

    render(
        make_compositor,
        outputs,
        &state,
        &running,
        stage,
        device,
        ndi_runtime,
    )?;

    running.store(false, Ordering::Relaxed);
    let _ = control_thread.join();
    tracing::info!("renderer stopped");
    Ok(())
}

/// Everything the render loop is built from: a way to make compositions, the
/// outputs that sample them, and — when they are physical — what it takes to
/// build them again.
type Stage = (
    CompositorFactory,
    Vec<Box<dyn OutputBackend>>,
    Option<DisplayStage>,
    // The device a network output's canvas is made on. Carried out separately
    // rather than read off the display stage, although that holds one too: a
    // machine publishing only to the network has no display stage at all, and a
    // second way to reach the same device is a second thing to get wrong. There
    // is one device here or none.
    Option<Arc<VulkanContext>>,
);

/// What the core is told about a set of displays.
///
/// One function, used for the first reading and for every one after it. Two
/// copies of this would be two answers to "what is this machine wired to", and
/// the one that ran less often would be the one that went out of date.
fn connectors_of(
    displays: &[ovs_renderer::vulkan::display::Display],
    gpu: &str,
) -> Vec<ovs_ipc::DiscoveredConnector> {
    displays
        .iter()
        .map(|display| ovs_ipc::DiscoveredConnector {
            gpu_pci_address: gpu.to_owned(),
            connector: display.connector.clone(),
            // A connector Vulkan enumerates is one the driver believes has
            // something on it; forcing at the kernel is what makes one that
            // does not appear (deploy/README.md).
            connected: true,
            model: display.model.clone(),
            modes: display
                .modes
                .iter()
                .map(|mode| ovs_ipc::ConnectorMode {
                    size: mode.size,
                    refresh_millihertz: mode.refresh_millihertz,
                })
                .collect(),
        })
        .collect()
}

/// Which connectors the show wants driven, and at what mode.
///
/// **The show decides, and nothing else adds to it.** A connector that is
/// available but carries no enabled output is not driven: deleting an output
/// takes its wall dark, and switching one off does too while the socket stays
/// reserved for it (owner, 2026-09-25).
///
/// `available` is the connector names this renderer is allowed to drive — its own
/// card's, with the role the show gave them. Names only, because what this decides
/// needs no modes, no models and no Vulkan: it is the show's own list, narrowed to
/// what exists.
///
/// An output with no connector name is a network output and belongs to no socket.
/// One naming another card is not this renderer's to drive.
fn wanted_modes(
    show: Option<&ovs_types::config::Show>,
    gpu_pci_address: &str,
    available: &[String],
) -> std::collections::BTreeMap<String, ovs_types::config::DisplayMode> {
    let mut wanted = std::collections::BTreeMap::new();
    let Some(show) = show else {
        return wanted;
    };

    for output in show.outputs.iter().filter(|output| output.enabled) {
        let Some(connector) = output.identity.connector.as_deref() else {
            continue;
        };
        if output.identity.gpu_pci_address.as_deref() != Some(gpu_pci_address) {
            continue;
        }
        if available
            .iter()
            .any(|name| name.eq_ignore_ascii_case(connector))
        {
            wanted.insert(connector.to_owned(), output.mode);
        }
    }

    wanted
}

/// A cheap reading of what the kernel says is plugged in.
///
/// Every connector's name, whether it is connected, and its EDID. Reading all of
/// them is tens of microseconds, against the 95 000 that asking Vulkan the same
/// question costs on this driver — which is why this exists: the expensive
/// question is worth asking when this answer changes and not otherwise.
///
/// A swap between two things that both present no EDID reads the same, and so
/// does one between two identical panels. Neither matters: what the driver
/// would offer is identical too.
///
/// Zero on a machine with no `/sys/class/drm`, which means the displays are read
/// once at startup and not again. That is the behaviour this whole path
/// replaced, and it is the honest one where the kernel cannot be asked.
fn plugged_in() -> u64 {
    use std::hash::{Hash, Hasher};

    let Ok(entries) = std::fs::read_dir("/sys/class/drm") else {
        return 0;
    };
    // Sorted, because a directory listing is in whatever order the filesystem
    // feels like and a hash over it would change for nothing.
    let mut connectors: Vec<std::path::PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.join("status").is_file())
        .collect();
    connectors.sort();

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for path in connectors {
        path.file_name().hash(&mut hasher);
        std::fs::read(path.join("status"))
            .unwrap_or_default()
            .hash(&mut hasher);
        // The EDID is what tells one panel from another, and it is what a
        // capture card presenting none differs from a monitor by.
        std::fs::read(path.join("edid"))
            .unwrap_or_default()
            .hash(&mut hasher);
    }
    hasher.finish()
}

/// Whether two readings describe the same sockets, panels and modes.
///
/// By what an operator would see, not by the handles: a handle changes whenever
/// the driver feels like it and means nothing to anybody. What matters is
/// whether the chooser a client is showing is still about what is plugged in.
fn same_connectors(
    one: &[ovs_ipc::DiscoveredConnector],
    other: &[ovs_ipc::DiscoveredConnector],
) -> bool {
    one.len() == other.len()
        && one.iter().zip(other).all(|(one, other)| {
            one.connector == other.connector
                && one.model == other.model
                && one.modes.len() == other.modes.len()
                && one.modes.iter().zip(&other.modes).all(|(one, other)| {
                    one.size == other.size && one.refresh_millihertz == other.refresh_millihertz
                })
        })
}

/// What rebuilding the physical outputs needs.
///
/// Held by the render thread because only it may touch an output. The modes it
/// is currently running are kept alongside so that a show asking for what is
/// already on the wall changes nothing — setting a mode that is already set
/// would still take the wall dark for a second.
pub struct DisplayStage {
    context: Arc<VulkanContext>,
    displays: Vec<ovs_renderer::vulkan::display::Display>,
    gpu_pci_address: String,
    running: std::collections::BTreeMap<String, ovs_types::config::DisplayMode>,
    /// The connector roles the last decision was made from.
    ///
    /// Kept so the building half sees exactly what the deciding half saw: the
    /// two run microseconds apart, and a role arriving between them would build
    /// a set nobody asked for.
    roles: std::sync::Arc<std::collections::BTreeMap<String, ovs_types::config::ConnectorRole>>,
    /// What the kernel said was plugged in, when the displays were last read.
    ///
    /// A hash, because nothing here needs to know *what* changed and a hash
    /// needs no mapping between the kernel's connector names and the driver's —
    /// `HDMI-A-5` against `HDMI-0` on the reference machine.
    plugged: u64,
    /// The mode each connector was last refused, so it is said once.
    ///
    /// The rebuild runs every second while the answer is wrong, and the refusal
    /// is a long line carrying everything the panel does offer. Two thousand
    /// identical copies of it bury the one line that said something new.
    refused: std::collections::BTreeMap<String, ovs_types::config::DisplayMode>,
}

/// Builds the compositor and the outputs the loop will drive.
///
/// A Vulkan backend that cannot open a device is a hard failure rather than a
/// quiet fall back to headless: an operator who asked for the GPU and silently
/// got nothing visible would have no way to tell.
///
/// The device is opened once and shared. A renderer process is one process on
/// one machine with one GPU, so a device per wall would divide the same card's
/// memory into separate allocations for no reason — and would make the
/// composition impossible, because an image rendered on one device cannot be
/// sampled by another. Binding every output to the compositor's canvas happens
/// here, which is the only place that connection is made.
fn build_outputs(
    backend: &str,
    count: usize,
    mode: DisplayMode,
    state: &Arc<SharedShowState>,
) -> Result<Stage, Box<dyn std::error::Error>> {
    let mut outputs: Vec<Box<dyn OutputBackend>> = Vec::with_capacity(count);

    let context = match backend {
        "vulkan" => {
            let context = Arc::new(VulkanContext::open(
                "ovs-renderer",
                wanted_device().as_deref(),
            )?);
            tracing::info!(
                device = %context.summary().name,
                kind = %context.summary().kind,
                dedicated_transfer = context.has_dedicated_transfer_queue(),
                outputs = count,
                "opened the GPU"
            );
            Some(context)
        }
        "display" => {
            let context = Arc::new(VulkanContext::open(
                "ovs-renderer",
                wanted_device().as_deref(),
            )?);
            return build_display_outputs(context, state);
        }
        "headless" => None,
        other => {
            return Err(format!(
                "unknown backend {other:?}; expected \"headless\", \"vulkan\" or \"display\""
            )
            .into())
        }
    };

    let Some(context) = context else {
        for index in 0..count {
            outputs.push(Box::new(HeadlessBackend::new(
                format!("Output {}", index + 1),
                mode,
            )));
        }
        // No device, so no canvas and no stream. A machine with no GPU
        // composes nothing anyway.
        return Ok((
            Box::new(|size| Ok(Box::new(HeadlessCompositor::sized(size)) as Box<dyn Compositor>)),
            outputs,
            None,
            None,
        ));
    };

    for index in 0..count {
        outputs.push(Box::new(VulkanOffscreenBackend::on(
            Arc::clone(&context),
            format!("Output {}", index + 1),
            mode,
        )?));
    }

    // A composition is made to whatever size the loaded show asks for, and
    // remade when that changes. The loop binds the outputs to it; the device is
    // all this closure has to carry.
    let device = Arc::clone(&context);
    let factory = move |size: SizePx| {
        let composition = DisplayMode::new(size, mode.refresh);
        VulkanOffscreenBackend::on(Arc::clone(&context), "Composition", composition)
            .map(|canvas| Box::new(canvas) as Box<dyn Compositor>)
            .map_err(|error| BackendError::UploadFailed {
                name: "the composition".to_owned(),
                reason: error.to_string(),
            })
    };

    Ok((Box::new(factory), outputs, None, Some(device)))
}

/// What one network output was built from.
///
/// Everything that decides whether the sender has to be built again: which
/// configured output it is, what it is called, and the size and rate it
/// publishes at. Compared whole, so any edit to any of them rebuilds that one
/// output and leaves the rest alone — including a rename, which is a rename on
/// the network too and so a different source to anybody watching.
#[derive(Debug, Clone, PartialEq)]
struct Streamed {
    id: ovs_types::id::OutputId,
    name: String,
    mode: DisplayMode,
}

/// Where the process that draws the System screen listens.
///
/// Its own runtime directory: the one place it can certainly create a socket,
/// and the unit that makes that directory is the unit that runs it.
/// `OVS_CONSOLE_SOCKET` moves it, for running the two by hand.
///
/// Unix, because its one caller is `build_console` and that is.
#[cfg(unix)]
fn console_socket() -> std::path::PathBuf {
    std::env::var("OVS_CONSOLE_SOCKET")
        .map_or_else(|_| "/run/ovs-console/console.sock".into(), Into::into)
}

/// What the System screen's output is currently built as.
///
/// The connector and the mode, so a pass that finds the same two does nothing.
/// Rebuilding costs a mode set, which takes the panel dark for about a second —
/// on the one screen somebody is looking at.
type ConsoleOutput = Option<(String, ovs_types::config::DisplayMode)>;

/// What is worth building, of what the show asks for.
///
/// Everything except a connector that has already refused this exact mode: that
/// one costs a release of every other output on the card and ends where it
/// began. A different mode on the same connector is worth trying, because the
/// refusal was about the pair; so is a mode on a connector that has never
/// refused anything.
///
/// Note what this is *not*: it does not stop the refused output being tried.
/// `build_panels` is still given everything the show asks for, so any rebuild
/// that happens for another reason retries it for free -- a mode lookup, no
/// mode set. What it stops is rebuilding *because* of it.
fn attainable_now(
    wanted: &std::collections::BTreeMap<String, ovs_types::config::DisplayMode>,
    refused: &std::collections::BTreeMap<String, ovs_types::config::DisplayMode>,
) -> std::collections::BTreeMap<String, ovs_types::config::DisplayMode> {
    wanted
        .iter()
        .filter(|(connector, mode)| refused.get(*connector) != Some(*mode))
        .map(|(connector, mode)| (connector.clone(), *mode))
        .collect()
}

/// Decides what every connector on this card should be, and rebuilds them all.
///
/// **The display outputs are one group.** Creating one while another is
/// presenting moves that other one's picture onto the new panel — measured on
/// two monitors on 2026-09-26, and the one arrangement out of six that failed.
/// So a change to any of them releases all of them and builds all of them, with
/// nothing presenting in between.
///
/// That is why the walls and the System screen are decided here together rather
/// than by two followers with an adoption each. Switching a System screen on
/// used to add an output to a set that was already running, which is precisely
/// the thing this driver will not survive.
///
/// Nothing happens at all when neither has moved, which is almost always: this
/// is two comparisons of a short map a second.
fn follow_displays(
    stage: &mut DisplayStage,
    state: &Arc<SharedShowState>,
    render: &mut RenderLoop,
    console_running: &mut ConsoleOutput,
    now: Instant,
) -> Result<(), String> {
    let panels = wanted_panels(stage, state);
    let console = wanted_console(stage, state);

    // **Measured against what can be built, not against what was asked for.**
    // A connector that has already refused this exact mode will refuse it
    // again, so comparing the wish with what is running finds a difference on
    // every pass and rebuilds the whole card for it -- and rebuilding releases
    // every display first, which takes the System screen down and the preview
    // stream with it.
    //
    // On the owner's machine on 2026-10-14 the show asked HDMI-0 for
    // 1920x1080@59.999 and the panel offers 59.940: **22 full rebuilds a
    // minute, for two hours**, each one a release-and-adopt of the one screen
    // somebody was looking at. The refusal was already remembered -- to say it
    // once rather than two thousand times -- and that same memory is what
    // decides here whether anything has moved.
    let attainable = attainable_now(&panels, &stage.refused);

    if attainable == stage.running && console == *console_running {
        return Ok(());
    }

    tracing::info!(
        walls = panels.len(),
        console = console.is_some(),
        "the connectors this card drives have changed; rebuilding all of them"
    );

    // **Released before anything is built.** The panels are dark from here until
    // the adoption below, which is a fraction of a second, and it is the whole
    // reason any of this is one function.
    render.release_displays();

    let (outputs, built) = build_panels(stage, &panels);

    // What was actually built, which is not always what was wanted: a panel that
    // refuses a mode leaves its entry out, and the next pass tries again.
    stage.running = built;

    // **The console half is Unix's.** `build_console` puts a CEF window on a DRM
    // connector and `adopt_displays` takes it only there, so calling either
    // unconditionally is what stopped this binary compiling on Windows.
    #[cfg(unix)]
    {
        let console_output = console
            .as_ref()
            .and_then(|(connector, mode)| build_console(stage, connector, *mode));
        *console_running = console_output
            .as_ref()
            .and(console.clone())
            .filter(|_| console_output.is_some());
        render.adopt_displays(outputs, console_output, now)
    }
    #[cfg(not(unix))]
    {
        // There is no System screen to run, so there is none running. Said
        // rather than left alone: `follow_displays` returned early above only
        // when nothing changed, and leaving a stale value here would have this
        // decide nothing changed the next time round.
        *console_running = None;
        render.adopt_displays(outputs, now)
    }
}

/// Which connector the System screen should be on, and at what mode.
///
/// From the connector roles rather than from the show: which socket carries the
/// operator's own screen is machine wiring, it outlives every show loaded on the
/// machine, and moving a show between servers must not move it.
fn wanted_console(stage: &DisplayStage, state: &Arc<SharedShowState>) -> ConsoleOutput {
    let chosen = ovs_renderer::vulkan::display::service_display(
        &stage.displays,
        &stage.gpu_pci_address,
        &stage.roles,
    )?;

    // What an operator chose in the admin screen, or the panel's own preference.
    // Absent is the ordinary answer: a monitor that reports an honest preference
    // is every monitor until one does not.
    let mode = state.console_mode().or_else(|| {
        chosen
            .preferred_mode()
            .map(|mode| ovs_types::config::DisplayMode {
                size: mode.size,
                refresh: mode.refresh(),
            })
    })?;

    Some((chosen.connector.clone(), mode))
}

/// Builds the System screen's output, with the source that feeds it.
///
/// **Only ever called with every display output already released**, for the
/// reason in `RenderLoop::release_displays`.
#[cfg(unix)]
fn build_console(
    stage: &DisplayStage,
    connector: &str,
    mode: ovs_types::config::DisplayMode,
) -> Option<(Box<dyn OutputBackend>, ovs_renderer::console::ConsoleSource)> {
    let display = stage
        .displays
        .iter()
        .find(|display| display.connector.eq_ignore_ascii_case(connector))?;

    // What the panel advertised, and failing that what the driver will build —
    // the same two steps the show's outputs take. A panel with no EDID
    // advertises a handful of fallback timings and nothing else, so a mode
    // somebody picked off a label is unreachable through the list.
    let display_mode = match display.mode_for(mode.size, mode.refresh) {
        Some(found) => found,
        None => match stage.context.force_mode(display, mode.size, mode.refresh) {
            Ok(forced) => forced,
            Err(error) => {
                tracing::error!(
                    connector,
                    width = mode.size.width,
                    height = mode.size.height,
                    "this panel does not offer that mode and the driver would not build it: \
                     {error}"
                );
                return None;
            }
        },
    };

    match VulkanDisplayBackend::on(
        Arc::clone(&stage.context),
        display.label(),
        display,
        display_mode,
        mode,
    ) {
        Ok(backend) => {
            tracing::info!(
                connector,
                width = mode.size.width,
                height = mode.size.height,
                "the System screen is on this connector"
            );
            let source =
                ovs_renderer::console::ConsoleSource::watching(console_socket(), mode.size);
            Some((Box::new(backend), source))
        }
        Err(error) => {
            tracing::error!(
                connector,
                "cannot drive the System screen on this connector: {error}"
            );
            None
        }
    }
}

/// Builds the streams the show asks for, and stops the ones it no longer does.
///
/// Nothing happens at all while the answer is unchanged, which is almost always:
/// this is one comparison of a short list a second.
fn follow_network_outputs(
    state: &Arc<SharedShowState>,
    render: &mut RenderLoop,
    device: Option<&Arc<VulkanContext>>,
    ndi: Option<&'static Ndi>,
    streaming: &mut Vec<Streamed>,
    said_no_streams: &mut bool,
    now: Instant,
) {
    // What this renderer is publishing, refreshed every time regardless of
    // whether the wanted set has moved: a sender learns its announced name
    // a moment after it is made, so the first look after building one
    // finds nothing to subtract and the next one finds it.
    state.set_published_ndi(render.published_ndi_names());

    let show = state.load();
    let wanted: Vec<Streamed> = show
        .show
        .as_deref()
        .map(|show| {
            show.outputs
                .iter()
                .filter(|output| output.enabled && output.is_on_the_network())
                .map(|output| Streamed {
                    id: output.id,
                    name: output.name.clone(),
                    mode: output.mode,
                })
                .collect()
        })
        .unwrap_or_default();

    if wanted == *streaming {
        return;
    }

    // Said once, not once a second. An operator installs the NDI runtime
    // themselves — `ovs_renderer::ndi` says why the package cannot carry it — so
    // its absence is an ordinary state of a machine, and a show that asks for a
    // stream on a machine without it is the one case where somebody needs to be
    // told which of the two is missing.
    let reason = match (ndi, device) {
        (Some(ndi), Some(device)) => Some((ndi, device)),
        (None, _) => {
            if !wanted.is_empty() && !*said_no_streams {
                *said_no_streams = true;
                tracing::warn!(
                    streams = wanted.len(),
                    "this show asks for network outputs and the NDI runtime is not installed; \
                     nothing is published"
                );
            }
            None
        }
        (Some(_), None) => {
            if !wanted.is_empty() && !*said_no_streams {
                *said_no_streams = true;
                tracing::warn!(
                    streams = wanted.len(),
                    "this show asks for network outputs and this machine has no GPU to compose \
                     them on; nothing is published"
                );
            }
            None
        }
    };
    let Some((ndi, device)) = reason else {
        return;
    };
    *said_no_streams = false;

    let mut built: Vec<Box<dyn OutputBackend>> = Vec::with_capacity(wanted.len());
    for want in &wanted {
        // A canvas of its own, exactly as a wall has: the pieces are composed
        // into it by the same code, and this file adds only where the finished
        // picture goes.
        match VulkanOffscreenBackend::on(Arc::clone(device), want.name.clone(), want.mode) {
            Ok(canvas) => built.push(Box::new(ovs_renderer::ndi::NdiOutput::new(
                ndi,
                want.id,
                &want.name,
                &want.name,
                Box::new(canvas),
            ))),
            // Named, and not retried every second: a canvas that cannot be made
            // is a device that is out of memory or gone, and restarting the
            // senders that did work once a second on the chance this one
            // recovers would be worse than leaving it out. It is tried again
            // when the show changes.
            Err(error) => tracing::error!(
                output = %want.name,
                stream = %want.name,
                "could not make a canvas for this network output: {error}"
            ),
        }
    }

    match render.adopt_network(built, now) {
        // What was asked for, even if one of them could not be made. The
        // alternative is rebuilding every working sender a second later to have
        // another go at the one that failed.
        Ok(()) => *streaming = wanted,
        Err(error) => tracing::error!("{error}"),
    }
}

/// The render thread.
///
/// Nothing in this function allocates per frame, touches the filesystem or waits
/// on the network. It presents, measures and sleeps.
fn render(
    make_compositor: CompositorFactory,
    outputs: Vec<Box<dyn OutputBackend>>,
    state: &Arc<SharedShowState>,
    running: &AtomicBool,
    mut displays: Option<DisplayStage>,
    device: Option<Arc<VulkanContext>>,
    ndi: Option<&'static Ndi>,
) -> Result<(), Box<dyn std::error::Error>> {
    let epoch = Instant::now();
    let mut render = RenderLoop::new(make_compositor, outputs, Arc::clone(state), epoch)?;
    render.watch_gpu(
        displays
            .as_ref()
            .map(|stage| stage.gpu_pci_address.as_str()),
    );
    let mut last_report = epoch;
    let mut last_preview = epoch;
    let mut last_modes = epoch;
    let mut last_streams = epoch;
    // What the network outputs were last built from.
    let mut streaming: Vec<Streamed> = Vec::new();
    let mut said_no_streams = false;
    // And which connector the System screen is on, at which mode.
    let mut console_output: ConsoleOutput = None;
    let mut last_fault = epoch;
    let mut said_idle = false;

    while running.load(Ordering::Relaxed) {
        let now = Instant::now();

        // Has the configuration changed what the hardware should be doing? Asked
        // once a second, not once a frame: the answer only changes when a show
        // is loaded or a connector is reassigned, and acting on it sets a mode,
        // which takes a wall dark for a second.
        //
        // **Every connector in one decision**, walls and System screen together.
        // Creating a display output while another is presenting moves that other
        // one's picture onto the new panel, so a change to any of them rebuilds
        // all of them — see `RenderLoop::release_displays`.
        if let Some(stage) = displays.as_mut() {
            if now.saturating_duration_since(last_modes) >= Duration::from_secs(1) {
                last_modes = now;
                if let Err(error) =
                    follow_displays(stage, state, &mut render, &mut console_output, now)
                {
                    tracing::error!("{error}");
                }
            }
        }

        // And which streams the show asks for. Once a second, like the modes
        // above: starting or stopping a sender changes what other machines on
        // the network can see, and doing that on a whim is worse than doing it a
        // second late. Not gated on there being displays — a machine that
        // publishes only to the network has no panels, and that is ordinary.
        if now.saturating_duration_since(last_streams) >= Duration::from_secs(1) {
            last_streams = now;
            follow_network_outputs(
                state,
                &mut render,
                device.as_ref(),
                ndi,
                &mut streaming,
                &mut said_no_streams,
                now,
            );
        }

        render.tick(now);

        // Two different states that used to look the same, ten times a second.
        //
        // **Nothing to drive** is ordinary. A machine whose only panel is the
        // System screen has no show outputs; so does one whose processors are
        // not powered up yet, and one waiting for a show to name a connector.
        // The renderer is still doing its job — it composes, and the previews it
        // produces are what the System screen and every client are looking at —
        // so it says so once and carries on quietly.
        //
        // **Everything faulted** is a fault, and stays one. It is said once a
        // second rather than once a pass, because a line every hundred
        // milliseconds is a log nobody can read afterwards.
        if render.output_count() == 0 {
            if !said_idle {
                said_idle = true;
                tracing::info!(
                    "no output is assigned to this renderer; composing and serving previews \
                     until one is"
                );
            }
        } else {
            said_idle = false;
            if !render.is_rendering()
                && now.saturating_duration_since(last_fault) >= Duration::from_secs(1)
            {
                last_fault = now;
                // Keep the process alive so the core can still reach it and an
                // operator can still see why; exiting would produce a restart
                // loop that explains nothing.
                tracing::error!(faults = ?render.faults(), "no output is being driven");
            }
        }

        // A once-a-second line is what makes a drop visible in a log after the
        // fact, when nobody was watching the console.
        // Taken on the render thread because only it may touch an output, and
        // encoded elsewhere because encoding is milliseconds. Faster than the
        // once-a-second report, because a picture that is a second old looks
        // broken where a number that is a second old is fine.
        if now.saturating_duration_since(last_preview) >= PREVIEW_INTERVAL {
            // Switched off, the readback and the encode do not happen at all —
            // which is the point. Anything already published is cleared, so a
            // console does not keep showing a picture that has stopped moving.
            if state.previews_enabled() {
                state.publish_previews(render.take_previews());
                state.publish_composition_preview(render.take_composition_preview());
            } else {
                state.publish_previews(Vec::new());
                state.publish_composition_preview(None);
            }
            last_preview = now;
        }

        if now.saturating_duration_since(last_report) >= Duration::from_secs(1) {
            // Published for the control thread to report to the core, and
            // logged so a drop is still visible after the fact when nobody was
            // watching the console.
            state.publish_telemetry(render.telemetry().to_vec());
            state.publish_gpu(render.gpu_telemetry());
            state.publish_playback(render.playback_telemetry());
            state.publish_frame_timings(render.frame_timings());
            // Where each playing layer has got to, so a client can draw a
            // progress bar on the pad an operator is about to cue away from.
            state.publish_playheads(render.playheads().to_vec());
            state.publish_missing_outputs(
                render
                    .missing_outputs()
                    .iter()
                    .map(|missing| ovs_types::telemetry::MissingOutput {
                        output_id: missing.output_id,
                        name: missing.name.clone(),
                        reason: missing.reason.clone(),
                    })
                    .collect(),
            );
            let resident = render.resident_media();
            for output in render.telemetry() {
                tracing::info!(
                    output = %output.name,
                    fps = output.actual_fps,
                    dropped = output.dropped_frames.since_reset,
                    lifetime_dropped = output.dropped_frames.lifetime,
                    media = resident,
                    "output"
                );
            }
            last_report = now;
        }

        let deadline = render.next_deadline(now);
        let wait = deadline
            .saturating_duration_since(Instant::now())
            .min(MAX_SLEEP);
        if !wait.is_zero() {
            std::thread::sleep(wait);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A show with one output on `connector`, at 1920x1080p60.
    fn show_on(connector: Option<&str>, gpu: &str, enabled: bool) -> ovs_types::config::Show {
        let mut show = ovs_types::config::Show::new("Show");
        let mut output = ovs_types::config::OutputConfig::new(
            "Wall",
            ovs_types::config::DisplayMode::new(SizePx::new(1920, 1080), FrameRate::FPS_60),
        );
        output.enabled = enabled;
        output.identity = ovs_types::config::OutputIdentity {
            gpu_pci_address: Some(gpu.to_owned()),
            connector: connector.map(str::to_owned),
            ..ovs_types::config::OutputIdentity::default()
        };
        show.outputs.push(output);
        show
    }

    const THIS_CARD: &str = "0000:01:00.0";

    /// A mode, short.
    fn mode(width: u32, height: u32, rate: FrameRate) -> ovs_types::config::DisplayMode {
        ovs_types::config::DisplayMode::new(SizePx::new(width, height), rate)
    }

    #[test]
    fn a_mode_already_refused_is_not_worth_rebuilding_the_card_for() {
        // The owner's machine, 2026-10-14: the show asks HDMI-0 for 59.999 and
        // the panel offers 59.940. Before this, the difference between the wish
        // and what was running was found again on every pass, and the whole
        // card -- System screen included -- was released and rebuilt for it 22
        // times a minute.
        let asked = FrameRate::new(59_999, 1_000);
        let wanted =
            std::collections::BTreeMap::from([("HDMI-0".to_owned(), mode(1920, 1080, asked))]);
        let refused =
            std::collections::BTreeMap::from([("HDMI-0".to_owned(), mode(1920, 1080, asked))]);

        assert!(attainable_now(&wanted, &refused).is_empty());
        // Which is what the running set is when nothing could be built, so the
        // pass finds nothing to do.
        assert_eq!(
            attainable_now(&wanted, &refused),
            std::collections::BTreeMap::new()
        );
    }

    #[test]
    fn a_different_mode_on_the_same_connector_is_tried() {
        // The operator picks 59.94 after being told what the panel offers. That
        // is news, and it has to be acted on at once.
        let refused = std::collections::BTreeMap::from([(
            "HDMI-0".to_owned(),
            mode(1920, 1080, FrameRate::new(59_999, 1_000)),
        )]);
        let wanted = std::collections::BTreeMap::from([(
            "HDMI-0".to_owned(),
            mode(1920, 1080, FrameRate::new(59_940, 1_000)),
        )]);

        assert_eq!(attainable_now(&wanted, &refused), wanted);
    }

    #[test]
    fn a_connector_that_never_refused_anything_is_untouched() {
        let refused = std::collections::BTreeMap::from([(
            "HDMI-0".to_owned(),
            mode(1920, 1080, FrameRate::new(59_999, 1_000)),
        )]);
        let wanted = std::collections::BTreeMap::from([
            (
                "HDMI-0".to_owned(),
                mode(1920, 1080, FrameRate::new(59_999, 1_000)),
            ),
            ("DP-4".to_owned(), mode(2560, 1440, FrameRate::FPS_60)),
        ]);

        let worth = attainable_now(&wanted, &refused);
        assert_eq!(worth.len(), 1);
        assert!(worth.contains_key("DP-4"));
    }

    #[test]
    fn a_wall_the_show_has_is_driven() {
        let show = show_on(Some("HDMI-0"), THIS_CARD, true);
        let wanted = wanted_modes(Some(&show), THIS_CARD, &["HDMI-0".to_owned()]);

        assert_eq!(wanted.len(), 1);
        assert_eq!(
            wanted.get("HDMI-0").map(|mode| mode.size),
            Some(SizePx::new(1920, 1080))
        );
    }

    #[test]
    fn a_socket_the_show_has_no_output_on_is_not_driven() {
        // **The owner's fault** (2026-09-25): "Output HDMI-0 gelöscht und dann
        // läuft alles weiter statt dass das display aus geht, weil es exisitert
        // in der show nicht mehr."
        //
        // Every available connector used to be added back here with whatever it
        // was running, so the wall stayed lit — and, having no pieces, drew the
        // whole composition.
        let show = ovs_types::config::Show::new("Show");
        let wanted = wanted_modes(Some(&show), THIS_CARD, &["HDMI-0".to_owned()]);

        assert!(wanted.is_empty(), "a deleted wall stayed lit: {wanted:?}");
    }

    #[test]
    fn a_wall_switched_off_is_not_driven() {
        // It keeps its socket — the role is derived from the show whether or not
        // the output is enabled, so nothing else takes it between two cues — and
        // holding a socket is not driving it.
        let show = show_on(Some("HDMI-0"), THIS_CARD, false);
        let wanted = wanted_modes(Some(&show), THIS_CARD, &["HDMI-0".to_owned()]);

        assert!(wanted.is_empty(), "a disabled wall was lit: {wanted:?}");
    }

    #[test]
    fn a_wall_on_another_card_is_not_this_renderer_s() {
        let show = show_on(Some("HDMI-0"), "0000:00:02.0", true);
        let wanted = wanted_modes(Some(&show), THIS_CARD, &["HDMI-0".to_owned()]);

        assert!(
            wanted.is_empty(),
            "another card's wall was taken: {wanted:?}"
        );
    }

    #[test]
    fn a_wall_on_a_socket_that_is_not_there_is_not_driven() {
        // Unplugged, or not this renderer's to drive. Nothing to drive it on.
        let show = show_on(Some("DP-9"), THIS_CARD, true);
        let wanted = wanted_modes(Some(&show), THIS_CARD, &["HDMI-0".to_owned()]);

        assert!(wanted.is_empty(), "a missing socket was driven: {wanted:?}");
    }

    #[test]
    fn an_output_with_no_socket_is_a_stream_and_takes_none() {
        let show = show_on(None, THIS_CARD, true);
        let wanted = wanted_modes(Some(&show), THIS_CARD, &["HDMI-0".to_owned()]);

        assert!(wanted.is_empty(), "a stream took a socket: {wanted:?}");
    }

    #[test]
    fn no_show_lights_nothing() {
        let wanted = wanted_modes(None, THIS_CARD, &["HDMI-0".to_owned()]);
        assert!(wanted.is_empty());
    }

    fn connector(
        name: &str,
        model: Option<&str>,
        modes: &[(u32, u32, u32)],
    ) -> ovs_ipc::DiscoveredConnector {
        ovs_ipc::DiscoveredConnector {
            gpu_pci_address: "0000:01:00.0".to_owned(),
            connector: name.to_owned(),
            connected: true,
            model: model.map(str::to_owned),
            modes: modes
                .iter()
                .map(|(width, height, millihertz)| ovs_ipc::ConnectorMode {
                    size: SizePx::new(*width, *height),
                    refresh_millihertz: *millihertz,
                })
                .collect(),
        }
    }

    /// The same machine reads the same, so nothing is republished for nothing.
    #[test]
    fn an_unchanged_machine_is_not_reported_as_changed() {
        let before = vec![connector(
            "HDMI-0",
            Some("Sony LMD-2110W"),
            &[(1920, 1080, 59940)],
        )];
        let after = before.clone();
        assert!(same_connectors(&before, &after));
    }

    /// A different panel on the same socket is a different machine to an
    /// operator, whose chooser is about what is plugged in.
    #[test]
    fn a_different_panel_on_the_same_socket_is_a_change() {
        let before = vec![connector(
            "HDMI-0",
            Some("Sony LMD-2110W"),
            &[(1920, 1080, 59940)],
        )];
        let after = vec![connector(
            "HDMI-0",
            Some("Capture card"),
            &[(1920, 1080, 59940)],
        )];
        assert!(!same_connectors(&before, &after));
    }

    /// **The one this was written for.** A capture card and a monitor can sit on
    /// the same socket and report no model either way; what tells them apart is
    /// the modes, and a comparison that ignored those would leave a client
    /// offering five fallback timings for a panel that does 1920x1080 (owner,
    /// 2026-09-25).
    #[test]
    fn the_same_socket_with_different_modes_is_a_change() {
        let fallbacks = vec![connector(
            "HDMI-0",
            None,
            &[(640, 480, 59940), (800, 600, 56250), (1024, 768, 60004)],
        )];
        let real = vec![connector(
            "HDMI-0",
            None,
            &[(1920, 1080, 59940), (1920, 1080, 50000), (1280, 720, 59940)],
        )];
        assert!(!same_connectors(&fallbacks, &real));
    }

    /// A rate that moved is a change, however slightly: 60.000 and 59.940 are
    /// different modes and the driver treats them as such.
    #[test]
    fn a_rate_that_moved_is_a_change() {
        let before = vec![connector("HDMI-0", None, &[(1920, 1080, 59940)])];
        let after = vec![connector("HDMI-0", None, &[(1920, 1080, 60000)])];
        assert!(!same_connectors(&before, &after));
    }

    /// A socket appearing or going away is a change, read either way round.
    #[test]
    fn a_socket_appearing_is_a_change() {
        let before = vec![connector("HDMI-0", None, &[(1920, 1080, 59940)])];
        let mut after = before.clone();
        after.push(connector("DP-3", None, &[(1920, 1080, 60000)]));
        assert!(!same_connectors(&before, &after));
        assert!(!same_connectors(&after, &before));
    }
}
