//! Bounded workspace observations, not attachments or a local filesystem index.
use crate::protocol::workspace::*;
use std::{
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserKind {
    Files,
    Grep,
}
#[derive(Clone, PartialEq, Eq)]
pub struct WorkspaceBrowser {
    pub kind: BrowserKind,
    pub session: String,
    pub epoch: u64,
    pub generation: u64,
    pub query: String,
    /// Directory for files; one path or a JSON string array for grep.
    pub scope: String,
    pub scope_focused: bool,
    pub case_sensitive: bool,
    pub due: Option<Instant>,
    pub cursor: Option<serde_json::Value>,
    pub files: Vec<FileEntry>,
    pub matches: Vec<FileMatch>,
    pub selected: usize,
    pub truncated: bool,
    pub scan_complete: bool,
    pub stopped_by: Option<ScanStop>,
    pub skipped: u64,
    pub error: Option<String>,
    pub limited: bool,
    pub origin: Option<ReferenceInsertion>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReferenceInsertion {
    pub line: usize,
    pub start: usize,
    pub end: usize,
    pub revision: u64,
}
impl std::fmt::Debug for WorkspaceBrowser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspaceBrowser")
            .field("kind", &self.kind)
            .field("generation", &self.generation)
            .field("rows", &self.len())
            .finish()
    }
}
impl WorkspaceBrowser {
    pub fn new(
        kind: BrowserKind,
        session: String,
        epoch: u64,
        generation: u64,
        now: Instant,
    ) -> Self {
        Self {
            kind,
            session,
            epoch,
            generation,
            query: String::new(),
            scope: String::new(),
            scope_focused: false,
            case_sensitive: false,
            due: Some(now),
            cursor: None,
            files: vec![],
            matches: vec![],
            selected: 0,
            truncated: false,
            scan_complete: false,
            stopped_by: None,
            skipped: 0,
            error: None,
            limited: false,
            origin: None,
        }
    }
    pub fn len(&self) -> usize {
        if self.kind == BrowserKind::Files {
            self.files.len()
        } else {
            self.matches.len()
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn retained_bytes(&self) -> usize {
        self.files.iter().map(|f| f.path.capacity()).sum::<usize>()
            + self
                .matches
                .iter()
                .map(|m| {
                    m.path.capacity()
                        + m.line_text.capacity()
                        + m.match_byte_ranges.capacity() * std::mem::size_of::<MatchRange>()
                })
                .sum::<usize>()
    }
    pub fn paths(&self) -> Result<Vec<String>, &'static str> {
        let scope = self.scope.trim();
        let paths: Vec<String> = if scope.is_empty() {
            vec![]
        } else if scope.starts_with('[') {
            serde_json::from_str(scope).map_err(|_| "paths: expected a JSON string array")?
        } else {
            vec![scope.to_owned()]
        };
        if paths.len() > 32 || paths.iter().any(|path| path.len() > 4096) {
            return Err("paths: maximum 32 paths, 4096 bytes each");
        }
        Ok(paths)
    }
    pub fn reset(&mut self, generation: u64, due: Instant) {
        self.generation = generation;
        self.due = Some(due);
        self.cursor = None;
        self.files.clear();
        self.matches.clear();
        self.selected = 0;
        self.error = None;
        self.truncated = false;
        self.scan_complete = false;
        self.stopped_by = None;
        self.skipped = 0;
        self.limited = false;
    }
}
#[derive(Debug)]
pub enum ReturnTarget {
    Conversation,
    Browser(Box<WorkspaceBrowser>),
}
#[derive(Default, Clone)]
pub struct FileBuffer {
    pub chunks: Vec<Arc<str>>,
    pub bytes: usize,
}
impl std::fmt::Debug for FileBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileBuffer")
            .field("bytes", &self.bytes)
            .field("chunks", &self.chunks.len())
            .finish()
    }
}
impl FileBuffer {
    pub fn append(&mut self, text: String) -> Result<(), &'static str> {
        if self.bytes.saturating_add(text.len()) > crate::limits::FILE_PREVIEW_BYTES {
            return Err("preview byte limit reached; refresh or narrow the file");
        }
        if text.is_empty() {
            return Ok(());
        }
        self.bytes += text.len();
        if self
            .chunks
            .last()
            .is_some_and(|tail| tail.len() < 16 * 1024)
        {
            let tail = self.chunks.pop().unwrap();
            self.chunks.push(Arc::from(format!("{tail}{text}")));
        } else {
            self.chunks.push(Arc::from(text));
        }
        Ok(())
    }
}
pub struct FilePreviewState {
    pub session: String,
    pub epoch: u64,
    pub generation: u64,
    pub path: String,
    pub conversation_scroll: Option<crate::state::session::ScrollState>,
    pub return_target: ReturnTarget,
    pub requested: FileRange,
    pub next: Option<FileRange>,
    pub revision: Option<String>,
    pub content: FileBuffer,
    pub content_revision: u64,
    pub status: Option<FileStatus>,
    pub error: Option<String>,
    pub wanted: bool,
    pub truncated: bool,
    pub line_truncated: bool,
    pub offset: usize,
    pub follow: bool,
    pub scrollbar_grab: Option<usize>,
    pub target: Option<FileRange>,
    pub layout: Option<FileLayout>,
    pub layout_pending: Option<FileLayoutIdentity>,
}
impl std::fmt::Debug for FilePreviewState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilePreview")
            .field("generation", &self.generation)
            .field("status", &self.status)
            .field("content", &self.content)
            .finish()
    }
}
impl FilePreviewState {
    pub fn accept(&mut self, page: FilePage) -> Result<(), &'static str> {
        if page.path != self.path || page.start_line != self.requested.start_line {
            return Err("file response identity/range mismatch");
        }
        self.wanted = false;
        if page.status != FileStatus::Ok {
            self.status = Some(page.status);
            self.next = None;
            return Ok(());
        }
        if page.encoding != FileEncoding::Utf8 || page.revision.is_none() {
            return Err("malformed UTF-8 file page");
        }
        if page
            .revision
            .as_ref()
            .is_some_and(|revision| revision.len() > 4096)
        {
            return Err("file revision metadata is too large");
        }
        if self
            .revision
            .as_ref()
            .is_some_and(|r| !page.revision.as_ref().unwrap().eq_ignore_ascii_case(r))
        {
            self.status = Some(FileStatus::Changed);
            self.next = None;
            return Ok(());
        }
        if let Some(next) = page.next_range {
            if (next.start_line, next.line_byte_offset)
                <= (self.requested.start_line, self.requested.line_byte_offset)
                || page.content.is_empty()
            {
                return Err("file range did not advance");
            }
        }
        self.content.append(page.content)?;
        self.content_revision = self.content_revision.wrapping_add(1);
        self.status = Some(FileStatus::Ok);
        self.revision = page.revision;
        self.next = page.next_range;
        self.truncated = page.truncated;
        self.line_truncated = page.line_truncated;
        // Selection from grep may require several exact pages to reach its source position.
        self.wanted = self.target.zip(self.next).is_some_and(|(target, next)| {
            (next.start_line, next.line_byte_offset) <= (target.start_line, target.line_byte_offset)
        });
        Ok(())
    }
    pub fn scroll_offset(&self, height: usize) -> usize {
        let max = self
            .layout
            .as_ref()
            .map_or(0, |l| l.rows.len())
            .saturating_sub(height);
        if self.follow {
            max
        } else {
            self.offset.min(max)
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileLayoutIdentity {
    pub generation: u64,
    pub revision: u64,
    pub width: u16,
}
pub struct FileLayoutRequest {
    pub identity: FileLayoutIdentity,
    pub content: FileBuffer,
    pub cancel: Arc<AtomicBool>,
}
#[derive(Debug)]
pub struct FileRow {
    pub text: Range<usize>,
    pub source: FileRange,
    pub source_bytes: Range<usize>,
}
pub struct FileLayout {
    pub identity: FileLayoutIdentity,
    pub text: Arc<str>,
    pub copy_text: Arc<str>,
    pub rows: Vec<FileRow>,
    pub display_limited: bool,
}
impl std::fmt::Debug for FileLayout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileLayout")
            .field("identity", &self.identity)
            .field("rows", &self.rows.len())
            .finish()
    }
}
impl FileLayout {
    pub fn retained_bytes(&self) -> usize {
        self.text.len()
            + self.copy_text.len()
            + self.rows.capacity() * std::mem::size_of::<FileRow>()
    }
    /// Only called by the existing serialized layout worker. Raw chunks stay immutable.
    pub fn build(request: FileLayoutRequest) -> Option<Self> {
        use unicode_segmentation::UnicodeSegmentation;
        use unicode_width::UnicodeWidthStr;
        let raw: String = request.content.chunks.iter().map(AsRef::as_ref).collect();
        let mut text = String::new();
        let mut copy = String::new();
        let mut rows = Vec::new();
        let mut source = FileRange::default();
        let mut row_source = source;
        let mut raw_start = 0;
        let mut start = 0;
        let mut cells = 0;
        let mut limited = false;
        let width = usize::from(request.identity.width).max(1);
        for (offset, g) in raw.grapheme_indices(true) {
            if request.cancel.load(Ordering::Relaxed) {
                return None;
            }
            if g == "\n" || g == "\r\n" {
                copy.push_str(g);
                rows.push(FileRow {
                    text: start..text.len(),
                    source: row_source,
                    source_bytes: raw_start..offset + g.len(),
                });
                text.push('\n');
                start = text.len();
                raw_start = offset + g.len();
                cells = 0;
                source = FileRange {
                    start_line: source.start_line + 1,
                    line_byte_offset: 0,
                };
                row_source = source;
                continue;
            }
            let safe = crate::safe_text::safe_display(g);
            copy.push_str(&safe);
            let display = if g.len() > 2048 {
                limited = true;
                "[oversized grapheme]".to_owned()
            } else {
                safe.replace('\t', "    ")
            };
            let size = display.width();
            if (cells + size > width || text.len() - start + display.len() > 8192)
                && text.len() > start
            {
                rows.push(FileRow {
                    text: start..text.len(),
                    source: row_source,
                    source_bytes: raw_start..offset,
                });
                start = text.len();
                raw_start = offset;
                row_source = source;
                cells = 0;
            }
            text.push_str(&display);
            cells += size;
            source.line_byte_offset += g.len() as u32;
        }
        if raw_start < raw.len() {
            rows.push(FileRow {
                text: start..text.len(),
                source: row_source,
                source_bytes: raw_start..raw.len(),
            });
        }
        Some(Self {
            identity: request.identity,
            text: Arc::from(text),
            copy_text: Arc::from(copy),
            rows,
            display_limited: limited,
        })
    }
}
/// A quoted, readable path, never file content or an attachment instruction.
pub fn reference_token(path: &str) -> String {
    use std::fmt::Write;
    let encoded = serde_json::to_string(path).expect("path string");
    let mut token = String::from("@");
    for character in encoded.chars() {
        if crate::safe_text::is_unsafe_display_control(character) {
            // The shared unsafe set consists of BMP controls. JSON escapes keep
            // the exact path reversible without hiding controls in the editor.
            write!(token, "\\u{:04x}", character as u32).expect("string write");
        } else {
            token.push(character);
        }
    }
    token
}
