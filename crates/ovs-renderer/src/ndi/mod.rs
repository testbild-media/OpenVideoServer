//! Receiving video over the network, where somebody else is sending it.
//!
//! NDI is how the rest of the building sends pictures around: a graphics
//! machine, a playback box, a camera with an adapter on it. An operator who
//! can put one of those on a layer does not have to run a cable for it.
//!
//! # Loaded at runtime, never linked
//!
//! `libndi.so.6` is opened by name when something first asks for it, and every
//! function is looked up by name. Nothing links against it and the package
//! carries none of it — the same arrangement [`crate::nvml`] has with NVIDIA's
//! management library, and for a stronger reason: the NDI SDK's licence does
//! not let this project redistribute it, and its terms would sit badly beside
//! the licence this project ships under. So the operator installs the runtime,
//! exactly as they already install a Vulkan driver.
//!
//! A machine without it loses NDI and nothing else. That is said out loud when
//! a show asks for a source this build cannot reach, because a dark layer with
//! no explanation anywhere is the fault this codebase is written against.
//!
//! # A live source is not a file
//!
//! Everything else the renderer plays has a length and a frame index, and the
//! scheduler above it decides which frame belongs to this moment. A network
//! sender decides that itself: frames arrive when they arrive, there is no
//! seeking and there is no end.
//!
//! Two consequences, both measured on the reference machine on 2026-09-25
//! rather than assumed:
//!
//! - **A sender may declare fifty frames a second and deliver one.** A still
//!   test pattern did exactly that — one frame per second, 999 ms between them
//!   — because an unchanged picture is not worth sending twice. A screen
//!   capture on the same network, through the same receiver, delivered 52.6 fps
//!   with 17 ms between frames. So the receiver holds its last frame and the
//!   layer keeps showing it. A receiver that drew only what arrived this pass
//!   would show that test pattern as one frame and fifty-nine blacks a second.
//! - **The newest frame wins.** When two arrive between one present and the
//!   next, the older is dropped rather than queued. A queue would trade
//!   latency for frames nobody will ever see, and latency is the thing a
//!   person standing in front of a wall actually notices.

#[allow(unsafe_code)]
mod discovery;
#[allow(unsafe_code)]
mod library;
mod output;
#[allow(unsafe_code)]
mod receiver;
#[allow(unsafe_code)]
mod sender;
#[allow(unsafe_code)]
mod sys;

pub use discovery::Discovery;
pub use library::{Finder, Ndi, NdiError};
pub use output::NdiOutput;
pub use receiver::{NdiReceiver, ReceiverReport};
pub use sender::{Picture, Sender, SenderReport};
