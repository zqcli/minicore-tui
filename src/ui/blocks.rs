//! Block rendering: one function per block kind, in the rail style. The layout
//! cache calls these once per change and stores the returned lines, so a block
//! never re-renders per frame. Column 0 of the width a block gets is its rail.

use std::sync::LazyLock;

use jiff::Timestamp as WallTime;
use jiff::tz::TimeZone;
use minicore_host_protocol::{PersistenceState, Timestamp, TurnOutcome};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::markdown;
use super::rails::{chrome, chrome_span, fill, indent, rail_row, surface_row};
use super::theme::Styles;
use crate::transcript::UserKind;
use crate::transcript::layout::Fold;
use crate::transcript::{Block, ToolCall};
use crate::transcript::{EXPANDED_ROWS, ToolState};
use crate::wrap;

/// Columns before block text: the rail and one pad cell.
pub const INSET: usize = 2;

/// Thinking rows a long block shows before its expand hint.
const THINKING_ROWS: usize = 3;

/// The column text wraps at in a block `width` wide, keeping one pad cell on
/// the right.
pub fn text_width(width: usize) -> usize {
    width.saturating_sub(INSET + 1)
}

/// Thinking text: dim italic, so it reads as an aside to the reply.
pub fn thinking_style(styles: Styles) -> Style {
    styles.dim().add_modifier(Modifier::ITALIC)
}

/// Renders one block at `width`. An assistant without visible content has no rows.
pub fn render(block: &Block, styles: Styles, width: u16, fold: Fold) -> Vec<Line<'static>> {
    if matches!(block, Block::Assistant { reasoning, text, partial: false, .. }
        if reasoning.trim().is_empty() && text.trim().is_empty())
    {
        return Vec::new();
    }
    if (width as usize) < MIN_WIDTH {
        return vec![Line::styled(
            wrap::truncate(&wrap::sanitize(&plain_text(block)), width as usize),
            styles.dim(),
        )];
    }
    let width = width as usize;
    match block {
        Block::User { text, kind, at } => user(text, *kind, *at, styles, width),
        Block::Assistant {
            reasoning,
            text,
            expanded,
            partial,
            ..
        } => {
            let thinking = markdown::render(
                &reasoning_source(reasoning),
                text_width(width),
                &styles,
                thinking_style(styles),
            );
            let reply = markdown::render(text, text_width(width), &styles, styles.text());
            let mut rows = assistant_rows(
                &thinking,
                &reply,
                expanded.unwrap_or(fold.expanded),
                fold.show_reasoning,
                styles,
                width,
            );
            if *partial {
                rows.push(chrome(indent(
                    Line::styled(
                        wrap::truncate("Partial model response", width - INSET),
                        styles.warning(),
                    ),
                    INSET,
                )));
            }
            rows
        }
        Block::Tool(call) => tool(call, styles, width, call.is_expanded(fold.expanded)),
        Block::TurnEnd {
            outcome,
            reason,
            duration,
            requests,
            tool_rounds,
            changes,
            saving,
            ..
        } => {
            let text = turn_end_line(
                outcome,
                *duration,
                *requests,
                *tool_rounds,
                changes.as_ref(),
                *saving,
            );
            let style = match outcome {
                TurnOutcome::Completed => styles.meta(),
                TurnOutcome::Failed => styles.failure(),
                _ => styles.warning(),
            };
            let room = width - INSET;
            // The diff view is the next step after a turn that changed files.
            let next = " · /diff";
            let mut spans = vec![Span::styled(wrap::truncate(&text, room), style)];
            if changes.as_ref().is_some_and(|changes| !changes.is_empty())
                && wrap::width(&text) + wrap::width(next) <= room
            {
                spans.push(Span::styled(next, styles.dim()));
            }
            let mut lines = vec![chrome(indent(Line::from(spans), INSET))];
            if let Some(reason) = reason.as_deref().filter(|reason| !reason.is_empty()) {
                lines.extend(quote(reason, styles, width, fold.expanded));
            }
            lines
        }
        Block::Summary { text } => summary(
            "summary",
            "History summary",
            text,
            styles,
            width,
            fold.expanded,
        ),
        Block::Compaction {
            source_position,
            covered_items,
            summary: body,
        } => summary(
            "compaction",
            &compaction_label(*source_position, *covered_items),
            body,
            styles,
            width,
            fold.expanded,
        ),
        Block::EarlierHistory { hidden_items } => vec![chrome(indent(
            Line::styled(
                format!("↑ {hidden_items} earlier items · home to load"),
                styles.faint(),
            ),
            INSET,
        ))],
    }
}

/// The text of a block without styles, for degenerate widths and `/copy`.
pub fn plain_text(block: &Block) -> String {
    match block {
        Block::User { text, .. } | Block::Assistant { text, .. } => text.clone(),
        Block::Tool(call) => format!("{} {}", call.name, call.header_detail()),
        Block::Summary { text } => text.clone(),
        Block::Compaction { summary, .. } => summary.clone(),
        _ => String::new(),
    }
}

/// Widths below this cannot hold a rail, a card border and one glyph. Such a
/// frame is a degenerate terminal; the block then draws as clipped plain text.
pub const MIN_WIDTH: usize = 8;

/// A user card: the composer surface with a pad row above and below.
fn user(
    text: &str,
    kind: UserKind,
    at: Option<Timestamp>,
    styles: Styles,
    width: usize,
) -> Vec<Line<'static>> {
    let row = |spans: Vec<Span<'static>>| {
        surface_row(styles.user_rail(), styles.composer(), spans, width)
    };
    let mut lines = vec![chrome(row(Vec::new()))];
    for line in markdown::render(text, text_width(width), &styles, styles.text()) {
        lines.push(row(padded(line.spans)));
    }
    let mut meta = Vec::new();
    meta.extend(at.map(|at| stamp_label(at, local_zone())));
    if kind == UserKind::Steering {
        meta.push("↗ steering".to_owned());
    }
    if !meta.is_empty() {
        lines.push(chrome(row(padded(vec![Span::styled(
            meta.join(" · "),
            styles.meta(),
        )]))));
    }
    lines.push(chrome(row(Vec::new())));
    lines
}

/// `HH:MM · MM/DD/YYYY` for an owner timestamp in `zone`.
fn stamp_label(at: Timestamp, zone: &TimeZone) -> String {
    timestamp(at)
        .to_zoned(zone.clone())
        .strftime("%H:%M · %m/%d/%Y")
        .to_string()
}

pub(crate) fn timestamp(at: Timestamp) -> WallTime {
    let value = serde_json::to_value(at).expect("timestamp serializes");
    value
        .as_str()
        .expect("timestamp is RFC3339")
        .parse()
        .expect("Host timestamp is valid")
}

fn local_zone() -> &'static TimeZone {
    // Windows reports an IANA name but supplies no zoneinfo database. Keep
    // jiff's tzdb-bundle-platform feature enabled: without it system() can
    // silently fall back to UTC, rendering 06:54 instead of local 14:54.
    // Tests render in UTC so snapshots do not depend on the machine.
    static ZONE: LazyLock<TimeZone> = LazyLock::new(|| match cfg!(test) {
        true => TimeZone::UTC,
        false => TimeZone::system(),
    });
    &ZONE
}

