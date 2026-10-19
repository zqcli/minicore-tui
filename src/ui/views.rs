//! Full-screen views: diff, tool output, context and help. Each one replaces
//! the transcript area and keeps the footer. Rendering is pure: the view is
//! built from data the app already holds (or from a fetched page) and the app
//! owns scrolling and selection.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::protocol::changes::{DiffLine, DiffPage, LineKind};
use crate::protocol::{ChangeKind, SessionContext, SessionPresentation};
use crate::theme::Palette;

use super::panel::truncate;
use super::wrap::{sanitize, width, wrap};

/// File-list column width, as a fraction of the view when the terminal is wide
/// enough for a side-by-side layout.
const FILE_COLUMN: u16 = 30;
const MIN_SPLIT_WIDTH: u16 = 80;
const MIN_SPLIT_HEIGHT: u16 = 12;

/// One selectable file in a diff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffFile {
    pub path: String,
    pub kind: ChangeKind,
    /// Added and removed lines, once the path's hunks have been read. Unknown
    /// until then: a row that has not been measured must not claim `0`.
    pub lines: Option<(usize, usize)>,
    /// Set when the read failed, so the row is not left looking unmeasured.
    pub failed: bool,
    /// Change records that touched this path, oldest first. One row per path,
    /// because a turn can change the same file several times.
    pub change_refs: Vec<String>,
}

impl DiffFile {
    /// The row's line column: `+a −b` once measured, `?` when the read failed
    /// and `·` while unknown.
    fn count_spans(&self, palette: &Palette) -> Vec<Span<'static>> {
        match self.lines {
            Some((added, removed)) => vec![
                Span::styled(format!("+{added}"), Style::default().fg(palette.added)),
                Span::raw(" "),
                Span::styled(format!("−{removed}"), Style::default().fg(palette.removed)),
            ],
            None if self.failed => vec![Span::styled("?", Style::default().fg(palette.dim))],
            None => vec![Span::styled("·", Style::default().fg(palette.dim))],
        }
    }

    fn marker(&self) -> &'static str {
        match self.kind {
            ChangeKind::Added => "A",
            ChangeKind::Deleted => "D",
            ChangeKind::Renamed => "R",
            ChangeKind::Conflict => "!",
            ChangeKind::Modified | ChangeKind::Unknown => "M",
        }
    }
}

/// The diff view's data: the file list from `changes.list` and, for the
/// selected file, the hunks from `changes.diff`.
#[derive(Clone, Debug)]
pub struct DiffView {
    pub title: String,
    /// The scope these records came from; a read from another scope must not
    /// paint this list.
    pub scope: crate::app::DiffScope,
    pub files: Vec<DiffFile>,
    pub selected: usize,
    pub scroll: usize,
    /// Diff lines of the selected file, or the reason they are unavailable.
    pub hunks: Vec<Line<'static>>,
    pub status: String,
}

impl DiffView {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            scope: crate::app::DiffScope::Session,
            files: Vec::new(),
            selected: 0,
            scroll: 0,
            hunks: Vec::new(),
            status: String::new(),
        }
    }

    pub fn set_files(&mut self, files: Vec<DiffFile>) {
        self.files = files;
        self.selected = self.selected.min(self.files.len().saturating_sub(1));
    }

    /// Selects a row and drops the previous row's hunks at once: the pane must
    /// never show one file's diff under another file's title, not even for the
    /// moment a read takes.
    pub fn select(&mut self, index: usize) {
        if index < self.files.len() {
            self.selected = index;
            self.scroll = 0;
            self.hunks.clear();
            self.status.clear();
        }
    }

    pub fn current(&self) -> Option<&DiffFile> {
        self.files.get(self.selected)
    }

    /// The diff lines scrolled to the top of the hunk pane.
    pub fn visible_hunks(&self, rows: usize) -> &[Line<'static>] {
        let start = self.scroll.min(self.hunks.len());
        let end = (start + rows).min(self.hunks.len());
        &self.hunks[start..end]
    }

    pub fn scroll_by(&mut self, delta: isize, rows: usize) {
        let max = self.hunks.len().saturating_sub(rows);
        let next = (self.scroll as isize)
            .saturating_add(delta)
            .clamp(0, max as isize);
        self.scroll = next as usize;
    }
}

/// The plain-text body `/copy` and the `c` key use. A row's last span is its
/// content; earlier spans are the line-number gutter, which is not copied.
pub fn diff_text(view: &DiffView) -> String {
    let mut out = String::new();
    if let Some(file) = view.current() {
        out.push_str(&file.path);
        out.push('\n');
    }
    for line in &view.hunks {
        if let Some(body) = line.spans.last() {
            out.push_str(&body.content);
        }
        out.push('\n');
    }
    out
}

/// Rows for every change record of one file. Several records (several edits
/// of the file) are separate comparisons, so each gets a dim separator naming
/// the tool that made it; reading their hunks as one diff would be wrong.
pub fn records_lines(
    pages: &[DiffPage],
    tools: &[Option<String>],
    palette: &Palette,
) -> Vec<Line<'static>> {
    if pages.len() <= 1 {
        return match pages.first() {
            Some(page) => hunk_lines(page, palette),
            None => vec![dim("No differences.", palette)],
        };
    }
    let mut lines = Vec::new();
    for (index, page) in pages.iter().enumerate() {
        let position = format!("change {} of {}", index + 1, pages.len());
        let label = match tools.get(index).and_then(Option::as_deref) {
            Some(tool) => format!("── {tool} · {position} ──"),
            None => format!("── {position} ──"),
        };
        if index > 0 {
            lines.push(Line::from(""));
        }
        lines.push(dim(&label, palette));
        lines.extend(hunk_lines(page, palette));
    }
    lines
}

