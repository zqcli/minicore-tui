//! Full-screen views: diff, tool output, context and help. Each one replaces
//! the transcript area and keeps the footer. Rendering is pure: the view is
//! built from data the app already holds (or from a fetched page) and the app
//! owns scrolling and selection.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::app::commands;
use crate::app::diff::{DiffFile, DiffState};
use crate::app::tasks::StreamText;
use crate::app::tool::ToolState;
use crate::app::{App, Overlay, Session};
use crate::protocol::changes::{DiffLine, DiffPage, LineKind};
use crate::protocol::{ChangeKind, SessionContext, SessionPresentation};
use crate::transcript::ToolCall;

use super::panel::truncate;
use super::theme::Styles;
use super::wrap::{sanitize, width, wrap};

/// File-list column width, as a fraction of the view when the terminal is wide
/// enough for a side-by-side layout.
const FILE_COLUMN: u16 = 30;
const MIN_SPLIT_WIDTH: u16 = 80;
const MIN_SPLIT_HEIGHT: u16 = 12;

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
    fn line(&self, styles: Styles) -> Line<'static> {
        match self {
            Self::Blank => Line::from(""),
            Self::Note(note) => styled(note, styles.dim()),
            Self::Hunk(header) => styled(header, styles.hunk()),
            Self::Line { kind, gutter, text } => {
                let style = match kind {
                    LineKind::Added => styles.added(),
                    LineKind::Removed => styles.removed(),
                    LineKind::Context | LineKind::Unknown => styles.dim(),
                };
                Line::from(vec![
                    Span::styled(gutter.clone(), styles.faint()),
                    Span::styled(format!("{}{text}", sign(*kind)), style),
                ])
            }
        }
    }

    /// The row as `c` copies it: without the line-number gutter.
    fn body(&self) -> String {
        match self {
            Self::Blank => String::new(),
            Self::Note(text) | Self::Hunk(text) => text.clone(),
            Self::Line { kind, text, .. } => format!("{}{text}", sign(*kind)),
        }
    }
}

fn sign(kind: LineKind) -> &'static str {
    match kind {
        LineKind::Added => "+",
        LineKind::Removed => "-",
        LineKind::Context | LineKind::Unknown => " ",
    }
}

