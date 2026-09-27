//! Choosing a GPU.
//!
//! Deliberately separated from the Vulkan calls that gather the facts, so the
//! choice itself is ordinary testable code. A CI runner has no GPU, and a
//! selection rule that is only exercised on a developer's machine is a rule
//! nobody checks.

use std::fmt;

/// What kind of device Vulkan reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DeviceKind {
    /// A separate card. What a media server should be using.
    Discrete,
    /// Built into the CPU. Fine for the Service Console, not for show output.
    Integrated,
    /// A GPU presented by a hypervisor.
    Virtual,
    /// Software rendering. Correct, and far too slow for this.
    Cpu,
    /// Something the driver did not classify.
    Other,
}

impl fmt::Display for DeviceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Discrete => "discrete",
            Self::Integrated => "integrated",
            Self::Virtual => "virtual",
            Self::Cpu => "software",
            Self::Other => "unclassified",
        };
        f.write_str(text)
    }
}

/// What is known about one candidate device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceSummary {
    /// Index in the order Vulkan enumerated them.
    pub index: usize,
    /// Name the driver reports, for example `NVIDIA GeForce RTX 3060 Ti`.
    pub name: String,
    /// What kind of device it is.
    pub kind: DeviceKind,
    /// Vulkan API version, as major, minor, patch.
    pub api_version: (u32, u32, u32),
    /// Total device-local memory in bytes.
    pub device_local_bytes: u64,
    /// A queue family that can do graphics, if there is one.
    pub graphics_family: Option<u32>,
    /// A queue family that can transfer but not do graphics, if there is one.
    ///
    /// The upload path wants this. Sharing the graphics queue means texture
    /// uploads contend with rendering for the same hardware queue, which is
    /// exactly the stall that matters.
    pub dedicated_transfer_family: Option<u32>,
    /// Where the card sits on the bus, for example `0000:01:00.0`.
    ///
    /// `None` on a driver with no `VK_EXT_pci_bus_info`, which software
    /// rasterisers and some virtual devices do not have. It is what names a
    /// connector unambiguously — `DP-1` exists on every card in the machine — so
    /// a device without one cannot have its connectors assigned.
    pub pci_address: Option<String>,
}

impl DeviceSummary {
    /// Returns true when this device could drive show output at all.
    ///
    /// A device with no graphics queue cannot, however impressive it otherwise
    /// looks.
    #[must_use]
    pub const fn is_usable(&self) -> bool {
        self.graphics_family.is_some()
    }

    /// Ranks a device. Higher is better.
    ///
    /// Kind dominates memory: a discrete card with less memory is still the
    /// right choice over an integrated one with more, because the integrated
    /// one's "memory" is system RAM it shares with everything else.
    fn rank(&self) -> (u8, u8, u64) {
        let kind_score = match self.kind {
            DeviceKind::Discrete => 4,
            DeviceKind::Integrated => 3,
            DeviceKind::Virtual => 2,
            DeviceKind::Other => 1,
            DeviceKind::Cpu => 0,
        };
        let transfer_score = u8::from(self.dedicated_transfer_family.is_some());
        (kind_score, transfer_score, self.device_local_bytes)
    }
}

/// Why no device could be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NoDevice {
    /// Vulkan reported no devices at all.
    #[error("no Vulkan device was found")]
    None,
    /// Devices exist, but none can do graphics.
    #[error("none of the {examined} Vulkan devices can drive an output: {names}")]
    NoneUsable {
        /// How many devices were examined.
        examined: usize,
        /// Their names, for the operator.
        names: String,
    },
}

