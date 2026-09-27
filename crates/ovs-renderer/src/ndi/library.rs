//! Opening the NDI runtime, once per process.
//!
//! Every entry point is looked up by name and kept as a function pointer. Those
//! pointers are only valid while the library is loaded, so the library is
//! loaded once and never unloaded: it is put behind a [`OnceLock`] and leaked
//! deliberately. That is not a leak in any sense that matters — it is one
//! mapping for the life of the process — and it is what makes a `&'static Ndi`
//! honest enough to hand to a receiver thread.
//!
//! `NDIlib_destroy` is therefore never called. The SDK asks for it at shutdown;
//! a process that is exiting has nothing to gain from it, and calling it while
//! any receiver still lives would be worse than not calling it at all.

use std::ffi::{c_char, c_uint, CStr};
use std::sync::OnceLock;

use super::sys;

/// Why NDI is not available on this machine.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NdiError {
    /// The runtime is not installed, or not where the loader looks.
    #[error("the NDI runtime is not installed on this machine ({0})")]
    Missing(String),
    /// Installed, but missing a function this needs.
    #[error("the installed NDI runtime has no {0}; it is older than this build expects")]
    Incomplete(String),
    /// Installed, and it declined to start.
    #[error("the NDI runtime refused to initialise; this processor is not one it supports")]
    Unsupported,
}

/// The names the runtime goes by.
///
/// The soname first, because that is what an installed runtime is called. The
/// bare symlink second, because a development install has one and a machine
/// that only ever unpacked the SDK should still work. The Windows name last so
/// that this file is not the reason the feature is absent there.
const NAMES: [&str; 3] = ["libndi.so.6", "libndi.so", "Processing.NDI.Lib.x64.dll"];

/// A loaded NDI runtime.
///
/// One per process, reached through [`Ndi::load`].
pub struct Ndi {
    /// Held so the mapping outlives the pointers below. Never dropped.
    _library: libloading::Library,
    /// What the runtime calls itself, for the log line that says it is there.
    version: String,
    pub(super) find_create: sys::FindCreateFn,
    pub(super) find_destroy: sys::FindDestroyFn,
    pub(super) find_wait: sys::FindWaitFn,
    pub(super) find_sources: sys::FindSourcesFn,
    pub(super) recv_create: sys::RecvCreateFn,
    pub(super) recv_destroy: sys::RecvDestroyFn,
    pub(super) recv_capture: sys::RecvCaptureFn,
    pub(super) recv_free_video: sys::RecvFreeVideoFn,
    pub(super) send_create: sys::SendCreateFn,
    pub(super) send_destroy: sys::SendDestroyFn,
    pub(super) send_source_name: sys::SendSourceNameFn,
    pub(super) send_video: sys::SendVideoFn,
}

// Safe because the library is never unloaded and the pointers are only ever
// read. NDI's own documentation states that a receiver may be driven from its
// own thread, which is the whole point of having these here: one receiver per
// layer, each on the thread that owns it.
unsafe impl Send for Ndi {}
unsafe impl Sync for Ndi {}

impl Ndi {
    /// Opens the runtime, or says why it could not.
    ///
    /// Answered from memory after the first call, including the failure: a
    /// machine without NDI should not pay for a failed `dlopen` every time a
    /// show is loaded, and it should not write the same line to the log twice.
    ///
    /// # Errors
    ///
    /// [`NdiError`] when the runtime is absent, too old, or refuses to start.
    pub fn load() -> Result<&'static Self, NdiError> {
        static LOADED: OnceLock<Result<&'static Ndi, NdiError>> = OnceLock::new();
        LOADED.get_or_init(Self::open).clone()
    }

    /// What the runtime calls itself.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Does the loading, exactly once.
    fn open() -> Result<&'static Self, NdiError> {
        let mut tried = Vec::new();
        let library = NAMES
            .iter()
            .find_map(|name| {
                // Safe only in the sense that every `dlopen` is: the library
                // runs its own initialisers. This one is a vendor runtime an
                // operator installed on purpose.
                match unsafe { libloading::Library::new(name) } {
                    Ok(library) => Some(library),
                    Err(_) => {
                        tried.push(*name);
                        None
                    }
                }
            })
            .ok_or_else(|| NdiError::Missing(format!("tried {}", tried.join(", "))))?;

        // A macro rather than nine near-identical blocks, because the thing
        // worth reading here is the list of names, not the lookup.
        macro_rules! symbol {
            ($name:literal) => {{
                let found: libloading::Symbol<'_, _> =
                    unsafe { library.get($name) }.map_err(|_| {
                        NdiError::Incomplete(
                            String::from_utf8_lossy(&$name[..$name.len() - 1]).into_owned(),
                        )
                    })?;
                *found
            }};
        }

        let initialise: sys::InitialiseFn = symbol!(b"NDIlib_initialize\0");
        let supported_cpu: sys::SupportedCpuFn = symbol!(b"NDIlib_is_supported_CPU\0");
        let version_of: sys::VersionFn = symbol!(b"NDIlib_version\0");
        let loaded = Self {
            find_create: symbol!(b"NDIlib_find_create_v2\0"),
            find_destroy: symbol!(b"NDIlib_find_destroy\0"),
            find_wait: symbol!(b"NDIlib_find_wait_for_sources\0"),
            find_sources: symbol!(b"NDIlib_find_get_current_sources\0"),
            recv_create: symbol!(b"NDIlib_recv_create_v3\0"),
            recv_destroy: symbol!(b"NDIlib_recv_destroy\0"),
            recv_capture: symbol!(b"NDIlib_recv_capture_v3\0"),
            recv_free_video: symbol!(b"NDIlib_recv_free_video_v2\0"),
            send_create: symbol!(b"NDIlib_send_create\0"),
            send_destroy: symbol!(b"NDIlib_send_destroy\0"),
            send_source_name: symbol!(b"NDIlib_send_get_source_name\0"),
            send_video: symbol!(b"NDIlib_send_send_video_v2\0"),
            version: unsafe { text(version_of()) }.unwrap_or_else(|| "unknown".to_owned()),
            _library: library,
        };

        // Asked before initialising, because the SDK's answer to an
        // unsupported processor is to refuse, and "your processor is too old"
        // is a better line than "it refused".
        if !unsafe { supported_cpu() } || !unsafe { initialise() } {
            return Err(NdiError::Unsupported);
        }

        tracing::info!(version = %loaded.version, "NDI runtime loaded");
        Ok(&*Box::leak(Box::new(loaded)))
    }

    /// Opens a finder that keeps listening until it is dropped.
    ///
    /// `None` when the runtime would not make one, which is not a state any
    /// machine has been seen in — it is here because a null instance is what
    /// the C API returns for failure and pretending otherwise would mean
    /// calling into it with nothing.
    #[must_use]
    pub fn finder(&'static self) -> Option<Finder> {
        let settings = sys::FindCreate {
            show_local_sources: true,
            p_groups: std::ptr::null(),
            p_extra_ips: std::ptr::null(),
        };
        // Safe: `settings` outlives the call and the SDK copies what it keeps.
        let instance = unsafe { (self.find_create)(&settings) };
        (!instance.is_null()).then_some(Finder {
            ndi: self,
            instance,
        })
    }

    /// The names of every source on the network, after waiting for answers.
    ///
    /// For a question asked once — a diagnostic, a test. Anything that wants
    /// the list to stay current holds a [`Finder`] instead, because this one
    /// throws the finder away and the next call starts over.
    #[must_use]
    pub fn sources(&'static self, patience_ms: u32) -> Vec<String> {
        self.finder()
            .map(|finder| finder.look(patience_ms))
            .unwrap_or_default()
    }
}

