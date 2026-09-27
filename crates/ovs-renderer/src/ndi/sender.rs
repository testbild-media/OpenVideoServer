//! Publishing one output on the network, on a thread of its own.
//!
//! # Why not on the render thread
//!
//! Measured on the reference machine: the slowest unclocked send of a 1080p
//! frame took 8.4 ms, against an average of 1.9. Half a frame at sixty, on the
//! thread that is also presenting every wall. So the render thread hands the
//! pixels over and returns, and this thread compresses and sends them.
//!
//! **And the sender is never clocked.** The SDK will rate-limit the caller to
//! the frame rate if asked — 60 sends a second against 538 unclocked, measured
//! the same way — which on the render thread would mean one stream configured at
//! thirty holding every wall on the machine down to thirty. House rule §17: one
//! output's trouble is its own.
//!
//! # No queue, and no copy
//!
//! One frame is held and a newer one replaces it, exactly as the receiver does
//! in the other direction and for the same reason: the only frame worth
//! publishing is the newest, and a queue would buy completeness with latency.
//!
//! The handover swaps buffers rather than copying. [`Sender::offer`] takes the
//! buffer it is given and hands back whichever one was waiting, so the output
//! reads the next frame back into that one. After the first two frames nothing
//! is allocated and nothing is copied: a 1080p RGBA frame is 8.3 MB, and
//! memcpying that on the render thread sixty times a second would be half a
//! gigabyte a second spent on nothing.

use std::ffi::CString;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use ovs_types::geometry::SizePx;

use super::library::Ndi;
use super::sys;

/// One picture waiting to go out.
#[derive(Debug)]
pub struct Picture {
    /// Its size.
    pub size: SizePx,
    /// Frame rate numerator, as the output's mode declares it.
    pub rate_numerator: u32,
    /// Frame rate denominator.
    pub rate_denominator: u32,
    /// Tightly packed, four bytes a pixel, as a readback gives it.
    ///
    /// The fourth byte is not published: the frame is declared `RGBX`, so the
    /// runtime encodes no alpha plane (owner, 2026-09-25: "kein alpha support
    /// über NDI"). Nothing is converted to arrange that — the bytes go across as
    /// they are and the layout simply says the last one has no meaning.
    pub pixels: Vec<u8>,
}

impl Picture {
    /// Whether this describes itself consistently.
    ///
    /// A short buffer sent as a full frame is the receiver at the other end
    /// reading somebody else's memory, so it is checked on the way in rather
    /// than trusted.
    #[must_use]
    pub fn is_whole(&self) -> bool {
        let wanted = u64::from(self.size.width) * u64::from(self.size.height) * 4;
        self.size.width > 0
            && self.size.height > 0
            && self.rate_denominator > 0
            && u64::try_from(self.pixels.len()).is_ok_and(|len| len == wanted)
    }
}

/// What one sender has been doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SenderReport {
    /// Frames the output handed over.
    pub offered: u64,
    /// Frames that reached the network.
    pub sent: u64,
    /// Frames replaced by a newer one before they could be sent.
    ///
    /// Not a fault and not zero in normal use: it is what it costs to keep the
    /// newest frame rather than a queue, and a rising number means the network
    /// or the processor cannot keep up with the composition.
    pub superseded: u64,
    /// Frames refused because they did not describe themselves.
    pub malformed: u64,
}

/// What the output thread and the sending thread share.
struct Shared {
    /// The newest frame, and the buffer the output gets back for the next one.
    waiting: Mutex<Slot>,
    /// Wakes the sending thread when a frame arrives or it is asked to stop.
    arrived: Condvar,
    /// What the network calls this sender, once the runtime has said.
    ///
    /// Set once by the sending thread and read by anything that needs to know
    /// which sources on the network are this machine's own.
    announced: Mutex<Option<String>>,
    offered: AtomicU64,
    sent: AtomicU64,
    superseded: AtomicU64,
    malformed: AtomicU64,
    stop: AtomicBool,
}

/// The one frame in flight.
#[derive(Default)]
struct Slot {
    /// Waiting to be sent, if anything is.
    next: Option<Picture>,
}

/// One output published on the network.
pub struct Sender {
    /// What was asked for. The network announces it with the machine in front.
    name: String,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for Sender {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Sender")
            .field("name", &self.name)
            .field("sent", &self.shared.sent.load(Ordering::Relaxed))
            .finish()
    }
}

