//! Draws the System screen off-screen, so a renderer can put it on a connector.
//!
//! ```text
//! ovs-console --url=http://127.0.0.1:7420/console
//! ovs-console --url=http://127.0.0.1:7420/console --socket=/tmp/c.sock --measure=10
//! ```
//!
//! The size is the renderer's to say, over the socket, because the connector is
//! the renderer's. `--size` is only what the page is laid out at until one
//! connects.
//!
//! # What this replaces, and why
//!
//! The System screen used to be `cage` — a kiosk compositor — running chromium.
//! A compositor takes a **card**, and every connector on it: wlroots is given
//! `WLR_DRM_DEVICES` and there is no variable for a connector, which was
//! established by reading the ones the linked wlroots actually knows. So the
//! System screen spread itself across every panel the card had, and the renderer
//! could not have the card at all — on a machine with one GPU the two could
//! never coexist (owner, 2026-09-25).
//!
//! The owner's rule is that a socket does one job and the card stays flexible:
//! "Das System Display KANN und DARF KEINE ganze Karte nehmen. Es darf sich nur
//! den Ausgang nehmen."
//!
//! So this asks the graphics stack for **nothing**. It lays the page out, paints
//! it into memory, and hands the pixels over. The renderer — which owns the card
//! already — puts them on one connector like any other output, and every other
//! connector stays a wall's to have.
//!
//! # Its own process, which is not a way round anything
//!
//! CEF is multi-process by construction: a browser process, and a child per tab
//! and per GPU. Putting the browser process *inside* the renderer would not
//! remove those children — it would add them to the process that has to put a
//! frame on a wall on time, and it would mean the renderer binary being
//! re-executed as a Chromium helper.
//!
//! What made the old arrangement a workaround was not that it was another
//! process. It was that the other process wanted a card. This one wants none.

mod page;
mod serve;

use std::sync::Arc;
use std::time::{Duration, Instant};

use cef::args::Args;
use cef::{
    do_message_loop_work, execute_process, initialize, shutdown, CefString, ImplBrowser,
    ImplBrowserHost, ImplCommandLine, MouseButtonType, MouseEvent, Settings,
};
use ovs_console_wire::ToConsole;

use crate::page::{AppBuilder, ClientBuilder, Loading, Painted, Panel, Size, FRAME_RATE};
use crate::serve::{Server, Wanted};

/// Where the renderer looks for the pictures, when nothing says otherwise.
///
/// The console's own runtime directory: the one place it can certainly create a
/// socket, and the unit that creates it is the unit that runs this.
const SOCKET: &str = "/run/ovs-console/console.sock";

/// How long to wait before loading the page again after it would not load.
///
/// Long enough that a server coming up is not hammered while it does, short
/// enough that nobody watching the panel at boot sees an error page for more
/// than a moment.
const RELOAD_AFTER: Duration = Duration::from_secs(1);

/// What CEF calls "the left button is down", as a modifier on other events.
///
/// `EVENTFLAG_LEFT_MOUSE_BUTTON`, which a move has to carry for a drag to be a
/// drag rather than a cursor wandering across a page with nothing held. Taken
/// from the generated bindings rather than written as 16.
const LEFT_HELD: u32 = cef::sys::cef_event_flags_t::EVENTFLAG_LEFT_MOUSE_BUTTON.0;

/// How long the message loop sleeps between pumps.
///
/// CEF is asked for an external message pump, which means nothing happens in it
/// unless this calls `do_message_loop_work`. Sixteen milliseconds is a frame at
/// sixty: fast enough that a press is handled within one repaint of a page
/// capped at thirty, and slow enough that a still page costs a thread almost
/// nothing.
const PUMP: Duration = Duration::from_millis(16);