/// Turns one `changes.diff` page into styled rows: `[gutter, body]` for diff
/// lines, one span for hunk headers. Header numbers are unified-diff 1-based;
/// the Agent's starts are 0-based line indexes.
pub fn hunk_lines(page: &DiffPage, palette: &Palette) -> Vec<Line<'static>> {
    if page.binary {
        return vec![dim("Binary file: the diff is not shown.", palette)];
    }
    if page.hunks.is_empty() {
        let note = match page.availability.as_deref() {
            Some("unavailable") => "The original content is unavailable for this change.",
            Some("partial") => "Only part of this diff is available.",
            _ => "No differences.",
        };
        return vec![dim(note, palette)];
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
    let mut lines = Vec::new();
    for hunk in &page.hunks {
        lines.push(Line::from(Span::styled(
            format!(
                "@@ -{},{} +{},{} @@",
                unified_start(hunk.old_start, hunk.old_count),
                hunk.old_count,
                unified_start(hunk.new_start, hunk.new_count),
                hunk.new_count
            ),
            Style::default().fg(palette.hunk),
        )));
        // A long line arrives as several fragments; only the last one is
        // `line_complete`. They are joined back into one row.
        let mut pending: Option<(&DiffLine, String)> = None;
        for line in &hunk.lines {
            let text = match pending.take() {
                Some((first, mut text)) => {
                    text.push_str(&line.text);
                    (first, text)
                }
                None => (line, line.text.clone()),
            };
            if line.line_complete {
                lines.push(diff_row(text.0, &text.1, digits, palette));
            } else {
                pending = Some(text);
            }
        }
        if let Some((first, text)) = pending {
            lines.push(diff_row(first, &text, digits, palette));
        }
    }
    if !page.complete {
        lines.push(dim("… the diff continues", palette));
    }
    lines
}

/// A hunk start as unified diff prints it: the first line's 1-based number, or
/// the line before an empty range.
fn unified_start(start: u64, count: u64) -> u64 {
    if count > 0 { start + 1 } else { start }
}

/// One diff row: a dim `old new` line-number gutter, then the signed text.
fn diff_row(line: &DiffLine, text: &str, digits: usize, palette: &Palette) -> Line<'static> {
    let (sign, style) = match line.kind {
        LineKind::Added => ("+", Style::default().fg(palette.added)),
        LineKind::Removed => ("-", Style::default().fg(palette.removed)),
        LineKind::Context | LineKind::Unknown => (" ", Style::default().fg(palette.dim)),
    };
    let number = |index: Option<u64>| index.map_or(String::new(), |index| (index + 1).to_string());
    let gutter = format!(
        "{:>digits$} {:>digits$} ",
        number(line.old_index),
        number(line.new_index)
    );
    // Diff text carries the original line terminator; the row already ends,
    // so only the trailing newline is dropped.
    let text = sanitize(text.trim_end_matches(['\n', '\r']));
    Line::from(vec![
        Span::styled(gutter, Style::default().fg(palette.faint)),
        Span::styled(format!("{sign}{text}"), style),
    ])
}

fn dim(text: &str, palette: &Palette) -> Line<'static> {
    Line::from(Span::styled(
        text.to_owned(),
        Style::default().fg(palette.dim),
    ))
}

/// Shared view chrome: the title row and the dim hint row.
fn chrome(frame: &mut Frame, area: Rect, palette: &Palette, title: &str, hint: &str) {
    let buffer = frame.buffer_mut();
    buffer.set_line(
        area.x,
        area.y,
        &Line::from(Span::styled(
            title.to_owned(),
            Style::default()
                .fg(palette.accent)
                .add_modifier(Modifier::BOLD),
        )),
        area.width,
    );
    if area.height > 1 {
        buffer.set_line(area.x, area.bottom() - 1, &dim(hint, palette), area.width);
    }
}

/// Where the file list and the hunk pane go.
fn split(area: Rect) -> (Option<Rect>, Rect) {
    let body = Rect {
        y: area.y + 1,
        height: area.height.saturating_sub(2),
        ..area
    };
    if area.width < MIN_SPLIT_WIDTH || body.height < MIN_SPLIT_HEIGHT {
        return (None, body);
    }
    let list = Rect {
        width: FILE_COLUMN,
        ..body
    };
    let hunks = Rect {
        x: body.x + FILE_COLUMN + 1,
        width: body.width.saturating_sub(FILE_COLUMN + 1),
        ..body
    };
    (Some(list), hunks)
}

pub fn render_diff(frame: &mut Frame, area: Rect, palette: &Palette, view: &DiffView) {
    let title = if let Some(file) = view.current() {
        format!("← {} · {}", view.title, file.path)
    } else {
        format!("← {}", view.title)
    };
    chrome(
        frame,
        area,
        palette,
        &title,
        "tab scope · ↑↓ file · PgUp/PgDn scroll · c copy · esc back",
    );
    let (list, hunks) = split(area);
    if let Some(list) = list {
        render_file_list(frame, list, palette, view);
    }
    render_hunks(frame, hunks, palette, view);
}

fn render_file_list(frame: &mut Frame, area: Rect, palette: &Palette, view: &DiffView) {
    let rows = area.height as usize;
    let first = view.selected.saturating_sub(rows.saturating_sub(1));
    let counts: Vec<Vec<Span<'static>>> = view
        .files
        .iter()
        .map(|file| file.count_spans(palette))
        .collect();
    let span_width = |spans: &[Span<'static>]| spans.iter().map(|span| width(&span.content)).sum();
    let column = counts
        .iter()
        .map(|spans| span_width(spans))
        .max()
        .unwrap_or(1);
    for (row, (index, file)) in view
        .files
        .iter()
        .enumerate()
        .skip(first)
        .take(rows)
        .enumerate()
    {
        let y = area.y + row as u16;
        let selected = index == view.selected;
        let style = if selected {
            let band = Rect { height: 1, ..area };
            let band = Rect { y, ..band };
            frame
                .buffer_mut()
                .set_style(band, Style::default().bg(palette.selection_bg));
            Style::default()
                .fg(palette.text)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette.dim)
        };
        let count = &counts[index];
        let mut spans = vec![Span::styled(format!("{} ", file.marker()), style)];
        spans.extend(count.iter().cloned());
        let path_width = (area.width as usize).saturating_sub(column + 3);
        spans.push(Span::raw(" ".repeat(column - span_width(count) + 1)));
        spans.push(Span::styled(truncate(&file.path, path_width), style));
        frame
            .buffer_mut()
            .set_line(area.x, y, &Line::from(spans), area.width);
    }
    if let Some(file) = view.current()
        && area.height > 1
    {
        let counts = match file.lines {
            Some((added, removed)) => Line::from(vec![
                Span::styled(format!("+{added} "), Style::default().fg(palette.added)),
                Span::styled(format!("−{removed}"), Style::default().fg(palette.removed)),
            ]),
            None => dim(
                if file.failed {
                    "line counts unavailable"
                } else {
                    "reading…"
                },
                palette,
            ),
        };
        frame
            .buffer_mut()
            .set_line(area.x, area.bottom() - 1, &counts, area.width);
    }
}

