//! The diff view: its state and the reads that fill it.

use super::{App, Overlay, SessionOp};
use crate::wrap::sanitize;
use minicore_host_protocol::{CallError, CallOptions, SessionIdentity};
use minicore_host_transport::SessionClient;
use minicore_host_workspace::{
    ChangeKind, ChangeRecord, ChangeRef, ChangeScope, ChangesListRequest, DiffComparison, DiffLine,
    DiffLineKind as LineKind, DiffRequest, DiffResult,
};

/// Which path's hunks a read is for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffQuery {
    pub path: String,
}

/// One row of a file's diff, before it is styled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiffRow {
    Blank,
    /// Dim text: the separator between records, or why there is no diff.
    Note(String),
    /// `@@ -a,b +c,d @@`
    Hunk(String),
    /// A diff line: the dim `old new` line-number gutter, then the text.
    Line {
        kind: LineKind,
        gutter: String,
        text: String,
    },
}

impl DiffRow {
    /// The row as `c` copies it: without the line-number gutter.
    pub fn body(&self) -> String {
        match self {
            Self::Blank => String::new(),
            Self::Note(text) | Self::Hunk(text) => text.clone(),
            Self::Line { kind, text, .. } => format!("{}{text}", sign(*kind)),
        }
    }
}

/// Added and removed lines of one path's records.
pub fn measure(pages: &[DiffResult]) -> (usize, usize) {
    pages
        .iter()
        .flat_map(|page| &page.hunks)
        .flat_map(|hunk| &hunk.lines)
        .filter(|line| line.line_byte_offset == 0)
        .fold((0, 0), |(added, removed), line| match line.kind {
            LineKind::Added => (added + 1, removed),
            LineKind::Removed => (added, removed + 1),
            LineKind::Context => (added, removed),
        })
}

pub fn sign(kind: LineKind) -> &'static str {
    match kind {
        LineKind::Added => "+",
        LineKind::Removed => "-",
        LineKind::Context => " ",
    }
}

/// The text of the diff view: the file's path, then its rows.
pub fn text(diff: &DiffState) -> String {
    let mut out = String::new();
    if let Some(file) = diff.current() {
        out.push_str(&file.path);
        out.push('\n');
    }
    for row in &diff.rows {
        out.push_str(&row.body());
        out.push('\n');
    }
    out
}

/// Rows for every change record of one file, told apart by a separator naming the tool.
pub fn rows(pages: &[DiffResult], tools: &[Option<String>]) -> Vec<DiffRow> {
    if pages.len() <= 1 {
        return match pages.first() {
            Some(page) => hunk_rows(page),
            None => vec![DiffRow::Note("No differences.".into())],
        };
    }
    let mut rows = Vec::new();
    for (index, page) in pages.iter().enumerate() {
        let position = format!("change {} of {}", index + 1, pages.len());
        let label = match tools.get(index).and_then(Option::as_deref) {
            Some(tool) => format!("── {tool} · {position} ──"),
            None => format!("── {position} ──"),
        };
        if index > 0 {
            rows.push(DiffRow::Blank);
        }
        rows.push(DiffRow::Note(label));
        rows.extend(hunk_rows(page));
    }
    rows
}

/// Turns one `changes.diff` page into rows.
pub(crate) fn hunk_rows(page: &DiffResult) -> Vec<DiffRow> {
    if page.binary {
        return vec![DiffRow::Note("Binary file: the diff is not shown.".into())];
    }
    if page.hunks.is_empty() {
        return vec![DiffRow::Note("No differences.".into())];
    }
    let digits = page
        .hunks
        .iter()
        .flat_map(|hunk| {
            [
                hunk.old_start + hunk.old_count,
                hunk.new_start + hunk.new_count,
            ]
        })
        .max()
        .unwrap_or(0)
        .to_string()
        .len();
    let mut rows = Vec::new();
    for hunk in &page.hunks {
        rows.push(DiffRow::Hunk(format!(
            "@@ -{},{} +{},{} @@",
            hunk.old_start, hunk.old_count, hunk.new_start, hunk.new_count
        )));
        // A long line arrives as several fragments; only the last one is
        // `line_complete`. They are joined back into one row.
        let mut pending: Option<(&DiffLine, String)> = None;
        for line in &hunk.lines {
            let (first, text) = match pending.take() {
                Some((first, mut text)) => {
                    text.push_str(&line.text);
                    (first, text)
                }
                None => (line, line.text.clone()),
            };
            if line.line_complete {
                rows.push(diff_row(first, &text, digits));
            } else {
                pending = Some((first, text));
            }
        }
        if let Some((first, text)) = pending {
            rows.push(diff_row(first, &text, digits));
        }
    }
    if !page.complete {
        rows.push(DiffRow::Note("… the diff continues".into()));
    }
    rows
}

