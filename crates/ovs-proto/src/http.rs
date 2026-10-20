//! The versioned HTTP API contract.
//!
//! Request and response bodies only: routing and handlers live in the server
//! core. Keeping the shapes here is what lets the desktop client be generated
//! from the same definitions as the server.
//!
//! Everything is served under [`API_PREFIX`]. Within a major version the API is
//! additive: a field may be added, never removed or repurposed.

use ovs_types::config::{
    ConnectorRole, DmxProtocols, ParameterMap, Route, Show, SourceRef, StartupPolicy,
};
use ovs_types::geometry::{FrameRate, SizePx, Transform2D};
use ovs_types::id::{AccountId, EffectInstanceId, MediaItemId, OutputId, ShowId};
use ovs_types::media::{MediaItem, MediaKind};
use ovs_types::telemetry::ServerTelemetry;
use ovs_types::time::Timestamp;
use ovs_types::version::{API_VERSION, REALTIME_PROTOCOL_VERSION};
use serde::{Deserialize, Serialize};

/// Path prefix every endpoint is served under.
pub const API_PREFIX: &str = "/api/v1";

/// An error returned by any endpoint.
///
/// A single shape for every failure, so a client has one thing to handle rather
/// than one per endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ApiError {
    /// Short machine-readable code, for example `show_not_found`.
    pub code: String,
    /// Human-readable description, suitable for showing an operator.
    pub message: String,
    /// Field-level problems, when the request failed validation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub details: Vec<ovs_types::ValidationError>,
}

impl ApiError {
    /// Builds an error with no field-level detail.
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: Vec::new(),
        }
    }

    /// Builds an error carrying every validation problem.
    pub fn invalid(errors: ovs_types::ValidationErrors) -> Self {
        Self {
            code: "invalid".to_owned(),
            message: "the request did not pass validation".to_owned(),
            details: errors.as_slice().to_vec(),
        }
    }
}

// --- /auth ---------------------------------------------------------------

/// Credentials for a sign-in.
///
/// `Debug` is implemented by hand so a password cannot reach a log through a
/// derived formatter.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LoginRequest {
    /// Login name.
    pub username: String,
    /// Plain password, sent over TLS and never stored.
    pub password: String,
}

impl std::fmt::Debug for LoginRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginRequest")
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .finish()
    }
}

/// A successful sign-in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LoginResponse {
    /// Bearer token for subsequent requests.
    pub token: String,
    /// When the token stops being accepted.
    pub expires_at: Timestamp,
    /// Which account signed in.
    ///
    /// V0.x has one account, but the response carries it so that a client built
    /// against this API keeps working when there are several.
    pub account_id: AccountId,
}

/// Creates an account.
///
/// Two callers, one shape. On a server with no accounts this is first-run setup
/// and needs no session; on one that has them it is an administrator adding a
/// colleague. What differs is the authority required, not the request.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CreateAccountRequest {
    /// Login name.
    pub username: String,
    /// Plain password.
    pub password: String,
    /// What the account may do.
    ///
    /// Absent is [`Role::Operator`](ovs_types::account::Role::Operator), which
    /// is the safe half: an account created without anybody saying is an
    /// account nobody decided to trust with the machine. First-run setup is the
    /// exception and makes an administrator whatever this says, because a
    /// server whose only account cannot administer it is a server nobody can
    /// finish setting up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<ovs_types::account::Role>,
}

impl std::fmt::Debug for CreateAccountRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreateAccountRequest")
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .field("role", &self.role)
            .finish()
    }
}

/// One account, as everything outside the server sees it.
///
/// **No password hash.** The field exists in storage and has no business on a
/// wire: an interface that has it is an interface somebody can be talked into
/// showing, and a hash is worth attacking offline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AccountSummary {
    /// Stable identifier.
    pub account_id: AccountId,
    /// Login name.
    pub username: String,
    /// What this account may do.
    pub role: ovs_types::account::Role,
    /// When it was created, in milliseconds since the Unix epoch.
    pub created_at: i64,
    /// When it last signed in, if it ever has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_login_at: Option<i64>,
    /// Whether this is the only account that can administer the server.
    ///
    /// Sent rather than worked out by the reader, because the rule belongs to
    /// the server: an interface that counted admins itself would grey out the
    /// wrong button the moment the rule changed, or offer one the server then
    /// refuses.
    pub last_administrator: bool,
}

/// Changes an account.
///
/// Every field is optional and an absent one means "leave this alone". The
/// password is not here: it has its own request, because changing one is a
/// different act with different authority behind it.
#[derive(Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UpdateAccountRequest {
    /// A new login name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// A new role.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<ovs_types::account::Role>,
}

impl std::fmt::Debug for UpdateAccountRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpdateAccountRequest")
            .field("username", &self.username)
            .field("role", &self.role)
            .finish()
    }
}

/// Sets an account's password.
///
/// **`current` is what separates the two ways in.** Somebody changing their own
/// password proves they know it; an administrator resetting somebody else's
/// does not have it and does not need it. A request that leaves it out for
/// itself is refused, which is what stops a borrowed unlocked screen from
/// becoming a stolen account.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SetPasswordRequest {
    /// The password in use now, when changing one's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<String>,
    /// The new password.
    pub password: String,
}

impl std::fmt::Debug for SetPasswordRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SetPasswordRequest")
            .field("current", &self.current.as_ref().map(|_| "<redacted>"))
            .field("password", &"<redacted>")
            .finish()
    }
}

/// Who a request is acting as.
///
/// What the interface asks for before it draws: a page that offered user
/// management to an operator would be a page whose buttons the server refuses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WhoAmIResponse {
    /// The account this session belongs to.
    pub account: AccountSummary,
}

// --- /server -------------------------------------------------------------