fn render_hunks(frame: &mut Frame, area: Rect, palette: &Palette, view: &DiffView) {
    if view.hunks.is_empty() {
        let message = if view.status.is_empty() {
            "No changes to show."
        } else {
            view.status.as_str()
        };
        frame
            .buffer_mut()
            .set_line(area.x, area.y, &dim(message, palette), area.width);
        return;
    }
    // The last span is the content and wraps; earlier spans are the gutter,
    // drawn on the first row and left blank on continuation rows.
    let mut y = area.y;
    for line in view.visible_hunks(area.height as usize) {
        let Some((body, gutter)) = line.spans.split_last() else {
            y += 1;
            continue;
        };
        let indent: usize = gutter.iter().map(|span| width(&span.content)).sum();
        let room = (area.width as usize).saturating_sub(indent).max(1);
        for (index, chunk) in wrap(&body.content, room).into_iter().enumerate() {
            if y >= area.bottom() {
                return;
            }
            let mut spans = if index == 0 {
                gutter.to_vec()
            } else {
                vec![Span::raw(" ".repeat(indent))]
            };
            spans.push(Span::styled(chunk, body.style));
            frame
                .buffer_mut()
                .set_line(area.x, y, &Line::from(spans), area.width);
            y += 1;
        }
    }
}

/// Human label of a tool card state, shared by the view and the card.
pub fn tool_state_label(state: crate::transcript::ToolState) -> &'static str {
    match state {
        crate::transcript::ToolState::Pending => "requested",
        crate::transcript::ToolState::Running => "running",
        crate::transcript::ToolState::Succeeded => "succeeded",
        crate::transcript::ToolState::Failed => "failed",
        crate::transcript::ToolState::Denied => "denied",
        crate::transcript::ToolState::Cancelled => "cancelled",
    }
}

/// The tool output view: the call's input on top, then the paged output.
/// Until every stream's pages arrive the output is the card's bounded tail.
#[derive(Clone, Debug, Default)]
pub struct ToolView {
    /// The call shown; a page for any other call is ignored.
    pub key: Option<crate::protocol::ToolRef>,
    pub title: String,
    pub detail: String,
    pub state: String,
    /// This call's place among the session's tool cards: `(index, count)`.
    pub position: Option<(usize, usize)>,
    pub input: Vec<Line<'static>>,
    pub output: Vec<Line<'static>>,
    /// Full output per stream, in display order; `None` while loading. Bash
    /// has stdout and stderr, which carry no total order between them, so
    /// they are shown as separate sections rather than interleaved.
    pub streams: Vec<(String, Option<crate::app::tasks::StreamText>)>,
    pub scroll: usize,
    pub status: String,
}

impl ToolView {
    /// Lines of a plain text body, made safe for the terminal.
    pub fn text_lines(text: &str) -> Vec<Line<'static>> {
        let text = text.strip_suffix('\n').unwrap_or(text);
        if text.is_empty() {
            return Vec::new();
        }
        text.split('\n')
            .map(|line| Line::raw(sanitize(line.strip_suffix('\r').unwrap_or(line)).into_owned()))
            .collect()
    }

    /// Records one stream's full text. The output is rebuilt once every stream
    /// has answered, so the tail is not replaced by half of the result.
    pub fn set_stream(
        &mut self,
        name: &str,
        text: crate::app::tasks::StreamText,
        palette: &Palette,
    ) {
        let Some(slot) = self.streams.iter_mut().find(|(stream, _)| stream == name) else {
            return;
        };
        slot.1 = Some(text);
        if self.streams.iter().all(|(_, text)| text.is_some()) {
            self.output = self.stream_lines(palette);
            self.status.clear();
            self.scroll = self.scroll.min(self.output.len().saturating_sub(1));
        }
    }

    fn stream_lines(&self, palette: &Palette) -> Vec<Line<'static>> {
        let sections = self.streams.len() > 1;
        let mut lines = Vec::new();
        for (name, text) in &self.streams {
            let Some(text) = text else { continue };
            let body = Self::text_lines(&text.text);
            let noted = text.dropped_head || text.expired || text.cut;
            if body.is_empty() && !noted {
                continue;
            }
            if sections {
                if !lines.is_empty() {
                    lines.push(Line::from(""));
                }
                let color = if name == "stderr" {
                    palette.warning
                } else {
                    palette.dim
                };
                lines.push(Line::styled(
                    format!("── {name} ──"),
                    Style::default().fg(color),
                ));
            }
            if text.dropped_head {
                lines.push(dim("… earlier output is no longer retained", palette));
            }
            if text.expired && body.is_empty() {
                lines.push(dim("… this output has expired from the agent", palette));
            }
            lines.extend(body);
            if text.cut {
                lines.push(dim("… cut at 1 MiB", palette));
            }
        }
        lines
    }

    pub fn visible_output(&self, rows: usize) -> &[Line<'static>] {
        let start = self.scroll.min(self.output.len());
        let end = (start + rows).min(self.output.len());
        &self.output[start..end]
    }

    pub fn scroll_by(&mut self, delta: isize, rows: usize) {
        let max = self.output.len().saturating_sub(rows);
        self.scroll = (self.scroll as isize)
            .saturating_add(delta)
            .clamp(0, max as isize) as usize;
    }

    /// `input` plus `output`, so `c` copies everything the user can see.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for line in self.input.iter().chain(self.output.iter()) {
            out.extend(line.spans.iter().map(|span| span.content.as_ref()));
            out.push('\n');
        }
        out
    }
}