fn diff_row(line: &DiffLine, text: &str, digits: usize) -> DiffRow {
    let number = |index: Option<usize>| index.map_or(String::new(), |index| index.to_string());
    // Diff text carries the original line terminator; the row already ends.
    DiffRow::Line {
        kind: line.kind,
        gutter: format!(
            "{:>digits$} {:>digits$} ",
            number(line.old_index),
            number(line.new_index)
        ),
        text: sanitize(text.trim_end_matches(['\n', '\r'])).into_owned(),
    }
}

/// One selectable file in a diff.
#[derive(Debug, PartialEq, Eq)]
pub struct DiffFile {
    pub path: String,
    pub kind: ChangeKind,
    /// Added and removed lines, once the path's hunks have been read; unknown
    /// until then, because an unmeasured row must not claim `0`.
    pub lines: Option<(usize, usize)>,
    /// The read failed, so the row is not left looking unmeasured.
    pub failed: bool,
    /// Host-issued change references for this path.
    pub change_refs: Vec<ChangeRef>,
}

/// The open diff view: the file list from `changes.list` and, for the selected
/// file, its rows from `changes.diff`, sharing the `Ctrl+B` split-pane model.
#[derive(Debug, Default)]
pub struct DiffState {
    pub files: Vec<DiffFile>,
    pub filter: String,
    /// Index into `matching_files()`.
    pub selected: usize,
    pub focus: crate::ui::views::split::PaneFocus,
    /// On narrow terminals (`< 96` cols), toggles between file list and hunks.
    pub detail_open: bool,
    pub scroll: usize,
    pub hit: crate::ui::views::split::SplitHitAreas,
    /// The selected file's diff; empty while it loads.
    pub rows: Vec<DiffRow>,
    /// Why there is nothing to show, or what failed.
    pub status: String,
    /// Files still to be measured, so each is read once.
    pending: Vec<(DiffQuery, Vec<ChangeRef>)>,
    /// The one read in flight.
    in_flight: Option<DiffQuery>,
}

impl DiffState {
    pub fn set_files(&mut self, files: Vec<DiffFile>) {
        let selected_path = self.current().map(|file| file.path.clone());
        self.files = files;
        let matched = self.matching_indices();
        let selected = selected_path
            .and_then(|path| {
                matched
                    .iter()
                    .position(|&index| self.files[index].path == path)
            })
            .unwrap_or(self.selected.min(matched.len().saturating_sub(1)));
        self.select(selected);
    }

    /// Indices into `self.files` that match the current `filter`.
    pub fn matching_indices(&self) -> Vec<usize> {
        let query = self.filter.trim();
        if query.is_empty() {
            return (0..self.files.len()).collect();
        }
        self.files
            .iter()
            .enumerate()
            .filter(|(_, file)| super::picker::fuzzy_match(&file.path, query))
            .map(|(idx, _)| idx)
            .collect()
    }

    /// Aggregate `(added, removed)` across all measured files.
    pub fn total_counts(&self) -> (usize, usize) {
        self.files
            .iter()
            .filter_map(|file| file.lines)
            .fold((0, 0), |(a, r), (da, dr)| (a + da, r + dr))
    }

    /// Selects a filtered row and drops the previous row's diff at once: the
    /// pane never shows one file's diff under another file's name.
    pub fn select(&mut self, index: usize) {
        let len = self.matching_indices().len();
        self.selected = index.min(len.saturating_sub(1));
        self.scroll = 0;
        self.rows.clear();
        self.status.clear();
    }

    pub fn current(&self) -> Option<&DiffFile> {
        let matched = self.matching_indices();
        let idx = self.selected.min(matched.len().saturating_sub(1));
        matched.get(idx).and_then(|&real| self.files.get(real))
    }
}

