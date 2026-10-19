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
pub const DESKS: [Desk; 1] = [Desk {
    name: "grandMA1",
    files: grandma1_files,
}];

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