pub fn render_tool(frame: &mut Frame, area: Rect, palette: &Palette, view: &ToolView) {
    let mut title = format!("← {} · {}", view.title, view.state);
    if let Some((index, count)) = view.position {
        title.push_str(&format!(" · tool {} of {count}", index + 1));
    }
    if !view.status.is_empty() && !view.output.is_empty() {
        title.push_str(&format!(" · {}", view.status));
    }
    chrome(
        frame,
        area,
        palette,
        &title,
        "[ ] previous/next tool · ↑↓ PgUp/PgDn scroll · c copy · esc back",
    );
    let mut y = area.y + 1;
    let bottom = area.bottom().saturating_sub(1);
    if !view.detail.is_empty() {
        frame.buffer_mut().set_line(
            area.x,
            y,
            &Span::styled(
                sanitize(&view.detail).into_owned(),
                Style::default().fg(palette.dim),
            )
            .into(),
            area.width,
        );
        y += 1;
    }
    for line in &view.input {
        if y >= bottom {
            return;
        }
        frame.buffer_mut().set_line(area.x, y, line, area.width);
        y += 1;
    }
    if y < bottom {
        y += 1;
    }
    let rows = bottom.saturating_sub(y) as usize;
    if rows == 0 {
        return;
    }
    if view.output.is_empty() {
        let message = if view.status.is_empty() {
            "No output."
        } else {
            view.status.as_str()
        };
        frame
            .buffer_mut()
            .set_line(area.x, y, &dim(message, palette), area.width);
        return;
    }
    for (row, line) in view.visible_output(rows).iter().enumerate() {
        frame
            .buffer_mut()
            .set_line(area.x, y + row as u16, line, area.width);
    }
}

/// The context view: one readable summary, not a ledger of twenty numbers.
#[derive(Clone, Debug, Default)]
pub struct ContextView {
    pub title: String,
    pub lines: Vec<Line<'static>>,
}

/// Builds the summary from the two authoritative views, labelling estimates
/// as estimates and keeping unknowns as `?` instead of zero.
pub fn context_view(
    title: impl Into<String>,
    context: Option<&SessionContext>,
    presentation: Option<&SessionPresentation>,
    palette: &Palette,
) -> ContextView {
    let mut lines = Vec::new();
    let usage = presentation.and_then(|view| view.context.as_ref());

    let reported = usage.and_then(|usage| match (usage.tokens, usage.window) {
        (Some(tokens), Some(window)) if window > 0 => {
            let percent = usage
                .percent
                .unwrap_or(tokens as f64 / window as f64 * 100.0);
            Some(format!(
                "Context: {} of {} tokens ({percent:.0}%) — reported by the last request",
                super::tokens(tokens),
                super::tokens(window)
            ))
        }
        _ => None,
    });
    let budget = context.and_then(|context| context.budget.as_ref());
    let estimated = budget.and_then(|budget| {
        let used = budget
            .estimated_request_context_tokens
            .or(budget.estimated_history_tokens)?;
        let limit = budget.input_budget_tokens?;
        let percent = if limit > 0 {
            format!(" ({:.0}%)", used as f64 / limit as f64 * 100.0)
        } else {
            String::new()
        };
        Some(format!(
            "History estimate: {} of {} input tokens{percent}",
            super::tokens(used),
            super::tokens(limit)
        ))
    });
    match reported.or(estimated) {
        Some(line) => lines.push(Line::from(Span::styled(
            line,
            Style::default().fg(palette.text),
        ))),
        None => lines.push(dim("Context usage is not reported yet.", palette)),
    }

    if let Some(budget) = budget {
        if let (Some(trigger), Some(limit)) = (budget.trigger_tokens, budget.input_budget_tokens) {
            let percent = if limit > 0 {
                format!(" ({:.0}%)", trigger as f64 / limit as f64 * 100.0)
            } else {
                String::new()
            };
            lines.push(dim(
                &format!(
                    "Auto-compacts at {} tokens{percent}{}",
                    super::tokens(trigger),
                    match budget.target_tokens {
                        Some(target) => format!(", keeping about {}", super::tokens(target)),
                        None => String::new(),
                    }
                ),
                palette,
            ));
        } else {
            lines.push(dim("Automatic compaction is disabled.", palette));
        }
    }

    if let Some(context) = context {
        if let Some(operation) = &context.current_operation {
            lines.push(Line::from(Span::styled(
                format!(
                    "Compacting now: {} · {}",
                    operation_label(&operation.phase),
                    phase_counts(operation)
                ),
                Style::default().fg(palette.warning),
            )));
        }
        if let Some(last) = &context.last_result {
            let origin = match last.origin.as_deref() {
                Some("automatic") => "automatic",
                Some("manual") => "manual",
                _ => "last",
            };
            let mut summary = match (last.before_tokens, last.after_tokens) {
                (Some(before), Some(after)) => match origin {
                    "last" => format!(
                        "Last compaction: {} → {} tokens",
                        super::tokens(before),
                        super::tokens(after)
                    ),
                    other => format!(
                        "Last compaction ({other}): {} → {} tokens",
                        super::tokens(before),
                        super::tokens(after)
                    ),
                },
                _ => format!("Last compaction: {}", compact_status(last.status)),
            };
            if let Some(covered) = last.covered_loop_count {
                summary.push_str(&format!(" · covers {covered} turns"));
            }
            if let (Some(items), Some(retained)) =
                (last.covered_item_count, last.retained_item_count)
            {
                summary.push_str(&format!(" · {items} items, {retained} kept"));
            }
            lines.push(dim(&summary, palette));
        }
        if let Some(coverage) = &context.coverage {
            lines.push(dim(
                &format!(
                    "Summary covers {} turns · {} items · {} kept",
                    coverage.covered_loop_count,
                    coverage.covered_item_count,
                    coverage.retained_item_count
                ),
                palette,
            ));
        }
        if let Some(failure) = &context.last_prepare_failure {
            lines.push(Line::from(Span::styled(
                format!("Last preparation failed: {failure}"),
                Style::default().fg(palette.failure),
            )));
        }
    } else {
        lines.push(dim(
            "No context report is loaded for this session.",
            palette,
        ));
    }
    lines.push(Line::from(""));
    lines.push(dim(
        "Estimates are byte-based and exclude the system prompt and tool schemas.",
        palette,
    ));

    ContextView {
        title: title.into(),
        lines,
    }
}

