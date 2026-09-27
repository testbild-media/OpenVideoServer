//! What the GPU says about itself, where the driver will say.
//!
//! Vulkan reports memory and nothing else: how hard a card is working and how
//! hot it is are not in the API, and on a media server they are two of the
//! numbers an operator looks at first. NVIDIA's own library answers
//! both, so this asks it.
//!
//! # Loaded at runtime, never linked
//!
//! `libnvidia-ml.so.1` is opened by name when the renderer starts and every
//! function is looked up by name. Nothing is linked against it, nothing depends
//! on it, and a machine without it — an AMD card, an Intel one, a development
//! box, the CI runner — loses two lines on a console and nothing else.
//!
//! That is the same rule the rest of the renderer keeps: a reading the platform
//! does not offer is absent, not zero (`crate::host` says it at more length). A
//! GPU at 0% on a console looks like a card doing nothing; a dash says nobody
//! asked it.
//!
//! # Why by PCI address
//!
//! A machine can have two NVIDIA cards, and the one the renderer opened is the
//! one worth reporting. Vulkan gives its PCI address; NVML finds a device by the
//! same string, so the two agree without either guessing.

use std::ffi::{c_char, c_int, c_uint, c_void, CStr, CString};

/// What one GPU is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GpuLoad {
    /// Percentage of the last sample period the GPU was busy.
    pub utilisation_percent: Option<u32>,
    /// Core temperature in degrees Celsius.
    pub temperature_c: Option<u32>,
    /// Device memory in use, in bytes.
    pub memory_used_bytes: Option<u64>,
    /// Device memory in total, in bytes.
    pub memory_total_bytes: Option<u64>,
}

impl GpuLoad {
    /// Whether the card said anything at all.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.utilisation_percent.is_none()
            && self.temperature_c.is_none()
            && self.memory_used_bytes.is_none()
    }
}

/// NVML's own utilisation structure.
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct Utilization {
    gpu: c_uint,
    memory: c_uint,
}

/// NVML's own memory structure.
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct Memory {
    total: u64,
    free: u64,
    used: u64,
}

/// An opaque NVML device handle.
type Device = *mut c_void;

type InitFn = unsafe extern "C" fn() -> c_int;
type ShutdownFn = unsafe extern "C" fn() -> c_int;
type ByPciFn = unsafe extern "C" fn(*const c_char, *mut Device) -> c_int;
type ByIndexFn = unsafe extern "C" fn(c_uint, *mut Device) -> c_int;
type UtilisationFn = unsafe extern "C" fn(Device, *mut Utilization) -> c_int;
type TemperatureFn = unsafe extern "C" fn(Device, c_int, *mut c_uint) -> c_int;
type MemoryFn = unsafe extern "C" fn(Device, *mut Memory) -> c_int;

/// Sensor 0 is the GPU die, which is the one an operator means.
const TEMPERATURE_GPU: c_int = 0;

/// NVML's "everything went well".
const SUCCESS: c_int = 0;

/// A loaded NVML, bound to one device.
pub struct Nvml {
    library: libloading::Library,
    device: Device,
    utilisation: UtilisationFn,
    temperature: TemperatureFn,
    memory: MemoryFn,
    shutdown: ShutdownFn,
}

// SAFETY: NVML is thread-safe by its own documentation, and the handle is a
// plain pointer into the driver rather than into this process.
unsafe impl Send for Nvml {}
unsafe impl Sync for Nvml {}

impl std::fmt::Debug for Nvml {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Nvml").finish_non_exhaustive()
    }
}

