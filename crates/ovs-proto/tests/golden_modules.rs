//! Golden test for the committed generator and effect document.
//!
//! `reference/MODULES.md` is what somebody reads to find out what this build offers
//! and what each thing takes. A list that disagrees with the machine is worse
//! than none, because it is believed — so the document is generated, committed
//! and checked here rather than maintained by hand, exactly as the personality
//! document is.

use std::path::PathBuf;

const REGENERATE: &str = "run `cargo run -p ovs-proto --bin ovs-module-doc` and commit the result";

#[test]
fn the_committed_module_document_matches_the_published_manifests() {
    // CARGO_MANIFEST_DIR is crates/ovs-proto.
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("reference")
        .join("MODULES.md");

    let committed = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "{} could not be read: {error}; {REGENERATE}",
            path.display()
        )
    });

    assert_eq!(
        committed.replace("\r\n", "\n"),
        ovs_proto::modules::markdown(),
        "the committed document is out of date; {REGENERATE}"
    );
}