fn main() -> std::process::ExitCode {
    let filter = tracing_subscriber::EnvFilter::try_from_env("OVS_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    // **Before anything else is asked of CEF.** CEF 154 versions its API, and
    // every entry point checks that a version has been chosen; without this the
    // first call — `command_line_create`, below — aborts the process with
    // SIGTRAP and prints nothing at all, because it happens before any logging
    // exists to print with.
    let _ = cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0);

    let args = Args::new();
    let Some(command_line) = args.as_cmd_line() else {
        tracing::error!("this process has no command line to read");
        return std::process::ExitCode::FAILURE;
    };

    // **Every child comes back through here.** CEF starts its helpers by running
    // this same binary again with `--type=`, so the first thing any of them must
    // do is hand control to CEF and leave. Only the process without that switch
    // is the browser, and only it goes on to the rest of this file.
    let kind = CefString::from("type");
    let is_browser = command_line.has_switch(Some(&kind)) != 1;
    let mut app = AppBuilder::console();
    let executed = execute_process(
        Some(args.as_main_args()),
        Some(&mut app),
        std::ptr::null_mut(),
    );

    if !is_browser {
        // A helper: it has done its work inside `execute_process` and must not
        // initialise anything of its own.
        return if executed >= 0 {
            std::process::ExitCode::SUCCESS
        } else {
            std::process::ExitCode::FAILURE
        };
    }
    if executed != -1 {
        tracing::error!(executed, "the browser process could not be started");
        return std::process::ExitCode::FAILURE;
    }

    let mut settings = Settings {
        windowless_rendering_enabled: 1,
        // Nothing here runs a message loop of its own, so CEF's is pumped by the
        // loop below. Letting CEF run its own would take this thread for ever
        // and leave nowhere to hand frames on from.
        external_message_pump: 1,
        // **No sandbox.** Chromium's on Linux wants a `chrome-sandbox` helper
        // owned by root and setuid, or user namespaces this account may use; a
        // service account in a rack has neither. What it would be sandboxing is
        // a page this machine serves to itself — `127.0.0.1`, and nothing else
        // is ever opened here — so the trade is a process we wrote against a
        // page we wrote.
        no_sandbox: 1,
        ..Default::default()
    };

    // **A cache of its own.** The default is shared, and CEF warns that two
    // processes using it fight over a singleton lock — which on a machine
    // running this beside a development copy is two browsers arguing in
    // somebody's home directory rather than drawing.
    let cache = std::env::var("OVS_CONSOLE_CACHE").unwrap_or_else(|_| {
        std::env::var("HOME").map_or_else(
            |_| "/tmp/ovs-console".to_owned(),
            |home| format!("{home}/.cache/ovs-console"),
        )
    });
    settings.root_cache_path = CefString::from(cache.as_str());

    // **Where CEF's own files are.** It looks for `icudtl.dat`, the `.pak`
    // files and the locales beside the executable; ours is in `target/` while
    // the framework is wherever `export-cef-dir` put it. Told rather than moved.
    //
    // Left alone when nothing says otherwise, because a packaged install puts
    // them beside the binary and then the defaults are right.
    if let Ok(framework) = std::env::var("CEF_PATH") {
        settings.resources_dir_path = CefString::from(framework.as_str());
        settings.locales_dir_path = CefString::from(format!("{framework}/locales").as_str());
    }

    if initialize(
        Some(args.as_main_args()),
        Some(&settings),
        Some(&mut app),
        std::ptr::null_mut(),
    ) != 1
    {
        tracing::error!("CEF would not initialise");
        return std::process::ExitCode::FAILURE;
    }

    let outcome = run(&command_line);
    shutdown();
    outcome
}

