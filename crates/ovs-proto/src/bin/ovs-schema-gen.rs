//! Writes the committed JSON Schema documents, domain and protocol.
//!
//! Run from the workspace root:
//!
//! ```text
//! cargo run -p ovs-proto --bin ovs-schema-gen -- schemas
//! ```
//!
//! The golden test in `tests/golden_schema.rs` checks that what is committed
//! matches what this produces, so this binary is the only sanctioned way to
//! change the files in `schemas/`.

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let target: PathBuf = std::env::args_os()
        .nth(1)
        .map_or_else(|| PathBuf::from("schemas"), PathBuf::from);

    if let Err(error) = std::fs::create_dir_all(&target) {
        eprintln!("cannot create {}: {error}", target.display());
        return ExitCode::FAILURE;
    }

    let mut written = 0usize;
    for schema in ovs_proto::schema::generate_all() {
        let path = target.join(schema.file_name);
        if let Err(error) = std::fs::write(&path, schema.contents.as_bytes()) {
            eprintln!("cannot write {}: {error}", path.display());
            return ExitCode::FAILURE;
        }
        written += 1;
    }

    println!("wrote {written} schema documents to {}", target.display());
    ExitCode::SUCCESS
}
