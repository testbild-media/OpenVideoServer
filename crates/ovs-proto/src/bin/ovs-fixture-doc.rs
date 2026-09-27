//! Writes the committed DMX personality document.
//!
//! Run from the workspace root:
//!
//! ```text
//! cargo run -p ovs-proto --bin ovs-fixture-doc
//! ```
//!
//! The golden test in `tests/golden_fixtures.rs` checks that what is committed
//! matches what this produces, so a personality cannot change without the
//! document a console is programmed from changing with it.

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let target: PathBuf = std::env::args_os().nth(1).map_or_else(
        || PathBuf::from("reference").join("FIXTURES.md"),
        PathBuf::from,
    );

    if let Some(parent) = target.parent() {
        if let Err(error) = std::fs::create_dir_all(parent) {
            eprintln!("cannot create {}: {error}", parent.display());
            return ExitCode::FAILURE;
        }
    }

    if let Err(error) = std::fs::write(&target, ovs_proto::fixtures::markdown()) {
        eprintln!("cannot write {}: {error}", target.display());
        return ExitCode::FAILURE;
    }

    println!("wrote {}", target.display());
    ExitCode::SUCCESS
}
