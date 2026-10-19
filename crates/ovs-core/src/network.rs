//! The machine's wired network, from its administration page.
//!
//! The owner, 2026-10-19: "Network Settings via Admin GUI, also ob DHCP/Static
//! inkl IP Subnetz Gateway DNS etc." And on the safety net: "ja aber mache
//! lieber 5 min."
//!
//! # A change is a trial until somebody keeps it
//!
//! A wrong address, mask or gateway set from across the network cuts off the
//! person who set it, and at a venue there may be nobody standing at the
//! machine. So a change is applied at once and **put back by itself after five
//! minutes** unless somebody keeps it: from the administration page at the new
//! address, from the same page where the address did not change, or on the
//! System screen. What that costs is five minutes of a mistake, at worst.
//!
//! # Two halves, as with updates
//!
//! The server only reads: which wired interfaces there are, what they have now,
//! and how `/etc/network/interfaces` brings them up. Changing any of it needs
//! root, so the server writes the request into its own state directory and asks
//! the init system to start `ovs-network.service` — the one unit a polkit rule
//! (`deploy/polkit/52-openvideoserver-network.rules`) lets it start. The unit
//! runs `deploy/network/ovs-network`, which reads that request as somebody
//! else's words: every value is checked again before anything is written, the
//! files it changes are kept, and it waits out the five minutes itself — it has
//! to, because the server may be the thing nobody can reach.
//!
//! IPv4 only, and wired only. A radio needs a network name and a key, and a VPN
//! is configured by itself; neither is offered here.

use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::process::Command;

use ovs_proto::http::{NetworkInterface, NetworkSettings, NetworkStatus, NetworkTrial};

/// How the machine brings its interfaces up.
pub const INTERFACES_FILE: &str = "/etc/network/interfaces";

/// Where the machine's nameservers are.
pub const RESOLV_FILE: &str = "/etc/resolv.conf";

/// Where the helper writes how far a change got.
pub const TRIAL_FILE: &str = "/var/lib/ovs-network/status.json";

/// The unit that changes the network.
pub const UNIT: &str = "ovs-network.service";

/// The server's own state directory, where the request and the answer go.
fn data_dir() -> PathBuf {
    std::env::var("OVS_DATA_DIR").map_or_else(|_| PathBuf::from("/var/lib/ovs"), PathBuf::from)
}

/// Where the request for the helper is written.
#[must_use]
pub fn request_path() -> PathBuf {
    data_dir().join("network-request")
}

/// Where keeping or undoing a change is written.
#[must_use]
pub fn answer_path() -> PathBuf {
    data_dir().join("network-answer")
}

/// What the machine's network is now, and the last change made to it.
#[must_use]
pub fn status() -> NetworkStatus {
    let configured = std::fs::read_to_string(INTERFACES_FILE).unwrap_or_default();
    let interfaces = wired_interfaces(Path::new("/sys/class/net"))
        .into_iter()
        .map(|name| describe(&name, &configured))
        .collect();
    NetworkStatus {
        interfaces,
        nameservers: nameservers(&std::fs::read_to_string(RESOLV_FILE).unwrap_or_default()),
        trial: trial(),
        unavailable: unavailable(),
    }
}

/// Why the network cannot be changed from here, if it cannot.
///
/// Only `ifupdown` is driven, which is what Debian's installer sets up. A
/// machine somebody moved to another manager keeps it, and is told so rather
/// than offered fields that would do nothing.
fn unavailable() -> Option<String> {
    let ifup = ["/usr/sbin/ifup", "/sbin/ifup"]
        .iter()
        .any(|path| Path::new(path).exists());
    if !ifup || !Path::new(INTERFACES_FILE).exists() {
        return Some(
            "this machine's network is not run by ifupdown, so it is changed where it is run"
                .to_owned(),
        );
    }
    if Path::new("/run/NetworkManager").exists() {
        return Some(
            "NetworkManager runs this machine's network, so it is changed there".to_owned(),
        );
    }
    None
}

