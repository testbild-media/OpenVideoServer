//! Touch and mouse on the System screen's panel, and nowhere else.
//!
//! The renderer reads the devices and tells the console where the pointer is.
//! Two reasons it is this way round rather than the console reading them itself.
//! The console hosts CEF, and every crate linked into that process is a gamble —
//! one ordinary dependency already stopped Chromium from starting. And "only on
//! the System Display" is a rule about *which panel*, which belongs in the
//! process that owns the panels.
//!
//! # `/dev/input`, not libinput
//!
//! libinput wants a seat, and a seat is what `logind` or `seatd` hands out to a
//! session. This has no session: it is a service that opens device nodes it has
//! been given the group for, exactly as it opens the card. What libinput would
//! add on top of this is pointer acceleration — a curve fitted to a hand on a
//! desk — and what is underneath it is three kinds of device, written out below.
//!
//! # Three kinds of device, and what each one means
//!
//! * **A touchscreen** reports `ABS_X`/`ABS_Y` in its own units and carries
//!   `INPUT_PROP_DIRECT`, which means "the surface is the screen". Its
//!   coordinates are scaled to the panel and used as they are.
//! * **A mouse** reports `REL_X`/`REL_Y`, which are movements. They are added up
//!   into a position and clamped to the panel.
//! * **A touchpad** reports absolute coordinates *of the pad* and carries
//!   `INPUT_PROP_POINTER`. Using those as screen coordinates makes the pointer
//!   jump to wherever the finger landed, which is the classic way to get this
//!   wrong — so what is used is the **difference** between one report and the
//!   next, as a fraction of the pad, applied as a fraction of the panel.
//!   Crossing the pad crosses the screen. That scale is derived from the two
//!   sizes rather than picked, which is why no sensitivity constant appears in
//!   this file.
//!
//! A pad needs three things a mouse gets for nothing. A finger lifted and put
//! down elsewhere must not move the pointer, so the position it is measured from
//! is forgotten on release. `BTN_TOUCH` on a pad means a finger is resting on
//! it and **not** that something was clicked — taking it for a click would press
//! the page every time a hand settled. And the position reported while two
//! fingers are down is neither finger, so movement waits until one of them
//! leaves.
//!
//! What a pad deliberately does not do here is tap-to-click. A touchpad of this
//! kind is a clickpad — `INPUT_PROP_BUTTONPAD`, with `BTN_LEFT` under the whole
//! surface — so pressing it works without one, and telling a tap from a short
//! slide needs a time and a distance that belong to a measurement on the panel
//! somebody is using rather than to a number invented here.

use std::collections::{HashMap, HashSet};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ovs_types::geometry::SizePx;

/// How long to wait before looking for devices again.
///
/// A touchscreen on a rack panel is plugged in when the rack is built and never
/// again, but a mouse is plugged in by whoever is standing there — so this looks
/// for new ones rather than deciding at startup what exists for ever.
const RESCAN: Duration = Duration::from_secs(2);

/// Where the kernel puts the event devices.
const DEVICES: &str = "/dev/input";

/// Where the pointer is on the panel, and how it got there.
///
/// Shared by every device: two mice and a touchscreen move one pointer, which is
/// what a person expects and what the page is drawn for.
#[derive(Debug)]
pub struct Cursor {
    panel: SizePx,
    x: f64,
    y: f64,
}

impl Cursor {
    /// A pointer in the middle of a panel of this size.
    ///
    /// The middle rather than a corner: the first thing a mouse does is move a
    /// little, and starting at 0,0 puts the first press in the corner of a page
    /// where a corner is usually something.
    #[must_use]
    pub fn middle_of(panel: SizePx) -> Self {
        Self {
            panel,
            x: f64::from(panel.width) / 2.0,
            y: f64::from(panel.height) / 2.0,
        }
    }

    /// Where the pointer is, in panel pixels.
    #[must_use]
    pub fn at(&self) -> (u32, u32) {
        // Clamped at the moment it is read as well as when it is moved, so a
        // panel that has been made smaller cannot report a pixel it no longer
        // has.
        let last_x = self.panel.width.saturating_sub(1);
        let last_y = self.panel.height.saturating_sub(1);
        (
            (self.x.round().max(0.0) as u32).min(last_x),
            (self.y.round().max(0.0) as u32).min(last_y),
        )
    }

    /// Moves the pointer by a mouse's report.
    pub fn by(&mut self, dx: f64, dy: f64) {
        self.x = (self.x + dx).clamp(0.0, f64::from(self.panel.width));
        self.y = (self.y + dy).clamp(0.0, f64::from(self.panel.height));
    }

    /// Moves the pointer by a fraction of the panel.
    ///
    /// What a finger sliding a fraction of the way across a touchpad means: one
    /// gesture traverses the pad and the panel alike, whatever either of them
    /// measures. Derived rather than tuned, so there is no sensitivity number
    /// here to be wrong about.
    pub fn slid(&mut self, dx: f64, dy: f64) {
        self.by(
            dx * f64::from(self.panel.width),
            dy * f64::from(self.panel.height),
        );
    }

    /// Puts the pointer where a touchscreen says a finger is.
    ///
    /// `value` is in the device's own units and `range` is what it reports as
    /// its minimum and maximum. A device whose range is empty — which a driver
    /// that has not been asked yet can report — is ignored rather than dividing
    /// by nothing.
    pub fn touched_x(&mut self, value: i32, range: (i32, i32)) {
        if let Some(fraction) = fraction_of(value, range) {
            self.x = fraction * f64::from(self.panel.width);
        }
    }

    /// The other axis.
    pub fn touched_y(&mut self, value: i32, range: (i32, i32)) {
        if let Some(fraction) = fraction_of(value, range) {
            self.y = fraction * f64::from(self.panel.height);
        }
    }
}

