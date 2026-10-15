//! Output backends.
//!
//! One trait, chosen by configuration rather than by conditional compilation,
//! because the production path cannot be exercised on the development machine
//! and the test path must not need a GPU at all (R12):
//!
//! - `headless` — offscreen, no GPU. Used by every automated test, so the whole
//!   frame loop is provable in CI.
//! - `window` — a swapchain in a window, for development.
//! - `display` — `VK_KHR_display` / DRM-KMS direct mode, production on Debian.
//!
//! Only `headless` exists so far. The other two arrive with the Vulkan work, and
//! this file will not claim otherwise until they do.

use std::collections::BTreeMap;

use ovs_types::config::DisplayMode;
use ovs_types::geometry::SizePx;
use ovs_types::id::OutputId;

use crate::media::DecodedImage;
use crate::scene::{Scene, TextureKey};

/// Something went wrong driving an output.
#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    /// The output went away: unplugged, or the device was lost.
    ///
    /// A fault for that output. The renderer keeps driving the others rather
    /// than stopping the show for one wall (house rule §17).
    #[error("output {name} is no longer available: {reason}")]
    OutputLost {
        /// Operator-visible name of the output.
        name: String,
        /// What happened.
        reason: String,
    },

    /// Presenting failed for this frame.
    #[error("could not present a frame on {name}: {reason}")]
    PresentFailed {
        /// Operator-visible name of the output.
        name: String,
        /// What happened.
        reason: String,
    },

    /// An image could not be made available to this output.
    ///
    /// Transient, like a failed present: the output keeps showing what it has,
    /// and the layer whose media this was draws its placeholder. Out of GPU
    /// memory is the case this exists for, and it is not a reason to darken a
    /// wall.
    #[error("could not give {name} the pixels for a media item: {reason}")]
    UploadFailed {
        /// Operator-visible name of the output.
        name: String,
        /// What happened.
        reason: String,
    },
}

impl BackendError {
    /// Returns true when this output cannot recover without operator action.
    ///
    /// A failed present is transient and the next frame may well work. A lost
    /// output is not, and its pieces stay dark until it is dealt with.
    #[must_use]
    pub const fn is_permanent(&self) -> bool {
        matches!(self, Self::OutputLost { .. })
    }
}

/// One frame the renderer is asking an output to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameRequest {
    /// Monotonic frame number since the renderer started.
    pub index: u64,
}

/// What an output needs in order to sample a composition.
///
/// Spelled out rather than hidden behind a downcast. The two halves of a frame
/// have to be the same kind of backend — an image rendered on one device cannot
/// be sampled by another, and a headless compositor has no image at all — and an
/// enum says which combinations exist where a type test would only discover it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompositionSource {
    /// Nothing to sample. A compositor that draws nowhere.
    #[default]
    Nothing,
    /// An image on the device every output in this renderer shares.
    Vulkan {
        /// The view an output's descriptor set points at.
        view: ash::vk::ImageView,
        /// The sampler to read it through.
        sampler: ash::vk::Sampler,
        /// The three buses, in `Route` order: A, B, M.
        ///
        /// A piece may take its rectangle out of one of these rather than out
        /// of the composition (owner, 2026-09-23), which is how a wall becomes
        /// a monitor of the look coming up rather than the one going out. Empty
        /// on a compositor that has not built its buses yet, in which case such
        /// a piece draws flat — the same thing it does before the first frame.
        buses: [Option<ash::vk::ImageView>; 3],
    },
}

/// A physical or virtual output the renderer drives.
pub trait OutputBackend: Send {
    /// Stable logical identifier of this output.
    fn id(&self) -> OutputId;

    /// Operator-visible name.
    fn name(&self) -> &str;

    /// The mode this output is running at.
    fn mode(&self) -> DisplayMode;

    /// Returns what this output is attached to, when the backend can tell.
    ///
    /// `None` means "this backend cannot say", which is different from "nothing
    /// is attached": a headless output has no hardware to describe and an
    /// offscreen canvas is not plugged into anything. Only a backend that talks
    /// to a real connector can answer, and until one exists every show is
    /// matched in order.
    fn identity(&self) -> Option<ovs_types::config::OutputIdentity> {
        None
    }