/// The wired interfaces under a `/sys/class/net`, in name order: those with a
/// device behind them that is not a radio. The loopback has no device, nor has
/// a VPN's tunnel.
#[must_use]
pub fn wired_interfaces(sysfs: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(sysfs) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_str()?.to_owned();
            let path = sysfs.join(&name);
            (path.join("device").exists() && !path.join("wireless").exists()).then_some(name)
        })
        .collect();
    names.sort();
    names
}

/// One interface, read from the kernel and from the configuration.
fn describe(name: &str, configured: &str) -> NetworkInterface {
    let sys = Path::new("/sys/class/net").join(name);
    let read = |file: &str| {
        std::fs::read_to_string(sys.join(file))
            .map(|text| text.trim().to_owned())
            .unwrap_or_default()
    };
    let (method, settings) = configuration(name, configured);
    NetworkInterface {
        name: name.to_owned(),
        mac: read("address"),
        link: read("carrier") == "1",
        // The kernel answers -1, or nothing at all, for a link that is down.
        speed_mbps: read("speed")
            .parse::<i64>()
            .ok()
            .and_then(|speed| u32::try_from(speed).ok()),
        addresses: addresses(name),
        gateway: gateway(name),
        method,
        configured: settings,
    }
}

/// The IPv4 addresses an interface has now, as `address/prefix`.
fn addresses(name: &str) -> Vec<String> {
    let Some(answer) = ip(&["-4", "-j", "addr", "show", "dev", name]) else {
        return Vec::new();
    };
    answer
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|link| link["addr_info"].as_array().cloned().unwrap_or_default())
        .filter_map(|info| {
            Some(format!(
                "{}/{}",
                info["local"].as_str()?,
                info["prefixlen"].as_u64()?
            ))
        })
        .collect()
}

/// The default gateway through an interface now, if there is one.
fn gateway(name: &str) -> Option<String> {
    let answer = ip(&["-4", "-j", "route", "show", "default", "dev", name])?;
    answer
        .as_array()?
        .iter()
        .find_map(|route| route["gateway"].as_str().map(str::to_owned))
}

/// Asks `ip` for JSON.
fn ip(arguments: &[&str]) -> Option<serde_json::Value> {
    let output = Command::new("ip").args(arguments).output().ok()?;
    output
        .status
        .success()
        .then(|| serde_json::from_slice(&output.stdout).ok())
        .flatten()
}

/// The nameservers in a `resolv.conf`.
#[must_use]
pub fn nameservers(resolv: &str) -> Vec<String> {
    resolv
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            (words.next()? == "nameserver").then(|| words.next().map(str::to_owned))?
        })
        .collect()
}

/// How `/etc/network/interfaces` brings an interface up: its IPv4 method, and
/// for `static` what it sets.
#[must_use]
pub fn configuration(name: &str, interfaces: &str) -> (String, Option<NetworkSettings>) {
    let mut method: Option<String> = None;
    let mut settings = NetworkSettings {
        interface: name.to_owned(),
        method: String::new(),
        address: None,
        netmask: None,
        gateway: None,
        nameservers: Vec::new(),
    };
    let mut inside = false;
    for line in interfaces.lines() {
        let indented = line.starts_with([' ', '\t']);
        let words: Vec<&str> = line.split_whitespace().collect();
        if !indented {
            inside = false;
        }
        match words.as_slice() {
            ["iface", interface, "inet", how, ..] if *interface == name => {
                method = Some((*how).to_owned());
                inside = true;
            }
            ["address", value] if inside => match value.split_once('/') {
                Some((address, prefix)) => {
                    settings.address = Some(address.to_owned());
                    settings.netmask = Some(prefix.to_owned());
                }
                None => settings.address = Some((*value).to_owned()),
            },
            ["netmask", value] if inside => settings.netmask = Some((*value).to_owned()),
            ["gateway", value] if inside => settings.gateway = Some((*value).to_owned()),
            _ => {}
        }
    }
    let method = method.unwrap_or_else(|| "none".to_owned());
    settings.method.clone_from(&method);
    let settings = (method == "static").then_some(settings);
    (method, settings)
}

