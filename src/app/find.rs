//! Find in the transcript.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::{App, Overlay, Scroll};
use crate::transcript::Block;
use crate::transcript::layout::Fold;
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
fn searchable(block: &Block, show_reasoning: bool) -> Vec<(Field, String)> {
    match block {
        Block::User { text, .. } | Block::Notice { text, .. } => vec![(Field::Text, text.clone())],
        Block::Assistant {
            reasoning, text, ..
        } => {
            let thinking = show_reasoning.then(|| (Field::Thinking, reasoning.clone()));
            thinking
                .into_iter()
                .chain([(Field::Text, text.clone())])
                .collect()
        }
        Block::Tool(call) => vec![
            (Field::Detail, call.detail.clone()),
            (Field::Output, call.output_content().join("\n")),
        ],
        Block::Compaction { summary, .. } => summary
            .iter()
            .map(|summary| (Field::Text, summary.clone()))
            .collect(),
        Block::TurnEnd { .. } | Block::EarlierHistory { .. } => Vec::new(),
    }
}

/// Every case-insensitive literal occurrence, in transcript order. Thinking is
/// searched only while it is shown.
pub fn find_matches(blocks: &[Block], query: &str, show_reasoning: bool) -> Vec<FindMatch> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return Vec::new();
    }
    let mut found = Vec::new();
    for (block, content) in blocks.iter().enumerate() {
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
        let Some(block) = self.transcript_blocks().get(found.block) else {
            return;
        };
        let width = self.layout.viewport.width.max(blocks::INSET as u16 + 1);
        let fold = Fold {
            expanded: self.tools_expanded,
            show_reasoning: self.show_reasoning,
        };
        let lines = blocks::render(block, self.styles, width, fold);
        // A block sits behind a separator row, or behind the welcome header.
        let lead = match found.block {
            0 => self.layout.state.header_rows,
            _ => 1,
        };
        let row = match_row(&lines, &query, found.ordinal).map_or(0, |row| lead + row);
        self.scroll = Scroll::Anchor {
            block: found.block,
            offset: row.saturating_sub(2).try_into().unwrap_or(u16::MAX),
        };
    }

    /// Opens the thinking or tool output a match sits in when it is folded.
    fn reveal(&mut self, found: FindMatch) {
        let all = self.tools_expanded;
        let Some(session) = self.session.as_mut() else {
            return;
        };
        match (found.field, session.transcript.block_mut(found.block)) {
            (Field::Thinking, Some(Block::Assistant { expanded, .. }))
                if !expanded.unwrap_or(all) =>
            {
                *expanded = Some(true);
            }
            (Field::Output, Some(Block::Tool(call))) if !call.is_expanded(all) => {
                call.expanded = Some(true);
            }
            _ => {}
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
    use crate::protocol::read::UserKind;

    fn user(text: &str) -> Block {
        Block::User {
            text: text.into(),
            kind: UserKind::Prompt,
        }
    }

    fn assistant(reasoning: &str, text: &str) -> Block {
        Block::Assistant {
            key: crate::transcript::RequestKey {
                loop_id: "l".into(),
                request_index: 0,
            },
            reasoning: reasoning.into(),
            text: text.into(),
            streaming: false,
            expanded: None,
        }
    }

    fn tool(detail: &str, output: &[&str]) -> Block {
        let mut call = crate::transcript::ToolCall::new(
            crate::protocol::ToolRef {
                session_id: "s".into(),
                loop_id: "l".into(),
                request_index: 0,
                tool_call_id: "c".into(),
            },
            "bash",
        );
        call.detail = detail.into();
        call.output = output.iter().map(|line| (*line).to_owned()).collect();
        Block::Tool(Box::new(call))
    }

    #[test]
    fn every_occurrence_counts_not_just_every_block() {
        let blocks = [
            user("Fix the parser"),
            assistant("", "The parser parser now works"),
            user("next"),
        ];
        let matches = find_matches(&blocks, "PARSER", true);
        let places: Vec<(usize, usize)> = matches.iter().map(|m| (m.block, m.ordinal)).collect();
        assert_eq!(places, [(0, 0), (1, 0), (1, 1)]);
        assert!(find_matches(&blocks, "   ", true).is_empty());
    }

    #[test]
    fn thinking_and_tool_output_are_searched() {
        let blocks = [
            assistant("weighing the cache option", "done"),
            tool("$ cargo test", &["running 3 tests", "test cache ... ok"]),
        ];
        let found = find_matches(&blocks, "cache", true);
        assert_eq!(
            found.iter().map(|m| (m.block, m.field)).collect::<Vec<_>>(),
            [(0, Field::Thinking), (1, Field::Output)]
        );
        assert_eq!(find_matches(&blocks, "cargo", true)[0].field, Field::Detail);
        assert_eq!(
            find_matches(&blocks, "cache", false).len(),
            1,
            "hidden thinking is not searched"
        );
    }

    #[test]
    fn notices_and_summaries_are_searchable() {
        let notice = Block::Notice {
            level: crate::transcript::NoticeLevel::Error,
            text: "compaction failed".into(),
        };
        assert_eq!(find_matches(&[notice], "compaction", true).len(), 1);
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
