//! The client, offered by the server it talks to.
//!
//! The owner, 2026-09-24: "ggf. kann man dort lokal den Clienten für Windows
//! schon runterladen", and, asked where the file should come from: in the
//! package.
//!
//! So the installer puts the client where this looks, and this hands it out.
//! The point is the evening where somebody needs the client on a second laptop
//! and the venue's wifi is a captive portal: the machine running the show has
//! the installer on it, and its address is already on a screen.
//!
//! # What is offered
//!
//! Whatever is in the directory, by its own name and size. Nothing is
//! generated and nothing is guessed, and a directory that is empty — a server
//! built from source rather than installed from the package — offers nothing.
//! A page promising a download the server does not have is worse than a page
//! promising none.

use std::path::{Path, PathBuf};

use serde::Serialize;

/// Where the package puts the client installers.
pub const DEFAULT_DIR: &str = "/usr/share/openvideoserver/client";

/// The environment variable that moves it, for a server run from a build.
pub const DIR_ENV: &str = "OVS_CLIENT_DIR";

/// One file this server can hand out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Download {
    /// The file's own name, which is also how it is asked for.
    pub name: String,
    /// How big it is, in bytes.
    pub bytes: u64,
}

/// Where the installers live on this machine.
#[must_use]
pub fn directory() -> PathBuf {
    std::env::var(DIR_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_DIR))
}

/// What this server can hand out.
#[must_use]
pub fn available() -> Vec<Download> {
    available_in(&directory())
}

/// Resolves a requested name to a file this server may hand out.
#[must_use]
pub fn resolve(name: &str) -> Option<PathBuf> {
    resolve_in(&directory(), name)
}

/// What is in one directory, newest name first.
///
/// Never fails: a missing directory, an unreadable one and an empty one are the
/// same answer, and that answer is a page which offers no download.
#[must_use]
pub fn available_in(directory: &Path) -> Vec<Download> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };

    let mut found: Vec<Download> = entries
        .flatten()
        .filter_map(|entry| {
            let metadata = entry.metadata().ok()?;
            if !metadata.is_file() {
                return None;
            }
            let name = entry.file_name().to_str()?.to_owned();
            // Nothing hidden and nothing part-written: a package manager
            // unpacking into this directory leaves both about for a moment, and
            // a half-written installer is the worst thing to hand somebody.
            if name.starts_with('.') || name.ends_with(".part") {
                return None;
            }
            Some(Download {
                name,
                bytes: metadata.len(),
            })
        })
        .collect();

    found.sort_by(|left, right| right.name.cmp(&left.name));
    found
}

/// Resolves a name against one directory, or answers nothing.
///
/// **The whole security of this module.** The name comes off a URL, so it is
/// somebody else's string: anything carrying a separator, a parent or a leading
/// dot is refused outright rather than cleaned up, because cleaning up is where
/// traversal bugs live. What comes back is only ever a plain file that exists in
/// the one directory asked about.
#[must_use]
pub fn resolve_in(directory: &Path, name: &str) -> Option<PathBuf> {
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
        || name.starts_with('.')
        || Path::new(name).components().count() != 1
    {
        return None;
    }

    let path = directory.join(name);
    path.is_file().then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_that_could_leave_the_directory_is_refused_rather_than_cleaned() {
        // Every one of these is a URL somebody can type. None of them is
        // cleaned up, because cleaning up is where traversal bugs live.
        let directory = Path::new("/tmp");
        for name in [
            "",
            "..",
            "../../etc/shadow",
            "/etc/shadow",
            "sub/dir",
            "sub\\dir",
            ".hidden",
            "a/../../b",
            "....//etc/shadow",
        ] {
            assert!(
                resolve_in(directory, name).is_none(),
                "{name} should be refused"
            );
        }
    }

    #[test]
    fn a_missing_directory_offers_nothing_rather_than_failing() {
        // A server built from source has no package directory, and that is an
        // ordinary server rather than a broken one.
        let nowhere = Path::new("/nonexistent/openvideoserver/client");
        assert_eq!(available_in(nowhere), Vec::new());
        assert!(resolve_in(nowhere, "anything.exe").is_none());
    }

    #[test]
    fn what_is_there_is_offered_by_name_and_size() {
        let directory = std::env::temp_dir().join(format!(
            "ovs-downloads-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&directory).expect("makes the directory");
        std::fs::write(directory.join("OpenVideoServer-0.2.0.exe"), b"installer").expect("writes");
        std::fs::write(directory.join(".part-file"), b"x").expect("writes");
        std::fs::write(directory.join("half.part"), b"x").expect("writes");

        let found = available_in(&directory);
        assert_eq!(found.len(), 1, "got {found:?}");
        assert_eq!(found[0].name, "OpenVideoServer-0.2.0.exe");
        assert_eq!(found[0].bytes, 9);
        assert!(resolve_in(&directory, "OpenVideoServer-0.2.0.exe").is_some());

        std::fs::remove_dir_all(&directory).ok();
    }
}