impl App {
    pub(super) fn open_diff(&self) -> DiffState {
        let Some(session) = self.resident.current() else {
            return DiffState {
                status: "No Session selected.".into(),
                ..Default::default()
            };
        };
        let epoch = session.epoch;
        let client = session.client.clone();
        let source = session_source(session);
        self.spawn_for(epoch, async move {
            SessionOp::ChangeFiles(read_changes(&client, &source).await)
        });
        DiffState {
            status: "Loading working-tree changes…".into(),
            ..Default::default()
        }
    }

    /// Re-reads workspace changes while `/diff` stays open (helpful during long turns).
    pub(super) fn refresh_diff(&mut self) {
        let Some(session) = self.resident.current() else {
            return;
        };
        let epoch = session.epoch;
        let client = session.client.clone();
        let source = session_source(session);
        if let Overlay::Diff(diff) = &mut self.overlay {
            diff.in_flight = None;
            diff.pending.clear();
            diff.status = "Refreshing working-tree changes…".into();
        }
        self.spawn_for(epoch, async move {
            SessionOp::ChangeFiles(read_changes(&client, &source).await)
        });
    }

    /// Fills the file column, then reads every path once for its counts.
    pub(super) fn on_change_files(&mut self, result: Result<Vec<ChangeRecord>, CallError>) {
        let Overlay::Diff(diff) = &mut self.overlay else {
            return;
        };
        let list = match result {
            Ok(list) => list,
            Err(error) => {
                diff.status = error.to_string();
                return;
            }
        };
        // Workspace change references describe Git status, not content versions;
        // refresh the counts and hunks even when those references are unchanged.
        let mut files: Vec<DiffFile> = Vec::new();
        for record in &list {
            match files.iter_mut().find(|file| file.path == record.path) {
                Some(file) => file.change_refs.push(record.change_ref.clone()),
                None => {
                    files.push(DiffFile {
                        path: record.path.clone(),
                        kind: record.kind,
                        lines: None,
                        failed: false,
                        change_refs: vec![record.change_ref.clone()],
                    });
                }
            }
        }
        diff.set_files(files);
        if diff.files.is_empty() {
            diff.status = "No changes in the working tree".into();
            return;
        }
        diff.status.clear();
        diff.in_flight = None;
        diff.pending = diff
            .files
            .iter()
            .filter(|file| file.lines.is_none() && !file.failed && !file.change_refs.is_empty())
            .map(|file| {
                let query = DiffQuery {
                    path: file.path.clone(),
                };
                (query, file.change_refs.clone())
            })
            .collect();
        // Paint the current selection first, then measure the remaining files.
        self.load_selected_diff();
        self.measure_next_diff_row();
    }

    /// Starts the next unmeasured file's read; one at a time keeps within the
    /// Host workspace reads.
    fn measure_next_diff_row(&mut self) {
        let Overlay::Diff(diff) = &mut self.overlay else {
            return;
        };
        if diff.in_flight.is_some() || diff.pending.is_empty() {
            return;
        }
        let (query, refs) = diff.pending.remove(0);
        diff.in_flight = Some(query.clone());
        self.read_diff(query, refs);
    }

    /// Reads the selected file's records; for an unmeasured file this read is
    /// its measurement too.
    pub(super) fn load_selected_diff(&mut self) {
        let Overlay::Diff(diff) = &mut self.overlay else {
            return;
        };
        let Some(file) = diff.current().filter(|file| !file.change_refs.is_empty()) else {
            return;
        };
        let query = DiffQuery {
            path: file.path.clone(),
        };
        if diff.in_flight.as_ref() == Some(&query) {
            return;
        }
        let refs = file.change_refs.clone();
        diff.pending.retain(|(pending, _)| *pending != query);
        diff.in_flight = Some(query.clone());
        self.read_diff(query, refs);
    }

    fn read_diff(&self, query: DiffQuery, refs: Vec<ChangeRef>) {
        let Some(session) = self.resident.current() else {
            return;
        };
        let epoch = session.epoch;
        let client = session.client.clone();
        let source = session_source(session);
        self.spawn_for(epoch, async move {
            SessionOp::DiffFile(query, read_diffs(&client, &source, refs).await)
        });
    }

