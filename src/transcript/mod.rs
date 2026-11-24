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
    grouping: Grouping,
    /// Drawn inside an Agent card: a later send into its child or its result.
    absorbed: bool,
}

impl Default for BlockMeta {
    fn default() -> Self {
        Self {
            revision: next_revision(),
            history: Vec::new(),
            user: None,
            grouping: Grouping::None,
            absorbed: false,
        }
    }
}

/// How a finished Turn's process blocks fold into one summary row. The answer
/// text and failed tools never join a group, so a fold hides only the steps.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Grouping {
    #[default]
    None,
    /// The Turn's first step row: draws the summary row, then itself when
    /// open or when it does not fold (a failure leading the Turn).
    Head {
        turn: TurnId,
        summary: GroupSummary,
        open: bool,
        folds: bool,
    },
    /// A later step: draws nothing while the group is closed.
    Member { turn: TurnId, open: bool },
}

impl Grouping {
    pub fn is_open(&self) -> bool {
        match self {
            Self::None => true,
            Self::Head { open, .. } | Self::Member { open, .. } => *open,
        }
    }
}

/// `Worked 2m 14s · 18 tools · 3 files changed (+142 −37) · 1 failed`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GroupSummary {
    pub duration: Option<std::time::Duration>,
    pub tools: usize,
    pub failed: usize,
    pub changes: Option<ChangeSummary>,
}

/// Whether a block draws as one-line rows that stack without a blank row:
/// a folded tool row or a thinking-only response.
pub(crate) fn is_row(block: &Block, expanded: bool) -> bool {
    match block {
        // A folded Agent card is one row too, and stacks with the steps.
        Block::Tool(call) => !call.is_delegation_receipt() && !call.is_expanded(expanded),
        Block::Assistant {
            text, reasoning, ..
        } => !expanded && text.trim().is_empty() && !reasoning.trim().is_empty(),
        _ => false,
    }
}

