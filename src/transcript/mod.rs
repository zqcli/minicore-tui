//! The transcript: block data model plus its layout cache.

pub mod block;
mod history;
pub mod layout;
mod tool_call;

use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::protocol::ToolRef;

pub use block::{Block, ChangeSummary, NoticeLevel, RequestKey};
pub use history::{append_history, append_history_item, merge_live, to_markdown};
pub use tool_call::{TRUNCATED_MARKER, ToolCall, ToolState};

/// Revisions come from one counter, so no two blocks of any session share one and
/// the layout cache can follow a block that moved.
fn next_revision() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Ordered blocks, each with a revision that changes on every mutation and keys the layout cache.
#[derive(Clone, Debug, Default)]
pub struct Transcript {
    blocks: Vec<Block>,
    revisions: Vec<u64>,
}

impl Transcript {
    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }

    pub fn revisions(&self) -> &[u64] {
        &self.revisions
    }

    fn bump(&mut self, index: usize) {
        if let Some(revision) = self.revisions.get_mut(index) {
            *revision = next_revision();
        }
    }

    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    pub fn push(&mut self, block: Block) -> usize {
        self.blocks.push(block);
        self.revisions.push(next_revision());
        self.blocks.len() - 1
    }

    pub fn insert(&mut self, index: usize, block: Block) {
        let index = index.min(self.blocks.len());
        self.blocks.insert(index, block);
        self.revisions.insert(index, next_revision());
    }

    pub fn clear(&mut self) {
        self.blocks.clear();
        self.revisions.clear();
    }

    pub fn remove(&mut self, index: usize) {
        if index < self.blocks.len() {
            self.blocks.remove(index);
            self.revisions.remove(index);
        }
    }

    /// Replaces `range` with `blocks`.
    pub fn splice(&mut self, range: Range<usize>, blocks: Vec<Block>) {
        let revisions: Vec<u64> = blocks.iter().map(|_| next_revision()).collect();
        self.blocks.splice(range.clone(), blocks);
        self.revisions.splice(range, revisions);
    }

    pub fn remove_front(&mut self, count: usize) {
        let count = count.min(self.blocks.len());
        self.blocks.drain(..count);
        self.revisions.drain(..count);
    }

    pub fn into_blocks(self) -> Vec<Block> {
        self.blocks
    }

    /// Every tool card, in order.
    pub fn tool_calls(&self) -> impl Iterator<Item = &ToolCall> {
        self.blocks.iter().filter_map(|block| match block {
            Block::Tool(call) => Some(call.as_ref()),
            _ => None,
        })
    }

    // Lookups scan from the end: live blocks sit there, so the cost does not
    // grow with the history.
    pub fn find_tool_by_call_id(&self, tool_call_id: &str) -> Option<&ToolCall> {
        self.blocks.iter().rev().find_map(|block| match block {
            Block::Tool(call) if call.key.tool_call_id == tool_call_id => Some(call.as_ref()),
            _ => None,
        })
    }

    pub fn find_tool_mut(&mut self, key: &ToolRef) -> Option<&mut ToolCall> {
        let index = self
            .blocks
            .iter()
            .rposition(|block| matches!(block, Block::Tool(call) if &call.key == key))?;
        self.bump(index);
        match &mut self.blocks[index] {
            Block::Tool(call) => Some(call.as_mut()),
            _ => None,
        }
    }

    /// One block for an in-place change, marked as changed.
    pub fn block_mut(&mut self, index: usize) -> Option<&mut Block> {
        self.bump(index);
        self.blocks.get_mut(index)
    }

    pub fn find_tool(&self, key: &ToolRef) -> Option<&ToolCall> {
        self.blocks.iter().rev().find_map(|block| match block {
            Block::Tool(call) if &call.key == key => Some(call.as_ref()),
            _ => None,
        })
    }

    /// The assistant block for one request, created if the stream is late.
    pub fn assistant_mut(&mut self, key: &RequestKey) -> &mut Block {
        let index = match self.blocks.iter().rposition(
            |block| matches!(block, Block::Assistant { key: existing, .. } if existing == key),
        ) {
            Some(index) => index,
            None => {
                self.blocks.push(Block::Assistant {
                    key: key.clone(),
                    reasoning: String::new(),
                    text: String::new(),
                    streaming: true,
                    expanded: None,
                });
                self.revisions.push(next_revision());
                self.blocks.len() - 1
            }
        };
        self.bump(index);
        &mut self.blocks[index]
    }

    /// The last assistant answer text, for `/copy`.
    pub fn last_answer(&self) -> Option<&str> {
        self.blocks.iter().rev().find_map(|block| match block {
            Block::Assistant { text, .. } if !text.is_empty() => Some(text.as_str()),
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::read::UserKind;

    #[test]
    fn assistant_blocks_are_created_once_per_request() {
        let mut transcript = Transcript::default();
        let request = RequestKey {
            loop_id: "l".into(),
            request_index: 2,
        };
        transcript.assistant_mut(&request);
        transcript.assistant_mut(&request);
        assert_eq!(transcript.len(), 1);
    }

    #[test]
    fn last_answer_ignores_reasoning_only_blocks() {
        let mut transcript = Transcript::default();
        transcript.push(Block::Assistant {
            key: RequestKey {
                loop_id: "l".into(),
                request_index: 0,
            },
            reasoning: "thinking".into(),
            text: String::new(),
            streaming: false,
            expanded: None,
        });
        transcript.push(Block::User {
            text: "again".into(),
            kind: UserKind::Prompt,
        });
        assert_eq!(transcript.last_answer(), None);
    }

    #[test]
    fn two_transcripts_never_share_a_revision() {
        let user = || Block::User {
            text: "a".into(),
            kind: UserKind::Prompt,
        };
        let mut first = Transcript::default();
        let mut second = Transcript::default();
        first.push(user());
        second.push(user());
        assert_ne!(first.revisions(), second.revisions());
        first.block_mut(0);
        assert_ne!(first.revisions(), second.revisions());
    }
}
