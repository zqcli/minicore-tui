//! `changes.list` / `changes.diff` DTOs. Change references are opaque: a
//! `workspace:` token is minted by the Agent and must be passed back verbatim.

use std::collections::HashSet;

use serde::Deserialize;
use serde_json::Value;

#[derive(Clone, Debug, Deserialize)]
pub struct ChangeList {
    pub records: Vec<ChangeRecord>,
    pub warnings: Vec<String>,
}

impl ChangeList {
    /// Distinct paths on this page: a turn scope has one record per mutating
    /// tool call, and "files changed" counts files, not calls.
    pub fn files(&self) -> usize {
        let paths: HashSet<&str> = self.records.iter().map(|record| &*record.path).collect();
        paths.len()
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct ChangeRecord {
    pub change_ref: String,
    pub path: String,
    pub kind: ChangeKind,
    pub details_available: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
    Conflict,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Deserialize)]
pub struct DiffPage {
    /// The tool call that made a `tool:` change; absent for workspace records.
    pub tool_ref: Option<super::ToolRef>,
    pub hunks: Vec<DiffHunk>,
    pub next_cursor: Option<Value>,
    pub complete: bool,
    pub binary: bool,
    pub availability: Option<String>,
    pub stale: bool,
    pub truncated: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct DiffHunk {
    pub old_start: u64,
    pub old_count: u64,
    pub new_start: u64,
    pub new_count: u64,
    pub lines: Vec<DiffLine>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct DiffLine {
    pub kind: LineKind,
    /// Source line numbers, shared by every fragment of one long line.
    pub old_index: Option<u64>,
    pub new_index: Option<u64>,
    pub text: String,
    pub line_complete: bool,
}

/// Counts added and removed logical lines across every page of one diff.
pub fn count_lines(pages: &[DiffPage]) -> (usize, usize) {
    let mut seen: HashSet<(u64, u64)> = HashSet::new();
    let (mut added, mut removed) = (0, 0);
    for page in pages {
        for hunk in &page.hunks {
            for line in &hunk.lines {
                // Only the last fragment of a long line is complete, and a page
                // may repeat it, so complete fragments are deduplicated.
                if !line.line_complete {
                    continue;
                }
                if let (Some(old), Some(new)) = (line.old_index, line.new_index)
                    && !seen.insert((old, new))
                {
                    continue;
                }
                match line.kind {
                    LineKind::Added => added += 1,
                    LineKind::Removed => removed += 1,
                    LineKind::Context | LineKind::Unknown => {}
                }
            }
        }
    }
    (added, removed)
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LineKind {
    Context,
    Added,
    Removed,
    #[serde(other)]
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn page(lines: Value) -> DiffPage {
        serde_json::from_value(json!({
            "change_ref": "tool:a", "path": "a.rs", "kind": "modified", "complete": true,
            "binary": false, "stale": false, "truncated": false, "availability": "available",
            "hunks": [{"old_start": 1, "old_count": 1, "new_start": 1, "new_count": 1,
                       "lines": lines}]
        }))
        .unwrap()
    }

    fn list(paths: &[&str]) -> ChangeList {
        let records: Vec<Value> = paths
            .iter()
            .enumerate()
            .map(|(index, path)| {
                json!({"change_ref": format!("tool:{index}"), "path": path, "kind": "modified",
                       "details_available": true})
            })
            .collect();
        serde_json::from_value(json!({"records": records, "total": records.len(), "warnings": []}))
            .unwrap()
    }

    #[test]
    fn diff_lines_decode_without_optional_indexes() {
        let page = page(json!([{"kind": "added", "text": "+x", "line_complete": true}]));
        assert_eq!(page.hunks[0].lines[0].kind, LineKind::Added);
        assert_eq!(page.hunks[0].lines[0].old_index, None);
    }

    #[test]
    fn repeated_calls_on_one_path_are_one_changed_file() {
        assert_eq!(list(&["app.py", "app.py", "app.py"]).files(), 1);
    }

    #[test]
    fn distinct_paths_are_counted_separately() {
        assert_eq!(list(&["app.py", "tests/test_app.py", "app.py"]).files(), 2);
    }

    #[test]
    fn added_and_removed_lines_are_counted() {
        let one = page(json!([
            {"kind": "context", "old_index": 1, "new_index": 1, "text": "keep", "line_complete": true},
            {"kind": "removed", "old_index": 2, "new_index": 2, "text": "-old", "line_complete": true},
            {"kind": "added", "old_index": 3, "new_index": 3, "text": "+new", "line_complete": true},
        ]));
        assert_eq!(count_lines(&[one]), (1, 1));
    }

    #[test]
    fn a_line_split_into_fragments_is_counted_once() {
        // One long added line arrives as three fragments of the same pair.
        let split = page(json!([
            {"kind": "added", "old_index": 1, "new_index": 1, "text": "aaa", "line_complete": false},
            {"kind": "added", "old_index": 1, "new_index": 1, "text": "bbb", "line_complete": false},
            {"kind": "added", "old_index": 1, "new_index": 1, "text": "ccc", "line_complete": true},
        ]));
        assert_eq!(count_lines(&[split]), (1, 0));
    }

    #[test]
    fn a_line_repeated_across_pages_is_counted_once() {
        let first = page(json!([
            {"kind": "added", "old_index": 1, "new_index": 1, "text": "+a", "line_complete": true},
        ]));
        let second = first.clone();
        assert_eq!(count_lines(&[first, second]), (1, 0));
    }

    #[test]
    fn counts_sum_across_files() {
        let one = page(json!([
            {"kind": "added", "old_index": 1, "new_index": 1, "text": "+a", "line_complete": true},
        ]));
        let two = page(json!([
            {"kind": "removed", "old_index": 2, "new_index": 2, "text": "-b", "line_complete": true},
            {"kind": "removed", "old_index": 3, "new_index": 3, "text": "-c", "line_complete": true},
        ]));
        assert_eq!(count_lines(&[one, two]), (1, 2));
    }
}
