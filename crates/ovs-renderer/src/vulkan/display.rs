//! Physical displays, as Vulkan sees them.
//!
//! This is the production output path. A media server
//! owns its connectors: it takes a display in direct mode, sets a mode on it and
//! presents to it, with no compositor between the renderer and the panel. A
//! compositor would insert a buffer, take the frame timing away and make the
//! dropped-frame count a guess.
//!
//! # Why Vulkan's display extensions rather than DRM directly
//!
//! `VK_KHR_display` enumerates the same connectors DRM does, gives their mode
//! lists with exact refresh rates in millihertz, and turns a chosen mode into an
//! ordinary `VkSurfaceKHR`. From there presenting is a swapchain, which is the
//! machinery the renderer already has. Going through DRM would mean exporting
//! images as dma-buf, importing them as framebuffers and driving page flips by
//! hand — more control than this needs, and a great deal more to get wrong.
//!
//! # What this module does not do
//!
//! It does not choose. Which connector carries a show is server configuration
//! and never inferred from what happens to be plugged in, so
//! this reports what exists and the layer above matches it against the show.

use std::ffi::CStr;

use ash::vk;
use ovs_types::geometry::{FrameRate, SizePx};

use super::VulkanError;

/// One mode a display can be set to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayMode {
    /// Resolution in pixels.
    pub size: SizePx,
    /// Refresh rate in millihertz, as the driver reports it.
    ///
    /// Kept in the driver's own units rather than converted: 59 940 mHz is
    /// 60000/1001 exactly, and a float would make the two rates a panel
    /// actually has look like the same one.
    pub refresh_millihertz: u32,
    /// The handle to hand back when creating a surface.
    pub handle: vk::DisplayModeKHR,
}

impl DisplayMode {
    /// Returns the refresh rate as an exact rational, the way the rest of the
    /// system carries frame rates.
    ///
    /// Not `millihertz / 1000`: a driver reporting 59 940 means 60000/1001, the
    /// rate every piece of broadcast equipment in the building calls 59.94, and
    /// 59940/1000 is a different rate that rounds to the same three decimals.
    /// [`FrameRate::from_millihertz`] knows which is which, and is the one place
    /// that knows it.
    #[must_use]
    pub fn refresh(self) -> FrameRate {
        // A mode with no rate is not one a panel can be set to, and the caller
        // has one in hand, so the only honest fallback is what was reported.
        FrameRate::from_millihertz(self.refresh_millihertz)
            .unwrap_or(FrameRate::new(self.refresh_millihertz, 1000))
    }
}

/// One physical display, and everything it can do.
#[derive(Debug, Clone)]
pub struct Display {
    /// The socket, as the driver names it: `HDMI-0`, `DP-3`, `eDP-1`.
    ///
    /// This is what a show forces an output onto, so it must be
    /// the socket and nothing else. NVIDIA reports
    /// `Sony LMD-2110W (HDMI-0)` — the model and the socket in one string — and
    /// keying on the whole of it would mean a monitor swapped between shows no
    /// longer matched the output it is plugged into. [`split_name`] takes the
    /// two apart.
    ///
    /// Empty when the driver offers no name, which is legal and happens. The
    /// index then identifies it, and the layer above says so rather than
    /// inventing one.
    pub connector: String,
    /// What is plugged into that socket, when the driver says: `Sony LMD-2110W`.
    ///
    /// For an operator to read, never to match on. Mesa offers nothing here and
    /// that is not a fault.
    pub model: Option<String>,
    /// Position in this device's enumeration, which is stable across a boot.
    pub index: usize,
    /// The panel's own resolution.
    pub native_size: SizePx,
    /// Physical size in millimetres, when the driver knows it.
    pub physical_size_mm: Option<(u32, u32)>,
    /// Every mode it offers, as reported.
    pub modes: Vec<DisplayMode>,
    /// The handle, for creating a surface on it.
    pub handle: vk::DisplayKHR,
}