    /// Returns how many bits a channel carries on this output's cable.
    ///
    /// The default is `Unknown`, which is the answer for every backend that is
    /// not a panel. Only a backend holding a real swapchain can say, and it
    /// says what the driver gave it rather than what the show asked for.
    fn output_depth(&self) -> ovs_types::depth::OutputDepth {
        ovs_types::depth::OutputDepth::Unknown
    }

    /// Returns how many bits a channel carries in the surfaces this output
    /// composes through.
    ///
    /// The compositor's own answer, forwarded, because an output shows what its
    /// compositor drew. Separate from [`Self::output_depth`] on purpose and by
    /// type: this one is the stage that quantises and that one is the cable.
    /// Neither of them is the depth the *material* was decoded at, which is a
    /// third number on each playing layer — see `ovs_types::depth`.
    fn composition_depth(&self) -> ovs_types::depth::CompositionDepth {
        ovs_types::depth::CompositionDepth::Eight
    }

    /// Draws `scene` and presents it, returning when it reaches the display.
    ///
    /// The scene is borrowed rather than owned: one scene is composed per pass
    /// and handed to every output, so each output in a pass shows the same
    /// state rather than a slightly different one.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] when the frame could not be shown.
    fn present(&mut self, frame: FrameRequest, scene: &Scene) -> Result<(), BackendError>;

    /// Returns true once, when this output has only just begun showing frames.
    ///
    /// A backend that has to bring hardware up answers true on the first pass
    /// after it succeeds, and the loop restarts that output's accounting: the
    /// second or so a panel spends setting a mode is the output starting, not a
    /// second of dropped frames. Everything that presents immediately — headless
    /// and offscreen — never answers true, and their accounting runs from the
    /// first pass as it always did.
    fn just_started(&mut self) -> bool {
        false
    }

    /// Copies a small picture of the last presented frame into `into`.
    ///
    /// Tightly packed RGBA at `size`. Returns false when this output cannot be
    /// read back, which a headless one cannot and a real display may not: a
    /// preview is a convenience, and an output that cannot offer one still
    /// drives a wall.
    fn snapshot(&self, size: ovs_types::geometry::SizePx, into: &mut Vec<u8>) -> bool;

    /// Points this output's pieces at a composition.
    ///
    /// Called once at startup and again whenever the composition is rebuilt,
    /// which happens when a show with a different composition size is
    /// loaded. An output that cannot sample the source given draws its pieces
    /// flat rather than failing: a mismatch here is a wiring mistake, and
    /// faulting a wall over it would turn a visible one into a dark one.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] when the binding itself fails.
    fn bind_composition(&mut self, source: CompositionSource) -> Result<(), BackendError>;

    /// Puts a picture on this output's own canvas.
    ///
    /// Almost everything an output draws comes from the composition, which is
    /// bound rather than uploaded. The exception is the service test pattern in
    /// its per-output form: that one is a different picture on every wall — its
    /// own size, its own rate, its own name — so it cannot come from a shared
    /// composition and has to live here.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] when the picture cannot be uploaded. A backend
    /// that holds no textures says so rather than pretending.
    fn upload_texture(
        &mut self,
        key: crate::scene::TextureKey,
        image: &DecodedImage,
    ) -> Result<(), BackendError> {
        let _ = (key, image);
        Err(BackendError::UploadFailed {
            name: self.name().to_owned(),
            reason: "this output holds no textures of its own".to_owned(),
        })
    }

    /// Frees one of those pictures. Returns whether there was one.
    fn drop_texture(&mut self, key: crate::scene::TextureKey) -> bool {
        let _ = key;
        false
    }

    /// Says whether anybody is going to look at what this output draws.
    ///
    /// As on a compositor: the copy is only worth making while somebody is
    /// watching.
    fn set_readback(&mut self, wanted: bool) {
        let _ = wanted;
    }

