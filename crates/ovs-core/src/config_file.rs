//! The machine's own configuration, as a file an administrator edits.
//!
//! One thing lives here and it is deliberately not in the database: **which
//! connector carries the System screen**. It is decided once, when the machine
//! is built, and it is the one setting that has to be right before anything else
//! can be: the renderer must not take that socket even for its first frame, and
//! the System screen's connector has to be known before any client exists to
//! say so.
//!
//! A database row would be the wrong place. An operator commissioning a rack
//! reads and edits a file; recovering a machine means copying one; and a server
//! whose screen is black because a row is missing is a server nobody can fix
//! from the screen.
//!
//! Everything else about a show — which walls exist, what they run at, what is
//! mapped onto them — belongs to the show, and the show is not
//! configuration.

use std::path::{Path, PathBuf};

use ovs_types::config::ConnectorRole;
use serde::{Deserialize, Serialize};

/// Where the file lives unless `OVS_CONFIG` says otherwise.
pub const DEFAULT_PATH: &str = "/etc/openvideoserver/config.json";

/// The environment variable that moves it.
pub const PATH_ENV: &str = "OVS_CONFIG";

/// One connector, named the way an operator reads it off a machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorRef {
    /// PCI address of the GPU, for example `0000:00:02.0`.
    ///
    /// Needed because `DP-1` exists on every card in the machine.
    /// `ovs-renderer --displays` prints both.
    pub gpu_pci_address: String,
    /// Connector name as the driver reports it.
    ///
    /// The name **Vulkan** reports, which is not always the one the kernel uses:
    /// Mesa calls a panel `monitor` where `/sys/class/drm` calls it `eDP-1`.
    /// `ovs-renderer --displays` prints the one that matters here.
    pub connector: String,
    /// The connector name the *kernel* uses, `eDP-1`.
    ///
    /// A second name for the same socket, because the two layers disagree: Mesa
    /// calls a panel `monitor` where `/sys/class/drm` calls it `eDP-1`. It is
    /// what lets the server say at startup whether the System screen's panel is
    /// actually plugged in, which is the difference between a black screen
    /// somebody has to drive to and a line in a log.
    ///
    /// It used not to be able to *choose* a connector either — wlroots offers no
    /// way to, which is how this machine once ended up with the System screen on
    /// the wrong panel. That is no longer how the screen is put anywhere: the
    /// renderer drives the connector by name, like any other output. This name
    /// is still the kernel's rather than Vulkan's, and still only for saying
    /// whether something is plugged in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drm_connector: Option<String>,
    /// The mode to drive this connector at, when the panel's own is not wanted.
    ///
    /// Absent means the panel's preference, which is right on every monitor that
    /// reports an honest one. It is not always: a panel with no EDID advertises
    /// a handful of fallback timings and nothing else, and one with a bad EDID
    /// advertises a mode it cannot show. The only person who can tell is the one
    /// standing in front of it, so this is theirs to set (owner, 2026-09-25:
    /// "Es fehlt noch eine Modeauswahl im Admin für das System Display").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<ovs_types::config::DisplayMode>,
}

impl ConnectorRef {
    /// Whether the panel named by `drm_connector` is plugged in.
    ///
    /// `None` when nothing was configured to check, or when the kernel has no
    /// such connector — a name that does not exist is worth reporting as a
    /// different thing from a panel that is unplugged.
    ///
    /// The directory is searched rather than built, because its name carries the
    /// card number (`card0-eDP-1`) and that number is exactly the thing which
    /// moves when a driver is installed.
    #[must_use]
    pub fn panel_status(&self, sysfs: &Path) -> Option<String> {
        let connector = self.drm_connector.as_ref()?;
        let suffix = format!("-{connector}");
        let entries = std::fs::read_dir(sysfs).ok()?;

        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("card") && name.ends_with(&suffix) {
                let status = std::fs::read_to_string(entry.path().join("status")).ok()?;
                return Some(status.trim().to_owned());
            }
        }

        None
    }
}

/// What this machine is, as opposed to what it is showing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerConfig {
    /// What this server calls itself on the network.
    ///
    /// The name an operator picks out of a list of servers, so it is theirs to
    /// choose: "Bühne links" is a better answer than a host name somebody typed
    /// once while installing Debian. Absent falls back to the host name, which
    /// is at least a name they chose earlier.
    ///
    /// Here rather than in the database for the same reason the Service Display
    /// is: it is a fact about the machine, it has to be readable before
    /// anything is running, and recovering a server means copying this file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The connector the System screen is on.
    ///
    /// Absent on a machine with no local screen, which is a legitimate server
    /// and not a fault — a rack unit an operator only ever reaches over the
    /// network. The renderer may then use every connector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_display: Option<ConnectorRef>,
}

impl ServerConfig {
    /// Reads the configuration, or returns an empty one.
    ///
    /// A missing file is not an error: a machine with no local screen needs no
    /// configuration at all. A file that cannot be *parsed* is different — it
    /// says something and this build cannot tell what — so that is reported and
    /// the server carries on with nothing rather than with half of it.
    #[must_use]
    pub fn load() -> Self {
        let path = Self::path();
        match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<Self>(&text) {
                Ok(config) => {
                    tracing::info!(path = %path.display(), "read the server configuration");
                    config
                }
                Err(error) => {
                    tracing::error!(
                        path = %path.display(),
                        "the server configuration cannot be read: {error}; carrying on as though \
                         there were none, so the System screen's connector is not reserved"
                    );
                    Self::default()
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(error) => {
                tracing::warn!(path = %path.display(), "cannot open the server configuration: {error}");
                Self::default()
            }
        }
    }

    /// Returns where the configuration is read from.
    #[must_use]
    pub fn path() -> PathBuf {
        std::env::var(PATH_ENV)
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(DEFAULT_PATH))
    }

