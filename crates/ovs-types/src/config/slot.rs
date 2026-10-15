//! Slot configuration.
//!
//! A layer holds many slots but plays exactly one at a time, so slots are cheap
//! to have and only the active one consumes playback resources.
//!
//! Slot 0 is permanently Black and is never represented here. It has no source,
//! no configuration and needs no decoder, which is why there is no separate
//! "layer black" mechanism anywhere in the model.

use serde::{Deserialize, Serialize};

use crate::config::EffectInstance;
use crate::error::{ValidationError, ValidationErrors};
use crate::geometry::Transform2D;
use crate::id::{GeneratorInstanceId, MediaItemId};

/// What happens when a source reaches the end of its timeline.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackMode {
    /// Restart from the beginning, seamlessly.
    #[default]
    Loop,
    /// Play through once, then hold the final frame.
    OnceFreeze,
    /// Play through once, then show black.
    OnceBlack,
    /// Play forward to the end, then backwards to the start, repeatedly.
    Bounce,
}

impl PlaybackMode {
    /// Returns true when the source keeps producing new frames indefinitely.
    #[must_use]
    pub const fn is_continuous(self) -> bool {
        matches!(self, Self::Loop | Self::Bounce)
    }
}

/// The direction a source's timeline advances in.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackDirection {
    /// Normal playback.
    #[default]
    Forward,
    /// Reversed playback.
    Reverse,
}

/// What a slot plays.
///
/// New source types are added as variants here (capture, DeckLink,
/// NDI and HTML as expected future types). Everything downstream of a source
/// treats them identically, so adding one must not require changes to the layer,
/// composition, piece or output model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SourceRef {
    /// A logical item from the media library.
    ///
    /// The show stores the media item's stable identifier and nothing else.
    /// Physical filenames and storage keys live on `MediaAsset` and never appear
    /// here.
    Media {
        /// Logical media identity.
        media_item_id: MediaItemId,
    },
    /// A procedural generator, which implements the same source contract.
    Generator {
        /// This instance's own identity.
        ///
        /// What a patched fixture points at and what a live value is keyed by,
        /// the same way an effect instance's is.
        ///
        /// Optional on the wire, so that a client need not invent one and a
        /// show written before generators had one still opens. The server
        /// gives it a real identity **when the show is saved**, because a
        /// patch entry points at this instance and lives in the same document:
        /// an identity that existed only in the loaded state would be minted
        /// again on the next open, and every fixture patched against the
        /// previous one would be pointing at a generator that no longer exists.
        /// A document already on disk without one gains it when it is opened
        /// (`Show::assign_missing_ids`).
        ///
        /// Nil rather than a fresh one as the `serde` default, because a
        /// default is written into the published schema and a schema that
        /// changes on every build can never be checked against a committed
        /// one — and left out of the wire form entirely when it is nil, so that
        /// "no identity" never reads as "the identity of twenty-six zeroes".
        #[serde(
            default = "GeneratorInstanceId::nil",
            skip_serializing_if = "GeneratorInstanceId::is_nil"
        )]
        instance_id: GeneratorInstanceId,
        /// Stable generator type identifier, for example `ovs.solid_colour`.
        type_id: String,
        /// Manifest version of that generator.
        version: u32,
        /// Generator parameters as defined by its manifest.
        #[serde(default, skip_serializing_if = "ParameterMap::is_empty")]
        parameters: ParameterMap,
    },
    /// A live NDI stream on the network.
    ///
    /// Held by the name the sender announces itself under, because that is the
    /// only handle NDI offers that survives anything: there is no identifier
    /// that outlives a sender restarting, and a position in a discovery list
    /// means nothing a moment later.
    ///
    /// A name that finds nothing on the network is a fault for that layer, the
    /// same as a file that has been deleted. It is not the show being wrong.
    Ndi {
        /// The full NDI source name, as the network announces it.
        ///
        /// `MACHINE (Source Name)` is the shape NDI uses, and it is carried
        /// whole rather than split: the two halves are one identifier and a
        /// client that took them apart would have to put them back together
        /// exactly to find the source again.
        name: String,
    },
}

