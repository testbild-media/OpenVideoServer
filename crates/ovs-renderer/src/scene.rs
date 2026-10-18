//! Turning show state into something the GPU can draw.
//!
//! The render path is `Source → Slot → Layer → Composition → Pieces → Output`
//!. This is the step that flattens the left-hand side of that
//! into a list of quads: what to draw, where, and how opaque.
//!
//! Both transforms in that path are applied here and they compose in its order:
//! the slot's acts on the source, the layer's acts on the result. See
//! [`composed`], which is where the arithmetic and its limits are written down.
//!
//! It is deliberately pure. No Vulkan, no GPU, no I/O — which means the rules
//! that decide whether a layer appears at all, and where, are testable on a
//! machine with no graphics hardware. Those rules are the ones an operator
//! notices when they are wrong.
//!
//! # What is not here yet
//!
//! Media. A slot referencing a media item renders as a flat placeholder colour,
//! because decoding arrives in Phase 6. That is an honest intermediate rather
//! than a stand-in pretending to be finished: the geometry, ordering, opacity
//! and routing are real and tested, and only the pixels inside each quad are
//! provisional.

use ovs_types::config::{BlendMode, Layer, Route, Show};
use ovs_types::geometry::{AnchorPoint, SizePx, Transform2D};
use std::collections::BTreeMap;

use ovs_types::id::{LayerId, MediaItemId};
use ovs_types::live::LayerLive;

use crate::buses::BusGains;
use crate::effects::EffectPass;
use crate::show_state::ShowState;

/// What a quad samples.
///
/// The two passes of a frame use the same quad, the same shader and the same
/// arithmetic; only this differs. The composition pass draws layers, which
/// sample media or nothing at all. The output pass draws pieces, which sample
/// the composition. Anything that works for one works for the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextureRef {
    /// Nothing: a flat fill.
    ///
    /// Drawn with a one-by-one white texture rather than a second shader, since
    /// white is the identity for the fragment shader's multiply. One path
    /// instead of a branch per fragment.
    #[default]
    None,
    /// One media item's pixels, shared by everything showing it.
    Media(MediaItemId),
    /// One of the three buses, as a finished picture.
    ///
    /// What the combine pass samples: A and B are mixed by the crossfader and M
    /// goes over the result. A bus image is premultiplied and
    /// carries coverage, so a quad naming one is passed through the shader
    /// rather than multiplied by its own alpha a second time.
    Bus(Route),
    /// The frame one layer is currently showing.
    ///
    /// A playing clip, whose pixels are this layer's playhead rather than the
    /// item's: the same file on two layers is two different frames.
    Frame(LayerId),
    /// The composition, as this frame's composition pass left it.
    ///
    /// What every piece samples, and the only thing an output draws.
    Composition,
    /// The commissioning test pattern, which belongs to no layer and no item.
    TestPattern,
    /// The System screen, as the process that draws it last painted it.
    ///
    /// Belongs to no layer and no item either, and unlike everything else here
    /// it is **BGRA**: it is what a browser paints and what this card's
    /// swapchain is, so it crosses the whole path without being converted. The
    /// texture's own layout carries that; nothing in the shader knows.
    Console,
}

/// What a texture is held under.
///
/// Two kinds of thing need a texture and they cannot share one key. A still is
/// the same pixels for everybody, so every layer showing it samples one
/// texture. A clip is at a different frame on every layer playing it — the same
/// file triggered twice is two playheads — so its texture belongs to the layer
/// rather than to the item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TextureKey {
    /// A still, shared by every quad naming the item.
    Media(MediaItemId),
    /// One layer's current frame.
    Layer(LayerId),
    /// The test pattern, of which there is one.
    TestPattern,
    /// The System screen, of which there is one.
    Console,
    /// What one layer's service pattern is drawn from.
    ///
    /// Not a picture any quad names: it is read by the pass that draws that
    /// layer's pattern on the card, and holds the parts of it that do not
    /// move - the bars, and the caption in a strip underneath. It lives in the
    /// same map as the rest because it is a texture with the same lifetime
    /// rules, and because one map is one place to free them from.
    PatternSource(LayerId),
}

impl From<MediaItemId> for TextureKey {
    fn from(media_item_id: MediaItemId) -> Self {
        Self::Media(media_item_id)
    }
}

impl From<LayerId> for TextureKey {
    fn from(layer_id: LayerId) -> Self {
        Self::Layer(layer_id)
    }
}

impl TextureRef {
    /// Returns true when this quad samples a bus.
    ///
    /// A bus image is a composited picture rather than a source image: it is
    /// already premultiplied, and multiplying it by its own alpha again would
    /// darken every soft edge in it.
    #[must_use]
    pub const fn is_bus(self) -> bool {
        matches!(self, Self::Bus(_))
    }

    /// Returns which texture this quad samples, if any.
    #[must_use]
    pub const fn key(self) -> Option<TextureKey> {
        match self {
            Self::Media(media_item_id) => Some(TextureKey::Media(media_item_id)),
            Self::Frame(layer_id) => Some(TextureKey::Layer(layer_id)),
            Self::TestPattern => Some(TextureKey::TestPattern),
            Self::Console => Some(TextureKey::Console),
            // Neither is a texture the compositor holds: one is the
            // composition, the others are the buses, and both are bound
            // directly.
            Self::None | Self::Composition | Self::Bus(_) => None,
        }
    }
}

/// A rectangle of a texture, normalised, with the origin at the top left.
///
/// Texture space, not clip space: `top` is a smaller number than `bottom`,
/// which is the convention every image decoder hands over pixels in. The shader
/// does the one flip that reconciles that with a quad built bottom-up.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SourceRect {
    /// Left edge, `0.0` at the left of the image.
    pub left: f32,
    /// Top edge, `0.0` at the top of the image.
    pub top: f32,
    /// Right edge.
    pub right: f32,
    /// Bottom edge, `1.0` at the bottom of the image.
    pub bottom: f32,
}

impl SourceRect {
    /// The whole image.
    pub const WHOLE: Self = Self {
        left: 0.0,
        top: 0.0,
        right: 1.0,
        bottom: 1.0,
    };
}

impl Default for SourceRect {
    fn default() -> Self {
        Self::WHOLE
    }
}

/// How a quad's source rectangle is turned before it is sampled.
///
/// Both ends of a piece turn (owner, 2026-09-23). The *output* rotation puts a
/// rectangle on a wall at an angle and lives on [`Quad::rotation_deg`]; this is
/// the other end — taking a rectangle out of the composition at an angle, which
/// is what a wall hung on the diagonal has to be fed.
///
/// # A matrix rather than an angle
///
/// Texture coordinates are not isotropic: one unit of U is the whole width and
/// one unit of V is the whole height, so turning a 16:9 region by an angle in
/// those coordinates would shear it rather than turn it. The correction needs
/// the texture's pixel size, which is known where the scene is built and not in
/// the shader, so [`about`](Self::about) does the arithmetic once per frame per
/// piece and what travels is the finished matrix.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SourceTurn {
    /// The two-by-two matrix, row-major: `[m00, m01, m10, m11]`.
    pub matrix: [f32; 4],
    /// The point it turns about, in texture space.
    pub pivot: [f32; 2],
}

impl SourceTurn {
    /// No turn at all: the identity, which every quad but a turned piece has.
    pub const NONE: Self = Self {
        matrix: [1.0, 0.0, 0.0, 1.0],
        pivot: [0.0, 0.0],
    };

    /// The turn for one rectangle, angle and anchor, on a texture of a size.
    ///
    /// Clockwise as seen on screen, like [`Quad::rotation_deg`]. Texture space
    /// has V running downwards, which is why the signs differ from
    /// [`quad_basis`]: the two describe the same direction of turn in spaces
    /// whose vertical axes point opposite ways.
    #[must_use]
    pub fn about(rect: SourceRect, anchor: AnchorPoint, degrees: f32, texture: SizePx) -> Self {
        let pivot = [
            rect.left + (rect.right - rect.left) * anchor.x,
            rect.top + (rect.bottom - rect.top) * anchor.y,
        ];
        if degrees == 0.0 || texture.width == 0 || texture.height == 0 {
            return Self {
                matrix: [1.0, 0.0, 0.0, 1.0],
                pivot,
            };
        }

        let (sin, cos) = degrees.to_radians().sin_cos();
        // One unit of U is `width` pixels and one unit of V is `height`, so an
        // arm is taken into pixels, turned, and brought back.
        let width = texture.width as f32;
        let height = texture.height as f32;
        Self {
            matrix: [cos, -(height / width) * sin, (width / height) * sin, cos],
            pivot,
        }
    }
}

impl Default for SourceTurn {
    fn default() -> Self {
        Self::NONE
    }
}

