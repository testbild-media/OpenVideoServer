//! Golden test for the committed JSON Schema documents.
//!
//! The schemas in `schemas/` are the contract the desktop client is built
//! against. This test fails whenever a change to the Rust types would change
//! that contract without the schemas being regenerated, which forces the change
//! to be deliberate and accompanied by a version bump and a migration
//! (house rule §13).

use std::path::PathBuf;

fn schema_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/ovs-proto.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("schemas")
}

const REGENERATE: &str =
    "run `cargo run -p ovs-proto --bin ovs-schema-gen -- schemas` and commit the result";

#[test]
fn committed_schemas_match_the_current_types() {
    let dir = schema_dir();
    let mut mismatched = Vec::new();

    for schema in ovs_proto::schema::generate_all() {
        let path = dir.join(schema.file_name);
        match std::fs::read_to_string(&path) {
            Ok(committed) if committed.replace("\r\n", "\n") == schema.contents => {}
            Ok(_) => mismatched.push(format!(
                "{} differs from the current types",
                schema.file_name
            )),
            Err(error) => {
                mismatched.push(format!("{} could not be read: {error}", schema.file_name));
            }
        }
    }

    assert!(
        mismatched.is_empty(),
        "the shared schema contract changed:\n  - {}\n\nIf this change is intended, {REGENERATE}. \
         If it alters a persisted contract, bump the matching version in `version.rs` and add a \
         migration in the same commit.",
        mismatched.join("\n  - ")
    );
}

#[test]
fn no_orphaned_schema_files_are_committed() {
    let dir = schema_dir();
    let expected: std::collections::BTreeSet<String> = ovs_proto::schema::generate_all()
        .into_iter()
        .map(|schema| schema.file_name.to_owned())
        .collect();

    let entries = std::fs::read_dir(&dir).expect("the schema directory exists");
    let mut orphans = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".schema.json") && !expected.contains(&name) {
            orphans.push(name);
        }
    }

    assert!(
        orphans.is_empty(),
        "these schema files are no longer generated and should be deleted: {}",
        orphans.join(", ")
    );
}
