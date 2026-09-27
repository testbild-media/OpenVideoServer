//! One network sender, received on a thread of its own.
//!
//! The thread does the whole of the expensive half: it waits for a frame, turns
//! it into the renderer's own picture type, and puts it where the render loop
//! can pick it up. What the render loop does is compare a counter and, when it
//! has moved, one upload — which is the same bargain every other source in this
//! renderer makes (house rule §5: nothing on the render path opens a file, and
//! nothing on it waits for a network either).
//!
//! # No queue
//!
//! Exactly one frame is kept, and a new one overwrites it. When two arrive
//! between one present and the next, the older is never drawn.
//!
//! That is the right trade for live video and the wrong one for a file. A clip
//! has a timeline and every frame of it belongs to a moment, so the player
//! keeps a ring and a dropped frame is a visible stutter. A camera has no
//! timeline: the only frame worth showing is the newest one, and a queue would
//! buy completeness with latency — which is what somebody standing in front of
//! the wall actually sees.
//!
//! # What arrives, and in what
//!
//! Measured on the reference machine on 2026-09-25 against NDI Test Pattern and
//! a screen capture, both 1920x1080:
//!
//! - **UYVY** from an opaque sender: 4:2:2 eight-bit, U Y0 V Y1, 3840 bytes a
//!   row. De-interleaved here into the three planes the shader reads, so no
//!   colour conversion happens on the processor at all.
//! - **RGBA** from a sender with an alpha channel, copied straight through.
//! - **BT.709, limited range.** Not assumed: the SDK will hand the same picture
//!   over as RGB, and its own conversion is the definitive statement of what
//!   its YUV means. Scored against it, BT.709 limited came out at a mean error
//!   of 1.15 of 255 where BT.601 limited gave 3.10 and either full-range
//!   reading gave over 9. The 1.15 that is left is chroma subsampling at hard
//!   colour edges, which is the difference between interpolating the pair and
//!   taking it.

use std::ffi::{CStr, CString};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use ovs_types::geometry::SizePx;

use super::library::Ndi;
use super::sys;
use crate::media::{ChromaFormat, ColourMatrix, DecodedImage, SampleDepth};

/// How long a capture waits before coming back empty.
///
/// Short enough that asking the thread to stop is answered promptly, and the
/// cost of it being short is nothing: a capture returns the moment a frame is
/// there, so this only decides how often an idle receiver goes round the loop.
/// A sender showing an unchanged picture is idle for a second at a time.
const CAPTURE_TIMEOUT_MS: u32 = 100;

/// What this receiver calls itself on the network.
///
/// Senders and other receivers can see it, so it says what it is rather than
/// leaving the SDK to name it after the executable and a number.
const RECEIVER_NAME: &CStr = c"OpenVideoServer";

/// What one receiver has been doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReceiverReport {
    /// Frames received since it connected.
    pub arrived: u64,
    /// Frames whose layout this build could not read.
    pub unusable: u64,
    /// Whether the sender is currently there.
    pub connected: bool,
    /// The rate the sender declares, as numerator and denominator.
    ///
    /// Worth reporting next to `arrived` rather than instead of it, because the
    /// two disagreeing is normal and reads as a fault: a still sender declares
    /// fifty and delivers one, and an operator looking at a slow number wants
    /// to see that the sender said so.
    pub declared_rate: Option<(i32, i32)>,
}

/// What the thread and the render loop share.
struct Shared {
    /// The newest frame. Replaced, never queued.
    latest: Mutex<Option<Arc<DecodedImage>>>,
    /// How many frames have arrived. The render loop compares this.
    arrived: AtomicU64,
    /// Frames in a layout this build cannot read.
    unusable: AtomicU64,
    /// Whether the sender is there.
    connected: AtomicBool,
    /// The declared rate, as the last frame stated it. Zero until one arrives.
    rate_n: AtomicI32,
    /// The declared rate's denominator.
    rate_d: AtomicI32,
    /// Set to ask the thread to finish.
    stop: AtomicBool,
}

