//! Getting pixels off the disk and onto the GPU.
//!
//! The one rule this module exists to keep: **nothing on the render path opens
//! a file.** Decoding a 4K still takes tens of milliseconds, which is several
//! frames; doing it between two presents would drop them. So decoding happens on
//! a worker thread and the render loop only ever does the cheap half — a copy
//! into a staging buffer that is already mapped (house rule §5).
//!
//! Nor does the render path *close* one. Letting go of a hardware decoder was
//! measured at a quarter of a second, which is the larger half of what a slot
//! change used to cost the thread that made it; [`Changeover`] has both figures
//! and does both halves somewhere else.
//!
//! What the renderer decodes is whatever the operator brought: the server plays
//! what it is given, so the list is FFmpeg's rather than ours. What
//! is short instead is the list of *shapes* a decoded picture comes in — RGBA,
//! three planes of Y, Cb and Cr, or NV12's two with the colour components
//! alternating — because that is what a texture has to be made of and it is
//! decided here rather than by the file. The third shape is there because a
//! card's decoder produces it and nothing on the way to the wall is allowed to
//! walk a frame on the processor to change it.

pub(crate) mod bands;
mod changeover;
mod generator;
mod geometry;
pub mod hap;
mod loader;
mod player;
mod residency;
pub mod snappy;
mod source;
mod still;
mod text;
mod timeline;
// The FFmpeg libraries are a build dependency of the decoder, and the machine
// this runs on is Debian. A Windows development machine builds everything else.
#[cfg(unix)]
mod hap_source;
#[cfg(unix)]
mod video;

pub use changeover::{Changeover, NotOpened, Opened, OpeningFor};
#[cfg(test)]
pub(crate) use generator::{generator_for_test, noise_for_test};
pub use generator::{GeneratorSource, Moment, MAX_GENERATED_SIDE};
#[cfg(unix)]
pub use hap_source::HapSource;
pub use loader::{Loaded, MediaLoader};
pub use player::{Player, PlayerReport, FRAMES_AHEAD};
pub use residency::{MediaReport, Residency, MAX_RESIDENT};
pub use source::{Frame, FrameSource, StillSource, SyntheticSource};
pub use still::{
    decode_still, Chroma, ChromaFormat, ColourMatrix, DecodedImage, MediaError, SampleDepth,
};
#[cfg(unix)]
pub use video::VideoSource;

pub use timeline::{
    demanded_fps, frame_for, pass_for, playhead_at, Pass, Playhead, ShowClock, SourceLength,
    Triggered, FPS_WORTH_WARNING_ABOUT,
};