/// Opens the page and pumps CEF until it is time to stop.
fn run(command_line: &cef::CommandLine) -> std::process::ExitCode {
    let url = switch_value(command_line, "url").unwrap_or_else(|| {
        // The machine's own status page, on the port a person would type.
        "http://127.0.0.1/".to_owned()
    });
    // What the page is laid out at until a renderer says what its connector is.
    // Not a size anything is expected to stay at: the first `Resize` over the
    // socket replaces it.
    let size = switch_value(command_line, "size")
        .and_then(|value| parse_size(&value))
        .unwrap_or(Size {
            width: 1920,
            height: 1080,
        });
    // How long to run before reporting and stopping. Only set by hand, for
    // measuring; without it this runs until it is stopped.
    let measure = switch_value(command_line, "measure").and_then(|value| value.parse::<u64>().ok());
    let socket = switch_value(command_line, "socket").unwrap_or_else(|| SOCKET.to_owned());

    let painted = Arc::new(Painted::default());
    let panel = Arc::new(Panel::new(size));
    let wanted = Arc::new(Wanted::default());
    let loading = Arc::new(Loading::default());

    // **Before the browser.** Bound first so a renderer that is already waiting
    // finds the socket at once, and so a console that cannot serve its pictures
    // says so instead of drawing happily into a slot nobody will ever read.
    let server = match Server::start(&socket, Arc::clone(&painted), Arc::clone(&wanted)) {
        Ok(server) => server,
        Err(error) => {
            tracing::error!(socket, "the renderer has nowhere to connect: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };

    let window = cef::WindowInfo {
        windowless_rendering_enabled: 1,
        ..Default::default()
    };
    let browser_settings = cef::BrowserSettings {
        windowless_frame_rate: FRAME_RATE,
        ..Default::default()
    };

    let browser = cef::browser_host_create_browser_sync(
        Some(&window),
        Some(&mut ClientBuilder::painting(
            Arc::clone(&panel),
            Arc::clone(&painted),
            Arc::clone(&loading),
        )),
        Some(&url.as_str().into()),
        Some(&browser_settings),
        None,
        None,
    );
    let Some(browser) = browser else {
        tracing::error!(%url, "the page could not be opened");
        return std::process::ExitCode::FAILURE;
    };

    tracing::info!(
        %url,
        socket = %server.path().display(),
        width = size.width,
        height = size.height,
        rate = FRAME_RATE,
        "the System screen is being drawn"
    );

    let began = Instant::now();
    let deadline = measure.map(|seconds| began + Duration::from_secs(seconds));
    // When to load the page again, once one attempt has failed.
    let mut reload_at: Option<Instant> = None;
    // Reused every pass, so handing input to the browser allocates nothing.
    let mut happened: Vec<ToConsole> = Vec::new();
    // Whether the button is down, so a move while it is carries the modifier
    // that makes a drag a drag.
    let mut held = false;
    // What the last report said, so the rate is frames *since* then rather than
    // an average that flattens exactly the moment worth seeing.
    let mut reported = (Instant::now(), 0u64);

    loop {
        do_message_loop_work();

        // **The renderer's panel, applied on the thread that owns CEF.** Setting
        // the size is only half of it: CEF asks for the rectangle when it is told
        // to, so without `was_resized` the page goes on being drawn at the old
        // size and the new one takes effect at whatever unrelated moment
        // something else invalidates the view.
        // **A page that would not load is loaded again.** Not from inside the
        // load handler: everything CEF owns belongs to the thread that pumps it,
        // and this is that thread.
        if loading.take_failure() {
            reload_at = Some(Instant::now() + RELOAD_AFTER);
        }
        if reload_at.is_some_and(|at| Instant::now() >= at) {
            reload_at = None;
            if let Some(host) = browser.host() {
                // `reload_ignore_cache`, because what is being waited for is a
                // server that was not there — and a cached copy of its error is
                // the one thing that must not be shown instead.
                let _ = host;
                browser.reload_ignore_cache();
                tracing::debug!(
                    failures = loading.failures(),
                    "loading the System screen's page again"
                );
            }
        }

        // **What somebody did on the panel.** On this thread, because everything
        // CEF owns belongs to the thread that pumps it.
        wanted.take_input(&mut happened);
        if !happened.is_empty() {
            if let Some(host) = browser.host() {
                for what in happened.drain(..) {
                    let modifiers = if held { LEFT_HELD } else { 0 };
                    match what {
                        ToConsole::Pointer { x, y } => {
                            let event = MouseEvent {
                                x: x as _,
                                y: y as _,
                                modifiers,
                            };
                            host.send_mouse_move_event(Some(&event), 0);
                        }
                        ToConsole::Button { x, y, down } => {
                            held = down;
                            let event = MouseEvent {
                                x: x as _,
                                y: y as _,
                                // The button being pressed is part of the state
                                // of the event that presses it, which is what
                                // makes a down and its up a click rather than
                                // two unrelated things.
                                modifiers: if down { LEFT_HELD } else { 0 },
                            };
                            // One click, never two. The System screen is a panel
                            // people press with a finger, and nothing on it is
                            // reached by pressing twice quickly.
                            host.send_mouse_click_event(
                                Some(&event),
                                MouseButtonType::LEFT,
                                i32::from(!down),
                                1,
                            );
                        }
                        ToConsole::Scroll { x, y, dx, dy } => {
                            let event = MouseEvent {
                                x: x as _,
                                y: y as _,
                                modifiers,
                            };
                            host.send_mouse_wheel_event(Some(&event), dx, dy);
                        }
                        // Handled where it arrives, because it is applied to the
                        // panel rather than sent into the page.
                        ToConsole::Resize { .. } => {}
                    }
                }
            } else {
                happened.clear();
                tracing::warn!("the browser has no host to press");
            }
        }

        // A size that is already the one in use is dropped rather than applied:
        // a renderer says what its connector is whenever it connects, and
        // `was_resized` costs a full repaint of the page.
        if let Some(size) = wanted.take_resize().filter(|size| *size != panel.get()) {
            if let Some(host) = browser.host() {
                panel.set(size);
                host.was_resized();
                tracing::info!(
                    width = size.width,
                    height = size.height,
                    "the page is being laid out for the connector it is on"
                );
            } else {
                // Left at the size it was. A page laid out at a size CEF was
                // never told about would be drawn at the old one and sent with
                // the new one's numbers on it.
                tracing::warn!("the browser has no host to resize");
            }
        }

        let now = Instant::now();
        if now.duration_since(reported.0) >= Duration::from_secs(1) {
            let painted_now = painted.count();
            let latest = painted.latest();
            tracing::info!(
                frames = painted_now - reported.1,
                total = painted_now,
                size = ?latest.as_ref().map(|frame| (frame.width, frame.height)),
                ink = ?latest.as_ref().map(|frame| format!("{:.1}%", frame.ink() * 100.0)),
                "painted"
            );
            reported = (now, painted_now);
        }

        if deadline.is_some_and(|deadline| now >= deadline) {
            let total = painted.count();
            let seconds = began.elapsed().as_secs_f64();
            let latest = painted.latest();
            tracing::info!(
                total,
                seconds = format!("{seconds:.1}"),
                a_second = format!("{:.1}", total as f64 / seconds),
                size = ?latest.as_ref().map(|frame| (frame.width, frame.height)),
                ink = ?latest.as_ref().map(|frame| format!("{:.1}%", frame.ink() * 100.0)),
                "measured"
            );
            return std::process::ExitCode::SUCCESS;
        }

        std::thread::sleep(PUMP);
    }
}

/// Reads `--name=value` from the command line, if it is there.
///
/// **The `=` is not optional.** Chromium's own parser is doing this, and it takes
/// the value from the switch itself; `--size 1024x600` is a switch with no value
/// and a separate argument, which would quietly become the default. Said out loud
/// rather than guessed at, because a page laid out at 1920x1080 on a 1024x600
/// panel looks like a bug in the layout.
fn switch_value(command_line: &cef::CommandLine, name: &str) -> Option<String> {
    let switch = CefString::from(name);
    if command_line.has_switch(Some(&switch)) != 1 {
        return None;
    }
    let value = CefString::from(&command_line.switch_value(Some(&switch))).to_string();
    if value.is_empty() {
        tracing::warn!("--{name} was given nothing; write it as --{name}=value");
        return None;
    }
    Some(value)
}

/// Reads `1920x1080`.
fn parse_size(value: &str) -> Option<Size> {
    let (width, height) = value.split_once(['x', 'X'])?;
    Some(Size {
        width: width.trim().parse().ok()?,
        height: height.trim().parse().ok()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_size_is_read_the_way_a_person_writes_one() {
        assert_eq!(
            parse_size("1920x1080"),
            Some(Size {
                width: 1920,
                height: 1080
            })
        );
        assert_eq!(
            parse_size(" 1024 X 600 "),
            Some(Size {
                width: 1024,
                height: 600
            })
        );
    }

    #[test]
    fn a_size_that_is_not_one_is_refused_rather_than_guessed_at() {
        // A default is better than a guess: a page laid out at half a size an
        // operator mistyped is harder to notice than one at the size the code
        // says it falls back to.
        for hostile in ["", "1920", "1920x", "x1080", "wide x tall", "1920*1080"] {
            assert_eq!(parse_size(hostile), None, "{hostile:?}");
        }
    }

    #[test]
    fn the_latest_frame_is_the_only_one_kept() {
        // One slot, like the NDI receiver's: a frame that has been overtaken is
        // a picture nobody will ever want, and holding it only delays the one
        // they do.
        let painted = Painted::default();
        assert_eq!(painted.count(), 0);
        assert!(painted.latest().is_none());

        for size in [(2u32, 2u32), (4, 4)] {
            painted.put(page::Frame {
                width: size.0,
                height: size.1,
                pixels: vec![0; (size.0 * size.1 * 4) as usize],
            });
        }

        assert_eq!(painted.count(), 2, "both arrivals were counted");
        let latest = painted.latest().expect("a frame");
        assert_eq!(
            (latest.width, latest.height),
            (4, 4),
            "the newer one is kept"
        );
    }
}