/// A live network source, feeding one layer.
pub struct NdiReceiver {
    /// The source this is connected to, as the show names it.
    name: String,
    shared: Arc<Shared>,
    /// Taken in [`Drop`], so the thread is joined rather than abandoned.
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for NdiReceiver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NdiReceiver")
            .field("name", &self.name)
            .field("arrived", &self.shared.arrived.load(Ordering::Relaxed))
            .finish()
    }
}

impl NdiReceiver {
    /// Connects to a source by name and starts receiving.
    ///
    /// Returns immediately. A name alone is enough — the SDK resolves it
    /// through discovery itself, in about a tenth of a second on the reference
    /// network — which is why a show stores the name and never an address: a
    /// sender that restarts comes back on a different port under the same name.
    ///
    /// A source that is not on the network is not an error. It is a sender that
    /// has not been switched on yet, which happens every time a show is opened
    /// before the rest of the building is ready, so the receiver waits for it
    /// and the layer shows its placeholder until it appears.
    #[must_use]
    pub fn connect(ndi: &'static Ndi, name: &str) -> Self {
        let shared = Arc::new(Shared {
            latest: Mutex::new(None),
            arrived: AtomicU64::new(0),
            unusable: AtomicU64::new(0),
            connected: AtomicBool::new(false),
            rate_n: AtomicI32::new(0),
            rate_d: AtomicI32::new(0),
            stop: AtomicBool::new(false),
        });

        let thread = {
            let shared = Arc::clone(&shared);
            let name = name.to_owned();
            std::thread::Builder::new()
                // Named after the source, because a machine with eight of these
                // and one of them misbehaving is the situation where a thread
                // list has to say which.
                .name(format!("ndi-recv {name}"))
                .spawn(move || receive(ndi, &name, &shared))
                .ok()
        };

        if thread.is_none() {
            tracing::warn!(source = %name, "could not start a thread to receive this NDI source");
        }

        Self {
            name: name.to_owned(),
            shared,
            thread,
        }
    }

    /// Which source this is connected to.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// How many frames have arrived.
    ///
    /// The render loop keeps the number it last uploaded and uploads again when
    /// this has moved. That way a failed upload is retried on the next pass
    /// rather than skipped, and an unchanging sender costs one comparison a
    /// frame and no copying at all.
    #[must_use]
    pub fn arrived(&self) -> u64 {
        self.shared.arrived.load(Ordering::Acquire)
    }

    /// The newest frame, if any has arrived.
    ///
    /// Cloned rather than taken, so it stays available: a picture that has been
    /// received is the right thing to show until a newer one arrives, however
    /// long that is.
    #[must_use]
    pub fn frame(&self) -> Option<Arc<DecodedImage>> {
        self.shared.latest.lock().ok()?.clone()
    }

    /// What this receiver has been doing.
    #[must_use]
    pub fn report(&self) -> ReceiverReport {
        let numerator = self.shared.rate_n.load(Ordering::Relaxed);
        let denominator = self.shared.rate_d.load(Ordering::Relaxed);
        ReceiverReport {
            arrived: self.shared.arrived.load(Ordering::Relaxed),
            unusable: self.shared.unusable.load(Ordering::Relaxed),
            connected: self.shared.connected.load(Ordering::Relaxed),
            declared_rate: (denominator != 0).then_some((numerator, denominator)),
        }
    }
}

impl Drop for NdiReceiver {
    /// Stops the thread and waits for it.
    ///
    /// Waits, rather than letting it run on: the thread holds an NDI receiver
    /// and destroys it on its way out, and a show being closed and reopened
    /// would otherwise leave a sender feeding a receiver nobody reads. An
    /// operator switching shows during a changeover does that repeatedly.
    ///
    /// The wait is bounded by the capture timeout, which is why that is short.
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            if thread.join().is_err() {
                tracing::warn!(source = %self.name, "the receiver thread for this NDI source panicked");
            }
        }
    }
}

