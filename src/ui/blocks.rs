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
use super::rails::{chrome, chrome_span, indent, rail_row, surface_row};
use super::theme::Styles;
use crate::transcript::UserKind;
use crate::transcript::layout::Fold;
use crate::transcript::{Block, GroupSummary, Grouping, StepKind, ToolCall};
use crate::transcript::{EXPANDED_ROWS, ToolState};
use crate::wrap;

/// Columns before block text: the rail and one pad cell.
pub const INSET: usize = 2;

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
    render_with(block, &Grouping::None, styles, width, fold)
}

/// [`render`] for a block that may be a step of a folded run of work: a closed
/// group's steps take no rows, and its first step draws the summary row with
/// the newest step under it.
pub fn render_with(
    block: &Block,
    grouping: &Grouping,
    styles: Styles,
    width: u16,
    fold: Fold,
) -> Vec<Line<'static>> {
    match grouping {
        Grouping::None => render_block(block, styles, width, fold),
        Grouping::Member { open, .. } if !open && !fold.expanded => Vec::new(),
        Grouping::Member { .. } => member(block, styles, width, fold),
        Grouping::Head { summary, open, .. } => {
            let open = *open || fold.expanded;
            let mut rows = vec![group_row(summary, open, styles, width.into())];
            if open {
                let own = member(block, styles, width, fold);
                if !own.is_empty() {
                    rows.push(chrome(Line::default()));
                }
                rows.extend(own);
            } else if summary.live
                && let Some((kind, text)) = &summary.latest
            {
                rows.push(latest_row(*kind, text, styles, width.into()));
            }
            rows
        }
        Grouping::Tail { summary, open, .. } => render_tail(
            block,
            summary.as_ref(),
            *open || fold.expanded,
            styles,
            width,
            fold,
        ),
    }
}

/// Renders a finished run's final assistant message: when the run's group is
/// closed, only the reply stays outside; when open, its thinking appears as
/// the group's indented final step directly above the reply. A thinking-only
/// run with no earlier steps draws its `Worked` summary row directly above.
fn render_tail(
    block: &Block,
    summary: Option<&GroupSummary>,
    open: bool,
    styles: Styles,
    width: u16,
    fold: Fold,
) -> Vec<Line<'static>> {
    let Block::Assistant {
        reasoning,
        text,
        expanded,
        partial,
        ..
    } = block
    else {
        return render_block(block, styles, width, fold);
    };
    if (width as usize) < MIN_WIDTH {
        return render_block(block, styles, width, fold);
    }
    let width = width as usize;
    let mut rows = Vec::new();
    if let Some(summary) = summary {
        rows.push(group_row(summary, open, styles, width));
    }
    if open {
        let step_width = width.saturating_sub(GROUP_INDENT);
        let thinking = markdown::render(
            &reasoning_source(reasoning),
            text_width(step_width),
            &styles,
            thinking_style(styles),
        );
        let thinking: Vec<&Line<'static>> = thinking.iter().collect();
        let thinking = thinking_rows(
            &thinking,
            expanded.unwrap_or(fold.expanded),
            fold.show_reasoning,
            styles,
            step_width,
        );
        if !thinking.is_empty() {
            if !rows.is_empty() {
                rows.push(chrome(Line::default()));
            }
            rows.extend(thinking.into_iter().map(|line| indent(line, GROUP_INDENT)));
        }
    }
    let reply = markdown::render(text, text_width(width), &styles, styles.text());
    if !reply.is_empty() {
        if !rows.is_empty() {
            rows.push(Line::default());
        }
        rows.extend(reply.iter().map(|line| indent(line.clone(), INSET)));
    }
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

/// A step of an open group, one level in under its summary row.
fn member(block: &Block, styles: Styles, width: u16, fold: Fold) -> Vec<Line<'static>> {
    let inner = width.saturating_sub(GROUP_INDENT as u16);
    render_block(block, styles, inner, fold)
        .into_iter()
        .map(|line| indent(line, GROUP_INDENT))
        .collect()
}

/// How far an open group's steps sit in from its summary row.
pub(super) const GROUP_INDENT: usize = 2;

/// `▸ Worked 2m 14s · 18 tools · 3 files changed (+142 −37) · 1 failed`, or
/// `▸ Working · 6 tools` while the run goes on.
pub(super) fn group_row(
    summary: &GroupSummary,
    open: bool,
    styles: Styles,
    width: usize,
) -> Line<'static> {
    let mut parts = vec![match summary.duration {
        _ if summary.live => "Working".to_owned(),
        Some(duration) => format!("Worked {}", format_duration(duration)),
        None => "Worked".to_owned(),
    }];
    if summary.tools > 0 {
        parts.push(plural(summary.tools as u64, "tool"));
    }
    if let Some(changes) = summary.changes.filter(|changes| !changes.is_empty()) {
        let files = format!("{} changed", plural(changes.files as u64, "file"));
        parts.push(match changes.lines {
            Some((added, removed)) => format!("{files} (+{added} −{removed})"),
            None => files,
        });
    }
    let mut spans = vec![
        chrome_span(Span::styled(
            if open { "▾ " } else { "▸ " },
            styles.accent(),
        )),
        Span::styled(
            wrap::truncate(&parts.join(" · "), width.saturating_sub(16)),
            styles.meta(),
        ),
    ];
    if summary.failed > 0 {
        spans.push(Span::styled(" · ", styles.meta()));
        spans.push(Span::styled(
            format!("{} failed", summary.failed),
            styles.failure(),
        ));
    }
    Line::from(spans)
}

