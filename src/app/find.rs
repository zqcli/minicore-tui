//! Find in the transcript.

use std::borrow::Cow;

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::{App, Overlay, Scroll};
use crate::transcript::layout::Fold;
use crate::transcript::{Block, Transcript, UserKind};
use crate::ui::blocks;

/// Which of a block's texts a match is in; some of them start folded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Text,
    Thinking,
    Detail,
    Output,
}

/// One occurrence of the query in a block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FindMatch {
    pub block: usize,
    pub field: Field,
    /// Its position among the block's occurrences, in reading order.
    pub ordinal: usize,
}

/// The texts of a block that find reads, in the order the block shows them.
fn searchable(block: &Block, show_reasoning: bool) -> Vec<(Field, Cow<'_, str>)> {
    match block {
        Block::User {
            text,
            kind: UserKind::Delegated,
            ..
        } => match crate::ui::delegated::summary(text) {
            Some(summary) => {
                let short: String = summary.session_id.chars().take(6).collect();
                vec![(
                    Field::Text,
                    format!("agent {} · Session {short}", summary.outcome).into(),
                )]
            }
            None => vec![(Field::Text, text.as_str().into())],
        },
        Block::User { text, .. } => vec![(Field::Text, text.as_str().into())],
        Block::Assistant {
            reasoning, text, ..
        } => {
            let thinking = show_reasoning.then_some((Field::Thinking, reasoning.as_str().into()));
            thinking
                .into_iter()
                .chain([(Field::Text, text.as_str().into())])
                .collect()
        }
        Block::Tool(call) if call.agent_card.is_some() => {
            let card = call.agent_card.as_deref().expect("checked");
            let mut fields = vec![(Field::Detail, card.task.as_str().into())];
            let mut body = Vec::new();
            if let Some(name) = &card.agent_name {
                body.push(name.clone());
            }
            body.push(card.key.child.session_id.to_string());
            let delivered = card
                .result
                .as_deref()
                .and_then(crate::ui::delegated::summary);
            if let Some(summary) = delivered {
                if let Some(fault) = summary.fault {
                    body.push(fault);
                }
                if let Some(answer) = summary.answer {
                    body.push(answer);
                }
            } else {
                if let Some(fault) = card.fault() {
                    body.push(fault.to_owned());
                }
                if let Some(answer) = card.transcript.last_answer() {
                    body.push(answer.to_owned());
                }
            }
            for step in card.transcript.blocks() {
                match step {
                    Block::Tool(child_call) => {
                        if child_call.detail.is_empty() {
                            body.push(child_call.name.clone());
                        } else {
                            body.push(format!("{} {}", child_call.name, child_call.detail));
                        }
                    }
                    Block::Assistant {
                        text, reasoning, ..
                    } => {
                        if !text.trim().is_empty() {
                            body.push(text.clone());
                        } else if show_reasoning && !reasoning.trim().is_empty() {
                            body.push(reasoning.clone());
                        }
                    }
                    _ => {}
                }
            }
            if !body.is_empty() {
                fields.push((Field::Output, body.join("\n").into()));
            }
            fields
        }
        Block::Tool(call) => {
            let mut fields = vec![
                (Field::Detail, call.detail.as_str().into()),
                (Field::Output, call.output_content().join("\n").into()),
            ];
            if let Some(error) = &call.result_error {
                fields.push((Field::Output, error.as_str().into()));
            }
            fields
        }
        Block::Summary { text } => vec![(Field::Text, text.as_str().into())],
        Block::Compaction {
            source_position,
            covered_items,
            summary,
        } => {
            let label = format!(
                "[compaction] {}",
                blocks::compaction_label(*source_position, *covered_items)
            );
            vec![
                (Field::Text, label.into()),
                (Field::Text, summary.as_str().into()),
            ]
        }
        Block::TurnEnd { .. } | Block::EarlierHistory { .. } => Vec::new(),
    }
}

/// Every case-insensitive literal occurrence in visible blocks, in transcript
/// order. Thinking is searched only while it is shown.
pub fn find_matches(transcript: &Transcript, query: &str, show_reasoning: bool) -> Vec<FindMatch> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return Vec::new();
    }
    let mut found = Vec::new();
    for (block, content) in transcript.blocks().iter().enumerate() {
        if transcript.absorbed(block) {
            continue;
        }
        let mut ordinal = 0;
        for (field, text) in searchable(content, show_reasoning) {
            for _ in text.to_lowercase().matches(&needle) {
                found.push(FindMatch {
                    block,
                    field,
                    ordinal,
                });
                ordinal += 1;
            }
        }
    }
    found
}