/// A typed parameter value for an effect or generator instance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum ParameterValue {
    /// A boolean switch.
    Bool(bool),
    /// A whole number.
    Int(i64),
    /// A real number.
    Float(f64),
    /// Free text.
    Text(String),
    /// A colour as linear RGBA in `0.0..=1.0`.
    Color([f32; 4]),
    /// One of a fixed list of named choices, by its stable identifier.
    ///
    /// The identifier, never the label and never the position in the list. A
    /// label is a thing that gets reworded and translated; a position is a
    /// thing that moves when somebody inserts a choice. A show saved last
    /// year has to open looking the way it was left, and the only
    /// value that survives both of those is a name nobody displays.
    Choice(String),
}

/// Parameter values keyed by the names declared in a manifest.
///
/// A sorted map so that serialised shows are byte-stable, which makes export
/// round-trip tests and content hashing meaningful.
pub type ParameterMap = std::collections::BTreeMap<String, ParameterValue>;

/// One configured slot on a layer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Slot {
    /// Slot number as shown in the Live Mode matrix. Never 0.
    pub index: u16,
    /// What this slot plays.
    pub source: SourceRef,
    /// What an operator calls this pad, where they have called it anything.
    ///
    /// Absent is the ordinary case and means the pad is named after what it
    /// plays: the clip's name in the library, or the generator's. A name here
    /// replaces that on the pad and nowhere else — the library keeps its own
    /// names, and the same clip in another slot is unaffected.
    ///
    /// Why it exists (owner, 2026-10-15): "Manchmal kann ja der selbe Inhalt
    /// sein, aber mit anderen Effekte drauf etc. für Dauerhafte installation
    /// wäre das noch gut." Two pads holding one clip with different effects on
    /// them read alike otherwise, and which is which is then remembered rather
    /// than read — once a week, in a venue, by whoever is standing there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Whether this slot follows the beat, and over how many beats.
    ///
    /// On the slot rather than on the layer, because it is a property of the
    /// clip: how long *this* piece of footage takes is a fact about the
    /// footage, and a layer that carried it would apply one answer to every
    /// slot on it (owner, 2026-09-23). A layer therefore has no rate of its
    /// own at all; what it shows is whatever the slot it is playing says.
    #[serde(default, skip_serializing_if = "SlotSync::is_free_run")]
    pub sync: SlotSync,
    /// How fast this slot runs through one pass, as a ratio.
    ///
    /// `1.0` is a pass exactly as long as [`sync`](Self::sync) says: the clip's
    /// own length when it free-runs, or the beats it was given when it is
    /// locked. `2.0` is twice as fast, `0.5` half.
    ///
    /// On **both** kinds of sync, and on the slot (owner, 2026-09-23): "pro
    /// Slot Scale sollt aber verfügbar sein für Free Run und BPM Sync." It is
    /// not the master's scale wearing a different hat — the master's does not
    /// reach a free-running clip at all, and must not, "weil 'Free' ist". This
    /// is the operator's hand on this one clip, and it is the only rate a
    /// free-running slot has.
    ///
    /// Between [`MIN_SPEED_SCALE`](crate::config::MIN_SPEED_SCALE) and
    /// [`MAX_SPEED_SCALE`](crate::config::MAX_SPEED_SCALE), the same ladder the
    /// master runs on, so an operator meets one range rather than two.
    ///
    /// What it cannot do is make a decoder faster. A pass shorter than the clip
    /// asks for frames at a multiple of the rate the file was written at, and
    /// past what the card can decode the picture stops advancing — see
    /// [`SlotSync::BeatSync`].
    #[serde(default = "unit_speed")]
    pub speed: f64,
    /// End-of-timeline behaviour.
    pub playback_mode: PlaybackMode,
    /// Timeline direction.
    pub direction: PlaybackDirection,
    /// Source transform, applied before the layer transform.
    ///
    /// Configuration only. The standard Layer Fixture drives the layer transform
    /// instead.
    pub transform: Transform2D,
    /// Effects applied to this source, in chain order.
    pub effects: Vec<EffectInstance>,
}