    /// The name this output is published under on the network, if it is.
    ///
    /// `None` for every output that drives a socket, which is nearly all of
    /// them. The whole announced name, machine prefix and all, because the one
    /// thing it is for is recognising this machine's own streams in a list of
    /// what is on the network — and a partial name would match somebody else's.
    fn published_as(&self) -> Option<String> {
        None
    }
}

/// A generator the card can draw for itself.
///
/// What crosses from the media side to the backend: a description rather than
/// a picture. The point of it is that nothing is drawn on the processor and
/// nothing is copied onto the card - on PC05 a 1920x1080 noise field cost the
/// render thread twenty-five milliseconds a pass and eight megabytes of copy,
/// and a 4K clip decoded on the card cost six microseconds.
///
/// Short on purpose. A kind belongs here only when the card can draw exactly
/// what the processor would, and a test proves it does; everything else keeps
/// taking the picture it always took.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Generated {
    /// Value noise, the field already worked out for this tick.
    Noise {
        /// How many pixels one cell of the lattice covers.
        scale: u32,
        /// Whether the three channels get independent fields.
        coloured: bool,
        /// How far from mid grey the field reaches, 0 to 1.
        amount: f32,
        /// Which four fields this tick uses.
        field: u64,
    },
    /// A flat fill, which is also what a strobe is on either half of its cycle.
    Solid {
        /// Straight, not premultiplied, as every colour here is.
        colour: [f32; 4],
    },
}

/// The canvas every output samples.
///
/// A frame is two passes. The compositor draws the layers once,
/// into an image at the composition's size; each output then draws its pieces,
/// which sample rectangles of that image. One composition for the whole
/// renderer, because that is what it means for three walls to be showing the
/// same show rather than three shows that look alike.
///
/// Media belongs to the compositor and not to the outputs, because the
/// composition pass is the only pass that samples media. An output samples the
/// composition and nothing else.
pub trait Compositor: Send {
    /// Draws the layers into the composition.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] when the composition could not be drawn. The
    /// outputs then present whatever the last successful pass left, which is a
    /// held frame rather than a black one.
    fn compose(&mut self, frame: FrameRequest, scene: &Scene) -> Result<(), BackendError>;

    /// Has the card draw a generator into the texture at `key`.
    ///
    /// Returns whether it did. `false` is not a failure: it is a backend that
    /// does not draw generators, or a kind this one cannot draw, and the
    /// caller then draws the picture on the processor and uploads it as it
    /// always did. That fallback is the reason this answers rather than
    /// refuses - a wall that is slower is a wall, and a wall that is black is
    /// not.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] when the card was asked and could not. The
    /// caller falls back to the processor for that pass.
    fn draw_generated(
        &mut self,
        key: TextureKey,
        size: SizePx,
        what: Generated,
    ) -> Result<bool, BackendError> {
        let _ = (key, size, what);
        Ok(false)
    }

    /// Tells the effect passes what time it is, in seconds.
    ///
    /// Free-running and shared by every pass in the frame: an effect that
    /// moves is moving against a clock, and two layers carrying the same
    /// effect have to move together. Nothing about the show reaches it — not
    /// the trigger, not the transport, not the master speed — for the same
    /// reason the test pattern's clock is its own: a glitch is a look, not a
    /// cue.
    ///
    /// Does nothing by default, because a backend with no effect passes has
    /// nothing to tell.
    fn set_effect_time(&mut self, seconds: f32) {
        let _ = seconds;
    }

    /// Makes `image` available to every quad that names `key`.
    ///
    /// Replaces whatever was held under that key.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError::UploadFailed`] when the pixels cannot be taken.
    /// The caller carries on without that image and the layer draws flat.
    fn upload_texture(&mut self, key: TextureKey, image: &DecodedImage)
        -> Result<(), BackendError>;