/// A change the helper will accept: every value checked, the mask as a prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// The interface.
    pub interface: String,
    /// `dhcp` or `static`.
    pub method: String,
    /// The address, for `static`.
    pub address: Option<Ipv4Addr>,
    /// The prefix length, for `static`.
    pub prefix: Option<u8>,
    /// The gateway, for `static`.
    pub gateway: Option<Ipv4Addr>,
    /// The nameservers, for `static`.
    pub nameservers: Vec<Ipv4Addr>,
}

impl Change {
    /// In a few words, as the trial is labelled.
    #[must_use]
    pub fn summary(&self) -> String {
        match (self.address, self.prefix) {
            (Some(address), Some(prefix)) => format!("static {address}/{prefix}"),
            _ => "dhcp".to_owned(),
        }
    }

    /// The request as the helper reads it: one `key=value` a line.
    #[must_use]
    pub fn request(&self, id: &str) -> String {
        let list = |addresses: &[Ipv4Addr]| {
            addresses
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" ")
        };
        format!(
            "id={id}\ninterface={}\nmethod={}\naddress={}\nprefix={}\ngateway={}\nnameservers={}\n",
            self.interface,
            self.method,
            self.address
                .map(|address| address.to_string())
                .unwrap_or_default(),
            self.prefix
                .map(|prefix| prefix.to_string())
                .unwrap_or_default(),
            self.gateway
                .map(|gateway| gateway.to_string())
                .unwrap_or_default(),
            list(&self.nameservers),
        )
    }
}

/// A subnet mask, as `255.255.255.0`, `/24` or `24`, as a prefix length.
#[must_use]
pub fn prefix_of(mask: &str) -> Option<u8> {
    let mask = mask.trim();
    let digits = mask.strip_prefix('/').unwrap_or(mask);
    if !digits.is_empty() && digits.len() <= 2 && digits.bytes().all(|byte| byte.is_ascii_digit()) {
        let prefix: u8 = digits.parse().ok()?;
        return (1..=32).contains(&prefix).then_some(prefix);
    }
    let bits = u32::from(mask.parse::<Ipv4Addr>().ok()?);
    // Contiguous ones from the top, and at least one of them.
    let prefix = bits.leading_ones();
    (bits.checked_shl(prefix).unwrap_or(0) == 0 && prefix >= 1)
        .then(|| u8::try_from(prefix).ok())
        .flatten()
}

