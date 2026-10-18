//! An offscreen render target, and the first backend that does real GPU work.
//!
//! This is the composition canvas in embryo. Everything downstream — pieces,
//! outputs, the direct-display path — draws into a target like this one, so
//! getting the image, the render pass and the submission right here is work that
//! the rest of the renderer inherits rather than repeats.
//!
//! It is also the only backend whose output can be checked. A frame presented to
//! a display can be believed; a frame rendered here can be read back and
//! compared, which is what makes "the GPU actually did the work" a test result
//! rather than an assumption.

#![allow(
    unsafe_code,
    reason = "every Vulkan entry point is unsafe; this module is the boundary"
)]

use std::time::Duration;

use std::collections::BTreeMap;
use std::sync::Arc;

use ash::vk;
use ovs_types::config::{BlendMode, DisplayMode, Route};
use ovs_types::geometry::SizePx;
use ovs_types::id::{EffectInstanceId, OutputId};

use crate::backend::{BackendError, CompositionSource, Compositor, FrameRequest, OutputBackend};
use crate::effects::{EffectPass, PassKind};
use crate::media::DecodedImage;
use crate::scene::{quad_basis, Quad, Scene, TextureKey, TextureRef};
use crate::vulkan::blend;
use crate::vulkan::generated::{GeneratedPipeline, GeneratedPush};
use crate::vulkan::texture::{find_memory_type, white_pixel, Texture, TextureLayout};
use crate::vulkan::{VulkanContext, VulkanError};

/// What every composition surface is made of.
///
/// One constant rather than seven repetitions of a format, because the number
/// of bits a layer bus carries is a property of this renderer and has to be
/// answerable in one place: [`Compositor::composition_depth`] reads it, and the
/// telemetry an operator sees is derived from it rather than restated beside it.
///
/// Eight bits a channel, which is what an LED processor wants and what a
/// readback can be compared against byte for byte. **Not** a function of what
/// the media is: ten-bit *material* is sampled at ten bits and does not widen
/// the buses, because widening them doubles the bus traffic of every layer of
/// every output for precision an eight-bit cable cannot carry (owner,
/// 2026-09-23 — eight bits stays the efficient path).
const COMPOSITION_FORMAT: vk::Format = vk::Format::R8G8B8A8_UNORM;

/// What the preview image is made of, which is its own decision.
const PREVIEW_FORMAT: vk::Format = vk::Format::R8G8B8A8_UNORM;

/// How many bits a channel of a composition surface carries.
///
/// Read off [`COMPOSITION_FORMAT`] rather than written down again, so the
/// telemetry cannot claim a depth the surfaces do not have. That is the whole
/// reason the constant exists.
const fn composition_depth_of(format: vk::Format) -> ovs_types::depth::CompositionDepth {
    use ovs_types::depth::CompositionDepth;
    // `match` on a `vk::Format` is not `const`-friendly — the type is a newtype
    // over an i32 with associated constants rather than an enum — so this
    // compares the raw values.
    if format.as_raw() == vk::Format::A2B10G10R10_UNORM_PACK32.as_raw()
        || format.as_raw() == vk::Format::A2R10G10B10_UNORM_PACK32.as_raw()
        || format.as_raw() == vk::Format::R16G16B16A16_UNORM.as_raw()
        || format.as_raw() == vk::Format::R16G16B16A16_SFLOAT.as_raw()
    {
        CompositionDepth::Ten
    } else {
        CompositionDepth::Eight
    }
}

/// How long to wait for the GPU before treating a frame as lost.
///
/// Generous: a frame that takes this long has already ruined the show, and the
/// point of the limit is only to turn a hung device into a reported fault rather
/// than a renderer that stops responding.
const FENCE_TIMEOUT: Duration = Duration::from_secs(2);

/// The colour the composition clears to before anything is drawn.
///
/// Black, and not configurable. Slot 0 is Black and a layer with nothing
/// selected shows black; a canvas that cleared to anything else would make that
/// impossible to see.
const CLEAR_COLOUR: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

/// How many quads one frame may contain.
///
/// The instance buffer is allocated once at this size, because allocating per
/// frame is exactly what the realtime rules forbid. A composition that exceeds
/// it draws the first `MAX_QUADS` and says so in the log rather than growing a
/// buffer mid-show; at layer counts a show actually uses this is unreachable,
/// and if it stops being so the number moves rather than the strategy.
const MAX_QUADS: usize = 1024;

/// Six vertices, two triangles, one quad. The geometry comes from the vertex
/// index; only the rectangle and colour are per-instance.
const VERTICES_PER_QUAD: u32 = 6;

/// Three vertices, one triangle, the whole canvas.
///
/// What an effect pass draws. One triangle larger than the canvas rather than
/// two that tile it: no pixel is rasterised twice along a diagonal seam, and
/// the part that hangs off is clipped for nothing.
const VERTICES_PER_FULL_SCREEN: u32 = 3;

/// How many scratch images an effect chain ping-pongs between.
///
/// Two, whatever the chain's length: a pass reads one and writes the other, so
/// a chain of twenty costs the same memory as a chain of one.
const SCRATCH_COUNT: usize = 2;

/// How many echoes one canvas remembers at once.
///
/// Each one keeps a picture at composition size — eight megabytes at 1080p,
/// thirty-three at 4K — and a Vulkan descriptor pool cannot grow, so there has
/// to be a ceiling. **Mine**: eight is one per layer on the owner's rig with
/// room over, and at 4K it is a quarter of a gigabyte, which a card driving a
/// show can carry. An echo beyond the eighth draws its layer untouched and says
/// so once.
const MAX_ECHOES: usize = 8;

/// How many threads may copy pictures into mapped memory at once.
///
/// One core writes about 4.8 GB/s across the bus and the link takes far more,
/// so this is cores rather than bandwidth. Four, because the machine also has
/// decoders on it and a renderer that takes every core to put pictures on the
/// card has only moved the problem.
const STAGING_THREADS: usize = 4;

/// How many distinct textures one canvas may hold at once.
///
/// One per active layer, plus the white default. The descriptor pool is sized
/// here because Vulkan pools cannot grow, so the limit is explicit rather than a
/// surprise on the night a show uses one texture too many; exceeding it draws
/// the quad flat and says so rather than failing the frame.
const MAX_TEXTURES: usize = 64;

/// One quad as the vertex shader wants it, tightly packed.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct InstanceData {
    /// The quad's centre in NDC, then where its source rectangle turns about.
    ///
    /// A `vec4` because a vertex attribute is read as one anyway and a
    /// two-float attribute followed by a four-float one is the kind of layout
    /// that works on one driver and not the next. The last two floats were
    /// padding until the input end of a piece learnt to turn, and they are the
    /// pivot in texture space — meaningless, and harmless, when the turn is the
    /// identity.
    placement: [f32; 4],
    /// Straight RGBA, alpha already scaled by opacity.
    colour: [f32; 4],
    /// Left, top, right, bottom, in texture space.
    source: [f32; 4],
    /// The basis the corners are placed through: x axis, then y axis.
    basis: [f32; 4],
    /// What this quad's blend mode fades towards.
    identity: f32,
    /// `1.0` when the texture is a composited picture rather than an image.
    ///
    /// A bus image is already premultiplied, so the shader must not scale it by
    /// its own alpha a second time — which would darken every soft edge in it.
    passthrough: f32,
    /// `0.0` for an RGBA texture, otherwise which Y'CbCr convention it is in.
    ///
    /// A decoded video frame is three planes of what its codec produced rather
    /// than one of RGBA, because the frame is copied twice a layer a frame and
    /// 4:2:0 is 2.7 times smaller. The conversion is in the shader,
    /// and this is how it is told which one to do — see
    /// [`TextureLayout::shader_code`].
    planar: f32,
    /// What a sample is multiplied by before it is converted.
    ///
    /// One for everything that is already in the domain the shader's colour
    /// constants are written in. Ten-bit planes are not: they go up as
    /// sixteen-bit and are sampled as a fraction of 65535, and this is what
    /// puts them back — see [`SampleDepth`](crate::media::SampleDepth), which
    /// is also where the surprising `1020` comes from.
    ///
    /// Its own attribute rather than folded into `planar`, because that number
    /// is a choice of convention and this is a multiplier. One float that
    /// meant two things would be the bitfield `planar` exists not to be.
    sample_scale: f32,
    /// The two-by-two matrix the source rectangle is turned by, row-major.
    ///
    /// The identity for everything but a piece whose input is taken at an
    /// angle. The correction for the texture's shape is already in it, because
    /// the shader does not know how many pixels across the picture it is
    /// sampling is — see [`SourceTurn`](crate::scene::SourceTurn).
    source_turn: [f32; 4],
}

/// Returns where a bus sits in the arrays that hold all three.
const fn bus_index(route: Route) -> usize {
    match route {
        Route::A => 0,
        Route::B => 1,
        Route::M => 2,
    }
}

/// How many buses there are: A, B and M.
pub(crate) const BUS_COUNT: usize = 3;

/// What a bus image is cleared to.
///
/// Transparent, unlike the composition. A bus is a picture *with coverage*: the
/// crossfade has to know where its layers are and where they are not, and a bus
/// cleared to opaque black would dissolve black over the other side instead of
/// letting it through (owner decision Q8).
const BUS_CLEAR: [f32; 4] = [0.0, 0.0, 0.0, 0.0];

/// An image this target draws into and then samples.
///
/// A bus is one; so is each half of an effect chain's scratch. They are the
/// same object with the same four handles, and holding them as one kind means
/// the code that makes one is written once.
struct SampledImage {
    image: vk::Image,
    memory: vk::DeviceMemory,
    view: vk::ImageView,
    framebuffer: vk::Framebuffer,
    /// The descriptor set whatever samples this image reads it through.
    set: vk::DescriptorSet,
}

/// The three buses and what is needed to draw into and out of them.
struct Buses {
    images: [SampledImage; BUS_COUNT],
    /// Clears transparent and ends ready to be sampled.
    render_pass: vk::RenderPass,
    /// Keeps what is already there, for a bus reopened after a chain has run.
    ///
    /// A layer with effects has to be composited after its chain, and a chain
    /// cannot run inside an open render pass. So the bus pass closes, the chain
    /// runs, and the bus is reopened with this — which loads what the earlier
    /// layers left rather than clearing it away.
    reopened: vk::RenderPass,
    /// One pipeline per blend mode, writing alpha as well as colour.
    pipelines: [vk::Pipeline; blend::BLEND_MODE_COUNT],
}

/// What a layer's effect chain is drawn through.
///
/// Two images at composition size and the pipelines that fill them. Made on the
/// first frame that has an effect to run rather than at startup: a show with no
/// effects never allocates any of this, which is what makes an empty chain cost
/// nothing rather than nearly nothing.
struct Scratch {
    images: [SampledImage; SCRATCH_COUNT],
    /// Clears transparent and ends ready to be sampled.
    render_pass: vk::RenderPass,
    /// Puts a layer's own picture into the first image, unblended.
    source: vk::Pipeline,
    /// Runs one pass, whichever effect it belongs to.
    ///
    /// One pipeline for all of them: which effect a pass is, and what it was
    /// set to, are push constants. Every fragment in a pass reads the same
    /// ones, so the branch in the shader is uniform and costs nothing
    /// measurable — and a pipeline per effect would be a pipeline per effect
    /// added for ever after.
    pass: vk::Pipeline,
    /// The layout the pass pipeline's push constants belong to.
    pass_layout: vk::PipelineLayout,
    shader: vk::ShaderModule,
    /// Nearest and clamped. A pass reads texel centres, so there is nothing for
    /// a filter to do but introduce error — and an effect at its published
    /// defaults has to give back the picture it was given, byte for byte.
    sampler: vk::Sampler,
}

/// What one echo drew last frame, which it reads this frame.
///
/// The one thing on a target that an effect keeps from one frame to the next.
/// Made like a scratch image and drawn through the same render pass and
/// pipeline, which is what lets an echo's result be copied into it without a
/// second kind of anything.
struct History {
    /// Whose memory it is.
    instance: EffectInstanceId,
    image: SampledImage,
    /// The composition it was last drawn in.
    ///
    /// An echo that was not drawn last frame — its layer faded out, its bus at
    /// nought, its slot not playing — starts again from nothing rather than
    /// from what it held when it stopped. Otherwise fading a layer back up
    /// would bring back a trail of a picture from minutes ago, which is a
    /// wrong picture that looks deliberate.
    drawn_in: u64,
    /// Whether it has to be cleared before this frame reads it.
    ///
    /// New, handed over from an echo that has gone, or not drawn last frame.
    /// The clear is recorded into the frame's own command buffer, before any
    /// chain, and costs a render pass with nothing drawn in it.
    clear: bool,
}

/// One effect pass as the shader reads it.
///
/// Laid out to match the `Effect` structure in `shaders/effect.wgsl`: four
/// floats, four floats, two floats, an integer, three more numbers, which WGSL
/// rounds up to sixty-four bytes. Every Vulkan implementation offers at least
/// 128 bytes of push constants, so this needs no buffer, no descriptor and no
/// allocation.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct EffectPush {
    values: [f32; 4],
    colour: [f32; 4],
    texel: [f32; 2],
    kind: u32,
    /// The clock an effect that moves reads, in seconds.
    ///
    /// In the four bytes WGSL was padding this structure out to anyway, so it
    /// costs nothing: the layout was already forty-eight bytes and still is.
    seconds: f32,
    /// How long one frame is on the wall, in seconds.
    frame: f32,
    /// Which frame this is, wrapping.
    tick: u32,
    /// What WGSL rounds the structure up to, written out so the two agree.
    padding: [u32; 2],
}

/// What every pass in one frame is told about time.
#[derive(Debug, Clone, Copy)]
struct EffectClock {
    seconds: f32,
    frame: f32,
    tick: u32,
}

impl EffectPush {
    /// The push constants for one pass on a canvas of the given texel size.
    const fn new(pass: &EffectPass, texel: [f32; 2], clock: EffectClock) -> Self {
        Self {
            values: pass.values,
            colour: pass.colour,
            texel,
            kind: pass.kind.code(),
            seconds: clock.seconds,
            frame: clock.frame,
            tick: clock.tick,
            padding: [0; 2],
        }
    }

    /// The pass that puts an echo's result into its memory.
    ///
    /// An echo at length nought, which hands back the picture it was given to
    /// the bit — and that is exactly a copy, through the pipeline and the
    /// render pass that are already there.
    const fn keeping(texel: [f32; 2]) -> Self {
        Self {
            values: [0.0; 4],
            colour: [0.0; 4],
            texel,
            kind: PassKind::Echo.code(),
            seconds: 0.0,
            frame: 0.0,
            tick: 0,
            padding: [0; 2],
        }
    }

    /// The bytes Vulkan copies into the command buffer.
    fn bytes(&self) -> &[u8] {
        // SAFETY: the structure is `repr(C)` and every field is a float or an
        // integer, so all of it is initialised and none of it has a niche.
        unsafe {
            std::slice::from_raw_parts(
                std::ptr::from_ref(self).cast::<u8>(),
                std::mem::size_of::<Self>(),
            )
        }
    }
}

/// A GPU image the renderer composites into.
pub struct OffscreenTarget {
    image: vk::Image,
    memory: vk::DeviceMemory,
    view: vk::ImageView,
    render_pass: vk::RenderPass,
    framebuffer: vk::Framebuffer,
    command_pool: vk::CommandPool,
    command_buffer: vk::CommandBuffer,
    fence: vk::Fence,
    /// Where the copies onto the card are recorded, and the queue they go to.
    ///
    /// A copy is not drawing. On this card it is a fixed-function engine that
    /// runs beside the one that draws, and the composition's own command buffer
    /// used to carry both — so a frame was copy *then* draw, in series, with
    /// the render thread waiting for the pair. Measured at seven 1080p layers:
    /// 1336 us of a 2209 us composition was the copy.
    ///
    /// A machine whose device has no transfer-only family gets the graphics
    /// queue here, which is legal, correct and exactly what it did before.
    transfer_pool: vk::CommandPool,
    transfer_buffer: vk::CommandBuffer,
    /// Signalled when the frame's copies are done, waited on before drawing.
    uploaded: vk::Semaphore,
    /// Which family owns an image while it is being written, and which reads
    /// it. Equal when the device has no separate transfer queue, and then no
    /// ownership passes at all.
    transfer_family: u32,
    graphics_family: u32,
    readback: vk::Buffer,
    readback_memory: vk::DeviceMemory,
    /// What every effect pass in the next frame is told the time is.
    ///
    /// Held here rather than passed through `compose`, because a frame request
    /// is built in ninety places and the time belongs to none of them. Set
    /// once a frame by the render loop; zero until it is, which is a legal
    /// moment like any other.
    effect_seconds: f32,
    /// How long one frame is on the wall, for the passes that fade.
    ///
    /// Set once a frame by the render loop from the rate the wall is really
    /// running at; until then, the rate this canvas was made for.
    effect_frame: f32,
    /// How many compositions this target has drawn, wrapping.
    ///
    /// What an echo's memory is dated by, so one that was not drawn last frame
    /// can be told apart from one that was.
    composed: u64,
    /// What each echo drew last, one picture per instance.
    ///
    /// A short list rather than a map: there are at most [`MAX_ECHOES`] of
    /// them and a frame looks each one up once. Never shrunk — a picture whose
    /// echo has gone is handed to the next echo that needs one — so a show
    /// that used echoes keeps at most that many pictures for as long as the
    /// canvas lives, and a show that never did keeps none.
    histories: Vec<History>,
    /// The echoes this frame draws, gathered before any picture is handed out,
    /// so that one about to be drawn is never the one taken for another.
    /// Kept between frames so a frame allocates nothing.
    wanted_histories: Vec<EffectInstanceId>,
    /// Whether "more echoes than this canvas remembers" has been logged.
    warned_about_echoes: bool,
    /// The finished frame, shrunk, on the card.
    ///
    /// Previews and streams want a small picture, and the cheapest way to get
    /// one is to let the card make it: a blit with linear filtering costs
    /// nothing measurable there, and what crosses the bus afterwards is the
    /// small picture instead of the whole frame. At 1080p that is the
    /// difference between eight megabytes a frame and one.
    ///
    /// It is also a better picture. The old path pulled the whole frame across
    /// and then took every nth pixel, which is the definition of aliasing; a
    /// blit averages.
    preview_image: vk::Image,
    preview_memory: vk::DeviceMemory,
    /// The size of that picture, which is what `read_scaled` reads from.
    preview_size: vk::Extent2D,
    /// Whether the finished frame is copied out to be looked at.
    ///
    /// The copy is a megabyte per megapixel, every frame, across the bus — and
    /// it is pure cost when nobody is watching. An operator who has switched
    /// previews off has said nobody is, so the copy stops with
    /// the encode rather than being recorded and thrown away.
    readback_wanted: bool,
    pipeline_layout: vk::PipelineLayout,
    /// One pipeline per blend mode, indexed by [`blend::index_of`].
    ///
    /// A blend mode is blend state and nothing else, so the modes differ only in
    /// which of these is bound. Eight pipelines built once at startup beats
    /// rebuilding one per frame, and beats a shader that reads the destination,
    /// which core Vulkan cannot do.
    pipelines: [vk::Pipeline; blend::BLEND_MODE_COUNT],
    shader: vk::ShaderModule,
    descriptor_layout: vk::DescriptorSetLayout,
    descriptor_pool: vk::DescriptorPool,
    /// Samples this target's own image, for whoever draws from it.
    ///
    /// Kept here rather than made by the caller so that everything sampling a
    /// composition uses the same filtering, which is what decides how a piece
    /// that scales looks on the wall.
    sampler: vk::Sampler,
    /// How many quads the last recorded frame contained.
    ///
    /// Only for reporting. Nothing on the render path reads it.
    last_quad_count: usize,
    /// Whether the "this blend mode is not drawn as configured" line has been
    /// logged. Once per target, not once per frame.
    warned_about_blend: bool,
    /// Points at another target's image, for quads that sample the composition.
    ///
    /// `None` until one is bound. A target with nothing bound draws a piece as
    /// its flat colour rather than failing, which is what the first frames of a
    /// renderer look like and what an output with no compositor would show.
    composition: Option<vk::DescriptorSet>,
    /// Another target's buses, for a piece that samples one. Empty on the
    /// compositor, which reads its own through `buses`.
    bound_buses: [Option<vk::DescriptorSet>; BUS_COUNT],
    /// The one-by-one white texture a quad with no image of its own uses.
    ///
    /// Not a placeholder: white is the identity for the shader's multiply, so a
    /// flat quad takes the same path as a textured one rather than needing a
    /// second pipeline or a branch per fragment.
    default_texture: Bound,
    /// One texture per media item the scene refers to.
    ///
    /// A sorted map rather than a hash map: the number of entries is small, the
    /// lookup happens once per run of quads rather than per quad, and iteration
    /// order being stable makes a test's failure reproducible.
    textures: BTreeMap<TextureKey, Bound>,
    /// The pipeline generators are drawn by, once something needs one.
    ///
    /// Made on the first generator rather than at startup, for the reason the
    /// scratch images are: a show with no generators in it never builds this,
    /// and a pipeline that is never used is a pipeline that cannot fail to be
    /// created on a machine that was going to be fine.
    generated: Option<GeneratedPipeline>,
    /// The generators to draw into their textures before this frame's buses.
    ///
    /// Recorded into the frame's own command buffer rather than submitted
    /// separately, exactly as an upload is: the draw cannot then race the
    /// composition pass that samples what it wrote.
    pending_generated: Vec<(TextureKey, GeneratedPush, Option<TextureKey>)>,
    /// Pictures handed over this pass, waiting to be copied together.
    ///
    /// Kept between frames so the allocation is made once. Drained by
    /// `flush_media_textures`, which `compose` calls before it draws.
    queued: Vec<(TextureKey, std::sync::Arc<DecodedImage>)>,
    instances: vk::Buffer,
    instances_memory: vk::DeviceMemory,
    /// The instance buffer, mapped once and kept mapped.
    ///
    /// Mapping per frame would be a driver round trip in the render path for no
    /// benefit: the memory is host-coherent, so writing through this pointer is
    /// visible to the GPU without a flush.
    instances_mapped: *mut InstanceData,
    size: vk::Extent2D,
    /// Whether this target's pipelines use the advanced blend operations.
    ///
    /// Recorded rather than asked of the device each frame, and it is what
    /// decides whether a mode is drawn as itself or as Normal with a warning.
    advanced_blend: bool,
    /// The three buses, on a target that composites a show.
    ///
    /// `None` on an output's target, which draws pieces out of a finished
    /// composition and has no buses of its own. Made on the first composition
    /// pass rather than at startup, because that is the moment it is known that
    /// this target is the one compositing.
    buses: Option<Buses>,
    /// What a layer's effect chain is drawn through.
    ///
    /// `None` until a scene arrives with an effect in it, and never made at all
    /// for a show that has none: two images at composition size are sixteen
    /// megabytes on a 1080p canvas, and a show with no effects must not pay
    /// them.
    scratch: Option<Scratch>,
    /// This frame's composition, as recorded work.
    ///
    /// Held here rather than returned from the upload so that a frame allocates
    /// nothing.
    upload: BusUpload,
}

// SAFETY: `instances_mapped` points into host-coherent memory owned by this
// target for as long as the target lives, and nothing else maps or frees it.
// The target itself is only ever used from one thread at a time, which is what
// `Send` promises.
unsafe impl Send for OffscreenTarget {}

/// One draw call: the quads from `first` to `first + count` share a texture.
///
/// The scene is uploaded in draw order, so a run is a contiguous slice of the
/// instance buffer and needs no index buffer or per-quad bookkeeping to express.
#[derive(Clone, Copy)]
struct DrawRun {
    set: vk::DescriptorSet,
    /// The blend mode's pipeline. Two neighbouring quads sharing a texture but
    /// not a blend mode are two runs, because the blend state is baked in.
    pipeline: vk::Pipeline,
    first: u32,
    count: u32,
}

/// A layer's effect chain, as work to record.
#[derive(Clone, Copy)]
struct Chain {
    /// Draws the layer's own picture into the first scratch image.
    source: DrawRun,
    /// Where this chain's passes are in the frame's list.
    first: usize,
    /// How many passes it runs.
    count: usize,
}

impl Chain {
    /// Which of the two scratch images the finished chain is in.
    ///
    /// The passes ping-pong from the image the source draw filled, so an even
    /// number of them ends back where it started.
    const fn result(self) -> usize {
        self.count % SCRATCH_COUNT
    }
}

/// One layer's picture, and what is needed to run its effect chain.
///
/// One thing rather than five parameters. They are always passed together and
/// they are all about the same layer, which is what a struct is for.
struct ChainInput<'a> {
    /// The layer's quad, as the scene has it and untinted.
    ///
    /// What goes into a chain is the picture the layer draws. What the master
    /// dimmer does to it happens at the far end, in `tint`.
    quad: &'a Quad,
    /// The descriptor set the layer's texture is reached through.
    set: vk::DescriptorSet,
    /// One composition pixel, which is what a pass measures a radius in.
    texel: [f32; 2],
    /// What the finished picture is scaled by as it is composited.
    tint: f32,
    /// The passes to run, in the order the show listed them.
    passes: &'a [EffectPass],
}

/// One stretch of a bus's recording.
///
/// A bus with no effects on it is one of these: a chain of nothing, then every
/// layer routed there. A chain has to run outside the bus's render pass, so
/// each one ends the stretch that was open and begins another.
#[derive(Clone, Copy)]
struct BusStep {
    /// The chain whose result the first of these draws composites.
    chain: Option<Chain>,
    /// Where these draws are in the bus's own list.
    first: usize,
    /// How many of them there are.
    count: usize,
}

/// One frame's composition, in the order it is recorded.
///
/// Kept on the target and emptied each frame rather than built fresh. A `Vec`
/// that is cleared keeps its capacity, so after the first few frames a
/// composition allocates nothing at all, which is what the realtime rules
/// require (house rule §5).
#[derive(Default)]
struct BusUpload {
    /// Each bus's draws, in draw order.
    runs: [Vec<DrawRun>; BUS_COUNT],
    /// Each bus's recording, as stretches of those draws.
    steps: [Vec<BusStep>; BUS_COUNT],
    /// Every effect pass this frame runs, one chain after another.
    passes: Vec<EffectPush>,
    /// The second texture each of those passes samples, one for one with them.
    ///
    /// Beside the passes rather than in them: a pass is pushed to the shader as
    /// constants and a descriptor set is not something that can be pushed. Every
    /// pass has an entry, and a pass with no picture of its own gets the white
    /// default — the set has to be bound either way, and white is a mask that
    /// does nothing.
    masks: Vec<vk::DescriptorSet>,
    /// Where each of those passes leaves its result as well, one for one with
    /// them: the echo's memory, by its place in the target's list, and `None`
    /// for every pass that remembers nothing.
    keeps: Vec<Option<usize>>,
    /// The quads that combine the buses into the composition.
    combine: Vec<DrawRun>,
}

impl BusUpload {
    /// Empties everything, keeping the allocations.
    fn clear(&mut self) {
        for runs in &mut self.runs {
            runs.clear();
        }
        for steps in &mut self.steps {
            steps.clear();
        }
        self.passes.clear();
        // One for one with the passes, so they are emptied together or the next
        // frame's passes read the last frame's pictures.
        self.masks.clear();
        self.keeps.clear();
        self.combine.clear();
    }
}

/// A texture together with the descriptor set that points at it.
///
/// Kept as one thing because they are only ever useful together: a texture the
/// shader cannot reach is not a texture, and a set pointing at a freed image is
/// worse than neither.
struct Bound {
    texture: Texture,
    set: vk::DescriptorSet,
    /// Where the card draws, for a texture a generator fills.
    ///
    /// `None` for every texture the processor fills, which is all of them bar
    /// the generators: a framebuffer is only needed by something that is
    /// drawn into, and making one for an image that is only ever copied into
    /// would be a handle to free for nothing.
    framebuffer: Option<vk::Framebuffer>,
    /// Whether the pixels still have to be copied to the GPU.
    ///
    /// The copy is recorded into the frame's own command buffer rather than
    /// submitted separately, so an upload costs no extra submit and cannot race
    /// the draw that reads it.
    dirty: bool,
}

impl Bound {
    /// Frees the texture, and the framebuffer if this is one the card drew.
    ///
    /// One place rather than five. Every path that lets a texture go - a
    /// resize, a layout change, a layer dropping its media, the teardown -
    /// has to free both, and the one that forgot would leak a framebuffer per
    /// resize of a generator rather than fail in any visible way.
    ///
    /// The device must be idle, for the reason [`Texture::destroy`] says:
    /// a frame in flight may still be reading what this frees.
    fn release(&mut self, device: &ash::Device) {
        if let Some(framebuffer) = self.framebuffer.take() {
            // SAFETY: the caller has waited for the device to be idle, so no
            // frame still refers to it.
            unsafe { device.destroy_framebuffer(framebuffer, None) };
        }
        self.texture.destroy(device);
    }
}

/// SPIR-V compiled from `shaders/canvas.wgsl` by the build script.
///
/// One module holding both entry points: Vulkan picks the stage by entry point
/// name, so splitting them would mean loading the same module twice.
const SHADER_SPIRV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/canvas.spv"));

/// SPIR-V compiled from `shaders/effect.wgsl` by the build script.
///
/// Loaded only by a target that has an effect to run, like everything else
/// about the scratch.
const EFFECT_SHADER_SPIRV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/effect.spv"));

/// SPIR-V compiled from `shaders/generator.wgsl` by the build script.
const GENERATOR_SHADER_SPIRV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/generator.spv"));

/// Points a descriptor set at a texture's planes and sampler.
fn point_set_at(device: &ash::Device, set: vk::DescriptorSet, texture: &Texture) {
    let (cb, cr) = texture.chroma_views();
    point_set_at_planes(device, set, texture.view(), cb, cr, texture.sampler());
}

/// Points a descriptor set at any image view and sampler.
///
/// For the images that are RGBA by construction — the composition, a bus, the
/// scratch — which name themselves for all three bindings.
fn point_set_at_image(
    device: &ash::Device,
    set: vk::DescriptorSet,
    view: vk::ImageView,
    sampler: vk::Sampler,
) {
    point_set_at_planes(device, set, view, view, view, sampler);
}

/// Points a descriptor set at three image views and a sampler.
///
/// Every binding the layout declares is written, always. A set with a binding
/// left empty is undefined behaviour the moment a shader that declares it is
/// bound, whether or not that shader reads it.
fn point_set_at_planes(
    device: &ash::Device,
    set: vk::DescriptorSet,
    luma: vk::ImageView,
    cb: vk::ImageView,
    cr: vk::ImageView,
    sampler: vk::Sampler,
) {
    let described = |view| {
        [vk::DescriptorImageInfo::default()
            .image_view(view)
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)]
    };
    let luma_info = described(luma);
    let cb_info = described(cb);
    let cr_info = described(cr);
    let sampler_info = [vk::DescriptorImageInfo::default().sampler(sampler)];

    let writes = [
        vk::WriteDescriptorSet::default()
            .dst_set(set)
            .dst_binding(0)
            .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
            .image_info(&luma_info),
        vk::WriteDescriptorSet::default()
            .dst_set(set)
            .dst_binding(1)
            .descriptor_type(vk::DescriptorType::SAMPLER)
            .image_info(&sampler_info),
        vk::WriteDescriptorSet::default()
            .dst_set(set)
            .dst_binding(2)
            .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
            .image_info(&cb_info),
        vk::WriteDescriptorSet::default()
            .dst_set(set)
            .dst_binding(3)
            .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
            .image_info(&cr_info),
    ];
    // SAFETY: every borrowed slice outlives the call, and the views and sampler
    // belong to a texture that outlives the set.
    unsafe { device.update_descriptor_sets(&writes, &[]) };
}

/// How large a frame is shrunk to on the card, before it crosses the bus.
///
/// A ceiling rather than a size: a surface smaller than this keeps its own
/// size, because upscaling a wall to make a preview of it would be inventing
/// detail. 720p is what a client preview is worth watching at,
/// and everything below it — the System screen's thumbnails — is a further step
/// down from a picture that has already been averaged, rather than from a full
/// frame sampled every nth pixel.
fn preview_extent(size: vk::Extent2D) -> vk::Extent2D {
    const CEILING: vk::Extent2D = vk::Extent2D {
        width: 1280,
        height: 720,
    };

    let width = u64::from(size.width.max(1));
    let height = u64::from(size.height.max(1));
    if width <= u64::from(CEILING.width) && height <= u64::from(CEILING.height) {
        return vk::Extent2D {
            width: size.width.max(1),
            height: size.height.max(1),
        };
    }

    // Whichever side runs out of room first decides the scale, in integers so
    // the answer cannot depend on how a float rounded. Even sides, because
    // every video encoder downstream of this wants them.
    if width * u64::from(CEILING.height) >= height * u64::from(CEILING.width) {
        let scaled = (height * u64::from(CEILING.width) / width).max(2) as u32;
        vk::Extent2D {
            width: CEILING.width,
            height: scaled & !1,
        }
    } else {
        let scaled = (width * u64::from(CEILING.height) / height).max(2) as u32;
        vk::Extent2D {
            width: scaled & !1,
            height: CEILING.height,
        }
    }
}

impl OffscreenTarget {
    /// Creates a target of the given size on `context`'s device.
    ///
    /// # Errors
    ///
    /// Returns [`VulkanError::Call`] naming the Vulkan call that failed.
    pub fn new(context: &VulkanContext, size: vk::Extent2D) -> Result<Self, VulkanError> {
        let device = context.device();

        let image_info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            // 8-bit RGBA, which is what an LED processor wants and what a
            // readback can be compared against byte for byte.
            .format(COMPOSITION_FORMAT)
            .extent(vk::Extent3D {
                width: size.width,
                height: size.height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            // SAMPLED as well as the obvious two: a composition is drawn into
            // one of these and then read by every output on the same device.
            .usage(
                vk::ImageUsageFlags::COLOR_ATTACHMENT
                    | vk::ImageUsageFlags::TRANSFER_SRC
                    | vk::ImageUsageFlags::SAMPLED,
            )
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);

        // SAFETY: `image_info` is fully initialised and outlives the call.
        let image = unsafe { device.create_image(&image_info, None) }.map_err(|result| {
            VulkanError::Call {
                operation: "vkCreateImage",
                result,
            }
        })?;

        // SAFETY: `image` was just created on this device.
        let requirements = unsafe { device.get_image_memory_requirements(image) };
        // SAFETY: the physical device belongs to this context's instance.
        let memory_properties = unsafe {
            context
                .instance_ref()
                .get_physical_device_memory_properties(context.physical_device())
        };

        let memory_type = find_memory_type(
            &memory_properties,
            requirements.memory_type_bits,
            vk::MemoryPropertyFlags::DEVICE_LOCAL,
        )
        .ok_or(VulkanError::Call {
            operation: "finding device-local memory for the canvas",
            result: vk::Result::ERROR_FEATURE_NOT_PRESENT,
        })?;

        let allocate = vk::MemoryAllocateInfo::default()
            .allocation_size(requirements.size)
            .memory_type_index(memory_type);
        // SAFETY: `allocate` is fully initialised.
        let memory = unsafe { device.allocate_memory(&allocate, None) }.map_err(|result| {
            VulkanError::Call {
                operation: "vkAllocateMemory",
                result,
            }
        })?;
        // SAFETY: `memory` was allocated for exactly this image's requirements.
        unsafe { device.bind_image_memory(image, memory, 0) }.map_err(|result| {
            VulkanError::Call {
                operation: "vkBindImageMemory",
                result,
            }
        })?;

        let view_info = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(COMPOSITION_FORMAT)
            .subresource_range(
                vk::ImageSubresourceRange::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .level_count(1)
                    .layer_count(1),
            );
        // SAFETY: `image` is bound and `view_info` is fully initialised.
        let view = unsafe { device.create_image_view(&view_info, None) }.map_err(|result| {
            VulkanError::Call {
                operation: "vkCreateImageView",
                result,
            }
        })?;

        let render_pass = Self::create_render_pass(device)?;

        let attachments = [view];
        let framebuffer_info = vk::FramebufferCreateInfo::default()
            .render_pass(render_pass)
            .attachments(&attachments)
            .width(size.width)
            .height(size.height)
            .layers(1);
        // SAFETY: the render pass and view are live, and `attachments` outlives
        // the call.
        let framebuffer =
            unsafe { device.create_framebuffer(&framebuffer_info, None) }.map_err(|result| {
                VulkanError::Call {
                    operation: "vkCreateFramebuffer",
                    result,
                }
            })?;

        let graphics_family = context.summary().graphics_family.ok_or(VulkanError::Call {
            operation: "the selected device has no graphics queue",
            result: vk::Result::ERROR_FEATURE_NOT_PRESENT,
        })?;
        let pool_info = vk::CommandPoolCreateInfo::default()
            // Buffers are re-recorded every frame, so they are reset
            // individually rather than the whole pool being torn down.
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
            .queue_family_index(graphics_family);
        // SAFETY: `pool_info` is fully initialised.
        let command_pool =
            unsafe { device.create_command_pool(&pool_info, None) }.map_err(|result| {
                VulkanError::Call {
                    operation: "vkCreateCommandPool",
                    result,
                }
            })?;

        let allocate_info = vk::CommandBufferAllocateInfo::default()
            .command_pool(command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        // SAFETY: the pool was just created on this device.
        let command_buffers =
            unsafe { device.allocate_command_buffers(&allocate_info) }.map_err(|result| {
                VulkanError::Call {
                    operation: "vkAllocateCommandBuffers",
                    result,
                }
            })?;
        let command_buffer = *command_buffers.first().ok_or(VulkanError::Call {
            operation: "vkAllocateCommandBuffers returned nothing",
            result: vk::Result::ERROR_INITIALIZATION_FAILED,
        })?;

        // The queue the copies go to. Its own pool, because a command pool
        // belongs to one family; the same family as graphics on a device with
        // no transfer-only queue, which costs nothing and needs no second path.
        let transfer_family = context.transfer_family();
        let transfer_pool_info = vk::CommandPoolCreateInfo::default()
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
            .queue_family_index(transfer_family);
        // SAFETY: `transfer_pool_info` is fully initialised.
        let transfer_pool = unsafe { device.create_command_pool(&transfer_pool_info, None) }
            .map_err(|result| VulkanError::Call {
                operation: "vkCreateCommandPool for uploads",
                result,
            })?;
        let transfer_allocate = vk::CommandBufferAllocateInfo::default()
            .command_pool(transfer_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        // SAFETY: the pool was just created on this device.
        let transfer_buffers = unsafe { device.allocate_command_buffers(&transfer_allocate) }
            .map_err(|result| VulkanError::Call {
                operation: "vkAllocateCommandBuffers for uploads",
                result,
            })?;
        let transfer_buffer = *transfer_buffers.first().ok_or(VulkanError::Call {
            operation: "vkAllocateCommandBuffers returned nothing",
            result: vk::Result::ERROR_INITIALIZATION_FAILED,
        })?;

        let semaphore_info = vk::SemaphoreCreateInfo::default();
        // SAFETY: `semaphore_info` is fully initialised.
        let uploaded =
            unsafe { device.create_semaphore(&semaphore_info, None) }.map_err(|result| {
                VulkanError::Call {
                    operation: "vkCreateSemaphore",
                    result,
                }
            })?;

        // Created signalled, so the first frame does not wait on a fence that
        // nothing has submitted to.
        let fence_info = vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED);
        // SAFETY: `fence_info` is fully initialised.
        let fence = unsafe { device.create_fence(&fence_info, None) }.map_err(|result| {
            VulkanError::Call {
                operation: "vkCreateFence",
                result,
            }
        })?;

        // Small enough to be cheap to carry, large enough that a client asking
        // for a 720p preview is not being shown an upscale.
        let preview_size = preview_extent(size);
        let (preview_image, preview_memory) =
            Self::create_preview_image(context, &memory_properties, preview_size)?;
        let (readback, readback_memory) =
            Self::create_readback_buffer(context, &memory_properties, preview_size)?;

        let (instances, instances_memory, instances_mapped) =
            Self::create_instance_buffer(context, &memory_properties)?;

        let shader = Self::create_shader_module(device, SHADER_SPIRV)?;
        let descriptor_layout = Self::create_descriptor_layout(device)?;
        let (pipeline_layout, pipelines) = Self::create_pipelines(
            device,
            render_pass,
            shader,
            descriptor_layout,
            size,
            context.has_advanced_blend(),
        )?;

        let descriptor_pool = Self::create_descriptor_pool(device)?;
        let sampler = Texture::create_sampler(device)?;

        // A one-by-one white texture, which is the identity for the shader's
        // multiply, so a quad with no image of its own needs no special case.
        let mut texture = Texture::new(context, 1, 1)?;
        texture.stage(&white_pixel())?;
        let set = Self::allocate_set(device, descriptor_pool, descriptor_layout, &texture)?;
        let default_texture = Bound {
            texture,
            set,
            framebuffer: None,
            dirty: true,
        };

        Ok(Self {
            image,
            memory,
            view,
            render_pass,
            framebuffer,
            command_pool,
            command_buffer,
            transfer_pool,
            transfer_buffer,
            uploaded,
            transfer_family,
            graphics_family,
            fence,
            readback,
            readback_memory,
            effect_seconds: 0.0,
            // A sixtieth until whoever makes this says otherwise; the backend
            // that owns it does, from the mode it was made for.
            effect_frame: 1.0 / 60.0,
            composed: 0,
            histories: Vec::new(),
            wanted_histories: Vec::new(),
            warned_about_echoes: false,
            readback_wanted: true,
            preview_image,
            preview_memory,
            preview_size,
            pipeline_layout,
            pipelines,
            shader,
            descriptor_layout,
            descriptor_pool,
            sampler,
            last_quad_count: 0,
            warned_about_blend: false,
            composition: None,
            bound_buses: [None; BUS_COUNT],
            default_texture,
            textures: BTreeMap::new(),
            generated: None,
            pending_generated: Vec::new(),
            queued: Vec::new(),
            instances,
            instances_memory,
            instances_mapped,
            size,
            advanced_blend: context.has_advanced_blend(),
            buses: None,
            scratch: None,
            upload: BusUpload::default(),
        })
    }

    /// Returns true when this target has allocated an effect chain's scratch.
    ///
    /// Only a scene with an enabled effect in it makes one, and this exists so
    /// that a test can say so: "a layer with no effects costs nothing extra" is
    /// a claim about what was allocated as much as about what was drawn. Not
    /// built into the renderer, because nothing on the render path may take a
    /// decision on it.
    #[cfg(test)]
    pub(crate) const fn has_scratch(&self) -> bool {
        self.scratch.is_some()
    }

    /// Creates the three bus images, once.
    fn ensure_buses(&mut self, context: &VulkanContext) -> Result<(), VulkanError> {
        if self.buses.is_some() {
            return Ok(());
        }

        let device = context.device();
        let render_pass = Self::create_sampled_render_pass(device, false)?;
        let reopened = Self::create_sampled_render_pass(device, true)?;
        let pipelines = Self::create_bus_pipelines(
            device,
            render_pass,
            self.shader,
            self.pipeline_layout,
            self.size,
            self.advanced_blend,
        )?;

        let mut images = Vec::with_capacity(BUS_COUNT);
        for _ in 0..BUS_COUNT {
            images.push(Self::create_sampled_image(
                context,
                render_pass,
                self.size,
                self.descriptor_pool,
                self.descriptor_layout,
                self.sampler,
            )?);
        }

        let images: [SampledImage; BUS_COUNT] =
            images.try_into().map_err(|_| VulkanError::Call {
                operation: "building the bus images",
                result: vk::Result::ERROR_INITIALIZATION_FAILED,
            })?;

        self.buses = Some(Buses {
            images,
            render_pass,
            reopened,
            pipelines,
        });
        Ok(())
    }

    /// Creates the two scratch images and the pipelines a chain runs through.
    ///
    /// Called only on a frame that has an effect to run, so a show with none
    /// never reaches it. The pass pipeline is built against the scratch's own
    /// render pass, and the source pipeline uses the composition shader — a
    /// layer's picture arrives in the scratch by exactly the arithmetic that
    /// would have put it on the bus.
    fn ensure_scratch(&mut self, context: &VulkanContext) -> Result<(), VulkanError> {
        if self.scratch.is_some() {
            return Ok(());
        }

        let device = context.device();
        let render_pass = Self::create_sampled_render_pass(device, false)?;
        let sampler = Self::create_scratch_sampler(device)?;
        let shader = Self::create_shader_module(device, EFFECT_SHADER_SPIRV)?;
        let source = Self::create_scratch_source_pipeline(
            device,
            render_pass,
            self.shader,
            self.pipeline_layout,
            self.size,
        )?;
        let (pass_layout, pass) = Self::create_pass_pipeline(
            device,
            render_pass,
            shader,
            self.descriptor_layout,
            self.size,
        )?;

        let mut images = Vec::with_capacity(SCRATCH_COUNT);
        for _ in 0..SCRATCH_COUNT {
            images.push(Self::create_sampled_image(
                context,
                render_pass,
                self.size,
                self.descriptor_pool,
                self.descriptor_layout,
                sampler,
            )?);
        }

        let images: [SampledImage; SCRATCH_COUNT] =
            images.try_into().map_err(|_| VulkanError::Call {
                operation: "building the scratch images",
                result: vk::Result::ERROR_INITIALIZATION_FAILED,
            })?;

        self.scratch = Some(Scratch {
            images,
            render_pass,
            source,
            pass,
            pass_layout,
            shader,
            sampler,
        });
        Ok(())
    }

    /// Gives every echo this frame draws a memory, and marks the ones that
    /// have to start again from nothing.
    ///
    /// A picture is made only for an echo that has never had one and only
    /// while there are fewer than [`MAX_ECHOES`]; past that, the one drawn
    /// longest ago that this frame does not want is handed over. So what a
    /// show holds is bounded whatever it does, and a frame allocates only the
    /// first time a new echo appears.
    ///
    /// Not an error when it cannot. An echo with no memory draws its layer
    /// untouched, which is what it does at length nought, and a wall missing a
    /// trail is still a wall.
    ///
    /// After the fence wait: a memory being cleared or handed over must not
    /// still be read by the frame before.
    fn ensure_histories(&mut self, context: &VulkanContext, scene: &Scene) {
        let mut wanted = std::mem::take(&mut self.wanted_histories);
        wanted.clear();
        for quad in scene.quads() {
            let Some(layer) = quad.layer_id else {
                continue;
            };
            for pass in scene.effects(layer) {
                if let Some(instance) = pass.history {
                    if !wanted.contains(&instance) {
                        wanted.push(instance);
                    }
                }
            }
        }

        let last_frame = self.composed.wrapping_sub(1);
        for &instance in &wanted {
            if let Some(history) = self
                .histories
                .iter_mut()
                .find(|history| history.instance == instance)
            {
                if history.drawn_in != last_frame {
                    history.clear = true;
                }
                continue;
            }

            if self.histories.len() < MAX_ECHOES {
                match self.create_history(context, instance) {
                    Ok(history) => self.histories.push(history),
                    Err(error) => self.warn_about_echoes(&error.to_string()),
                }
                continue;
            }

            let spare = self
                .histories
                .iter_mut()
                .filter(|history| !wanted.contains(&history.instance))
                .min_by_key(|history| history.drawn_in);
            match spare {
                Some(history) => {
                    history.instance = instance;
                    history.clear = true;
                }
                None => self.warn_about_echoes("every memory this canvas has is in use"),
            }
        }

        self.wanted_histories = wanted;
    }

    /// Makes one echo's memory: a scratch image of its own.
    fn create_history(
        &self,
        context: &VulkanContext,
        instance: EffectInstanceId,
    ) -> Result<History, VulkanError> {
        let scratch = self.scratch.as_ref().ok_or(VulkanError::Call {
            operation: "remembering an echo with no scratch image",
            result: vk::Result::ERROR_INITIALIZATION_FAILED,
        })?;
        let image = Self::create_sampled_image(
            context,
            scratch.render_pass,
            self.size,
            self.descriptor_pool,
            self.descriptor_layout,
            scratch.sampler,
        )?;
        Ok(History {
            instance,
            image,
            drawn_in: self.composed,
            clear: true,
        })
    }

    /// Says once that an echo is being drawn without its memory.
    fn warn_about_echoes(&mut self, reason: &str) {
        if !self.warned_about_echoes {
            tracing::warn!(
                remembered = MAX_ECHOES,
                reason,
                "an echo has no memory to draw from; its layer is drawn untouched"
            );
            self.warned_about_echoes = true;
        }
    }

    /// Creates one image this target draws into and samples, with its view,
    /// framebuffer and descriptor set.
    fn create_sampled_image(
        context: &VulkanContext,
        render_pass: vk::RenderPass,
        size: vk::Extent2D,
        descriptor_pool: vk::DescriptorPool,
        descriptor_layout: vk::DescriptorSetLayout,
        sampler: vk::Sampler,
    ) -> Result<SampledImage, VulkanError> {
        let device = context.device();

        let image_info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(COMPOSITION_FORMAT)
            .extent(vk::Extent3D {
                width: size.width,
                height: size.height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            // Drawn into, then sampled by the combine pass. Never copied out:
            // what is read back is the composition, not a bus.
            .usage(vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::SAMPLED)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);

        // SAFETY: `image_info` is fully initialised and outlives the call.
        let image = unsafe { device.create_image(&image_info, None) }.map_err(|result| {
            VulkanError::Call {
                operation: "vkCreateImage for a bus",
                result,
            }
        })?;

        // SAFETY: `image` was just created on this device.
        let requirements = unsafe { device.get_image_memory_requirements(image) };
        // SAFETY: the physical device belongs to this context's instance.
        let memory_properties = unsafe {
            context
                .instance_ref()
                .get_physical_device_memory_properties(context.physical_device())
        };
        let memory_type = find_memory_type(
            &memory_properties,
            requirements.memory_type_bits,
            vk::MemoryPropertyFlags::DEVICE_LOCAL,
        )
        .ok_or(VulkanError::Call {
            operation: "finding device-local memory for a bus",
            result: vk::Result::ERROR_FEATURE_NOT_PRESENT,
        })?;

        let allocate = vk::MemoryAllocateInfo::default()
            .allocation_size(requirements.size)
            .memory_type_index(memory_type);
        // SAFETY: `allocate` is fully initialised.
        let memory = unsafe { device.allocate_memory(&allocate, None) }.map_err(|result| {
            VulkanError::Call {
                operation: "vkAllocateMemory for a bus",
                result,
            }
        })?;
        // SAFETY: the allocation matches this image's requirements.
        unsafe { device.bind_image_memory(image, memory, 0) }.map_err(|result| {
            VulkanError::Call {
                operation: "vkBindImageMemory for a bus",
                result,
            }
        })?;

        let view_info = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(COMPOSITION_FORMAT)
            .subresource_range(
                vk::ImageSubresourceRange::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .level_count(1)
                    .layer_count(1),
            );
        // SAFETY: `image` is bound and `view_info` is fully initialised.
        let view = unsafe { device.create_image_view(&view_info, None) }.map_err(|result| {
            VulkanError::Call {
                operation: "vkCreateImageView for a bus",
                result,
            }
        })?;

        let attachments = [view];
        let framebuffer_info = vk::FramebufferCreateInfo::default()
            .render_pass(render_pass)
            .attachments(&attachments)
            .width(size.width)
            .height(size.height)
            .layers(1);
        // SAFETY: the render pass and view are live and outlive the call.
        let framebuffer =
            unsafe { device.create_framebuffer(&framebuffer_info, None) }.map_err(|result| {
                VulkanError::Call {
                    operation: "vkCreateFramebuffer for a bus",
                    result,
                }
            })?;

        let set = Self::allocate_empty_set(device, descriptor_pool, descriptor_layout)?;
        point_set_at_image(device, set, view, sampler);

        Ok(SampledImage {
            image,
            memory,
            view,
            framebuffer,
            set,
        })
    }

    /// The render pass an image that will be sampled is drawn with.
    ///
    /// Ends in `SHADER_READ_ONLY_OPTIMAL`, because what happens to a bus or a
    /// scratch image next is that something samples it. The subpass
    /// dependencies are what make that legal in one command buffer: without
    /// them the combine could read pixels the bus pass had not finished
    /// writing, and a chain's second pass could overwrite the image its first
    /// pass was still being read from.
    ///
    /// `keep` says whether what is already in the image survives. A bus
    /// reopened to composite a layer after its chain has run must keep the
    /// layers drawn before it; an image being filled from nothing discards
    /// whatever it held, which is both cheaper and what `UNDEFINED` is for.
    fn create_sampled_render_pass(
        device: &ash::Device,
        keep: bool,
    ) -> Result<vk::RenderPass, VulkanError> {
        let attachment = vk::AttachmentDescription::default()
            .format(COMPOSITION_FORMAT)
            .samples(vk::SampleCountFlags::TYPE_1)
            .load_op(if keep {
                vk::AttachmentLoadOp::LOAD
            } else {
                vk::AttachmentLoadOp::CLEAR
            })
            .store_op(vk::AttachmentStoreOp::STORE)
            .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
            .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
            .initial_layout(if keep {
                // Where the pass that drew it last left it.
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL
            } else {
                vk::ImageLayout::UNDEFINED
            })
            .final_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);

        let colour_reference = vk::AttachmentReference::default()
            .attachment(0)
            .layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
        let colour_references = [colour_reference];

        let subpass = vk::SubpassDescription::default()
            .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
            .color_attachments(&colour_references);
        let subpasses = [subpass];

        let dependencies = [
            // Whatever was reading this image has finished before it is written
            // again. That is the hazard a ping-pong creates and a bus does not.
            //
            // `COLOR_ATTACHMENT_READ` as well as the write, because a pass that
            // keeps what is already there loads it, and a load is a read at the
            // colour attachment stage. Without it, synchronisation validation
            // reports a `READ_AFTER_WRITE` on `vkCmdBeginRenderPass` itself:
            // the load racing the layout transition that the same command
            // performs. It was found the day `ovs-upload-cost` learnt to run an
            // effect chain, because until then nothing outside the show
            // exercised these passes at all.
            vk::SubpassDependency::default()
                .src_subpass(vk::SUBPASS_EXTERNAL)
                .dst_subpass(0)
                .src_stage_mask(vk::PipelineStageFlags::FRAGMENT_SHADER)
                .src_access_mask(vk::AccessFlags::SHADER_READ)
                .dst_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
                .dst_access_mask(
                    vk::AccessFlags::COLOR_ATTACHMENT_WRITE
                        | vk::AccessFlags::COLOR_ATTACHMENT_READ,
                ),
            // And what this pass writes is visible to whatever samples it next.
            vk::SubpassDependency::default()
                .src_subpass(0)
                .dst_subpass(vk::SUBPASS_EXTERNAL)
                .src_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
                .src_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)
                .dst_stage_mask(vk::PipelineStageFlags::FRAGMENT_SHADER)
                .dst_access_mask(vk::AccessFlags::SHADER_READ),
        ];

        let attachments = [attachment];
        let info = vk::RenderPassCreateInfo::default()
            .attachments(&attachments)
            .subpasses(&subpasses)
            .dependencies(&dependencies);

        // SAFETY: every borrowed slice outlives the call.
        unsafe { device.create_render_pass(&info, None) }.map_err(|result| VulkanError::Call {
            operation: "vkCreateRenderPass for an image that will be sampled",
            result,
        })
    }

    /// The sampler an effect pass reads its source through.
    ///
    /// Nearest, and this is not a preference either. A pass reads texel centres
    /// and nothing else, so a linear filter has nothing to interpolate and can
    /// only round; an effect at its published defaults has to hand back the
    /// picture it was given without changing a byte of it. Clamped, so that a
    /// blur at the edge of the canvas finds the edge rather than the far side.
    fn create_scratch_sampler(device: &ash::Device) -> Result<vk::Sampler, VulkanError> {
        let info = vk::SamplerCreateInfo::default()
            .mag_filter(vk::Filter::NEAREST)
            .min_filter(vk::Filter::NEAREST)
            .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .border_color(vk::BorderColor::FLOAT_TRANSPARENT_BLACK);

        // SAFETY: `info` is fully initialised.
        unsafe { device.create_sampler(&info, None) }.map_err(|result| VulkanError::Call {
            operation: "vkCreateSampler for the scratch",
            result,
        })
    }

    /// Creates the per-instance buffer and maps it for the lifetime of the target.
    fn create_instance_buffer(
        context: &VulkanContext,
        memory_properties: &vk::PhysicalDeviceMemoryProperties,
    ) -> Result<(vk::Buffer, vk::DeviceMemory, *mut InstanceData), VulkanError> {
        let device = context.device();
        let bytes = (MAX_QUADS * std::mem::size_of::<InstanceData>()) as u64;

        let info = vk::BufferCreateInfo::default()
            .size(bytes)
            .usage(vk::BufferUsageFlags::VERTEX_BUFFER)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        // SAFETY: `info` is fully initialised.
        let buffer =
            unsafe { device.create_buffer(&info, None) }.map_err(|result| VulkanError::Call {
                operation: "vkCreateBuffer for instances",
                result,
            })?;

        // SAFETY: `buffer` was just created on this device.
        let requirements = unsafe { device.get_buffer_memory_requirements(buffer) };
        let memory_type = find_memory_type(
            memory_properties,
            requirements.memory_type_bits,
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
        )
        .ok_or(VulkanError::Call {
            operation: "finding host-visible memory for instances",
            result: vk::Result::ERROR_FEATURE_NOT_PRESENT,
        })?;

        let allocate = vk::MemoryAllocateInfo::default()
            .allocation_size(requirements.size)
            .memory_type_index(memory_type);
        // SAFETY: `allocate` is fully initialised.
        let memory = unsafe { device.allocate_memory(&allocate, None) }.map_err(|result| {
            VulkanError::Call {
                operation: "vkAllocateMemory for instances",
                result,
            }
        })?;
        // SAFETY: allocated for exactly this buffer's requirements.
        unsafe { device.bind_buffer_memory(buffer, memory, 0) }.map_err(|result| {
            VulkanError::Call {
                operation: "vkBindBufferMemory for instances",
                result,
            }
        })?;

        // Mapped once, at offset zero, which satisfies the alignment rule that
        // an arbitrary offset would not.
        // SAFETY: the memory is host-visible and host-coherent and nothing else
        // maps it.
        let mapped =
            unsafe { device.map_memory(memory, 0, vk::WHOLE_SIZE, vk::MemoryMapFlags::empty()) }
                .map_err(|result| VulkanError::Call {
                    operation: "vkMapMemory for instances",
                    result,
                })?;

        Ok((buffer, memory, mapped.cast::<InstanceData>()))
    }

    /// How the samples in the texture a quad draws are arranged.
    ///
    /// Everything that is not a media texture is RGBA: the white default, the
    /// composition, a bus, the scratch an effect chain runs in. Only a decoded
    /// video frame is planar, and only if its decoder produced it that way.
    fn layout_for(&self, texture: TextureRef) -> TextureLayout {
        match texture {
            TextureRef::Media(_)
            | TextureRef::Frame(_)
            | TextureRef::TestPattern
            | TextureRef::Console => texture
                .key()
                .and_then(|key| self.textures.get(&key))
                .map_or(TextureLayout::Rgba, |bound| bound.texture.layout()),
            TextureRef::None | TextureRef::Composition | TextureRef::Bus(_) => TextureLayout::Rgba,
        }
    }

    /// The descriptor set a quad samples from.
    ///
    /// A media item whose texture has not been uploaded yet falls back to
    /// white, which draws the quad as its flat colour rather than dropping it:
    /// a layer whose media is still loading stays visible instead of vanishing
    /// mid-show.
    fn set_for(&self, texture: TextureRef) -> vk::DescriptorSet {
        match texture {
            TextureRef::None => self.default_texture.set,
            TextureRef::Media(_)
            | TextureRef::Frame(_)
            | TextureRef::TestPattern
            | TextureRef::Console => texture
                .key()
                .and_then(|key| self.textures.get(&key))
                .map_or(self.default_texture.set, |bound| bound.set),
            // Filled in with the compositor's image when this target is an
            // output. A target with no composition bound draws the piece flat,
            // which is what the first frames of a renderer look like.
            TextureRef::Composition => self.composition.unwrap_or(self.default_texture.set),
            // Only a target that composites a show has buses, and only its own
            // combine pass names them.
            // A compositor reads its own buses; an output reads the ones it was
            // pointed at, which are the compositor's images through sets of its
            // own. Neither is a reason to fail: a piece with nothing behind it
            // draws flat, as it does before the first frame.
            TextureRef::Bus(route) => self.buses.as_ref().map_or_else(
                || self.bound_buses[bus_index(route)].unwrap_or(self.default_texture.set),
                |buses| buses.images[bus_index(route)].set,
            ),
        }
    }

    /// Writes one quad into the instance buffer at `index`.
    ///
    /// # Safety
    ///
    /// `index` must be below [`MAX_QUADS`], which is what the mapping covers.
    unsafe fn write_instance(&self, index: usize, quad: &Quad, passthrough: bool) {
        // Asked of the texture rather than passed in, exactly as the descriptor
        // set is: the two have to agree about the same texture, and a caller
        // that had to remember both would eventually not.
        let layout = self.layout_for(quad.texture);
        let planar = layout.shader_code();
        let sample_scale = layout.sample_scale();
        let (centre, basis) = quad_basis(
            quad,
            SizePx::new(self.size.width.max(1), self.size.height.max(1)),
        );
        let data = InstanceData {
            // The last two are where the source rectangle turns about, in
            // texture space. Padding until a piece's input end learnt to turn.
            placement: [
                centre[0],
                centre[1],
                quad.source_turn.pivot[0],
                quad.source_turn.pivot[1],
            ],
            basis,
            colour: quad.colour,
            source: [
                quad.source.left,
                quad.source.top,
                quad.source.right,
                quad.source.bottom,
            ],
            identity: blend::identity(quad.blend),
            passthrough: if passthrough { 1.0 } else { 0.0 },
            planar,
            sample_scale,
            source_turn: quad.source_turn.matrix,
        };
        // SAFETY: the caller promises `index` is within the mapping.
        unsafe { std::ptr::write(self.instances_mapped.add(index), data) };
    }

    /// Writes the scene into the instance buffer, bus by bus, then the combine.
    ///
    /// Three passes of layers and one of three quads. Grouping by bus is what
    /// makes each bus a picture of its own: layers composite among the layers
    /// they share a bus with, and never between two layers of another
    /// (owner decision Q8).
    ///
    /// A layer with an effect chain takes two instances rather than one. The
    /// first draws its picture into the scratch, where the chain runs; the
    /// second composites what comes out into the bus, with the layer's own
    /// opacity and blend mode. A layer with no chain is written exactly as it
    /// was before any of this existed, into the one stretch of draws a bus with
    /// no effects on it has.
    ///
    /// # M is the master path, not a fourth picture
    ///
    /// When anything is routed to M, the crossfaded A and B go into M's image
    /// first and the layers routed there composite on top of them. So a layer
    /// set to `Add` on M adds to the show; before this it replaced it, because
    /// the M image was built against transparent black and then laid over the
    /// pair with `Normal`, which threw the blend away (owner, 2026-09-23).
    ///
    /// The master dimmer still applies once. A and B carry it in the gains they
    /// are seeded at, and the layers on M are tinted by it as they are written,
    /// so the finished image goes onto the composition unscaled.
    ///
    /// With nothing on M the seeding is skipped entirely and the two sides go
    /// straight onto the composition, which is what this did before and costs a
    /// render pass and a full-canvas draw less on every show that does not use
    /// the bus.
    fn upload_buses(&mut self, scene: &Scene) {
        // Taken out and put back rather than borrowed: the upload buffers
        // belong to the target so that a frame allocates nothing, and holding
        // them aside is what lets the rest of the target be read while they are
        // filled.
        let mut upload = std::mem::take(&mut self.upload);
        upload.clear();

        let quads = scene.quads();
        // Three instances are held back for the combine, which is the one thing
        // that must always fit.
        let room = MAX_QUADS - BUS_COUNT;

        let bus_pipelines = self
            .buses
            .as_ref()
            .map_or(self.pipelines, |buses| buses.pipelines);

        // A bus at zero gain will not be seen, so it is not drawn at all:
        // skipping the pass is what makes a faded-out side cost nothing rather
        // than a full-canvas draw that is then multiplied by zero.
        let gains = scene.gains();

        // One composition pixel, which is what a pass measures a block or a
        // radius in. In composition pixels rather than as a fraction of the
        // surface, so a look set up on one canvas means the same thing after
        // the pieces are mapped onto a wall of another size.
        let texel = [
            1.0 / self.size.width.max(1) as f32,
            1.0 / self.size.height.max(1) as f32,
        ];

        let mut index = 0_usize;
        let mut unsupported_blend = None;
        let mut dropped = 0_usize;

        // Whether M is carrying anything this frame. The bus is the master
        // path: when something is on it, it holds the whole show and the
        // composition takes it alone; when nothing is, there is no reason to
        // pay for a pass that would only copy A and B from one image to
        // another.
        let m_carries_a_layer = quads
            .iter()
            .any(|quad| quad.route == Route::M && gains.m > 0.0);

        for bus in [Route::A, Route::B, Route::M] {
            if gains.for_route(bus) <= 0.0 {
                continue;
            }
            let slot = bus_index(bus);

            // The show, under whatever is about to be routed over it. Added
            // rather than composited, because that is how the crossfader mixes
            // two finished pictures (owner decision Q8), and at the gains that
            // already carry the master dimmer.
            if bus == Route::M && m_carries_a_layer {
                for side in [Route::A, Route::B] {
                    // Exactly the sides that were drawn. A bus faded out is not
                    // drawn at all, and sampling an image this frame never
                    // wrote would be reading whatever the last one left.
                    if upload.steps[bus_index(side)].is_empty() || index >= room {
                        continue;
                    }
                    let gain = gains.for_route(side);
                    let mut seed = Quad::whole_canvas();
                    seed.texture = TextureRef::Bus(side);
                    seed.blend = BlendMode::Add;
                    seed.colour = [gain, gain, gain, 1.0];

                    // SAFETY: `index` is below `room`, which is below
                    // `MAX_QUADS`.
                    unsafe { self.write_instance(index, &seed, true) };
                    Self::extend_step(
                        &mut upload,
                        slot,
                        DrawRun {
                            set: self.set_for(seed.texture),
                            pipeline: bus_pipelines[blend::index_of(BlendMode::Add)],
                            first: u32::try_from(index).unwrap_or(u32::MAX),
                            count: 1,
                        },
                    );
                    index += 1;
                }
            }

            // What a layer's own colour is scaled by as it is written. One on
            // M is composited straight onto the show, so the master dimmer is
            // applied here; one on A or B is drawn at full into a picture that
            // is then seeded at its gain.
            let tint = if bus == Route::M { gains.m } else { 1.0 };

            for quad in quads.iter().filter(|quad| quad.route == bus) {
                let passes = quad
                    .layer_id
                    .map_or(&[][..], |layer_id| scene.effects(layer_id));
                // A chain with nowhere to run is no chain: the scratch is made
                // before the upload on any frame that has one, so this is only
                // false when the layer has no effects at all.
                let chained = !passes.is_empty() && self.scratch.is_some();

                let wanted = if chained { 2 } else { 1 };
                if index + wanted > room {
                    dropped += 1;
                    continue;
                }

                let set = self.set_for(quad.texture);
                let pipeline = bus_pipelines[blend::index_of(quad.blend)];
                if !blend::is_supported(quad.blend, self.advanced_blend) {
                    unsupported_blend = Some(quad.blend);
                }

                let chain = if chained {
                    self.upload_chain(
                        &mut upload,
                        ChainInput {
                            quad,
                            set,
                            texel,
                            tint,
                            passes,
                        },
                        &mut index,
                    )
                } else {
                    None
                };

                match chain {
                    Some(chain) => {
                        let result = chain.result();
                        let scratch_set = self
                            .scratch
                            .as_ref()
                            .map_or(self.default_texture.set, |scratch| {
                                scratch.images[result].set
                            });

                        // A chain has to run before the bus pass that
                        // composites its result, and a render pass cannot begin
                        // inside another. So the composite begins a stretch of
                        // its own, and whatever follows it on this bus joins
                        // that stretch.
                        Self::begin_step(
                            &mut upload,
                            slot,
                            chain,
                            DrawRun {
                                set: scratch_set,
                                pipeline,
                                first: u32::try_from(index).unwrap_or(u32::MAX),
                                count: 1,
                            },
                        );
                    }
                    None => {
                        let mut drawn = *quad;
                        drawn.colour[0] *= tint;
                        drawn.colour[1] *= tint;
                        drawn.colour[2] *= tint;
                        // SAFETY: `index` is below `room`, which is below
                        // `MAX_QUADS`.
                        unsafe { self.write_instance(index, &drawn, false) };
                        Self::extend_step(
                            &mut upload,
                            slot,
                            DrawRun {
                                set,
                                pipeline,
                                first: u32::try_from(index).unwrap_or(u32::MAX),
                                count: 1,
                            },
                        );
                    }
                }
                index += 1;
            }
        }

        if dropped > 0 {
            tracing::warn!(
                dropped,
                held = room,
                "the composition needs more instances than the buffer holds"
            );
        }
        self.warn_about_blend(unsupported_blend);

        // The combine. With something on M, that bus already holds the whole
        // show — the crossfaded pair was seeded into it and the layers routed
        // there composited on top — so it goes on unscaled and alone. With
        // nothing on M, the two sides go straight on at their gains, which
        // already carry the master dimmer.
        let combining: &[(Route, f32)] = if m_carries_a_layer {
            &[(Route::M, 1.0)]
        } else {
            &[(Route::A, gains.a), (Route::B, gains.b)]
        };

        for &(bus, gain) in combining {
            // Exactly the buses that were drawn, which is what stops the
            // combine sampling an image this frame never wrote.
            if upload.steps[bus_index(bus)].is_empty() {
                continue;
            }

            // Added rather than composited: two finished pictures mixed by the
            // crossfader, and M alone when it is carrying the show, where over
            // opaque black the two are the same thing.
            let blend_mode = BlendMode::Add;

            let mut quad = Quad::whole_canvas();
            quad.texture = TextureRef::Bus(bus);
            quad.blend = blend_mode;
            // The gain scales the colour; alpha stays coverage.
            quad.colour = [gain, gain, gain, 1.0];

            // SAFETY: at most `BUS_COUNT` of these, in the room held back.
            unsafe { self.write_instance(index, &quad, true) };
            upload.combine.push(DrawRun {
                set: self.set_for(quad.texture),
                pipeline: self.pipelines[blend::index_of(quad.blend)],
                first: u32::try_from(index).unwrap_or(u32::MAX),
                count: 1,
            });
            index += 1;
        }

        self.last_quad_count = quads.len().min(room);
        self.upload = upload;
    }

    /// Writes one layer's picture and its chain's passes, returning the chain.
    ///
    /// The picture goes in at full alpha and unpremultiplied: opacity belongs to
    /// the composite at the far end of the chain, and an effect that was handed
    /// a faded picture would be an effect whose result changed as a console
    /// moved a fader. The layer's own placeholder colour stays, because it is
    /// part of what the layer looks like rather than part of its opacity.
    fn upload_chain(
        &self,
        upload: &mut BusUpload,
        input: ChainInput<'_>,
        index: &mut usize,
    ) -> Option<Chain> {
        let ChainInput {
            quad,
            set,
            texel,
            tint,
            passes,
        } = input;
        let scratch = self.scratch.as_ref()?;

        let mut source_quad = *quad;
        source_quad.colour[3] = 1.0;
        // SAFETY: the caller checked there is room for both instances.
        unsafe { self.write_instance(*index, &source_quad, true) };
        let source = DrawRun {
            set,
            pipeline: scratch.source,
            first: u32::try_from(*index).unwrap_or(u32::MAX),
            count: 1,
        };
        *index += 1;

        let clock = EffectClock {
            seconds: self.effect_seconds,
            frame: self.effect_frame,
            // Wrapping, which is all a dither needs of it.
            tick: self.composed as u32,
        };
        let first = upload.passes.len();
        for pass in passes {
            let mut push = EffectPush::new(pass, texel, clock);
            let kept = pass.history.and_then(|instance| {
                self.histories
                    .iter()
                    .position(|history| history.instance == instance)
            });
            // Resolved here rather than at record time, because this is where
            // the textures are. A picture that is not on the GPU yet resolves
            // to white, which is a mask that does nothing: an effect waiting
            // for its picture to decode leaves the layer alone, the same way a
            // layer whose clip is still opening keeps drawing.
            let second = match (pass.history, kept) {
                (_, Some(at)) => self.histories[at].image.set,
                // An echo with no memory is drawn at length nought, which is
                // the picture untouched. White is a mask that does nothing,
                // but read as a trail it would be opaque white over all of it.
                (Some(_), None) => {
                    push.values[0] = 0.0;
                    self.default_texture.set
                }
                (None, None) => pass.mask.map_or(self.default_texture.set, |media_item_id| {
                    self.set_for(TextureRef::Media(media_item_id))
                }),
            };
            upload.passes.push(push);
            upload.masks.push(second);
            upload.keeps.push(kept);
        }

        // The composite quad: the whole canvas, because the scratch already
        // holds the layer where its transform put it. White, so that what the
        // chain produced reaches the bus unaltered, with the layer's opacity in
        // the alpha exactly as an uneffected layer carries it.
        //
        // The tint belongs here and not on the source: a layer on M is dimmed
        // by the master as it is composited, and dimming what went *into* the
        // chain would be an effect whose result changed as a fader moved.
        let mut composite = Quad::whole_canvas();
        composite.colour = [tint, tint, tint, quad.colour[3]];
        composite.blend = quad.blend;
        composite.layer_id = quad.layer_id;
        // SAFETY: as above.
        unsafe { self.write_instance(*index, &composite, false) };

        // `index` is left on the composite instance rather than moved past it,
        // because the caller is the one that knows which descriptor set the
        // finished chain ends up in and so has to build that draw itself.
        Some(Chain {
            source,
            first,
            count: passes.len(),
        })
    }

    /// Adds a draw to the stretch a bus has open, opening one if it has none.
    fn extend_step(upload: &mut BusUpload, slot: usize, run: DrawRun) {
        if upload.steps[slot].is_empty() {
            upload.steps[slot].push(BusStep {
                chain: None,
                first: 0,
                count: 0,
            });
        }

        let Some(step) = upload.steps[slot].last_mut() else {
            return;
        };
        let runs = &mut upload.runs[slot];
        match runs.last_mut() {
            // Only within the stretch that is open: merging into a draw from an
            // earlier one would put it on the wrong side of a chain.
            Some(last)
                if step.count > 0 && last.set == run.set && last.pipeline == run.pipeline =>
            {
                last.count += 1;
            }
            _ => {
                runs.push(run);
                step.count += 1;
            }
        }
    }

    /// Opens a new stretch on a bus, preceded by the chain it composites.
    fn begin_step(upload: &mut BusUpload, slot: usize, chain: Chain, run: DrawRun) {
        let first = upload.runs[slot].len();
        upload.runs[slot].push(run);
        upload.steps[slot].push(BusStep {
            chain: Some(chain),
            first,
            count: 1,
        });
    }

    /// Says once that a blend mode cannot be drawn as configured.
    fn warn_about_blend(&mut self, unsupported: Option<BlendMode>) {
        if let Some(mode) = unsupported {
            if !self.warned_about_blend {
                tracing::warn!(
                    ?mode,
                    "this device has no VK_EXT_blend_operation_advanced, and core Vulkan cannot express this mode; drawing it as Normal"
                );
                self.warned_about_blend = true;
            }
        }
    }

    /// Writes the scene into the mapped instance buffer, in draw order.
    ///
    /// Returns one [`DrawRun`] per run of consecutive quads sharing a texture.
    /// Consecutive is the point: layer order is composition order, so merging
    /// only neighbours keeps what is drawn over what, and a show whose layers
    /// alternate between two clips still costs one draw call per layer rather
    /// than one per quad. The scene's quad count is capped at [`MAX_QUADS`].
    fn upload(&mut self, scene: &Scene) -> Vec<DrawRun> {
        let quads = scene.quads();
        let count = quads.len().min(MAX_QUADS);
        if quads.len() > MAX_QUADS {
            tracing::warn!(
                wanted = quads.len(),
                drawn = MAX_QUADS,
                "the composition has more quads than the instance buffer holds"
            );
        }

        let mut runs: Vec<DrawRun> = Vec::new();
        let mut unsupported_blend = None;
        for (index, quad) in quads.iter().take(count).enumerate() {
            // SAFETY: `index` is below `count`, which is at most `MAX_QUADS`.
            unsafe { self.write_instance(index, quad, quad.texture.is_bus()) };

            let set = self.set_for(quad.texture);
            let pipeline = self.pipelines[blend::index_of(quad.blend)];
            if !blend::is_supported(quad.blend, self.advanced_blend) {
                unsupported_blend = Some(quad.blend);
            }

            match runs.last_mut() {
                Some(run) if run.set == set && run.pipeline == pipeline => run.count += 1,
                _ => runs.push(DrawRun {
                    set,
                    pipeline,
                    first: u32::try_from(index).unwrap_or(u32::MAX),
                    count: 1,
                }),
            }
        }

        // Once per process, not once per frame. A mode that cannot be drawn as
        // configured is drawn as Normal, and an operator who is never told would
        // spend the evening wondering why Overlay looks like Normal.
        if let Some(mode) = unsupported_blend {
            if !self.warned_about_blend {
                tracing::warn!(
                    ?mode,
                    "this device has no VK_EXT_blend_operation_advanced, and core Vulkan cannot express this mode; drawing it as Normal"
                );
                self.warned_about_blend = true;
            }
        }

        runs
    }

    /// Loads one SPIR-V module.
    fn create_shader_module(
        device: &ash::Device,
        spirv: &[u8],
    ) -> Result<vk::ShaderModule, VulkanError> {
        // Vulkan wants 32-bit words. The build script writes little-endian
        // bytes, so they are reassembled rather than transmuted: a byte slice
        // from `include_bytes!` carries no alignment guarantee.
        let (words, remainder) = spirv.as_chunks::<4>();
        if !remainder.is_empty() {
            return Err(VulkanError::Call {
                operation: "a compiled shader is not a whole number of SPIR-V words",
                result: vk::Result::ERROR_INITIALIZATION_FAILED,
            });
        }
        let words: Vec<u32> = words.iter().copied().map(u32::from_le_bytes).collect();

        let info = vk::ShaderModuleCreateInfo::default().code(&words);
        // SAFETY: `words` outlives the call and `info` borrows it.
        unsafe { device.create_shader_module(&info, None) }.map_err(|result| VulkanError::Call {
            operation: "vkCreateShaderModule",
            result,
        })
    }

    /// Describes what the fragment shader binds: three images and a sampler.
    ///
    /// Separate bindings rather than combined image samplers, because that is
    /// how WGSL models them, and matching the shader beats matching a
    /// convention.
    ///
    /// Three images because a decoded video frame is three planes. An RGBA
    /// texture names its own image for all three and the shader is told not to
    /// read the other two, which costs a descriptor write and keeps one layout,
    /// one pipeline and one shader for both kinds of picture.
    fn create_descriptor_layout(
        device: &ash::Device,
    ) -> Result<vk::DescriptorSetLayout, VulkanError> {
        let image = |binding| {
            vk::DescriptorSetLayoutBinding::default()
                .binding(binding)
                .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::FRAGMENT)
        };
        let bindings = [
            image(0),
            vk::DescriptorSetLayoutBinding::default()
                .binding(1)
                .descriptor_type(vk::DescriptorType::SAMPLER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::FRAGMENT),
            image(2),
            image(3),
        ];
        let info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);

        // SAFETY: `bindings` outlives the call and `info` borrows it.
        unsafe { device.create_descriptor_set_layout(&info, None) }.map_err(|result| {
            VulkanError::Call {
                operation: "vkCreateDescriptorSetLayout",
                result,
            }
        })
    }

    /// Creates the pool every texture's descriptor set is allocated from.
    ///
    /// Vulkan pools cannot grow, so the ceiling is explicit here rather than a
    /// surprise on the night a show uses one texture too many.
    ///
    /// `FREE_DESCRIPTOR_SET`, so that a set can be given back when its texture
    /// is dropped. Without it a pool only ever hands sets out: a show that
    /// imports and deletes media over an evening reaches the ceiling not by
    /// holding too many textures at once but by having held them one after
    /// another, and every upload after that fails — which an operator sees as
    /// a layer that has silently stopped playing and gone to its placeholder.
    /// The flag costs a pool that cannot use the simplest allocator, which is
    /// nothing next to that.
    fn create_descriptor_pool(device: &ash::Device) -> Result<vk::DescriptorPool, VulkanError> {
        // Every set the target can hold at once: the textures and the white
        // default, and the images it draws into and samples itself — the
        // buses, the scratch and the echoes' memories. Those used to be left
        // out, so a canvas that had made its buses and scratch could not hold
        // its last five textures.
        let count = u32::try_from(MAX_TEXTURES + 1 + BUS_COUNT + SCRATCH_COUNT + MAX_ECHOES)
            .unwrap_or(u32::MAX);
        let sizes = [
            vk::DescriptorPoolSize::default()
                .ty(vk::DescriptorType::SAMPLED_IMAGE)
                // Three a set: luma and two colour planes, or one image named
                // three times for an RGBA texture.
                .descriptor_count(count.saturating_mul(3)),
            vk::DescriptorPoolSize::default()
                .ty(vk::DescriptorType::SAMPLER)
                .descriptor_count(count),
        ];
        let info = vk::DescriptorPoolCreateInfo::default()
            .flags(vk::DescriptorPoolCreateFlags::FREE_DESCRIPTOR_SET)
            .pool_sizes(&sizes)
            .max_sets(count);

        // SAFETY: `sizes` outlives the call and `info` borrows it.
        unsafe { device.create_descriptor_pool(&info, None) }.map_err(|result| VulkanError::Call {
            operation: "vkCreateDescriptorPool",
            result,
        })
    }

    /// Allocates a descriptor set, pointing at nothing yet.
    fn allocate_empty_set(
        device: &ash::Device,
        pool: vk::DescriptorPool,
        layout: vk::DescriptorSetLayout,
    ) -> Result<vk::DescriptorSet, VulkanError> {
        let layouts = [layout];
        let allocate = vk::DescriptorSetAllocateInfo::default()
            .descriptor_pool(pool)
            .set_layouts(&layouts);
        // SAFETY: `layouts` outlives the call; a pool that is full returns an
        // error rather than misbehaving.
        let sets = unsafe { device.allocate_descriptor_sets(&allocate) }.map_err(|result| {
            VulkanError::Call {
                operation: "vkAllocateDescriptorSets",
                result,
            }
        })?;
        sets.first().copied().ok_or(VulkanError::Call {
            operation: "vkAllocateDescriptorSets returned nothing",
            result: vk::Result::ERROR_INITIALIZATION_FAILED,
        })
    }

    /// Allocates a descriptor set and points it at one texture.
    fn allocate_set(
        device: &ash::Device,
        pool: vk::DescriptorPool,
        layout: vk::DescriptorSetLayout,
        texture: &Texture,
    ) -> Result<vk::DescriptorSet, VulkanError> {
        let layouts = [layout];
        let allocate = vk::DescriptorSetAllocateInfo::default()
            .descriptor_pool(pool)
            .set_layouts(&layouts);
        // SAFETY: `layouts` outlives the call; a pool that is full returns an
        // error rather than misbehaving.
        let sets = unsafe { device.allocate_descriptor_sets(&allocate) }.map_err(|result| {
            VulkanError::Call {
                operation: "vkAllocateDescriptorSets",
                result,
            }
        })?;
        let set = *sets.first().ok_or(VulkanError::Call {
            operation: "vkAllocateDescriptorSets returned nothing",
            result: vk::Result::ERROR_INITIALIZATION_FAILED,
        })?;

        point_set_at(device, set, texture);
        Ok(set)
    }

    /// Builds the graphics pipeline the canvas draws with.    /// Builds the graphics pipeline the canvas draws with.
    fn create_pipelines(
        device: &ash::Device,
        render_pass: vk::RenderPass,
        shader: vk::ShaderModule,
        descriptor_layout: vk::DescriptorSetLayout,
        size: vk::Extent2D,
        advanced: bool,
    ) -> Result<(vk::PipelineLayout, [vk::Pipeline; blend::BLEND_MODE_COUNT]), VulkanError> {
        let set_layouts = [descriptor_layout];
        let layout_info = vk::PipelineLayoutCreateInfo::default().set_layouts(&set_layouts);
        // SAFETY: `layout_info` is fully initialised.
        let layout =
            unsafe { device.create_pipeline_layout(&layout_info, None) }.map_err(|result| {
                VulkanError::Call {
                    operation: "vkCreatePipelineLayout",
                    result,
                }
            })?;

        let pipelines =
            Self::build_pipelines(device, render_pass, shader, layout, size, advanced, false)?;
        Ok((layout, pipelines))
    }

    /// Builds the pipelines a bus is drawn with: the same set, writing alpha.
    fn create_bus_pipelines(
        device: &ash::Device,
        render_pass: vk::RenderPass,
        shader: vk::ShaderModule,
        layout: vk::PipelineLayout,
        size: vk::Extent2D,
        advanced: bool,
    ) -> Result<[vk::Pipeline; blend::BLEND_MODE_COUNT], VulkanError> {
        Self::build_pipelines(device, render_pass, shader, layout, size, advanced, true)
    }

    /// Builds one pipeline per blend mode against a render pass.
    ///
    /// `writes_alpha` is the only difference between the composition's set and
    /// a bus's: a bus keeps coverage, and the composition is opaque.
    fn build_pipelines(
        device: &ash::Device,
        render_pass: vk::RenderPass,
        shader: vk::ShaderModule,
        layout: vk::PipelineLayout,
        size: vk::Extent2D,
        advanced: bool,
        writes_alpha: bool,
    ) -> Result<[vk::Pipeline; blend::BLEND_MODE_COUNT], VulkanError> {
        // Both stages come from one module, selected by entry point name.
        let stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(shader)
                .name(c"vertex_main"),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(shader)
                .name(c"fragment_main"),
        ];

        let bindings = Self::instance_bindings();
        let attributes = Self::instance_attributes();
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::default()
            .vertex_binding_descriptions(&bindings)
            .vertex_attribute_descriptions(&attributes);
        let assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
            .topology(vk::PrimitiveTopology::TRIANGLE_LIST);

        // A plain, positive viewport. naga already emits the Y conversion when
        // it lowers WGSL to SPIR-V, so +Y is up in shader source without a
        // viewport flip; adding one here would apply the conversion twice and
        // put every wall back upside down. That is a property of the toolchain
        // rather than of Vulkan, so `the_coordinate_convention_is_y_up` asserts
        // it against a rendered frame rather than trusting this comment.
        let viewports = [vk::Viewport::default()
            .width(size.width as f32)
            .height(size.height as f32)
            .max_depth(1.0)];
        let scissors = [vk::Rect2D::default().extent(size)];
        let viewport_state = vk::PipelineViewportStateCreateInfo::default()
            .viewports(&viewports)
            .scissors(&scissors);

        let rasteriser = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(vk::PolygonMode::FILL)
            // No culling: a piece may legitimately be mirrored by its transform,
            // and culling would make that silently disappear.
            .cull_mode(vk::CullModeFlags::NONE)
            .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
            .line_width(1.0);

        let multisample = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1);

        // One pipeline per blend mode, differing only here. Built in one call
        // so the driver sees them together, and in `ALL_BLEND_MODES` order so
        // the result indexes the same way the table does.
        let attachments: Vec<[vk::PipelineColorBlendAttachmentState; 1]> = blend::ALL_BLEND_MODES
            .into_iter()
            .map(|mode| {
                [if writes_alpha {
                    blend::attachment_for_bus(mode, advanced)
                } else {
                    blend::attachment_for(mode, advanced)
                }]
            })
            .collect();

        // Both sides premultiplied: the shader hands over premultiplied colour,
        // and the composition is opaque by construction. Telling the blender
        // otherwise would make it divide by an alpha that is already accounted
        // for, which is a dimmer picture rather than an error.
        let mut advanced_states: Vec<vk::PipelineColorBlendAdvancedStateCreateInfoEXT<'_>> =
            blend::ALL_BLEND_MODES
                .into_iter()
                .map(|_| {
                    vk::PipelineColorBlendAdvancedStateCreateInfoEXT::default()
                        .src_premultiplied(true)
                        .dst_premultiplied(true)
                        .blend_overlap(vk::BlendOverlapEXT::UNCORRELATED)
                })
                .collect();

        let blends: Vec<vk::PipelineColorBlendStateCreateInfo<'_>> = attachments
            .iter()
            .zip(advanced_states.iter_mut())
            .zip(blend::ALL_BLEND_MODES)
            .map(|((attachment, advanced_state), mode)| {
                let state =
                    vk::PipelineColorBlendStateCreateInfo::default().attachments(attachment);
                if advanced && blend::advanced_op(mode).is_some() {
                    state.push_next(advanced_state)
                } else {
                    state
                }
            })
            .collect();

        let infos: Vec<vk::GraphicsPipelineCreateInfo<'_>> = blends
            .iter()
            .map(|blend_state| {
                vk::GraphicsPipelineCreateInfo::default()
                    .stages(&stages)
                    .vertex_input_state(&vertex_input)
                    .input_assembly_state(&assembly)
                    .viewport_state(&viewport_state)
                    .rasterization_state(&rasteriser)
                    .multisample_state(&multisample)
                    .color_blend_state(blend_state)
                    .layout(layout)
                    .render_pass(render_pass)
                    .subpass(0)
            })
            .collect();

        // SAFETY: every borrowed slice outlives the call, and the render pass
        // and shader modules are live.
        let created =
            unsafe { device.create_graphics_pipelines(vk::PipelineCache::null(), &infos, None) }
                .map_err(|(_, result)| VulkanError::Call {
                    operation: "vkCreateGraphicsPipelines",
                    result,
                })?;

        created.try_into().map_err(|_| VulkanError::Call {
            operation: "vkCreateGraphicsPipelines returned the wrong number of pipelines",
            result: vk::Result::ERROR_INITIALIZATION_FAILED,
        })
    }

    /// One binding, stepped per instance.
    ///
    /// The quad's six vertices come from the vertex index; only the rectangles
    /// and the colour vary per quad.
    fn instance_bindings() -> [vk::VertexInputBindingDescription; 1] {
        [vk::VertexInputBindingDescription::default()
            .binding(0)
            .stride(std::mem::size_of::<InstanceData>() as u32)
            .input_rate(vk::VertexInputRate::INSTANCE)]
    }

    /// What one instance holds, as the vertex shader's inputs.
    fn instance_attributes() -> [vk::VertexInputAttributeDescription; 9] {
        [
            vk::VertexInputAttributeDescription::default()
                .location(0)
                .binding(0)
                .format(vk::Format::R32G32B32A32_SFLOAT)
                .offset(0),
            vk::VertexInputAttributeDescription::default()
                .location(1)
                .binding(0)
                .format(vk::Format::R32G32B32A32_SFLOAT)
                .offset(std::mem::size_of::<[f32; 4]>() as u32),
            vk::VertexInputAttributeDescription::default()
                .location(2)
                .binding(0)
                .format(vk::Format::R32G32B32A32_SFLOAT)
                .offset(2 * std::mem::size_of::<[f32; 4]>() as u32),
            vk::VertexInputAttributeDescription::default()
                .location(4)
                .binding(0)
                .format(vk::Format::R32G32B32A32_SFLOAT)
                .offset(3 * std::mem::size_of::<[f32; 4]>() as u32),
            vk::VertexInputAttributeDescription::default()
                .location(3)
                .binding(0)
                .format(vk::Format::R32_SFLOAT)
                .offset(4 * std::mem::size_of::<[f32; 4]>() as u32),
            vk::VertexInputAttributeDescription::default()
                .location(5)
                .binding(0)
                .format(vk::Format::R32_SFLOAT)
                .offset(
                    4 * std::mem::size_of::<[f32; 4]>() as u32 + std::mem::size_of::<f32>() as u32,
                ),
            vk::VertexInputAttributeDescription::default()
                .location(6)
                .binding(0)
                .format(vk::Format::R32_SFLOAT)
                .offset(
                    4 * std::mem::size_of::<[f32; 4]>() as u32
                        + 2 * std::mem::size_of::<f32>() as u32,
                ),
            vk::VertexInputAttributeDescription::default()
                .location(7)
                .binding(0)
                .format(vk::Format::R32_SFLOAT)
                .offset(
                    4 * std::mem::size_of::<[f32; 4]>() as u32
                        + 3 * std::mem::size_of::<f32>() as u32,
                ),
            vk::VertexInputAttributeDescription::default()
                .location(8)
                .binding(0)
                .format(vk::Format::R32G32B32A32_SFLOAT)
                .offset(
                    4 * std::mem::size_of::<[f32; 4]>() as u32
                        + 4 * std::mem::size_of::<f32>() as u32,
                ),
        ]
    }

    /// Builds the pipeline a layer's own picture reaches the scratch through.
    ///
    /// The composition shader, unblended, writing alpha. Unblended because the
    /// scratch was just cleared and exactly one quad is drawn into it, so there
    /// is nothing underneath to blend against; alpha because a chain has to
    /// know where the layer is and where it is not, exactly as a bus does.
    fn create_scratch_source_pipeline(
        device: &ash::Device,
        render_pass: vk::RenderPass,
        shader: vk::ShaderModule,
        layout: vk::PipelineLayout,
        size: vk::Extent2D,
    ) -> Result<vk::Pipeline, VulkanError> {
        let stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(shader)
                .name(c"vertex_main"),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(shader)
                .name(c"fragment_main"),
        ];

        let bindings = Self::instance_bindings();
        let attributes = Self::instance_attributes();
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::default()
            .vertex_binding_descriptions(&bindings)
            .vertex_attribute_descriptions(&attributes);

        Self::build_one_pipeline(device, render_pass, &stages, &vertex_input, layout, size)
    }

    /// Builds the pipeline every effect pass is drawn with, and its layout.
    ///
    /// No vertex input at all: the full-screen triangle comes from the vertex
    /// index, so there is nothing per-instance to feed it. The push constant
    /// range is what carries the pass — which effect it is, what it was set to,
    /// and how big one composition pixel is.
    fn create_pass_pipeline(
        device: &ash::Device,
        render_pass: vk::RenderPass,
        shader: vk::ShaderModule,
        descriptor_layout: vk::DescriptorSetLayout,
        size: vk::Extent2D,
    ) -> Result<(vk::PipelineLayout, vk::Pipeline), VulkanError> {
        // Twice, of the same shape. The second is the picture a mask pass
        // samples; every other pass binds the white default there and never
        // reads it. One layout rather than two because a texture's descriptor
        // set is already written against this one, so a mask is bound by naming
        // the set the compositor already holds for it.
        let set_layouts = [descriptor_layout, descriptor_layout];
        // Both stages, although only the fragment shader reads it: a range that
        // covers a stage which does not use it is legal and costs nothing,
        // while one that misses a stage which does is a pipeline the driver is
        // entitled to refuse.
        let ranges = [vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT)
            .offset(0)
            .size(std::mem::size_of::<EffectPush>() as u32)];
        let layout_info = vk::PipelineLayoutCreateInfo::default()
            .set_layouts(&set_layouts)
            .push_constant_ranges(&ranges);
        // SAFETY: every borrowed slice outlives the call.
        let layout =
            unsafe { device.create_pipeline_layout(&layout_info, None) }.map_err(|result| {
                VulkanError::Call {
                    operation: "vkCreatePipelineLayout for an effect pass",
                    result,
                }
            })?;

        let stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(shader)
                .name(c"vertex_main"),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(shader)
                .name(c"fragment_main"),
        ];
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::default();

        let pipeline =
            Self::build_one_pipeline(device, render_pass, &stages, &vertex_input, layout, size)?;
        Ok((layout, pipeline))
    }

    /// Builds one pipeline that replaces whatever it draws over.
    ///
    /// No blending and every channel written: both of the passes that use this
    /// fill an image from nothing, so what comes out of the fragment shader is
    /// what the image holds. That is also what keeps alpha straight through a
    /// chain, since a blend would premultiply it.
    fn build_one_pipeline(
        device: &ash::Device,
        render_pass: vk::RenderPass,
        stages: &[vk::PipelineShaderStageCreateInfo<'_>],
        vertex_input: &vk::PipelineVertexInputStateCreateInfo<'_>,
        layout: vk::PipelineLayout,
        size: vk::Extent2D,
    ) -> Result<vk::Pipeline, VulkanError> {
        let assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
            .topology(vk::PrimitiveTopology::TRIANGLE_LIST);

        let viewports = [vk::Viewport::default()
            .width(size.width as f32)
            .height(size.height as f32)
            .max_depth(1.0)];
        let scissors = [vk::Rect2D::default().extent(size)];
        let viewport_state = vk::PipelineViewportStateCreateInfo::default()
            .viewports(&viewports)
            .scissors(&scissors);

        let rasteriser = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::NONE)
            .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
            .line_width(1.0);

        let multisample = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1);

        let attachments = [vk::PipelineColorBlendAttachmentState::default()
            .color_write_mask(
                vk::ColorComponentFlags::R
                    | vk::ColorComponentFlags::G
                    | vk::ColorComponentFlags::B
                    | vk::ColorComponentFlags::A,
            )
            .blend_enable(false)];
        let blend_state =
            vk::PipelineColorBlendStateCreateInfo::default().attachments(&attachments);

        let infos = [vk::GraphicsPipelineCreateInfo::default()
            .stages(stages)
            .vertex_input_state(vertex_input)
            .input_assembly_state(&assembly)
            .viewport_state(&viewport_state)
            .rasterization_state(&rasteriser)
            .multisample_state(&multisample)
            .color_blend_state(&blend_state)
            .layout(layout)
            .render_pass(render_pass)
            .subpass(0)];

        // SAFETY: every borrowed slice outlives the call, and the render pass
        // and shader module are live.
        let created =
            unsafe { device.create_graphics_pipelines(vk::PipelineCache::null(), &infos, None) }
                .map_err(|(_, result)| VulkanError::Call {
                    operation: "vkCreateGraphicsPipelines",
                    result,
                })?;

        created.first().copied().ok_or(VulkanError::Call {
            operation: "vkCreateGraphicsPipelines returned nothing",
            result: vk::Result::ERROR_INITIALIZATION_FAILED,
        })
    }

    fn create_render_pass(device: &ash::Device) -> Result<vk::RenderPass, VulkanError> {
        let attachment = vk::AttachmentDescription::default()
            .format(COMPOSITION_FORMAT)
            .samples(vk::SampleCountFlags::TYPE_1)
            .load_op(vk::AttachmentLoadOp::CLEAR)
            .store_op(vk::AttachmentStoreOp::STORE)
            .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
            .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            // Ends ready to be copied out, which is what a readback and a
            // present both need next.
            .final_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL);

        let colour_reference = vk::AttachmentReference::default()
            .attachment(0)
            .layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
        let colour_references = [colour_reference];

        let subpass = vk::SubpassDescription::default()
            .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
            .color_attachments(&colour_references);
        let subpasses = [subpass];

        // Both transitions, said out loud.
        //
        // A render pass that declares neither gets implicit dependencies, and
        // the outgoing implicit one ends at `BOTTOM_OF_PIPE` with an empty
        // access mask: it makes the colour writes available and visible to
        // nothing, and no barrier recorded after `vkCmdEndRenderPass` can
        // repair that, because the transition it needs to be ordered against
        // happened inside the pass.
        //
        // The result was a real hazard, reported by synchronisation validation
        // every frame a preview was being read back: `vkCmdBlitImage` reading
        // an image written by the transition `vkCmdEndRenderPass` performed
        //. This driver tolerated it. One that did not would show a
        // torn or a stale preview — the worst kind of fault here, because a
        // preview is what an operator checks the wall against.
        let dependencies = [
            // Last frame's readback has finished before this frame's clear
            // overwrites what it was copying.
            vk::SubpassDependency::default()
                .src_subpass(vk::SUBPASS_EXTERNAL)
                .dst_subpass(0)
                .src_stage_mask(
                    vk::PipelineStageFlags::TRANSFER | vk::PipelineStageFlags::FRAGMENT_SHADER,
                )
                .src_access_mask(vk::AccessFlags::TRANSFER_READ | vk::AccessFlags::SHADER_READ)
                .dst_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
                .dst_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE),
            // And this frame, including the transition into
            // `TRANSFER_SRC_OPTIMAL`, is finished and visible before anything
            // copies it out. Naming the transfer stage on this side is what
            // puts the transition itself in a scope the readback can be
            // ordered against.
            vk::SubpassDependency::default()
                .src_subpass(0)
                .dst_subpass(vk::SUBPASS_EXTERNAL)
                .src_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
                .src_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)
                .dst_stage_mask(vk::PipelineStageFlags::TRANSFER)
                .dst_access_mask(vk::AccessFlags::TRANSFER_READ | vk::AccessFlags::TRANSFER_WRITE),
        ];

        let attachments = [attachment];
        let info = vk::RenderPassCreateInfo::default()
            .attachments(&attachments)
            .subpasses(&subpasses)
            .dependencies(&dependencies);

        // SAFETY: every borrowed slice outlives the call.
        unsafe { device.create_render_pass(&info, None) }.map_err(|result| VulkanError::Call {
            operation: "vkCreateRenderPass",
            result,
        })
    }

    /// Changes how big the picture the card keeps is.
    ///
    /// Everything that looks at a surface without being the surface — a
    /// thumbnail on the System screen, a stream to a client, a test reading one
    /// pixel — reads this picture rather than the frame itself. Production
    /// keeps it at 720p or below, which is what makes a preview cost a
    /// megabyte a frame instead of eight; a test that wants to compare a
    /// rendered pixel byte for byte sets it to the frame's own size.
    ///
    /// Rare enough to be allowed to be expensive: it waits for the device and
    /// rebuilds two objects.
    ///
    /// # Errors
    ///
    /// Returns [`VulkanError::Call`] naming the Vulkan call that failed. The
    /// old picture is kept when the new one cannot be made.
    pub fn set_preview_size(
        &mut self,
        context: &VulkanContext,
        size: SizePx,
    ) -> Result<(), VulkanError> {
        // As asked for, not through the default ceiling — but never larger
        // than the frame, because a preview cannot have detail the frame does
        // not, and never odd, because every encoder downstream is 4:2:0.
        let wanted = vk::Extent2D {
            width: (size.width.clamp(2, self.size.width.max(2))) & !1,
            height: (size.height.clamp(2, self.size.height.max(2))) & !1,
        };
        if wanted == self.preview_size {
            return Ok(());
        }

        let device = context.device();
        // SAFETY: waiting for everything this device is doing, so nothing below
        // is in use when it is destroyed.
        unsafe { device.device_wait_idle() }.map_err(|result| VulkanError::Call {
            operation: "vkDeviceWaitIdle",
            result,
        })?;

        // SAFETY: the physical device belongs to this context's instance.
        let memory_properties = unsafe {
            context
                .instance_ref()
                .get_physical_device_memory_properties(context.physical_device())
        };

        let (image, memory) = Self::create_preview_image(context, &memory_properties, wanted)?;
        let (readback, readback_memory) =
            Self::create_readback_buffer(context, &memory_properties, wanted)?;

        // SAFETY: the device is idle and these are this target's own objects.
        unsafe {
            device.destroy_image(self.preview_image, None);
            device.free_memory(self.preview_memory, None);
            device.destroy_buffer(self.readback, None);
            device.free_memory(self.readback_memory, None);
        }

        self.preview_image = image;
        self.preview_memory = memory;
        self.readback = readback;
        self.readback_memory = readback_memory;
        self.preview_size = wanted;
        Ok(())
    }

    /// The size of the picture the card keeps for anybody looking on.
    #[must_use]
    pub const fn preview_size(&self) -> SizePx {
        SizePx::new(self.preview_size.width, self.preview_size.height)
    }

    /// Creates the small image the finished frame is shrunk into.
    fn create_preview_image(
        context: &VulkanContext,
        memory_properties: &vk::PhysicalDeviceMemoryProperties,
        size: vk::Extent2D,
    ) -> Result<(vk::Image, vk::DeviceMemory), VulkanError> {
        let device = context.device();

        let info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            // Eight-bit RGBA whatever the composition is made of, and not
            // `COMPOSITION_FORMAT`. This is a tap for somebody watching a
            // browser, encoded as JPEG at a fraction of the size; the blit that
            // fills it converts, and precision nobody can see is not worth the
            // bus.
            .format(PREVIEW_FORMAT)
            .extent(vk::Extent3D {
                width: size.width,
                height: size.height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            // Blitted into, then copied out of.
            .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::TRANSFER_SRC)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);

        // SAFETY: `info` is fully initialised and outlives the call.
        let image =
            unsafe { device.create_image(&info, None) }.map_err(|result| VulkanError::Call {
                operation: "vkCreateImage",
                result,
            })?;

        // SAFETY: `image` was just created on this device.
        let requirements = unsafe { device.get_image_memory_requirements(image) };
        let memory_type = find_memory_type(
            memory_properties,
            requirements.memory_type_bits,
            vk::MemoryPropertyFlags::DEVICE_LOCAL,
        )
        .ok_or(VulkanError::Call {
            operation: "finding device memory for the preview image",
            result: vk::Result::ERROR_FEATURE_NOT_PRESENT,
        })?;

        let allocate = vk::MemoryAllocateInfo::default()
            .allocation_size(requirements.size)
            .memory_type_index(memory_type);
        // SAFETY: the size and type come from this image's own requirements.
        let memory = unsafe { device.allocate_memory(&allocate, None) }.map_err(|result| {
            VulkanError::Call {
                operation: "vkAllocateMemory",
                result,
            }
        })?;
        // SAFETY: the memory was allocated for this image and nothing is bound
        // to it yet.
        unsafe { device.bind_image_memory(image, memory, 0) }.map_err(|result| {
            VulkanError::Call {
                operation: "vkBindImageMemory",
                result,
            }
        })?;

        Ok((image, memory))
    }

    fn create_readback_buffer(
        context: &VulkanContext,
        memory_properties: &vk::PhysicalDeviceMemoryProperties,
        size: vk::Extent2D,
    ) -> Result<(vk::Buffer, vk::DeviceMemory), VulkanError> {
        let device = context.device();
        let bytes = u64::from(size.width) * u64::from(size.height) * 4;

        let info = vk::BufferCreateInfo::default()
            .size(bytes)
            .usage(vk::BufferUsageFlags::TRANSFER_DST)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        // SAFETY: `info` is fully initialised.
        let buffer =
            unsafe { device.create_buffer(&info, None) }.map_err(|result| VulkanError::Call {
                operation: "vkCreateBuffer",
                result,
            })?;

        // SAFETY: `buffer` was just created on this device.
        let requirements = unsafe { device.get_buffer_memory_requirements(buffer) };

        // Cached first, and this matters more than it looks. Host-visible
        // device memory is typically write-combined: excellent to write, and so
        // slow to *read* that pulling a 1080p frame out of it costs several
        // frames of render time. This buffer exists to be read, so cached
        // memory is asked for first and the write-combined kind is the fallback
        // for a device that has nothing better.
        let memory_type = find_memory_type(
            memory_properties,
            requirements.memory_type_bits,
            vk::MemoryPropertyFlags::HOST_VISIBLE
                | vk::MemoryPropertyFlags::HOST_COHERENT
                | vk::MemoryPropertyFlags::HOST_CACHED,
        )
        .or_else(|| {
            find_memory_type(
                memory_properties,
                requirements.memory_type_bits,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
            )
        })
        .ok_or(VulkanError::Call {
            operation: "finding host-visible memory for readback",
            result: vk::Result::ERROR_FEATURE_NOT_PRESENT,
        })?;

        let allocate = vk::MemoryAllocateInfo::default()
            .allocation_size(requirements.size)
            .memory_type_index(memory_type);
        // SAFETY: `allocate` is fully initialised.
        let memory = unsafe { device.allocate_memory(&allocate, None) }.map_err(|result| {
            VulkanError::Call {
                operation: "vkAllocateMemory",
                result,
            }
        })?;
        // SAFETY: allocated for exactly this buffer's requirements.
        unsafe { device.bind_buffer_memory(buffer, memory, 0) }.map_err(|result| {
            VulkanError::Call {
                operation: "vkBindBufferMemory",
                result,
            }
        })?;

        Ok((buffer, memory))
    }

    /// Renders one frame and waits for the GPU to finish it.
    ///
    /// Waiting is deliberate here. This backend exists to be measured and read
    /// back, so a frame that has not finished is not a frame.
    ///
    /// # Errors
    ///
    /// Returns [`VulkanError::Call`] naming what failed.
    pub fn render(&mut self, context: &VulkanContext, scene: &Scene) -> Result<(), VulkanError> {
        let device = context.device();
        let fences = [self.fence];

        // SAFETY: the fence belongs to this device and was created signalled.
        unsafe {
            device
                .wait_for_fences(
                    &fences,
                    true,
                    FENCE_TIMEOUT.as_nanos().min(u128::from(u64::MAX)) as u64,
                )
                .map_err(|result| VulkanError::Call {
                    operation: "vkWaitForFences",
                    result,
                })?;
            device
                .reset_fences(&fences)
                .map_err(|result| VulkanError::Call {
                    operation: "vkResetFences",
                    result,
                })?;
        }

        // Whatever was handed over since the last frame, copied into mapped
        // memory now that the card has finished reading it.
        self.flush_media_textures(context)?;

        // Written after the fence wait: the GPU has finished reading last
        // frame's instances, so overwriting them now cannot race it.
        let runs = self.upload(scene);
        self.last_quad_count = scene.len();
        // Onto the card on its own queue, before the drawing is recorded.
        self.submit_transfers(context)?;
        self.record(device, &runs)?;

        // The copies are submitted, so the pixels are the GPU's problem.
        self.default_texture.dirty = false;
        for bound in self.textures.values_mut() {
            bound.dirty = false;
        }

        let command_buffers = [self.command_buffer];
        let waits = [self.uploaded];
        let stages = [vk::PipelineStageFlags::TOP_OF_PIPE];
        let submit = vk::SubmitInfo::default()
            .command_buffers(&command_buffers)
            .wait_semaphores(&waits)
            .wait_dst_stage_mask(&stages);
        let submits = [submit];

        // SAFETY: the command buffer has been recorded and is not in use; the
        // fence was just reset.
        unsafe { device.queue_submit(context.graphics_queue(), &submits, self.fence) }.map_err(
            |result| VulkanError::Call {
                operation: "vkQueueSubmit",
                result,
            },
        )?;

        // SAFETY: the fence was passed to the submit above.
        unsafe {
            device
                .wait_for_fences(
                    &fences,
                    true,
                    FENCE_TIMEOUT.as_nanos().min(u128::from(u64::MAX)) as u64,
                )
                .map_err(|result| VulkanError::Call {
                    operation: "vkWaitForFences",
                    result,
                })
        }
    }

    /// Composites a show: each bus into its own image, then the combine.
    ///
    /// The difference from [`render`](Self::render) is the whole of
    /// owner decision Q8. A and B are mixed by the crossfader — a true dissolve
    /// between two finished pictures, so the middle of a fade is both at half
    /// rather than one at half of what is left of the other — and M carries the
    /// result, untouched by the crossfader.
    ///
    /// `Carries`, not `goes over`: a layer routed to M composites onto the
    /// crossfaded pair with its own blend mode, so `Add` on M adds to the show.
    /// M's image is seeded with A and B for exactly that reason
    /// (`upload_buses`, owner 2026-09-23). The master dimmer scales the whole
    /// of it once.
    ///
    /// # Errors
    ///
    /// Returns [`VulkanError::Call`] naming what failed.
    pub fn compose(&mut self, context: &VulkanContext, scene: &Scene) -> Result<(), VulkanError> {
        self.ensure_buses(context)?;
        if scene.has_effects() {
            // Only here, and only once. A show with no effects in it never
            // allocates a scratch image, which is what makes an empty chain
            // cost nothing rather than nearly nothing.
            self.ensure_scratch(context)?;
        }

        let device = context.device();
        self.wait_for_frame(device)?;

        // After the fence wait, so the last frame has finished with every
        // memory that is about to be cleared or handed to another echo.
        self.composed = self.composed.wrapping_add(1);
        if scene.has_effects() {
            self.ensure_histories(context, scene);
        }

        // After the fence wait, because the copy the last frame submitted has
        // finished reading these buffers only now. Before the recording, because
        // the copies onto the card go into the command buffer below.
        self.flush_media_textures(context)?;

        // Written after the fence wait: the GPU has finished reading last
        // frame's instances, so overwriting them now cannot race it.
        self.upload_buses(scene);
        // Dated by what was really drawn rather than by what the scene asked
        // for: a layer on a bus at nought is in the scene and is not drawn, and
        // its echo has to start again when it comes back.
        let composed = self.composed;
        for kept in self.upload.keeps.iter().flatten() {
            if let Some(history) = self.histories.get_mut(*kept) {
                history.drawn_in = composed;
            }
        }
        // The copies go to their own queue before the drawing is recorded, so
        // the card is moving pixels while the processor is still writing the
        // command buffer that will read them.
        self.submit_transfers(context)?;
        self.record_buses(device)?;
        self.mark_uploaded();
        self.submit_and_wait(context)
    }

    /// Waits for the previous frame and resets the fence.
    fn wait_for_frame(&self, device: &ash::Device) -> Result<(), VulkanError> {
        let fences = [self.fence];
        // SAFETY: the fence belongs to this device and was created signalled.
        unsafe {
            device
                .wait_for_fences(
                    &fences,
                    true,
                    FENCE_TIMEOUT.as_nanos().min(u128::from(u64::MAX)) as u64,
                )
                .map_err(|result| VulkanError::Call {
                    operation: "vkWaitForFences",
                    result,
                })?;
            device
                .reset_fences(&fences)
                .map_err(|result| VulkanError::Call {
                    operation: "vkResetFences",
                    result,
                })
        }
    }

    /// Notes that the staged texture copies are now the GPU's problem.
    fn mark_uploaded(&mut self) {
        // The generators were recorded into this frame's command buffer, so
        // what is left here is spent. Cleared rather than kept: a generator
        // that was not asked for again this pass has not changed, and its
        // texture still holds what the last pass drew.
        self.pending_generated.clear();
        self.default_texture.dirty = false;
        for bound in self.textures.values_mut() {
            bound.dirty = false;
        }
        // The clears were recorded into this frame too.
        for history in &mut self.histories {
            history.clear = false;
        }
    }

    /// Submits the recorded command buffer and waits for it.
    fn submit_and_wait(&self, context: &VulkanContext) -> Result<(), VulkanError> {
        let device = context.device();
        let command_buffers = [self.command_buffer];
        let waits = [self.uploaded];
        // At the top, not at the fragment shader. The acquire barrier this
        // buffer starts with is the first thing in it, and a semaphore waited
        // on later than the barrier that depends on it is a race — the sort
        // that draws the frame before last on one machine and nothing at all
        // on the next.
        let stages = [vk::PipelineStageFlags::TOP_OF_PIPE];
        let submit = vk::SubmitInfo::default()
            .command_buffers(&command_buffers)
            .wait_semaphores(&waits)
            .wait_dst_stage_mask(&stages);
        let submits = [submit];
        let fences = [self.fence];

        // SAFETY: the command buffer has been recorded and is not in use; the
        // fence was just reset.
        unsafe { device.queue_submit(context.graphics_queue(), &submits, self.fence) }.map_err(
            |result| VulkanError::Call {
                operation: "vkQueueSubmit",
                result,
            },
        )?;

        // SAFETY: the fence was passed to the submit above.
        unsafe {
            device
                .wait_for_fences(
                    &fences,
                    true,
                    FENCE_TIMEOUT.as_nanos().min(u128::from(u64::MAX)) as u64,
                )
                .map_err(|result| VulkanError::Call {
                    operation: "vkWaitForFences",
                    result,
                })
        }
    }

    /// Records the three bus passes, the chains they need, and the combine.
    ///
    /// A bus is one render pass when nothing on it has effects, which is the
    /// whole of what this did before chains existed. A layer with a chain
    /// breaks its bus into stretches: the pass closes, the chain runs into its
    /// own images, and the bus is reopened to composite the result over what
    /// the earlier layers left.
    fn record_buses(&self, device: &ash::Device) -> Result<(), VulkanError> {
        let buses = self.buses.as_ref().ok_or(VulkanError::Call {
            operation: "composing without buses",
            result: vk::Result::ERROR_INITIALIZATION_FAILED,
        })?;

        self.begin_recording(device)?;
        self.record_generated(device);
        self.record_history_clears(device);

        let transparent = vk::ClearValue {
            color: vk::ClearColorValue { float32: BUS_CLEAR },
        };
        let bus_clear = [transparent];

        for (slot, image) in buses.images.iter().enumerate() {
            // Nothing to draw, and nothing samples it: the combine quad for an
            // empty bus is not there either.
            let mut opened = false;

            for step in &self.upload.steps[slot] {
                if let Some(chain) = step.chain {
                    // Outside the bus's render pass, because a render pass
                    // cannot begin inside another one.
                    self.record_chain(device, &chain)?;
                }

                let begin = vk::RenderPassBeginInfo::default()
                    .render_pass(if opened {
                        buses.reopened
                    } else {
                        buses.render_pass
                    })
                    .framebuffer(image.framebuffer)
                    .render_area(vk::Rect2D {
                        offset: vk::Offset2D { x: 0, y: 0 },
                        extent: self.size,
                    })
                    .clear_values(&bus_clear);

                let runs = &self.upload.runs[slot][step.first..step.first + step.count];

                // SAFETY: recording is in progress and every handle is live.
                unsafe {
                    device.cmd_begin_render_pass(
                        self.command_buffer,
                        &begin,
                        vk::SubpassContents::INLINE,
                    );
                    self.record_runs(device, runs);
                    device.cmd_end_render_pass(self.command_buffer);
                }
                opened = true;
            }
        }

        let opaque = vk::ClearValue {
            color: vk::ClearColorValue {
                float32: CLEAR_COLOUR,
            },
        };
        let clear_values = [opaque];
        let begin = vk::RenderPassBeginInfo::default()
            .render_pass(self.render_pass)
            .framebuffer(self.framebuffer)
            .render_area(vk::Rect2D {
                offset: vk::Offset2D { x: 0, y: 0 },
                extent: self.size,
            })
            .clear_values(&clear_values);

        // SAFETY: as above; the bus images are in SHADER_READ_ONLY_OPTIMAL by
        // their render pass's final layout, and its subpass dependency makes
        // the writes visible to this pass.
        unsafe {
            device.cmd_begin_render_pass(self.command_buffer, &begin, vk::SubpassContents::INLINE);
            self.record_runs(device, &self.upload.combine);
            device.cmd_end_render_pass(self.command_buffer);
        }

        self.record_readback(device);
        self.end_recording(device)
    }

    /// Records one layer's effect chain: its picture, then every pass.
    ///
    /// The picture goes into the first scratch image and the passes ping-pong
    /// from there, so a chain of any length costs two images. Each pass is its
    /// own render pass because each one reads the whole of what the one before
    /// it wrote: a pixel here depends on its neighbours, which is exactly what
    /// a subpass may not do.
    fn record_chain(&self, device: &ash::Device, chain: &Chain) -> Result<(), VulkanError> {
        let scratch = self.scratch.as_ref().ok_or(VulkanError::Call {
            operation: "running an effect chain with no scratch image",
            result: vk::Result::ERROR_INITIALIZATION_FAILED,
        })?;

        let transparent = vk::ClearValue {
            color: vk::ClearColorValue { float32: BUS_CLEAR },
        };
        let clear = [transparent];
        let area = vk::Rect2D {
            offset: vk::Offset2D { x: 0, y: 0 },
            extent: self.size,
        };

        let begin = vk::RenderPassBeginInfo::default()
            .render_pass(scratch.render_pass)
            .framebuffer(scratch.images[0].framebuffer)
            .render_area(area)
            .clear_values(&clear);

        // SAFETY: recording is in progress, no render pass is open, and every
        // handle came from this target.
        unsafe {
            device.cmd_begin_render_pass(self.command_buffer, &begin, vk::SubpassContents::INLINE);
            device.cmd_bind_vertex_buffers(self.command_buffer, 0, &[self.instances], &[0]);
            device.cmd_bind_pipeline(
                self.command_buffer,
                vk::PipelineBindPoint::GRAPHICS,
                scratch.source,
            );
            device.cmd_bind_descriptor_sets(
                self.command_buffer,
                vk::PipelineBindPoint::GRAPHICS,
                self.pipeline_layout,
                0,
                &[chain.source.set],
                &[],
            );
            device.cmd_draw(
                self.command_buffer,
                VERTICES_PER_QUAD,
                1,
                0,
                chain.source.first,
            );
            device.cmd_end_render_pass(self.command_buffer);
        }

        let passes = &self.upload.passes[chain.first..chain.first + chain.count];
        let masks = &self.upload.masks[chain.first..chain.first + chain.count];
        for (step, push) in passes.iter().enumerate() {
            let from = step % SCRATCH_COUNT;
            let into = (step + 1) % SCRATCH_COUNT;

            let begin = vk::RenderPassBeginInfo::default()
                .render_pass(scratch.render_pass)
                .framebuffer(scratch.images[into].framebuffer)
                .render_area(area)
                .clear_values(&clear);

            // SAFETY: as above. The image being read was left in
            // SHADER_READ_ONLY_OPTIMAL by the pass that wrote it, and the
            // render pass's dependencies order the read against this write.
            unsafe {
                device.cmd_begin_render_pass(
                    self.command_buffer,
                    &begin,
                    vk::SubpassContents::INLINE,
                );
                device.cmd_bind_pipeline(
                    self.command_buffer,
                    vk::PipelineBindPoint::GRAPHICS,
                    scratch.pass,
                );
                device.cmd_bind_descriptor_sets(
                    self.command_buffer,
                    vk::PipelineBindPoint::GRAPHICS,
                    scratch.pass_layout,
                    0,
                    &[scratch.images[from].set, masks[step]],
                    &[],
                );
                device.cmd_push_constants(
                    self.command_buffer,
                    scratch.pass_layout,
                    vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                    0,
                    push.bytes(),
                );
                device.cmd_draw(self.command_buffer, VERTICES_PER_FULL_SCREEN, 1, 0, 0);
                device.cmd_end_render_pass(self.command_buffer);
            }

            // An echo's result goes into its memory as well as on down the
            // chain, for the next frame to read. A pass of its own rather than
            // a second attachment, because the memory is what this pass just
            // read: an image cannot be sampled and drawn into at once. The
            // chain carries on from `into` as before, so the ping-pong does
            // not notice.
            if let Some(history) = self
                .upload
                .keeps
                .get(chain.first + step)
                .copied()
                .flatten()
                .and_then(|at| self.histories.get(at))
            {
                self.record_keep(device, scratch, scratch.images[into].set, history);
            }
        }

        Ok(())
    }

    /// Copies a finished echo into its memory.
    fn record_keep(
        &self,
        device: &ash::Device,
        scratch: &Scratch,
        from: vk::DescriptorSet,
        history: &History,
    ) {
        let clear = [vk::ClearValue {
            color: vk::ClearColorValue { float32: BUS_CLEAR },
        }];
        let begin = vk::RenderPassBeginInfo::default()
            .render_pass(scratch.render_pass)
            .framebuffer(history.image.framebuffer)
            .render_area(vk::Rect2D {
                offset: vk::Offset2D { x: 0, y: 0 },
                extent: self.size,
            })
            .clear_values(&clear);
        let texel = [
            1.0 / self.size.width.max(1) as f32,
            1.0 / self.size.height.max(1) as f32,
        ];
        let push = EffectPush::keeping(texel);

        // SAFETY: recording is in progress, no render pass is open, and every
        // handle came from this target. The memory was last read by the echo
        // pass just recorded; the render pass's own dependency orders that
        // read before this write.
        unsafe {
            device.cmd_begin_render_pass(self.command_buffer, &begin, vk::SubpassContents::INLINE);
            device.cmd_bind_pipeline(
                self.command_buffer,
                vk::PipelineBindPoint::GRAPHICS,
                scratch.pass,
            );
            device.cmd_bind_descriptor_sets(
                self.command_buffer,
                vk::PipelineBindPoint::GRAPHICS,
                scratch.pass_layout,
                0,
                &[from, self.default_texture.set],
                &[],
            );
            device.cmd_push_constants(
                self.command_buffer,
                scratch.pass_layout,
                vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                0,
                push.bytes(),
            );
            device.cmd_draw(self.command_buffer, VERTICES_PER_FULL_SCREEN, 1, 0, 0);
            device.cmd_end_render_pass(self.command_buffer);
        }
    }

    /// Clears the memories that have to start again from nothing.
    ///
    /// Before any chain, outside any render pass, and in this frame's own
    /// command buffer, so the clear cannot race the echo that reads it. A
    /// render pass that clears and draws nothing, which is also what leaves the
    /// image in the layout the echo samples it in.
    fn record_history_clears(&self, device: &ash::Device) {
        let Some(scratch) = self.scratch.as_ref() else {
            return;
        };
        let clear = [vk::ClearValue {
            color: vk::ClearColorValue { float32: BUS_CLEAR },
        }];
        for history in self.histories.iter().filter(|history| history.clear) {
            let begin = vk::RenderPassBeginInfo::default()
                .render_pass(scratch.render_pass)
                .framebuffer(history.image.framebuffer)
                .render_area(vk::Rect2D {
                    offset: vk::Offset2D { x: 0, y: 0 },
                    extent: self.size,
                })
                .clear_values(&clear);
            // SAFETY: recording is in progress, no render pass is open, and the
            // framebuffer was made against this render pass.
            unsafe {
                device.cmd_begin_render_pass(
                    self.command_buffer,
                    &begin,
                    vk::SubpassContents::INLINE,
                );
                device.cmd_end_render_pass(self.command_buffer);
            }
        }
    }

    /// Records the frame: clear the canvas, then copy it out for readback.
    /// Opens the command buffer and stages whatever textures changed.
    ///
    /// The texture copies go in before any render pass begins: a copy cannot be
    /// issued inside one, and recording it here rather than as a separate
    /// submit means the upload cannot race the draw that samples it.
    fn begin_recording(&self, device: &ash::Device) -> Result<(), VulkanError> {
        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        // SAFETY: the buffer is not in use; the fence wait guaranteed that.
        unsafe { device.begin_command_buffer(self.command_buffer, &begin) }.map_err(|result| {
            VulkanError::Call {
                operation: "vkBeginCommandBuffer",
                result,
            }
        })?;

        // The copies themselves went to the transfer queue. What is owed here
        // is taking ownership of what it wrote — and only when it was a queue
        // of its own, because an image does not change hands between a family
        // and itself.
        if let Some(families) = self.ownership() {
            if self.default_texture.dirty {
                self.default_texture
                    .texture
                    .record_acquire(device, self.command_buffer, families);
            }
            for bound in self.textures.values() {
                if bound.dirty {
                    bound
                        .texture
                        .record_acquire(device, self.command_buffer, families);
                }
            }
        }
        Ok(())
    }

    /// The family pair an uploaded image passes between, or `None` when the
    /// copies run on the queue that draws.
    const fn ownership(&self) -> Option<(u32, u32)> {
        if self.transfer_family == self.graphics_family {
            None
        } else {
            Some((self.transfer_family, self.graphics_family))
        }
    }

    /// Records this frame's copies onto the card and submits them.
    ///
    /// Always submits, even with nothing to copy: the graphics submit waits on
    /// [`uploaded`](Self::uploaded), and a binary semaphore nobody signalled is
    /// a frame that never starts.
    ///
    /// # Errors
    ///
    /// Returns [`VulkanError::Call`] naming what failed.
    fn submit_transfers(&self, context: &VulkanContext) -> Result<(), VulkanError> {
        let device = context.device();
        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        // SAFETY: the buffer is not in use; the fence wait guaranteed that,
        // because the graphics submit that waited on this one has completed.
        unsafe { device.begin_command_buffer(self.transfer_buffer, &begin) }.map_err(|result| {
            VulkanError::Call {
                operation: "vkBeginCommandBuffer for uploads",
                result,
            }
        })?;

        let families = self.ownership();
        if self.default_texture.dirty {
            self.default_texture
                .texture
                .record_upload(device, self.transfer_buffer, families);
        }
        for bound in self.textures.values() {
            if bound.dirty {
                bound
                    .texture
                    .record_upload(device, self.transfer_buffer, families);
            }
        }

        // SAFETY: recording began above and nothing else writes this buffer.
        unsafe { device.end_command_buffer(self.transfer_buffer) }.map_err(|result| {
            VulkanError::Call {
                operation: "vkEndCommandBuffer for uploads",
                result,
            }
        })?;

        let command_buffers = [self.transfer_buffer];
        let signals = [self.uploaded];
        let submit = vk::SubmitInfo::default()
            .command_buffers(&command_buffers)
            .signal_semaphores(&signals);
        let submits = [submit];
        // SAFETY: the buffer has been recorded and the semaphore belongs to
        // this device.
        unsafe { device.queue_submit(context.transfer_queue(), &submits, vk::Fence::null()) }
            .map_err(|result| VulkanError::Call {
                operation: "vkQueueSubmit for uploads",
                result,
            })
    }

    /// Records the draws of one pass, inside a render pass that is open.
    ///
    /// One draw per run of quads sharing a texture and a blend mode, at an
    /// instance offset into the buffer the whole frame already sits in. No runs
    /// means no draws at all, and the attachment keeps its clear.
    fn record_runs(&self, device: &ash::Device, runs: &[DrawRun]) {
        if runs.is_empty() {
            return;
        }

        // SAFETY: recording is in progress, a render pass is open, and every
        // handle in the runs came from this target.
        unsafe {
            device.cmd_bind_vertex_buffers(self.command_buffer, 0, &[self.instances], &[0]);
            for run in runs {
                device.cmd_bind_pipeline(
                    self.command_buffer,
                    vk::PipelineBindPoint::GRAPHICS,
                    run.pipeline,
                );
                device.cmd_bind_descriptor_sets(
                    self.command_buffer,
                    vk::PipelineBindPoint::GRAPHICS,
                    self.pipeline_layout,
                    0,
                    &[run.set],
                    &[],
                );
                device.cmd_draw(
                    self.command_buffer,
                    VERTICES_PER_QUAD,
                    run.count,
                    0,
                    run.first,
                );
            }
        }
    }

    fn record(&self, device: &ash::Device, runs: &[DrawRun]) -> Result<(), VulkanError> {
        self.begin_recording(device)?;

        let clear = vk::ClearValue {
            color: vk::ClearColorValue {
                float32: CLEAR_COLOUR,
            },
        };
        let clear_values = [clear];
        let render_pass_begin = vk::RenderPassBeginInfo::default()
            .render_pass(self.render_pass)
            .framebuffer(self.framebuffer)
            .render_area(vk::Rect2D {
                offset: vk::Offset2D { x: 0, y: 0 },
                extent: self.size,
            })
            .clear_values(&clear_values);

        // SAFETY: recording is in progress and every handle is live.
        unsafe {
            device.cmd_begin_render_pass(
                self.command_buffer,
                &render_pass_begin,
                vk::SubpassContents::INLINE,
            );
            self.record_runs(device, runs);
            device.cmd_end_render_pass(self.command_buffer);
        }

        self.record_readback(device);
        self.end_recording(device)
    }

    /// Copies the finished image out and leaves it ready to be sampled.
    ///
    /// The render pass left it in `TRANSFER_SRC_OPTIMAL`, so the copy needs no
    /// transition; the barrier afterwards is what lets an output sample this
    /// frame. A target nobody samples pays one barrier for it, which is nothing
    /// next to the copy.
    ///
    /// # What orders this against the pass that drew the frame
    ///
    /// Not a barrier here — it cannot be. The thing the blit races is the
    /// render pass's *own* final layout transition, which happens inside the
    /// pass at the scope of its outgoing subpass dependency; a barrier recorded
    /// afterwards has no way to name it. So the ordering is declared where the
    /// transition is, on the render pass (`create_render_pass`), and the blit
    /// below simply inherits it.
    fn record_readback(&self, device: &ash::Device) {
        let region = vk::BufferImageCopy::default()
            .image_subresource(
                vk::ImageSubresourceLayers::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .layer_count(1),
            )
            .image_extent(vk::Extent3D {
                width: self.preview_size.width,
                height: self.preview_size.height,
                depth: 1,
            });
        let regions = [region];

        let layers = vk::ImageSubresourceLayers::default()
            .aspect_mask(vk::ImageAspectFlags::COLOR)
            .layer_count(1);
        let blit = vk::ImageBlit::default()
            .src_subresource(layers)
            .src_offsets([
                vk::Offset3D::default(),
                vk::Offset3D {
                    x: self.size.width as i32,
                    y: self.size.height as i32,
                    z: 1,
                },
            ])
            .dst_subresource(layers)
            .dst_offsets([
                vk::Offset3D::default(),
                vk::Offset3D {
                    x: self.preview_size.width as i32,
                    y: self.preview_size.height as i32,
                    z: 1,
                },
            ]);
        let blits = [blit];

        let range = vk::ImageSubresourceRange::default()
            .aspect_mask(vk::ImageAspectFlags::COLOR)
            .level_count(1)
            .layer_count(1);
        // The small image is written from scratch every frame, so what it held
        // before is of no interest: `UNDEFINED` says so and lets the driver
        // skip preserving it.
        let to_blit_target = vk::ImageMemoryBarrier::default()
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(self.preview_image)
            .subresource_range(range)
            .src_access_mask(vk::AccessFlags::empty())
            .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE);
        let to_copy_source = vk::ImageMemoryBarrier::default()
            .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(self.preview_image)
            .subresource_range(range)
            .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .dst_access_mask(vk::AccessFlags::TRANSFER_READ);

        let whole = vk::ImageSubresourceRange::default()
            .aspect_mask(vk::ImageAspectFlags::COLOR)
            .level_count(1)
            .layer_count(1);

        // The render pass's final transition is ordered into the transfer
        // stage by the dependency `create_render_pass` declares, so this one
        // source covers both ways of arriving here: with a readback, where the
        // last thing to touch this image was the blit's read, and without one,
        // where it was that transition.
        let to_shader_read = vk::ImageMemoryBarrier::default()
            .old_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
            .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(self.image)
            .subresource_range(whole)
            .src_access_mask(vk::AccessFlags::TRANSFER_READ | vk::AccessFlags::TRANSFER_WRITE)
            .dst_access_mask(vk::AccessFlags::SHADER_READ);

        // SAFETY: recording is in progress; the image is in the layout named.
        unsafe {
            if self.readback_wanted {
                // Shrink on the card, then carry the small one. The filter is
                // linear, so this is an average rather than every nth pixel.
                device.cmd_pipeline_barrier(
                    self.command_buffer,
                    vk::PipelineStageFlags::TOP_OF_PIPE,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &[to_blit_target],
                );
                device.cmd_blit_image(
                    self.command_buffer,
                    self.image,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    self.preview_image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &blits,
                    vk::Filter::LINEAR,
                );
                device.cmd_pipeline_barrier(
                    self.command_buffer,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &[to_copy_source],
                );
                device.cmd_copy_image_to_buffer(
                    self.command_buffer,
                    self.preview_image,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    self.readback,
                    &regions,
                );
            }
            device.cmd_pipeline_barrier(
                self.command_buffer,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::FRAGMENT_SHADER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[to_shader_read],
            );
        }
    }

    /// Closes the command buffer.
    fn end_recording(&self, device: &ash::Device) -> Result<(), VulkanError> {
        // SAFETY: recording is in progress and every pass has been ended.
        unsafe { device.end_command_buffer(self.command_buffer) }.map_err(|result| {
            VulkanError::Call {
                operation: "vkEndCommandBuffer",
                result,
            }
        })
    }

    /// Gives one media item a texture, or replaces the one it has.
    ///
    /// `pixels` is tightly packed RGBA. A texture kept at the same size is
    /// replaced in place, so a video source that keeps its resolution allocates
    /// nothing per frame and only a resolution change is expensive — which is
    /// the right way round.
    ///
    /// # Errors
    ///
    /// Returns [`VulkanError`] when the pixels are not `width * height * 4`
    /// bytes, when the texture cannot be created, or when the canvas already
    /// holds [`MAX_TEXTURES`] distinct images.
    /// Stages several pictures at once, one thread per few.
    ///
    /// # Why this is not a loop over [`set_media_texture`](Self::set_media_texture)
    ///
    /// Copying a decoded picture into mapped memory is the single largest thing
    /// the render thread does at seven layers: measured with `ovs-upload-cost`
    /// on the test machine, 4628 us of an 11275 us frame, against 1539 us for
    /// the same twenty-two megabytes copied plainly in host memory. The
    /// difference is that this write goes across the bus, and one core does not
    /// fill a PCIe 4.0 link.
    ///
    /// Several cores do. The textures are distinct objects with distinct
    /// staging buffers, so nothing is shared between the threads but the
    /// pictures they read — and those are behind an `Arc` and never written.
    ///
    /// Creating or resizing a texture stays on this thread. It is rare, it
    /// touches the descriptor pool, and it can wait for the device to go idle.
    ///
    /// # Errors
    ///
    /// The first error any of them hit, with the rest still finished: a
    /// half-staged frame is a torn picture, and a thread that has started is
    /// cheaper to let finish than to unwind.
    pub fn queue_media_texture(&mut self, key: TextureKey, image: std::sync::Arc<DecodedImage>) {
        // The newest wins. A layer asked to show two frames in one pass is a
        // playhead that moved twice, and only the second is on screen.
        if let Some(waiting) = self.queued.iter_mut().find(|(queued, _)| *queued == key) {
            waiting.1 = image;
            return;
        }
        self.queued.push((key, image));
    }

    /// Copies every queued picture into the card's memory.
    ///
    /// # Errors
    ///
    /// The first error any thread hit.
    pub fn flush_media_textures(&mut self, context: &VulkanContext) -> Result<(), VulkanError> {
        if self.queued.is_empty() {
            return Ok(());
        }
        let pictures = std::mem::take(&mut self.queued);
        let outcome = self.stage_media_textures(context, &pictures);
        // Kept rather than dropped, so the allocation is made once and the
        // queue is a buffer rather than a per-frame `Vec`.
        self.queued = pictures;
        self.queued.clear();
        outcome
    }

    fn stage_media_textures(
        &mut self,
        context: &VulkanContext,
        pictures: &[(TextureKey, std::sync::Arc<DecodedImage>)],
    ) -> Result<(), VulkanError> {
        if pictures.len() < 2 {
            // One picture is one memcpy, and a thread costs more to start than
            // it saves. This is also every test and most shows.
            for (key, image) in pictures {
                self.set_media_texture(context, *key, image)?;
            }
            return Ok(());
        }

        // The shape first, on this thread, because it may destroy a texture a
        // frame in flight still holds.
        for (key, image) in pictures {
            self.prepare_media_texture(context, *key, image)?;
        }

        let wanted: std::collections::HashMap<TextureKey, &DecodedImage> = pictures
            .iter()
            .map(|(key, image)| (*key, image.as_ref()))
            .collect();
        let mut jobs: Vec<(&mut Texture, &DecodedImage)> = self
            .textures
            .iter_mut()
            .filter_map(|(key, bound)| {
                wanted.get(key).map(|image| {
                    bound.dirty = true;
                    (&mut bound.texture, *image)
                })
            })
            .collect();

        let threads = jobs.len().clamp(1, STAGING_THREADS);
        let each = jobs.len().div_ceil(threads);
        let mut failures = Vec::new();
        std::thread::scope(|scope| {
            let handles: Vec<_> = jobs
                .chunks_mut(each)
                .map(|chunk| {
                    scope.spawn(move || {
                        let mut first = Ok(());
                        for (texture, image) in chunk {
                            if let Err(error) = texture.stage_image(image) {
                                if first.is_ok() {
                                    first = Err(error);
                                }
                            }
                        }
                        first
                    })
                })
                .collect();
            for handle in handles {
                match handle.join() {
                    Ok(Err(error)) => failures.push(error),
                    Ok(Ok(())) => {}
                    Err(_) => failures.push(VulkanError::Call {
                        operation: "a staging thread panicked",
                        result: vk::Result::ERROR_UNKNOWN,
                    }),
                }
            }
        });

        failures.into_iter().next().map_or(Ok(()), Err)
    }

    /// Makes sure a texture of the right shape exists for this key, without
    /// putting anything in it.
    /// A texture with its picture already in it, or nothing left behind.
    ///
    /// Two allocations that can each fail — the images and the staging buffer
    /// on one side, copying the picture into them on the other — and a
    /// `Texture` has no `Drop`, because releasing a Vulkan object needs a
    /// device and a `Drop` cannot be handed one. So a `?` between the two is a
    /// leak, and on this renderer the staging buffer lives in **video** memory,
    /// which makes it a leak of the one resource a show cannot do without.
    ///
    /// That is not hypothetical. It is how a renderer came to be holding 7.9
    /// GiB of an 8 GiB card: the descriptor pool had filled, every upload after
    /// that failed *after* its texture had been built, and the render loop tried
    /// again the next frame.
    fn staged_texture(
        context: &VulkanContext,
        width: u32,
        height: u32,
        layout: TextureLayout,
        image: &DecodedImage,
    ) -> Result<Texture, VulkanError> {
        let mut texture = Texture::with_layout(context, width, height, layout)?;
        if let Err(error) = texture.stage_image(image) {
            texture.destroy(context.device());
            return Err(error);
        }
        Ok(texture)
    }

    fn prepare_media_texture(
        &mut self,
        context: &VulkanContext,
        key: TextureKey,
        image: &DecodedImage,
    ) -> Result<(), VulkanError> {
        let size = image.size();
        let layout = TextureLayout::of(image);
        let wanted = (size.width.max(1), size.height.max(1));
        let device = context.device();

        match self.textures.get_mut(&key) {
            Some(bound) if bound.texture.size() == wanted && bound.texture.layout() == layout => {
                Ok(())
            }
            Some(bound) => {
                // SAFETY: the texture about to be destroyed may still be
                // referenced by a frame in flight, so the device must be idle.
                unsafe {
                    let _ = device.device_wait_idle();
                }
                bound.release(device);
                // Left destroyed if this fails, which is why destroying twice
                // has to be harmless: the entry stays, and the next pass at
                // this key finds a texture of the wrong size and rebuilds it.
                bound.texture = Texture::with_layout(context, wanted.0, wanted.1, layout)?;
                point_set_at(device, bound.set, &bound.texture);
                Ok(())
            }
            None => {
                if self.textures.len() >= MAX_TEXTURES {
                    return Err(VulkanError::Call {
                        operation: "the canvas already holds as many textures as it can",
                        result: vk::Result::ERROR_OUT_OF_POOL_MEMORY,
                    });
                }
                let mut texture = Texture::with_layout(context, wanted.0, wanted.1, layout)?;
                let set = match Self::allocate_set(
                    device,
                    self.descriptor_pool,
                    self.descriptor_layout,
                    &texture,
                ) {
                    Ok(set) => set,
                    Err(error) => {
                        // The texture exists and nothing will ever point at it.
                        texture.destroy(device);
                        return Err(error);
                    }
                };
                self.textures.insert(
                    key,
                    Bound {
                        texture,
                        set,
                        framebuffer: None,
                        dirty: false,
                    },
                );
                Ok(())
            }
        }
    }

    /// Draws one generator into the texture at `key`, on the card.
    ///
    /// Nothing is copied: the picture is made where it is going to be looked
    /// at. The texture is an ordinary media texture in every way the
    /// composition cares about, so a layer showing a generator drawn here is
    /// indistinguishable downstream from one showing an uploaded picture.
    ///
    /// # Errors
    ///
    /// Returns [`VulkanError`] when the pipeline, the texture or its
    /// framebuffer cannot be made, or when the canvas already holds
    /// [`MAX_TEXTURES`] distinct images. The caller then draws the generator
    /// on the processor instead, which is slower and correct.
    pub fn set_generated(
        &mut self,
        context: &VulkanContext,
        key: impl Into<TextureKey>,
        size: ovs_types::geometry::SizePx,
        push: GeneratedPush,
        source: Option<TextureKey>,
    ) -> Result<(), VulkanError> {
        let key = key.into();
        let device = context.device();
        let width = size.width.max(1);
        let height = size.height.max(1);

        if self.generated.is_none() {
            self.generated = Some(GeneratedPipeline::new(
                device,
                GENERATOR_SHADER_SPIRV,
                self.descriptor_layout,
            )?);
        }
        let render_pass = self
            .generated
            .as_ref()
            .map(|pipeline| pipeline.render_pass)
            .ok_or(VulkanError::Call {
                operation: "a generator pipeline that was just made is missing",
                result: vk::Result::ERROR_INITIALIZATION_FAILED,
            })?;

        let wanted = (width, height);
        let rebuild = match self.textures.get(&key) {
            // The same picture as last pass: the texture and its framebuffer
            // are kept and only what is drawn into them changes.
            Some(bound) => {
                bound.framebuffer.is_none()
                    || bound.texture.size() != wanted
                    || bound.texture.layout() != TextureLayout::Rgba
            }
            None => true,
        };

        if rebuild {
            if self.textures.len() >= MAX_TEXTURES && !self.textures.contains_key(&key) {
                return Err(VulkanError::Call {
                    operation: "the canvas already holds as many textures as it can",
                    result: vk::Result::ERROR_OUT_OF_POOL_MEMORY,
                });
            }

            let texture = Texture::drawable(context, width, height)?;
            let framebuffer = match Self::create_generated_framebuffer(
                device,
                render_pass,
                &texture,
                width,
                height,
            ) {
                Ok(framebuffer) => framebuffer,
                Err(error) => {
                    let mut texture = texture;
                    texture.destroy(device);
                    return Err(error);
                }
            };

            match self.textures.get_mut(&key) {
                Some(bound) => {
                    // SAFETY: what is being replaced may still be referenced by
                    // a frame in flight, so the device must be idle first.
                    unsafe {
                        let _ = device.device_wait_idle();
                    }
                    if let Some(old) = bound.framebuffer.take() {
                        // SAFETY: idle, and nothing else holds it.
                        unsafe { device.destroy_framebuffer(old, None) };
                    }
                    bound.release(device);
                    bound.texture = texture;
                    bound.framebuffer = Some(framebuffer);
                    bound.dirty = false;
                    point_set_at(device, bound.set, &bound.texture);
                }
                None => {
                    let mut texture = texture;
                    let set = match Self::allocate_set(
                        device,
                        self.descriptor_pool,
                        self.descriptor_layout,
                        &texture,
                    ) {
                        Ok(set) => set,
                        Err(error) => {
                            texture.destroy(device);
                            // SAFETY: made above, pointed at by nothing.
                            unsafe { device.destroy_framebuffer(framebuffer, None) };
                            return Err(error);
                        }
                    };
                    self.textures.insert(
                        key,
                        Bound {
                            texture,
                            set,
                            framebuffer: Some(framebuffer),
                            dirty: false,
                        },
                    );
                }
            }
        }

        self.pending_generated.push((key, push, source));
        Ok(())
    }

    /// The framebuffer a generator is drawn into.
    fn create_generated_framebuffer(
        device: &ash::Device,
        render_pass: vk::RenderPass,
        texture: &Texture,
        width: u32,
        height: u32,
    ) -> Result<vk::Framebuffer, VulkanError> {
        let views = [texture.view()];
        let info = vk::FramebufferCreateInfo::default()
            .render_pass(render_pass)
            .attachments(&views)
            .width(width)
            .height(height)
            .layers(1);
        // SAFETY: the view outlives the call and belongs to this device.
        unsafe { device.create_framebuffer(&info, None) }.map_err(|result| VulkanError::Call {
            operation: "vkCreateFramebuffer for a generator",
            result,
        })
    }

    /// Records this pass's generators, before anything samples them.
    ///
    /// A generator whose texture or framebuffer went away between being asked
    /// for and being drawn is skipped rather than guessed at: the layer then
    /// shows what the last pass left, which is a held picture rather than a
    /// black one.
    fn record_generated(&self, device: &ash::Device) {
        let Some(pipeline) = self.generated.as_ref() else {
            return;
        };
        for (key, push, source) in &self.pending_generated {
            let Some(bound) = self.textures.get(key) else {
                continue;
            };
            let Some(framebuffer) = bound.framebuffer else {
                continue;
            };
            // What the pass reads, for the one kind that reads anything. A
            // kind with no picture of its own is handed the white default:
            // a set has to be bound for a layout that declares one, and the
            // shader never samples it.
            let set = match source {
                Some(from) => match self.textures.get(from) {
                    Some(bound) => bound.set,
                    // Its picture has not arrived yet. Skipped rather than
                    // drawn against the wrong texture: the layer then shows
                    // what the last pass left.
                    None => continue,
                },
                None => self.default_texture.set,
            };
            let (width, height) = bound.texture.size();
            // SAFETY: recording is in progress, no render pass is open, and
            // the framebuffer was made for this pipeline's render pass against
            // this texture at this size.
            unsafe {
                pipeline.draw(
                    device,
                    self.command_buffer,
                    framebuffer,
                    vk::Extent2D { width, height },
                    set,
                    push,
                );
            }
        }
    }

    /// Puts one picture in a texture at once, creating or resizing it first.
    ///
    /// The immediate path, for the pictures a show draws rarely: a still as it
    /// loads, a generator whose second has ticked, a test pattern. A clip's
    /// frames go through [`queue_media_texture`](Self::queue_media_texture)
    /// instead, because there are several of them in a pass and they are worth
    /// copying together.
    ///
    /// # Errors
    ///
    /// Returns [`VulkanError`] when the picture does not match the texture's
    /// layout, when a texture cannot be made, or when the canvas already holds
    /// [`MAX_TEXTURES`] distinct images.
    pub fn set_media_texture(
        &mut self,
        context: &VulkanContext,
        key: impl Into<TextureKey>,
        image: &DecodedImage,
    ) -> Result<(), VulkanError> {
        let key = key.into();
        let size = image.size();
        let layout = TextureLayout::of(image);
        let width = size.width;
        let height = size.height;
        let wanted = (width.max(1), height.max(1));
        let device = context.device();

        match self.textures.get_mut(&key) {
            // The same shape as last frame, which is what a playing clip is:
            // the texture is kept and only its bytes change.
            //
            // **`takes_uploads` is the third condition and the one that was
            // missing.** A generator drawn on the card leaves a texture of
            // exactly this size and layout under exactly this key, and that
            // texture has no staging buffer to push a picture through. A layer
            // switching from a card-drawn strobe to a solid colour uploaded
            // from the processor therefore matched here, staged into a mapping
            // one byte long, and killed the renderer — eighteen times on the
            // show machine on 2026-10-16 before it was reported. It falls
            // through to the rebuild below instead, which is what a change of
            // size or layout has always done.
            Some(bound)
                if bound.texture.size() == wanted
                    && bound.texture.layout() == layout
                    && bound.texture.takes_uploads() =>
            {
                bound.texture.stage_image(image)?;
                bound.dirty = true;
                return Ok(());
            }
            Some(bound) => {
                // SAFETY: the texture about to be destroyed may still be
                // referenced by a frame in flight, so the device must be idle.
                unsafe {
                    let _ = device.device_wait_idle();
                }
                bound.release(device);
                // As above: destroyed and left in place if the rebuild fails,
                // rather than a half-built texture nothing will free.
                bound.texture = Self::staged_texture(context, width, height, layout, image)?;
                bound.dirty = true;
                point_set_at(device, bound.set, &bound.texture);
                return Ok(());
            }
            None => {}
        }

        if self.textures.len() >= MAX_TEXTURES {
            return Err(VulkanError::Call {
                operation: "the canvas already holds as many textures as it can",
                result: vk::Result::ERROR_OUT_OF_POOL_MEMORY,
            });
        }

        let mut texture = Self::staged_texture(context, width, height, layout, image)?;
        let set = match Self::allocate_set(
            device,
            self.descriptor_pool,
            self.descriptor_layout,
            &texture,
        ) {
            Ok(set) => set,
            Err(error) => {
                texture.destroy(device);
                return Err(error);
            }
        };
        self.textures.insert(
            key,
            Bound {
                texture,
                set,
                framebuffer: None,
                dirty: true,
            },
        );
        Ok(())
    }

    /// Forgets a media item's texture, freeing its memory.
    ///
    /// Returns true when there was one.
    ///
    /// Its descriptor set goes back to the pool as well as its memory. That
    /// matters more than it sounds: a pool cannot grow, and one that only ever
    /// hands sets out is exhausted by a show that imports and deletes media
    /// over an evening — not by holding too many textures at once, but by
    /// having held them one after another. What an operator sees then is a
    /// layer that has silently stopped playing and gone to its placeholder.
    pub fn drop_media_texture(
        &mut self,
        context: &VulkanContext,
        key: impl Into<TextureKey>,
    ) -> bool {
        let Some(mut bound) = self.textures.remove(&key.into()) else {
            return false;
        };
        let device = context.device();
        // SAFETY: a frame in flight may still reference it.
        unsafe {
            let _ = device.device_wait_idle();
        }
        bound.release(device);
        // SAFETY: the set came from this pool, the pool was created with
        // `FREE_DESCRIPTOR_SET`, and the device is idle so nothing is reading
        // it. A failure here is not worth failing the drop over — the memory
        // is already back — so it is dropped.
        let _ = unsafe { device.free_descriptor_sets(self.descriptor_pool, &[bound.set]) };
        true
    }

    /// Returns the view of this target's image, for another target to sample.
    #[must_use]
    pub const fn view(&self) -> vk::ImageView {
        self.view
    }

    /// Returns the sampler to read this target's image with.
    #[must_use]
    pub const fn sampler(&self) -> vk::Sampler {
        self.sampler
    }

    /// Returns the views of this target's three buses, for another target.
    ///
    /// `None` for a target that has no buses — every output, and a compositor
    /// before the first frame that builds them.
    #[must_use]
    pub fn bus_views(&self) -> [Option<vk::ImageView>; BUS_COUNT] {
        match &self.buses {
            None => [None; BUS_COUNT],
            Some(buses) => std::array::from_fn(|index| Some(buses.images[index].view)),
        }
    }

    /// Points this target's bus quads at another target's bus images.
    ///
    /// A piece may take its rectangle out of a bus rather than out of the
    /// composition (owner, 2026-09-23), and a bus lives on the compositor. The
    /// two targets are on one device — a renderer opens one for the whole
    /// process — so an output samples the compositor's image through a set of
    /// its own.
    ///
    /// # Errors
    ///
    /// Returns [`VulkanError`] when a descriptor set cannot be allocated.
    pub fn bind_buses(
        &mut self,
        device: &ash::Device,
        views: [Option<vk::ImageView>; BUS_COUNT],
        sampler: vk::Sampler,
    ) -> Result<(), VulkanError> {
        for (index, view) in views.into_iter().enumerate() {
            let Some(view) = view else {
                continue;
            };
            let set = match self.bound_buses[index] {
                Some(set) => set,
                None => {
                    let set = Self::allocate_empty_set(
                        device,
                        self.descriptor_pool,
                        self.descriptor_layout,
                    )?;
                    self.bound_buses[index] = Some(set);
                    set
                }
            };
            point_set_at_image(device, set, view, sampler);
        }
        Ok(())
    }

    /// Points this target's composition quads at another target's image.
    ///
    /// The two must be on the same device: an image rendered on one device
    /// cannot be sampled by another, which is why a renderer opens one device
    /// for the whole process.
    ///
    /// # Errors
    ///
    /// Returns [`VulkanError`] when a descriptor set cannot be allocated.
    pub fn bind_composition(
        &mut self,
        device: &ash::Device,
        view: vk::ImageView,
        sampler: vk::Sampler,
    ) -> Result<(), VulkanError> {
        let set = match self.composition {
            Some(set) => set,
            None => {
                let set =
                    Self::allocate_empty_set(device, self.descriptor_pool, self.descriptor_layout)?;
                self.composition = Some(set);
                set
            }
        };

        point_set_at_image(device, set, view, sampler);
        Ok(())
    }

    /// Returns how many quads the last recorded frame contained.
    #[must_use]
    pub const fn last_quad_count(&self) -> usize {
        self.last_quad_count
    }

    /// Returns how many media textures the canvas holds.
    #[must_use]
    pub fn texture_count(&self) -> usize {
        self.textures.len()
    }

    /// Copies a scaled-down picture of the last frame into `into`.
    ///
    /// Tightly packed RGBA at `target`, sampled nearest-neighbour. Nearest
    /// rather than averaged because this is a preview an operator glances at to
    /// see *what* is on a wall, not to judge it: a box filter would cost several
    /// times the work for a difference nobody would use.
    ///
    /// The mapping happens once for the whole frame. Reading pixel by pixel
    /// through [`read_pixel`](Self::read_pixel) would map and unmap a hundred
    /// thousand times, which is a driver round trip per pixel.
    ///
    /// # Errors
    ///
    /// Returns [`VulkanError::Call`] when the readback buffer cannot be mapped.
    pub fn read_scaled(
        &self,
        context: &VulkanContext,
        target: SizePx,
        into: &mut Vec<u8>,
    ) -> Result<(), VulkanError> {
        let width = target.width.max(1);
        let height = target.height.max(1);
        let device = context.device();
        let source_width = self.preview_size.width;
        let source_height = self.preview_size.height;
        let frame_bytes = source_width as usize * source_height as usize * 4;

        // SAFETY: the memory is host-visible and host-coherent, nothing else
        // maps it, and the frame has completed.
        let mapped = unsafe {
            device.map_memory(
                self.readback_memory,
                0,
                vk::WHOLE_SIZE,
                vk::MemoryMapFlags::empty(),
            )
        }
        .map_err(|result| VulkanError::Call {
            operation: "vkMapMemory",
            result,
        })?;
        // SAFETY: the mapping covers the whole buffer, which holds the
        // preview picture and is exactly `frame_bytes` long; it stays mapped
        // until the unmap below, and nothing writes to it in between.
        let mapping = unsafe { std::slice::from_raw_parts(mapped.cast::<u8>(), frame_bytes) };

        // **The size it already is: one copy, straight out.** That is what a
        // preview stream asks for — `stream_size` and the card's own shrink
        // share one ceiling — and it used to go the long way round: a zeroed
        // copy of the whole picture, then the same picture again pixel by
        // pixel, two 64-bit divisions a pixel, nine hundred thousand times.
        // Measured on the development machine at 2,259 us a 720p read, on the
        // render thread, between composing a frame and showing it — once a
        // frame since a hardware encoder made the preview sixty a second, and
        // on the location server on 2026-10-18 that was a wall missing a
        // frame every few seconds whenever a client opened the preview.
        if width == source_width && height == source_height {
            into.clear();
            into.extend_from_slice(mapping);
            // SAFETY: the mapping was made above and `mapping` is not used
            // after this.
            unsafe { device.unmap_memory(self.readback_memory) };
            return Ok(());
        }

        // Another size: copied out in one pass before anything is sampled.
        // Reading the mapping directly, pixel by scattered pixel, is a separate
        // bus transaction each time; one sequential copy is what the hardware
        // is built for. On this machine that was the difference between 57 fps
        // and 60.
        let frame = mapping.to_vec();

        // SAFETY: the mapping was made above, and only the copy is read below.
        unsafe { device.unmap_memory(self.readback_memory) };

        // Sampled at the centre of each target pixel, so a scale that is not a
        // whole number does not bias the picture up and to the left. Where
        // each column comes from is the same on every row, so it is worked out
        // once rather than once a pixel.
        let centre = |index: u32, of: u32, from: u32| -> usize {
            let source = (u64::from(index) * 2 + 1) * u64::from(from) / (u64::from(of) * 2);
            (source as u32).min(from.saturating_sub(1)) as usize
        };
        let columns: Vec<usize> = (0..width)
            .map(|column| centre(column, width, source_width) * 4)
            .collect();

        into.clear();
        into.reserve(width as usize * height as usize * 4);
        for row in 0..height {
            let row_start = centre(row, height, source_height) * source_width as usize * 4;
            for &column in &columns {
                let offset = row_start + column;
                into.extend_from_slice(&frame[offset..offset + 4]);
            }
        }

        Ok(())
    }

    /// Reads one pixel of the last rendered frame, as `[r, g, b, a]`.
    ///
    /// This is what turns "the GPU did the work" into something checkable
    /// rather than assumed, and reading an arbitrary pixel is what makes
    /// rasterisation checkable rather than just execution.
    ///
    /// # Errors
    ///
    /// Returns [`VulkanError::Call`] when the coordinate is outside the canvas
    /// or the memory cannot be mapped.
    pub fn read_pixel(
        &self,
        context: &VulkanContext,
        x: u32,
        y: u32,
    ) -> Result<[u8; 4], VulkanError> {
        if x >= self.size.width || y >= self.size.height {
            return Err(VulkanError::Call {
                operation: "reading a pixel outside the canvas",
                result: vk::Result::ERROR_OUT_OF_DATE_KHR,
            });
        }

        let device = context.device();
        let offset = (y as usize * self.size.width as usize + x as usize) * 4;

        // Mapped from zero, then indexed. Vulkan requires a map offset to be a
        // multiple of `minMemoryMapAlignment`, so mapping at an arbitrary pixel
        // offset is undefined behaviour — which shows up as a readback that is
        // right at one canvas size and wrong at another.
        //
        // SAFETY: the memory is host-visible and host-coherent, nothing else
        // maps it, and the render above has completed.
        let mapped = unsafe {
            device.map_memory(
                self.readback_memory,
                0,
                vk::WHOLE_SIZE,
                vk::MemoryMapFlags::empty(),
            )
        }
        .map_err(|result| VulkanError::Call {
            operation: "vkMapMemory",
            result,
        })?;

        // SAFETY: the mapping covers the whole buffer, which holds
        // `width * height * 4` bytes, and `offset` was bounds-checked above.
        let pixel =
            unsafe { std::ptr::read_unaligned(mapped.cast::<u8>().add(offset).cast::<[u8; 4]>()) };

        // SAFETY: the mapping was made immediately above.
        unsafe { device.unmap_memory(self.readback_memory) };

        Ok(pixel)
    }

    /// Releases every Vulkan object, in reverse order of creation.
    fn destroy(&mut self, device: &ash::Device) {
        // The echoes' memories before the scratch, whose render pass their
        // framebuffers were made against.
        for history in self.histories.drain(..) {
            // SAFETY: the caller has waited for the device to be idle. The
            // descriptor sets go with the pool, below.
            unsafe {
                device.destroy_framebuffer(history.image.framebuffer, None);
                device.destroy_image_view(history.image.view, None);
                device.destroy_image(history.image.image, None);
                device.free_memory(history.image.memory, None);
            }
        }

        // The scratch first, for the same reason the buses go before the rest:
        // it borrows this target's descriptor pool and its composition shader.
        if let Some(scratch) = self.scratch.take() {
            // SAFETY: the caller has waited for the device to be idle, so
            // nothing below is in use. Descriptor sets are not freed
            // individually: the pool they came from is destroyed below.
            unsafe {
                device.destroy_pipeline(scratch.pass, None);
                device.destroy_pipeline_layout(scratch.pass_layout, None);
                device.destroy_pipeline(scratch.source, None);
                device.destroy_render_pass(scratch.render_pass, None);
                device.destroy_shader_module(scratch.shader, None);
                device.destroy_sampler(scratch.sampler, None);
                for image in scratch.images {
                    device.destroy_framebuffer(image.framebuffer, None);
                    device.destroy_image_view(image.view, None);
                    device.destroy_image(image.image, None);
                    device.free_memory(image.memory, None);
                }
            }
        }

        // The buses next: they borrow this target's pipeline layout and
        // descriptor pool, so they cannot outlive either.
        if let Some(buses) = self.buses.take() {
            // SAFETY: the caller has waited for the device to be idle, so
            // nothing below is in use. Descriptor sets are not freed
            // individually: the pool they came from is destroyed below.
            unsafe {
                for pipeline in buses.pipelines {
                    device.destroy_pipeline(pipeline, None);
                }
                device.destroy_render_pass(buses.render_pass, None);
                device.destroy_render_pass(buses.reopened, None);
                for image in buses.images {
                    device.destroy_framebuffer(image.framebuffer, None);
                    device.destroy_image_view(image.view, None);
                    device.destroy_image(image.image, None);
                    device.free_memory(image.memory, None);
                }
            }
        }

        // SAFETY: the caller has waited for the device to be idle.
        unsafe { device.destroy_sampler(self.sampler, None) };
        self.default_texture.texture.destroy(device);
        for bound in self.textures.values_mut() {
            bound.release(device);
        }
        self.textures.clear();
        if let Some(generated) = self.generated.as_mut() {
            // SAFETY: as above, and every framebuffer made against its render
            // pass has just been destroyed.
            unsafe { generated.destroy(device) };
        }
        self.generated = None;
        self.pending_generated.clear();
        // SAFETY: the caller has waited for the device to be idle, so nothing
        // below is in use.
        unsafe {
            device.unmap_memory(self.instances_memory);
            device.destroy_buffer(self.instances, None);
            device.free_memory(self.instances_memory, None);
            for pipeline in self.pipelines {
                device.destroy_pipeline(pipeline, None);
            }
            device.destroy_pipeline_layout(self.pipeline_layout, None);
            device.destroy_descriptor_pool(self.descriptor_pool, None);
            device.destroy_descriptor_set_layout(self.descriptor_layout, None);
            device.destroy_shader_module(self.shader, None);
            device.destroy_fence(self.fence, None);
            device.destroy_semaphore(self.uploaded, None);
            device.destroy_command_pool(self.transfer_pool, None);
            device.destroy_command_pool(self.command_pool, None);
            device.destroy_framebuffer(self.framebuffer, None);
            device.destroy_render_pass(self.render_pass, None);
            device.destroy_image_view(self.view, None);
            device.destroy_image(self.image, None);
            device.free_memory(self.memory, None);
            device.destroy_buffer(self.readback, None);
            device.free_memory(self.readback_memory, None);
            device.destroy_image(self.preview_image, None);
            device.free_memory(self.preview_memory, None);
        }
    }
}

/// An output backed by a real GPU, rendering offscreen.
///
/// Not a display yet. What it proves is the part that a display path cannot be
/// built without: that the device initialises, that a frame's worth of GPU work
/// is recorded, submitted and completed, and that the result is what was asked
/// for.
pub struct VulkanOffscreenBackend {
    /// The device this output draws on, shared with every other output.
    ///
    /// Shared and not owned, because a renderer process is one process on one
    /// machine with one GPU. A device per output would mean each wall's textures
    /// living in a separate allocation on the same card, and it would make the
    /// composition pass impossible: an image rendered on one device cannot be
    /// sampled by another.
    context: Arc<VulkanContext>,
    target: OffscreenTarget,
    id: OutputId,
    name: String,
    mode: DisplayMode,
}

impl VulkanOffscreenBackend {
    /// Opens a device of its own and creates a canvas of the mode's size.
    ///
    /// For a renderer driving several outputs, open one context and use
    /// [`on`](Self::on) instead; this is the single-output and single-test case.
    ///
    /// # Errors
    ///
    /// Returns [`VulkanError`] when Vulkan is unavailable or the canvas cannot
    /// be created.
    pub fn new(name: impl Into<String>, mode: DisplayMode) -> Result<Self, VulkanError> {
        Self::on(Arc::new(VulkanContext::new("ovs-renderer")?), name, mode)
    }

    /// Creates a canvas on a device that is already open.
    ///
    /// # Errors
    ///
    /// Returns [`VulkanError`] when the canvas cannot be created.
    pub fn on(
        context: Arc<VulkanContext>,
        name: impl Into<String>,
        mode: DisplayMode,
    ) -> Result<Self, VulkanError> {
        let name = name.into();
        let mut target = OffscreenTarget::new(
            &context,
            vk::Extent2D {
                width: mode.size.width,
                height: mode.size.height,
            },
        )?;
        // The rate this canvas was made for, until the render loop says what
        // the wall is really running at.
        let hertz = mode.refresh.as_f64();
        if hertz > 0.0 {
            target.effect_frame = (1.0 / hertz) as f32;
        }

        Ok(Self {
            context,
            target,
            id: OutputId::new(),
            name,
            mode,
        })
    }

    /// Returns the device this backend is running on.
    #[must_use]
    pub fn context(&self) -> &VulkanContext {
        &self.context
    }

    /// Gives one media item a texture, or replaces the one it has.
    ///
    /// # Errors
    ///
    /// Returns [`VulkanError`] when the picture's planes are the wrong size, the
    /// texture cannot be created, or the canvas is already full.
    pub fn set_media_texture(
        &mut self,
        key: impl Into<TextureKey>,
        image: &DecodedImage,
    ) -> Result<(), VulkanError> {
        self.target
            .set_media_texture(&self.context, key.into(), image)
    }

    /// Has the card draw a generator into one key's texture.
    ///
    /// Nothing crosses the bus: the picture is made where it will be looked
    /// at. It is drawn into this frame's command buffer, before anything
    /// samples it.
    ///
    /// # Errors
    ///
    /// Returns [`VulkanError`] when the pipeline, the texture or its
    /// framebuffer cannot be made, or the canvas is already full. The caller
    /// then draws the generator on the processor, which is slower and right.
    pub fn set_generated(
        &mut self,
        key: impl Into<TextureKey>,
        size: ovs_types::geometry::SizePx,
        push: GeneratedPush,
        source: Option<TextureKey>,
    ) -> Result<(), VulkanError> {
        self.target
            .set_generated(&self.context, key.into(), size, push, source)
    }

    /// Forgets a media item's texture, freeing its memory.
    ///
    /// Returns true when there was one.
    pub fn drop_media_texture(&mut self, key: impl Into<TextureKey>) -> bool {
        self.target.drop_media_texture(&self.context, key.into())
    }

    /// Turns a Vulkan failure into the right kind of backend failure.
    ///
    /// A lost device is permanent; anything else may well work next frame, so
    /// it is reported as a present failure and the loop counts a dropped frame
    /// rather than faulting the output.
    fn present_failure(&self, error: VulkanError) -> BackendError {
        let permanent = matches!(
            &error,
            VulkanError::Call { result, .. }
                if *result == vk::Result::ERROR_DEVICE_LOST
                    || *result == vk::Result::ERROR_SURFACE_LOST_KHR
        );
        if permanent {
            BackendError::OutputLost {
                name: self.name.clone(),
                reason: error.to_string(),
            }
        } else {
            BackendError::PresentFailed {
                name: self.name.clone(),
                reason: error.to_string(),
            }
        }
    }

    /// Returns this canvas's image, for whatever puts it on a panel.
    ///
    /// It is left in `TRANSFER_SRC_OPTIMAL` by its own render pass, so a blit
    /// out of it needs no barrier.
    #[must_use]
    pub(crate) const fn image(&self) -> vk::Image {
        self.target.image
    }

    /// Returns true when this canvas blends with the advanced operations.
    ///
    /// What decides whether Overlay and Difference are drawn as themselves. A
    /// device without the extension draws them as Normal and says so once.
    #[must_use]
    pub fn has_advanced_blend(&self) -> bool {
        self.context.has_advanced_blend()
    }

    /// Returns how many media textures this output holds.
    #[must_use]
    pub fn texture_count(&self) -> usize {
        self.target.texture_count()
    }

    /// Returns true when this canvas has allocated an effect chain's scratch.
    ///
    /// "A layer with no effects costs nothing extra" is a claim about what was
    /// allocated as much as about what was drawn, and this is how a test says
    /// the first half of it.
    #[cfg(test)]
    pub(crate) const fn has_scratch(&self) -> bool {
        self.target.has_scratch()
    }

    /// Reads one pixel of the last rendered frame.
    ///
    /// # Errors
    ///
    /// Returns [`VulkanError`] when the coordinate is outside the canvas or the
    /// readback fails.
    pub fn read_pixel(&self, x: u32, y: u32) -> Result<[u8; 4], VulkanError> {
        self.target.read_pixel(&self.context, x, y)
    }

    /// Copies a scaled-down picture of the last frame into `into`.
    ///
    /// # Errors
    ///
    /// Returns [`VulkanError`] when the frame cannot be read back.
    /// Changes how big the picture this canvas keeps for onlookers is.
    ///
    /// # Errors
    ///
    /// Returns [`VulkanError::Call`] naming the Vulkan call that failed.
    pub fn set_preview_size(&mut self, size: SizePx) -> Result<(), VulkanError> {
        self.target.set_preview_size(&self.context, size)
    }

    /// The size of that picture.
    #[must_use]
    pub const fn preview_size(&self) -> SizePx {
        self.target.preview_size()
    }

    /// Copies a scaled-down picture of the last frame into `into`.
    ///
    /// Taken from the picture the card already shrank (`set_preview_size`), so
    /// asking for something smaller is a step down from an averaged image
    /// rather than from a full frame sampled every nth pixel.
    ///
    /// # Errors
    ///
    /// Returns [`VulkanError`] when the frame cannot be read back.
    pub fn read_scaled(&self, target: SizePx, into: &mut Vec<u8>) -> Result<(), VulkanError> {
        self.target.read_scaled(&self.context, target, into)
    }
}

impl OutputBackend for VulkanOffscreenBackend {
    fn id(&self) -> OutputId {
        self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn mode(&self) -> DisplayMode {
        self.mode
    }

    fn present(&mut self, _frame: FrameRequest, scene: &Scene) -> Result<(), BackendError> {
        self.target
            .render(&self.context, scene)
            .map_err(|error| self.present_failure(error))
    }

    fn set_readback(&mut self, wanted: bool) {
        self.target.readback_wanted = wanted;
    }

    fn snapshot(&self, size: SizePx, into: &mut Vec<u8>) -> bool {
        match self.target.read_scaled(&self.context, size, into) {
            Ok(()) => true,
            Err(error) => {
                // Not a fault. A preview that cannot be taken is a preview an
                // operator does not see; the wall is unaffected.
                tracing::debug!(output = %self.name, "could not read back a preview: {error}");
                false
            }
        }
    }

    fn bind_composition(&mut self, source: CompositionSource) -> Result<(), BackendError> {
        match source {
            CompositionSource::Vulkan {
                view,
                sampler,
                buses,
            } => {
                let device = self.context.device().clone();
                self.target
                    .bind_composition(&device, view, sampler)
                    .and_then(|()| self.target.bind_buses(&device, buses, sampler))
                    .map_err(|error| BackendError::UploadFailed {
                        name: self.name.clone(),
                        reason: error.to_string(),
                    })
            }
            // A Vulkan output with nothing to sample. Its pieces draw flat,
            // which is visibly wrong and better than a dark wall.
            CompositionSource::Nothing => {
                tracing::warn!(
                    output = %self.name,
                    "this output has no composition to sample; its pieces will draw flat"
                );
                Ok(())
            }
        }
    }
}

impl Compositor for VulkanOffscreenBackend {
    fn set_effect_time(&mut self, seconds: f32) {
        self.target.effect_seconds = seconds;
    }

    fn set_effect_frame(&mut self, seconds: f32) {
        self.target.effect_frame = seconds;
    }

    fn compose(&mut self, _frame: FrameRequest, scene: &Scene) -> Result<(), BackendError> {
        // Not the same pass as an output's. A composition is three buses drawn
        // into three images and then combined, because that is what a bus is
        //; an output draws finished pieces out of the result.
        self.target
            .compose(&self.context, scene)
            .map_err(|error| self.present_failure(error))
    }

    fn queue_texture(
        &mut self,
        key: TextureKey,
        image: std::sync::Arc<DecodedImage>,
    ) -> Result<(), BackendError> {
        self.target.queue_media_texture(key, image);
        Ok(())
    }

    fn flush_textures(&mut self) -> Result<(), BackendError> {
        self.target
            .flush_media_textures(&self.context)
            .map_err(|error| BackendError::UploadFailed {
                name: self.name.clone(),
                reason: error.to_string(),
            })
    }

    fn upload_texture(
        &mut self,
        key: TextureKey,
        image: &DecodedImage,
    ) -> Result<(), BackendError> {
        self.target
            .set_media_texture(&self.context, key, image)
            .map_err(|error| BackendError::UploadFailed {
                name: self.name.clone(),
                reason: error.to_string(),
            })
    }

    fn draw_generated(
        &mut self,
        key: TextureKey,
        size: ovs_types::geometry::SizePx,
        what: crate::backend::Generated,
    ) -> Result<bool, BackendError> {
        use crate::backend::Generated;

        // The description turns into immediates here and nowhere else: the
        // media side describes a picture and this is the only thing that knows
        // what the shader's branches are numbered.
        // Every kind is told how big the picture is, because most of them read
        // no texture at all and could not ask one.
        let blank = GeneratedPush {
            values: [0.0; 4],
            colour: [0.0, 0.0, 0.0, 1.0],
            colour_b: [0.0, 0.0, 0.0, 1.0],
            field: [0, 0],
            size: [size.width.max(1), size.height.max(1)],
            kind: GeneratedPush::SOLID,
            flags: 0,
        };
        let push = match what {
            Generated::Noise {
                scale,
                coloured,
                amount,
                field,
            } => GeneratedPush {
                values: [scale as f32, amount, 0.0, 0.0],
                field: [field as u32, (field >> 32) as u32],
                kind: GeneratedPush::NOISE,
                flags: if coloured { GeneratedPush::COLOURED } else { 0 },
                ..blank
            },
            Generated::Solid { colour } => GeneratedPush {
                colour,
                kind: GeneratedPush::SOLID,
                ..blank
            },
            Generated::Pattern {
                shift,
                sweep_start,
                sweep_trail,
                label_side,
                ..
            } => GeneratedPush {
                values: [shift[0] as f32, shift[1] as f32, sweep_start, sweep_trail],
                colour: [label_side as f32, 0.0, 0.0, 0.0],
                kind: GeneratedPush::PATTERN,
                ..blank
            },
            Generated::Gradient {
                from,
                to,
                angle_deg,
            } => GeneratedPush {
                values: [angle_deg, 0.0, 0.0, 0.0],
                colour: from,
                colour_b: to,
                kind: GeneratedPush::GRADIENT,
                ..blank
            },
            Generated::Grid {
                spacing,
                line,
                line_colour,
                background,
            } => GeneratedPush {
                values: [spacing as f32, line as f32, 0.0, 0.0],
                colour: line_colour,
                colour_b: background,
                kind: GeneratedPush::GRID,
                ..blank
            },
            Generated::Shape {
                figure,
                extent,
                rotation_deg,
                colour,
                background,
            } => GeneratedPush {
                values: [extent[0], extent[1], rotation_deg, figure as f32],
                colour,
                colour_b: background,
                kind: GeneratedPush::SHAPE,
                ..blank
            },
        };
        let source = match what {
            Generated::Pattern { source, .. } => Some(source),
            _ => None,
        };

        self.target
            .set_generated(&self.context, key, size, push, source)
            .map(|()| true)
            .map_err(|error| BackendError::UploadFailed {
                name: self.name.clone(),
                reason: error.to_string(),
            })
    }

    fn drop_texture(&mut self, key: TextureKey) -> bool {
        self.target.drop_media_texture(&self.context, key)
    }

    fn snapshot(&self, size: ovs_types::geometry::SizePx, into: &mut Vec<u8>) -> bool {
        // The same readback an output's preview uses, on the image the layers
        // were drawn into. What it shows is the show itself, before it is cut
        // into pieces and sent to walls.
        match self.target.read_scaled(&self.context, size, into) {
            Ok(()) => true,
            Err(error) => {
                tracing::debug!("could not take a composition preview: {error}");
                false
            }
        }
    }

    fn texture_count(&self) -> usize {
        self.target.texture_count()
    }

    fn last_quad_count(&self) -> usize {
        self.target.last_quad_count()
    }

    fn source(&self) -> CompositionSource {
        CompositionSource::Vulkan {
            view: self.target.view(),
            sampler: self.target.sampler(),
            // The three buses, for a piece that takes its rectangle out of one
            // rather than out of the composition. None before the first frame
            // that builds them, which is what a piece naming one draws flat
            // against — the same thing every piece does until there is a
            // picture.
            buses: self.target.bus_views(),
        }
    }

    fn composition_depth(&self) -> ovs_types::depth::CompositionDepth {
        composition_depth_of(COMPOSITION_FORMAT)
    }

    fn size(&self) -> SizePx {
        self.mode.size
    }

    fn gpu_telemetry(&self) -> Option<ovs_types::telemetry::GpuTelemetry> {
        let (used, total) = self.context.memory_usage()?;
        Some(ovs_types::telemetry::GpuTelemetry {
            device_name: self.context.summary().name.clone(),
            vram_used_bytes: Some(used),
            vram_total_bytes: Some(total),
            // Vulkan reports memory and nothing else. Utilisation and
            // temperature come from the driver's own library, and are filled in
            // above this if the machine has one (`crate::nvml`).
            utilisation_percent: None,
            temperature_c: None,
        })
    }
}

impl Drop for VulkanOffscreenBackend {
    fn drop(&mut self) {
        // SAFETY: waiting before destroying is what Vulkan requires; a failure
        // means the device is already lost, which the destruction handles.
        unsafe {
            let _ = self.context.device().device_wait_idle();
        }
        let device = self.context.device().clone();
        self.target.destroy(&device);
    }
}

#[cfg(test)]
mod preview_sizing {
    use super::*;

    #[test]
    fn a_surface_smaller_than_the_ceiling_keeps_its_own_size() {
        // Upscaling a wall to make a preview of it would be inventing detail.
        let small = vk::Extent2D {
            width: 640,
            height: 360,
        };
        let sized = preview_extent(small);

        assert_eq!((sized.width, sized.height), (640, 360));
    }

    #[test]
    fn a_larger_one_is_fitted_inside_it_at_its_own_shape() {
        let sized = preview_extent(vk::Extent2D {
            width: 1920,
            height: 1080,
        });
        assert_eq!((sized.width, sized.height), (1280, 720));

        // A wide wall gets a shorter picture, not a squashed one.
        let wide = preview_extent(vk::Extent2D {
            width: 3840,
            height: 1080,
        });
        assert_eq!((wide.width, wide.height), (1280, 360));

        // And a tall one a narrower picture.
        let tall = preview_extent(vk::Extent2D {
            width: 1080,
            height: 1920,
        });
        assert_eq!((tall.width, tall.height), (404, 720));
    }

    #[test]
    fn every_side_is_even() {
        // Every video encoder downstream of this is 4:2:0, and 4:2:0 has
        // half-resolution chroma.
        for (width, height) in [(1999, 1001), (3841, 1081), (1081, 1921)] {
            let sized = preview_extent(vk::Extent2D { width, height });
            assert_eq!(sized.width % 2, 0, "{width}x{height} -> {sized:?}");
            assert_eq!(sized.height % 2, 0, "{width}x{height} -> {sized:?}");
        }
    }

    #[test]
    fn nothing_is_ever_zero() {
        let sized = preview_extent(vk::Extent2D {
            width: 0,
            height: 0,
        });
        assert!(sized.width >= 1 && sized.height >= 1, "{sized:?}");
    }
}

#[cfg(test)]
mod tests {
    use ovs_types::config::{BlendMode, EffectInstance, ParameterValue, Route};
    use ovs_types::geometry::{FrameRate, SizePx};
    use ovs_types::id::{LayerId, MediaItemId};

    use crate::effects::EffectPass;
    use crate::media::{ChromaFormat, ColourMatrix};
    use crate::scene::{Quad, SourceRect, SourceTurn, TextureRef};
    use ovs_types::geometry::AnchorPoint;

    use super::*;

    fn mode(width: u32, height: u32) -> DisplayMode {
        DisplayMode::new(SizePx::new(width, height), FrameRate::FPS_60)
    }

    /// Wraps loose RGBA bytes as a picture, for the upload calls.
    ///
    /// Panics on a wrong length, which in a test is what is wanted: a picture
    /// that does not describe itself is a mistake in the test rather than a
    /// case the renderer has to survive.
    fn rgba(pixels: &[u8], width: u32, height: u32) -> DecodedImage {
        DecodedImage::new(pixels.to_vec(), SizePx::new(width, height))
            .unwrap_or_else(|| panic!("{} bytes is not {width}x{height} RGBA", pixels.len()))
    }

    /// Three planes of flat Y, Cb and Cr, for a picture of `size`.
    ///
    /// Flat because what these tests check is the conversion and the plumbing —
    /// that three planes reach three bindings and come back as the colour they
    /// encode — rather than anything about the content.
    fn planar(
        size: u32,
        luma: u8,
        cb: u8,
        cr: u8,
        subsample: u32,
        full_range: bool,
        matrix: ColourMatrix,
    ) -> DecodedImage {
        let wide = size.div_ceil(subsample);
        DecodedImage::planar(
            vec![luma; (size * size) as usize],
            vec![cb; (wide * wide) as usize],
            vec![cr; (wide * wide) as usize],
            SizePx::new(size, size),
            ChromaFormat {
                subsample_x: subsample,
                subsample_y: subsample,
                full_range,
                matrix,
                depth: crate::media::SampleDepth::Eight,
            },
        )
        .expect("well formed planes")
    }

    /// Opens a backend, or explains why the test is skipping.
    ///
    /// A CI runner has no GPU, so a skip has to be possible; printing why makes
    /// a skip visible in the output rather than a silent pass.
    fn backend(width: u32, height: u32) -> Option<VulkanOffscreenBackend> {
        match VulkanOffscreenBackend::new("Canvas", mode(width, height)) {
            Ok(mut backend) => {
                // What the card keeps for anybody looking on is normally a
                // small picture — 720p at most, averaged. A test comparing a
                // rendered pixel against an exact colour wants the frame
                // itself, so it asks for it.
                backend
                    .set_preview_size(SizePx::new(width, height))
                    .expect("the canvas can keep its own frame");
                Some(backend)
            }
            Err(VulkanError::NoLoader(reason)) => {
                println!("skipped: no Vulkan loader ({reason})");
                None
            }
            Err(VulkanError::NoDevice(reason)) => {
                println!("skipped: no usable device ({reason})");
                None
            }
            Err(error) => panic!("Vulkan is present but the canvas failed: {error}"),
        }
    }

    /// Opens a device, or explains why the test is skipping.
    fn shared_context() -> Option<Arc<VulkanContext>> {
        match VulkanContext::new("ovs-renderer-test") {
            Ok(context) => Some(Arc::new(context)),
            Err(VulkanError::NoLoader(reason)) => {
                println!("skipped: no Vulkan loader ({reason})");
                None
            }
            Err(VulkanError::NoDevice(reason)) => {
                println!("skipped: no usable device ({reason})");
                None
            }
            Err(error) => panic!("Vulkan is present but the device failed: {error}"),
        }
    }

    /// A quad covering the given NDC rectangle in the given colour.
    fn quad_at(left: f32, bottom: f32, right: f32, top: f32, colour: [f32; 4]) -> Quad {
        Quad {
            left,
            bottom,
            right,
            top,
            colour,
            rotation_deg: 0.0,
            anchor: ovs_types::geometry::AnchorPoint::CENTRE,
            layer_id: Some(LayerId::new()),
            source: SourceRect::WHOLE,
            source_turn: SourceTurn::NONE,
            texture: TextureRef::None,
            route: Route::M,
            blend: BlendMode::Normal,
        }
    }

    /// A scene holding one quad covering the whole canvas.
    fn full_canvas(colour: [f32; 4]) -> Scene {
        let mut scene = Scene::new();
        scene.push(quad_at(-1.0, -1.0, 1.0, 1.0, colour));
        scene
    }

    /// A quad covering the given NDC rectangle, sampling `media`.
    fn textured_quad_at(
        left: f32,
        bottom: f32,
        right: f32,
        top: f32,
        colour: [f32; 4],
        media: MediaItemId,
    ) -> Quad {
        Quad {
            texture: TextureRef::Media(media),
            ..quad_at(left, bottom, right, top, colour)
        }
    }

    /// A scene holding one white quad covering the whole canvas, sampling
    /// `media`. White because it is the identity for the shader's multiply, so
    /// what is read back is the texture itself.
    fn textured_canvas(media: MediaItemId) -> Scene {
        let mut scene = Scene::new();
        scene.push(textured_quad_at(
            -1.0,
            -1.0,
            1.0,
            1.0,
            [1.0, 1.0, 1.0, 1.0],
            media,
        ));
        scene
    }

    /// Three planes of flat ten-bit Y, Cb and Cr, as one of the two layouts.
    ///
    /// The values are given as ten-bit numbers — 64 to 940 for narrow-range
    /// luma — and put where the layout says they go: at the bottom of each
    /// sixteen-bit element for a software decoder's `yuv420p10le`, and at the
    /// top for a card's P010.
    fn planar_ten(
        size: u32,
        luma: u16,
        cb: u16,
        cr: u16,
        depth: crate::media::SampleDepth,
        full_range: bool,
        matrix: ColourMatrix,
    ) -> DecodedImage {
        let place = |value: u16| -> [u8; 2] {
            let stored = match depth {
                crate::media::SampleDepth::TenHigh => value << 6,
                _ => value,
            };
            stored.to_le_bytes()
        };
        let fill = |value: u16, count: usize| -> Vec<u8> {
            place(value)
                .iter()
                .copied()
                .cycle()
                .take(count * 2)
                .collect()
        };

        let samples = (size * size) as usize;
        let chroma_samples = ((size / 2) * (size / 2)) as usize;
        DecodedImage::planar(
            fill(luma, samples),
            fill(cb, chroma_samples),
            fill(cr, chroma_samples),
            SizePx::new(size, size),
            crate::media::ChromaFormat {
                subsample_x: 2,
                subsample_y: 2,
                full_range,
                matrix,
                depth,
            },
        )
        .expect("a ten-bit picture")
    }

    /// Draws a flat ten-bit picture and reads the middle of it back.
    fn ten_bit_colour(
        luma: u16,
        cb: u16,
        cr: u16,
        depth: crate::media::SampleDepth,
        full_range: bool,
        matrix: ColourMatrix,
    ) -> Option<[u8; 4]> {
        let mut canvas = backend(16, 16)?;
        let clip = MediaItemId::new();
        canvas
            .set_media_texture(
                clip,
                &planar_ten(16, luma, cb, cr, depth, full_range, matrix),
            )
            .expect("a ten-bit texture is one this card takes");
        canvas
            .compose(FrameRequest { index: 0 }, &textured_canvas(clip))
            .expect("composes");

        let mut pixels = Vec::new();
        assert!(
            crate::backend::Compositor::snapshot(&canvas, SizePx::new(16, 16), &mut pixels),
            "reads back"
        );
        let middle = (8 * 16 + 8) * 4;
        Some([
            pixels[middle],
            pixels[middle + 1],
            pixels[middle + 2],
            pixels[middle + 3],
        ])
    }

    #[test]
    fn ten_bit_planes_come_back_as_the_colour_they_encode() {
        // The same colours as the eight-bit test, written as the ten-bit
        // numbers that mean them: every value is four times its eight-bit
        // self, because narrow range is 16..235 at eight bits and 64..940 at
        // ten. If the scale in the shader is right, both tests agree.
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        for depth in [
            crate::media::SampleDepth::TenLow,
            crate::media::SampleDepth::TenHigh,
        ] {
            // Red: the eight-bit test's 63, 102, 240, four times over.
            let Some(red) = ten_bit_colour(252, 408, 960, depth, false, ColourMatrix::Bt709) else {
                return;
            };
            about(red, [255, 0, 0, 255], &format!("{depth:?} red"));

            let grey =
                ten_bit_colour(504, 512, 512, depth, false, ColourMatrix::Bt709).expect("renders");
            about(grey, [128, 128, 128, 255], &format!("{depth:?} mid grey"));
        }
    }

    /// One flat eight-bit picture, as `planar` builds them.
    ///
    /// Named apart from `planar_ten` so a test that mixes the two says which is
    /// which at the call site rather than in a parameter.
    fn planar_eight(size: u32, luma: u8, cb: u8, cr: u8) -> DecodedImage {
        planar(size, luma, cb, cr, 2, false, ColourMatrix::Bt709)
    }

    #[test]
    fn eight_bit_and_ten_bit_clips_draw_side_by_side_in_one_composition() {
        // The owner's item three, and the reason `sample_scale` is a
        // per-instance attribute rather than a uniform or a second pipeline: two
        // clips of different depths are two quads in one pass, and each one is
        // scaled by its own number. A scale held anywhere per-frame would give
        // both quads the last one uploaded — which reads as one of the two being
        // sixty-four times too dark, or four times too bright.
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };

        let eight = MediaItemId::new();
        let ten = MediaItemId::new();
        // The same red, written both ways: 63/102/240 at eight bits is
        // 252/408/960 at ten.
        canvas
            .set_media_texture(eight, &planar_eight(16, 63, 102, 240))
            .expect("an eight-bit texture");
        canvas
            .set_media_texture(
                ten,
                &planar_ten(
                    16,
                    252,
                    408,
                    960,
                    crate::media::SampleDepth::TenLow,
                    false,
                    ColourMatrix::Bt709,
                ),
            )
            .expect("a ten-bit texture");

        // Left half eight-bit, right half ten-bit, in one scene.
        let mut scene = Scene::new();
        scene.push(textured_quad_at(-1.0, -1.0, 0.0, 1.0, [1.0; 4], eight));
        scene.push(textured_quad_at(0.0, -1.0, 1.0, 1.0, [1.0; 4], ten));
        canvas
            .compose(FrameRequest { index: 0 }, &scene)
            .expect("composes");

        let left = canvas.read_pixel(4, 8).expect("reads the eight-bit half");
        let right = canvas.read_pixel(12, 8).expect("reads the ten-bit half");
        about(left, [255, 0, 0, 255], "the eight-bit half");
        about(right, [255, 0, 0, 255], "the ten-bit half");

        // And the other layout in the same pass, which is the pairing that would
        // break a shared scale sixty-four times over.
        let high = MediaItemId::new();
        canvas
            .set_media_texture(
                high,
                &planar_ten(
                    16,
                    504,
                    512,
                    512,
                    crate::media::SampleDepth::TenHigh,
                    false,
                    ColourMatrix::Bt709,
                ),
            )
            .expect("a P010 texture");
        let mut mixed = Scene::new();
        mixed.push(textured_quad_at(-1.0, -1.0, 0.0, 1.0, [1.0; 4], eight));
        mixed.push(textured_quad_at(0.0, -1.0, 1.0, 1.0, [1.0; 4], high));
        canvas
            .compose(FrameRequest { index: 1 }, &mixed)
            .expect("composes");
        about(
            canvas.read_pixel(4, 8).expect("reads"),
            [255, 0, 0, 255],
            "eight-bit red beside P010",
        );
        about(
            canvas.read_pixel(12, 8).expect("reads"),
            [128, 128, 128, 255],
            "P010 mid grey beside eight-bit red",
        );
    }

    #[test]
    fn a_blend_mode_blends_a_ten_bit_layer_the_same_as_an_eight_bit_one() {
        // Items nine and ten. A blend equation runs on what the fragment shader
        // returned, which is already colour — so depth should make no difference
        // at all, and a test that says so is what makes that claim checkable
        // rather than assumed. Multiply against a ten-bit red, with the
        // backdrop eight-bit white.
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };

        let white = MediaItemId::new();
        let red = MediaItemId::new();
        canvas
            .set_media_texture(white, &planar_eight(16, 235, 128, 128))
            .expect("eight-bit white");
        canvas
            .set_media_texture(
                red,
                &planar_ten(
                    16,
                    252,
                    408,
                    960,
                    crate::media::SampleDepth::TenLow,
                    false,
                    ColourMatrix::Bt709,
                ),
            )
            .expect("ten-bit red");

        let mut scene = Scene::new();
        scene.push(textured_quad_at(-1.0, -1.0, 1.0, 1.0, [1.0; 4], white));
        let mut multiplying = textured_quad_at(-1.0, -1.0, 1.0, 1.0, [1.0, 1.0, 1.0, 1.0], red);
        multiplying.blend = ovs_types::config::BlendMode::Multiply;
        scene.push(multiplying);

        let pixel = composed(&mut canvas, &scene);
        close_to(
            pixel[0],
            255,
            "white multiplied by ten-bit red keeps its red",
        );
        close_to(pixel[1], 0, "and loses its green");
        close_to(pixel[2], 0, "and its blue");
    }

    #[test]
    fn a_ten_bit_layer_at_half_opacity_covers_an_eight_bit_one_by_half() {
        // The compositing half of item ten. A ten-bit clip's alpha is the quad's
        // alpha — a codec decoded this way carries none of its own — so this is
        // asking whether depth leaks into coverage. It must not: half of a
        // ten-bit red over an eight-bit green is the same pixel as half of an
        // eight-bit red over it.
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };

        let green = MediaItemId::new();
        let ten = MediaItemId::new();
        let eight = MediaItemId::new();
        // BT.709 narrow-range green: Y' = 173, Cb = 42, Cr = 26.
        canvas
            .set_media_texture(green, &planar_eight(16, 173, 42, 26))
            .expect("eight-bit green");
        canvas
            .set_media_texture(
                ten,
                &planar_ten(
                    16,
                    252,
                    408,
                    960,
                    crate::media::SampleDepth::TenLow,
                    false,
                    ColourMatrix::Bt709,
                ),
            )
            .expect("ten-bit red");
        canvas
            .set_media_texture(eight, &planar_eight(16, 63, 102, 240))
            .expect("eight-bit red");

        let half = |over| {
            let mut scene = Scene::new();
            scene.push(textured_quad_at(-1.0, -1.0, 1.0, 1.0, [1.0; 4], green));
            scene.push(textured_quad_at(
                -1.0,
                -1.0,
                1.0,
                1.0,
                [1.0, 1.0, 1.0, 0.5],
                over,
            ));
            scene
        };

        let with_ten = composed(&mut canvas, &half(ten));
        let with_eight = composed(&mut canvas, &half(eight));
        for channel in 0..3 {
            assert!(
                with_ten[channel].abs_diff(with_eight[channel]) <= 3,
                "half a ten-bit red gave {with_ten:?} and half an eight-bit one                  {with_eight:?} — coverage must not depend on depth"
            );
        }
        assert_eq!(with_ten[3], 255, "the composition stays opaque");
    }

    const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];

    /// Draws a flat planar picture and reads the middle of it back.
    fn planar_colour(
        luma: u8,
        cb: u8,
        cr: u8,
        subsample: u32,
        full_range: bool,
        matrix: ColourMatrix,
    ) -> Option<[u8; 4]> {
        let mut canvas = backend(16, 16)?;
        let clip = MediaItemId::new();
        canvas
            .set_media_texture(
                clip,
                &planar(16, luma, cb, cr, subsample, full_range, matrix),
            )
            .expect("uploads");
        canvas
            .present(FrameRequest { index: 0 }, &textured_canvas(clip))
            .expect("renders");
        Some(canvas.read_pixel(8, 8).expect("reads"))
    }

    /// Asserts two colours match, allowing for the arithmetic being in floats.
    ///
    /// The conversion is a handful of multiplies on eight-bit samples and then
    /// a round back to eight bits, so an exact equality would be asserting the
    /// rounding of a particular driver. Two is tight enough to catch a wrong
    /// matrix, a wrong range or a swapped plane, all of which are off by tens.
    fn about(got: [u8; 4], wanted: [u8; 4], what: &str) {
        for channel in 0..4 {
            let difference = i32::from(got[channel]) - i32::from(wanted[channel]);
            assert!(
                difference.abs() <= 2,
                "{what}: got {got:?}, wanted about {wanted:?}"
            );
        }
    }

    #[test]
    fn a_planar_picture_comes_back_as_the_colour_it_encodes() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // The whole of the change, end to end: three planes are
        // uploaded, three bindings are sampled, and the shader does the
        // conversion the decoder used to do on the processor into a frame 2.7
        // times larger.
        //
        // BT.709, narrow range, which is what nearly every video file is. Red
        // is Y'=63, Cb=102, Cr=240 and mid grey is Y'=126 with both colour
        // planes at their centre.
        let Some(red) = planar_colour(63, 102, 240, 2, false, ColourMatrix::Bt709) else {
            return;
        };
        about(red, [255, 0, 0, 255], "red");

        let grey = planar_colour(126, 128, 128, 2, false, ColourMatrix::Bt709).expect("renders");
        about(grey, [128, 128, 128, 255], "mid grey");
    }

    /// Draws a flat NV12 picture and reads the middle of it back.
    ///
    /// The same samples as [`planar_colour`], carried the way a card hands them
    /// over: one luma plane and one half-size plane with Cb and Cr alternating.
    fn nv12_colour(
        luma: u8,
        cb: u8,
        cr: u8,
        full_range: bool,
        matrix: ColourMatrix,
    ) -> Option<[u8; 4]> {
        let mut canvas = backend(16, 16)?;
        let clip = MediaItemId::new();
        // Cb, Cr, Cb, Cr … across each of the eight rows of eight samples.
        let pairs: Vec<u8> = [cb, cr].iter().copied().cycle().take(8 * 8 * 2).collect();
        let picture = DecodedImage::interleaved(
            vec![luma; 16 * 16],
            pairs,
            SizePx::new(16, 16),
            ChromaFormat {
                subsample_x: 2,
                subsample_y: 2,
                full_range,
                matrix,
                depth: crate::media::SampleDepth::Eight,
            },
        )
        .expect("well formed NV12");
        canvas
            .set_media_texture(clip, &picture)
            .expect("uploads NV12");
        canvas
            .present(FrameRequest { index: 0 }, &textured_canvas(clip))
            .expect("renders");
        Some(canvas.read_pixel(8, 8).expect("reads"))
    }

    #[test]
    fn an_nv12_picture_comes_back_as_the_colour_it_encodes() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // What a card's decoder produces, drawn without being taken apart on
        // the processor first. Two images reach three bindings: the chroma
        // image is bound twice, once as it lies and once with its green
        // component swizzled into red, so the shader samples Cr out of the
        // second half of each pair without knowing a card was involved.
        //
        // This is the test that catches the swizzle being wrong, and it catches
        // it loudly: Cb and Cr the wrong way round turns red into blue.
        let Some(red) = nv12_colour(63, 102, 240, false, ColourMatrix::Bt709) else {
            return;
        };
        about(red, [255, 0, 0, 255], "red");

        let grey = nv12_colour(126, 128, 128, false, ColourMatrix::Bt709).expect("renders");
        about(grey, [128, 128, 128, 255], "mid grey");

        // Mid grey is the one colour a swapped swizzle cannot be caught by, so
        // a colour whose two components differ is checked against the three
        // plane path as well. Same samples, same wall: the whole claim of this
        // layout is that how the chroma arrived is nobody's business above the
        // texture.
        for (luma, cb, cr, what) in [
            (63, 102, 240, "red"),
            (173, 42, 26, "green"),
            (32, 240, 118, "blue"),
            (210, 16, 146, "a yellow"),
        ] {
            let split =
                planar_colour(luma, cb, cr, 2, false, ColourMatrix::Bt709).expect("renders");
            let together = nv12_colour(luma, cb, cr, false, ColourMatrix::Bt709).expect("renders");
            about(
                together,
                split,
                &format!("{what} as NV12 against three planes"),
            );
        }
    }

    #[test]
    fn a_full_range_nv12_picture_is_told_apart_from_a_narrow_one() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // The range reaches the shader through the texture's layout, and the
        // NV12 layout is a different variant from the planar one: a layout that
        // forgot to carry the range would draw every clip off a card washed
        // out, which is the kind of wrong that looks like a grade.
        let Some(narrow) = nv12_colour(126, 128, 128, false, ColourMatrix::Bt709) else {
            return;
        };
        let full = nv12_colour(126, 128, 128, true, ColourMatrix::Bt709).expect("renders");
        about(narrow, [128, 128, 128, 255], "mid grey, narrow range");
        about(full, [126, 126, 126, 255], "the same samples, full range");
    }

    /// One Hap Alpha movie this machine's FFmpeg wrote, as a path.
    ///
    /// Kept rather than deleted between the two calls below, because both want
    /// the same file and writing it twice would be writing two.
    fn hap_movie(size: u32) -> Option<std::path::PathBuf> {
        let movie = std::env::temp_dir().join(format!("ovs-hap-round-{size}.mov"));
        if movie.exists() {
            return Some(movie);
        }
        let made = std::process::Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                &format!("testsrc2=size={size}x{size}:rate=1:duration=1"),
                "-c:v",
                "hap",
                "-format",
                "hap_alpha",
            ])
            .arg(&movie)
            .status();
        if matches!(made, Ok(status) if status.success()) {
            Some(movie)
        } else {
            println!("skipped: no FFmpeg to write a HAP file with");
            None
        }
    }

    /// The frame as stored: what the renderer is handed, with no decoder.
    fn hap_frame(size: u32) -> Option<Vec<u8>> {
        let movie = hap_movie(size)?;
        let raw = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-i"])
            .arg(&movie)
            .args(["-c", "copy", "-f", "rawvideo", "-"])
            .output()
            .ok()?;
        raw.status.success().then_some(raw.stdout)
    }

    /// The same frame as FFmpeg decodes it, which is the answer to compare to.
    fn decoded_rgba(size: u32) -> Option<Vec<u8>> {
        let movie = hap_movie(size)?;
        let raw = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-i"])
            .arg(&movie)
            .args(["-frames:v", "1", "-pix_fmt", "rgba", "-f", "rawvideo", "-"])
            .output()
            .ok()?;
        raw.status
            .success()
            .then_some(raw.stdout)
            .filter(|bytes| bytes.len() == (size * size * 4) as usize)
    }

    #[test]
    fn a_hap_frame_reaches_the_wall_as_the_picture_it_holds() {
        // The whole of the alpha path, end to end and on real hardware: a file
        // an encoder wrote, read as blocks without a decoder, uploaded without
        // a conversion, sampled by the card, and read back.
        //
        // Against FFmpeg's own decode of the same file, within a tolerance.
        // Not exactly, and that is not slack: a card's BC3 decoder and a
        // processor's are both allowed to round differently, and the DXT block
        // format itself is lossy. What this catches is the failure that
        // matters — blocks read as the wrong format, or the wrong way up, or
        // with the channels swapped — all of which are tens of levels out, not
        // ones.
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        let Some(frame) = hap_frame(64) else {
            return;
        };
        let Some(wanted) = decoded_rgba(64) else {
            return;
        };

        let mut blocks = Vec::new();
        let format =
            crate::media::hap::read_frame(&frame, 64, 64, &mut blocks).expect("the frame reads");
        let picture = DecodedImage::compressed(blocks, SizePx::new(64, 64), format)
            .expect("the blocks are a texture");

        let Some(mut canvas) = backend(64, 64) else {
            return;
        };
        let media = MediaItemId::new();
        crate::backend::Compositor::upload_texture(&mut canvas, media.into(), &picture)
            .expect("a compressed texture is one this card takes");

        crate::backend::Compositor::compose(
            &mut canvas,
            FrameRequest { index: 0 },
            &textured_canvas(media),
        )
        .expect("composes");

        let mut pixels = Vec::new();
        assert!(
            crate::backend::Compositor::snapshot(&canvas, SizePx::new(64, 64), &mut pixels),
            "the frame reads back"
        );

        // Every pixel, because a format read wrongly is wrong everywhere and
        // an average would hide it.
        let mut worst = 0i32;
        for (got, want) in pixels
            .as_chunks::<4>()
            .0
            .iter()
            .zip(wanted.as_chunks::<4>().0)
        {
            for channel in 0..3 {
                worst = worst.max((i32::from(got[channel]) - i32::from(want[channel])).abs());
            }
        }
        assert!(
            worst <= 24,
            "the card's picture and FFmpeg's differ by {worst}, which is more than two codecs rounding"
        );
    }

    #[test]
    fn a_clips_worth_of_texture_made_and_given_back_does_not_fill_the_card() {
        // A texture is two allocations: the images, and a staging buffer which
        // on this renderer lives in *video* memory rather than host memory, so
        // that the copy onto the card is one hop instead of two. Giving back
        // only the first would be invisible in every small test and fatal in a
        // show.
        //
        // A 1080p RGBA texture is 8.3 MiB of images and as much again staging.
        // Five hundred of those is eight gigabytes, which is the whole of the
        // card this was found on — so if anything here is kept, the run stops.
        //
        // The size matters as much as the count. The same loop at 64 by 64
        // passes with the staging buffer leaking every round, which is how it
        // went unnoticed.
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        let Some(mut canvas) = backend(64, 64) else {
            return;
        };

        let size = SizePx::new(1920, 1080);
        let picture = DecodedImage::new(vec![200u8; 1920 * 1080 * 4], size)
            .expect("a picture the size of a frame");

        for round in 0..500 {
            let key = TextureKey::Layer(LayerId::new());
            crate::backend::Compositor::upload_texture(&mut canvas, key, &picture).unwrap_or_else(
                |error| {
                    panic!("round {round} could not upload, so something is being kept: {error}")
                },
            );
            assert!(crate::backend::Compositor::drop_texture(&mut canvas, key));
        }

        assert_eq!(
            crate::backend::Compositor::texture_count(&canvas),
            0,
            "everything was given back"
        );
    }

    #[test]
    fn media_coming_and_going_all_evening_does_not_use_the_canvas_up() {
        // What an operator actually does to a running server: import something,
        // put it in a slot, take it out again, import something else. Each of
        // those is a texture created and a texture dropped, and a Vulkan
        // descriptor pool cannot grow.
        //
        // Before the pool was created with `FREE_DESCRIPTOR_SET` and the drop
        // gave the set back, this stopped working after `MAX_TEXTURES`
        // *cumulative* textures rather than `MAX_TEXTURES` at once — and what
        // it looked like was a layer that quietly stopped playing and went to
        // its placeholder, some hours in, with nothing in the log about it.
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        let Some(mut canvas) = backend(64, 64) else {
            return;
        };

        // Four times over the ceiling, one at a time, which is the shape that
        // used to fail. The picture is small because this is about the pool
        // rather than about the pixels.
        let picture =
            DecodedImage::new(vec![200u8; 64 * 64 * 4], SizePx::new(64, 64)).expect("a picture");
        for round in 0..(MAX_TEXTURES * 4) {
            let key = TextureKey::Layer(LayerId::new());
            crate::backend::Compositor::upload_texture(&mut canvas, key, &picture)
                .unwrap_or_else(|error| panic!("round {round} could not upload: {error}"));
            assert!(
                crate::backend::Compositor::drop_texture(&mut canvas, key),
                "round {round} had a texture"
            );
        }

        // And the canvas still works afterwards, which is the thing the
        // operator cares about rather than the count.
        let key = TextureKey::Layer(LayerId::new());
        crate::backend::Compositor::upload_texture(&mut canvas, key, &picture)
            .expect("the canvas still takes a texture after all that");
        assert_eq!(
            crate::backend::Compositor::texture_count(&canvas),
            1,
            "one held, the rest given back"
        );
    }

    #[test]
    fn as_many_textures_at_once_as_the_canvas_says_it_holds() {
        // The other half, and the one that is a real ceiling: held at the same
        // time rather than one after another. This must still be refused at
        // `MAX_TEXTURES`, with the refusal that names it, because a pool that
        // silently over-committed would fail somewhere else entirely.
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        let Some(mut canvas) = backend(64, 64) else {
            return;
        };

        // Everything the canvas draws into for itself made first — the buses,
        // the scratch and an echo's memory — because those come out of the
        // same descriptor pool.
        let echo = effect_at("ovs.echo", 1, &[("length_s", ParameterValue::Float(1.0))]);
        canvas
            .compose(
                FrameRequest { index: 0 },
                &effected_flat(LayerId::new(), RED, &echo),
            )
            .expect("composes");

        let picture =
            DecodedImage::new(vec![200u8; 64 * 64 * 4], SizePx::new(64, 64)).expect("a picture");
        let mut held = Vec::new();
        for _ in 0..MAX_TEXTURES {
            let key = TextureKey::Layer(LayerId::new());
            if crate::backend::Compositor::upload_texture(&mut canvas, key, &picture).is_err() {
                break;
            }
            held.push(key);
        }

        // All of them. This used to allow for the buses and the scratch taking
        // sets out of the same pool, which is a ceiling that is not the one the
        // constant names; the pool now has room for those as well.
        assert_eq!(
            held.len(),
            MAX_TEXTURES,
            "the canvas holds what it says it holds"
        );
        for key in held {
            assert!(crate::backend::Compositor::drop_texture(&mut canvas, key));
        }
    }

    #[test]
    fn the_range_a_file_declares_is_the_range_it_is_shown_in() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // Sixteen is black in a narrow-range file and a dark grey in a full
        // one. A clip shown in the wrong range is washed out or crushed, which
        // looks like a grade rather than like a fault — so it has to be right
        // rather than nearly right.
        let Some(narrow) = planar_colour(16, 128, 128, 2, false, ColourMatrix::Bt709) else {
            return;
        };
        about(narrow, [0, 0, 0, 255], "16 in a narrow-range file is black");

        let full = planar_colour(16, 128, 128, 2, true, ColourMatrix::Bt709).expect("renders");
        about(full, [16, 16, 16, 255], "16 in a full-range file is not");
    }

    #[test]
    fn the_matrix_a_file_declares_is_the_matrix_it_is_shown_with() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // The same samples through the two conventions. BT.601 and BT.709
        // differ most in green, which is where a wrong choice shows first.
        let Some(seven_oh_nine) = planar_colour(126, 80, 160, 2, false, ColourMatrix::Bt709) else {
            return;
        };
        let six_oh_one =
            planar_colour(126, 80, 160, 2, false, ColourMatrix::Bt601).expect("renders");

        // Which channel moves most depends on the samples chosen, so the
        // assertion is about the colour rather than about one of them.
        let apart = (0..3)
            .map(|channel| {
                (i32::from(seven_oh_nine[channel]) - i32::from(six_oh_one[channel])).abs()
            })
            .max()
            .unwrap_or(0);
        assert!(
            apart > 4,
            "709 gave {seven_oh_nine:?} and 601 gave {six_oh_one:?}, which is the same picture"
        );
    }

    #[test]
    fn every_subsampling_a_decoder_produces_draws() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // 4:2:0, 4:2:2 and 4:4:4 are three different plane sizes and therefore
        // three different sets of image extents and copy regions. Flat planes,
        // so what is being checked is that they arrive rather than how they are
        // filtered.
        let Some(_) = backend(16, 16) else {
            return;
        };
        for subsample in [1, 2] {
            let colour = planar_colour(63, 102, 240, subsample, false, ColourMatrix::Bt709)
                .expect("renders");
            about(
                colour,
                [255, 0, 0, 255],
                &format!("{subsample}x subsampled"),
            );
        }
    }

    #[test]
    fn a_layer_that_changes_from_planar_to_rgba_gets_a_new_texture() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // What a slot change between a clip and a still is. The same key, the
        // same size, a different shape of picture: keeping the old texture
        // would sample a one-byte luma plane as though it were RGBA.
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };
        let clip = MediaItemId::new();

        canvas
            .set_media_texture(
                clip,
                &planar(16, 63, 102, 240, 2, false, ColourMatrix::Bt709),
            )
            .expect("uploads the clip");
        canvas
            .present(FrameRequest { index: 0 }, &textured_canvas(clip))
            .expect("renders");
        about(
            canvas.read_pixel(8, 8).expect("reads"),
            [255, 0, 0, 255],
            "the clip",
        );

        canvas
            .set_media_texture(clip, &rgba(&[0, 0, 255, 255].repeat(256), 16, 16))
            .expect("uploads the still");
        canvas
            .present(FrameRequest { index: 1 }, &textured_canvas(clip))
            .expect("renders");
        assert_eq!(
            canvas.read_pixel(8, 8).expect("reads"),
            [0, 0, 255, 255],
            "the still that replaced it"
        );
    }

    #[test]
    fn an_empty_scene_leaves_the_canvas_black() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // A show with every layer on Slot 0 composes to nothing, and nothing is
        // exactly what should reach the wall.
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };
        canvas
            .present(FrameRequest { index: 0 }, &Scene::new())
            .expect("renders");

        for (x, y) in [(0, 0), (8, 8), (15, 15)] {
            assert_eq!(canvas.read_pixel(x, y).expect("reads"), [0, 0, 0, 255]);
        }
    }

    #[test]
    fn a_full_canvas_quad_fills_the_canvas() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };
        canvas
            .present(FrameRequest { index: 0 }, &full_canvas(RED))
            .expect("renders");

        for (x, y) in [(0, 0), (8, 8), (15, 15), (0, 15), (15, 0)] {
            assert_eq!(
                canvas.read_pixel(x, y).expect("reads"),
                [255, 0, 0, 255],
                "pixel ({x}, {y}) should be covered"
            );
        }
    }

    #[test]
    fn the_coordinate_convention_is_y_up() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // Pins the toolchain's coordinate convention. The shaders are written
        // with +Y up, as GLSL means it, and naga applies the conversion to
        // Vulkan's +Y-down clip space. If that ever stops, every piece of
        // geometry in the renderer mirrors vertically, and this is the test that
        // says so rather than an operator noticing on a wall.
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };

        let mut scene = Scene::new();
        scene.push(quad_at(-1.0, 0.0, 1.0, 1.0, RED));
        canvas
            .present(FrameRequest { index: 0 }, &scene)
            .expect("renders");

        assert_eq!(
            canvas.read_pixel(8, 2).expect("reads"),
            [255, 0, 0, 255],
            "NDC +Y must be the top of the image"
        );
        assert_eq!(
            canvas.read_pixel(8, 13).expect("reads"),
            [0, 0, 0, 255],
            "and the bottom must be untouched"
        );
    }

    #[test]
    fn a_quad_lands_where_its_rectangle_says() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // The middle quarter: a layer at half scale, centred.
        let Some(mut canvas) = backend(64, 64) else {
            return;
        };
        let mut scene = Scene::new();
        scene.push(quad_at(-0.5, -0.5, 0.5, 0.5, RED));
        canvas
            .present(FrameRequest { index: 0 }, &scene)
            .expect("renders");

        assert_eq!(
            canvas.read_pixel(32, 32).expect("reads"),
            [255, 0, 0, 255],
            "centre"
        );
        for (x, y) in [(2, 2), (61, 2), (2, 61), (61, 61)] {
            assert_eq!(
                canvas.read_pixel(x, y).expect("reads"),
                [0, 0, 0, 255],
                "corner ({x}, {y}) is outside the quad"
            );
        }
    }

    #[test]
    fn an_offset_quad_moves_in_the_direction_it_was_offset() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        let Some(mut canvas) = backend(64, 64) else {
            return;
        };
        let mut scene = Scene::new();
        scene.push(quad_at(0.0, -1.0, 1.0, 1.0, RED));
        canvas
            .present(FrameRequest { index: 0 }, &scene)
            .expect("renders");

        assert_eq!(
            canvas.read_pixel(48, 32).expect("reads"),
            [255, 0, 0, 255],
            "right half"
        );
        assert_eq!(
            canvas.read_pixel(16, 32).expect("reads"),
            [0, 0, 0, 255],
            "left half"
        );
    }

    #[test]
    fn opacity_blends_against_what_is_underneath() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // Half-opacity white over the black canvas is mid grey. This is
        // BlendMode::Normal doing what it says, checked rather than assumed.
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };
        canvas
            .present(
                FrameRequest { index: 0 },
                &full_canvas([1.0, 1.0, 1.0, 0.5]),
            )
            .expect("renders");

        let pixel = canvas.read_pixel(8, 8).expect("reads");
        for channel in &pixel[..3] {
            assert!(
                (120..=136).contains(channel),
                "expected roughly half brightness, got {pixel:?}"
            );
        }
    }

    #[test]
    fn quads_composite_in_the_order_they_were_added() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // Later quads go on top. Getting this backwards would put every show's
        // layers in reverse.
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };
        let mut scene = full_canvas(RED);
        scene.push(quad_at(-1.0, -1.0, 1.0, 1.0, [0.0, 0.0, 1.0, 1.0]));
        canvas
            .present(FrameRequest { index: 0 }, &scene)
            .expect("renders");

        assert_eq!(
            canvas.read_pixel(8, 8).expect("reads"),
            [0, 0, 255, 255],
            "the last quad added must be on top"
        );
    }

    #[test]
    fn many_quads_render_in_one_pass() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // A real show has tens of layers and pieces, not one. This also
        // exercises the instance buffer well past a single entry.
        let Some(mut canvas) = backend(64, 64) else {
            return;
        };
        let mut scene = Scene::new();
        for index in 0..500 {
            let shade = index as f32 / 500.0;
            scene.push(quad_at(-1.0, -1.0, 1.0, 1.0, [shade, 0.0, 0.0, 1.0]));
        }
        canvas
            .present(FrameRequest { index: 0 }, &scene)
            .expect("renders 500 quads");

        // The last one wins, and it is nearly full red.
        let pixel = canvas.read_pixel(32, 32).expect("reads");
        assert!(
            pixel[0] > 250,
            "the topmost quad should be visible, got {pixel:?}"
        );
    }

    #[test]
    fn a_texture_actually_reaches_the_fragment_shader() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // A two-by-two texture stretched over the canvas. Reading back the four
        // quadrants proves the upload, the sampler, the descriptor set and the
        // UV mapping all line up, which no single one of them could prove alone.
        let Some(mut canvas) = backend(64, 64) else {
            return;
        };

        // Rows are top to bottom in image order: red, green on top; blue, white
        // below.
        let pixels: [u8; 16] = [
            255, 0, 0, 255, /* */ 0, 255, 0, 255, //
            0, 0, 255, 255, /* */ 255, 255, 255, 255,
        ];
        let clip = MediaItemId::new();
        canvas
            .set_media_texture(clip, &rgba(&pixels, 2, 2))
            .expect("uploads");
        canvas
            .present(FrameRequest { index: 0 }, &textured_canvas(clip))
            .expect("renders");

        // Sampled near the centre of each quadrant, away from the bilinear
        // blend along the seams.
        assert_eq!(
            canvas.read_pixel(8, 8).expect("reads"),
            [255, 0, 0, 255],
            "top left"
        );
        assert_eq!(
            canvas.read_pixel(55, 8).expect("reads"),
            [0, 255, 0, 255],
            "top right"
        );
        assert_eq!(
            canvas.read_pixel(8, 55).expect("reads"),
            [0, 0, 255, 255],
            "bottom left"
        );
        assert_eq!(
            canvas.read_pixel(55, 55).expect("reads"),
            [255, 255, 255, 255],
            "bottom right"
        );
    }

    #[test]
    fn a_quads_colour_multiplies_its_texture() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // Which is why an untextured quad can be given a white texture and take
        // the same path: white is the identity for this multiply.
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };
        let clip = MediaItemId::new();
        canvas
            .set_media_texture(clip, &rgba(&[128, 128, 128, 255], 1, 1))
            .expect("uploads");
        let mut scene = Scene::new();
        scene.push(textured_quad_at(
            -1.0,
            -1.0,
            1.0,
            1.0,
            [1.0, 0.0, 0.0, 1.0],
            clip,
        ));
        canvas
            .present(FrameRequest { index: 0 }, &scene)
            .expect("renders");

        let pixel = canvas.read_pixel(8, 8).expect("reads");
        assert!(
            (120..=136).contains(&pixel[0]),
            "red channel kept, got {pixel:?}"
        );
        assert_eq!(pixel[1], 0, "green multiplied away");
        assert_eq!(pixel[2], 0, "blue multiplied away");
    }

    #[test]
    fn a_texture_can_be_replaced_between_frames() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // A video source does this sixty times a second. Replacing it at the
        // same size must not leak, and the new pixels must actually appear.
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };
        let clip = MediaItemId::new();
        let scene = textured_canvas(clip);

        for shade in [0_u8, 64, 128, 192, 255] {
            canvas
                .set_media_texture(clip, &rgba(&[shade, shade, shade, 255], 1, 1))
                .expect("uploads");
            canvas
                .present(FrameRequest { index: 0 }, &scene)
                .expect("renders");
            assert_eq!(
                canvas.read_pixel(8, 8).expect("reads"),
                [shade, shade, shade, 255],
                "frame with shade {shade}"
            );
        }
    }

    #[test]
    fn a_texture_can_change_size() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // Which costs an allocation, so it is the expensive case and a video
        // source keeping its resolution is the cheap one.
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };
        let clip = MediaItemId::new();
        canvas
            .set_media_texture(clip, &rgba(&[255, 0, 0, 255], 1, 1))
            .expect("uploads");
        canvas
            .present(FrameRequest { index: 0 }, &textured_canvas(clip))
            .expect("renders");
        assert_eq!(canvas.read_pixel(8, 8).expect("reads"), [255, 0, 0, 255]);

        canvas
            .set_media_texture(clip, &rgba(&[0, 255, 0, 255].repeat(64), 8, 8))
            .expect("uploads a bigger one");
        canvas
            .present(FrameRequest { index: 1 }, &textured_canvas(clip))
            .expect("renders");
        assert_eq!(canvas.read_pixel(8, 8).expect("reads"), [0, 255, 0, 255]);
        assert_eq!(canvas.texture_count(), 1, "replaced, not accumulated");
    }

    #[test]
    fn pixels_that_do_not_describe_themselves_never_reach_a_texture() {
        // This used to be a test of the uploader, which counted the bytes it
        // was handed against the size it was told. It cannot be reached that
        // way any more: an upload takes a `DecodedImage`, and there is no way
        // to make one whose planes are the wrong size for it. The check moved
        // to where the picture is made, which is the place that knows.
        assert!(
            DecodedImage::new(vec![0, 0, 0], SizePx::new(1, 1)).is_none(),
            "too short"
        );
        assert!(
            DecodedImage::new(vec![0; 64], SizePx::new(1, 1)).is_none(),
            "too long"
        );
    }

    #[test]
    fn a_frame_of_another_size_replaces_the_one_before_it_cleanly() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // What a slot change is, and what a clip whose resolution changes part
        // way through is: the same key, a different size. The texture has to be
        // rebuilt rather than written into, and the frame after it has to be
        // the new picture rather than a torn one.
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };
        let clip = MediaItemId::new();
        canvas
            .set_media_texture(clip, &rgba(&[0, 255, 0, 255], 1, 1))
            .expect("uploads");
        let scene = textured_canvas(clip);
        canvas
            .present(FrameRequest { index: 0 }, &scene)
            .expect("renders");
        assert_eq!(canvas.read_pixel(8, 8).expect("reads"), [0, 255, 0, 255]);

        canvas
            .set_media_texture(clip, &rgba(&[0, 0, 255, 255].repeat(16), 4, 4))
            .expect("uploads a larger one under the same key");
        assert_eq!(canvas.texture_count(), 1, "one texture, not two");

        canvas
            .present(FrameRequest { index: 1 }, &scene)
            .expect("renders");
        assert_eq!(
            canvas.read_pixel(8, 8).expect("reads"),
            [0, 0, 255, 255],
            "the new frame, at its new size"
        );
    }

    #[test]
    fn a_decoded_png_reaches_the_canvas_the_right_way_up() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // The one test where the decoder and the shader meet. The shader
        // inverts V because a decoder hands over the top row first; if either
        // side of that ever changed, every still would be upside down and every
        // test using a hand-built array would still pass.
        let Some(mut canvas) = backend(64, 64) else {
            return;
        };

        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = directory.path().join("halves.png");
        let mut buffer = image::RgbaImage::from_pixel(1, 2, image::Rgba([0, 0, 0, 255]));
        buffer.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
        buffer.put_pixel(0, 1, image::Rgba([0, 0, 255, 255]));
        buffer.save(&path).expect("writes");

        let decoded = crate::media::decode_still(&path).expect("decodes");
        let clip = MediaItemId::new();
        // Named, because this canvas is both a compositor and an output now and
        // each of them can be handed a texture.
        Compositor::upload_texture(&mut canvas, clip.into(), &decoded)
            .expect("the output takes it");
        canvas
            .present(FrameRequest { index: 0 }, &textured_canvas(clip))
            .expect("renders");

        assert_eq!(
            canvas.read_pixel(32, 8).expect("reads"),
            [255, 0, 0, 255],
            "the image's top row belongs at the top of the canvas"
        );
        assert_eq!(
            canvas.read_pixel(32, 55).expect("reads"),
            [0, 0, 255, 255],
            "and its bottom row at the bottom"
        );
    }

    #[test]
    fn a_quad_can_sample_one_corner_of_its_texture() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // What a piece does: take a rectangle of an image and put it somewhere.
        // The whole canvas shows the texture's top-left quadrant, magnified.
        let Some(mut canvas) = backend(64, 64) else {
            return;
        };

        // Red, green on top; blue, white below.
        let pixels: [u8; 16] = [
            255, 0, 0, 255, /* */ 0, 255, 0, 255, //
            0, 0, 255, 255, /* */ 255, 255, 255, 255,
        ];
        let clip = MediaItemId::new();
        canvas
            .set_media_texture(clip, &rgba(&pixels, 2, 2))
            .expect("uploads");

        // The rectangle sits inside the bottom-right texel rather than on the
        // quadrant boundary. The sampler is bilinear, so a rectangle ending
        // exactly at 0.5 of a two-by-two image samples the seam between two
        // texels and comes back blended — correct filtering, and nothing to do
        // with whether the rectangle itself was applied.
        let mut quad = textured_quad_at(-1.0, -1.0, 1.0, 1.0, [1.0, 1.0, 1.0, 1.0], clip);
        quad.source = SourceRect {
            left: 0.75,
            top: 0.75,
            right: 1.0,
            bottom: 1.0,
        };
        let mut scene = Scene::new();
        scene.push(quad);
        canvas
            .present(FrameRequest { index: 0 }, &scene)
            .expect("renders");

        // Without the rectangle the canvas would show all four quadrants.
        for (x, y) in [(8, 8), (55, 8), (8, 55), (55, 55)] {
            assert_eq!(
                canvas.read_pixel(x, y).expect("reads"),
                [255, 255, 255, 255],
                "every corner should be the texture's bottom-right pixel, at ({x}, {y})"
            );
        }
    }

    #[test]
    fn source_rectangles_keep_their_orientation() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // The easiest way to get this wrong is a V flip that cancels out for the
        // whole image and reverses for anything smaller. Two halves, checked
        // against the rows they came from.
        let Some(mut canvas) = backend(32, 32) else {
            return;
        };

        // Top row red, bottom row blue.
        let pixels: [u8; 8] = [255, 0, 0, 255, 0, 0, 255, 255];
        let clip = MediaItemId::new();
        canvas
            .set_media_texture(clip, &rgba(&pixels, 1, 2))
            .expect("uploads");

        let white = [1.0, 1.0, 1.0, 1.0];
        let mut top_half = textured_quad_at(-1.0, 0.0, 1.0, 1.0, white, clip);
        top_half.source = SourceRect {
            left: 0.0,
            top: 0.0,
            right: 1.0,
            bottom: 0.5,
        };
        let mut bottom_half = textured_quad_at(-1.0, -1.0, 1.0, 0.0, white, clip);
        bottom_half.source = SourceRect {
            left: 0.0,
            top: 0.5,
            right: 1.0,
            bottom: 1.0,
        };

        let mut scene = Scene::new();
        scene.push(top_half);
        scene.push(bottom_half);
        canvas
            .present(FrameRequest { index: 0 }, &scene)
            .expect("renders");

        assert_eq!(
            canvas.read_pixel(16, 4).expect("reads"),
            [255, 0, 0, 255],
            "the image's top row, at the top of the canvas"
        );
        assert_eq!(
            canvas.read_pixel(16, 27).expect("reads"),
            [0, 0, 255, 255],
            "and its bottom row at the bottom"
        );
    }

    #[test]
    fn two_quads_can_sample_two_different_textures_in_one_frame() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // The whole point of the change: a show has one clip per layer, not one
        // clip. Left half red, right half green, from two separate images in a
        // single frame.
        let Some(mut canvas) = backend(64, 64) else {
            return;
        };
        let left_clip = MediaItemId::new();
        let right_clip = MediaItemId::new();
        canvas
            .set_media_texture(left_clip, &rgba(&[255, 0, 0, 255], 1, 1))
            .expect("uploads the left one");
        canvas
            .set_media_texture(right_clip, &rgba(&[0, 255, 0, 255], 1, 1))
            .expect("uploads the right one");

        let white = [1.0, 1.0, 1.0, 1.0];
        let mut scene = Scene::new();
        scene.push(textured_quad_at(-1.0, -1.0, 0.0, 1.0, white, left_clip));
        scene.push(textured_quad_at(0.0, -1.0, 1.0, 1.0, white, right_clip));
        canvas
            .present(FrameRequest { index: 0 }, &scene)
            .expect("renders");

        assert_eq!(
            canvas.read_pixel(16, 32).expect("reads"),
            [255, 0, 0, 255],
            "left half samples the first texture"
        );
        assert_eq!(
            canvas.read_pixel(48, 32).expect("reads"),
            [0, 255, 0, 255],
            "right half samples the second"
        );
        assert_eq!(canvas.texture_count(), 2);
    }

    #[test]
    fn quads_sharing_a_texture_stay_in_composition_order() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // Batching merges neighbours only. Merging every quad that shares a
        // texture regardless of position would reorder the composition, which
        // would put a show's layers on top of each other in the wrong order and
        // only under some scenes — the worst kind of bug to find live.
        let Some(mut canvas) = backend(32, 32) else {
            return;
        };
        let clip = MediaItemId::new();
        canvas
            .set_media_texture(clip, &rgba(&[255, 255, 255, 255], 1, 1))
            .expect("uploads");

        // Red through the texture, then opaque blue flat on top, then red
        // through the texture again but only over the left half.
        let mut scene = Scene::new();
        scene.push(textured_quad_at(
            -1.0,
            -1.0,
            1.0,
            1.0,
            [1.0, 0.0, 0.0, 1.0],
            clip,
        ));
        scene.push(quad_at(-1.0, -1.0, 1.0, 1.0, [0.0, 0.0, 1.0, 1.0]));
        scene.push(textured_quad_at(
            -1.0,
            -1.0,
            0.0,
            1.0,
            [1.0, 0.0, 0.0, 1.0],
            clip,
        ));
        canvas
            .present(FrameRequest { index: 0 }, &scene)
            .expect("renders");

        assert_eq!(
            canvas.read_pixel(8, 16).expect("reads"),
            [255, 0, 0, 255],
            "the third quad is on top of the second"
        );
        assert_eq!(
            canvas.read_pixel(24, 16).expect("reads"),
            [0, 0, 255, 255],
            "the second quad is on top of the first"
        );
    }

    #[test]
    fn a_quad_whose_media_has_no_texture_yet_draws_flat_rather_than_vanishing() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // Media arrives after the scene names it. A layer that disappeared for
        // the frames in between would look like a fault from the auditorium.
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };
        let mut scene = Scene::new();
        scene.push(textured_quad_at(
            -1.0,
            -1.0,
            1.0,
            1.0,
            RED,
            MediaItemId::new(),
        ));
        canvas
            .present(FrameRequest { index: 0 }, &scene)
            .expect("renders");

        assert_eq!(canvas.read_pixel(8, 8).expect("reads"), [255, 0, 0, 255]);
        assert_eq!(canvas.texture_count(), 0, "nothing was uploaded");
    }

    #[test]
    fn a_dropped_texture_is_forgotten_and_the_quad_falls_back() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // Unloading a clip mid-show must free its memory and must not leave the
        // descriptor set pointing at a destroyed image.
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };
        let clip = MediaItemId::new();
        canvas
            .set_media_texture(clip, &rgba(&[0, 0, 255, 255], 1, 1))
            .expect("uploads");
        let scene = textured_canvas(clip);
        canvas
            .present(FrameRequest { index: 0 }, &scene)
            .expect("renders");
        assert_eq!(canvas.read_pixel(8, 8).expect("reads"), [0, 0, 255, 255]);

        assert!(canvas.drop_media_texture(clip), "there was one");
        assert!(!canvas.drop_media_texture(clip), "and now there is not");
        assert_eq!(canvas.texture_count(), 0);

        canvas
            .present(FrameRequest { index: 1 }, &scene)
            .expect("renders");
        assert_eq!(
            canvas.read_pixel(8, 8).expect("reads"),
            [255, 255, 255, 255],
            "white, which is the flat quad's own colour"
        );
    }

    #[test]
    fn two_outputs_can_share_one_device_and_keep_their_own_textures() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // What a renderer driving several walls actually does. Each output owns
        // its own canvas and its own textures; only the device is shared, which
        // is what makes a shared composition possible at all.
        let Some(context) = shared_context() else {
            return;
        };

        let mut left = VulkanOffscreenBackend::on(Arc::clone(&context), "Left", mode(16, 16))
            .expect("opens the first canvas");
        let mut right = VulkanOffscreenBackend::on(Arc::clone(&context), "Right", mode(16, 16))
            .expect("opens the second canvas");

        let clip = MediaItemId::new();
        left.set_media_texture(clip, &rgba(&[255, 0, 0, 255], 1, 1))
            .expect("uploads");
        right
            .set_media_texture(clip, &rgba(&[0, 0, 255, 255], 1, 1))
            .expect("uploads");

        let scene = textured_canvas(clip);
        left.present(FrameRequest { index: 0 }, &scene)
            .expect("renders");
        right
            .present(FrameRequest { index: 0 }, &scene)
            .expect("renders");

        assert_eq!(left.read_pixel(8, 8).expect("reads"), [255, 0, 0, 255]);
        assert_eq!(right.read_pixel(8, 8).expect("reads"), [0, 0, 255, 255]);
    }

    #[test]
    fn one_output_going_away_leaves_the_others_on_the_same_device_alone() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // A wall being unplugged must not take the device down with it. The
        // tearing-down output waits for the device to be idle, which every other
        // output on it is also using.
        let Some(context) = shared_context() else {
            return;
        };

        let mut survivor =
            VulkanOffscreenBackend::on(Arc::clone(&context), "Kept", mode(16, 16)).expect("opens");
        let clip = MediaItemId::new();
        survivor
            .set_media_texture(clip, &rgba(&[0, 255, 0, 255], 1, 1))
            .expect("uploads");

        {
            let mut doomed = VulkanOffscreenBackend::on(Arc::clone(&context), "Lost", mode(16, 16))
                .expect("opens");
            doomed
                .set_media_texture(clip, &rgba(&[255, 255, 0, 255], 1, 1))
                .expect("uploads");
            doomed
                .present(FrameRequest { index: 0 }, &textured_canvas(clip))
                .expect("renders");
        }

        survivor
            .present(FrameRequest { index: 1 }, &textured_canvas(clip))
            .expect("still renders");
        assert_eq!(survivor.read_pixel(8, 8).expect("reads"), [0, 255, 0, 255]);
    }

    #[test]
    fn an_output_samples_the_composition_through_its_piece() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // The two passes of a frame, on one device. The composition is drawn
        // once; the output takes a rectangle of it and puts it somewhere. This
        // is the whole right-hand half of the render path.
        let Some(context) = shared_context() else {
            return;
        };

        let mut composition =
            VulkanOffscreenBackend::on(Arc::clone(&context), "Composition", mode(64, 64))
                .expect("opens the composition");
        let mut output = VulkanOffscreenBackend::on(Arc::clone(&context), "Wall", mode(64, 64))
            .expect("opens the output");
        output
            .bind_composition(composition.source())
            .expect("binds the composition");

        // A composition in two halves: red on the left, blue on the right.
        let mut composed = Scene::new();
        composed.push(quad_at(-1.0, -1.0, 0.0, 1.0, RED));
        composed.push(quad_at(0.0, -1.0, 1.0, 1.0, [0.0, 0.0, 1.0, 1.0]));
        composition
            .present(FrameRequest { index: 0 }, &composed)
            .expect("composes");

        // One piece: the left half of the composition, over the whole output.
        let mut piece = quad_at(-1.0, -1.0, 1.0, 1.0, [1.0, 1.0, 1.0, 1.0]);
        piece.texture = TextureRef::Composition;
        piece.source = SourceRect {
            left: 0.0,
            top: 0.0,
            right: 0.5,
            bottom: 1.0,
        };
        let mut pieces = Scene::new();
        pieces.push(piece);
        output
            .present(FrameRequest { index: 0 }, &pieces)
            .expect("draws its pieces");

        // Away from the right edge, where the sampler blends across the seam
        // between the composition's two halves.
        for x in [2, 20, 40, 56] {
            assert_eq!(
                output.read_pixel(x, 32).expect("reads"),
                [255, 0, 0, 255],
                "the whole output should be the composition's left half, at x={x}"
            );
        }

        // And the composition itself is untouched by having been sampled.
        assert_eq!(
            composition.read_pixel(48, 32).expect("reads"),
            [0, 0, 255, 255]
        );
    }

    #[test]
    fn a_piece_can_take_its_rectangle_out_of_the_composition_turned() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // The owner's first, 2026-09-23: "Sowohl Input als auch Output Piece
        // können Rotiert werden und einen Anchor haben." The output end already
        // turned. This is the input end: a wall hung on the diagonal is fed a
        // rectangle taken out of the composition at an angle.
        //
        // Red on the left of the composition and blue on the right, and a piece
        // taking the whole of it turned half a turn about its centre. Half a
        // turn is the case with no rounding in it: the left of the output must
        // be blue and the right red, exactly swapped.
        let Some(context) = shared_context() else {
            return;
        };

        let mut composition =
            VulkanOffscreenBackend::on(Arc::clone(&context), "Composition", mode(64, 64))
                .expect("opens the composition");
        let mut output = VulkanOffscreenBackend::on(Arc::clone(&context), "Wall", mode(64, 64))
            .expect("opens the output");
        output
            .bind_composition(composition.source())
            .expect("binds the composition");

        let mut composed = Scene::new();
        composed.push(quad_at(-1.0, -1.0, 0.0, 1.0, RED));
        composed.push(quad_at(0.0, -1.0, 1.0, 1.0, [0.0, 0.0, 1.0, 1.0]));
        composition
            .present(FrameRequest { index: 0 }, &composed)
            .expect("composes");

        let mut piece = quad_at(-1.0, -1.0, 1.0, 1.0, [1.0, 1.0, 1.0, 1.0]);
        piece.texture = TextureRef::Composition;
        piece.source = SourceRect::WHOLE;
        piece.source_turn = SourceTurn::about(
            SourceRect::WHOLE,
            AnchorPoint::CENTRE,
            180.0,
            SizePx::new(64, 64),
        );
        let mut pieces = Scene::new();
        pieces.push(piece);
        output
            .present(FrameRequest { index: 0 }, &pieces)
            .expect("draws its pieces");

        assert_eq!(
            output.read_pixel(16, 32).expect("reads"),
            [0, 0, 255, 255],
            "the composition's right half is now on the left"
        );
        assert_eq!(
            output.read_pixel(48, 32).expect("reads"),
            [255, 0, 0, 255],
            "and its left half on the right"
        );
    }

    #[test]
    fn turning_the_input_turns_it_in_pixels_and_not_in_texture_coordinates() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // The trap in the whole feature. One unit of U is the composition's
        // width and one of V is its height, so a turn expressed in texture
        // coordinates shears a rectangle that is not square instead of turning
        // it — and a composition is 16:9 almost always.
        //
        // A composition twice as wide as it is tall: white on the left half,
        // black on the right. The piece takes the left half, which is a
        // *square* in pixels, and turns it a quarter. A square turned a quarter
        // is the same square, so the output must still be entirely white.
        // Without the correction the region becomes twice as wide and half as
        // tall, reaches into the black half, and the right of the output goes
        // dark.
        let Some(context) = shared_context() else {
            return;
        };

        let mut composition =
            VulkanOffscreenBackend::on(Arc::clone(&context), "Composition", mode(64, 32))
                .expect("opens the composition");
        let mut output = VulkanOffscreenBackend::on(Arc::clone(&context), "Wall", mode(32, 32))
            .expect("opens the output");
        output
            .bind_composition(composition.source())
            .expect("binds the composition");

        let mut composed = Scene::new();
        composed.push(quad_at(-1.0, -1.0, 0.0, 1.0, [1.0, 1.0, 1.0, 1.0]));
        composition
            .present(FrameRequest { index: 0 }, &composed)
            .expect("composes");

        let square = SourceRect {
            left: 0.0,
            top: 0.0,
            right: 0.5,
            bottom: 1.0,
        };
        let mut piece = quad_at(-1.0, -1.0, 1.0, 1.0, [1.0, 1.0, 1.0, 1.0]);
        piece.texture = TextureRef::Composition;
        piece.source = square;
        piece.source_turn =
            SourceTurn::about(square, AnchorPoint::CENTRE, 90.0, SizePx::new(64, 32));
        let mut pieces = Scene::new();
        pieces.push(piece);
        output
            .present(FrameRequest { index: 0 }, &pieces)
            .expect("draws its pieces");

        // Away from the edges, where the sampler blends across the seam.
        for (x, y) in [(6, 16), (16, 6), (16, 26), (26, 16)] {
            assert_eq!(
                output.read_pixel(x, y).expect("reads"),
                [255, 255, 255, 255],
                "a square region turned a quarter is the same square, at {x},{y}"
            );
        }
    }

    #[test]
    fn a_ten_bit_clip_reaches_a_piece_of_a_wall_as_the_colour_it_encodes() {
        // The owner's item eleven, and the reason it is worth a test even though
        // it should be free: a piece samples the *composition*, which is colour
        // by then and has no depth of its own, so the ten-bit half of the path
        // is entirely behind it. This says so on real hardware rather than by
        // argument — and it is the only test here that runs both passes of a
        // frame over ten-bit material, which is what a wall actually does.
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        let Some(context) = shared_context() else {
            return;
        };

        let mut composition =
            VulkanOffscreenBackend::on(Arc::clone(&context), "Composition", mode(64, 64))
                .expect("opens the composition");
        let mut output = VulkanOffscreenBackend::on(Arc::clone(&context), "Wall", mode(64, 64))
            .expect("opens the output");
        output
            .bind_composition(composition.source())
            .expect("binds the composition");

        // The left half a ten-bit red, the right half a ten-bit mid grey, and
        // the two layouts one each so a piece cannot be right by taking the
        // half that happens to work.
        let red = MediaItemId::new();
        let grey = MediaItemId::new();
        Compositor::upload_texture(
            &mut composition,
            TextureKey::Media(red),
            &planar_ten(
                64,
                252,
                408,
                960,
                crate::media::SampleDepth::TenLow,
                false,
                ColourMatrix::Bt709,
            ),
        )
        .expect("a yuv420p10le texture");
        Compositor::upload_texture(
            &mut composition,
            TextureKey::Media(grey),
            &planar_ten(
                64,
                504,
                512,
                512,
                crate::media::SampleDepth::TenHigh,
                false,
                ColourMatrix::Bt709,
            ),
        )
        .expect("a P010 texture");

        let mut composed = Scene::new();
        composed.push(textured_quad_at(-1.0, -1.0, 0.0, 1.0, [1.0; 4], red));
        composed.push(textured_quad_at(0.0, -1.0, 1.0, 1.0, [1.0; 4], grey));
        composition
            .present(FrameRequest { index: 0 }, &composed)
            .expect("composes");

        // One piece: the left half of the composition, over the whole output.
        let mut piece = quad_at(-1.0, -1.0, 1.0, 1.0, [1.0, 1.0, 1.0, 1.0]);
        piece.texture = TextureRef::Composition;
        piece.source = SourceRect {
            left: 0.0,
            top: 0.0,
            right: 0.5,
            bottom: 1.0,
        };
        let mut pieces = Scene::new();
        pieces.push(piece);
        output
            .present(FrameRequest { index: 0 }, &pieces)
            .expect("draws its pieces");

        // Away from the seam, where the sampler blends the two halves.
        for x in [2, 20, 40, 56] {
            about(
                output.read_pixel(x, 32).expect("reads"),
                [255, 0, 0, 255],
                &format!("the piece at x={x} should be the ten-bit red half"),
            );
        }
        about(
            composition.read_pixel(48, 32).expect("reads"),
            [128, 128, 128, 255],
            "and the composition's own P010 half is still grey",
        );
    }

    #[test]
    fn a_piece_with_no_composition_bound_draws_flat_rather_than_failing() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // The first frames of a renderer, before anything has been composed.
        // A piece that failed here would fault an output over a timing detail.
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };
        let mut piece = quad_at(-1.0, -1.0, 1.0, 1.0, RED);
        piece.texture = TextureRef::Composition;
        let mut scene = Scene::new();
        scene.push(piece);

        canvas
            .present(FrameRequest { index: 0 }, &scene)
            .expect("renders");
        assert_eq!(canvas.read_pixel(8, 8).expect("reads"), [255, 0, 0, 255]);
    }

    #[test]
    fn a_composition_can_be_redrawn_between_output_frames() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // Sixty times a second, for the length of a show. The output must show
        // the frame that was just composed, not the one before it.
        let Some(context) = shared_context() else {
            return;
        };
        let mut composition =
            VulkanOffscreenBackend::on(Arc::clone(&context), "Composition", mode(16, 16))
                .expect("opens");
        let mut output =
            VulkanOffscreenBackend::on(Arc::clone(&context), "Wall", mode(16, 16)).expect("opens");
        output
            .bind_composition(composition.source())
            .expect("binds");

        let mut piece = quad_at(-1.0, -1.0, 1.0, 1.0, [1.0, 1.0, 1.0, 1.0]);
        piece.texture = TextureRef::Composition;
        let mut pieces = Scene::new();
        pieces.push(piece);

        for (index, shade) in [0.0_f32, 0.25, 0.5, 0.75, 1.0].into_iter().enumerate() {
            let index = index as u64;
            let mut composed = Scene::new();
            composed.push(quad_at(-1.0, -1.0, 1.0, 1.0, [shade, shade, shade, 1.0]));
            composition
                .present(FrameRequest { index }, &composed)
                .expect("composes");
            output
                .present(FrameRequest { index }, &pieces)
                .expect("presents");

            let expected = (shade * 255.0).round() as u8;
            let pixel = output.read_pixel(8, 8).expect("reads");
            assert!(
                pixel[0].abs_diff(expected) <= 1,
                "frame {index}: expected about {expected}, got {pixel:?}"
            );
        }
    }

    /// Draws `source` over `backdrop` with `mode` and reads the middle pixel.
    ///
    /// Both quads cover the whole canvas, so what comes back is the blend and
    /// nothing else.
    fn blended(
        canvas: &mut VulkanOffscreenBackend,
        backdrop: [f32; 4],
        source: [f32; 4],
        mode: BlendMode,
    ) -> [u8; 4] {
        let mut scene = Scene::new();
        scene.push(quad_at(-1.0, -1.0, 1.0, 1.0, backdrop));
        let mut over = quad_at(-1.0, -1.0, 1.0, 1.0, source);
        over.blend = mode;
        scene.push(over);

        canvas
            .present(FrameRequest { index: 0 }, &scene)
            .expect("renders");
        canvas.read_pixel(8, 8).expect("reads")
    }

    /// Asserts a channel is within rounding distance of what was expected.
    fn close_to(actual: u8, expected: u8, what: &str) {
        assert!(
            actual.abs_diff(expected) <= 2,
            "{what}: expected about {expected}, got {actual}"
        );
    }

    #[test]
    fn every_blend_mode_does_what_its_name_says() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // One backdrop, one source, six answers. Measured against a rendered
        // frame rather than against the blend state, because the blend state
        // being what the code says and the picture being right are two
        // different claims.
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };

        let grey = [0.5, 0.5, 0.5, 1.0];
        let dark = [0.25, 0.25, 0.25, 1.0];

        for (mode, expected) in [
            (BlendMode::Normal, 64_u8),
            (BlendMode::Add, 191),
            (BlendMode::Multiply, 32),
            (BlendMode::Screen, 159),
            (BlendMode::Lighten, 128),
            (BlendMode::Darken, 64),
        ] {
            let pixel = blended(&mut canvas, grey, dark, mode);
            close_to(pixel[0], expected, &format!("{mode:?}"));
            assert_eq!(pixel[3], 255, "{mode:?} must leave the canvas opaque");
        }
    }

    #[test]
    fn fading_a_darken_layer_out_leaves_the_backdrop_alone() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // The case a naive premultiply gets wrong. Darken takes a minimum, so
        // its identity is white, not black: a layer at zero opacity that
        // premultiplied towards black would take everything under it to black
        // with it — a fade that blacks out the wall instead of revealing it.
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };

        let white = [1.0, 1.0, 1.0, 1.0];

        let gone = blended(&mut canvas, white, [0.0, 0.0, 0.0, 0.0], BlendMode::Darken);
        close_to(gone[0], 255, "fully faded out");

        let half = blended(&mut canvas, white, [0.0, 0.0, 0.0, 0.5], BlendMode::Darken);
        close_to(half[0], 128, "half faded in");

        let full = blended(&mut canvas, white, [0.0, 0.0, 0.0, 1.0], BlendMode::Darken);
        close_to(full[0], 0, "fully faded in");
    }

    #[test]
    fn fading_an_additive_layer_out_leaves_the_backdrop_alone() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };
        let grey = [0.5, 0.5, 0.5, 1.0];

        for (alpha, expected) in [(0.0_f32, 128_u8), (0.5, 191), (1.0, 255)] {
            let pixel = blended(&mut canvas, grey, [0.5, 0.5, 0.5, alpha], BlendMode::Add);
            close_to(pixel[0], expected, &format!("alpha {alpha}"));
        }
    }

    #[test]
    fn overlay_and_difference_draw_as_themselves_where_the_device_can() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // The two modes core Vulkan cannot express. With
        // VK_EXT_blend_operation_advanced they are the hardware's own
        // equations; without it they are drawn as Normal and the renderer says
        // so, because something that composites beats nothing on a wall.
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };
        let grey = [0.5, 0.5, 0.5, 1.0];
        let red = [1.0, 0.0, 0.0, 1.0];

        if !canvas.has_advanced_blend() {
            println!("skipped: this device has no advanced blending");
            for mode in [BlendMode::Overlay, BlendMode::Difference] {
                let pixel = blended(&mut canvas, grey, red, mode);
                assert_eq!(pixel, [255, 0, 0, 255], "{mode:?} falls back to Normal");
            }
            return;
        }

        // Overlay against a backdrop just above a half: 1 - 2(1-Cs)(1-Cd).
        // Red stays at the top, and the other two channels come up off zero by
        // the amount the backdrop is above a half — which is what makes this
        // recognisably not Normal.
        let overlay = blended(&mut canvas, grey, red, BlendMode::Overlay);
        close_to(overlay[0], 255, "overlay red");
        close_to(overlay[1], 1, "overlay green");
        close_to(overlay[2], 1, "overlay blue");

        // Difference: |Cs - Cd| per channel, against a backdrop of 128.
        let difference = blended(&mut canvas, grey, red, BlendMode::Difference);
        close_to(difference[0], 127, "difference red");
        close_to(difference[1], 128, "difference green");
        close_to(difference[2], 128, "difference blue");
    }

    #[test]
    fn fading_an_advanced_blend_out_leaves_the_backdrop_alone() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // The property that makes a blend mode usable live: at zero opacity the
        // layer must do nothing at all, and every step between has to get
        // there smoothly. The advanced equations weight by the source's alpha,
        // so this is a property of the pipeline state rather than of the
        // shader — and worth a test that would catch it being set up wrong.
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };
        if !canvas.has_advanced_blend() {
            println!("skipped: this device has no advanced blending");
            return;
        }

        let grey = [0.5, 0.5, 0.5, 1.0];
        for mode in [BlendMode::Overlay, BlendMode::Difference] {
            let faded = blended(&mut canvas, grey, [1.0, 0.0, 0.0, 0.0], mode);
            close_to(faded[0], 128, "faded out, red");
            close_to(faded[1], 128, "faded out, green");
            close_to(faded[2], 128, "faded out, blue");
        }
    }

    #[test]
    fn two_layers_with_different_blend_modes_are_two_draws_and_still_in_order() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // Batching groups neighbours by texture *and* blend mode. Grouping by
        // texture alone would reorder a Multiply under an Add, which is a
        // different picture entirely.
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };

        let mut scene = Scene::new();
        scene.push(quad_at(-1.0, -1.0, 1.0, 1.0, [0.5, 0.5, 0.5, 1.0]));
        let mut added = quad_at(-1.0, -1.0, 1.0, 1.0, [0.25, 0.25, 0.25, 1.0]);
        added.blend = BlendMode::Add;
        scene.push(added);
        let mut multiplied = quad_at(-1.0, -1.0, 1.0, 1.0, [0.5, 0.5, 0.5, 1.0]);
        multiplied.blend = BlendMode::Multiply;
        scene.push(multiplied);

        canvas
            .present(FrameRequest { index: 0 }, &scene)
            .expect("renders");

        // 0.5 + 0.25 = 0.75, then halved by the multiply.
        close_to(
            canvas.read_pixel(8, 8).expect("reads")[0],
            96,
            "add then multiply",
        );
    }

    #[test]
    fn a_turned_quad_lands_turned() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // A wide strip across the middle, turned a quarter turn, becomes a tall
        // strip down the middle. Measured rather than trusted, because the
        // arithmetic has a sign in it and both signs look plausible.
        let Some(mut canvas) = backend(32, 32) else {
            return;
        };

        let mut strip = quad_at(-1.0, -0.25, 1.0, 0.25, RED);
        strip.rotation_deg = 90.0;
        let mut scene = Scene::new();
        scene.push(strip);
        canvas
            .present(FrameRequest { index: 0 }, &scene)
            .expect("renders");

        assert_eq!(
            canvas.read_pixel(16, 4).expect("reads"),
            [255, 0, 0, 255],
            "the strip should now run top to bottom"
        );
        assert_eq!(
            canvas.read_pixel(16, 27).expect("reads"),
            [255, 0, 0, 255],
            "top to bottom, both ends"
        );
        assert_eq!(
            canvas.read_pixel(4, 16).expect("reads"),
            [0, 0, 0, 255],
            "and no longer left to right"
        );
    }

    #[test]
    fn turning_a_quad_a_quarter_at_a_time_comes_back_to_where_it_started() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // Four quarter turns is the identity, and a sign error somewhere in the
        // basis would show up as the strip landing somewhere else on one of
        // them.
        let Some(mut canvas) = backend(32, 32) else {
            return;
        };

        for (index, angle) in [0.0_f32, 90.0, 180.0, 270.0, 360.0].into_iter().enumerate() {
            let mut strip = quad_at(-1.0, -0.25, 1.0, 0.25, RED);
            strip.rotation_deg = angle;
            let mut scene = Scene::new();
            scene.push(strip);
            canvas
                .present(
                    FrameRequest {
                        index: index as u64,
                    },
                    &scene,
                )
                .expect("renders");

            let horizontal = angle % 180.0 == 0.0;
            let across = canvas.read_pixel(4, 16).expect("reads");
            let down = canvas.read_pixel(16, 4).expect("reads");

            if horizontal {
                assert_eq!(across, [255, 0, 0, 255], "at {angle} degrees");
                assert_eq!(down, [0, 0, 0, 255], "at {angle} degrees");
            } else {
                assert_eq!(across, [0, 0, 0, 255], "at {angle} degrees");
                assert_eq!(down, [255, 0, 0, 255], "at {angle} degrees");
            }
        }
    }

    #[test]
    fn a_held_composition_is_still_there_to_sample() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // What freeze relies on: a composition nobody redrew this frame still
        // holds last frame's picture, and an output drawing from it gets that
        // picture rather than a cleared canvas.
        let Some(context) = shared_context() else {
            return;
        };
        let mut composition =
            VulkanOffscreenBackend::on(Arc::clone(&context), "Composition", mode(16, 16))
                .expect("opens");
        let mut output =
            VulkanOffscreenBackend::on(Arc::clone(&context), "Wall", mode(16, 16)).expect("opens");
        output
            .bind_composition(composition.source())
            .expect("binds");

        let mut composed = Scene::new();
        composed.push(quad_at(-1.0, -1.0, 1.0, 1.0, RED));
        composition
            .present(FrameRequest { index: 0 }, &composed)
            .expect("composes");

        let mut piece = quad_at(-1.0, -1.0, 1.0, 1.0, [1.0, 1.0, 1.0, 1.0]);
        piece.texture = TextureRef::Composition;
        let mut pieces = Scene::new();
        pieces.push(piece);

        // Several output frames with no composition pass in between, which is
        // exactly what the loop does while frozen.
        for index in 1..5 {
            output
                .present(FrameRequest { index }, &pieces)
                .expect("presents");
            assert_eq!(
                output.read_pixel(8, 8).expect("reads"),
                [255, 0, 0, 255],
                "frame {index} should still show the held composition"
            );
        }
    }

    #[test]
    fn reading_outside_the_canvas_is_refused_rather_than_returning_rubbish() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        let Some(canvas) = backend(16, 16) else {
            return;
        };
        assert!(canvas.read_pixel(16, 0).is_err());
        assert!(canvas.read_pixel(0, 16).is_err());
        assert!(canvas.read_pixel(15, 15).is_ok());
    }

    #[test]
    fn frames_can_be_rendered_back_to_back() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // Each frame re-records, re-uploads and re-submits, so a missing fence
        // wait would show up here as a validation failure, a torn instance
        // buffer or a hang, rather than at frame two thousand of a show.
        let Some(mut canvas) = backend(32, 32) else {
            return;
        };
        let scene = full_canvas(RED);

        for index in 0..120 {
            canvas
                .present(FrameRequest { index }, &scene)
                .unwrap_or_else(|error| panic!("frame {index} failed: {error}"));
        }
        assert_eq!(
            canvas.read_pixel(16, 16).expect("reads back"),
            [255, 0, 0, 255]
        );
    }

    /// Composes a scene through the buses and reads the middle pixel.
    fn composed(canvas: &mut VulkanOffscreenBackend, scene: &Scene) -> [u8; 4] {
        canvas
            .compose(FrameRequest { index: 0 }, scene)
            .expect("composes");
        canvas.read_pixel(8, 8).expect("reads")
    }

    /// A full-canvas quad on a bus.
    fn on_bus(route: Route, colour: [f32; 4]) -> Quad {
        let mut quad = quad_at(-1.0, -1.0, 1.0, 1.0, colour);
        quad.route = route;
        quad
    }

    /// The master state at a crossfader position and dimmer.
    fn gains(crossfader: f32, dimmer: f32) -> crate::buses::BusGains {
        crate::buses::BusGains::from_master(
            &ovs_types::live::MasterLive {
                crossfader,
                dimmer,
                ..ovs_types::live::MasterLive::default()
            },
            ovs_types::config::CrossfaderCurve::Linear,
        )
    }

    #[test]
    fn the_crossfader_dissolves_between_two_finished_pictures() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // owner decision Q8, and the reason it was asked: red on A under blue
        // on B, halfway across. Applying the fader to each layer in one pass
        // gave (64, 0, 127) — the blue at half, the red at half of what was
        // left beneath it. Three buses mixed give both at half.
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };

        let mut scene = Scene::new();
        scene.push(on_bus(Route::A, RED));
        scene.push(on_bus(Route::B, [0.0, 0.0, 1.0, 1.0]));

        scene.set_gains(gains(0.5, 1.0));
        let middle = composed(&mut canvas, &scene);
        close_to(middle[0], 128, "red at half");
        close_to(middle[2], 128, "blue at half");

        scene.set_gains(gains(0.0, 1.0));
        assert_eq!(composed(&mut canvas, &scene), [255, 0, 0, 255], "hard to A");

        scene.set_gains(gains(1.0, 1.0));
        assert_eq!(composed(&mut canvas, &scene), [0, 0, 255, 255], "hard to B");
    }

    #[test]
    fn a_layer_on_m_is_untouched_by_the_crossfader() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // M bypasses it by definition: a persistent overlay must not fade out
        // because an operator moved to the other side.
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };

        let mut scene = Scene::new();
        scene.push(on_bus(Route::M, [0.0, 1.0, 0.0, 1.0]));

        for position in [0.0, 0.5, 1.0] {
            scene.set_gains(gains(position, 1.0));
            assert_eq!(
                composed(&mut canvas, &scene),
                [0, 255, 0, 255],
                "crossfader at {position}"
            );
        }
    }

    #[test]
    fn m_is_composited_above_the_crossfaded_pair() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // The owner's answer to Q8: M goes over the A/B image. A half-covered
        // M lets what is underneath show through the part it does not cover,
        // which is what "above" means for a picture with coverage.
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };

        let mut scene = Scene::new();
        scene.push(on_bus(Route::A, RED));
        // Covers the left half only.
        let mut overlay = quad_at(-1.0, -1.0, 0.0, 1.0, [0.0, 1.0, 0.0, 1.0]);
        overlay.route = Route::M;
        scene.push(overlay);
        scene.set_gains(gains(0.0, 1.0));

        canvas
            .compose(FrameRequest { index: 0 }, &scene)
            .expect("composes");
        assert_eq!(
            canvas.read_pixel(4, 8).expect("reads"),
            [0, 255, 0, 255],
            "M covers the left"
        );
        assert_eq!(
            canvas.read_pixel(12, 8).expect("reads"),
            [255, 0, 0, 255],
            "and A shows through on the right"
        );
    }

    #[test]
    fn a_layer_on_m_adds_to_the_show_rather_than_covering_it() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // The owner's report, 2026-09-23: "Wenn aber Ebene die auf M liegt mit
        // Add drin ist, \u00fcberlagert M einfach den kompletten Bildschirm und
        // es passiert kein Add zu A oder B."
        //
        // Red on A, green added on M. Additive means they make yellow. What it
        // did before was show green: M was composited against transparent
        // black, where adding is the same as replacing, and the finished image
        // then went over the pair with Normal — so the blend an operator had
        // chosen had nothing to blend with.
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };

        let mut scene = Scene::new();
        scene.push(on_bus(Route::A, RED));
        let mut adding = on_bus(Route::M, [0.0, 1.0, 0.0, 1.0]);
        adding.blend = ovs_types::config::BlendMode::Add;
        scene.push(adding);
        scene.set_gains(gains(0.0, 1.0));

        assert_eq!(
            composed(&mut canvas, &scene),
            [255, 255, 0, 255],
            "red under green, added, is yellow"
        );
    }

    #[test]
    fn a_multiply_on_m_multiplies_the_crossfaded_pair() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // The same rule in the direction that used to be worst. Multiplying
        // against transparent black gives black, so a Multiply layer on M put
        // a black rectangle on the wall; against the show it does what it
        // says.
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };

        let mut scene = Scene::new();
        scene.push(on_bus(Route::A, [1.0, 1.0, 1.0, 1.0]));
        let mut multiplying = on_bus(Route::M, RED);
        multiplying.blend = ovs_types::config::BlendMode::Multiply;
        scene.push(multiplying);
        scene.set_gains(gains(0.0, 1.0));

        assert_eq!(
            composed(&mut canvas, &scene),
            [255, 0, 0, 255],
            "white multiplied by red is red"
        );
    }

    #[test]
    fn the_dimmer_applies_once_to_a_show_that_has_a_layer_on_m() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // Seeding M with the crossfaded pair is a second place the gains reach
        // the picture, and the way to get this wrong is to apply the dimmer in
        // both. Half a dimmer is half of everything, once.
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };

        let mut scene = Scene::new();
        scene.push(on_bus(Route::A, [1.0, 1.0, 1.0, 1.0]));
        let mut adding = on_bus(Route::M, [0.0, 0.0, 1.0, 1.0]);
        adding.blend = ovs_types::config::BlendMode::Add;
        scene.push(adding);
        scene.set_gains(gains(0.0, 0.5));

        let pixel = composed(&mut canvas, &scene);
        close_to(pixel[0], 128, "half of A's white");
        close_to(pixel[2], 255, "half of A's white plus half of M's blue");
    }

    #[test]
    fn a_layer_on_m_still_covers_what_it_is_over_when_it_is_set_to_normal() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // The bus did not stop being the master path. A Normal layer on M is
        // an overlay and covers what it lands on, exactly as before, and the
        // crossfader still does not touch it.
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };

        let mut scene = Scene::new();
        scene.push(on_bus(Route::A, RED));
        scene.push(on_bus(Route::M, [0.0, 1.0, 0.0, 1.0]));

        for position in [0.0, 0.5, 1.0] {
            scene.set_gains(gains(position, 1.0));
            assert_eq!(
                composed(&mut canvas, &scene),
                [0, 255, 0, 255],
                "crossfader at {position}"
            );
        }
    }

    #[test]
    fn layers_composite_within_their_own_bus() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // The other half of Q8. Two layers on B, one over the other: the upper
        // one wins on B whatever A is doing, because they are one picture and A
        // is another.
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };

        let mut scene = Scene::new();
        scene.push(on_bus(Route::A, RED));
        scene.push(on_bus(Route::B, [0.0, 1.0, 0.0, 1.0]));
        scene.push(on_bus(Route::B, [0.0, 0.0, 1.0, 1.0]));
        scene.set_gains(gains(1.0, 1.0));

        assert_eq!(
            composed(&mut canvas, &scene),
            [0, 0, 255, 255],
            "the later B layer is on top of the earlier one"
        );
    }

    #[test]
    fn a_blend_mode_blends_against_its_own_bus() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // The claim the single-pass model could not make: Multiply on B
        // multiplies against what B has, not against whatever A has already put
        // down. With the crossfader hard over to B, A is not even drawn — and
        // the result is the multiply against black that B alone gives.
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };

        let mut scene = Scene::new();
        scene.push(on_bus(Route::A, [1.0, 1.0, 1.0, 1.0]));
        scene.push(on_bus(Route::B, [1.0, 0.0, 0.0, 1.0]));
        let mut multiplying = on_bus(Route::B, [1.0, 1.0, 1.0, 1.0]);
        multiplying.blend = ovs_types::config::BlendMode::Multiply;
        scene.push(multiplying);

        scene.set_gains(gains(1.0, 1.0));
        assert_eq!(
            composed(&mut canvas, &scene),
            [255, 0, 0, 255],
            "white multiplied by B's own red is red"
        );
    }

    #[test]
    fn the_master_dimmer_applies_after_the_buses_are_combined() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // Which is why a half-dimmed crossfade at the midpoint is half of the
        // mix rather than a quarter of each side.
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };

        let mut scene = Scene::new();
        scene.push(on_bus(Route::A, [1.0, 1.0, 1.0, 1.0]));
        scene.push(on_bus(Route::B, [1.0, 1.0, 1.0, 1.0]));
        scene.set_gains(gains(0.5, 0.5));

        let pixel = composed(&mut canvas, &scene);
        close_to(pixel[0], 128, "half of white");
    }

    #[test]
    fn a_blackout_is_black_and_still_a_frame() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };

        let mut scene = Scene::new();
        scene.push(on_bus(Route::A, RED));
        scene.push(on_bus(Route::M, [0.0, 1.0, 0.0, 1.0]));
        scene.set_gains(gains(0.0, 0.0));

        assert_eq!(composed(&mut canvas, &scene), [0, 0, 0, 255]);
    }

    #[test]
    fn a_composition_stays_opaque_however_little_covers_it() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // An output samples the composition, so a composition that had gone
        // half-transparent would show as a half-dark wall. The buses carry
        // coverage; the picture they are combined into does not.
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };

        let mut scene = Scene::new();
        // A quarter-opacity layer on one bus and nothing anywhere else.
        scene.push(on_bus(Route::A, [1.0, 1.0, 1.0, 0.25]));
        scene.set_gains(gains(0.0, 1.0));

        let pixel = composed(&mut canvas, &scene);
        assert_eq!(pixel[3], 255, "opaque");
        close_to(pixel[0], 64, "a quarter of white over black");
    }

    /// The passes one published effect runs, at the values given.
    ///
    /// Through the manifest, exactly as a show reaches the renderer: a test
    /// that wrote the numbers itself would pass on a build whose published
    /// defaults had drifted away from what the shader does with them.
    fn effect(type_id: &str, values: &[(&str, ParameterValue)]) -> Vec<EffectPass> {
        effect_at(type_id, 1, values)
    }

    /// The passes one effect runs, at a named version.
    fn effect_at(
        type_id: &str,
        version: u32,
        values: &[(&str, ParameterValue)],
    ) -> Vec<EffectPass> {
        let mut instance = EffectInstance::new(type_id, version);
        for (key, value) in values {
            instance.parameters.insert((*key).to_owned(), value.clone());
        }

        let mut passes = Vec::new();
        crate::effects::passes_into(&[instance], &mut passes);
        passes
    }

    /// A square RGBA image whose pixels come from `pixel`.
    fn pattern(size: u32, pixel: impl Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
        let mut pixels = Vec::with_capacity((size * size * 4) as usize);
        for y in 0..size {
            for x in 0..size {
                pixels.extend_from_slice(&pixel(x, y));
            }
        }
        pixels
    }

    /// A scene holding one whole-canvas layer sampling `media`, with a chain.
    fn effected(layer: LayerId, media: MediaItemId, passes: &[EffectPass]) -> Scene {
        let mut scene = Scene::new();
        let mut quad = textured_quad_at(-1.0, -1.0, 1.0, 1.0, [1.0, 1.0, 1.0, 1.0], media);
        quad.layer_id = Some(layer);
        scene.push(quad);
        scene.set_effects(layer, passes);
        scene
    }

    /// A scene holding one whole-canvas layer of a flat colour, with a chain.
    fn effected_flat(layer: LayerId, colour: [f32; 4], passes: &[EffectPass]) -> Scene {
        let mut scene = Scene::new();
        let mut quad = quad_at(-1.0, -1.0, 1.0, 1.0, colour);
        quad.layer_id = Some(layer);
        scene.push(quad);
        scene.set_effects(layer, passes);
        scene
    }

    /// Composes a scene and reads the whole frame back, tightly packed RGBA.
    fn composed_frame(canvas: &mut VulkanOffscreenBackend, scene: &Scene, size: u32) -> Vec<u8> {
        canvas
            .compose(FrameRequest { index: 0 }, scene)
            .expect("composes");
        let mut frame = Vec::new();
        canvas
            .read_scaled(SizePx::new(size, size), &mut frame)
            .expect("reads the frame back");
        frame
    }

    #[test]
    fn the_card_draws_the_noise_the_processor_draws() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // **The whole claim of drawing generators on the card.** The same
        // generator still draws on the processor, for the export path and for
        // the tests that decide what these pictures are, so two
        // implementations of one picture exist and must not drift. The shader
        // spells out a 64-bit hash in 32-bit halves because WGSL has no such
        // type, and a single bit wrong in that arithmetic gives a picture that
        // is still noise and still wrong — which is exactly the kind of fault
        // nobody can see by looking.
        const SIDE: u32 = 64;
        let Some(mut canvas) = backend(SIDE, SIDE) else {
            return;
        };
        let clip = MediaItemId::new();

        for (scale, coloured, amount, tick) in [
            (8_u32, true, 1.0_f32, 7_u32),
            (8, false, 1.0, 7),
            (1, true, 1.0, 3),
            (16, true, 0.35, 11),
        ] {
            let seed = 0xABCD_u64;
            // The field the processor works out, handed over as it is: the
            // shader cannot do the 64-bit multiply that makes it cheaply, and
            // it is one number a frame rather than one a pixel.
            let field = seed
                .wrapping_mul(0x9E37_79B9_7F4A_7C15)
                .wrapping_add(u64::from(tick).wrapping_mul(4));
            let push = GeneratedPush {
                values: [scale as f32, amount, 0.0, 0.0],
                colour: [0.0, 0.0, 0.0, 1.0],
                colour_b: [0.0, 0.0, 0.0, 1.0],
                field: [field as u32, (field >> 32) as u32],
                size: [SIDE, SIDE],
                kind: GeneratedPush::NOISE,
                flags: if coloured { GeneratedPush::COLOURED } else { 0 },
            };
            canvas
                .set_generated(clip, SizePx::new(SIDE, SIDE), push, None)
                .expect("the card can draw a generator");
            let drawn = composed_frame(&mut canvas, &textured_canvas(clip), SIDE);

            let wanted = crate::media::noise_for_test(
                SizePx::new(SIDE, SIDE),
                scale,
                coloured,
                amount,
                tick,
                seed,
            );

            // Two implementations of the same arithmetic in floats, one of
            // them on a card that is allowed to fuse a multiply and an add.
            // Two levels is tight enough to catch a wrong constant, a wrong
            // shift or a swapped channel, all of which are off by tens.
            let mut worst = 0_i32;
            for (at, (got, want)) in drawn.iter().zip(wanted.iter()).enumerate() {
                let difference = i32::from(*got) - i32::from(*want);
                if difference.abs() > worst.abs() {
                    worst = difference;
                }
                assert!(
                    difference.abs() <= 2,
                    "scale {scale}, coloured {coloured}, amount {amount}: \
                     byte {at} is {got} on the card and {want} on the processor"
                );
            }
            println!("scale {scale}, coloured {coloured}: worst difference {worst}");
        }
    }

    #[test]
    fn the_card_draws_the_service_pattern_the_processor_draws() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // The pattern is a measurement tool: an operator reads the mode off
        // the caption and checks the mapping against the bars, so "near
        // enough" is not a standard it can be held to. The card reads the same
        // picture of the unmoving parts, drifts it by the same number of
        // pixels, lifts it by the same ramp and puts the same caption back.
        const SIDE: u32 = 128;
        let Some(mut canvas) = backend(SIDE, SIDE) else {
            return;
        };

        let mut pattern = crate::pattern::ServicePattern::new(
            SizePx::new(SIDE, SIDE),
            ovs_types::geometry::FrameRate::FPS_60,
            "OUTPUT 1",
        );
        let source_key = TextureKey::PatternSource(ovs_types::id::LayerId::new());
        let drawn_key = MediaItemId::new();
        canvas
            .set_media_texture(source_key, &pattern.source_picture())
            .expect("the unmoving parts go up");

        // Several moments, so the drift is not zero and the sweep is caught
        // both inside the picture and gone from it.
        for seconds in [0.0_f64, 0.9, 2.4, 4.8] {
            let moment = pattern.moment(seconds);
            let push = GeneratedPush {
                values: [
                    moment.shift_x as f32,
                    moment.shift_y as f32,
                    moment.sweep_start,
                    moment.sweep_trail,
                ],
                colour: [moment.label_side as f32, 0.0, 0.0, 0.0],
                colour_b: [0.0, 0.0, 0.0, 1.0],
                field: [0, 0],
                size: [SIDE, SIDE],
                kind: GeneratedPush::PATTERN,
                flags: 0,
            };
            canvas
                .set_generated(drawn_key, SizePx::new(SIDE, SIDE), push, Some(source_key))
                .expect("the card draws a pattern");
            let on_card = composed_frame(&mut canvas, &textured_canvas(drawn_key), SIDE);
            let on_processor = pattern.draw(seconds).pixels().to_vec();

            // Three levels. The processor quantises the sweep's ramp into 256
            // steps and blends in whole 255ths; the card does both in floats.
            // That is a difference of about one level, and three is still far
            // inside a wrong offset, a wrong ramp or a caption in the wrong
            // place, all of which are off by tens or hundreds.
            let mut worst = 0_i32;
            let mut at_worst = 0_usize;
            for (at, (got, want)) in on_card.iter().zip(on_processor.iter()).enumerate() {
                let difference = i32::from(*got) - i32::from(*want);
                if difference.abs() > worst.abs() {
                    worst = difference;
                    at_worst = at;
                }
            }
            assert!(
                worst.abs() <= 3,
                "at {seconds}s: byte {at_worst} is {} on the card and {} on the processor",
                on_card[at_worst],
                on_processor[at_worst]
            );
            println!("at {seconds}s: worst difference {worst}");
        }
    }

    #[test]
    fn the_card_draws_the_other_generators_the_processor_draws() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // The three that are arithmetic and nothing else. Each is drawn both
        // ways and compared byte for byte, because each has a detail that
        // would survive a glance and not a measurement: the gradient's ramp is
        // normalised across the pixel *centres*, the grid is measured from the
        // picture's origin rather than its middle, and a shape's edge is a
        // coverage ramp rather than a test.
        const SIDE: u32 = 96;
        let Some(mut canvas) = backend(SIDE, SIDE) else {
            return;
        };
        let size = SizePx::new(SIDE, SIDE);
        let key = MediaItemId::new();

        // Both opaque, and that is a property of the measurement rather than
        // of the generators. What comes back here has been through the
        // composition, which premultiplies; the processor's picture is straight
        // alpha. Comparing them is only like for like where alpha is one. The
        // correction `over` makes for a transparent bed is the same arithmetic
        // on both sides and is checked where it is written.
        let red = [1.0_f32, 0.1, 0.2, 1.0];
        let blue = [0.1_f32, 0.2, 1.0, 1.0];
        for what in [
            crate::backend::Generated::Gradient {
                from: red,
                to: blue,
                angle_deg: 0.0,
            },
            crate::backend::Generated::Gradient {
                from: red,
                to: blue,
                angle_deg: 37.0,
            },
            crate::backend::Generated::Grid {
                spacing: 12,
                line: 3,
                line_colour: red,
                background: blue,
            },
            crate::backend::Generated::Shape {
                figure: 0,
                extent: [60.0, 40.0],
                rotation_deg: 0.0,
                colour: red,
                background: blue,
            },
            crate::backend::Generated::Shape {
                figure: 1,
                extent: [50.0, 70.0],
                rotation_deg: 31.0,
                colour: red,
                background: blue,
            },
            crate::backend::Generated::Shape {
                figure: 2,
                extent: [70.0, 55.0],
                rotation_deg: 0.0,
                colour: red,
                background: blue,
            },
            // A shape with no size at all encloses nothing, so the bed is the
            // whole of the answer. Reachable only from a show written against
            // another build, and it must not be a canvas of NaN.
            crate::backend::Generated::Shape {
                figure: 0,
                extent: [0.0, 0.0],
                rotation_deg: 0.0,
                colour: red,
                background: blue,
            },
        ] {
            canvas
                .draw_generated(TextureKey::Media(key), size, what)
                .expect("the card draws it");
            let on_card = composed_frame(&mut canvas, &textured_canvas(key), SIDE);
            let on_processor = crate::media::generator_for_test(what, size);

            let mut worst = 0_i32;
            let mut at_worst = 0_usize;
            for (at, (got, want)) in on_card.iter().zip(on_processor.iter()).enumerate() {
                let difference = i32::from(*got) - i32::from(*want);
                if difference.abs() > worst.abs() {
                    worst = difference;
                    at_worst = at;
                }
            }
            // Two levels: both sides do the same arithmetic in floats, and one
            // of them is a card that may fuse a multiply and an add.
            assert!(
                worst.abs() <= 2,
                "{what:?}: byte {at_worst} is {} on the card and {} on the processor",
                on_card[at_worst],
                on_processor[at_worst]
            );
            println!("{what:?}: worst difference {worst}");
        }
    }

    #[test]
    fn a_layer_with_no_effects_costs_nothing_extra() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // The first thing the chain had to get right: a show with no effects in
        // it must take the path it took before any of this existed. Not nearly
        // the same path — the same one, with no scratch image allocated and no
        // pass recorded.
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };
        let media = MediaItemId::new();
        let layer = LayerId::new();
        let image = pattern(16, |x, y| [(x * 16) as u8, (y * 16) as u8, 64, 255]);
        canvas
            .set_media_texture(media, &rgba(&image, 16, 16))
            .expect("uploads");

        let plain = effected(layer, media, &[]);
        let frame = composed_frame(&mut canvas, &plain, 16);

        // A whole-canvas layer at full opacity over an empty bus is its own
        // picture, pixel for pixel. Nothing about the chain may come between
        // the two.
        assert_eq!(frame, image, "the layer did not draw what it was given");
        assert!(
            !canvas.has_scratch(),
            "a scene with no effects allocated a scratch image"
        );
    }

    #[test]
    fn a_disabled_effect_changes_nothing() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // And costs nothing either: switching an effect off has to put its
        // layer back on the path a layer with no chain takes, or an operator
        // who switched one off would still be paying for it.
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };
        let media = MediaItemId::new();
        let layer = LayerId::new();
        canvas
            .set_media_texture(
                media,
                &rgba(
                    &pattern(16, |x, y| [(x * 16) as u8, (y * 16) as u8, 64, 255]),
                    16,
                    16,
                ),
            )
            .expect("uploads");

        let before = composed_frame(&mut canvas, &effected(layer, media, &[]), 16);

        let mut off = EffectInstance::new("ovs.blur", 1);
        off.parameters
            .insert("radius_px".to_owned(), ParameterValue::Float(8.0));
        off.enabled = false;
        let mut passes = Vec::new();
        crate::effects::passes_into(&[off], &mut passes);
        assert!(passes.is_empty(), "a disabled effect asked for a pass");

        let after = composed_frame(&mut canvas, &effected(layer, media, &passes), 16);
        assert_eq!(after, before, "a disabled effect changed the picture");
        assert!(
            !canvas.has_scratch(),
            "a disabled effect allocated a scratch image"
        );
    }

    #[test]
    fn a_mask_keeps_as_much_of_the_picture_as_the_one_it_names_is_bright() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // Grey is the point rather than a special case, so the stencil has a
        // band of it: a mask that treated its picture as a yes or no would
        // pass on black and white alone and give a hard edge where the owner
        // asked for a soft one.
        let Some(mut canvas) = backend(32, 32) else {
            return;
        };
        let media = MediaItemId::new();
        let stencil = MediaItemId::new();
        let layer = LayerId::new();

        canvas
            .set_media_texture(media, &rgba(&pattern(32, |_, _| [255; 4]), 32, 32))
            .expect("uploads the picture");
        canvas
            .set_media_texture(
                stencil,
                &rgba(
                    &pattern(32, |x, _| {
                        let level = if x < 10 {
                            0
                        } else if x < 21 {
                            128
                        } else {
                            255
                        };
                        [level, level, level, 255]
                    }),
                    32,
                    32,
                ),
            )
            .expect("uploads the stencil");

        let passes = effect(
            "ovs.mask",
            &[("image", ParameterValue::Text(stencil.to_string()))],
        );
        let frame = composed_frame(&mut canvas, &effected(layer, media, &passes), 32);
        let light = |x: usize, y: usize| frame[(y * 32 + x) * 4];

        close_to(light(4, 16), 0, "black in the mask takes the picture away");
        close_to(light(16, 16), 128, "grey in the mask keeps half of it");
        close_to(light(27, 16), 255, "white in the mask keeps all of it");
    }

    #[test]
    fn inverting_a_mask_keeps_what_it_was_hiding() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };
        let media = MediaItemId::new();
        let stencil = MediaItemId::new();
        let layer = LayerId::new();

        canvas
            .set_media_texture(media, &rgba(&pattern(16, |_, _| [255; 4]), 16, 16))
            .expect("uploads the picture");
        canvas
            .set_media_texture(
                stencil,
                &rgba(
                    &pattern(16, |x, _| if x < 8 { [0, 0, 0, 255] } else { [255; 4] }),
                    16,
                    16,
                ),
            )
            .expect("uploads the stencil");

        let passes = effect(
            "ovs.mask",
            &[
                ("image", ParameterValue::Text(stencil.to_string())),
                ("invert", ParameterValue::Bool(true)),
            ],
        );
        let frame = composed_frame(&mut canvas, &effected(layer, media, &passes), 16);
        let light = |x: usize| frame[(8 * 16 + x) * 4];

        close_to(light(4), 255, "the half the mask was hiding is kept");
        close_to(light(12), 0, "and the half it was keeping is gone");
    }

    #[test]
    fn cutting_out_shows_the_layer_underneath_where_multiplying_blacks_it() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // **The two modes are two pictures, and this is the difference.** Both
        // look identical over black, which is what a test with one layer would
        // have been measuring: multiplying darkens the layer and leaves it
        // covering what is under it, cutting out takes the layer's own opacity
        // away so the one underneath comes through.
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };
        let stencil = MediaItemId::new();
        let under = LayerId::new();
        let over = LayerId::new();

        canvas
            .set_media_texture(
                stencil,
                &rgba(
                    &pattern(16, |x, _| if x < 8 { [0, 0, 0, 255] } else { [255; 4] }),
                    16,
                    16,
                ),
            )
            .expect("uploads the stencil");

        let scene = |mode: &str| {
            let mut scene = Scene::new();
            let mut below = quad_at(-1.0, -1.0, 1.0, 1.0, RED);
            below.layer_id = Some(under);
            scene.push(below);

            let mut above = quad_at(-1.0, -1.0, 1.0, 1.0, [1.0, 1.0, 1.0, 1.0]);
            above.layer_id = Some(over);
            scene.push(above);
            scene.set_effects(
                over,
                &effect(
                    "ovs.mask",
                    &[
                        ("image", ParameterValue::Text(stencil.to_string())),
                        ("mode", ParameterValue::Choice(mode.to_owned())),
                    ],
                ),
            );
            scene
        };

        let multiplied = composed_frame(&mut canvas, &scene("multiply"), 16);
        let cut = composed_frame(&mut canvas, &scene("cut_out"), 16);
        let at = |frame: &[u8], x: usize| {
            let first = (8 * 16 + x) * 4;
            [frame[first], frame[first + 1], frame[first + 2]]
        };

        close_to(at(&multiplied, 4)[0], 0, "multiplied, the hole is black");
        close_to(at(&multiplied, 4)[1], 0, "with nothing of the red");
        close_to(at(&cut, 4)[0], 255, "cut out, the red underneath comes up");
        close_to(at(&cut, 4)[1], 0, "and it is the red rather than white");
        close_to(
            at(&cut, 12)[1],
            255,
            "where the mask is white the top layer stays",
        );
    }

    #[test]
    fn a_mask_naming_a_picture_the_card_does_not_have_leaves_the_layer_alone() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // A mask still decoding, and a mask whose picture was deleted from the
        // library, are the same case: the layer goes on as it was rather than
        // being blanked by a stencil nobody can see.
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };
        let media = MediaItemId::new();
        let layer = LayerId::new();
        canvas
            .set_media_texture(
                media,
                &rgba(
                    &pattern(16, |x, y| [(x * 16) as u8, (y * 16) as u8, 64, 255]),
                    16,
                    16,
                ),
            )
            .expect("uploads");

        let before = composed_frame(&mut canvas, &effected(layer, media, &[]), 16);
        let passes = effect(
            "ovs.mask",
            // An item that was never uploaded. Nothing else about the effect is
            // touched, so what is being measured is the missing picture.
            &[(
                "image",
                ParameterValue::Text(MediaItemId::new().to_string()),
            )],
        );
        let after = composed_frame(&mut canvas, &effected(layer, media, &passes), 16);

        assert_eq!(
            after, before,
            "a mask with no picture on the card changed it"
        );
    }

    #[test]
    fn every_published_effect_at_its_published_defaults_changes_nothing() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // The promise an effect list makes: adding one and leaving it alone
        // cannot alter a show that was already right. Tested through the whole
        // chain rather than on the manifest, because "the default is neutral"
        // and "the shader does nothing with the default" are two claims.
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };
        let media = MediaItemId::new();
        let layer = LayerId::new();
        canvas
            .set_media_texture(
                media,
                &rgba(
                    &pattern(16, |x, y| [(x * 16) as u8, (y * 16) as u8, 64, 255]),
                    16,
                    16,
                ),
            )
            .expect("uploads");

        let before = composed_frame(&mut canvas, &effected(layer, media, &[]), 16);

        for manifest in ovs_types::config::EFFECTS {
            // The version the manifest is, not version one: a second version
            // of an effect is a second set of defaults, and this promise is
            // about each of them. The white point arrived this way, and a
            // neutral that was only nearly neutral would show up here.
            let passes = effect_at(manifest.type_id, manifest.version, &[]);
            assert!(
                !passes.is_empty(),
                "{} v{} runs nothing",
                manifest.type_id,
                manifest.version
            );

            let after = composed_frame(&mut canvas, &effected(layer, media, &passes), 16);
            assert_eq!(
                after, before,
                "{} v{} at its published defaults changed the picture",
                manifest.type_id, manifest.version
            );
        }

        assert!(
            canvas.has_scratch(),
            "the chain did not run at all, so this proves nothing"
        );
    }

    #[test]
    fn a_brightness_of_two_doubles_a_mid_grey() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };
        let layer = LayerId::new();
        let passes = effect(
            "ovs.colour_correct",
            &[("brightness", ParameterValue::Float(2.0))],
        );

        let scene = effected_flat(layer, [0.25, 0.25, 0.25, 1.0], &passes);
        canvas
            .compose(FrameRequest { index: 0 }, &scene)
            .expect("composes");

        let pixel = canvas.read_pixel(8, 8).expect("reads");
        close_to(pixel[0], 128, "a quarter of white, doubled");
        assert_eq!(pixel[3], 255, "and the composition is still opaque");
    }

    #[test]
    fn a_saturation_of_zero_leaves_a_colour_at_its_own_luma() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // Rec. 709 weights, which is what the content is graded in: pure red is
        // a little over a fifth as bright as white, not a third.
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };
        let layer = LayerId::new();
        let passes = effect(
            "ovs.colour_correct",
            &[("saturation", ParameterValue::Float(0.0))],
        );

        let scene = effected_flat(layer, [1.0, 0.0, 0.0, 1.0], &passes);
        canvas
            .compose(FrameRequest { index: 0 }, &scene)
            .expect("composes");

        let pixel = canvas.read_pixel(8, 8).expect("reads");
        close_to(pixel[0], 54, "red at its own luma");
        close_to(pixel[1], 54, "and grey means all three the same");
        close_to(pixel[2], 54, "and grey means all three the same");
    }

    #[test]
    fn a_tint_at_full_amount_takes_the_colour_it_was_given() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };
        let layer = LayerId::new();
        let passes = effect(
            "ovs.tint",
            &[
                ("colour", ParameterValue::Color([1.0, 0.0, 0.0, 1.0])),
                ("amount", ParameterValue::Float(1.0)),
                ("preserve_luma", ParameterValue::Bool(false)),
            ],
        );

        let scene = effected_flat(layer, [0.2, 0.2, 0.2, 1.0], &passes);
        canvas
            .compose(FrameRequest { index: 0 }, &scene)
            .expect("composes");

        assert_eq!(canvas.read_pixel(8, 8).expect("reads"), [255, 0, 0, 255]);
    }

    #[test]
    fn keeping_the_brightness_takes_only_the_tints_direction() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // A fifth-grey tinted fully red stays a fifth as bright as white, so
        // the red channel comes up to where red is that bright — not to full.
        // This is what stops a tint turning a subtle look into a floodlight.
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };
        let layer = LayerId::new();
        let passes = effect(
            "ovs.tint",
            &[
                ("colour", ParameterValue::Color([1.0, 0.0, 0.0, 1.0])),
                ("amount", ParameterValue::Float(1.0)),
                ("preserve_luma", ParameterValue::Bool(true)),
            ],
        );

        let scene = effected_flat(layer, [0.2, 0.2, 0.2, 1.0], &passes);
        canvas
            .compose(FrameRequest { index: 0 }, &scene)
            .expect("composes");

        let pixel = canvas.read_pixel(8, 8).expect("reads");
        close_to(pixel[0], 240, "red at the picture's own luma");
        close_to(pixel[1], 0, "and nothing else");
        close_to(pixel[2], 0, "and nothing else");
    }

    #[test]
    fn a_sixteen_pixel_block_makes_a_sixteen_pixel_neighbourhood_one_colour() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // Measured in composition pixels rather than as a fraction of the
        // surface, which is what makes a look survive being mapped onto a wall
        // of another size: a 32-wide canvas takes two blocks, not sixteen.
        let Some(mut canvas) = backend(32, 32) else {
            return;
        };
        let media = MediaItemId::new();
        let layer = LayerId::new();
        // A ramp across, so a block that was not averaged would show it.
        canvas
            .set_media_texture(
                media,
                &rgba(&pattern(32, |x, _| [(x * 8) as u8, 0, 0, 255]), 32, 32),
            )
            .expect("uploads");

        let passes = effect("ovs.pixelate", &[("block_px", ParameterValue::Float(16.0))]);
        let frame = composed_frame(&mut canvas, &effected(layer, media, &passes), 32);
        let red = |x: usize, y: usize| frame[(y * 32 + x) * 4];

        for (x, y) in [(0, 0), (7, 5), (15, 15), (3, 31)] {
            close_to(
                red(x, y),
                60,
                &format!("({x}, {y}) is the mean of the left block"),
            );
        }
        for (x, y) in [(16, 0), (23, 5), (31, 31)] {
            close_to(
                red(x, y),
                188,
                &format!("({x}, {y}) is the mean of the right block"),
            );
        }
    }

    #[test]
    fn a_glitch_moves_because_a_pass_can_read_the_clock() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // **The whole of what the clock bought.** Without it every effect here
        // is a function of position alone and nothing can travel, flicker or
        // roll. This proves the chain end to end: a field on the target, the
        // four bytes of the push constant that used to be padding, and a
        // shader branch that reads them.
        let Some(mut canvas) = backend(64, 64) else {
            return;
        };
        let media = MediaItemId::new();
        let layer = LayerId::new();
        canvas
            .set_media_texture(
                media,
                &rgba(&pattern(64, |_, _| [128, 128, 128, 255]), 64, 64),
            )
            .expect("uploads");

        let passes = effect_at(
            "ovs.tv_glitch",
            1,
            &[("amount", ParameterValue::Float(1.0))],
        );

        canvas.set_effect_time(0.0);
        let first = composed_frame(&mut canvas, &effected(layer, media, &passes), 64);
        canvas.set_effect_time(1.0);
        let later = composed_frame(&mut canvas, &effected(layer, media, &passes), 64);

        assert_ne!(first, later, "a second later and not one pixel moved");

        // And it is doing something at all, rather than two identical
        // nothings: the flat grey it was given is not what came back.
        let untouched = composed_frame(&mut canvas, &effected(layer, media, &[]), 64);
        assert_ne!(first, untouched, "the glitch left the picture alone");
    }

    #[test]
    fn losing_the_vertical_hold_moves_the_picture_up_and_down_and_nowhere_else() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // **Vertically, and only vertically** (owner, 2026-10-13: "ein
        // kompletter vertikaler zufaelliger versatz ... also dass man wirklich
        // out of sync waere"). Everything else this effect does moves the
        // picture sideways, so a displacement that leaked into x would look
        // like more of what was already there and pass unnoticed.
        //
        // The picture is a vertical ramp: every row is one flat colour and no
        // two rows are alike. A purely vertical displacement reorders rows and
        // leaves each of them flat; anything sideways breaks that.
        let Some(mut canvas) = backend(64, 64) else {
            return;
        };
        let media = MediaItemId::new();
        let layer = LayerId::new();
        canvas
            .set_media_texture(
                media,
                // Opaque, and the ramp only in the colour: an alpha that
                // ramped as well would be multiplied into the colour as the
                // layer is composited, and the row number read back would be
                // the square of the one that went in.
                &rgba(
                    &pattern(64, |_, y| {
                        [(y * 4) as u8, (y * 4) as u8, (y * 4) as u8, 255]
                    }),
                    64,
                    64,
                ),
            )
            .expect("uploads");

        // The hold on its own: the other three faults are at nought, so what
        // is measured is this control and not the ones beside it.
        let passes = effect_at(
            "ovs.tv_glitch",
            2,
            &[
                ("amount", ParameterValue::Float(1.0)),
                ("tear", ParameterValue::Float(0.0)),
                ("noise", ParameterValue::Float(0.0)),
                ("roll", ParameterValue::Float(0.0)),
                ("vertical_hold", ParameterValue::Float(1.0)),
            ],
        );

        let scene = effected(layer, media, &passes);
        let row_of = |frame: &[u8], y: usize| frame[y * 64 * 4];
        let flat_rows = |frame: &[u8]| {
            (0..64).all(|y| {
                let first = row_of(frame, y);
                (0..64).all(|x| frame[(y * 64 + x) * 4].abs_diff(first) <= 1)
            })
        };

        let mut seen = std::collections::BTreeSet::new();
        let mut moved_by = 0_usize;
        for tick in 0..12 {
            canvas.set_effect_time(tick as f32 * 0.4);
            let frame = composed_frame(&mut canvas, &scene, 64);
            assert!(
                flat_rows(&frame),
                "a row stopped being one colour, so the displacement was not purely vertical"
            );
            // Where the middle row's colour came from, as a row number: the
            // ramp makes that readable straight off the pixel.
            let from = usize::from(row_of(&frame, 32)) / 4;
            moved_by = moved_by.max(from.abs_diff(32));
            seen.insert(from);
        }

        assert!(
            seen.len() >= 3,
            "the picture sat in the same place all the way through: {seen:?}"
        );
        assert!(
            moved_by >= 2,
            "nothing was thrown far enough to see: {moved_by} rows"
        );
        // Twenty per cent of sixty-four rows is thirteen, and a band may jump
        // half as far again on top of that. Much past that and this is not the
        // owner's twenty per cent any more.
        assert!(
            moved_by <= 20,
            "thrown further than the control allows: {moved_by} rows"
        );
    }

    #[test]
    fn a_glitch_saved_under_another_number_draws_the_same_glitch() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // There is one TV Glitch (owner, 2026-10-18: "TV Glitch V2 ist TV
        // Glitch"). A show saved while it was numbered two draws exactly what
        // one numbered one does with the same settings, on the card.
        let Some(mut canvas) = backend(32, 32) else {
            return;
        };
        let media = MediaItemId::new();
        let layer = LayerId::new();
        canvas
            .set_media_texture(
                media,
                &rgba(
                    &pattern(32, |x, y| [(x * 8) as u8, (y * 8) as u8, 90, 255]),
                    32,
                    32,
                ),
            )
            .expect("uploads");

        let settings = [
            ("amount", ParameterValue::Float(1.0)),
            ("vertical_hold", ParameterValue::Float(0.4)),
        ];
        let one = effect_at("ovs.tv_glitch", 1, &settings);
        let two = effect_at("ovs.tv_glitch", 2, &settings);

        canvas.set_effect_time(2.5);
        let drawn_by_one = composed_frame(&mut canvas, &effected(layer, media, &one), 32);
        canvas.set_effect_time(2.5);
        let drawn_by_two = composed_frame(&mut canvas, &effected(layer, media, &two), 32);

        assert_eq!(
            drawn_by_one, drawn_by_two,
            "the number a show carries changed the picture"
        );
    }

    #[test]
    fn mirroring_folds_the_half_it_was_told_to_keep() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // Which half is kept is the whole of the control: a logo in the left
        // third and one in the right third survive opposite choices, so "fold"
        // on its own would be half an answer.
        let Some(mut canvas) = backend(32, 32) else {
            return;
        };
        let media = MediaItemId::new();
        let layer = LayerId::new();
        // A ramp across, so a fold reads as a mirrored ramp rather than as
        // nothing happening.
        canvas
            .set_media_texture(
                media,
                &rgba(&pattern(32, |x, _| [(x * 8) as u8, 0, 0, 255]), 32, 32),
            )
            .expect("uploads");

        let folded = |canvas: &mut VulkanOffscreenBackend, side: &str| {
            let passes = effect_at(
                "ovs.mirror",
                1,
                &[("horizontal", ParameterValue::Choice(side.to_owned()))],
            );
            composed_frame(canvas, &effected(layer, media, &passes), 32)
        };
        let red = |frame: &[u8], x: usize| frame[(16 * 32 + x) * 4];

        let keep_left = folded(&mut canvas, "left");
        close_to(red(&keep_left, 0), 0, "the kept edge is where it was");
        close_to(red(&keep_left, 31), 0, "and the far edge is its mirror");

        let keep_right = folded(&mut canvas, "right");
        close_to(
            red(&keep_right, 31),
            248,
            "the other choice keeps the other half",
        );
        close_to(red(&keep_right, 0), 248, "and folds it the other way");
    }

    #[test]
    fn colorize_with_no_saturation_leaves_the_picture_in_monochrome() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // The bottom of the control, and a useful place to stop rather than a
        // dead position: the luma is kept and no colour is painted back.
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };
        let media = MediaItemId::new();
        let layer = LayerId::new();
        canvas
            .set_media_texture(media, &rgba(&pattern(16, |_, _| [255, 0, 0, 255]), 16, 16))
            .expect("uploads");

        let passes = effect_at(
            "ovs.colorize",
            1,
            &[
                ("amount", ParameterValue::Float(1.0)),
                ("saturation", ParameterValue::Float(0.0)),
            ],
        );
        let frame = composed_frame(&mut canvas, &effected(layer, media, &passes), 16);
        let at = (8 * 16 + 8) * 4;

        // Rec. 709: a full red is a little over a fifth of the brightness of
        // white, and all three channels come back carrying that.
        close_to(frame[at], 54, "red becomes its own luma");
        close_to(frame[at + 1], 54, "and so does green");
        close_to(frame[at + 2], 54, "and blue");
    }

    #[test]
    fn an_rgb_white_level_moves_one_channel_and_leaves_the_others() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // Six controls that each touch one channel. One that reached two would
        // be useless for the thing this exists for, which is matching two
        // panel batches to each other.
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };
        let media = MediaItemId::new();
        let layer = LayerId::new();
        canvas
            .set_media_texture(media, &rgba(&pattern(16, |_, _| [64, 64, 64, 255]), 16, 16))
            .expect("uploads");

        let passes = effect_at(
            "ovs.rgb_correct",
            1,
            &[("gain_r", ParameterValue::Float(2.0))],
        );
        let frame = composed_frame(&mut canvas, &effected(layer, media, &passes), 16);
        let at = (8 * 16 + 8) * 4;

        close_to(frame[at], 128, "the red white level doubled the red");
        close_to(frame[at + 1], 64, "and left the green alone");
        close_to(frame[at + 2], 64, "and the blue");
    }

    #[test]
    fn splitting_the_channels_moves_red_and_blue_opposite_ways() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // An edge is where this is visible at all: either side of one, red has
        // already crossed and blue has not.
        let Some(mut canvas) = backend(32, 32) else {
            return;
        };
        let media = MediaItemId::new();
        let layer = LayerId::new();
        // Black on the left, white on the right, with the edge at sixteen.
        canvas
            .set_media_texture(
                media,
                &rgba(
                    &pattern(32, |x, _| {
                        if x < 16 {
                            [0, 0, 0, 255]
                        } else {
                            [255, 255, 255, 255]
                        }
                    }),
                    32,
                    32,
                ),
            )
            .expect("uploads");

        let passes = effect_at(
            "ovs.rgb_split",
            1,
            &[("offset_px", ParameterValue::Float(4.0))],
        );
        let frame = composed_frame(&mut canvas, &effected(layer, media, &passes), 32);
        // Two pixels left of the edge: red reads from four to the right, which
        // is across it; blue reads from four to the left, which is not.
        let at = (16 * 32 + 14) * 4;

        close_to(frame[at], 255, "red has crossed the edge");
        close_to(frame[at + 2], 0, "and blue has not");
    }

    #[test]
    fn a_split_leaves_the_outline_of_a_picture_with_transparency() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // The owner, 2026-10-18: with graphics or clips that have alpha "werden
        // die RGB Kanäle nicht korrekt geteilt, weil Alpha ja als Maske alles
        // rausstanzt". Nothing on the left, white on the right: the red that
        // crosses the edge has to show where the picture itself is clear.
        let Some(mut canvas) = backend(32, 32) else {
            return;
        };
        let media = MediaItemId::new();
        let layer = LayerId::new();
        canvas
            .set_media_texture(
                media,
                &rgba(
                    &pattern(32, |x, _| {
                        if x < 16 {
                            [0, 0, 0, 0]
                        } else {
                            [255, 255, 255, 255]
                        }
                    }),
                    32,
                    32,
                ),
            )
            .expect("uploads");

        let passes = effect_at(
            "ovs.rgb_split",
            1,
            &[("offset_px", ParameterValue::Float(4.0))],
        );
        let frame = composed_frame(&mut canvas, &effected(layer, media, &passes), 32);
        // Two pixels left of the edge, where the picture is clear: red reads
        // from across the edge, green and blue from the clear side. Over the
        // canvas's black that is red and nothing else.
        let outside = (16 * 32 + 14) * 4;
        close_to(frame[outside], 255, "red has left the outline");
        close_to(frame[outside + 1], 0, "green has not");
        close_to(frame[outside + 2], 0, "nor blue");
        // Two pixels inside the edge: blue reads from the clear side, so the
        // white loses its blue and nothing else.
        let inside = (16 * 32 + 18) * 4;
        close_to(frame[inside], 255, "red is there inside");
        close_to(frame[inside + 1], 255, "and green");
        close_to(frame[inside + 2], 0, "and blue has gone");
    }

    #[test]
    fn switching_invert_on_turns_every_colour_into_its_opposite() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // The owner, 2026-10-16: "Invert Color. mit einfachen on/off auch per
        // dmx." Measured on the card, at three colours rather than one, so
        // that a pass which merely darkened the picture could not pass for a
        // negative.
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };
        let media = MediaItemId::new();
        let layer = LayerId::new();
        canvas
            .set_media_texture(
                media,
                &rgba(&pattern(16, |_, _| [200, 60, 10, 255]), 16, 16),
            )
            .expect("uploads");

        let at = |canvas: &mut VulkanOffscreenBackend, on: bool| {
            let passes = effect_at("ovs.invert", 1, &[("on", ParameterValue::Bool(on))]);
            let frame = composed_frame(canvas, &effected(layer, media, &passes), 16);
            let at_middle = (8 * 16 + 8) * 4;
            [
                frame[at_middle],
                frame[at_middle + 1],
                frame[at_middle + 2],
                frame[at_middle + 3],
            ]
        };

        let off = at(&mut canvas, false);
        let on = at(&mut canvas, true);

        close_to(off[0], 200, "switched off it is the picture");
        close_to(off[1], 60, "unchanged");
        close_to(off[2], 10, "in every channel");

        close_to(on[0], 55, "switched on, the red is its opposite");
        close_to(on[1], 195, "and the green");
        close_to(on[2], 245, "and the blue");
        assert_eq!(on[3], off[3], "and the alpha is left where it was");
    }

    #[test]
    fn edges_keep_the_outline_in_its_colour_and_let_the_rest_through() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // The owner, 2026-10-17: "nur Outlines … als eine Farbe", and "sollte
        // transparent sein". A picture black on the left and white on the
        // right has one edge, down the middle, over a red layer.
        //
        // **Transparent has to be proved with something underneath.** Over the
        // composition's own black, a pixel the effect left transparent and one
        // it painted opaque black are the same pixel — which is what this test
        // measured the first time it was written, and passed for the wrong
        // reason. So: the flat white must show the *red* layer below it.
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };
        let media = MediaItemId::new();
        let under = LayerId::new();
        let over = LayerId::new();
        canvas
            .set_media_texture(
                media,
                &rgba(
                    &pattern(16, |x, _| if x < 8 { [0, 0, 0, 255] } else { [255; 4] }),
                    16,
                    16,
                ),
            )
            .expect("uploads");

        let scene = |amount: f64| {
            let mut scene = Scene::new();
            let mut below = quad_at(-1.0, -1.0, 1.0, 1.0, RED);
            below.layer_id = Some(under);
            scene.push(below);

            let mut above = textured_quad_at(-1.0, -1.0, 1.0, 1.0, [1.0, 1.0, 1.0, 1.0], media);
            above.layer_id = Some(over);
            scene.push(above);
            scene.set_effects(
                over,
                &effect_at(
                    "ovs.edges",
                    1,
                    &[
                        ("amount", ParameterValue::Float(amount)),
                        ("threshold", ParameterValue::Float(0.2)),
                        ("width_px", ParameterValue::Float(1.0)),
                        ("colour", ParameterValue::Color([0.0, 1.0, 0.0, 1.0])),
                    ],
                ),
            );
            scene
        };
        let at = |frame: &[u8], x: usize| {
            let first = (8 * 16 + x) * 4;
            [frame[first], frame[first + 1], frame[first + 2]]
        };

        let on = composed_frame(&mut canvas, &scene(1.0), 16);
        close_to(at(&on, 8)[1], 255, "on the edge, the outline is green");
        close_to(at(&on, 8)[0], 0, "and covers the red below it");
        close_to(
            at(&on, 13)[0],
            255,
            "on the flat white there is no outline, and the red comes through",
        );
        close_to(
            at(&on, 13)[1],
            0,
            "the red, not the white the picture had there",
        );
        close_to(at(&on, 3)[0], 255, "and the same on the flat black");

        let off = composed_frame(&mut canvas, &scene(0.0), 16);
        close_to(
            at(&off, 13)[0],
            255,
            "at amount nought the picture is back: white",
        );
        close_to(at(&off, 13)[1], 255, "in every channel, covering the red");
        close_to(at(&off, 3)[0], 0, "and its black covers the red too");
    }

    #[test]
    fn a_wider_line_reaches_further_from_the_edge() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // Why the width exists: a one-pixel line spread over LED panels with a
        // coarse pitch disappears. Two pixels from the edge the narrow line has
        // already ended; the wide one has not.
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };
        let media = MediaItemId::new();
        let layer = LayerId::new();
        canvas
            .set_media_texture(
                media,
                &rgba(
                    &pattern(16, |x, _| if x < 8 { [0, 0, 0, 255] } else { [255; 4] }),
                    16,
                    16,
                ),
            )
            .expect("uploads");

        // Brightness rather than alpha: the composition is opaque, so the
        // outline is read as the white line against the black it leaves.
        let light_at = |canvas: &mut VulkanOffscreenBackend, width: f64, x: usize| {
            let passes = effect_at(
                "ovs.edges",
                1,
                &[
                    ("amount", ParameterValue::Float(1.0)),
                    ("threshold", ParameterValue::Float(0.2)),
                    ("width_px", ParameterValue::Float(width)),
                ],
            );
            let frame = composed_frame(canvas, &effected(layer, media, &passes), 16);
            frame[(8 * 16 + x) * 4]
        };

        close_to(
            light_at(&mut canvas, 1.0, 11),
            0,
            "three pixels out, a thin line has ended",
        );
        close_to(
            light_at(&mut canvas, 4.0, 11),
            255,
            "and a wide one is still there",
        );
    }

    /// One echo, made once, so every frame of a test is the same instance.
    ///
    /// Not [`effect_at`], which makes a new instance each call — and an echo's
    /// memory belongs to its instance, so a test calling that every frame
    /// would be testing a new echo every frame.
    fn echo(length_s: f64, mode: &str) -> Vec<EffectPass> {
        let mut instance = EffectInstance::new("ovs.echo", 1);
        instance
            .parameters
            .insert("length_s".to_owned(), ParameterValue::Float(length_s));
        instance
            .parameters
            .insert("mode".to_owned(), ParameterValue::Choice(mode.to_owned()));
        let mut passes = Vec::new();
        crate::effects::passes_into(&[instance], &mut passes);
        passes
    }

    /// A red layer, and over it a white one between two edges, carrying `passes`.
    fn echoed(layer: LayerId, left: f32, right: f32, passes: &[EffectPass]) -> Scene {
        let mut scene = Scene::new();
        scene.push(quad_at(-1.0, -1.0, 1.0, 1.0, RED));
        let mut quad = quad_at(left, -1.0, right, 1.0, [1.0, 1.0, 1.0, 1.0]);
        quad.layer_id = Some(layer);
        scene.push(quad);
        scene.set_effects(layer, passes);
        scene
    }

    /// The first three channels of the middle row's pixel at `x`.
    fn middle_at(frame: &[u8], x: usize) -> [u8; 3] {
        let first = (8 * 16 + x) * 4;
        [frame[first], frame[first + 1], frame[first + 2]]
    }

    #[test]
    fn an_echo_leaves_a_trail_that_fades_into_what_is_underneath_and_ends() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // The owner, 2026-10-17: the newest frame in front, the ones before it
        // "immer Dunkler" and "verschwinden im Hintergrund". A white layer on
        // the left for one frame, then on the right: the left is where it was
        // a moment ago, and over a red layer that has to be the white
        // dissolving into the red — not a dark veil over it, and not still
        // there a long time later.
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };
        canvas.set_effect_frame(1.0 / 60.0);
        let layer = LayerId::new();
        // Half a second, which is thirty frames at sixty: one frame keeps
        // two to the minus eight-thirtieths, about 0.83, of the trail.
        let passes = echo(0.5, "behind");

        composed_frame(&mut canvas, &echoed(layer, -1.0, 0.0, &passes), 16);
        let after_one = composed_frame(&mut canvas, &echoed(layer, 0.0, 1.0, &passes), 16);

        close_to(
            middle_at(&after_one, 12)[1],
            255,
            "the newest frame, white, in front",
        );
        let trail = middle_at(&after_one, 3);
        close_to(
            trail[0],
            255,
            "where it was, the red under it and the white over it",
        );
        close_to(trail[1], 212, "five sixths of the white, one frame on");
        close_to(trail[2], 212, "in green and in blue alike");

        let mut last = after_one;
        for _ in 0..90 {
            last = composed_frame(&mut canvas, &echoed(layer, 0.0, 1.0, &passes), 16);
        }
        assert_eq!(
            middle_at(&last, 3),
            [255, 0, 0],
            "three lengths later the trail has to be gone to the last bit, leaving the red"
        );
        for x in 0..8 {
            assert_eq!(
                middle_at(&last, x),
                [255, 0, 0],
                "a level left standing at {x} is a ghost that never fades"
            );
        }
    }

    #[test]
    fn an_echo_at_length_nought_remembers_nothing() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // The published default, and it must be the picture untouched even
        // though the memory is being kept: the layer moves and the place it
        // left is the red underneath at once.
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };
        canvas.set_effect_frame(1.0 / 60.0);
        let layer = LayerId::new();
        let passes = echo(0.0, "behind");

        composed_frame(&mut canvas, &echoed(layer, -1.0, 0.0, &passes), 16);
        let moved = composed_frame(&mut canvas, &echoed(layer, 0.0, 1.0, &passes), 16);
        assert_eq!(middle_at(&moved, 3), [255, 0, 0], "no trail at nought");
        assert_eq!(middle_at(&moved, 12), [255, 255, 255]);
    }

    #[test]
    fn an_echo_that_was_not_drawn_starts_again_from_nothing() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // A layer faded out and back up must not bring back a trail of what it
        // showed when it went: that is a picture from minutes ago, and it
        // looks deliberate. One frame on the left, one frame with the layer
        // gone, then the right — and the left must be the red alone.
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };
        canvas.set_effect_frame(1.0 / 60.0);
        let layer = LayerId::new();
        let passes = echo(2.0, "behind");

        composed_frame(&mut canvas, &echoed(layer, -1.0, 0.0, &passes), 16);
        let mut gone = Scene::new();
        gone.push(quad_at(-1.0, -1.0, 1.0, 1.0, RED));
        composed_frame(&mut canvas, &gone, 16);
        let back = composed_frame(&mut canvas, &echoed(layer, 0.0, 1.0, &passes), 16);

        assert_eq!(
            middle_at(&back, 3),
            [255, 0, 0],
            "the trail came back from before the layer went"
        );
        close_to(
            middle_at(&back, 12)[1],
            255,
            "and the layer itself is drawn",
        );
    }

    #[test]
    fn lighten_leaves_a_trail_on_a_full_picture_and_behind_does_not() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // Why there are two modes. On a full-frame picture the newest frame
        // covers everything, so behind shows no trail at all; lighten keeps
        // whichever is brighter, so white then black leaves the white fading.
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };
        canvas.set_effect_frame(1.0 / 60.0);

        let after = |canvas: &mut VulkanOffscreenBackend, mode: &str| {
            let layer = LayerId::new();
            let passes = echo(0.5, mode);
            composed_frame(
                canvas,
                &effected_flat(layer, [1.0, 1.0, 1.0, 1.0], &passes),
                16,
            );
            let frame = composed_frame(
                canvas,
                &effected_flat(layer, [0.0, 0.0, 0.0, 1.0], &passes),
                16,
            );
            middle_at(&frame, 8)
        };

        assert_eq!(
            after(&mut canvas, "behind"),
            [0, 0, 0],
            "behind: the newest frame is opaque and in front of everything"
        );
        let lightened = after(&mut canvas, "lighten");
        close_to(
            lightened[0],
            212,
            "lighten: the white of a frame ago, fading",
        );
        close_to(lightened[1], 212, "in every channel");
        close_to(lightened[2], 212, "alike");
    }

    #[test]
    fn more_echoes_than_the_canvas_remembers_draw_their_layers_untouched() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // The ceiling is a number of pictures, and a show that goes past it
        // must still draw every layer — the ones over it without a trail —
        // rather than failing the frame or drawing a white one.
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };
        canvas.set_effect_frame(1.0 / 60.0);

        let layers: Vec<(LayerId, Vec<EffectPass>)> = (0..=MAX_ECHOES)
            .map(|_| (LayerId::new(), echo(1.0, "behind")))
            .collect();
        let scene = |left: f32, right: f32| {
            let mut scene = Scene::new();
            scene.push(quad_at(-1.0, -1.0, 1.0, 1.0, RED));
            for (layer, passes) in &layers {
                let mut quad = quad_at(left, -1.0, right, 1.0, [1.0, 1.0, 1.0, 1.0]);
                quad.layer_id = Some(*layer);
                scene.push(quad);
                scene.set_effects(*layer, passes);
            }
            scene
        };

        composed_frame(&mut canvas, &scene(-1.0, 0.0), 16);
        let moved = composed_frame(&mut canvas, &scene(0.0, 1.0), 16);
        assert_eq!(
            middle_at(&moved, 12),
            [255, 255, 255],
            "every layer is drawn"
        );
        assert_eq!(
            canvas.target.histories.len(),
            MAX_ECHOES,
            "and no more pictures than the ceiling"
        );
    }

    #[test]
    fn warming_a_picture_lifts_the_red_and_drops_the_blue_without_dimming_it() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // What temperature means, measured on the card rather than argued
        // about: below the neutral the picture goes warm, above it goes cool,
        // and neither changes how bright it is — an operator reaching for
        // temperature is not asking about exposure.
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };
        let media = MediaItemId::new();
        let layer = LayerId::new();
        canvas
            .set_media_texture(
                media,
                &rgba(&pattern(16, |_, _| [128, 128, 128, 255]), 16, 16),
            )
            .expect("uploads");

        let at = |canvas: &mut VulkanOffscreenBackend, kelvin: f32| {
            let passes = effect_at(
                "ovs.colour_correct",
                2,
                &[("temperature_k", ParameterValue::Float(f64::from(kelvin)))],
            );
            let frame = composed_frame(canvas, &effected(layer, media, &passes), 16);
            let at_middle = (8 * 16 + 8) * 4;
            [frame[at_middle], frame[at_middle + 1], frame[at_middle + 2]]
        };

        let neutral = at(&mut canvas, 6500.0);
        let warm = at(&mut canvas, 3000.0);
        let cool = at(&mut canvas, 9000.0);

        close_to(neutral[0], 128, "neutral leaves the red where it was");
        close_to(neutral[2], 128, "and the blue");

        assert!(warm[0] > neutral[0], "warm lifts the red: {warm:?}");
        assert!(warm[2] < neutral[2], "and drops the blue: {warm:?}");
        assert!(cool[0] < neutral[0], "cool does the opposite: {cool:?}");
        assert!(cool[2] > neutral[2], "{cool:?}");

        // Luma held, which is what keeps this from being a second brightness
        // control. Rec. 709 weights, the same ones the shader uses.
        let luma = |rgb: [u8; 3]| {
            0.2126 * f32::from(rgb[0]) + 0.7152 * f32::from(rgb[1]) + 0.0722 * f32::from(rgb[2])
        };
        for (what, rgb) in [("warm", warm), ("cool", cool)] {
            let difference = (luma(rgb) - luma(neutral)).abs();
            assert!(
                difference < 4.0,
                "{what} changed the brightness by {difference}"
            );
        }
    }

    #[test]
    fn a_crop_takes_exactly_as_many_pixels_off_each_edge_as_it_was_given() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // Counted on the card rather than reasoned about: an edge out by one,
        // or one measuring a fraction of the surface instead of pixels, would
        // still look exactly like a crop.
        let Some(mut canvas) = backend(32, 32) else {
            return;
        };
        let media = MediaItemId::new();
        let layer = LayerId::new();
        canvas
            .set_media_texture(media, &rgba(&pattern(32, |_, _| [255, 0, 0, 255]), 32, 32))
            .expect("uploads");

        // Four different numbers, because four edges reading one of them would
        // pass a test that used the same value twice.
        let passes = effect(
            "ovs.crop",
            &[
                ("left_px", ParameterValue::Float(4.0)),
                ("right_px", ParameterValue::Float(2.0)),
                ("top_px", ParameterValue::Float(6.0)),
                ("bottom_px", ParameterValue::Float(0.0)),
            ],
        );
        let frame = composed_frame(&mut canvas, &effected(layer, media, &passes), 32);
        let red = |x: usize, y: usize| frame[(y * 32 + x) * 4];

        close_to(red(3, 20), 0, "the column before the left edge is gone");
        close_to(red(4, 20), 255, "and the column at it is kept");
        close_to(red(20, 5), 0, "the row above the top edge is gone");
        close_to(red(20, 6), 255, "and the row at it is kept");
        close_to(red(29, 20), 255, "the last column the right edge keeps");
        close_to(red(30, 20), 0, "and the two beyond it are gone");
        close_to(red(31, 20), 0, "both of them");
        close_to(red(20, 31), 255, "nothing taken off the bottom");
    }

    #[test]
    fn a_blur_spreads_a_bright_block_into_its_neighbours_and_keeps_the_light() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // Both halves matter. A blur that spread light without conserving it
        // would dim every soft-edged look on the wall, and one that conserved
        // it without spreading would be doing nothing at all.
        let Some(mut canvas) = backend(32, 32) else {
            return;
        };
        let media = MediaItemId::new();
        let layer = LayerId::new();
        let block = |x: u32, y: u32| (12..20).contains(&x) && (12..20).contains(&y);
        canvas
            .set_media_texture(
                media,
                &rgba(
                    &pattern(32, |x, y| {
                        if block(x, y) {
                            [255, 255, 255, 255]
                        } else {
                            [0, 0, 0, 255]
                        }
                    }),
                    32,
                    32,
                ),
            )
            .expect("uploads");

        let sharp = composed_frame(&mut canvas, &effected(layer, media, &[]), 32);
        let passes = effect("ovs.blur", &[("radius_px", ParameterValue::Float(4.0))]);
        let blurred = composed_frame(&mut canvas, &effected(layer, media, &passes), 32);

        let red = |frame: &[u8], x: usize, y: usize| u32::from(frame[(y * 32 + x) * 4]);
        assert_eq!(red(&sharp, 10, 16), 0, "nothing was there to begin with");
        assert!(
            red(&blurred, 10, 16) > 0,
            "the blur did not reach two pixels outside the block"
        );
        assert!(
            red(&blurred, 16, 16) > 128,
            "the middle of the block went dark"
        );

        let total = |frame: &[u8]| -> u32 {
            (0..32)
                .map(|y| (0..32).map(move |x| red(frame, x, y)).sum::<u32>())
                .sum()
        };
        let before = total(&sharp);
        let after = total(&blurred);
        assert!(
            after.abs_diff(before) * 10 < before,
            "the light was not conserved: {before} became {after}"
        );
    }

    #[test]
    fn a_chain_applies_its_effects_in_the_order_the_show_listed_them() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // Brightening and then stretching contrast is not the same picture as
        // stretching and then brightening, which is exactly why the order is
        // the show's and not the renderer's.
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };
        let layer = LayerId::new();
        let brightness = effect(
            "ovs.colour_correct",
            &[("brightness", ParameterValue::Float(3.0))],
        );
        let contrast = effect(
            "ovs.colour_correct",
            &[("contrast", ParameterValue::Float(2.0))],
        );

        let mut listed = brightness.clone();
        listed.extend_from_slice(&contrast);
        let scene = effected_flat(layer, [0.2, 0.2, 0.2, 1.0], &listed);
        canvas
            .compose(FrameRequest { index: 0 }, &scene)
            .expect("composes");
        let forwards = canvas.read_pixel(8, 8).expect("reads");

        let mut swapped = contrast;
        swapped.extend_from_slice(&brightness);
        let scene = effected_flat(layer, [0.2, 0.2, 0.2, 1.0], &swapped);
        canvas
            .compose(FrameRequest { index: 0 }, &scene)
            .expect("composes");
        let backwards = canvas.read_pixel(8, 8).expect("reads");

        close_to(forwards[0], 178, "brightened, then stretched about grey");
        assert_eq!(
            backwards[0], 0,
            "stretched about grey first, a fifth of white goes below black"
        );
        assert_ne!(forwards, backwards, "the order made no difference");
    }

    #[test]
    fn a_chain_does_not_change_how_its_layer_is_composited() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // The chain ends where the layer would have begun: what comes out of it
        // is composited with the layer's own opacity and blend mode, into the
        // layer's own bus. Nothing about that is the chain's business.
        //
        // Within rounding rather than byte for byte, and only here. A chain
        // puts the layer through one more eight-bit image than an uneffected
        // layer passes through, so a colour that does not land on the
        // eight-bit grid — as a flat quad's does not, and as a frame of media
        // always does — can come back one step away from where it started.
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };
        let neutral = effect("ovs.colour_correct", &[]);

        for mode in [BlendMode::Normal, BlendMode::Multiply, BlendMode::Add] {
            for opacity in [1.0, 0.5, 0.25] {
                let layer = LayerId::new();
                let scene = |passes: &[EffectPass]| {
                    let mut scene = Scene::new();
                    scene.push(quad_at(-1.0, -1.0, 1.0, 1.0, [1.0, 0.25, 0.0, 1.0]));
                    let mut over = quad_at(-1.0, -1.0, 1.0, 1.0, [0.5, 0.75, 1.0, opacity]);
                    over.blend = mode;
                    over.layer_id = Some(layer);
                    scene.push(over);
                    scene.set_effects(layer, passes);
                    scene
                };

                canvas
                    .compose(FrameRequest { index: 0 }, &scene(&[]))
                    .expect("composes");
                let plain = canvas.read_pixel(8, 8).expect("reads");

                canvas
                    .compose(FrameRequest { index: 0 }, &scene(&neutral))
                    .expect("composes");
                let chained = canvas.read_pixel(8, 8).expect("reads");

                for (channel, (was, now)) in plain.iter().zip(&chained).enumerate() {
                    close_to(
                        *now,
                        *was,
                        &format!("{mode:?} at {opacity} opacity, channel {channel}"),
                    );
                }
            }
        }
    }

    #[test]
    fn an_effected_layer_leaves_the_part_of_the_bus_it_does_not_cover_alone() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // The chain's result is composited as a full-canvas quad, because the
        // scratch already holds the layer where its transform put it. What the
        // layer does not cover is transparent there, and transparent has to
        // mean "this layer is not here" for every blend mode.
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };
        let neutral = effect("ovs.colour_correct", &[]);

        for mode in [
            BlendMode::Normal,
            BlendMode::Multiply,
            BlendMode::Add,
            BlendMode::Darken,
            BlendMode::Lighten,
            BlendMode::Screen,
        ] {
            let layer = LayerId::new();
            let mut scene = Scene::new();
            scene.push(quad_at(-1.0, -1.0, 1.0, 1.0, RED));
            // The left half only.
            let mut half = quad_at(-1.0, -1.0, 0.0, 1.0, [0.0, 1.0, 0.0, 1.0]);
            half.blend = mode;
            half.layer_id = Some(layer);
            scene.push(half);
            scene.set_effects(layer, &neutral);

            canvas
                .compose(FrameRequest { index: 0 }, &scene)
                .expect("composes");
            assert_eq!(
                canvas.read_pixel(12, 8).expect("reads"),
                [255, 0, 0, 255],
                "{mode:?} changed the half its layer does not cover"
            );
        }
    }

    #[test]
    fn two_layers_with_chains_keep_their_own_pictures_and_their_own_order() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // Two chains share the two scratch images, so the second one overwrites
        // what the first left. The bus is reopened between them for exactly
        // that reason, and this is the test that says the reopening works.
        let Some(mut canvas) = backend(16, 16) else {
            return;
        };
        let under = LayerId::new();
        let over = LayerId::new();

        let mut scene = Scene::new();
        let mut first = quad_at(-1.0, -1.0, 1.0, 1.0, [0.5, 0.5, 0.5, 1.0]);
        first.layer_id = Some(under);
        scene.push(first);
        // The upper layer covers the right half only, so both are visible.
        let mut second = quad_at(0.0, -1.0, 1.0, 1.0, [0.25, 0.25, 0.25, 1.0]);
        second.layer_id = Some(over);
        scene.push(second);

        scene.set_effects(
            under,
            &effect(
                "ovs.tint",
                &[
                    ("colour", ParameterValue::Color([1.0, 0.0, 0.0, 1.0])),
                    ("amount", ParameterValue::Float(1.0)),
                    ("preserve_luma", ParameterValue::Bool(false)),
                ],
            ),
        );
        scene.set_effects(
            over,
            &effect(
                "ovs.colour_correct",
                &[("brightness", ParameterValue::Float(2.0))],
            ),
        );

        canvas
            .compose(FrameRequest { index: 0 }, &scene)
            .expect("composes");

        assert_eq!(
            canvas.read_pixel(4, 8).expect("reads"),
            [255, 0, 0, 255],
            "the lower layer kept its own chain"
        );
        let upper = canvas.read_pixel(12, 8).expect("reads");
        close_to(upper[0], 128, "and the upper one kept its own, on top");
        close_to(upper[1], 128, "and the upper one kept its own, on top");
    }

    #[test]
    fn a_canvas_reports_the_mode_it_was_created_for() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        let Some(canvas) = backend(1920, 1080) else {
            return;
        };
        assert_eq!(canvas.mode(), mode(1920, 1080));
        assert_eq!(canvas.name(), "Canvas");
    }

    #[test]
    fn several_canvases_can_exist_at_once() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // Each one opens its own device. A leak would show up as the second
        // failing, and a shared-handle mistake as a crash on drop.
        let Some(first) = backend(16, 16) else {
            return;
        };
        let Some(second) = backend(16, 16) else {
            return;
        };
        assert_ne!(first.id(), second.id());
        drop(first);
        drop(second);
    }

    #[test]
    fn a_full_frame_canvas_renders_the_scene_correctly() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // 1920x1080 is the reference composition size, and it is not square, so
        // this also catches a viewport that used one dimension for both axes.
        let Some(mut canvas) = backend(1920, 1080) else {
            return;
        };
        let mut scene = Scene::new();
        scene.push(quad_at(-1.0, -1.0, 0.0, 1.0, RED));
        canvas
            .present(FrameRequest { index: 0 }, &scene)
            .expect("renders a full frame");

        assert_eq!(
            canvas.read_pixel(400, 540).expect("reads"),
            [255, 0, 0, 255]
        );
        assert_eq!(canvas.read_pixel(1500, 540).expect("reads"), [0, 0, 0, 255]);
        println!("rendered on {}", canvas.context().summary().name);
    }
}