/// Whether a slot runs at its own rate or to the show's beat.
///
/// Free run is the default and stays the default: a clip dropped into a slot
/// plays at the speed it was made at, whatever the tempo is doing, until
/// somebody decides otherwise (owner, 2026-09-23). That is what an operator
/// expects of a file they just imported, and it is the only setting under
/// which a clip is certain to be playable — a beat-locked clip runs at whatever
/// multiple of its own rate the tempo asks for, and a decoder has a ceiling.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(tag = "kind", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SlotSync {
    /// Plays at the clip's own rate, at one times, whatever the tempo is.
    #[default]
    FreeRun,
    /// Plays through in this many beats of the show's clock.
    ///
    /// Beats rather than bars, because a clip is not obliged to be a whole
    /// number of bars long and an operator counting a loop counts beats. One
    /// upwards, with no ceiling but the clip's own length: a long clip over few
    /// beats is a fast clip, and how fast is a question for the decoder rather
    /// than for this type.
    BeatSync {
        /// How many beats one pass takes. At least one.
        beats: u32,
    },
}

/// What a slot with no rate written down runs at.
///
/// One: its own. Every document written before a slot had a rate is a document
/// of clips playing at the speed they were made at, and reading one back any
/// other way would change a show that nobody edited.
const fn unit_speed() -> f64 {
    1.0
}

/// The fewest beats a slot may be locked to.
///
/// One. The owner's range is one upwards with no ceiling (2026-09-23), and the
/// only end of it that needs a number is this one: zero beats is not a length,
/// and a pass of no beats is a clip that never advances.
pub const MIN_SLOT_BEATS: u32 = 1;

/// The longest name an operator may give a pad.
///
/// Sixty-four characters, which is what a server's own name is held to, so
/// somebody who has met one limit in this product has met this one. A pad is
/// far too small to show that much — the label ends in an ellipsis long
/// before — but where the cut falls is a question for whoever is naming it
/// and not a reason to refuse what they typed.
pub const MAX_SLOT_NAME_CHARS: usize = 64;

impl SlotSync {
    /// Whether this is the default, which is the one that is left off the wire.
    #[must_use]
    pub const fn is_free_run(&self) -> bool {
        matches!(self, Self::FreeRun)
    }

    /// How many beats a pass takes, or `None` for a slot that free-runs.
    #[must_use]
    pub const fn beats(&self) -> Option<u32> {
        match self {
            Self::FreeRun => None,
            Self::BeatSync { beats } => Some(*beats),
        }
    }
}

impl Slot {
    /// Creates a slot playing a media item with default playback settings.
    #[must_use]
    pub fn media(index: u16, media_item_id: MediaItemId) -> Self {
        Self::with_source(index, SourceRef::Media { media_item_id })
    }

    /// Creates a slot playing any source, with default playback settings.
    ///
    /// What a pad an operator has just dropped something onto becomes: the
    /// clip they chose, and everything else at the value a slot starts at.
    #[must_use]
    pub fn with_source(index: u16, source: SourceRef) -> Self {
        Self {
            index,
            source,
            // Named after what it plays until somebody says otherwise.
            name: None,
            sync: SlotSync::default(),
            speed: 1.0,
            playback_mode: PlaybackMode::default(),
            direction: PlaybackDirection::default(),
            transform: Transform2D::IDENTITY,
            effects: Vec::new(),
        }
    }

    /// Returns the media item this slot references, if it references one.
    #[must_use]
    pub fn media_item_id(&self) -> Option<MediaItemId> {
        match &self.source {
            SourceRef::Media { media_item_id } => Some(*media_item_id),
            SourceRef::Generator { .. } | SourceRef::Ndi { .. } => None,
        }
    }