    /// Hands over a picture to be uploaded with the rest of the frame's.
    ///
    /// # Why this is not [`upload_texture`](Self::upload_texture)
    ///
    /// Copying a decoded picture into the card's memory is a write across the
    /// bus, and one core does not fill a PCIe 4.0 link. Measured on the test
    /// machine with `ovs-upload-cost`, seven 1080p layers: 8223 us one at a
    /// time against 6118 us four at a time, in a frame that is 16666 us long.
    ///
    /// Copies can only run together if they are known together, so the render
    /// path queues them and [`flush_textures`](Self::flush_textures) does the
    /// work. `compose` flushes first, so forgetting to call it is not a way to
    /// draw a stale frame.
    ///
    /// An `Arc` rather than a reference because the picture outlives the call
    /// and is read from several threads. It is what a decoded frame already is.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] when the picture cannot be accepted at all. The
    /// copy itself is reported by `flush_textures`.
    fn queue_texture(
        &mut self,
        key: TextureKey,
        image: std::sync::Arc<DecodedImage>,
    ) -> Result<(), BackendError> {
        self.upload_texture(key, &image)
    }

    /// Copies every queued picture into the card's memory.
    ///
    /// Called by `compose` before it draws, and directly by anything that wants
    /// to time the two separately. Doing it twice is not an error: the second
    /// call has nothing to do.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] for the first picture that could not be
    /// copied. The others are still copied: a half-staged frame is a torn
    /// picture, and a thread that has started is cheaper to let finish.
    fn flush_textures(&mut self) -> Result<(), BackendError> {
        Ok(())
    }

    /// Frees one texture. Returns true when there was one.
    fn drop_texture(&mut self, key: TextureKey) -> bool;

    /// Copies a small picture of the composition into `into`.
    ///
    /// Returns false when this compositor has no picture to give, which is the
    /// ordinary answer for one that draws nowhere. A preview is a tap for
    /// somebody watching and must never be able to hold up a frame
    ///, so this is called from outside the render path and a
    /// refusal costs nothing.
    fn snapshot(&self, _size: ovs_types::geometry::SizePx, _into: &mut Vec<u8>) -> bool {
        false
    }

    /// Returns how many media textures are held.
    fn texture_count(&self) -> usize;

    /// Returns how many quads the last composed frame contained.
    fn last_quad_count(&self) -> usize;

    /// Returns what an output needs in order to sample this composition.
    fn source(&self) -> CompositionSource;

    /// Returns the size the composition is drawn at.
    fn size(&self) -> ovs_types::geometry::SizePx;

    /// Returns how many bits a channel carries in this compositor's surfaces.
    ///
    /// Read off what was created, not off what was asked for. The default is
    /// eight because that is what a compositor with no surfaces at all is
    /// honestly doing, and because eight is this renderer's efficient path
    /// (owner, 2026-09-23): ten-bit *material* does not widen the buses.
    fn composition_depth(&self) -> ovs_types::depth::CompositionDepth {
        ovs_types::depth::CompositionDepth::Eight
    }

    /// Says whether anybody is going to look at what this draws.
    ///
    /// Copying a finished frame out for a preview costs a megabyte per
    /// megapixel across the bus, every frame. That is worth paying while
    /// somebody is watching and pure waste when nobody is, and an operator who
    /// switched previews off has said which.
    fn set_readback(&mut self, wanted: bool) {
        let _ = wanted;
    }

    /// Returns what the device reports about itself.
    ///
    /// `None` for a compositor with no device, and for one whose driver cannot
    /// say. A guess would be a number an operator would act on.
    fn gpu_telemetry(&self) -> Option<ovs_types::telemetry::GpuTelemetry> {
        None
    }
}

/// Makes a compositor of a given size.
///
/// The composition is the show's size, and a show can be loaded at any
/// time, so the renderer has to be able to make a new one mid-show. The loop
/// holds one of these rather than a compositor it cannot replace.
pub type CompositorFactory =
    Box<dyn Fn(ovs_types::geometry::SizePx) -> Result<Box<dyn Compositor>, BackendError> + Send>;