/// Checks a requested change against the interfaces there are.
///
/// # Errors
///
/// Says what is wrong, in a sentence for the page: an unknown interface, an
/// address that is not one, a mask that is not one, a gateway outside the
/// subnet, too many nameservers.
pub fn validate(settings: &NetworkSettings, wired: &[String]) -> Result<Change, String> {
    if !wired.contains(&settings.interface) {
        return Err(format!(
            "{:?} is not a wired interface of this machine",
            settings.interface
        ));
    }
    let blank = |value: &Option<String>| {
        value
            .as_deref()
            .map(str::trim)
            .unwrap_or_default()
            .is_empty()
    };
    match settings.method.as_str() {
        "dhcp" => Ok(Change {
            interface: settings.interface.clone(),
            method: "dhcp".to_owned(),
            address: None,
            prefix: None,
            gateway: None,
            nameservers: Vec::new(),
        }),
        "static" => {
            let address: Ipv4Addr = settings
                .address
                .as_deref()
                .map(str::trim)
                .unwrap_or_default()
                .parse()
                .map_err(|_| "the address is not an IPv4 address".to_owned())?;
            let prefix = settings
                .netmask
                .as_deref()
                .and_then(prefix_of)
                .ok_or_else(|| "the subnet mask is not one, such as 255.255.255.0".to_owned())?;
            let mask = u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0);
            let network = u32::from(address) & mask;
            if address.is_unspecified()
                || address.is_loopback()
                || address.is_multicast()
                || address.is_broadcast()
            {
                return Err(format!("{address} cannot be this machine's address"));
            }
            if prefix <= 30
                && (u32::from(address) == network || u32::from(address) == network | !mask)
            {
                return Err(format!(
                    "{address} is the subnet's own {} address, not one a machine can have",
                    if u32::from(address) == network {
                        "network"
                    } else {
                        "broadcast"
                    }
                ));
            }
            let gateway = if blank(&settings.gateway) {
                None
            } else {
                let gateway: Ipv4Addr = settings
                    .gateway
                    .as_deref()
                    .map(str::trim)
                    .unwrap_or_default()
                    .parse()
                    .map_err(|_| "the gateway is not an IPv4 address".to_owned())?;
                if u32::from(gateway) & mask != network {
                    return Err(format!(
                        "the gateway {gateway} is outside {}/{prefix}, so it could not be reached",
                        Ipv4Addr::from(network)
                    ));
                }
                if gateway == address {
                    return Err("the gateway cannot be this machine's own address".to_owned());
                }
                Some(gateway)
            };
            let nameservers = settings
                .nameservers
                .iter()
                .map(|server| server.trim())
                .filter(|server| !server.is_empty())
                .map(|server| {
                    server
                        .parse::<Ipv4Addr>()
                        .map_err(|_| format!("the nameserver {server:?} is not an IPv4 address"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            if nameservers.len() > 3 {
                return Err("a machine takes three nameservers at most".to_owned());
            }
            Ok(Change {
                interface: settings.interface.clone(),
                method: "static".to_owned(),
                address: Some(address),
                prefix: Some(prefix),
                gateway,
                nameservers,
            })
        }
        other => Err(format!("{other:?} is neither dhcp nor static")),
    }
}

/// The last change made here and how far it got, if any.
#[must_use]
pub fn trial() -> Option<NetworkTrial> {
    let path = std::env::var("OVS_NETWORK_TRIAL").unwrap_or_else(|_| TRIAL_FILE.to_owned());
    read_trial(&std::fs::read_to_string(path).ok()?)
}

/// Reads what the helper wrote; nothing for anything else.
#[must_use]
pub fn read_trial(written: &str) -> Option<NetworkTrial> {
    serde_json::from_str(written).ok()
}

/// Whether a change is part way through.
#[must_use]
pub fn is_open(trial: &NetworkTrial) -> bool {
    matches!(trial.stage.as_str(), "applying" | "waiting")
}

/// What asking for a change, or answering one, produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// Whether it was done.
    pub accepted: bool,
    /// What was asked for, or why nothing was.
    pub detail: String,
}

impl Outcome {
    fn refused(detail: impl Into<String>) -> Self {
        Self {
            accepted: false,
            detail: detail.into(),
        }
    }
}

/// Writes the request and starts the helper. Does not wait: the network may go
/// away underneath the request that asked.
#[must_use]
pub fn apply(change: &Change) -> Outcome {
    if let Some(reason) = unavailable() {
        return Outcome::refused(reason);
    }
    if let Some(open) = trial().filter(is_open) {
        return Outcome::refused(format!(
            "a change to {} is already {}; keep it or undo it first",
            open.interface, open.stage
        ));
    }
    let Ok(token) = crate::auth::issue_opaque_token() else {
        return Outcome::refused("this machine could not make an identifier for the change");
    };
    let id = &token[..16];
    // No answer from an earlier change can stand for this one.
    let _ = std::fs::remove_file(answer_path());
    if let Err(error) = std::fs::write(request_path(), change.request(id)) {
        return Outcome::refused(format!(
            "the request could not be written to {}: {error}",
            request_path().display()
        ));
    }

    let printable = format!("systemctl start --no-block {UNIT}");
    match Command::new("systemctl")
        .args(["start", "--no-block", UNIT])
        .output()
    {
        Ok(result) if result.status.success() => Outcome {
            accepted: true,
            detail: format!(
                "{} on {}, on trial for five minutes",
                change.summary(),
                change.interface
            ),
        },
        Ok(result) => Outcome::refused(format!(
            "`{printable}` was refused ({}): {}",
            result.status,
            String::from_utf8_lossy(&result.stderr)
                .lines()
                .map(str::trim)
                .rfind(|line| !line.is_empty())
                .unwrap_or("it said nothing")
        )),
        Err(error) => Outcome::refused(format!("`{printable}` could not be run: {error}")),
    }
}

/// Keeps the change on trial, or puts the previous settings back now.
#[must_use]
pub fn answer(keep: bool) -> Outcome {
    let Some(open) = trial().filter(|trial| trial.stage == "waiting") else {
        return Outcome::refused("no change is waiting to be kept");
    };
    let word = if keep { "keep" } else { "undo" };
    match std::fs::write(answer_path(), format!("{word} {}\n", open.id)) {
        Ok(()) => Outcome {
            accepted: true,
            detail: if keep {
                format!("{} on {} is kept", open.summary, open.interface)
            } else {
                format!(
                    "the previous settings of {} are being put back",
                    open.interface
                )
            },
        },
        Err(error) => Outcome::refused(format!(
            "the answer could not be written to {}: {error}",
            answer_path().display()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(
        method: &str,
        address: &str,
        mask: &str,
        gateway: &str,
        dns: &[&str],
    ) -> NetworkSettings {
        NetworkSettings {
            interface: "eno1".to_owned(),
            method: method.to_owned(),
            address: Some(address.to_owned()),
            netmask: Some(mask.to_owned()),
            gateway: Some(gateway.to_owned()),
            nameservers: dns.iter().map(|server| (*server).to_owned()).collect(),
        }
    }

    fn wired() -> Vec<String> {
        vec!["eno1".to_owned()]
    }

    #[test]
    fn a_mask_is_taken_either_way_and_only_if_it_is_one() {
        assert_eq!(prefix_of("255.255.255.0"), Some(24));
        assert_eq!(prefix_of("255.255.0.0"), Some(16));
        assert_eq!(prefix_of("255.255.255.252"), Some(30));
        assert_eq!(prefix_of("255.255.255.255"), Some(32));
        assert_eq!(prefix_of("/24"), Some(24));
        assert_eq!(prefix_of("24"), Some(24));
        for not in [
            "255.0.255.0",
            "0.0.0.0",
            "255.255.255.1",
            "33",
            "/0",
            "",
            "24x",
            "255.255.255",
        ] {
            assert_eq!(prefix_of(not), None, "{not:?}");
        }
    }

    #[test]
    fn a_static_change_is_checked_whole() {
        let change = validate(
            &settings(
                "static",
                "10.110.0.50",
                "255.255.255.0",
                "10.110.0.1",
                &["10.110.0.1", "1.1.1.1"],
            ),
            &wired(),
        )
        .expect("valid");
        assert_eq!(change.summary(), "static 10.110.0.50/24");
        assert_eq!(
            change.request("0123456789abcdef"),
            "id=0123456789abcdef\ninterface=eno1\nmethod=static\naddress=10.110.0.50\nprefix=24\n\
             gateway=10.110.0.1\nnameservers=10.110.0.1 1.1.1.1\n"
        );
    }

    #[test]
    fn a_show_network_needs_no_gateway_and_no_nameserver() {
        let change =
            validate(&settings("static", "10.0.0.5", "8", "", &[]), &wired()).expect("valid");
        assert_eq!(change.gateway, None);
        assert!(change.nameservers.is_empty());
    }

    #[test]
    fn what_would_cut_the_machine_off_is_refused_before_it_is_tried() {
        let cases = [
            settings("static", "10.110.0.256", "24", "", &[]),
            settings("static", "10.110.0.50", "255.0.255.0", "", &[]),
            settings("static", "10.110.0.0", "24", "", &[]),
            settings("static", "10.110.0.255", "24", "", &[]),
            settings("static", "127.0.0.1", "8", "", &[]),
            settings("static", "10.110.0.50", "24", "10.111.0.1", &[]),
            settings("static", "10.110.0.50", "24", "10.110.0.50", &[]),
            settings(
                "static",
                "10.110.0.50",
                "24",
                "",
                &["1.1.1.1", "8.8.8.8", "9.9.9.9", "1.0.0.1"],
            ),
            settings("static", "10.110.0.50; reboot", "24", "", &[]),
            settings("static", "10.110.0.50", "24", "", &["1.1.1.1\nmethod=dhcp"]),
            settings("bootp", "", "", "", &[]),
        ];
        for case in cases {
            assert!(validate(&case, &wired()).is_err(), "{case:?} was accepted");
        }
        let mut elsewhere = settings("dhcp", "", "", "", &[]);
        elsewhere.interface = "tailscale0".to_owned();
        assert!(
            validate(&elsewhere, &wired()).is_err(),
            "not a wired interface"
        );
        elsewhere.interface = "eno1\nmethod=static".to_owned();
        assert!(
            validate(&elsewhere, &wired()).is_err(),
            "a name with a line in it"
        );
    }

    #[test]
    fn dhcp_carries_nothing_else() {
        let change = validate(
            &settings("dhcp", "1.2.3.4", "24", "1.2.3.1", &["1.1.1.1"]),
            &wired(),
        )
        .expect("valid");
        assert_eq!(change.summary(), "dhcp");
        assert_eq!(
            change.request("0123456789abcdef"),
            "id=0123456789abcdef\ninterface=eno1\nmethod=dhcp\naddress=\nprefix=\ngateway=\nnameservers=\n"
        );
    }

    #[test]
    fn the_configuration_is_read_as_debian_writes_it() {
        let debian = "source /etc/network/interfaces.d/*\n\nauto lo\niface lo inet loopback\n\n\
                      allow-hotplug eno1\niface eno1 inet dhcp\n";
        assert_eq!(configuration("eno1", debian), ("dhcp".to_owned(), None));
        assert_eq!(configuration("eno2", debian).0, "none");

        let ours =
            "auto lo\niface lo inet loopback\n\n# OpenVideoServer: eno1\nallow-hotplug eno1\n\
                    iface eno1 inet static\n\taddress 10.110.0.50/24\n\tgateway 10.110.0.1\n";
        let (method, settings) = configuration("eno1", ours);
        assert_eq!(method, "static");
        let settings = settings.expect("static settings");
        assert_eq!(settings.address.as_deref(), Some("10.110.0.50"));
        assert_eq!(settings.netmask.as_deref(), Some("24"));
        assert_eq!(settings.gateway.as_deref(), Some("10.110.0.1"));

        let classic =
            "iface eno1 inet static\n    address 192.168.1.10\n    netmask 255.255.255.0\n";
        let settings = configuration("eno1", classic).1.expect("static");
        assert_eq!(settings.netmask.as_deref(), Some("255.255.255.0"));
    }

    #[test]
    fn nameservers_are_read_from_resolv_conf() {
        let resolv =
            "# Generated by dhcpcd\ndomain example\nnameserver 10.110.0.1\nnameserver 1.1.1.1\n";
        assert_eq!(nameservers(resolv), vec!["10.110.0.1", "1.1.1.1"]);
    }

    #[test]
    fn only_wired_interfaces_are_offered() {
        let sysfs = std::env::temp_dir().join(format!("ovs-net-{}", std::process::id()));
        for (name, device, wireless) in [
            ("lo", false, false),
            ("eno1", true, false),
            ("enp3s0", true, false),
            ("wlan0", true, true),
            ("tailscale0", false, false),
        ] {
            std::fs::create_dir_all(sysfs.join(name)).expect("makes");
            if device {
                std::fs::create_dir_all(sysfs.join(name).join("device")).expect("makes");
            }
            if wireless {
                std::fs::create_dir_all(sysfs.join(name).join("wireless")).expect("makes");
            }
        }
        assert_eq!(wired_interfaces(&sysfs), vec!["eno1", "enp3s0"]);
        std::fs::remove_dir_all(&sysfs).ok();
    }

    #[test]
    fn the_helpers_progress_is_read_as_it_writes_it() {
        let written = r#"{"id":"0123456789abcdef","interface":"eno1","summary":"static 10.110.0.50/24","stage":"waiting","detail":"keep it within five minutes","deadline":1792380000,"at":"2026-10-19T03:00:00Z"}"#;
        let trial = read_trial(written).expect("reads");
        assert!(is_open(&trial));
        assert_eq!(trial.deadline, Some(1_792_380_000));
        let kept = NetworkTrial {
            stage: "kept".to_owned(),
            deadline: None,
            ..trial
        };
        assert!(!is_open(&kept));
    }

    #[test]
    fn the_unit_is_the_one_the_polkit_rule_allows() {
        let rule = include_str!("../../../deploy/polkit/52-openvideoserver-network.rules");
        assert!(rule.contains(&format!("action.lookup(\"unit\") === \"{UNIT}\"")));
    }
}
