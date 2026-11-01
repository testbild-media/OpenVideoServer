//! Every lighting desk this server writes fixture types for.
//!
//! The owner, 2026-10-19: "Aktuell haben wir nur GrandMA1, aber später könnten
//! wir auch weitere Pult hinzufügen und die dort einpflegen." A desk is added
//! here, beside the module that writes its files, and the server's fixture
//! download carries it from then on: one folder per desk, named as the desk is.

/// One desk, and how to write its fixture types.
#[derive(Debug, Clone, Copy)]
pub struct Desk {
    /// What the desk is called, which is also its folder in the download.
    pub name: &'static str,
    /// Its fixture types, one file each, named as the desk accepts them.
    pub files: fn() -> Vec<DeskFile>,
}

/// One file a desk imports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeskFile {
    /// The file's name, in whatever form the desk insists on.
    pub name: String,
    /// What is in it, byte for byte.
    pub contents: Vec<u8>,
}

/// Every desk there are fixture types for.
pub const DESKS: [Desk; 4] = [GRANDMA1, GRANDMA2, DOT2, GRANDMA3];

const GRANDMA1: Desk = Desk {
    name: "grandMA1",
    files: grandma1_files,
};

/// The XML MA's Fixture Builder writes, which a grandMA2 imports (owner,
/// 2026-10-21).
const GRANDMA2: Desk = Desk {
    name: "grandMA2",
    files: grandma2_files,
};

/// The very same files: a dot2 reads a grandMA2's fixture types, from a folder
/// of its own on the stick.
const DOT2: Desk = Desk {
    name: "dot2",
    files: grandma2_files,
};

/// GDTF, which is what a grandMA3 imports of anybody's (owner, 2026-10-21).
const GRANDMA3: Desk = Desk {
    name: "grandMA3",
    files: grandma3_files,
};

/// A desk whose files are committed under `reference/` by `ovs-desk-files`.
#[derive(Debug, Clone, Copy)]
pub struct Committed {
    /// The desk.
    pub desk: Desk,
    /// Its folder under `reference/`.
    pub folder: &'static str,
    /// Whether a file in that folder is one of ours, so a retired one is found.
    pub is_ours: fn(&str) -> bool,
}

/// The desks after the grandMA1, whose files `ovs-desk-files` writes. The
/// grandMA1 has a writer of its own.
pub const LATER_DESKS: [Committed; 3] = [
    Committed {
        desk: GRANDMA2,
        folder: "grandma2",
        is_ours: crate::grandma2::is_fixture_file,
    },
    Committed {
        desk: DOT2,
        folder: "dot2",
        is_ours: crate::grandma2::is_fixture_file,
    },
    Committed {
        desk: GRANDMA3,
        folder: "grandma3",
        is_ours: crate::grandma3::is_fixture_file,
    },
];

fn grandma2_files() -> Vec<DeskFile> {
    crate::grandma2::fixture_files()
        .into_iter()
        .map(|file| DeskFile {
            name: file.file_name,
            contents: file.contents.into_bytes(),
        })
        .collect()
}

fn grandma3_files() -> Vec<DeskFile> {
    crate::grandma3::fixture_files()
        .into_iter()
        .map(|file| DeskFile {
            name: file.file_name,
            contents: file.contents,
        })
        .collect()
}

fn grandma1_files() -> Vec<DeskFile> {
    crate::grandma1::fixture_files()
        .into_iter()
        .map(|file| DeskFile {
            name: file.file_name,
            contents: file.contents.into_bytes(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_desk_has_files_and_a_name_that_is_a_folder() {
        for desk in &DESKS {
            assert!(!desk.name.is_empty());
            assert!(
                !desk.name.contains(['/', '\\', '.']),
                "{} cannot be a folder",
                desk.name
            );
            let files = (desk.files)();
            assert!(!files.is_empty(), "{} has no files", desk.name);
            for file in &files {
                assert!(!file.name.contains(['/', '\\']), "{}", file.name);
            }
        }
    }

    #[test]
    fn the_grandma1_files_are_the_committed_ones() {
        // The same bytes as `reference/grandma1/`, which the golden test holds
        // to the generator: what a desk gets from the server is what is in the
        // repository for the same build.
        let files = (DESKS[0].files)();
        let generated = crate::grandma1::fixture_files();
        assert_eq!(files.len(), generated.len());
        for (file, generated) in files.iter().zip(&generated) {
            assert_eq!(file.name, generated.file_name);
            assert_eq!(file.contents, generated.contents.as_bytes());
        }
    }
}
