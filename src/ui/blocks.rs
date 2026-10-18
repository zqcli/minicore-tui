//! Block rendering: one function per block kind, following the rail style of
//! design §5.5 and §7.2. The layout cache calls these once per change and
//! stores the returned lines, so a block never re-renders per frame.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::markdown;
use super::rails;
use super::theme::Styles;
use super::wrap;
use crate::protocol::read::UserKind;
use crate::transcript::block::{NoticeLevel, ToolState};
use crate::transcript::layout::Fold;
use crate::transcript::{Block, ToolCall};

/// Columns the assistant's inset costs, including the rail.
pub const INSET: usize = 2;

/// Renders one block at `width`. Always returns at least one line so a block
/// occupies a row even when it has no content of its own.
pub fn render(block: &Block, styles: Styles, width: u16, fold: Fold) -> Vec<Line<'static>> {
    if (width as usize) < MIN_WIDTH {
        return vec![Line::styled(
            wrap::truncate(&wrap::sanitize(&plain_text(block)), width as usize),
            styles.dim(),
        )];
    }
    let width = width as usize;
    match block {
        Block::User { text, kind } => user(text, *kind, styles, width),
        Block::Assistant {
            reasoning,
            text,
            streaming,
            ..
        } => assistant(reasoning, text, *streaming, styles, width, fold),
        Block::Tool(call) => tool(call, styles, width, fold.expanded_tools),
        Block::TurnEnd {
            outcome,
            duration,
            requests,
            tool_rounds,
            changes,
            ..
        } => vec![turn_end(
            outcome,
            *duration,
            *requests,
            *tool_rounds,
            changes.as_ref(),
            styles,
            width,
        )],
        Block::Compaction {
            before,
            after,
            summary,
        } => compaction(*before, *after, summary.as_deref(), styles, width, fold),
        Block::Notice { level, text } => {
            let style = match level {
                NoticeLevel::Error => styles.error(),
                NoticeLevel::Info => styles.dim(),
            };
            wrap::wrap(text, width)
                .into_iter()
                .map(|line| Line::styled(line, style))
                .collect()
        }
        Block::EarlierHistory { hidden_items } => vec![Line::styled(
            format!("↑ {hidden_items} earlier items · home to load"),
            styles.subtle(),
        )],
    }
}

/// The text of a block without styles, for degenerate widths and `/copy`.
pub fn plain_text(block: &Block) -> String {
    match block {
        Block::User { text, .. } | Block::Assistant { text, .. } => text.clone(),
        Block::Tool(call) => format!("{}{}", call.name, call.detail),
        Block::Notice { text, .. } => text.clone(),
        Block::Compaction { summary, .. } => summary.clone().unwrap_or_default(),
        _ => String::new(),
    }
}

/// Widths below this cannot hold a rail, a card border and one glyph. Such a
/// frame is a degenerate terminal; the block then draws as clipped plain text.
pub const MIN_WIDTH: usize = 8;

fn user(text: &str, kind: UserKind, styles: Styles, width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if kind == UserKind::Steering {
        lines.push(Line::styled("↳ steering", styles.dim()));
    }
    let body = markdown::render(text, width.saturating_sub(INSET), &styles, styles.text());
    let mut body: Vec<Line<'static>> = body
        .into_iter()
        .map(|line| rails::user_rail(line, styles))
        .collect();
    if body.is_empty() {
        body.push(rails::user_rail(Line::raw(""), styles));
    }
    lines.extend(body);
    lines
}

