//! The browser that draws the System screen, and the frames it produces.
//!
//! # Off-screen, because a screen is what the renderer has
//!
//! CEF is asked for *windowless* rendering: it lays the page out, paints it, and
//! hands over the pixels instead of putting them anywhere. That is the whole
//! reason this exists. The kiosk it replaces was a compositor, and a compositor
//! takes a card — every connector on it — which is what made the System screen
//! spill across two panels and take the show's card with it (owner, 2026-09-25).
//!
//! This asks the graphics stack for nothing at all. The renderer owns the card,
//! puts these pixels on one connector, and every other connector stays a wall's
//! to have.
//!
//! # Painted when it changes, not sixty times a second
//!
//! CEF's off-screen rendering is damage-driven: `on_paint` arrives when
//! something on the page actually changed. A page sitting still produces no
//! frames at all and costs nothing, which is most of what a System screen does.
//!
//! So [`FRAME_RATE`] is a ceiling rather than a rate. What it governs is how
//! quickly a press is answered — the button that goes down, the character that
//! appears — and that is the half an operator feels.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use cef::rc::Rc as _;
use cef::*;

/// The most often the page may be repainted, in frames a second.
///
/// **A ceiling, not a rate.** CEF paints on damage, so a still page produces
/// nothing whatever this says; what it decides is the longest a press can wait
/// to be answered. Thirty puts that at thirty-three milliseconds.
///
/// The owner's instinct was one a second, on the grounds that nothing on this
/// page moves quickly (2026-09-26: "für das System Display reicht ja auch
/// 1fps"). True of the content and not of the touch: at one a second a finger
/// waits up to a second for the button to go down, and the saving is nothing,
/// because an idle page is already free.
pub const FRAME_RATE: i32 = 30;