/// An output that renders nowhere.
///
/// Not a stub for something missing: it is how the frame loop, the timing and
/// the telemetry are tested on a machine with no GPU, which is every CI runner.
/// It can also be told to fail, so the renderer's behaviour when an output dies
/// is tested rather than assumed.
#[derive(Debug)]
pub struct HeadlessBackend {
    id: OutputId,
    name: String,
    mode: DisplayMode,
    presented: Vec<u64>,
    /// How many quads the last presented frame contained.
    ///
    /// The only thing a headless output can honestly report about what it was
    /// asked to draw, and enough to test that the scene reaches the backend.
    last_quad_count: usize,
    /// What this output was last told to sample.
    bound: CompositionSource,
    failure: Option<Failure>,
    /// What this stand-in claims about its own precision.
    ///
    /// Two fields rather than one, because a test needs to be able to set them
    /// to different values: the two stages are independent and a test that could
    /// not build a disagreeing pair could not check that they stay apart.
    composition_depth: ovs_types::depth::CompositionDepth,
    output_depth: ovs_types::depth::OutputDepth,
    /// Pictures uploaded to this output's own canvas, by size and first pixel.
    ///
    /// Only the service test pattern arrives this way, and this is how a test
    /// checks that the right one reached the right wall.
    textures: std::collections::BTreeMap<
        crate::scene::TextureKey,
        (ovs_types::geometry::SizePx, [u8; 4]),
    >,
    /// What this output claims to be attached to.
    ///
    /// A headless output is attached to nothing, so this is `None` unless a
    /// test says otherwise — which is how output identity matching is tested
    /// without hardware that can be unplugged.
    identity: Option<ovs_types::config::OutputIdentity>,
}

#[derive(Debug, Clone, Copy)]
enum Failure {
    /// Fail every present from the given frame index onwards.
    TransientFrom(u64),
    /// Fail transiently for an inclusive range of frame indices, then recover.
    TransientBetween(u64, u64),
    /// Report the output lost from the given frame index onwards.
    LostFrom(u64),
}

impl HeadlessBackend {
    /// Creates a headless output that reports a hardware identity.
    ///
    /// For testing what happens when a wall is unplugged, which is otherwise
    /// only reproducible by unplugging one.
    #[must_use]
    pub fn identified(
        name: impl Into<String>,
        mode: DisplayMode,
        identity: ovs_types::config::OutputIdentity,
    ) -> Self {
        Self {
            identity: Some(identity),
            ..Self::new(name, mode)
        }
    }

    /// Creates a headless output.
    #[must_use]
    pub fn new(name: impl Into<String>, mode: DisplayMode) -> Self {
        Self {
            id: OutputId::new(),
            name: name.into(),
            mode,
            presented: Vec::new(),
            last_quad_count: 0,
            bound: CompositionSource::Nothing,
            failure: None,
            textures: std::collections::BTreeMap::new(),
            identity: None,
            // What a stand-in honestly is: the default composition depth, and
            // no claim at all about a cable it does not have.
            composition_depth: ovs_types::depth::CompositionDepth::Eight,
            output_depth: ovs_types::depth::OutputDepth::Unknown,
        }
    }

    /// Creates a headless output that claims a given pair of depths.
    ///
    /// The two are set independently on purpose. The configuration that has to
    /// be reported correctly is ten bits through the compositor into an
    /// eight-bit socket, and a helper that derived one from the other could not
    /// build it.
    #[must_use]
    pub fn at_depths(
        name: impl Into<String>,
        mode: DisplayMode,
        composition_depth: ovs_types::depth::CompositionDepth,
        output_depth: ovs_types::depth::OutputDepth,
    ) -> Self {
        Self {
            composition_depth,
            output_depth,
            ..Self::new(name, mode)
        }
    }

    /// Returns the size and first pixel of what this output holds under a key.
    #[must_use]
    pub fn texture(
        &self,
        key: crate::scene::TextureKey,
    ) -> Option<(ovs_types::geometry::SizePx, [u8; 4])> {
        self.textures.get(&key).copied()
    }

    /// Returns how many quads the last presented frame contained.
    #[must_use]
    pub const fn last_quad_count(&self) -> usize {
        self.last_quad_count
    }

    /// Makes every present from `index` onwards fail transiently.
    #[must_use]
    pub fn failing_from(mut self, index: u64) -> Self {
        self.failure = Some(Failure::TransientFrom(index));
        self
    }

