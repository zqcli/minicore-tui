//! Block rendering: one function per block kind. These are the visual rules of
//! design §5.5 and §7.2; the layout cache calls them once per change.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::theme::Palette;
use crate::transcript::block::{NoticeLevel, ToolState};
use crate::transcript::layout::Fold;
use crate::transcript::{Block, ToolCall};

/// Renders one block at `width`. Always returns at least one line so the layout
/// cache can rely on a block occupying a row.
pub fn render(block: &Block, palette: Palette, width: u16, fold: Fold) -> Vec<Line<'static>> {
    let content = width.saturating_sub(2) as usize;
    let mut lines = match block {
        Block::User { text, kind } => user(text, kind, palette, content),
        Block::Assistant {
            reasoning,
            text,
            streaming,
            ..
        } => assistant(reasoning, text, *streaming, palette, content, fold),
        Block::Tool(call) => tool(call, palette, content, fold.expanded_tools),
        Block::TurnEnd {
            outcome,
            duration,
            requests,
            tool_rounds,
            changes,
            ..
        } => vec![Line::styled(
            format!(
                "── {} ──",
                crate::app::turn_end_line(
                    outcome,
                    *duration,
                    *requests,
                    *tool_rounds,
                    changes.as_ref()
                )
            ),
            if outcome.is_completed() {
                palette.dim()
            } else {
                palette.warning()
            },
        )],
        Block::Compaction {
            operation_id: _,
            before,
            after,
            summary: _,
        } => vec![Line::styled(
            match (before, after) {
                (Some(before), Some(after)) => format!(
                    "─ Context compacted · {} → {} tokens ─",
                    crate::app::format_tokens(*before),
                    crate::app::format_tokens(*after)
                ),
                _ => "─ Context compacted ─".to_owned(),
            },
            palette.dim(),
        )],
        Block::Notice { level, text } => {
            let style = match level {
                NoticeLevel::Error => palette.failure(),
                NoticeLevel::Info => palette.dim(),
            };
            wrap(text, style, content)
        }
        Block::EarlierHistory { hidden_items } => vec![Line::styled(
            format!("↑ {hidden_items} earlier items · PgUp to load"),
            palette.faint(),
        )],
    };
    if !matches!(
        block,
        Block::User { .. } | Block::Assistant { .. } | Block::Tool(_)
    ) {
        lines.push(Line::raw(""));
    }
    if lines.is_empty() {
        lines.push(Line::raw(""));
    }
    lines
}

fn wrap(text: &str, style: Style, width: usize) -> Vec<Line<'static>> {
    crate::ui::wrap::wrap(text, width)
        .into_iter()
        .map(|line| Line::styled(line, style))
        .collect()
}

fn user(
    text: &str,
    kind: &crate::protocol::read::UserKind,
    palette: Palette,
    width: usize,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if matches!(kind, crate::protocol::read::UserKind::Steering) {
        lines.push(Line::styled("↳ steering", palette.dim()));
    }
    for line in wrap(text, palette.text(), width.saturating_sub(2)) {
        lines.push(rail(line, "▎ ", palette.user_rail));
    }
    lines.push(Line::raw(""));
    lines
}

fn assistant(
    reasoning: &str,
    text: &str,
    streaming: bool,
    palette: Palette,
    width: usize,
    fold: Fold,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let thinking = reasoning.trim();
    if !thinking.is_empty() {
        if fold.show_reasoning {
            for line in wrap(thinking, palette.italic_dim(), width.saturating_sub(2)) {
                lines.push(rail(line, "▏ ", palette.thinking_rail));
            }
        } else if streaming {
            let tail: Vec<&str> = thinking.lines().rev().take(3).collect();
            let tail: Vec<&str> = tail.into_iter().rev().collect();
            for line in wrap(
                &tail.join("\n"),
                palette.italic_dim(),
                width.saturating_sub(2),
            ) {
                lines.push(rail(line, "▏ ", palette.thinking_rail));
            }
        } else {
            lines.push(Line::styled(
                "thought · Ctrl+T to show",
                palette.italic_dim(),
            ));
        }
    }
    if !text.trim().is_empty() {
        for line in wrap(text, palette.text(), width.saturating_sub(2)) {
            lines.push(rail(line, "  ", palette.text));
        }
    }
    lines.push(Line::raw(""));
    lines
}

