//! The OpenVideoServer renderer.
//!
//! Realtime infrastructure. It owns the GPU and the physical outputs, and it is
//! a separate process from the core precisely so that the core stopping,
//! crashing or being upgraded cannot interrupt show output.
//!
//! # What holds here
//!
//! - **The loop runs whether or not anything else does.** No core, no show
//!   and no client are all ordinary states; the renderer presents black rather
//!   than stopping, because stopping would make "nothing loaded" and "renderer
//!   broken" look identical from the auditorium.
//! - **Reading show state never blocks.** [`show_state::SharedShowState`] is
//!   wait-free to read, so a large show arriving over the control channel
//!   cannot stall a frame.
//! - **A failing output degrades.** One lost wall faults on its own and the
//!   others carry on (house rule §17).
//! - **Dropped frames are measured, not counted.** A drop is a vblank interval
//!   that passed with nothing presented into it, which is what an operator sees.
//!
//! # No async runtime
//!
//! Deliberate. The control channel runs on its own thread with blocking I/O, so
//! nothing on the render path waits on a scheduler.

// `deny` rather than `forbid`: three modules lift it, and all for the same
// reason — they are the places where this program stops being Rust. `vulkan`,
// because every Vulkan entry point is unsafe by construction and forbidding it
// there would only mean pushing the GPU into a separate language;
// `nvml`, because NVIDIA's management library is a C library opened by name
// at runtime; and `ndi`, for the same reason with somebody else's library.
// Nowhere else in the workspace lifts it.
#![deny(unsafe_code)]
#![warn(missing_docs, clippy::all)]

pub mod backend;
pub mod buses;
// A Unix socket, and the System screen is a Linux arrangement throughout: the
// process that draws it hosts CEF, and the panel it lands on is a DRM connector.
#[cfg(unix)]
pub mod console;
pub mod control;
pub mod effects;
pub mod frame_clock;
// Device nodes under /dev/input, which is a Linux arrangement; the panel this
// reads for is a DRM connector, so there is nowhere else for it to be.
#[cfg(unix)]
pub mod input;
pub mod media;
pub mod pattern;
// The FFmpeg libraries are a build dependency of the encoder, as of the
// decoder, and the machine this runs on is Debian.
#[cfg(unix)]
pub mod stream;

// NVML is a C library opened by name at runtime, so calling it is unsafe by
// construction — and the alternative is running `nvidia-smi` as a subprocess
// once a second, which is a process per sample for two numbers.
#[allow(unsafe_code)]
pub mod nvml;

// And the network sources, which are somebody else's C library opened the same
// way. The module says at more length why it is opened rather than linked: its
// licence does not let this project carry it.
pub mod ndi;

pub mod mask;
pub mod outputs;
pub mod pieces;
pub mod playback;
pub mod render_loop;
pub mod scene;
pub mod show_state;
pub mod vulkan;

pub use backend::{BackendError, FrameRequest, HeadlessBackend, OutputBackend};
pub use buses::BusGains;
pub use control::{serve, Disconnected};
pub use effects::{EffectPass, PassKind};
pub use frame_clock::FrameClock;
pub use outputs::{plan as plan_outputs, Assignment, Missing, Plan};
pub use pieces::{MappedPiece, OutputMapping};
pub use playback::{Playback, PlaybackReport};
pub use render_loop::{RenderLoop, TickReport};
pub use scene::{Quad, Scene, SourceTurn};
pub use show_state::{SharedShowState, ShowState};
pub use vulkan::{DeviceKind, DeviceSummary, VulkanContext, VulkanError, VulkanOffscreenBackend};