/// `spans` behind the one-cell inner pad.
fn padded(spans: Vec<Span<'static>>) -> Vec<Span<'static>> {
    let mut row = vec![Span::raw(" ")];
    row.extend(spans);
    row
}

/// An assistant message from its rendered thinking and reply rows: thinking on
/// the purple rail, folded to its first rows unless `expanded`, then the reply
/// at the same text column without a rail.
pub fn assistant_rows(
    thinking: &[Line<'static>],
    reply: &[Line<'static>],
    expanded: bool,
    show_reasoning: bool,
    styles: Styles,
    width: usize,
) -> Vec<Line<'static>> {
    let thinking: Vec<&Line<'static>> = thinking.iter().collect();
    let mut lines = thinking_rows(&thinking, expanded, show_reasoning, styles, width);
    if !reply.is_empty() {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        lines.extend(reply.iter().map(|line| indent(line.clone(), INSET)));
    }
    lines
}

/// The thinking half of an assistant message; a fold keeps the newest rows
/// under a hint for the earlier ones.
pub fn thinking_rows(
    thinking: &[&Line<'static>],
    expanded: bool,
    show_reasoning: bool,
    styles: Styles,
    width: usize,
) -> Vec<Line<'static>> {
    let rail = styles.reasoning_rail();
    let mut lines = Vec::new();
    if !thinking.is_empty() && !show_reasoning {
        lines.push(chrome(rail_row(
            rail,
            padded(vec![Span::styled("Thinking...", thinking_style(styles))]),
            width,
        )));
    } else if !thinking.is_empty() {
        let total = thinking.len();
        let shown = if !expanded && total > THINKING_ROWS {
            let hint = expand_hint(total - THINKING_ROWS, "earlier", true, width - 2, styles);
            lines.push(chrome(rail_row(rail, hint, width)));
            &thinking[total - THINKING_ROWS..]
        } else {
            thinking
        };
        lines.extend(
            shown
                .iter()
                .map(|line| rail_row(rail, padded(line.spans.clone()), width)),
        );
    }
    lines
}

/// Reasoning arrives as one short heading or sentence per line; markdown would
/// fold those lines into one paragraph, so every newline is a hard break.
pub fn reasoning_source(reasoning: &str) -> String {
    reasoning.trim().replace('\n', "  \n")
}

/// `... (N more lines, click or ctrl+o to expand)`; the click is left out when
/// the hint would not fit in `room` cells.
fn expand_hint(
    hidden: usize,
    noun: &str,
    clickable: bool,
    room: usize,
    styles: Styles,
) -> Vec<Span<'static>> {
    let count = plural(hidden as u64, &format!("{noun} line"));
    let long = format!("... ({count}, click or ctrl+o to expand)");
    let click = clickable && wrap::width(&long) <= room;
    let mut spans = vec![
        Span::styled(format!("... ({count},"), styles.meta()),
        Span::raw(" "),
    ];
    if click {
        spans.push(Span::styled("click or ", styles.meta()));
    }
    spans.extend([
        Span::styled("ctrl+o", styles.faint()),
        Span::styled(" to expand", styles.meta()),
        Span::styled(")", styles.text()),
    ]);
    spans
}

fn tool(call: &ToolCall, styles: Styles, width: usize, expanded: bool) -> Vec<Line<'static>> {
    let (rail, surface) = match (call.state, call.result_error.is_some()) {
        (_, true) => (styles.card_failure_rail(), styles.card_failure()),
        (ToolState::Pending | ToolState::Running, false) => (styles.card_rail(), styles.card()),
        (ToolState::Succeeded, false) => (styles.card_success_rail(), styles.card_success()),
        (ToolState::Failed | ToolState::Denied | ToolState::Cancelled, false) => {
            (styles.card_failure_rail(), styles.card_failure())
        }
    };
    let row = |spans: Vec<Span<'static>>| surface_row(rail, surface, spans, width);
    let inner = text_width(width);
    let body = card_body(call, expanded);
    // A bash call can take seconds to print anything; say it is alive.
    let waiting = call.is_running() && body.lines.is_empty() && body.hidden == 0;
    let running_row = || chrome(row(padded(vec![Span::styled("running…", styles.meta())])));
    if !expanded {
        let name = Span::styled(
            wrap::truncate(&card_title(call), inner),
            styles.tool_title(),
        );
        let mut lines = vec![row(title_spans(
            vec![name],
            meta(call, styles, true),
            inner,
        ))];
        let detail = one_line(&call.header_detail());
        if !detail.is_empty() {
            lines.push(row(padded(vec![Span::styled(
                detail,
                styles.tool_output(),
            )])));
        }
        if let Some(progress) = &call.progress {
            lines.push(chrome(row(padded(vec![Span::styled(
                wrap::truncate(progress, inner),
                styles.meta(),
            )]))));
        }
        if let Some(error) = error_line(call) {
            lines.push(row(padded(vec![Span::styled(
                one_line(error),
                styles.failure(),
            )])));
        }
        for delegation in call.delegations.iter().take(3) {
            lines.push(row(padded(vec![Span::styled(
                wrap::truncate(
                    &format!(
                        "child {} · agent {}",
                        delegation.child.session_id, delegation.child.agent_id
                    ),
                    inner,
                ),
                styles.meta(),
            )])));
        }
        if call.delegations.len() > 3 {
            lines.push(chrome(row(padded(vec![Span::styled(
                format!("{} more child sessions", call.delegations.len() - 3),
                styles.meta(),
            )]))));
        }
        let hidden = body.lines.len() + body.hidden;
        if hidden > 0 {
            lines.push(chrome(row(padded(expand_hint(
                hidden,
                "more",
                true,
                inner.saturating_sub(1),
                styles,
            )))));
        } else if waiting {
            lines.push(running_row());
        }
        return lines;
    }

    let bash = call.name == "bash";
    let spacer = || chrome(row(Vec::new()));
    let mut lines = vec![spacer()];
    let title_style = styles.tool_title().add_modifier(Modifier::BOLD);
    let mut right = meta(call, styles, !bash);
    let title = expanded_title(call);
    let title_rows = title
        .lines()
        .take(EXPANDED_ROWS)
        .flat_map(|line| wrap::wrap_line(&wrap::sanitize(line), inner));
    for (index, line) in title_rows.enumerate() {
        let left = vec![Span::styled(line, title_style)];
        lines.push(row(if index == 0 {
            title_spans(left, std::mem::take(&mut right), inner)
        } else {
            padded(left)
        }));
    }

    if let Some(progress) = &call.progress {
        lines.push(chrome(row(padded(vec![Span::styled(
            wrap::truncate(progress, inner),
            styles.meta(),
        )]))));
    }
    if let Some((value, truncated)) = call.argument_display() {
        lines.push(spacer());
        lines.push(chrome(row(padded(vec![Span::styled(
            "Arguments",
            styles.meta(),
        )]))));
        let mut argument_rows = value
            .lines()
            .flat_map(|line| wrap::wrap_line(&wrap::sanitize(line), inner));
        for argument in argument_rows.by_ref().take(EXPANDED_ROWS) {
            lines.push(row(padded(vec![Span::styled(
                argument,
                styles.tool_output(),
            )])));
        }
        if truncated || argument_rows.next().is_some() {
            lines.push(chrome(row(padded(vec![Span::styled(
                "… more arguments · /export for full JSON",
                styles.meta(),
            )]))));
        }
    }
    for delegation in call.delegations.iter().take(EXPANDED_ROWS) {
        lines.push(spacer());
        for detail in [
            format!("Child Agent: {}", delegation.child.agent_id),
            format!("Child Session: {}", delegation.child.session_id),
            format!("Task Message: {}", delegation.task_message_id),
        ] {
            for line in wrap::wrap_line(&detail, inner) {
                lines.push(row(padded(vec![Span::styled(line, styles.meta())])));
            }
        }
    }
    if call.delegations.len() > EXPANDED_ROWS {
        lines.push(chrome(row(padded(vec![Span::styled(
            format!(
                "{} more child sessions",
                call.delegations.len() - EXPANDED_ROWS
            ),
            styles.meta(),
        )]))));
    }

    let failed = (call.state == ToolState::Failed || call.result_error.is_some()) && !bash;
    let mut body_rows: Vec<Line<'static>> = body
        .lines
        .iter()
        .map(|line| {
            let style = if body.patch && is_patch_header(line) {
                styles.dim()
            } else if body.patch {
                styles.patch(line)
            } else if failed {
                styles.failure()
            } else {
                styles.tool_output()
            };
            let text = wrap::truncate(&wrap::sanitize(line), inner);
            row(padded(vec![Span::styled(text, style)]))
        })
        .collect();
    if body.hidden > 0 {
        let noun = match (body.lines.is_empty(), body.tail) {
            (true, _) => "line",
            (false, true) => "earlier line",
            (false, false) => "more line",
        };
        let hint = format!("… {}", plural(body.hidden as u64, noun));
        let hint = chrome(row(padded(vec![Span::styled(
            wrap::truncate(&hint, inner),
            styles.meta(),
        )])));
        if body.tail {
            body_rows.insert(0, hint);
        } else {
            body_rows.push(hint);
        }
    }
    if call.truncated {
        body_rows.push(chrome(row(padded(vec![Span::styled(
            wrap::truncate(
                "… output preview truncated · /export for full result",
                inner,
            ),
            styles.meta(),
        )]))));
    }
    if !body_rows.is_empty() {
        lines.push(spacer());
        lines.extend(body_rows);
    } else if waiting {
        lines.push(spacer());
        lines.push(running_row());
    }
    if bash {
        let mut footer: Vec<Span<'static>> = Vec::new();
        if let Some(duration) = call.duration {
            footer.push(Span::styled(
                format!("Took {}", format_duration(duration)),
                styles.meta(),
            ));
        }
        if let Some(code) = call.exit_code.filter(|code| *code != 0) {
            if !footer.is_empty() {
                footer.push(Span::styled(" · ", styles.meta()));
            }
            footer.push(Span::styled(format!("exit {code}"), styles.failure()));
        }
        if !footer.is_empty() {
            lines.push(spacer());
            lines.push(chrome(row(padded(footer))));
        }
    }
    lines.push(spacer());
    lines
}