    pub(super) fn on_diff_file(
        &mut self,
        query: DiffQuery,
        result: Result<Vec<DiffResult>, CallError>,
    ) {
        let Overlay::Diff(diff) = &mut self.overlay else {
            return;
        };
        if diff.in_flight.as_ref() == Some(&query) {
            diff.in_flight = None;
        }
        let shown = diff.current().is_some_and(|file| file.path == query.path);
        let file = diff.files.iter_mut().find(|file| file.path == query.path);
        match result {
            Ok(pages) => {
                let (added, removed) = measure(&pages);
                if let Some(file) = file {
                    file.lines = Some((added, removed));
                    file.failed = false;
                }
                if shown {
                    let tools: Vec<Option<String>> = pages
                        .iter()
                        .map(|page| {
                            let ChangeRef::Tool { tool_call, .. } = &page.change_ref else {
                                return None;
                            };
                            Some(
                                self.resident
                                    .current()?
                                    .transcript
                                    .find_tool(tool_call)?
                                    .name
                                    .clone(),
                            )
                        })
                        .collect();
                    diff.rows = rows(&pages, &tools);
                    diff.scroll = 0;
                    // Records with equal before and after content have no hunks.
                    let unchanged = pages
                        .iter()
                        .all(|page| page.hunks.is_empty() && !page.binary);
                    diff.status = if unchanged {
                        "No line changes (content unchanged).".into()
                    } else if pages
                        .iter()
                        .any(|page| page.stale || page.versions_refreshed)
                    {
                        "Workspace changed after the file list; this diff shows the latest available versions.".into()
                    } else {
                        String::new()
                    };
                }
            }
            Err(error) => {
                if let Some(file) = file {
                    file.failed = true;
                }
                if shown {
                    diff.rows.clear();
                    diff.status = error.to_string();
                }
            }
        }
        self.measure_next_diff_row();
    }
}

fn session_source(session: &super::Session) -> SessionIdentity {
    SessionIdentity {
        host_id: session.state.host_id.clone(),
        host_incarnation: session.state.host_incarnation.clone(),
        agent_id: session.info.agent_id.clone(),
        session_id: session.info.id.clone(),
    }
}

pub async fn read_changes(
    client: &SessionClient,
    source: &SessionIdentity,
) -> Result<Vec<ChangeRecord>, CallError> {
    let mut records = Vec::new();
    let mut cursor = None;
    loop {
        let page = client
            .changes_list(
                ChangesListRequest {
                    scope: ChangeScope::Workspace,
                    cursor: cursor.clone(),
                    limit: 200,
                },
                CallOptions::default(),
            )
            .await?;
        if &page.source != source {
            return Err(CallError::Protocol {
                message: "workspace changes belong to another Session or Host incarnation".into(),
            });
        }
        let page = page.data;
        if page.next_cursor.is_some() && page.next_cursor == cursor {
            return Err(CallError::Protocol {
                message: "workspace changes cursor did not advance".into(),
            });
        }
        records.extend(page.records);
        cursor = page.next_cursor;
        if cursor.is_none() {
            return Ok(records);
        }
    }
}

pub async fn read_diffs(
    client: &SessionClient,
    source: &SessionIdentity,
    references: Vec<ChangeRef>,
) -> Result<Vec<DiffResult>, CallError> {
    let mut diffs = Vec::new();
    for change_ref in references {
        let comparison = match change_ref {
            ChangeRef::Tool { .. } => DiffComparison::ToolBeforeAfter,
            ChangeRef::Workspace { .. } => DiffComparison::HeadToWorktree,
        };
        let mut cursor = None;
        let mut complete: Option<DiffResult> = None;
        loop {
            let page = client
                .changes_diff(
                    DiffRequest {
                        change_ref: change_ref.clone(),
                        comparison,
                        context_lines: 3,
                        max_bytes: 64 * 1024,
                        cursor: cursor.clone(),
                    },
                    CallOptions::default(),
                )
                .await?;
            if &page.source != source
                || page.data.change_ref != change_ref
                || page.data.comparison != comparison
            {
                return Err(CallError::Protocol {
                    message: "workspace diff belongs to another source or comparison".into(),
                });
            }
            let page = page.data;
            if page.next_cursor.is_some() && page.next_cursor == cursor {
                return Err(CallError::Protocol {
                    message: "workspace diff cursor did not advance".into(),
                });
            }
            cursor = page.next_cursor.clone();
            match &mut complete {
                None => complete = Some(page),
                Some(held) => merge_diff_page(held, page),
            }
            if cursor.is_none() {
                break;
            }
        }
        diffs.push(complete.expect("one diff page was read"));
    }
    Ok(diffs)
}