/// Picks the device to render with.
///
/// # Errors
///
/// Returns [`NoDevice`] when nothing suitable was found, naming what was
/// examined so the failure is diagnosable from a log rather than from a rack.
pub fn choose(candidates: &[DeviceSummary]) -> Result<&DeviceSummary, NoDevice> {
    if candidates.is_empty() {
        return Err(NoDevice::None);
    }

    candidates
        .iter()
        .filter(|device| device.is_usable())
        // `max_by_key` returns the last maximum; devices are enumerated in a
        // stable order, so reversing first makes it the first, and the choice
        // repeatable across boots.
        .rev()
        .max_by_key(|device| device.rank())
        .ok_or_else(|| NoDevice::NoneUsable {
            examined: candidates.len(),
            names: candidates
                .iter()
                .map(|device| device.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(index: usize, name: &str, kind: DeviceKind, gigabytes: u64) -> DeviceSummary {
        DeviceSummary {
            index,
            name: name.to_owned(),
            kind,
            api_version: (1, 3, 0),
            device_local_bytes: gigabytes * 1_000_000_000,
            graphics_family: Some(0),
            dedicated_transfer_family: Some(1),
            pci_address: Some(format!("0000:{index:02x}:00.0")),
        }
    }

    #[test]
    fn no_devices_is_reported_as_such() {
        assert_eq!(choose(&[]), Err(NoDevice::None));
    }

    #[test]
    fn a_device_with_no_graphics_queue_cannot_be_chosen() {
        let unusable = DeviceSummary {
            graphics_family: None,
            ..device(0, "Compute Only", DeviceKind::Discrete, 16)
        };
        let error = choose(&[unusable]).expect_err("unusable");
        assert!(
            error.to_string().contains("Compute Only"),
            "the log must name it"
        );
    }

    #[test]
    fn a_discrete_card_beats_an_integrated_one_with_more_memory() {
        // An integrated device's "memory" is system RAM shared with everything
        // else on the machine, so more of it does not make it the better choice.
        let candidates = [
            device(0, "Intel UHD Graphics 770", DeviceKind::Integrated, 32),
            device(1, "NVIDIA GeForce RTX 3060 Ti", DeviceKind::Discrete, 8),
        ];
        assert_eq!(
            choose(&candidates).expect("chooses").name,
            "NVIDIA GeForce RTX 3060 Ti"
        );
    }

    #[test]
    fn software_rendering_is_the_last_resort_but_still_a_resort() {
        // Correct and far too slow, but better than refusing to start: an
        // operator can at least see the system come up and diagnose it.
        let candidates = [device(0, "llvmpipe", DeviceKind::Cpu, 0)];
        assert_eq!(choose(&candidates).expect("chooses").name, "llvmpipe");
    }

    #[test]
    fn a_dedicated_transfer_queue_breaks_a_tie() {
        // Sharing the graphics queue makes uploads contend with rendering.
        let candidates = [
            DeviceSummary {
                dedicated_transfer_family: None,
                ..device(0, "Without", DeviceKind::Discrete, 8)
            },
            device(1, "With", DeviceKind::Discrete, 8),
        ];
        assert_eq!(choose(&candidates).expect("chooses").name, "With");
    }

    #[test]
    fn memory_only_decides_between_otherwise_equal_devices() {
        let candidates = [
            device(0, "Smaller", DeviceKind::Discrete, 8),
            device(1, "Larger", DeviceKind::Discrete, 24),
        ];
        assert_eq!(choose(&candidates).expect("chooses").name, "Larger");
    }

    #[test]
    fn the_choice_is_repeatable_across_boots() {
        // Two identical cards must not swap places between restarts, or an
        // output mapping would follow whichever one Vulkan happened to list
        // first that day.
        let candidates = [
            device(0, "Card A", DeviceKind::Discrete, 8),
            device(1, "Card B", DeviceKind::Discrete, 8),
        ];
        let first = choose(&candidates).expect("chooses");
        for _ in 0..10 {
            assert_eq!(choose(&candidates).expect("chooses"), first);
        }
        assert_eq!(
            first.index, 0,
            "ties resolve to the first enumerated device"
        );
    }

    #[test]
    fn an_unusable_device_is_skipped_rather_than_disqualifying_the_machine() {
        let candidates = [
            DeviceSummary {
                graphics_family: None,
                ..device(0, "Compute Only", DeviceKind::Discrete, 48)
            },
            device(1, "NVIDIA GeForce RTX 3060 Ti", DeviceKind::Discrete, 8),
        ];
        assert_eq!(
            choose(&candidates).expect("chooses").name,
            "NVIDIA GeForce RTX 3060 Ti"
        );
    }

    #[test]
    fn device_kinds_read_plainly_in_a_log() {
        assert_eq!(DeviceKind::Discrete.to_string(), "discrete");
        assert_eq!(DeviceKind::Cpu.to_string(), "software");
    }
}