    pub(crate) fn validate(&self, path: &str, out: &mut ValidationErrors) {
        self.transform.validate(&format!("{path}.transform"), out);

        // A name that is there and says nothing is not a name: a pad carrying
        // an empty string would show nothing where its source's name used to
        // be, which looks like a pad that lost its clip. Absent is how a pad
        // says it is called after what it plays, and that is the only way to
        // say it.
        if let Some(name) = &self.name {
            if name.trim().is_empty() {
                out.push(ValidationError::new(
                    format!("{path}.name"),
                    "must not be empty — leave it out to use the source's own name",
                ));
            }
            let length = name.chars().count();
            if length > MAX_SLOT_NAME_CHARS {
                out.push(ValidationError::new(
                    format!("{path}.name"),
                    format!("must be at most {MAX_SLOT_NAME_CHARS} characters, not {length}"),
                ));
            }
        }

        // A rate outside the ladder is a rate no interface offers and no
        // console can send, so it can only come from a hand-edited document.
        // Refused rather than clamped: a show that says 40x and plays at 4x
        // is a show that lies about itself.
        if !self.speed.is_finite()
            || !(crate::config::MIN_SPEED_SCALE..=crate::config::MAX_SPEED_SCALE)
                .contains(&self.speed)
        {
            out.push(ValidationError::new(
                format!("{path}.speed"),
                format!(
                    "must be between {} and {}",
                    crate::config::MIN_SPEED_SCALE,
                    crate::config::MAX_SPEED_SCALE
                ),
            ));
        }

        // Zero beats is not a number of beats. The renderer already refuses to
        // build a pass out of one, so a slot set this way would simply never
        // play — which is a layer showing its placeholder on the night for a
        // reason nobody can see. Refusing the save is where this belongs.
        if let SlotSync::BeatSync { beats } = self.sync {
            if beats < MIN_SLOT_BEATS {
                out.push(ValidationError::new(
                    format!("{path}.sync.beats"),
                    format!("must be at least {MIN_SLOT_BEATS}"),
                ));
            }
        }

        // An NDI source with no name is a slot that searches the network for
        // nothing and finds it. The same reasoning as the beat count above:
        // it would simply never play, and a layer dark on the night for a
        // reason nobody can see is what refusing the save prevents.
        if let SourceRef::Ndi { name } = &self.source {
            if name.trim().is_empty() {
                out.push(ValidationError::new(
                    format!("{path}.source.name"),
                    "an NDI source is named, and the name is how it is found",
                ));
            }
        }

        if let SourceRef::Generator {
            type_id,
            version,
            parameters,
            ..
        } = &self.source
        {
            if type_id.trim().is_empty() {
                out.push(ValidationError::new(
                    format!("{path}.source.type_id"),
                    "must not be empty",
                ));
            } else {
                // Checked here so that a show which cannot be drawn is
                // refused when it is saved, rather than becoming a layer that
                // shows its placeholder on the night.
                match crate::config::generators::manifest(type_id, *version) {
                    Some(manifest) => {
                        for (key, reason) in manifest.problems(parameters) {
                            out.push(ValidationError::new(
                                format!("{path}.source.parameters.{key}"),
                                reason,
                            ));
                        }
                    }
                    None => out.push(ValidationError::new(
                        format!("{path}.source.type_id"),
                        format!("this build does not publish {type_id} version {version}"),
                    )),
                }
            }
        }

        for (index, effect) in self.effects.iter().enumerate() {
            effect.validate(&format!("{path}.effects[{index}]"), out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ndi_slot_carries_its_name_whole_and_holds_no_media() {
        // `MACHINE (Source Name)` is one identifier. A slot that took it apart
        // would have to put it back together exactly to find the source again,
        // and the two halves mean nothing separately.
        let slot = Slot {
            source: SourceRef::Ndi {
                name: "RACK-2 (Camera 1)".to_owned(),
            },
            ..Slot::media(1, MediaItemId::new())
        };

        assert_eq!(
            slot.media_item_id(),
            None,
            "a stream is not an item in the library, and anything asking which item it is \
             has asked the wrong question"
        );

        let mut errors = ValidationErrors::new();
        slot.validate("layers[0].slots[0]", &mut errors);
        assert!(
            errors.into_result().is_ok(),
            "a named stream is a valid source"
        );
    }

    #[test]
    fn an_ndi_slot_with_no_name_is_refused_rather_than_silently_dark() {
        // A name is the only handle NDI offers, so an empty one searches the
        // network for nothing and finds it. Same reasoning as zero beats
        // below: refused at the save, not discovered on the night.
        let slot = Slot {
            source: SourceRef::Ndi {
                name: "   ".to_owned(),
            },
            ..Slot::media(1, MediaItemId::new())
        };

        let mut errors = ValidationErrors::new();
        slot.validate("layers[0].slots[0]", &mut errors);
        let problems = errors
            .into_result()
            .expect_err("an unnamed stream is refused");
        assert!(
            problems
                .as_slice()
                .iter()
                .any(|problem| problem.path == "layers[0].slots[0].source.name"),
            "{problems:?}"
        );
    }

    #[test]
    fn a_slot_locked_to_no_beats_is_refused_rather_than_silently_unplayable() {
        // The renderer already declines to build a pass out of zero beats, so a
        // slot set this way would never advance — a layer showing its placeholder
        // for a reason nobody standing at the wall can see. The save is where this
        // belongs.
        let mut errors = ValidationErrors::new();
        let mut slot = Slot::media(1, MediaItemId::new());
        slot.sync = SlotSync::BeatSync { beats: 0 };
        slot.validate("layers[0].slots[0]", &mut errors);
        let problems = errors.into_result().expect_err("zero beats is refused");
        assert!(
            problems
                .as_slice()
                .iter()
                .any(|problem| problem.path == "layers[0].slots[0].sync.beats"),
            "{problems:?}"
        );
    }

    #[test]
    fn one_beat_and_a_free_running_slot_are_both_fine() {
        // The ends of the owner's range: one beat upwards, and free run, which is
        // the default and carries no number at all.
        for sync in [
            SlotSync::FreeRun,
            SlotSync::BeatSync {
                beats: MIN_SLOT_BEATS,
            },
            SlotSync::BeatSync { beats: 512 },
        ] {
            let mut errors = ValidationErrors::new();
            let mut slot = Slot::media(1, MediaItemId::new());
            slot.sync = sync;
            slot.validate("slot", &mut errors);
            assert!(errors.into_result().is_ok(), "{sync:?} should be fine");
        }
    }

    #[test]
    fn continuous_modes_are_the_ones_that_never_end() {
        assert!(PlaybackMode::Loop.is_continuous());
        assert!(PlaybackMode::Bounce.is_continuous());
        assert!(!PlaybackMode::OnceFreeze.is_continuous());
        assert!(!PlaybackMode::OnceBlack.is_continuous());
    }

    #[test]
    fn a_media_slot_exposes_only_a_logical_identity() {
        let media = MediaItemId::new();
        let slot = Slot::media(3, media);
        assert_eq!(slot.media_item_id(), Some(media));

        let json = serde_json::to_string(&slot).expect("serialises");
        assert!(json.contains("\"media_item_id\""));
        // Nothing resembling a physical path may ever reach a show file.
        assert!(!json.contains("/storage/"));
        assert!(!json.contains(".mov"));
    }

    #[test]
    fn source_refs_round_trip_through_json() {
        let slot = Slot {
            index: 7,
            name: Some("Upstage bars".to_owned()),
            sync: SlotSync::default(),
            speed: 1.0,
            source: SourceRef::Generator {
                instance_id: GeneratorInstanceId::new(),
                type_id: "ovs.solid_colour".to_owned(),
                version: 1,
                parameters: ParameterMap::from([(
                    "colour".to_owned(),
                    ParameterValue::Color([1.0, 0.0, 0.0, 1.0]),
                )]),
            },
            playback_mode: PlaybackMode::OnceFreeze,
            direction: PlaybackDirection::Reverse,
            transform: Transform2D::IDENTITY,
            effects: Vec::new(),
        };
        let json = serde_json::to_string(&slot).expect("serialises");
        let back: Slot = serde_json::from_str(&json).expect("deserialises");
        assert_eq!(back, slot);
        assert!(back.media_item_id().is_none());
    }

    #[test]
    fn playback_modes_serialise_to_stable_names() {
        assert_eq!(
            serde_json::to_string(&PlaybackMode::OnceFreeze).expect("ok"),
            "\"once_freeze\""
        );
        assert_eq!(
            serde_json::to_string(&PlaybackMode::OnceBlack).expect("ok"),
            "\"once_black\""
        );
        assert_eq!(
            serde_json::to_string(&PlaybackMode::Bounce).expect("ok"),
            "\"bounce\""
        );
    }
}
