//! Golden test for the committed fixture types of the desks after the grandMA1.
//!
//! `reference/<desk>/` is what a desk imports. A fixture type that disagrees
//! with the personality the server decodes moves the wrong thing from the right
//! fader, so the files are generated, committed and checked here rather than
//! kept by hand. The grandMA1's are checked in `golden_grandma1.rs`.

use std::path::PathBuf;

use ovs_proto::desks::LATER_DESKS;

const REGENERATE: &str = "run `cargo run -p ovs-proto --bin ovs-desk-files` and commit the result";

fn directory(folder: &str) -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/ovs-proto.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("reference")
        .join(folder)
}

#[test]
fn every_committed_fixture_type_matches_the_published_personalities() {
    for desk in &LATER_DESKS {
        for file in (desk.desk.files)() {
            let path = directory(desk.folder).join(&file.name);
            let committed = std::fs::read(&path).unwrap_or_else(|error| {
                panic!(
                    "{} could not be read: {error}; {REGENERATE}",
                    path.display()
                )
            });
            // Byte for byte: git keeps these as they are (`binary` for a
            // GDTF, which is a ZIP).
            assert!(
                committed == file.contents,
                "{}/{} no longer matches what this build publishes; {REGENERATE}",
                desk.folder,
                file.name
            );
        }
    }
}

#[test]
fn nothing_is_committed_that_no_personality_makes() {
    // A file left behind by a retired module would be imported with the rest
    // and patched by somebody who had no way to know.
    for desk in &LATER_DESKS {
        let made: Vec<String> = (desk.desk.files)()
            .into_iter()
            .map(|file| file.name)
            .collect();
        let listed = std::fs::read_dir(directory(desk.folder)).unwrap_or_else(|error| {
            panic!(
                "{}: {error}; {REGENERATE}",
                directory(desk.folder).display()
            )
        });
        for entry in listed {
            let name = entry.expect("a directory entry").file_name();
            let name = name.to_string_lossy().to_string();
            if (desk.is_ours)(&name) {
                assert!(
                    made.contains(&name),
                    "{}/{name} is committed and nothing makes it; {REGENERATE}",
                    desk.folder
                );
            }
        }
    }
}