/// What this server can do.
///
/// A client reads this before offering anything, so that a server without, say,
/// a second GPU does not present options that cannot work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CapabilitiesResponse {
    /// Server build version.
    pub server_version: String,
    /// Major version of this API.
    pub api_version: u32,
    /// Realtime protocol version the server speaks.
    pub realtime_protocol_version: u32,
    /// Show schema version the server writes.
    pub show_schema_version: u32,
    /// Whether an operator account exists yet.
    ///
    /// `false` means the server is in first-run setup.
    pub configured: bool,
    /// Codecs this server has a decoder for, by `FFmpeg`'s short name.
    ///
    /// The server plays what it is given, so this is what a client
    /// consults before offering to convert a file rather than send it. It is
    /// the interesting subset rather than every decoder in the binary, and it
    /// is asked of the binary rather than declared in the source.
    ///
    /// Empty means *this server cannot say* — a build with no `FFmpeg` — and
    /// must not be read as "plays nothing".
    pub media_codecs: Vec<String>,
    /// Physical connectors the renderer can drive.
    ///
    /// Its own card's, and the only ones a show output may be put on: the
    /// renderer enumerates displays on the one Vulkan device it renders with and
    /// matches roles against the names that device reports.
    pub connectors: Vec<ConnectorInfo>,
    /// Sockets on the other cards in the machine, which the renderer cannot
    /// drive.
    ///
    /// The System screen is chosen by *card* — wlroots takes a card, not a
    /// connector — so these can carry it, and on a machine with two GPUs that is
    /// where it belongs: the show's card should not also be carrying a browser.
    /// They can never be a show output, which is why they are a second list
    /// rather than more of the first.
    ///
    /// Named by the kernel (`eDP-1`) rather than by Vulkan (`monitor`), because
    /// no renderer has seen them. The two layers disagree about names and there
    /// is no safe way to map one onto the other, so nothing tries.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub connectors_elsewhere: Vec<ConnectorInfo>,
    /// Every mode an output published on the network may run.
    ///
    /// Fixed by this build rather than by the hardware: a stream has no panel
    /// to satisfy, and what a receiver in another room expects is a broadcast
    /// raster. Sent so that a client offers what this server actually allows
    /// instead of carrying its own copy of the list — two copies are two lists,
    /// and the one edited less often is the one that is wrong.
    ///
    /// Whole modes rather than a number of hertz, because the rates are ratios:
    /// 23.98 is 24000/1001, and a receiver locking to it written as a decimal
    /// drifts against everything else in the building.
    pub network_output_modes: Vec<ovs_types::config::DisplayMode>,
}

/// A GPU connector as discovered on the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ConnectorInfo {
    /// PCI address of the GPU driving it.
    pub gpu_pci_address: String,
    /// Connector name as the driver reports it, for example `DP-1`.
    pub connector: String,
    /// What this connector is configured for.
    pub role: ConnectorRole,
    /// Whether something is currently plugged in.
    pub connected: bool,
    /// EDID model name, when the attached device reports one.
    ///
    /// For recognising hardware in the client and for support calls. It decides
    /// nothing: outputs are forced onto connectors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The modes this connector offers, as the driver reports them.
    ///
    /// Empty until a renderer has said. An operator choosing a mode for a wall
    /// needs the list, and inventing one would offer modes the panel cannot do.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modes: Vec<ConnectorModeInfo>,
}

/// One mode a connector offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ConnectorModeInfo {
    /// Resolution in pixels.
    pub size: ovs_types::geometry::SizePx,
    /// Refresh rate in millihertz, as the driver reports it.
    ///
    /// Not a float: 59 940 is 60000/1001 exactly, and the two rates a panel
    /// actually has must not round into one.
    pub refresh_millihertz: u32,
}

/// Current server state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct StatusResponse {
    /// Measured telemetry.
    pub telemetry: ServerTelemetry,
    /// What this server opens on start.
    pub startup_policy: StartupPolicy,
    /// Which DMX-over-IP protocols reach the show.
    ///
    /// A server setting rather than a show's: which protocols a building
    /// speaks is a fact about the building.
    #[serde(default)]
    pub dmx_protocols: DmxProtocols,
    /// Whether the renderer is producing previews at all.
    ///
    /// Reported because the alternative is guessing. A page with no picture to
    /// show cannot otherwise tell "switched off" from "the renderer has not
    /// sent one yet", and those want opposite things done about them.
    ///
    /// Defaulted rather than required, so a client built against an older
    /// server still reads this response.
    #[serde(default)]
    pub previews_enabled: bool,
}

// --- /shows -----------------------------------------------------------

/// One entry in the show list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ShowListEntry {
    /// Stable identifier.
    pub id: ShowId,
    /// Operator-visible name.
    pub name: String,
    /// Schema version the stored show uses.
    pub schema_version: u32,
    /// When it was created.
    pub created_at: Timestamp,
    /// When it was last saved.
    pub updated_at: Timestamp,
    /// Whether this server build can open it.
    ///
    /// A show it cannot open is still listed, so it does not look deleted.
    pub loadable: bool,
    /// Where it is filed in this server's list.
    ///
    /// Slash-separated, empty for the root. Not part of the document: where a
    /// show is filed is a fact about this server's list rather than about
    /// the show (owner, 2026-09-23).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub folder: String,
}

/// The folders the show list is organised into.
///
/// Its own endpoint rather than a field on the list, because `GET /shows`
/// answers with an array and this API is additive within a major version: a
/// field may be added to an entry, but the shape of a response is not
/// repurposed under a client that is already reading it.
///
/// Sent whole rather than worked out from the shows, because an operator
/// makes a folder in order to put a show in it: one derived from its contents
/// could not exist between being made and being filled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ShowFoldersResponse {
    /// Every folder, in path order. The root is not in it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub folders: Vec<String>,
}

/// Files one show in a folder. The document is untouched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FileShowRequest {
    /// Where it is to be filed. Empty is the root.
    pub folder: String,
}

/// Creates a show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CreateShowRequest {
    /// Name for the new show.
    pub name: String,
}

/// Saves a show.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SaveShowRequest {
    /// The complete show.
    ///
    /// Shows are saved whole. [`AssignSlotRequest`] is the one edit that
    /// does not send the whole document, and it is still applied to one: the
    /// server reads the show, puts the source in the slot and validates and
    /// stores the result, so there is no half-applied edit.
    pub show: Show,
}

/// Result of storing what is on stage into the open show.
///
/// The same shape a save answers with, because it is the same thing from the
/// operator's side: the show now holds what they can see, and this is when
/// the server recorded it.
pub type CaptureLiveResponse = SaveShowResponse;

/// Puts a source in one slot of one layer.
///
/// The one edit with an endpoint of its own, because it is the one edit an
/// operator makes with a show running: a clip dragged from the browser onto a
/// pad (owner, 2026-09-23). Sending the whole document for it would work and
/// would be wrong — storing a show the server has open reloads it there,
/// and reloading a show because somebody filled an empty pad puts every layer
/// back to its opening slot in front of an audience.
///
/// Whatever else the slot was set to — how it loops, its direction, its beat
/// sync, its effects — is kept. This changes what plays, not how.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AssignSlotRequest {
    /// What the slot is to play.
    pub source: SourceRef,
}

