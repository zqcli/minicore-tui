//! `workspace.files` DTOs for the `@` picker.

use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
pub struct FilesPage {
    pub entries: Vec<FileEntry>,
    /// The page is a partial view: more entries may match.
    pub truncated: bool,
}

impl FilesPage {
    /// The paths of the regular files, the entries the `@` picker offers.
    pub fn files(self) -> Vec<String> {
        self.entries
            .into_iter()
            .filter(|entry| entry.kind == FileKind::File)
            .map(|entry| entry.path)
            .collect()
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct FileEntry {
    pub path: String,
    pub kind: FileKind,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    File,
    Directory,
    Symlink,
    #[serde(other)]
    Other,
}