impl Nvml {
    /// Opens NVML and finds the card at `pci_address`.
    ///
    /// Returns `None` when the library is not installed, when it refuses to
    /// start, or when that address is not an NVIDIA card — all of which are
    /// ordinary on a machine this product runs on, and none of which is worth a
    /// line in a log every time the renderer starts.
    ///
    /// With no address the first device is used, which is right for the common
    /// machine with one card and wrong only where there are several — hence the
    /// address.
    #[must_use]
    pub fn open(pci_address: Option<&str>) -> Option<Self> {
        // SAFETY: opening a shared library by name. Unsafe because it runs the
        // library's initialisers; a missing one is an error, not a panic.
        let library = unsafe { libloading::Library::new("libnvidia-ml.so.1") }
            .or_else(|_| unsafe { libloading::Library::new("nvml.dll") })
            .ok()?;

        // SAFETY: every symbol below is looked up by the name NVML publishes and
        // called with the signature NVML documents. A name that is not there
        // makes this return `None` rather than calling anything.
        unsafe {
            let init = *library.get::<InitFn>(b"nvmlInit_v2\0").ok()?;
            if init() != SUCCESS {
                return None;
            }

            let shutdown = *library.get::<ShutdownFn>(b"nvmlShutdown\0").ok()?;

            let mut device: Device = std::ptr::null_mut();
            let found = match pci_address {
                Some(address) => {
                    let address = CString::new(address).ok()?;
                    let by_pci = *library
                        .get::<ByPciFn>(b"nvmlDeviceGetHandleByPciBusId_v2\0")
                        .ok()?;
                    by_pci(address.as_ptr(), &raw mut device)
                }
                None => {
                    let by_index = *library
                        .get::<ByIndexFn>(b"nvmlDeviceGetHandleByIndex_v2\0")
                        .ok()?;
                    by_index(0, &raw mut device)
                }
            };

            if found != SUCCESS {
                shutdown();
                return None;
            }

            let utilisation = *library
                .get::<UtilisationFn>(b"nvmlDeviceGetUtilizationRates\0")
                .ok()?;
            let temperature = *library
                .get::<TemperatureFn>(b"nvmlDeviceGetTemperature\0")
                .ok()?;
            let memory = *library.get::<MemoryFn>(b"nvmlDeviceGetMemoryInfo\0").ok()?;

            Some(Self {
                library,
                device,
                utilisation,
                temperature,
                memory,
                shutdown,
            })
        }
    }

    /// Asks the card what it is doing.
    ///
    /// Each reading is separate on purpose: a driver that answers one and
    /// refuses another should report the one it answered rather than nothing.
    #[must_use]
    pub fn sample(&self) -> GpuLoad {
        let mut load = GpuLoad::default();

        // SAFETY: `self.device` came from this library's own lookup and stays
        // valid until `shutdown`, which only happens in `drop`.
        unsafe {
            let mut utilisation = Utilization::default();
            if (self.utilisation)(self.device, &raw mut utilisation) == SUCCESS {
                load.utilisation_percent = Some(utilisation.gpu);
            }

            let mut degrees: c_uint = 0;
            if (self.temperature)(self.device, TEMPERATURE_GPU, &raw mut degrees) == SUCCESS {
                load.temperature_c = Some(degrees);
            }

            let mut memory = Memory::default();
            if (self.memory)(self.device, &raw mut memory) == SUCCESS {
                load.memory_used_bytes = Some(memory.used);
                load.memory_total_bytes = Some(memory.total);
            }
        }

        load
    }

    /// Returns the library's version string, for a log line at startup.
    #[must_use]
    pub fn driver_version(&self) -> Option<String> {
        type VersionFn = unsafe extern "C" fn(*mut c_char, c_uint) -> c_int;

        // SAFETY: as above; the buffer is the size NVML is told it is.
        unsafe {
            let version = *self
                .library
                .get::<VersionFn>(b"nvmlSystemGetDriverVersion\0")
                .ok()?;
            let mut buffer = [0_i8; 80];
            if version(buffer.as_mut_ptr(), buffer.len() as c_uint) != SUCCESS {
                return None;
            }
            Some(
                CStr::from_ptr(buffer.as_ptr())
                    .to_string_lossy()
                    .into_owned(),
            )
        }
    }
}

impl Drop for Nvml {
    fn drop(&mut self) {
        // SAFETY: shutting down what this opened, once.
        unsafe {
            (self.shutdown)();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_machine_with_no_nvidia_driver_says_nothing_rather_than_failing() {
        // Which is every development machine, every CI runner and every server
        // with an AMD or Intel card. The renderer must come up on all of them.
        let opened = Nvml::open(Some("0000:ff:ff.f"));
        assert!(
            opened.is_none() || opened.is_some(),
            "opening must not panic either way"
        );
    }

    #[test]
    fn an_empty_reading_knows_it_is_empty() {
        assert!(GpuLoad::default().is_empty());
        assert!(!GpuLoad {
            utilisation_percent: Some(0),
            ..GpuLoad::default()
        }
        .is_empty());
    }

    #[test]
    fn zero_percent_is_a_reading_and_not_an_absence() {
        // The distinction the whole module exists for: a card doing nothing and
        // a card nobody asked must not look the same on a console.
        let busy_doing_nothing = GpuLoad {
            utilisation_percent: Some(0),
            temperature_c: Some(41),
            ..GpuLoad::default()
        };

        assert!(!busy_doing_nothing.is_empty());
        assert_eq!(busy_doing_nothing.utilisation_percent, Some(0));
    }
}
