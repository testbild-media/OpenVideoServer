//! Physical output configuration and identity.
//!
//! Persisting transient display numbering is forbidden, and so is
//! silently remapping a missing output. Both rules are enforced by the identity
//! model here: an output is matched by a composite of GPU, connector and EDID,
//! and a match that is not good enough produces a fault rather than a guess.

use serde::{Deserialize, Serialize};

use crate::error::{ValidationError, ValidationErrors};
use crate::geometry::{FrameRate, SizePx};
use crate::id::OutputId;

/// What a GPU connector is used for.
///
/// This is server configuration, not show data: it describes the machine, and
/// it must never be inferred from what happens to be plugged in.
/// The reference server has no integrated GPU, so the single-GPU case where a
/// service display and media outputs share one card is normal, not exceptional.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ConnectorRole {
    /// Drives the 1024x600 local service console.
    ServiceDisplay,
    /// Drives show output. Never used for anything else.
    MediaOutput,
    /// Deliberately unused.
    #[default]
    Unused,
}

/// Which surfaces are showing the service test pattern.
///
/// Two different questions, which is why this is not a switch.
///
/// **Composition** asks "is the show reaching the walls, and is my mapping
/// right?" The pattern becomes the composition, so the pieces cut it up and
/// each wall shows its own region of it: a wall showing the middle of the
/// pattern is mapped to the middle of the composition.
///
/// **Outputs** asks "is this cable reaching this wall, and in what mode?" Each
/// output draws its own pattern at its own size and rate, with its own name on
/// it, and the mapping is bypassed entirely. An engineer reads the answer off
/// the wall.
///
/// Doing both at once would answer neither, so this is one setting with three
/// values rather than two switches.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TestPatternMode {
    /// The show, as normal.
    #[default]
    Off,
    /// The composition is the pattern, and the pieces map it onto the walls.
    Composition,
    /// Each output is its own pattern, at its own mode, bypassing the mapping.
    Outputs,
    /// Each output draws the pieces that land on it, and nothing else.
    ///
    /// Black, then one filled rectangle per piece at the exact pixels it was
    /// given. What it answers is neither of the other two: not "is this the
    /// wall I think it is" and not "is my mapping sensible", but "is this
    /// rectangle where I drew it, to the pixel, on this wall". On an LED wall
    /// a piece one pixel out is invisible in a preview and obvious here.
    Pieces,
}

impl TestPatternMode {
    /// Whether anything is showing a pattern.
    #[must_use]
    pub const fn is_on(self) -> bool {
        !matches!(self, Self::Off)
    }
}

/// A display mode: resolution plus exact refresh rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DisplayMode {
    /// Resolution in pixels.
    pub size: SizePx,
    /// Refresh rate as an exact rational value.
    pub refresh: FrameRate,
}

impl DisplayMode {
    /// Creates a display mode.
    #[must_use]
    pub const fn new(size: SizePx, refresh: FrameRate) -> Self {
        Self { size, refresh }
    }

    pub(crate) fn validate(&self, path: &str, out: &mut ValidationErrors) {
        self.size.validate(&format!("{path}.size"), out);
        self.refresh.validate(&format!("{path}.refresh"), out);
    }
}

/// How confidently a configured output was matched to real hardware.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum IdentityConfidence {
    /// This is the connector the output is forced onto.
    Forced,
    /// It is not. The output is faulted and its pieces stay dark.
    Unmatched,
}

impl IdentityConfidence {
    /// Returns true when this output may be driven by this connector.
    #[must_use]
    pub const fn is_usable(self) -> bool {
        matches!(self, Self::Forced)
    }
}

/// Which connector an output is forced onto, and what was found there.
///
/// **The GPU and the connector decide; nothing else does.** An output is wired
/// to a socket on the back of a machine and stays there, so a show names that
/// socket and the renderer drives it — whatever is plugged in, and whether or not
/// the driver thinks anything is. Outputs are not hotplugged.
///
/// The EDID fields are what was discovered there, kept for the patch page and
/// for support: "DP-3 says it is a Novastar" is worth being able to read. They
/// are never matched against, because the hardware this product drives cannot be
/// relied on to identify itself — LED processors commonly report no EDID at all,
/// or the same serial on every unit in the rack — and because a
/// processor that is powered off after the server boots would otherwise take its
/// wall dark for the night.
#[derive(
    Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema,
)]
pub struct OutputIdentity {
    /// PCI address of the GPU, for example `0000:01:00.0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_pci_address: Option<String>,
    /// Connector name as reported by the driver, for example `DP-1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connector: Option<String>,
    /// Hash of the EDID identification block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edid_hash: Option<String>,
    /// EDID manufacturer name, when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
    /// EDID model name, when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// EDID serial number, when reported and non-empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial: Option<String>,
}

