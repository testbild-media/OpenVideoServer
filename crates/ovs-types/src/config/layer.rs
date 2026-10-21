//! Layer configuration.
//!
//! A layer is one compositing layer with exactly one active slot at a time
//!. Everything here is persistent configuration. Values that a
//! lighting console drives live — opacity and transform in particular — are
//! stored separately in [`crate::live`] and must never be written back here
//!. The fields below are the *defaults* a layer
//! starts from, which is why they are named as such.

use serde::{Deserialize, Serialize};

use crate::config::slot::{Slot, SourceRef};
use crate::config::EffectInstance;
use crate::error::{ValidationError, ValidationErrors};
use crate::geometry::Transform2D;
use crate::id::LayerId;

/// The bank count a layer has when its document does not say.
///
/// One: the owner's default of 1–12 alone (2026-09-23). A function rather
/// than a literal because that is what `serde` takes, and `Default` would be
/// zero, which is not a number of banks.
const fn one_bank() -> u16 {
    1
}

/// What moving a slot came to ([`Layer::move_slot`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotMove {
    /// Nothing moved: there was nothing at the pad, or it was dropped on itself.
    Nothing,
    /// It went to a pad that was empty.
    Moved,
    /// The pad it went to had a slot of its own, which went the other way.
    Swapped,
}

/// Why a slot index was refused.
///
/// Its own type rather than a string, because two callers act on it
/// differently: a document being validated collects the reason into a list
/// of problems, and an endpoint turns it into a status code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotIndexError {
    /// Slot 0 is permanently Black and holds nothing.
    Black,
    /// Past the last bank a layer may hold.
    PastTheLastBank {
        /// What was asked for.
        index: u16,
        /// The last slot there is.
        highest: u16,
    },
}

impl std::fmt::Display for SlotIndexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Black => {
                write!(f, "slot 0 is permanently Black and cannot be configured")
            }
            Self::PastTheLastBank { index, highest } => {
                write!(f, "slot {index} is past the last slot, which is {highest}")
            }
        }
    }
}

impl std::error::Error for SlotIndexError {}

/// Which bus a layer feeds.
///
/// Exactly one of three, never a blend of them. `A` and `B` feed the global
/// crossfader; `M` bypasses it and goes straight to the master path.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
pub enum Route {
    /// Crossfader side A.
    A,
    /// Crossfader side B.
    B,
    /// Direct master path, bypassing the crossfader.
    #[default]
    M,
}

impl Route {
    /// Returns true when this route passes through the A/B crossfader.
    #[must_use]
    pub const fn is_crossfaded(self) -> bool {
        matches!(self, Self::A | Self::B)
    }
}

/// How a layer composites onto the bus below it.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum BlendMode {
    /// Standard source-over alpha compositing.
    #[default]
    Normal,
    /// Additive.
    Add,
    /// Multiplicative.
    Multiply,
    /// Inverse multiply.
    Screen,
    /// Multiply or screen depending on the backdrop.
    Overlay,
    /// Per-channel maximum.
    Lighten,
    /// Per-channel minimum.
    Darken,
    /// Absolute per-channel difference.
    Difference,
}

/// Who may choose which slot a layer plays.
///
/// The owner, 2026-10-16: "Layer Slotauswahl, pro Layer von DMX oder GUI
/// abschaltbar, also das nur eins schalten kann. Ich habe layer die ich gerne
/// transformieren möchte, aber keinen Slot auswählen möchte per DMX."
///
/// **It exists because the two fight.** On his rig the FX layer is driven
/// entirely from the desk, slots included, while layers 1 to 6 have their slots
/// picked in the client — people who are not lighting operators simply press
/// the picture they want, which turned out to be the practical thing in the
/// room. A console holding its slot channel at a value re-sends it forty-four
/// times a second, so without this the desk wins every time and the button
/// somebody just pressed comes undone under their hand.
///
/// This is only about *choosing a slot*. Everything else a console does to such
/// a layer — the dimmer, the transform, the effects — is untouched, which is
/// the whole point: he wants to transform these layers from the desk and only
/// not to select on them.
///
/// **One or the other, never both** (owner, 2026-10-19: "Pro Layer Slot
/// entweder DMX oder GUI, nicht beides!"). There used to be a third setting
/// that let either choose, and it was the default; the two fought exactly as
/// described above. A document that still says `either`, `client_only` or
/// `desk_only` reads as the one of these two it meant.
///
/// [`Dmx`](Self::Dmx) does not by itself take the slots away from the client.
/// It hands them to a desk *when one is patched to this layer and the DMX
/// switch is on*; until then nothing drives them but the client, which is why
/// it can be the default (see [`crate::control`]).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum SlotControl {
    /// A patched console chooses, while DMX is on. The default.
    #[default]
    #[serde(alias = "either", alias = "desk_only")]
    Dmx,
    /// The client chooses, always. A console's slot channel is read and
    /// ignored here, and everything else it drives on the layer still moves.
    #[serde(alias = "client_only")]
    Gui,
}

