//! Fixed Agent 0617433 changes.rs/diff.rs/status.rs contracts; opaque refs stay opaque.
use super::*;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeScope {
    Workspace,
    Session,
    Turn { loop_id: String },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
    Conflict,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeOrigin {
    Tool,
    WorkspaceUnknown,
}
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChangeRevision {
    Missing,
    Content {
        sha256: String,
        bytes: usize,
    },
    Metadata {
        bytes: u64,
        modified_unix_ms: Option<u64>,
    },
    Unknown,
}
impl fmt::Debug for ChangeRevision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ChangeRevision")
            .field(&std::mem::discriminant(self))
            .finish()
    }
}
impl ChangeRevision {
    pub fn valid(&self) -> bool {
        match self {
            Self::Content { sha256, .. } => {
                sha256.len() == 64 && sha256.bytes().all(|b| b.is_ascii_hexdigit())
            }
            _ => true,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitState {
    NotCommitted,
    Applied,
    Conflict,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeCoverage {
    Complete,
    Partial,
    Unavailable,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeConsistency {
    Live,
    Cold,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeWarning {
    NoRepository,
    StatusIncomplete,
    DetailsUnavailable,
    RecordsSkipped,
    StaleCursor,
}
#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct ChangeRecord {
    pub change_ref: String,
    pub path: String,
    pub kind: ChangeKind,
    pub origin: ChangeOrigin,
    pub tool_ref: Option<ToolRefWire>,
    pub original_path: Option<String>,
    pub before: ChangeRevision,
    pub after: ChangeRevision,
    pub commit_state: CommitState,
    pub details_available: bool,
    pub coverage: ChangeCoverage,
}
impl fmt::Debug for ChangeRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChangeRecord")
            .field("origin", &self.origin)
            .field("kind", &self.kind)
            .finish()
    }
}
#[derive(Clone, Deserialize)]
pub struct ChangesList {
    pub session_id: String,
    pub scope: ChangeScope,
    pub records: Vec<ChangeRecord>,
    pub next_cursor: Option<Value>,
    pub total: usize,
    pub complete: bool,
    pub stale: bool,
    pub consistency: ChangeConsistency,
    pub observed_at_unix_ms: Option<u64>,
    pub warnings: Vec<ChangeWarning>,
}
impl fmt::Debug for ChangesList {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChangesList")
            .field("records", &self.records.len())
            .field("stale", &self.stale)
            .finish()
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Comparison {
    ToolBeforeAfter,
    HeadToIndex,
    IndexToWorktree,
    HeadToWorktree,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffKind {
    Context,
    Added,
    Removed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffAvailability {
    Available,
    Partial,
    Binary,
    Unavailable,
}
#[derive(Clone, Deserialize)]
pub struct DiffLine {
    pub kind: DiffKind,
    pub old_index: Option<usize>,
    pub new_index: Option<usize>,
    pub line_byte_offset: usize,
    pub line_byte_len: usize,
    pub text: String,
    pub line_complete: bool,
}
impl fmt::Debug for DiffLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DiffLine")
            .field("kind", &self.kind)
            .field("offset", &self.line_byte_offset)
            .field("bytes", &self.text.len())
            .finish()
    }
}
#[derive(Clone, Debug, Deserialize)]
pub struct DiffHunk {
    pub old_start: usize,
    pub old_count: usize,
    pub new_start: usize,
    pub new_count: usize,
    pub lines: Vec<DiffLine>,
}
#[derive(Clone, Deserialize)]
pub struct DiffPage {
    pub change_ref: String,
    pub path: String,
    pub kind: ChangeKind,
    pub origin: ChangeOrigin,
    pub tool_ref: Option<ToolRefWire>,
    pub comparison: Comparison,
    pub base_version: ChangeRevision,
    pub target_version: ChangeRevision,
    pub commit_state: CommitState,
    pub coverage: ChangeCoverage,
    pub binary: bool,
    pub stale: bool,
    pub versions_refreshed: bool,
    pub availability: DiffAvailability,
    pub hunks: Vec<DiffHunk>,
    pub complete: bool,
    pub truncated: bool,
    pub next_cursor: Option<Value>,
}
impl fmt::Debug for DiffPage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DiffPage")
            .field("hunks", &self.hunks.len())
            .field("availability", &self.availability)
            .finish()
    }
}
// Decode known cursor fields to reject malformed peers, but forward the original object.
#[derive(Deserialize)]
struct ListCursor {
    session_id: String,
    scope: ChangeScope,
    offset: usize,
    observation: Option<String>,
}
#[derive(Deserialize)]
struct DiffCursor {
    session_id: String,
    change_ref: String,
    tool_ref: Option<ToolRefWire>,
    ops_fingerprint: String,
    context_lines: usize,
    hunk_index: usize,
    line_index: usize,
    line_byte_offset: usize,
}
impl ChangesList {
    pub fn valid_cursor(&self) -> bool {
        self.next_cursor.as_ref().is_none_or(|v| {
            serde_json::from_value::<ListCursor>(v.clone()).is_ok_and(|c| {
                c.session_id == self.session_id
                    && c.scope == self.scope
                    && (c.offset == 0 || c.observation.is_some())
            })
        })
    }
}
impl DiffPage {
    pub fn valid_cursor(&self, session: &str) -> bool {
        self.next_cursor.as_ref().is_none_or(|v| {
            serde_json::from_value::<DiffCursor>(v.clone()).is_ok_and(|c| {
                let _ = (c.hunk_index, c.line_index, c.line_byte_offset);
                c.session_id == session
                    && c.change_ref == self.change_ref
                    && c.context_lines == 3
                    && !c.ops_fingerprint.is_empty()
                    && c.tool_ref.as_ref().is_none_or(|t| t.session_id == session)
            })
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusWarning {
    GitUnavailable,
    StatusFailed,
    OutputTruncated,
    Deadline,
    SkippedPaths,
    NestedRepository,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusKind {
    Ordinary,
    Renamed,
    Unmerged,
    Untracked,
}
#[derive(Clone, Deserialize)]
pub struct StatusEntry {
    pub path: String,
    pub kind: StatusKind,
    pub index_status: Option<String>,
    pub worktree_status: Option<String>,
    pub original_path: Option<String>,
}
impl fmt::Debug for StatusEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StatusEntry")
            .field("kind", &self.kind)
            .finish()
    }
}
#[derive(Clone, Deserialize)]
pub struct WorkspaceStatus {
    pub repo_available: bool,
    pub head_oid: Option<String>,
    pub branch: Option<String>,
    pub detached: bool,
    pub staged: u64,
    pub unstaged: u64,
    pub untracked: u64,
    pub conflicted: u64,
    pub entries: Vec<StatusEntry>,
    pub skipped_paths: u64,
    pub complete: bool,
    pub warnings: Vec<StatusWarning>,
    pub consistency: workspace::ScanConsistency,
    pub observed_at_unix_ms: u64,
}
impl fmt::Debug for WorkspaceStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorkspaceStatus")
            .field("complete", &self.complete)
            .field("entries", &self.entries.len())
            .field("warnings", &self.warnings)
            .finish()
    }
}
impl OutgoingRequest {
    pub fn changes_list(
        id: RequestId,
        session: &str,
        scope: &ChangeScope,
        cursor: Option<&Value>,
    ) -> Self {
        Self::new(
            id,
            METHOD_CHANGES_LIST,
            serde_json::json!({"session_id":session,"scope":scope,"cursor":cursor,"limit":100,"max_bytes":65536}),
        )
    }
    pub fn changes_diff(
        id: RequestId,
        session: &str,
        reference: &str,
        comparison: Comparison,
        cursor: Option<&Value>,
    ) -> Self {
        Self::new(
            id,
            METHOD_CHANGES_DIFF,
            serde_json::json!({"session_id":session,"change_ref":reference,"comparison":comparison,"context_lines":3,"cursor":cursor,"max_bytes":65536}),
        )
    }
    pub fn workspace_status(id: RequestId, session: &str) -> Self {
        Self::new(
            id,
            METHOD_WORKSPACE_STATUS,
            serde_json::json!({"session_id":session,"max_bytes":65536}),
        )
    }
}