impl OutputIdentity {
    /// Returns whether `discovered` is the connector this output is forced onto.
    ///
    /// The GPU and the connector, and nothing else. What is plugged in may
    /// change, be powered off, or never have identified itself; the socket does
    /// not move.
    #[must_use]
    pub fn match_against(&self, discovered: &Self) -> IdentityConfidence {
        let forced = self.gpu_pci_address.is_some()
            && self.gpu_pci_address == discovered.gpu_pci_address
            && self.connector.is_some()
            && self.connector == discovered.connector;

        if forced {
            IdentityConfidence::Forced
        } else {
            IdentityConfidence::Unmatched
        }
    }

    /// Returns true when no field carries any identifying information.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Every mode a network output may run.
///
/// Fixed rather than typed (owner, 2026-09-25). A stream has no panel to
/// satisfy, so any size would work on the wire — but a receiver in another room
/// expects a broadcast raster, and a list of three by eight is what an operator
/// picks from without thinking about it. **Interlaced is deliberately absent**:
/// this renderer composes progressive frames and says so rather than offering a
/// shape it would have to invent.
///
/// The rates are exact ratios and not decimals. 23.98 is 24000/1001 and 29.97 is
/// 30000/1001: an NDI frame carries its rate as two integers, and one written as
/// 2997/100 drifts against everything else in the building.
///
/// Published in the server's capabilities, so the client offers what this build
/// actually allows rather than a second copy of this list.
pub const NETWORK_MODES: [DisplayMode; 24] = {
    const fn mode(width: u32, height: u32, numerator: u32, denominator: u32) -> DisplayMode {
        DisplayMode {
            size: SizePx::new(width, height),
            refresh: FrameRate {
                numerator,
                denominator,
            },
        }
    }
    [
        mode(1280, 720, 24000, 1001),
        mode(1280, 720, 24, 1),
        mode(1280, 720, 25, 1),
        mode(1280, 720, 30000, 1001),
        mode(1280, 720, 30, 1),
        mode(1280, 720, 50, 1),
        mode(1280, 720, 60000, 1001),
        mode(1280, 720, 60, 1),
        mode(1920, 1080, 24000, 1001),
        mode(1920, 1080, 24, 1),
        mode(1920, 1080, 25, 1),
        mode(1920, 1080, 30000, 1001),
        mode(1920, 1080, 30, 1),
        mode(1920, 1080, 50, 1),
        mode(1920, 1080, 60000, 1001),
        mode(1920, 1080, 60, 1),
        mode(3840, 2160, 24000, 1001),
        mode(3840, 2160, 24, 1),
        mode(3840, 2160, 25, 1),
        mode(3840, 2160, 30000, 1001),
        mode(3840, 2160, 30, 1),
        mode(3840, 2160, 50, 1),
        mode(3840, 2160, 60000, 1001),
        mode(3840, 2160, 60, 1),
    ]
};

/// Where an output's picture goes.
///
/// A reading of [`OutputConfig`]'s fields, never stored. One stored
/// representation and one way to read it: the alternative is two fields that can
/// both be set and a rule about which wins, and that rule is the bug.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Destination<'a> {
    /// A socket on the back of this machine.
    Connector(&'a OutputIdentity),
    /// A stream on the network, under a name others can find.
    Network(&'a str),
}

/// A configured output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct OutputConfig {
    /// Stable logical identifier, persisted and referenced by pieces.
    pub id: OutputId,
    /// Operator-visible name, for example `LED Wall Left`.
    pub name: String,
    /// Whether this output is driven at all.
    pub enabled: bool,
    /// Hardware identity used to find this output again after a reboot.
    ///
    /// Empty on an output that goes on the network, and refused as non-empty
    /// there: an output cannot be a socket and a stream at once.
    pub identity: OutputIdentity,
    /// The mode this output is configured to run at.
    ///
    /// On a network output this is what the stream declares itself as rather
    /// than a mode set on a panel: the size the composition is sampled into and
    /// the rate the frames are stamped with.
    pub mode: DisplayMode,
    /// Published on the network instead of driven out of a socket.
    ///
    /// The stream carries this output's own [`name`](Self::name), so there is
    /// one name and not two. **What the operator typed is the part in
    /// brackets**: the runtime puts the machine in front of it, so `Wall Left`
    /// on a server called `OPENVIDEOSERVER` is announced as
    /// `OPENVIDEOSERVER (Wall Left)` — measured on the reference machine rather
    /// than assumed. Storing the announced name would rename every stream in a
    /// show the day it moved to another machine.
    ///
    /// A flag rather than a second name, because a second name would have to be
    /// kept in step with the first and the rule about which one wins is where
    /// the fault would live.
    ///
    /// Not decided once and for all: an output can be moved between a socket and
    /// the network either way, which is what happens when a wall is unplugged
    /// for an evening and its feed has to go somewhere.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub on_the_network: bool,
}

impl OutputConfig {
    /// Creates an enabled output with the given name and mode.
    #[must_use]
    pub fn new(name: impl Into<String>, mode: DisplayMode) -> Self {
        Self {
            id: OutputId::new(),
            name: name.into(),
            enabled: true,
            identity: OutputIdentity::default(),
            mode,
            on_the_network: false,
        }
    }

