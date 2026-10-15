//! Vulkan initialisation.
//!
//! This module is the one place in OpenVideoServer where `unsafe` is permitted
//!. Every Vulkan call is `unsafe` by construction, so forbidding it
//! here would only mean pushing the GPU into a separate language. Everywhere
//! else in the workspace forbids it.
//!
//! What lives here is only the part that talks to the driver. Which device to
//! use is decided in [`selection`], which is ordinary code with ordinary tests,
//! because a CI runner has no GPU and a rule that is only exercised on a
//! developer's machine is a rule nobody checks.

#![allow(
    unsafe_code,
    reason = "every Vulkan entry point is unsafe; this module is the boundary"
)]

mod blend;
pub mod display;
pub mod generated;
pub mod offscreen;
pub mod present;
pub mod selection;
pub mod texture;

use std::ffi::{c_char, CStr, CString};

use ash::vk;

/// The instance extensions needed to reach a physical display.
///
/// All or nothing: a loader with `VK_KHR_display` but not `VK_KHR_surface`
/// cannot turn a display into something presentable, and reporting "displays
/// are available" in that state would be a promise this cannot keep.
const DISPLAY_INSTANCE_EXTENSIONS: [&CStr; 2] = [ash::khr::surface::NAME, ash::khr::display::NAME];

/// The instance extensions that let a display be taken from the kernel.
///
/// **Not all or nothing, and not optional in practice.** `VK_KHR_display` lets a
/// surface be created for a display; it does not hand the display over. While
/// the kernel is still DRM master of that connector, what a surface created for
/// it does is undefined — and on the driver this was written against, every
/// surface lands on whichever display the driver already holds, whatever mode it
/// was created with.
///
/// Measured on 2026-09-26: eight surfaces on two monitors, four of them created
/// with the second monitor's own mode, and all eight appeared on the first
/// panel. A single output had always worked because there was only ever one to
/// get wrong.
///
/// A loader without them is left to the old behaviour rather than refused: one
/// output is what it could always do, and refusing to start would turn a
/// working single-wall machine into a dark one.
const ACQUIRE_INSTANCE_EXTENSIONS: [&CStr; 2] = [
    ash::ext::direct_mode_display::NAME,
    ash::ext::acquire_drm_display::NAME,
];

/// Where the kernel puts the card nodes, keyed by what a GPU is rather than by
/// the order its driver bound in.
///
/// `/dev/dri/card0` does not do: which card is zero depends on which driver
/// bound first, and installing a second one is enough to swap them.
const DRM_BY_PATH: &str = "/dev/dri/by-path";

pub use offscreen::{OffscreenTarget, VulkanOffscreenBackend};
pub use selection::{choose, DeviceKind, DeviceSummary, NoDevice};
pub use texture::Texture;

/// Why Vulkan could not be brought up.
#[derive(Debug, thiserror::Error)]
pub enum VulkanError {
    /// The Vulkan loader is not installed.
    ///
    /// On a rack server this means the graphics driver is missing, which is a
    /// deployment problem rather than a bug, and the message says so.
    #[error("no Vulkan loader is installed on this machine: {0}")]
    NoLoader(String),

    /// A Vulkan call failed.
    #[error("{operation} failed: {result:?}")]
    Call {
        /// What was being attempted.
        operation: &'static str,
        /// The Vulkan result code.
        result: vk::Result,
    },

    /// No device could be used.
    #[error(transparent)]
    NoDevice(#[from] NoDevice),

    /// A string the driver returned was not valid UTF-8.
    #[error("the driver returned an unreadable device name")]
    UnreadableName,
    /// No plane on this device can drive the display that was asked for.
    #[error("no display plane can drive that display")]
    NoDisplayPlane,
    /// The machine's Vulkan offers no way to reach a physical display.
    #[error("this Vulkan build has no display extensions: {0}")]
    NoDisplaySupport(String),
    /// A device was named and this machine does not have it.
    #[error("no GPU matches {wanted:?}; this machine has: {found}")]
    NoSuchDevice {
        /// What was asked for.
        wanted: String,
        /// What is actually here.
        found: String,
    },
}

/// Returns the device a name or index asks for.
///
/// Matched by index first, then by a case-insensitive substring of the name, so
/// `OVS_RENDERER_DEVICE=nvidia` finds the card without anybody having to type
/// what the driver calls it in full.
fn pick_named<'a>(
    candidates: &'a [DeviceSummary],
    wanted: &str,
) -> Result<&'a DeviceSummary, VulkanError> {
    let trimmed = wanted.trim();

    if let Ok(index) = trimmed.parse::<usize>() {
        if let Some(device) = candidates.iter().find(|device| device.index == index) {
            return Ok(device);
        }
    }

    let lowered = trimmed.to_lowercase();
    candidates
        .iter()
        .find(|device| device.name.to_lowercase().contains(&lowered))
        .ok_or_else(|| VulkanError::NoSuchDevice {
            wanted: trimmed.to_owned(),
            found: candidates
                .iter()
                .map(|device| format!("{}: {}", device.index, device.name))
                .collect::<Vec<_>>()
                .join(", "),
        })
}

/// Returns where a device sits on the PCI bus, in the form Linux writes it.
///
/// `0000:01:00.0`: domain, bus, device, function — the same string
/// `/sys/class/drm/card0/device` resolves to, and the same one `lspci -D`
/// prints. Matching that spelling matters: it is what an operator reads off a
/// machine, and what a support instruction names.
///
/// `None` when the driver does not offer `VK_EXT_pci_bus_info`. Software
/// rasterisers do not, and neither do some virtual devices; both are legitimate
/// and neither has connectors to assign.
fn pci_address_of(instance: &ash::Instance, device: vk::PhysicalDevice) -> Option<String> {
    let available = available_extensions(instance, device);
    if !available
        .iter()
        .any(|have| have == ash::ext::pci_bus_info::NAME)
    {
        return None;
    }

    let mut bus = vk::PhysicalDevicePCIBusInfoPropertiesEXT::default();
    let mut properties = vk::PhysicalDeviceProperties2::default().push_next(&mut bus);
    // SAFETY: the device came from this instance's enumeration, and the chained
    // structure lives until the call returns.
    unsafe { instance.get_physical_device_properties2(device, &mut properties) };

    Some(format!(
        "{:04x}:{:02x}:{:02x}.{:x}",
        bus.pci_domain, bus.pci_bus, bus.pci_device, bus.pci_function
    ))
}