fn merge_diff_page(held: &mut DiffResult, page: DiffResult) {
    for hunk in page.hunks {
        if let Some(previous) = held.hunks.last_mut()
            && (
                previous.old_start,
                previous.old_count,
                previous.new_start,
                previous.new_count,
            ) == (
                hunk.old_start,
                hunk.old_count,
                hunk.new_start,
                hunk.new_count,
            )
        {
            previous.lines.extend(hunk.lines);
        } else {
            held.hunks.push(hunk);
        }
    }
    held.complete = page.complete;
    held.truncated = page.truncated;
    held.next_cursor = page.next_cursor;
    held.stale |= page.stale;
    held.versions_refreshed |= page.versions_refreshed;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> DiffState {
        let file = |path: &str| DiffFile {
            path: path.into(),
            kind: ChangeKind::Modified,
            lines: None,
            failed: false,
            change_refs: Vec::new(),
        };
        let mut diff = DiffState::default();
        diff.set_files(vec![file("a.rs"), file("b.rs")]);
        diff
    }

    #[test]
    fn selecting_a_file_drops_the_previous_files_diff() {
        let mut diff = state();
        diff.rows = vec![DiffRow::Blank];
        diff.scroll = 3;
        diff.select(1);
        assert_eq!(diff.current().map(|file| file.path.as_str()), Some("b.rs"));
        assert!(diff.rows.is_empty() && diff.scroll == 0);
        diff.set_files(Vec::new());
        assert_eq!((diff.current(), diff.selected), (None, 0));
    }

    fn fragmented_diff(text: &str, offset: usize, complete: bool) -> DiffResult {
        use minicore_host_protocol::{ToolCallId, ToolCallRef};
        use minicore_host_workspace::{ChangeOrigin, DiffCursor, DiffHunk};
        DiffResult {
            change_ref: ChangeRef::Tool {
                session_id: "00000000000000000001".parse().unwrap(),
                index: 0,
                tool_call: ToolCallRef {
                    turn_id: "00000000000000000002".parse().unwrap(),
                    tool_call_id: ToolCallId("call".into()),
                },
            },
            comparison: DiffComparison::ToolBeforeAfter,
            origin: ChangeOrigin::Tool,
            hunks: vec![DiffHunk {
                old_start: 0,
                old_count: 0,
                new_start: 1,
                new_count: 1,
                lines: vec![DiffLine {
                    kind: LineKind::Added,
                    old_index: None,
                    new_index: Some(1),
                    line_byte_offset: offset,
                    text: text.into(),
                    line_complete: complete,
                }],
            }],
            binary: false,
            stale: false,
            versions_refreshed: false,
            next_cursor: (!complete).then_some(DiffCursor {
                fingerprint: "fixture".into(),
                hunk_index: 0,
                line_index: 0,
                line_byte_offset: 3,
            }),
            complete,
            truncated: !complete,
        }
    }

    #[test]
    fn native_unicode_fragments_join_once_and_keep_one_based_line_numbers() {
        let mut first = fragmented_diff("你", 0, false);
        merge_diff_page(&mut first, fragmented_diff("好\n", 3, true));
        assert_eq!(measure(std::slice::from_ref(&first)), (1, 0));
        assert!(first.complete && !first.truncated);
        let rendered = hunk_rows(&first);
        assert_eq!(rendered[0], DiffRow::Hunk("@@ -0,0 +1,1 @@".into()));
        assert_eq!(
            rendered[1],
            DiffRow::Line {
                kind: LineKind::Added,
                gutter: "  1 ".into(),
                text: "你好".into()
            }
        );
        assert_eq!(rendered.len(), 2);
    }
}
