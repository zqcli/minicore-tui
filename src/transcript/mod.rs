//! Presentation blocks and the per-block layout cache.

pub mod agent_card;
pub mod block;
mod history;
pub mod layout;
mod tool_call;

use std::sync::atomic::{AtomicU64, Ordering};

use minicore_host_protocol::{MessageId, Timestamp, ToolCallRef, TurnId};

pub use agent_card::ParentActions;
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
    latest_response: Option<(u64, minicore_host_runtime::model::Usage)>,
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

/// How a Turn's work folds. Each run of work between two user messages (a
/// steer splits a Turn) is one group: its tools, thinking and the notes the
/// model wrote between tool calls. The text that ends the run is its answer
/// and stays outside.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Grouping {
    #[default]
    None,
    /// The group's first step: draws the summary row and, closed, the latest
    /// step under it; open, itself.
    Head {
        turn: TurnId,
        segment: usize,
        summary: GroupSummary,
        open: bool,
    },
    /// A later step: draws nothing while the group is closed.
    Member {
        turn: TurnId,
        segment: usize,
        open: bool,
    },
    /// The final assistant message of a finished grouped run when it carries
    /// thinking: its thinking folds inside the group while its answer stays out.
    /// When the run had no earlier steps (thinking-only Turn), `summary` carries
    /// the `Worked` row directly on this block.
    Tail {
        turn: TurnId,
        segment: usize,
        summary: Option<GroupSummary>,
        open: bool,
    },
}

impl Grouping {
    pub fn is_open(&self) -> bool {
        match self {
            Self::None => true,
            Self::Head { open, .. } | Self::Member { open, .. } | Self::Tail { open, .. } => *open,
        }
    }

    fn key(&self) -> Option<(&TurnId, usize)> {
        match self {
            Self::None => None,
            Self::Head { turn, segment, .. }
            | Self::Member { turn, segment, .. }
            | Self::Tail { turn, segment, .. } => Some((turn, *segment)),
        }
    }
}

/// `Worked 2m 14s · 18 tools · 3 files changed (+142 −37) · 1 failed`, and the
/// newest step for the row under it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GroupSummary {
    /// Still running: `Working` instead of `Worked`.
    pub live: bool,
    /// The newest step is still actively streaming thinking.
    pub thinking: bool,
    pub duration: Option<std::time::Duration>,
    pub tools: usize,
    pub failed: usize,
    pub changes: Option<ChangeSummary>,
    pub latest: Option<(StepKind, String)>,
}

/// What the newest step of a group is, for its glyph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepKind {
    Running,
    Done,
    Failed,
    Stopped,
    Thinking,
    Note,
}

fn failed(call: &ToolCall) -> bool {
    matches!(call.state, ToolState::Failed)
        || call.result_error.is_some()
        || call.exit_code.is_some_and(|code| code != 0)
}

/// A line worth showing alone: markup stripped, with at least one word.
fn readable(line: &str) -> Option<String> {
    let line = line
        .trim()
        .trim_start_matches(['#', '-', '*', '>', '|', '`'])
        .trim_end_matches(['|', '`'])
        .replace("**", "")
        .replace('`', "")
        .trim()
        .to_owned();
    line.chars().any(char::is_alphanumeric).then_some(line)
}