/// Returns the extension names a device offers.
fn available_extensions(
    instance: &ash::Instance,
    physical_device: vk::PhysicalDevice,
) -> Vec<std::ffi::CString> {
    // SAFETY: the physical device belongs to this instance.
    let Ok(properties) =
        (unsafe { instance.enumerate_device_extension_properties(physical_device) })
    else {
        return Vec::new();
    };

    properties
        .iter()
        .filter_map(|property| property.extension_name_as_c_str().ok())
        .map(std::ffi::CStr::to_owned)
        .collect()
}

/// An initialised Vulkan device, ready to have a renderer built on it.
pub struct VulkanContext {
    /// Kept alive: the instance and device borrow from the loader. Also used
    /// directly, because the display extensions are loaded from it.
    entry: ash::Entry,
    instance: ash::Instance,
    device: ash::Device,
    physical_device: vk::PhysicalDevice,
    graphics_queue: vk::Queue,
    transfer_queue: vk::Queue,
    /// Whether the device can say how much of its memory is in use.
    has_memory_budget: bool,
    /// Whether the device can blend with the advanced operations.
    ///
    /// Two of the eight blend modes are not expressible with core Vulkan
    /// blending. With this they are the hardware's own equations; without it
    /// they draw as Normal and the renderer says so once (see `vulkan::blend`).
    has_advanced_blend: bool,
    /// Whether this machine's Vulkan can reach a physical display at all.
    has_display: bool,
    /// Whether the open device can present to one.
    has_swapchain: bool,
    /// The card node this GPU is, open, for taking displays from the kernel.
    ///
    /// Opened once and kept: `vkAcquireDrmDisplayEXT` wants a file descriptor
    /// for the same device, and opening one per display would be one more thing
    /// to fail halfway through bringing a wall up.
    ///
    /// `None` on a machine with no such node, or one where this account may not
    /// open it — both of which leave the old behaviour, which is one output.
    drm: Option<std::fs::File>,
    /// The displays taken from the kernel and not yet given back.
    ///
    /// By raw handle, because `vk::DisplayKHR` is not ordered. Kept so a display
    /// is taken once rather than once per rebuild, and so every one of them is
    /// given back when this is dropped.
    acquired: std::sync::Mutex<std::collections::BTreeSet<u64>>,
    /// Loaded when the driver offers it; the call that takes a display.
    acquire_drm: Option<ash::ext::acquire_drm_display::Instance>,
    /// And the one that gives it back.
    direct_mode: Option<ash::ext::direct_mode_display::Instance>,
    /// Which display planes this renderer has handed out and not got back.
    ///
    /// **Its own ledger, because the driver's answer arrives too late.** A plane
    /// reports `current_display` once it is actually driving one; two outputs
    /// built a second apart both read every plane as free, both take the first,
    /// and the two pictures land on one panel with the other dark.
    ///
    /// Released when the output holding it is dropped, which is what lets a
    /// connector be given up and taken again.
    taken_planes: std::sync::Mutex<std::collections::BTreeSet<u32>>,
    summary: DeviceSummary,
}

/// What opening a device produced.
struct OpenedDevice {
    device: ash::Device,
    graphics_queue: vk::Queue,
    transfer_queue: vk::Queue,
    has_memory_budget: bool,
    has_advanced_blend: bool,
    has_swapchain: bool,
}

/// Returns true when a device's advanced blending is usable as this renderer
/// needs it.
///
/// Two properties matter. Every advanced operation must be supported, because
/// the two modes this is for are not the ones a reduced set is guaranteed to
/// include; and the operations must be coherent, because the alternative is a
/// pipeline barrier between every pair of overlapping quads, which is the whole
/// composition pass on a busy show.
fn advanced_blend_is_usable(instance: &ash::Instance, physical_device: vk::PhysicalDevice) -> bool {
    let mut advanced = vk::PhysicalDeviceBlendOperationAdvancedPropertiesEXT::default();
    let mut properties = vk::PhysicalDeviceProperties2::default().push_next(&mut advanced);
    // SAFETY: `physical_device` came from this instance, and the chained
    // structure lives until the call returns.
    unsafe { instance.get_physical_device_properties2(physical_device, &mut properties) };

    let mut features = vk::PhysicalDeviceBlendOperationAdvancedFeaturesEXT::default();
    let mut supported = vk::PhysicalDeviceFeatures2::default().push_next(&mut features);
    // SAFETY: as above.
    unsafe { instance.get_physical_device_features2(physical_device, &mut supported) };

    advanced.advanced_blend_all_operations == vk::TRUE
        && advanced.advanced_blend_max_color_attachments >= 1
        && features.advanced_blend_coherent_operations == vk::TRUE
}

impl VulkanContext {
    /// Brings up Vulkan and opens the best available device.
    ///
    /// # Errors
    ///
    /// Returns [`VulkanError::NoLoader`] when Vulkan is not installed, which is
    /// the ordinary case on a machine with no graphics driver, and
    /// [`VulkanError::NoDevice`] when nothing suitable was found.
    pub fn new(application_name: &str) -> Result<Self, VulkanError> {
        Self::open(application_name, None)
    }