/// Result of assigning or clearing a slot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AssignSlotResponse {
    /// When the server recorded the edit.
    pub updated_at: Timestamp,
    /// What the slot held before, or `None` when it was empty.
    ///
    /// Returned rather than assumed so that a client can say what it replaced
    /// after the fact. The asking happens before the request is sent, in front
    /// of the operator; this is the record of what was actually there, which is
    /// not always what the client thought.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaced: Option<SourceRef>,
}

/// Result of a save.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SaveShowResponse {
    /// When the server recorded the save.
    pub updated_at: Timestamp,
}

/// Adds an effect to the end of a layer's or a slot's chain.
///
/// **Appended, never replacing.** An effect dropped on a slot or a layer
/// stacks on top of what is there (owner, 2026-09-23): a chain is an order,
/// and dropping a second effect is not a request to undo the first.
///
/// No parameters. Every one of them has a default in the effect's own
/// manifest, and a client that sent copies of those defaults would be writing
/// down values nobody chose — which then stop following the manifest the day
/// it changes. They are set afterwards, where they are set today.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AddEffectRequest {
    /// Stable effect type identifier, for example `ovs.blur`.
    pub type_id: String,
    /// Not read: there is one effect of each type (owner, 2026-10-18: "Es gibt
    /// kein Invert V2.. nirgendwo"), and the instance is stored numbered as
    /// that one. Kept because every client already in use sends it.
    pub version: u32,
}

/// Result of adding or removing an effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AddEffectResponse {
    /// When the server recorded the edit.
    pub updated_at: Timestamp,
    /// The instance added, or the one removed.
    ///
    /// The identity is the server's: it lives in the same document as the
    /// fixture patch that may point at it, so a client that invented one would
    /// be naming something the stored show does not have.
    pub effect_instance_id: EffectInstanceId,
}

/// Sets the order of a layer's or a slot's chain.
///
/// **The whole order, not a move.** A move is a pair of positions a client works
/// out from a list it may already be behind on, and two clients moving different
/// effects at once would each apply theirs to the other's result. An order is one
/// statement of what the chain is: it either matches what is there or it is
/// refused, and sending it twice changes nothing.
///
/// Every instance currently in the chain must appear exactly once. That is what
/// stops this from being a way to remove one by leaving it out — removing is
/// `DELETE`, where it says so.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct OrderEffectsRequest {
    /// Every instance in the chain, in the order it is to run.
    pub order: Vec<EffectInstanceId>,
}

/// Result of setting a chain's order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct OrderEffectsResponse {
    /// When the server recorded the edit.
    pub updated_at: Timestamp,
}

/// What a lock is put on: one effect, or one generator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LockTarget {
    /// One effect instance, on a layer or on a slot.
    Effect {
        /// The instance.
        effect_instance_id: EffectInstanceId,
    },
    /// One generator instance, in its slot.
    Generator {
        /// The instance.
        generator_instance_id: ovs_types::id::GeneratorInstanceId,
    },
}

/// Locks one effect or generator where it is, or unlocks it.
///
/// **Administrators only, and the only way a lock changes** (owner,
/// 2026-10-19). A locked effect or generator cannot be removed, moved or put in
/// another order by anybody, an administrator included; unlocking is this
/// request, made deliberately, before any of those. A show sent whole carries
/// the locks it already has whatever its copy says, so an editor's draft from
/// before a lock cannot undo one by being saved.
///
/// What it is doing is not locked: its values follow the patch like any other
/// module's, and switching an effect on or off stays everybody's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SetLockRequest {
    /// What to lock or unlock.
    pub target: LockTarget,
    /// Locked, or not.
    pub locked: bool,
}

/// Result of locking or unlocking.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SetLockResponse {
    /// When the server recorded the edit.
    pub updated_at: Timestamp,
}

/// Sets how many banks of twelve a layer's matrix shows.
///
/// Its own request because an operator asks for a bank in order to put
/// something in it, and a count worked out from the slots would collapse back
/// to what is filled on the next redraw (owner, 2026-09-23).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SetBanksRequest {
    /// How many banks to show, between one and `Layer::MAX_BANKS`.
    pub banks: u16,
}

/// Moves a slot to another pad of the same layer, with everything it carries.
///
/// A pad dragged onto another on the live matrix (owner, 2026-10-21: "Slot 1
/// per Drag and Drop auf Slot 10 schieben"). A pad that is taken swaps with the
/// one that moves, rather than being overwritten.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MoveSlotRequest {
    /// The pad it goes to, between 1 and `Layer::MAX_SLOT_INDEX`.
    pub to: u16,
}

/// Result of moving a slot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MoveSlotResponse {
    /// When the server recorded the edit.
    pub updated_at: Timestamp,
    /// The pad it went to had a slot of its own, which is now where this one was.
    pub swapped: bool,
}
// --- /media --------------------------------------------------------------

/// The media library.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MediaListResponse {
    /// Every item, most recently imported first.
    pub items: Vec<MediaListItem>,
    /// Every folder the library holds, in path order.
    ///
    /// Sent whole rather than worked out from the items, because a folder
    /// exists while it is empty: an operator makes one in order to put
    /// things in it (owner, 2026-09-23).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub folders: Vec<String>,
}

/// Makes a folder in the library.
///
/// Every folder above it that is not there yet is made with it, because a tree
/// with a hole in it cannot be drawn. Making one that already exists changes
/// nothing and is not an error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CreateFolderRequest {
    /// The whole path, slash-separated, for example `Intros/2026`.
    pub path: String,
}

/// Renames a folder, which moves everything filed under it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RenameFolderRequest {
    /// The path as it is now.
    pub from: String,
    /// The path it is to have. Moving a folder inside itself is refused.
    pub to: String,
}

/// Which folder a request is about, for the ones that take no body.
///
/// A query parameter rather than a path segment, because a folder path has
/// slashes in it and a slash in a path segment is a different route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FolderQuery {
    /// The whole path, slash-separated.
    pub path: String,
}

/// Renames one media item, or moves it to another folder, or both.
///
/// **Neither touches the file.** A display name is a label and a folder is a
/// label; the asset keeps its object key and the content hash it is found by,
/// so a show that plays this item goes on playing it (owner, 2026-09-23:
/// "organising media must not move the real files").
///
/// A field left out means "leave this alone", so a rename need not send a
/// folder and a move need not send a name.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FileMediaRequest {
    /// The name the library shows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Where it is filed. Empty is the root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
}