fn assistant(
    reasoning: &str,
    text: &str,
    streaming: bool,
    styles: Styles,
    width: usize,
    fold: Fold,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let thinking = reasoning.trim_end();
    if !thinking.is_empty() {
        if fold.show_reasoning {
            for line in
                markdown::render(thinking, width.saturating_sub(INSET), &styles, styles.dim())
            {
                lines.push(rails::reasoning_rail(line, styles));
            }
        } else if streaming {
            // While a message streams, the live reasoning tail is the useful
            // part; the folded summary replaces it once the message ends.
            let tail: Vec<&str> = thinking.lines().rev().take(3).collect();
            let tail: Vec<&str> = tail.into_iter().rev().collect();
            for line in markdown::render(
                &tail.join("\n"),
                width.saturating_sub(INSET),
                &styles,
                styles.dim(),
            ) {
                lines.push(rails::reasoning_rail(line, styles));
            }
        } else {
            let rows = thinking.lines().count().max(1);
            lines.push(Line::styled(
                format!("thought for {rows} lines · Ctrl+T to show"),
                styles.dim().add_modifier(ratatui::style::Modifier::ITALIC),
            ));
        }
    }
    if !text.trim().is_empty() {
        for line in markdown::render(text, width.saturating_sub(INSET), &styles, styles.text()) {
            lines.push(rails::rail(line, styles));
        }
    }
    lines
}

fn tool(call: &ToolCall, styles: Styles, width: usize, expanded: bool) -> Vec<Line<'static>> {
    let (glyph, state_style, band) = match call.state {
        ToolState::Pending => ("·", styles.dim(), styles.theme.tool_bg),
        ToolState::Running => ("⠋", styles.accent(), styles.theme.tool_bg),
        ToolState::Succeeded => ("✓", styles.success(), styles.theme.tool_success_bg),
        ToolState::Failed => ("✗", styles.error(), styles.theme.tool_error_bg),
        ToolState::Denied | ToolState::Cancelled => {
            ("⊘", styles.warning(), styles.theme.tool_error_bg)
        }
    };
    let mut meta = String::new();
    if let Some(duration) = call.duration {
        meta.push_str(&crate::app::format_duration(duration));
    }
    if let Some(code) = call.exit_code {
        if !meta.is_empty() {
            meta.push_str(" · ");
        }
        meta.push_str(&format!("exit {code}"));
    }
    if call.truncated {
        if !meta.is_empty() {
            meta.push_str(" · ");
        }
        meta.push_str("truncated");
    }
    let header = format!("{glyph} {}  {}", call.name, call.detail);
    let mut lines = vec![card_row(
        &wrap::clip(&header, &meta, width.saturating_sub(INSET)),
        state_style,
        styles,
        band,
    )];
    let limit = if expanded { 200 } else { 4 };
    let body: Vec<&str> = call.body_preview(limit);
    for line in &body {
        lines.push(card_row(
            &format!(
                "  {}",
                wrap::truncate(&wrap::sanitize(line), width.saturating_sub(INSET + 2))
            ),
            styles.subtle(),
            styles,
            band,
        ));
    }
    if let Some(total) = call.output_lines
        && total > body.len()
    {
        lines.push(card_row(
            &wrap::truncate(
                &format!(
                    "  … {} earlier lines · enter for full output",
                    total - body.len()
                ),
                width.saturating_sub(INSET),
            ),
            styles.subtle(),
            styles,
            band,
        ));
    }
    lines
}

/// One card row: rail, body, and the card's background band.
fn card_row(
    text: &str,
    style: Style,
    styles: Styles,
    band: ratatui::style::Color,
) -> Line<'static> {
    let fill = if styles.color { style.bg(band) } else { style };
    Line {
        style: fill,
        spans: vec![Span::styled(format!("▎ {text}"), fill)],
        alignment: None,
    }
}

fn turn_end(
    outcome: &crate::protocol::LoopOutcome,
    duration: Option<std::time::Duration>,
    requests: u32,
    tool_rounds: u64,
    changes: Option<&crate::transcript::ChangeSummary>,
    styles: Styles,
    width: usize,
) -> Line<'static> {
    let mut text = crate::app::turn_end_line(outcome, duration, requests, tool_rounds, changes);
    if let Some(changes) = changes
        && !changes.is_empty()
    {
        text.push_str(&format!(" (+{} −{})", changes.added, changes.removed));
    }
    let labeled = format!("── {text} ──");
    let style = if outcome.is_completed() {
        styles.dim()
    } else {
        styles.warning()
    };
    Line::styled(wrap::truncate(&labeled, width), style)
}

