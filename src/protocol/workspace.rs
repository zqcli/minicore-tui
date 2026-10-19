//! `workspace.files` DTOs for the `@` picker.

use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
pub struct FilesPage {
    pub entries: Vec<FileEntry>,
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
