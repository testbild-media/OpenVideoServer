//! Records which commit this build came from.
//!
//! **Two packages that say the same thing can be different software.** The
//! Debian version is read out of `Cargo.toml`, so every build of 0.2.0 is called
//! `openvideoserver_0.2.0_amd64.deb` whatever is inside it. On 2026-10-10 that
//! cost two installs and two wrong conclusions: a package was installed, the
//! binary did not have the flag that had just been added, and the only symptom
//! was an unrelated error about a port. Nothing on the machine could answer
//! "which build is this".
//!
//! So the commit is baked in. Missing is an ordinary state — a source tarball
//! has no git — and says so rather than failing a build.

use std::process::Command;

fn main() {
    // Only when the commit could change. Without this the build script runs on
    // every compile and rebuilds the crate with it.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs/heads");
    println!("cargo:rerun-if-env-changed=OVS_BUILD_COMMIT");

    // A workflow that builds from an archive rather than a checkout can say so
    // itself, which is also how this stays honest in a container that has the
    // files but not the history.
    if let Ok(given) = std::env::var("OVS_BUILD_COMMIT") {
        if !given.trim().is_empty() {
            println!("cargo:rustc-env=OVS_BUILD_COMMIT={}", given.trim());
            return;
        }
    }

    let described = Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty());

    let dirty = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| !output.stdout.is_empty())
        .unwrap_or(false);

    let commit = match described {
        // Said out loud: a build with uncommitted changes in it is not the
        // commit it names, and that is exactly the build somebody is holding
        // when they cannot work out why a fix is not in it.
        Some(commit) if dirty => format!("{commit}-modified"),
        Some(commit) => commit,
        None => "unknown".to_owned(),
    };

    println!("cargo:rustc-env=OVS_BUILD_COMMIT={commit}");
}
