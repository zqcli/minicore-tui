//! Presentation blocks and the per-block layout cache.

pub mod agent_card;
pub mod block;
mod history;
pub mod layout;
mod tool_call;

use std::sync::atomic::{AtomicU64, Ordering};

use minicore_host_protocol::{MessageId, Timestamp, ToolCallRef, TurnId};

pub use block::{Block, ChangeSummary, RequestKey, UserKind};
pub use history::{to_markdown, write_markdown};
pub use tool_call::{EXPANDED_ROWS, ToolCall, ToolKey, ToolState};

/// Wall time between two Host timestamps; none if the clock went backwards.
pub fn elapsed(start: Timestamp, end: Timestamp) -> Option<std::time::Duration> {
    let start = crate::ui::blocks::timestamp(start);
    let end = crate::ui::blocks::timestamp(end);
    std::time::Duration::try_from(end.duration_since(start)).ok()
}

/// The files a Turn's successful file tools changed, with line counts when
/// every change is countable. Arbitrary shell edits are not attributed.
pub fn turn_changes(blocks: &[Block], turn: &TurnId) -> Option<ChangeSummary> {
    let mut files = std::collections::HashSet::new();
    let mut lines = (0, 0);
    let mut counted = true;
    for block in blocks {
        let Block::Tool(call) = block else { continue };
        if call.key.turn_id() != Some(turn)
            || call.state != ToolState::Succeeded
            || !matches!(call.name.as_str(), "write" | "edit" | "apply_patch")
        {
            continue;
        }
        if let Some(path) = call
            .arguments
            .as_ref()
            .and_then(|arguments| arguments["path"].as_str())
        {
            files.insert(path.to_owned());
        }
        match call.line_changes() {
            Some((added, removed)) => {
                lines.0 += added;
                lines.1 += removed;
            }
            None => counted = false,
        }
    }
    (!files.is_empty()).then(|| ChangeSummary {
        files: files.len(),
        lines: counted.then_some(lines),
    })
}

fn next_revision() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

#[derive(Clone, Debug, Default)]
pub struct Transcript {
    blocks: Vec<Block>,
    metadata: Vec<BlockMeta>,
}

#[derive(Clone, Debug)]
struct BlockMeta {
    revision: u64,
    history: Vec<HistorySource>,
    user: Option<UserIdentity>,
}

