//! Every socket on the machine, including the ones the renderer cannot reach.
//!
//! # Why the renderer is not the whole answer
//!
//! The renderer enumerates displays on the one Vulkan device it renders with, so
//! what it reports is every socket on *its* card and nothing else. On a machine
//! with two cards that leaves the other card's sockets invisible — and the other
//! card is exactly where the System screen belongs, because the show's card
//! should not be carrying a browser (owner, 2026-09-25: "ich kann jetzt auch
//! nicht den internen Monitor auswählen. Es geht nur kein lokaler und HDMI-0",
//! and "Es müssen alle Ausgänge angezeigt werden").
//!
//! So this reads the kernel, which knows all of them.
//!
//! # Only the cards the renderer does not render with
//!
//! Sockets on the renderer's own card come from the renderer and are not
//! repeated here, and that is not tidiness — it is because **the two layers use
//! different names for the same socket**. Mesa calls a panel `monitor` where
//! `/sys/class/drm` calls it `eDP-1`, and the Sony on the reference machine is
//! `HDMI-0` to Vulkan and `HDMI-A-5` to the kernel. There is no reliable way to
//! say which kernel name is which Vulkan name, and a guess would put a show on
//! the wrong wall.
//!
//! What decides a show output is the Vulkan name, because that is what the
//! renderer matches roles against. What decides the System screen is the *card*
//! — wlroots takes a card, not a connector — so a kernel name is enough there,
//! and it is what `ConnectorRef::drm_connector` is for.
//!
//! Which is the whole division: the renderer's list is authoritative for its own
//! card and is the only list a show output may come from; this adds the cards it
//! cannot see, for the one setting that is chosen by card.

use std::path::Path;

/// Where the kernel keeps the connectors.
const DRM_SYSFS: &str = "/sys/class/drm";

/// One socket the kernel knows about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineConnector {
    /// PCI address of the card it is on.
    pub gpu_pci_address: String,
    /// The kernel's name for it, `eDP-1` or `HDMI-A-5`.
    pub connector: String,
    /// Whether something is plugged into it.
    pub connected: bool,
}

/// Every socket on a card the renderer does not render with.
///
/// `renderer_gpu` is the renderer's own PCI address, whose sockets are left out;
/// pass an empty string on a machine with no renderer, and every socket comes
/// back.
#[must_use]
pub fn elsewhere(renderer_gpu: &str) -> Vec<MachineConnector> {
    elsewhere_in(Path::new(DRM_SYSFS), renderer_gpu)
}

/// The same, against a given `/sys/class/drm`.
#[must_use]
pub fn elsewhere_in(sysfs: &Path, renderer_gpu: &str) -> Vec<MachineConnector> {
    let cards = cards_in(sysfs);
    let Ok(entries) = std::fs::read_dir(sysfs) else {
        return Vec::new();
    };

    let mut found: Vec<MachineConnector> = Vec::new();
    for entry in entries.filter_map(std::result::Result::ok) {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        // `card0-eDP-1`: the card number, then the connector. The number is the
        // thing that moves when a driver is installed, which is why it is turned
        // into a PCI address before anything else looks at it.
        let Some((card, connector)) = name.split_once('-') else {
            continue;
        };
        let Some(address) = cards.get(card) else {
            continue;
        };
        if address == renderer_gpu {
            continue;
        }

        // A socket with no status is not a connector — `cardN-render` and the
        // card node itself both live here.
        let Ok(status) = std::fs::read_to_string(entry.path().join("status")) else {
            continue;
        };

        found.push(MachineConnector {
            gpu_pci_address: address.clone(),
            connector: connector.to_owned(),
            connected: status.trim() == "connected",
        });
    }

    // By card and then by name, so the list an operator reads is the same every
    // time. A directory listing is in whatever order the filesystem feels like.
    found.sort_by(|left, right| {
        (&left.gpu_pci_address, &left.connector).cmp(&(&right.gpu_pci_address, &right.connector))
    });
    found
}