impl SlotControl {
    /// Whether a console may select a slot on a layer set this way.
    #[must_use]
    pub const fn desk_may_select(self) -> bool {
        matches!(self, Self::Dmx)
    }
}

/// One compositing layer of a show.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Layer {
    /// Stable identifier.
    pub id: LayerId,
    /// Operator-visible name.
    pub name: String,
    /// Composite order; lower values composite first, so higher values are on top.
    pub order: u32,
    /// Whether the layer contributes to its bus at all.
    pub enabled: bool,
    /// Which bus this layer feeds.
    pub route: Route,
    /// Opacity the layer starts from, in `0.0..=1.0`.
    ///
    /// Live opacity is DMX-driven and lives in [`crate::live`].
    pub default_opacity: f32,
    /// Transform the layer starts from.
    ///
    /// Live transform is DMX-driven and lives in [`crate::live`].
    pub default_transform: Transform2D,
    /// How this layer composites onto the bus.
    pub blend_mode: BlendMode,
    /// Configured slots, never including index 0.
    ///
    /// Slot 0 is permanently Black and is implicit on every layer
    ///. It is not stored, cannot be assigned and needs no decoder.
    pub slots: Vec<Slot>,
    /// How many banks of twelve the matrix shows for this layer.
    ///
    /// Stored rather than worked out from the slots, because an operator adds
    /// a bank in order to put something in it: one derived from the slots would
    /// collapse back to what is filled the moment the screen redrew, and the
    /// empty bank they had just asked for would vanish (owner, 2026-09-23).
    ///
    /// One by default, which is the owner's default of 1–12 alone, and at most
    /// [`Layer::MAX_BANKS`]. A document written before this field existed reads
    /// as one bank and still shows every slot it has — see [`Layer::bank_count`].
    #[serde(default = "one_bank")]
    pub banks: u16,
    /// Who may choose this layer's slot.
    ///
    /// Absent from a document written before the setting existed, which reads
    /// as [`SlotControl::Dmx`]: a desk patched to the layer chose its slots then
    /// too.
    #[serde(default)]
    pub slot_control: SlotControl,
    /// Effects applied to the whole layer, in chain order.
    pub effects: Vec<EffectInstance>,
}

impl Layer {
    /// The reserved index of the permanently black slot.
    pub const BLACK_SLOT_INDEX: u16 = 0;

    /// Slots shown in one bank of the Live Mode matrix.
    ///
    /// Fixed by the interface layout, which shows twelve columns.
    pub const SLOTS_PER_BANK: u16 = 12;

    /// Banks a layer may hold.
    ///
    /// Seven, which is what the reference mock-up draws. This was written down
    /// here as explicitly *not* a ceiling; the owner has since given it one
    /// (2026-09-23): a layer starts with bank 1 alone and further banks are
    /// added deliberately, up to this. A number is needed because an operator
    /// paging right has to be told where the matrix ends.
    pub const MAX_BANKS: u16 = 7;

    /// The highest slot index a layer may hold: seven banks of twelve.
    pub const MAX_SLOT_INDEX: u16 = Self::SLOTS_PER_BANK * Self::MAX_BANKS;

    /// Returns the bank a slot index falls in, counting from zero.
    ///
    /// Slot indices are one-based, so slots 1 to 12 are bank 0 and slot 13
    /// starts bank 1. The permanently black slot 0 belongs to no bank and
    /// returns `None`.
    #[must_use]
    pub const fn bank_of(slot_index: u16) -> Option<u16> {
        if slot_index == Self::BLACK_SLOT_INDEX {
            return None;
        }
        Some((slot_index - 1) / Self::SLOTS_PER_BANK)
    }