/// One thing to draw, in normalised device coordinates.
///
/// NDC rather than pixels because that is what the vertex shader wants, and
/// converting once here keeps the conversion out of the per-frame GPU path.
/// The convention is the shaders' convention: `+Y` is up, `-1` is the bottom
/// and left edges.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quad {
    /// Left edge, in NDC.
    pub left: f32,
    /// Bottom edge, in NDC.
    pub bottom: f32,
    /// Right edge, in NDC.
    pub right: f32,
    /// Top edge, in NDC.
    pub top: f32,
    /// Colour, as linear RGBA with the alpha already multiplied by opacity.
    pub colour: [f32; 4],
    /// Which layer this came from, for diagnostics and for tests.
    ///
    /// `None` for a piece, which comes from the output mapping rather than from
    /// any layer.
    pub layer_id: Option<LayerId>,
    /// Clockwise rotation in degrees, about [`anchor`](Self::anchor).
    ///
    /// Applied in pixels rather than in normalised coordinates, so a square
    /// turns into a square on a 16:9 canvas instead of into a lozenge. That
    /// correction needs the size of whatever is being drawn into, which is why
    /// it happens in the backend rather than here.
    pub rotation_deg: f32,
    /// The point in the quad the turn happens about.
    ///
    /// A fraction of the quad's own size from its top-left corner, so
    /// `(0.5, 0.5)` is the centre and the arithmetic in [`quad_basis`]
    /// collapses to what it was before anchors existed.
    pub anchor: AnchorPoint,
    /// Which rectangle of the texture to sample.
    ///
    /// [`SourceRect::WHOLE`] for a layer, which shows all of its image. Not for
    /// a piece: a piece takes one rectangle of the composition and puts it on an
    /// output, which is the same drawing operation with a different rectangle
    /// rather than a second code path.
    pub source: SourceRect,
    /// How that rectangle is turned before it is sampled.
    ///
    /// [`SourceTurn::NONE`] for everything but a piece whose input is taken at
    /// an angle, and the identity there too when the angle is zero — so there
    /// is one path through the shader rather than two.
    pub source_turn: SourceTurn,
    /// What this quad samples.
    pub texture: TextureRef,
    /// Which bus the layer feeds.
    pub route: Route,
    /// How this quad composites onto what is under it.
    pub blend: BlendMode,
}

impl Quad {
    /// Returns true when this quad would put nothing on screen.
    ///
    /// A fully transparent or zero-area quad still costs a draw, so the
    /// composer drops them rather than asking the GPU to do nothing slowly.
    #[must_use]
    pub fn is_invisible(&self) -> bool {
        self.colour[3] <= 0.0 || self.right <= self.left || self.top <= self.bottom
    }
}

/// The flattened description of one frame.
///
/// Reused between frames. Clearing a `Vec` keeps its capacity, so after the
/// first few frames composing allocates nothing, which is what the realtime
/// rules require (house rule §5).
#[derive(Debug, Default)]
pub struct Scene {
    quads: Vec<Quad>,
    /// How the three buses combine into the picture.
    ///
    /// Part of the scene rather than of the frame request, because it is part
    /// of what to draw: the same quads with a different crossfader position are
    /// a different picture.
    gains: BusGains,
    /// Every effect pass this frame runs, one chain after another.
    ///
    /// Flat, and reused between frames like the quads: a chain is a handful of
    /// `Copy` values, so a frame's worth of them costs no allocation once the
    /// buffer has grown.
    passes: Vec<EffectPass>,
    /// Which stretch of `passes` belongs to which layer.
    chains: Vec<LayerChain>,
}

/// Where one layer's chain sits in the scene's pass list.
///
/// Keyed by layer rather than by position in the quad list, because a quad may
/// be dropped or reordered between being composed and being drawn and a chain
/// that had drifted onto another layer would be a wrong picture that looks
/// deliberate.
#[derive(Debug, Clone, Copy)]
struct LayerChain {
    /// Whose chain it is.
    layer: LayerId,
    /// Where it starts in the scene's pass list.
    first: usize,
    /// How many passes it runs.
    count: usize,
}

impl Scene {
    /// Creates an empty scene.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns what is to be drawn, in composite order: bottom first.
    #[must_use]
    pub fn quads(&self) -> &[Quad] {
        &self.quads
    }

    /// Returns true when nothing is to be drawn.
    ///
    /// An ordinary state, not an error: a show with every layer on Slot 0
    /// composes to nothing and the canvas shows its clear colour, which is
    /// black, which is exactly right.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.quads.is_empty()
    }

    /// Returns how many quads are in the scene.
    #[must_use]
    pub fn len(&self) -> usize {
        self.quads.len()
    }

    /// Adds a quad directly.
    ///
    /// Composition fills a scene from show state; pieces and previews will add
    /// to one the same way, so this is the general entry point rather than a
    /// test affordance.
    pub fn push(&mut self, quad: Quad) {
        self.quads.push(quad);
    }

    /// Removes every quad and every effect chain, keeping the allocations.
    ///
    /// The bus gains are left alone: they belong to the master, not to any
    /// particular set of quads.
    pub fn clear(&mut self) {
        self.quads.clear();
        self.passes.clear();
        self.chains.clear();
    }

    /// Gives one layer an effect chain for this frame.
    ///
    /// A layer has one chain, set once. Setting a second one for the same layer
    /// leaves the first in place, because the first is the one its quad was
    /// composed with.
    pub fn set_effects(&mut self, layer: LayerId, passes: &[EffectPass]) {
        if passes.is_empty() || self.chain_of(layer).is_some() {
            return;
        }
        let first = self.passes.len();
        self.passes.extend_from_slice(passes);
        self.chains.push(LayerChain {
            layer,
            first,
            count: passes.len(),
        });
    }

    /// Returns the passes one layer's chain runs, in chain order.
    ///
    /// Empty for a layer with no effects, with every effect disabled, or with
    /// no chain set — which are the same thing to everything downstream, and
    /// deliberately so: all three must take the path a layer with no effects
    /// takes.
    #[must_use]
    pub fn effects(&self, layer: LayerId) -> &[EffectPass] {
        self.chain_of(layer).map_or(&[], |chain| {
            &self.passes[chain.first..chain.first + chain.count]
        })
    }

    /// Returns true when anything in this scene has an effect to run.
    ///
    /// What the compositor asks before it allocates a scratch image, so a show
    /// with no effects in it never has one.
    #[must_use]
    pub fn has_effects(&self) -> bool {
        !self.chains.is_empty()
    }

    /// Returns where a layer's chain is, if it has one.
    fn chain_of(&self, layer: LayerId) -> Option<LayerChain> {
        self.chains
            .iter()
            .copied()
            .find(|chain| chain.layer == layer)
    }

    /// Returns how the buses combine.
    #[must_use]
    pub const fn gains(&self) -> BusGains {
        self.gains
    }

    /// Sets how the buses combine.
    pub const fn set_gains(&mut self, gains: BusGains) {
        self.gains = gains;
    }

    /// Modifies every quad in place, dropping the ones the closure rejects.
    ///
    /// One pass over the scene rather than a map followed by a filter, so a
    /// per-frame transformation costs no allocation.
    pub fn retain_map(&mut self, mut keep: impl FnMut(&mut Quad) -> bool) {
        self.quads.retain_mut(|quad| keep(quad));
    }

    /// Fills this scene from the current show state, reusing its capacity.
    ///
    /// `showing` says, for the layers the scheduler is responsible for,
    /// whether the layer has a frame on the GPU. A layer with one samples its
    /// own texture, which is what lets the same clip run on two layers at two
    /// different frames; a layer without one draws its placeholder colour,
    /// because the alternative is a white rectangle where a clip should be —
    /// there is no shared texture for a clip to fall back to.
    pub fn compose(&mut self, state: &ShowState, showing: &BTreeMap<LayerId, bool>) {
        self.clear();

        let Some(show) = state.show.as_deref() else {
            return;
        };

        for layer in show.layers_in_composite_order() {
            let Some(live) = state.layer(layer.id) else {
                // A layer the core has not sent live state for yet. Skipping is
                // right: guessing at a slot would put something on stage that
                // nobody selected.
                continue;
            };

            if !layer.enabled || !live.enabled || live.is_black() {
                // Slot 0 is permanently Black and needs no decoder, no quad and
                // no special case downstream.
                //
                // A layer switched off live is the same in the composition and
                // different everywhere else: it keeps its slots, its transform
                // and its chain, and the show never hears about it
                // (owner decision Q16). A paused or stopped layer is *not*
                // here — a held frame is a picture, so it is still on stage.
                continue;
            }

            let slot = layer.slot(live.active_slot);

            // What this layer samples. The scheduler's answer comes first,
            // because it covers both the things that have a texture of their
            // own: a clip at its own playhead, and a generator that drew
            // itself. Neither has a shared texture to fall back to.
            let media = match showing.get(&layer.id) {
                // Something with a frame ready: its own texture.
                Some(true) => TextureRef::Frame(layer.id),
                // Something with nothing to show — no decoder, a source that
                // failed, or a first frame still on its way. Nothing is drawn
                // for it at all; see below.
                Some(false) => TextureRef::None,
                // Not the scheduler's: a still, shared by item, or a slot with
                // nothing in it at all.
                None => slot
                    .and_then(ovs_types::config::Slot::media_item_id)
                    .map_or(TextureRef::None, TextureRef::Media),
            };

            // A slot the show has nothing configured for has nothing to
            // transform, which is what the identity already means.
            let slot_transform = slot.map_or(Transform2D::IDENTITY, |slot| slot.transform);

            // **A layer with nothing to show shows nothing** (owner, 2026-09-25:
            // "black/Empty sollte einfach nichts sein").
            //
            // It used to draw a flat colour derived from the layer's
            // identifier, which was a debug aid from before there was a decoder
            // — the comment on it said "until media decoding arrives in Phase
            // 6", and decoding arrived two phases ago. An empty pad looked like
            // content somebody had put there.
            //
            // Nothing rather than black: a black quad still covers what is
            // under it, and a layer with nothing in it should be as if it were
            // not in the show.
            if matches!(media, TextureRef::None) {
                continue;
            }

            let mut quad = quad_for(
                layer,
                live,
                live.opacity,
                &slot_transform,
                &live.transform,
                show.composition.size,
                media,
            );
            // A source may ask for its own way of landing on what is under it.
            // One does: see [`asks_for_blend`].
            if let Some(blend) = slot.and_then(|slot| asks_for_blend(slot, state)) {
                quad.blend = blend;
            }
            if quad.is_invisible() {
                continue;
            }
            self.quads.push(quad);

            // Built here rather than in the compositor because it is part of
            // what to draw, like the bus gains: a layer's chain belongs to the
            // frame it was composed for. A chain that is empty, or whose every
            // effect is disabled, adds nothing and leaves the layer on the path
            // it takes with no effects at all.
            //
            // The live values go in with the configured ones. An effect whose
            // parameters cannot be moved during a show is half an effect, and
            // this is the point where the other half reaches the wall.
            //
            // # Two chains, one sequence
            //
            // The slot's effects go in first, then the layer's, which is the
            // render chain TECHNICAL states and the owner confirmed
            // (owner decision Q12): a slot effect belongs to that content and
            // comes back whenever that slot is selected; a layer effect belongs
            // to the layer and applies to whichever slot is playing.
            //
            // They are one list because that is what they are on the wall: both
            // are full-screen passes over the layer's picture, run in order,
            // and the compositor has no reason to know where one chain stops
            // and the other starts. Splitting them would be a second scratch
            // image and a second round trip for a result identical to this.
            //
            // What is *not* reproduced literally is the transform between them.
            // The chain reads `Slot Transform -> Slot Effects`, and both
            // transforms reach the wall as the quad's own basis rather than as
            // passes — which is how the layer's transform has always worked and
            // is why a layer effect sees the source at its native size too.
            let first = self.passes.len();
            if let Some(slot) = slot {
                crate::effects::live_passes_into(&slot.effects, &state.effects, &mut self.passes);
            }
            crate::effects::live_passes_into(&layer.effects, &state.effects, &mut self.passes);
            let count = self.passes.len() - first;
            if count > 0 {
                self.chains.push(LayerChain {
                    layer: layer.id,
                    first,
                    count,
                });
            }
        }
    }
}

