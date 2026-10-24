//! The diff view: its state and the reads that fill it.

use serde_json::json;

use super::{App, Overlay, SessionOp, tasks};
use crate::protocol::changes::{DiffLine, DiffPage, LineKind};
use crate::protocol::{self, ChangeKind, ChangeList, RpcError};
use crate::wrap::sanitize;

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
    fn body(&self) -> String {
        match self {
            Self::Blank => String::new(),
            Self::Note(text) | Self::Hunk(text) => text.clone(),
            Self::Line { kind, text, .. } => format!("{}{text}", sign(*kind)),
        }
    }
}

pub fn sign(kind: LineKind) -> &'static str {
    match kind {
        LineKind::Added => "+",
        LineKind::Removed => "-",
        LineKind::Context | LineKind::Unknown => " ",
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
pub fn rows(pages: &[DiffPage], tools: &[Option<String>]) -> Vec<DiffRow> {
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
pub(crate) fn hunk_rows(page: &DiffPage) -> Vec<DiffRow> {
    if page.binary {
        return vec![DiffRow::Note("Binary file: the diff is not shown.".into())];
    }
    if page.hunks.is_empty() {
        let note = match page.availability.as_deref() {
            Some("unavailable") => "The original content is unavailable for this change.",
            Some("partial") => "Only part of this diff is available.",
            _ => "No differences.",
        };
        return vec![DiffRow::Note(note.into())];
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
            unified_start(hunk.old_start, hunk.old_count),
            hunk.old_count,
            unified_start(hunk.new_start, hunk.new_count),
            hunk.new_count
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

/// A hunk start as unified diff prints it: the first line's 1-based number, or
/// the line before an empty range.
fn unified_start(start: u64, count: u64) -> u64 {
    if count > 0 { start + 1 } else { start }
}

fn diff_row(line: &DiffLine, text: &str, digits: usize) -> DiffRow {
    let number = |index: Option<u64>| index.map_or(String::new(), |index| (index + 1).to_string());
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
    /// Opaque change references for this path.
    pub change_refs: Vec<String>,
}

/// The open diff view: the file list from `changes.list` and, for the selected
/// file, its rows from `changes.diff`.
#[derive(Debug, Default)]
pub struct DiffState {
    pub files: Vec<DiffFile>,
    pub selected: usize,
    pub scroll: usize,
    /// The selected file's diff; empty while it loads.
    pub rows: Vec<DiffRow>,
    /// Why there is nothing to show, or what failed.
    pub status: String,
    /// Files still to be measured, so each is read once.
    pending: Vec<(DiffQuery, Vec<String>)>,
    /// The one read in flight.
    in_flight: Option<DiffQuery>,
}

impl DiffState {
    pub fn set_files(&mut self, files: Vec<DiffFile>) {
        self.files = files;
        self.selected = self.selected.min(self.files.len().saturating_sub(1));
    }

    /// Selects a row and drops the previous row's diff at once: the pane never
    /// shows one file's diff under another file's name.
    pub fn select(&mut self, index: usize) {
        self.selected = index;
        self.scroll = 0;
        self.rows.clear();
        self.status.clear();
    }

    pub fn current(&self) -> Option<&DiffFile> {
        self.files.get(self.selected)
    }

    pub fn scroll_by(&mut self, delta: isize, rows: usize) {
        let max = self.rows.len().saturating_sub(rows);
        self.scroll = (self.scroll as isize)
            .saturating_add(delta)
            .clamp(0, max as isize) as usize;
    }
}

impl App {
    /// Opens the workspace's live Git changes through the Agent.
    pub(super) fn open_diff(&self) -> DiffState {
        self.call(
            protocol::METHOD_CHANGES_LIST,
            json!({"scope": "workspace", "limit": 200, "max_bytes": 262_144}),
            SessionOp::ChangeFiles,
        );
        DiffState {
            status: "Loading working-tree changes…".into(),
            ..Default::default()
        }
    }

    /// Fills the file column, then reads every path once for its counts.
    pub(super) fn on_change_files(&mut self, result: Result<ChangeList, RpcError>) {
        let Overlay::Diff(diff) = &mut self.overlay else {
            return;
        };
        let list = match result {
            Ok(list) => list,
            Err(error) => {
                diff.status = error.message;
                return;
            }
        };
        // One row per path. A row keeps its counts only while its records are
        // the same set.
        let previous = std::mem::take(&mut diff.files);
        let mut files: Vec<DiffFile> = Vec::new();
        for record in &list.records {
            match files.iter_mut().find(|file| file.path == record.path) {
                Some(file) => file.change_refs.push(record.change_ref.clone()),
                None => {
                    let measured = previous
                        .iter()
                        .find(|file| file.path == record.path)
                        .filter(|file| file.change_refs == [record.change_ref.clone()]);
                    files.push(DiffFile {
                        path: record.path.clone(),
                        kind: record.kind,
                        lines: measured.and_then(|file| file.lines),
                        failed: measured.is_some_and(|file| file.failed),
                        change_refs: vec![record.change_ref.clone()],
                    });
                }
            }
        }
        diff.set_files(files);
        if diff.files.is_empty() {
            diff.status = if list
                .warnings
                .iter()
                .any(|warning| warning == "no_repository")
            {
                "Git working tree unavailable."
            } else if list
                .warnings
                .iter()
                .any(|warning| warning == "status_incomplete")
            {
                "Git status is incomplete."
            } else {
                "No changes in the working tree"
            }
            .into();
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
        self.measure_next_diff_row();
        // Every file was already measured: the selected one still needs painting.
        if matches!(&self.overlay, Overlay::Diff(diff) if diff.in_flight.is_none()) {
            self.load_selected_diff();
        }
    }

    /// Starts the next unmeasured file's read; one at a time keeps within the
    /// Agent's query allowance.
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

    fn read_diff(&self, query: DiffQuery, refs: Vec<String>) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let client = self.client.clone();
        let session_id = session.info.session_id.clone();
        self.spawn(async move {
            let pages = tasks::read_diffs(client, session_id, refs).await;
            SessionOp::DiffFile(query, pages)
        });
    }

    pub(super) fn on_diff_file(
        &mut self,
        query: DiffQuery,
        result: Result<Vec<DiffPage>, RpcError>,
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
                // Records are counted one by one: line indexes only identify a
                // line within a record.
                let (added, removed) = pages
                    .iter()
                    .map(|page| protocol::changes::count_lines(std::slice::from_ref(page)))
                    .fold((0, 0), |sum, count| (sum.0 + count.0, sum.1 + count.1));
                if let Some(file) = file {
                    file.lines = Some((added, removed));
                    file.failed = false;
                }
                if shown {
                    let tools: Vec<Option<String>> = pages
                        .iter()
                        .map(|page| {
                            let key = page.tool_ref.as_ref()?;
                            let call = self.session.as_ref()?.transcript.find_tool(key)?;
                            Some(call.name.clone())
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
                    diff.status = error.message;
                }
            }
        }
        self.measure_next_diff_row();
    }
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

    #[test]
    fn scrolling_stops_at_the_last_full_page() {
        let mut diff = state();
        diff.rows = (0..10)
            .map(|index| DiffRow::Note(index.to_string()))
            .collect();
        diff.scroll_by(3, 4);
        assert_eq!(diff.scroll, 3);
        diff.scroll_by(99, 4);
        assert_eq!(diff.scroll, 6, "the last page stays full");
        diff.scroll_by(-99, 4);
        assert_eq!(diff.scroll, 0);
    }
}
