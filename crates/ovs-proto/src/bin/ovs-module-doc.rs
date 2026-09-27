//! Writes the committed generator and effect document.
//!
//! Run from the workspace root:
//!
//! ```text
//! cargo run -p ovs-proto --bin ovs-module-doc
//! ```
//!
//! The golden test in `tests/golden_modules.rs` checks that what is committed
//! matches what this produces, so a manifest cannot change without the document
//! an operator reads changing with it.

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let target: PathBuf = std::env::args_os().nth(1).map_or_else(
        || PathBuf::from("reference").join("MODULES.md"),
        PathBuf::from,
    );

    if let Some(parent) = target.parent() {
        if let Err(error) = std::fs::create_dir_all(parent) {
            eprintln!("cannot create {}: {error}", parent.display());
            return ExitCode::FAILURE;
        }
    }

    if let Err(error) = std::fs::write(&target, ovs_proto::modules::markdown()) {
        eprintln!("cannot write {}: {error}", target.display());
        return ExitCode::FAILURE;
    }

    println!("wrote {}", target.display());
    ExitCode::SUCCESS
}