/// The receiving thread.
fn receive(ndi: &'static Ndi, name: &str, shared: &Arc<Shared>) {
    let Ok(wanted) = CString::new(name) else {
        // A name with a NUL in it cannot be passed to a C library, and cannot
        // have come from a sender either. Refused here rather than truncated,
        // because a truncated name would quietly connect to the wrong source.
        tracing::warn!(source = %name, "this NDI source name cannot be used; it contains a NUL");
        return;
    };

    let create = sys::RecvCreate {
        source_to_connect_to: sys::Source {
            p_ndi_name: wanted.as_ptr(),
            p_url_address: std::ptr::null(),
        },
        color_format: sys::COLOR_FORMAT_UYVY_RGBA,
        bandwidth: sys::BANDWIDTH_HIGHEST,
        allow_video_fields: false,
        p_ndi_recv_name: RECEIVER_NAME.as_ptr(),
    };

    // Safe: `create` and the two strings it points at outlive the call, and the
    // SDK copies what it keeps.
    let instance = unsafe { (ndi.recv_create)(&create) };
    if instance.is_null() {
        tracing::warn!(source = %name, "the NDI runtime would not make a receiver for this source");
        return;
    }

    let mut frame = sys::VideoFrame::default();
    let mut said_connected = false;
    while !shared.stop.load(Ordering::Acquire) {
        // Safe: the receiver is live, `frame` is ours, and the two null
        // pointers are the documented way to say "no audio, no metadata".
        let kind = unsafe {
            (ndi.recv_capture)(
                instance,
                &mut frame,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                CAPTURE_TIMEOUT_MS,
            )
        };

        match kind {
            sys::frame_type::VIDEO => {
                shared.connected.store(true, Ordering::Relaxed);
                shared.rate_n.store(frame.frame_rate_n, Ordering::Relaxed);
                shared.rate_d.store(frame.frame_rate_d, Ordering::Relaxed);
                if !said_connected {
                    said_connected = true;
                    tracing::info!(
                        source = %name,
                        width = frame.xres,
                        height = frame.yres,
                        "receiving this NDI source"
                    );
                }

                // Safe: this frame is one the receiver just lent and has
                // not been freed yet, so it describes itself.
                match unsafe { picture(&frame) } {
                    Some(image) => {
                        if let Ok(mut latest) = shared.latest.lock() {
                            *latest = Some(Arc::new(image));
                        }
                        // Released after the frame is in place, so a render
                        // pass that sees the new count sees the new frame.
                        shared.arrived.fetch_add(1, Ordering::Release);
                    }
                    None => {
                        let before = shared.unusable.fetch_add(1, Ordering::Relaxed);
                        if before == 0 {
                            tracing::warn!(
                                source = %name,
                                four_cc = frame.four_cc,
                                stride = frame.line_stride_in_bytes,
                                width = frame.xres,
                                height = frame.yres,
                                "this NDI source sends a layout this build cannot read; \
                                 the layer keeps whatever it last showed"
                            );
                        }
                    }
                }

                // Freed whatever happened to it. The receiver lent it and wants
                // it back; keeping one costs the sender a buffer and, a few
                // frames later, the connection.
                unsafe { (ndi.recv_free_video)(instance, &frame) };
            }
            sys::frame_type::ERROR => {
                // The sender went away. Not fatal and not worth tearing down:
                // the SDK reconnects on its own when it comes back, which is
                // what an operator power-cycling a graphics machine needs.
                if shared.connected.swap(false, Ordering::Relaxed) {
                    said_connected = false;
                    tracing::info!(source = %name, "this NDI source went away; still listening for it");
                }
            }
            // Ordinary, and most of what this loop does. A sender with an
            // unchanged picture says nothing for a second at a time, and an
            // idle receiver goes round here ten times a second finding nothing.
            sys::frame_type::NONE => {}
            // Status and source changes, which carry nothing to free and
            // nothing this renderer acts on.
            _ => {}
        }
    }

    unsafe { (ndi.recv_destroy)(instance) };
}