    /// Returns the connector roles this configuration fixes.
    ///
    /// Only the System screen's, and only as `SERVICE_DISPLAY`. Which sockets
    /// carry a show is the show's business, not the machine's: a server is
    /// bought once and re-purposed all evening.
    #[must_use]
    pub fn roles(&self) -> std::collections::BTreeMap<String, ConnectorRole> {
        let mut roles = std::collections::BTreeMap::new();
        if let Some(display) = &self.service_display {
            roles.insert(
                ovs_db::connector_key(&display.gpu_pci_address, &display.connector),
                ConnectorRole::ServiceDisplay,
            );
        }
        roles
    }

    /// Writes the configuration, creating the directory if it is missing.
    ///
    /// **Replaced whole or not at all.** [`Self::load`] says what a file it
    /// cannot parse costs: the System screen's connector is not reserved, the
    /// renderer takes that socket, and the panel somebody has to stand at shows
    /// a wall. A write interrupted halfway — a full disk, a power cut, a server
    /// killed mid-request — produces precisely that file. So this writes beside
    /// the real one and renames, which on the same filesystem is one step: a
    /// reader sees the old file or the new one, never half of either.
    ///
    /// # Errors
    ///
    /// Returns the io error when the file cannot be written.
    pub fn write(&self, path: &Path) -> std::io::Result<()> {
        if let Some(directory) = path.parent() {
            std::fs::create_dir_all(directory)?;
        }
        let mut text = serde_json::to_string_pretty(self)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        text.push('\n');

        // Beside the target rather than in /tmp, because rename is only atomic
        // within one filesystem and /tmp is very often another one.
        let mut temporary = path.as_os_str().to_owned();
        temporary.push(".new");
        let temporary = PathBuf::from(temporary);

        std::fs::write(&temporary, text)?;
        match std::fs::rename(&temporary, path) {
            Ok(()) => Ok(()),
            Err(error) => {
                // The half-written file must not be left behind wearing a name
                // close enough to the real one to be copied by mistake.
                let _ = std::fs::remove_file(&temporary);
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> ServerConfig {
        ServerConfig {
            name: None,
            service_display: Some(ConnectorRef {
                gpu_pci_address: "0000:00:02.0".to_owned(),
                connector: "eDP-1".to_owned(),
                drm_connector: Some("eDP-1".to_owned()),
                mode: None,
            }),
        }
    }

    #[test]
    fn the_system_screens_connector_is_reserved() {
        let roles = config().roles();

        assert_eq!(
            roles.get("0000:00:02.0/eDP-1"),
            Some(&ConnectorRole::ServiceDisplay)
        );
        assert_eq!(roles.len(), 1, "and nothing else is decided here");
    }

    #[test]
    fn a_machine_with_no_local_screen_reserves_nothing() {
        // A rack unit an operator only ever reaches over the network. Every
        // connector is then a show's to use.
        assert!(ServerConfig::default().roles().is_empty());
    }

    #[test]
    fn it_survives_a_round_trip_through_the_file() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let path = directory.path().join("nested").join("config.json");

        config().write(&path).expect("writes");
        let text = std::fs::read_to_string(&path).expect("reads");
        let read: ServerConfig = serde_json::from_str(&text).expect("parses");

        assert_eq!(read, config());
    }

    #[test]
    fn the_file_reads_the_way_an_operator_would_write_it() {
        // Hand-written, on a commissioning day, from what `--displays` printed.
        let written = r#"{
            "service_display": { "gpu_pci_address": "0000:01:00.0", "connector": "DP-1" }
        }"#;

        let read: ServerConfig = serde_json::from_str(written).expect("parses");
        assert_eq!(read.service_display.expect("a display").connector, "DP-1");
    }

    #[test]
    fn the_server_can_say_whether_the_panel_is_plugged_in() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let connector = directory.path().join("card0-eDP-1");
        std::fs::create_dir_all(&connector).expect("a directory");
        std::fs::write(connector.join("status"), "connected\n").expect("a status");

        let display = config().service_display.expect("a display");

        assert_eq!(
            display.panel_status(directory.path()).as_deref(),
            Some("connected")
        );
    }

    #[test]
    fn a_panel_is_found_whatever_number_its_card_got() {
        // The point of searching rather than building the name: installing a
        // second driver renumbers the cards, and the panel has not moved.
        let directory = tempfile::tempdir().expect("a temporary directory");
        let connector = directory.path().join("card3-eDP-1");
        std::fs::create_dir_all(&connector).expect("a directory");
        std::fs::write(connector.join("status"), "disconnected\n").expect("a status");

        let display = config().service_display.expect("a display");

        assert_eq!(
            display.panel_status(directory.path()).as_deref(),
            Some("disconnected")
        );
    }

    #[test]
    fn a_connector_the_kernel_does_not_have_says_nothing() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let display = config().service_display.expect("a display");

        assert_eq!(display.panel_status(directory.path()), None);
    }

    #[test]
    fn a_file_that_says_nothing_is_a_machine_that_reserves_nothing() {
        let read: ServerConfig = serde_json::from_str("{}").expect("parses");
        assert_eq!(read, ServerConfig::default());
    }
}
