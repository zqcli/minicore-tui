//! One bounded change list and its single selected comparison; not an aggregate diff.
use crate::protocol::changes::*;
use crate::state::{
    session::ScrollState,
    workspace::{FileBuffer, FileLayout, FileLayoutIdentity, FileLayoutRequest},
};
use std::{
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Debug, Default)]
pub struct StatusObservation {
    pub value: Option<WorkspaceStatus>,
    pub generation: u64,
    pub wanted: bool,
    pub stale: bool,
    pub error: bool,
    pub opened_epoch: Option<u64>,
}
impl StatusObservation {
    pub fn label(&self) -> String {
        let suffix = if self.stale || self.error {
            "stale"
        } else {
            "seen"
        };
        let Some(value) = &self.value else {
            return "git?".into();
        };
        if !value.complete {
            return "git? [incomplete]".into();
        }
        if !value.repo_available {
            return if self.error || self.stale {
                "git? [stale]"
            } else {
                "no-git"
            }
            .into();
        }
        if value.detached {
            return format!(
                "detached:{} [{suffix}]",
                value
                    .head_oid
                    .as_deref()
                    .unwrap_or("?")
                    .chars()
                    .take(8)
                    .collect::<String>()
            );
        }
        format!("{} [{suffix}]", value.branch.as_deref().unwrap_or("git?"))
    }
}
pub struct ChangesState {
    pub session: String,
    pub epoch: u64,
    pub generation: u64,
    pub conversation_scroll: Option<ScrollState>,
    pub scope: ChangeScope,
    pub records: Vec<ChangeRecord>,
    pub list_page: Option<ChangesList>,
    pub cursor: Option<serde_json::Value>,
    pub wanted: bool,
    pub selected: usize,
    pub offset: usize,
    pub error: Option<String>,
    pub limited: bool,
    pub detail: Option<DiffState>,
    pub in_diff: bool,
}
pub struct DiffState {
    pub record: ChangeRecord,
    pub comparison: Comparison,
    pub cursor: Option<serde_json::Value>,
    pub wanted: bool,
    pub meta: Option<DiffPage>,
    pub buffer: DiffBuffer,
    pub revision: u64,
    pub stale: bool,
    pub error: Option<String>,
    pub offset: usize,
    pub follow: bool,
    pub layout: Option<DiffLayout>,
    pub pending: Option<FileLayoutIdentity>,
}
impl std::fmt::Debug for ChangesState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChangesState")
            .field("records", &self.records.len())
            .field("in_diff", &self.in_diff)
            .finish()
    }
}
impl std::fmt::Debug for DiffState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiffState")
            .field("comparison", &self.comparison)
            .field("bytes", &self.buffer.bytes)
            .finish()
    }
}
impl DiffState {
    pub fn new(record: ChangeRecord) -> Self {
        let comparison = if record.origin == ChangeOrigin::Tool {
            Comparison::ToolBeforeAfter
        } else {
            Comparison::HeadToWorktree
        };
        Self {
            record,
            comparison,
            cursor: None,
            wanted: true,
            meta: None,
            buffer: DiffBuffer::default(),
            revision: 0,
            stale: false,
            error: None,
            offset: 0,
            follow: false,
            layout: None,
            pending: None,
        }
    }
    pub fn accept(&mut self, mut page: DiffPage) -> Result<(), &'static str> {
        if page.change_ref != self.record.change_ref
            || page.path != self.record.path
            || page.origin != self.record.origin
            || page.tool_ref != self.record.tool_ref
            || page.comparison != self.comparison
        {
            return Err("diff identity mismatch");
        }
        self.wanted = false;
        if page.stale
            || self.meta.as_ref().is_some_and(|old| {
                old.base_version != page.base_version || old.target_version != page.target_version
            })
        {
            self.stale = true;
            self.cursor = None;
            return Ok(());
        }
        let hunks = std::mem::take(&mut page.hunks);
        if !page.binary
            && !matches!(
                page.availability,
                DiffAvailability::Unavailable | DiffAvailability::Binary
            )
        {
            self.buffer.append(hunks)?;
        }
        self.cursor = page.next_cursor.clone();
        self.meta = Some(page);
        self.revision = self.revision.wrapping_add(1);
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HunkKey {
    pub old_start: usize,
    pub old_count: usize,
    pub new_start: usize,
    pub new_count: usize,
}
#[derive(Debug, Clone)]
pub struct DiffSourceLine {
    pub hunk: HunkKey,
    pub kind: DiffKind,
    pub old: Option<usize>,
    pub new: Option<usize>,
    pub length: usize,
    pub complete: bool,
    pub body: FileBuffer,
}
#[derive(Debug, Default, Clone)]
pub struct DiffBuffer {
    pub lines: Vec<Arc<DiffSourceLine>>,
    pub bytes: usize,
}
impl DiffBuffer {
    pub fn append(&mut self, hunks: Vec<DiffHunk>) -> Result<(), &'static str> {
        // Validate a bounded candidate before publishing any part of a malformed page.
        let mut next = self.clone();
        for h in hunks {
            if h.old_start.checked_add(h.old_count).is_none()
                || h.new_start.checked_add(h.new_count).is_none()
            {
                return Err("diff hunk range overflow");
            }
            let key = HunkKey {
                old_start: h.old_start,
                old_count: h.old_count,
                new_start: h.new_start,
                new_count: h.new_count,
            };
            for line in h.lines {
                let end = line
                    .line_byte_offset
                    .checked_add(line.text.len())
                    .ok_or("diff offset overflow")?;
                if end > line.line_byte_len
                    || line.line_complete != (end == line.line_byte_len)
                    || line.text.is_empty() && !line.line_complete
                {
                    return Err("invalid diff fragment length/completion");
                }
                let in_range = |index: Option<usize>, start: usize, count: usize| {
                    index.is_some_and(|i| {
                        i >= start && start.checked_add(count).is_some_and(|end| i < end)
                    })
                };
                if match line.kind {
                    DiffKind::Context => {
                        !in_range(line.old_index, h.old_start, h.old_count)
                            || !in_range(line.new_index, h.new_start, h.new_count)
                    }
                    DiffKind::Added => {
                        line.old_index.is_some()
                            || !in_range(line.new_index, h.new_start, h.new_count)
                    }
                    DiffKind::Removed => {
                        line.new_index.is_some()
                            || !in_range(line.old_index, h.old_start, h.old_count)
                    }
                } {
                    return Err("invalid diff line identity");
                }
                if next.bytes.saturating_add(line.text.len()) > crate::limits::DIFF_BODY_BYTES {
                    return Err("diff byte limit reached; loaded prefix only");
                }
                if line.line_byte_offset == 0 {
                    if next.lines.last().is_some_and(|l| !l.complete) {
                        return Err("incomplete diff line before next line");
                    }
                    if next.lines.len() >= crate::limits::DIFF_LINES {
                        return Err("diff line limit reached; loaded prefix only");
                    }
                    next.lines.push(Arc::new(DiffSourceLine {
                        hunk: key,
                        kind: line.kind,
                        old: line.old_index,
                        new: line.new_index,
                        length: line.line_byte_len,
                        complete: false,
                        body: FileBuffer::default(),
                    }));
                }
                let previous = next
                    .lines
                    .last_mut()
                    .ok_or("diff continuation without prefix")?;
                let previous = Arc::make_mut(previous);
                if previous.hunk != key
                    || previous.kind != line.kind
                    || previous.old != line.old_index
                    || previous.new != line.new_index
                    || previous.length != line.line_byte_len
                    || previous.body.bytes != line.line_byte_offset
                    || previous.complete
                {
                    return Err("diff fragment gap or identity mismatch");
                }
                next.bytes += line.text.len();
                previous.body.append(line.text)?;
                previous.complete = line.line_complete;
            }
        }
        *self = next;
        Ok(())
    }
    pub fn partial_line(&self) -> bool {
        self.lines.last().is_some_and(|l| !l.complete)
    }
}
pub struct DiffLayoutRequest {
    pub identity: FileLayoutIdentity,
    pub buffer: DiffBuffer,
    pub cancel: Arc<AtomicBool>,
}
#[derive(Debug)]
pub struct DiffRow {
    pub text: Range<usize>,
    pub kind: Option<DiffKind>,
    pub old: Option<usize>,
    pub new: Option<usize>,
}
pub struct DiffLayout {
    pub identity: FileLayoutIdentity,
    pub text: Arc<str>,
    pub copy_text: Arc<str>,
    pub rows: Vec<DiffRow>,
}
impl std::fmt::Debug for DiffLayout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiffLayout")
            .field("identity", &self.identity)
            .field("rows", &self.rows.len())
            .finish()
    }
}
impl DiffLayout {
    pub fn retained_bytes(&self) -> usize {
        self.text.len()
            + self.copy_text.len()
            + self.rows.capacity() * std::mem::size_of::<DiffRow>()
    }
    pub fn build(request: DiffLayoutRequest) -> Option<Self> {
        let mut text = String::new();
        let mut copy = String::new();
        let mut rows = Vec::new();
        let mut hunk = None;
        for line in &request.buffer.lines {
            if request.cancel.load(Ordering::Relaxed) {
                return None;
            }
            if hunk != Some(line.hunk) {
                let k = line.hunk;
                let start = text.len();
                text.push_str(&format!(
                    "@@ -{},{} +{},{} @@",
                    k.old_start + usize::from(k.old_count > 0),
                    k.old_count,
                    k.new_start + usize::from(k.new_count > 0),
                    k.new_count
                ));
                rows.push(DiffRow {
                    text: start..text.len(),
                    kind: None,
                    old: None,
                    new: None,
                });
                hunk = Some(k);
            }
            let built = FileLayout::build(FileLayoutRequest {
                identity: request.identity.clone(),
                content: line.body.clone(),
                cancel: request.cancel.clone(),
            })?;
            // Copy is explicitly line-source text, not an applicable patch. It
            // never includes colors, +/- signs, hunk/line labels or soft wraps.
            copy.push_str(&built.copy_text);
            let base = text.len();
            text.push_str(&built.text);
            for row in built.rows {
                rows.push(DiffRow {
                    text: base + row.text.start..base + row.text.end,
                    kind: Some(line.kind),
                    old: line.old,
                    new: line.new,
                });
            }
        }
        Some(Self {
            identity: request.identity,
            text: Arc::from(text),
            copy_text: Arc::from(copy),
            rows,
        })
    }
}
