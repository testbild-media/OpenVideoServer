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
//! Written here rather than by a library: a ZIP that only stores is, for each
//! file, a header, the bytes and an entry in a directory at the end. The files
//! are a few kilobytes of text, so compressing them would save nothing anybody
//! waits for, and storing them keeps every byte exactly as the desk wrote it.
//! Checked once, 2026-10-19, against three readers that are not this one:
//! Python's `zipfile` (every CRC good, every file byte for byte the committed
//! one), .NET's `ZipFile` and Windows' own `Expand-Archive`.

use std::sync::OnceLock;

use ovs_proto::desks::DESKS;
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

/// 1 January 1980, midnight: the first moment a ZIP can say, and the same in
/// every build, so two downloads of one version are the same bytes.
const DOS_DATE: u16 = 1 << 5 | 1;
const DOS_TIME: u16 = 0;

/// The version of the format a stored file needs, 2.0, as every tool writes it.
const VERSION: u16 = 20;

/// A ZIP holding `entries` as they are, or nothing if one is too big for the
/// format's 32-bit sizes — which a fixture type never is.
fn stored_zip(entries: &[(String, Vec<u8>)]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut directory = Vec::new();

    for (name, contents) in entries {
        let offset = u32::try_from(out.len()).ok()?;
        let size = u32::try_from(contents.len()).ok()?;
        let name_length = u16::try_from(name.len()).ok()?;
        let crc = crc32fast::hash(contents);

        // The local header, then the file.
        out.extend_from_slice(&0x0403_4b50_u32.to_le_bytes());
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&0_u16.to_le_bytes()); // flags
        out.extend_from_slice(&0_u16.to_le_bytes()); // stored
        out.extend_from_slice(&DOS_TIME.to_le_bytes());
        out.extend_from_slice(&DOS_DATE.to_le_bytes());
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes()); // compressed
        out.extend_from_slice(&size.to_le_bytes()); // uncompressed
        out.extend_from_slice(&name_length.to_le_bytes());
        out.extend_from_slice(&0_u16.to_le_bytes()); // no extra field
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(contents);

        // Its entry in the directory at the end.
        directory.extend_from_slice(&0x0201_4b50_u32.to_le_bytes());
        directory.extend_from_slice(&VERSION.to_le_bytes()); // made by
        directory.extend_from_slice(&VERSION.to_le_bytes()); // needed
        directory.extend_from_slice(&0_u16.to_le_bytes()); // flags
        directory.extend_from_slice(&0_u16.to_le_bytes()); // stored
        directory.extend_from_slice(&DOS_TIME.to_le_bytes());
        directory.extend_from_slice(&DOS_DATE.to_le_bytes());
        directory.extend_from_slice(&crc.to_le_bytes());
        directory.extend_from_slice(&size.to_le_bytes());
        directory.extend_from_slice(&size.to_le_bytes());
        directory.extend_from_slice(&name_length.to_le_bytes());
        directory.extend_from_slice(&0_u16.to_le_bytes()); // no extra field
        directory.extend_from_slice(&0_u16.to_le_bytes()); // no comment
        directory.extend_from_slice(&0_u16.to_le_bytes()); // first disc
        directory.extend_from_slice(&0_u16.to_le_bytes()); // internal attributes
        directory.extend_from_slice(&0_u32.to_le_bytes()); // external attributes
        directory.extend_from_slice(&offset.to_le_bytes());
        directory.extend_from_slice(name.as_bytes());
    }

    let directory_offset = u32::try_from(out.len()).ok()?;
    let directory_size = u32::try_from(directory.len()).ok()?;
    let count = u16::try_from(entries.len()).ok()?;
    out.extend_from_slice(&directory);

    // The end of the directory, which is where a reader starts.
    out.extend_from_slice(&0x0605_4b50_u32.to_le_bytes());
    out.extend_from_slice(&0_u16.to_le_bytes()); // this disc
    out.extend_from_slice(&0_u16.to_le_bytes()); // the directory's disc
    out.extend_from_slice(&count.to_le_bytes()); // entries on this disc
    out.extend_from_slice(&count.to_le_bytes()); // entries in all
    out.extend_from_slice(&directory_size.to_le_bytes());
    out.extend_from_slice(&directory_offset.to_le_bytes());
    out.extend_from_slice(&0_u16.to_le_bytes()); // no comment
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u16_at(bytes: &[u8], at: usize) -> u16 {
        u16::from_le_bytes([bytes[at], bytes[at + 1]])
    }

    fn u32_at(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
    }

    /// Reads a stored ZIP the way an unzipper does: from the end, through the
    /// directory, to each file — and checks each file's CRC on the way.
    fn read(zip: &[u8]) -> Vec<(String, Vec<u8>)> {
        let end = zip.len() - 22;
        assert_eq!(u32_at(zip, end), 0x0605_4b50, "no end of directory");
        let count = usize::from(u16_at(zip, end + 10));
        let mut at = u32_at(zip, end + 16) as usize;
        assert_eq!(
            at + u32_at(zip, end + 12) as usize,
            end,
            "the directory does not end where the end record starts"
        );

        let mut files = Vec::new();
        for _ in 0..count {
            assert_eq!(u32_at(zip, at), 0x0201_4b50, "not a directory entry");
            let crc = u32_at(zip, at + 16);
            let size = u32_at(zip, at + 24) as usize;
            let name_length = usize::from(u16_at(zip, at + 28));
            let local = u32_at(zip, at + 42) as usize;
            let name =
                String::from_utf8(zip[at + 46..at + 46 + name_length].to_vec()).expect("a name");
            at += 46 + name_length;

            assert_eq!(u32_at(zip, local), 0x0403_4b50, "{name}: no local header");
            assert_eq!(u16_at(zip, local + 8), 0, "{name}: not stored");
            let start = local + 30 + usize::from(u16_at(zip, local + 26));
            let contents = zip[start..start + size].to_vec();
            assert_eq!(crc32fast::hash(&contents), crc, "{name}: CRC");
            files.push((name, contents));
        }
        files
    }

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
    fn a_reader_finds_what_was_put_in_and_nothing_else() {
        // A reader that cannot be fooled by the writer: two files of known
        // contents, one empty, and their CRCs worked out independently.
        let zip = stored_zip(&[
            ("a/one.txt".to_owned(), b"123456789".to_vec()),
            ("a/empty.txt".to_owned(), Vec::new()),
        ])
        .expect("written");
        // The check value every CRC-32 implementation publishes for "123456789".
        assert_eq!(u32_at(&zip, 14), 0xCBF4_3926);
        assert_eq!(
            read(&zip),
            vec![
                ("a/one.txt".to_owned(), b"123456789".to_vec()),
                ("a/empty.txt".to_owned(), Vec::new()),
            ]
        );
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