/// Where `value` sits between the ends of `range`, as 0 to 1.
fn fraction_of(value: i32, range: (i32, i32)) -> Option<f64> {
    let (low, high) = range;
    let span = f64::from(high) - f64::from(low);
    if span <= 0.0 {
        return None;
    }
    Some(((f64::from(value) - f64::from(low)) / span).clamp(0.0, 1.0))
}

/// Why a device is not being read, when the answer is not "it is".
enum NotOpened {
    /// A device this never wanted. Most of `/dev/input` is keyboards, lid
    /// switches and audio jacks, and a device can be unplugged between the
    /// listing and the open; there is nothing to say about either.
    NotWanted,
    /// The kernel would not let this process open it.
    ///
    /// **Never ordinary, and it used to be silent.** The service is given the
    /// `input` group on purpose, so a refusal means something else is in the
    /// way — and what that something was, for as long as this file has existed,
    /// was the renderer's own unit: a `DeviceAllow=` line for the GPU turns the
    /// device policy into a whitelist and closes everything not on it. Every
    /// device refused, every refusal was counted as "not a pointer", and the
    /// result was a System screen nobody could touch with not one line in the
    /// journal to say why.
    Refused(std::io::Error),
}

/// Whether the kernel refusing a device is worth saying out loud.
///
/// `EACCES` and `EPERM` both arrive as `PermissionDenied`, and both mean the
/// same thing here: this was meant to be allowed and is not. Anything else is
/// the ordinary race with a device being unplugged.
const fn is_a_refusal(kind: std::io::ErrorKind) -> bool {
    matches!(kind, std::io::ErrorKind::PermissionDenied)
}

/// Devices that have been looked at, are not pointers, and are not looked at
/// again.
///
/// **Looking at one is expensive, and looking at all of them every pass is why
/// the pointer crawled.** Measured on the reference machine: a device that *is* a
/// pointer costs under 100 microseconds to examine, and every device that is not
/// costs between 24 and 76 milliseconds — a keyboard, a lid switch, a power
/// button, an audio jack. Twenty-four of those is 1.11 seconds, and the loop did
/// it before reading a single event, so the pointer could move about once a
/// second. Every kind of device looked broken rather than slow (owner,
/// 2026-09-26: "auch Maus grundsätzlich und am ende auch Touchscreen").
///
/// Keyed by the node's inode as well as its path, because `/dev/input/event20` is
/// handed to whatever is plugged in next: devtmpfs gives a new device a new
/// inode, so a recycled node does not match what is remembered here and is
/// looked at again.
///
/// A device the kernel *refused* is deliberately not remembered. That is a
/// configuration fault which can be corrected while this is running, and
/// retrying costs nothing — the refusal comes before any of the work that makes
/// looking at a device expensive.
#[derive(Default)]
struct Ignored(HashMap<PathBuf, u64>);

impl Ignored {
    /// Whether this exact node has already been looked at and passed over.
    fn already(&self, path: &Path, node: u64) -> bool {
        self.0.get(path) == Some(&node)
    }

    /// Remembers that it is not a pointer.
    fn pass_over(&mut self, path: PathBuf, node: u64) {
        self.0.insert(path, node);
    }

    /// Forgets nodes that are no longer there, so this cannot grow for ever on a
    /// machine where things are plugged in and out all evening.
    fn keep_only(&mut self, there: &HashSet<PathBuf>) {
        self.0.retain(|path, _| there.contains(path));
    }
}

/// What has already been said, so a rescan every two seconds says it once.
#[derive(Default)]
struct Said {
    /// That there is no `/dev/input` at all.
    no_directory: bool,
    /// That the kernel refused a device.
    refused: bool,
    /// That there is nothing on this machine to point with.
    nothing_to_point_with: bool,
}

/// What a device is, once it has been looked at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Reports where on the screen a finger is.
    Touchscreen,
    /// Reports how far something moved.
    Mouse,
    /// Reports where on *itself* a finger is, which becomes a movement once the
    /// difference between two reports is taken.
    Touchpad,
    /// Reports something this does not turn into pointer movement.
    NotAPointer,
}

/// Decides what a device is from what it says it can do.
///
/// Separated from opening one so the decision can be tested without a device:
/// which of these four a touchpad is, is the thing most likely to be got wrong
/// and the hardest to notice on a machine that has no touchscreen to compare it
/// against.
///
/// **The order is the whole of it.** `INPUT_PROP_DIRECT` means the surface is
/// the screen, so it decides first. Relative axes come next, because a pad that
/// also reports movement has done this arithmetic in its own firmware and doing
/// it again would double every gesture. Only then is an absolute
/// `INPUT_PROP_POINTER` device a pad — the property is what separates it from a
/// joystick or a dial, which report absolute positions of something that is not
/// a pointer at all.
#[must_use]
pub const fn kind_of(direct: bool, pointer: bool, absolute_xy: bool, relative_xy: bool) -> Kind {
    if absolute_xy && direct {
        Kind::Touchscreen
    } else if relative_xy {
        Kind::Mouse
    } else if absolute_xy && pointer {
        Kind::Touchpad
    } else {
        Kind::NotAPointer
    }
}

