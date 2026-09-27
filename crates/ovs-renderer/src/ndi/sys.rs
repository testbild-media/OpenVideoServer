//! NDI's own types, exactly as its headers declare them.
//!
//! Nothing in here decides anything. It is the translation of
//! `Processing.NDI.structs.h`, `Processing.NDI.Find.h` and
//! `Processing.NDI.Recv.h` into Rust, kept in one file so that the only place
//! this program can disagree with the SDK about the shape of a struct is a
//! place somebody can read against the header.
//!
//! Transcribed from SDK 6.3.2.0. A field in the wrong order here is not a
//! compile error and not a crash — it is a picture with its colours in the
//! wrong places, or a stride read as a resolution. So the two unions the
//! headers use are written out as the single field they actually are, with the
//! reason beside them, rather than guessed at.

use std::ffi::{c_char, c_float, c_int, c_uint, c_void};

/// What `NDIlib_recv_capture_v3` found.
///
/// Audio is 2 and metadata is 3, and neither can arrive here: capture is called
/// with a null pointer for both, which the header says means that kind is not
/// captured. So they are written down rather than named — a constant nothing
/// reads is a claim that something does.
///
/// 100 and 101 are status and source changes. They carry nothing to free and
/// nothing this renderer acts on, so they fall to the catch-all with the rest.
pub mod frame_type {
    /// The timeout passed with nothing to show for it. Not an error: a sender
    /// with an unchanged picture says nothing for a second at a time.
    pub const NONE: i32 = 0;
    /// A video frame, which must be freed.
    pub const VIDEO: i32 = 1;
    /// The connection is gone.
    pub const ERROR: i32 = 4;
}

/// The pixel layouts a receiver can be handed.
///
/// Four characters packed little-endian, which is what the header's
/// `NDI_LIB_FOURCC` macro builds.
pub mod four_cc {
    /// Builds the constant the header's macro would.
    const fn code(text: &[u8; 4]) -> i32 {
        (text[0] as i32)
            | ((text[1] as i32) << 8)
            | ((text[2] as i32) << 16)
            | ((text[3] as i32) << 24)
    }

    /// 4:2:2 eight-bit, interleaved U Y0 V Y1. What an opaque sender gives.
    pub const UYVY: i32 = code(b"UYVY");
    /// Eight-bit RGBA. What a sender with an alpha channel gives.
    pub const RGBA: i32 = code(b"RGBA");
    /// Eight-bit RGB with the fourth byte unused.
    pub const RGBX: i32 = code(b"RGBX");
}

/// Ask for UYVY when the sender is opaque and RGBA when it has alpha.
///
/// The one choice in this file that is a decision rather than a transcription,
/// so it is worth its lines. Both halves land in the renderer's own picture
/// type without a colour conversion anywhere: UYVY becomes three planes the
/// shader already knows how to read, and RGBA is already what a still is.
///
/// The obvious-looking `fastest` is wrong here. It hands back UYVA for a sender
/// with alpha — UYVY with an alpha plane bolted on — and the renderer's picture
/// type cannot express that, so it would have to be converted on the processor
/// at sixty frames a second to say the same thing RGBA already says.
///
/// And `BGRX_BGRA`, which the first measurement used, makes the receiver
/// convert every frame out of the 4:2:2 the wire carries into eight-bit RGB:
/// 8.3 MB a frame where 3.1 would do, for a conversion the card does for free
/// in a pass it is already running.
pub const COLOR_FORMAT_UYVY_RGBA: c_int = 3;

/// Full resolution. The other settings drop resolution to save bandwidth,
/// which is not a trade a video server gets to make on the operator's behalf.
pub const BANDWIDTH_HIGHEST: c_int = 100;

/// An opaque finder.
pub type FindInstance = *mut c_void;

/// An opaque receiver.
pub type RecvInstance = *mut c_void;

/// An opaque sender.
pub type SendInstance = *mut c_void;

