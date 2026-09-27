//! What the machine itself is doing.
//!
//! The Service Console leads with these: CPU, memory, disk, address, uptime
//!. They are the numbers an operator looks at when a show is
//! misbehaving and nothing in the application layer explains why.
//!
//! # Sampled, not measured on demand
//!
//! CPU utilisation is a difference between two readings, so it cannot be
//! answered by a request that arrives once: asking twice in quick succession
//! would report nonsense, and refreshing inside a request handler would put a
//! platform call on a path that is supposed to be a database read. A background
//! task refreshes; a request reads the last sample and how old it is.
//!
//! # Absent rather than invented
//!
//! A reading the platform does not offer is `None`, not zero. A temperature of
//! `0 °C` on a console looks like a sensor that works and says the machine is
//! freezing; nothing there says nothing is known (house rule §16).

use std::net::IpAddr;
use std::path::Path;
use std::sync::Mutex;
use std::time::Instant;

use sysinfo::{Components, Disks, MemoryRefreshKind, RefreshKind, System};

/// One sample of the host.
#[derive(Debug, Clone, PartialEq)]
pub struct HostSample {
    /// Total CPU utilisation, 0 to 100.
    pub cpu_percent: f32,
    /// CPU package temperature, when the platform offers one.
    pub cpu_temperature_c: Option<f32>,
    /// Memory utilisation, 0 to 100.
    pub ram_percent: f32,
    /// Memory in use, in bytes.
    pub ram_used_bytes: u64,
    /// Memory this machine has, in bytes.
    pub ram_total_bytes: u64,
    /// Bytes used on the filesystem holding the media store.
    pub disk_used_bytes: u64,
    /// Bytes on that filesystem in total.
    pub disk_total_bytes: u64,
    /// The address a client would reach this server on.
    pub ip_address: String,
    /// When this sample was taken.
    pub taken_at: Instant,
}

impl HostSample {
    /// A sample that knows nothing, for before the first refresh.
    #[must_use]
    pub fn unknown() -> Self {
        Self {
            cpu_percent: 0.0,
            cpu_temperature_c: None,
            ram_percent: 0.0,
            ram_used_bytes: 0,
            ram_total_bytes: 0,
            disk_used_bytes: 0,
            disk_total_bytes: 0,
            ip_address: String::new(),
            taken_at: Instant::now(),
        }
    }
}

/// Samples the host on demand from a background task.
#[derive(Debug)]
pub struct HostMonitor {
    system: Mutex<System>,
    components: Mutex<Components>,
    disks: Mutex<Disks>,
    latest: Mutex<HostSample>,
}

impl Default for HostMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl HostMonitor {
    /// Creates a monitor that has not sampled anything yet.
    #[must_use]
    pub fn new() -> Self {
        let refreshes = RefreshKind::nothing()
            .with_cpu(sysinfo::CpuRefreshKind::nothing().with_cpu_usage())
            .with_memory(MemoryRefreshKind::nothing().with_ram());

        Self {
            system: Mutex::new(System::new_with_specifics(refreshes)),
            components: Mutex::new(Components::new_with_refreshed_list()),
            disks: Mutex::new(Disks::new_with_refreshed_list()),
            latest: Mutex::new(HostSample::unknown()),
        }
    }

    /// Takes a sample.
    ///
    /// `media_root` decides which filesystem is reported: what an operator wants
    /// to know is whether there is room for another show, which is a question
    /// about the disk the media is on and not about the one the system is on.
    pub fn refresh(&self, media_root: Option<&Path>) {
        let (cpu_percent, ram_percent, ram_used_bytes, ram_total_bytes) = {
            let mut system = self
                .system
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            system.refresh_cpu_usage();
            system.refresh_memory();

            let total = system.total_memory();
            let used = system.used_memory();
            let ram = if total == 0 {
                0.0
            } else {
                (used as f64 / total as f64 * 100.0) as f32
            };
            (system.global_cpu_usage(), ram, used, total)
        };

        let cpu_temperature_c = {
            let mut components = self
                .components
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            components.refresh(false);
            package_temperature(&components)
        };

        let (disk_used_bytes, disk_total_bytes) = {
            let mut disks = self
                .disks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            disks.refresh(false);
            media_disk(&disks, media_root)
        };

        let sample = HostSample {
            cpu_percent,
            cpu_temperature_c,
            ram_percent,
            ram_used_bytes,
            ram_total_bytes,
            disk_used_bytes,
            disk_total_bytes,
            ip_address: primary_address(),
            taken_at: Instant::now(),
        };

        let mut latest = self
            .latest
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *latest = sample;
    }

    /// Returns the bytes still free on the filesystem holding the media store.
    ///
    /// Read fresh rather than taken from the last sample. The question this
    /// answers is whether a file about to be sent will fit, and a periodic
    /// sample is minutes old — long enough for another upload to have taken the
    /// room. Only the disks are refreshed, so this costs far less than a full
    /// sample. Without a media root there is no filesystem to ask about, and
    /// nothing is reported rather than a number about the wrong disk.
    pub fn available_bytes(&self, media_root: Option<&Path>) -> Option<u64> {
        let mut disks = self
            .disks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        disks.refresh(false);
        media_disk_free(&disks, media_root?)
    }

