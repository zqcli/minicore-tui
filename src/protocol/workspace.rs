//! Read-only workspace contracts from pinned Agent 0617433 workspace/{query,listing,search}.rs.
//! Cursors remain opaque JSON, including additive fields; known fields are validated.
use super::*;

const MAX_WORKSPACE_METADATA_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct FileRange {
    pub start_line: u32,
    pub line_byte_offset: u32,
}
impl Default for FileRange {
    fn default() -> Self {
        Self {
            start_line: 1,
            line_byte_offset: 0,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileStatus {
    Ok,
    Binary,
    Changed,
    TooLarge,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileEncoding {
    Utf8,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    File,
    Directory,
    Symlink,
    Other,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanStop {
    End,
    Page,
    Entries,
    Bytes,
    Depth,
    Rules,
    Deadline,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanConsistency {
    Live,
}
#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct FilePage {
    pub path: String,
    pub content: String,
    pub start_line: u32,
    pub returned_lines: u32,
    pub revision: Option<String>,
    pub truncated: bool,
    pub line_truncated: bool,
    pub next_range: Option<FileRange>,
    pub encoding: FileEncoding,
    pub status: FileStatus,
    pub file_bytes: u64,
    pub file_modified_unix_ms: Option<u64>,
}
#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct FileEntry {
    pub path: String,
    pub kind: FileKind,
    pub size: Option<u64>,
}
#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct FilesPage {
    pub directory: String,
    pub entries: Vec<FileEntry>,
    pub next_cursor: Option<Value>,
    pub truncated: bool,
    pub scan_complete: bool,
    pub stopped_by: ScanStop,
    pub skipped_count: u64,
    pub consistency: ScanConsistency,
    pub observed_at_unix_ms: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
pub struct MatchRange {
    pub start: u32,
    pub end: u32,
}
#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct FileMatch {
    pub path: String,
    pub line_number: u32,
    pub line_text_byte_offset: u32,
    pub match_byte_ranges: Vec<MatchRange>,
    pub line_text: String,
    pub line_truncated: bool,
}
#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct SearchPage {
    pub matches: Vec<FileMatch>,
    pub next_cursor: Option<Value>,
    pub truncated: bool,
    pub scan_complete: bool,
    pub stopped_by: ScanStop,
    pub skipped_files: u64,
    pub consistency: ScanConsistency,
    pub observed_at_unix_ms: u64,
}
// Never derive Debug for path, query, cursor or content owners.
impl fmt::Debug for FilePage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FilePage")
            .field("bytes", &self.content.len())
            .field("status", &self.status)
            .finish()
    }
}
impl fmt::Debug for FileEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileEntry")
            .field("kind", &self.kind)
            .finish()
    }
}
impl fmt::Debug for FileMatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileMatch")
            .field("line", &self.line_number)
            .field("bytes", &self.line_text.len())
            .finish()
    }
}
impl fmt::Debug for FilesPage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FilesPage")
            .field("entries", &self.entries.len())
            .field("stop", &self.stopped_by)
            .finish()
    }
}
impl fmt::Debug for SearchPage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SearchPage")
            .field("matches", &self.matches.len())
            .field("stop", &self.stopped_by)
            .finish()
    }
}

impl FileMatch {
    pub fn valid_ranges(&self) -> bool {
        self.line_number > 0
            && !self.match_byte_ranges.is_empty()
            && self.match_byte_ranges.iter().all(|range| {
                let (start, end) = (range.start as usize, range.end as usize);
                start < end
                    && end <= self.line_text.len()
                    && self.line_text.is_char_boundary(start)
                    && self.line_text.is_char_boundary(end)
            })
    }
}
fn cursor_valid(cursor: &Option<Value>, search: bool) -> bool {
    let Some(value) = cursor else {
        return true;
    };
    #[derive(Deserialize)]
    struct ListCursor {
        entry: u64,
        scope: String,
    }
    #[derive(Deserialize)]
    struct SearchCursor {
        path_index: u32,
        entry: u64,
        line: u32,
        line_byte_offset: u32,
        scope: String,
    }
    if search {
        serde_json::from_value::<SearchCursor>(value.clone()).is_ok_and(|c| {
            let _ = (c.entry, c.line);
            c.path_index < 32
                && c.line_byte_offset <= 512 * 1024
                && !c.scope.is_empty()
                && c.scope.len() <= MAX_WORKSPACE_METADATA_BYTES
        })
    } else {
        serde_json::from_value::<ListCursor>(value.clone()).is_ok_and(|c| {
            let _ = c.entry;
            !c.scope.is_empty() && c.scope.len() <= MAX_WORKSPACE_METADATA_BYTES
        })
    }
}
impl FilesPage {
    pub fn validate(&self) -> bool {
        cursor_valid(&self.next_cursor, false)
    }
}
impl SearchPage {
    pub fn validate(&self) -> bool {
        cursor_valid(&self.next_cursor, true) && self.matches.iter().all(FileMatch::valid_ranges)
    }
}
impl OutgoingRequest {
    pub fn workspace_read(
        id: RequestId,
        session: &str,
        path: &str,
        range: FileRange,
        revision: Option<&str>,
    ) -> Self {
        Self::new(
            id,
            METHOD_WORKSPACE_READ,
            serde_json::json!({"session_id":session,"path":path,"start_line":range.start_line,"line_byte_offset":range.line_byte_offset,"max_lines":400,"max_bytes":65536,"if_revision":revision}),
        )
    }
    pub fn workspace_files(
        id: RequestId,
        session: &str,
        directory: &str,
        query: &str,
        cursor: Option<&Value>,
    ) -> Self {
        Self::new(
            id,
            METHOD_WORKSPACE_FILES,
            serde_json::json!({"session_id":session,"directory":directory,"recursive":true,"query":query,"cursor":cursor,"limit":100,"max_bytes":65536}),
        )
    }
    pub fn workspace_search(
        id: RequestId,
        session: &str,
        query: &str,
        paths: &[String],
        case_sensitive: bool,
        cursor: Option<&Value>,
    ) -> Self {
        Self::new(
            id,
            METHOD_WORKSPACE_SEARCH,
            serde_json::json!({"session_id":session,"query":query,"paths":paths,"case_sensitive":case_sensitive,"cursor":cursor,"max_matches":100,"max_bytes":65536}),
        )
    }
}
