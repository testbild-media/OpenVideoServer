//! Saying where this server is, so nobody has to type an address.
//!
//! The owner, 2026-09-24: "Kann man noch etwas einbauen, dass ein Client direkt
//! den Server findet… Dann kann man direkt auf den Server klicken und sich
//! einloggen und muss nicht jedes Mal die IP eingeben die man vergessen kann
//! oder sich in anderen Netzwerken ändern kann wenn der Server mobil ist."
//!
//! The mobile case is the one that matters. A server carried between venues
//! takes a different address in each of them, and an address an operator wrote
//! down at the last venue is worse than none: it is a thing to try that will
//! not work, at the hour when nothing else is working either.
//!
//! # What this is not
//!
//! **Not a security boundary and not a claim.** Anything on the segment can see
//! this announcement, which is the point of it — and nothing in it grants
//! access. A client that finds a server still has to sign in, and this carries
//! no credential, no token and nothing about what is on the machine. What it
//! carries is what a person would otherwise read off a label: a name, a port,
//! a version, and whether the machine has been set up yet.
//!
//! **Not required.** A network with mDNS filtered, or a client on a different
//! subnet, is a network where an operator types the address as before. This
//! removes a chore; it must never become the only way in.

use std::net::Ipv4Addr;

use mdns_sd::{ServiceDaemon, ServiceInfo};

/// The service type this server answers to.
///
/// Unregistered with IANA on purpose: a name under `_openvideoserver` is ours
/// and collides with nothing, where a registered short name would be a claim on
/// a word other products use.
pub const SERVICE_TYPE: &str = "_openvideoserver._tcp.local.";

/// The TXT key carrying the server's own name.
pub const KEY_NAME: &str = "name";
/// The TXT key carrying the build version.
pub const KEY_VERSION: &str = "version";
/// The TXT key carrying the major version of the HTTP API.
pub const KEY_API: &str = "api";
/// The TXT key that says whether first-run setup is still needed.
///
/// `yes` or `no`. A client showing a machine that has never been set up offers
/// to set it up rather than a password box nothing will answer — which is the
/// difference between a server somebody can commission from the sofa and one
/// they have to find a keyboard for.
pub const KEY_SETUP: &str = "setup";

/// A running announcement. Dropping it takes the server off the network.
pub struct Advertisement {
    daemon: ServiceDaemon,
    full_name: String,
}

impl Advertisement {
    /// The instance name this server registered under.
    #[must_use]
    pub fn full_name(&self) -> &str {
        &self.full_name
    }
}

impl Drop for Advertisement {
    /// Withdraws the announcement.
    ///
    /// Politeness that matters: without it, a client keeps offering a server
    /// that has been switched off until the record's time to live runs out, and
    /// an operator clicks a machine that is not there.
    fn drop(&mut self) {
        if let Err(error) = self.daemon.unregister(&self.full_name) {
            tracing::debug!("could not withdraw the discovery announcement: {error}");
        }
        if let Err(error) = self.daemon.shutdown() {
            tracing::debug!("could not stop the discovery responder: {error}");
        }
    }
}

/// What to announce.
pub struct Announcement<'a> {
    /// The name an operator sees in a list of servers.
    pub name: &'a str,
    /// The port the HTTP API is on.
    pub port: u16,
    /// This build's version.
    pub version: &'a str,
    /// Major version of the HTTP API.
    pub api_version: u32,
    /// Whether the server still needs its first account.
    pub needs_setup: bool,
}

/// Starts announcing this server on the local network.
///
/// # Errors
///
/// Returns the underlying error when the responder cannot be started or the
/// service cannot be registered. **A caller should log this and carry on.** A
/// server that refused to start because a discovery beacon failed would have
/// turned a convenience into a single point of failure, and the address still
/// works.
pub fn advertise(announcement: &Announcement<'_>) -> Result<Advertisement, mdns_sd::Error> {
    let daemon = ServiceDaemon::new()?;

    // The instance name is what a person reads; the host name is what the
    // protocol resolves. Keeping them apart means a server called "Bühne links"
    // is announced under exactly that, while the address it resolves to stays a
    // legal host name.
    let host = format!("{}.local.", sanitise_host(announcement.name));

    let properties = [
        (KEY_NAME, announcement.name.to_owned()),
        (KEY_VERSION, announcement.version.to_owned()),
        (KEY_API, announcement.api_version.to_string()),
        (
            KEY_SETUP,
            if announcement.needs_setup {
                "yes"
            } else {
                "no"
            }
            .to_owned(),
        ),
    ];

    // Addresses are left to the responder, which watches the interfaces. A
    // server carried to another venue changes address without anything here
    // being told, and a list of addresses captured at startup would announce
    // the one it had this morning.
    let service = ServiceInfo::new(
        SERVICE_TYPE,
        announcement.name,
        &host,
        (),
        announcement.port,
        &properties[..],
    )?
    .enable_addr_auto();

    let full_name = service.get_fullname().to_owned();
    daemon.register(service)?;
    tracing::info!(
        name = announcement.name,
        port = announcement.port,
        "announcing this server on the local network"
    );
    confirm(&daemon, &full_name);

    Ok(Advertisement { daemon, full_name })
}

