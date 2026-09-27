//! The System screen's pictures, arriving from the process that draws them.
//!
//! `ovs-console` hosts a browser with no window, paints the page into memory and
//! listens on a Unix socket. This connects to it, says how big the connector is,
//! and keeps the newest frame in a slot for the render loop to take.
//!
//! # Why the renderer connects, when it listens for everything else
//!
//! The control channel has the core connecting to the renderer, because the
//! renderer is the thing that must survive the other going away. Here the same
//! reasoning points the other way: the console is the one **holding state** — a
//! page with a history, a session, a scroll position — and the renderer holds
//! none of it. A console that outlives a renderer restart is a System screen
//! that comes back where it was; the reverse would be a browser thrown away
//! because something else was upgraded.
//!
//! So this is the first outbound connection in this crate, and it retries for
//! ever. A console that is not up yet is the ordinary case at boot.
//!
//! # One frame, kept, not queued
//!
//! The same slot the NDI receiver uses, for the same reason: a picture that has
//! been overtaken is one nobody will ever want, and holding it only delays the
//! one they do. `arrived` is the cheap half — the render loop compares a count
//! and takes the frame only when it moved.

use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ovs_types::geometry::SizePx;

use crate::media::DecodedImage;

/// How long to wait before trying the socket again.
///
/// The console is on this machine and starts in about a second, so this is
/// short. What it must not be is zero: a socket that is not there yet would
/// otherwise be a thread spinning on `connect` for the whole of boot.
const RETRY: Duration = Duration::from_millis(500);

/// What the threads and the render loop share.
#[derive(Debug)]
struct Shared {
    /// The newest frame, or none yet.
    latest: Mutex<Option<Arc<DecodedImage>>>,
    /// How many have arrived since this started.
    arrived: AtomicU64,
    /// Whether the socket is up right now.
    connected: AtomicBool,
    /// Set when the source is dropped, so the threads stop retrying.
    stop: AtomicBool,
    /// The half of the socket that input goes out on, while there is one.
    ///
    /// Behind a lock because two threads reach it: the reader puts it here when
    /// it connects and takes it away when it does not, and the input watcher
    /// writes through it. Never the render thread — a write to a socket can
    /// block, and the render thread has sixteen milliseconds.
    writer: Mutex<Option<UnixStream>>,
}

/// A connection to the console, kept up, with the newest frame in a slot.
///
/// And the panel's input, going the other way. One object for one panel: it
/// exists while a connector is the System screen, and the devices are watched
/// for exactly that long.
#[derive(Debug)]
pub struct ConsoleSource {
    path: PathBuf,
    size: SizePx,
    shared: Arc<Shared>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// Dropped before the connection, so nothing is sent into a closing socket.
    input: Option<crate::input::Watcher>,
}

impl ConsoleSource {
    /// Starts watching `path`, telling the console the page is `size`.
    ///
    /// **The size is fixed for the life of this.** It is the connector's mode,
    /// and a connector whose mode changes is a connector whose output is rebuilt
    /// — so the caller drops this and makes another, which costs one
    /// reconnection and saves this from having to carry a second channel back.
    #[must_use]
    pub fn watching(path: impl AsRef<Path>, size: SizePx) -> Self {
        let path = path.as_ref().to_path_buf();
        let shared = Arc::new(Shared {
            latest: Mutex::new(None),
            arrived: AtomicU64::new(0),
            connected: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            writer: Mutex::new(None),
        });

        let working = Arc::clone(&shared);
        let watching = path.clone();
        let thread = std::thread::Builder::new()
            .name("ovs-console-watch".to_owned())
            .spawn(move || watch(&watching, size, &working))
            .inspect_err(|error| {
                tracing::error!("the System screen has nobody watching for it: {error}");
            })
            .ok();

        // The panel's own devices, sending through the same socket. Started
        // after the reader, so the first thing on that socket is the size.
        let sending = Arc::clone(&shared);
        let input = Some(crate::input::Watcher::watching(size, move |what| {
            send_to_console(&sending, what);
        }));

        Self {
            path,
            size,
            shared,
            thread,
            input,
        }
    }

    /// Where it is watching.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The size it asked the console for.
    #[must_use]
    pub const fn size(&self) -> SizePx {
        self.size
    }

    /// How many frames have arrived.
    ///
    /// Compare this rather than taking the frame: an unchanged count is the
    /// whole per-frame cost of a System screen nobody is touching, which is most
    /// of the time.
    #[must_use]
    pub fn arrived(&self) -> u64 {
        self.shared.arrived.load(Ordering::Acquire)
    }

