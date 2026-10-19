//! Full-screen views: diff, tool output, context and help. Each one replaces
//! the transcript area and keeps the footer. Rendering is pure: the view is
//! built from data the app already holds (or from a fetched page) and the app
//! owns scrolling and selection.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::protocol::changes::{DiffPage, LineKind};
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

/// The change records of the row at `path`, for a caller that does not keep
/// them (a preview row built from tool cards has none).
impl DiffFile {
    /// The row's line column: the measured total, or a marker while unknown.
    fn count(&self) -> String {
        match self.lines {
            Some((added, removed)) => format!("{}", added + removed),
            None if self.failed => "?".into(),
            None => "·".into(),
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
        let next = (self.scroll as isize + delta).clamp(0, max as isize);
        self.scroll = next as usize;
    }
}

/// The plain-text body `/copy` and the `c` key use.
pub fn diff_text(view: &DiffView) -> String {
    let mut out = String::new();
    if let Some(file) = view.current() {
        out.push_str(&file.path);
        out.push('\n');
    }
    for line in &view.hunks {
        out.push_str(
            &line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>(),
        );
        out.push('\n');
    }
    out
}

/// Turns one `changes.diff` page into Styled lines. Hunk headers keep their
/// real `@@` form, added and removed lines take the diff colors.
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
    let mut lines = Vec::new();
    for hunk in &page.hunks {
        lines.push(Line::from(Span::styled(
            format!(
                "@@ -{},{} +{},{} @@",
                hunk.old_start, hunk.old_count, hunk.new_start, hunk.new_count
            ),
            Style::default().fg(palette.hunk),
        )));
        for line in &hunk.lines {
            let (sign, style) = match line.kind {
                LineKind::Added => ("+", Style::default().fg(palette.added)),
                LineKind::Removed => ("-", Style::default().fg(palette.removed)),
                LineKind::Context | LineKind::Unknown => (" ", Style::default().fg(palette.dim)),
            };
            // Diff text carries the original line terminator; the row already
            // ends, so only the trailing newline is dropped.
            let text = clean(line.text.trim_end_matches(['\n', '\r']));
            lines.push(Line::from(Span::styled(format!("{sign}{text}"), style)));
        }
    }
    if !page.complete {
        lines.push(dim("… the diff continues", palette));
    }
    lines
}