    /// Returns the last sample.
    #[must_use]
    pub fn latest(&self) -> HostSample {
        self.latest
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

/// Returns the CPU package temperature, if one of the sensors is it.
///
/// Sensor naming is a platform's own business, so this looks for the ones that
/// conventionally mean "the processor" and reports nothing rather than picking
/// the hottest thing in the machine — which on a media server is the GPU.
fn package_temperature(components: &Components) -> Option<f32> {
    const NAMES: [&str; 4] = ["package id 0", "cpu", "tctl", "k10temp"];

    components
        .list()
        .iter()
        .find(|component| {
            let label = component.label().to_ascii_lowercase();
            NAMES.iter().any(|name| label.contains(name))
        })
        .and_then(sysinfo::Component::temperature)
}

/// Returns used and total bytes of the filesystem holding `media_root`.
///
/// Without a media root — a server that cannot store media — this reports
/// nothing rather than picking a disk at random.
fn media_disk(disks: &Disks, media_root: Option<&Path>) -> (u64, u64) {
    let Some(root) = media_root else {
        return (0, 0);
    };

    holding(disks, root).map_or((0, 0), |disk| {
        let total = disk.total_space();
        (total.saturating_sub(disk.available_space()), total)
    })
}

/// Returns the free bytes of the filesystem holding `media_root`.
///
/// Reported as the space actually available rather than as total minus used: a
/// filesystem keeps blocks in reserve that the used figure counts as free, and
/// an upload sized against them would fail on its last chunk.
fn media_disk_free(disks: &Disks, media_root: &Path) -> Option<u64> {
    holding(disks, media_root).map(sysinfo::Disk::available_space)
}

/// Returns the disk whose filesystem holds `path`.
///
/// Matched by the longest mount point that is a prefix of the path, which is how
/// a filesystem is identified when several are mounted inside one another. The
/// store's own directory may not exist yet, and a path that is not there still
/// belongs to whichever filesystem would hold it.
fn holding<'a>(disks: &'a Disks, path: &Path) -> Option<&'a sysinfo::Disk> {
    disks
        .list()
        .iter()
        .filter(|disk| path.starts_with(disk.mount_point()))
        .max_by_key(|disk| disk.mount_point().as_os_str().len())
}

/// Returns the address a client would reach this server on.
///
/// The first non-loopback address of the first interface that has one. A server
/// with several is a server whose console shows one of them; which one is a
/// question for the network settings this does not have yet, and showing
/// nothing would help nobody find the machine.
fn primary_address() -> String {
    let Ok(interfaces) = local_addresses() else {
        return String::new();
    };

    interfaces
        .into_iter()
        .find(|address| !address.is_loopback() && !is_link_local(*address))
        .map_or_else(String::new, |address| address.to_string())
}

/// Returns this machine's addresses.
///
/// By asking the routing table where a packet to a public address would leave
/// from. No packet is sent — a UDP socket that is connected but never written to
/// does not put anything on the wire — and this needs no platform-specific
/// interface enumeration.
fn local_addresses() -> std::io::Result<Vec<IpAddr>> {
    use std::net::UdpSocket;

    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket.connect("192.0.2.1:9")?;
    Ok(vec![socket.local_addr()?.ip()])
}

/// Returns true for the addresses a machine gives itself when nothing else worked.
fn is_link_local(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => address.is_link_local() || address.is_unspecified(),
        IpAddr::V6(address) => address.is_unspecified(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_monitor_that_has_not_sampled_yet_knows_nothing() {
        // Zero for a percentage is a reading; `None` for a temperature is the
        // absence of one. A console must be able to tell them apart.
        let monitor = HostMonitor::new();
        let sample = monitor.latest();

        assert_eq!(sample.cpu_temperature_c, None);
        assert_eq!(sample.disk_total_bytes, 0);
        assert!(sample.ip_address.is_empty());
    }

    #[test]
    fn a_sample_reports_memory_this_machine_actually_has() {
        let monitor = HostMonitor::new();
        monitor.refresh(None);
        let sample = monitor.latest();

        assert!(
            (0.0..=100.0).contains(&sample.ram_percent),
            "got {}",
            sample.ram_percent
        );
        assert!(sample.ram_percent > 0.0, "a running machine uses memory");
        assert!((0.0..=100.0).contains(&sample.cpu_percent));
        assert!(sample.ram_total_bytes > 0, "the machine has memory");
        assert!(sample.ram_used_bytes <= sample.ram_total_bytes);
    }

    #[test]
    fn without_a_media_root_no_disk_is_reported() {
        // Picking one at random would put a number on a console that means
        // nothing, which is worse than a blank.
        let monitor = HostMonitor::new();
        monitor.refresh(None);

        assert_eq!(monitor.latest().disk_total_bytes, 0);
        assert_eq!(monitor.latest().disk_used_bytes, 0);
    }

    #[test]
    fn the_disk_holding_the_media_is_the_one_reported() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let monitor = HostMonitor::new();
        monitor.refresh(Some(directory.path()));
        let sample = monitor.latest();

        assert!(sample.disk_total_bytes > 0, "the machine has a disk");
        assert!(sample.disk_used_bytes <= sample.disk_total_bytes);
    }

    #[test]
    fn a_sample_carries_an_address_a_client_could_use() {
        let monitor = HostMonitor::new();
        monitor.refresh(None);
        let address = monitor.latest().ip_address;

        // A machine with no network at all is a legitimate state, so an empty
        // answer is allowed; what is not allowed is loopback, which would send
        // an operator looking for the server on their own laptop.
        if !address.is_empty() {
            assert_ne!(address, "127.0.0.1");
            assert_ne!(address, "::1");
        }
    }

    #[test]
    fn refreshing_twice_gives_a_cpu_reading_rather_than_nonsense() {
        // Utilisation is a difference between two readings. The first refresh
        // has nothing to subtract from, which is exactly why this is sampled in
        // the background rather than answered inside a request.
        let monitor = HostMonitor::new();
        monitor.refresh(None);
        std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
        monitor.refresh(None);

        let sample = monitor.latest();
        assert!(
            (0.0..=100.0).contains(&sample.cpu_percent),
            "got {}",
            sample.cpu_percent
        );
    }
}