/// Splits what a driver calls a display into the socket and the panel.
///
/// NVIDIA writes `Sony LMD-2110W (HDMI-0)`; Mesa writes `monitor`. The socket is
/// what everything matches on, so a name that carries both has to be taken
/// apart, and a name that carries only one is the socket.
#[must_use]
pub fn split_name(raw: &str) -> (Option<String>, String) {
    let raw = raw.trim();

    if raw.ends_with(')') {
        if let Some(open) = raw.rfind('(') {
            let connector = raw[open + 1..raw.len() - 1].trim();
            let model = raw[..open].trim();
            if !connector.is_empty() {
                return (
                    (!model.is_empty()).then(|| model.to_owned()),
                    connector.to_owned(),
                );
            }
        }
    }

    (None, raw.to_owned())
}

impl Display {
    /// Returns what to call this display in a log or a client list.
    ///
    /// The panel and the socket, because an engineer looking at a rack wants
    /// both. Never used as a key.
    #[must_use]
    pub fn label(&self) -> String {
        match &self.model {
            Some(model) => format!("{model} ({})", self.connector),
            None => self.connector.clone(),
        }
    }

    /// Returns the mode matching a size and rate exactly, if there is one.
    ///
    /// Exact because an output's refresh rate is a promise: a show built for
    /// 60000/1001 that quietly ran at 60 would drift by a frame every sixteen
    /// seconds, which is the kind of fault nobody finds until the night.
    #[must_use]
    pub fn mode_for(&self, size: SizePx, refresh: FrameRate) -> Option<DisplayMode> {
        let wanted = refresh.millihertz()?;
        self.modes
            .iter()
            .find(|mode| mode.size == size && mode.refresh_millihertz == wanted)
            .copied()
    }

    /// Asks the driver for a mode this display did not advertise.
    ///
    /// The list a display offers comes from its EDID, and a display that does
    /// not present one -- a switch in the way, a processor that does not pass
    /// it through, a cable -- leaves the driver with a handful of fallback
    /// timings and nothing else. A wall that is genuinely 1920x1080 is then
    /// unreachable through the list, which is what this is for.
    ///
    /// **It is a request, not a setting.** The driver builds the timings and
    /// may refuse: NVIDIA rejects what it thinks the link cannot carry, and a
    /// refusal is reported rather than worked around. Nothing here makes a
    /// panel do something it cannot do; it removes the client's own guess
    /// about what that is.
    ///
    /// # Errors
    ///
    /// Returns the driver's own result when it will not build the mode.
    pub fn force_mode(
        &self,
        entry: &ash::Entry,
        instance: &ash::Instance,
        physical_device: vk::PhysicalDevice,
        size: SizePx,
        refresh: FrameRate,
    ) -> Result<DisplayMode, VulkanError> {
        let millihertz = refresh.millihertz().ok_or(VulkanError::Call {
            operation: "vkCreateDisplayModeKHR",
            result: vk::Result::ERROR_INITIALIZATION_FAILED,
        })?;

        let parameters = vk::DisplayModeParametersKHR::default()
            .visible_region(vk::Extent2D {
                width: size.width,
                height: size.height,
            })
            .refresh_rate(millihertz);
        let create = vk::DisplayModeCreateInfoKHR::default().parameters(parameters);

        let display_instance = ash::khr::display::Instance::new(entry, instance);
        // SAFETY: `self.handle` came from this device's enumeration, and the
        // create info is fully written above.
        let handle = unsafe {
            display_instance.create_display_mode(physical_device, self.handle, &create, None)
        }
        .map_err(|result| VulkanError::Call {
            operation: "vkCreateDisplayModeKHR",
            result,
        })?;

        Ok(DisplayMode {
            size,
            refresh_millihertz: millihertz,
            handle,
        })
    }

    /// Returns the mode to use when a show names none.
    ///
    /// The largest resolution the panel offers, at **60 Hz** if it has it, and
    /// at its highest rate otherwise. A panel's own resolution is almost always
    /// the one an operator means; the rate is not, and defaulting to the fastest
    /// is a desktop instinct that does not belong here. 60 is what this product
    /// is built and measured against, it is what media is
    /// authored at, and a wall asked to run faster than the machine can compose
    /// for it drops frames to no purpose — which is exactly what the test server
    /// did at 165 Hz until it was asked for 60.
    ///
    /// A show that wants something else says so, and gets it or a refusal.
    #[must_use]
    pub fn preferred_mode(&self) -> Option<DisplayMode> {
        /// What the reference workload runs at.
        const REFERENCE_MILLIHERTZ: u32 = 60_000;

        self.modes.iter().copied().max_by_key(|mode| {
            (
                u64::from(mode.size.width) * u64::from(mode.size.height),
                u8::from(mode.refresh_millihertz == REFERENCE_MILLIHERTZ),
                mode.refresh_millihertz,
            )
        })
    }
}

