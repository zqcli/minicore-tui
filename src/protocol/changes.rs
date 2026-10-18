//! `changes.list` / `changes.diff` DTOs. Change references are opaque: a
//! `workspace:` token is minted by the Agent and must be passed back verbatim.

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
    pub fn count(&self) -> usize {
        if self.total > 0 {
            self.total.max(self.records.len())
        } else {
            self.records.len()
        }
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
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub line_complete: bool,
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
