//! Updating the server from its own administration page.
//!
//! The owner, 2026-10-19: "ob der server selber Updates über das Repo abfragen
//! kann und bei bedarf durch den User in der AdminGUI direkt installiert werden
//! kann, dass man kein ssh zugriff oder ähnlich mehr braucht." And on how: "gar
//! nicht automatisch prüfen. nur auf knopfdruck. ohne signatur."
//!
//! # Two halves, and the server is the smaller one
//!
//! **Asking** is done here: an administrator presses a button, the server asks
//! the repository for its newest release and says whether it is newer than
//! what runs. Never unasked — a machine at a venue with no internet must not
//! sit trying, and one with internet must not talk to anybody nobody asked it
//! to.
//!
//! **Installing** is not. The server runs as an unprivileged user and installing
//! a package needs root, so it asks the init system to start
//! `ovs-update@<version>.service` and nothing more — the same shape as
//! [`crate::power`], and decided the same way, by a polkit rule
//! (`deploy/polkit/51-openvideoserver-update.rules`) that lets this one user
//! start that one unit for a version-shaped name and nothing else. The unit runs
//! `deploy/update/ovs-update`, which fetches the release from the repository
//! that is written into it, checks the package against the checksum published
//! beside it, refuses anything not newer than what is installed, and installs it
//! with apt.
//!
//! It has to be a unit of its own: the package it installs restarts this server,
//! and an install this server had started itself would be killed half way.
//!
//! # What an administrator can do with it, and what not
//!
//! Install a newer release of this server, from its own repository. Not an older
//! one — a newer version may already have changed the database — not a package
//! from anywhere else, and not anything uploaded.

use std::process::Command;
use std::sync::Mutex;

use ovs_proto::http::{UpdateCheck, UpdateProgress};

/// Where releases come from. Written into the installer too, which does not
/// take the server's word for it.
pub const REPOSITORY: &str = "testbild-media/OpenVideoServer";

/// Where the installer writes how far it got.
pub const PROGRESS_FILE: &str = "/var/lib/ovs-update/status.json";

/// The environment variable that moves it, for a server run from a build.
pub const PROGRESS_ENV: &str = "OVS_UPDATE_STATUS";

/// What the last check found, kept until the server restarts.
static LAST_CHECK: Mutex<Option<UpdateCheck>> = Mutex::new(None);

/// A version as the releases are tagged: three numbers.
#[must_use]
pub fn parse_version(text: &str) -> Option<(u32, u32, u32)> {
    let mut parts = text.split('.');
    let mut next = || -> Option<u32> {
        let part = parts.next()?;
        if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) || part.len() > 6 {
            return None;
        }
        part.parse().ok()
    };
    let version = (next()?, next()?, next()?);
    parts.next().is_none().then_some(version)
}

/// Whether `candidate` is a newer release than `installed`.
#[must_use]
pub fn is_newer(candidate: &str, installed: &str) -> bool {
    match (parse_version(candidate), parse_version(installed)) {
        (Some(candidate), Some(installed)) => candidate > installed,
        _ => false,
    }
}

/// What Debian calls this machine's architecture, which is in a package's name.
#[must_use]
pub fn architecture() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    }
}

/// Reads the repository's answer about its newest release.
///
/// Separate from asking so that what is made of an answer is testable without a
/// network. `newer` only where the release carries a package for this machine:
/// a release still being published, or one built for another architecture, is
/// not something to offer.
#[must_use]
pub fn read_release(answer: &str, installed: &str, architecture: &str, now: i64) -> UpdateCheck {
    let mut check = UpdateCheck {
        checked_at: now,
        latest: None,
        newer: false,
        published_at: None,
        notes_url: None,
        problem: None,
    };
    let Ok(release) = serde_json::from_str::<serde_json::Value>(answer) else {
        check.problem = Some("the repository's answer could not be read".to_owned());
        return check;
    };

    let tag = release["tag_name"].as_str().unwrap_or_default();
    let Some(version) = tag
        .strip_prefix('v')
        .filter(|version| parse_version(version).is_some())
    else {
        check.problem = Some(format!(
            "the newest release is tagged {tag:?}, not a version"
        ));
        return check;
    };
    check.latest = Some(version.to_owned());
    check.published_at = release["published_at"].as_str().map(str::to_owned);
    check.notes_url = release["html_url"].as_str().map(str::to_owned);

    if !is_newer(version, installed) {
        return check;
    }
    let prefix = format!("openvideoserver_{version}-");
    let suffix = format!("_{architecture}.deb");
    let names: Vec<&str> = release["assets"]
        .as_array()
        .map(|assets| {
            assets
                .iter()
                .filter_map(|asset| asset["name"].as_str())
                .collect()
        })
        .unwrap_or_default();
    let package = names
        .iter()
        .find(|name| name.starts_with(&prefix) && name.ends_with(&suffix));
    match package {
        Some(package) if names.contains(&format!("{package}.sha256").as_str()) => {
            check.newer = true;
        }
        Some(_) => {
            check.problem = Some(format!("{version} has no checksum beside its package yet"));
        }
        None => {
            check.problem = Some(format!("{version} has no package for {architecture} yet"));
        }
    }
    check
}