    /// Brings up Vulkan and opens a named device, or the best available.
    ///
    /// `wanted` is matched against the device name, case-insensitively and as a
    /// substring, or as an index into the enumeration. A machine with a media
    /// GPU and a service GPU has to be told which is which — inferring it from
    /// which is faster would put a show on whichever card the driver happened to
    /// enumerate first.
    ///
    /// A name that matches nothing is a hard failure rather than a fall back to
    /// the automatic choice. An operator who asked for the NVIDIA card and
    /// silently got the integrated one would have a show running on the wrong
    /// hardware with nothing saying so.
    ///
    /// # Errors
    ///
    /// As [`Self::new`], plus [`VulkanError::NoSuchDevice`] when `wanted`
    /// matches no device on this machine.
    pub fn open(application_name: &str, wanted: Option<&str>) -> Result<Self, VulkanError> {
        // SAFETY: loading the system Vulkan loader. Unsafe because it dlopens a
        // library; there is no way to do this safely, and a missing loader is
        // reported as an error rather than a panic.
        let entry = unsafe { ash::Entry::load() }
            .map_err(|error| VulkanError::NoLoader(error.to_string()))?;

        let (instance, has_display, has_acquire) = Self::create_instance(&entry, application_name)?;
        let candidates = Self::enumerate(&instance)?;

        let chosen = match wanted {
            Some(wanted) => pick_named(&candidates, wanted)?.clone(),
            None => choose(&candidates)?.clone(),
        };
        tracing::info!(
            device = %chosen.name,
            kind = %chosen.kind,
            api = ?chosen.api_version,
            memory_gb = chosen.device_local_bytes / 1_000_000_000,
            dedicated_transfer = chosen.dedicated_transfer_family.is_some(),
            "selected a GPU"
        );

        // SAFETY: `chosen.index` came from the enumeration just above, so it
        // indexes the same list.
        let physical_device = unsafe {
            instance
                .enumerate_physical_devices()
                .map_err(|result| VulkanError::Call {
                    operation: "vkEnumeratePhysicalDevices",
                    result,
                })?[chosen.index]
        };

        let opened = Self::create_device(&instance, physical_device, &chosen)?;

        // Loaded here rather than in the initialiser below: both borrow the
        // entry and the instance, and the struct takes ownership of them.
        //
        // Only when the loader offered them, which `create_instance` decided by
        // name — loading one that was not enabled gives function pointers that
        // are not there.
        let acquire_drm =
            has_acquire.then(|| ash::ext::acquire_drm_display::Instance::new(&entry, &instance));
        let direct_mode =
            has_acquire.then(|| ash::ext::direct_mode_display::Instance::new(&entry, &instance));
        let drm = has_acquire
            .then(|| Self::open_drm(chosen.pci_address.as_deref()))
            .flatten();

        Ok(Self {
            entry,
            instance,
            device: opened.device,
            physical_device,
            has_memory_budget: opened.has_memory_budget,
            has_advanced_blend: opened.has_advanced_blend,
            graphics_queue: opened.graphics_queue,
            transfer_queue: opened.transfer_queue,
            has_display,
            has_swapchain: opened.has_swapchain,
            taken_planes: std::sync::Mutex::new(std::collections::BTreeSet::new()),
            acquired: std::sync::Mutex::new(std::collections::BTreeSet::new()),
            acquire_drm,
            direct_mode,
            drm,
            summary: chosen,
        })
    }

    fn create_instance(
        entry: &ash::Entry,
        application_name: &str,
    ) -> Result<(ash::Instance, bool, bool), VulkanError> {
        let name = CString::new(application_name).unwrap_or_else(|_| {
            CString::new("OpenVideoServer").unwrap_or_else(|_| c"ovs".to_owned())
        });
        let engine = c"OpenVideoServer";

        let application = vk::ApplicationInfo::default()
            .application_name(&name)
            .application_version(vk::make_api_version(0, 0, 0, 1))
            .engine_name(engine)
            .engine_version(vk::make_api_version(0, 0, 0, 1))
            // 1.2 is comfortably available on the reference hardware and brings
            // timeline semaphores, which the frame pacing will want.
            .api_version(vk::API_VERSION_1_2);

        // The display extensions, when the loader has them. Asked for here
        // because an instance's extensions are fixed at creation and the
        // renderer does not yet know whether a show will name a physical
        // output — and asked for conditionally because a machine with no
        // display support is a legitimate render node, where requesting them
        // would fail the whole instance.
        let available = Self::available_instance_extensions(entry);
        let wanted: Vec<&'static CStr> = DISPLAY_INSTANCE_EXTENSIONS
            .iter()
            .copied()
            .chain(ACQUIRE_INSTANCE_EXTENSIONS.iter().copied())
            .filter(|name| available.iter().any(|have| have.as_c_str() == *name))
            .collect();
        let enabled: Vec<*const c_char> = wanted.iter().map(|name| name.as_ptr()).collect();

        let create = vk::InstanceCreateInfo::default()
            .application_info(&application)
            .enabled_extension_names(&enabled);

        // SAFETY: `create` is fully initialised above and outlives the call;
        // `enabled` borrows from `wanted`, which is alive here.
        let instance = unsafe { entry.create_instance(&create, None) }.map_err(|result| {
            // **A loader with no driver behind it is a machine with no Vulkan**,
            // not a call that went wrong. Windows ships `vulkan-1.dll` whether
            // or not anything ever registered an ICD, so a machine with no
            // graphics driver gets past the loader check and fails here — which
            // is every CI runner, and is how 88 offscreen tests failed on the
            // first Windows job instead of skipping the way they are written to.
            //
            // Only this one result. It means exactly "nothing here implements
            // the version asked for", and widening it would start swallowing
            // failures that are worth hearing about on a machine that does have
            // a card.
            if result == vk::Result::ERROR_INCOMPATIBLE_DRIVER {
                return VulkanError::NoDevice(NoDevice::None);
            }
            VulkanError::Call {
                operation: "vkCreateInstance",
                result,
            }
        })?;

        // Two answers: whether a display can be *reached*, and whether it can be
        // *taken*. The second is what decides whether a second output lands
        // where it was asked to, and a loader can offer the first without it.
        let reaches = DISPLAY_INSTANCE_EXTENSIONS
            .iter()
            .all(|name| wanted.contains(name));
        let takes = ACQUIRE_INSTANCE_EXTENSIONS
            .iter()
            .all(|name| wanted.contains(name));
        if reaches && !takes {
            tracing::warn!(
                "this Vulkan can reach a display but not take it from the kernel; with more than \
                 one output a picture may appear on the wrong panel"
            );
        }
        Ok((instance, reaches, takes))
    }

