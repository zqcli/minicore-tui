//! The resident session: transcript, live turn, queue, usage and status.

use std::time::{Duration, Instant};

use crate::protocol::{
    CompactStatus, CompactionProgress, CompactionResult, ContextBudget, LoopOutcome, ModelInfo,
    SessionInfo, SessionPresentation, SessionState, ToolRef, TurnRef, Usage, UsageTotals,
};
use crate::transcript::{Block, RequestKey, Transcript};

#[derive(Clone, Debug)]
pub struct Session {
    pub info: SessionInfo,
    /// Bumped when a different session becomes resident; stale results are
    /// dropped when it moves.
    pub epoch: u64,
    pub transcript: Transcript,
    pub live: Option<LiveTurn>,
    pub queue: Vec<Queued>,
    pub usage: UsageTotals,
    pub presentation: Option<SessionPresentation>,
    pub context: Option<ContextBudget>,
    pub compaction: Option<CompactionProgress>,
    pub interaction: Option<PendingInteraction>,
    /// The `first_item` boundary of the display projection: older items are
    /// behind the compaction summary.
    pub first_item: usize,
    /// The oldest source item currently loaded.
    pub loaded_from: usize,
    /// Items that exist between the projection boundary and `loaded_from`.
    pub hidden_items: usize,
    pub loading_older: bool,
    /// Set once the display floor was passed, so paging switches to the
    /// canonical history behind the compaction summary.
    pub reached_floor: bool,
    pub applied_steers: u64,
    /// Where the finished turn's streamed blocks start, kept after `live` is
    /// cleared so the durable items can replace exactly that region.
    pub reconcile_from: Option<(String, usize)>,
    /// Where the next turn's region starts, captured before the optimistic
    /// prompt block is pushed so the durable items replace it too.
    pub next_turn_start: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct LiveTurn {
    pub turn: TurnRef,
    pub started: Instant,
    pub requests: u32,
    pub tool_rounds: u64,
    pub phase: Phase,
    /// Index in the transcript where this turn's live blocks start.
    pub start: usize,
    /// Set when the Agent reported dropped events for this session.
    pub gap: bool,
    /// The tool currently running, for the status line.
    pub running_tool: Option<(String, String)>,
    /// Set once Esc asked for cancellation and `turn.wait` has not answered.
    pub interrupting: bool,
    pub usage: Usage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Thinking,
    Writing,
    Waiting,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Self::Thinking => "Thinking",
            Self::Writing => "Writing",
            Self::Waiting => "Waiting for model",
        }
    }
}

impl LiveTurn {
    pub fn new(turn: TurnRef, start: usize) -> Self {
        Self {
            turn,
            started: Instant::now(),
            requests: 1,
            tool_rounds: 0,
            phase: Phase::Thinking,
            start,
            gap: false,
            running_tool: None,
            interrupting: false,
            usage: Usage::default(),
        }
    }
}

/// A queued steering message (sent, awaiting apply) or a local follow-up.
#[derive(Clone, Debug)]
pub struct Queued {
    pub text: String,
    pub kind: QueuedKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueuedKind {
    Steer,
    Follow,
}

impl Queued {
    pub fn label(&self) -> &'static str {
        match self.kind {
            QueuedKind::Steer => "steer",
            QueuedKind::Follow => "next",
        }
    }
}

#[derive(Clone, Debug)]
pub struct PendingInteraction {
    pub turn: TurnRef,
    pub interaction_id: String,
    pub tool_name: String,
    pub prompt: String,
    pub kind: InteractionKind,
    pub detail: Option<String>,
}

#[derive(Clone, Debug)]
pub enum InteractionKind {
    Approval,
    Choice(Vec<String>),
}

