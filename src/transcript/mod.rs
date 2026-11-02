//! The transcript: block data model plus its layout cache.

pub mod block;
mod history;
pub mod layout;
mod tool_call;

use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::protocol::ToolRef;

pub use block::{Block, ChangeSummary, RequestKey, Saving};
pub use history::{PreservedKey, merge_live};

pub use history::{append_history_item, push_interrupted, to_markdown};
pub use tool_call::{EXPANDED_ROWS, ToolCall, ToolState};

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
    metadata: Vec<BlockMeta>,
    /// Keys of blocks a reconcile kept although the canonical body omits them.
    preserved: Vec<PreservedKey>,
}

#[derive(Clone, Debug)]
struct BlockMeta {
    revision: u64,
    origin: Origin,
}

impl BlockMeta {
    fn new(origin: Origin) -> Self {
        Self {
            revision: next_revision(),
            origin,
        }
    }
}

#[derive(Clone, Debug)]
pub struct UserIdentity {
    pub loop_id: String,
    pub input_id: Option<String>,
    pub request_index: Option<u32>,
}

#[derive(Clone, Debug, Default)]
struct Origin {
    user: Option<UserIdentity>,
    history: Option<HistorySource>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct HistorySource {
    pub item: usize,
    part: usize,
}

pub struct MergedRegion {
    pub blocks: Vec<Block>,
    preserved: Vec<PreservedKey>,
    origins: Vec<Origin>,
}

impl Transcript {
    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }

    pub fn revisions(&self) -> impl ExactSizeIterator<Item = u64> + DoubleEndedIterator + Clone {
        self.metadata.iter().map(|meta| meta.revision)
    }