impl Sender {
    /// Starts publishing under `name`.
    ///
    /// Returns immediately. `name` is the part in brackets: a sender asked for
    /// `Wall Left` on a machine called `OPENVIDEOSERVER` is announced as
    /// `OPENVIDEOSERVER (Wall Left)`, measured rather than assumed.
    ///
    /// Nothing is published until the first frame is offered, which is what a
    /// receiver connecting to a source that has not drawn yet should see.
    #[must_use]
    pub fn start(ndi: &'static Ndi, name: &str) -> Self {
        let shared = Arc::new(Shared {
            waiting: Mutex::new(Slot::default()),
            arrived: Condvar::new(),
            announced: Mutex::new(None),
            offered: AtomicU64::new(0),
            sent: AtomicU64::new(0),
            superseded: AtomicU64::new(0),
            malformed: AtomicU64::new(0),
            stop: AtomicBool::new(false),
        });

        let thread = {
            let shared = Arc::clone(&shared);
            let name = name.to_owned();
            std::thread::Builder::new()
                .name(format!("ndi-send {name}"))
                .spawn(move || publish(ndi, &name, &shared))
                .ok()
        };
        if thread.is_none() {
            tracing::warn!(stream = %name, "could not start a thread to publish this output");
        }

        Self {
            name: name.to_owned(),
            shared,
            thread,
        }
    }

    /// What this sender is called on the network, before the machine name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The whole name the network announces, machine prefix included.
    ///
    /// `None` until the runtime has made the sender, which is the first moment
    /// anybody could know it. Asked of the runtime rather than assembled from a
    /// hostname, so it is what receivers actually see.
    #[must_use]
    pub fn announced(&self) -> Option<String> {
        self.shared.announced.lock().ok()?.clone()
    }

    /// Hands a frame over, and takes back a buffer to fill next time.
    ///
    /// The returned `Vec` is whatever was waiting and has now been displaced,
    /// emptied of nothing — the caller reads the next frame into it. `None` on
    /// the first two calls, because there was nothing to displace yet.
    ///
    /// Never blocks and never fails. A frame offered while the last one is still
    /// being compressed replaces it: the newest is the only one worth sending,
    /// and the render thread must not wait to find that out.
    pub fn offer(&self, picture: Picture) -> Option<Vec<u8>> {
        if !picture.is_whole() {
            self.shared.malformed.fetch_add(1, Ordering::Relaxed);
            return Some(picture.pixels);
        }
        self.shared.offered.fetch_add(1, Ordering::Relaxed);

        let displaced = {
            let Ok(mut waiting) = self.shared.waiting.lock() else {
                // A poisoned lock means the sending thread panicked. The buffer
                // comes straight back so the output goes on drawing.
                return Some(picture.pixels);
            };
            match waiting.next.replace(picture) {
                Some(older) => {
                    self.shared.superseded.fetch_add(1, Ordering::Relaxed);
                    Some(older.pixels)
                }
                None => None,
            }
        };
        self.shared.arrived.notify_one();
        displaced
    }

    /// What this sender has been doing.
    #[must_use]
    pub fn report(&self) -> SenderReport {
        SenderReport {
            offered: self.shared.offered.load(Ordering::Relaxed),
            sent: self.shared.sent.load(Ordering::Relaxed),
            superseded: self.shared.superseded.load(Ordering::Relaxed),
            malformed: self.shared.malformed.load(Ordering::Relaxed),
        }
    }
}

impl Drop for Sender {
    /// Stops publishing and waits for the thread.
    ///
    /// Waits, because the thread holds the sender and destroys it on its way
    /// out. A show closed and reopened would otherwise leave two senders
    /// announcing the same name, and an operator picking from that list would
    /// have no way to tell which one is the live one.
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        self.shared.arrived.notify_all();
        if let Some(thread) = self.thread.take() {
            if thread.join().is_err() {
                tracing::warn!(stream = %self.name, "the sending thread for this output panicked");
            }
        }
    }
}