/// A live listener for sources appearing and disappearing.
pub struct Finder {
    ndi: &'static Ndi,
    instance: sys::FindInstance,
}

// Safe because the finder is only ever used from the thread that holds it, and
// the runtime behind it is never unloaded. `Send` so that thread can be a
// different one from the thread that made it.
unsafe impl Send for Finder {}

impl Finder {
    /// Waits for the network to say something, then returns what is there now.
    ///
    /// Returns early when the set changes, so a source appearing is seen at
    /// once rather than at the end of the wait. Sorted, so an operator's list
    /// does not reshuffle itself between two looks at it; and de-duplicated,
    /// because two senders may announce the same name and offering the same
    /// entry twice would be a list nobody can choose from.
    #[must_use]
    pub fn look(&self, patience_ms: u32) -> Vec<String> {
        // Safe: the instance is live for as long as this struct is.
        unsafe { (self.ndi.find_wait)(self.instance, patience_ms) };

        let mut count: c_uint = 0;
        let found = unsafe { (self.ndi.find_sources)(self.instance, &mut count) };
        if found.is_null() {
            return Vec::new();
        }

        let mut names = std::collections::BTreeSet::new();
        for index in 0..count as usize {
            // Safe: NDI says there are `count` of them, and the names stay
            // valid until the next call on this finder or its destruction.
            let source = unsafe { &*found.add(index) };
            if let Some(name) = unsafe { text(source.p_ndi_name) } {
                names.insert(name);
            }
        }
        names.into_iter().collect()
    }
}

impl Drop for Finder {
    /// Leaves the multicast group.
    fn drop(&mut self) {
        unsafe { (self.ndi.find_destroy)(self.instance) };
    }
}

/// Copies a C string out, or `None` for a null or unreadable one.
///
/// Used by the finder and by the sender, which both hand back strings NDI owns.
///
/// # Safety
///
/// `pointer` is null or points at a NUL-terminated string that stays valid for
/// the length of this call.
pub(super) unsafe fn text(pointer: *const c_char) -> Option<String> {
    if pointer.is_null() {
        return None;
    }
    CStr::from_ptr(pointer).to_str().ok().map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Loading twice must not load twice.
    ///
    /// Runs on a machine with no NDI as well, which is the case worth having a
    /// test for: the failure has to be remembered too, or a show with six NDI
    /// layers does six failed `dlopen`s a load and says so six times.
    #[test]
    fn loading_is_answered_from_memory() {
        let first = Ndi::load();
        let second = Ndi::load();
        match (first, second) {
            (Ok(one), Ok(two)) => assert!(std::ptr::eq(one, two), "two runtimes for one process"),
            (Err(one), Err(two)) => assert_eq!(one, two, "the same machine gave two answers"),
            _ => panic!("one call found NDI and the other did not"),
        }
    }

    /// The four-character codes have to be the numbers the SDK builds.
    ///
    /// Little-endian, which is easy to get backwards and impossible to notice:
    /// a wrong code here means a frame falls through to "a layout this build
    /// does not know" and the layer stays dark with a puzzling reason.
    #[test]
    fn four_character_codes_are_packed_the_way_the_header_packs_them() {
        assert_eq!(sys::four_cc::UYVY, 0x5956_5955, "UYVY");
        assert_eq!(sys::four_cc::RGBA, 0x4142_4752, "RGBA");
        assert_eq!(sys::four_cc::RGBX, 0x5842_4752, "RGBX");
    }
}