/// The row of `lines` that holds the `ordinal`-th occurrence of the query, or
/// the last row with one when the rows show fewer.
fn match_row(lines: &[Line<'_>], query: &str, ordinal: usize) -> Option<usize> {
    let needle = query.trim().to_lowercase();
    let mut seen = 0;
    let mut last = None;
    for (row, line) in lines.iter().enumerate() {
        let count = line.to_string().to_lowercase().matches(&needle).count();
        if count > 0 {
            if ordinal < seen + count {
                return Some(row);
            }
            seen += count;
            last = Some(row);
        }
    }
    last
}

/// `line` with every occurrence of the query drawn in `style`, or `None` when it
/// has none; matches are found on the rendered text, so formatting inside a word hides it.
pub fn mark(line: &Line<'static>, query: &str, style: Style) -> Option<Line<'static>> {
    let lower = |ch: char| ch.to_lowercase().next().unwrap_or(ch);
    let needle: Vec<char> = query.trim().chars().map(lower).collect();
    let chars: Vec<(char, usize)> = line
        .spans
        .iter()
        .enumerate()
        .flat_map(|(span, content)| content.content.chars().map(move |ch| (ch, span)))
        .collect();
    let folded: Vec<char> = chars.iter().map(|(ch, _)| lower(*ch)).collect();
    let mut marked = vec![false; chars.len()];
    let mut at = 0;
    while !needle.is_empty() && at + needle.len() <= folded.len() {
        if folded[at..].starts_with(&needle) {
            marked[at..at + needle.len()].fill(true);
            at += needle.len();
        } else {
            at += 1;
        }
    }
    if !marked.contains(&true) {
        return None;
    }
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut run = String::new();
    let mut run_key: Option<(usize, bool)> = None;
    for ((ch, span), on) in chars.into_iter().zip(marked) {
        if run_key.is_some_and(|key| key != (span, on)) {
            let (span, on) = run_key.take().unwrap_or_default();
            spans.push(piece(
                &line.spans[span],
                std::mem::take(&mut run),
                on,
                style,
            ));
        }
        run_key = Some((span, on));
        run.push(ch);
    }
    if let Some((span, on)) = run_key {
        spans.push(piece(&line.spans[span], run, on, style));
    }
    Some(Line {
        spans,
        ..line.clone()
    })
}

fn piece(span: &Span<'static>, text: String, marked: bool, style: Style) -> Span<'static> {
    let own = span.style;
    Span::styled(text, if marked { own.patch(style) } else { own })
}

impl App {
    /// Puts the find bar's current match in view: its block opens if the match
    /// is in folded text, and the view lands on the row that holds it.
    pub(super) fn show_match(&mut self) {
        let Overlay::Find {
            query,
            matches,
            current,
        } = &self.overlay
        else {
            return;
        };
        let Some(found) = matches.get(*current).copied() else {
            return;
        };
        let query = query.clone();
        self.reveal(found);
        let Some(session) = self.resident.current() else {
            return;
        };
        let Some(block) = session.transcript.blocks().get(found.block) else {
            return;
        };
        let grouping = session.transcript.grouping(found.block);
        let width = self.layout.viewport.width.max(blocks::INSET as u16 + 1);
        let fold = Fold {
            expanded: self.tools_expanded,
            show_reasoning: self.show_reasoning,
        };
        let lines = blocks::render_with(block, grouping, self.styles, width, fold);
        // A block sits behind a separator row, or behind the welcome header.
        let lead = match found.block {
            0 => self.layout.state.header_rows,
            _ => 1,
        };
        let row = match_row(&lines, &query, found.ordinal).map_or(0, |row| lead + row);
        self.held = false;
        self.scroll = Scroll::Anchor {
            block: found.block,
            offset: row.saturating_sub(2).try_into().unwrap_or(u16::MAX),
        };
    }

    /// Opens any closed work group and folded thinking or tool output a match sits in.
    fn reveal(&mut self, found: FindMatch) {
        let all = self.tools_expanded;
        let Some(session) = self.resident.current_mut() else {
            return;
        };
        let needs_group = match session.transcript.grouping(found.block) {
            crate::transcript::Grouping::Tail { .. } => found.field == Field::Thinking,
            _ => true,
        };
        if needs_group {
            session.transcript.open_group(found.block);
        }
        let folded = match (found.field, session.transcript.blocks().get(found.block)) {
            (Field::Thinking, Some(Block::Assistant { expanded, .. })) => !expanded.unwrap_or(all),
            (Field::Output, Some(Block::Tool(call))) => !call.is_expanded(all),
            (
                Field::Text,
                Some(Block::User {
                    kind: UserKind::Delegated,
                    expanded,
                    ..
                }),
            ) => !expanded.unwrap_or(all),
            _ => false,
        };
        if folded {
            match session.transcript.block_mut(found.block) {
                Some(Block::Assistant { expanded, .. }) | Some(Block::User { expanded, .. }) => {
                    *expanded = Some(true)
                }
                Some(Block::Tool(call)) => call.expanded = Some(true),
                _ => unreachable!("a folded match has a thinking, delegated or tool block"),
            }
        }
    }
}

/// The next or previous match, wrapping at either end.
pub fn step_match(current: usize, total: usize, forward: bool) -> usize {
    if total == 0 {
        0
    } else if forward {
        (current + 1) % total
    } else {
        (current + total - 1) % total
    }
}

#[cfg(test)]
mod tests {
    use ratatui::style::Color;

    use super::*;
    use crate::transcript::UserKind;

    fn user(text: &str) -> Block {
        Block::User {
            text: text.into(),
            kind: UserKind::Prompt,
            at: None,
            expanded: None,
        }
    }

    fn transcript(blocks: impl IntoIterator<Item = Block>) -> Transcript {
        let mut out = Transcript::default();
        for block in blocks {
            out.push(block);
        }
        out
    }

    fn assistant(reasoning: &str, text: &str) -> Block {
        Block::Assistant {
            key: crate::transcript::RequestKey::response(
                "00000000000000000001".parse().unwrap(),
                0,
            ),
            reasoning: reasoning.into(),
            text: text.into(),
            streaming: false,
            partial: false,
            expanded: None,
        }
    }

    fn tool(detail: &str, output: &[&str]) -> Block {
        let mut call = crate::transcript::ToolCall::new(
            minicore_host_protocol::ToolCallRef {
                turn_id: "00000000000000000001".parse().unwrap(),
                tool_call_id: minicore_host_protocol::ToolCallId("c".into()),
            },
            "bash",
        );
        call.detail = detail.into();
        call.output = output.iter().map(|line| (*line).to_owned()).collect();
        Block::Tool(Box::new(call))
    }

    #[test]
    fn every_occurrence_counts_not_just_every_block() {
        let transcript = transcript([
            user("Fix the parser"),
            assistant("", "The parser parser now works"),
            user("next"),
        ]);
        let matches = find_matches(&transcript, "PARSER", true);
        let places: Vec<(usize, usize)> = matches.iter().map(|m| (m.block, m.ordinal)).collect();
        assert_eq!(places, [(0, 0), (1, 0), (1, 1)]);
        assert!(find_matches(&transcript, "   ", true).is_empty());
    }

    #[test]
    fn thinking_and_tool_output_are_searched() {
        let transcript = transcript([
            assistant("weighing the cache option", "done"),
            tool("$ cargo test", &["running 3 tests", "test cache ... ok"]),
        ]);
        let found = find_matches(&transcript, "cache", true);
        assert_eq!(
            found.iter().map(|m| (m.block, m.field)).collect::<Vec<_>>(),
            [(0, Field::Thinking), (1, Field::Output)]
        );
        assert_eq!(
            find_matches(&transcript, "cargo", true)[0].field,
            Field::Detail
        );
        assert_eq!(
            find_matches(&transcript, "cache", false).len(),
            1,
            "hidden thinking is not searched"
        );
    }

    #[test]
    fn a_compaction_marker_is_found_by_its_label_and_its_summary() {
        let marker = |summary: &str| {
            transcript([Block::Compaction {
                source_position: 4,
                covered_items: 3,
                summary: summary.into(),
            }])
        };
        assert_eq!(find_matches(&marker(""), "compaction", true).len(), 1);
        assert_eq!(
            find_matches(&marker("kept the plan"), "the plan", true).len(),
            1
        );
    }

    #[test]
    fn find_searches_agent_card_fields_and_skips_absorbed_blocks() {
        use crate::transcript::agent_card::AgentCard;
        use minicore_host_protocol::{DelegationAssociation, SessionRef, ToolCallRef};

        let origin = ToolCallRef {
            turn_id: "00000000000000000001".parse().unwrap(),
            tool_call_id: "start".to_owned().into(),
        };
        let parent = SessionRef {
            agent_id: "00000000000000000002".parse().unwrap(),
            session_id: "00000000000000000003".parse().unwrap(),
        };
        let child = SessionRef {
            agent_id: parent.agent_id.clone(),
            session_id: "00000000000000000004".parse().unwrap(),
        };
        let task_message_id: minicore_host_protocol::MessageId =
            "00000000000000000005".parse().unwrap();
        let association = DelegationAssociation {
            parent,
            origin: origin.clone(),
            child,
            accepted_message_ids: vec![task_message_id.clone()],
            task_message_id,
            inputs: Vec::new(),
            completion: None,
        };
        let mut card = AgentCard::new("00000000000000000006".parse().unwrap(), association);
        card.agent_name = Some("tokyo-agent".into());
        card.task = "Inspect weather".into();
        let mut call = crate::transcript::ToolCall::new(origin, "agent");
        call.agent_card = Some(Box::new(card));
        let result = Block::User {
            text: r#"{"source":{"agent_id":"00000000000000000002","session_id":"00000000000000000004"},"task_message_id":"00000000000000000005","status":"ended","results":[{"outcome":"completed","output":["Sunny 18C in Shibuya"]}]}"#.into(),
            kind: UserKind::Delegated,
            at: None,
            expanded: None,
        };
        let mut transcript = transcript([Block::Tool(Box::new(call)), result]);
        transcript.link_delegations();
        assert!(
            transcript.absorbed(1),
            "delivered result stays hidden in main transcript and links to its task card"
        );
        let task_hits = find_matches(&transcript, "weather", true);
        assert_eq!(task_hits.len(), 1);
        assert_eq!((task_hits[0].block, task_hits[0].field), (0, Field::Detail));
        let result_hits = find_matches(&transcript, "Shibuya", true);
        assert_eq!(
            result_hits.len(),
            1,
            "delivered result is not double-counted across banner and task record"
        );
        assert_eq!(
            (result_hits[0].block, result_hits[0].field),
            (0, Field::Output)
        );
    }

    #[test]
    fn the_row_is_the_one_holding_the_nth_occurrence() {
        let lines: Vec<Line> = ["intro", "cache and cache", "none", "last cache"]
            .into_iter()
            .map(Line::from)
            .collect();
        assert_eq!(match_row(&lines, "cache", 0), Some(1));
        assert_eq!(match_row(&lines, "cache", 1), Some(1));
        assert_eq!(match_row(&lines, "cache", 2), Some(3));
        assert_eq!(
            match_row(&lines, "cache", 9),
            Some(3),
            "clamped to the last"
        );
        assert_eq!(match_row(&lines, "absent", 0), None);
    }

    #[test]
    fn marking_styles_each_occurrence_across_spans_and_keeps_the_text() {
        let style = Style::new().bg(Color::Yellow);
        let line = Line::from(vec![
            Span::styled("a Pa", Style::new().fg(Color::Red)),
            Span::raw("rser b parser"),
        ]);
        let marked = mark(&line, "parser", style).expect("a match");
        assert_eq!(marked.to_string(), line.to_string());
        let on: String = marked
            .spans
            .iter()
            .filter(|span| span.style.bg == Some(Color::Yellow))
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(on, "Parserparser");
        assert!(
            marked.spans[1].style.fg == Some(Color::Red),
            "a marked piece keeps its own colour: {:?}",
            marked.spans
        );
        assert_eq!(mark(&line, "zzz", style), None);
        assert_eq!(mark(&line, "", style), None);
    }

    #[test]
    fn steps_wrap_in_both_directions() {
        assert_eq!(step_match(0, 3, true), 1);
        assert_eq!(step_match(2, 3, true), 0);
        assert_eq!(step_match(0, 3, false), 2);
        assert_eq!(step_match(0, 0, true), 0);
    }
}