/// A step of a Turn's work, as opposed to its answer or a problem to see.
fn is_step(block: &Block) -> bool {
    match block {
        Block::Tool(call) => {
            call.agent_card.is_none()
                && call.delegations.is_empty()
                && matches!(call.state, ToolState::Succeeded)
                && call.result_error.is_none()
                && call.exit_code.is_none_or(|code| code == 0)
        }
        Block::Assistant {
            text,
            reasoning,
            streaming: false,
            partial: false,
            ..
        } => text.trim().is_empty() && !reasoning.trim().is_empty(),
        _ => false,
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

    /// Whether the block is shown inside an Agent card rather than on its own.
    pub fn absorbed(&self, index: usize) -> bool {
        self.metadata.get(index).is_some_and(|meta| meta.absorbed)
    }

    /// One card per delegation: later sends into a child and the result it
    /// delivers belong to the card of the call that started it, when that card
    /// is loaded. Otherwise they keep their own blocks.
    pub fn link_delegations(&mut self) {
        use std::collections::HashMap;
        let mut by_session: HashMap<String, usize> = HashMap::new();
        // A task message names its card only within the same child Session.
        let mut by_task: HashMap<(String, String), usize> = HashMap::new();
        let mut sends: HashMap<usize, usize> = HashMap::new();
        let mut results: HashMap<usize, String> = HashMap::new();
        let mut absorbed = vec![false; self.blocks.len()];
        for (index, block) in self.blocks.iter().enumerate() {
            match block {
                Block::Tool(call) if call.agent_card.is_some() => {
                    let card = call.agent_card.as_deref().expect("checked");
                    let session = card.key.child.session_id.to_string();
                    by_task.insert(
                        (session.clone(), card.key.task_message_id.to_string()),
                        index,
                    );
                    by_session.insert(session, index);
                }
                Block::Tool(call) if call.is_delegation_receipt() => {
                    let Some(card) = call
                        .delegations
                        .first()
                        .and_then(|first| by_session.get(&first.child.session_id.to_string()))
                        .copied()
                    else {
                        continue;
                    };
                    absorbed[index] = true;
                    *sends.entry(card).or_default() += 1;
                    for association in &call.delegations {
                        by_task.insert(
                            (
                                association.child.session_id.to_string(),
                                association.task_message_id.to_string(),
                            ),
                            card,
                        );
                    }
                }
                Block::User {
                    kind: UserKind::Delegated,
                    text,
                    ..
                } => {
                    let Some(summary) = crate::ui::delegated::summary(text) else {
                        continue;
                    };
                    let card = summary
                        .task_message_id
                        .and_then(|task| by_task.get(&(summary.session_id.clone(), task)))
                        .or_else(|| by_session.get(&summary.session_id));
                    if let Some(&card) = card {
                        absorbed[index] = true;
                        results.insert(card, text.clone());
                    }
                }
                _ => {}
            }
        }
        for (index, absorbed) in absorbed.into_iter().enumerate() {
            let mut changed = self.metadata[index].absorbed != absorbed;
            self.metadata[index].absorbed = absorbed;
            if let Block::Tool(call) = &mut self.blocks[index]
                && let Some(card) = call.agent_card.as_deref_mut()
            {
                let sent = sends.get(&index).copied().unwrap_or(0);
                let result = results.remove(&index);
                if card.sends != sent || card.result != result {
                    card.sends = sent;
                    card.result = result;
                    changed = true;
                }
            }
            if changed {
                self.bump(index);
            }
        }
    }

    pub fn grouping(&self, index: usize) -> &Grouping {
        self.metadata
            .get(index)
            .map_or(&Grouping::None, |meta| &meta.grouping)
    }

    /// Folds a finished Turn's steps under one summary row. A Turn with fewer
    /// than two successful tools keeps its rows: a summary would not be shorter.
    pub fn regroup(&mut self, turn: &TurnId) {
        // History blocks carry their Turn on the record source, not the key.
        let turn_of = |index: usize| {
            self.blocks[index].turn_id().or_else(|| {
                self.metadata[index]
                    .history
                    .iter()
                    .find_map(|source| source.turn_id.as_ref())
            })
        };
        let members: Vec<usize> = (0..self.blocks.len())
            .filter(|&index| turn_of(index) == Some(turn))
            .collect();
        let steps: Vec<usize> = members
            .iter()
            .copied()
            .filter(|&index| is_step(&self.blocks[index]))
            .collect();
        // The summary leads the first step row, folded or not, so failures
        // before the first success stay under it rather than above it.
        let head = members
            .iter()
            .copied()
            .find(|&index| is_row(&self.blocks[index], false));
        let tools = steps
            .iter()
            .filter(|&&index| matches!(self.blocks[index], Block::Tool(_)))
            .count();
        let failed = members
            .iter()
            .filter(|&&index| {
                matches!(&self.blocks[index], Block::Tool(call)
                        if matches!(call.state, ToolState::Failed)
                            || call.result_error.is_some()
                            || call.exit_code.is_some_and(|code| code != 0))
            })
            .count();
        let (duration, changes) = self
            .blocks
            .iter()
            .rev()
            .find_map(|block| match block {
                Block::TurnEnd {
                    turn_id,
                    duration,
                    changes,
                    ..
                } if turn_id == turn => Some((*duration, *changes)),
                _ => None,
            })
            .unwrap_or_default();
        let open = head.is_some_and(|index| {
            self.metadata[index].grouping.is_open()
                && self.metadata[index].grouping != Grouping::None
        });
        for index in members {
            let step = steps.contains(&index);
            let wanted = if tools < 2 {
                Grouping::None
            } else if Some(index) == head {
                Grouping::Head {
                    turn: turn.clone(),
                    summary: GroupSummary {
                        duration,
                        tools,
                        failed,
                        changes,
                    },
                    open,
                    folds: step,
                }
            } else if step {
                Grouping::Member {
                    turn: turn.clone(),
                    open,
                }
            } else {
                Grouping::None
            };
            if self.metadata[index].grouping != wanted {
                self.metadata[index].grouping = wanted;
                self.bump(index);
            }
        }
    }

    /// Opens or closes the group the step at `index` belongs to.
    pub fn toggle_group(&mut self, index: usize) -> bool {
        let turn = match self.grouping(index) {
            Grouping::Head { turn, .. } | Grouping::Member { turn, .. } => turn.clone(),
            Grouping::None => return false,
        };
        for at in 0..self.metadata.len() {
            match &mut self.metadata[at].grouping {
                Grouping::Head {
                    turn: known, open, ..
                }
                | Grouping::Member { turn: known, open }
                    if *known == turn =>
                {
                    *open = !*open;
                    self.metadata[at].revision = next_revision();
                }
                _ => {}
            }
        }
        true
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
    fn a_finished_turn_folds_its_successful_steps_and_keeps_failures_out() {
        let turn: TurnId = "00000000000000000001".parse().unwrap();
        let mut transcript = Transcript::default();
        let tool = |id: &str, state| {
            let mut call = ToolCall::new(
                ToolCallRef {
                    turn_id: turn.clone(),
                    tool_call_id: id.to_owned().into(),
                },
                "read",
            );
            call.set_state(state);
            Block::Tool(Box::new(call))
        };
        transcript.push(tool("a", ToolState::Succeeded));
        transcript.push(tool("b", ToolState::Failed));
        transcript.push(tool("c", ToolState::Succeeded));
        transcript.push(Block::TurnEnd {
            turn_id: turn.clone(),
            outcome: minicore_host_protocol::TurnOutcome::Completed,
            reason: None,
            duration: None,
            requests: 2,
            tool_rounds: 3,
            usage: Default::default(),
            changes: None,
            saving: None,
        });
        transcript.regroup(&turn);
        assert!(matches!(
            transcript.grouping(0),
            Grouping::Head {
                summary: GroupSummary {
                    tools: 2,
                    failed: 1,
                    ..
                },
                open: false,
                folds: true,
                ..
            }
        ));
        assert_eq!(transcript.grouping(1), &Grouping::None);
        assert!(matches!(
            transcript.grouping(2),
            Grouping::Member { open: false, .. }
        ));
        assert!(transcript.toggle_group(2));
        assert!(transcript.grouping(0).is_open());

        // A Turn that starts with a failure: the summary leads that row, which
        // stays visible while the steps after it fold.
        let mut failing = Transcript::default();
        failing.push(tool("d", ToolState::Failed));
        failing.push(tool("e", ToolState::Succeeded));
        failing.push(tool("f", ToolState::Succeeded));
        failing.push(transcript.blocks()[3].clone());
        failing.regroup(&turn);
        assert!(matches!(
            failing.grouping(0),
            Grouping::Head {
                folds: false,
                open: false,
                ..
            }
        ));
        assert!(matches!(failing.grouping(1), Grouping::Member { .. }));
        assert!(matches!(failing.grouping(2), Grouping::Member { .. }));
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
