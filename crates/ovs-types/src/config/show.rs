//! The show: the unit an operator opens, edits, saves and exports.
//!
//! A show holds composition, layers, slot assignments, playback settings,
//! effects, generators, master settings, pieces, outputs, the DMX patch and
//! media references. It holds no live state at all.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::config::layer::Layer;
use crate::config::output::OutputConfig;
use crate::config::patch::{FixtureInstance, FixtureTarget};
use crate::config::piece::Piece;
use crate::error::{ValidationError, ValidationErrors};
use crate::geometry::{FrameRate, SizePx};
use crate::id::{LayerId, MediaItemId, OutputId, ShowId};
use crate::time::Timestamp;
use crate::version::SHOW_SCHEMA_VERSION;

/// Lowest master BPM the clock accepts.
pub const MIN_BPM: f64 = 20.0;
/// Highest master BPM the clock accepts.
pub const MAX_BPM: f64 = 999.0;
/// Lowest master speed scale.
///
/// Both ends are the ends of the master fixture's scale channel
/// (`MASTER_SCALE_STEPS`, 2026-09-22): a desk that can send a value the show
/// refuses is a desk that reads as broken.
pub const MIN_SPEED_SCALE: f64 = 0.125;
/// Highest master speed scale.
pub const MAX_SPEED_SCALE: f64 = 4.0;

/// The canvas everything composites onto before being cut into pieces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Composition {
    /// Canvas size in pixels.
    pub size: SizePx,
    /// The rate the composition is rendered at.
    ///
    /// Outputs carry their own refresh rate. When they differ, this is the rate
    /// the compositor runs at and outputs present from.
    pub render_rate: FrameRate,
}

impl Default for Composition {
    fn default() -> Self {
        Self {
            size: SizePx::new(1920, 1080),
            render_rate: FrameRate::FPS_60,
        }
    }
}

impl Composition {
    pub(crate) fn validate(&self, path: &str, out: &mut ValidationErrors) {
        self.size.validate(&format!("{path}.size"), out);
        self.render_rate
            .validate(&format!("{path}.render_rate"), out);
    }
}

/// How the A/B crossfader converts its position into bus contributions.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CrossfaderCurve {
    /// Contribution is the fader position, so both buses sit at 0.5 in the middle.
    ///
    /// Which means the *middle of a crossfade is visibly darker than either end*:
    /// two pictures at half contribution sum to half the light, and an audience
    /// sees the wall dip every time the fader crosses. It is here because some
    /// material wants it — a hard cut between two graphics rather than a blend —
    /// and it is no longer the default.
    Linear,
    /// Contribution follows a quarter-sine, holding perceived brightness across the fade.
    ///
    /// The default since the owner's decision of 2026-09-23, and the one a
    /// crossfade should be: the two contributions square-sum to one, so the light
    /// on the wall stays where it was all the way across.
    #[default]
    EqualPower,
}

/// Show-level master settings.
///
/// These are the values a show *starts* from. What the master is doing right
/// now lives in [`crate::live::MasterLive`] and is never written back here.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MasterConfig {
    /// BPM the master clock starts at.
    pub default_bpm: f64,
    /// Beats per bar, which is what the clock counts in.
    pub beats_per_bar: u32,
    /// Master speed scale the show starts at.
    pub default_speed_scale: f64,
    /// Master dimmer a restart returns to, in `0.0..=1.0`.
    ///
    /// Live state is not restored after a restart, so this is the value the show
    /// sits at until a console sends one.
    pub default_dimmer: f32,
    /// Crossfader position a restart returns to: `0.0` is fully A, `1.0` fully B.
    pub default_crossfader: f32,
    /// Crossfader response curve.
    pub crossfader_curve: CrossfaderCurve,
}

impl Default for MasterConfig {
    fn default() -> Self {
        Self {
            default_bpm: 120.0,
            beats_per_bar: 4,
            default_speed_scale: 1.0,
            default_dimmer: 1.0,
            default_crossfader: 0.0,
            crossfader_curve: CrossfaderCurve::default(),
        }
    }
}

impl MasterConfig {
    pub(crate) fn validate(&self, path: &str, out: &mut ValidationErrors) {
        if !self.default_bpm.is_finite() || !(MIN_BPM..=MAX_BPM).contains(&self.default_bpm) {
            out.push(ValidationError::new(
                format!("{path}.default_bpm"),
                format!("must be between {MIN_BPM} and {MAX_BPM}"),
            ));
        }
        if self.beats_per_bar == 0 {
            out.push(ValidationError::new(
                format!("{path}.beats_per_bar"),
                "must be greater than zero",
            ));
        }
        if !self.default_speed_scale.is_finite()
            || !(MIN_SPEED_SCALE..=MAX_SPEED_SCALE).contains(&self.default_speed_scale)
        {
            out.push(ValidationError::new(
                format!("{path}.default_speed_scale"),
                format!("must be between {MIN_SPEED_SCALE} and {MAX_SPEED_SCALE}"),
            ));
        }
        for (name, value) in [
            ("default_dimmer", self.default_dimmer),
            ("default_crossfader", self.default_crossfader),
        ] {
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                out.push(ValidationError::new(
                    format!("{path}.{name}"),
                    "must be between 0.0 and 1.0",
                ));
            }
        }
    }
}