    /// Makes presents fail transiently for frames `first` to `last` inclusive,
    /// then recover.
    ///
    /// This is the interesting case: an output that stutters and comes back,
    /// which must show up as dropped frames rather than as a fault.
    #[must_use]
    pub fn failing_between(mut self, first: u64, last: u64) -> Self {
        self.failure = Some(Failure::TransientBetween(first, last));
        self
    }

    /// Makes this output report itself lost from `index` onwards.
    #[must_use]
    pub fn lost_from(mut self, index: u64) -> Self {
        self.failure = Some(Failure::LostFrom(index));
        self
    }

    /// Returns the frame indices that were presented, in order.
    #[must_use]
    pub fn presented(&self) -> &[u64] {
        &self.presented
    }

    /// Returns what this output was last told to sample.
    #[must_use]
    pub const fn bound(&self) -> CompositionSource {
        self.bound
    }
}

impl OutputBackend for HeadlessBackend {
    fn composition_depth(&self) -> ovs_types::depth::CompositionDepth {
        self.composition_depth
    }

    fn output_depth(&self) -> ovs_types::depth::OutputDepth {
        self.output_depth
    }

    fn id(&self) -> OutputId {
        self.id
    }

    fn upload_texture(
        &mut self,
        key: crate::scene::TextureKey,
        image: &DecodedImage,
    ) -> Result<(), BackendError> {
        let first = image
            .pixels()
            .get(..4)
            .map_or([0; 4], |pixel| [pixel[0], pixel[1], pixel[2], pixel[3]]);
        self.textures.insert(key, (image.size(), first));
        Ok(())
    }

    fn drop_texture(&mut self, key: crate::scene::TextureKey) -> bool {
        self.textures.remove(&key).is_some()
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn identity(&self) -> Option<ovs_types::config::OutputIdentity> {
        self.identity.clone()
    }

    fn mode(&self) -> DisplayMode {
        self.mode
    }

    fn present(&mut self, frame: FrameRequest, scene: &Scene) -> Result<(), BackendError> {
        match self.failure {
            Some(Failure::LostFrom(from)) if frame.index >= from => {
                return Err(BackendError::OutputLost {
                    name: self.name.clone(),
                    reason: "simulated".to_owned(),
                })
            }
            Some(Failure::TransientFrom(from)) if frame.index >= from => {
                return Err(BackendError::PresentFailed {
                    name: self.name.clone(),
                    reason: "simulated".to_owned(),
                })
            }
            Some(Failure::TransientBetween(first, last))
                if frame.index >= first && frame.index <= last =>
            {
                return Err(BackendError::PresentFailed {
                    name: self.name.clone(),
                    reason: "simulated".to_owned(),
                })
            }
            _ => {}
        }

        self.presented.push(frame.index);
        self.last_quad_count = scene.len();
        Ok(())
    }

    fn bind_composition(&mut self, source: CompositionSource) -> Result<(), BackendError> {
        self.bound = source;
        Ok(())
    }

    fn snapshot(&self, _size: ovs_types::geometry::SizePx, _into: &mut Vec<u8>) -> bool {
        // An output that renders nowhere has nothing to show.
        false
    }
}

/// A compositor that draws nowhere.
///
/// The same idea as [`HeadlessBackend`] and for the same reason: the media path,
/// the residency rules and the loop's ordering are all testable on a machine
/// with no GPU, which is every CI runner. It records what it was given, which is
/// what those tests need to see.
#[derive(Debug)]
pub struct HeadlessCompositor {
    composed: Vec<u64>,
    last_quad_count: usize,
    textures: BTreeMap<TextureKey, (ovs_types::geometry::SizePx, [u8; 4])>,
    uploads_fail: bool,
    size: ovs_types::geometry::SizePx,
}

impl Default for HeadlessCompositor {
    fn default() -> Self {
        Self::new()
    }
}

impl HeadlessCompositor {
    /// Creates a compositor that accepts everything, at a nominal size.
    #[must_use]
    pub fn new() -> Self {
        Self::sized(ovs_types::geometry::SizePx::new(1920, 1080))
    }