/// Asks the repository for its newest release, and remembers the answer.
///
/// Blocking, and it can take as long as the network does: call it off the
/// runtime's own threads.
#[must_use]
pub fn check(installed: &str) -> UpdateCheck {
    let now = ovs_types::time::Timestamp::now().as_millis();
    let address = format!("https://api.github.com/repos/{REPOSITORY}/releases/latest");
    let answer = Command::new("curl")
        .args([
            "--fail",
            "--silent",
            "--show-error",
            "--location",
            "--max-time",
            "20",
            "--header",
            "Accept: application/vnd.github+json",
            "--header",
            "User-Agent: OpenVideoServer",
            &address,
        ])
        .output();

    let check = match answer {
        Ok(output) if output.status.success() => read_release(
            &String::from_utf8_lossy(&output.stdout),
            installed,
            architecture(),
            now,
        ),
        Ok(output) => UpdateCheck {
            checked_at: now,
            latest: None,
            newer: false,
            published_at: None,
            notes_url: None,
            problem: Some(format!(
                "the repository could not be reached: {}",
                last_line(&output.stderr).unwrap_or_else(|| output.status.to_string())
            )),
        },
        Err(error) => UpdateCheck {
            checked_at: now,
            latest: None,
            newer: false,
            published_at: None,
            notes_url: None,
            problem: Some(format!("curl could not be run: {error}")),
        },
    };

    if let Ok(mut last) = LAST_CHECK.lock() {
        *last = Some(check.clone());
    }
    check
}

/// What the last check found, if anybody has asked since the server started.
#[must_use]
pub fn last_check() -> Option<UpdateCheck> {
    LAST_CHECK.lock().ok().and_then(|last| last.clone())
}

/// What the installer last wrote, if it has ever run.
#[must_use]
pub fn progress() -> Option<UpdateProgress> {
    let path = std::env::var(PROGRESS_ENV).unwrap_or_else(|_| PROGRESS_FILE.to_owned());
    read_progress(&std::fs::read_to_string(path).ok()?)
}

/// Reads what the installer wrote. Nothing, rather than an error, for anything
/// that is not what it writes: a half-written file is a moment, not a fault.
#[must_use]
pub fn read_progress(written: &str) -> Option<UpdateProgress> {
    serde_json::from_str(written).ok()
}

/// Whether the installer is part way through something.
#[must_use]
pub fn is_running(progress: &UpdateProgress) -> bool {
    matches!(
        progress.stage.as_str(),
        "starting" | "downloading" | "verifying" | "installing"
    )
}

/// The unit that installs `version`.
#[must_use]
pub fn unit_for(version: &str) -> String {
    format!("ovs-update@{version}.service")
}

/// What starting the installer produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// Whether the machine started it.
    pub accepted: bool,
    /// What was run, or why nothing was.
    pub detail: String,
}

/// Starts the installer for `version`, if it is a newer release than
/// `installed`, and does not wait for it.
#[must_use]
pub fn install(version: &str, installed: &str) -> Outcome {
    if parse_version(version).is_none() {
        return Outcome {
            accepted: false,
            detail: format!("{version:?} is not a version"),
        };
    }
    if !is_newer(version, installed) {
        return Outcome {
            accepted: false,
            detail: format!("{version} is not newer than {installed}, which is installed"),
        };
    }
    if let Some(progress) = progress().filter(is_running) {
        return Outcome {
            accepted: false,
            detail: format!(
                "the installer is already {} {}",
                progress.stage, progress.version
            ),
        };
    }

    let unit = unit_for(version);
    let printable = format!("systemctl start --no-block {unit}");
    match Command::new("systemctl")
        .args(["start", "--no-block", &unit])
        .output()
    {
        Ok(result) if result.status.success() => Outcome {
            accepted: true,
            detail: printable,
        },
        Ok(result) => Outcome {
            accepted: false,
            detail: format!(
                "`{printable}` was refused ({}): {}",
                result.status,
                last_line(&result.stderr).unwrap_or_else(|| "it said nothing".to_owned())
            ),
        },
        Err(error) => Outcome {
            accepted: false,
            detail: format!("`{printable}` could not be run: {error}"),
        },
    }
}