/// Let the runtime work out the timecode.
///
/// `INT64_MAX`, which the header defines as "synthesize": the runtime stamps the
/// frame from the ones before it and the time since. A timecode from here would
/// be this machine's idea of now, and a show's clock is not the wall clock.
pub const SEND_TIMECODE_SYNTHESISE: i64 = i64::MAX;

/// A whole frame rather than one field of two.
///
/// What this renderer composes. The interlaced values exist in the header and
/// nothing here produces them.
pub const FRAME_FORMAT_PROGRESSIVE: c_int = 1;

/// One source, as the network announces it.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Source {
    /// The full name, shaped `MACHINE (Source Name)`.
    pub p_ndi_name: *const c_char,
    /// Where it is. The header has a union of this and a deprecated
    /// `p_ip_address`, both `const char*` — one pointer either way.
    pub p_url_address: *const c_char,
}

impl Default for Source {
    fn default() -> Self {
        Self {
            p_ndi_name: std::ptr::null(),
            p_url_address: std::ptr::null(),
        }
    }
}

/// How to look for sources.
#[repr(C)]
pub struct FindCreate {
    /// Whether senders on this machine count. They do: a renderer and a sender
    /// on one box is an ordinary way to test, and hiding it would make the
    /// first thing anybody tries look broken.
    pub show_local_sources: bool,
    /// Restrict to named groups. Null means the default group.
    pub p_groups: *const c_char,
    /// Extra addresses to search beyond what discovery finds.
    pub p_extra_ips: *const c_char,
}

/// How to build a receiver.
#[repr(C)]
pub struct RecvCreate {
    /// Which source to connect to.
    pub source_to_connect_to: Source,
    /// Which pixel layout to be handed.
    pub color_format: c_int,
    /// How much of the source to take.
    pub bandwidth: c_int,
    /// Whether fields may arrive as fields.
    ///
    /// False here, so the SDK deinterlaces and everything above this sees
    /// progressive frames. The header calls the cost "almost no performance
    /// impact", and the alternative is teaching the compositor what half a
    /// picture is.
    pub allow_video_fields: bool,
    /// What this receiver calls itself on the network.
    pub p_ndi_recv_name: *const c_char,
}

/// How to build a sender.
#[repr(C)]
pub struct SendCreate {
    /// What the network is told this source is called.
    ///
    /// The part in brackets: the runtime puts the machine in front, so `Wall
    /// Left` is announced as `OPENVIDEOSERVER (Wall Left)`.
    pub p_ndi_name: *const c_char,
    /// Restrict to named groups. Null means the default group.
    pub p_groups: *const c_char,
    /// Whether submitting a frame should block until its time is due.
    ///
    /// **False, always.** The header calls it rate limiting and it is: measured
    /// on the reference machine, a clocked sender returns exactly sixty times a
    /// second and an unclocked one five hundred and thirty-eight. An output
    /// driven from the render thread must never hold it, because one stream
    /// configured at thirty would take every wall on the machine down to thirty
    /// with it (house rule §17: a failing output degrades on its own).
    pub clock_video: bool,
    /// Whether audio submission should do the same. This product sends none.
    pub clock_audio: bool,
}

/// One video frame the receiver is lending out.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VideoFrame {
    /// Width in pixels.
    pub xres: c_int,
    /// Height in pixels.
    pub yres: c_int,
    /// Which layout `p_data` is in.
    pub four_cc: c_int,
    /// Frame rate numerator, as the sender declares it.
    pub frame_rate_n: c_int,
    /// Frame rate denominator.
    pub frame_rate_d: c_int,
    /// Display aspect ratio, or zero for square pixels.
    pub picture_aspect_ratio: c_float,
    /// Progressive, interleaved, or one field of two.
    pub frame_format_type: c_int,
    /// The sender's timecode.
    pub timecode: i64,
    /// The pixels. Owned by the receiver until freed.
    pub p_data: *mut u8,
    /// Bytes from one row to the next.
    ///
    /// The header unions this with `data_size_in_bytes`, which is what it means
    /// for a compressed layout. Nothing asked for here is compressed, so it is
    /// always the stride — and it is **not** width times bytes per pixel: a
    /// sender may pad rows, so every read has to go through this.
    pub line_stride_in_bytes: c_int,
    /// Per-frame metadata, as XML.
    pub p_metadata: *const c_char,
    /// When the sender produced it, in 100 ns units.
    pub timestamp: i64,
}