/// How a source asks to land on what is under it, where it asks at all.
///
/// `None` for every source but one, which is the point: a layer composites the
/// way its operator set it, and nothing in a slot overrules that by accident.
///
/// **The strobe's second mode is the exception** (owner, 2026-10-16: "einen
/// modus für invert color strobe, wo der weiße blitz dann inverted color
/// ist"). A flash that inverts what is under it is not a picture the generator
/// can draw — a generator draws its own layer and has never seen the
/// composition — so the inversion is in how the layer lands: a difference
/// against white is the picture with every channel turned round, and the clear
/// half of the cycle, which is transparent, leaves it alone.
///
/// The live value first, so a desk can change the mode mid-show; the slot's
/// own value behind it; the manifest's default behind that.
fn asks_for_blend(slot: &ovs_types::config::Slot, state: &ShowState) -> Option<BlendMode> {
    let ovs_types::config::slot::SourceRef::Generator {
        instance_id,
        type_id,
        parameters,
        ..
    } = &slot.source
    else {
        return None;
    };
    if type_id != "ovs.strobe" {
        return None;
    }

    let manifest = ovs_types::config::generators::manifest(type_id)?;
    let live = state
        .generators
        .get(instance_id)
        .and_then(|values| values.get("mode"));
    let value = live
        .cloned()
        .or_else(|| manifest.value(parameters, "mode"))?;

    match value {
        ovs_types::config::slot::ParameterValue::Choice(mode) if mode == "invert" => {
            Some(BlendMode::Difference)
        }
        _ => None,
    }
}

/// Builds the quad for one layer.
///
/// Two transforms reach here, and both apply: `slot` is the one configured on
/// the source and `layer_transform` is the live one a console drives. They
/// compose in the order the render chain states — see
/// [`composed`].
fn quad_for(
    layer: &Layer,
    live: &LayerLive,
    opacity: f32,
    slot: &Transform2D,
    layer_transform: &Transform2D,
    composition: SizePx,
    media: TextureRef,
) -> Quad {
    let transform = composed(slot, layer_transform);

    let half_width = composition.width as f32 / 2.0;
    let half_height = composition.height as f32 / 2.0;

    // A layer fills the composition before its transform is applied, so an
    // untransformed layer covers the whole canvas. Scale then offset, both in
    // composition pixels, converted once to NDC here.
    let half_w = half_width * transform.scale_x;
    let half_h = half_height * transform.scale_y;

    let centre_x = transform.x;
    let centre_y = transform.y;

    let to_ndc_x = |pixels: f32| pixels / half_width;
    let to_ndc_y = |pixels: f32| pixels / half_height;

    Quad {
        left: to_ndc_x(centre_x - half_w),
        right: to_ndc_x(centre_x + half_w),
        bottom: to_ndc_y(centre_y - half_h),
        top: to_ndc_y(centre_y + half_h),
        // White times the opacity, whatever the texture is: the shader
        // multiplies the sampled pixel by this, so anything else would tint
        // real media. A layer with no texture never reaches here — it draws
        // nothing at all.
        colour: [1.0, 1.0, 1.0, opacity],
        rotation_deg: transform.rotation_deg,
        // A layer turns about its own centre. An anchor is a piece's, because
        // a piece is hung on a wall and a layer is not (owner, 2026-09-23);
        // giving one to a `Transform2D` as well would be two answers to a
        // question nobody has asked yet.
        anchor: AnchorPoint::CENTRE,
        layer_id: Some(layer.id),
        // A layer shows all of its image. Cropping within a slot is not a
        // feature these releases have; a piece's rectangle is a different thing
        // and belongs to the output pass.
        source: SourceRect::WHOLE,
        // And is not taken at an angle. Turning the *input* is a piece's, for
        // the same reason the anchor above is.
        source_turn: SourceTurn::NONE,
        texture: media,
        // **The live ones, not the show's.** Both are things an operator
        // changes while looking at the wall, and reading them off the document
        // meant a bus change did nothing until the show was saved — which
        // is the same as saying it did not work (owner, 2026-09-23).
        route: live.route,
        blend: live.blend_mode,
    }
}

/// Composes a slot's transform with its layer's.
///
/// The render chain is `Source → Slot Transform → Slot Effects → Layer → Layer
/// Effects → Layer Transform`. The slot's transform therefore
/// acts on the source, and the layer's acts on the result of that: the two
/// compose, and neither replaces the other. A slot scaled by a half inside a
/// layer moved right is a half-size picture, moved right.
///
/// The offset composes the way that order implies. A slot's offset is measured
/// in the source's own space, so the layer scales and turns it before adding its
/// own: moving a slot a hundred pixels right inside a layer scaled to a half
/// moves the picture fifty, which is what "the layer is smaller now" has to
/// mean. Getting this the other way round would make a console's Position
/// channel do different things depending on a setting the operator cannot see
/// from the desk.
///
/// # Where the angles are exact
///
/// Rotations add. That is exactly right whenever one of the two is unturned —
/// which is every show today, since the slot transform is configuration and
/// the Layer Fixture is what turns a layer — and whenever the
/// layer's scale is the same on both axes, which is everything that fixture's
/// single Scale channel can produce.
///
/// The one case it cannot express is a turned slot inside a layer that is both
/// turned *and* scaled unevenly: the true result has two axes that are no longer
/// at right angles, and a [`Quad`] here is a rectangle with an angle rather than
/// a free pair of axes. Making that exact means giving every quad a full basis
/// and changing the vertex path that consumes it, for a combination nothing in
/// the specification asks for. It is written down here rather than discovered on
/// a wall.
fn composed(slot: &Transform2D, layer: &Transform2D) -> Transform2D {
    let offset = turned_clockwise(
        [slot.x * layer.scale_x, slot.y * layer.scale_y],
        layer.rotation_deg,
    );

    Transform2D {
        x: layer.x + offset[0],
        y: layer.y + offset[1],
        scale_x: layer.scale_x * slot.scale_x,
        scale_y: layer.scale_y * slot.scale_y,
        rotation_deg: layer.rotation_deg + slot.rotation_deg,
    }
}

