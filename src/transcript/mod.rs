//! Presentation blocks and the per-block layout cache.

pub mod block;
mod history;
pub mod layout;
mod tool_call;

use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};

use minicore_host_protocol::{MessageId, Timestamp, ToolCallRef, TurnId};

pub use block::{Block, ChangeSummary, RequestKey, UserKind};
pub use history::{append_history_record, to_markdown, write_markdown};
pub use tool_call::{EXPANDED_ROWS, ToolCall, ToolKey, ToolState};

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

    fn bump(&mut self, index: usize) {
        self.metadata[index].revision = next_revision();
    }

    pub fn push(&mut self, block: Block) -> usize {
        self.blocks.push(block);
        self.metadata.push(BlockMeta::default());
        self.blocks.len() - 1
    }

    pub fn insert(&mut self, index: usize, block: Block) {
        self.blocks.insert(index, block);
        self.metadata.insert(index, BlockMeta::default());
    }

    pub fn clear(&mut self) {
        self.blocks.clear();
        self.metadata.clear();
    }

    pub fn remove(&mut self, index: usize) {
        self.blocks.remove(index);
        self.metadata.remove(index);
    }

    pub fn splice(&mut self, range: Range<usize>, blocks: Vec<Block>) {
        let metadata = (0..blocks.len()).map(|_| BlockMeta::default());
        self.metadata.splice(range.clone(), metadata);
        self.blocks.splice(range, blocks);
    }

    pub fn remove_front(&mut self, count: usize) {
        self.blocks.drain(..count);
        self.metadata.drain(..count);
    }

    pub fn splice_copy(&mut self, range: Range<usize>, source: &Self, copied: Range<usize>) {
        self.blocks
            .splice(range.clone(), source.blocks[copied.clone()].iter().cloned());
        self.metadata
            .splice(range, source.metadata[copied].iter().cloned());
    }

    pub fn splice_transcript(&mut self, range: Range<usize>, source: Self) {
        self.blocks.splice(range.clone(), source.blocks);
        self.metadata.splice(range, source.metadata);
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

    pub fn contains_history_position(&self, position: u64) -> bool {
        self.metadata.iter().any(|meta| {
            meta.history
                .iter()
                .any(|source| source.position == position)
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
        let mut order: Vec<_> = (0..self.blocks.len()).collect();
        order.sort_by_key(|&at| {
            self.metadata[at]
                .history
                .first()
                .map(|source| (source.position, source.part))
                .unwrap_or((u64::MAX, at))
        });
        if order.iter().enumerate().all(|(at, &old)| at == old) {
            return;
        }
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