/// One item in the library, with the one fact about its content that an
/// operator needs and the item itself deliberately does not carry.
///
/// A [`MediaItem`] is the logical identity: it never reveals where its content
/// is or which profile was chosen for it, because an operator picking a clip
/// should not have to know either. How much disc it takes is a
/// different kind of fact — it is what an operator uses to decide what to
/// delete before a show, and no part of it says anything about codecs or paths.
///
/// Flattened, so what a client already read out of this list still reads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MediaListItem {
    /// The item.
    #[serde(flatten)]
    pub item: MediaItem,
    /// How much disc its playable content takes, in bytes.
    ///
    /// Zero for an item whose content has not arrived, which is what an
    /// imported show's items look like until they are filled.
    pub byte_size: u64,
}

/// Changes one slot's settings.
///
/// Every field is optional and an absent one means "leave this alone", so the
/// control an operator moved is the only thing that travels. It is a
/// *document* edit made with a show running, like putting a source in a slot:
/// storing the whole show would reload the show, and deciding
/// that a clip should run to the beat, or nudging where it sits, is something
/// done while watching it.
///
/// What it does not carry is the source. A slot's content is changed by
/// dragging something onto the pad, which is its own endpoint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SetSlotRequest {
    /// What an operator calls this pad.
    ///
    /// **Empty means "no name of its own"**, which puts the pad back to being
    /// named after what it plays. Absent means "leave it alone", like every
    /// other field here — and a name has to have a way of being taken off
    /// again, which a field that can only be absent or set does not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// What happens when the clip reaches the end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playback_mode: Option<ovs_types::config::PlaybackMode>,
    /// Which way the timeline runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<ovs_types::config::PlaybackDirection>,
    /// Whether it runs at its own rate or to the show's beat, and over how many.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync: Option<ovs_types::config::SlotSync>,
    /// How fast it runs through one pass, as a ratio.
    ///
    /// Both kinds of sync have one (owner, 2026-09-23): "pro Slot Scale sollt
    /// aber verfügbar sein für Free Run und BPM Sync." A free-running slot has
    /// no other rate — the master's scale deliberately does not reach it — and
    /// a beat-locked one is scaled on top of the beats it was given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<f64>,
    /// Where this source sits, before the layer's own transform.
    ///
    /// The slot's, not the layer's: two clips in one layer are rarely framed
    /// the same way, and this is where an operator puts one of them right
    /// without moving the other (owner, 2026-09-23).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transform: Option<ovs_types::geometry::Transform2D>,
}

/// Replaces a show's mapping: its outputs and the pieces that draw to them.
///
/// The two together, because they are one thing and refer to each other: a
/// piece names the output it lands on, and sending the pieces alone would mean
/// a moment in which one names an output that is not there yet.
///
/// Whole rather than one piece at a time, because a mapping is dragged as a
/// group — several pieces move together and must land together, in one frame
/// rather than four (owner, 2026-09-23).
///
/// It is a *document* edit made with a show running, like putting a source in
/// a slot: storing the whole show would reload the show, and
/// lining a wall up is the one job that has to be done while looking at it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SetMappingRequest {
    /// Every output this show drives.
    pub outputs: Vec<ovs_types::config::OutputConfig>,
    /// Every piece, in the order the document lists them.
    pub pieces: Vec<ovs_types::config::Piece>,
}

/// What deleting a media item would affect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MediaUsageResponse {
    /// Names of shows that reference this item.
    ///
    /// Deleting media must warn where it is used rather than silently breaking
    /// an archived show.
    pub used_by_shows: Vec<String>,
}

// --- /shows/{id}/export, /shows/import ------------------------------

/// One media item a package needs, and everything required to ask for it.
///
/// No physical path and no storage key: those belong to the server that holds
/// the file, and a package moves between servers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MediaManifestEntry {
    /// The item as the exporting server knew it.
    pub media_item_id: MediaItemId,
    /// Name to show in the library.
    pub display_name: String,
    /// The filename it was imported from, for the operator's orientation.
    pub original_filename: String,
    /// Video or image.
    pub kind: ovs_types::media::MediaKind,
    /// SHA-256 of the playable asset, lowercase hex.
    ///
    /// This is what identifies the content. An importing server that already
    /// holds these bytes needs none of them sent again.
    pub sha256: String,
    /// Size of the asset in bytes.
    pub byte_size: u64,
    /// Pixel dimensions.
    pub dimensions: SizePx,
    /// Duration in milliseconds. `None` for a still.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Frame rate. `None` for a still.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_rate: Option<FrameRate>,
    /// Whether it carries an alpha channel.
    pub has_alpha: bool,
    /// Container format, as the exporting server read it.
    pub container: String,
    /// Codec name, as the exporting server read it.
    ///
    /// Carried so an importing server can say what a package needs before a
    /// byte of it moves — a package built around H.264 on a machine that has
    /// no H.264 decoder is worth knowing about at import rather than at
    /// showtime.
    pub codec: String,
}

/// A show and the inventory of what it needs to play.
///
/// The media itself is not in here. A package is a document an operator can read
/// and a server can validate before a byte of video moves; the files come
/// afterwards, through the same upload path as any other import, and only the
/// ones the receiving server does not already hold.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ShowExport {
    /// Format version of this package.
    pub package_version: u32,
    /// Version of the server that wrote it, for diagnostics.
    pub exported_by: String,
    /// When it was written.
    pub exported_at: Timestamp,
    /// The show itself.
    pub show: Show,
    /// Every media item the show refers to.
    pub media: Vec<MediaManifestEntry>,
}

/// One reference an import had to point somewhere else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Relinked {
    /// The item the package named.
    pub from: MediaItemId,
    /// The item on this server holding the same content.
    pub to: MediaItemId,
}

/// What an import did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ImportShowResponse {
    /// The show as it now exists here.
    pub show_id: ShowId,
    /// References pointed at content this server already held.
    ///
    /// Not an error and not a loss: the bytes are the same bytes, and reusing
    /// them is why a package carries hashes rather than files.
    pub relinked: Vec<Relinked>,
    /// Media this server does not have yet.
    ///
    /// The show is saved either way; these layers show nothing until the
    /// content arrives, which an operator does through the ordinary upload path.
    pub missing: Vec<MediaManifestEntry>,
}

// --- /live ---------------------------------------------------------------