/// Reads the displays one physical device is wired to.
///
/// Returns an empty list — not an error — when the device has no connectors or
/// the loader does not offer `VK_KHR_display`. A render-only GPU is a legitimate
/// thing for a machine to have, and so is a machine with no display extensions
/// at all; both are "nothing to present on" rather than a failure.
///
/// # Errors
///
/// Returns [`VulkanError::Call`] when the driver refuses an enumeration it
/// accepted the existence of.
pub fn enumerate(
    entry: &ash::Entry,
    instance: &ash::Instance,
    physical_device: vk::PhysicalDevice,
) -> Result<Vec<Display>, VulkanError> {
    let display_instance = ash::khr::display::Instance::new(entry, instance);

    // SAFETY: the physical device belongs to this instance, and the extension
    // was enabled at instance creation or this call is not reached.
    let properties =
        match unsafe { display_instance.get_physical_device_display_properties(physical_device) } {
            Ok(properties) => properties,
            // A driver that has the entry point but no displays answers this way
            // rather than with an empty list.
            Err(vk::Result::ERROR_INITIALIZATION_FAILED) => return Ok(Vec::new()),
            Err(result) => {
                return Err(VulkanError::Call {
                    operation: "vkGetPhysicalDeviceDisplayPropertiesKHR",
                    result,
                })
            }
        };

    let mut displays = Vec::with_capacity(properties.len());
    for (index, property) in properties.iter().enumerate() {
        // SAFETY: the driver guarantees a NUL-terminated string or null.
        let name = if property.display_name.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(property.display_name) }
                .to_string_lossy()
                .into_owned()
        };

        // SAFETY: `property.display` came from the enumeration above.
        let modes = unsafe {
            display_instance.get_display_mode_properties(physical_device, property.display)
        }
        .map_err(|result| VulkanError::Call {
            operation: "vkGetDisplayModePropertiesKHR",
            result,
        })?;

        let (model, connector) = split_name(&name);

        let physical = property.physical_dimensions;
        displays.push(Display {
            connector,
            model,
            index,
            native_size: SizePx::new(
                property.physical_resolution.width,
                property.physical_resolution.height,
            ),
            physical_size_mm: (physical.width > 0 && physical.height > 0)
                .then_some((physical.width, physical.height)),
            modes: modes
                .iter()
                .map(|mode| DisplayMode {
                    size: SizePx::new(
                        mode.parameters.visible_region.width,
                        mode.parameters.visible_region.height,
                    ),
                    refresh_millihertz: mode.parameters.refresh_rate,
                    handle: mode.display_mode,
                })
                .collect(),
            handle: property.display,
        });
    }

    Ok(displays)
}

/// Returns which of these displays a set of roles says carries a show.
///
/// Order matters and is the operator's: the connectors are returned in the order
/// their names were given, so "DP-3, DP-4" means the first output is DP-3
/// whatever the driver enumerated first.
///
/// A connector with no role is **not** a show output. An unassigned socket
/// staying dark is the safe direction; the other way round puts a show on the
/// Service Display, which is the one screen an operator is looking at.
#[must_use]
pub fn media_outputs<'a>(
    displays: &'a [Display],
    gpu_pci_address: &str,
    roles: &std::collections::BTreeMap<String, ovs_types::config::ConnectorRole>,
) -> Vec<&'a Display> {
    let mut chosen: Vec<&Display> = displays
        .iter()
        .filter(|display| {
            let key = format!("{gpu_pci_address}/{}", display.connector);
            matches!(
                roles.get(&key),
                Some(ovs_types::config::ConnectorRole::MediaOutput)
            )
        })
        .collect();

    // By name, so the same machine produces the same order every boot. The
    // driver's enumeration order is not promised to be stable and a wall that
    // swapped with the one beside it after a reboot would be a long evening.
    chosen.sort_by(|left, right| left.connector.cmp(&right.connector));
    chosen
}

