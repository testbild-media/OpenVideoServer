//! Every desk's fixture types, as one ZIP from the status page.
//!
//! The owner, 2026-10-19: "können wir auf dem Server der Admin GUI/Status Seite
//! noch alle Fixtures als Zip mitliefern? Aktuell haben wir nur GrandMA1, aber
//! später könnten wir auch weitere Pult hinzufügen und die dort einpflegen."
//!
//! # Made by the server it is for
//!
//! Generated here, from the same catalogue the server decodes a desk with,
//! rather than shipped beside it: the fixture types a desk gets from this
//! machine are the ones for the version this machine is running, whatever that
//! is. One folder per desk in [`ovs_proto::desks::DESKS`], so a desk added there
//! is in the download without anything here changing.
//!
//! # Stored, not compressed
//!
//! [`ovs_proto::zip::stored`]: the files are a few kilobytes, so compressing
//! them would save nothing anybody waits for, and storing them keeps every byte
//! exactly as the desk wrote it — a grandMA3's GDTF file inside included, which
//! is itself a stored ZIP.

use std::sync::OnceLock;

use ovs_proto::desks::DESKS;
use ovs_proto::zip::stored as stored_zip;
use serde::Serialize;

/// What the status page says about the download.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Summary {
    /// The file a browser saves it as.
    pub file: String,
    /// How big it is, in bytes.
    pub bytes: u64,
    /// Which desks it has fixture types for.
    pub desks: Vec<DeskSummary>,
}

/// One desk in the download.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeskSummary {
    /// What the desk is called, which is also its folder.
    pub name: &'static str,
    /// How many fixture types there are for it.
    pub fixture_types: usize,
}

/// The archive, built once: what is in it is fixed for a build.
static ARCHIVE: OnceLock<Option<Vec<u8>>> = OnceLock::new();

/// The whole download, or nothing if it could not be written.
#[must_use]
pub fn archive() -> Option<&'static [u8]> {
    ARCHIVE.get_or_init(|| stored_zip(&entries())).as_deref()
}

/// What a browser saves it as: the server's version in the name, so a stick
/// with three of these on it says which is which.
#[must_use]
pub fn file_name(version: &str) -> String {
    // The version is ours, but it goes into a header: anything that is not
    // plainly part of a version is left out rather than trusted.
    let version: String = version
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || "._-+~".contains(*character))
        .collect();
    format!("OpenVideoServer-{version}-fixtures.zip")
}

/// What the status page says about the download.
#[must_use]
pub fn summary(version: &str) -> Summary {
    Summary {
        file: file_name(version),
        bytes: archive().map_or(0, |bytes| bytes.len() as u64),
        desks: DESKS
            .iter()
            .map(|desk| DeskSummary {
                name: desk.name,
                fixture_types: (desk.files)().len(),
            })
            .collect(),
    }
}

/// Every file in the download, under its desk's folder.
fn entries() -> Vec<(String, Vec<u8>)> {
    DESKS
        .iter()
        .flat_map(|desk| {
            (desk.files)()
                .into_iter()
                .map(move |file| (format!("{}/{}", desk.name, file.name), file.contents))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ovs_proto::zip::read_stored as read;

    #[test]
    fn every_desks_files_are_in_their_folder_byte_for_byte() {
        let zip = archive().expect("written");
        let files = read(zip);
        let mut expected = Vec::new();
        for desk in &DESKS {
            for file in (desk.files)() {
                expected.push((format!("{}/{}", desk.name, file.name), file.contents));
            }
        }
        assert_eq!(files, expected);
        assert!(files
            .iter()
            .any(|(name, _)| name == "grandMA1/TESTBILD MEDIA@OVS MASTER.TXT"));
    }

    #[test]
    fn the_same_build_writes_the_same_bytes() {
        assert_eq!(stored_zip(&entries()), stored_zip(&entries()));
    }

    #[test]
    fn the_file_name_carries_the_version_and_nothing_that_could_break_a_header() {
        assert_eq!(file_name("0.2.44"), "OpenVideoServer-0.2.44-fixtures.zip");
        assert_eq!(
            file_name("0.2.44\"\r\nX: y"),
            "OpenVideoServer-0.2.44Xy-fixtures.zip"
        );
    }

    #[test]
    fn the_summary_names_every_desk_and_how_many_types_it_has() {
        let summary = summary("0.2.44");
        assert_eq!(summary.desks.len(), DESKS.len());
        assert_eq!(summary.desks[0].name, "grandMA1");
        assert_eq!(
            summary.desks[0].fixture_types,
            ovs_proto::grandma1::fixture_files().len()
        );
        assert_eq!(summary.bytes, archive().expect("written").len() as u64);
    }
}