/// Which DMX-over-IP protocols reach this show.
///
/// Two switches rather than one, because a rig is not one thing: a venue's
/// house desk may speak sACN while a visiting console brings Art-Net, and an
/// operator who wants one of them to stop wants the other to keep going
/// (owner, 2026-09-23).
///
/// **Switched off means it does not drive anything, not that it is not
/// listened for.** The sockets stay open and the status keeps saying what is on
/// the wire, exactly as `MasterLive::dmx_enabled` does for both at once: an
/// operator taking a protocol off the show still needs to see that the desk is
/// sending, or they cannot tell "I switched it off" from "the cable is out".
///
/// A server setting rather than a show's. Which protocols a building speaks
/// is a fact about the building, and carrying it in a show would mean a show
/// taken to another venue arrives with the wrong answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DmxProtocols {
    /// Whether Art-Net drives this show.
    pub artnet: bool,
    /// Whether sACN drives this show.
    pub sacn: bool,
}

impl Default for DmxProtocols {
    /// Both on, which is what a server that nobody has configured should do:
    /// a desk that is plugged in and sending should reach the wall.
    fn default() -> Self {
        Self {
            artnet: true,
            sacn: true,
        }
    }
}

impl DmxProtocols {
    /// Neither, which is every DMX source ignored.
    pub const NONE: Self = Self {
        artnet: false,
        sacn: false,
    };

    /// Whether anything at all reaches the show.
    #[must_use]
    pub const fn any(self) -> bool {
        self.artnet || self.sacn
    }
}

/// What a server loads after a reboot.
///
/// This is server configuration rather than show data, but it names a show,
/// so it lives beside the show model.
///
/// Three modes, with `LastActive` as the default (owner decision Q6).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StartupPolicy {
    /// Reopen whichever show was active when the server stopped.
    #[default]
    LastActive,
    /// Always open one specific show, whatever was running before.
    Pinned {
        /// The show to open.
        show_id: ShowId,
    },
    /// Start with no show loaded and all outputs black.
    None,
}

/// The most a clip may be prepared at before it is sent to this show.
///
/// **A property of the wall, not of the laptop that happens to be uploading.**
/// What reaches a panel is never the clip: it is the composition, cut into
/// pieces. On the owner's own wall the largest piece is 840x280, taken from a
/// 1366x444 region of a 1080p composition — so a 720-line clip already puts
/// more than one source pixel behind every pixel out there, while a 2160-line
/// one carries 8.3 megapixels across the bus to show 0.235 of them.
///
/// That waste is paid every frame, for ever: a decoded frame is copied into a
/// staging buffer and then onto the card, and the copy at 2160 is four times
/// the copy at 1080 — about 8.7 ms a layer at 1080p, measured 2026-10-14, on a
/// render thread that has 16.7 ms for everything. One house wants 1080 and
/// another is a wall of small panels where 720 is plenty, so the number lives
/// with the show rather than being guessed per file.
///
/// A ceiling, never a target: nothing is ever made bigger, and a clip that is
/// already under it is left exactly as it is.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
pub enum UploadCeiling {
    /// However big the file is. What a show says until somebody decides.
    #[default]
    #[serde(rename = "SOURCE")]
    Source,
    /// At most 2160 lines.
    #[serde(rename = "2160P")]
    Uhd,
    /// At most 1080 lines.
    #[serde(rename = "1080P")]
    Hd,
    /// At most 720 lines.
    #[serde(rename = "720P")]
    Hd720,
}

impl UploadCeiling {
    /// The number of lines, or `None` where the file's own size is kept.
    #[must_use]
    pub const fn lines(self) -> Option<u32> {
        match self {
            Self::Source => None,
            Self::Uhd => Some(2160),
            Self::Hd => Some(1080),
            Self::Hd720 => Some(720),
        }
    }

    /// Whether this is the "leave it alone" value.
    #[must_use]
    pub const fn is_source(&self) -> bool {
        matches!(self, Self::Source)
    }
}

/// Show-level preferences that are not part of the show itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ShowSettings {
    /// Whether edits are saved automatically.
    pub auto_save: bool,
    /// Free-form operator notes.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
    /// The most a clip may be prepared at before it is uploaded here.
    ///
    /// Left out of the document when it is `SOURCE`, so a show nobody has
    /// decided this for reads exactly as it did before this existed.
    #[serde(default, skip_serializing_if = "UploadCeiling::is_source")]
    pub upload_ceiling: UploadCeiling,
}

impl Default for ShowSettings {
    fn default() -> Self {
        Self {
            auto_save: true,
            notes: String::new(),
            upload_ceiling: UploadCeiling::Source,
        }
    }
}

/// A complete show.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Show {
    /// Stable identifier.
    pub id: ShowId,
    /// Schema version this show was written with.
    ///
    /// Always present and always checked on load, so a show written by a newer
    /// build is refused with an explanation rather than partially understood.
    pub schema_version: u32,
    /// Operator-visible name.
    pub name: String,
    /// When the show was created.
    pub created_at: Timestamp,
    /// When the show was last written.
    pub updated_at: Timestamp,
    /// The composition canvas.
    pub composition: Composition,
    /// Master settings this show starts from.
    pub master: MasterConfig,
    /// Compositing layers.
    pub layers: Vec<Layer>,
    /// Rectangular mappings from composition to outputs.
    pub pieces: Vec<Piece>,
    /// Configured physical outputs.
    pub outputs: Vec<OutputConfig>,
    /// DMX-over-IP patch.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fixture_patch: Vec<FixtureInstance>,
    /// Show preferences.
    pub settings: ShowSettings,
}