/// Returns which of these displays a set of roles says is the System screen.
///
/// At most one, because there is one console. If more than one connector is
/// marked — which the server should not allow, and which a hand-edited
/// configuration file can still produce — the first by name is taken and the
/// rest are left dark rather than each being given a copy: two panels showing
/// the same console is the failure this whole arrangement exists to end.
///
/// A connector with no role is not the System screen, for the mirror of the
/// reason in [`media_outputs`]: an unassigned socket stays dark.
#[must_use]
pub fn service_display<'a>(
    displays: &'a [Display],
    gpu_pci_address: &str,
    roles: &std::collections::BTreeMap<String, ovs_types::config::ConnectorRole>,
) -> Option<&'a Display> {
    let mut marked: Vec<&Display> = displays
        .iter()
        .filter(|display| {
            let key = format!("{gpu_pci_address}/{}", display.connector);
            matches!(
                roles.get(&key),
                Some(ovs_types::config::ConnectorRole::ServiceDisplay)
            )
        })
        .collect();

    marked.sort_by(|left, right| left.connector.cmp(&right.connector));
    if marked.len() > 1 {
        tracing::warn!(
            marked = marked.len(),
            taking = marked[0].connector.as_str(),
            "more than one connector is marked as the System screen"
        );
    }
    marked.first().copied()
}

/// What one display plane is, for reporting.
#[derive(Debug, Clone)]
pub struct PlaneReport {
    /// Its index, which is what a surface names.
    pub index: u32,
    /// The display it is currently driving, if the driver says it is driving one.
    pub current: Option<vk::DisplayKHR>,
    /// Where in the stack it sits on that display.
    pub current_stack_index: u32,
    /// Every display it is able to drive.
    pub supports: Vec<vk::DisplayKHR>,
}

/// Reads every display plane this device has.
///
/// **For `--displays`, and for finding out why two outputs landed on one panel.**
/// A plane is the thing a surface is actually created on, and nothing in this
/// renderer said anything about them until two outputs needed two.
///
/// # Errors
///
/// Returns the driver's own result when the planes cannot be enumerated.
pub fn planes(
    entry: &ash::Entry,
    instance: &ash::Instance,
    physical_device: vk::PhysicalDevice,
) -> Result<Vec<PlaneReport>, VulkanError> {
    let display_instance = ash::khr::display::Instance::new(entry, instance);

    // SAFETY: the physical device belongs to this instance.
    let properties =
        unsafe { display_instance.get_physical_device_display_plane_properties(physical_device) }
            .map_err(|result| VulkanError::Call {
            operation: "vkGetPhysicalDeviceDisplayPlanePropertiesKHR",
            result,
        })?;

    let mut reports = Vec::with_capacity(properties.len());
    for (index, plane) in properties.iter().enumerate() {
        let index = u32::try_from(index).unwrap_or(u32::MAX);

        // SAFETY: `index` is within the enumeration above.
        let supports = unsafe {
            display_instance.get_display_plane_supported_displays(physical_device, index)
        }
        .map_err(|result| VulkanError::Call {
            operation: "vkGetDisplayPlaneSupportedDisplaysKHR",
            result,
        })?;

        reports.push(PlaneReport {
            index,
            current: (plane.current_display != vk::DisplayKHR::null())
                .then_some(plane.current_display),
            current_stack_index: plane.current_stack_index,
            supports,
        });
    }

    Ok(reports)
}

/// What one plane can do with one mode.
///
/// From `vkGetDisplayPlaneCapabilitiesKHR`, which is the precise question:
/// `vkGetDisplayPlaneSupportedDisplaysKHR` says which displays a plane is wired
/// to and on this card every plane says every display, which decides nothing.
#[derive(Debug, Clone, Copy)]
pub struct PlaneFit {
    /// The largest image this plane can show of that mode.
    pub max: (u32, u32),
    /// The smallest.
    pub min: (u32, u32),
    /// Which alpha modes it offers, as the driver reports them.
    pub alpha: vk::DisplayPlaneAlphaFlagsKHR,
}

