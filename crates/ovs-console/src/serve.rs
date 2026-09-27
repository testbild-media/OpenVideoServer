//! Handing frames to the renderer, and taking its answer back.
//!
//! One socket, one renderer at a time. The console listens and the renderer
//! connects, which is the same direction the core and the renderer already use
//! and for the same reason: when the thing that connects goes away — an upgrade,
//! a crash, a restart — the thing that listens is not a client that lost its
//! server. It is still here, still drawing, and the next connection finds the
//! page where it left it.
//!
//! # Two threads, because the message loop belongs to CEF
//!
//! The main thread pumps CEF and must not sit in a socket write. So a frame
//! leaves on a writer thread, which waits on the painted slot and sends what it
//! finds, and what comes back is read on another. Between them and CEF there is
//! nothing but the slot and a shared size.
//!
//! # The wire has no dependencies, and must not gain any
//!
//! `ovs-console-wire` rather than `ovs-ipc`, because linking `getrandom` beside
//! CEF stops Chromium starting, and `ovs-ipc` reaches it through `ovs-types` and
//! `ulid`. That crate's own documentation carries the measurement.

use std::io::{BufReader, BufWriter};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ovs_console_wire::{self as wire, ToConsole, WireError};

use crate::page::{Painted, Size};

/// How long a writer waits for a frame before looking up.
///
/// It is not waiting for a frame so much as for the chance to notice that the
/// renderer has gone or the process is stopping. A page nobody is touching
/// paints nothing for minutes at a time, and this is the longest such a page
/// delays a shutdown.
const PATIENCE: Duration = Duration::from_millis(250);

/// What the socket is set to once it exists.
///
/// The console and the renderer are separate service accounts, so the renderer
/// cannot connect to a socket only its owner may write to — and connecting needs
/// *write* permission, which is the part that is easy to get wrong by reasoning
/// about it as reading pictures. The group is whatever the console's own is; the
/// unit is what puts the renderer in it.
const SOCKET_MODE: u32 = 0o660;

/// The most input that may wait to be handed to CEF.
///
/// The pump drains this every sixteen milliseconds, so two or three events is a
/// busy moment and this is never reached by anything that is working. It is here
/// because the queue is filled by a socket: a peer that sent faster than the
/// browser could be told would otherwise grow it until the machine died, and a
/// System screen that stops responding is better than a server that stops.
const MOST_WAITING: usize = 512;

/// What a served connection can ask of the browser.
///
/// Applied on the main thread, because everything CEF owns belongs to the thread
/// that pumps it. This is the shared cell between the two.
#[derive(Debug, Default)]
pub struct Wanted {
    /// The size the renderer last asked for, when it differs from the current.
    resize: Mutex<Option<Size>>,
    /// What somebody did on the panel, in the order they did it.
    ///
    /// A queue rather than a slot, unlike the resize above: two sizes in one
    /// pass mean the second one, and two presses in one pass mean two presses.
    input: Mutex<Vec<ToConsole>>,
}

impl Wanted {
    /// Records a size the browser should be laid out at.
    pub fn resize(&self, size: Size) {
        *self
            .resize
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(size);
    }

    /// Takes the size, if one was asked for since this was last called.
    #[must_use]
    pub fn take_resize(&self) -> Option<Size> {
        self.resize
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    /// Records something somebody did on the panel.
    pub fn did(&self, what: ToConsole) {
        let mut input = self
            .input
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if input.len() >= MOST_WAITING {
            // Said once a full queue, not once an event: a queue this full is
            // either a peer misbehaving or a pump that has stopped, and neither
            // is improved by a line per press.
            if input.len() == MOST_WAITING {
                tracing::warn!(
                    waiting = input.len(),
                    "the System screen is not keeping up with what is being pressed"
                );
                input.push(what);
            }
            return;
        }
        input.push(what);
    }

    /// Takes everything that has happened since this was last called.
    ///
    /// Into the caller's own buffer, which it reuses: this runs sixty times a
    /// second on the thread that must not stop to allocate.
    pub fn take_input(&self, into: &mut Vec<ToConsole>) {
        into.clear();
        let mut input = self
            .input
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::mem::swap(into, &mut *input);
    }
}

/// Listens for the renderer and serves it frames until told to stop.
pub struct Server {
    path: PathBuf,
    running: Arc<AtomicBool>,
}

impl Server {
    /// Starts listening at `path`, serving frames from `painted`.
    ///
    /// # Errors
    ///
    /// Returns the underlying [`std::io::Error`] when the socket cannot be bound
    /// or the threads cannot be started.
    pub fn start(
        path: impl AsRef<Path>,
        painted: Arc<Painted>,
        wanted: Arc<Wanted>,
    ) -> std::io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(directory) = path.parent() {
            std::fs::create_dir_all(directory)?;
        }
        // A socket left behind by a process that was killed would refuse the
        // bind, and a console that will not start because it did not shut down
        // tidily last time is a black screen for a reason nobody can see.
        let _ = std::fs::remove_file(&path);