/// Terminal-safe text, borrowing from either `sanitize` signature.
fn clean(text: &str) -> String {
    sanitize(text).to_string()
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
        let label = format!(
            "{} {:>4} {}",
            file.marker(),
            file.count(),
            truncate(&file.path, area.width as usize - 8)
        );
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
        frame.buffer_mut().set_line(
            area.x,
            y,
            &Line::from(Span::styled(label, style)),
            area.width,
        );
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
    for (row, line) in view.visible_hunks(area.height as usize).iter().enumerate() {
        let text = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        let style = line
            .spans
            .first()
            .map(|span| span.style)
            .unwrap_or_default();
        let y = area.y + row as u16;
        for (index, chunk) in wrap(&text, area.width as usize).into_iter().enumerate() {
            if area.y + row as u16 + index as u16 >= area.bottom() {
                return;
            }
            frame.buffer_mut().set_line(
                area.x,
                y + index as u16,
                &Line::from(Span::styled(chunk, style)),
                area.width,
            );
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
#[derive(Clone, Debug)]
pub struct ToolView {
    pub title: String,
    pub detail: String,
    pub state: String,
    pub input: Vec<Line<'static>>,
    pub output: Vec<Line<'static>>,
    pub scroll: usize,
    pub status: String,
}

impl ToolView {
    pub fn visible_output(&self, rows: usize) -> &[Line<'static>] {
        let start = self.scroll.min(self.output.len());
        let end = (start + rows).min(self.output.len());
        &self.output[start..end]
    }

    pub fn scroll_by(&mut self, delta: isize, rows: usize) {
        let max = self.output.len().saturating_sub(rows);
        self.scroll = (self.scroll as isize + delta).clamp(0, max as isize) as usize;
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
    let title = format!("← {} · {}", view.title, view.state);
    chrome(
        frame,
        area,
        palette,
        &title,
        "PgUp/PgDn scroll · c copy · esc back",
    );
    let mut y = area.y + 1;
    let bottom = area.bottom().saturating_sub(1);
    if !view.detail.is_empty() {
        frame.buffer_mut().set_line(
            area.x,
            y,
            &Span::styled(clean(&view.detail), Style::default().fg(palette.dim)).into(),
            area.width,
        );
        y += 1;
    }
    for line in &view.input {
        if y >= bottom {
            return;
        }
        draw_clipped(frame, area, y, palette, line);
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
        draw_clipped(frame, area, y + row as u16, palette, line);
    }
}

fn draw_clipped(frame: &mut Frame, area: Rect, y: u16, _palette: &Palette, line: &Line<'static>) {
    frame.buffer_mut().set_line(area.x, y, line, area.width);
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
                thousands(tokens),
                thousands(window)
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
            thousands(used),
            thousands(limit)
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
                    thousands(trigger),
                    match budget.target_tokens {
                        Some(target) => format!(", keeping about {}", thousands(target)),
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
                        thousands(before),
                        thousands(after)
                    ),
                    other => format!(
                        "Last compaction ({other}): {} → {} tokens",
                        thousands(before),
                        thousands(after)
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

/// `32100` → `32.1k`, so a number fits a footer or a one-line marker.
pub fn thousands(value: u64) -> String {
    if value < 1_000 {
        return value.to_string();
    }
    let tenths = value as f64 / 1_000.0;
    if tenths < 100.0 {
        format!("{tenths:.1}k")
    } else {
        format!("{:.0}k", tenths)
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

pub fn help_view(commands: &[(&'static str, &'static str)], palette: &Palette) -> HelpView {
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
    for (name, description) in commands {
        lines.push(Line::from(vec![
            Span::styled(format!("/{name:<25}"), Style::default().fg(palette.accent)),
            Span::styled(*description, Style::default().fg(palette.text)),
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

/// Width of the longest label, for column alignment in a view.
pub fn label_width(labels: impl Iterator<Item = usize>) -> usize {
    labels.max().unwrap_or(0)
}

/// Trims a path to the last `cells` columns with a leading `…`.
pub fn shorten_path(path: &str, cells: usize) -> String {
    if width(path) <= cells {
        return path.to_owned();
    }
    let mut tail = Vec::new();
    let mut used = 1;
    for ch in path.chars().rev() {
        let cells_of = width(&ch.to_string());
        if used + cells_of > cells {
            break;
        }
        tail.push(ch);
        used += cells_of;
    }
    tail.reverse();
    std::iter::once('…').chain(tail).collect()
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
            "hunks": [{"old_start": 10, "old_count": 2, "new_start": 10, "new_count": 3,
                       "lines": [
                         {"kind": "context", "text": " fn main() {", "line_complete": true},
                         {"kind": "removed", "text": "  old();", "line_complete": true},
                         {"kind": "added", "text": "  new();", "line_complete": true},
                       ]}]
        }))
        .unwrap()
    }

    #[test]
    fn hunk_lines_keep_the_diff_shape_and_colors() {
        let palette = Palette::dark();
        let lines = hunk_lines(&page(), &palette);
        let text: Vec<String> = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect();
        assert_eq!(
            text,
            [
                "@@ -10,2 +10,3 @@",
                "  fn main() {",
                "-  old();",
                "+  new();"
            ]
        );
        assert_eq!(lines[0].spans[0].style.fg, Some(palette.hunk));
        assert_eq!(lines[2].spans[0].style.fg, Some(palette.removed));
        assert_eq!(lines[3].spans[0].style.fg, Some(palette.added));
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
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
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
            text.iter().any(|row| row.contains("M    4 a.rs")),
            "{text:?}"
        );
        assert!(
            text.iter().any(|row| row.contains("@@ -10,2 +10,3 @@")),
            "{text:?}"
        );
        assert!(text.iter().any(|row| row.contains("-  old();")), "{text:?}");
        assert!(text.last().unwrap().contains("esc back"), "{text:?}");
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
            !text.iter().any(|row| row.contains("M    4 a.rs")),
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
        assert!(text[0].contains("31.2k of 40.0k tokens (78%)"), "{text:?}");
        assert!(text[0].contains("reported"), "{text:?}");
        assert!(
            text.iter()
                .any(|line| line.contains("Auto-compacts at 32.0k tokens (80%)")),
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
                .any(|line| line.contains("History estimate: 1.0k of 4.0k")),
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
    fn thousands_keeps_small_numbers_exact_and_large_ones_short() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1.0k");
        assert_eq!(thousands(32_200), "32.2k");
        assert_eq!(thousands(123_456), "123k");
    }

    #[test]
    fn paths_shorten_from_the_left_so_the_file_name_survives() {
        assert_eq!(shorten_path("src/ui/panel.rs", 30), "src/ui/panel.rs");
        assert_eq!(
            shorten_path("a/very/long/path/to/panel.rs", 12),
            "…to/panel.rs"
        );
        assert_eq!(shorten_path("src/ui/panel.rs", 8), "…anel.rs");
        assert_eq!(shorten_path("中文/路径.rs", 6), "…径.rs");
    }

    #[test]
    fn the_tool_view_copies_input_and_output_together() {
        let view = ToolView {
            title: "bash".into(),
            detail: "python3 -m unittest".into(),
            state: "succeeded".into(),
            input: vec![Line::from("$ python3 -m unittest")],
            output: vec![Line::from("OK"), Line::from("Ran 30 tests")],
            scroll: 0,
            status: String::new(),
        };
        assert_eq!(view.text(), "$ python3 -m unittest\nOK\nRan 30 tests\n");
    }

    #[test]
    fn the_tool_view_scrolls_within_the_output() {
        let mut view = ToolView {
            title: "bash".into(),
            detail: String::new(),
            state: "succeeded".into(),
            input: Vec::new(),
            output: (0..10).map(|index| Line::from(index.to_string())).collect(),
            scroll: 0,
            status: String::new(),
        };
        view.scroll_by(4, 5);
        assert_eq!(view.scroll, 4);
        assert_eq!(view.visible_output(5).len(), 5);
        view.scroll_by(99, 5);
        assert_eq!(view.scroll, 5);
    }

    #[test]
    fn help_lists_every_key_row_and_command() {
        let palette = Palette::dark();
        let view = help_view(&[("compact", "Compact the context now")], &palette);
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