/// Watches the panel's input devices and says what happened on it.
///
/// Dropped when the System screen's output goes, which is what stops the thread
/// and closes the devices.
#[derive(Debug)]
pub struct Watcher {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Watcher {
    /// Starts watching, sending what happens to `send`.
    ///
    /// `send` is called on this watcher's own thread and may block — it writes a
    /// socket. That is why it is not called on the render thread.
    pub fn watching(
        panel: SizePx,
        send: impl FnMut(ovs_console_wire::ToConsole) + Send + 'static,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("ovs-console-input".to_owned())
            .spawn(move || watch(panel, send, &stopping))
            .inspect_err(|error| {
                tracing::error!("the System screen cannot be touched: {error}");
            })
            .ok();

        Self { stop, thread }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Not joined: the thread is in a blocking read of a device nobody is
        // going to touch, and waiting for it would hold up whatever is rebuilding
        // the output. It notices the flag when its read ends.
        drop(self.thread.take());
    }
}

/// Opens what is there, reads it, and looks for more.
fn watch(panel: SizePx, mut send: impl FnMut(ovs_console_wire::ToConsole), stop: &Arc<AtomicBool>) {
    let mut cursor = Cursor::middle_of(panel);
    let mut open: Vec<Opened> = Vec::new();
    let mut ignored = Ignored::default();
    let mut said = Said::default();
    // `None` so the first pass scans. Afterwards a scan happens every `RESCAN`,
    // which is what that constant has always said it does — the loop called this
    // on every pass, and a pass is as often as the thread can manage.
    let mut scanned: Option<Instant> = None;

    while !stop.load(Ordering::Relaxed) {
        if scanned.is_none_or(|at| at.elapsed() >= RESCAN) {
            scanned = Some(Instant::now());
            adopt_new_devices(&mut open, &mut ignored, &mut said);
        }

        // Nothing to read from, so nothing to do but look again.
        if open.is_empty() {
            std::thread::sleep(RESCAN);
            continue;
        }

        let mut anything = false;
        open.retain_mut(|device| match device.drain(&mut cursor, &mut send) {
            Ok(read) => {
                anything |= read;
                true
            }
            Err(error) => {
                tracing::info!(
                    device = %device.path.display(),
                    "this input device has gone: {error}"
                );
                false
            }
        });

        // **Polled rather than blocked on.** A blocking read per device needs a
        // thread per device or an epoll; polling eight devices costs eight
        // non-blocking reads that return nothing, and the interval is what a
        // press waits for. Eight milliseconds is half a frame at sixty and
        // invisible to a finger.
        if !anything {
            std::thread::sleep(Duration::from_millis(8));
        }
    }
}

/// Opens any pointer device that is not open already.
fn adopt_new_devices(open: &mut Vec<Opened>, ignored: &mut Ignored, said: &mut Said) {
    let Ok(entries) = std::fs::read_dir(DEVICES) else {
        if !said.no_directory {
            said.no_directory = true;
            tracing::warn!("there is no {DEVICES} to read, so the System screen cannot be touched");
        }
        return;
    };

    let mut refusals = 0_usize;
    let mut refused: Option<(PathBuf, std::io::Error)> = None;
    let mut there: HashSet<PathBuf> = HashSet::new();

    for entry in entries.flatten() {
        let path = entry.path();
        if !path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("event"))
        {
            continue;
        }
        there.insert(path.clone());

        if open.iter().any(|device| device.path == path) {
            continue;
        }
        // Which node this is, not only where it is: see `Ignored`. A node whose
        // metadata cannot be read is looked at rather than remembered, because
        // remembering it under the wrong identity would hide a real device.
        let node = entry.metadata().ok().map(|about| about.ino());
        if node.is_some_and(|node| ignored.already(&path, node)) {
            continue;
        }

        match Opened::open(&path) {
            Ok(opened) => open.push(opened),
            Err(NotOpened::NotWanted) => {
                if let Some(node) = node {
                    ignored.pass_over(path, node);
                }
            }
            Err(NotOpened::Refused(error)) => {
                refusals += 1;
                if refused.is_none() {
                    refused = Some((path, error));
                }
            }
        }
    }

    ignored.keep_only(&there);

    // **Said, where it used to be swallowed.** A refusal is a configuration
    // fault, and the shape it takes is every device refusing at once — so it
    // carries a count and one example rather than arriving twenty-eight times,
    // and it is said once rather than every two seconds for the rest of the
    // evening. The cure is named in the message because the message is the only
    // thing the person reading the journal has.
    if refusals > 0 && !said.refused {
        said.refused = true;
        if let Some((path, error)) = refused.as_ref() {
            tracing::error!(
                refused = refusals,
                opened = open.len(),
                device = %path.display(),
                "the kernel will not let this process read the input devices, so the System \
                 screen cannot be touched: {error}. The service is given the `input` group for \
                 this, which is not enough on its own — any `DeviceAllow=` line in its unit \
                 turns the device policy into a whitelist, and then `char-input` has to be on it"
            );
        }
    }

    // A rack with nothing plugged into it, or a panel whose driver did not come
    // up. Worth one line, because it is the difference between "this is broken"
    // and "there is nothing here for it to do".
    if open.is_empty() && refusals == 0 && !said.nothing_to_point_with {
        said.nothing_to_point_with = true;
        tracing::info!(
            "no touchscreen, touchpad or mouse on this machine, so there is nothing to point \
             at the System screen with"
        );
    }
}

/// What a touchpad is doing, none of which a mouse needs.
#[derive(Debug)]
struct Pad {
    /// How many of the pad's own units make a whole traverse, per axis.
    ///
    /// The scale between a gesture and the panel, and the reason there is no
    /// sensitivity constant in this file.
    x_span: f64,
    y_span: f64,
    /// Where the finger is, as last reported — one axis at a time, because a
    /// report carries only the axis that changed and a gesture is both.
    x: Option<i32>,
    y: Option<i32>,
    /// The position the next movement is measured from.
    ///
    /// `None` between touches, and that is the whole point of it: without
    /// forgetting where the last gesture ended, a finger put down somewhere else
    /// throws the pointer across the panel by the distance between the two
    /// places it was touched.
    from: Option<(i32, i32)>,
    /// Whether a finger is on the pad at all.
    touching: bool,
    /// Whether more than one is. The emulated position while two fingers are
    /// down is neither of them, so nothing moves until one leaves.
    many: bool,
}

impl Pad {
    /// A pad whose axes span these ranges, with nothing touching it.
    fn spanning(x_range: (i32, i32), y_range: (i32, i32)) -> Self {
        Self {
            x_span: f64::from(x_range.1) - f64::from(x_range.0),
            y_span: f64::from(y_range.1) - f64::from(y_range.0),
            x: None,
            y: None,
            from: None,
            touching: false,
            many: false,
        }
    }