/// The newest step as one line: its kind and `name subject`, a heading or the
/// newest readable line of a note.
fn latest(block: &Block) -> Option<(StepKind, String)> {
    let first = |text: &str| text.lines().find_map(readable).unwrap_or_default();
    match block {
        Block::Tool(call) if call.agent_card.is_some() => {
            let card = call.agent_card.as_deref().expect("checked");
            let kind = match card.outcome() {
                None => StepKind::Running,
                Some(minicore_host_protocol::TurnOutcome::Completed) => StepKind::Done,
                Some(minicore_host_protocol::TurnOutcome::Failed) => StepKind::Failed,
                Some(minicore_host_protocol::TurnOutcome::Cancelled) => StepKind::Stopped,
            };
            Some((
                kind,
                format!("agent {}", first(&card.task)).trim_end().to_owned(),
            ))
        }
        Block::Tool(call) => {
            let kind = match call.state {
                ToolState::Pending | ToolState::Running => StepKind::Running,
                ToolState::Denied | ToolState::Cancelled => StepKind::Stopped,
                _ if failed(call) => StepKind::Failed,
                _ => StepKind::Done,
            };
            let detail = call.header_detail();
            let subject = first(detail.trim_start_matches("$ "));
            Some((
                kind,
                format!("{} {subject}", call.name).trim_end().to_owned(),
            ))
        }
        Block::Assistant {
            text, reasoning, ..
        } if text.trim().is_empty() => {
            // Thinking that has not written a heading yet still says so.
            let heading = reasoning
                .lines()
                .rev()
                .find_map(readable)
                .unwrap_or_else(|| "thinking…".to_owned());
            Some((StepKind::Thinking, heading))
        }
        // The newest line, so streaming text moves the row along.
        Block::Assistant { text, .. } => {
            let line = text
                .lines()
                .rev()
                .find_map(readable)
                .unwrap_or_else(|| "responding…".to_owned());
            Some((StepKind::Note, line))
        }
        _ => None,
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
        use std::collections::{HashMap, HashSet};
        let mut by_session: HashMap<String, usize> = HashMap::new();
        // A task message names its card only within the same child Session.
        let mut by_task: HashMap<(String, String), usize> = HashMap::new();
        let mut by_turn: HashMap<TurnId, Vec<usize>> = HashMap::new();
        let mut settled: HashSet<usize> = HashSet::new();
        let mut parent: HashMap<usize, ParentActions> = HashMap::new();
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
                    if let Some(turn) = call.key.turn_id() {
                        by_turn.entry(turn.clone()).or_default().push(index);
                    }
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
                    settled.remove(&card);
                    parent.entry(card).or_default().sends += 1;
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
                // The parent's other dealings with a child: shown in its card
                // when they worked, as their own rows when they failed.
                Block::Tool(call) if call.name == "agent" && call.delegations.is_empty() => {
                    let Some(arguments) = call.arguments.as_ref() else {
                        continue;
                    };
                    if matches!(
                        call.state,
                        ToolState::Failed | ToolState::Denied | ToolState::Cancelled
                    ) || call.result_error.is_some()
                    {
                        continue;
                    }
                    let action = arguments.get("action").and_then(|value| value.as_str());
                    let target = arguments
                        .pointer("/target/session_id")
                        .and_then(|value| value.as_str())
                        .and_then(|session| by_session.get(session))
                        .copied();
                    match (action, target) {
                        (Some("wait"), _) => {
                            let Some(cards) = call.key.turn_id().and_then(|turn| by_turn.get(turn))
                            else {
                                continue;
                            };
                            absorbed[index] = true;
                            let active: Vec<usize> = cards
                                .iter()
                                .copied()
                                .filter(|card| !settled.contains(card))
                                .collect();
                            let targets = if active.is_empty() {
                                cards.as_slice()
                            } else {
                                active.as_slice()
                            };
                            let waiting =
                                matches!(call.state, ToolState::Pending | ToolState::Running);
                            for &card in targets {
                                let actions = parent.entry(card).or_default();
                                if waiting {
                                    actions.waiting = true;
                                } else {
                                    actions.waits += 1;
                                    actions.waited += call.duration.unwrap_or_default();
                                }
                            }
                        }
                        (
                            Some(action @ ("send" | "inspect" | "update" | "control")),
                            Some(card),
                        ) => {
                            absorbed[index] = true;
                            let actions = parent.entry(card).or_default();
                            match action {
                                "send" => {
                                    settled.remove(&card);
                                    actions.sends += 1;
                                }
                                "inspect" => actions.inspections += 1,
                                _ => actions.changes += 1,
                            }
                        }
                        _ => {}
                    }
                }
                Block::User {
                    kind: UserKind::Delegated,
                    text,
                    ..
                } => {
                    // Delegated child completion notifications stay out of the main
                    // conversation (status lives in the dock and full details live
                    // in Ctrl+B); still attach the delivered result to its card.
                    absorbed[index] = true;
                    let Some(summary) = crate::ui::delegated::summary(text) else {
                        continue;
                    };
                    let card = summary
                        .task_message_id
                        .and_then(|task| by_task.get(&(summary.session_id.clone(), task)))
                        .or_else(|| by_session.get(&summary.session_id));
                    if let Some(&card) = card {
                        settled.insert(card);
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
                let actions = parent.remove(&index).unwrap_or_default();
                let result = results.remove(&index);
                if card.parent != actions || card.result != result {
                    card.parent = actions;
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

    /// Folds each run of a Turn's work under one summary row, live or done. A
    /// run without a tool, or with a single step, keeps its rows: a summary
    /// would not be shorter.
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
        let mut members = (0..self.blocks.len()).filter(|&index| turn_of(index) == Some(turn));
        let Some(first) = members.next() else {
            return;
        };
        let last = members.next_back().unwrap_or(first);
        let end = self.blocks[first..=last]
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
            });
        // A user message inside the Turn (a consumed steer) starts a new run.
        // A child's delivered result is not one: it lives in its Agent card.
        let starts_run = |index: usize| {
            matches!(&self.blocks[index], Block::User { kind, .. } if *kind != UserKind::Delegated)
                && !self.metadata[index].absorbed
        };
        let mut segments: Vec<Vec<usize>> = vec![Vec::new()];
        for (index, meta) in self.metadata.iter().enumerate().take(last + 1).skip(first) {
            if starts_run(index) {
                if !segments.last().is_some_and(Vec::is_empty) {
                    segments.push(Vec::new());
                }
            } else if turn_of(index) == Some(turn) && !meta.absorbed {
                segments.last_mut().expect("one segment").push(index);
            }
        }
        let count = segments.len();
        let mut wanted: Vec<(usize, Grouping)> = Vec::new();
        for (segment, blocks) in segments.into_iter().enumerate() {
            let final_run = segment + 1 == count;
            let live = end.is_none() && final_run;
            // While a run goes on everything in it folds, Agent cards and text
            // included. Once it is done, only the final assistant answer stays
            // outside; all earlier steps, notes and thinking (including any
            // thinking on that final response) fold into Worked.
            let last_call = blocks
                .iter()
                .rposition(|&index| matches!(self.blocks[index], Block::Tool(_)));
            let last_answer = (!live)
                .then(|| {
                    blocks.iter().rposition(|&index| {
                        matches!(
                            &self.blocks[index],
                            Block::Assistant { text, .. } if !text.trim().is_empty()
                        )
                    })
                })
                .flatten()
                .filter(|&pos| last_call.is_none_or(|last| pos > last));
            let steps: Vec<usize> = blocks
                .iter()
                .enumerate()
                .filter(|&(position, &index)| match &self.blocks[index] {
                    Block::Tool(_) => true,
                    // An empty response draws nothing and is no step.
                    Block::Assistant {
                        text, reasoning, ..
                    } if text.trim().is_empty() && reasoning.trim().is_empty() => false,
                    Block::Assistant { .. } => Some(position) != last_answer,
                    _ => false,
                })
                .map(|(_, &index)| index)
                .collect();
            let tools = steps
                .iter()
                .filter(|&&index| matches!(self.blocks[index], Block::Tool(_)))
                .count();
            // Live, the run folds from its very first step (thinking, text or
            // tool) so Working tracks progress immediately. Done, a run folds
            // when it has multiple tool/agent steps or any thinking on the
            // final answer.
            let tail_block = last_answer.map(|pos| blocks[pos]);
            let tail_thinking = tail_block.is_some_and(|index| {
                matches!(
                    &self.blocks[index],
                    Block::Assistant { reasoning, .. } if !reasoning.trim().is_empty()
                )
            });
            let has_thinking = tail_thinking
                || steps.iter().any(|&index| {
                    matches!(
                        &self.blocks[index],
                        Block::Assistant { reasoning, .. } if !reasoning.trim().is_empty()
                    )
                });
            let grouped = if live {
                !steps.is_empty()
            } else {
                (steps.len() >= 2 && tools > 0) || has_thinking
            };
            let head_index = steps
                .first()
                .copied()
                .or(tail_block.filter(|_| tail_thinking));
            let open = head_index.is_some_and(|index| {
                let current = &self.metadata[index].grouping;
                current.key() == Some((turn, segment)) && current.is_open()
            });
            // One run is the whole Turn; a run a steer cut is timed from the
            // message that opened it to the message or end that closed it.
            let duration = match (blocks.first(), blocks.last()) {
                _ if live => None,
                _ if count == 1 => end.and_then(|(duration, _)| duration),
                (Some(&from), Some(&to)) => {
                    let opened = (0..from).rev().find(|&at| starts_run(at));
                    // The run's own blocks include the Turn's end.
                    let closed = (to..self.blocks.len()).find(|&at| {
                        starts_run(at) || matches!(self.blocks[at], Block::TurnEnd { .. })
                    });
                    opened
                        .zip(closed)
                        .and_then(|(opened, closed)| self.elapsed(opened, closed))
                }
                _ => None,
            };
            let summary = GroupSummary {
                live,
                thinking: steps.last().is_some_and(|&index| {
                    matches!(
                        &self.blocks[index],
                        Block::Assistant {
                            text,
                            streaming: true,
                            ..
                        } if text.trim().is_empty()
                    )
                }),
                duration,
                tools,
                failed: steps
                    .iter()
                    .filter(
                        |&&index| matches!(&self.blocks[index], Block::Tool(call) if failed(call)),
                    )
                    .count(),
                changes: end.filter(|_| final_run).and_then(|(_, changes)| changes),
                latest: steps.last().and_then(|&index| latest(&self.blocks[index])),
            };
            let mut members = steps.iter().copied().peekable();
            for &index in &blocks {
                let grouping = match grouped && members.next_if_eq(&index).is_some() {
                    true if Some(index) == head_index => Grouping::Head {
                        turn: turn.clone(),
                        segment,
                        summary: summary.clone(),
                        open,
                    },
                    true => Grouping::Member {
                        turn: turn.clone(),
                        segment,
                        open,
                    },
                    _ if grouped && tail_thinking && Some(index) == tail_block => Grouping::Tail {
                        turn: turn.clone(),
                        segment,
                        summary: steps.is_empty().then(|| summary.clone()),
                        open,
                    },
                    _ => Grouping::None,
                };
                wanted.push((index, grouping));
            }
        }
        for (index, grouping) in wanted {
            if self.metadata[index].grouping != grouping {
                self.metadata[index].grouping = grouping;
                self.bump(index);
            }
        }
    }

    /// Recorded time from block `from` to block `to`, when both are saved.
    fn elapsed(&self, from: usize, to: usize) -> Option<std::time::Duration> {
        let at = |index: usize| {
            self.metadata[index]
                .history
                .first()
                .map(|source| crate::ui::blocks::timestamp(source.recorded_at))
        };
        let span = at(to)?.duration_since(at(from)?);
        std::time::Duration::try_from(span).ok()
    }

    /// Opens the group the step at `index` belongs to, if it is closed.
    pub fn open_group(&mut self, index: usize) -> bool {
        if self.grouping(index).is_open() {
            return false;
        }
        self.toggle_group(index)
    }

    /// Opens or closes the group the step at `index` belongs to.
    pub fn toggle_group(&mut self, index: usize) -> bool {
        let Some((turn, segment)) = self
            .grouping(index)
            .key()
            .map(|(turn, segment)| (turn.clone(), segment))
        else {
            return false;
        };
        for at in 0..self.metadata.len() {
            if self.metadata[at].grouping.key() == Some((&turn, segment)) {
                match &mut self.metadata[at].grouping {
                    Grouping::Head { open, .. }
                    | Grouping::Member { open, .. }
                    | Grouping::Tail { open, .. } => *open = !*open,
                    Grouping::None => {}
                }
                self.metadata[at].revision = next_revision();
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
    fn each_run_of_work_folds_with_its_notes_and_failures_and_keeps_its_answer() {
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
        let text = |index: u64, text: &str| Block::Assistant {
            key: RequestKey::response(turn.clone(), index),
            reasoning: String::new(),
            text: text.into(),
            streaming: false,
            partial: false,
            expanded: None,
        };
        let user = |text: &str| Block::User {
            text: text.into(),
            kind: UserKind::Steering,
            at: None,
            expanded: None,
        };
        transcript.push(user("go"));
        transcript.push(tool("a", ToolState::Succeeded)); // 1
        transcript.push(text(1, "read a, now b")); // 2: a note
        transcript.push(tool("b", ToolState::Failed)); // 3
        transcript.push(user("also c")); // 4: a steer
        transcript.push(tool("c", ToolState::Running)); // 5
        transcript.push(tool("d", ToolState::Pending)); // 6
        transcript.push(text(3, "the answer so far")); // 7

        // Live: both runs fold, and the running one folds its text too.
        transcript.regroup(&turn);
        let Grouping::Head {
            summary,
            open: false,
            segment: 0,
            ..
        } = transcript.grouping(1).clone()
        else {
            panic!("{:?}", transcript.grouping(1))
        };
        assert_eq!((summary.tools, summary.failed, summary.live), (2, 1, false));
        assert_eq!(summary.latest, Some((StepKind::Failed, "read".into())));
        assert!(
            matches!(transcript.grouping(2), Grouping::Member { segment: 0, .. }),
            "a note"
        );
        assert!(
            matches!(transcript.grouping(3), Grouping::Member { .. }),
            "a failure"
        );
        let Grouping::Head {
            summary,
            segment: 1,
            ..
        } = transcript.grouping(5).clone()
        else {
            panic!("{:?}", transcript.grouping(5))
        };
        assert!(summary.live, "the run still going is Working");
        assert_eq!(
            summary.latest,
            Some((StepKind::Note, "the answer so far".into()))
        );
        assert!(
            matches!(transcript.grouping(7), Grouping::Member { .. }),
            "live text folds"
        );

        // Opening one run leaves the other closed, and survives a regroup.
        assert!(transcript.toggle_group(3));
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
        assert!(transcript.grouping(1).is_open());
        assert!(matches!(
            transcript.grouping(5),
            Grouping::Head {
                open: false,
                summary: GroupSummary { live: false, .. },
                ..
            }
        ));
        assert_eq!(
            transcript.grouping(7),
            &Grouping::None,
            "done, the answer is out"
        );

        // A run with a single step keeps its row.
        let mut short = Transcript::default();
        short.push(user("one"));
        short.push(tool("e", ToolState::Succeeded));
        short.push(text(0, "done"));
        short.push(transcript.blocks().last().unwrap().clone());
        short.regroup(&turn);
        assert_eq!(short.grouping(1), &Grouping::None);
    }

    #[test]
    fn live_run_folds_from_the_first_thinking_step_and_finishes_into_worked() {
        let turn: TurnId = "00000000000000000001".parse().unwrap();
        let mut transcript = Transcript::default();
        transcript.push(Block::User {
            text: "explain".into(),
            kind: UserKind::Prompt,
            at: None,
            expanded: None,
        });
        transcript.push(Block::Assistant {
            key: RequestKey::response(turn.clone(), 0),
            reasoning: "**Planning answer**\nChecking details".into(),
            text: String::new(),
            streaming: true,
            partial: false,
            expanded: None,
        });

        // Live: even a single thinking block with zero tools starts Working immediately.
        transcript.regroup(&turn);
        let Grouping::Head { summary, open, .. } = transcript.grouping(1).clone() else {
            panic!("expected Working head, got {:?}", transcript.grouping(1));
        };
        assert!(summary.live);
        assert!(!open);
        assert_eq!(summary.tools, 0);
        assert_eq!(
            summary.latest,
            Some((StepKind::Thinking, "Checking details".into()))
        );

        // Once the answer arrives and the Turn finishes, thinking folds into Worked
        // on the Tail block and only the final answer remains outside.
        if let Some(Block::Assistant {
            text, streaming, ..
        }) = transcript.block_mut(1)
        {
            *text = "Here is the answer.".into();
            *streaming = false;
        }
        transcript.push(Block::TurnEnd {
            turn_id: turn.clone(),
            outcome: minicore_host_protocol::TurnOutcome::Completed,
            reason: None,
            duration: Some(std::time::Duration::from_secs(4)),
            requests: 1,
            tool_rounds: 0,
            usage: Default::default(),
            changes: None,
            saving: None,
        });
        transcript.regroup(&turn);
        let Grouping::Tail {
            summary: Some(summary),
            open: false,
            ..
        } = transcript.grouping(1).clone()
        else {
            panic!(
                "expected thinking-only Worked tail, got {:?}",
                transcript.grouping(1)
            );
        };
        assert!(!summary.live);
        assert_eq!(summary.duration, Some(std::time::Duration::from_secs(4)));
        assert!(transcript.toggle_group(1));
        assert!(transcript.grouping(1).is_open());
    }

    #[test]
    fn a_childs_delivered_result_does_not_start_a_new_run() {
        let turn: TurnId = "00000000000000000001".parse().unwrap();
        let tool = |id: &str| {
            let mut call = ToolCall::new(
                ToolCallRef {
                    turn_id: turn.clone(),
                    tool_call_id: id.to_owned().into(),
                },
                "read",
            );
            call.set_state(ToolState::Succeeded);
            Block::Tool(Box::new(call))
        };
        let mut transcript = Transcript::default();
        transcript.push(Block::User {
            text: "go".into(),
            kind: UserKind::Prompt,
            at: None,
            expanded: None,
        });
        transcript.push(tool("a"));
        transcript.push(Block::User {
            text: "{}".into(),
            kind: UserKind::Delegated,
            at: None,
            expanded: None,
        });
        transcript.push(tool("b"));
        transcript.regroup(&turn);
        assert!(matches!(
            transcript.grouping(1),
            Grouping::Head {
                segment: 0,
                summary: GroupSummary {
                    tools: 2,
                    live: true,
                    ..
                },
                ..
            }
        ));
        assert!(matches!(
            transcript.grouping(3),
            Grouping::Member { segment: 0, .. }
        ));
    }

    #[test]
    fn independent_transcripts_never_share_cache_revisions() {
        let user = || Block::User {
            text: "same".into(),
            kind: UserKind::Prompt,
            at: None,
            expanded: None,
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