    /// Creates an enabled output that goes on the network under this name.
    ///
    /// One name: what the pieces on this surface are labelled with in the
    /// mapping editor is what somebody in another room picks from a list.
    ///
    /// `mode` has to be one of [`NETWORK_MODES`] or the show will not save.
    #[must_use]
    pub fn published(name: impl Into<String>, mode: DisplayMode) -> Self {
        Self {
            id: OutputId::new(),
            name: name.into(),
            enabled: true,
            identity: OutputIdentity::default(),
            mode,
            on_the_network: true,
        }
    }

    /// Where this output's picture goes.
    #[must_use]
    pub fn destination(&self) -> Destination<'_> {
        if self.on_the_network {
            Destination::Network(&self.name)
        } else {
            Destination::Connector(&self.identity)
        }
    }

    /// Whether this output goes on the network rather than out of a socket.
    ///
    /// Beside [`Self::destination`] because most callers only need the
    /// question and not the answer — a planner handing out connectors, for one.
    #[must_use]
    pub const fn is_on_the_network(&self) -> bool {
        self.on_the_network
    }

    pub(crate) fn validate(&self, path: &str, out: &mut ValidationErrors) {
        if self.name.trim().is_empty() {
            out.push(ValidationError::new(
                format!("{path}.name"),
                "must not be empty",
            ));
        }
        // A stream runs one of the published rasters and nothing else. Refused
        // here rather than left to the client, because the list is only worth
        // having if a document that ignores it does not load.
        if self.on_the_network && !NETWORK_MODES.contains(&self.mode) {
            out.push(ValidationError::new(
                format!("{path}.mode"),
                "must be one of the modes a network output can run",
            ));
        }
        // The two destinations are exclusive, so there is never a rule about
        // which one wins. Refused at the save rather than resolved at the load:
        // a document claiming both was edited by hand or written by a client
        // with a fault, and guessing would put a show on a socket its author
        // meant for the network. The name is already refused empty above, which
        // is also what would make a stream nobody could find.
        if self.on_the_network && !self.identity.is_empty() {
            out.push(ValidationError::new(
                format!("{path}.identity"),
                "must be empty on an output that goes on the network",
            ));
        }
        self.mode.validate(&format!("{path}.mode"), out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(gpu: &str, connector: &str, edid: Option<&str>) -> OutputIdentity {
        OutputIdentity {
            gpu_pci_address: Some(gpu.to_owned()),
            connector: Some(connector.to_owned()),
            edid_hash: edid.map(str::to_owned),
            ..OutputIdentity::default()
        }
    }

    #[test]
    fn an_output_that_does_not_say_which_card_it_is_on_matches_nothing() {
        // Both halves, always. A renderer that reported only the connector
        // drove the panel at the mode the show asked for while the show's
        // own output sat in `missing_outputs` and its pieces stayed dark — the
        // most confusing shape a fault can take, because the screen was lit.
        let stored = identity("0000:01:00.0", "HDMI-0", None);
        let discovered = OutputIdentity {
            gpu_pci_address: None,
            ..stored.clone()
        };

        assert_eq!(
            stored.match_against(&discovered),
            IdentityConfidence::Unmatched
        );
    }

    #[test]
    fn the_connector_it_is_forced_onto_drives_it() {
        let stored = identity("0000:01:00.0", "DP-1", Some("abc"));
        assert_eq!(
            stored.match_against(&stored.clone()),
            IdentityConfidence::Forced
        );
    }

    #[test]
    fn a_different_device_on_the_same_connector_still_drives_it() {
        // The socket is what a show names. A processor swapped for a spare
        // between shows, or one that has just been powered on and reports a
        // different EDID, is the same wall — and refusing it would take that
        // wall dark for the night.
        let stored = identity("0000:01:00.0", "DP-1", Some("abc"));
        let discovered = identity("0000:01:00.0", "DP-1", Some("a different unit"));

        assert_eq!(
            stored.match_against(&discovered),
            IdentityConfidence::Forced
        );
    }

    #[test]
    fn nothing_plugged_in_is_still_the_connector_it_is_forced_onto() {
        // The ordinary case for this product: an LED processor that reports no
        // EDID at all, on a connector a show names.
        let stored = identity("0000:01:00.0", "DP-1", None);
        let discovered = identity("0000:01:00.0", "DP-1", None);

        assert!(stored.match_against(&discovered).is_usable());
    }

    #[test]
    fn a_moved_cable_does_not_take_the_show_with_it() {
        // The failure §18 exists to forbid. The same processor on DP-2 is not
        // the output that was forced onto DP-1, however well it identifies
        // itself: an operator who moved a cable has changed the rig, and the
        // renderer says so rather than following the cable.
        let stored = identity("0000:01:00.0", "DP-1", Some("abc"));
        let discovered = identity("0000:01:00.0", "DP-2", Some("abc"));

        let confidence = stored.match_against(&discovered);
        assert_eq!(confidence, IdentityConfidence::Unmatched);
        assert!(!confidence.is_usable());
    }

    #[test]
    fn the_same_connector_name_on_another_gpu_is_another_socket() {
        // `DP-1` exists on every card in the machine.
        let stored = identity("0000:01:00.0", "DP-1", None);
        let discovered = identity("0000:02:00.0", "DP-1", None);

        assert_eq!(
            stored.match_against(&discovered),
            IdentityConfidence::Unmatched
        );
    }

    #[test]
    fn an_output_that_names_no_connector_is_forced_nowhere() {
        // It has said nothing about where it belongs, so nothing can claim it
        // on those grounds; the planner drives it in order and says so.
        let stored = OutputIdentity::default();
        let discovered = identity("0000:01:00.0", "DP-1", None);

        assert_eq!(
            stored.match_against(&discovered),
            IdentityConfidence::Unmatched
        );
    }

    #[test]
    fn a_missing_output_is_a_fault_rather_than_a_remap() {
        let stored = identity("0000:01:00.0", "DP-1", Some("abc"));
        let other = identity("0000:02:00.0", "HDMI-1", Some("xyz"));
        assert_eq!(stored.match_against(&other), IdentityConfidence::Unmatched);
    }

    #[test]
    fn an_empty_identity_never_matches_anything() {
        let empty = OutputIdentity::default();
        assert!(empty.is_empty());
        assert_eq!(
            empty.match_against(&identity("0000:01:00.0", "DP-1", Some("abc"))),
            IdentityConfidence::Unmatched
        );
        // Two unknown outputs are not the same output.
        assert_eq!(
            empty.match_against(&OutputIdentity::default()),
            IdentityConfidence::Unmatched
        );
    }

    #[test]
    fn connector_roles_serialise_to_the_names_used_in_the_specification() {
        assert_eq!(
            serde_json::to_string(&ConnectorRole::ServiceDisplay).expect("ok"),
            "\"SERVICE_DISPLAY\""
        );
        assert_eq!(
            serde_json::to_string(&ConnectorRole::MediaOutput).expect("ok"),
            "\"MEDIA_OUTPUT\""
        );
        assert_eq!(
            serde_json::to_string(&ConnectorRole::Unused).expect("ok"),
            "\"UNUSED\""
        );
    }

    #[test]
    fn an_output_is_a_socket_or_a_stream_and_says_which() {
        let mode = DisplayMode::new(
            crate::geometry::SizePx::new(1920, 1080),
            crate::geometry::FrameRate::FPS_60,
        );

        let wall = OutputConfig::new("LED Wall Left", mode);
        assert!(!wall.is_on_the_network());
        assert!(matches!(wall.destination(), Destination::Connector(_)));

        let stream = OutputConfig::published("Programme feed", mode);
        assert!(stream.is_on_the_network());
        // One name: the stream is called what the output is called.
        assert_eq!(stream.destination(), Destination::Network("Programme feed"));
    }

    #[test]
    fn a_network_output_may_not_also_name_a_connector() {
        let mode = DisplayMode::new(
            crate::geometry::SizePx::new(1920, 1080),
            crate::geometry::FrameRate::FPS_60,
        );
        let mut output = OutputConfig::published("Programme feed", mode);
        output.identity = identity("0000:01:00.0", "DP-1", None);

        let mut errors = ValidationErrors::default();
        output.validate("outputs[0]", &mut errors);
        assert!(
            errors
                .as_slice()
                .iter()
                .any(|error| error.path == "outputs[0].identity"),
            "claiming a socket and a stream at once must be refused, got {errors:?}"
        );
    }

    #[test]
    fn a_stream_nobody_can_find_is_refused() {
        // The same rule as any other output's, now that the stream carries the
        // output's own name: an unnamed one is a wall dark on the night for a
        // reason nobody standing at it can see.
        let mode = DisplayMode::new(
            crate::geometry::SizePx::new(1920, 1080),
            crate::geometry::FrameRate::FPS_60,
        );
        let output = OutputConfig::published("   ", mode);

        let mut errors = ValidationErrors::default();
        output.validate("outputs[0]", &mut errors);
        assert!(
            errors
                .as_slice()
                .iter()
                .any(|error| error.path == "outputs[0].name"),
            "an unnamed stream must be refused, got {errors:?}"
        );
    }

    /// The published list is what the owner asked for and nothing else.
    #[test]
    fn a_network_output_runs_one_of_three_rasters_at_one_of_eight_rates() {
        assert_eq!(
            NETWORK_MODES.len(),
            24,
            "three rasters, eight rates, no interlace"
        );

        let rasters: std::collections::BTreeSet<(u32, u32)> = NETWORK_MODES
            .iter()
            .map(|mode| (mode.size.width, mode.size.height))
            .collect();
        assert_eq!(
            rasters,
            [(1280, 720), (1920, 1080), (3840, 2160)]
                .into_iter()
                .collect()
        );

        // The fractional ones are ratios, not decimals. A receiver locking to
        // 29.97 written as 2997/100 drifts against everything else in the
        // building.
        assert!(NETWORK_MODES.iter().any(|mode| mode.refresh
            == crate::geometry::FrameRate {
                numerator: 30000,
                denominator: 1001
            }));
        assert!(NETWORK_MODES.iter().any(|mode| mode.refresh
            == crate::geometry::FrameRate {
                numerator: 24000,
                denominator: 1001
            }));
        assert!(NETWORK_MODES.iter().any(|mode| mode.refresh
            == crate::geometry::FrameRate {
                numerator: 60000,
                denominator: 1001
            }));
    }

    #[test]
    fn a_stream_may_not_run_a_size_nobody_publishes() {
        let odd = DisplayMode::new(
            crate::geometry::SizePx::new(1234, 567),
            crate::geometry::FrameRate::FPS_60,
        );
        let output = OutputConfig::published("Programme feed", odd);

        let mut errors = ValidationErrors::default();
        output.validate("outputs[0]", &mut errors);
        assert!(
            errors
                .as_slice()
                .iter()
                .any(|error| error.path == "outputs[0].mode"),
            "a stream outside the list must be refused, got {errors:?}"
        );
    }

    #[test]
    fn a_wall_is_not_held_to_the_streaming_list() {
        // A panel runs whatever its driver offers, which is nothing to do with
        // what a stream may publish.
        let odd = DisplayMode::new(
            crate::geometry::SizePx::new(1024, 768),
            crate::geometry::FrameRate::FPS_60,
        );
        let output = OutputConfig::new("LED Wall", odd);

        let mut errors = ValidationErrors::default();
        output.validate("outputs[0]", &mut errors);
        assert!(
            errors.is_empty(),
            "a wall was refused for a stream's rule: {errors:?}"
        );
    }
}
