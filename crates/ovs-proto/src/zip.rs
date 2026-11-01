//! A ZIP that stores and does not compress.
//!
//! Two things are ZIPs here: the server's download of every desk's fixture
//! types, and a GDTF fixture type itself, which is a ZIP with its description
//! in it. Written here rather than by a library: a ZIP that only stores is, for
//! each file, a header, the bytes and an entry in a directory at the end.
//!
//! **Stored is what a grandMA3 needs, not only what is simplest.** Its release
//! notes for 2.4 fix "It was not possible to import compressed GDTF zip files",
//! so a console before 2.4 imports a GDTF only when nothing in it is
//! compressed — which is how GDTF Builder writes them anyway.
//!
//! Checked once, 2026-10-19, against three readers that are not this one:
//! Python's `zipfile` (every CRC good, every file byte for byte), .NET's
//! `ZipFile` and Windows' own `Expand-Archive`.

/// 1 January 1980, midnight: the first moment a ZIP can say, and the same in
/// every build, so two archives of one version are the same bytes.
const DOS_DATE: u16 = 1 << 5 | 1;
const DOS_TIME: u16 = 0;

/// The version of the format a stored file needs, 2.0, as every tool writes it.
const VERSION: u16 = 20;

/// A ZIP holding `entries` as they are, in order, or nothing if one is too big
/// for the format's 32-bit sizes — which nothing written here ever is.
#[must_use]
pub fn stored(entries: &[(String, Vec<u8>)]) -> Option<Vec<u8>> {
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

/// Reads a stored ZIP the way an unzipper does — from the end, through the
/// directory, to each file — checking every CRC on the way. For tests, and for
/// anything that has to look inside an archive this crate wrote.
///
/// # Panics
///
/// When the archive is not one this module would write.
#[must_use]
pub fn read_stored(zip: &[u8]) -> Vec<(String, Vec<u8>)> {
    let u16_at = |at: usize| u16::from_le_bytes([zip[at], zip[at + 1]]);
    let u32_at = |at: usize| u32::from_le_bytes([zip[at], zip[at + 1], zip[at + 2], zip[at + 3]]);

    let end = zip.len() - 22;
    assert_eq!(u32_at(end), 0x0605_4b50, "no end of directory");
    let count = usize::from(u16_at(end + 10));
    let mut at = u32_at(end + 16) as usize;
    assert_eq!(
        at + u32_at(end + 12) as usize,
        end,
        "the directory does not end where the end record starts"
    );

    let mut files = Vec::new();
    for _ in 0..count {
        assert_eq!(u32_at(at), 0x0201_4b50, "not a directory entry");
        let crc = u32_at(at + 16);
        let size = u32_at(at + 24) as usize;
        let name_length = usize::from(u16_at(at + 28));
        let local = u32_at(at + 42) as usize;
        let name = String::from_utf8(zip[at + 46..at + 46 + name_length].to_vec())
            .unwrap_or_else(|_| panic!("a name that is not UTF-8"));
        at += 46 + name_length;

        assert_eq!(u32_at(local), 0x0403_4b50, "{name}: no local header");
        assert_eq!(u16_at(local + 8), 0, "{name}: not stored");
        let start = local + 30 + usize::from(u16_at(local + 26));
        let contents = zip[start..start + size].to_vec();
        assert_eq!(crc32fast::hash(&contents), crc, "{name}: CRC");
        files.push((name, contents));
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reader_finds_what_was_put_in_and_nothing_else() {
        // A reader that cannot be fooled by the writer: two files of known
        // contents, one empty, and their CRCs worked out independently.
        let zip = stored(&[
            ("a/one.txt".to_owned(), b"123456789".to_vec()),
            ("a/empty.txt".to_owned(), Vec::new()),
        ])
        .expect("written");
        // The check value every CRC-32 implementation publishes for "123456789".
        assert_eq!(
            u32::from_le_bytes([zip[14], zip[15], zip[16], zip[17]]),
            0xCBF4_3926
        );
        assert_eq!(
            read_stored(&zip),
            vec![
                ("a/one.txt".to_owned(), b"123456789".to_vec()),
                ("a/empty.txt".to_owned(), Vec::new()),
            ]
        );
    }

    #[test]
    fn the_same_entries_are_the_same_bytes() {
        let entries = vec![("x".to_owned(), b"y".to_vec())];
        assert_eq!(stored(&entries), stored(&entries));
    }
}
