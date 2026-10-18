//! Golden test for the committed grandMA1 fixture types.
//!
//! `reference/grandma1/` is what a desk imports. A fixture type that disagrees
//! with the personality the server decodes moves the wrong thing from the right
//! fader, so the files are generated, committed and checked here rather than
//! kept by hand.

use std::path::PathBuf;

use ovs_proto::grandma1::{fixture_files, is_fixture_file};

const REGENERATE: &str = "run `cargo run -p ovs-proto --bin ovs-grandma1` and commit the result";

fn directory() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/ovs-proto.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("reference")
        .join("grandma1")
}

#[test]
fn every_committed_fixture_type_matches_the_published_personalities() {
    for file in fixture_files() {
        let path = directory().join(&file.file_name);
        let committed = std::fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!(
                "{} could not be read: {error}; {REGENERATE}",
                path.display()
            )
        });
        // Compared without line ends: git keeps these CRLF on checkout, and a
        // checkout that did not would still be the same fixture type.
        assert_eq!(
            committed.replace("\r\n", "\n"),
            file.contents.replace("\r\n", "\n"),
            "{} no longer matches what this build publishes; {REGENERATE}",
            file.file_name
        );
    }
}

#[test]
fn nothing_is_committed_that_no_personality_makes() {
    // A file left behind by a retired module would be imported with the rest
    // and patched by somebody who had no way to know — and one under the old
    // lower-case name with a dot in it is a file the desk refuses outright.
    let made: Vec<String> = fixture_files()
        .into_iter()
        .map(|file| file.file_name)
        .collect();
    let listed = std::fs::read_dir(directory())
        .unwrap_or_else(|error| panic!("{}: {error}; {REGENERATE}", directory().display()));
    for entry in listed {
        let name = entry.expect("a directory entry").file_name();
        let name = name.to_string_lossy().to_string();
        if is_fixture_file(&name) {
            assert!(
                made.contains(&name),
                "{name} is committed and nothing makes it; {REGENERATE}"
            );
        }
    }
}