/// Selects a slot on a layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SelectSlotRequest {
    /// The slot to select. `0` is the permanently black slot, which is always
    /// selectable on any layer without being configured anywhere.
    pub slot_index: u16,
}

/// Changes part of the live master state.
///
/// Every field is optional and means "leave this alone". A console sending one
/// fader must not have to send the others, and a client that sent a default for
/// a field it was not thinking about would move something on stage.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UpdateMasterRequest {
    /// Master dimmer in `0.0..=1.0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dimmer: Option<f32>,
    /// Crossfader position: `0.0` is fully A, `1.0` is fully B.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crossfader: Option<f32>,
    /// Master BPM. There is exactly one of these in the whole system.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bpm: Option<f64>,
    /// Global speed scale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed_scale: Option<f64>,
    /// Whether the final output is frozen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freeze: Option<bool>,
    /// Whether incoming DMX reaches this show at all.
    ///
    /// The operator taking the show off the console by hand. Not something a
    /// console can ask for: a desk that could switch itself off could not
    /// switch itself back on, and the whole point of the switch is that it is
    /// reached for when the desk is the problem.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dmx_enabled: Option<bool>,
}

/// Changes part of one layer's live state.
///
/// Optional for the same reason as [`UpdateMasterRequest`].
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UpdateLayerRequest {
    /// Live opacity in `0.0..=1.0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f32>,
    /// Live transform.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transform: Option<Transform2D>,
    /// Which bus this layer feeds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<Route>,
    /// How this layer composites onto its bus.
    ///
    /// Live, not configuration, for the same reason the grid above it is: an
    /// operator changes it while looking at the wall, and the show keeps its
    /// own default for the next time it opens (owner 2026-09-23).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blend_mode: Option<ovs_types::config::BlendMode>,
    /// What the playhead is doing: playing, paused or stopped.
    ///
    /// Separate from selecting slot 0, which is Black and a different thing
    /// (owner decision Q16). Pausing holds the frame that is on screen and
    /// resuming carries on from it; stopping rewinds to the start of the slot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<ovs_types::live::Transport>,
    /// Whether the layer is in the composition at all.
    ///
    /// Live, beside the show's own [`Layer::enabled`], which is where it
    /// starts. Changing it here never writes into the show: the layer keeps
    /// its slots, its transform and its chain.
    ///
    /// [`Layer::enabled`]: ovs_types::config::Layer::enabled
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

/// Changes the live values on one effect instance.
///
/// A live value sits on top of what the show configured for that instance and
/// is never written back to it: an operator can push an effect during a number
/// and the show still opens tomorrow at the value it was built with
///. Editing what it opens with is a show endpoint; this is not a
/// shortcut to one.
///
/// Only the keys sent are moved, for the same reason the fields of
/// [`UpdateLayerRequest`] are optional: a client moving one control must not
/// have to send the others, and one that sent a default for a control it was not
/// thinking about would change something on stage.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UpdateEffectRequest {
    /// Live values to put on top of the configured ones, by parameter key.
    ///
    /// Checked against the effect's manifest exactly as a configured value is:
    /// a value of the wrong type, one outside its declared range, and one for a
    /// parameter the effect never declared are all refused, and a refused
    /// request changes nothing.
    #[serde(default, skip_serializing_if = "ParameterMap::is_empty")]
    pub overrides: ParameterMap,
    /// Whether to drop the live values this instance already had, first.
    ///
    /// How an operator puts an effect back the way the show was built. It needs
    /// its own field because "no live value" is an absence: there is no value a
    /// client could send that means "stop overriding this", and sending the
    /// configured value instead would only look the same until somebody edited
    /// the show.
    #[serde(default)]
    pub reset: bool,
}

/// What a patch editor asks when it is about to add a fixture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AddressSuggestionRequest {
    /// Which universe to look in.
    pub universe: u16,
    /// How many channels the fixture takes.
    pub footprint: u16,
    /// The fixture being re-addressed, if this is a move rather than an add.
    ///
    /// A fixture does not collide with where it already is, so without this an
    /// editor could not suggest the address a fixture already has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub moving: Option<ovs_types::id::FixtureInstanceId>,
}

/// Where a fixture fits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AddressSuggestionResponse {
    /// The universe that was asked about.
    pub universe: u16,
    /// The lowest address it fits at, or `None` when there is no room.
    ///
    /// No room is a real answer: a universe has 512 channels and a show can
    /// want more than that.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_address: Option<u16>,
}

/// Whether the renderer should produce previews.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PreviewSwitchRequest {
    /// True to produce them, false to stop.
    pub enabled: bool,
}

/// Which counters an operator has asked to put back to zero.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
pub struct ResetCountersRequest {
    /// The group to clear. Everything, unless something narrower is named.
    #[serde(default)]
    pub group: ovs_types::telemetry::CounterGroup,
}

/// Which surfaces should show the commissioning test pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TestPatternRequest {
    /// The composition, every output, or neither.
    ///
    /// Not a switch, because the two patterns answer different questions: one
    /// tests the mapping, the other tests the cabling and the mode
    /// (`TestPatternMode` says which is which).
    pub mode: ovs_types::config::TestPatternMode,
    /// The piece drawn as the chosen one, when the mode is `PIECES`.
    ///
    /// One, never several: the pattern is what somebody is looking at while
    /// they line one rectangle up, and two highlighted rectangles answer a
    /// question nobody asked. Absent draws them all as unchosen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub highlighted: Option<ovs_types::id::PieceId>,
}

/// What to do to the machine.
///
/// Both of these end the show. The API does not ask for confirmation — that is
/// the console's job, in front of the operator who is about to press it — but
/// it does insist on being told which one, because a single "power" endpoint
/// that defaulted to something would eventually default to the wrong thing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum PowerAction {
    /// Restart the machine.
    Reboot,
    /// Turn the machine off.
    Shutdown,
}

/// A permission to watch one preview stream, for a short while.
///
/// Minted by a request that carried a session, and narrower than one on
/// purpose: it opens one surface, it expires within the minute, and it is spent
/// the first time it is used. A client that has this cannot load a show,
/// move a fader or stop the machine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct StreamTicketResponse {
    /// The ticket, to be sent as the first message on the stream socket.
    pub ticket: String,
    /// When it stops working, in milliseconds since the epoch.
    pub expires_at: i64,
}