/// The last line a command said anything on, which is where a refusal is.
fn last_line(said: &[u8]) -> Option<String> {
    String::from_utf8_lossy(said)
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shaped like the repository's real answer for a release, 2026-10-19:
    /// the fields read here, and the assets a release carries.
    fn release(tag: &str, assets: &[&str]) -> String {
        let assets: Vec<serde_json::Value> = assets
            .iter()
            .map(|name| {
                serde_json::json!({
                    "name": name,
                    "browser_download_url": format!(
                        "https://github.com/{REPOSITORY}/releases/download/{tag}/{name}"
                    ),
                })
            })
            .collect();
        serde_json::json!({
            "tag_name": tag,
            "html_url": format!("https://github.com/{REPOSITORY}/releases/tag/{tag}"),
            "published_at": "2026-10-19T02:00:00Z",
            "assets": assets,
        })
        .to_string()
    }

    const FULL: [&str; 4] = [
        "OpenVideoServer-0.2.46-58-setup.exe",
        "OpenVideoServer-0.2.46-58-setup.exe.sha256",
        "openvideoserver_0.2.46-58_amd64.deb",
        "openvideoserver_0.2.46-58_amd64.deb.sha256",
    ];

    #[test]
    fn a_version_is_three_numbers_and_nothing_else() {
        assert_eq!(parse_version("0.2.46"), Some((0, 2, 46)));
        assert_eq!(parse_version("10.0.0"), Some((10, 0, 0)));
        for not in [
            "",
            "0.2",
            "0.2.46.1",
            "v0.2.46",
            "0.2.46-58",
            "0.2.x",
            "0..2",
            "0.2.46 ",
            "-1.0.0",
            "0.2.46\n",
            "1.2.3/../x",
        ] {
            assert_eq!(parse_version(not), None, "{not:?}");
        }
    }

    #[test]
    fn newer_is_numeric_not_alphabetical() {
        assert!(is_newer("0.2.46", "0.2.45"));
        assert!(is_newer("0.10.0", "0.9.9"), "ten is more than nine");
        assert!(!is_newer("0.2.45", "0.2.45"), "the same is not newer");
        assert!(!is_newer("0.2.44", "0.2.45"), "an older one never is");
        assert!(!is_newer("nonsense", "0.2.45"));
    }

    #[test]
    fn a_newer_release_with_a_package_for_this_machine_is_offered() {
        let check = read_release(&release("v0.2.46", &FULL), "0.2.45", "amd64", 7);
        assert_eq!(check.latest.as_deref(), Some("0.2.46"));
        assert!(check.newer, "{check:?}");
        assert_eq!(check.problem, None);
        assert_eq!(check.checked_at, 7);
        assert_eq!(
            check.notes_url.as_deref(),
            Some("https://github.com/testbild-media/OpenVideoServer/releases/tag/v0.2.46")
        );
    }

    #[test]
    fn the_same_version_is_not_offered_and_is_not_a_problem() {
        let check = read_release(&release("v0.2.45", &FULL), "0.2.45", "amd64", 0);
        assert!(!check.newer);
        assert_eq!(check.problem, None);
    }

    #[test]
    fn a_release_without_this_machines_package_or_its_checksum_is_not_offered() {
        // A release is visible before its last asset is uploaded, and one built
        // for another machine is not one to install here.
        let arm = read_release(&release("v0.2.46", &FULL), "0.2.45", "arm64", 0);
        assert!(!arm.newer);
        assert!(arm
            .problem
            .as_deref()
            .is_some_and(|problem| problem.contains("arm64")));

        let without_sum = read_release(&release("v0.2.46", &FULL[..3]), "0.2.45", "amd64", 0);
        assert!(!without_sum.newer);
        assert!(without_sum
            .problem
            .as_deref()
            .is_some_and(|problem| problem.contains("checksum")));
    }

    #[test]
    fn an_answer_that_is_not_a_release_says_so() {
        assert!(read_release("<html>", "0.2.45", "amd64", 0)
            .problem
            .is_some());
        let odd = read_release(&release("nightly", &FULL), "0.2.45", "amd64", 0);
        assert!(!odd.newer);
        assert!(odd.problem.is_some());
    }

    #[test]
    fn the_installers_progress_is_read_as_it_writes_it() {
        let written = r#"{"version":"0.2.46","stage":"installing","detail":"apt-get install","at":"2026-10-19T02:01:00Z"}"#;
        let progress = read_progress(written).expect("reads");
        assert_eq!(progress.stage, "installing");
        assert!(is_running(&progress));
        for done in ["installed", "failed", "refused"] {
            let finished = UpdateProgress {
                stage: done.to_owned(),
                ..progress.clone()
            };
            assert!(!is_running(&finished), "{done}");
        }
        assert_eq!(read_progress("{\"version\":"), None, "half written");
    }

    #[test]
    fn only_a_newer_version_is_ever_asked_for() {
        // Refused before anything is run, so neither of these touches the
        // machine running the test.
        assert!(!install("0.2.44", "0.2.45").accepted);
        assert!(!install("0.2.45", "0.2.45").accepted);
        assert!(!install("0.2.46; rm -rf /", "0.2.45").accepted);
        assert!(!install("../../0.2.46", "0.2.45").accepted);
    }

    #[test]
    fn the_unit_name_is_the_one_the_polkit_rule_allows() {
        assert_eq!(unit_for("0.2.46"), "ovs-update@0.2.46.service");
        let rule = include_str!("../../../deploy/polkit/51-openvideoserver-update.rules");
        assert!(
            rule.contains(r"/^ovs-update@[0-9]+\.[0-9]+\.[0-9]+\.service$/"),
            "the rule no longer allows the unit this starts"
        );
    }
}
