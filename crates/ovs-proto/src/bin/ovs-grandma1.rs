//! Writes the committed grandMA1 fixture types.
//!
//! Run from the workspace root:
//!
//! ```text
//! cargo run -p ovs-proto --bin ovs-grandma1
//! cargo run -p ovs-proto --bin ovs-grandma1 -- somewhere/else
//! ```
//!
//! One `.TXT` per personality, into `reference/grandma1/` unless told
//! otherwise. A file of ours that the catalogue no longer produces is removed,
//! so the directory is always exactly what a desk should import. The golden
//! test in `tests/golden_grandma1.rs` checks that what is committed matches.

use std::path::PathBuf;
use std::process::ExitCode;

use ovs_proto::grandma1::{fixture_files, is_fixture_file};

fn main() -> ExitCode {
    let target: PathBuf = std::env::args_os().nth(1).map_or_else(
        || PathBuf::from("reference").join("grandma1"),
        PathBuf::from,
    );

    if let Err(error) = std::fs::create_dir_all(&target) {
        eprintln!("cannot create {}: {error}", target.display());
        return ExitCode::FAILURE;
    }

    let files = fixture_files();

    // Whatever of ours is there and is no longer made goes first, so an
    // import of the whole directory never brings back a retired module — nor
    // a file under the name the desk refused, before the names were capitals.
    if let Ok(listed) = std::fs::read_dir(&target) {
        for entry in listed.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if is_fixture_file(&name) && !files.iter().any(|file| file.file_name == name) {
                match std::fs::remove_file(entry.path()) {
                    Ok(()) => println!("removed {name}"),
                    Err(error) => {
                        eprintln!("cannot remove {name}: {error}");
                        return ExitCode::FAILURE;
                    }
                }
            }
        }
    }

    for file in &files {
        let path = target.join(&file.file_name);
        if let Err(error) = std::fs::write(&path, &file.contents) {
            eprintln!("cannot write {}: {error}", path.display());
            return ExitCode::FAILURE;
        }
        println!("{:<30} {:>6} bytes", file.name, file.contents.len());
    }

    println!("{} fixture types into {}", files.len(), target.display());
    ExitCode::SUCCESS
}