impl Default for VideoFrame {
    fn default() -> Self {
        Self {
            xres: 0,
            yres: 0,
            four_cc: 0,
            frame_rate_n: 0,
            frame_rate_d: 0,
            picture_aspect_ratio: 0.0,
            frame_format_type: 0,
            timecode: 0,
            p_data: std::ptr::null_mut(),
            line_stride_in_bytes: 0,
            p_metadata: std::ptr::null(),
            timestamp: 0,
        }
    }
}

/// `NDIlib_initialize`.
pub type InitialiseFn = unsafe extern "C" fn() -> bool;
/// `NDIlib_is_supported_CPU`.
pub type SupportedCpuFn = unsafe extern "C" fn() -> bool;
/// `NDIlib_version`.
pub type VersionFn = unsafe extern "C" fn() -> *const c_char;
/// `NDIlib_find_create_v2`.
pub type FindCreateFn = unsafe extern "C" fn(*const FindCreate) -> FindInstance;
/// `NDIlib_find_destroy`.
pub type FindDestroyFn = unsafe extern "C" fn(FindInstance);
/// `NDIlib_find_wait_for_sources`.
pub type FindWaitFn = unsafe extern "C" fn(FindInstance, c_uint) -> bool;
/// `NDIlib_find_get_current_sources`.
pub type FindSourcesFn = unsafe extern "C" fn(FindInstance, *mut c_uint) -> *const Source;
/// `NDIlib_recv_create_v3`.
pub type RecvCreateFn = unsafe extern "C" fn(*const RecvCreate) -> RecvInstance;
/// `NDIlib_recv_destroy`.
pub type RecvDestroyFn = unsafe extern "C" fn(RecvInstance);
/// `NDIlib_recv_capture_v3`.
///
/// The audio and metadata pointers are typed as nothing at all because they are
/// always null: the header says a null pointer means that kind is not captured,
/// and this product has no audio path and nothing to do with per-frame metadata.
/// Typing them faithfully would mean carrying two structs and two free
/// functions to describe frames that never arrive.
pub type RecvCaptureFn =
    unsafe extern "C" fn(RecvInstance, *mut VideoFrame, *mut c_void, *mut c_void, c_uint) -> c_int;
/// `NDIlib_recv_free_video_v2`.
pub type RecvFreeVideoFn = unsafe extern "C" fn(RecvInstance, *const VideoFrame);
/// `NDIlib_send_create`.
pub type SendCreateFn = unsafe extern "C" fn(*const SendCreate) -> SendInstance;
/// `NDIlib_send_destroy`.
pub type SendDestroyFn = unsafe extern "C" fn(SendInstance);
/// `NDIlib_send_get_source_name`.
///
/// What the network actually calls this sender, machine prefix included. Asked
/// rather than assembled: the runtime decides how this machine's name appears,
/// and a guess about hostnames would be a source wrongly filtered out of an
/// operator's list.
///
/// The pointer is valid until the sender is destroyed.
pub type SendSourceNameFn = unsafe extern "C" fn(SendInstance) -> *const Source;
/// `NDIlib_send_send_video_v2`.
///
/// The synchronous one. There is an async variant that returns sooner and
/// requires the pixels to stay valid and untouched until the *following* call,
/// which is a rule easy to break by accident and impossible to notice when
/// broken — it would show as tearing on somebody else's screen. This one
/// compresses before it returns, which the 1.9 ms average measured for a 1080p
/// frame confirms, and it buys back nothing this renderer needs: the copy it
/// would save is one this design does not make.
pub type SendVideoFn = unsafe extern "C" fn(SendInstance, *const VideoFrame);