/// Turns one lent frame into the renderer's own picture, or `None`.
///
/// # Safety
///
/// `frame.p_data` is null, or describes itself truthfully: at least
/// `line_stride_in_bytes` bytes for each of the first `yres - 1` rows and a full
/// row after them. That is the contract the SDK offers for a frame it lent, and
/// it holds until the frame is freed.
unsafe fn picture(frame: &sys::VideoFrame) -> Option<DecodedImage> {
    let width = u32::try_from(frame.xres).ok()?;
    let height = u32::try_from(frame.yres).ok()?;
    let stride = usize::try_from(frame.line_stride_in_bytes).ok()?;
    if width == 0 || height == 0 || frame.p_data.is_null() {
        return None;
    }
    let size = SizePx::new(width, height);

    let row_bytes = match frame.four_cc {
        sys::four_cc::UYVY => (width as usize).checked_mul(2)?,
        sys::four_cc::RGBA | sys::four_cc::RGBX => (width as usize).checked_mul(4)?,
        _ => return None,
    };
    if stride < row_bytes {
        return None;
    }

    // The last row need not be padded, so the slice stops at the end of it
    // rather than a stride later. A frame is the one place in this program
    // where reading a few bytes too far would be reading somebody else's
    // memory, and the sender chose these numbers.
    let len = stride
        .checked_mul((height as usize).checked_sub(1)?)?
        .checked_add(row_bytes)?;
    let bytes = unsafe { std::slice::from_raw_parts(frame.p_data, len) };

    match frame.four_cc {
        sys::four_cc::UYVY => planes_from_uyvy(bytes, size, stride),
        sys::four_cc::RGBA => rgba_from(bytes, size, stride, false),
        sys::four_cc::RGBX => rgba_from(bytes, size, stride, true),
        _ => None,
    }
}

/// Splits interleaved 4:2:2 into the three planes the shader reads.
///
/// UYVY stores each pair of horizontal pixels as U, Y0, V, Y1 — one colour
/// sample shared by two luma samples. Which is already 4:2:2, so this moves
/// bytes and converts nothing: 4.1 MB read and 3.1 MB written for a 1080p
/// frame, on the receiver's own thread, against the 8.3 MB of RGBA that asking
/// the SDK to convert would have produced.
fn planes_from_uyvy(bytes: &[u8], size: SizePx, stride: usize) -> Option<DecodedImage> {
    let width = size.width as usize;
    let height = size.height as usize;
    // UYVY is defined on pairs, so an odd width has half a pair at the end of
    // every row. No sender produces one; refused rather than guessed at.
    if !width.is_multiple_of(2) {
        return None;
    }
    // Checked here as well as by the caller: this reads at an offset it was
    // handed, so the number deciding where it reads is its own business.
    if stride < width * 2 || height == 0 {
        return None;
    }

    let mut luma = Vec::with_capacity(width * height);
    let mut cb = Vec::with_capacity(width / 2 * height);
    let mut cr = Vec::with_capacity(width / 2 * height);
    for row in 0..height {
        let from = row * stride;
        let line = bytes.get(from..from + width * 2)?;
        // Fixed-size groups rather than `chunks_exact`, so the four reads below
        // are known to be in bounds and not checked one at a time. This walks
        // four megabytes a frame at sixty frames a second.
        for [u, y0, v, y1] in line.as_chunks::<4>().0 {
            cb.push(*u);
            luma.push(*y0);
            cr.push(*v);
            luma.push(*y1);
        }
    }

    DecodedImage::planar(
        luma,
        cb,
        cr,
        size,
        ChromaFormat {
            subsample_x: 2,
            subsample_y: 1,
            full_range: false,
            // BT.709 whatever the size. There used to be a standard-definition
            // branch here, written from the convention that a 576-line raster
            // carries BT.601 — and measured afterwards, it was wrong: the SDK
            // encodes BT.709 at 64, 480, 576, 720 and 1080 lines alike, at an
            // error of 6 of 255 against 80 for BT.601, identical at all five.
            // There is no height at which NDI switches.
            //
            // A third-party sender pushing genuinely standard-definition
            // material could still encode BT.601, and nothing in an NDI frame
            // says which. NDI's own implementation is the reference for what
            // NDI means, and guessing from the height breaks a small picture
            // for certain to maybe help a large one.
            matrix: ColourMatrix::Bt709,
            depth: SampleDepth::Eight,
        },
    )
}