/// What happened to a request that the machine restart or stop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PowerResponse {
    /// Whether the machine accepted it.
    ///
    /// False is ordinary rather than exceptional: a server that has not been
    /// given permission to reboot itself is a server that will not, and saying
    /// so beats an operator pressing a button that does nothing.
    pub accepted: bool,
    /// What was run, or why nothing was.
    pub detail: String,
}

/// What this server knows about updating itself.
///
/// The owner, 2026-10-19: "ob der server selber Updates über das Repo abfragen
/// kann und bei bedarf durch den User in der AdminGUI direkt installiert werden
/// kann" — and "gar nicht automatisch prüfen. nur auf knopfdruck." So nothing
/// here is ever fetched unasked: `check` is empty until an administrator has
/// pressed the button since the server started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UpdateStatus {
    /// The version this server is running.
    pub installed: String,
    /// What the last check found, if anybody has asked since the server started.
    pub check: Option<UpdateCheck>,
    /// What the installer last did, if it has ever run on this machine.
    pub install: Option<UpdateProgress>,
}

/// What asking the repository for its newest release found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UpdateCheck {
    /// When the repository was asked, in milliseconds since the epoch.
    pub checked_at: i64,
    /// The newest release's version, such as `0.2.46`, when there was an answer.
    pub latest: Option<String>,
    /// Whether that is newer than what is installed, and carries a package for
    /// this machine — which is the one case in which there is something to
    /// install.
    pub newer: bool,
    /// When that release was published, as the repository writes it.
    pub published_at: Option<String>,
    /// The release's own page, where what changed is written.
    pub notes_url: Option<String>,
    /// Why there is no answer, or why the newest release cannot be installed
    /// here: no connection, no package for this machine.
    pub problem: Option<String>,
}

/// Where the installer is, or where it stopped.
///
/// Written by the installer itself, which runs apart from the server: the
/// server is restarted by the package it installs, and an install it ran itself
/// would be killed half way.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UpdateProgress {
    /// The version it was asked for.
    pub version: String,
    /// `starting`, `downloading`, `verifying`, `installing`, then `installed`,
    /// `failed` or `refused`.
    pub stage: String,
    /// What it is doing, or why it stopped, in a sentence.
    pub detail: String,
    /// When it last wrote this, as UTC in ISO 8601.
    pub at: String,
}

/// Which release to install.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct InstallUpdateRequest {
    /// The version, such as `0.2.46`. Only a newer one than is installed.
    pub version: String,
}

/// Whether the installer was started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct InstallUpdateResponse {
    /// Whether the machine started it. Its progress is in [`UpdateStatus`].
    pub accepted: bool,
    /// What was run, or why nothing was.
    pub detail: String,
}

/// The machine's wired network interfaces, and any change on trial.
///
/// The owner, 2026-10-19: "Network Settings via Admin GUI, also ob DHCP/Static
/// inkl IP Subnetz Gateway DNS etc." — and on the safety net, five minutes to
/// confirm before the previous settings come back by themselves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct NetworkStatus {
    /// Every wired interface, in name order. Not the loopback, not a VPN and
    /// not a radio: none of those is configured here.
    pub interfaces: Vec<NetworkInterface>,
    /// The nameservers the machine uses now, whichever interface set them.
    pub nameservers: Vec<String>,
    /// The last change and where it got, if one has ever been made here.
    pub trial: Option<NetworkTrial>,
    /// Why nothing can be changed from here, where nothing can: a machine
    /// whose network is not run by `ifupdown` is changed where it is run.
    pub unavailable: Option<String>,
}

/// One wired interface, as it is and as it is configured.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct NetworkInterface {
    /// Its name, such as `eno1`.
    pub name: String,
    /// Its hardware address.
    pub mac: String,
    /// Whether a cable with something at the other end is plugged in.
    pub link: bool,
    /// The link's speed in megabits a second, where the driver says.
    pub speed_mbps: Option<u32>,
    /// The IPv4 addresses it has now, as `address/prefix`.
    pub addresses: Vec<String>,
    /// The default gateway through it now, if there is one.
    pub gateway: Option<String>,
    /// How it is configured to come up: `dhcp`, `static`, or `none` where the
    /// configuration does not mention it.
    pub method: String,
    /// The configured static settings, where the method is `static`.
    pub configured: Option<NetworkSettings>,
}

/// Settings for one interface: the request to change them, and what a static
/// configuration holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct NetworkSettings {
    /// The interface, such as `eno1`.
    pub interface: String,
    /// `dhcp` or `static`.
    pub method: String,
    /// The IPv4 address, for `static`.
    #[serde(default)]
    pub address: Option<String>,
    /// The subnet mask, as `255.255.255.0` or as a prefix length such as `24`.
    #[serde(default)]
    pub netmask: Option<String>,
    /// The default gateway, for `static`. Optional: a show network often has
    /// none.
    #[serde(default)]
    pub gateway: Option<String>,
    /// Up to three nameservers, for `static`.
    #[serde(default)]
    pub nameservers: Vec<String>,
}

/// A change to the network, and how far it got.
///
/// Written by the machine's own network helper, which runs apart from the
/// server: it has to be able to put the previous settings back when nobody can
/// reach the server any more.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct NetworkTrial {
    /// Which change this is.
    pub id: String,
    /// The interface it changed.
    pub interface: String,
    /// What it set, in a few words: `static 10.110.0.50/24` or `dhcp`.
    pub summary: String,
    /// `applying`, `waiting` for somebody to keep it, then `kept`, `restored`,
    /// `failed` or `refused`.
    pub stage: String,
    /// What it is doing, or why it stopped, in a sentence.
    pub detail: String,
    /// When the previous settings come back by themselves, in seconds since
    /// the epoch, while it is `waiting`.
    pub deadline: Option<i64>,
    /// When it last wrote this, as UTC in ISO 8601.
    pub at: String,
}

/// Whether a change was started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct NetworkChangeResponse {
    /// Whether the machine started it. How far it got is in [`NetworkStatus`].
    pub accepted: bool,
    /// What was asked for, or why nothing was.
    pub detail: String,
}

/// The answer to a change on trial.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct NetworkTrialAnswer {
    /// True to keep the new settings, false to put the previous ones back now.
    pub keep: bool,
}

/// What a tap did to the tempo.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TapTempoResponse {
    /// The master BPM after this tap.
    pub bpm: f64,
    /// Whether this tap changed it.
    ///
    /// The first tap of a run never does: one tap is not an interval. A client
    /// showing "tapping…" until this turns true is showing the truth.
    pub changed: bool,
}