/// The text of the diff view: the file's path, then its rows.
pub fn diff_text(diff: &DiffState) -> String {
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

/// Rows for every change record of one file; each record is its own
/// comparison, so several are told apart by a separator naming the tool.
pub fn diff_rows(pages: &[DiffPage], tools: &[Option<String>]) -> Vec<DiffRow> {
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

/// Turns one `changes.diff` page into rows. Header numbers are unified-diff
/// 1-based; the Agent's starts are 0-based line indexes.
fn hunk_rows(page: &DiffPage) -> Vec<DiffRow> {
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

/// One row in one style. The style sits on the span, which is what `set_line` paints.
fn styled(text: &str, style: Style) -> Line<'static> {
    Line::from(Span::styled(text.to_owned(), style))
}

/// Shared view chrome: the title row and the dim hint row.
fn chrome(frame: &mut Frame, area: Rect, styles: Styles, title: &str, hint: &str) {
    let buffer = frame.buffer_mut();
    buffer.set_line(area.x, area.y, &styled(title, styles.title()), area.width);
    if area.height > 1 {
        buffer.set_line(
            area.x,
            area.bottom() - 1,
            &styled(hint, styles.dim()),
            area.width,
        );
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

pub fn render_diff(frame: &mut Frame, area: Rect, styles: Styles, diff: &DiffState) {
    let mut title = format!("← Diff · {}", diff.scope.label());
    if let Some(file) = diff.current() {
        title.push_str(" · ");
        title.push_str(&file.path);
    }
    chrome(
        frame,
        area,
        styles,
        &title,
        "tab scope · ↑↓ file · PgUp/PgDn scroll · c copy · esc back",
    );
    let (list, hunks) = split(area);
    if let Some(list) = list {
        render_file_list(frame, list, styles, diff);
    }
    render_hunks(frame, hunks, styles, diff);
}

/// The file's line column: `+a −b` once measured, `?` when the read failed and
/// `·` while unknown.
fn count_spans(file: &DiffFile, styles: Styles) -> Vec<Span<'static>> {
    match file.lines {
        Some((added, removed)) => vec![
            Span::styled(format!("+{added}"), styles.added()),
            Span::raw(" "),
            Span::styled(format!("−{removed}"), styles.removed()),
        ],
        None if file.failed => vec![Span::styled("?", styles.dim())],
        None => vec![Span::styled("·", styles.dim())],
    }
}

fn marker(kind: ChangeKind) -> &'static str {
    match kind {
        ChangeKind::Added => "A",
        ChangeKind::Deleted => "D",
        ChangeKind::Renamed => "R",
        ChangeKind::Conflict => "!",
        ChangeKind::Modified | ChangeKind::Unknown => "M",
    }
}

fn render_file_list(frame: &mut Frame, area: Rect, styles: Styles, diff: &DiffState) {
    let rows = area.height as usize;
    let first = diff.selected.saturating_sub(rows.saturating_sub(1));
    let counts: Vec<Vec<Span<'static>>> = diff
        .files
        .iter()
        .map(|file| count_spans(file, styles))
        .collect();
    let span_width = |spans: &[Span<'static>]| spans.iter().map(|span| width(&span.content)).sum();
    let column = counts
        .iter()
        .map(|spans| span_width(spans))
        .max()
        .unwrap_or(1);
    for (row, (index, file)) in diff
        .files
        .iter()
        .enumerate()
        .skip(first)
        .take(rows)
        .enumerate()
    {
        let y = area.y + row as u16;
        let style = if index == diff.selected {
            let band = Rect {
                y,
                height: 1,
                ..area
            };
            frame.buffer_mut().set_style(band, styles.highlight());
            styles.bold()
        } else {
            styles.dim()
        };
        let count = &counts[index];
        let mut spans = vec![Span::styled(format!("{} ", marker(file.kind)), style)];
        spans.extend(count.iter().cloned());
        let path_width = (area.width as usize).saturating_sub(column + 3);
        spans.push(Span::raw(" ".repeat(column - span_width(count) + 1)));
        spans.push(Span::styled(truncate(&file.path, path_width), style));
        frame
            .buffer_mut()
            .set_line(area.x, y, &Line::from(spans), area.width);
    }
    if let Some(file) = diff.current()
        && area.height > 1
    {
        let counts = match file.lines {
            Some((added, removed)) => Line::from(vec![
                Span::styled(format!("+{added} "), styles.added()),
                Span::styled(format!("−{removed}"), styles.removed()),
            ]),
            None if file.failed => styled("line counts unavailable", styles.dim()),
            None => styled("reading…", styles.dim()),
        };
        frame
            .buffer_mut()
            .set_line(area.x, area.bottom() - 1, &counts, area.width);
    }
}

fn render_hunks(frame: &mut Frame, area: Rect, styles: Styles, diff: &DiffState) {
    if diff.rows.is_empty() {
        let message = if diff.status.is_empty() {
            "No changes to show."
        } else {
            diff.status.as_str()
        };
        frame
            .buffer_mut()
            .set_line(area.x, area.y, &styled(message, styles.dim()), area.width);
        return;
    }
    // The last span is the content and wraps; earlier spans are the gutter,
    // drawn on the first row and left blank on continuation rows.
    let mut y = area.y;
    for row in diff
        .rows
        .iter()
        .skip(diff.scroll)
        .take(area.height as usize)
    {
        let line = row.line(styles);
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

/// One row of the tool view's output, before it is styled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolRow {
    Text(String),
    /// The heading of a stream: `── stderr ──`.
    Section(String),
    Note(&'static str),
}

impl ToolRow {
    fn text(&self) -> String {
        match self {
            Self::Text(text) => text.clone(),
            Self::Section(name) => format!("── {name} ──"),
            Self::Note(note) => (*note).to_owned(),
        }
    }

    fn line(&self, styles: Styles) -> Line<'static> {
        match self {
            Self::Text(text) => Line::raw(text.clone()),
            Self::Section(name) => {
                let style = if name == "stderr" {
                    styles.warning()
                } else {
                    styles.dim()
                };
                styled(&self.text(), style)
            }
            Self::Note(note) => styled(note, styles.dim()),
        }
    }
}

/// The lines of plain text as rows, made safe for the terminal.
pub fn text_rows(text: &str) -> Vec<ToolRow> {
    text.lines()
        .map(|line| ToolRow::Text(sanitize(line).into_owned()))
        .collect()
}

/// The output of the answered streams; a stream with nothing to show is
/// skipped, and several streams each get a heading.
pub fn stream_rows(streams: &[(String, Option<StreamText>)]) -> Vec<ToolRow> {
    let sections = streams.len() > 1;
    let mut rows = Vec::new();
    for (name, text) in streams {
        let Some(text) = text else { continue };
        let body = text_rows(&text.text);
        let noted = text.dropped_head || text.expired || text.cut;
        if body.is_empty() && !noted {
            continue;
        }
        if sections {
            if !rows.is_empty() {
                rows.push(ToolRow::Text(String::new()));
            }
            rows.push(ToolRow::Section(name.clone()));
        }
        if text.dropped_head {
            rows.push(ToolRow::Note("… earlier output is no longer retained"));
        }
        if text.expired && body.is_empty() {
            rows.push(ToolRow::Note("… this output has expired from the agent"));
        }
        rows.extend(body);
        if text.cut {
            rows.push(ToolRow::Note("… cut at 1 MiB"));
        }
    }
    rows
}

/// The call's input as the view shows it: a one-line input that is the detail
/// already (a bash command) is not repeated.
fn tool_input(call: &ToolCall) -> &str {
    call.expanded_input
        .as_deref()
        .filter(|input| input.trim() != call.detail.trim())
        .unwrap_or_default()
}

/// The text of the tool view, input then output, for `c`.
pub fn tool_text(tool: &ToolState, session: Option<&Session>) -> String {
    let input = session
        .and_then(|session| session.transcript.find_tool(&tool.key))
        .map_or("", tool_input);
    let mut out = String::new();
    for line in input
        .lines()
        .map(str::to_owned)
        .chain(tool.rows.iter().map(ToolRow::text))
    {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

pub fn render_tool(
    frame: &mut Frame,
    area: Rect,
    styles: Styles,
    tool: &ToolState,
    session: Option<&Session>,
) {
    let Some((session, call)) =
        session.and_then(|session| Some((session, session.transcript.find_tool(&tool.key)?)))
    else {
        return;
    };
    let mut title = format!("← {} · {}", call.name, tool_state_label(call.state));
    let calls = || session.transcript.tool_calls();
    if let Some(index) = calls().position(|other| other.key == tool.key) {
        title.push_str(&format!(" · tool {} of {}", index + 1, calls().count()));
    }
    if !tool.status.is_empty() && !tool.rows.is_empty() {
        title.push_str(&format!(" · {}", tool.status));
    }
    chrome(
        frame,
        area,
        styles,
        &title,
        "[ ] previous/next tool · ↑↓ PgUp/PgDn scroll · c copy · esc back",
    );
    let mut y = area.y + 1;
    let bottom = area.bottom().saturating_sub(1);
    if !call.detail.is_empty() {
        let detail = styled(&sanitize(&call.detail), styles.dim());
        frame.buffer_mut().set_line(area.x, y, &detail, area.width);
        y += 1;
    }
    for text in tool_input(call).lines() {
        if y >= bottom {
            return;
        }
        let line = styled(&sanitize(text), styles.dim());
        frame.buffer_mut().set_line(area.x, y, &line, area.width);
        y += 1;
    }
    if y < bottom {
        y += 1;
    }
    let rows = bottom.saturating_sub(y) as usize;
    if rows == 0 {
        return;
    }
    if tool.rows.is_empty() {
        let message = if tool.status.is_empty() {
            "No output."
        } else {
            tool.status.as_str()
        };
        frame
            .buffer_mut()
            .set_line(area.x, y, &styled(message, styles.dim()), area.width);
        return;
    }
    for (offset, row) in tool.rows.iter().skip(tool.scroll).take(rows).enumerate() {
        frame
            .buffer_mut()
            .set_line(area.x, y + offset as u16, &row.line(styles), area.width);
    }
}

/// The context view's lines: one readable summary, not a ledger of numbers.
pub fn context_lines(session: Option<&Session>, styles: Styles) -> Vec<Line<'static>> {
    summary_lines(
        session.and_then(|session| session.context.as_ref()),
        session.and_then(|session| session.presentation.as_ref()),
        styles,
    )
}

/// The context view as plain text, for `c`.
pub fn context_text(session: Option<&Session>, styles: Styles) -> String {
    let lines = context_lines(session, styles);
    lines
        .iter()
        .map(Line::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

/// How full the context is, in percent: as the Agent reported it after the
/// last request, else estimated against the input budget.
pub fn context_percent(session: &Session) -> Option<f64> {
    let reported = session
        .presentation
        .as_ref()
        .and_then(|presentation| presentation.context.as_ref())
        .filter(|usage| usage.tokens.is_some());
    if let Some(usage) = reported {
        return usage
            .percent
            .or_else(|| match (usage.tokens, usage.window) {
                (Some(tokens), Some(window)) if window > 0 => {
                    Some(tokens as f64 / window as f64 * 100.0)
                }
                _ => None,
            });
    }
    let budget = session.context.as_ref()?.budget.as_ref()?;
    let tokens = budget
        .estimated_request_context_tokens
        .or(budget.estimated_history_tokens)?;
    let limit = budget.input_budget_tokens.filter(|limit| *limit > 0)?;
    Some(tokens as f64 / limit as f64 * 100.0)
}

/// Builds the summary from the two authoritative views, labelling estimates
/// as estimates and keeping unknowns as `?` instead of zero.
fn summary_lines(
    context: Option<&SessionContext>,
    presentation: Option<&SessionPresentation>,
    styles: Styles,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let usage = presentation.and_then(|view| view.context.as_ref());

    let reported = usage.and_then(|usage| match (usage.tokens, usage.window) {
        (Some(tokens), Some(window)) if window > 0 => {
            let percent = usage
                .percent
                .unwrap_or(tokens as f64 / window as f64 * 100.0);
            Some(format!(
                "Context: {} of {} tokens ({percent:.0}%) — reported by the last request",
                super::format_tokens(tokens),
                super::format_tokens(window)
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
            super::format_tokens(used),
            super::format_tokens(limit)
        ))
    });
    match reported.or(estimated) {
        Some(line) => lines.push(styled(&line, styles.text())),
        None => lines.push(styled("Context usage is not reported yet.", styles.dim())),
    }

    if let Some(budget) = budget {
        if let (Some(trigger), Some(limit)) = (budget.trigger_tokens, budget.input_budget_tokens) {
            let percent = if limit > 0 {
                format!(" ({:.0}%)", trigger as f64 / limit as f64 * 100.0)
            } else {
                String::new()
            };
            lines.push(styled(
                &format!(
                    "Auto-compacts at {} tokens{percent}{}",
                    super::format_tokens(trigger),
                    match budget.target_tokens {
                        Some(target) => format!(", keeping about {}", super::format_tokens(target)),
                        None => String::new(),
                    }
                ),
                styles.dim(),
            ));
        } else {
            lines.push(styled("Automatic compaction is disabled.", styles.dim()));
        }
    }

    if let Some(context) = context {
        if let Some(operation) = &context.current_operation {
            lines.push(styled(
                &format!(
                    "Compacting now: {} · {}",
                    operation_label(&operation.phase),
                    phase_counts(operation)
                ),
                styles.warning(),
            ));
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
                        super::format_tokens(before),
                        super::format_tokens(after)
                    ),
                    other => format!(
                        "Last compaction ({other}): {} → {} tokens",
                        super::format_tokens(before),
                        super::format_tokens(after)
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
            lines.push(styled(&summary, styles.dim()));
        }
        if let Some(coverage) = &context.coverage {
            lines.push(styled(
                &format!(
                    "Summary covers {} turns · {} items · {} kept",
                    coverage.covered_loop_count,
                    coverage.covered_item_count,
                    coverage.retained_item_count
                ),
                styles.dim(),
            ));
        }
        if let Some(failure) = &context.last_prepare_failure {
            lines.push(styled(
                &format!("Last preparation failed: {failure}"),
                styles.failure(),
            ));
        }
    } else {
        lines.push(styled(
            "No context report is loaded for this session.",
            styles.dim(),
        ));
    }
    lines.push(Line::from(""));
    lines.push(styled(
        "Estimates are byte-based and exclude the system prompt and tool schemas.",
        styles.dim(),
    ));

    lines
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

pub fn render_context(frame: &mut Frame, area: Rect, styles: Styles, lines: &[Line<'static>]) {
    chrome(frame, area, styles, "← Context", "r refresh · esc back");
    let bottom = area.bottom().saturating_sub(1);
    let mut y = area.y + 1;
    for line in lines {
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

pub fn help_view(commands: &[crate::app::commands::Command], styles: Styles) -> HelpView {
    let mut lines = Vec::new();
    lines.push(styled("Keys", styles.dim()));
    for row in KEYS {
        lines.push(Line::from(vec![
            Span::styled(format!("{:<26}", row.keys), styles.accent()),
            Span::styled(row.action, styles.text()),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(styled("Commands", styles.dim()));
    for command in commands {
        lines.push(Line::from(vec![
            Span::styled(format!("/{:<25}", command.name), styles.accent()),
            Span::styled(command.description, styles.text()),
        ]));
    }
    HelpView {
        title: "Help".into(),
        lines,
    }
}

pub fn render_help(frame: &mut Frame, area: Rect, styles: Styles, view: &HelpView, scroll: usize) {
    chrome(
        frame,
        area,
        styles,
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

/// Draws whichever view owns the transcript area; false when none is open.
pub fn draw(frame: &mut Frame, area: Rect, app: &App, styles: Styles) -> bool {
    match &app.overlay {
        Overlay::Diff(diff) => render_diff(frame, area, styles, diff),
        Overlay::Tool(tool) => render_tool(frame, area, styles, tool, app.session.as_ref()),
        Overlay::Context => {
            let lines = context_lines(app.session.as_ref(), styles);
            render_context(frame, area, styles, &lines);
        }
        Overlay::Help { scroll } => {
            let help = help_view(commands::COMMANDS, styles);
            render_help(frame, area, styles, &help, *scroll);
        }
        _ => return false,
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use serde_json::json;

    use crate::app::diff::DiffScope;
    use crate::ui::theme::Palette;

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
            "binary": false, "stale": false, "truncated": false,
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

    fn hunk_lines(page: &DiffPage, styles: Styles) -> Vec<Line<'static>> {
        hunk_rows(page).iter().map(|row| row.line(styles)).collect()
    }

    fn records_lines(
        pages: &[DiffPage],
        tools: &[Option<String>],
        styles: Styles,
    ) -> Vec<Line<'static>> {
        diff_rows(pages, tools)
            .iter()
            .map(|row| row.line(styles))
            .collect()
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
        let lines = hunk_lines(&page(), Styles::of(palette));
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
        let styles = Styles::of(Palette::dark());
        let page: DiffPage = serde_json::from_value(json!({
            "binary": false, "stale": false, "truncated": false,
            "change_ref": "tool:a", "path": "a.py", "complete": true,
            "hunks": [
                {"old_start": 0, "old_count": 2, "new_start": 0, "new_count": 2, "lines": []},
                {"old_start": 5, "old_count": 0, "new_start": 5, "new_count": 1, "lines": []}
            ]
        }))
        .unwrap();
        let text: Vec<String> = hunk_lines(&page, styles).iter().map(row_text).collect();
        assert_eq!(text, ["@@ -1,2 +1,2 @@", "@@ -5,0 +6,1 @@"]);
    }

    #[test]
    fn a_line_split_into_fragments_is_one_row() {
        let styles = Styles::of(Palette::dark());
        let page: DiffPage = serde_json::from_value(json!({
            "binary": false, "stale": false, "truncated": false,
            "change_ref": "tool:a", "path": "a.py", "complete": true,
            "hunks": [{"old_start": 0, "old_count": 0, "new_start": 0, "new_count": 1,
                       "lines": [
                         {"kind": "added", "new_index": 0, "text": "long ", "line_complete": false},
                         {"kind": "added", "new_index": 0, "text": "line\n", "line_complete": true}
                       ]}]
        }))
        .unwrap();
        let text: Vec<String> = hunk_lines(&page, styles).iter().map(row_text).collect();
        assert_eq!(text[1..], ["  1 +long line"]);
    }

    #[test]
    fn several_records_of_one_file_are_separated_and_named() {
        let styles = Styles::of(Palette::dark());
        let pages = [page(), page(), page()];
        let tools = [Some("write".to_owned()), Some("edit".to_owned()), None];
        let text: Vec<String> = records_lines(&pages, &tools, styles)
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
        let single: Vec<String> = records_lines(&pages[..1], &tools[..1], styles)
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
        let styles = Styles::of(Palette::dark());
        let page: DiffPage = serde_json::from_value(json!({
            "binary": false, "stale": false, "truncated": false,
            "change_ref": "tool:abc", "path": "a.rs", "complete": true,
            "hunks": [{"old_start": 1, "old_count": 0, "new_start": 1, "new_count": 2,
                       "lines": [
                         {"kind": "added", "text": "added();\n", "line_complete": true},
                         {"kind": "removed", "text": "gone();\r\n", "line_complete": true},
                       ]}]
        }))
        .unwrap();
        let lines = hunk_lines(&page, styles);
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
        let styles = Styles::of(Palette::dark());
        let page = |binary: bool| -> DiffPage {
            serde_json::from_value(json!({
                "hunks": [], "complete": false, "binary": binary, "stale": false, "truncated": false
            }))
            .unwrap()
        };
        let (empty, binary) = (page(false), page(true));
        assert_eq!(hunk_lines(&empty, styles).len(), 1);
        let text: String = hunk_lines(&binary, styles)[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(text.contains("Binary"), "{text}");
    }

    fn view() -> DiffState {
        let mut view = DiffState::new(DiffScope::Session);
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
        let styles = Styles::of(Palette::dark());
        let mut view = view();
        view.rows = hunk_rows(&page());
        let text = capture(
            |frame| render_diff(frame, frame.area(), styles, &view),
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
        let styles = Styles::of(Palette::dark());
        let mut view = view();
        view.files[1].lines = None;
        let text = capture(
            |frame| render_diff(frame, frame.area(), styles, &view),
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
        let styles = Styles::of(Palette::dark());
        let mut view = view();
        view.rows = hunk_rows(&page());
        let text = capture(
            |frame| render_diff(frame, frame.area(), styles, &view),
            60,
            16,
        );
        assert!(text[0].contains("Diff · session · a.rs"), "{}", text[0]);
        assert!(
            !text.iter().any(|row| row.contains("+3 −1 a.rs")),
            "{text:?}"
        );
        assert!(text.iter().any(|row| row.contains("+  new();")), "{text:?}");
    }

    #[test]
    fn copied_diff_text_is_the_path_plus_the_hunks() {
        let mut view = view();
        view.rows = hunk_rows(&page());
        let text = diff_text(&view);
        assert!(text.starts_with("a.rs\n"), "{text:?}");
        assert!(text.contains("+  new();"), "{text:?}");
    }

    #[test]
    fn the_context_view_prefers_reported_numbers_and_labels_estimates() {
        let styles = Styles::of(Palette::dark());
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
        let lines = summary_lines(Some(&context), Some(&presentation), styles);
        let text: Vec<String> = lines
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
        let styles = Styles::of(Palette::dark());
        let context: SessionContext = serde_json::from_value(json!({
            "session_id": "ses_1",
            "budget": {"estimated_history_tokens": 1000, "input_budget_tokens": 4000}
        }))
        .unwrap();
        let lines = summary_lines(Some(&context), None, styles);
        let text: Vec<String> = lines
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
        let styles = Styles::of(Palette::dark());
        let context: SessionContext = serde_json::from_value(json!({
            "session_id": "ses_1",
            "last_result": {"operation_id": "auto-1", "origin": "automatic", "status": "compacted",
                            "before_tokens": 32100, "after_tokens": 2700}
        }))
        .unwrap();
        let lines = summary_lines(Some(&context), None, styles);
        let text: String = lines
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
        let styles = Styles::of(Palette::dark());
        let context: SessionContext = serde_json::from_value(json!({
            "session_id": "ses_1",
            "budget": {"input_budget_tokens": 40000},
            "automatic": {}
        }))
        .unwrap();
        let lines = summary_lines(Some(&context), None, styles);
        let text: String = lines
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
        let styles = Styles::of(Palette::dark());
        let context: SessionContext = serde_json::from_value(json!({
            "session_id": "ses_1",
            "current_operation": {"operation_id": "auto-1", "phase": "summarizing",
                                  "covered_item_count": 12, "retained_item_count": 0}
        }))
        .unwrap();
        let lines = summary_lines(Some(&context), None, styles);
        let text: String = lines
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
    fn help_lists_every_key_row_and_command() {
        let styles = Styles::of(Palette::dark());
        let view = help_view(crate::app::commands::COMMANDS, styles);
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
        let styles = Styles::of(Palette::dark());
        let view = help_view(&[], styles);
        let text = capture(
            |frame| render_help(frame, frame.area(), styles, &view, 5),
            80,
            10,
        );
        assert!(text[0].starts_with("← Help"), "{:?}", text[0]);
        assert!(!text.join("\n").contains("send, or steer"), "{text:?}");
    }

    #[test]
    fn long_diff_lines_wrap_instead_of_overflowing() {
        let styles = Styles::of(Palette::dark());
        let mut view = view();
        view.rows = vec![DiffRow::Line {
            kind: LineKind::Added,
            gutter: String::new(),
            text: "x".repeat(120),
        }];
        let text = capture(
            |frame| render_diff(frame, frame.area(), styles, &view),
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
        let styles = Styles::of(palette);
        let context: SessionContext = serde_json::from_value(json!({
            "session_id": "ses_1",
            "last_prepare_failure": "context_uncompressible"
        }))
        .unwrap();
        let lines = summary_lines(Some(&context), None, styles);
        let failure = lines
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
        let styles = Styles::of(Palette::dark());
        let context: SessionContext = serde_json::from_value(json!({
            "session_id": "ses_1",
            "last_result": {"operation_id": "auto-1", "status": "failed", "failure_kind": "no_progress"}
        }))
        .unwrap();
        let lines = summary_lines(Some(&context), None, styles);
        let text: String = lines
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