impl Default for BlockMeta {
    fn default() -> Self {
        Self {
            revision: next_revision(),
            history: Vec::new(),
            user: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistorySource {
    pub position: u64,
    pub recorded_at: Timestamp,
    pub turn_id: Option<TurnId>,
    pub response_index: Option<u64>,
    pub part: usize,
}

#[derive(Clone, Debug)]
pub struct UserIdentity {
    pub turn_id: Option<TurnId>,
    pub message_id: Option<MessageId>,
}

impl Transcript {
    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }

    pub fn revisions(&self) -> impl ExactSizeIterator<Item = u64> + DoubleEndedIterator + Clone {
        self.metadata.iter().map(|meta| meta.revision)
    }

    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    pub fn retain_recent(&mut self, count: usize) -> bool {
        let dropped = self.blocks.len().saturating_sub(count);
        self.blocks.drain(..dropped);
        self.metadata.drain(..dropped);
        dropped > 0
    }

    fn bump(&mut self, index: usize) {
        self.metadata[index].revision = next_revision();
    }

    pub fn push(&mut self, block: Block) -> usize {
        self.blocks.push(block);
        self.metadata.push(BlockMeta::default());
        self.blocks.len() - 1
    }

    #[cfg(test)]
    pub fn insert(&mut self, index: usize, block: Block) {
        self.blocks.insert(index, block);
        self.metadata.insert(index, BlockMeta::default());
    }

    pub fn block_mut(&mut self, index: usize) -> Option<&mut Block> {
        if index >= self.blocks.len() {
            return None;
        }
        self.bump(index);
        self.blocks.get_mut(index)
    }

    pub fn history_source(&self, index: usize) -> Option<&HistorySource> {
        self.metadata.get(index)?.history.first()
    }

    pub fn history_sources(&self, index: usize) -> &[HistorySource] {
        &self.metadata[index].history
    }

    pub fn find_history_source(&self, source: &HistorySource) -> Option<usize> {
        self.metadata.iter().position(|meta| {
            meta.history
                .iter()
                .any(|known| known.position == source.position && known.part == source.part)
        })
    }

    pub fn identify_user(
        &mut self,
        index: usize,
        turn_id: Option<TurnId>,
        message_id: Option<MessageId>,
    ) {
        self.metadata[index].user = Some(UserIdentity {
            turn_id,
            message_id,
        });
    }

    pub fn user_identity(&self, index: usize) -> Option<&UserIdentity> {
        self.metadata[index].user.as_ref()
    }

    pub fn tool_calls(&self) -> impl DoubleEndedIterator<Item = &ToolCall> {
        self.blocks.iter().filter_map(|block| match block {
            Block::Tool(call) => Some(call.as_ref()),
            _ => None,
        })
    }

    pub fn find_tool(&self, key: &ToolCallRef) -> Option<&ToolCall> {
        self.tool_calls()
            .rev()
            .find(|call| call.key.call_ref() == Some(key))
    }

    pub fn find_tool_mut(&mut self, key: &ToolCallRef) -> Option<&mut ToolCall> {
        let at = self.blocks.iter().rposition(
            |block| matches!(block, Block::Tool(call) if call.key.call_ref() == Some(key)),
        )?;
        self.bump(at);
        match &mut self.blocks[at] {
            Block::Tool(call) => Some(call),
            _ => unreachable!(),
        }
    }

    pub fn assistant_mut(&mut self, key: &RequestKey) -> &mut Block {
        let at = self
            .blocks
            .iter()
            .rposition(|block| matches!(block, Block::Assistant { key: known, .. } if known == key))
            .unwrap_or_else(|| {
                self.push(Block::Assistant {
                    key: key.clone(),
                    reasoning: String::new(),
                    text: String::new(),
                    streaming: true,
                    partial: false,
                    expanded: None,
                })
            });
        self.bump(at);
        &mut self.blocks[at]
    }

    pub fn has_canonical_response(&self, key: &RequestKey) -> bool {
        self.blocks
            .iter()
            .zip(&self.metadata)
            .rev()
            .find(|(block, _)| matches!(block, Block::Assistant { key: known, .. } if known == key))
            .is_some_and(|(block, meta)| {
                matches!(
                    block,
                    Block::Assistant {
                        streaming: false,
                        ..
                    }
                ) && !meta.history.is_empty()
            })
    }

    pub fn last_answer(&self) -> Option<&str> {
        self.blocks.iter().rev().find_map(|block| match block {
            Block::Assistant { text, .. } if !text.is_empty() => Some(text.as_str()),
            _ => None,
        })
    }

    fn add_history_source(&mut self, at: usize, source: HistorySource) {
        let sources = &mut self.metadata[at].history;
        if !sources.contains(&source) {
            sources.push(source);
            sources.sort_by_key(|source| (source.position, source.part));
        }
        self.bump(at);
    }

    fn sort_history(&mut self) {
        let key = |at: usize| {
            self.metadata[at]
                .history
                .first()
                .map(|source| (source.position, source.part))
                .unwrap_or((u64::MAX, at))
        };
        if (0..self.blocks.len()).map(key).is_sorted() {
            return;
        }
        let mut order: Vec<_> = (0..self.blocks.len()).collect();
        order.sort_by_key(|&at| key(at));
        let mut blocks: Vec<_> = std::mem::take(&mut self.blocks)
            .into_iter()
            .map(Some)
            .collect();
        let mut metadata: Vec<_> = std::mem::take(&mut self.metadata)
            .into_iter()
            .map(Some)
            .collect();
        for at in order {
            self.blocks.push(blocks[at].take().expect("permutation"));
            self.metadata
                .push(metadata[at].take().expect("permutation"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_turns_changes_count_its_successful_file_tools_only() {
        let turn: TurnId = "00000000000000000001".parse().unwrap();
        let tool = |id: &str, name: &str, arguments: serde_json::Value, state| {
            let mut call = ToolCall::new(
                ToolCallRef {
                    turn_id: turn.clone(),
                    tool_call_id: id.to_owned().into(),
                },
                name,
            );
            call.set_arguments(&arguments);
            call.set_state(state);
            Block::Tool(Box::new(call))
        };
        let blocks = [
            tool(
                "w",
                "write",
                serde_json::json!({"path":"a.rs","content":"1\n2\n"}),
                ToolState::Succeeded,
            ),
            tool(
                "e",
                "edit",
                serde_json::json!({"path":"a.rs","old_text":"1","new_text":"one"}),
                ToolState::Succeeded,
            ),
            tool(
                "f",
                "edit",
                serde_json::json!({"path":"b.rs","old_text":"x","new_text":"y"}),
                ToolState::Failed,
            ),
            tool(
                "r",
                "read",
                serde_json::json!({"path":"c.rs"}),
                ToolState::Succeeded,
            ),
        ];
        assert_eq!(
            turn_changes(&blocks, &turn),
            Some(ChangeSummary {
                files: 1,
                lines: Some((3, 1))
            })
        );
        let other: TurnId = "00000000000000000002".parse().unwrap();
        assert_eq!(turn_changes(&blocks, &other), None);
    }

    #[test]
    fn independent_transcripts_never_share_cache_revisions() {
        let user = || Block::User {
            text: "same".into(),
            kind: UserKind::Prompt,
            at: None,
        };
        let mut first = Transcript::default();
        let mut second = Transcript::default();
        first.push(user());
        second.push(user());
        assert_ne!(first.revisions().next(), second.revisions().next());
        first.block_mut(0);
        assert_ne!(first.revisions().next(), second.revisions().next());
    }
}
