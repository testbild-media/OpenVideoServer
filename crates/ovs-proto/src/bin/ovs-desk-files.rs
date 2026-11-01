//! Writes the committed fixture types for the desks after the grandMA1.
//!
//! Run from the workspace root:
//!
//! ```text
//! cargo run -p ovs-proto --bin ovs-desk-files
//! cargo run -p ovs-proto --bin ovs-desk-files -- somewhere/else
//! ```
//!
//! One folder per desk under `reference/` unless told otherwise: `grandma2`,
//! `dot2` and `grandma3`. A file of ours that the catalogue no longer produces
//! is removed, so each folder is always exactly what a desk should import. The
//! golden test in `tests/golden_desks.rs` checks that what is committed
//! matches. The grandMA1's files have a writer of their own, `ovs-grandma1`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use ovs_proto::desks::{DeskFile, LATER_DESKS};

fn main() -> ExitCode {
    let root: PathBuf = std::env::args_os()
        .nth(1)
        .map_or_else(|| PathBuf::from("reference"), PathBuf::from);

    for desk in &LATER_DESKS {
        let target = root.join(desk.folder);
        if let Err(message) = write(&target, &(desk.desk.files)(), desk.is_ours) {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}

fn write(target: &Path, files: &[DeskFile], is_ours: fn(&str) -> bool) -> Result<(), String> {
    std::fs::create_dir_all(target)
        .map_err(|error| format!("cannot create {}: {error}", target.display()))?;

    // Whatever of ours is there and is no longer made goes first, so an import
    // of the whole folder never brings back a retired module.
    if let Ok(listed) = std::fs::read_dir(target) {
        for entry in listed.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if is_ours(&name) && !files.iter().any(|file| file.name == name) {
                std::fs::remove_file(entry.path())
                    .map_err(|error| format!("cannot remove {name}: {error}"))?;
                println!("removed {name}");
            }
        }
    }

    for file in files {
        let path = target.join(&file.name);
        std::fs::write(&path, &file.contents)
            .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
        println!("{:<50} {:>6} bytes", file.name, file.contents.len());
    }
    println!("{} fixture types into {}", files.len(), target.display());
    Ok(())
}
