//! `changes.list` / `changes.diff` DTOs. Change references are opaque: a
//! `workspace:` token is minted by the Agent and must be passed back verbatim.

use std::collections::HashSet;

use serde::Deserialize;
use serde_json::Value;

#[derive(Clone, Debug, Deserialize)]
pub struct ChangeList {
    #[serde(default)]
    pub records: Vec<ChangeRecord>,
    #[serde(default)]
    pub next_cursor: Option<Value>,
    #[serde(default)]
    pub total: usize,
    #[serde(default)]
    pub complete: bool,
    #[serde(default)]
    pub stale: bool,
    #[serde(default)]
    pub warnings: Vec<String>,
}

impl ChangeList {
    /// Distinct files this page reports. A turn scope returns one record per
    /// mutating tool call, so the same path appears once per write/edit/patch;
    /// "files changed" counts the path, not the calls.
    pub fn files(&self) -> usize {
        let mut paths: HashSet<&str> = HashSet::new();
        for record in &self.records {
            if record.path.is_empty() {
                // Without a path the record cannot be grouped; count it as its
                // own file rather than dropping it.
                return self.records.len();
            }
            paths.insert(record.path.as_str());
        }
        paths.len()
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct ChangeRecord {
    pub change_ref: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub kind: ChangeKind,
    #[serde(default)]
    pub original_path: Option<String>,
    #[serde(default)]
    pub before: Option<FileVersion>,
    #[serde(default)]
    pub after: Option<FileVersion>,
    #[serde(default)]
    pub details_available: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
    Conflict,
    #[default]
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FileVersion {
    Content {
        #[serde(default)]
        bytes: u64,
    },
    Missing,
    Metadata {
        #[serde(default)]
        bytes: u64,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Deserialize)]
pub struct DiffPage {
    #[serde(default)]
    pub change_ref: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub hunks: Vec<DiffHunk>,
    #[serde(default)]
    pub next_cursor: Option<Value>,
    #[serde(default)]
    pub complete: bool,
    #[serde(default)]
    pub binary: bool,
    #[serde(default)]
    pub availability: Option<String>,
    #[serde(default)]
    pub stale: bool,
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct DiffHunk {
    #[serde(default)]
    pub old_start: u64,
    #[serde(default)]
    pub old_count: u64,
    #[serde(default)]
    pub new_start: u64,
    #[serde(default)]
    pub new_count: u64,
    #[serde(default)]
    pub lines: Vec<DiffLine>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct DiffLine {
    pub kind: LineKind,
    /// Source line numbers. They identify a logical line across the fragments a
    /// long line is split into.
    #[serde(default)]
    pub old_index: Option<u64>,
    #[serde(default)]
    pub new_index: Option<u64>,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub line_complete: bool,
}
/// Counts added and removed logical lines across every page of one diff.
///
/// A long line arrives as several fragments sharing one `(old_index,
/// new_index)` pair, in offset order; only the fragment with `line_complete`
/// ends it. Counting complete fragments and deduplicating by that pair counts
/// each line exactly once, however it was split and however often it was paged.
pub fn count_lines(pages: &[DiffPage]) -> (usize, usize) {
    let mut seen: HashSet<(u64, u64)> = HashSet::new();
    let (mut added, mut removed) = (0, 0);
    for page in pages {
        for hunk in &page.hunks {
            for line in &hunk.lines {
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

/// `changes.list` scope. Serialized inline into the request params.
#[derive(Clone, Debug)]
pub enum ChangeScope {
    Workspace,
    Session,
    Turn(String),
}

impl ChangeScope {
    pub fn value(&self) -> Value {
        match self {
            Self::Workspace => Value::String("workspace".into()),
            Self::Session => Value::String("session".into()),
            Self::Turn(loop_id) => serde_json::json!({"turn": {"loop_id": loop_id}}),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn turn_scope_serializes_as_nested_object() {
        assert_eq!(
            ChangeScope::Turn("lup_1".into()).value(),
            json!({"turn": {"loop_id": "lup_1"}})
        );
        assert_eq!(ChangeScope::Workspace.value(), json!("workspace"));
    }

    #[test]
    fn diff_lines_decode_without_optional_offsets() {
        let page: DiffPage = serde_json::from_value(json!({
            "change_ref": "tool:abc", "path": "a.rs",
            "hunks": [{"old_start": 1, "old_count": 1, "new_start": 1, "new_count": 2,
                       "lines": [{"kind": "added", "text": "+x", "line_complete": true}]}]
        }))
        .unwrap();
        assert_eq!(page.hunks[0].lines[0].kind, LineKind::Added);
    }
}

#[cfg(test)]
mod count_tests {
    use super::*;
    use serde_json::json;

    fn page(lines: Value) -> DiffPage {
        serde_json::from_value(json!({
            "change_ref": "tool:a", "path": "a.rs", "complete": true,
            "hunks": [{"old_start": 1, "old_count": 1, "new_start": 1, "new_count": 1,
                       "lines": lines}]
        }))
        .unwrap()
    }

    fn list(records: Value) -> ChangeList {
        serde_json::from_value(json!({"records": records, "total": 1})).unwrap()
    }

    #[test]
    fn repeated_calls_on_one_path_are_one_changed_file() {
        // A turn scope returns one record per mutating tool call: write, then
        // edit, then apply_patch on the same file is one changed file.
        let list = list(json!([
            {"change_ref": "tool:a", "path": "app.py"},
            {"change_ref": "tool:b", "path": "app.py"},
            {"change_ref": "tool:c", "path": "app.py"},
        ]));
        assert_eq!(list.files(), 1);
    }

    #[test]
    fn distinct_paths_are_counted_separately() {
        let list = list(json!([
            {"change_ref": "tool:a", "path": "app.py"},
            {"change_ref": "tool:b", "path": "tests/test_app.py"},
            {"change_ref": "tool:c", "path": "app.py"},
        ]));
        assert_eq!(list.files(), 2);
    }

    #[test]
    fn a_record_without_a_path_still_counts() {
        let list = list(json!([
            {"change_ref": "tool:a", "path": "app.py"},
            {"change_ref": "tool:b"},
        ]));
        assert_eq!(list.files(), 2);
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