    /// Returns the extension names the loader offers.
    fn available_instance_extensions(entry: &ash::Entry) -> Vec<std::ffi::CString> {
        // SAFETY: enumerating with no layer name is always valid.
        let Ok(properties) = (unsafe { entry.enumerate_instance_extension_properties(None) })
        else {
            return Vec::new();
        };

        properties
            .iter()
            .filter_map(|property| property.extension_name_as_c_str().ok())
            .map(std::ffi::CStr::to_owned)
            .collect()
    }

    /// Whether this machine's Vulkan can reach a physical display at all.
    ///
    /// False on a render node with no display extensions, which is a legitimate
    /// machine and not a fault — it simply cannot be an output server.
    ///
    /// Both halves are needed: the instance has to be able to enumerate a
    /// display and the device has to be able to present to one. A machine with
    /// one and not the other would enumerate a connector and then fail on the
    /// first frame, which is a worse way to find out.
    #[must_use]
    pub const fn has_display(&self) -> bool {
        self.has_display && self.has_swapchain
    }

    /// Returns the physical displays this device is wired to.
    ///
    /// Empty when the device drives no connectors, or when this build of Vulkan
    /// has no display extensions. Both mean "nothing to present on"; neither is
    /// an error, because a renderer composing for a client's preview is doing
    /// its job without a wall.
    ///
    /// # Errors
    ///
    /// Returns [`VulkanError::Call`] when the driver refuses an enumeration.
    pub fn displays(&self) -> Result<Vec<display::Display>, VulkanError> {
        if !self.has_display {
            return Ok(Vec::new());
        }
        display::enumerate(&self.entry, &self.instance, self.physical_device)
    }

    /// Asks the driver for a mode a display did not advertise.
    ///
    /// See [`display::Display::force_mode`]. Here because the entry, the
    /// instance and the physical device live on the context and a `Display` is
    /// a description rather than a handle to all three.
    ///
    /// # Errors
    ///
    /// Returns the driver's own result when it will not build the mode.
    pub fn force_mode(
        &self,
        display: &display::Display,
        size: ovs_types::geometry::SizePx,
        refresh: ovs_types::geometry::FrameRate,
    ) -> Result<display::DisplayMode, VulkanError> {
        display.force_mode(
            &self.entry,
            &self.instance,
            self.physical_device,
            size,
            refresh,
        )
    }

    /// Returns a plane that can drive `handle` on this device.
    ///
    /// # Errors
    ///
    /// Returns [`VulkanError::NoDisplayPlane`] when nothing can drive it.
    pub fn display_plane_for(&self, handle: vk::DisplayKHR) -> Result<u32, VulkanError> {
        let mut taken = self
            .taken_planes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let plane = display::plane_for(
            &self.entry,
            &self.instance,
            self.physical_device,
            handle,
            &taken,
        )?;
        taken.insert(plane);
        Ok(plane)
    }

    /// Every display plane this device has, and what each one is doing.
    ///
    /// # Errors
    ///
    /// Returns the driver's own result when they cannot be enumerated.
    pub fn display_planes(&self) -> Result<Vec<display::PlaneReport>, VulkanError> {
        display::planes(&self.entry, &self.instance, self.physical_device)
    }

    /// What one plane can do with one display mode.
    ///
    /// # Errors
    ///
    /// Returns the driver's own result when the capabilities cannot be read.
    pub fn display_plane_fit(
        &self,
        mode: vk::DisplayModeKHR,
        plane: u32,
    ) -> Result<display::PlaneFit, VulkanError> {
        display::plane_fit(
            &self.entry,
            &self.instance,
            self.physical_device,
            mode,
            plane,
        )
    }

    /// This context's card node as a raw descriptor.
    ///
    /// A descriptor is a Unix idea and `VK_EXT_acquire_drm_display` is a Linux
    /// extension, so the other half of this is here to compile rather than to
    /// run: without the extension there is no `acquire_drm`, without
    /// `/sys/class/drm` there is no `drm`, and every caller has already
    /// returned on one of those. It panics rather than answering `-1`, because
    /// an invalid descriptor handed to a driver is the kind of wrong that comes
    /// back as something else entirely.
    #[cfg(unix)]
    fn card_fd(drm: &std::fs::File) -> std::os::fd::RawFd {
        use std::os::fd::AsRawFd as _;
        drm.as_raw_fd()
    }

    #[cfg(not(unix))]
    fn card_fd(_drm: &std::fs::File) -> i32 {
        unreachable!("a card node is only ever opened where /sys/class/drm is")
    }

    /// Takes a display from the kernel, so a surface created for it lands on it.
    ///
    /// **Without this the display is still the kernel's**, and what a surface
    /// created for it does is undefined. On the driver this was written against
    /// every surface went to whichever display the driver already held —
    /// measured on 2026-09-26 with eight surfaces across two monitors, four of
    /// them carrying the second monitor's own mode, all eight on one panel.
    ///
    /// Taken once and remembered: acquiring a display twice is not an error but
    /// it is a syscall per rebuild, and the ledger is also what gives them all
    /// back at the end.
    ///
    /// **A failure here is reported, not fatal.** A machine without the
    /// extensions, or without permission to open its own card node, is a machine
    /// that can drive one output — which is what it could always do, and better
    /// than refusing to start.
    pub fn acquire_display(&self, display: vk::DisplayKHR) {
        let Some(acquire) = &self.acquire_drm else {
            return;
        };
        let Some(drm) = &self.drm else {
            return;
        };

        let mut acquired = self
            .acquired
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let raw = vk::Handle::as_raw(display);
        if acquired.contains(&raw) {
            return;
        }

        // SAFETY: the physical device belongs to this instance, and the
        // descriptor is this device's own card node, opened by this context.
        match unsafe {
            acquire.acquire_drm_display(self.physical_device, Self::card_fd(drm), display)
        } {
            Ok(()) => {
                acquired.insert(raw);
                tracing::info!("this display is now this renderer's rather than the kernel's");
            }
            // Said out loud every time, because it is the difference between a
            // picture on the right panel and a picture on some other one, and
            // the symptom is three layers away from the cause.
            Err(result) => tracing::warn!(
                ?result,
                "this display could not be taken from the kernel; a second output on this card \
                 may appear on the wrong panel"
            ),
        }
    }