        let listener = UnixListener::bind(&path)?;
        listener.set_nonblocking(false)?;
        // After the bind, because the file does not exist before it. A umask
        // could have made this unnecessary; saying it is better than depending on
        // what a service manager happened to inherit.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(SOCKET_MODE))?;

        let running = Arc::new(AtomicBool::new(true));
        let accepting = Arc::clone(&running);
        std::thread::Builder::new()
            .name("ovs-console-serve".to_owned())
            .spawn(move || accept(&listener, &painted, &wanted, &accepting))?;

        Ok(Self { path, running })
    }

    /// Where it is listening.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        // The socket goes with the process. Left behind it would make the next
        // start look like something is already listening.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Takes one renderer at a time, for as long as this is running.
fn accept(
    listener: &UnixListener,
    painted: &Arc<Painted>,
    wanted: &Arc<Wanted>,
    running: &Arc<AtomicBool>,
) {
    while running.load(Ordering::Relaxed) {
        let stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(error) => {
                tracing::warn!("could not accept a renderer: {error}");
                std::thread::sleep(PATIENCE);
                continue;
            }
        };

        tracing::info!("a renderer is watching the System screen");
        serve(stream, painted, wanted, running);
        tracing::info!("the renderer stopped watching");
    }
}

/// Serves one renderer until it goes away.
fn serve(
    stream: UnixStream,
    painted: &Arc<Painted>,
    wanted: &Arc<Wanted>,
    running: &Arc<AtomicBool>,
) {
    let Ok(reading) = stream.try_clone() else {
        tracing::warn!("this connection could not be split, so nothing is served on it");
        return;
    };

    // The reader ends when the renderer goes, which drops the stream and makes
    // the writer's next send fail. Neither has to tell the other.
    let listening = Arc::clone(wanted);
    let reader = std::thread::Builder::new()
        .name("ovs-console-listen".to_owned())
        .spawn(move || {
            let mut reading = BufReader::new(reading);
            loop {
                match wire::read_control(&mut reading) {
                    Ok(ToConsole::Resize { width, height }) => match asked_size(width, height) {
                        Ok(size) => listening.resize(size),
                        Err(why) => {
                            tracing::warn!("{why}");
                        }
                    },
                    // Not bounds-checked here. A press outside the page is a
                    // press CEF ignores, and the renderer sends panel pixels it
                    // worked out from a mode it set — clamping them here would
                    // turn a bug in that arithmetic into a press on the edge of
                    // the screen, which is harder to notice than one that does
                    // nothing.
                    Ok(what) => listening.did(what),
                    Err(WireError::Closed) => return,
                    Err(error) => {
                        tracing::warn!("the renderer said something unreadable: {error}");
                        return;
                    }
                }
            }
        });

    // Kept so the reader can be woken when the writer decides to stop: it is
    // blocked in a read that only the far end closing would end, and the far end
    // is not the one stopping.
    let closing = stream.try_clone().ok();
    let mut writing = BufWriter::new(stream);
    // **The picture as it stands goes first.** Asking for something newer than
    // one before the current count sends whatever is in the slot immediately,
    // however old it is. A renderer that connected and then waited for a change
    // would show black until somebody touched the page — and a System screen
    // nobody has touched yet is exactly when one is wanted.
    let mut seen = painted.count().saturating_sub(1);

    while running.load(Ordering::Relaxed) {
        let Some((count, frame)) = painted.newer_than(seen, PATIENCE) else {
            continue;
        };
        seen = count;

        let sending = wire::Frame {
            width: frame.width,
            height: frame.height,
            pixels: frame.pixels,
        };
        if let Err(error) = wire::write_frame(&mut writing, &sending) {
            if !matches!(error, WireError::Closed) {
                tracing::warn!("a frame could not be sent: {error}");
            }
            break;
        }
    }

    if let Some(closing) = closing {
        let _ = closing.shutdown(std::net::Shutdown::Both);
    }
    if let Ok(reader) = reader {
        let _ = reader.join();
    }
}

