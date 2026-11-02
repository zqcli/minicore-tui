//! Workspace file listings and Git observations.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize)]
pub struct WorkspaceStatus {
    pub repo_available: bool,
    pub branch: Option<String>,
    pub complete: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct WorkspaceStatusEntry {
    pub path: String,
    pub kind: WorkspaceStatusEntryKind,
    pub index_status: Option<String>,
    pub worktree_status: Option<String>,
    pub original_path: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceStatusEntryKind {
    Ordinary,
    Renamed,
    Unmerged,
    Untracked,
}

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