/// Puts a styled rail in front of an already-styled line.
fn rail(line: Line<'static>, rail: &str, rail_color: ratatui::style::Color) -> Line<'static> {
    let style = line.style;
    let mut spans = vec![Span::styled(
        rail.to_owned(),
        Style::default().fg(rail_color),
    )];
    spans.extend(line.spans);
    Line {
        style,
        spans,
        alignment: None,
    }
}

fn tool(call: &ToolCall, palette: Palette, width: usize, expanded: bool) -> Vec<Line<'static>> {
    let (glyph, style) = match call.state {
        ToolState::Pending => ("·", palette.dim()),
        ToolState::Running => ("⠋", palette.accent()),
        ToolState::Succeeded => ("✓", palette.success()),
        ToolState::Failed => ("✗", palette.failure()),
        ToolState::Denied | ToolState::Cancelled => ("⊘", palette.warning()),
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
    let mut lines = vec![Line::styled(
        format!(
            "▎ {}",
            crate::ui::wrap::clip(&header, &meta, width.saturating_sub(2))
        ),
        style,
    )];
    let limit = if expanded { 200 } else { 4 };
    let body = call.body_preview(limit);
    for line in &body {
        lines.push(Line::styled(
            format!("▎   {}", crate::ui::wrap::sanitize(line)),
            palette.faint(),
        ));
    }
    if let Some(total) = call.output_lines
        && total > body.len()
    {
        lines.push(Line::styled(
            format!(
                "▎   … {} more lines · Enter for full output",
                total - body.len()
            ),
            palette.faint(),
        ));
    }
    lines.push(Line::raw(""));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::read::UserKind;
    use crate::protocol::{LoopOutcome, ToolRef};
    use std::time::Duration;

    fn fold() -> Fold {
        Fold {
            expanded_tools: false,
            show_reasoning: false,
        }
    }

    fn text(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn a_user_block_has_a_rail_and_a_trailing_blank() {
        let block = Block::User {
            text: "hello world".into(),
            kind: UserKind::Prompt,
        };
        let lines = render(&block, Palette::plain(), 40, fold());
        assert!(text(&lines)[0].starts_with('▎'));
        assert_eq!(text(&lines).last().unwrap(), "");
    }

    #[test]
    fn steering_blocks_are_labelled() {
        let block = Block::User {
            text: "focus on tests".into(),
            kind: UserKind::Steering,
        };
        let lines = render(&block, Palette::plain(), 40, fold());
        assert!(text(&lines)[0].contains("steering"));
    }

    #[test]
    fn finished_reasoning_collapses_to_one_line() {
        let block = Block::Assistant {
            key: crate::transcript::RequestKey {
                loop_id: "l".into(),
                request_index: 0,
            },
            reasoning: "a long chain of thought".into(),
            text: "the answer".into(),
            streaming: false,
        };
        let lines = render(&block, Palette::plain(), 60, fold());
        let body = text(&lines);
        assert!(body.iter().any(|line| line.contains("Ctrl+T")));
        assert!(body.iter().any(|line| line.contains("the answer")));
    }

    #[test]
    fn streaming_reasoning_shows_only_the_tail() {
        let block = Block::Assistant {
            key: crate::transcript::RequestKey {
                loop_id: "l".into(),
                request_index: 0,
            },
            reasoning: "one\ntwo\nthree\nfour\nfive".into(),
            text: String::new(),
            streaming: true,
        };
        let lines = text(&render(&block, Palette::plain(), 60, fold()));
        assert!(!lines.iter().any(|line| line.contains("one")));
        assert!(lines.iter().any(|line| line.contains("five")));
    }

    #[test]
    fn a_tool_card_reports_status_duration_and_exit_code() {
        let call = ToolCall {
            key: ToolRef {
                session_id: "s".into(),
                loop_id: "l".into(),
                request_index: 0,
                tool_call_id: "c".into(),
            },
            name: "bash".into(),
            detail: "$ python3 -m unittest".into(),
            expanded_input: None,
            state: ToolState::Succeeded,
            started_at: None,
            duration: Some(Duration::from_millis(400)),
            exit_code: Some(0),
            output: vec!["OK".into()],
            output_lines: Some(12),
            truncated: false,
            stream_offsets: [None, None],
            full_output: None,
        };
        let block = Block::Tool(Box::new(call));
        let body = text(&render(&block, Palette::plain(), 60, fold()));
        assert!(body[0].contains("✓ bash"));
        assert!(body[0].contains("0.4s"));
        assert!(body[0].contains("exit 0"));
        assert!(body.iter().any(|line| line.contains("more lines")));
    }

    #[test]
    fn a_folded_card_caps_its_body_and_an_expanded_one_shows_more() {
        let mut call = ToolCall::new(
            ToolRef {
                session_id: "s".into(),
                loop_id: "l".into(),
                request_index: 0,
                tool_call_id: "c".into(),
            },
            "read",
        );
        call.state = ToolState::Succeeded;
        call.output = (0..50).map(|index| format!("line {index}")).collect();
        let folded = render(
            &Block::Tool(Box::new(call.clone())),
            Palette::plain(),
            60,
            fold(),
        );
        let expanded = render(
            &Block::Tool(Box::new(call)),
            Palette::plain(),
            60,
            Fold {
                expanded_tools: true,
                show_reasoning: false,
            },
        );
        assert!(expanded.len() > folded.len());
        assert!(expanded.len() <= 202);
    }

    #[test]
    fn a_turn_end_line_names_the_outcome() {
        let block = Block::TurnEnd {
            loop_id: "l".into(),
            outcome: LoopOutcome::Completed,
            duration: Some(Duration::from_secs(187)),
            requests: 12,
            tool_rounds: 11,
            usage: Default::default(),
            changes: Some(crate::transcript::ChangeSummary {
                files: 3,
                lines: Some((40, 7)),
            }),
        };
        let body = text(&render(&block, Palette::plain(), 100, fold()));
        let line = &body[0];
        assert!(line.contains("completed"));
        assert!(line.contains("3m07s"));
        assert!(line.contains("12 requests"));
        assert!(line.contains("3 files changed (+40 −7)"));
    }

    #[test]
    fn a_compaction_marker_shows_before_and_after() {
        let block = Block::Compaction {
            operation_id: "tui-1".into(),
            before: Some(32_200),
            after: Some(2_700),
            summary: None,
        };
        let body = text(&render(&block, Palette::plain(), 80, fold()));
        assert!(body[0].contains("32.2k → 2.7k"));
    }
}
