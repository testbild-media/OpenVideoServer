//! Golden test for the committed DMX personality document.
//!
//! `reference/FIXTURES.md` is what an operator programmes a lighting console from. A
//! printed channel list that disagrees with the machine is worse than none,
//! because it is believed — so the document is generated, committed and checked
//! here rather than maintained by hand.

use std::path::PathBuf;

const REGENERATE: &str = "run `cargo run -p ovs-proto --bin ovs-fixture-doc` and commit the result";

#[test]
fn the_committed_personality_document_matches_the_published_personalities() {
    // CARGO_MANIFEST_DIR is crates/ovs-proto.
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("reference")
        .join("FIXTURES.md");

    let committed = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "{} could not be read: {error}; {REGENERATE}",
            path.display()
        )
    });

    assert_eq!(
        committed.replace("\r\n", "\n"),
        ovs_proto::fixtures::markdown(),
        "the personality document no longer matches the personalities this build publishes; {REGENERATE}"
    );
}