/// Turns a vector clockwise on screen, in composition pixels.
///
/// The same direction and the same guard as [`quad_basis`]: `+Y` is up in these
/// coordinates, so a clockwise turn is the negative direction, and a nonsense
/// angle leaves the vector where it was rather than sending a layer into an
/// undefined corner of the canvas.
fn turned_clockwise(vector: [f32; 2], degrees: f32) -> [f32; 2] {
    if degrees == 0.0 || !degrees.is_finite() {
        return vector;
    }

    let (sin, cos) = degrees.to_radians().sin_cos();
    [
        vector[0] * cos + vector[1] * sin,
        -vector[0] * sin + vector[1] * cos,
    ]
}

/// Fills `into` with the one quad an output with nothing mapped onto it draws.
///
/// The whole composition, over the whole output. This is what a renderer shows
/// when the show has no output configured for it, which is the ordinary state
/// of a single-wall setup nobody has mapped: the useful thing to show is
/// everything, not nothing.
pub fn compose_whole_composition(into: &mut Scene) {
    into.clear();
    into.push(Quad::whole_canvas());
}

/// Fills the composition with the test pattern, and nothing else.
///
/// Deliberately not a layer. The pattern is for lining a wall up before there
/// is a show, so it does not belong to a bus, is not affected by the crossfader
/// or the dimmer, and does not compose *over* anything: while it is on, it is
/// what the composition is.
pub fn compose_test_pattern(into: &mut Scene) {
    into.clear();
    into.set_gains(crate::buses::BusGains::default());

    let mut quad = Quad::whole_canvas();
    quad.texture = TextureRef::TestPattern;
    quad.route = Route::M;
    into.push(quad);
}

/// One quad across the whole output, showing the System screen.
///
/// No mapping and no pieces: this output is a panel somebody reads and touches,
/// not a part of a wall. The page was laid out for this connector's mode, so the
/// quad is the picture at one to one — and a frame that arrives at another size,
/// which happens for a moment while a resize is being drawn, is scaled rather
/// than refused.
pub fn compose_console(into: &mut Scene) {
    into.clear();
    into.set_gains(crate::buses::BusGains::default());

    let mut quad = Quad::whole_canvas();
    quad.texture = TextureRef::Console;
    quad.route = Route::M;
    into.push(quad);
}

impl Quad {
    /// A quad covering the whole canvas, sampling a whole texture.
    ///
    /// What every full-screen pass is built from: the piece that shows a whole
    /// composition, and the three that combine the buses.
    #[must_use]
    pub const fn whole_canvas() -> Self {
        Self {
            left: -1.0,
            bottom: -1.0,
            right: 1.0,
            top: 1.0,
            colour: [1.0, 1.0, 1.0, 1.0],
            rotation_deg: 0.0,
            anchor: AnchorPoint::CENTRE,
            layer_id: None,
            source: SourceRect::WHOLE,
            source_turn: SourceTurn::NONE,
            texture: TextureRef::Composition,
            route: Route::M,
            blend: BlendMode::Normal,
        }
    }
}

/// Where a quad's four corners go, as a centre and a two-by-two basis.
///
/// Returns `(centre, [xx, xy, yx, yy])`. A corner at sign `(u, v)`, each `-1` or
/// `+1`, lands at `centre + (u, v)` through that basis. With no rotation the
/// basis is just the quad's half-extents, which is what makes this the same
/// arithmetic for every quad rather than a special case for turned ones.
///
/// `target` is what is being drawn into: the composition for a layer, the output
/// for a piece. Rotation is a pixel-space operation, so the basis converts into
/// pixels, turns, and converts back — otherwise a square on a 16:9 canvas would
/// come out a lozenge.
#[must_use]
pub fn quad_basis(quad: &Quad, target: SizePx) -> ([f32; 2], [f32; 4]) {
    let centre = [
        (quad.left + quad.right) / 2.0,
        (quad.bottom + quad.top) / 2.0,
    ];
    let half = [
        (quad.right - quad.left) / 2.0,
        (quad.top - quad.bottom) / 2.0,
    ];

    if quad.rotation_deg == 0.0 || !quad.rotation_deg.is_finite() {
        return (centre, [half[0], 0.0, 0.0, half[1]]);
    }

    // Clockwise on screen. `+Y` is up in these coordinates, so a clockwise turn
    // is the negative direction and the matrix is transposed from the usual one.
    let radians = quad.rotation_deg.to_radians();
    let (sin, cos) = radians.sin_cos();

    // Half-extents in pixels, which is the space the turn has to happen in.
    let width = target.width.max(1) as f32;
    let height = target.height.max(1) as f32;
    let half_px = [half[0] * width / 2.0, half[1] * height / 2.0];

    // Rotate each half-extent vector, then convert the result back to NDC.
    let x_axis = [half_px[0] * cos, -half_px[0] * sin];
    let y_axis = [half_px[1] * sin, half_px[1] * cos];

    // And the centre itself about the anchor, in the same pixels and by the
    // same matrix. A quad turned about its own centre does not move, which is
    // why this was not here before; one turned about a corner swings, and that
    // is the whole of what an anchor point is (owner, 2026-09-23).
    //
    // `anchor.y` is measured downwards, as every pixel coordinate an operator
    // types is, while `+Y` is up here — hence `top - height * y`.
    let pivot = [
        quad.left + (quad.right - quad.left) * quad.anchor.x,
        quad.top - (quad.top - quad.bottom) * quad.anchor.y,
    ];
    let arm_px = [
        (centre[0] - pivot[0]) * width / 2.0,
        (centre[1] - pivot[1]) * height / 2.0,
    ];
    let turned_px = [
        arm_px[0] * cos + arm_px[1] * sin,
        -arm_px[0] * sin + arm_px[1] * cos,
    ];
    let centre = [
        pivot[0] + turned_px[0] * 2.0 / width,
        pivot[1] + turned_px[1] * 2.0 / height,
    ];

    (
        centre,
        [
            x_axis[0] * 2.0 / width,
            x_axis[1] * 2.0 / height,
            y_axis[0] * 2.0 / width,
            y_axis[1] * 2.0 / height,
        ],
    )
}
/// Returns the composition size a scene was composed against.
///
/// `None` when no show is loaded.
#[must_use]
pub fn composition_size(show: Option<&Show>) -> Option<SizePx> {
    show.map(|show| show.composition.size)
}

#[cfg(test)]
mod tests {
    use ovs_proto::realtime::Sequence;
    use ovs_types::config::{EffectInstance, Show, Slot};
    use ovs_types::id::MediaItemId;
    use ovs_types::live::{LayerLive, MasterLive};

    use super::*;

    /// A show with `count` layers, each holding one media slot.
    fn show_with(count: u32) -> Show {
        let mut show = Show::new("Show");
        for order in 0..count {
            let mut layer = Layer::new(format!("Layer {}", order + 1), order);
            layer.slots.push(Slot::media(1, MediaItemId::new()));
            show.layers.push(layer);
        }
        show
    }

    /// Show state with every layer showing slot 1.
    fn state_showing_everything(show: Show) -> ShowState {
        let layers = show
            .layers
            .iter()
            // `on_restart` rather than `black`, because that is what opening a
            // show does: the live state starts from the layer's own defaults.
            // A fixture that started from black would not see a route or a blend
            // mode the show configured, which is the thing several tests here
            // are about.
            .map(|layer| LayerLive {
                active_slot: 1,
                ..LayerLive::on_restart(layer, None)
            })
            .collect();
        ShowState {
            sequence: Sequence(1),
            show: Some(Box::new(show)),
            master: MasterLive::default(),
            layers,
            ..ShowState::default()
        }
    }

    #[test]
    fn no_show_composes_to_nothing() {
        let mut scene = Scene::new();
        scene.compose(&ShowState::default(), &BTreeMap::new());
        assert!(scene.is_empty());
    }

    #[test]
    fn a_layer_with_no_effects_has_no_chain() {
        // What the compositor's fast path is keyed on: no chain at all, rather
        // than a chain of nothing.
        let state = state_showing_everything(show_with(1));
        let layer = state.show.as_ref().expect("a show").layers[0].id;

        let mut scene = Scene::new();
        scene.compose(&state, &BTreeMap::new());
        assert!(scene.effects(layer).is_empty());
        assert!(!scene.has_effects());
    }