    /// Asks the kernel to make this process DRM master of its card.
    ///
    /// **Taking a display from the kernel is a master's privilege.** Nothing
    /// here ever asked to be one, and `vkAcquireDrmDisplayEXT` refused every
    /// display on the machine this was written against — including ones fetched
    /// by connector id, which rules out the display handle being wrong.
    ///
    /// `DRM_IOCTL_SET_MASTER` is `_IO('d', 0x1e)`: no argument and no data, so
    /// there is nothing to get wrong about its layout. It succeeds for the first
    /// opener of a node that has no master, and fails with `EBUSY` or `EACCES`
    /// otherwise — both of which are answers worth having rather than a silent
    /// fall back to one output.
    ///
    /// # Errors
    ///
    /// The kernel's own error.
    #[cfg(unix)]
    pub fn become_drm_master(&self) -> std::io::Result<()> {
        /// `_IO('d', 0x1e)`, which on Linux is the type in the high byte.
        const DRM_IOCTL_SET_MASTER: libc::c_ulong = 0x641e;

        let Some(drm) = &self.drm else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "this renderer has no card node open",
            ));
        };

        // SAFETY: an ioctl with no argument on a descriptor this context owns.
        // Nothing is read from or written to userspace memory.
        let result = unsafe { libc::ioctl(Self::card_fd(drm), DRM_IOCTL_SET_MASTER, 0) };
        if result == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    /// Says that this is a Linux thing, which is the whole of it elsewhere.
    ///
    /// # Errors
    ///
    /// Always [`std::io::ErrorKind::Unsupported`]. There is no `ioctl` and no
    /// card node to send one to, and a build that answered `Ok(())` would be
    /// telling the caller it had taken something it cannot even name.
    #[cfg(not(unix))]
    pub fn become_drm_master(&self) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "DRM mastering is Linux's; this build has no card node to ask",
        ))
    }

    /// The display one kernel connector id names, if this card has it.
    ///
    /// **A connector id is a name that cannot drift.** Vulkan calls this
    /// machine's panels `DP-4` and `HDMI-0`; the kernel calls the same two
    /// sockets `eDP-1` and `HDMI-A-1`, and one of them reports its EDID name as
    /// "California institute of Technology". The number in
    /// `/sys/class/drm/card0-eDP-1/connector_id` is the same on both sides.
    ///
    /// # Errors
    ///
    /// Returns the driver's own result, including when this card has no such
    /// connector or the extension is not loaded.
    pub fn drm_display(&self, connector_id: u32) -> Result<vk::DisplayKHR, VulkanError> {
        let (Some(acquire), Some(drm)) = (&self.acquire_drm, &self.drm) else {
            return Err(VulkanError::Call {
                operation: "vkGetDrmDisplayEXT without VK_EXT_acquire_drm_display",
                result: vk::Result::ERROR_EXTENSION_NOT_PRESENT,
            });
        };

        // SAFETY: the physical device belongs to this instance, and the
        // descriptor is this device's own card node, opened by this context.
        unsafe { acquire.get_drm_display(self.physical_device, Self::card_fd(drm), connector_id) }
            .map_err(|result| VulkanError::Call {
                operation: "vkGetDrmDisplayEXT",
                result,
            })
    }

    /// Takes a display and says whether it worked.
    ///
    /// [`Self::acquire_display`] swallows the answer on purpose — a renderer
    /// that could not take a display still has one output to drive. This is for
    /// `ovs-planes`, which exists to find out.
    ///
    /// # Errors
    ///
    /// Returns the driver's own result.
    pub fn acquire_drm_display_for_test(&self, display: vk::DisplayKHR) -> Result<(), VulkanError> {
        let (Some(acquire), Some(drm)) = (&self.acquire_drm, &self.drm) else {
            return Err(VulkanError::Call {
                operation: "vkAcquireDrmDisplayEXT without VK_EXT_acquire_drm_display",
                result: vk::Result::ERROR_EXTENSION_NOT_PRESENT,
            });
        };

        // SAFETY: as above.
        unsafe { acquire.acquire_drm_display(self.physical_device, Self::card_fd(drm), display) }
            .map_err(|result| VulkanError::Call {
                operation: "vkAcquireDrmDisplayEXT",
                result,
            })
    }

    /// Gives a display back to the kernel.
    ///
    /// What lets a connector be handed to something else — or to this renderer
    /// again, at another mode, after a rebuild.
    pub fn release_display(&self, display: vk::DisplayKHR) {
        let Some(direct) = &self.direct_mode else {
            return;
        };

        let mut acquired = self
            .acquired
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let raw = vk::Handle::as_raw(display);
        if !acquired.remove(&raw) {
            return;
        }

        // Through the loaded pointer: this ash has no safe wrapper for
        // `VK_EXT_direct_mode_display`, whose whole surface is this one call.
        //
        // SAFETY: the physical device belongs to this instance, the pointer was
        // loaded from it, and this display was taken by the call above.
        let result = unsafe { (direct.fp().release_display_ext)(self.physical_device, display) };
        if result != vk::Result::SUCCESS {
            tracing::warn!(
                ?result,
                "this display could not be given back to the kernel"
            );
        }
    }

    /// Whether the kernel has a KMS driver holding this machine's connectors.
    ///
    /// Answered by looking for them: a card with KMS publishes one directory per
    /// connector, `card0-eDP-1` and the like. Without KMS there is only the card
    /// itself.
    fn kms_in_charge() -> bool {
        let Ok(entries) = std::fs::read_dir("/sys/class/drm") else {
            return false;
        };
        entries.flatten().any(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with("card") && name.contains('-'))
        })
    }

    /// The card node for this GPU, open, or nothing.
    ///
    /// By PCI address rather than by number: which card is `card0` depends on
    /// which driver bound first, and installing a second one is enough to swap
    /// them.
    fn open_drm(address: Option<&str>) -> Option<std::fs::File> {
        // **Only when the kernel is holding the connectors.** With
        // `nvidia-drm.modeset=0` it registers no KMS driver, there are no
        // connectors under `/sys/class/drm`, and the outputs belong to the
        // display engine Vulkan talks to. Taking a display from the kernel then
        // fails — correctly, and pointlessly, once per output built.
        if !Self::kms_in_charge() {
            tracing::info!(
                "the kernel is not managing this card's connectors, so nothing has to be taken \
                 from it"
            );
            return None;
        }

        let address = address?;
        let path = std::path::Path::new(DRM_BY_PATH).join(format!("pci-{address}-card"));
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
        {
            Ok(file) => {
                tracing::info!(path = %path.display(), "opened this card to take displays with");
                Some(file)
            }
            Err(error) => {
                tracing::warn!(
                    path = %path.display(),
                    "cannot open this card, so its displays stay the kernel's: {error}"
                );
                None
            }
        }
    }

    /// Gives a plane back, so the next output may have it.
    ///
    /// Called when a display output is dropped. A plane that was never returned
    /// is a connector this renderer can no longer drive, which on a machine with
    /// two planes and two panels is the show missing after the second rebuild.
    pub fn release_display_plane(&self, plane: u32) {
        self.taken_planes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&plane);
    }

    /// Returns what the device reports about its own memory.
    ///
    /// `None` when the driver does not offer `VK_EXT_memory_budget`, which is
    /// the only portable way to ask. A guess from the heap sizes would be a
    /// capacity rather than a budget, and a number that is wrong in the
    /// direction that matters — it would say there is room when there is not.
    #[must_use]
    pub fn memory_usage(&self) -> Option<(u64, u64)> {
        if !self.has_memory_budget {
            return None;
        }

        let mut budget = vk::PhysicalDeviceMemoryBudgetPropertiesEXT::default();
        let mut properties = vk::PhysicalDeviceMemoryProperties2::default().push_next(&mut budget);
        // SAFETY: the physical device belongs to this instance, and the chained
        // structure is alive for the call.
        unsafe {
            self.instance
                .get_physical_device_memory_properties2(self.physical_device, &mut properties)
        };

        let heaps = &properties.memory_properties;
        let mut used = 0_u64;
        let mut total = 0_u64;
        for index in 0..heaps.memory_heap_count as usize {
            if heaps.memory_heaps[index]
                .flags
                .contains(vk::MemoryHeapFlags::DEVICE_LOCAL)
            {
                used = used.saturating_add(budget.heap_usage[index]);
                total = total.saturating_add(budget.heap_budget[index]);
            }
        }

        (total > 0).then_some((used, total))
    }

    /// Gathers the facts about every device, without deciding anything.
    fn enumerate(instance: &ash::Instance) -> Result<Vec<DeviceSummary>, VulkanError> {
        // SAFETY: `instance` is live for the duration of this call.
        let devices = unsafe { instance.enumerate_physical_devices() }.map_err(|result| {
            VulkanError::Call {
                operation: "vkEnumeratePhysicalDevices",
                result,
            }
        })?;

        let mut summaries = Vec::with_capacity(devices.len());
        for (index, device) in devices.into_iter().enumerate() {
            // SAFETY: `device` came from the enumeration above.
            let properties = unsafe { instance.get_physical_device_properties(device) };
            let pci_address = pci_address_of(instance, device);
            // SAFETY: as above.
            let memory = unsafe { instance.get_physical_device_memory_properties(device) };
            // SAFETY: as above.
            let families = unsafe { instance.get_physical_device_queue_family_properties(device) };

            let graphics_family = families
                .iter()
                .position(|family| family.queue_flags.contains(vk::QueueFlags::GRAPHICS))
                .map(|index| u32::try_from(index).unwrap_or(u32::MAX));

            // A family that transfers but does not do graphics. On NVIDIA this
            // is the copy engine, which is what lets an upload run while the
            // graphics queue is busy rendering.
            let dedicated_transfer_family = families
                .iter()
                .position(|family| {
                    family.queue_flags.contains(vk::QueueFlags::TRANSFER)
                        && !family.queue_flags.contains(vk::QueueFlags::GRAPHICS)
                })
                .map(|index| u32::try_from(index).unwrap_or(u32::MAX));

            let device_local_bytes = memory
                .memory_heaps
                .iter()
                .take(memory.memory_heap_count as usize)
                .filter(|heap| heap.flags.contains(vk::MemoryHeapFlags::DEVICE_LOCAL))
                .map(|heap| heap.size)
                .max()
                .unwrap_or(0);

            summaries.push(DeviceSummary {
                index,
                name: device_name(&properties.device_name)?,
                kind: kind_of(properties.device_type),
                api_version: (
                    vk::api_version_major(properties.api_version),
                    vk::api_version_minor(properties.api_version),
                    vk::api_version_patch(properties.api_version),
                ),
                device_local_bytes,
                graphics_family,
                dedicated_transfer_family,
                pci_address,
            });
        }

        Ok(summaries)
    }

    fn create_device(
        instance: &ash::Instance,
        physical_device: vk::PhysicalDevice,
        chosen: &DeviceSummary,
    ) -> Result<OpenedDevice, VulkanError> {
        let graphics_family = chosen.graphics_family.ok_or(NoDevice::NoneUsable {
            examined: 1,
            names: chosen.name.clone(),
        })?;
        let transfer_family = chosen.dedicated_transfer_family.unwrap_or(graphics_family);

        let priorities = [1.0_f32];
        let mut queue_infos = vec![vk::DeviceQueueCreateInfo::default()
            .queue_family_index(graphics_family)
            .queue_priorities(&priorities)];
        if transfer_family != graphics_family {
            queue_infos.push(
                vk::DeviceQueueCreateInfo::default()
                    .queue_family_index(transfer_family)
                    .queue_priorities(&priorities),
            );
        }

        // Asked for only when the device has it. Reporting how much of the
        // card is in use is worth an extension; failing to open the device
        // because a driver lacks one is not. The same goes for advanced
        // blending: without it two blend modes draw as Normal and say so, which
        // is what this renderer did everywhere until a device offered it.
        let available = available_extensions(instance, physical_device);
        let has = |name: &std::ffi::CStr| available.iter().any(|have| have == name);

        let has_memory_budget = has(ash::ext::memory_budget::NAME);
        let has_advanced_blend = has(ash::ext::blend_operation_advanced::NAME)
            && advanced_blend_is_usable(instance, physical_device);
        // What turns a rendered image into a frame on a panel. Asked for the
        // same way as the others — a render node that cannot present is still a
        // usable device, and failing to open it would take the offscreen path
        // down with the display one.
        let has_swapchain = has(ash::khr::swapchain::NAME);

        let mut enabled: Vec<*const std::ffi::c_char> = Vec::new();
        if has_memory_budget {
            enabled.push(ash::ext::memory_budget::NAME.as_ptr());
        }
        if has_advanced_blend {
            enabled.push(ash::ext::blend_operation_advanced::NAME.as_ptr());
        }
        if has_swapchain {
            enabled.push(ash::khr::swapchain::NAME.as_ptr());
        }

        // Coherent operations are what let overlapping quads blend correctly
        // without a barrier between every draw. `advanced_blend_is_usable`
        // refused the extension outright if the device does not have them, so
        // asking for them here cannot fail.
        let mut advanced_features = vk::PhysicalDeviceBlendOperationAdvancedFeaturesEXT::default()
            .advanced_blend_coherent_operations(true);

        let mut create = vk::DeviceCreateInfo::default()
            .queue_create_infos(&queue_infos)
            .enabled_extension_names(&enabled);
        if has_advanced_blend {
            create = create.push_next(&mut advanced_features);
        }

        // SAFETY: `physical_device` came from this instance's enumeration, and
        // `create` borrows `queue_infos`, `priorities` and `enabled`, all alive
        // here.
        let device = unsafe { instance.create_device(physical_device, &create, None) }.map_err(
            |result| VulkanError::Call {
                operation: "vkCreateDevice",
                result,
            },
        )?;

        // SAFETY: both families were requested in `queue_infos` above.
        let graphics_queue = unsafe { device.get_device_queue(graphics_family, 0) };
        // SAFETY: as above; falls back to the graphics family when there is no
        // separate transfer one, which is a legal queue to fetch twice.
        let transfer_queue = unsafe { device.get_device_queue(transfer_family, 0) };

        Ok(OpenedDevice {
            device,
            graphics_queue,
            transfer_queue,
            has_memory_budget,
            has_advanced_blend,
            has_swapchain,
        })
    }

    /// Returns what was selected.
    #[must_use]
    pub const fn summary(&self) -> &DeviceSummary {
        &self.summary
    }

    /// Returns true when the device blends with the advanced operations.
    ///
    /// What decides whether Overlay and Difference draw as themselves or as
    /// Normal with a line in the log.
    #[must_use]
    pub const fn has_advanced_blend(&self) -> bool {
        self.has_advanced_blend
    }

    /// Returns the graphics queue.
    #[must_use]
    pub const fn graphics_queue(&self) -> vk::Queue {
        self.graphics_queue
    }

    /// Returns the queue uploads should use.
    ///
    /// The dedicated transfer queue when the device has one, otherwise the
    /// graphics queue.
    #[must_use]
    pub const fn transfer_queue(&self) -> vk::Queue {
        self.transfer_queue
    }

    /// The family [`transfer_queue`](Self::transfer_queue) belongs to.
    ///
    /// The same as the graphics family when the device has no transfer-only
    /// one, and then an image written there needs no ownership transfer.
    ///
    /// # Panics
    ///
    /// Panics if the chosen device has no graphics family, which
    /// [`selection::choose`] refuses to return.
    #[must_use]
    pub fn transfer_family(&self) -> u32 {
        self.summary
            .dedicated_transfer_family
            .unwrap_or_else(|| self.graphics_family())
    }

    /// Returns true when uploads can run on a queue of their own.
    #[must_use]
    pub const fn has_dedicated_transfer_queue(&self) -> bool {
        self.summary.dedicated_transfer_family.is_some()
    }

    /// Returns the logical device.
    #[must_use]
    pub const fn device(&self) -> &ash::Device {
        &self.device
    }

    /// Returns the physical device.
    #[must_use]
    pub const fn entry(&self) -> &ash::Entry {
        &self.entry
    }

    /// The instance every extension is loaded from.
    #[must_use]
    pub const fn instance(&self) -> &ash::Instance {
        &self.instance
    }

    /// The queue family graphics work is submitted on.
    ///
    /// # Panics
    ///
    /// Panics if the chosen device has no graphics family, which
    /// [`selection::choose`] refuses to return.
    #[must_use]
    pub fn graphics_family(&self) -> u32 {
        self.summary
            .graphics_family
            .expect("a chosen device always has a graphics family")
    }

    /// The device the renderer draws on.
    #[must_use]
    pub const fn physical_device(&self) -> vk::PhysicalDevice {
        self.physical_device
    }

    /// Returns the instance, for queries that belong to it rather than the device.
    #[must_use]
    pub const fn instance_ref(&self) -> &ash::Instance {
        &self.instance
    }
}

