//! Writes every desk's fixture types as the one ZIP a server hands out.
//!
//! ```text
//! ovs-fixtures-zip <directory>
//! ```
//!
//! Run by `tools/package.sh` and `tools/deploy.sh`, into the directory the
//! server offers fixture types from, under the name the status page shows:
//! `OpenVideoServer-<version>-fixtures.zip`, this build's version. The server
//! writes nothing itself — it hands out the file that is there, as it hands out
//! the client (owner, 2026-11-09). Anything else of ours in the directory is
//! removed first, so a server updated in place offers this build's fixture
//! types and not the last one's as well.

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let Some(directory) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: ovs-fixtures-zip <directory>");
        return ExitCode::from(2);
    };

    let Some(zip) = ovs_proto::desks::archive() else {
        eprintln!("the fixture types do not fit in a ZIP");
        return ExitCode::FAILURE;
    };
    let name = ovs_proto::desks::archive_name(env!("CARGO_PKG_VERSION"));

    if let Err(error) = std::fs::create_dir_all(&directory) {
        eprintln!("cannot create {}: {error}", directory.display());
        return ExitCode::FAILURE;
    }
    if let Ok(listed) = std::fs::read_dir(&directory) {
        for entry in listed.flatten() {
            let found = entry.file_name().to_string_lossy().to_string();
            if found != name
                && found.starts_with("OpenVideoServer-")
                && found.ends_with("-fixtures.zip")
            {
                if let Err(error) = std::fs::remove_file(entry.path()) {
                    eprintln!("cannot remove {found}: {error}");
                    return ExitCode::FAILURE;
                }
                println!("removed {found}");
            }
        }
    }

    let path = directory.join(&name);
    if let Err(error) = std::fs::write(&path, &zip) {
        eprintln!("cannot write {}: {error}", path.display());
        return ExitCode::FAILURE;
    }
    println!("{} {} bytes", path.display(), zip.len());
    ExitCode::SUCCESS
}