impl PlaneFit {
    /// Whether this plane can show a picture of `size` at all.
    #[must_use]
    pub const fn fits(&self, size: SizePx) -> bool {
        size.width >= self.min.0
            && size.height >= self.min.1
            && size.width <= self.max.0
            && size.height <= self.max.1
    }
}

/// Asks what `plane` can do with `mode`.
///
/// # Errors
///
/// Returns the driver's own result. A plane that cannot be used with this mode
/// at all is a refusal here rather than a surface that lands somewhere else.
pub fn plane_fit(
    entry: &ash::Entry,
    instance: &ash::Instance,
    physical_device: vk::PhysicalDevice,
    mode: vk::DisplayModeKHR,
    plane: u32,
) -> Result<PlaneFit, VulkanError> {
    let display_instance = ash::khr::display::Instance::new(entry, instance);

    // SAFETY: the mode and the plane index both came from this device's own
    // enumeration.
    let capabilities =
        unsafe { display_instance.get_display_plane_capabilities(physical_device, mode, plane) }
            .map_err(|result| VulkanError::Call {
                operation: "vkGetDisplayPlaneCapabilitiesKHR",
                result,
            })?;

    Ok(PlaneFit {
        max: (
            capabilities.max_dst_extent.width,
            capabilities.max_dst_extent.height,
        ),
        min: (
            capabilities.min_dst_extent.width,
            capabilities.min_dst_extent.height,
        ),
        alpha: capabilities.supported_alpha,
    })
}