    #[test]
    fn a_layers_effects_reach_the_scene_in_the_order_they_were_listed() {
        let mut show = show_with(1);
        show.layers[0].effects = vec![
            EffectInstance::new("ovs.tint", 1),
            EffectInstance::new("ovs.colour_correct", 1),
        ];
        let state = state_showing_everything(show);
        let layer = state.show.as_ref().expect("a show").layers[0].id;

        let mut scene = Scene::new();
        scene.compose(&state, &BTreeMap::new());

        assert!(scene.has_effects());
        assert_eq!(
            scene
                .effects(layer)
                .iter()
                .map(|pass| pass.kind)
                .collect::<Vec<_>>(),
            vec![
                crate::effects::PassKind::Tint,
                crate::effects::PassKind::ColourCorrect,
                crate::effects::PassKind::WhiteBalance
            ]
        );
    }

    /// Show state with every layer showing one particular slot.
    fn state_showing(show: Show, slot_index: u16) -> ShowState {
        let mut state = state_showing_everything(show);
        for live in &mut state.layers {
            live.active_slot = slot_index;
        }
        state
    }

    #[test]
    fn a_slots_chain_runs_before_the_layers_and_comes_back_with_the_slot() {
        // The render chain TECHNICAL states and the owner confirmed
        // (owner decision Q12): a slot effect belongs to that content and
        // returns whenever that slot is selected; a layer effect belongs to the
        // layer and applies to whichever slot is playing.
        let mut show = show_with(1);
        show.layers[0].effects = vec![EffectInstance::new("ovs.tint", 1)];
        show.layers[0].slots[0].effects = vec![EffectInstance::new("ovs.pixelate", 1)];
        let mut second = Slot::media(2, MediaItemId::new());
        second.effects = vec![EffectInstance::new("ovs.blur", 1)];
        show.layers[0].slots.push(second);
        let layer = show.layers[0].id;

        let kinds = |slot_index| {
            let mut scene = Scene::new();
            scene.compose(&state_showing(show.clone(), slot_index), &BTreeMap::new());
            scene
                .effects(layer)
                .iter()
                .map(|pass| pass.kind)
                .collect::<Vec<_>>()
        };

        // Pixelate is two passes, across then down, so the slot's chain is the
        // first two and the layer's is what follows it.
        assert_eq!(
            kinds(1),
            vec![
                crate::effects::PassKind::PixelateAcross,
                crate::effects::PassKind::PixelateDown,
                crate::effects::PassKind::Tint
            ],
            "the slot's chain, then the layer's"
        );

        let second = kinds(2);
        assert_eq!(
            second.last(),
            Some(&crate::effects::PassKind::Tint),
            "the layer's chain applies to whichever slot is playing: {second:?}"
        );
        assert!(
            !second.contains(&crate::effects::PassKind::PixelateAcross),
            "slot 1's effect came back on slot 2: {second:?}"
        );
    }

    #[test]
    fn a_slot_with_no_chain_leaves_the_layer_on_the_path_it_always_took() {
        // The rule every chain here obeys: nothing, all disabled and none at
        // all are the same thing, and none of them costs a scratch image.
        let show = show_with(1);
        let layer = show.layers[0].id;

        let mut scene = Scene::new();
        scene.compose(&state_showing(show, 1), &BTreeMap::new());

        assert!(scene.effects(layer).is_empty());
        assert!(!scene.has_effects());
    }

    #[test]
    fn a_chain_of_disabled_effects_is_no_chain_at_all() {
        let mut show = show_with(1);
        let mut effect = EffectInstance::new("ovs.blur", 1);
        effect.enabled = false;
        show.layers[0].effects = vec![effect];
        let state = state_showing_everything(show);
        let layer = state.show.as_ref().expect("a show").layers[0].id;

        let mut scene = Scene::new();
        scene.compose(&state, &BTreeMap::new());
        assert!(scene.effects(layer).is_empty());
        assert!(!scene.has_effects());
    }

    #[test]
    fn each_layer_keeps_its_own_chain() {
        // Keyed by layer rather than by position, because a chain that had
        // drifted onto the layer next to it would be a wrong picture that looks
        // deliberate.
        let mut show = show_with(3);
        show.layers[0].effects = vec![EffectInstance::new("ovs.tint", 1)];
        show.layers[2].effects = vec![EffectInstance::new("ovs.blur", 1)];
        let state = state_showing_everything(show);
        let layers: Vec<LayerId> = state
            .show
            .as_ref()
            .expect("a show")
            .layers
            .iter()
            .map(|layer| layer.id)
            .collect();

        let mut scene = Scene::new();
        scene.compose(&state, &BTreeMap::new());

        assert_eq!(scene.effects(layers[0]).len(), 1, "one pass for a tint");
        assert!(scene.effects(layers[1]).is_empty());
        assert_eq!(scene.effects(layers[2]).len(), 2, "two for a blur");
    }

    #[test]
    fn a_live_effect_value_changes_what_the_renderer_draws() {
        // The point of the whole path: an operator moves an effect control and
        // the picture changes, without the show being edited.
        let mut show = show_with(1);
        let mut blur = EffectInstance::new("ovs.blur", 1);
        let instance = blur.id;
        blur.parameters.insert(
            "radius_px".to_owned(),
            ovs_types::config::ParameterValue::Float(8.0),
        );
        show.layers[0].effects = vec![blur];

        let mut state = state_showing_everything(show);
        let layer = state.show.as_ref().expect("a show").layers[0].id;

        let mut scene = Scene::new();
        scene.compose(&state, &BTreeMap::new());
        assert_eq!(
            scene.effects(layer)[0].values[0],
            8.0,
            "the show's radius, before anybody touches it"
        );

        state.effects = std::sync::Arc::new(ovs_types::live::EffectOverrides::from([(
            instance,
            ovs_types::config::ParameterMap::from([(
                "radius_px".to_owned(),
                ovs_types::config::ParameterValue::Float(40.0),
            )]),
        )]));
        scene.compose(&state, &BTreeMap::new());

        assert_eq!(scene.effects(layer)[0].values[0], 40.0);
        assert_eq!(
            state.show.as_ref().expect("a show").layers[0].effects[0]
                .parameters
                .get("radius_px"),
            Some(&ovs_types::config::ParameterValue::Float(8.0)),
            "and the show the scene was built from is untouched"
        );
    }

    #[test]
    fn a_live_value_for_an_effect_this_show_does_not_have_draws_nothing_different() {
        // Overrides are keyed by instance, so a set left over from another
        // show — or one that crossed with a show load — matches nothing
        // rather than landing on whatever effect happens to be there.
        let mut show = show_with(1);
        show.layers[0].effects = vec![EffectInstance::new("ovs.blur", 1)];

        let mut state = state_showing_everything(show);
        let layer = state.show.as_ref().expect("a show").layers[0].id;
        state.effects = std::sync::Arc::new(ovs_types::live::EffectOverrides::from([(
            ovs_types::id::EffectInstanceId::new(),
            ovs_types::config::ParameterMap::from([(
                "radius_px".to_owned(),
                ovs_types::config::ParameterValue::Float(40.0),
            )]),
        )]));

        let mut scene = Scene::new();
        scene.compose(&state, &BTreeMap::new());
        assert_eq!(
            scene.effects(layer)[0].values[0],
            0.0,
            "the published default, which is what this show asked for"
        );
    }

    #[test]
    fn composing_again_forgets_the_chains_of_the_frame_before() {
        // The scene is reused between frames, and a chain left behind would be
        // an effect an operator had removed still running.
        let mut show = show_with(1);
        show.layers[0].effects = vec![EffectInstance::new("ovs.tint", 1)];
        let state = state_showing_everything(show.clone());

        let mut scene = Scene::new();
        scene.compose(&state, &BTreeMap::new());
        assert!(scene.has_effects());

        show.layers[0].effects.clear();
        scene.compose(&state_showing_everything(show), &BTreeMap::new());
        assert!(!scene.has_effects());
    }

    #[test]
    fn a_layer_on_the_black_slot_contributes_nothing() {
        // Slot 0 is permanently Black, and that is the whole mechanism: no
        // quad, no decoder, no special case anywhere downstream.
        let show = show_with(1);
        let mut state = state_showing_everything(show);
        state.layers[0].active_slot = 0;

        let mut scene = Scene::new();
        scene.compose(&state, &BTreeMap::new());
        assert!(scene.is_empty());
    }

    #[test]
    fn a_disabled_layer_contributes_nothing() {
        let mut show = show_with(1);
        show.layers[0].enabled = false;
        let state = state_showing_everything(show);

        let mut scene = Scene::new();
        scene.compose(&state, &BTreeMap::new());
        assert!(scene.is_empty());
    }