/// Copies RGBA out row by row, filling in alpha where the layout has none.
///
/// `opaque` is for RGBX, whose fourth byte the SDK leaves undefined. Copying it
/// through would be the worst kind of fault to look for: whatever happened to be
/// in that byte becomes the layer's opacity, so a source would be fully visible
/// on one machine and invisible on the next.
fn rgba_from(bytes: &[u8], size: SizePx, stride: usize, opaque: bool) -> Option<DecodedImage> {
    let width = size.width as usize;
    let height = size.height as usize;
    if stride < width * 4 || height == 0 {
        return None;
    }
    let mut pixels = Vec::with_capacity(width * height * 4);
    for row in 0..height {
        let from = row * stride;
        let line = bytes.get(from..from + width * 4)?;
        if opaque {
            for [red, green, blue, _unused] in line.as_chunks::<4>().0 {
                pixels.extend_from_slice(&[*red, *green, *blue, 0xff]);
            }
        } else {
            pixels.extend_from_slice(line);
        }
    }
    DecodedImage::new(pixels, size)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds one padded UYVY row after another.
    fn uyvy(
        width: usize,
        height: usize,
        stride: usize,
        fill: impl Fn(usize, usize) -> u8,
    ) -> Vec<u8> {
        let mut bytes = vec![0xAA; stride * height];
        for row in 0..height {
            for byte in 0..width * 2 {
                bytes[row * stride + byte] = fill(row, byte);
            }
        }
        // The last row is not padded, which is what the receiver assumes.
        bytes.truncate(stride * (height - 1) + width * 2);
        bytes
    }

    /// The de-interleave has to put every byte where the shader looks for it.
    ///
    /// Written out by hand for a 4x2 picture rather than checked with a
    /// property, because the thing that goes wrong here is an index, and an
    /// index that is wrong the same way in the test and the code agrees with
    /// itself all day.
    #[test]
    fn uyvy_splits_into_the_planes_the_shader_reads() {
        // U0 Y0 V0 Y1  U1 Y2 V1 Y3  per row, with the row number in the top bits
        // so a row mix-up shows up as a value and not as a shrug.
        let bytes = uyvy(4, 2, 12, |row, byte| (row as u8) << 4 | byte as u8);
        let image = planes_from_uyvy(&bytes, SizePx::new(4, 2), 12).expect("a 4x2 UYVY frame");

        assert_eq!(
            image.pixels(),
            &[0x01, 0x03, 0x05, 0x07, 0x11, 0x13, 0x15, 0x17],
            "luma is every second byte, row by row"
        );
        let chroma = image.chroma().expect("planar");
        assert_eq!(
            chroma.cb(),
            &[0x00, 0x04, 0x10, 0x14],
            "Cb is the first of each pair"
        );
        assert_eq!(
            chroma.cr(),
            &[0x02, 0x06, 0x12, 0x16],
            "Cr is the third of each pair"
        );
        assert_eq!(chroma.subsample_x(), 2);
        assert_eq!(chroma.subsample_y(), 1);
    }

    /// Padding between rows must not end up in the picture.
    ///
    /// A sender is free to pad, and reading stride as width times two is the
    /// mistake that produces a picture sheared diagonally across the wall.
    #[test]
    fn padding_between_rows_is_skipped() {
        let tight = uyvy(4, 3, 8, |row, byte| (row as u8) << 4 | byte as u8);
        let padded = uyvy(4, 3, 24, |row, byte| (row as u8) << 4 | byte as u8);
        let from_tight = planes_from_uyvy(&tight, SizePx::new(4, 3), 8).expect("tight");
        let from_padded = planes_from_uyvy(&padded, SizePx::new(4, 3), 24).expect("padded");
        assert_eq!(
            from_tight.pixels(),
            from_padded.pixels(),
            "the same picture, padded and not, must come out the same"
        );
    }

    /// Every size gets the coefficients that were measured at every size.
    ///
    /// This used to assert a standard-definition branch, written from the
    /// convention and not from a measurement. The measurement, once taken,
    /// disagreed at all five rasters — so the test that held the branch in
    /// place now holds it out.
    #[test]
    fn the_coefficients_do_not_depend_on_the_number_of_lines() {
        for height in [64, 480, 576, 720, 1080] {
            let bytes = uyvy(4, height, 8, |_, _| 128);
            let image = planes_from_uyvy(&bytes, SizePx::new(4, height as u32), 8)
                .expect("a frame of this height");
            assert_eq!(
                image.chroma().expect("planar").matrix(),
                ColourMatrix::Bt709,
                "{height} lines"
            );
        }
    }

    /// An RGBX frame's undefined fourth byte must not become opacity.
    #[test]
    fn rgbx_alpha_is_filled_in_rather_than_copied() {
        // Every fourth byte zero, which is what "undefined" looks like on the
        // day it makes a layer invisible.
        let bytes: Vec<u8> = (0..2 * 2 * 4)
            .map(|index| if index % 4 == 3 { 0 } else { 0x40 })
            .collect();
        let opaque = rgba_from(&bytes, SizePx::new(2, 2), 8, true).expect("RGBX");
        assert!(
            opaque
                .pixels()
                .iter()
                .skip(3)
                .step_by(4)
                .all(|alpha| *alpha == 0xff),
            "RGBX must come out fully opaque"
        );

        let kept = rgba_from(&bytes, SizePx::new(2, 2), 8, false).expect("RGBA");
        assert!(
            kept.pixels()
                .iter()
                .skip(3)
                .step_by(4)
                .all(|alpha| *alpha == 0),
            "RGBA's alpha is the sender's and must be left alone"
        );
    }

    /// A frame that does not describe itself is refused, not read anyway.
    #[test]
    fn a_frame_that_does_not_add_up_is_refused() {
        let bytes = uyvy(4, 2, 8, |_, _| 128);
        assert!(
            planes_from_uyvy(&bytes, SizePx::new(4, 2), 4).is_none(),
            "a stride shorter than a row cannot be read"
        );
        assert!(
            planes_from_uyvy(&bytes, SizePx::new(8, 2), 8).is_none(),
            "a width wider than the rows cannot be read"
        );
        assert!(
            planes_from_uyvy(&bytes, SizePx::new(3, 2), 8).is_none(),
            "an odd width is not a UYVY picture"
        );
    }

    /// A layout nothing asked for is refused rather than read as another.
    #[test]
    fn an_unknown_layout_is_refused() {
        let mut frame = sys::VideoFrame {
            xres: 4,
            yres: 2,
            four_cc: 0x3631_3250, // P216, which the renderer has no ten-bit path for here
            line_stride_in_bytes: 16,
            ..sys::VideoFrame::default()
        };
        let mut bytes = vec![128_u8; 64];
        frame.p_data = bytes.as_mut_ptr();
        // Safe: `bytes` is longer than the frame describes and outlives the call.
        assert!(
            unsafe { picture(&frame) }.is_none(),
            "P216 is not one of the two asked for"
        );
    }
}