fn phase_counts(operation: &crate::protocol::CompactionProgress) -> String {
    format!(
        "{} items prepared · {} kept",
        operation.covered_item_count, operation.retained_item_count
    )
}

fn operation_label(phase: &crate::protocol::CompactionPhase) -> &'static str {
    match phase {
        crate::protocol::CompactionPhase::Preparing => "preparing",
        crate::protocol::CompactionPhase::Summarizing => "summarizing",
        crate::protocol::CompactionPhase::Merging => "merging",
        crate::protocol::CompactionPhase::Committing => "committing",
        crate::protocol::CompactionPhase::Unknown => "working",
    }
}

fn compact_status(status: crate::protocol::CompactStatus) -> &'static str {
    match status {
        crate::protocol::CompactStatus::Compacted => "compacted",
        crate::protocol::CompactStatus::Noop => "nothing to compact",
        crate::protocol::CompactStatus::Failed => "failed",
        crate::protocol::CompactStatus::UnknownWrite => "write outcome unknown",
        crate::protocol::CompactStatus::Unknown => "unknown",
    }
}

pub fn render_context(frame: &mut Frame, area: Rect, palette: &Palette, view: &ContextView) {
    chrome(
        frame,
        area,
        palette,
        &format!("← {}", view.title),
        "r refresh · esc back",
    );
    let bottom = area.bottom().saturating_sub(1);
    let mut y = area.y + 1;
    for line in &view.lines {
        if y >= bottom {
            break;
        }
        let text: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        for chunk in wrap(&text, area.width as usize) {
            if y >= bottom {
                return;
            }
            let style = line
                .spans
                .first()
                .map(|span| span.style)
                .unwrap_or_default();
            frame.buffer_mut().set_line(
                area.x,
                y,
                &Line::from(Span::styled(chunk, style)),
                area.width,
            );
            y += 1;
        }
    }
}

/// The help view: keys on the left, commands on the right when there is room.
#[derive(Clone, Debug)]
pub struct HelpView {
    pub title: String,
    pub lines: Vec<Line<'static>>,
}

/// Key rows from one table, so the app and the help view cannot disagree.
pub struct KeyRow {
    pub keys: &'static str,
    pub action: &'static str,
}

pub const KEYS: &[KeyRow] = &[
    KeyRow {
        keys: "enter",
        action: "send, or steer while a turn runs",
    },
    KeyRow {
        keys: "alt+enter",
        action: "queue a follow-up / newline",
    },
    KeyRow {
        keys: "shift+enter, ctrl+j",
        action: "newline",
    },
    KeyRow {
        keys: "esc",
        action: "close panel or view · clear selection · interrupt",
    },
    KeyRow {
        keys: "ctrl+c",
        action: "clear the composer; twice to quit",
    },
    KeyRow {
        keys: "ctrl+d",
        action: "quit when the composer is empty",
    },
    KeyRow {
        keys: "↑ ↓",
        action: "move in the text, then browse prompt history",
    },
    KeyRow {
        keys: "alt+↑",
        action: "pull the last queued message back",
    },
    KeyRow {
        keys: "ctrl+g",
        action: "edit the draft in $VISUAL/$EDITOR",
    },
    KeyRow {
        keys: "/",
        action: "command palette",
    },
    KeyRow {
        keys: "@",
        action: "file picker",
    },
    KeyRow {
        keys: "ctrl+r",
        action: "sessions",
    },
    KeyRow {
        keys: "ctrl+l",
        action: "model",
    },
    KeyRow {
        keys: "shift+tab",
        action: "cycle the reasoning level",
    },
    KeyRow {
        keys: "ctrl+o",
        action: "expand or fold all tool cards",
    },
    KeyRow {
        keys: "ctrl+t",
        action: "show or hide reasoning",
    },
    KeyRow {
        keys: "ctrl+f",
        action: "find in the transcript",
    },
    KeyRow {
        keys: "pgup/pgdn, ctrl+home/end",
        action: "scroll the transcript",
    },
    KeyRow {
        keys: "ctrl+↑ ↓",
        action: "jump between user messages",
    },
    KeyRow {
        keys: "f1",
        action: "this help",
    },
];