    /// Creates one that reports the given size.
    #[must_use]
    pub fn sized(size: ovs_types::geometry::SizePx) -> Self {
        Self {
            composed: Vec::new(),
            last_quad_count: 0,
            textures: BTreeMap::new(),
            uploads_fail: false,
            size,
        }
    }

    /// Makes every upload fail, as a full GPU would.
    #[must_use]
    pub const fn refusing_uploads(mut self) -> Self {
        self.uploads_fail = true;
        self
    }

    /// Returns the frame indices that were composed, in order.
    #[must_use]
    pub fn composed(&self) -> &[u64] {
        &self.composed
    }

    /// Returns the size and first pixel of what is held under a key.
    #[must_use]
    pub fn texture(
        &self,
        key: impl Into<TextureKey>,
    ) -> Option<(ovs_types::geometry::SizePx, [u8; 4])> {
        self.textures.get(&key.into()).copied()
    }
}

impl Compositor for HeadlessCompositor {
    fn compose(&mut self, frame: FrameRequest, scene: &Scene) -> Result<(), BackendError> {
        self.composed.push(frame.index);
        self.last_quad_count = scene.len();
        Ok(())
    }

    fn upload_texture(
        &mut self,
        key: TextureKey,
        image: &DecodedImage,
    ) -> Result<(), BackendError> {
        if self.uploads_fail {
            return Err(BackendError::UploadFailed {
                name: "the headless compositor".to_owned(),
                reason: "simulated".to_owned(),
            });
        }

        // Through the picture's own accessor, which converts a planar frame
        // the same way the shader does: a test asking what colour a layer is
        // must get the same answer whichever compositor it is talking to.
        self.textures.insert(key, (image.size(), image.pixel(0, 0)));
        Ok(())
    }

    fn drop_texture(&mut self, key: TextureKey) -> bool {
        self.textures.remove(&key).is_some()
    }

    fn texture_count(&self) -> usize {
        self.textures.len()
    }

    fn last_quad_count(&self) -> usize {
        self.last_quad_count
    }

    fn source(&self) -> CompositionSource {
        CompositionSource::Nothing
    }

    fn size(&self) -> ovs_types::geometry::SizePx {
        self.size
    }
}

#[cfg(test)]
mod tests {
    use ovs_types::geometry::{FrameRate, SizePx};

    use super::*;

    fn mode() -> DisplayMode {
        DisplayMode::new(SizePx::new(1920, 1080), FrameRate::FPS_60)
    }

    fn empty() -> Scene {
        Scene::new()
    }

    #[test]
    fn a_headless_output_records_what_it_was_asked_to_show() {
        let mut output = HeadlessBackend::new("Wall", mode());
        for index in 0..5 {
            output
                .present(FrameRequest { index }, &empty())
                .expect("presents");
        }
        assert_eq!(output.presented(), &[0, 1, 2, 3, 4]);
        assert_eq!(output.name(), "Wall");
        assert_eq!(output.mode(), mode());
    }

    #[test]
    fn a_transient_failure_is_not_permanent() {
        let mut output = HeadlessBackend::new("Wall", mode()).failing_from(2);
        assert!(output.present(FrameRequest { index: 0 }, &empty()).is_ok());
        assert!(output.present(FrameRequest { index: 1 }, &empty()).is_ok());

        let error = output
            .present(FrameRequest { index: 2 }, &empty())
            .expect_err("fails");
        assert!(!error.is_permanent(), "the next frame may well work");
        assert_eq!(output.presented(), &[0, 1]);
    }

    #[test]
    fn a_lost_output_is_permanent() {
        let mut output = HeadlessBackend::new("Wall", mode()).lost_from(1);
        assert!(output.present(FrameRequest { index: 0 }, &empty()).is_ok());

        let error = output
            .present(FrameRequest { index: 1 }, &empty())
            .expect_err("lost");
        assert!(error.is_permanent());
        assert!(error.to_string().contains("no longer available"));
    }

    #[test]
    fn outputs_have_distinct_identities() {
        let first = HeadlessBackend::new("Left", mode());
        let second = HeadlessBackend::new("Right", mode());
        assert_ne!(first.id(), second.id());
    }
}