impl Drop for VulkanContext {
    fn drop(&mut self) {
        // SAFETY: nothing else holds the device at this point, and waiting
        // before destroying is what Vulkan requires. A failure here means the
        // device was already lost, which the destroy below handles.
        unsafe {
            let _ = self.device.device_wait_idle();
            self.device.destroy_device(None);
            self.instance.destroy_instance(None);
        }
    }
}

impl std::fmt::Debug for VulkanContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VulkanContext")
            .field("device", &self.summary.name)
            .field("kind", &self.summary.kind)
            .field("dedicated_transfer", &self.has_dedicated_transfer_queue())
            .finish()
    }
}

fn kind_of(device_type: vk::PhysicalDeviceType) -> DeviceKind {
    match device_type {
        vk::PhysicalDeviceType::DISCRETE_GPU => DeviceKind::Discrete,
        vk::PhysicalDeviceType::INTEGRATED_GPU => DeviceKind::Integrated,
        vk::PhysicalDeviceType::VIRTUAL_GPU => DeviceKind::Virtual,
        vk::PhysicalDeviceType::CPU => DeviceKind::Cpu,
        _ => DeviceKind::Other,
    }
}

/// Reads a fixed-size, NUL-terminated name out of a Vulkan properties struct.
fn device_name(raw: &[c_char]) -> Result<String, VulkanError> {
    // SAFETY: Vulkan guarantees this array is NUL-terminated within its bounds.
    let name = unsafe { CStr::from_ptr(raw.as_ptr()) };
    name.to_str()
        .map(str::to_owned)
        .map_err(|_| VulkanError::UnreadableName)
}

