//! Workspace query DTOs. Cursors stay opaque `serde_json::Value` so a cursor
//! shape change in the Agent cannot stop pagination.

use serde::Deserialize;
use serde_json::Value;

#[derive(Clone, Debug, Deserialize)]
pub struct FilesPage {
    #[serde(default)]
    pub entries: Vec<FileEntry>,
    #[serde(default)]
    pub next_cursor: Option<Value>,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub scan_complete: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct FileEntry {
    pub path: String,
    #[serde(default)]
    pub kind: FileKind,
    #[serde(default)]
    pub size: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    #[default]
    File,
    Directory,
    Symlink,
    #[serde(other)]
    Other,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SearchPage {
    #[serde(default)]
    pub matches: Vec<SearchMatch>,
    #[serde(default)]
    pub next_cursor: Option<Value>,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub scan_complete: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SearchMatch {
    pub path: String,
    #[serde(default)]
    pub line_number: u64,
    #[serde(default)]
    pub line_text: String,
    #[serde(default)]
    pub match_byte_ranges: Vec<ByteRange>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
pub struct ByteRange {
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug, Deserialize)]
pub struct WorkspaceStatus {
    #[serde(default)]
    pub repo_available: bool,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub head_oid: Option<String>,
    #[serde(default)]
    pub detached: bool,
    #[serde(default)]
    pub staged: usize,
    #[serde(default)]
    pub unstaged: usize,
    #[serde(default)]
    pub untracked: usize,
    #[serde(default)]
    pub conflicted: usize,
}

impl WorkspaceStatus {
    pub fn is_clean(&self) -> bool {
        self.staged + self.unstaged + self.untracked + self.conflicted == 0
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct WorkspaceRead {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub truncated: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_cursor_shape_change_does_not_break_paging() {
        let old: FilesPage = serde_json::from_value(
            json!({"entries": [], "next_cursor": {"path": "a", "scope": "s"}}),
        )
        .unwrap();
        let new: FilesPage = serde_json::from_value(
            json!({"entries": [], "next_cursor": {"entry": 3, "scope": "s"}}),
        )
        .unwrap();
        let search: SearchPage = serde_json::from_value(json!({
            "matches": [], "next_cursor": {"path_index": 0, "path": "a", "line": 2,
                                           "line_byte_offset": 0, "scope": "s"}
        }))
        .unwrap();
        assert!(old.next_cursor.is_some());
        assert!(new.next_cursor.is_some());
        assert!(search.next_cursor.is_some());
    }

    #[test]
    fn clean_status_counts_every_section() {
        let status: WorkspaceStatus = serde_json::from_value(json!({
            "repo_available": true, "branch": "main", "untracked": 2
        }))
        .unwrap();
        assert_eq!(status.branch.as_deref(), Some("main"));
        assert!(!status.is_clean());
    }
}