impl PendingInteraction {
    /// Builds the prompt from the wire `kind` object the Agent sent.
    pub fn from_wire(
        turn: TurnRef,
        interaction: &crate::protocol::PendingInteraction,
        detail: Option<String>,
    ) -> Self {
        let data = interaction.kind.get("data");
        let field = |name: &str| data.and_then(|data| data.get(name));
        let prompt = field("prompt")
            .and_then(|prompt| prompt.as_str())
            .unwrap_or("The agent needs an answer")
            .to_owned();
        let choices: Vec<String> = field("choices")
            .and_then(|choices| choices.as_array())
            .map(|choices| {
                choices
                    .iter()
                    .filter_map(|choice| choice.get("text").and_then(|text| text.as_str()))
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        let is_approval = interaction
            .kind
            .get("type")
            .and_then(|kind| kind.as_str())
            .is_some_and(|kind| kind == "approval");
        Self {
            turn,
            interaction_id: interaction.interaction_id.clone(),
            tool_name: interaction.tool_name.clone(),
            prompt,
            kind: if is_approval || choices.is_empty() {
                InteractionKind::Approval
            } else {
                InteractionKind::Choice(choices)
            },
            detail,
        }
    }

    /// Rows the prompt offers, in order. Index 0 is "allow" for approvals.
    pub fn options(&self) -> Vec<String> {
        match &self.kind {
            InteractionKind::Approval => vec!["Allow once".into(), "Deny".into()],
            InteractionKind::Choice(choices) => choices.clone(),
        }
    }
}

impl Session {
    pub fn new(info: SessionInfo) -> Self {
        Self {
            info,
            epoch: 0,
            transcript: Transcript::default(),
            live: None,
            queue: Vec::new(),
            usage: UsageTotals::default(),
            presentation: None,
            context: None,
            compaction: None,
            interaction: None,
            first_item: 0,
            loaded_from: 0,
            hidden_items: 0,
            loading_older: false,
            reached_floor: false,
            applied_steers: 0,
            reconcile_from: None,
            next_turn_start: None,
        }
    }

    pub fn is_busy(&self) -> bool {
        self.live.is_some()
    }

    /// The status-line phase while a turn runs.
    pub fn phase_label(&self) -> Option<&'static str> {
        self.live.as_ref().map(|live| {
            if live.interrupting {
                "Interrupting"
            } else {
                live.phase.label()
            }
        })
    }

    pub fn model<'a>(&self, models: &'a [ModelInfo]) -> Option<&'a ModelInfo> {
        models.iter().find(|model| model.id == self.info.model)
    }

    /// Removes the optimistic prompt block a refused send left behind, so the
    /// transcript does not claim a turn the Agent never accepted.
    pub fn unsend_prompt(&mut self, text: &str) {
        if let Some(index) = self
            .transcript
            .blocks()
            .iter()
            .rposition(|block| matches!(block, Block::User { text: sent, .. } if sent == text))
        {
            self.transcript.remove(index);
        }
        self.next_turn_start = None;
    }

    pub fn push_follow_up(&mut self, text: String) {
        self.queue.push(Queued {
            text,
            kind: QueuedKind::Follow,
        });
    }

    pub fn take_follow_up(&mut self) -> Option<String> {
        let index = self
            .queue
            .iter()
            .position(|queued| queued.kind == QueuedKind::Follow)?;
        Some(self.queue.remove(index).text)
    }

    /// A turn ended: unapplied steering becomes a follow-up, because the Agent
    /// discards unapplied steers.
    pub fn abandon_unapplied_steers(&mut self) {
        for queued in self.queue.iter_mut() {
            if queued.kind == QueuedKind::Steer {
                queued.kind = QueuedKind::Follow;
            }
        }
        self.applied_steers = 0;
    }

    /// Drops steers the Agent has now applied and shows them as transcript
    /// blocks, in acceptance order.
    pub fn apply_steer_progress(&mut self, loop_id: &str, applied_count: u64) {
        while self.applied_steers < applied_count {
            let Some(index) = self
                .queue
                .iter()
                .position(|queued| queued.kind == QueuedKind::Steer)
            else {
                break;
            };
            let queued = self.queue.remove(index);
            let insert = self
                .transcript
                .blocks()
                .iter()
                .rposition(|block| block.loop_id() == Some(loop_id))
                .map_or(self.transcript.len(), |index| index + 1);
            self.transcript.insert(
                insert,
                Block::User {
                    text: queued.text,
                    kind: crate::protocol::read::UserKind::Steering,
                },
            );
            self.applied_steers += 1;
        }
    }

    pub fn sync_session_id_into_tools(&mut self) {
        let session_id = self.info.session_id.clone();
        for block in self.transcript.blocks_mut() {
            if let Block::Tool(call) = block {
                call.key.session_id = session_id.clone();
            }
        }
    }

    pub fn apply_state(&mut self, state: &SessionState) {
        self.compaction = state.compaction.clone();
        if state.status == crate::protocol::SessionStatus::Idle {
            // A settled session that reports no compaction is free again.
            self.compaction = None;
        }
    }

    /// Records terminal facts for a tool card.
    pub fn finish_tool(&mut self, key: &ToolRef) {
        if let Some(call) = self.transcript.find_tool_mut(key)
            && call.duration.is_none()
        {
            call.duration = call.started_at.map(|start| start.elapsed());
        }
        if let Some(live) = self.live.as_mut()
            && live.turn.loop_id == key.loop_id
        {
            live.tool_rounds += 1;
            live.running_tool = None;
            live.phase = Phase::Thinking;
        }
    }

    /// Marks every assistant block as no longer streaming.
    pub fn finish_streaming(&mut self) {
        for block in self.transcript.blocks_mut() {
            if let Block::Assistant { streaming, .. } = block {
                *streaming = false;
            }
        }
    }

    /// Truncates the live region of the given turn so durable items can replace
    /// it, keeping everything before it.
    pub fn drop_live_region(&mut self, start: usize) {
        self.transcript.truncate(start);
    }

    /// Replaces the region a finished turn streamed into with its durable
    /// items. The turn-end line stays last, so it is lifted out first and put
    /// back after the items. Returns whether a turn was waiting to reconcile.
    pub fn reconcile_turn(
        &mut self,
        items: &[(usize, crate::protocol::read::RawHistoryItem)],
    ) -> bool {
        let Some((loop_id, start)) = self.reconcile_from.take() else {
            return false;
        };
        let turn_end = self.take_turn_end(&loop_id);
        self.drop_live_region(start);
        for (_, item) in items {
            crate::transcript::block::append_history_item(
                &mut self.transcript,
                item,
                &self.info.session_id,
            );
        }
        if let Some(turn_end) = turn_end {
            self.transcript.push(turn_end);
        }
        true
    }

    fn take_turn_end(&mut self, loop_id: &str) -> Option<Block> {
        let index = self.transcript.blocks().iter().position(
            |block| matches!(block, Block::TurnEnd { loop_id: id, .. } if id == loop_id),
        )?;
        let block = self.transcript.blocks()[index].clone();
        self.transcript.remove(index);
        Some(block)
    }

    pub fn request_key(loop_id: &str, request_index: u32) -> RequestKey {
        RequestKey {
            loop_id: loop_id.to_owned(),
            request_index,
        }
    }

    /// Appends the turn-end line and folds the turn usage into the totals.
    pub fn finish_turn(
        &mut self,
        loop_id: &str,
        outcome: LoopOutcome,
        usage: Usage,
        requests: u32,
        tool_rounds: u64,
        duration: Option<Duration>,
    ) {
        self.usage.add(&usage);
        self.transcript.push(Block::TurnEnd {
            loop_id: loop_id.to_owned(),
            outcome,
            duration,
            requests,
            tool_rounds,
            usage,
            changes: None,
        });
    }

    /// Adds the one-line compaction marker. Loaded blocks are never removed.
    /// Adds the one-line compaction marker. Loaded blocks are never removed,
    /// and one operation is reported once: the manual response, the auto
    /// report and a state refresh can all describe the same compaction.
    pub fn push_compaction(&mut self, operation_id: &str, result: &CompactionResult) {
        if result.status != CompactStatus::Compacted {
            return;
        }
        let known = self.transcript.blocks().iter().any(|block| {
            matches!(block, Block::Compaction { operation_id: id, .. }
                if !id.is_empty() && id == operation_id)
        });
        if known {
            return;
        }
        self.transcript.push(Block::Compaction {
            operation_id: operation_id.to_owned(),
            before: result.before_tokens,
            after: result.after_tokens,
            summary: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::read::UserKind;
    use crate::protocol::{SessionStatus, ToolExecutionState};
    use crate::transcript::{ToolCall, ToolState};

    fn session() -> Session {
        Session::new(SessionInfo {
            session_id: "s".into(),
            title: None,
            profile: "coding".into(),
            workspace: "/w".into(),
            model: "mock".into(),
            reasoning: crate::protocol::Reasoning::Medium,
            loaded: true,
            created_at: String::new(),
            updated_at: String::new(),
        })
    }

    fn key(call: &str) -> ToolRef {
        ToolRef {
            session_id: "s".into(),
            loop_id: "l".into(),
            request_index: 0,
            tool_call_id: call.into(),
        }
    }

    #[test]
    fn applied_steers_become_steering_blocks_in_order() {
        let mut session = session();
        session.live = Some(LiveTurn::new(TurnRef::new("s", "l"), 0));
        session.queue.push(Queued {
            text: "first".into(),
            kind: QueuedKind::Steer,
        });
        session.queue.push(Queued {
            text: "second".into(),
            kind: QueuedKind::Steer,
        });
        session.queue.push(Queued {
            text: "later".into(),
            kind: QueuedKind::Follow,
        });
        session.apply_steer_progress("l", 1);
        assert_eq!(session.queue.len(), 2);
        assert!(matches!(
            &session.transcript.blocks()[0],
            Block::User {
                text,
                kind: UserKind::Steering
            } if text == "first"
        ));
        session.apply_steer_progress("l", 2);
        assert_eq!(session.queue.len(), 1);
        assert!(matches!(
            &session.transcript.blocks()[1],
            Block::User { text, .. } if text == "second"
        ));
        // A repeated receipt is idempotent.
        session.apply_steer_progress("l", 2);
        assert_eq!(session.transcript.len(), 2);
    }

    #[test]
    fn a_finished_turn_turns_unapplied_steers_into_follow_ups() {
        let mut session = session();
        session.queue.push(Queued {
            text: "steer".into(),
            kind: QueuedKind::Steer,
        });
        session.abandon_unapplied_steers();
        assert_eq!(session.queue[0].kind, QueuedKind::Follow);
        assert_eq!(session.take_follow_up().as_deref(), Some("steer"));
    }

    #[test]
    fn terminal_tool_facts_stamp_a_duration_once() {
        let mut session = session();
        let mut call = ToolCall::new(key("c"), "bash");
        call.state = ToolState::Running;
        call.started_at = Some(Instant::now());
        session.transcript.push(Block::Tool(Box::new(call)));
        session.finish_tool(&key("c"));
        let Block::Tool(call) = &session.transcript.blocks()[0] else {
            panic!("expected a card");
        };
        assert!(call.duration.is_some());
    }

    #[test]
    fn idle_state_clears_a_stale_compaction_phase() {
        let mut session = session();
        session.compaction = Some(CompactionProgress {
            operation_id: "op".into(),
            phase: crate::protocol::CompactionPhase::Summarizing,
            covered_item_count: 1,
            retained_item_count: 2,
        });
        session.apply_state(&SessionState {
            session_id: "s".into(),
            status: SessionStatus::Idle,
            active_loop: None,
            block_reason: None,
            compaction: None,
        });
        assert!(session.compaction.is_none());
    }

    #[test]
    fn execution_state_maps_to_card_state() {
        assert_eq!(
            ToolState::from_execution(ToolExecutionState::AwaitingPolicy),
            ToolState::Running
        );
        assert_eq!(
            ToolState::from_execution(ToolExecutionState::Denied),
            ToolState::Denied
        );
    }
}