/// Holds the card for the length of one test.
///
/// Every test that draws opens a Vulkan device of its own, and the harness runs
/// tests on as many threads as the machine has cores. Two dozen devices coming
/// up at once against one driver is not something this product ever does — it
/// opens one — and on the Debian test machine it wedged: twenty-five threads
/// asleep on a futex and one spinning at a hundred per cent, for three hours,
/// with no output and no way to tell it from a slow build.
///
/// Serialising the tests that touch the card costs nothing, because the whole
/// renderer suite runs in four and a half seconds that way, and it removes a
/// hang that could not be reproduced on demand. It is a test-only rule: there
/// is no queue lock in the context itself, because the render loop is one
/// thread and paying for a lock it cannot contend on would be paying for
/// nothing.
#[cfg(test)]
pub(crate) fn one_gpu_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    static CARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

    // A test that panicked holding this poisoned it. That is a failed test, not
    // a reason for every test after it to fail too.
    CARD.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Opens a real device, where there is one.
    ///
    /// Skipped rather than failed where Vulkan is not installed, because a CI
    /// runner has no GPU. The test prints what it did either way, so a skip is
    /// visible in the output rather than silently passing.
    #[test]
    fn a_real_device_can_be_opened_where_one_exists() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        match VulkanContext::new("ovs-renderer tests") {
            Ok(context) => {
                let summary = context.summary();
                println!(
                    "opened {} ({}), Vulkan {}.{}.{}, {} GB, dedicated transfer: {}",
                    summary.name,
                    summary.kind,
                    summary.api_version.0,
                    summary.api_version.1,
                    summary.api_version.2,
                    summary.device_local_bytes / 1_000_000_000,
                    context.has_dedicated_transfer_queue(),
                );

                assert!(
                    summary.is_usable(),
                    "a chosen device must be able to do graphics"
                );
                assert!(!summary.name.is_empty());
                assert_ne!(
                    context.graphics_queue(),
                    vk::Queue::null(),
                    "the graphics queue must be real"
                );
                assert_ne!(context.transfer_queue(), vk::Queue::null());
                assert!(summary.api_version.0 >= 1);
            }
            Err(VulkanError::NoLoader(reason)) => {
                println!("skipped: no Vulkan loader on this machine ({reason})");
            }
            Err(VulkanError::NoDevice(reason)) => {
                println!("skipped: a loader but no usable device ({reason})");
            }
            Err(error) => panic!("Vulkan is installed but could not be brought up: {error}"),
        }
    }

    #[test]
    fn a_context_can_be_opened_and_closed_repeatedly() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // Leaking an instance or a device per open would show up here as a
        // failure to open the fourth one.
        for _ in 0..3 {
            match VulkanContext::new("ovs-renderer tests") {
                Ok(context) => drop(context),
                Err(VulkanError::NoLoader(_) | VulkanError::NoDevice(_)) => return,
                Err(error) => panic!("could not reopen Vulkan: {error}"),
            }
        }
    }

    #[test]
    fn device_types_map_to_the_kinds_selection_reasons_about() {
        assert_eq!(
            kind_of(vk::PhysicalDeviceType::DISCRETE_GPU),
            DeviceKind::Discrete
        );
        assert_eq!(
            kind_of(vk::PhysicalDeviceType::INTEGRATED_GPU),
            DeviceKind::Integrated
        );
        assert_eq!(
            kind_of(vk::PhysicalDeviceType::VIRTUAL_GPU),
            DeviceKind::Virtual
        );
        assert_eq!(kind_of(vk::PhysicalDeviceType::CPU), DeviceKind::Cpu);
        assert_eq!(kind_of(vk::PhysicalDeviceType::OTHER), DeviceKind::Other);
    }
    #[test]
    fn a_device_says_how_much_of_its_memory_is_in_use() {
        let _gpu = crate::vulkan::one_gpu_at_a_time();
        // Through VK_EXT_memory_budget or not at all. A figure worked out from
        // heap sizes would be a capacity rather than a budget, and wrong in the
        // direction that matters: it would say there is room when there is not.
        let context = match VulkanContext::new("ovs-renderer tests") {
            Ok(context) => context,
            // A CI runner has no GPU, so a skip has to be possible; printing
            // why makes it visible rather than a silent pass.
            Err(error @ (VulkanError::NoLoader(_) | VulkanError::NoDevice(_))) => {
                println!("skipped: {error}");
                return;
            }
            Err(error) => panic!("Vulkan is present but the device failed: {error}"),
        };

        match context.memory_usage() {
            Some((used, total)) => {
                assert!(total > 0, "a budget of nothing is not a budget");
                assert!(used <= total, "{used} of {total} in use");
            }
            None => println!("skipped: this driver does not offer a memory budget"),
        }
    }
}