fn compaction(
    before: Option<u64>,
    after: Option<u64>,
    summary: Option<&str>,
    styles: Styles,
    width: usize,
    fold: Fold,
) -> Vec<Line<'static>> {
    let text = match (before, after) {
        (Some(before), Some(after)) => format!(
            "─ Context compacted · {} → {} tokens ─",
            crate::app::format_tokens(before),
            crate::app::format_tokens(after)
        ),
        _ => "─ Context compacted ─".to_owned(),
    };
    let mut lines = vec![Line::styled(wrap::truncate(&text, width), styles.dim())];
    if fold.show_reasoning
        && let Some(summary) = summary
    {
        for line in markdown::render(summary, width.saturating_sub(INSET), &styles, styles.dim()) {
            lines.push(rails::rail(line, styles));
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::read::UserKind;
    use crate::protocol::{LoopOutcome, ToolRef};
    use crate::theme::{Palette, ThemeKind};
    use crate::transcript::{ChangeSummary, RequestKey};
    use std::time::Duration;

    fn styles() -> Styles {
        Styles::new(Palette::for_kind(ThemeKind::Dark), false)
    }

    fn fold() -> Fold {
        Fold {
            expanded_tools: false,
            show_reasoning: false,
        }
    }

    fn text(lines: &[Line<'static>]) -> Vec<String> {
        lines.iter().map(Line::to_string).collect()
    }

    fn assistant(text: &str, reasoning: &str, streaming: bool) -> Block {
        Block::Assistant {
            key: RequestKey {
                loop_id: "l".into(),
                request_index: 0,
            },
            reasoning: reasoning.into(),
            text: text.into(),
            streaming,
        }
    }

    #[test]
    fn a_user_block_is_marked_by_its_rail() {
        let block = Block::User {
            text: "hello world".into(),
            kind: UserKind::Prompt,
        };
        let body = text(&render(&block, styles(), 40, fold()));
        assert!(body[0].starts_with("▎ hello world"), "{body:?}");
    }

    #[test]
    fn steering_blocks_are_labelled() {
        let block = Block::User {
            text: "focus on tests".into(),
            kind: UserKind::Steering,
        };
        let body = text(&render(&block, styles(), 40, fold()));
        assert!(body[0].contains("steering"));
    }

    #[test]
    fn assistant_prose_is_markdown_not_raw_text() {
        let block = assistant("## Title\n\nsome **bold** prose", "", false);
        let body = text(&render(&block, styles(), 60, fold()));
        assert!(body.iter().any(|line| line.contains("Title")));
        assert!(body.iter().any(|line| line.contains("bold")));
        assert!(
            !body.iter().any(|line| line.contains("**")),
            "markdown markers are not shown: {body:?}"
        );
    }

    #[test]
    fn finished_reasoning_collapses_and_ctrl_t_shows_it() {
        let block = assistant("the answer", "one\ntwo\nthree", false);
        let folded = text(&render(&block, styles(), 60, fold()));
        assert!(folded.iter().any(|line| line.contains("Ctrl+T")));
        assert!(folded.iter().any(|line| line.contains("the answer")));

        let shown = text(&render(
            &block,
            styles(),
            60,
            Fold {
                expanded_tools: false,
                show_reasoning: true,
            },
        ));
        assert!(shown.iter().any(|line| line.contains("three")));
        assert!(
            shown[0].starts_with('▏'),
            "reasoning rows carry the reasoning rail: {shown:?}"
        );
    }

    #[test]
    fn streaming_reasoning_shows_only_the_tail() {
        let block = assistant("", "one\ntwo\nthree\nfour\nfive", true);
        let lines = text(&render(&block, styles(), 60, fold()));
        assert!(!lines.iter().any(|line| line.contains("one")));
        assert!(lines.iter().any(|line| line.contains("five")));
    }

    fn call() -> ToolCall {
        let mut call = ToolCall::new(
            ToolRef {
                session_id: "s".into(),
                loop_id: "l".into(),
                request_index: 0,
                tool_call_id: "c".into(),
            },
            "bash",
        );
        call.detail = "$ python3 -m unittest".into();
        call.state = ToolState::Succeeded;
        call.duration = Some(Duration::from_millis(400));
        call.exit_code = Some(0);
        call.output = vec!["Ran 30 tests".into(), "OK".into()];
        call.output_lines = Some(14);
        call
    }

    #[test]
    fn a_tool_card_reports_status_duration_and_exit_code() {
        let block = Block::Tool(Box::new(call()));
        let body = text(&render(&block, styles(), 60, fold()));
        assert!(body[0].contains("✓ bash"));
        assert!(body[0].contains("0.4s"));
        assert!(body[0].contains("exit 0"));
        assert!(body.iter().any(|line| line.contains("12 earlier lines")));
    }

    #[test]
    fn a_folded_card_is_small_and_an_expanded_one_is_bounded() {
        let mut card = call();
        card.output = (0..400).map(|index| format!("line {index}")).collect();
        let block = Block::Tool(Box::new(card));
        let folded = render(&block, styles(), 60, fold());
        let expanded = render(
            &block,
            styles(),
            60,
            Fold {
                expanded_tools: true,
                show_reasoning: false,
            },
        );
        assert!(folded.len() <= 8, "folded cards stay small");
        assert!(expanded.len() > folded.len());
        assert!(expanded.len() <= 202, "expanded cards cap at 200 rows");
    }

    #[test]
    fn a_failed_card_uses_the_error_band() {
        let mut card = call();
        card.state = ToolState::Failed;
        card.output = vec!["Traceback (most recent call last)".into()];
        let lines = render(&Block::Tool(Box::new(card)), styles(), 60, fold());
        assert_eq!(lines[0].style.bg, Some(styles().theme.tool_error_bg));
    }

    #[test]
    fn tool_output_is_sanitized_before_it_reaches_the_terminal() {
        let mut card = call();
        card.output = vec!["\u{1b}[31mred\u{1b}[0m".into()];
        let body = text(&render(&Block::Tool(Box::new(card)), styles(), 60, fold()));
        assert!(body.iter().any(|line| line.contains("red")));
        assert!(!body.iter().any(|line| line.contains('\u{1b}')));
    }

    #[test]
    fn a_turn_end_line_names_the_outcome_and_the_change_size() {
        let block = Block::TurnEnd {
            loop_id: "l".into(),
            outcome: LoopOutcome::Completed,
            duration: Some(Duration::from_secs(187)),
            requests: 12,
            tool_rounds: 11,
            usage: Default::default(),
            changes: Some(ChangeSummary {
                files: 3,
                added: 40,
                removed: 7,
            }),
        };
        let body = text(&render(&block, styles(), 100, fold()));
        let line = &body[0];
        assert!(line.contains("completed"), "{line}");
        assert!(line.contains("3m07s"), "{line}");
        assert!(line.contains("12 requests"), "{line}");
        assert!(line.contains("3 files changed (+40 −7)"), "{line}");
    }

    #[test]
    fn a_compaction_marker_shows_what_it_saved() {
        let block = Block::Compaction {
            before: Some(32_200),
            after: Some(2_700),
            summary: None,
        };
        let body = text(&render(&block, styles(), 80, fold()));
        assert!(body[0].contains("32.2k → 2.7k"), "{body:?}");
    }

    #[test]
    fn errors_are_red_and_archived_history_is_dim() {
        let error = Block::Notice {
            level: NoticeLevel::Error,
            text: "turn failed".into(),
        };
        let lines = render(&error, styles(), 40, fold());
        assert_eq!(lines[0].style.fg, Some(styles().theme.error));

        let history = Block::EarlierHistory { hidden_items: 40 };
        let body = text(&render(&history, styles(), 40, fold()));
        assert!(body[0].contains("40 earlier items"));
    }

    #[test]
    fn narrow_widths_never_overflow() {
        for block in [
            Block::User {
                text: "a very long word without any break opportunity at all".into(),
                kind: UserKind::Prompt,
            },
            assistant(
                "```rust\nfn main() { println!(\"wide code\"); }\n```",
                "",
                false,
            ),
            Block::Tool(Box::new(call())),
        ] {
            for width in 3..12 {
                for line in render(&block, styles(), width, fold()) {
                    assert!(
                        crate::ui::wrap::width(&line.to_string()) <= width as usize,
                        "{:?} at width {width}: {line:?}",
                        block.loop_id()
                    );
                }
            }
        }
    }
}