fn display_name(call: &ToolCall) -> &str {
    match call.name.as_str() {
        "apply_patch" => "apply-patch",
        name => name,
    }
}

/// The collapsed title: the tool, and for a patch how many lines it has.
fn card_title(call: &ToolCall) -> String {
    match (call.name.as_str(), call.expanded_input()) {
        ("apply_patch", Some(input)) => {
            let lines = input.lines().count();
            format!("apply-patch ({})", plural(lines as u64, "line"))
        }
        _ => display_name(call).to_owned(),
    }
}

/// Why a call failed without running to an exit code, for its collapsed card.
fn error_line(call: &ToolCall) -> Option<&str> {
    if let Some(error) = call.result_error.as_deref() {
        return Some(error);
    }
    if call.state != ToolState::Failed || call.exit_code.is_some() {
        return None;
    }
    call.output_content()
        .into_iter()
        .map(str::trim)
        .find(|line| !line.is_empty())
}

/// The bold first rows of an expanded card: the whole command for `bash`,
/// else the tool name and its subject.
fn expanded_title(call: &ToolCall) -> String {
    let detail = call.header_detail();
    if call.name == "bash"
        && let Some(command) = call.expanded_input()
    {
        return format!("$ {}", command.trim());
    }
    match (call.name == "bash", detail.is_empty()) {
        (_, true) => display_name(call).to_owned(),
        (true, false) => detail,
        (false, false) => format!("{} {detail}", display_name(call)),
    }
}

/// What a card adds right of its title: completed duration, an unusual end, a
/// failing exit code and the size of a change. Running cards have no spinner or
/// timer. An expanded `bash` card puts duration and exit code in its footer.
fn meta(call: &ToolCall, styles: Styles, facts: bool) -> Vec<Span<'static>> {
    let mut items: Vec<Vec<Span<'static>>> = Vec::new();
    // "0.0s" says nothing, so instant tools show no time.
    if facts && let Some(duration) = call.duration.filter(|d| d.as_millis() >= 50) {
        items.push(vec![Span::styled(format_duration(duration), styles.meta())]);
    }
    match call.state {
        ToolState::Denied => items.push(vec![Span::styled("denied", styles.meta())]),
        ToolState::Cancelled => items.push(vec![Span::styled("cancelled", styles.meta())]),
        ToolState::Failed if call.exit_code.is_none() => {
            items.push(vec![Span::styled("failed", styles.failure())]);
        }
        _ => {}
    }
    if facts && let Some(code) = call.exit_code.filter(|code| *code != 0) {
        items.push(vec![Span::styled(format!("exit {code}"), styles.failure())]);
    }
    // A change that did not apply is not counted as one.
    if matches!(
        call.state,
        ToolState::Pending | ToolState::Running | ToolState::Succeeded
    ) && call.result_error.is_none()
        && let Some((added, removed)) = call.line_changes()
        && added + removed > 0
    {
        items.push(vec![
            Span::styled(format!("+{added}"), styles.added()),
            Span::raw(" "),
            Span::styled(format!("−{removed}"), styles.removed()),
        ]);
    }
    let mut spans = Vec::new();
    for (index, item) in items.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(" · ", styles.meta()));
        }
        spans.extend(item);
    }
    spans
}

/// A title row: `left` after the pad cell and `right` flush with the text
/// edge, which is left out when both do not fit.
fn title_spans(
    left: Vec<Span<'static>>,
    right: Vec<Span<'static>>,
    inner: usize,
) -> Vec<Span<'static>> {
    let (left_width, right_width) = (
        super::status::spans_width(&left),
        super::status::spans_width(&right),
    );
    let mut spans = padded(left);
    if right_width > 0 && left_width + right_width < inner {
        spans.push(chrome_span(Span::raw(
            " ".repeat(inner - left_width - right_width),
        )));
        spans.extend(right);
    }
    spans
}

/// One row of `text`: control characters gone and whitespace runs collapsed.
fn one_line(text: &str) -> String {
    wrap::sanitize(text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// What an expanded card shows under its title.
struct Body<'a> {
    lines: Vec<&'a str>,
    /// Lines left out: before the shown ones for a tail, after them for a head.
    hidden: usize,
    tail: bool,
    patch: bool,
}

/// `bash` shows its output tail; `edit`, `apply_patch` and `write` the head of
/// their input; `read`, failed and other tools the head of their result.
fn card_body(call: &ToolCall, expanded: bool) -> Body<'_> {
    let limit = if expanded { EXPANDED_ROWS } else { 0 };
    let failed = call.state == ToolState::Failed || call.result_error.is_some();
    let tail = call.name == "bash";
    // A failed change explains itself in its result, not in its input.
    if !tail
        && let Some(input) = call.expanded_input()
        && (!failed || call.output.is_empty())
    {
        let write = call.name == "write";
        let mut source = input.lines().filter(|line| write || !is_noise(line));
        let lines = source.by_ref().take(limit).collect();
        return Body {
            hidden: source.count(),
            lines,
            tail: false,
            patch: !write,
        };
    }
    let count = call.output.len() - usize::from(call.output.last().is_some_and(String::is_empty));
    let output = &call.output[..count];
    let shown = limit.min(count);
    let total = if tail && !output.is_empty() {
        call.output_dropped + count
    } else {
        call.output_lines.unwrap_or(0).max(count)
    };
    let lines = if tail {
        &output[count - shown..]
    } else {
        &output[..shown]
    };
    Body {
        hidden: total - shown,
        lines: lines.iter().map(String::as_str).collect(),
        tail,
        patch: false,
    }
}