    /// Moves the pointer by however far the finger went since the last report.
    ///
    /// Says whether it moved anything. A pad whose driver has not said how big
    /// it is is left alone rather than divided by, the same as a touchscreen's
    /// empty range.
    fn slide(&mut self, cursor: &mut Cursor) -> bool {
        let (Some(x), Some(y)) = (self.x, self.y) else {
            return false;
        };
        if !self.touching || self.many {
            self.from = None;
            return false;
        }

        let moved = match self.from {
            Some((was_x, was_y)) if self.x_span > 0.0 && self.y_span > 0.0 => {
                let dx = (f64::from(x) - f64::from(was_x)) / self.x_span;
                let dy = (f64::from(y) - f64::from(was_y)) / self.y_span;
                if dx == 0.0 && dy == 0.0 {
                    false
                } else {
                    cursor.slid(dx, dy);
                    true
                }
            }
            // The first report of a gesture is where it starts, and a start is
            // not a movement.
            _ => false,
        };
        self.from = Some((x, y));
        moved
    }
}

/// One device this is reading.
struct Opened {
    path: PathBuf,
    device: evdev::Device,
    kind: Kind,
    /// The ranges the touchscreen reports for its two axes.
    x_range: (i32, i32),
    y_range: (i32, i32),
    /// Whether a press is currently down, so an up is only sent for a down.
    down: bool,
    /// The pad's state, for a device that reports finger positions of itself.
    pad: Option<Pad>,
}

impl Opened {
    /// Opens a device if it is a pointer, and says what it is.
    fn open(path: &Path) -> Result<Self, NotOpened> {
        let device = match evdev::Device::open(path) {
            Ok(device) => device,
            // **Told apart, not lumped together.** This arm used to be
            // `Err(_) => return None`, which made a service that may not open
            // any device indistinguishable from a machine full of keyboards.
            Err(error) if is_a_refusal(error.kind()) => {
                return Err(NotOpened::Refused(error));
            }
            // The ordinary case: most of `/dev/input` is keyboards, lid
            // switches and audio jacks. Nothing is said, because there is
            // nothing to say about a device that was never wanted.
            Err(_) => return Err(NotOpened::NotWanted),
        };

        let properties = device.properties();
        let direct = properties.contains(evdev::PropType::DIRECT);
        let pointer = properties.contains(evdev::PropType::POINTER);
        let absolute = device.supported_absolute_axes().is_some_and(|axes| {
            axes.contains(evdev::AbsoluteAxisCode::ABS_X)
                && axes.contains(evdev::AbsoluteAxisCode::ABS_Y)
        });
        let relative = device.supported_relative_axes().is_some_and(|axes| {
            axes.contains(evdev::RelativeAxisCode::REL_X)
                && axes.contains(evdev::RelativeAxisCode::REL_Y)
        });

        let kind = kind_of(direct, pointer, absolute, relative);
        if kind == Kind::NotAPointer {
            return Err(NotOpened::NotWanted);
        }

        // **Non-blocking, because one thread reads all of them.** A blocking
        // read would need a thread per device or an epoll; the loop polls
        // instead, and a read with nothing behind it has to say so rather than
        // wait for a finger.
        if let Err(error) = device.set_nonblocking(true) {
            tracing::warn!(
                device = %path.display(),
                "this device cannot be polled: {error}"
            );
            return Err(NotOpened::NotWanted);
        }

        let (x_range, y_range) = absolute_ranges(&device);
        // Only a pad carries this. A mouse has no position of its own to
        // remember, and a touchscreen's position is the answer rather than
        // something to subtract from.
        let pad = (kind == Kind::Touchpad).then(|| Pad::spanning(x_range, y_range));
        tracing::info!(
            device = %path.display(),
            name = device.name().unwrap_or("?"),
            kind = ?kind,
            "this device reaches the System screen"
        );

        Ok(Self {
            path: path.to_path_buf(),
            device,
            kind,
            x_range,
            y_range,
            down: false,
            pad,
        })
    }