/// The newest step of a closed group, one line under its summary: it changes
/// as the work moves on, so the row reads like a list scrolling up.
fn latest_row(kind: StepKind, text: &str, styles: Styles, width: usize) -> Line<'static> {
    let (glyph, glyph_style) = match kind {
        StepKind::Running => ("●", styles.accent()),
        StepKind::Done => ("✓", styles.success()),
        StepKind::Failed => ("✗", styles.failure()),
        StepKind::Stopped => ("⊘", styles.meta()),
        StepKind::Thinking => ("∴", styles.meta()),
        StepKind::Note => ("›", styles.meta()),
    };
    let style = match kind {
        StepKind::Thinking => thinking_style(styles),
        StepKind::Note => styles.dim(),
        _ => styles.tool_output(),
    };
    Line::from(vec![
        chrome_span(Span::styled("  │ ", styles.meta())),
        chrome_span(Span::styled(format!("{glyph} "), glyph_style)),
        Span::styled(
            wrap::truncate(&wrap::sanitize(text), width.saturating_sub(7)),
            style,
        ),
    ])
}

fn render_block(block: &Block, styles: Styles, width: u16, fold: Fold) -> Vec<Line<'static>> {
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
        Block::User {
            text,
            kind: UserKind::Delegated,
            at,
            expanded,
        } => super::delegated::render(text, *at, styles, width, expanded.unwrap_or(fold.expanded))
            .unwrap_or_else(|| {
                user(
                    text,
                    UserKind::Prompt,
                    *at,
                    styles,
                    width,
                    expanded.unwrap_or(fold.expanded),
                )
            }),
        Block::User {
            text,
            kind,
            at,
            expanded,
        } => user(
            text,
            *kind,
            *at,
            styles,
            width,
            expanded.unwrap_or(fold.expanded),
        ),
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
        Block::Tool(call) if call.agent_card.is_some() => {
            super::agent_cards::render(call, styles, width, call.is_expanded(fold.expanded))
        }
        Block::Tool(call) if call.is_delegation_receipt() => {
            super::agent_cards::receipt(call, styles, width, call.is_expanded(fold.expanded))
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
        Block::Summary { text } => summary("History summary", text, styles, width, fold.expanded),
        Block::Compaction {
            source_position,
            covered_items,
            summary: body,
        } => summary(
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
/// Rows of a folded user message; pasted logs and code stay readable as text.
const USER_ROWS: usize = 12;

fn user(
    text: &str,
    kind: UserKind,
    at: Option<Timestamp>,
    styles: Styles,
    width: usize,
    expanded: bool,
) -> Vec<Line<'static>> {
    let row = |spans: Vec<Span<'static>>| {
        surface_row(styles.user_rail(), styles.composer(), spans, width)
    };
    let mut lines = vec![chrome(row(Vec::new()))];
    // What the user typed is shown as typed: line breaks and indentation are
    // content, not Markdown soft breaks.
    let mut body = wrap::wrapped(&wrap::sanitize(text), text_width(width), styles.text());
    let hidden = body.len().saturating_sub(USER_ROWS);
    if !expanded && hidden > 0 {
        body.truncate(USER_ROWS - 1);
    }
    for line in body {
        lines.push(row(padded(line.spans)));
    }
    if !expanded && hidden > 0 {
        lines.push(chrome(row(padded(vec![Span::styled(
            format!("… {} more lines", hidden + 1),
            styles.dim(),
        )]))));
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
pub(super) fn stamp_label(at: Timestamp, zone: &TimeZone) -> String {
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

pub(super) fn local_zone() -> &'static TimeZone {
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
pub(super) fn padded(spans: Vec<Span<'static>>) -> Vec<Span<'static>> {
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
    } else if !thinking.is_empty() && !expanded {
        // Folded thinking is one row: its newest heading, which also tracks a
        // live stream, and how much more there is.
        let latest: String = thinking
            .iter()
            .rev()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .find(|text| !text.trim().is_empty())
            .unwrap_or_default();
        let more = thinking.len().saturating_sub(1);
        let count = (more > 0).then(|| format!("  +{more}"));
        // The glyph and the count; the last column is the scrollbar's.
        let room = width.saturating_sub(2 + count.as_deref().map_or(0, wrap::width) + 1);
        // A step row: the glyph in the rail column, like tool rows.
        let mut spans = vec![
            chrome_span(Span::styled("∴ ", styles.meta())),
            Span::styled(wrap::truncate(latest.trim(), room), thinking_style(styles)),
        ];
        if let Some(count) = count {
            spans.push(chrome_span(Span::styled(count, styles.faint())));
        }
        lines.push(Line::from(spans));
    } else if !thinking.is_empty() {
        lines.extend(
            thinking
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

pub(super) fn tool(
    call: &ToolCall,
    styles: Styles,
    width: usize,
    expanded: bool,
) -> Vec<Line<'static>> {
    if !expanded {
        return tool_row(call, styles, width);
    }
    let mut lines = vec![tool_header_row(call, styles, width)];
    let card_width = width.saturating_sub(INSET);
    if card_width < MIN_WIDTH {
        return lines;
    }
    let (rail, surface) = match (call.state, call.result_error.is_some()) {
        (_, true) | (ToolState::Failed, false) => {
            (styles.card_failure_rail(), styles.card_failure())
        }
        (ToolState::Succeeded, false) => (styles.card_success_rail(), styles.card_success()),
        (
            ToolState::Pending | ToolState::Running | ToolState::Denied | ToolState::Cancelled,
            false,
        ) => (styles.card_rail(), styles.card()),
    };
    let row =
        |spans: Vec<Span<'static>>| indent(surface_row(rail, surface, spans, card_width), INSET);
    let inner = text_width(card_width);
    let body = card_body(call);
    // A bash call can take seconds to print anything; say it is alive.
    let waiting = call.is_running() && body.lines.is_empty() && body.hidden == 0;
    let running_row = || chrome(row(padded(vec![Span::styled("running…", styles.meta())])));

    let bash = call.name == "bash";
    let spacer = || chrome(row(Vec::new()));
    lines.push(spacer());

    // Keep the full wrapped command for bash, or a subject that wrapped/clipped
    // on the kept header row; short single-line paths avoid repeating.
    let subject = expanded_title(call);
    let multiline = subject.lines().skip(1).any(|line| !line.trim().is_empty());
    let header_clipped = lines[0]
        .spans
        .iter()
        .any(|span| span.content.ends_with('…'));
    if bash || multiline || header_clipped {
        let subject_rows = subject
            .lines()
            .take(EXPANDED_ROWS)
            .flat_map(|line| wrap::wrap_line(&wrap::sanitize(line), inner));
        for line in subject_rows {
            lines.push(row(padded(vec![Span::styled(
                line,
                styles.tool_output().add_modifier(Modifier::BOLD),
            )])));
        }
    }

    if let Some(progress) = &call.progress {
        lines.push(chrome(row(padded(vec![Span::styled(
            wrap::truncate(progress, inner),
            styles.meta(),
        )]))));
    }
    // These cards already show their main input as the subject or the body.
    let shown_inline = matches!(
        call.name.as_str(),
        "bash" | "write" | "edit" | "apply_patch"
    );
    if let Some((value, truncated)) = call.argument_display().filter(|_| !shown_inline) {
        if lines.len() > 2 {
            lines.push(spacer());
        }
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
        if lines.len() > 2 {
            lines.push(spacer());
        }
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
    // A file result can fail without saying so in its output; wrap long errors
    // so the failure message stays readable without truncating at one line.
    if let Some(error) = call.result_error.as_deref()
        && !body.lines.iter().any(|line| line.contains(error))
    {
        const ERROR_ROWS: usize = 8;
        let mut wrapped = wrap::wrapped(&wrap::sanitize(error), inner, styles.failure());
        let clipped = wrapped.len() > ERROR_ROWS;
        wrapped.truncate(ERROR_ROWS);
        let mut error_rows: Vec<Line<'static>> = wrapped
            .into_iter()
            .map(|line| row(padded(line.spans)))
            .collect();
        if clipped {
            error_rows.push(chrome(row(padded(vec![Span::styled(
                wrap::truncate("… /export for full error", inner),
                styles.meta(),
            )]))));
        }
        error_rows.extend(body_rows);
        body_rows = error_rows;
    }
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
        if lines.len() > 2 {
            lines.push(spacer());
        }
        lines.extend(body_rows);
    } else if waiting {
        if lines.len() > 2 {
            lines.push(spacer());
        }
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

/// The tool's own name, as the model and the docs spell it.
fn display_name(call: &ToolCall) -> &str {
    call.name.as_str()
}

/// Rows of live output under a running tool's row.
const LIVE_ROWS: usize = 3;

/// The single-line step header row shared by both folded and expanded tool cards.
fn tool_header_row(call: &ToolCall, styles: Styles, width: usize) -> Line<'static> {
    let (glyph, glyph_style) = tool_glyph(call, styles);
    let subject = one_line(call.header_detail().trim_start_matches("$ "));
    let mut right = meta(call, styles);
    if call.state == ToolState::Succeeded
        && call.line_changes().is_none()
        && let Some(lines) = call.output_lines.filter(|lines| *lines > 0)
    {
        if !right.is_empty() {
            right.push(Span::styled(" · ", styles.meta()));
        }
        right.push(Span::styled(plural(lines as u64, "line"), styles.meta()));
    }
    if call.is_running() && right.is_empty() {
        right.push(Span::styled("running", styles.meta()));
    }
    if right.is_empty() && glyph == "✗" {
        right.push(Span::styled("failed", styles.failure()));
    }
    let name = format!("{:<6}", display_name(call));
    let left = vec![
        chrome_span(Span::styled(format!("{glyph} "), glyph_style)),
        Span::styled(name, styles.tool_title()),
        Span::raw(" "),
        Span::styled(subject, styles.tool_output()),
    ];
    Line::from(row_spans(left, right, width))
}

/// A folded tool: one row of status glyph, name, subject and its most useful
/// fact; a running call adds its newest output.
fn tool_row(call: &ToolCall, styles: Styles, width: usize) -> Vec<Line<'static>> {
    // A failure's reason is part of the card; folded, the row says only that
    // it failed, so a long Turn does not fill with error text.
    let mut lines = vec![tool_header_row(call, styles, width)];
    if call.is_running() {
        let output = call.output_content();
        let start = output.len().saturating_sub(LIVE_ROWS);
        for text in &output[start..] {
            lines.push(live_row(&one_line(text), styles, width));
        }
    }
    lines
}

/// `●` running, `✓` done, `✗` failed, `⊘` denied or cancelled.
pub(super) fn tool_glyph(call: &ToolCall, styles: Styles) -> (&'static str, Style) {
    match call.state {
        ToolState::Pending | ToolState::Running => ("●", styles.accent()),
        ToolState::Succeeded
            if call.result_error.is_none() && call.exit_code.is_none_or(|code| code == 0) =>
        {
            ("✓", styles.success())
        }
        ToolState::Denied | ToolState::Cancelled => ("⊘", styles.meta()),
        _ => ("✗", styles.failure()),
    }
}

/// One line of a running step's newest output, under its name.
pub(super) fn live_row(text: &str, styles: Styles, width: usize) -> Line<'static> {
    Line::from(vec![
        chrome_span(Span::styled("  │ ", styles.meta())),
        Span::styled(
            wrap::truncate(text, width.saturating_sub(5)),
            styles.tool_output(),
        ),
    ])
}

/// `left` from the rail column and `right` flush with the edge; the subject
/// is cut first when both do not fit.
pub(super) fn row_spans(
    mut left: Vec<Span<'static>>,
    right: Vec<Span<'static>>,
    width: usize,
) -> Vec<Span<'static>> {
    // The last column belongs to the scrollbar.
    let room = width.saturating_sub(1);
    let right_width = super::status::spans_width(&right);
    let left_room = if right_width > 0 {
        room.saturating_sub(right_width + 2)
    } else {
        room
    };
    let mut used = 0;
    for span in &mut left {
        let own = wrap::width(&span.content);
        if used + own > left_room {
            let keep = left_room.saturating_sub(used);
            *span = Span::styled(wrap::truncate(&span.content, keep), span.style);
        }
        used += wrap::width(&span.content);
    }
    let mut spans = left;
    if right_width > 0 && used + right_width + 2 <= room {
        spans.push(chrome_span(Span::raw(
            " ".repeat(room - used - right_width),
        )));
        spans.extend(right);
    }
    spans
}

/// The subject under an expanded card's title: the whole `bash` command, or
/// the same detail the folded card shows.
fn expanded_title(call: &ToolCall) -> String {
    if call.name == "bash"
        && let Some(command) = call.expanded_input()
    {
        return format!("$ {}", command.trim());
    }
    call.header_detail()
}

/// What a card adds right of its title: completed duration, an unusual end, a
/// failing exit code and the size of a change. Running cards have no spinner or
/// timer. An expanded `bash` card puts duration and exit code in its footer.
fn meta(call: &ToolCall, styles: Styles) -> Vec<Span<'static>> {
    let mut items: Vec<Vec<Span<'static>>> = Vec::new();
    // "0.0s" says nothing, so instant tools show no time.
    if let Some(duration) = call.duration.filter(|d| d.as_millis() >= 50) {
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
    if let Some(code) = call.exit_code.filter(|code| *code != 0) {
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

/// One row of `text`: control characters gone and whitespace runs collapsed.
pub(super) fn one_line(text: &str) -> String {
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
fn card_body(call: &ToolCall) -> Body<'_> {
    let limit = EXPANDED_ROWS;
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
    // Saving is expected; only a save that has not happened is news, and a
    // reopened history must read the same as the live one did.
    match saving {
        Some(PersistenceState::Pending) => parts.push("save pending".into()),
        Some(PersistenceState::Failed) => parts.push("save failed".into()),
        Some(PersistenceState::Persisted | PersistenceState::NotApplicable) | None => {}
    }
    wrap::sanitize(&parts.join(" · ")).into_owned()
}

/// The committed marker's covered item count and source position.
pub fn compaction_label(source_position: u64, covered_items: u64) -> String {
    format!(
        "Context compacted · {} · through position {source_position}",
        plural(covered_items, "item")
    )
}

/// A divider row naming what happened to the history; the summary text opens
/// under it behind a neutral rail.
fn summary(
    heading: &str,
    summary: &str,
    styles: Styles,
    width: usize,
    expanded: bool,
) -> Vec<Line<'static>> {
    let has_summary = !summary.trim().is_empty();
    let mut label = heading.to_owned();
    if has_summary && !expanded {
        label.push_str(" · ctrl+o");
    }
    // The lead, the label and a rule to the scrollbar column.
    let text = wrap::truncate(&label, width.saturating_sub(5));
    let rule = width.saturating_sub(wrap::width(&text) + 4);
    let mut lines = vec![chrome(Line::from(vec![
        Span::styled("─ ", styles.faint()),
        Span::styled(text, styles.meta()),
        Span::styled(format!(" {}", "─".repeat(rule)), styles.faint()),
    ]))];
    if has_summary && expanded {
        for line in markdown::render(summary, text_width(width), &styles, styles.dim()) {
            lines.push(rail_row(styles.reasoning_rail(), padded(line.spans), width));
        }
    }
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
            expanded: None,
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
    fn user_text_keeps_its_lines_and_long_pastes_fold() {
        let block = Block::User {
            text: "Error: boom\n    at foo (a.js:1:1)\n    at bar (b.js:2:2)".into(),
            kind: UserKind::Prompt,
            at: None,
            expanded: None,
        };
        assert_eq!(
            text(&render(&block, styles(), 40, fold())),
            [
                "▎",
                "▎ Error: boom",
                "▎     at foo (a.js:1:1)",
                "▎     at bar (b.js:2:2)",
                "▎"
            ]
        );
        let long = Block::User {
            text: (0..40).map(|i| format!("log line {i}\n")).collect(),
            kind: UserKind::Prompt,
            at: None,
            expanded: None,
        };
        let folded = text(&render(&long, styles(), 40, fold()));
        assert!(
            folded.contains(&"▎ … 30 more lines".to_owned()),
            "{folded:?}"
        );
        assert!(!folded.iter().any(|row| row.contains("log line 39")));
        let open = text(&render(&long, styles(), 40, expanded()));
        assert!(open.iter().any(|row| row.contains("log line 39")));
    }

    #[test]
    fn steering_blocks_are_labelled() {
        let block = Block::User {
            text: "focus on tests".into(),
            kind: UserKind::Steering,
            at: None,
            expanded: None,
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
            expanded: None,
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
        assert_eq!(body[0], "∴ plan");
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
            assert_eq!(folded[0], "∴ five  +4", "the newest line and a count");
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
        let lines = render(&block, styles(), 60, expanded());
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
    fn a_folded_tool_is_one_row_of_glyph_name_subject_and_facts() {
        assert_eq!(
            card(call(), fold()),
            [format!(
                "✓ bash   python3 -m unittest{}0.4s",
                " ".repeat(27)
            )]
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
    fn a_started_tool_without_output_says_running_at_the_right() {
        assert_eq!(
            card(started(), fold()),
            [format!(
                "● bash   python3 -m unittest{}running",
                " ".repeat(24)
            )]
        );
    }

    #[test]
    fn a_running_tool_shows_its_latest_output_under_the_row() {
        let mut running = started();
        running.push_output("one\ntwo\nthree\nfour\n");
        let body = card(running, fold());
        assert_eq!(body[1..], ["  │ two", "  │ three", "  │ four"]);
    }

    #[test]
    fn an_expanded_started_card_without_output_says_running_as_its_body() {
        let body = card(started(), expanded());
        assert!(
            body[0].starts_with("● bash   python3 -m unittest"),
            "{body:?}"
        );
        assert_eq!(
            body[1..],
            [
                "  ▎",
                "  ▎ $ python3 -m unittest",
                "  ▎",
                "  ▎ running…",
                "  ▎",
            ]
        );
    }

    #[test]
    fn the_running_row_is_chrome_that_a_selection_skips() {
        let block = Block::Tool(Box::new(started()));
        let lines = render(&block, styles(), 60, expanded());
        let row = lines
            .iter()
            .find(|line| line.to_string().contains("running…"))
            .unwrap();
        assert_eq!(content_cells(row), 0..0);
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
        assert_eq!(body.len(), 1, "{body:?}");
        assert!(body[0].starts_with("✓ bash   cd app && xxx"), "{body:?}");
        assert!(body[0].ends_with("…  0.4s"), "{body:?}");
        assert!(
            wrap::width(&body[0]) < 60,
            "the scrollbar column stays free"
        );
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
            let lines = render(&Block::Tool(Box::new(card)), styles(), 60, expanded());
            // Line 0 is the kept header; line 1 is the indented top pad row (`  ` + `▎` + surface).
            assert_eq!(lines[1].spans[1].style.fg, Some(rail));
            assert_eq!(lines[1].spans[2].style.bg, Some(surface));
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
        let body = card(long, expanded());
        assert!(body[0].starts_with("✗ bash   cd app"), "{body:?}");
        assert!(body[0].ends_with("0.4s · exit 2"), "{body:?}");
        assert_eq!(
            body[1..],
            [
                "  ▎",
                "  ▎ $ cd app",
                "  ▎ python3 -m unittest",
                "  ▎",
                "  ▎ … 12 earlier lines",
                "  ▎ Ran 30 tests",
                "  ▎ OK",
                "  ▎",
                "  ▎ Took 0.4s · exit 2",
                "  ▎",
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
            (call(), Some("  ▎ … 12 earlier lines"), true),
            (read, Some("  ▎ … 498 more lines"), false),
            (whole, None, false),
        ] {
            let lines = render(&Block::Tool(Box::new(call)), styles(), 60, expanded());
            let rows = text(&lines);
            assert!(!rows.iter().any(|row| row.contains("full output")));
            if let Some(hint) = hint {
                let at = rows.iter().position(|row| row == hint).unwrap();
                let body = rows
                    .iter()
                    .position(|row| row == "  ▎ Ran 30 tests")
                    .unwrap();
                assert_eq!(at < body, before, "{rows:?}");
                assert_eq!(lines[at].spans[3].style.fg, Some(Palette::dark().meta));
                assert!(lines[at].style.sub_modifier.contains(Modifier::HIDDEN));
            } else {
                assert!(!rows.iter().any(|row| row.starts_with("  ▎ …")), "{rows:?}");
            }
        }
    }

    #[test]
    fn expanded_titles_keep_header_and_bold_wrapped_subject_when_needed() {
        let mut read = call();
        read.name = "read".into();
        read.detail = "src/app.py:1-40".into();
        read.exit_code = None;
        let lines = render(&Block::Tool(Box::new(read)), styles(), 60, expanded());
        // Short single-line read keeps its header row without repeating inside the card.
        assert!(lines[0].to_string().starts_with("✓ read   src/app.py:1-40"));
        assert_eq!(text(&lines)[1], "  ▎");
        assert!(
            !text(&lines)[1..]
                .iter()
                .any(|row| row.contains("src/app.py:1-40"))
        );

        // Long clipped path expands in bold inside the indented card.
        let mut long_read = call();
        long_read.name = "read".into();
        long_read.detail =
            "src/very/deeply/nested/module/components/service/handler_impl.rs:1-40".into();
        long_read.exit_code = None;
        let long_lines = render(&Block::Tool(Box::new(long_read)), styles(), 60, expanded());
        assert!(long_lines[2].to_string().starts_with("  ▎ src/very/"));
        assert!(
            long_lines[2].spans[3]
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
        assert!(folded[0].starts_with("✓ apply_patch app.py"), "{folded:?}");
        assert!(folded[0].ends_with("0.4s · +1 −0"), "{folded:?}");
        assert_eq!(folded.len(), 1);

        let lines = render(
            &Block::Tool(Box::new(patch_card())),
            styles(),
            60,
            expanded(),
        );
        let body = text(&lines);
        // Expanding keeps the folded header row and unfolds the indented colored diff below it.
        assert!(body[0].starts_with("✓ apply_patch app.py"), "{body:?}");
        assert!(body[0].ends_with("0.4s · +1 −0"), "{body:?}");
        assert_eq!(body[1], "  ▎");
        assert!(body.iter().any(|row| row == "  ▎ @@ -1,2 +1,3 @@"));
        let added = body
            .iter()
            .position(|row| row == "  ▎ +    \"\"\"Greet.\"\"\"")
            .unwrap();
        assert_eq!(lines[added].spans[3].style.fg, Some(Palette::dark().added));
        // The patch is the body; its argument JSON is not repeated.
        assert!(
            !body.iter().any(|row| row.contains("\"patch\":")),
            "{body:?}"
        );
    }

    #[test]
    fn large_patch_bodies_count_hidden_lines_without_collecting_them() {
        let mut patch = patch_card();
        patch.set_arguments(&serde_json::json!({"patch":format!(
            "*** Begin Patch\n{}*** End Patch\n", "+line\n@@\n".repeat(10_000)
        )}));
        let expanded = card_body(&patch);
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
        assert!(card(read.clone(), fold())[0].ends_with("2000 lines"));
        let open = card(read, expanded());
        assert_eq!(
            open[1..8],
            [
                "  ▎",
                "  ▎ 1: alpha",
                "  ▎ 2: beta",
                "  ▎ 3: gamma",
                "  ▎ 4: delta",
                "  ▎ 1",
                "  ▎ … 1995 more lines",
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
            assert!(folded[0].ends_with("42 lines"), "{folded:?}");
            let open = self::card(card, expanded());
            assert!(open.contains(&"  ▎ … 42 lines".to_owned()), "{open:?}");
        }
    }

    #[test]
    fn one_card_can_be_expanded_while_the_others_stay_folded() {
        let mut one = call();
        one.expanded = Some(true);
        let body = card(one, fold());
        assert_eq!(
            body.last().unwrap(),
            "  ▎",
            "an expanded card ends on its pad row"
        );
    }

    #[test]
    fn an_expanded_card_is_bounded() {
        let mut long = call();
        long.output = (0..400).map(|index| format!("line {index}")).collect();
        let open = card(long, expanded());
        assert!(open.len() <= 212, "{}", open.len());
        assert!(open.contains(&"  ▎ line 399".to_owned()));
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
    fn a_compaction_marker_is_one_divider_row_that_opens_its_summary() {
        let folded = render(
            &compaction_block(Some("kept the plan")),
            styles(),
            90,
            fold(),
        );
        assert_eq!(folded.len(), 1);
        let row = folded[0].to_string();
        assert!(
            row.starts_with("─ Context compacted · 177 items · through position 190 · ctrl+o ─"),
            "{row}"
        );
        assert_eq!(wrap::width(&row), 89, "the rule stops at the scrollbar");
        assert_eq!(content_cells(&folded[0]), 0..0, "a divider is chrome");
        let open = text(&render(
            &compaction_block(Some("kept the plan")),
            styles(),
            90,
            expanded(),
        ));
        assert_eq!(open[1], "▎ kept the plan", "{open:?}");
        let bare = text(&render(&compaction_block(None), styles(), 90, fold()));
        assert!(bare[0].starts_with("─ Context compacted · 177 items · through position 190 ─"));
    }

    #[test]
    fn an_imported_summary_does_not_claim_compaction_coverage() {
        let block = Block::Summary {
            text: "Retained summary".into(),
        };
        let body = text(&render(&block, styles(), 80, expanded())).join("\n");
        assert!(body.starts_with("─ History summary ─"), "{body}");
        assert!(body.contains("Retained summary"));
        assert!(!body.contains("compacted"));
    }

    #[test]
    fn earlier_history_names_the_hidden_items() {
        let history = Block::EarlierHistory { hidden_items: 40 };
        let body = text(&render(&history, styles(), 40, fold()));
        assert!(body[0].contains("40 earlier items"));
    }

    #[test]
    fn tool_lifecycle_only_labels_running_work_and_has_no_live_timer() {
        for name in ["bash", "read", "apply_patch"] {
            for state in [
                ToolState::Pending,
                ToolState::Running,
                ToolState::Succeeded,
                ToolState::Failed,
                ToolState::Denied,
                ToolState::Cancelled,
            ] {
                for mode in [fold(), expanded()] {
                    let mut call = quiet(state);
                    call.name = name.into();
                    call.started_at = Some(std::time::Instant::now());
                    let body = card(call, mode).join("\n");
                    assert_eq!(
                        body.contains("running…"),
                        state == ToolState::Running && mode.expanded,
                        "{name} {state:?} {mode:?}: {body}"
                    );
                    assert!(!body.chars().any(|ch| "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏".contains(ch)), "{body}");
                    assert!(!body.contains("0.0s"), "{body}");
                }
            }
        }
        assert_eq!(
            card(quiet(ToolState::Pending), fold()),
            ["● bash   python3 -m unittest"]
        );
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
        let body = card(failed.clone(), fold());
        assert_eq!(body.len(), 1, "folded, a failure is one row: {body:?}");
        assert!(body[0].starts_with("✗ apply_patch app.py"), "{body:?}");
        assert!(body[0].ends_with("failed"), "{body:?}");
        assert!(!body[0].contains("+1"), "{body:?}");
        let open = card(failed, expanded()).join("\n");
        assert!(open.contains("tool invocation is invalid"), "{open}");
    }

    #[test]
    fn a_completed_tool_with_a_failed_file_result_shows_the_result_error() {
        let mut call = patch_card();
        call.result_error = Some("No matching file".into());
        call.output = vec!["No matching file".into()];
        let block = Block::Tool(Box::new(call));
        let collapsed = render(&block, styles(), 70, fold());
        let rows = text(&collapsed);
        assert_eq!(rows.len(), 1, "the reason waits in the card: {rows:?}");
        assert!(!rows[0].contains("+1"), "failed changes are not applied");
        assert!(rows[0].starts_with("✗"), "{rows:?}");
        let expanded = text(&render(&block, styles(), 70, expanded())).join("\n");
        assert!(expanded.contains("No matching file"), "{expanded}");
        assert!(
            !expanded
                .lines()
                .any(|row| row == "  ▎ +    \"\"\"Greet.\"\"\"")
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
        assert!(
            card(bash, fold())[0].starts_with("✓ bash   git diff -- README.md && git diff --check"),
            "the first command line, not the summary"
        );
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
                expanded: None,
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
        // Only a save that has not happened is shown.
        for (persistence, label) in [
            (PersistenceState::Pending, "completed · save pending"),
            (PersistenceState::Persisted, "completed"),
            (PersistenceState::NotApplicable, "completed"),
        ] {
            assert_eq!(
                turn_end_line(&TurnOutcome::Completed, None, 0, 0, None, Some(persistence)),
                label
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
        // A tool whose input is not already its subject or body shows its JSON.
        call.name = "fetch".into();
        call.set_arguments(&serde_json::json!({"command":"cargo test", "timeout_ms":30000}));
        call.delegations.push(
            serde_json::from_value(serde_json::json!({
                "parent":{"agent_id":"00000000000000000002","session_id":"00000000000000000003"},
                "origin":{"turn_id":call.key.turn_id(),"tool_call_id":call.key.tool_call_id()},
                "child":{"agent_id":"00000000000000000002","session_id":"00000000000000000003"},
                "task_message_id":"00000000000000000004", "accepted_message_ids":["00000000000000000004"],
                "inputs":[], "completion":null
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

    #[test]
    fn long_tool_result_errors_wrap_instead_of_truncating_to_one_line() {
        let mut call = call();
        call.name = "agent".into();
        call.state = ToolState::Failed;
        call.output.clear();
        call.result_error = Some(
            "cannot create session: target agent configuration rejected because workspace path does not match allowed root".into(),
        );
        let rows = text(&render(
            &Block::Tool(Box::new(call)),
            styles(),
            48,
            expanded(),
        ));
        let joined = rows.join("\n");
        assert!(joined.contains("cannot create session:"), "{joined}");
        assert!(joined.contains("allowed root"), "{joined}");
    }

    #[test]
    fn cancelled_and_denied_tools_use_neutral_card_surface() {
        for state in [ToolState::Cancelled, ToolState::Denied] {
            let mut call = call();
            call.state = state;
            let rows = render(&Block::Tool(Box::new(call)), styles(), 60, expanded());
            let surface_bg = rows[1].spans.last().unwrap().style.bg;
            assert_eq!(surface_bg, styles().card().bg);
            assert_ne!(surface_bg, styles().card_failure().bg);
        }
    }

    #[test]
    fn worked_hides_latest_step_and_folds_final_thinking_while_working_shows_latest() {
        let mut summary = GroupSummary {
            live: true,
            tools: 2,
            latest: Some((StepKind::Running, "bash sleep 1".into())),
            ..GroupSummary::default()
        };
        let working = Grouping::Head {
            turn: turn_id(),
            segment: 0,
            summary: summary.clone(),
            open: false,
        };
        let working_rows = text(&render_with(
            &Block::Tool(Box::new(call())),
            &working,
            styles(),
            70,
            fold(),
        ));
        assert_eq!(working_rows.len(), 2, "{working_rows:?}");
        assert!(
            working_rows[0].contains("Working · 2 tools"),
            "{working_rows:?}"
        );
        assert!(working_rows[1].contains("bash sleep 1"), "{working_rows:?}");

        summary.live = false;
        let worked = Grouping::Head {
            turn: turn_id(),
            segment: 0,
            summary,
            open: false,
        };
        let worked_rows = text(&render_with(
            &Block::Tool(Box::new(call())),
            &worked,
            styles(),
            70,
            fold(),
        ));
        assert_eq!(
            worked_rows.len(),
            1,
            "closed Worked shows only its summary: {worked_rows:?}"
        );
        assert!(
            worked_rows[0].contains("Worked · 2 tools"),
            "{worked_rows:?}"
        );

        let final_reply = assistant("Final answer text.", "Compiling weather citations", false);
        let closed_tail = text(&render_with(
            &final_reply,
            &Grouping::Tail {
                turn: turn_id(),
                segment: 0,
                summary: None,
                open: false,
            },
            styles(),
            70,
            fold(),
        ));
        assert_eq!(
            closed_tail,
            ["  Final answer text."],
            "closed Worked keeps final thinking folded"
        );
        let open_tail = text(&render_with(
            &final_reply,
            &Grouping::Tail {
                turn: turn_id(),
                segment: 0,
                summary: None,
                open: true,
            },
            styles(),
            70,
            fold(),
        ));
        assert!(
            open_tail[0].contains("∴ Compiling weather citations"),
            "{open_tail:?}"
        );
        assert!(
            open_tail.iter().any(|row| row == "  Final answer text."),
            "{open_tail:?}"
        );
    }
}