    /// Returns the number of banks the matrix shows for this layer.
    ///
    /// The larger of what the layer asks for and what its slots need. The
    /// second half is not defensive: a document written before [`banks`]
    /// existed reads as one bank, and showing one bank of a layer with
    /// thirty slots in it would be hiding an operator's own work.
    ///
    /// At least one, so an empty layer still shows an empty first bank.
    ///
    /// [`banks`]: Self::banks
    #[must_use]
    pub fn bank_count(&self) -> u16 {
        let filled = self
            .slots
            .iter()
            .filter_map(|slot| Self::bank_of(slot.index))
            .max()
            .map_or(1, |highest| highest + 1);
        filled.max(self.banks).max(1)
    }

    /// Creates an empty layer at the given composite order.
    #[must_use]
    pub fn new(name: impl Into<String>, order: u32) -> Self {
        Self {
            id: LayerId::new(),
            name: name.into(),
            order,
            enabled: true,
            route: Route::default(),
            default_opacity: 1.0,
            default_transform: Transform2D::IDENTITY,
            blend_mode: BlendMode::default(),
            slots: Vec::new(),
            banks: 1,
            slot_control: SlotControl::Dmx,
            effects: Vec::new(),
        }
    }

    /// Returns the slot with the given index, if it is configured.
    ///
    /// Index 0 always returns `None` because the black slot has no configuration.
    #[must_use]
    pub fn slot(&self, index: u16) -> Option<&Slot> {
        self.slots.iter().find(|slot| slot.index == index)
    }

    /// Puts a source in a slot, keeping everything else that slot was set to.
    ///
    /// **The settings stay.** An operator who has set a slot to bounce over
    /// sixteen beats and then drags a different clip onto it has changed the
    /// clip, not the way the slot plays; resetting the rest would quietly undo
    /// work they did on purpose. An empty slot is created with the defaults.
    ///
    /// A name the pad was given stays for the same reason, and it is the one
    /// of these settings that is about the pad rather than about the clip:
    /// "Upstage wash" is where an operator reaches, whatever is in it this
    /// week. Emptying the pad takes the name with it, because then there is no
    /// pad left to be called anything.
    ///
    /// Returns what was there before, so a caller can say what it replaced.
    ///
    /// # Errors
    ///
    /// Returns [`SlotIndexError`] for slot 0 and for anything past the last
    /// bank.
    pub fn assign_slot(
        &mut self,
        index: u16,
        source: SourceRef,
    ) -> Result<Option<SourceRef>, SlotIndexError> {
        Self::check_slot_index(index)?;
        if let Some(slot) = self.slots.iter_mut().find(|slot| slot.index == index) {
            return Ok(Some(std::mem::replace(&mut slot.source, source)));
        }

        // Kept in index order, so the document reads the way the matrix draws
        // and two shows with the same slots serialise the same bytes.
        let at = self
            .slots
            .partition_point(|existing| existing.index < index);
        self.slots.insert(at, Slot::with_source(index, source));
        Ok(None)
    }

    /// Puts a whole slot in at its own index, in index order.
    ///
    /// For a slot arriving from another layer ([`crate::config::Show::move_slot`]),
    /// whose pad here the caller has already emptied.
    pub(crate) fn put_slot(&mut self, slot: Slot) {
        let at = self
            .slots
            .partition_point(|existing| existing.index < slot.index);
        self.slots.insert(at, slot);
    }

    /// Empties a slot, leaving the pad unassigned.
    ///
    /// Returns what was in it, so clearing twice is not an error, does not
    /// have to be published as a change, and can be put back.
    pub fn clear_slot(&mut self, index: u16) -> Option<Slot> {
        let at = self.slots.iter().position(|slot| slot.index == index)?;
        Some(self.slots.remove(at))
    }

    /// Moves a slot to another pad of this layer, with everything it carries.
    ///
    /// **The whole slot goes**: the source, how it plays, its effects and the
    /// name the pad was given. Dragging a pad from 1 to 10 is moving the thing
    /// an operator built there, not its clip alone (owner, 2026-10-21).
    ///
    /// **A pad that is taken swaps rather than being overwritten.** The slot
    /// that was at `to` goes to `from`, so a drag that missed by one pad loses
    /// nothing and is undone by dragging back. Replacing what is in a pad is
    /// what dropping a clip from the browser does, and that asks first.
    ///
    /// # Errors
    ///
    /// Returns [`SlotIndexError`] for slot 0 and for anything past the last
    /// bank, at either end.
    pub fn move_slot(&mut self, from: u16, to: u16) -> Result<SlotMove, SlotIndexError> {
        Self::check_slot_index(from)?;
        Self::check_slot_index(to)?;
        if from == to {
            return Ok(SlotMove::Nothing);
        }
        let Some(moving) = self.slots.iter().position(|slot| slot.index == from) else {
            return Ok(SlotMove::Nothing);
        };
        let taken = self.slots.iter().position(|slot| slot.index == to);
        self.slots[moving].index = to;
        if let Some(other) = taken {
            self.slots[other].index = from;
        }
        // Kept in index order, as `assign_slot` keeps it.
        self.slots.sort_by_key(|slot| slot.index);
        Ok(if taken.is_some() {
            SlotMove::Swapped
        } else {
            SlotMove::Moved
        })
    }