// --- /uploads ------------------------------------------------------------

/// Asks the server to accept an upload.
///
/// The client has already probed, prepared and hashed the file, so the server
/// can answer "I already have that" before a single byte crosses the network.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CreateUploadRequest {
    /// Name to show in the library.
    pub display_name: String,
    /// Where in the library the new item is filed.
    ///
    /// Slash-separated, empty for the root. What the operator had open when
    /// they dropped the files: a clip imported while standing in a folder
    /// belongs in that folder, and appearing in All media instead is a move
    /// somebody then has to make by hand (owner, 2026-09-23).
    ///
    /// Made, with every folder above it, if it is not there yet — an upload is
    /// not the place to lose somebody's work over a folder deleted in another
    /// window. Ignored when `media_item_id` names an item that already exists,
    /// because that item is already filed somewhere.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub folder: String,
    /// The file the operator picked, kept for their orientation only.
    pub original_filename: String,
    /// SHA-256 of the prepared file, lowercase hex.
    pub sha256: String,
    /// Size of the prepared file in bytes.
    pub byte_size: u64,
    /// The item these bytes belong to, when there already is one.
    ///
    /// Set after importing a package: the item exists, empty, waiting for its
    /// content, and the show already refers to it. Left out for an ordinary
    /// import, where the server creates the item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_item_id: Option<MediaItemId>,
    /// Whether these bytes are a moving or a still image.
    ///
    /// Declared rather than inferred, because it decides which checks the
    /// server runs: a still is decoded and measured, a video is probed. A
    /// declaration that disagrees with the file is refused.
    pub kind: MediaKind,
    /// Pixel dimensions the client measured.
    ///
    /// The client has already opened the file to prepare it, so it knows.
    /// The server checks what it can — for a still it decodes the file and
    /// compares — rather than taking this on trust, because a wrong size here
    /// is a wrong size in the library and in every show built on it.
    pub dimensions: SizePx,
    /// Duration in milliseconds. `None` for a still.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Frame rate. `None` for a still.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_rate: Option<FrameRate>,
    /// Whether the file carries an alpha channel.
    #[serde(default)]
    pub has_alpha: bool,
    /// Container format the client read.
    ///
    /// Believed only as far as naming the stored object. What the server
    /// records is what its own probe found.
    #[serde(default)]
    pub container: String,
    /// Codec name the client read.
    ///
    /// Believed for nothing. It is here so a client that probed the file can
    /// say what it thought, and so a disagreement between the two readings is
    /// visible in the log rather than silent.
    #[serde(default)]
    pub codec: String,
}

/// The server's answer to an upload request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum CreateUploadResponse {
    /// The server already holds this exact content.
    ///
    /// Nothing is uploaded. This is what stops an operator sending the same
    /// show file twice.
    AlreadyPresent {
        /// The item that already holds it.
        media_item_id: MediaItemId,
    },
    /// Send the bytes.
    Accepted(UploadSession),
}

/// A resumable upload in progress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UploadSession {
    /// Identifies this upload.
    pub upload_id: String,
    /// The item the content will belong to once it is verified.
    pub media_item_id: MediaItemId,
    /// Total bytes expected.
    pub byte_size: u64,
    /// Bytes already received, contiguous from the start.
    ///
    /// A client resumes by sending from this offset. Contiguous rather than a
    /// set of ranges because a single offset is all a resumable PUT needs, and
    /// a range set would invite out-of-order writes that complicate
    /// verification for no operator-visible benefit.
    pub received_bytes: u64,
    /// Chunk size the server prefers, in bytes.
    pub chunk_size: u64,
}

impl UploadSession {
    /// Returns the byte offset a client should send from next.
    #[must_use]
    pub const fn resume_offset(&self) -> u64 {
        self.received_bytes
    }

    /// Returns true when every byte has arrived.
    ///
    /// Complete is not the same as ready: the server still has to verify the
    /// hash before the item can be used.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.received_bytes >= self.byte_size
    }

    /// Returns progress between 0.0 and 1.0.
    #[must_use]
    pub fn progress(&self) -> f32 {
        if self.byte_size == 0 {
            return 1.0;
        }
        (self.received_bytes as f64 / self.byte_size as f64).min(1.0) as f32
    }
}

// --- /outputs ------------------------------------------------------------

/// Assigns a role to a connector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SetConnectorRoleRequest {
    /// PCI address of the GPU.
    pub gpu_pci_address: String,
    /// Connector name.
    pub connector: String,
    /// The role to assign.
    ///
    /// Always explicit. A connector's role is never inferred from what happens
    /// to be plugged in.
    pub role: ConnectorRole,
}

/// Resets an output's operator-visible dropped-frame counter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ResetDroppedFramesRequest {
    /// The output to reset.
    ///
    /// Only the operator counter is reset; the lifetime counter is not
    /// resettable, so a long-run problem stays visible.
    pub output_id: OutputId,
}

/// Builds the capabilities this build reports.
///
/// Kept beside the type so the version fields cannot drift from the constants
/// they are supposed to mirror.
#[must_use]
pub fn capabilities(
    server_version: impl Into<String>,
    configured: bool,
    connectors: Vec<ConnectorInfo>,
    connectors_elsewhere: Vec<ConnectorInfo>,
    media_codecs: Vec<String>,
) -> CapabilitiesResponse {
    CapabilitiesResponse {
        server_version: server_version.into(),
        api_version: API_VERSION,
        realtime_protocol_version: REALTIME_PROTOCOL_VERSION,
        show_schema_version: ovs_types::SHOW_SCHEMA_VERSION,
        configured,
        media_codecs,
        connectors,
        connectors_elsewhere,
        network_output_modes: ovs_types::config::NETWORK_MODES.to_vec(),
    }
}

/// The machine's own configuration, as the administration page reads it.
///
/// Not the show's. What walls exist and what is mapped onto them belongs to the
/// show; this is what the machine *is*, and it lives in a file rather than the
/// database because it has to be readable before anything is running
/// (`ovs_core::config_file`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SystemConfigResponse {
    /// What this server calls itself on the network.
    ///
    /// Absent means the host name is used, which is a name somebody chose once
    /// while installing Debian rather than one they chose for this rack.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The host name, so a page can show what "absent" will actually mean.
    pub host_name: String,
    /// The connector the System screen is on, if this machine has one.
    ///
    /// Absent is legitimate and common: a rack unit reached only over the
    /// network has no local screen, and the renderer may then use every
    /// connector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_display: Option<ServiceDisplayInfo>,
    /// Where the file lives, so somebody can edit it by hand instead.
    pub path: String,
    /// Whether the server can actually write that file.
    ///
    /// Tested rather than assumed: the unit runs under a sandbox with a
    /// read-only system, and a page offering a Save button that cannot save is
    /// worse than one that says why.
    pub writable: bool,
}