    /// Reads everything waiting and sends what it means. Says whether there was any.
    fn drain(
        &mut self,
        cursor: &mut Cursor,
        send: &mut impl FnMut(ovs_console_wire::ToConsole),
    ) -> std::io::Result<bool> {
        let events: Vec<evdev::InputEvent> = match self.device.fetch_events() {
            Ok(events) => events.collect(),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(false),
            Err(error) => return Err(error),
        };
        if events.is_empty() {
            return Ok(false);
        }

        // Where the pointer ends up after this batch, sent once. A mouse
        // reports each axis separately and a touchscreen reports a whole
        // position; sending after every one of them would be two moves for one
        // movement.
        let mut moved = false;
        let mut presses: Vec<bool> = Vec::new();
        let mut scroll = (0_i32, 0_i32);

        for event in events {
            match event.destructure() {
                evdev::EventSummary::RelativeAxis(_, axis, value) => match axis {
                    evdev::RelativeAxisCode::REL_X => {
                        cursor.by(f64::from(value), 0.0);
                        moved = true;
                    }
                    evdev::RelativeAxisCode::REL_Y => {
                        cursor.by(0.0, f64::from(value));
                        moved = true;
                    }
                    // One notch, in the units a page expects: a wheel event's
                    // value is notches and a browser's is pixels.
                    evdev::RelativeAxisCode::REL_WHEEL => scroll.1 += value * NOTCH,
                    evdev::RelativeAxisCode::REL_HWHEEL => scroll.0 += value * NOTCH,
                    _ => {}
                },
                evdev::EventSummary::AbsoluteAxis(_, axis, value)
                    if self.kind == Kind::Touchscreen =>
                {
                    match axis {
                        evdev::AbsoluteAxisCode::ABS_X
                        | evdev::AbsoluteAxisCode::ABS_MT_POSITION_X => {
                            cursor.touched_x(value, self.x_range);
                            moved = true;
                        }
                        evdev::AbsoluteAxisCode::ABS_Y
                        | evdev::AbsoluteAxisCode::ABS_MT_POSITION_Y => {
                            cursor.touched_y(value, self.y_range);
                            moved = true;
                        }
                        _ => {}
                    }
                }
                // A pad reports where on *itself* the finger is. Kept rather
                // than used: the movement is worked out after the batch,
                // because a report carries one axis and a gesture is both.
                evdev::EventSummary::AbsoluteAxis(_, axis, value)
                    if self.kind == Kind::Touchpad =>
                {
                    if let Some(pad) = self.pad.as_mut() {
                        match axis {
                            evdev::AbsoluteAxisCode::ABS_X => pad.x = Some(value),
                            evdev::AbsoluteAxisCode::ABS_Y => pad.y = Some(value),
                            _ => {}
                        }
                    }
                }
                // **A finger on the pad, which is not a click.** Letting this
                // reach the arm below would press the page every time a hand
                // settled on the pad. What clicks a clickpad is `BTN_LEFT`,
                // under its whole surface, and that still does.
                evdev::EventSummary::Key(_, evdev::KeyCode::BTN_TOUCH, value)
                    if self.kind == Kind::Touchpad && value != 2 =>
                {
                    if let Some(pad) = self.pad.as_mut() {
                        pad.touching = value == 1;
                    }
                }
                // How many fingers are down, which is all this needs to know
                // about a gesture: the emulated position while there is more
                // than one is neither of them.
                evdev::EventSummary::Key(
                    _,
                    evdev::KeyCode::BTN_TOOL_DOUBLETAP
                    | evdev::KeyCode::BTN_TOOL_TRIPLETAP
                    | evdev::KeyCode::BTN_TOOL_QUADTAP
                    | evdev::KeyCode::BTN_TOOL_QUINTTAP,
                    value,
                ) if value != 2 => {
                    if let Some(pad) = self.pad.as_mut() {
                        pad.many = value == 1;
                    }
                }
                // 2 is a key repeat, which a button does not have: a finger
                // held down is one press, not a stream of them.
                evdev::EventSummary::Key(
                    _,
                    evdev::KeyCode::BTN_LEFT | evdev::KeyCode::BTN_TOUCH,
                    value,
                ) if value != 2 => presses.push(value == 1),
                _ => {}
            }
        }

        // The pad's movement, once, after everything in the batch has been
        // read — and before the position is taken, so a click in the same batch
        // lands where the finger left the pointer.
        if let Some(pad) = self.pad.as_mut() {
            moved |= pad.slide(cursor);
        }

        let (x, y) = cursor.at();
        if moved {
            send(ovs_console_wire::ToConsole::Pointer { x, y });
        }
        for down in presses {
            // A device that reports a press it never released, or releases
            // twice, would otherwise leave the page with a button held.
            if down == self.down {
                continue;
            }
            self.down = down;
            send(ovs_console_wire::ToConsole::Button { x, y, down });
        }
        if scroll != (0, 0) {
            send(ovs_console_wire::ToConsole::Scroll {
                x,
                y,
                dx: scroll.0,
                dy: scroll.1,
            });
        }

        Ok(true)
    }
}

/// How far one notch of a wheel scrolls, in pixels.
///
/// What a browser uses for a line of text on a desktop. A System screen has
/// nothing long enough to scroll today, and the day it has, this is the number
/// that decides whether one notch feels like one.
const NOTCH: i32 = 53;