    /// Whether a slot index is one a layer could hold.
    ///
    /// # Errors
    ///
    /// Returns [`SlotIndexError`] for slot 0 and for anything past the last
    /// bank.
    pub const fn check_slot_index(index: u16) -> Result<(), SlotIndexError> {
        if index == Self::BLACK_SLOT_INDEX {
            return Err(SlotIndexError::Black);
        }
        if index > Self::MAX_SLOT_INDEX {
            return Err(SlotIndexError::PastTheLastBank {
                index,
                highest: Self::MAX_SLOT_INDEX,
            });
        }
        Ok(())
    }

    pub(crate) fn validate(&self, path: &str, out: &mut ValidationErrors) {
        if self.name.trim().is_empty() {
            out.push(ValidationError::new(
                format!("{path}.name"),
                "must not be empty",
            ));
        }
        if !(0.0..=1.0).contains(&self.default_opacity) || !self.default_opacity.is_finite() {
            out.push(ValidationError::new(
                format!("{path}.default_opacity"),
                "must be between 0.0 and 1.0",
            ));
        }
        self.default_transform
            .validate(&format!("{path}.default_transform"), out);

        if self.banks == 0 || self.banks > Self::MAX_BANKS {
            out.push(ValidationError::new(
                format!("{path}.banks"),
                format!("must be between 1 and {}", Self::MAX_BANKS),
            ));
        }

        let mut seen = std::collections::BTreeSet::new();
        for (index, slot) in self.slots.iter().enumerate() {
            let slot_path = format!("{path}.slots[{index}]");
            if slot.index == Self::BLACK_SLOT_INDEX {
                out.push(ValidationError::new(
                    format!("{slot_path}.index"),
                    "slot 0 is permanently Black and cannot be configured",
                ));
            }
            if slot.index > Self::MAX_SLOT_INDEX {
                out.push(ValidationError::new(
                    format!("{slot_path}.index"),
                    SlotIndexError::PastTheLastBank {
                        index: slot.index,
                        highest: Self::MAX_SLOT_INDEX,
                    }
                    .to_string(),
                ));
            }
            if !seen.insert(slot.index) {
                out.push(ValidationError::new(
                    format!("{slot_path}.index"),
                    format!("duplicate slot index {}", slot.index),
                ));
            }
            slot.validate(&slot_path, out);
        }

        for (index, effect) in self.effects.iter().enumerate() {
            effect.validate(&format!("{path}.effects[{index}]"), out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::MediaItemId;

    #[test]
    fn a_clip_dropped_on_an_empty_pad_lands_in_index_order() {
        // The document reads the way the matrix draws, and two shows with the
        // same slots serialise the same bytes.
        let mut layer = Layer::new("Layer 1", 0);
        for index in [7, 2, 11] {
            layer
                .assign_slot(
                    index,
                    SourceRef::Media {
                        media_item_id: MediaItemId::new(),
                    },
                )
                .expect("a slot a layer can hold");
        }
        let order: Vec<u16> = layer.slots.iter().map(|slot| slot.index).collect();
        assert_eq!(order, vec![2, 7, 11]);
    }

    #[test]
    fn dropping_a_clip_on_an_occupied_pad_keeps_how_the_pad_plays() {
        // An operator who set this pad up did so on purpose. Changing the clip is
        // not a request to undo that.
        let mut layer = Layer::new("Layer 1", 0);
        let before = MediaItemId::new();
        let mut slot = Slot::media(4, before);
        slot.sync = crate::config::SlotSync::BeatSync { beats: 16 };
        slot.playback_mode = crate::config::PlaybackMode::Bounce;
        layer.slots.push(slot);

        let after = MediaItemId::new();
        let replaced = layer
            .assign_slot(
                4,
                SourceRef::Media {
                    media_item_id: after,
                },
            )
            .expect("a slot a layer can hold");

        assert_eq!(
            replaced,
            Some(SourceRef::Media {
                media_item_id: before
            })
        );
        let slot = layer.slot(4).expect("still there");
        assert_eq!(slot.media_item_id(), Some(after));
        assert_eq!(slot.sync, crate::config::SlotSync::BeatSync { beats: 16 });
        assert_eq!(slot.playback_mode, crate::config::PlaybackMode::Bounce);
        assert_eq!(layer.slots.len(), 1, "a second slot 4 was added");
    }

    #[test]
    fn the_black_slot_and_the_end_of_the_matrix_are_not_storage() {
        let mut layer = Layer::new("Layer 1", 0);
        let source = SourceRef::Media {
            media_item_id: MediaItemId::new(),
        };
        assert_eq!(
            layer.assign_slot(Layer::BLACK_SLOT_INDEX, source.clone()),
            Err(SlotIndexError::Black)
        );
        assert_eq!(
            layer.assign_slot(Layer::MAX_SLOT_INDEX + 1, source.clone()),
            Err(SlotIndexError::PastTheLastBank {
                index: Layer::MAX_SLOT_INDEX + 1,
                highest: Layer::MAX_SLOT_INDEX,
            })
        );
        assert!(layer.assign_slot(Layer::MAX_SLOT_INDEX, source).is_ok());
        assert_eq!(Layer::MAX_SLOT_INDEX, 84, "the owner's number");
    }

    #[test]
    fn emptying_a_pad_hands_back_what_was_in_it_and_is_safe_to_repeat() {
        let mut layer = Layer::new("Layer 1", 0);
        let clip = MediaItemId::new();
        layer.slots.push(Slot::media(3, clip));

        let removed = layer.clear_slot(3).expect("there was something in it");
        assert_eq!(removed.media_item_id(), Some(clip));
        assert!(
            layer.clear_slot(3).is_none(),
            "clearing twice is not an error"
        );
    }

    #[test]
    fn routes_know_whether_they_cross_the_fader() {
        assert!(Route::A.is_crossfaded());
        assert!(Route::B.is_crossfaded());
        assert!(!Route::M.is_crossfaded());
    }

    #[test]
    fn a_slot_is_chosen_by_one_side_and_the_old_settings_say_which() {
        // The owner, 2026-10-19: "entweder DMX oder GUI, nicht beides!"
        assert_eq!(
            serde_json::to_string(&SlotControl::Dmx).expect("ok"),
            "\"dmx\""
        );
        assert_eq!(
            serde_json::to_string(&SlotControl::Gui).expect("ok"),
            "\"gui\""
        );
        for (written, meant) in [
            ("\"either\"", SlotControl::Dmx),
            ("\"desk_only\"", SlotControl::Dmx),
            ("\"client_only\"", SlotControl::Gui),
        ] {
            assert_eq!(
                serde_json::from_str::<SlotControl>(written).expect("an older document"),
                meant,
                "{written}"
            );
        }
        assert_eq!(Layer::new("Layer 1", 0).slot_control, SlotControl::Dmx);
    }

    #[test]
    fn enums_serialise_to_the_labels_used_in_the_interface() {
        assert_eq!(serde_json::to_string(&Route::M).expect("ok"), "\"M\"");
        assert_eq!(
            serde_json::to_string(&BlendMode::Normal).expect("ok"),
            "\"normal\""
        );
    }

    #[test]
    fn slot_banks_hold_twelve_slots_and_are_not_capped() {
        assert_eq!(Layer::bank_of(0), None, "the black slot belongs to no bank");
        assert_eq!(Layer::bank_of(1), Some(0));
        assert_eq!(Layer::bank_of(12), Some(0));
        assert_eq!(Layer::bank_of(13), Some(1));
        assert_eq!(Layer::bank_of(84), Some(6));
        // The reference mock-up shows seven banks; nothing stops an eighth.
        assert_eq!(Layer::bank_of(85), Some(7));
        assert_eq!(Layer::bank_of(1000), Some(83));
    }

    #[test]
    fn a_slot_moves_to_an_empty_pad_with_everything_it_carries() {
        let mut layer = Layer::new("Layer 1", 0);
        let clip = MediaItemId::new();
        let mut slot = Slot::media(1, clip);
        slot.name = Some("Upstage wash".to_owned());
        slot.playback_mode = crate::config::PlaybackMode::Bounce;
        layer.slots.push(slot);

        assert_eq!(layer.move_slot(1, 10), Ok(SlotMove::Moved));

        assert!(layer.slot(1).is_none(), "the pad it left is empty");
        let moved = layer.slot(10).expect("it is at 10");
        assert_eq!(
            moved.source,
            SourceRef::Media {
                media_item_id: clip
            }
        );
        assert_eq!(moved.name.as_deref(), Some("Upstage wash"));
        assert_eq!(moved.playback_mode, crate::config::PlaybackMode::Bounce);
    }

    #[test]
    fn a_slot_dropped_on_a_taken_pad_swaps_with_it() {
        // Nothing is lost to a drag that missed by one pad.
        let mut layer = Layer::new("Layer 1", 0);
        let (first, tenth) = (MediaItemId::new(), MediaItemId::new());
        layer.slots.push(Slot::media(1, first));
        layer.slots.push(Slot::media(10, tenth));

        assert_eq!(layer.move_slot(1, 10), Ok(SlotMove::Swapped));

        assert_eq!(
            layer.slot(10).map(|slot| slot.source.clone()),
            Some(SourceRef::Media {
                media_item_id: first
            })
        );
        assert_eq!(
            layer.slot(1).map(|slot| slot.source.clone()),
            Some(SourceRef::Media {
                media_item_id: tenth
            })
        );
        assert_eq!(layer.slots.len(), 2);
    }

    #[test]
    fn slots_stay_in_index_order_after_a_move() {
        // The document reads the way the matrix draws, as `assign_slot` keeps it.
        let mut layer = Layer::new("Layer 1", 0);
        for index in [1, 2, 3] {
            layer.slots.push(Slot::media(index, MediaItemId::new()));
        }
        layer.move_slot(1, 30).expect("moves");
        let order: Vec<u16> = layer.slots.iter().map(|slot| slot.index).collect();
        assert_eq!(order, vec![2, 3, 30]);
        assert_eq!(layer.bank_count(), 3, "slot 30 is in the third bank");
    }

    #[test]
    fn moving_nothing_or_onto_itself_changes_nothing() {
        let mut layer = Layer::new("Layer 1", 0);
        layer.slots.push(Slot::media(1, MediaItemId::new()));
        let before = layer.clone();
        assert_eq!(layer.move_slot(5, 6), Ok(SlotMove::Nothing));
        assert_eq!(layer.move_slot(1, 1), Ok(SlotMove::Nothing));
        assert_eq!(layer, before);
    }

    #[test]
    fn a_move_to_slot_zero_or_past_the_last_bank_is_refused() {
        let mut layer = Layer::new("Layer 1", 0);
        layer.slots.push(Slot::media(1, MediaItemId::new()));
        assert_eq!(layer.move_slot(1, 0), Err(SlotIndexError::Black));
        assert!(matches!(
            layer.move_slot(1, Layer::MAX_SLOT_INDEX + 1),
            Err(SlotIndexError::PastTheLastBank { .. })
        ));
        assert!(layer.slot(1).is_some(), "it stayed where it was");
    }

    #[test]
    fn a_bank_an_operator_added_does_not_collapse_when_it_is_empty() {
        // The whole reason the count is stored. An operator adds a bank in
        // order to put something in it.
        let mut layer = Layer::new("Layer 1", 0);
        layer.banks = 3;
        assert_eq!(layer.bank_count(), 3);
    }

    #[test]
    fn a_document_written_before_banks_existed_still_shows_all_its_slots() {
        // It reads as one bank. Showing one bank of a layer with thirty slots
        // in it would be hiding an operator's own work.
        let mut layer = Layer::new("Layer 1", 0);
        layer.banks = 1;
        layer.slots.push(Slot::media(30, MediaItemId::new()));
        assert_eq!(layer.bank_count(), 3);
    }

    #[test]
    fn bank_count_follows_the_highest_configured_slot() {
        let mut layer = Layer::new("Layer 1", 0);
        assert_eq!(layer.bank_count(), 1, "an empty layer still shows one bank");

        layer
            .slots
            .push(Slot::media(12, crate::id::MediaItemId::new()));
        assert_eq!(layer.bank_count(), 1);

        layer
            .slots
            .push(Slot::media(13, crate::id::MediaItemId::new()));
        assert_eq!(layer.bank_count(), 2);

        layer
            .slots
            .push(Slot::media(200, crate::id::MediaItemId::new()));
        assert_eq!(layer.bank_count(), 17);
    }

    #[test]
    fn a_new_layer_has_no_configurable_black_slot() {
        let layer = Layer::new("Layer 1", 0);
        assert!(layer.slots.is_empty());
        assert!(layer.slot(Layer::BLACK_SLOT_INDEX).is_none());
    }
}
