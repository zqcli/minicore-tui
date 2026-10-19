//! Find in the transcript.

use crate::transcript::Block;

/// One match: the block it is in and the byte range of the query in its text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FindMatch {
    pub block: usize,
    pub start: usize,
    pub len: usize,
}

fn searchable(block: &Block) -> Option<&str> {
    match block {
        Block::User { text, .. } | Block::Assistant { text, .. } | Block::Notice { text, .. } => {
            Some(text)
        }
        Block::Tool(call) => Some(&call.detail),
        Block::Compaction { summary, .. } => summary.as_deref(),
        Block::TurnEnd { .. } | Block::EarlierHistory { .. } => None,
    }
}

/// Case-insensitive literal matches, at most one per block so stepping moves
/// through the conversation instead of through one long answer.
pub fn find_matches(blocks: &[Block], query: &str) -> Vec<FindMatch> {
    let query = query.trim();
    if query.is_empty() {
        return Vec::new();
    }
    let needle = query.to_lowercase();
    blocks
        .iter()
        .enumerate()
        .filter_map(|(block, content)| {
            let start = searchable(content)?.to_lowercase().find(&needle)?;
            Some(FindMatch {
                block,
                start,
                len: query.len(),
            })
        })
        .collect()
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
    use super::*;
    use crate::protocol::read::UserKind;

    fn user(text: &str) -> Block {
        Block::User {
            text: text.into(),
            kind: UserKind::Prompt,
        }
    }

    fn assistant(text: &str) -> Block {
        Block::Assistant {
            key: crate::transcript::RequestKey {
                loop_id: "l".into(),
                request_index: 0,
            },
            reasoning: String::new(),
            text: text.into(),
            streaming: false,
        }
    }

    #[test]
    fn matches_are_case_insensitive_and_one_per_block() {
        let blocks = [
            user("Fix the parser"),
            assistant("The parser parser now works"),
            user("next"),
        ];
        let matches = find_matches(&blocks, "PARSER");
        assert_eq!(
            matches,
            [
                FindMatch {
                    block: 0,
                    start: 8,
                    len: 6
                },
                FindMatch {
                    block: 1,
                    start: 4,
                    len: 6
                }
            ]
        );
        assert!(find_matches(&blocks, "   ").is_empty());
    }

    #[test]
    fn notices_and_tool_details_are_searchable() {
        let mut call = crate::transcript::ToolCall::new(
            crate::protocol::ToolRef {
                session_id: "s".into(),
                loop_id: "l".into(),
                request_index: 0,
                tool_call_id: "c".into(),
            },
            "bash",
        );
        call.detail = "$ cargo test".into();
        let blocks = [
            Block::Notice {
                level: crate::transcript::NoticeLevel::Error,
                text: "compaction failed".into(),
            },
            Block::Tool(Box::new(call)),
        ];
        assert_eq!(find_matches(&blocks, "compaction").len(), 1);
        assert_eq!(find_matches(&blocks, "cargo").len(), 1);
    }

    #[test]
    fn steps_wrap_in_both_directions() {
        assert_eq!(step_match(0, 3, true), 1);
        assert_eq!(step_match(2, 3, true), 0);
        assert_eq!(step_match(0, 3, false), 2);
        assert_eq!(step_match(0, 0, true), 0);
    }
}