    fn bump(&mut self, index: usize) {
        if let Some(meta) = self.metadata.get_mut(index) {
            meta.revision = next_revision();
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
        self.metadata.push(BlockMeta::new(Origin::default()));
        self.blocks.len() - 1
    }

    pub fn insert(&mut self, index: usize, block: Block) {
        let index = index.min(self.blocks.len());
        self.blocks.insert(index, block);
        self.metadata
            .insert(index, BlockMeta::new(Origin::default()));
    }

    pub fn clear(&mut self) {
        self.blocks.clear();
        self.metadata.clear();
        self.preserved.clear();
    }

    pub fn remove(&mut self, index: usize) {
        if index < self.blocks.len() {
            self.preserved
                .retain(|key| !key.belongs_to(&self.blocks[index]));
            self.blocks.remove(index);
            self.metadata.remove(index);
        }
    }

    /// Replaces `range` with `blocks`.
    pub fn splice(&mut self, range: Range<usize>, blocks: Vec<Block>) {
        self.preserved.retain(|key| {
            !self.blocks[range.clone()]
                .iter()
                .any(|block| key.belongs_to(block))
        });
        let metadata = (0..blocks.len()).map(|_| BlockMeta::new(Origin::default()));
        self.metadata.splice(range.clone(), metadata);
        self.blocks.splice(range, blocks);
    }

    pub fn remove_front(&mut self, count: usize) {
        let count = count.min(self.blocks.len());
        self.preserved.retain(|key| {
            !self.blocks[..count]
                .iter()
                .any(|block| key.belongs_to(block))
        });
        self.blocks.drain(..count);
        self.metadata.drain(..count);
    }

    pub fn splice_copy(&mut self, range: Range<usize>, source: &Self, copied: Range<usize>) {
        let start = range.start;
        self.splice(range, source.blocks[copied.clone()].to_vec());
        for (target, source) in self.metadata[start..]
            .iter_mut()
            .zip(&source.metadata[copied.clone()])
        {
            target.origin.clone_from(&source.origin);
        }
        self.mark_preserved(source.preserved_in(copied));
    }

    pub fn splice_transcript(&mut self, range: Range<usize>, source: Self) {
        self.splice_merged(
            range,
            MergedRegion {
                blocks: source.blocks,
                preserved: source.preserved,
                origins: source
                    .metadata
                    .into_iter()
                    .map(|meta| meta.origin)
                    .collect(),
            },
        );
    }

    pub fn splice_merged(&mut self, range: Range<usize>, source: MergedRegion) {
        let start = range.start;
        self.splice(range, source.blocks);
        for (target, origin) in self.metadata[start..].iter_mut().zip(source.origins) {
            target.origin = origin;
        }
        self.mark_preserved(source.preserved);
    }

    pub fn merged(mut durable: Self, live: &Self, range: Range<usize>) -> MergedRegion {
        durable.keep_delivered_rows(live, range.clone());
        let (blocks, mut preserved, sources) =
            merge_live(durable.blocks, &live.blocks[range.clone()]);
        preserved.extend(durable.preserved);
        let mut summaries = std::collections::BTreeSet::new();
        let (blocks, origins) = blocks
            .into_iter()
            .zip(sources)
            .filter_map(|(block, source)| {
                let origin = if source < durable.metadata.len() {
                    durable.metadata[source].origin.clone()
                } else {
                    live.metadata[range.start + source - durable.metadata.len()]
                        .origin
                        .clone()
                };
                if matches!(block, Block::Compaction { .. })
                    && origin
                        .history
                        .is_some_and(|source| !summaries.insert(source))
                {
                    return None;
                }
                Some((block, origin))
            })
            .unzip();
        MergedRegion {
            blocks,
            preserved,
            origins,
        }
    }

    pub fn history_source(&self, index: usize) -> Option<HistorySource> {
        self.metadata
            .get(index)
            .and_then(|meta| meta.origin.history)
    }

    pub fn find_history_source(&self, source: HistorySource) -> Option<usize> {
        self.metadata
            .iter()
            .position(|meta| meta.origin.history == Some(source))
    }

    fn keep_delivered_rows(&mut self, live: &Self, range: Range<usize>) {
        enum Source {
            Canonical(usize),
            Delivered(usize),
        }
        let steering = |block: &Block| {
            matches!(
                block,
                Block::User {
                    kind: crate::protocol::read::UserKind::Steering,
                    ..
                }
            )
        };
        let known: Vec<usize> = range
            .filter(|index| {
                steering(&live.blocks[*index])
                    && live
                        .user_identity(*index)
                        .is_some_and(|user| user.input_id.is_some() && user.request_index.is_some())
            })
            .collect();
        if known.is_empty()
            || known.len() != self.blocks.iter().filter(|block| steering(block)).count()
        {
            return;
        }
        let mut plan: Vec<Source> = self
            .blocks
            .iter()
            .enumerate()
            .filter(|(_, block)| !steering(block))
            .map(|(index, _)| Source::Canonical(index))
            .collect();
        for index in known {
            let identity = live.user_identity(index).unwrap();
            let request = identity.request_index.unwrap();
            let at = plan
                .iter()
                .position(|source| match source {
                    Source::Canonical(at) => match &self.blocks[*at] {
                        Block::Assistant { key, .. } => {
                            key.loop_id == identity.loop_id && key.request_index >= request
                        }
                        Block::Tool(call) => {
                            call.key.loop_id == identity.loop_id
                                && call.key.request_index >= request
                        }
                        Block::TurnEnd { loop_id, .. } => *loop_id == identity.loop_id,
                        _ => false,
                    },
                    Source::Delivered(at) => live.user_identity(*at).is_some_and(|user| {
                        user.loop_id == identity.loop_id
                            && user
                                .request_index
                                .is_some_and(|boundary| boundary > request)
                    }),
                })
                .unwrap_or(plan.len());
            plan.insert(at, Source::Delivered(index));
        }
        // Preserve identified source rows only when their authoritative boundaries
        // reproduce the entire verified canonical layout and content exactly.
        let equivalent = plan.iter().enumerate().all(|(at, source)| match source {
            Source::Canonical(index) => at == *index,
            Source::Delivered(index) => match (&self.blocks[at], &live.blocks[*index]) {
                (
                    Block::User {
                        text: canonical,
                        kind: canonical_kind,
                        ..
                    },
                    Block::User {
                        text: delivered,
                        kind: delivered_kind,
                        ..
                    },
                ) => {
                    canonical == delivered
                        && canonical_kind == delivered_kind
                        && self.user_identity(at).is_some_and(|user| {
                            user.loop_id == live.user_identity(*index).unwrap().loop_id
                        })
                }
                _ => false,
            },
        });
        if !equivalent {
            return;
        }
        for (at, source) in plan.into_iter().enumerate() {
            if let Source::Delivered(index) = source {
                self.blocks[at] = live.blocks[index].clone();
                self.metadata[at].origin.user = live.metadata[index].origin.user.clone();
            }
        }
    }

    pub fn identify_user(&mut self, index: usize, loop_id: &str, input_id: Option<&str>) {
        assert!(matches!(self.blocks[index], Block::User { .. }));
        self.metadata[index].origin.user = Some(UserIdentity {
            loop_id: loop_id.to_owned(),
            input_id: input_id.map(str::to_owned),
            request_index: None,
        });
    }

    pub fn identify_steer(
        &mut self,
        index: usize,
        loop_id: &str,
        input_id: &str,
        request_index: u32,
    ) {
        self.identify_user(index, loop_id, Some(input_id));
        self.metadata[index]
            .origin
            .user
            .as_mut()
            .unwrap()
            .request_index = Some(request_index);
    }

    pub fn user_identity(&self, index: usize) -> Option<&UserIdentity> {
        self.metadata[index].origin.user.as_ref()
    }

    /// Marks live-only content without clearing another region's provenance.
    pub fn mark_preserved(&mut self, keys: impl IntoIterator<Item = PreservedKey>) {
        for key in keys {
            if !self.preserved.contains(&key) {
                self.preserved.push(key);
            }
        }
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

    /// Whether a reconcile preserved the whole block because the canonical body lacks it.
    pub fn is_preserved(&self, index: usize) -> bool {
        self.blocks
            .get(index)
            .is_some_and(|block| self.preserved.iter().any(|key| key.matches_block(block)))
    }

    pub fn is_preserved_channel(&self, key: &RequestKey, reasoning: bool) -> bool {
        self.preserved.iter().any(|marker| match marker {
            PreservedKey::AssistantReasoning(known) => reasoning && known == key,
            PreservedKey::AssistantText(known) => !reasoning && known == key,
            _ => false,
        })
    }

    pub fn preserved_in(&self, range: Range<usize>) -> Vec<PreservedKey> {
        self.preserved
            .iter()
            .filter(|key| {
                self.blocks[range.clone()]
                    .iter()
                    .any(|block| key.belongs_to(block))
            })
            .cloned()
            .collect()
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
                self.push(Block::Assistant {
                    key: key.clone(),
                    reasoning: String::new(),
                    text: String::new(),
                    streaming: true,
                    expanded: None,
                });
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
    fn reconciling_another_region_does_not_relabel_temporary_content_as_canonical() {
        let mut transcript = Transcript::default();
        let first = RequestKey {
            loop_id: "A".into(),
            request_index: 0,
        };
        let second = RequestKey {
            loop_id: "B".into(),
            request_index: 0,
        };
        transcript.assistant_mut(&first);
        transcript.assistant_mut(&second);
        transcript.mark_preserved([PreservedKey::Assistant(first)]);
        transcript.mark_preserved([PreservedKey::Assistant(second)]);
        assert!(transcript.is_preserved(0));
        assert!(transcript.is_preserved(1));
        let canonical = transcript.blocks()[0].clone();
        transcript.splice(0..1, vec![canonical]);
        assert!(!transcript.is_preserved(0));
        assert!(transcript.is_preserved(1));
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
            at: None,
        });
        assert_eq!(transcript.last_answer(), None);
    }

    #[test]
    fn two_transcripts_never_share_a_revision() {
        let user = || Block::User {
            text: "a".into(),
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