/// A diff's file and hunk header line.
fn is_patch_header(line: &str) -> bool {
    ["--- ", "+++ ", "diff --git", "index ", "@@", "*** "]
        .iter()
        .any(|prefix| line.starts_with(prefix))
}

/// Patch lines that say nothing: the envelope and a bare `@@`.
fn is_noise(line: &str) -> bool {
    matches!(line.trim_end(), "*** Begin Patch" | "*** End Patch" | "@@")
}

/// `1 request`, `2 requests`.
pub fn plural(count: u64, noun: &str) -> String {
    match count {
        1 => format!("1 {noun}"),
        _ => format!("{count} {noun}s"),
    }
}

/// A short duration: `0.4s`, `12s`, `3m07s`.
pub fn format_duration(duration: std::time::Duration) -> String {
    let seconds = duration.as_secs_f64();
    if seconds < 10.0 {
        format!("{seconds:.1}s")
    } else if seconds < 60.0 {
        format!("{}s", seconds as u64)
    } else {
        format!("{}m{:02}s", seconds as u64 / 60, seconds as u64 % 60)
    }
}

const QUOTE_RAIL: &str = "│ ";
const QUOTE_MORE: &str = " · ctrl+o full message";
/// Rows of a Host failure shown before its expand hint.
const QUOTE_ROWS: usize = 3;

/// The Host failure behind a quote rail; the rail and the hint are chrome.
fn quote(message: &str, styles: Styles, width: usize, expanded: bool) -> Vec<Line<'static>> {
    let room = text_width(width) - wrap::width(QUOTE_RAIL);
    let message = wrap::sanitize(message.trim());
    let rows = if expanded {
        wrap::wrap(&message, room)
    } else {
        wrap::wrap_prefix(&message, room, QUOTE_ROWS + 1)
    };
    let cut = !expanded && rows.len() > QUOTE_ROWS;
    let shown = if cut { QUOTE_ROWS } else { rows.len() };
    rows.into_iter()
        .take(shown)
        .enumerate()
        .map(|(index, row)| {
            let mut spans = vec![chrome_span(Span::styled(QUOTE_RAIL, styles.faint()))];
            if cut && index == shown - 1 {
                let more = match room > 2 * wrap::width(QUOTE_MORE) {
                    true => format!("…{QUOTE_MORE}"),
                    false => "…".to_owned(),
                };
                let end = wrap::char_breaks(&row, room - wrap::width(&more))[0].end;
                spans.push(Span::styled(row[..end].to_owned(), styles.text()));
                spans.push(chrome_span(Span::styled(more, styles.dim())));
            } else {
                spans.push(Span::styled(row, styles.text()));
            }
            indent(Line::from(spans), INSET)
        })
        .collect()
}

/// `completed in 3m07s · 12 requests · 11 tool rounds · 3 files changed (+40 −7)`;
/// a turn that did not complete leads with its native outcome.
pub fn turn_end_line(
    outcome: &TurnOutcome,
    duration: Option<std::time::Duration>,
    requests: u64,
    tool_rounds: u64,
    changes: Option<&crate::transcript::ChangeSummary>,
    saving: Option<PersistenceState>,
) -> String {
    let completed = *outcome == TurnOutcome::Completed;
    let mut parts: Vec<String> = Vec::new();
    match (completed, duration) {
        (true, Some(duration)) => parts.push(format!("completed in {}", format_duration(duration))),
        (true, None) => parts.push("completed".into()),
        (false, duration) => {
            parts.push(
                match outcome {
                    TurnOutcome::Completed => "completed",
                    TurnOutcome::Failed => "failed",
                    TurnOutcome::Cancelled => "cancelled",
                }
                .to_owned(),
            );
            parts.extend(duration.map(format_duration));
        }
    }
    if completed && requests > 0 {
        parts.push(plural(requests, "request"));
    }
    if tool_rounds > 0 {
        parts.push(plural(tool_rounds, "tool round"));
    }
    if let Some(changes) = changes.filter(|changes| !changes.is_empty()) {
        let files = format!("{} changed", plural(changes.files as u64, "file"));
        parts.push(match changes.lines {
            Some((added, removed)) => format!("{files} (+{added} −{removed})"),
            None => files,
        });
    }
    match saving {
        Some(PersistenceState::Pending) => parts.push("save pending".into()),
        Some(PersistenceState::Failed) => parts.push("save failed".into()),
        Some(PersistenceState::NotApplicable) => parts.push("save not applicable".into()),
        Some(PersistenceState::Persisted) => parts.push("saved".into()),
        None => {}
    }
    wrap::sanitize(&parts.join(" · ")).into_owned()
}

/// The committed marker's covered item count and source position.
pub fn compaction_label(source_position: u64, covered_items: u64) -> String {
    format!(
        "Compacted {} · through position {source_position}",
        plural(covered_items, "item")
    )
}