/// The ranges a device reports for its two absolute axes.
fn absolute_ranges(device: &evdev::Device) -> ((i32, i32), (i32, i32)) {
    let mut x = (0, 0);
    let mut y = (0, 0);
    if let Ok(states) = device.get_abs_state() {
        // Indexed by the axis code, which is what the array is: one entry per
        // axis the kernel knows about, whether or not this device has it. A
        // range of nothing is what an axis it does not have reports, and
        // `Cursor` ignores those rather than dividing by them.
        let read = |axis: evdev::AbsoluteAxisCode, into: &mut (i32, i32)| {
            if let Some(state) = states.get(usize::from(axis.0)) {
                if state.maximum > state.minimum {
                    *into = (state.minimum, state.maximum);
                }
            }
        };
        // The multi-touch axes second, so a device that reports both is measured
        // by the ones its fingers actually arrive on.
        read(evdev::AbsoluteAxisCode::ABS_X, &mut x);
        read(evdev::AbsoluteAxisCode::ABS_Y, &mut y);
        read(evdev::AbsoluteAxisCode::ABS_MT_POSITION_X, &mut x);
        read(evdev::AbsoluteAxisCode::ABS_MT_POSITION_Y, &mut y);
    }
    (x, y)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PANEL: SizePx = SizePx {
        width: 1024,
        height: 600,
    };

    #[test]
    fn a_touchpad_is_not_mistaken_for_a_touchscreen() {
        // **The one that matters.** A touchpad reports absolute coordinates of
        // the *pad*; used as screen coordinates the pointer jumps to wherever
        // the finger landed. What tells them apart is one property bit.
        assert_eq!(
            kind_of(true, false, true, false),
            Kind::Touchscreen,
            "a direct absolute device is a touchscreen"
        );
        assert_eq!(
            kind_of(false, true, true, false),
            Kind::Touchpad,
            "an absolute pointer device is a pad, and is driven as one"
        );
        assert_eq!(
            kind_of(false, false, true, false),
            Kind::NotAPointer,
            "an absolute device that is neither is a joystick or a dial, and is left alone"
        );
        assert_eq!(
            kind_of(false, false, false, true),
            Kind::Mouse,
            "relative axes are a mouse"
        );
        assert_eq!(
            kind_of(false, true, true, true),
            Kind::Mouse,
            "a pad that also reports movement has done this arithmetic already"
        );
        assert_eq!(kind_of(false, false, false, false), Kind::NotAPointer);
    }

    #[test]
    fn a_finger_crossing_the_pad_takes_the_pointer_the_same_way_across_the_panel() {
        let mut cursor = Cursor::middle_of(PANEL);
        // The shape of the pad on the machine this was written against.
        let mut pad = Pad::spanning((0, 1200), (0, 800));

        // **A finger arrives somewhere the pointer is not.** Arriving is not
        // moving: taking the pad's coordinates for the screen's would put the
        // pointer at the finger instead, which is the whole bug this avoids.
        pad.touching = true;
        pad.x = Some(100);
        pad.y = Some(700);
        assert!(
            !pad.slide(&mut cursor),
            "putting a finger down moved the pointer"
        );
        assert_eq!(cursor.at(), (512, 300), "the pointer jumped to the finger");

        // A quarter of the way across the pad is a quarter of the way across the
        // panel, whatever either of them measures.
        pad.x = Some(400);
        assert!(pad.slide(&mut cursor));
        assert_eq!(
            cursor.at(),
            (512 + PANEL.width / 4, 300),
            "a quarter of the pad was not a quarter of the panel"
        );

        // Lifted, and put down at the other end. The reference position exists
        // for exactly this.
        let before = cursor.at();
        pad.touching = false;
        assert!(!pad.slide(&mut cursor));
        pad.touching = true;
        pad.x = Some(1100);
        pad.y = Some(100);
        assert!(
            !pad.slide(&mut cursor),
            "the pointer leapt to the new finger"
        );
        assert_eq!(cursor.at(), before);

        // A second finger stops the pointer rather than flinging it at whatever
        // the emulated position becomes, and going back to one starts over.
        pad.many = true;
        pad.x = Some(200);
        assert!(!pad.slide(&mut cursor), "two fingers moved the pointer");
        assert_eq!(cursor.at(), before);
        pad.many = false;
        assert!(
            !pad.slide(&mut cursor),
            "the finger left over moved the pointer"
        );
        assert_eq!(cursor.at(), before);
        pad.x = Some(500);
        assert!(
            pad.slide(&mut cursor),
            "the pad stopped working after a gesture"
        );

        // A driver that has not said how big the pad is is left alone rather
        // than divided by.
        let mut unmeasured = Pad::spanning((0, 0), (0, 0));
        unmeasured.touching = true;
        unmeasured.x = Some(10);
        unmeasured.y = Some(10);
        assert!(!unmeasured.slide(&mut cursor));
        let before = cursor.at();
        unmeasured.x = Some(600);
        assert!(
            !unmeasured.slide(&mut cursor),
            "a pad of no size moved the pointer"
        );
        assert_eq!(cursor.at(), before);
    }

    #[test]
    fn a_finger_lands_where_the_panel_is_touched() {
        let mut cursor = Cursor::middle_of(PANEL);
        assert_eq!(cursor.at(), (512, 300), "the pointer starts in the middle");

        // A device reporting 0..4095 on both axes, as a resistive panel does.
        let range = (0, 4095);
        cursor.touched_x(0, range);
        cursor.touched_y(0, range);
        assert_eq!(cursor.at(), (0, 0), "the top left corner");

        cursor.touched_x(4095, range);
        cursor.touched_y(4095, range);
        assert_eq!(
            cursor.at(),
            (1023, 599),
            "the bottom right, and inside the panel rather than one past it"
        );

        // A range that does not start at zero, which plenty report.
        cursor.touched_x(2048, (1024, 3072));
        assert_eq!(cursor.at().0, 512, "halfway along an offset range");
    }

    #[test]
    fn a_device_that_reports_no_range_is_ignored_rather_than_divided_by() {
        let mut cursor = Cursor::middle_of(PANEL);
        cursor.touched_x(500, (0, 0));
        cursor.touched_y(500, (7, 7));
        assert_eq!(
            cursor.at(),
            (512, 300),
            "a range of nothing moved the pointer"
        );
    }

    #[test]
    fn a_mouse_cannot_be_pushed_off_the_panel() {
        // It is the only way a pointer leaves: a mouse reports movement for as
        // long as somebody keeps moving it, and a pointer that had wandered to
        // -4000 would take four thousand pixels of movement to come back.
        let mut cursor = Cursor::middle_of(PANEL);
        cursor.by(-100_000.0, -100_000.0);
        assert_eq!(cursor.at(), (0, 0));

        cursor.by(1.0, 1.0);
        assert_eq!(cursor.at(), (1, 1), "and it comes back immediately");

        cursor.by(100_000.0, 100_000.0);
        assert_eq!(cursor.at(), (1023, 599));
    }

    /// A touchscreen, invented, pressed in a corner, and followed to the panel.
    ///
    /// **Run it by hand, as root:**
    ///
    /// ```text
    /// sudo -E $HOME/.cargo/bin/cargo test -p ovs-renderer --lib \
    ///     -- --ignored --nocapture a_touchscreen_this_machine_does_not_have
    /// ```
    ///
    /// Ignored because `/dev/uinput` is `root`-only and a test that fails for
    /// everybody who is not root is a test people learn to skip. It is here
    /// because the reference Service Display is a touch panel and the machine
    /// this is proved on is a laptop: without it, the touchscreen path would ship
    /// having never run.
    #[test]
    #[ignore = "needs /dev/uinput, which is root-only"]
    fn a_touchscreen_this_machine_does_not_have() {
        use std::sync::mpsc;

        // **The property that decides everything.** Without `DIRECT` this is a
        // touchpad as far as `kind_of` is concerned, and is left alone.
        let mut direct = evdev::AttributeSet::<evdev::PropType>::new();
        direct.insert(evdev::PropType::DIRECT);
        let mut touch = evdev::AttributeSet::<evdev::KeyCode>::new();
        touch.insert(evdev::KeyCode::BTN_TOUCH);

        let mut screen = evdev::uinput::VirtualDevice::builder()
            .expect("/dev/uinput — run this as root")
            .name("OVS test touchscreen")
            .with_properties(&direct)
            .expect("properties")
            .with_keys(&touch)
            .expect("a touch button")
            .with_absolute_axis(&evdev::UinputAbsSetup::new(
                evdev::AbsoluteAxisCode::ABS_X,
                evdev::AbsInfo::new(0, 0, 4095, 0, 0, 1),
            ))
            .expect("an x axis")
            .with_absolute_axis(&evdev::UinputAbsSetup::new(
                evdev::AbsoluteAxisCode::ABS_Y,
                evdev::AbsInfo::new(0, 0, 4095, 0, 0, 1),
            ))
            .expect("a y axis")
            .build()
            .expect("a virtual touchscreen");

        // The watcher finds it the way it finds any other: by looking in
        // /dev/input. Nothing tells it this one is not real.
        let (said, heard) = mpsc::channel();
        let watcher = Watcher::watching(PANEL, move |what| {
            let _ = said.send(what);
        });
        std::thread::sleep(Duration::from_millis(300));

        // The bottom right corner, pressed and released.
        let press = [
            evdev::InputEvent::new(
                evdev::EventType::ABSOLUTE.0,
                evdev::AbsoluteAxisCode::ABS_X.0,
                4095,
            ),
            evdev::InputEvent::new(
                evdev::EventType::ABSOLUTE.0,
                evdev::AbsoluteAxisCode::ABS_Y.0,
                4095,
            ),
            evdev::InputEvent::new(evdev::EventType::KEY.0, evdev::KeyCode::BTN_TOUCH.0, 1),
        ];
        screen.emit(&press).expect("a press");
        std::thread::sleep(Duration::from_millis(150));
        screen
            .emit(&[evdev::InputEvent::new(
                evdev::EventType::KEY.0,
                evdev::KeyCode::BTN_TOUCH.0,
                0,
            )])
            .expect("a release");

        let mut seen = Vec::new();
        let until = std::time::Instant::now() + Duration::from_secs(3);
        while std::time::Instant::now() < until {
            match heard.recv_timeout(Duration::from_millis(100)) {
                Ok(what) => seen.push(what),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        drop(watcher);

        // A real mouse on the machine may have moved while this ran, so what is
        // asserted is that the expected things are among what was seen — not
        // that nothing else was.
        let corner = (PANEL.width - 1, PANEL.height - 1);
        assert!(
            seen.iter().any(|what| matches!(
                what,
                ovs_console_wire::ToConsole::Pointer { x, y } if (*x, *y) == corner
            )),
            "the finger did not reach the far corner of the panel: {seen:?}"
        );
        assert!(
            seen.iter()
                .any(|what| matches!(what, ovs_console_wire::ToConsole::Button { down: true, .. })),
            "the press never arrived: {seen:?}"
        );
        assert!(
            seen.iter().any(|what| matches!(
                what,
                ovs_console_wire::ToConsole::Button { down: false, .. }
            )),
            "the finger was never lifted: {seen:?}"
        );
    }

    #[test]
    fn a_device_the_kernel_refuses_is_not_mistaken_for_one_nobody_wanted() {
        // **The whole of the bug this is here for.** Every failure to open was
        // counted as "not a pointer", so a unit whose `DeviceAllow` list left
        // `char-input` off produced a System screen nobody could touch and an
        // empty journal. Telling the two apart is the fix, and this is the line
        // that does it.
        assert!(is_a_refusal(std::io::ErrorKind::PermissionDenied));
        assert!(
            !is_a_refusal(std::io::ErrorKind::NotFound),
            "a device unplugged between the listing and the open is not a fault"
        );

        let directory =
            std::env::temp_dir().join(format!("ovs-input-refused-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("a directory");
        let shut = directory.join("event0");
        std::fs::write(&shut, b"not a device").expect("a file");
        std::fs::set_permissions(&shut, std::os::unix::fs::PermissionsExt::from_mode(0o000))
            .expect("no permissions at all");

        // Root ignores the mode bits, so there would be nothing to classify and
        // nothing for this to say. Asked rather than assumed, because the test
        // that proves the touchscreen path is run as root.
        if std::fs::File::open(&shut).is_ok() {
            let _ = std::fs::remove_dir_all(&directory);
            return;
        }

        assert!(
            matches!(Opened::open(&shut), Err(NotOpened::Refused(_))),
            "a device this process may not open was reported as one nobody wanted"
        );

        let _ = std::fs::remove_dir_all(&directory);
    }

    /// A touchpad, invented, with a finger dragged across it.
    ///
    /// **Run it by hand, as root:**
    ///
    /// ```text
    /// sudo -E $HOME/.cargo/bin/cargo test -p ovs-renderer --lib \
    ///     -- --ignored --nocapture a_pad_drives_the_pointer
    /// ```
    ///
    /// Ignored for the same reason as the touchscreen above: `/dev/uinput` is
    /// root-only. It is here because the arithmetic being right is not the same
    /// as a pad driving the pointer, and the pad that prompted this work is on a
    /// machine that cannot run a test against itself.
    #[test]
    #[ignore = "needs /dev/uinput, which is root-only"]
    fn a_pad_drives_the_pointer() {
        // **The property that makes it a pad.** Without `POINTER` and with no
        // relative axes this is a joystick as far as `kind_of` is concerned.
        let mut pointer = evdev::AttributeSet::<evdev::PropType>::new();
        pointer.insert(evdev::PropType::POINTER);
        let mut buttons = evdev::AttributeSet::<evdev::KeyCode>::new();
        buttons.insert(evdev::KeyCode::BTN_TOUCH);
        buttons.insert(evdev::KeyCode::BTN_LEFT);
        buttons.insert(evdev::KeyCode::BTN_TOOL_FINGER);

        let mut pad = evdev::uinput::VirtualDevice::builder()
            .expect("/dev/uinput — run this as root")
            .name("OVS test touchpad")
            .with_properties(&pointer)
            .expect("properties")
            .with_keys(&buttons)
            .expect("buttons")
            .with_absolute_axis(&evdev::UinputAbsSetup::new(
                evdev::AbsoluteAxisCode::ABS_X,
                evdev::AbsInfo::new(0, 0, 1200, 0, 0, 12),
            ))
            .expect("an x axis")
            .with_absolute_axis(&evdev::UinputAbsSetup::new(
                evdev::AbsoluteAxisCode::ABS_Y,
                evdev::AbsInfo::new(0, 0, 800, 0, 0, 12),
            ))
            .expect("a y axis")
            .build()
            .expect("a virtual touchpad");

        // **Read directly, not through a `Watcher`.** A watcher opens every
        // device on the machine, so a real touchpad under somebody's hand moves
        // the same pointer: the first two runs of this measured the owner's
        // finger rather than the invented one, and disagreed with each other.
        // Finding devices is proved by the touchscreen test above; what is
        // proved here is the arithmetic, and that has to be alone with its own
        // device.
        let mine = std::fs::read_dir(DEVICES)
            .expect("/dev/input")
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("event"))
            })
            .find(|path| {
                evdev::Device::open(path)
                    .ok()
                    .and_then(|device| device.name().map(|name| name == "OVS test touchpad"))
                    .unwrap_or(false)
            })
            .expect("the invented pad never appeared in /dev/input");

        // Opened the way the watcher opens one, so a failure here says the
        // classification is wrong rather than the movement.
        let Ok(mut opened) = Opened::open(&mine) else {
            panic!("the invented pad at {} was not opened", mine.display())
        };
        assert_eq!(
            opened.kind,
            Kind::Touchpad,
            "the invented pad was taken for something else"
        );
        let state = opened.pad.as_ref().expect("a pad with no pad state");
        assert!(
            state.x_span > 0.0 && state.y_span > 0.0,
            "the pad said it has no size, so nothing it reports can be scaled: {state:?}"
        );

        let at = |x: i32, y: i32| {
            [
                evdev::InputEvent::new(
                    evdev::EventType::ABSOLUTE.0,
                    evdev::AbsoluteAxisCode::ABS_X.0,
                    x,
                ),
                evdev::InputEvent::new(
                    evdev::EventType::ABSOLUTE.0,
                    evdev::AbsoluteAxisCode::ABS_Y.0,
                    y,
                ),
            ]
        };

        let mut cursor = Cursor::middle_of(PANEL);
        let mut seen: Vec<ovs_console_wire::ToConsole> = Vec::new();
        let middle = (PANEL.width / 2, PANEL.height / 2);

        // **The finger lands in a corner of the pad, deliberately.** Starting it
        // in the middle would put the jump-to-the-finger bug and the right
        // answer in the same place, and this would pass either way.
        pad.emit(&[evdev::InputEvent::new(
            evdev::EventType::KEY.0,
            evdev::KeyCode::BTN_TOUCH.0,
            1,
        )])
        .expect("a finger");
        pad.emit(&at(100, 700)).expect("where it landed");
        std::thread::sleep(Duration::from_millis(50));
        opened
            .drain(&mut cursor, &mut |what| seen.push(what))
            .expect("the pad was readable");
        assert_eq!(
            cursor.at(),
            middle,
            "putting a finger down moved the pointer to where the finger was"
        );

        // Dragged a quarter of the way across the pad, and nowhere down.
        for x in (130..=400).step_by(30) {
            pad.emit(&at(x, 700)).expect("a drag");
            std::thread::sleep(Duration::from_millis(20));
            opened
                .drain(&mut cursor, &mut |what| seen.push(what))
                .expect("the pad was readable");
        }

        assert_eq!(
            cursor.at(),
            (middle.0 + PANEL.width / 4, middle.1),
            "a quarter of the pad was not a quarter of the panel; what arrived was {seen:?}"
        );
        assert!(
            seen.iter()
                .any(|what| matches!(what, ovs_console_wire::ToConsole::Pointer { .. })),
            "the pointer arrived in the right place and nothing was ever sent: {seen:?}"
        );
        assert!(
            !seen
                .iter()
                .any(|what| matches!(what, ovs_console_wire::ToConsole::Button { .. })),
            "a finger resting on a pad was reported as a click: {seen:?}"
        );
    }

    /// A scan that finds nothing new costs nothing worth measuring.
    ///
    /// **The regression this exists for.** Examining a device that is not a
    /// pointer costs 24 to 76 ms on the reference machine — a keyboard, a lid
    /// switch, an audio jack — against under 100 us for one that is. All
    /// twenty-eight of its nodes came to 1.11 s, the loop did that before reading
    /// a single event, and so the pointer moved about once a second on a mouse, a
    /// touchpad and a touchscreen alike.
    ///
    /// Only bites where the devices can actually be opened. Elsewhere every one
    /// of them is refused before any of the expensive work, a refusal is cheap,
    /// and this passes for a reason that harms nothing.
    #[test]
    fn a_scan_that_finds_nothing_new_is_cheap() {
        let mut open = Vec::new();
        let mut ignored = Ignored::default();
        let mut said = Said::default();

        // The first is allowed to be slow: it is where every device on the
        // machine is looked at, once.
        adopt_new_devices(&mut open, &mut ignored, &mut said);

        let began = Instant::now();
        for _ in 0..5 {
            adopt_new_devices(&mut open, &mut ignored, &mut said);
        }
        let each = began.elapsed() / 5;
        assert!(
            each < Duration::from_millis(50),
            "a scan that learned nothing took {each:?}, which is a gesture's worth of \
             pointer movement thrown away before it could be read"
        );
    }

    #[test]
    fn the_pointer_is_inside_a_panel_of_any_size() {
        for panel in [
            SizePx {
                width: 1,
                height: 1,
            },
            SizePx {
                width: 800,
                height: 480,
            },
            SizePx {
                width: 3840,
                height: 2160,
            },
        ] {
            let mut cursor = Cursor::middle_of(panel);
            cursor.by(f64::from(panel.width) * 2.0, f64::from(panel.height) * 2.0);
            let (x, y) = cursor.at();
            assert!(x < panel.width, "{x} is outside {}", panel.width);
            assert!(y < panel.height, "{y} is outside {}", panel.height);
        }
    }
}