    #[test]
    fn a_layer_with_no_live_state_yet_is_skipped_rather_than_guessed_at() {
        let show = show_with(2);
        let mut state = state_showing_everything(show);
        state.layers.remove(0);

        let mut scene = Scene::new();
        scene.compose(&state, &BTreeMap::new());
        assert_eq!(
            scene.len(),
            1,
            "only the layer whose state actually arrived"
        );
    }

    #[test]
    fn an_untransformed_layer_fills_the_composition() {
        let state = state_showing_everything(show_with(1));
        let mut scene = Scene::new();
        scene.compose(&state, &BTreeMap::new());

        let quad = scene.quads()[0];
        assert!(
            (quad.left + 1.0).abs() < 1e-6,
            "left edge at -1, got {}",
            quad.left
        );
        assert!((quad.right - 1.0).abs() < 1e-6);
        assert!((quad.bottom + 1.0).abs() < 1e-6);
        assert!((quad.top - 1.0).abs() < 1e-6);
    }

    #[test]
    fn a_half_scale_layer_covers_the_middle_quarter() {
        let show = show_with(1);
        let mut state = state_showing_everything(show);
        state.layers[0].transform = Transform2D {
            scale_x: 0.5,
            scale_y: 0.5,
            ..Transform2D::IDENTITY
        };

        let mut scene = Scene::new();
        scene.compose(&state, &BTreeMap::new());
        let quad = scene.quads()[0];

        assert!((quad.left + 0.5).abs() < 1e-6, "got {}", quad.left);
        assert!((quad.right - 0.5).abs() < 1e-6);
        assert!((quad.bottom + 0.5).abs() < 1e-6);
        assert!((quad.top - 0.5).abs() < 1e-6);
    }

    #[test]
    fn an_offset_is_measured_in_composition_pixels() {
        // A 1920-wide composition: offsetting by 480 pixels is a quarter of the
        // width, which is 0.5 in NDC.
        let show = show_with(1);
        assert_eq!(show.composition.size, SizePx::new(1920, 1080));
        let mut state = state_showing_everything(show);
        state.layers[0].transform = Transform2D {
            x: 480.0,
            ..Transform2D::IDENTITY
        };

        let mut scene = Scene::new();
        scene.compose(&state, &BTreeMap::new());
        let quad = scene.quads()[0];

        assert!((quad.left - -0.5).abs() < 1e-6, "got {}", quad.left);
        assert!(
            (quad.right - 1.5).abs() < 1e-6,
            "a layer may hang off the edge"
        );
    }

    #[test]
    fn a_slot_transform_is_applied_even_when_the_layer_has_none() {
        // The slot transform is part of the render chain, not a field the
        // show carries and the renderer ignores. A show that set it and
        // saw nothing happen would be the worst kind of wrong: silent.
        let mut show = show_with(1);
        show.layers[0].slots[0].transform = Transform2D {
            scale_x: 0.5,
            scale_y: 0.5,
            ..Transform2D::IDENTITY
        };
        let state = state_showing_everything(show);

        let mut scene = Scene::new();
        scene.compose(&state, &BTreeMap::new());
        let quad = scene.quads()[0];

        assert!((quad.left + 0.5).abs() < 1e-6, "got {}", quad.left);
        assert!((quad.right - 0.5).abs() < 1e-6);
        assert!((quad.bottom + 0.5).abs() < 1e-6);
        assert!((quad.top - 0.5).abs() < 1e-6);
    }

    #[test]
    fn a_slot_scaled_by_a_half_inside_a_layer_moved_right_lands_at_both() {
        // The composition order, pinned to one answer. A 1920-wide composition
        // moved 480 pixels right is half a screen right in NDC; scaled by a half
        // the picture is one screen wide. Left edge at 0, right edge at 1.
        let mut show = show_with(1);
        assert_eq!(show.composition.size, SizePx::new(1920, 1080));
        show.layers[0].slots[0].transform = Transform2D {
            scale_x: 0.5,
            scale_y: 0.5,
            ..Transform2D::IDENTITY
        };
        let mut state = state_showing_everything(show);
        state.layers[0].transform = Transform2D {
            x: 480.0,
            ..Transform2D::IDENTITY
        };

        let mut scene = Scene::new();
        scene.compose(&state, &BTreeMap::new());
        let quad = scene.quads()[0];

        assert!(quad.left.abs() < 1e-6, "left, got {}", quad.left);
        assert!((quad.right - 1.0).abs() < 1e-6, "right, got {}", quad.right);
        assert!((quad.bottom + 0.5).abs() < 1e-6);
        assert!((quad.top - 0.5).abs() < 1e-6);
    }

    #[test]
    fn swapping_which_of_the_two_carries_the_move_gives_a_different_picture() {
        // The same two numbers in the other order are not the same look: the
        // layer scales what the slot moved, so a move on the slot is a smaller
        // move on the wall. That is the whole reason the order is specified.
        let composed_quad = |slot: Transform2D, layer: Transform2D| {
            let mut show = show_with(1);
            show.layers[0].slots[0].transform = slot;
            let mut state = state_showing_everything(show);
            state.layers[0].transform = layer;

            let mut scene = Scene::new();
            scene.compose(&state, &BTreeMap::new());
            scene.quads()[0]
        };

        let half = Transform2D {
            scale_x: 0.5,
            scale_y: 0.5,
            ..Transform2D::IDENTITY
        };
        let right = Transform2D {
            x: 480.0,
            ..Transform2D::IDENTITY
        };

        let scaled_slot = composed_quad(half, right);
        let scaled_layer = composed_quad(right, half);

        // Both are half-size, so the difference is only where they sit.
        assert!((scaled_slot.right - scaled_slot.left - 1.0).abs() < 1e-6);
        assert!((scaled_layer.right - scaled_layer.left - 1.0).abs() < 1e-6);

        assert!(scaled_slot.left.abs() < 1e-6, "got {}", scaled_slot.left);
        assert!(
            (scaled_layer.left + 0.25).abs() < 1e-6,
            "a slot's move is measured in the source's own space, so the layer \
             halves it: got {}",
            scaled_layer.left
        );
        assert_ne!(scaled_slot.left, scaled_layer.left);
    }

    #[test]
    fn two_scales_multiply_rather_than_one_replacing_the_other() {
        let mut show = show_with(1);
        show.layers[0].slots[0].transform = Transform2D {
            scale_x: 0.5,
            scale_y: 0.5,
            ..Transform2D::IDENTITY
        };
        let mut state = state_showing_everything(show);
        state.layers[0].transform = Transform2D {
            scale_x: 0.5,
            scale_y: 0.5,
            ..Transform2D::IDENTITY
        };

        let mut scene = Scene::new();
        scene.compose(&state, &BTreeMap::new());
        let quad = scene.quads()[0];

        assert!((quad.left + 0.25).abs() < 1e-6, "got {}", quad.left);
        assert!((quad.right - 0.25).abs() < 1e-6);
    }

    #[test]
    fn the_two_rotations_add() {
        let mut show = show_with(1);
        show.layers[0].slots[0].transform = Transform2D {
            rotation_deg: 30.0,
            ..Transform2D::IDENTITY
        };
        let mut state = state_showing_everything(show);
        state.layers[0].transform = Transform2D {
            rotation_deg: 60.0,
            ..Transform2D::IDENTITY
        };

        let mut scene = Scene::new();
        scene.compose(&state, &BTreeMap::new());
        assert!((scene.quads()[0].rotation_deg - 90.0).abs() < 1e-6);
    }

    #[test]
    fn a_turned_layer_turns_where_its_slot_sits_as_well_as_what_it_shows() {
        // A quarter turn clockwise takes the layer's `+X` axis to pointing
        // down, so a slot offset to the right ends up below the centre. A
        // transform that turned the picture but left its offset pointing the old
        // way would tear the two apart as the console's rotation channel moved.
        let mut show = show_with(1);
        show.layers[0].slots[0].transform = Transform2D {
            x: 480.0,
            ..Transform2D::IDENTITY
        };
        let mut state = state_showing_everything(show);
        state.layers[0].transform = Transform2D {
            rotation_deg: 90.0,
            ..Transform2D::IDENTITY
        };

        let mut scene = Scene::new();
        scene.compose(&state, &BTreeMap::new());
        let quad = scene.quads()[0];

        let centre_x = (quad.left + quad.right) / 2.0;
        let centre_y = (quad.bottom + quad.top) / 2.0;
        assert!(centre_x.abs() < 1e-6, "no longer to the right: {centre_x}");
        // 480 composition pixels down, on a canvas half of whose height is 540.
        assert!(
            (centre_y + 480.0 / 540.0).abs() < 1e-6,
            "and below the centre instead: {centre_y}"
        );
    }