fn summary(
    kind: &str,
    heading: &str,
    summary: &str,
    styles: Styles,
    width: usize,
    expanded: bool,
) -> Vec<Line<'static>> {
    let surface = styles.custom_message();
    let row = |spans: Vec<Span<'static>>| Line::from(fill(surface, spans, width));
    let label = |spans: Vec<Span<'static>>| chrome(row(spans));
    let text = wrap::truncate(heading, width - INSET);
    let mut lines = vec![
        label(Vec::new()),
        label(padded(vec![Span::styled(
            format!("[{kind}]"),
            styles.reasoning_rail().add_modifier(Modifier::BOLD),
        )])),
        label(Vec::new()),
    ];
    match (!summary.trim().is_empty()).then_some(summary) {
        Some(summary) if expanded => {
            lines.push(label(padded(vec![Span::styled(text, styles.bold())])));
            lines.push(label(Vec::new()));
            for line in markdown::render(summary, text_width(width), &styles, styles.text()) {
                lines.push(row(padded(line.spans)));
            }
        }
        Some(_) => lines.push(label(padded(vec![
            Span::styled(text, styles.text()),
            Span::styled(" (", styles.meta()),
            Span::styled("ctrl+o", styles.faint()),
            Span::styled(" to expand)", styles.meta()),
        ]))),
        None => lines.push(label(padded(vec![Span::styled(text, styles.text())]))),
    }
    lines.push(label(Vec::new()));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcript::UserKind;
    use crate::transcript::{ChangeSummary, RequestKey};
    use crate::ui::rails::content_cells;
    use crate::ui::theme::Palette;
    use minicore_host_protocol::{ToolCallRef, TurnId};
    use std::time::Duration;

    fn styles() -> Styles {
        Styles::of(Palette::dark())
    }

    fn fold() -> Fold {
        Fold {
            expanded: false,
            show_reasoning: true,
        }
    }

    fn expanded() -> Fold {
        Fold {
            expanded: true,
            show_reasoning: true,
        }
    }

    fn text(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| line.to_string().trim_end().to_owned())
            .collect()
    }

    fn assistant(text: &str, reasoning: &str, streaming: bool) -> Block {
        Block::Assistant {
            key: RequestKey::response(turn_id(), 0),
            reasoning: reasoning.into(),
            text: text.into(),
            streaming,
            partial: false,
            expanded: None,
        }
    }

    #[test]
    fn a_user_card_is_a_padded_surface_behind_the_rail() {
        let block = Block::User {
            text: "hello world".into(),
            kind: UserKind::Prompt,
            at: None,
        };
        let lines = render(&block, styles(), 40, fold());
        assert_eq!(text(&lines), ["▎", "▎ hello world", "▎"]);
        for line in &lines {
            assert_eq!(wrap::width(&line.to_string()), 40, "full width: {line:?}");
            assert_eq!(line.spans[0].style.bg, None, "the rail cell has no surface");
            assert_eq!(line.spans[1].style.bg, Some(Palette::dark().composer_bg));
        }
    }

    #[test]
    fn steering_blocks_are_labelled() {
        let block = Block::User {
            text: "focus on tests".into(),
            kind: UserKind::Steering,
            at: None,
        };
        let body = text(&render(&block, styles(), 40, fold()));
        assert_eq!(body[2], "▎ ↗ steering");
    }

    fn turn_id() -> TurnId {
        "00000000000000000001".parse().unwrap()
    }

    fn sent(stamp: &str) -> Option<Timestamp> {
        serde_json::from_value(serde_json::json!(stamp)).ok()
    }

    #[test]
    fn the_stamp_reads_in_the_given_zone_and_the_date_follows_it() {
        let at = sent("2026-10-20T21:38:05.250Z").unwrap();
        let zone = |hours| TimeZone::fixed(jiff::tz::Offset::constant(hours));
        assert_eq!(stamp_label(at, &zone(0)), "21:38 · 10/20/2026");
        assert_eq!(stamp_label(at, &zone(-5)), "16:38 · 10/20/2026");
        assert_eq!(stamp_label(at, &zone(8)), "05:38 · 10/21/2026");
        let early = sent("2026-01-02T03:04:00Z").unwrap();
        assert_eq!(stamp_label(early, &zone(0)), "03:04 · 01/02/2026");
    }

    #[test]
    fn a_stamped_card_shows_the_time_below_the_text_and_beside_the_steering_label() {
        let card = |kind, at| Block::User {
            text: "hello".into(),
            kind,
            at,
        };
        let at = sent("2026-10-20T21:38:00Z");
        let prompt = render(&card(UserKind::Prompt, at), styles(), 40, fold());
        assert_eq!(text(&prompt), ["▎", "▎ hello", "▎ 21:38 · 10/20/2026", "▎"]);
        let meta = &prompt[2];
        assert_eq!(wrap::width(&meta.to_string()), 40);
        assert_eq!(meta.spans[2].style.fg, Some(Palette::dark().meta));
        assert_eq!(meta.spans[2].style.bg, Some(Palette::dark().composer_bg));

        let steer = render(&card(UserKind::Steering, at), styles(), 40, fold());
        assert_eq!(
            steer[2].to_string().trim_end(),
            "▎ 21:38 · 10/20/2026 · ↗ steering"
        );
    }

    #[test]
    fn assistant_prose_is_markdown_at_the_thinking_column() {
        let block = assistant("## Title\n\nsome **bold** prose", "plan", false);
        let body = text(&render(&block, styles(), 60, fold()));
        assert_eq!(body[0], "▎ plan");
        assert_eq!(body[1], "");
        assert_eq!(
            body[2], "  Title",
            "the reply has no rail but the same column"
        );
        assert!(
            !body.iter().any(|line| line.contains("**")),
            "markdown markers are not shown: {body:?}"
        );
    }

    #[test]
    fn folded_thinking_keeps_its_newest_rows_whether_streaming_or_done() {
        let reasoning = "one\ntwo\nthree\nfour\nfive";
        for (reply, streaming) in [("", true), ("the answer", true), ("the answer", false)] {
            let folded = text(&render(
                &assistant(reply, reasoning, streaming),
                styles(),
                60,
                fold(),
            ));
            assert_eq!(
                folded[..4],
                [
                    "▎... (2 earlier lines, click or ctrl+o to expand)",
                    "▎ three",
                    "▎ four",
                    "▎ five"
                ],
                "{folded:?}"
            );
        }
        let open = text(&render(
            &assistant("the answer", reasoning, false),
            styles(),
            60,
            expanded(),
        ));
        assert_eq!(open[0], "▎ one", "{open:?}");
        assert!(open.iter().all(|line| !line.contains("ctrl+o")));
    }

    #[test]
    fn thinking_is_dim_italic_with_bold_headings() {
        let block = assistant("", "**Reading the tests**\nthen run them", false);
        let lines = render(&block, styles(), 60, fold());
        assert_eq!(text(&lines), ["▎ Reading the tests", "▎ then run them"]);
        let heading = &lines[0].spans[2].style;
        assert!(
            heading
                .add_modifier
                .contains(Modifier::BOLD | Modifier::ITALIC)
        );
        assert_eq!(lines[1].spans[2].style, thinking_style(styles()));
        assert_eq!(
            lines[0].spans[0].style.fg,
            Some(Palette::dark().thinking_rail)
        );
    }

    #[test]
    fn one_thinking_block_can_open_while_the_rest_stay_folded() {
        let mut block = assistant("", "a\nb\nc\nd", false);
        if let Block::Assistant { expanded, .. } = &mut block {
            *expanded = Some(true);
        }
        assert_eq!(render(&block, styles(), 60, fold()).len(), 4);
    }

    #[test]
    fn hidden_thinking_is_one_row() {
        let block = assistant("answer", "a\nb\nc\nd", false);
        let hidden = Fold {
            expanded: true,
            show_reasoning: false,
        };
        let body = text(&render(&block, styles(), 60, hidden));
        assert_eq!(body, ["▎ Thinking...", "", "  answer"]);
    }

    fn call() -> ToolCall {
        let mut call = ToolCall::new(
            ToolCallRef {
                turn_id: turn_id(),
                tool_call_id: "c".to_owned().into(),
            },
            "bash",
        );
        call.detail = "$ python3 -m unittest".into();
        call.state = ToolState::Succeeded;
        call.duration = Some(Duration::from_millis(400));
        call.exit_code = Some(0);
        call.output = vec!["Ran 30 tests".into(), "OK".into()];
        call.output_dropped = 12;
        call
    }

    fn card(call: ToolCall, fold: Fold) -> Vec<String> {
        text(&render(&Block::Tool(Box::new(call)), styles(), 60, fold))
    }

    #[test]
    fn a_folded_card_is_title_detail_and_hint() {
        assert_eq!(
            card(call(), fold()),
            [
                format!("▎ bash{}0.4s", " ".repeat(49)),
                "▎ $ python3 -m unittest".into(),
                "▎ ... (14 more lines, click or ctrl+o to expand)".into(),
            ]
        );
    }

    fn quiet(state: ToolState) -> ToolCall {
        let mut quiet = call();
        quiet.state = state;
        quiet.duration = None;
        quiet.exit_code = None;
        quiet.output.clear();
        quiet.output_dropped = 0;
        quiet
    }

    // Running is a native state, independent of observing its local start time.
    fn started() -> ToolCall {
        let mut started = quiet(ToolState::Running);
        started.started_at = Some(std::time::Instant::now());
        started
    }

    #[test]
    fn a_started_card_without_output_says_running_where_the_hint_goes() {
        assert_eq!(
            card(started(), fold()),
            ["▎ bash", "▎ $ python3 -m unittest", "▎ running…"]
        );
    }

    #[test]
    fn running_gives_way_to_the_hint_once_output_arrives() {
        let mut running = started();
        running.push_output("Ran 30 tests\n");
        let body = card(running, fold());
        assert_eq!(body[2], "▎ ... (1 more line, click or ctrl+o to expand)");
        assert!(!body.join("\n").contains("running…"));
    }

    #[test]
    fn a_card_that_has_not_started_says_nothing_running() {
        for mode in [fold(), expanded()] {
            let body = card(quiet(ToolState::Pending), mode).join("\n");
            assert!(!body.contains("running…"), "{body}");
        }
        assert_eq!(card(quiet(ToolState::Pending), fold()).len(), 2);
        assert!(
            card(quiet(ToolState::Running), fold())
                .join("\n")
                .contains("running…")
        );
    }

    #[test]
    fn a_finished_card_says_nothing_running() {
        for state in [
            ToolState::Succeeded,
            ToolState::Failed,
            ToolState::Denied,
            ToolState::Cancelled,
        ] {
            for mode in [fold(), expanded()] {
                let mut finished = quiet(state);
                finished.started_at = Some(std::time::Instant::now());
                let body = card(finished, mode).join("\n");
                assert!(!body.contains("running…"), "{state:?}: {body}");
            }
        }
    }

    #[test]
    fn an_expanded_started_card_without_output_says_running_as_its_body() {
        let body = card(started(), expanded());
        assert_eq!(
            body,
            ["▎", "▎ $ python3 -m unittest", "▎", "▎ running…", "▎",]
        );
    }

    #[test]
    fn the_running_row_is_chrome_that_a_selection_skips() {
        for mode in [fold(), expanded()] {
            let block = Block::Tool(Box::new(started()));
            let lines = render(&block, styles(), 60, mode);
            let row = lines
                .iter()
                .find(|line| line.to_string().contains("running…"))
                .unwrap();
            assert_eq!(content_cells(row), 0..0);
        }
    }

    #[test]
    fn a_settled_card_looks_the_same_as_one_that_kept_all_its_output() {
        let mut bash = call();
        for line in 0..400 {
            bash.push_output(&format!("line {line}\n"));
        }
        let mut read = call();
        read.name = "read".into();
        read.output = (0..400).map(|line| format!("line {line}")).collect();
        read.output_lines = Some(400);
        for call in [bash, read] {
            let mut settled = call.clone();
            settled.settle();
            assert!(settled.output.len() < call.output.len());
            for fold in [fold(), expanded()] {
                assert_eq!(card(settled.clone(), fold), card(call.clone(), fold));
            }
        }
    }

    #[test]
    fn folded_card_rows_are_one_line_and_cut_at_the_edge() {
        let mut long = call();
        long.detail = format!("$ cd app &&\n  {}", "x".repeat(80));
        let body = card(long, fold());
        assert_eq!(body[1], format!("▎ $ cd app && {}", "x".repeat(46)));
    }

    #[test]
    fn a_card_is_colored_by_its_state() {
        let palette = Palette::dark();
        for (state, rail, surface) in [
            (ToolState::Running, palette.card_rail, palette.card_bg),
            (
                ToolState::Succeeded,
                palette.card_success_rail,
                palette.card_success_bg,
            ),
            (
                ToolState::Failed,
                palette.card_failure_rail,
                palette.card_failure_bg,
            ),
        ] {
            let mut card = call();
            card.state = state;
            let lines = render(&Block::Tool(Box::new(card)), styles(), 60, fold());
            assert_eq!(lines[0].spans[0].style.fg, Some(rail));
            assert_eq!(lines[0].spans[1].style.bg, Some(surface));
        }
    }

    #[test]
    fn only_a_failing_exit_code_is_shown_and_it_is_red() {
        let mut failing = call();
        failing.exit_code = Some(1);
        let lines = render(&Block::Tool(Box::new(failing)), styles(), 60, fold());
        let exit = lines[0]
            .spans
            .iter()
            .find(|span| span.content == "exit 1")
            .expect("the exit code is in the title row");
        assert_eq!(exit.style.fg, Some(Palette::dark().failure));
        assert!(!card(call(), fold())[0].contains("exit"));
    }

    #[test]
    fn an_expanded_bash_card_shows_the_command_output_and_time() {
        let mut long = call();
        long.exit_code = Some(2);
        long.arguments = Some(serde_json::json!({"command":"cd app\npython3 -m unittest"}));
        assert_eq!(
            card(long, expanded()),
            [
                "▎",
                "▎ $ cd app",
                "▎ python3 -m unittest",
                "▎",
                "▎ … 12 earlier lines",
                "▎ Ran 30 tests",
                "▎ OK",
                "▎",
                "▎ Took 0.4s · exit 2",
                "▎",
            ]
        );
    }

    #[test]
    fn hidden_line_counts_mark_the_truncated_end_and_are_meta_chrome() {
        let mut read = call();
        read.name = "read".into();
        read.output_lines = Some(500);
        let mut whole = call();
        whole.output_dropped = 0;
        for (call, hint, before) in [
            (call(), Some("▎ … 12 earlier lines"), true),
            (read, Some("▎ … 498 more lines"), false),
            (whole, None, false),
        ] {
            let lines = render(&Block::Tool(Box::new(call)), styles(), 60, expanded());
            let rows = text(&lines);
            assert!(!rows.iter().any(|row| row.contains("full output")));
            if let Some(hint) = hint {
                let at = rows.iter().position(|row| row == hint).unwrap();
                let body = rows.iter().position(|row| row == "▎ Ran 30 tests").unwrap();
                assert_eq!(at < body, before, "{rows:?}");
                assert_eq!(lines[at].spans[2].style.fg, Some(Palette::dark().meta));
                assert!(lines[at].style.sub_modifier.contains(Modifier::HIDDEN));
            } else {
                assert!(!rows.iter().any(|row| row.starts_with("▎ …")), "{rows:?}");
            }
        }
    }

    #[test]
    fn expanded_titles_are_bold_and_name_the_subject() {
        let mut read = call();
        read.name = "read".into();
        read.detail = "src/app.py:1-40".into();
        read.exit_code = None;
        let lines = render(&Block::Tool(Box::new(read)), styles(), 60, expanded());
        assert!(lines[1].to_string().starts_with("▎ read src/app.py:1-40"));
        assert!(
            lines[1].spans[2]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
    }

    fn patch_card() -> ToolCall {
        let mut card = call();
        card.name = "apply_patch".into();
        card.exit_code = None;
        card.output = vec!["Applied".into()];
        card.set_arguments(&serde_json::json!({"patch":
            "diff --git a/app.py b/app.py\nindex 1..2 100644\n--- a/app.py\n+++ b/app.py\n\
             @@ -1,2 +1,3 @@\n def greet(name):\n+    \"\"\"Greet.\"\"\"\n     return name"
        }));
        card
    }

    #[test]
    fn a_patch_card_counts_its_change_and_expands_to_a_colored_diff() {
        let folded = card(patch_card(), fold());
        assert_eq!(folded[1], "▎ app.py");
        assert!(
            folded[0].starts_with("▎ apply-patch (8 lines)"),
            "{folded:?}"
        );
        assert!(folded[0].ends_with("0.4s · +1 −0"), "{folded:?}");
        assert_eq!(folded[2], "▎ ... (8 more lines, click or ctrl+o to expand)");

        let lines = render(
            &Block::Tool(Box::new(patch_card())),
            styles(),
            60,
            expanded(),
        );
        let body = text(&lines);
        assert_eq!(
            body[1],
            format!("▎ apply-patch app.py{}0.4s · +1 −0", " ".repeat(27))
        );
        assert!(body.iter().any(|row| row == "▎ @@ -1,2 +1,3 @@"));
        let added = body
            .iter()
            .position(|row| row == "▎ +    \"\"\"Greet.\"\"\"")
            .unwrap();
        assert_eq!(lines[added].spans[2].style.fg, Some(Palette::dark().added));
        assert!(
            body.iter().any(|row| row.contains("\"patch\":")),
            "{body:?}"
        );
    }

    #[test]
    fn large_patch_bodies_count_hidden_lines_without_collecting_them() {
        let mut patch = patch_card();
        patch.set_arguments(&serde_json::json!({"patch":format!(
            "*** Begin Patch\n{}*** End Patch\n", "+line\n@@\n".repeat(10_000)
        )}));
        let collapsed = card_body(&patch, false);
        assert_eq!(collapsed.lines.capacity(), 0);
        assert_eq!(collapsed.hidden, 10_000);
        let expanded = card_body(&patch, true);
        assert_eq!(expanded.lines, vec!["+line"; EXPANDED_ROWS]);
        assert_eq!(expanded.hidden, 10_000 - EXPANDED_ROWS);
    }

    #[test]
    fn a_truncated_read_counts_every_line_it_leaves_out() {
        let mut read = call();
        read.name = "read".into();
        read.detail = "large.txt:1-2000".into();
        read.duration = None;
        read.exit_code = None;
        read.output_dropped = 0;
        read.output_lines = Some(2000);
        read.output = Vec::new();
        // Structured metadata identifies a preview cut inside line 5.
        read.truncated = true;
        read.push_output("1: alpha\n2: beta\n3: gamma\n4: delta\n1");
        assert_eq!(
            card(read.clone(), fold())[2],
            "▎ ... (2000 more lines, click or ctrl+o to expand)"
        );
        let open = card(read, expanded());
        assert_eq!(
            open[2..9],
            [
                "▎",
                "▎ 1: alpha",
                "▎ 2: beta",
                "▎ 3: gamma",
                "▎ 4: delta",
                "▎ 1",
                "▎ … 1995 more lines",
            ]
        );
    }

    #[test]
    fn a_card_from_history_shows_its_unloaded_line_count() {
        for name in ["bash", "read"] {
            let mut card = call();
            card.name = name.into();
            card.output = Vec::new();
            card.output_lines = Some(42);
            let folded = self::card(card.clone(), fold());
            assert_eq!(
                folded[2],
                "▎ ... (42 more lines, click or ctrl+o to expand)"
            );
            let open = self::card(card, expanded());
            assert!(open.contains(&"▎ … 42 lines".to_owned()), "{open:?}");
        }
    }

    #[test]
    fn one_card_can_be_expanded_while_the_others_stay_folded() {
        let mut one = call();
        one.expanded = Some(true);
        let body = card(one, fold());
        assert_eq!(
            body.last().unwrap(),
            "▎",
            "an expanded card ends on its pad row"
        );
    }

    #[test]
    fn an_expanded_card_is_bounded() {
        let mut long = call();
        long.output = (0..400).map(|index| format!("line {index}")).collect();
        let open = card(long, expanded());
        assert!(open.len() <= 210, "{}", open.len());
        assert!(open.contains(&"▎ line 399".to_owned()));
    }

    #[test]
    fn tool_output_is_sanitized_before_it_reaches_the_terminal() {
        let mut dirty = call();
        dirty.output = vec!["\u{1b}[31mred\u{1b}[0m".into()];
        let body = card(dirty, expanded());
        assert!(body.iter().any(|line| line.contains("red")));
        assert!(!body.iter().any(|line| line.contains('\u{1b}')));
    }

    #[test]
    fn a_turn_end_line_is_quiet_and_names_the_change_size() {
        let block = Block::TurnEnd {
            turn_id: turn_id(),
            outcome: TurnOutcome::Completed,
            reason: None,
            duration: Some(Duration::from_secs(187)),
            requests: 12,
            tool_rounds: 11,
            usage: Default::default(),
            changes: Some(ChangeSummary {
                files: 3,
                lines: Some((40, 7)),
            }),
            saving: None,
        };
        let lines = render(&block, styles(), 100, fold());
        assert_eq!(
            lines[0].to_string(),
            "  completed in 3m07s · 12 requests · 11 tool rounds · 3 files changed (+40 −7) · /diff"
        );
        assert_eq!(lines[0].spans[1].style.fg, Some(Palette::dark().meta));
        assert_eq!(lines[0].spans[2].style.fg, Some(Palette::dark().dim));
    }

    fn ended(outcome: TurnOutcome, files: usize) -> Block {
        Block::TurnEnd {
            turn_id: turn_id(),
            outcome,
            reason: None,
            duration: Some(Duration::from_secs(31)),
            requests: 3,
            tool_rounds: 6,
            usage: Default::default(),
            changes: (files > 0).then_some(ChangeSummary { files, lines: None }),
            saving: None,
        }
    }

    #[test]
    fn a_turn_end_line_that_changed_files_points_to_the_diff_when_it_fits() {
        let interrupted = TurnOutcome::Cancelled;
        let line = |block: &Block, width| text(&render(block, styles(), width, fold()))[0].clone();
        assert_eq!(
            line(&ended(interrupted, 2), 100),
            "  cancelled · 31s · 6 tool rounds · 2 files changed · /diff"
        );
        assert_eq!(
            line(&ended(TurnOutcome::Completed, 0), 80),
            "  completed in 31s · 3 requests · 6 tool rounds"
        );
        // A hint that does not fit is dropped whole, not cut.
        let expected = "  completed in 31s · 3 requests · 6 tool rounds · 2 files changed";
        assert_eq!(
            line(
                &ended(TurnOutcome::Completed, 2),
                wrap::width(expected) as u16 + 2
            ),
            expected
        );
    }

    fn compaction_block(summary: Option<&str>) -> Block {
        Block::Compaction {
            source_position: 190,
            covered_items: 177,
            summary: summary.unwrap_or_default().into(),
        }
    }

    #[test]
    fn a_compaction_marker_shows_native_coverage_and_opens_its_summary() {
        let folded = text(&render(
            &compaction_block(Some("kept the plan")),
            styles(),
            90,
            fold(),
        ));
        assert_eq!(folded[1], " [compaction]");
        assert_eq!(
            folded[3],
            " Compacted 177 items · through position 190 (ctrl+o to expand)"
        );
        let open = text(&render(
            &compaction_block(Some("kept the plan")),
            styles(),
            90,
            expanded(),
        ));
        assert_eq!(open[5], " kept the plan", "{open:?}");
        let bare = text(&render(&compaction_block(None), styles(), 90, fold()));
        assert_eq!(bare[3], " Compacted 177 items · through position 190");
    }

    #[test]
    fn an_imported_summary_does_not_claim_compaction_coverage() {
        let block = Block::Summary {
            text: "Retained summary".into(),
        };
        let body = text(&render(&block, styles(), 80, expanded())).join("\n");
        assert!(body.contains("[summary]"));
        assert!(body.contains("Retained summary"));
        assert!(!body.contains("Compacted"));
    }

    #[test]
    fn earlier_history_names_the_hidden_items() {
        let history = Block::EarlierHistory { hidden_items: 40 };
        let body = text(&render(&history, styles(), 40, fold()));
        assert!(body[0].contains("40 earlier items"));
    }

    #[test]
    fn running_cards_have_no_spinner_or_elapsed_time() {
        for name in ["bash", "read", "apply_patch"] {
            for state in [ToolState::Pending, ToolState::Running] {
                for mode in [fold(), expanded()] {
                    let mut running = call();
                    running.name = name.into();
                    running.state = state;
                    running.duration = None;
                    running.exit_code = None;
                    running.started_at = Some(std::time::Instant::now());
                    assert!(meta(&running, styles(), true).is_empty());
                    let body = card(running, mode).join("\n");
                    assert!(!body.chars().any(|ch| "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏".contains(ch)), "{body}");
                }
            }
        }
        let mut waiting = call();
        waiting.state = ToolState::Pending;
        waiting.duration = None;
        assert_eq!(card(waiting, fold())[0], "▎ bash", "not started: no timer");
    }

    #[test]
    fn a_failed_patch_gives_its_reason_instead_of_a_change_count() {
        let mut failed = patch_card();
        failed.state = ToolState::Failed;
        failed.duration = None;
        failed.output = vec![
            "tool invocation is invalid".into(),
            "no hunk matched".into(),
        ];
        let body = card(failed, fold());
        assert_eq!(
            body[0],
            format!("▎ apply-patch (8 lines){}failed", " ".repeat(30))
        );
        assert_eq!(body[1], "▎ app.py");
        assert_eq!(body[2], "▎ tool invocation is invalid");
        assert_eq!(body[3], "▎ ... (2 more lines, click or ctrl+o to expand)");
    }

    #[test]
    fn a_completed_tool_with_a_failed_file_result_shows_the_result_error() {
        let mut call = patch_card();
        call.result_error = Some("No matching file".into());
        call.output = vec!["No matching file".into()];
        let block = Block::Tool(Box::new(call));
        let collapsed = render(&block, styles(), 70, fold());
        let rows = text(&collapsed);
        assert!(
            rows.iter().any(|row| row.contains("No matching file")),
            "{rows:?}"
        );
        assert!(!rows[0].contains("+1"), "failed changes are not applied");
        assert_eq!(collapsed[0].spans[0].style, styles().card_failure_rail());
        let expanded = text(&render(&block, styles(), 70, expanded())).join("\n");
        assert!(expanded.contains("No matching file"), "{expanded}");
        assert!(
            !expanded
                .lines()
                .any(|row| row == "▎ +    \"\"\"Greet.\"\"\"")
        );
        assert!(matches!(block, Block::Tool(call) if call.state == ToolState::Succeeded));
    }

    #[test]
    fn a_bash_card_shows_the_command_that_ran_not_the_summary() {
        let mut bash = call();
        bash.set_arguments(&serde_json::json!({
            "command":"\ngit diff -- README.md && git diff --check\necho done"
        }));
        bash.detail = "git diff --check && git diff --".into();
        assert_eq!(
            card(bash, fold())[1],
            "▎ $ git diff -- README.md && git diff --check"
        );
    }

    #[test]
    fn the_click_hint_is_left_out_when_it_does_not_fit() {
        let hint = |room| -> String {
            expand_hint(14, "more", true, room, styles())
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        };
        assert_eq!(hint(46), "... (14 more lines, click or ctrl+o to expand)");
        assert_eq!(hint(45), "... (14 more lines, ctrl+o to expand)");
    }

    #[test]
    fn narrow_widths_never_overflow() {
        let mut open = call();
        open.expanded = Some(true);
        for block in [
            Block::User {
                text: "a very long word without any break opportunity at all".into(),
                kind: UserKind::Prompt,
                at: None,
            },
            assistant(
                "```rust\nfn main() { println!(\"wide code\"); }\n```",
                "one\ntwo\nthree\nfour",
                false,
            ),
            Block::Tool(Box::new(call())),
            Block::Tool(Box::new(open)),
            compaction_block(Some("summary")),
            ended(TurnOutcome::Failed, 0),
        ] {
            for width in 3..12 {
                for line in render(&block, styles(), width, fold()) {
                    assert!(
                        crate::wrap::width(&line.to_string()) <= width as usize,
                        "{:?} at width {width}: {line:?}",
                        block.turn_id()
                    );
                }
            }
        }
    }

    #[test]
    fn turn_end_lines_preserve_native_tool_rounds_and_persistence() {
        assert_eq!(
            turn_end_line(
                &TurnOutcome::Failed,
                Some(Duration::from_millis(20)),
                1,
                0,
                None,
                None
            ),
            "failed · 0.0s"
        );
        let changes = ChangeSummary {
            files: 1,
            lines: Some((2, 1)),
        };
        assert_eq!(
            turn_end_line(
                &TurnOutcome::Completed,
                Some(Duration::from_secs(5)),
                1,
                1,
                Some(&changes),
                None
            ),
            "completed in 5.0s · 1 request · 1 tool round · 1 file changed (+2 −1)"
        );
        assert_eq!(
            turn_end_line(
                &TurnOutcome::Completed,
                None,
                8,
                7,
                None,
                Some(PersistenceState::Failed)
            ),
            "completed · 8 requests · 7 tool rounds · save failed"
        );
        for (persistence, label) in [
            (PersistenceState::Pending, "save pending"),
            (PersistenceState::Persisted, "saved"),
            (PersistenceState::NotApplicable, "save not applicable"),
        ] {
            assert_eq!(
                turn_end_line(&TurnOutcome::Completed, None, 0, 0, None, Some(persistence)),
                format!("completed · {label}")
            );
        }
        assert_eq!(format_duration(Duration::from_secs(187)), "3m07s");
    }

    #[test]
    fn a_partial_response_is_visible_and_its_label_is_excluded_from_copy() {
        let mut block = assistant("retained text", "", false);
        if let Block::Assistant { partial, .. } = &mut block {
            *partial = true;
        }
        let lines = render(&block, styles(), 60, fold());
        assert!(
            text(&lines)
                .iter()
                .any(|line| line.contains("Partial model response"))
        );
        assert!(content_cells(lines.last().unwrap()).is_empty());
        for width in 3..20 {
            assert!(
                render(&block, styles(), width, fold())
                    .iter()
                    .all(|line| { crate::wrap::width(&line.to_string()) <= width as usize })
            );
        }
    }

    #[test]
    fn expanded_tools_show_complete_arguments_and_delegated_session_identity() {
        let mut call = call();
        call.set_arguments(&serde_json::json!({"command":"cargo test", "timeout_ms":30000}));
        call.delegations.push(
            serde_json::from_value(serde_json::json!({
                "child":{"agent_id":"00000000000000000002","session_id":"00000000000000000003"},
                "task_message_id":"00000000000000000004"
            }))
            .unwrap(),
        );
        let block = Block::Tool(Box::new(call));
        let rendered = text(&render(&block, styles(), 80, expanded())).join("\n");
        assert!(rendered.contains("\"timeout_ms\": 30000"), "{rendered}");
        assert!(
            rendered.contains("Child Agent: 00000000000000000002"),
            "{rendered}"
        );
        assert!(
            rendered.contains("Child Session: 00000000000000000003"),
            "{rendered}"
        );
        assert!(
            rendered.contains("Task Message: 00000000000000000004"),
            "{rendered}"
        );
        for width in 3..20 {
            assert!(
                render(&block, styles(), width, expanded())
                    .iter()
                    .all(|line| { crate::wrap::width(&line.to_string()) <= width as usize })
            );
        }
    }
}