pub fn help_view(commands: &[crate::app::commands::Command], palette: &Palette) -> HelpView {
    let mut lines = Vec::new();
    lines.push(dim("Keys", palette));
    for row in KEYS {
        lines.push(Line::from(vec![
            Span::styled(
                format!("{:<26}", row.keys),
                Style::default().fg(palette.accent),
            ),
            Span::styled(row.action, Style::default().fg(palette.text)),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(dim("Commands", palette));
    for command in commands {
        lines.push(Line::from(vec![
            Span::styled(
                format!("/{:<25}", command.name),
                Style::default().fg(palette.accent),
            ),
            Span::styled(command.description, Style::default().fg(palette.text)),
        ]));
    }
    HelpView {
        title: "Help".into(),
        lines,
    }
}

pub fn render_help(
    frame: &mut Frame,
    area: Rect,
    palette: &Palette,
    view: &HelpView,
    scroll: usize,
) {
    chrome(
        frame,
        area,
        palette,
        &format!("← {}", view.title),
        "pgup/pgdn scroll · esc back",
    );
    let bottom = area.bottom().saturating_sub(1);
    let rows = bottom.saturating_sub(area.y + 1) as usize;
    let start = scroll.min(view.lines.len());
    for (row, line) in view.lines.iter().skip(start).take(rows).enumerate() {
        frame
            .buffer_mut()
            .set_line(area.x, area.y + 1 + row as u16, line, area.width);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use serde_json::json;

    fn capture(draw: impl FnOnce(&mut Frame), width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        buffer
            .content()
            .chunks(width as usize)
            .map(|row| {
                row.iter()
                    .map(|cell| cell.symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    fn page() -> DiffPage {
        serde_json::from_value(json!({
            "change_ref": "tool:abc", "path": "src/main.rs", "complete": true,
            "hunks": [{"old_start": 9, "old_count": 2, "new_start": 9, "new_count": 3,
                       "lines": [
                         {"kind": "context", "old_index": 9, "new_index": 9,
                          "text": " fn main() {", "line_complete": true},
                         {"kind": "removed", "old_index": 10, "text": "  old();",
                          "line_complete": true},
                         {"kind": "added", "new_index": 10, "text": "  new();",
                          "line_complete": true},
                       ]}]
        }))
        .unwrap()
    }

    fn row_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn hunk_lines_keep_the_diff_shape_and_colors() {
        let palette = Palette::dark();
        let lines = hunk_lines(&page(), &palette);
        let text: Vec<String> = lines.iter().map(row_text).collect();
        assert_eq!(
            text,
            [
                "@@ -10,2 +10,3 @@",
                "10 10   fn main() {",
                "11    -  old();",
                "   11 +  new();"
            ]
        );
        assert_eq!(lines[0].spans[0].style.fg, Some(palette.hunk));
        assert_eq!(lines[1].spans[0].style.fg, Some(palette.faint), "gutter");
        assert_eq!(lines[2].spans[1].style.fg, Some(palette.removed));
        assert_eq!(lines[3].spans[1].style.fg, Some(palette.added));
    }

    /// The Agent's starts are 0-based indexes; a header prints the 1-based
    /// first line, or the line before an empty range, as unified diff does.
    #[test]
    fn hunk_headers_use_unified_one_based_numbers() {
        let palette = Palette::dark();
        let page: DiffPage = serde_json::from_value(json!({
            "change_ref": "tool:a", "path": "a.py", "complete": true,
            "hunks": [
                {"old_start": 0, "old_count": 2, "new_start": 0, "new_count": 2, "lines": []},
                {"old_start": 5, "old_count": 0, "new_start": 5, "new_count": 1, "lines": []}
            ]
        }))
        .unwrap();
        let text: Vec<String> = hunk_lines(&page, &palette).iter().map(row_text).collect();
        assert_eq!(text, ["@@ -1,2 +1,2 @@", "@@ -5,0 +6,1 @@"]);
    }

    #[test]
    fn a_line_split_into_fragments_is_one_row() {
        let palette = Palette::dark();
        let page: DiffPage = serde_json::from_value(json!({
            "change_ref": "tool:a", "path": "a.py", "complete": true,
            "hunks": [{"old_start": 0, "old_count": 0, "new_start": 0, "new_count": 1,
                       "lines": [
                         {"kind": "added", "new_index": 0, "text": "long ", "line_complete": false},
                         {"kind": "added", "new_index": 0, "text": "line\n", "line_complete": true}
                       ]}]
        }))
        .unwrap();
        let text: Vec<String> = hunk_lines(&page, &palette).iter().map(row_text).collect();
        assert_eq!(text[1..], ["  1 +long line"]);
    }

    #[test]
    fn several_records_of_one_file_are_separated_and_named() {
        let palette = Palette::dark();
        let pages = [page(), page(), page()];
        let tools = [Some("write".to_owned()), Some("edit".to_owned()), None];
        let text: Vec<String> = records_lines(&pages, &tools, &palette)
            .iter()
            .map(row_text)
            .collect();
        let separators: Vec<&String> = text.iter().filter(|row| row.starts_with("──")).collect();
        assert_eq!(
            separators,
            [
                "── write · change 1 of 3 ──",
                "── edit · change 2 of 3 ──",
                "── change 3 of 3 ──"
            ]
        );
        // A single record needs no separator.
        let single: Vec<String> = records_lines(&pages[..1], &tools[..1], &palette)
            .iter()
            .map(row_text)
            .collect();
        assert!(
            !single.iter().any(|row| row.starts_with("──")),
            "{single:?}"
        );
    }

    #[test]
    fn diff_text_never_leaks_the_original_line_terminator() {
        let palette = Palette::dark();
        let page: DiffPage = serde_json::from_value(json!({
            "change_ref": "tool:abc", "path": "a.rs", "complete": true,
            "hunks": [{"old_start": 1, "old_count": 0, "new_start": 1, "new_count": 2,
                       "lines": [
                         {"kind": "added", "text": "added();\n", "line_complete": true},
                         {"kind": "removed", "text": "gone();\r\n", "line_complete": true},
                       ]}]
        }))
        .unwrap();
        let lines = hunk_lines(&page, &palette);
        let text: Vec<String> = lines
            .iter()
            .map(|line| line.spans.last().unwrap().content.to_string())
            .collect();
        assert_eq!(text[1], "+added();");
        assert_eq!(text[2], "-gone();");
        assert!(!text[1].contains('\n'), "{text:?}");
    }

    #[test]
    fn an_empty_page_explains_itself_instead_of_drawing_nothing() {
        let palette = Palette::dark();
        let empty: DiffPage =
            serde_json::from_value(json!({"change_ref": "tool:a", "path": "a"})).unwrap();
        assert_eq!(hunk_lines(&empty, &palette).len(), 1);
        let binary: DiffPage =
            serde_json::from_value(json!({"change_ref": "tool:a", "path": "a", "binary": true}))
                .unwrap();
        let text: String = hunk_lines(&binary, &palette)[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(text.contains("Binary"), "{text}");
    }

    fn view() -> DiffView {
        let mut view = DiffView::new("Diff · session · 2 files");
        view.set_files(vec![
            DiffFile {
                path: "a.rs".into(),
                kind: ChangeKind::Modified,
                lines: Some((3, 1)),
                failed: false,
                change_refs: Vec::new(),
            },
            DiffFile {
                path: "b.rs".into(),
                kind: ChangeKind::Added,
                lines: Some((9, 0)),
                failed: false,
                change_refs: Vec::new(),
            },
        ]);
        view
    }

    #[test]
    fn the_diff_view_shows_a_file_column_and_the_hunks() {
        let palette = Palette::dark();
        let mut view = view();
        view.hunks = hunk_lines(&page(), &palette);
        let text = capture(
            |frame| render_diff(frame, frame.area(), &palette, &view),
            100,
            20,
        );
        assert!(text[0].contains("a.rs"), "{}", text[0]);
        assert!(
            text.iter().any(|row| row.starts_with("M +3 −1 a.rs")),
            "{text:?}"
        );
        assert!(
            text.iter().any(|row| row.starts_with("A +9 −0 b.rs")),
            "{text:?}"
        );
        assert!(
            text.iter().any(|row| row.contains("@@ -10,2 +10,3 @@")),
            "{text:?}"
        );
        assert!(
            text.iter().any(|row| row.contains("11    -  old();")),
            "{text:?}"
        );
        assert!(text.last().unwrap().contains("esc back"), "{text:?}");
    }

    #[test]
    fn an_unmeasured_file_row_shows_a_dot_instead_of_a_number() {
        let palette = Palette::dark();
        let mut view = view();
        view.files[1].lines = None;
        let text = capture(
            |frame| render_diff(frame, frame.area(), &palette, &view),
            100,
            20,
        );
        assert!(
            text.iter().any(|row| row.starts_with("A ·     b.rs")),
            "{text:?}"
        );
    }

    #[test]
    fn a_narrow_terminal_drops_the_file_column_instead_of_wrapping_it() {
        let palette = Palette::dark();
        let mut view = view();
        view.hunks = hunk_lines(&page(), &palette);
        let text = capture(
            |frame| render_diff(frame, frame.area(), &palette, &view),
            60,
            16,
        );
        assert!(text[0].contains("Diff · session · 2 files"), "{}", text[0]);
        assert!(
            !text.iter().any(|row| row.contains("+3 −1 a.rs")),
            "{text:?}"
        );
        assert!(text.iter().any(|row| row.contains("+  new();")), "{text:?}");
    }

    #[test]
    fn selecting_a_file_keeps_the_selection_inside_the_list() {
        let mut view = view();
        view.select(1);
        assert_eq!(view.current().map(|file| file.path.as_str()), Some("b.rs"));
        view.select(9);
        assert_eq!(view.selected, 1);
        view.set_files(vec![]);
        assert_eq!(view.current(), None);
        assert_eq!(view.selected, 0);
    }

    #[test]
    fn hunk_scrolling_stops_at_the_last_full_page() {
        let mut view = view();
        view.hunks = (0..10).map(|index| Line::from(index.to_string())).collect();
        view.scroll_by(3, 4);
        assert_eq!(view.scroll, 3);
        view.scroll_by(99, 4);
        assert_eq!(view.scroll, 6, "the last page must stay full");
        view.scroll_by(-99, 4);
        assert_eq!(view.scroll, 0);
    }

    #[test]
    fn copied_diff_text_is_the_path_plus_the_hunks() {
        let palette = Palette::dark();
        let mut view = view();
        view.hunks = hunk_lines(&page(), &palette);
        let text = diff_text(&view);
        assert!(text.starts_with("a.rs\n"), "{text:?}");
        assert!(text.contains("+  new();"), "{text:?}");
    }

    #[test]
    fn the_context_view_prefers_reported_numbers_and_labels_estimates() {
        let palette = Palette::dark();
        let presentation: SessionPresentation = serde_json::from_value(json!({
            "session_id": "ses_1",
            "context": {"tokens": 31200, "window": 40000, "percent": 78.0, "kind": "reported"}
        }))
        .unwrap();
        let context: SessionContext = serde_json::from_value(json!({
            "session_id": "ses_1",
            "coverage": {"covered_loop_count": 6, "covered_item_count": 12, "retained_item_count": 3},
            "budget": {"estimated_history_tokens": 370, "input_budget_tokens": 40000,
                       "trigger_tokens": 32000, "target_tokens": 16000},
            "last_result": {"operation_id": "auto-1", "status": "compacted",
                            "before_tokens": 4200, "after_tokens": 900, "covered_loop_count": 6}
        }))
        .unwrap();
        let view = context_view("Context", Some(&context), Some(&presentation), &palette);
        let text: Vec<String> = view
            .lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect();
        assert!(text[0].contains("31.2k of 40k tokens (78%)"), "{text:?}");
        assert!(text[0].contains("reported"), "{text:?}");
        assert!(
            text.iter()
                .any(|line| line.contains("Auto-compacts at 32k tokens (80%)")),
            "{text:?}"
        );
        assert!(
            text.iter()
                .any(|line| line.contains("Last compaction: 4.2k → 900 tokens")),
            "{text:?}"
        );
        assert!(
            text.iter()
                .any(|line| line.contains("Summary covers 6 turns")),
            "{text:?}"
        );
    }

    #[test]
    fn the_context_view_falls_back_to_estimates_and_says_so() {
        let palette = Palette::dark();
        let context: SessionContext = serde_json::from_value(json!({
            "session_id": "ses_1",
            "budget": {"estimated_history_tokens": 1000, "input_budget_tokens": 4000}
        }))
        .unwrap();
        let view = context_view("Context", Some(&context), None, &palette);
        let text: Vec<String> = view
            .lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect();
        assert!(
            text.iter()
                .any(|line| line.contains("History estimate: 1k of 4k")),
            "{text:?}"
        );
        assert!(
            text.iter()
                .any(|line| line.contains("Estimates are byte-based")),
            "{text:?}"
        );
    }

    #[test]
    fn a_compaction_result_names_whether_it_was_automatic() {
        let palette = Palette::dark();
        let context: SessionContext = serde_json::from_value(json!({
            "session_id": "ses_1",
            "last_result": {"operation_id": "auto-1", "origin": "automatic", "status": "compacted",
                            "before_tokens": 32100, "after_tokens": 2700}
        }))
        .unwrap();
        let view = context_view("Context", Some(&context), None, &palette);
        let text: String = view
            .lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains("Last compaction (automatic): 32.1k → 2.7k tokens"),
            "{text}"
        );
    }

    #[test]
    fn the_context_view_never_prints_zero_for_unknown_numbers() {
        let palette = Palette::dark();
        let context: SessionContext = serde_json::from_value(json!({
            "session_id": "ses_1",
            "budget": {"input_budget_tokens": 40000},
            "automatic": {}
        }))
        .unwrap();
        let view = context_view("Context", Some(&context), None, &palette);
        let text: String = view
            .lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!text.contains("0 tokens"), "{text}");
        assert!(text.contains("not reported yet"), "{text}");
    }

    #[test]
    fn a_current_operation_is_shown_as_compacting_now() {
        let palette = Palette::dark();
        let context: SessionContext = serde_json::from_value(json!({
            "session_id": "ses_1",
            "current_operation": {"operation_id": "auto-1", "phase": "summarizing",
                                  "covered_item_count": 12, "retained_item_count": 0}
        }))
        .unwrap();
        let view = context_view("Context", Some(&context), None, &palette);
        let text: String = view
            .lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Compacting now: summarizing"), "{text}");
    }

    #[test]
    fn the_tool_view_copies_input_and_output_together() {
        let view = ToolView {
            title: "bash".into(),
            detail: "python3 -m unittest".into(),
            state: "succeeded".into(),
            input: vec![Line::from("$ python3 -m unittest")],
            output: vec![Line::from("OK"), Line::from("Ran 30 tests")],
            ..ToolView::default()
        };
        assert_eq!(view.text(), "$ python3 -m unittest\nOK\nRan 30 tests\n");
    }

    #[test]
    fn the_tool_view_scrolls_within_the_output() {
        let mut view = ToolView {
            title: "bash".into(),
            state: "succeeded".into(),
            output: (0..10).map(|index| Line::from(index.to_string())).collect(),
            ..ToolView::default()
        };
        view.scroll_by(4, 5);
        assert_eq!(view.scroll, 4);
        assert_eq!(view.visible_output(5).len(), 5);
        view.scroll_by(99, 5);
        assert_eq!(view.scroll, 5);
        // Home and End scroll by the largest step there is.
        view.scroll_by(-isize::MAX, 5);
        assert_eq!(view.scroll, 0);
        view.scroll_by(isize::MAX, 5);
        assert_eq!(view.scroll, 5);
    }

    fn stream(text: &str) -> crate::app::tasks::StreamText {
        crate::app::tasks::StreamText {
            text: text.into(),
            ..Default::default()
        }
    }

    /// Bash streams have no total order, so they are separate sections, and
    /// the card's tail stays until both have answered.
    #[test]
    fn bash_output_shows_stdout_and_stderr_sections_once_both_arrive() {
        let palette = Palette::dark();
        let mut view = ToolView {
            output: vec![Line::from("tail")],
            streams: vec![("stdout".into(), None), ("stderr".into(), None)],
            status: "loading the full output…".into(),
            ..ToolView::default()
        };
        view.set_stream("stdout", stream("ok 1\nok 2\n"), &palette);
        assert_eq!(
            view.text(),
            "tail\n",
            "half a result does not replace the tail"
        );
        view.set_stream("stderr", stream("warn\u{1b}[31m!\n"), &palette);
        assert_eq!(
            view.text(),
            "── stdout ──\nok 1\nok 2\n\n── stderr ──\nwarn!\n"
        );
        assert!(view.status.is_empty());
    }

    #[test]
    fn an_empty_stream_is_omitted_and_retention_is_explained() {
        let palette = Palette::dark();
        let mut view = ToolView {
            streams: vec![("stdout".into(), None), ("stderr".into(), None)],
            ..ToolView::default()
        };
        let mut stdout = stream("last line\n");
        stdout.dropped_head = true;
        view.set_stream("stdout", stdout, &palette);
        view.set_stream("stderr", stream(""), &palette);
        let text = view.text();
        assert!(!text.contains("stderr"), "{text}");
        assert!(text.contains("no longer retained\nlast line"), "{text}");

        let mut view = ToolView {
            streams: vec![("output".into(), None)],
            ..ToolView::default()
        };
        view.set_stream("output", stream("result\n"), &palette);
        assert_eq!(
            view.text(),
            "result\n",
            "one stream needs no section header"
        );
    }

    #[test]
    fn help_lists_every_key_row_and_command() {
        let palette = Palette::dark();
        let view = help_view(crate::app::commands::COMMANDS, &palette);
        let text: String = view
            .lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("ctrl+f"), "{text}");
        assert!(text.contains("/compact"), "{text}");
        assert!(text.contains("Compact the context now"), "{text}");
    }

    #[test]
    fn help_scrolling_shows_a_later_window() {
        let palette = Palette::dark();
        let view = help_view(&[], &palette);
        let text = capture(
            |frame| render_help(frame, frame.area(), &palette, &view, 5),
            80,
            10,
        );
        assert!(text[0].starts_with("← Help"), "{:?}", text[0]);
        assert!(!text.join("\n").contains("send, or steer"), "{text:?}");
    }

    #[test]
    fn long_diff_lines_wrap_instead_of_overflowing() {
        let palette = Palette::dark();
        let mut view = view();
        view.hunks = vec![Line::from(Span::styled(
            format!("+{}", "x".repeat(120)),
            Style::default().fg(palette.added),
        ))];
        let text = capture(
            |frame| render_diff(frame, frame.area(), &palette, &view),
            60,
            12,
        );
        let x_rows = text.iter().filter(|row| row.contains('x')).count();
        assert!(x_rows >= 2, "{text:?}");
        for row in &text {
            assert!(width(row) <= 60);
        }
    }

    #[test]
    fn a_failed_context_line_is_red() {
        let palette = Palette::dark();
        let context: SessionContext = serde_json::from_value(json!({
            "session_id": "ses_1",
            "last_prepare_failure": "context_uncompressible"
        }))
        .unwrap();
        let view = context_view("Context", Some(&context), None, &palette);
        let failure = view
            .lines
            .iter()
            .find(|line| {
                line.spans
                    .iter()
                    .any(|span| span.content.contains("context_uncompressible"))
            })
            .expect("failure line");
        assert_eq!(failure.spans[0].style.fg, Some(palette.failure));
    }

    #[test]
    fn an_automatic_last_observation_without_numbers_does_not_panic() {
        let palette = Palette::dark();
        let context: SessionContext = serde_json::from_value(json!({
            "session_id": "ses_1",
            "last_result": {"operation_id": "auto-1", "status": "failed", "failure_kind": "no_progress"}
        }))
        .unwrap();
        let view = context_view("Context", Some(&context), None, &palette);
        let text: String = view
            .lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Last compaction: failed"), "{text}");
    }
}