    #[test]
    fn an_identity_slot_transform_draws_exactly_where_the_layer_alone_would() {
        // Every show committed today has an identity slot transform, and
        // none of them may move because this arrived.
        let mut state = state_showing_everything(show_with(1));
        state.layers[0].transform = Transform2D {
            x: 120.0,
            y: -80.0,
            scale_x: 0.75,
            scale_y: 1.25,
            rotation_deg: 17.0,
        };

        let mut scene = Scene::new();
        scene.compose(&state, &BTreeMap::new());
        let quad = scene.quads()[0];

        // 960 and 540 are the composition's half-extents.
        assert!((quad.left - (120.0 - 960.0 * 0.75) / 960.0).abs() < 1e-6);
        assert!((quad.right - (120.0 + 960.0 * 0.75) / 960.0).abs() < 1e-6);
        assert!((quad.bottom - (-80.0 - 540.0 * 1.25) / 540.0).abs() < 1e-6);
        assert!((quad.top - (-80.0 + 540.0 * 1.25) / 540.0).abs() < 1e-6);
        assert!((quad.rotation_deg - 17.0).abs() < 1e-6);
    }

    #[test]
    fn a_slot_scaled_to_nothing_is_dropped_like_a_layer_scaled_to_nothing() {
        let mut show = show_with(1);
        show.layers[0].slots[0].transform = Transform2D {
            scale_y: 0.0,
            ..Transform2D::IDENTITY
        };
        let state = state_showing_everything(show);

        let mut scene = Scene::new();
        scene.compose(&state, &BTreeMap::new());
        assert!(scene.is_empty());
    }

    #[test]
    fn a_slot_the_show_has_not_configured_draws_nothing_and_moves_nothing() {
        // Two things at once, because the first used to be the second.
        //
        // This asserted that such a layer keeps its own transform, read off the
        // quad it drew — the worry being that guessing at the last slot's
        // transform would move a layer an operator had just emptied. Nothing is
        // drawn for it now, which answers that worry completely: a layer that
        // is not drawn cannot be moved anywhere.
        //
        // The transform still has to reach a layer that *does* have something,
        // so the second half checks that rather than letting it go.
        let show = show_with(1);
        let mut state = state_showing_everything(show);
        state.layers[0].transform = Transform2D {
            x: 480.0,
            ..Transform2D::IDENTITY
        };

        state.layers[0].active_slot = 9;
        let mut scene = Scene::new();
        scene.compose(&state, &BTreeMap::new());
        assert!(
            scene.quads().is_empty(),
            "an emptied layer put something on the wall: {:?}",
            scene.quads()
        );

        // Slot 1 is the one the fixture configures; slot 0 is Black on every
        // layer and draws nothing by design.
        state.layers[0].active_slot = 1;
        scene.compose(&state, &BTreeMap::new());
        let quad = scene.quads()[0];
        assert!((quad.left + 0.5).abs() < 1e-6, "got {}", quad.left);
    }

    #[test]
    fn opacity_becomes_alpha() {
        let show = show_with(1);
        let mut state = state_showing_everything(show);
        state.layers[0].opacity = 0.25;

        let mut scene = Scene::new();
        scene.compose(&state, &BTreeMap::new());
        assert!((scene.quads()[0].colour[3] - 0.25).abs() < 1e-6);
    }

    #[test]
    fn a_fully_transparent_layer_is_dropped_rather_than_drawn() {
        // Drawing it would cost a whole quad to change nothing.
        let show = show_with(1);
        let mut state = state_showing_everything(show);
        state.layers[0].opacity = 0.0;

        let mut scene = Scene::new();
        scene.compose(&state, &BTreeMap::new());
        assert!(scene.is_empty());
    }

    #[test]
    fn a_zero_area_layer_is_dropped() {
        let show = show_with(1);
        let mut state = state_showing_everything(show);
        state.layers[0].transform = Transform2D {
            scale_x: 0.0,
            ..Transform2D::IDENTITY
        };

        let mut scene = Scene::new();
        scene.compose(&state, &BTreeMap::new());
        assert!(scene.is_empty());
    }

    #[test]
    fn layers_compose_bottom_first() {
        // Order 0 composites first and ends up underneath, so it must be drawn
        // first. Getting this backwards would put every show's layers in
        // reverse.
        let show = show_with(3);
        let expected: Vec<LayerId> = show.layers.iter().map(|layer| layer.id).collect();
        let state = state_showing_everything(show);

        let mut scene = Scene::new();
        scene.compose(&state, &BTreeMap::new());

        let drawn: Vec<LayerId> = scene
            .quads()
            .iter()
            .filter_map(|quad| quad.layer_id)
            .collect();
        assert_eq!(drawn, expected);
    }

    #[test]
    fn a_layer_with_media_is_not_tinted_by_its_placeholder_colour() {
        // The placeholder is a distinct hue per layer so that a mapping check
        // means something before there is any media. Once there is, multiplying
        // the picture by that hue would be a wrong colour on the wall that looks
        // like a deliberate one.
        let mut show = Show::new("Show");
        let mut layer = Layer::new("Layer 1", 0);
        layer.slots.push(Slot::media(1, MediaItemId::new()));
        let layer_id = layer.id;
        show.layers.push(layer);

        let state = ShowState {
            sequence: Sequence(1),
            show: Some(Box::new(show)),
            master: MasterLive::default(),
            layers: vec![LayerLive {
                active_slot: 1,
                ..LayerLive::black(layer_id)
            }],
            ..ShowState::default()
        };

        let mut scene = Scene::new();
        scene.compose(&state, &BTreeMap::new());

        let quad = scene.quads().first().expect("one quad");
        assert_eq!(
            quad.colour,
            [1.0, 1.0, 1.0, 1.0],
            "white is the identity for the shader's multiply"
        );
    }

    #[test]
    fn a_quad_carries_the_route_and_blend_mode_of_its_layer() {
        // A/B/M routing and blending are decided upstream of the GPU; the quad
        // only has to carry them faithfully.
        //
        // It carries the **live** ones. They start from what the show
        // configured, which is what this sets, and they move the instant an
        // operator moves them — reading the document instead meant a bus change
        // did nothing until somebody saved (owner, 2026-09-23).
        let mut show = show_with(1);
        show.layers[0].route = Route::B;
        show.layers[0].blend_mode = BlendMode::Add;
        let state = state_showing_everything(show);

        let mut scene = Scene::new();
        scene.compose(&state, &BTreeMap::new());
        assert_eq!(scene.quads()[0].route, Route::B);
        assert_eq!(scene.quads()[0].blend, BlendMode::Add);
    }

    #[test]
    fn a_strobe_set_to_invert_lands_as_a_difference() {
        // The owner, 2026-10-16: "einen modus für invert color strobe, wo der
        // weiße blitz dann inverted color ist." A generator draws its own
        // layer and has never seen what is under it, so the inversion is in
        // how the layer lands — a difference against white — and that is the
        // one case where a slot overrules the blend mode its layer was given.
        let strobe = |mode: Option<&str>| {
            let mut show = show_with(1);
            show.layers[0].blend_mode = BlendMode::Add;
            let parameters = mode.map_or_else(ovs_types::config::slot::ParameterMap::new, |mode| {
                ovs_types::config::slot::ParameterMap::from([(
                    "mode".to_owned(),
                    ovs_types::config::slot::ParameterValue::Choice(mode.to_owned()),
                )])
            });
            show.layers[0].slots[0].source = ovs_types::config::slot::SourceRef::Generator {
                instance_id: ovs_types::id::GeneratorInstanceId::new(),
                type_id: "ovs.strobe".to_owned(),
                version: 2,
                parameters,
            };
            let state = state_showing_everything(show);
            let mut showing = BTreeMap::new();
            showing.insert(state.layers[0].layer_id, true);
            let mut scene = Scene::new();
            scene.compose(&state, &showing);
            scene.quads()[0].blend
        };

        assert_eq!(
            strobe(Some("invert")),
            BlendMode::Difference,
            "an inverting strobe asks for a difference"
        );
        assert_eq!(
            strobe(Some("colour")),
            BlendMode::Add,
            "and the ordinary one leaves the layer's own blend alone"
        );
        assert_eq!(
            strobe(None),
            BlendMode::Add,
            "as does one set to nothing, which is the published default"
        );
    }

    #[test]
    fn a_layer_with_nothing_to_show_draws_nothing() {
        // **Empty is nothing, not a colour** (owner, 2026-09-25: "black/Empty
        // sollte einfach nichts sein").
        //
        // It used to draw a flat hue derived from the layer's identifier — a
        // debug aid from before there was a decoder — so an empty pad looked
        // like content somebody had put there. Nothing rather than black,
        // because a black quad still covers what is under it.
        let mut show = Show::new("Show");
        let mut layer = Layer::new("Layer 1", 0);
        layer.slots.push(Slot::media(1, MediaItemId::new()));
        let layer_id = layer.id;
        show.layers.push(layer);

        let state = state_showing_everything(show);
        // The scheduler's answer for a layer whose clip has no frame: no
        // decoder, a source that failed, or a first frame still on its way.
        let mut showing = BTreeMap::new();
        showing.insert(layer_id, false);

        let mut scene = Scene::new();
        scene.compose(&state, &showing);
        assert!(
            scene.quads().is_empty(),
            "a layer with nothing to show put something on the wall: {:?}",
            scene.quads()
        );

        // And with a frame it is there again, so this is not simply a layer
        // that never draws.
        showing.insert(layer_id, true);
        scene.compose(&state, &showing);
        assert_eq!(scene.quads().len(), 1, "a layer with a frame draws it");
    }