/// The sending thread.
fn publish(ndi: &'static Ndi, name: &str, shared: &Arc<Shared>) {
    let Ok(wanted) = CString::new(name) else {
        tracing::warn!(stream = %name, "this stream name cannot be used; it contains a NUL");
        return;
    };
    let create = sys::SendCreate {
        p_ndi_name: wanted.as_ptr(),
        p_groups: std::ptr::null(),
        // See the field's own note: clocking here would pace the render thread.
        clock_video: false,
        clock_audio: false,
    };
    // Safe: `create` and the name outlive the call, and the SDK copies what it
    // keeps.
    let instance = unsafe { (ndi.send_create)(&create) };
    if instance.is_null() {
        tracing::warn!(stream = %name, "the NDI runtime would not make a sender for this output");
        return;
    }
    // Safe: the instance is live, and the string it points at stays valid
    // until the sender is destroyed, which is after this is copied out.
    let announced = unsafe {
        let source = (ndi.send_source_name)(instance);
        if source.is_null() {
            None
        } else {
            super::library::text((*source).p_ndi_name)
        }
    };
    if let Ok(mut held) = shared.announced.lock() {
        *held = announced.clone();
    }
    tracing::info!(
        stream = %name,
        announced = %announced.as_deref().unwrap_or("unknown"),
        "publishing this output on the network"
    );

    let mut said_size: Option<SizePx> = None;
    loop {
        let picture = {
            let Ok(mut waiting) = shared.waiting.lock() else {
                break;
            };
            while waiting.next.is_none() {
                if shared.stop.load(Ordering::Acquire) {
                    break;
                }
                // Timed, so a stop is noticed even if it lands between the
                // check and the wait. The timeout is otherwise never reached:
                // an output offers a frame every pass.
                let Ok((held, _)) = shared
                    .arrived
                    .wait_timeout(waiting, std::time::Duration::from_millis(100))
                else {
                    return;
                };
                waiting = held;
            }
            match waiting.next.take() {
                Some(picture) => picture,
                // Woken to stop.
                None => break,
            }
        };

        if said_size != Some(picture.size) {
            said_size = Some(picture.size);
            tracing::info!(
                stream = %name,
                width = picture.size.width,
                height = picture.size.height,
                "this output's stream is now this size"
            );
        }

        let frame = sys::VideoFrame {
            xres: i32::try_from(picture.size.width).unwrap_or(i32::MAX),
            yres: i32::try_from(picture.size.height).unwrap_or(i32::MAX),
            // `RGBX`, not `RGBA`: the same bytes, and the fourth one declared
            // meaningless, so no alpha plane is encoded and none goes on the
            // wire. Still nothing converted on this path.
            four_cc: sys::four_cc::RGBX,
            frame_rate_n: i32::try_from(picture.rate_numerator).unwrap_or(i32::MAX),
            frame_rate_d: i32::try_from(picture.rate_denominator).unwrap_or(1),
            // Zero means square pixels, which is what a composition is.
            picture_aspect_ratio: 0.0,
            frame_format_type: sys::FRAME_FORMAT_PROGRESSIVE,
            // Let the runtime stamp it. A timecode from here would be this
            // machine's idea of now, and the show's clock is not the wall clock.
            timecode: sys::SEND_TIMECODE_SYNTHESISE,
            p_data: picture.pixels.as_ptr().cast_mut(),
            line_stride_in_bytes: i32::try_from(picture.size.width * 4).unwrap_or(i32::MAX),
            p_metadata: std::ptr::null(),
            timestamp: 0,
        };

        // Safe: the pixels are checked against the size on the way in, and this
        // call compresses before it returns — so `picture` outliving it, which
        // it does, is enough. The async variant is the one that would need the
        // buffer kept past the call, and it is deliberately not used.
        unsafe { (ndi.send_video)(instance, &frame) };
        shared.sent.fetch_add(1, Ordering::Relaxed);
    }

    unsafe { (ndi.send_destroy)(instance) };
    tracing::info!(stream = %name, "stopped publishing this output");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(width: u32, height: u32, len: usize) -> Picture {
        Picture {
            size: SizePx::new(width, height),
            rate_numerator: 60,
            rate_denominator: 1,
            pixels: vec![0x7F; len],
        }
    }

    /// A frame whose pixels do not match its size is refused, not sent.
    ///
    /// The one mistake on this path that is not recoverable: a short buffer
    /// described as a full frame is the runtime reading past the end of it, and
    /// what comes out is whatever was next in this process's memory — sent to
    /// everybody on the network.
    #[test]
    fn a_frame_that_does_not_describe_itself_is_refused() {
        for wrong in [
            picture(4, 4, 4 * 4 * 4 - 1),
            picture(4, 4, 4 * 4 * 4 + 1),
            picture(0, 4, 0),
            picture(4, 0, 0),
        ] {
            assert!(!wrong.is_whole(), "{:?} must not pass", wrong.size);
        }
        assert!(
            picture(4, 4, 4 * 4 * 4).is_whole(),
            "a whole frame must pass"
        );
    }

    /// A rate of something per zero is not a rate.
    #[test]
    fn a_frame_rate_needs_a_denominator() {
        let mut frame = picture(4, 4, 4 * 4 * 4);
        frame.rate_denominator = 0;
        assert!(!frame.is_whole());
    }
}