/// The connector the System screen is on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ServiceDisplayInfo {
    /// PCI address of the GPU driving it, `0000:00:02.0`.
    pub gpu_pci_address: String,
    /// Connector name as Vulkan reports it.
    pub connector: String,
    /// Whether the renderer currently reports this connector at all.
    ///
    /// False is not a fault on its own — a machine being commissioned may have
    /// no renderer up yet, and a connector is driven whether or not anything is
    /// plugged into it. It is worth saying out loud, because the
    /// other reason for false is a typo that will cost somebody a drive.
    pub present: bool,
    /// The mode it has been told to use, when one was chosen.
    ///
    /// Absent means the panel's own preference. A client showing a chooser needs
    /// to know which entry to put the tick beside, and "none of them, the panel
    /// decides" is one of the answers — so this is in the same shape as the list
    /// it is compared against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<ConnectorModeInfo>,
}

/// Changing the machine's own configuration.
///
/// Both fields are sent every time and both are meaningful when absent, so this
/// replaces rather than merges: absent `name` means "use the host name" and
/// absent `service_display` means "this machine has no local screen".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SetSystemConfigRequest {
    /// What this server should call itself, or absent for the host name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The connector the System screen is on, or absent for none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_display: Option<ServiceDisplayChoice>,
}

/// Which connector the System screen should be on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ServiceDisplayChoice {
    /// PCI address of the GPU driving it.
    pub gpu_pci_address: String,
    /// Connector name as Vulkan reports it.
    pub connector: String,
    /// The mode to drive it at, or absent for the panel's own preference.
    ///
    /// Absent is the ordinary answer and the one every machine starts with. It
    /// is here because a panel's preference is not always a mode it can show,
    /// and the person in front of it is the only one who can tell.
    ///
    /// **One of the entries from [`ConnectorInfo::modes`], handed back.** In
    /// millihertz rather than as a rate, because 59.94 is 60000/1001 and not
    /// 59940/1000 — two different rates that round to the same three decimals —
    /// and the one place that knows which is which is on the server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<ConnectorModeInfo>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_api_prefix_carries_the_major_version() {
        assert_eq!(API_PREFIX, format!("/api/v{API_VERSION}"));
    }

    #[test]
    fn a_password_never_reaches_a_debug_log() {
        let request = LoginRequest {
            username: "operator".to_owned(),
            password: "correct horse battery staple".to_owned(),
        };
        let rendered = format!("{request:?}");
        assert!(rendered.contains("operator"));
        assert!(
            !rendered.contains("correct horse"),
            "a password must never be formatted"
        );
        assert!(rendered.contains("<redacted>"));

        let setup = CreateAccountRequest {
            username: "operator".to_owned(),
            password: "hunter2".to_owned(),
            role: None,
        };
        assert!(!format!("{setup:?}").contains("hunter2"));
    }

    #[test]
    fn a_password_is_still_serialised_because_it_has_to_reach_the_server() {
        // The redaction is about logs, not about the wire.
        let request = LoginRequest {
            username: "operator".to_owned(),
            password: "hunter2".to_owned(),
        };
        let json = serde_json::to_string(&request).expect("serialises");
        assert!(json.contains("hunter2"));
    }

    #[test]
    fn capabilities_mirror_the_version_constants() {
        let reported = capabilities("0.0.1", true, Vec::new(), Vec::new(), Vec::new());
        assert_eq!(reported.api_version, API_VERSION);
        assert_eq!(
            reported.realtime_protocol_version,
            REALTIME_PROTOCOL_VERSION
        );
        assert_eq!(reported.show_schema_version, ovs_types::SHOW_SCHEMA_VERSION);
    }

    #[test]
    fn an_upload_resumes_from_where_it_stopped() {
        let session = UploadSession {
            upload_id: "u1".to_owned(),
            media_item_id: MediaItemId::new(),
            byte_size: 1_000_000,
            received_bytes: 250_000,
            chunk_size: 8 * 1024 * 1024,
        };
        assert_eq!(session.resume_offset(), 250_000);
        assert!(!session.is_complete());
        assert!((session.progress() - 0.25).abs() < 1e-6);
    }

    #[test]
    fn a_complete_upload_is_not_the_same_as_a_ready_one() {
        // Completeness is about bytes; readiness needs verification.
        let session = UploadSession {
            upload_id: "u1".to_owned(),
            media_item_id: MediaItemId::new(),
            byte_size: 1_000,
            received_bytes: 1_000,
            chunk_size: 1_000,
        };
        assert!(session.is_complete());
        assert_eq!(session.progress(), 1.0);

        let json = serde_json::to_string(&session).expect("serialises");
        assert!(
            !json.contains("ready"),
            "an upload session says nothing about readiness"
        );
    }

    #[test]
    fn an_empty_upload_does_not_divide_by_zero() {
        let session = UploadSession {
            upload_id: "u1".to_owned(),
            media_item_id: MediaItemId::new(),
            byte_size: 0,
            received_bytes: 0,
            chunk_size: 1,
        };
        assert_eq!(session.progress(), 1.0);
        assert!(session.is_complete());
    }

    #[test]
    fn duplicate_content_is_answered_without_an_upload() {
        let response = CreateUploadResponse::AlreadyPresent {
            media_item_id: MediaItemId::new(),
        };
        let json = serde_json::to_string(&response).expect("serialises");
        assert!(json.contains("already_present"));
        let back: CreateUploadResponse = serde_json::from_str(&json).expect("deserialises");
        assert_eq!(back, response);
    }

    #[test]
    fn an_api_error_carries_every_validation_problem() {
        let mut errors = ovs_types::ValidationErrors::new();
        errors.push(ovs_types::ValidationError::new(
            "layers[0].name",
            "must not be empty",
        ));
        errors.push(ovs_types::ValidationError::new(
            "master.default_bpm",
            "out of range",
        ));

        let error = ApiError::invalid(errors);
        assert_eq!(error.code, "invalid");
        assert_eq!(
            error.details.len(),
            2,
            "an operator should see every problem at once"
        );
    }
}