/// Which card is at which PCI address, keyed by `card0`, `card1`.
///
/// **From sysfs, not from `/dev/dri/by-path`.** That directory answers the same
/// question in one readdir and does not exist for this process: `ovsd.service`
/// carries `PrivateDevices=yes`, which gives it a private `/dev` holding a
/// minimal set of pseudo-devices and nothing else. Reading it returned nothing,
/// every connector was skipped for having no card to name it by, and the page
/// offered an empty list on a machine with two cards in it.
///
/// `/sys/class/drm/card0/device` is a symlink whose target ends in the address.
/// The link is read rather than resolved, because the last component is the whole
/// answer and `read_link` does not need the target to exist — which is also what
/// lets the tests below use a fixture instead of a real PCI tree.
fn cards_in(sysfs: &Path) -> std::collections::BTreeMap<String, String> {
    let mut cards = std::collections::BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(sysfs) else {
        return cards;
    };

    for entry in entries.filter_map(std::result::Result::ok) {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        // `card0`, and not `card0-eDP-1`: a connector has a device link of its
        // own pointing at the same card, and taking both would map the connector's
        // name to an address as though it were a card.
        if !name.starts_with("card") || name.contains('-') {
            continue;
        }
        let Ok(target) = std::fs::read_link(entry.path().join("device")) else {
            continue;
        };
        let Some(address) = target.file_name().and_then(std::ffi::OsStr::to_str) else {
            continue;
        };
        cards.insert(name.to_owned(), address.to_owned());
    }

    cards
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a `/sys/class/drm` and a `/dev/dri/by-path` like the reference
    /// machine's: an Intel adapter with the laptop's panel, and an NVIDIA card
    /// with the Sony on it.
    ///
    /// Unix, because every test that uses it is: a `/sys/class/drm` built out
    /// of symlinks is not a thing a Windows test can make, and without this the
    /// Windows test build carries a helper nothing calls.
    #[cfg(unix)]
    fn machine() -> tempfile::TempDir {
        let root = tempfile::tempdir().expect("a directory");
        let sysfs = root.path().join("drm");
        std::fs::create_dir_all(&sysfs).expect("creates");

        for (card, connectors) in [
            (
                "card0",
                vec![("eDP-1", "connected"), ("HDMI-A-1", "disconnected")],
            ),
            (
                "card1",
                vec![("HDMI-A-5", "connected"), ("DP-3", "disconnected")],
            ),
        ] {
            // The card node itself, which has no status and must be skipped.
            std::fs::create_dir_all(sysfs.join(card)).expect("creates");
            for (connector, status) in connectors {
                let directory = sysfs.join(format!("{card}-{connector}"));
                std::fs::create_dir_all(&directory).expect("creates");
                std::fs::write(directory.join("status"), format!("{status}\n")).expect("writes");
            }
        }

        // Symlinks, because that is what the kernel puts there and a test that
        // wrote regular files would pass against code that never read a link.
        // The target does not exist and does not need to: what is read off it is
        // the last component.
        #[cfg(unix)]
        for (card, address) in [("card0", "0000:00:02.0"), ("card1", "0000:01:00.0")] {
            std::os::unix::fs::symlink(
                format!("../../../devices/pci0000:00/{address}"),
                sysfs.join(card).join("device"),
            )
            .expect("links");
        }

        root
    }

    #[cfg(unix)]
    #[test]
    fn the_other_card_is_listed_and_the_renderer_s_own_is_not() {
        // The whole point: the System screen belongs on the card the show is not
        // using, and that card is invisible to the renderer.
        let root = machine();
        let found = elsewhere_in(&root.path().join("drm"), "0000:01:00.0");

        assert_eq!(
            found,
            vec![
                MachineConnector {
                    gpu_pci_address: "0000:00:02.0".to_owned(),
                    connector: "HDMI-A-1".to_owned(),
                    connected: false,
                },
                MachineConnector {
                    gpu_pci_address: "0000:00:02.0".to_owned(),
                    connector: "eDP-1".to_owned(),
                    connected: true,
                },
            ],
            "the renderer's own card was listed, or the other one was not"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_machine_with_no_renderer_shows_every_socket() {
        // Before a renderer has said anything, which is the state a fresh
        // machine is in while somebody is configuring it.
        let root = machine();
        let found = elsewhere_in(&root.path().join("drm"), "");

        assert_eq!(found.len(), 4, "got {found:?}");
        assert!(found.iter().any(|one| one.connector == "HDMI-A-5"));
    }

    #[cfg(unix)]
    #[test]
    fn a_socket_whose_card_has_no_address_is_left_out() {
        // Rather than guessed at. `card0-eDP-1` says which card it is on by a
        // number that moves when a driver is installed, and a connector this
        // cannot name by PCI address is one nothing could act on anyway.
        let root = machine();
        std::fs::remove_file(root.path().join("drm/card0/device")).expect("removes");

        let found = elsewhere_in(&root.path().join("drm"), "");
        assert!(
            found
                .iter()
                .all(|one| one.gpu_pci_address == "0000:01:00.0"),
            "a socket was listed under a card that could not be named: {found:?}"
        );
    }

    #[test]
    fn a_machine_with_no_kernel_to_ask_answers_nothing() {
        // Windows, and any Linux where `/sys` is not mounted. An empty list is
        // "this cannot be asked", and the page offers what the renderer said.
        let found = elsewhere_in(Path::new("/nowhere/class/drm"), "");
        assert!(found.is_empty());
    }
}