/// One painted picture of the page.
///
/// BGRA, which is what CEF hands over, and what is kept rather than converted:
/// the renderer uploads it as it is.
#[derive(Debug, Clone)]
pub struct Frame {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width * height * 4` bytes, BGRA, top row first.
    pub pixels: Vec<u8>,
}

impl Frame {
    /// How much of this frame is not black, as a fraction.
    ///
    /// **A frame arriving is not a page being drawn.** A buffer of zeroes has
    /// the same shape as a picture, and the difference between "CEF called back"
    /// and "the page rendered" is the whole of what a first measurement is for.
    ///
    /// Alpha is skipped: a fully transparent white pixel is not ink, and CEF
    /// hands over BGRA with the alpha it composited to.
    #[must_use]
    pub fn ink(&self) -> f32 {
        if self.pixels.is_empty() {
            return 0.0;
        }
        let lit = self
            .pixels
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|pixel| pixel[0] != 0 || pixel[1] != 0 || pixel[2] != 0)
            .count();
        lit as f32 / (self.pixels.len() / 4) as f32
    }
}

/// The latest painted frame, and how many there have been.
///
/// One slot rather than a queue, for the same reason the NDI receiver keeps one:
/// a frame that has been overtaken is a picture nobody will ever want, and
/// holding it only delays the one they do.
#[derive(Debug, Default)]
pub struct Painted {
    latest: Mutex<Option<Frame>>,
    /// How many paints have arrived since this started.
    ///
    /// The cheap half of "has anything changed": a reader compares the count and
    /// takes the frame only when it moved.
    count: AtomicU64,
    /// Woken when a frame arrives.
    ///
    /// Because a page nobody is touching paints nothing for minutes at a time,
    /// and a sender that polled for that would either burn a core or add its
    /// interval to how long a press takes to reach the panel.
    arrived: Condvar,
}

impl Painted {
    /// Puts a frame in the slot, replacing whatever was there.
    pub fn put(&self, frame: Frame) {
        let mut latest = self
            .latest
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *latest = Some(frame);
        // **Counted while the slot is held.** A reader holding this lock then
        // sees a count and a frame that belong to each other; incremented
        // outside it, a reader could take the new count with the old picture and
        // never come back for the new one.
        self.count.fetch_add(1, Ordering::Release);
        drop(latest);

        // After the lock, so whoever wakes does not immediately block on it.
        self.arrived.notify_all();
    }

    /// How many frames have been painted.
    #[must_use]
    pub fn count(&self) -> u64 {
        self.count.load(Ordering::Acquire)
    }

    /// Takes a copy of the latest frame, if there is one.
    #[must_use]
    pub fn latest(&self) -> Option<Frame> {
        self.latest
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Waits for a frame newer than `seen`, giving up after `patience`.
    ///
    /// Returns the count that frame arrived at, so the caller can ask for the
    /// next one. `None` means nothing new within `patience` — which on this page
    /// is the ordinary case, not a fault: it is how a sender gets the chance to
    /// notice the far end has gone or the process is stopping.
    #[must_use]
    pub fn newer_than(&self, seen: u64, patience: Duration) -> Option<(u64, Frame)> {
        let mut latest = self
            .latest
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        loop {
            let count = self.count();
            if count > seen {
                return latest.clone().map(|frame| (count, frame));
            }

            let (guard, timed_out) = self
                .arrived
                .wait_timeout(latest, patience)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            latest = guard;
            if timed_out.timed_out() {
                // Checked once more rather than returning: a frame can arrive
                // between the wait expiring and this reacquiring the lock, and
                // dropping it would hold a press back by a whole `patience`.
                let count = self.count();
                return if count > seen {
                    latest.clone().map(|frame| (count, frame))
                } else {
                    None
                };
            }
        }
    }
}

/// Whether the page loaded, and when to try again if it did not.
///
/// **A page that will not load must keep trying.** The System screen reads a
/// server on this machine, and at boot both start together — `After=` in a unit
/// orders the start and says nothing about readiness, so the first load can
/// arrive a moment before anything is listening. Chromium answers that with an
/// error page and then does nothing for ever, which in a rack is a black screen
/// with somebody else's words on it.
#[derive(Debug)]
pub struct Loading {
    /// Set by the load handler when a load fails, read by the pump.
    ///
    /// A flag rather than a reload from inside the callback: everything CEF owns
    /// belongs to the thread that pumps it, and the handler is not always on it.
    failed: AtomicBool,
    /// How many loads have failed since the last one succeeded.
    ///
    /// Only so the log says "still" rather than repeating itself, and so the
    /// first failure is the one an operator reads.
    failures: AtomicU64,
}

impl Default for Loading {
    fn default() -> Self {
        Self {
            failed: AtomicBool::new(false),
            failures: AtomicU64::new(0),
        }
    }
}

impl Loading {
    /// Records that a load failed.
    pub fn failed(&self, url: &str, why: &str) {
        let before = self.failures.fetch_add(1, Ordering::Relaxed);
        if before == 0 {
            tracing::warn!(%url, "the System screen's page could not be loaded: {why}");
        }
        self.failed.store(true, Ordering::Release);
    }

    /// Records that a load succeeded.
    pub fn loaded(&self) {
        if self.failures.swap(0, Ordering::Relaxed) > 0 {
            tracing::info!("the System screen's page is up");
        }
        self.failed.store(false, Ordering::Release);
    }

    /// Takes the failure, if there is one to act on.
    #[must_use]
    pub fn take_failure(&self) -> bool {
        self.failed.swap(false, Ordering::Acquire)
    }

    /// How many loads have failed in a row.
    #[must_use]
    pub fn failures(&self) -> u64 {
        self.failures.load(Ordering::Relaxed)
    }
}

/// What the page is drawn at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

/// The size the page is currently laid out at.
///
/// Shared, because the panel decides it and the panel is the renderer's. When a
/// connector is given the System screen the renderer says how big it is, and the
/// page is laid out for that — which is what makes it responsive to the socket it
/// lands on rather than to a number written down here (owner, 2026-09-26: "Das
/// Frame muss nicht 1024x600 sein... es kann jede beliebige größe haben").
#[derive(Debug)]
pub struct Panel {
    /// Both sides in one word: width in the high half, height in the low.
    ///
    /// **One word rather than two, or a lock.** CEF asks for the rectangle on its
    /// own thread, and it asks at the moment a resize is being applied. Two
    /// separate numbers could be read across a change — a new width with an old
    /// height — and CEF would lay the page out at a size that was never asked
    /// for. A lock would do as well and would mean the pump thread waiting on
    /// CEF's to answer a question about a number.
    packed: AtomicU64,
}

impl Panel {
    /// A panel of this size.
    #[must_use]
    pub fn new(size: Size) -> Self {
        let panel = Self {
            packed: AtomicU64::new(0),
        };
        panel.set(size);
        panel
    }

    /// What the page is laid out at now.
    #[must_use]
    pub fn get(&self) -> Size {
        let packed = self.packed.load(Ordering::Acquire);
        Size {
            width: (packed >> 32) as u32,
            height: (packed & 0xffff_ffff) as u32,
        }
    }

    /// Lays the page out at this size from now on.
    ///
    /// Telling CEF to ask again is the caller's, on the thread that pumps it.
    pub fn set(&self, size: Size) {
        let packed = (u64::from(size.width) << 32) | u64::from(size.height);
        self.packed.store(packed, Ordering::Release);
    }
}

/// The application, which exists to answer for the whole process.
///
/// CEF calls this before it has decided what kind of process it is in, so what
/// goes here has to be true of every one of them.
#[derive(Clone)]
pub struct ConsoleApp;

wrap_app! {
    pub struct AppBuilder {
        app: ConsoleApp,
    }

    impl App {
        fn on_before_command_line_processing(
            &self,
            _process_type: Option<&CefStringUtf16>,
            command_line: Option<&mut CommandLine>,
        ) {
            let Some(command_line) = command_line else {
                return;
            };

            // **There is no display server, and none is wanted.** Ozone —
            // Chromium's platform layer — defaults to X11 and refuses to start
            // without one: "Missing X server or $DISPLAY". Right for a browser
            // on a desktop, wrong for a process whose whole job is to paint into
            // memory for a renderer that owns the card.
            command_line.append_switch_with_value(
                Some(&"ozone-platform".into()),
                Some(&"headless".into()),
            );
            // **And no card.** Its GPU process would otherwise bring up EGL,
            // find no display, and exit — but giving it one is exactly the wrong
            // answer: the renderer owns the card, which is the whole reason this
            // exists instead of a compositor, and a second client of it would be
            // the same conflict one layer down.
            //
            // A page that changes when somebody presses something costs a
            // fraction of a core to composite in software. The expensive half —
            // getting the finished pixels onto a panel — is the renderer's, and
            // it is one upload.
            command_line.append_switch(Some(&"disable-gpu".into()));
            command_line.append_switch(Some(&"disable-gpu-compositing".into()));
            // No window is ever wanted either: nothing here has a screen to put
            // one on.
            command_line.append_switch(Some(&"no-startup-window".into()));
            // A dialogue on a machine in a rack is a dialogue nobody will ever
            // press, in front of the page somebody is trying to read.
            command_line.append_switch(Some(&"noerrdialogs".into()));
            command_line.append_switch(Some(&"hide-crash-restore-bubble".into()));
            // So a `CHECK` says what it checked. Without it Chromium aborts with
            // a signal and nothing else, which is how the first run of this
            // looked: exit 133 and an empty log.
            command_line.append_switch(Some(&"enable-logging=stderr".into()));
        }
    }
}

impl AppBuilder {
    /// Builds the application object CEF is handed at startup.
    #[must_use]
    pub fn console() -> App {
        Self::new(ConsoleApp)
    }
}

/// The render handler: where the pixels come out.
#[derive(Clone)]
pub struct ConsoleRenderHandler {
    panel: Arc<Panel>,
    painted: Arc<Painted>,
}

impl ConsoleRenderHandler {
    /// Draws at whatever `panel` says, putting each painted frame in `painted`.
    #[must_use]
    pub fn new(panel: Arc<Panel>, painted: Arc<Painted>) -> Self {
        Self { panel, painted }
    }
}

wrap_render_handler! {
    pub struct RenderHandlerBuilder {
        handler: ConsoleRenderHandler,
    }

    impl RenderHandler {
        fn view_rect(&self, _browser: Option<&mut Browser>, rect: Option<&mut Rect>) {
            // What CEF lays the page out for. The renderer's output decides it,
            // which is what makes the page responsive to the panel rather than
            // to a size written down somewhere.
            if let Some(rect) = rect {
                let size = self.handler.panel.get();
                rect.x = 0;
                rect.y = 0;
                rect.width = size.width as _;
                rect.height = size.height as _;
            }
        }

        fn on_paint(
            &self,
            _browser: Option<&mut Browser>,
            _type_: PaintElementType,
            _dirty_rects: Option<&[Rect]>,
            buffer: *const u8,
            width: ::std::os::raw::c_int,
            height: ::std::os::raw::c_int,
        ) {
            if buffer.is_null() || width <= 0 || height <= 0 {
                return;
            }

            let width = width as u32;
            let height = height as u32;
            let bytes = width as usize * height as usize * 4;

            // SAFETY: CEF documents this buffer as `width * height * 4` bytes of
            // BGRA, valid for the duration of this call. It is copied out here
            // and never held: the next paint reuses it.
            let pixels = unsafe { std::slice::from_raw_parts(buffer, bytes) }.to_vec();

            self.handler.painted.put(Frame {
                width,
                height,
                pixels,
            });
        }
    }
}

/// Watches whether the page loaded.
#[derive(Clone)]
pub struct ConsoleLoadHandler {
    loading: Arc<Loading>,
}

wrap_load_handler! {
    pub struct LoadHandlerBuilder {
        handler: ConsoleLoadHandler,
    }

    impl LoadHandler {
        fn on_load_error(
            &self,
            _browser: Option<&mut Browser>,
            // `cef::Frame`, spelled out: a frame of a document. This module's
            // own `Frame` is a painted picture, and the two words are the same.
            frame: Option<&mut cef::Frame>,
            _error_code: Errorcode,
            error_text: Option<&CefStringUtf16>,
            failed_url: Option<&CefStringUtf16>,
        ) {
            // The main frame only. A failed favicon or a sub-frame is not a
            // reason to throw the page away and start again.
            if !frame.is_some_and(|frame| frame.is_main() == 1) {
                return;
            }

            let url = failed_url.map(ToString::to_string).unwrap_or_default();
            let why = error_text.map(|text| text.to_string()).unwrap_or_default();
            self.handler.loading.failed(&url, &why);
        }

        fn on_load_end(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut cef::Frame>,
            http_status_code: ::std::os::raw::c_int,
        ) {
            if !frame.is_some_and(|frame| frame.is_main() == 1) {
                return;
            }

            // **A status is not a load.** A server that is up and answering 500
            // has "loaded" as far as Chromium is concerned, and the page on the
            // panel is its error text. Anything but a success is worth trying
            // again, because on this machine the page and the server ship
            // together and a 404 means one of them is the wrong build.
            if (200..400).contains(&http_status_code) {
                self.handler.loading.loaded();
            } else {
                self.handler
                    .loading
                    .failed("", &format!("the server answered {http_status_code}"));
            }
        }
    }
}

wrap_client! {
    pub struct ClientBuilder {
        render_handler: RenderHandler,
        load_handler: LoadHandler,
    }

    impl Client {
        fn render_handler(&self) -> Option<RenderHandler> {
            Some(self.render_handler.clone())
        }

        fn load_handler(&self) -> Option<LoadHandler> {
            Some(self.load_handler.clone())
        }
    }
}

impl ClientBuilder {
    /// Builds a client that paints into `painted` at whatever `panel` says.
    #[must_use]
    pub fn painting(panel: Arc<Panel>, painted: Arc<Painted>, loading: Arc<Loading>) -> Client {
        Self::new(
            RenderHandlerBuilder::new(ConsoleRenderHandler::new(panel, painted)),
            LoadHandlerBuilder::new(ConsoleLoadHandler { loading }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panel_hands_back_the_size_it_was_given() {
        let panel = Panel::new(Size {
            width: 1024,
            height: 600,
        });
        assert_eq!(
            panel.get(),
            Size {
                width: 1024,
                height: 600
            }
        );

        // The renderer says what the connector is, and the page follows.
        panel.set(Size {
            width: 3840,
            height: 2160,
        });
        assert_eq!(
            panel.get(),
            Size {
                width: 3840,
                height: 2160
            }
        );
    }

    #[test]
    fn a_waiting_sender_is_woken_by_a_frame_rather_than_by_the_clock() {
        let painted = Arc::new(Painted::default());

        // Nothing painted: the wait ends by itself, which is what gives a sender
        // the chance to notice its renderer has gone.
        let began = std::time::Instant::now();
        assert!(painted.newer_than(0, Duration::from_millis(50)).is_none());
        assert!(began.elapsed() >= Duration::from_millis(40));

        // A frame arriving during the wait ends it early. The wait is generous
        // and the sleep short, so a pass here is the condvar and not the clock:
        // waiting out the timeout would take twenty times as long.
        let arriving = Arc::clone(&painted);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            arriving.put(Frame {
                width: 2,
                height: 2,
                pixels: vec![9; 16],
            });
        });

        let began = std::time::Instant::now();
        let (count, frame) = painted
            .newer_than(0, Duration::from_secs(10))
            .expect("the frame that arrived");
        assert_eq!(count, 1);
        assert_eq!(frame.pixels[0], 9);
        assert!(
            began.elapsed() < Duration::from_secs(1),
            "the wait ran to its timeout instead of being woken"
        );

        // And asking again for something newer than what was just taken waits,
        // rather than handing the same picture over for ever.
        assert!(painted
            .newer_than(count, Duration::from_millis(50))
            .is_none());
    }
}