    /// Whether the console is connected right now.
    #[must_use]
    pub fn connected(&self) -> bool {
        self.shared.connected.load(Ordering::Relaxed)
    }

    /// The newest frame, if one has arrived.
    #[must_use]
    pub fn frame(&self) -> Option<Arc<DecodedImage>> {
        self.shared
            .latest
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl Drop for ConsoleSource {
    fn drop(&mut self) {
        // The devices first. Everything else here is about letting a socket go,
        // and a press arriving while that happens has nowhere to be.
        drop(self.input.take());
        self.shared.stop.store(true, Ordering::Relaxed);
        // Not joined. The thread is blocked reading a socket nobody is going to
        // write to, and waiting for it would hold up whatever is rebuilding the
        // output — which on a mode change is the render thread. It notices the
        // flag when its read ends, and ends; until then it holds an `Arc` and
        // nothing else.
        drop(self.thread.take());
    }
}

/// Sends one thing somebody did on the panel, if the console is connected.
///
/// Dropped when it is not. A press with nobody to send it to is a press into a
/// process that is being restarted, and holding it to deliver afterwards would
/// mean the console waking up to a press from before it existed.
fn send_to_console(shared: &Arc<Shared>, what: ovs_console_wire::ToConsole) {
    let mut writer = shared
        .writer
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(stream) = writer.as_mut() else {
        return;
    };

    if let Err(error) = ovs_console_wire::write_control(stream, what) {
        if !matches!(error, ovs_console_wire::WireError::Closed) {
            tracing::warn!("a press could not be sent to the System screen: {error}");
        }
        // Taken away rather than kept and retried: the reader will notice the
        // same socket has gone and reconnect, and this is put back then.
        *writer = None;
    }
}

/// Connects, says how big the page is, and reads frames until told to stop.
fn watch(path: &Path, size: SizePx, shared: &Arc<Shared>) {
    // Said once per spell of being unable to connect rather than every retry: a
    // console that is not up yet is the ordinary case at boot, and twice a
    // second for ever is how a journal stops being readable.
    let mut complained = false;

    while !shared.stop.load(Ordering::Relaxed) {
        let stream = match UnixStream::connect(path) {
            Ok(stream) => stream,
            Err(error) => {
                if !complained {
                    tracing::info!(
                        socket = %path.display(),
                        "waiting for the System screen to start drawing: {error}"
                    );
                    complained = true;
                }
                std::thread::sleep(RETRY);
                continue;
            }
        };

        complained = false;
        tracing::info!(
            socket = %path.display(),
            width = size.width,
            height = size.height,
            "watching the System screen"
        );
        read_frames(stream, size, shared);
        shared.connected.store(false, Ordering::Relaxed);
        // Before saying the connection is over, so nothing is written into a
        // socket whose far end has gone.
        *shared
            .writer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        tracing::info!("the System screen stopped sending");
        std::thread::sleep(RETRY);
    }
}

/// Tells the console the size and then reads until the socket ends.
fn read_frames(stream: UnixStream, size: SizePx, shared: &Arc<Shared>) {
    let Ok(mut writing) = stream.try_clone() else {
        tracing::warn!("this connection could not be split, so nothing is read on it");
        return;
    };

    // First, and once. The page is laid out for the connector rather than for a
    // size written down somewhere, and this is where the connector says what it
    // is.
    if let Err(error) = ovs_console_wire::write_control(
        &mut writing,
        ovs_console_wire::ToConsole::Resize {
            width: size.width,
            height: size.height,
        },
    ) {
        tracing::warn!("the System screen could not be told its size: {error}");
        return;
    }

    // Now that the console has been told the size, input may go out on it.
    if let Ok(sending) = writing.try_clone() {
        *shared
            .writer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(sending);
    }
    shared.connected.store(true, Ordering::Relaxed);

    let mut reading = BufReader::new(stream);
    while !shared.stop.load(Ordering::Relaxed) {
        let frame = match ovs_console_wire::read_frame(&mut reading) {
            Ok(frame) => frame,
            Err(ovs_console_wire::WireError::Closed) => return,
            Err(error) => {
                tracing::warn!("the System screen sent something unreadable: {error}");
                return;
            }
        };

        // **Whatever size it is.** The first frame after connecting is the page
        // as it stood, which is the old size until the resize has been drawn —
        // and a page being resized paints at both sizes for a moment. The output
        // draws one quad across the panel, so a frame of the wrong size is one
        // scaled frame rather than a fault.
        let arrived = SizePx {
            width: frame.width,
            height: frame.height,
        };
        let Some(image) = DecodedImage::bgra(frame.pixels, arrived) else {
            // The wire checks this too. If it is ever reached, the two checks
            // disagree, and drawing a picture of the wrong length is worse than
            // dropping one.
            tracing::warn!(
                width = arrived.width,
                height = arrived.height,
                "a System screen frame did not describe itself"
            );
            continue;
        };

        if let Ok(mut latest) = shared.latest.lock() {
            *latest = Some(Arc::new(image));
        }
        // Released after the frame is in place, so a render pass that sees the
        // new count sees the new frame.
        shared.arrived.fetch_add(1, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::os::unix::net::UnixListener;

    /// A directory this test owns, named after the test and the process.
    fn somewhere(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("ovs-console-source-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("a directory");
        directory
    }

    fn wait_for(mut ready: impl FnMut() -> bool) -> bool {
        let began = std::time::Instant::now();
        while began.elapsed() < Duration::from_secs(5) {
            if ready() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    #[test]
    fn the_size_is_the_first_thing_said_and_the_frames_come_back() {
        let directory = somewhere("talks");
        let path = directory.join("console.sock");
        let listener = UnixListener::bind(&path).expect("listens");

        // Held open until the test has looked, because `connected` is about the
        // socket being up and a console that had already gone would answer no —
        // truthfully, and about the wrong moment.
        let (release, released) = std::sync::mpsc::channel::<()>();
        let console = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("a renderer");
            let mut reading = BufReader::new(stream.try_clone().expect("splits"));
            let asked = ovs_console_wire::read_control(&mut reading).expect("a size");

            // Two frames, so the count can be seen to move.
            let mut writing = stream;
            for shade in [3_u8, 9] {
                ovs_console_wire::write_frame(
                    &mut writing,
                    &ovs_console_wire::Frame {
                        width: 4,
                        height: 2,
                        pixels: vec![shade; 4 * 2 * 4],
                    },
                )
                .expect("writes");
            }
            let _ = released.recv_timeout(Duration::from_secs(10));
            asked
        });

        let source = ConsoleSource::watching(
            &path,
            SizePx {
                width: 1024,
                height: 600,
            },
        );

        assert!(
            wait_for(|| source.arrived() >= 2),
            "two frames did not arrive"
        );
        assert!(
            source.connected(),
            "the socket is up and this says it is not"
        );

        drop(release);
        let asked = console.join().expect("the console thread");
        assert_eq!(
            asked,
            ovs_console_wire::ToConsole::Resize {
                width: 1024,
                height: 600
            },
            "the console was not told the connector's size"
        );

        let frame = source.frame().expect("a frame");
        assert_eq!(frame.pixels()[0], 9, "the newer frame is the one kept");
        assert!(
            frame.is_bgra(),
            "the picture would be drawn with red for blue"
        );

        drop(source);
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_console_that_is_not_there_yet_is_waited_for_rather_than_given_up_on() {
        // The ordinary case at boot: the renderer comes up first, because the
        // console it will show is served by a process that starts later.
        let directory = somewhere("waits");
        let path = directory.join("console.sock");

        let source = ConsoleSource::watching(
            &path,
            SizePx {
                width: 800,
                height: 480,
            },
        );
        assert!(!source.connected());
        assert_eq!(source.arrived(), 0);

        // Started a moment later, as a unit ordered after this one would.
        std::thread::sleep(Duration::from_millis(300));
        let listener = UnixListener::bind(&path).expect("listens");
        let console = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("a renderer");
            let mut head = [0_u8; 9];
            std::io::Read::read_exact(&mut stream, &mut head).expect("the size");
            ovs_console_wire::write_frame(
                &mut stream,
                &ovs_console_wire::Frame {
                    width: 2,
                    height: 2,
                    pixels: vec![7; 16],
                },
            )
            .expect("writes");
            let _ = stream.flush();
        });

        assert!(
            wait_for(|| source.arrived() >= 1),
            "the console started and nobody noticed"
        );
        console.join().expect("the console thread");

        drop(source);
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// A press on a device comes out of the socket the console listens on.
    ///
    /// **Run it by hand, as root:**
    ///
    /// ```text
    /// sudo -E $HOME/.cargo/bin/cargo test -p ovs-renderer --lib \\
    ///     -- --ignored --nocapture a_press_reaches_the_console
    /// ```
    ///
    /// Ignored for the same reason as the touchscreen test it borrows: inventing
    /// a device needs `/dev/uinput`, and that is root-only.
    #[test]
    #[ignore = "needs /dev/uinput, which is root-only"]
    fn a_press_reaches_the_console() {
        use std::os::unix::net::UnixListener;

        let directory = somewhere("press");
        let path = directory.join("console.sock");
        let listener = UnixListener::bind(&path).expect("listens");

        // A console that reads its socket and says what it was told.
        let (said, heard) = std::sync::mpsc::channel();
        let console = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("a renderer");
            let mut reading = BufReader::new(stream);
            while let Ok(what) = ovs_console_wire::read_control(&mut reading) {
                if said.send(what).is_err() {
                    return;
                }
            }
        });

        let panel = SizePx {
            width: 1024,
            height: 600,
        };
        let source = ConsoleSource::watching(&path, panel);

        // The size comes first, always, and the writer is only put in place
        // after it — so a press cannot overtake it.
        let first = heard
            .recv_timeout(Duration::from_secs(5))
            .expect("the size");
        assert_eq!(
            first,
            ovs_console_wire::ToConsole::Resize {
                width: 1024,
                height: 600
            }
        );

        let mut touch = evdev::AttributeSet::<evdev::KeyCode>::new();
        touch.insert(evdev::KeyCode::BTN_TOUCH);
        let mut direct = evdev::AttributeSet::<evdev::PropType>::new();
        direct.insert(evdev::PropType::DIRECT);

        let mut screen = evdev::uinput::VirtualDevice::builder()
            .expect("/dev/uinput — run this as root")
            .name("OVS test panel")
            .with_properties(&direct)
            .expect("properties")
            .with_keys(&touch)
            .expect("a touch button")
            .with_absolute_axis(&evdev::UinputAbsSetup::new(
                evdev::AbsoluteAxisCode::ABS_X,
                evdev::AbsInfo::new(0, 0, 1023, 0, 0, 1),
            ))
            .expect("an x axis")
            .with_absolute_axis(&evdev::UinputAbsSetup::new(
                evdev::AbsoluteAxisCode::ABS_Y,
                evdev::AbsInfo::new(0, 0, 599, 0, 0, 1),
            ))
            .expect("a y axis")
            .build()
            .expect("a virtual panel");

        // The watcher rescans, so it needs a moment to find a device that did
        // not exist when it started.
        std::thread::sleep(Duration::from_secs(3));

        screen
            .emit(&[
                evdev::InputEvent::new(
                    evdev::EventType::ABSOLUTE.0,
                    evdev::AbsoluteAxisCode::ABS_X.0,
                    512,
                ),
                evdev::InputEvent::new(
                    evdev::EventType::ABSOLUTE.0,
                    evdev::AbsoluteAxisCode::ABS_Y.0,
                    300,
                ),
                evdev::InputEvent::new(evdev::EventType::KEY.0, evdev::KeyCode::BTN_TOUCH.0, 1),
            ])
            .expect("a press");

        let mut pressed = false;
        let until = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < until {
            match heard.recv_timeout(Duration::from_millis(200)) {
                Ok(ovs_console_wire::ToConsole::Button { down: true, .. }) => {
                    pressed = true;
                    break;
                }
                Ok(_) => {}
                Err(_) => {}
            }
        }
        assert!(pressed, "the press never came out of the socket");

        // **Not joined.** The fake console reads until its socket ends, and it
        // does not end here: `ConsoleSource` deliberately leaves its reader
        // thread blocked rather than waiting for it, because that wait would be
        // on the render thread when a panel's mode changes. So the thread is
        // left to go with the process, which is what the comment on that `Drop`
        // is describing.
        drop(source);
        drop(console);
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_console_that_goes_away_is_reconnected_to() {
        // An upgrade, or a crash. The renderer must not need restarting for the
        // System screen to come back.
        let directory = somewhere("again");
        let path = directory.join("console.sock");
        let listener = UnixListener::bind(&path).expect("listens");

        let serving = std::thread::spawn(move || {
            for shade in [1_u8, 2] {
                let (mut stream, _) = listener.accept().expect("a renderer");
                let mut head = [0_u8; 9];
                std::io::Read::read_exact(&mut stream, &mut head).expect("the size");
                ovs_console_wire::write_frame(
                    &mut stream,
                    &ovs_console_wire::Frame {
                        width: 2,
                        height: 2,
                        pixels: vec![shade; 16],
                    },
                )
                .expect("writes");
                let _ = stream.flush();
                // And then the console goes.
                drop(stream);
            }
        });

        let source = ConsoleSource::watching(
            &path,
            SizePx {
                width: 640,
                height: 480,
            },
        );

        assert!(
            wait_for(|| source.frame().is_some_and(|frame| frame.pixels()[0] == 2)),
            "the second connection's frame never arrived"
        );
        serving.join().expect("the console thread");

        drop(source);
        let _ = std::fs::remove_dir_all(&directory);
    }
}