    #[test]
    fn composing_repeatedly_reuses_the_same_allocation() {
        // The realtime rules forbid allocating per frame. Clearing a Vec keeps
        // its capacity, so after the first compose the capacity must not grow.
        let state = state_showing_everything(show_with(8));
        let mut scene = Scene::new();

        scene.compose(&state, &BTreeMap::new());
        let capacity = scene.quads.capacity();
        assert!(capacity >= 8);

        for _ in 0..1_000 {
            scene.compose(&state, &BTreeMap::new());
        }
        assert_eq!(
            scene.quads.capacity(),
            capacity,
            "composing must not reallocate"
        );
        assert_eq!(scene.len(), 8);
    }

    #[test]
    fn the_composition_size_comes_from_the_show() {
        let show = show_with(1);
        assert_eq!(composition_size(Some(&show)), Some(SizePx::new(1920, 1080)));
        assert_eq!(composition_size(None), None);
    }
    /// A quad centred on the canvas with the given NDC half-extents.
    fn sized(half_x: f32, half_y: f32, rotation_deg: f32) -> Quad {
        Quad {
            left: -half_x,
            bottom: -half_y,
            right: half_x,
            top: half_y,
            colour: [1.0, 1.0, 1.0, 1.0],
            rotation_deg,
            anchor: AnchorPoint::CENTRE,
            layer_id: None,
            source: SourceRect::WHOLE,
            source_turn: SourceTurn::NONE,
            texture: TextureRef::None,
            route: Route::M,
            blend: BlendMode::Normal,
        }
    }

    fn about(actual: f32, expected: f32, what: &str) {
        assert!(
            (actual - expected).abs() < 0.001,
            "{what}: expected {expected}, got {actual}"
        );
    }

    #[test]
    fn an_unturned_quad_has_its_half_extents_for_a_basis() {
        // The common case by far, and the one that has to stay exact: a quad
        // that is not turned must land pixel for pixel where its rectangle
        // says, not one rounding error away from it.
        let (centre, basis) = quad_basis(&sized(0.5, 0.25, 0.0), SizePx::new(1920, 1080));

        about(centre[0], 0.0, "centre x");
        about(centre[1], 0.0, "centre y");
        assert_eq!(basis, [0.5, 0.0, 0.0, 0.25]);
    }

    #[test]
    fn a_quad_turned_about_its_own_centre_does_not_move() {
        // The default, and every document written before anchors existed. If
        // this ever stopped holding, every wall already mapped would shift.
        let size = SizePx::new(512, 512);
        let mut quad = sized(0.5, 0.25, 37.0);
        quad.left += 0.3;
        quad.right += 0.3;
        let (centre, _) = quad_basis(&quad, size);

        about(centre[0], (quad.left + quad.right) / 2.0, "centre x");
        about(centre[1], (quad.bottom + quad.top) / 2.0, "centre y");
    }

    #[test]
    fn a_quad_turned_about_a_corner_swings_about_that_corner() {
        // A square canvas, a square quad and a quarter turn, so the answer can
        // be read off rather than trusted: hinged at its top-left, the quad
        // swings so that its centre ends up below and left of where it was.
        let size = SizePx::new(512, 512);
        let mut quad = sized(0.25, 0.25, 90.0);
        quad.anchor = AnchorPoint { x: 0.0, y: 0.0 };

        let (centre, _) = quad_basis(&quad, size);

        // The pivot is the top-left corner at (-0.25, +0.25). The centre sits
        // one quarter right and one quarter down from it; a clockwise quarter
        // turn takes "right and down" to "down and left".
        about(centre[0], -0.5, "centre x");
        about(centre[1], 0.0, "centre y");
    }

    #[test]
    fn the_anchor_moves_the_quad_but_never_its_shape() {
        // Where the turn happens changes where the quad lands, not how big it
        // is or which way round it is. An anchor that changed the basis would
        // be a scale hiding in a rotation.
        let size = SizePx::new(1920, 1080);
        let middle = sized(0.5, 0.25, 41.0);
        let mut cornered = middle;
        cornered.anchor = AnchorPoint { x: 1.0, y: 0.0 };

        let (centre, basis) = quad_basis(&middle, size);
        let (moved, same_basis) = quad_basis(&cornered, size);

        assert_eq!(basis, same_basis, "the anchor changed the quad's shape");
        assert!(
            (centre[0] - moved[0]).abs() > 1e-3 || (centre[1] - moved[1]).abs() > 1e-3,
            "the anchor changed nothing at all"
        );
    }

    #[test]
    fn an_anchor_on_a_quad_that_is_not_turned_changes_nothing() {
        // No turn, nothing to turn about. Worth holding because the cheap path
        // returns before the anchor is ever read.
        let size = SizePx::new(1920, 1080);
        let mut quad = sized(0.5, 0.25, 0.0);
        quad.anchor = AnchorPoint { x: 0.0, y: 1.0 };

        let (centre, basis) = quad_basis(&quad, size);

        about(centre[0], 0.0, "centre x");
        about(centre[1], 0.0, "centre y");
        assert_eq!(basis, [0.5, 0.0, 0.0, 0.25]);
    }

    #[test]
    fn a_quarter_turn_swaps_a_quads_extents() {
        // On a square canvas, where there is no aspect correction to confuse
        // the arithmetic.
        let (_, basis) = quad_basis(&sized(0.5, 0.25, 90.0), SizePx::new(512, 512));

        about(basis[0], 0.0, "x axis, x");
        about(basis[1], -0.5, "x axis, y");
        about(basis[2], 0.25, "y axis, x");
        about(basis[3], 0.0, "y axis, y");
    }

    #[test]
    fn a_square_in_pixels_is_still_square_in_pixels_after_a_quarter_turn() {
        // The reason rotation happens in pixel space. Turning in normalised
        // coordinates on a 16:9 canvas would stretch a square into a lozenge,
        // which is exactly what an operator would report as "the rotation is
        // broken".
        let size = SizePx::new(1920, 1080);
        let half_px = 270.0_f32;
        let quad = sized(half_px * 2.0 / 1920.0, half_px * 2.0 / 1080.0, 90.0);

        let (_, basis) = quad_basis(&quad, size);

        // Back to pixels: the turned x axis should be 270 pixels long, and so
        // should the y axis.
        let x_px = (basis[0] * 1920.0 / 2.0).hypot(basis[1] * 1080.0 / 2.0);
        let y_px = (basis[2] * 1920.0 / 2.0).hypot(basis[3] * 1080.0 / 2.0);
        about(x_px, half_px, "x axis length in pixels");
        about(y_px, half_px, "y axis length in pixels");
    }

    #[test]
    fn a_half_turn_is_the_quad_back_to_front() {
        let (_, basis) = quad_basis(&sized(0.5, 0.25, 180.0), SizePx::new(512, 512));

        about(basis[0], -0.5, "x axis, x");
        about(basis[1], 0.0, "x axis, y");
        about(basis[2], 0.0, "y axis, x");
        about(basis[3], -0.25, "y axis, y");
    }

    #[test]
    fn rotation_is_clockwise_on_screen() {
        // A quarter turn clockwise takes the quad's `+X` axis, which points
        // right, to pointing down — which in these coordinates is `-Y`.
        let (_, basis) = quad_basis(&sized(0.5, 0.5, 90.0), SizePx::new(512, 512));

        assert!(
            basis[1] < 0.0,
            "the x axis should now point down: {basis:?}"
        );
        assert!(basis[2] > 0.0, "and the y axis to the right: {basis:?}");
    }

    #[test]
    fn a_nonsense_angle_leaves_the_quad_alone() {
        // A console sending NaN must not make a layer vanish into an undefined
        // corner of the canvas.
        for angle in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let (_, basis) = quad_basis(&sized(0.5, 0.25, angle), SizePx::new(512, 512));
            assert_eq!(basis, [0.5, 0.0, 0.0, 0.25], "angle {angle}");
        }
    }

    #[test]
    fn a_full_turn_is_where_it_started() {
        let (_, basis) = quad_basis(&sized(0.5, 0.25, 360.0), SizePx::new(512, 512));

        about(basis[0], 0.5, "x axis, x");
        about(basis[1], 0.0, "x axis, y");
        about(basis[2], 0.0, "y axis, x");
        about(basis[3], 0.25, "y axis, y");
    }
}