impl Show {
    /// Creates an empty show at the current schema version.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        let now = Timestamp::now();
        Self {
            id: ShowId::new(),
            schema_version: SHOW_SCHEMA_VERSION,
            name: name.into(),
            created_at: now,
            updated_at: now,
            composition: Composition::default(),
            master: MasterConfig::default(),
            layers: Vec::new(),
            pieces: Vec::new(),
            outputs: Vec::new(),
            fixture_patch: Vec::new(),
            settings: ShowSettings::default(),
        }
    }

    /// Returns the layer with the given identifier.
    #[must_use]
    pub fn layer(&self, id: LayerId) -> Option<&Layer> {
        self.layers.iter().find(|layer| layer.id == id)
    }

    /// Settles anything an older build left behind in this document.
    ///
    /// Two things.
    ///
    /// **An identity a generator has none of** — a show written before
    /// generators could be patched, or one a client built without inventing
    /// identities of its own. This is where it becomes a real one.
    ///
    /// **A trigger grid still on a layer.** A grid is beats and beats belong to
    /// Called in two places, and both are needed. **On save**, so that the
    /// document which holds the fixture patch also holds the identity that
    /// patch names; without it the identity would be minted again on every
    /// open and every fixture patched against the previous one would point at
    /// a generator that no longer exists. **On load**, for a document already
    /// on disk that was written before any of this existed.
    ///
    /// Returns whether anything changed, so a caller can tell a document it
    /// only meant to read from one it has just altered.
    pub fn assign_missing_ids(&mut self) -> bool {
        let mut changed = false;
        for layer in &mut self.layers {
            for slot in &mut layer.slots {
                if let crate::config::slot::SourceRef::Generator { instance_id, .. } =
                    &mut slot.source
                {
                    if instance_id.is_nil() {
                        *instance_id = crate::id::GeneratorInstanceId::new();
                        changed = true;
                    }
                }
            }
        }
        changed
    }

    /// Brings a show saved while some modules had a second version into line
    /// with there being one of each, and says whether it changed anything.
    ///
    /// The owner, 2026-10-18: "Es gibt kein Invert V2.. nirgendwo. Es ist
    /// Invert!" Every effect, generator and patch entry of a type this build
    /// publishes is given that type's version — one — so nothing a client
    /// shows or a document holds says otherwise. Lookups never read the number
    /// any more; this is so the documents stop carrying it.
    ///
    /// **And a patch entry is given its personality's footprint**, where the
    /// one it recorded is another — an older master's six channels, a strobe
    /// patched before its mode existed — and the new one overlaps no other
    /// entry. Without this, validation would refuse every later edit of the
    /// show until somebody re-patched, with a message about channels that has
    /// nothing to do with the edit. The desk is programmed again for the new
    /// fixture types anyway (owner, 2026-10-18: "das Pult muss eh neu gemacht
    /// werden"). An entry whose new footprint would overlap another is left as
    /// it was, so validation names it rather than this moving a fixture onto
    /// another's channels.
    pub fn adopt_one_of_each(&mut self) -> bool {
        let mut changed = false;
        let mut adopt = |version: &mut u32, current: Option<u32>| {
            if let Some(current) = current {
                if *version != current {
                    *version = current;
                    changed = true;
                }
            }
        };

        for layer in &mut self.layers {
            for effect in &mut layer.effects {
                adopt(
                    &mut effect.version,
                    crate::config::effects::manifest(&effect.type_id).map(|found| found.version),
                );
            }
            for slot in &mut layer.slots {
                for effect in &mut slot.effects {
                    adopt(
                        &mut effect.version,
                        crate::config::effects::manifest(&effect.type_id)
                            .map(|found| found.version),
                    );
                }
                if let crate::config::slot::SourceRef::Generator {
                    type_id, version, ..
                } = &mut slot.source
                {
                    adopt(
                        version,
                        crate::config::generators::manifest(type_id).map(|found| found.version),
                    );
                }
            }
        }

        for index in 0..self.fixture_patch.len() {
            let Some(published) =
                crate::config::fixtures::definition(&self.fixture_patch[index].type_id)
            else {
                continue;
            };
            let fixture = &mut self.fixture_patch[index];
            if fixture.version != published.version {
                fixture.version = published.version;
                changed = true;
            }
            if fixture.footprint == published.footprint {
                continue;
            }
            let mut resized = self.fixture_patch[index].clone();
            resized.footprint = published.footprint;
            let fits = resized.channel_range().is_some()
                && !self
                    .fixture_patch
                    .iter()
                    .enumerate()
                    .any(|(other, entry)| other != index && resized.collides_with(entry));
            if fits {
                self.fixture_patch[index] = resized;
                changed = true;
            }
        }

        changed
    }

    /// Returns every media item this show references, deduplicated and sorted.
    ///
    /// This is what export packaging and reference-counted media deletion are
    /// built on.
    #[must_use]
    pub fn referenced_media(&self) -> BTreeSet<MediaItemId> {
        let mut referenced: BTreeSet<MediaItemId> = self
            .layers
            .iter()
            .flat_map(|layer| layer.slots.iter())
            .filter_map(crate::config::slot::Slot::media_item_id)
            .collect();

        // And the pictures the effects name. A slot was the only way a show
        // could name one until the Mask effect, and the two things this list is
        // built on are exactly the two that would be wrong: a package without
        // the mask in it, and a deletion that reported the picture as used by
        // nobody.
        for layer in &self.layers {
            Self::pictures_of(&layer.effects, &mut referenced);
            for slot in &layer.slots {
                Self::pictures_of(&slot.effects, &mut referenced);
            }
        }
        referenced
    }

    /// Every picture a chain's effects name.
    ///
    /// Through the manifest rather than by looking for a key called `image`:
    /// which parameters name a picture is the effect's own declaration, so an
    /// effect added later needs nothing here.
    fn pictures_of(chain: &[crate::config::EffectInstance], into: &mut BTreeSet<MediaItemId>) {
        for instance in chain {
            let Some(manifest) = crate::config::effects::manifest(&instance.type_id) else {
                // An effect this build does not publish. Its parameters are not
                // ours to read, so nothing is claimed about what it names.
                continue;
            };
            for parameter in manifest.parameters {
                if !matches!(parameter.kind, crate::config::ParameterKind::Image) {
                    continue;
                }
                if let Some(media_item_id) = manifest.media(&instance.parameters, parameter.key) {
                    into.insert(media_item_id);
                }
            }
        }
    }

    /// Returns layers in composite order, bottom first.
    #[must_use]
    pub fn layers_in_composite_order(&self) -> Vec<&Layer> {
        let mut ordered: Vec<&Layer> = self.layers.iter().collect();
        ordered.sort_by_key(|layer| (layer.order, layer.id));
        ordered
    }

    /// Checks every invariant of the show model.
    ///
    /// Collects all problems rather than stopping at the first, so an operator
    /// sees the whole picture in one pass.
    ///
    /// # Errors
    ///
    /// Returns every violated invariant.
    pub fn validate(&self) -> Result<(), ValidationErrors> {
        let mut out = ValidationErrors::new();

        if self.name.trim().is_empty() {
            out.push(ValidationError::new("name", "must not be empty"));
        }
        if self.schema_version == 0 {
            out.push(ValidationError::new("schema_version", "must be at least 1"));
        }

        self.composition.validate("composition", &mut out);
        self.master.validate("master", &mut out);

        self.validate_layers(&mut out);
        let output_ids = self.validate_outputs(&mut out);
        self.validate_pieces(&output_ids, &mut out);
        self.validate_patch(&mut out);

        out.into_result()
    }

    fn validate_layers(&self, out: &mut ValidationErrors) {
        let mut seen_ids = BTreeSet::new();
        let mut seen_orders = BTreeSet::new();

        for (index, layer) in self.layers.iter().enumerate() {
            let path = format!("layers[{index}]");
            if !seen_ids.insert(layer.id) {
                out.push(ValidationError::new(
                    format!("{path}.id"),
                    format!("duplicate layer id {}", layer.id),
                ));
            }
            if !seen_orders.insert(layer.order) {
                out.push(ValidationError::new(
                    format!("{path}.order"),
                    format!(
                        "composite order {} is used by more than one layer, which makes \
                         compositing non-deterministic",
                        layer.order
                    ),
                ));
            }
            layer.validate(&path, out);
        }
    }

    fn validate_outputs(&self, out: &mut ValidationErrors) -> Vec<OutputId> {
        let mut seen = BTreeSet::new();
        for (index, output) in self.outputs.iter().enumerate() {
            let path = format!("outputs[{index}]");
            if !seen.insert(output.id) {
                out.push(ValidationError::new(
                    format!("{path}.id"),
                    format!("duplicate output id {}", output.id),
                ));
            }
            output.validate(&path, out);
        }
        self.outputs.iter().map(|output| output.id).collect()
    }

    fn validate_pieces(&self, output_ids: &[OutputId], out: &mut ValidationErrors) {
        let mut seen = BTreeSet::new();
        for (index, piece) in self.pieces.iter().enumerate() {
            let path = format!("pieces[{index}]");
            if !seen.insert(piece.id) {
                out.push(ValidationError::new(
                    format!("{path}.id"),
                    format!("duplicate piece id {}", piece.id),
                ));
            }
            piece.validate(&path, self.composition.size, output_ids, out);
        }
    }

    fn validate_patch(&self, out: &mut ValidationErrors) {
        /// Checks a patch entry against the personality it names.
        ///
        /// Refused here rather than discovered on stage. A console has the
        /// channel layout physically programmed into its show file, so a patch
        /// that disagrees with the published personality means every fixture
        /// after it is offset — and the first anyone would know of it is a
        /// fader doing the wrong thing during a show.
        fn check_personality(fixture: &FixtureInstance, path: &str, out: &mut ValidationErrors) {
            match crate::config::fixtures::definition(&fixture.type_id) {
                Some(published) => {
                    if published.footprint != fixture.footprint {
                        out.push(ValidationError::new(
                            format!("{path}.footprint"),
                            format!(
                                "{} occupies {} channels, not {}",
                                fixture.type_id, published.footprint, fixture.footprint
                            ),
                        ));
                    }
                    if !published.drives(fixture.target) {
                        out.push(ValidationError::new(
                            format!("{path}.target"),
                            format!(
                                "{} cannot drive what this patch entry points at",
                                fixture.type_id
                            ),
                        ));
                    }
                }
                // A plugin this build does not have. Its personality comes
                // from its own manifest, so a type this build cannot describe
                // is one it cannot check either — and refusing it would make a
                // show unopenable on a machine missing one plugin.
                None if matches!(
                    fixture.target,
                    FixtureTarget::Effect { .. } | FixtureTarget::Generator { .. }
                ) => {}
                None => out.push(ValidationError::new(
                    format!("{path}.type_id"),
                    format!("this build does not publish {}", fixture.type_id),
                )),
            }
        }

        let mut masters = 0usize;
        let known_layers: BTreeSet<LayerId> = self.layers.iter().map(|layer| layer.id).collect();

        for (index, fixture) in self.fixture_patch.iter().enumerate() {
            let path = format!("fixture_patch[{index}]");
            fixture.validate(&path, out);

            match &fixture.target {
                FixtureTarget::Master => masters += 1,
                FixtureTarget::Layer { layer_id } => {
                    if !known_layers.contains(layer_id) {
                        out.push(ValidationError::new(
                            format!("{path}.target.layer_id"),
                            format!("references layer {layer_id} which is not in this show"),
                        ));
                    }
                }
                FixtureTarget::Effect { .. } | FixtureTarget::Generator { .. } => {}
            }

            check_personality(fixture, &path, out);

            for (other_index, other) in self.fixture_patch.iter().enumerate().skip(index + 1) {
                if fixture.collides_with(other) {
                    out.push(ValidationError::new(
                        format!("{path}.start_address"),
                        format!(
                            "collides with fixture_patch[{other_index}] in universe {}",
                            fixture.universe
                        ),
                    ));
                }
            }
        }

        if masters > 1 {
            out.push(ValidationError::new(
                "fixture_patch",
                format!("a show has exactly one master fixture, found {masters}"),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::layer::Layer;
    use crate::config::patch::FixtureInstance;
    use crate::config::slot::Slot;
    use crate::geometry::RectPx;
    use crate::id::FixtureInstanceId;

    #[test]
    fn a_show_that_was_never_told_a_ceiling_keeps_its_own_size() {
        // Every show written before this field existed, which on 2026-10-15 was
        // all of them. The default has to be "leave the file alone": a client
        // that started shrinking an operator's material because a document was
        // silent would be inventing a decision nobody made.
        let written = r#"{"auto_save": true}"#;
        let settings: ShowSettings = serde_json::from_str(written).expect("an older show");
        assert_eq!(settings.upload_ceiling, UploadCeiling::Source);
        assert_eq!(settings.upload_ceiling.lines(), None);
    }

    #[test]
    fn a_ceiling_survives_being_written_and_read_again() {
        for ceiling in [
            UploadCeiling::Source,
            UploadCeiling::Uhd,
            UploadCeiling::Hd,
            UploadCeiling::Hd720,
        ] {
            let settings = ShowSettings {
                upload_ceiling: ceiling,
                ..ShowSettings::default()
            };
            let written = serde_json::to_string(&settings).expect("writes");
            let read: ShowSettings = serde_json::from_str(&written).expect("reads");
            assert_eq!(read.upload_ceiling, ceiling, "in {written}");
        }
    }

    #[test]
    fn the_ceiling_is_left_out_of_a_document_that_does_not_use_it() {
        // A show nobody has decided this for reads exactly as it did before the
        // field existed, which is what keeps a diff of two shows about the show.
        let plain = serde_json::to_string(&ShowSettings::default()).expect("writes");
        assert!(!plain.contains("upload_ceiling"), "{plain}");

        let decided = serde_json::to_string(&ShowSettings {
            upload_ceiling: UploadCeiling::Hd720,
            ..ShowSettings::default()
        })
        .expect("writes");
        assert!(decided.contains("720P"), "{decided}");
    }

    #[test]
    fn every_ceiling_says_how_many_lines_it_means() {
        assert_eq!(UploadCeiling::Uhd.lines(), Some(2160));
        assert_eq!(UploadCeiling::Hd.lines(), Some(1080));
        assert_eq!(UploadCeiling::Hd720.lines(), Some(720));
        assert_eq!(UploadCeiling::Source.lines(), None);
    }

    fn show_with_two_layers() -> Show {
        let mut show = Show::new("Show");
        show.layers.push(Layer::new("Layer 1", 0));
        show.layers.push(Layer::new("Layer 2", 1));
        show
    }

    fn master_fixture() -> FixtureInstance {
        FixtureInstance {
            id: FixtureInstanceId::new(),
            type_id: crate::config::MASTER_V1.type_id.to_owned(),
            version: crate::config::MASTER_V1.version,
            target: FixtureTarget::Master,
            universe: 0,
            start_address: 1,
            footprint: crate::config::MASTER_V1.footprint,
            enabled: true,
        }
    }

    fn layer_fixture(layer_id: LayerId) -> FixtureInstance {
        FixtureInstance {
            type_id: crate::config::LAYER_V1.type_id.to_owned(),
            version: crate::config::LAYER_V1.version,
            target: FixtureTarget::Layer { layer_id },
            footprint: crate::config::LAYER_V1.footprint,
            ..master_fixture()
        }
    }

    #[test]
    fn an_empty_show_is_valid() {
        let show = Show::new("Show");
        assert_eq!(show.validate(), Ok(()));
        assert_eq!(show.schema_version, SHOW_SCHEMA_VERSION);
    }

    #[test]
    fn a_show_round_trips_through_json() {
        let mut show = show_with_two_layers();
        let output = OutputConfig::new(
            "Wall",
            crate::config::output::DisplayMode::new(SizePx::new(1920, 1080), FrameRate::FPS_60),
        );
        show.pieces.push(Piece::new(
            "Full",
            output.id,
            RectPx::covering(show.composition.size),
            RectPx::new(0, 0, 1920, 1080),
        ));
        show.outputs.push(output);
        show.layers[0]
            .slots
            .push(Slot::media(1, MediaItemId::new()));
        assert_eq!(show.validate(), Ok(()));

        let json = serde_json::to_string_pretty(&show).expect("serialises");
        let back: Show = serde_json::from_str(&json).expect("deserialises");
        assert_eq!(back, show);
    }

    #[test]
    fn a_document_written_when_the_trigger_grid_existed_still_opens() {
        // **The owner has a show saved with these fields in it.** Their show
        // on the reference server carries `default_quantization` on every
        // layer, written by a build from before 2026-09-23. Removing a field
        // from a model changes what a stored document means, and the one thing
        // that must not happen is a show that will not open on the night.
        //
        // Nothing in this model refuses unknown fields, so serde drops them —
        // but that is a property worth holding down rather than remembering,
        // because one `deny_unknown_fields` added for a good reason somewhere
        // else would take a show off stage.
        //
        // Built by putting the removed fields back into a document this build
        // writes, rather than by pasting one out of the past: a fixture written
        // by hand goes stale the first time an unrelated field is added, and
        // then tests nothing while still passing.
        let mut show = Show::new("Show");
        let mut layer = Layer::new("Layer 1", 0);
        let mut slot = Slot::media(2, MediaItemId::new());
        slot.sync = crate::config::SlotSync::BeatSync { beats: 7 };
        layer.slots.push(slot);
        show.layers.push(layer);

        let mut document: serde_json::Value = serde_json::to_value(&show).expect("serialises");
        document["layers"][0]["default_quantization"] = serde_json::json!("4");
        document["layers"][0]["slots"][0]["quantization"] = serde_json::json!("1");
        // And the layer's rate, which went a day earlier and is the same case.
        document["layers"][0]["default_speed_ratio"] = serde_json::json!(2.0);

        let opened: Show = serde_json::from_value(document).expect("an older document still opens");
        assert_eq!(opened.validate(), Ok(()));

        let slot = &opened.layers[0].slots[0];
        assert_eq!(slot.sync, crate::config::SlotSync::BeatSync { beats: 7 });
        // What it was doing before it had a rate of its own: the clip's.
        assert!((slot.speed - 1.0).abs() < f64::EPSILON);

        // And none of it comes back on the way out.
        let json = serde_json::to_string(&opened).expect("serialises");
        assert!(!json.contains("quantization"), "the grid does not return");
        assert!(!json.contains("speed_ratio"), "nor the layer's old rate");
    }

    #[test]
    fn duplicate_composite_order_is_rejected() {
        let mut show = Show::new("Show");
        show.layers.push(Layer::new("Layer 1", 0));
        show.layers.push(Layer::new("Layer 2", 0));
        let errors = show.validate().expect_err("orders collide");
        assert!(errors.to_string().contains("non-deterministic"));
    }

    #[test]
    fn slot_zero_cannot_be_configured() {
        let mut show = Show::new("Show");
        let mut layer = Layer::new("Layer 1", 0);
        layer.slots.push(Slot::media(0, MediaItemId::new()));
        show.layers.push(layer);

        let errors = show.validate().expect_err("slot 0 is reserved");
        assert!(errors.to_string().contains("slot 0 is permanently Black"));
    }

    #[test]
    fn duplicate_slot_indices_are_rejected() {
        let mut show = Show::new("Show");
        let mut layer = Layer::new("Layer 1", 0);
        layer.slots.push(Slot::media(4, MediaItemId::new()));
        layer.slots.push(Slot::media(4, MediaItemId::new()));
        show.layers.push(layer);

        let errors = show.validate().expect_err("slot indices collide");
        assert!(errors.to_string().contains("duplicate slot index 4"));
    }

    #[test]
    fn referenced_media_is_deduplicated_across_layers_and_slots() {
        let shared = MediaItemId::new();
        let other = MediaItemId::new();
        let mut show = show_with_two_layers();
        show.layers[0].slots.push(Slot::media(1, shared));
        show.layers[0].slots.push(Slot::media(2, other));
        show.layers[1].slots.push(Slot::media(1, shared));

        let referenced = show.referenced_media();
        assert_eq!(referenced.len(), 2);
        assert!(referenced.contains(&shared));
        assert!(referenced.contains(&other));
    }

    #[test]
    fn a_picture_an_effect_masks_with_is_referenced_media() {
        // **What this list is for decides this.** It is what a package is built
        // from and what "which shows use this?" is answered with before a
        // deletion, so a picture that is only ever a mask would be left out of
        // the package and reported as used by nobody — and the operator who
        // then deleted it would have broken a show that was working.
        let stencil = MediaItemId::new();
        let clip = MediaItemId::new();
        let mut show = Show::new("Show");
        let mut layer = Layer::new("Layer 1", 0);

        let mut on_the_layer = crate::config::EffectInstance::new("ovs.mask", 1);
        on_the_layer.parameters.insert(
            "image".to_owned(),
            crate::config::slot::ParameterValue::Text(stencil.to_string()),
        );
        layer.effects.push(on_the_layer);

        // And one on a slot, because a chain hangs off both.
        let other = MediaItemId::new();
        let mut on_a_slot = crate::config::EffectInstance::new("ovs.mask", 1);
        on_a_slot.parameters.insert(
            "image".to_owned(),
            crate::config::slot::ParameterValue::Text(other.to_string()),
        );
        let mut slot = Slot::media(1, clip);
        slot.effects.push(on_a_slot);
        layer.slots.push(slot);
        show.layers.push(layer);

        let referenced = show.referenced_media();
        assert!(
            referenced.contains(&stencil),
            "the layer's mask: {referenced:?}"
        );
        assert!(
            referenced.contains(&other),
            "the slot's mask: {referenced:?}"
        );
        assert!(referenced.contains(&clip), "and what the slot plays");
    }

    #[test]
    fn an_effect_with_no_picture_chosen_references_nothing() {
        // The default is no picture, and "no picture" must not read as an
        // identifier of twenty-six zeroes or as an empty name in the list.
        let mut show = Show::new("Show");
        let mut layer = Layer::new("Layer 1", 0);
        layer
            .effects
            .push(crate::config::EffectInstance::new("ovs.mask", 1));
        show.layers.push(layer);

        assert!(show.referenced_media().is_empty());
    }

    #[test]
    fn generators_contribute_no_media_references() {
        let mut show = Show::new("Show");
        let mut layer = Layer::new("Layer 1", 0);
        layer.slots.push(Slot {
            index: 1,
            name: None,
            sync: crate::config::slot::SlotSync::default(),
            speed: 1.0,
            source: crate::config::slot::SourceRef::Generator {
                instance_id: crate::id::GeneratorInstanceId::new(),
                type_id: "ovs.solid_colour".to_owned(),
                version: 1,
                parameters: crate::config::slot::ParameterMap::new(),
            },
            playback_mode: crate::config::slot::PlaybackMode::Loop,
            direction: crate::config::slot::PlaybackDirection::Forward,
            transform: crate::geometry::Transform2D::IDENTITY,
            effects: Vec::new(),
        });
        show.layers.push(layer);
        assert!(show.referenced_media().is_empty());
    }

    #[test]
    fn composite_order_is_stable_for_equal_orders() {
        // Equal orders are rejected by validation, but ordering must still be
        // deterministic for a show that somehow contains them.
        let mut show = Show::new("Show");
        show.layers.push(Layer::new("Top", 5));
        show.layers.push(Layer::new("Bottom", 1));
        let ordered = show.layers_in_composite_order();
        assert_eq!(ordered[0].name, "Bottom");
        assert_eq!(ordered[1].name, "Top");
    }

    #[test]
    fn a_show_accepts_exactly_one_master_fixture() {
        let mut show = show_with_two_layers();
        show.fixture_patch.push(master_fixture());
        assert_eq!(show.validate(), Ok(()));

        show.fixture_patch.push(FixtureInstance {
            start_address: 100,
            ..master_fixture()
        });
        let errors = show.validate().expect_err("two masters");
        assert!(errors
            .to_string()
            .contains("exactly one master fixture, found 2"));
    }

    #[test]
    fn patch_collisions_are_detected() {
        let mut show = show_with_two_layers();
        show.fixture_patch.push(master_fixture());
        // The master's last channel, so the two overlap by exactly one however
        // wide the personalities become.
        show.fixture_patch.push(FixtureInstance {
            start_address: crate::config::MASTER_V1.footprint,
            ..layer_fixture(show.layers[0].id)
        });
        let errors = show.validate().expect_err("addresses overlap");
        assert!(errors
            .to_string()
            .contains("collides with fixture_patch[1]"));
    }

    #[test]
    fn a_fixture_pointing_at_a_missing_layer_is_rejected() {
        let mut show = show_with_two_layers();
        show.fixture_patch.push(layer_fixture(LayerId::new()));
        let errors = show.validate().expect_err("unknown layer");
        assert!(errors.to_string().contains("is not in this show"));
    }

    #[test]
    fn a_personality_this_build_does_not_publish_is_refused_at_save() {
        // A show built against a build that is not this one. Refusing it when
        // it is saved is the last moment where it is still somebody's editing
        // mistake rather than a fader doing the wrong thing during a show.
        let mut show = show_with_two_layers();
        show.fixture_patch.push(FixtureInstance {
            type_id: "ovs.spotlight".to_owned(),
            ..master_fixture()
        });
        let errors = show.validate().expect_err("no such personality");
        assert!(errors
            .to_string()
            .contains("does not publish ovs.spotlight"));
    }

    #[test]
    fn a_version_number_is_not_read() {
        // "Es gibt kein Invert V2.. nirgendwo" (owner, 2026-10-18): a patch
        // entry numbered otherwise is the one master all the same.
        let mut show = show_with_two_layers();
        show.fixture_patch.push(FixtureInstance {
            version: 2,
            ..master_fixture()
        });
        assert_eq!(show.validate(), Ok(()));
    }

    /// A show as it was saved while modules still had second versions, and
    /// the master six channels.
    fn show_from_before() -> Show {
        let mut show = show_with_two_layers();
        show.layers[0]
            .effects
            .push(crate::config::EffectInstance::new("ovs.invert", 2));
        show.layers[0].slots.push(crate::config::Slot::with_source(
            1,
            crate::config::slot::SourceRef::Generator {
                instance_id: crate::id::GeneratorInstanceId::new(),
                type_id: "ovs.strobe".to_owned(),
                version: 2,
                parameters: crate::config::slot::ParameterMap::new(),
            },
        ));
        show.fixture_patch.push(FixtureInstance {
            version: 2,
            footprint: 6,
            ..master_fixture()
        });
        show
    }

    #[test]
    fn a_show_from_before_is_numbered_as_the_modules_now_are() {
        let mut show = show_from_before();
        assert!(show.adopt_one_of_each(), "it had something to change");

        assert_eq!(show.layers[0].effects[0].version, 1);
        let crate::config::slot::SourceRef::Generator { version, .. } =
            &show.layers[0].slots[0].source
        else {
            panic!("the slot holds the strobe");
        };
        assert_eq!(*version, 1);
        assert_eq!(show.fixture_patch[0].version, 1);

        // And the master's six channels become its five, so the show can be
        // saved again without anybody having to re-patch it first.
        assert_eq!(
            show.fixture_patch[0].footprint,
            crate::config::MASTER_V1.footprint
        );
        assert_eq!(show.validate(), Ok(()));

        // Done once.
        assert!(!show.adopt_one_of_each());
    }

    #[test]
    fn a_footprint_that_would_land_on_another_fixture_is_left_for_validation() {
        // A layer patched straight after a master that took seven channels:
        // shrinking the master overlaps nothing, but a footprint that grew would
        // have, and it is left as it was so validation names it rather than
        // this moving one fixture onto another's channels.
        let mut show = show_with_two_layers();
        let layer_id = show.layers[0].id;
        let published = crate::config::LAYER_V1.footprint;
        show.fixture_patch.push(FixtureInstance {
            footprint: published - 2,
            ..layer_fixture(layer_id)
        });
        show.fixture_patch.push(FixtureInstance {
            start_address: 1 + published - 2,
            ..master_fixture()
        });

        assert!(!show.adopt_one_of_each());
        assert_eq!(show.fixture_patch[0].footprint, published - 2);
        assert!(show
            .validate()
            .expect_err("still the wrong footprint")
            .to_string()
            .contains("occupies"));
    }

    #[test]
    fn a_footprint_that_disagrees_with_the_personality_is_refused() {
        // The console has the real answer physically programmed into its show
        // file, so a disagreement means every fixture patched after this one is
        // offset.
        let mut show = show_with_two_layers();
        let real = crate::config::MASTER_V1.footprint;
        show.fixture_patch.push(FixtureInstance {
            footprint: real + 2,
            ..master_fixture()
        });
        let errors = show.validate().expect_err("wrong footprint");
        assert!(errors
            .to_string()
            .contains(&format!("occupies {real} channels, not {}", real + 2)));
    }

    #[test]
    fn a_personality_aimed_at_the_wrong_kind_of_target_is_refused() {
        // It would give one layer's opacity fader the master dimmer.
        let mut show = show_with_two_layers();
        show.fixture_patch.push(FixtureInstance {
            target: FixtureTarget::Master,
            ..layer_fixture(show.layers[0].id)
        });
        let errors = show.validate().expect_err("wrong target");
        assert!(errors
            .to_string()
            .contains("cannot drive what this patch entry points at"));
    }

    #[test]
    fn an_effect_fixture_is_not_checked_against_a_published_personality() {
        // An effect's personality comes from that effect's own manifest, so
        // nothing here can check it, and refusing it would make the feature
        // unpatchable before it exists.
        let mut show = show_with_two_layers();
        show.fixture_patch.push(FixtureInstance {
            type_id: "third.party.strobe".to_owned(),
            target: FixtureTarget::Effect {
                effect_instance_id: crate::id::EffectInstanceId::new(),
            },
            footprint: 4,
            ..master_fixture()
        });
        assert_eq!(show.validate(), Ok(()));
    }

    #[test]
    fn master_settings_are_range_checked() {
        let mut show = Show::new("Show");
        show.master.default_bpm = 5.0;
        show.master.default_speed_scale = 100.0;
        show.master.beats_per_bar = 0;
        let errors = show.validate().expect_err("out of range");
        assert_eq!(errors.len(), 3);
    }

    #[test]
    fn master_restart_defaults_are_range_checked() {
        let mut show = Show::new("Show");
        show.master.default_dimmer = 2.0;
        show.master.default_crossfader = -1.0;
        let errors = show.validate().expect_err("out of range");
        assert_eq!(errors.len(), 2);
        assert!(errors.to_string().contains("master.default_dimmer"));
        assert!(errors.to_string().contains("master.default_crossfader"));
    }

    #[test]
    fn validation_reports_every_problem_at_once() {
        let mut show = Show::new("   ");
        show.master.default_bpm = f64::NAN;
        show.layers.push(Layer {
            default_opacity: 2.0,
            ..Layer::new("", 0)
        });
        let errors = show.validate().expect_err("several problems");
        assert!(errors.len() >= 4, "expected several problems, got {errors}");
    }
}