/// Checks a size the renderer asked for before CEF is laid out at it.
///
/// **Bounded here rather than trusted.** The wire carries two numbers; what they
/// mean is a browser being told to lay a page out, and a zero or a hundred
/// thousand would be a page that cannot be drawn or an allocation that takes the
/// machine down. Refused with a line in the log, which leaves the page at the
/// size it was — visible and wrong-sized beats gone.
fn asked_size(width: u32, height: u32) -> Result<Size, String> {
    if width == 0 || height == 0 || width > wire::MAX_SIDE || height > wire::MAX_SIDE {
        return Err(format!(
            "the renderer asked for a {width}x{height} page, which is not a size a panel has"
        ));
    }
    Ok(Size { width, height })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_size_a_panel_could_have_is_taken_and_one_it_could_not_is_refused() {
        assert_eq!(
            asked_size(1024, 600),
            Ok(Size {
                width: 1024,
                height: 600
            })
        );
        assert!(asked_size(wire::MAX_SIDE, wire::MAX_SIDE).is_ok());

        for (width, height) in [
            (0, 600),
            (1024, 0),
            (wire::MAX_SIDE + 1, 600),
            (1024, wire::MAX_SIDE + 1),
        ] {
            assert!(asked_size(width, height).is_err(), "{width}x{height}");
        }
    }

    #[test]
    fn the_last_size_asked_for_is_the_one_applied_and_it_is_applied_once() {
        // The pump reads this once a loop and a renderer can say it twice in
        // between. What matters is that the newer one wins and that a size is not
        // reapplied on every pass — telling CEF it was resized costs it a full
        // repaint.
        let wanted = Wanted::default();
        assert!(wanted.take_resize().is_none());

        wanted.resize(Size {
            width: 1024,
            height: 600,
        });
        wanted.resize(Size {
            width: 1920,
            height: 1080,
        });
        assert_eq!(
            wanted.take_resize(),
            Some(Size {
                width: 1920,
                height: 1080
            })
        );
        assert!(wanted.take_resize().is_none(), "the size was applied twice");
    }

    #[test]
    fn a_renderer_gets_the_page_as_it_stands_and_then_what_changes() {
        let painted = Arc::new(Painted::default());
        let wanted = Arc::new(Wanted::default());

        // A page that was drawn before anybody connected.
        painted.put(crate::page::Frame {
            width: 2,
            height: 2,
            pixels: vec![1; 16],
        });

        let directory =
            std::env::temp_dir().join(format!("ovs-console-test-{}", std::process::id()));
        let path = directory.join("console.sock");
        let server =
            Server::start(&path, Arc::clone(&painted), Arc::clone(&wanted)).expect("listens");

        let mut renderer = UnixStream::connect(server.path()).expect("connects");
        renderer
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("a timeout, so a failure here is a failure and not a hang");

        // The old frame arrives without anything having changed: a renderer that
        // had to wait for a repaint would show black on a page nobody is
        // touching.
        let first = wire::read_frame(&mut renderer).expect("the page as it stands");
        assert_eq!((first.width, first.height), (2, 2));
        assert_eq!(first.pixels[0], 1);

        // And then what changes, once.
        painted.put(crate::page::Frame {
            width: 2,
            height: 2,
            pixels: vec![7; 16],
        });
        let second = wire::read_frame(&mut renderer).expect("the change");
        assert_eq!(second.pixels[0], 7);

        // The other direction, on the same socket.
        wire::write_control(
            &mut renderer,
            ToConsole::Resize {
                width: 800,
                height: 480,
            },
        )
        .expect("writes");

        let began = std::time::Instant::now();
        let applied = loop {
            if let Some(size) = wanted.take_resize() {
                break Some(size);
            }
            if began.elapsed() > Duration::from_secs(5) {
                break None;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(
            applied,
            Some(Size {
                width: 800,
                height: 480
            })
        );

        drop(server);
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_socket_left_by_a_killed_process_does_not_stop_the_next_start() {
        // A console that will not start because it did not shut down tidily is a
        // black screen for a reason nobody can see from the front.
        let directory =
            std::env::temp_dir().join(format!("ovs-console-stale-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("a directory");
        let path = directory.join("console.sock");
        std::fs::write(&path, b"what a killed process left").expect("something in the way");

        let server = Server::start(
            &path,
            Arc::new(Painted::default()),
            Arc::new(Wanted::default()),
        )
        .expect("binds over what was there");
        assert!(UnixStream::connect(server.path()).is_ok());

        // And the mode is one the renderer's account can connect through, which
        // needs write rather than read.
        let mode = std::fs::metadata(server.path())
            .expect("the socket")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, SOCKET_MODE, "the renderer's group could not connect");

        drop(server);
        let _ = std::fs::remove_dir_all(&directory);
    }
}