/// Returns the first plane that can drive `display`, and how many layers it
/// stacks.
///
/// A display is driven through a plane, and which planes can reach which
/// displays is the hardware's business. Picking the first that can is right for
/// a media server: the extra planes are for overlays and cursors, and this
/// renderer composites everything itself before it gets here.
///
/// # Errors
///
/// Returns [`VulkanError::Call`] when a plane's displays cannot be read, and
/// [`VulkanError::NoDisplayPlane`] when nothing can drive this display.
pub fn plane_for(
    entry: &ash::Entry,
    instance: &ash::Instance,
    physical_device: vk::PhysicalDevice,
    display: vk::DisplayKHR,
    taken: &std::collections::BTreeSet<u32>,
) -> Result<u32, VulkanError> {
    let display_instance = ash::khr::display::Instance::new(entry, instance);

    // SAFETY: the physical device belongs to this instance.
    let planes =
        unsafe { display_instance.get_physical_device_display_plane_properties(physical_device) }
            .map_err(|result| VulkanError::Call {
            operation: "vkGetPhysicalDeviceDisplayPlanePropertiesKHR",
            result,
        })?;

    for (index, plane) in planes.iter().enumerate() {
        let index = u32::try_from(index).unwrap_or(u32::MAX);

        // **Already handed out by this renderer.** Checked before the driver's
        // own answer, because the driver's arrives too late to be useful: a
        // plane reports `current_display` once it is actually driving one, and
        // two outputs built a second apart both read every plane as free.
        if taken.contains(&index) {
            continue;
        }

        // And a plane the driver says is bound to another display — which
        // catches a plane something outside this renderer is using.
        if plane.current_display != vk::DisplayKHR::null() && plane.current_display != display {
            continue;
        }

        // SAFETY: `index` is within the enumeration above.
        let supported = unsafe {
            display_instance.get_display_plane_supported_displays(physical_device, index)
        }
        .map_err(|result| VulkanError::Call {
            operation: "vkGetDisplayPlaneSupportedDisplaysKHR",
            result,
        })?;

        if supported.contains(&display) {
            return Ok(index);
        }
    }

    Err(VulkanError::NoDisplayPlane)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(width: u32, height: u32, millihertz: u32) -> DisplayMode {
        DisplayMode {
            size: SizePx::new(width, height),
            refresh_millihertz: millihertz,
            handle: vk::DisplayModeKHR::null(),
        }
    }

    /// A display on a named socket, with one ordinary mode.
    fn named(connector: &str) -> Display {
        Display {
            connector: connector.to_owned(),
            ..display(vec![mode(1920, 1080, 60_000)])
        }
    }

    fn display(modes: Vec<DisplayMode>) -> Display {
        Display {
            connector: "DP-1".to_owned(),
            model: None,
            index: 0,
            native_size: SizePx::new(1920, 1080),
            physical_size_mm: Some((600, 340)),
            modes,
            handle: vk::DisplayKHR::null(),
        }
    }

    #[test]
    fn a_driver_that_names_the_panel_and_the_socket_is_taken_apart() {
        // What NVIDIA reports. Keying on the whole string would mean a monitor
        // swapped for another between shows no longer matched the output it is
        // plugged into, which is the opposite of forcing an output onto a
        // socket.
        let (model, connector) = split_name("Sony LMD-2110W (HDMI-0)");

        assert_eq!(model.as_deref(), Some("Sony LMD-2110W"));
        assert_eq!(connector, "HDMI-0");
    }

    #[test]
    fn a_driver_that_names_only_the_socket_has_named_the_socket() {
        // What Mesa reports, and it is not a fault: there is no panel name to
        // be had, and the socket is the part that matters.
        let (model, connector) = split_name("monitor");

        assert_eq!(model, None);
        assert_eq!(connector, "monitor");
    }

    #[test]
    fn a_name_that_is_only_brackets_is_left_alone() {
        // Nothing to gain by inventing a socket called nothing.
        let (model, connector) = split_name("()");

        assert_eq!(model, None);
        assert_eq!(connector, "()");
    }

    #[test]
    fn what_an_operator_reads_is_both() {
        let panel = Display {
            model: Some("Sony LMD-2110W".to_owned()),
            ..named("HDMI-0")
        };

        assert_eq!(panel.label(), "Sony LMD-2110W (HDMI-0)");
        assert_eq!(named("HDMI-0").label(), "HDMI-0");
    }

    #[test]
    fn a_mode_is_matched_on_the_rate_as_well_as_the_size() {
        // The whole reason the rate is carried exactly. A panel offering both
        // 60 and 59.94 offers two different shows.
        let panel = display(vec![
            mode(1920, 1080, 60_000),
            mode(1920, 1080, 59_940),
            mode(1280, 720, 60_000),
        ]);

        let exact = panel
            .mode_for(SizePx::new(1920, 1080), FrameRate::FPS_59_94)
            .expect("the panel has it");
        assert_eq!(exact.refresh_millihertz, 59_940);

        let other = panel
            .mode_for(SizePx::new(1920, 1080), FrameRate::FPS_60)
            .expect("and the other one");
        assert_eq!(other.refresh_millihertz, 60_000);
    }

    #[test]
    fn a_mode_the_panel_does_not_have_is_not_almost_matched() {
        // Better a refusal an operator can read than a wall running at a rate
        // nobody asked for.
        let panel = display(vec![mode(1920, 1080, 60_000)]);

        assert_eq!(
            panel.mode_for(SizePx::new(3840, 2160), FrameRate::FPS_60),
            None
        );
        assert_eq!(
            panel.mode_for(SizePx::new(1920, 1080), FrameRate::FPS_59_94),
            None
        );
    }

    #[test]
    fn the_preferred_mode_is_the_biggest_one_at_sixty() {
        let panel = display(vec![
            mode(1280, 720, 60_000),
            mode(1920, 1080, 30_000),
            mode(1920, 1080, 60_000),
        ]);

        let preferred = panel.preferred_mode().expect("a mode");
        assert_eq!(preferred.size, SizePx::new(1920, 1080));
        assert_eq!(preferred.refresh_millihertz, 60_000);
    }

    #[test]
    fn a_faster_panel_is_still_run_at_sixty() {
        // The test server's internal panel offers 165 Hz and 60. At 165 the
        // machine composed 131 frames a second and dropped the rest; at 60 it
        // held 60.00 with nothing dropped. Fastest is a desktop default.
        let panel = display(vec![mode(2560, 1440, 165_003), mode(2560, 1440, 60_000)]);

        assert_eq!(
            panel.preferred_mode().expect("a mode").refresh_millihertz,
            60_000
        );
    }

    #[test]
    fn a_panel_with_no_sixty_runs_at_its_fastest() {
        // Some processors offer 50 and nothing else, and some offer 59.94.
        // Refusing to drive them would be worse than driving them at what they
        // have.
        let panel = display(vec![mode(1920, 1080, 50_000), mode(1920, 1080, 59_940)]);

        assert_eq!(
            panel.preferred_mode().expect("a mode").refresh_millihertz,
            59_940
        );
    }

    #[test]
    fn a_display_with_no_modes_prefers_nothing() {
        // It happens: a connector with nothing plugged in enumerates and offers
        // no modes. Inventing one would be inventing a wall.
        assert_eq!(display(Vec::new()).preferred_mode(), None);
    }

    #[test]
    fn millihertz_becomes_the_rate_the_rest_of_the_system_uses() {
        assert_eq!(mode(1920, 1080, 60_000).refresh().as_f64(), 60.0);
        // Not 59940/1000. What a driver means by 59 940 is the rate the rest
        // of the building calls 59.94, and the two differ by a frame every
        // four and a half hours.
        assert_eq!(
            mode(1920, 1080, 59_940).refresh(),
            FrameRate::new(60_000, 1001)
        );
        assert_eq!(
            mode(1920, 1080, 59_940).refresh().millihertz(),
            Some(59_940),
            "and it goes back to the number the driver reported"
        );
    }

    #[test]
    fn only_the_connectors_marked_as_media_outputs_carry_a_show() {
        // The one that matters: the Service Display's socket is not a show
        // output, and a renderer that took it would put a show on the screen an
        // operator is looking at and the console nowhere.
        let mut roles = std::collections::BTreeMap::new();
        roles.insert(
            "0000:00:02.0/eDP-1".to_owned(),
            ovs_types::config::ConnectorRole::ServiceDisplay,
        );
        roles.insert(
            "0000:00:02.0/DP-3".to_owned(),
            ovs_types::config::ConnectorRole::MediaOutput,
        );

        let displays = vec![named("eDP-1"), named("DP-3"), named("HDMI-A-1")];
        let chosen = media_outputs(&displays, "0000:00:02.0", &roles);

        assert_eq!(chosen.len(), 1);
        assert_eq!(chosen[0].connector, "DP-3");
    }

    #[test]
    fn a_connector_nobody_has_decided_about_is_left_alone() {
        // "Nothing has been said" is not "use it". An operator plugging a
        // monitor in to check something must not find a show on it.
        let displays = vec![named("DP-3")];
        let chosen = media_outputs(
            &displays,
            "0000:00:02.0",
            &std::collections::BTreeMap::new(),
        );

        assert!(chosen.is_empty());
    }

    #[test]
    fn the_same_connector_name_on_another_gpu_is_a_different_socket() {
        let mut roles = std::collections::BTreeMap::new();
        roles.insert(
            "0000:01:00.0/DP-1".to_owned(),
            ovs_types::config::ConnectorRole::MediaOutput,
        );

        let displays = vec![named("DP-1")];
        assert!(media_outputs(&displays, "0000:00:02.0", &roles).is_empty());
        assert_eq!(media_outputs(&displays, "0000:01:00.0", &roles).len(), 1);
    }

    #[test]
    fn media_outputs_come_back_in_the_same_order_every_boot() {
        // The driver's enumeration order is not promised to be stable, and two
        // walls that swapped places after a reboot would be a long evening.
        let mut roles = std::collections::BTreeMap::new();
        for connector in ["DP-4", "DP-3"] {
            roles.insert(
                format!("0000:00:02.0/{connector}"),
                ovs_types::config::ConnectorRole::MediaOutput,
            );
        }

        let displays = vec![named("DP-4"), named("DP-3")];
        let chosen = media_outputs(&displays, "0000:00:02.0", &roles);

        assert_eq!(
            chosen
                .iter()
                .map(|display| display.connector.as_str())
                .collect::<Vec<_>>(),
            vec!["DP-3", "DP-4"]
        );
    }
}