/// How long to wait to hear this server's own announcement come back.
///
/// Long enough for a responder to answer its own query on a quiet network,
/// short enough that a server which never will is reported while somebody is
/// still reading the log of the start they just watched.
const CONFIRM_WITHIN: std::time::Duration = std::time::Duration::from_secs(5);

/// Listens for this server's own announcement, and says so when it never comes.
///
/// **Registering is not announcing**, and the difference cost an hour on
/// 2026-09-24. The responder asks the kernel which interfaces this machine has;
/// under the service unit's sandbox that question was refused, so it had none,
/// announced on none — and logged that it was announcing, because from inside
/// the registration had gone perfectly. The server said one thing and the
/// network saw another, with nothing anywhere to tell them apart.
///
/// So the server listens for itself. It runs on a thread and blocks nothing: a
/// machine that cannot be found is a chore, and a server that would not start
/// because of one would be a fault.
fn confirm(daemon: &ServiceDaemon, full_name: &str) {
    let Ok(events) = daemon.browse(SERVICE_TYPE) else {
        return;
    };
    let wanted = full_name.to_owned();

    std::thread::Builder::new()
        .name("ovs-discovery-check".to_owned())
        .spawn(move || {
            let deadline = std::time::Instant::now() + CONFIRM_WITHIN;
            while let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) {
                let Ok(event) = events.recv_timeout(left) else {
                    break;
                };
                if let mdns_sd::ServiceEvent::ServiceResolved(info) = event {
                    if info.get_fullname() == wanted && !info.get_addresses().is_empty() {
                        tracing::info!(
                            addresses = ?info.get_addresses(),
                            "this server can be found on the network"
                        );
                        return;
                    }
                }
            }

            tracing::warn!(
                "this server registered a discovery announcement but never heard it on the \
                 network, so clients will not find it and will have to be given its address. \
                 The usual cause is a sandbox that will not let it ask the kernel which \
                 interfaces exist: a systemd unit needs AF_NETLINK in RestrictAddressFamilies. \
                 A network that filters multicast looks the same from here and needs nothing \
                 done."
            );
        })
        .ok();
}

/// Turns a server's name into something legal in a host name.
///
/// A name an operator typed is a name with spaces, umlauts and punctuation in
/// it. The label a host name is made of allows letters, digits and hyphens, so
/// everything else becomes a hyphen and a name that had nothing usable in it at
/// all falls back to a fixed one rather than to an empty label.
fn sanitise_host(name: &str) -> String {
    let mut host: String = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();

    while host.contains("--") {
        host = host.replace("--", "-");
    }
    let host = host.trim_matches('-').to_owned();

    // A label is at most 63 octets, and the trim is repeated because cutting
    // may have left a hyphen at the end.
    let host: String = host.chars().take(63).collect();
    let host = host.trim_matches('-').to_owned();

    if host.is_empty() {
        "openvideoserver".to_owned()
    } else {
        host
    }
}

/// This machine's name, for a server that has not been given one.
///
/// The host name, which is what an operator called the machine when they built
/// it and what they will recognise in a list. `OpenVideoServer` when even that
/// cannot be read, because a list entry with no name is a row nobody can pick.
#[must_use]
pub fn default_name() -> String {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|text| text.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "OpenVideoServer".to_owned())
}

/// Whether an address is one a client on another machine could reach.
///
/// Loopback is not: a server announcing `127.0.0.1` would fill every client's
/// list with an entry that resolves to the client itself.
#[must_use]
pub fn is_reachable(address: Ipv4Addr) -> bool {
    !address.is_loopback() && !address.is_unspecified() && !address.is_broadcast()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_an_operator_typed_becomes_a_legal_host_label() {
        assert_eq!(sanitise_host("Bühne links"), "b-hne-links");
        assert_eq!(sanitise_host("OVS Rack 1"), "ovs-rack-1");
        assert_eq!(sanitise_host("--weird--"), "weird");
        assert_eq!(sanitise_host("  "), "openvideoserver");
        assert_eq!(sanitise_host(""), "openvideoserver");
    }

    #[test]
    fn a_long_name_is_cut_to_a_label_and_not_left_ending_in_a_hyphen() {
        let host = sanitise_host(&"a".repeat(80));
        assert_eq!(host.len(), 63);

        // Cut mid-punctuation, which is where a naive truncation leaves a
        // trailing hyphen and an illegal label.
        let awkward = format!("{}{}", "a".repeat(62), " b");
        let host = sanitise_host(&awkward);
        assert!(!host.ends_with('-'), "got {host}");
    }

    #[test]
    fn loopback_is_not_somewhere_another_machine_can_reach() {
        assert!(!is_reachable(Ipv4Addr::LOCALHOST));
        assert!(!is_reachable(Ipv4Addr::UNSPECIFIED));
        assert!(is_reachable(Ipv4Addr::new(10, 11, 0, 105)));
        assert!(is_reachable(Ipv4Addr::new(192, 168, 1, 20)));
    }
}
