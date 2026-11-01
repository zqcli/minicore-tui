//! A live session: transcript, live turn, queue, usage and status.

use std::collections::{BTreeSet, VecDeque};
use std::time::{Duration, Instant};

use crate::protocol::event::{PreviewState, ToolOutcome};
use crate::protocol::read::TurnPersistence;
use crate::protocol::read::{InterruptedRequest, TurnRecord};
use crate::protocol::read::{RawHistoryItem, RuntimeItem, UserKind};
use crate::protocol::stamp::{now_millis, unix_millis};
use crate::protocol::{
    AgentEvent, BlockReason, CompactStatus, CompactionProgress, CompactionResult, ContextUsage,
    DeliveredInput, Queue, SessionContext, SessionInfo, SessionPresentation, SessionSnapshot,
    SnapshotInteraction, ToolRef, TurnRef, TurnWaitResult, Usage, UsageTotals,
};
use crate::transcript::{Block, EXPANDED_ROWS, RequestKey, ToolCall, ToolState, Transcript};
use crate::transcript::{append_history_item, push_interrupted};

use super::Scroll;
use super::tasks::History;

/// How soon the footer's figures may be asked for again while a turn runs.
const PRESENTATION_EVERY: Duration = Duration::from_secs(2);
/// Closed turns remembered for late-delta suppression.
const CLOSED_TURNS: usize = 256;
/// Turn regions remembered for a completion that may still arrive.
const RECONCILE_REGIONS: usize = 256;
pub const EARLY_BYTES: usize = 2 * 1024 * 1024;
pub const EARLY_TURNS: usize = 8;

pub struct Session {
    #[cfg(feature = "e2e-observe")]
    pub observed_installs: Vec<super::observe::Installed>,
    pub info: SessionInfo,
    /// Identifies this live session; results posted for another one are stale.
    pub epoch: u64,
    pub transcript: Transcript,
    pub live: Option<LiveTurn>,
    pub(super) ended_live: VecDeque<LiveTurn>,
    pub(super) auto_title_asked: bool,
    /// The Agent's queue as last reported; the Agent owns it.
    pub queue: Queue,
    early: VecDeque<(AgentEvent, usize)>,
    pub early_bytes: usize,
    #[cfg(feature = "e2e-observe")]
    pub early_peak_bytes: usize,
    pub early_gap: bool,
    pub usage: UsageTotals,
    pub presentation: Option<SessionPresentation>,
    /// When `presentation` was last asked for, and whether that answer is still due.
    pub presentation_asked: Option<tokio::time::Instant>,
    pub presentation_in_flight: bool,
    pub context: Option<SessionContext>,
    pub control_in_flight: bool,
    pub control_due: bool,
    /// The resident instance the Agent reports; `None` until an open installs it.
    pub load: Option<String>,
    /// Completion discovery watermarks and pending bodies for that instance.
    pub recovery: crate::app::recovery::RecoveryState,
    pub recovery_tasks: crate::app::recovery::RecoveryTasks,
    pub body_recovery: VecDeque<BodyRecovery>,
    /// The Agent's execution as the last snapshot observed it, which differs
    /// from `live` while a waited turn has not answered yet.
    pub execution: Option<crate::protocol::SessionExecution>,
    pub catalog: crate::protocol::CompletionDirectory,
    /// Turns whose canonical body is installed; their late deltas are dropped.
    closed: VecDeque<String>,
    pub(super) delivered: VecDeque<DeliveredIdentity>,
    completion_order: VecDeque<(u64, String)>,
    /// A turn a control snapshot adopted, which needs its own wait registered.
    pub adopted: Option<TurnRef>,
    /// A recovered turn whose ordering anchor is not loaded, so its position
    /// among the unsaved turns is unknown.
    pub ordering_gap: bool,
    /// Why the Agent blocks this session, if it does.
    pub block_reason: Option<BlockReason>,
    /// When `session.context` was last asked for while a turn ran.
    pub context_asked: Option<tokio::time::Instant>,
    /// The latest turn-start compaction attempt `session.context` reported.
    auto_seen: Option<String>,
    pub compaction: Option<CompactionProgress>,
    /// Coverage of the current summary from `session.context`; a summary
    /// replayed from history carries none of its own.
    pub summary_coverage: Option<(usize, usize)>,
    pub interaction: Option<PendingInteraction>,
    pub interaction_editor: super::composer::Composer,
    /// When the pending prompt starts taking answer keys; a stray key before then moves it back.
    pub armed_at: tokio::time::Instant,
    /// Tools whose approvals the user allowed for the rest of this session.
    pub allowed_tools: BTreeSet<String>,
    /// The display projection's first item; older items are behind the
    /// compaction summary.
    pub first_item: usize,
    /// The oldest source item loaded.
    pub loaded_from: usize,
    /// The exclusive end of the contiguous window that starts at `loaded_from`.
    loaded_end: usize,
    pub loading_older: bool,
    /// Pages still to load behind the summary until the latest answer shows.
    pub backfill: u8,
    /// The finished turns whose durable items will replace their streamed
    /// regions, with where each region starts.
    pub reconcile_from: Vec<(String, usize)>,
    /// The composer text this session had when it left the foreground.
    pub draft: Option<String>,
    /// Refused texts awaiting local control because no remote recipient received them.
    pub returned: Vec<String>,
    /// The connection that last submitted text to this session.
    /// A turn ended while this session was in the background.
    pub unseen: bool,
    /// The view this session had when it left the foreground.
    pub scroll: Scroll,
}

/// What a live session is doing, for the picker, the footer and the phone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveStatus {
    Idle,
    Running,
    Approval,
    Compacting,
}

pub struct BodyRecovery {
    pub loop_id: String,
    pub status: super::recovery::RecoveryStatus,
    pub message: String,
}

pub(super) struct DeliveredIdentity {
    #[cfg(feature = "e2e-observe")]
    pub loop_id: String,
    pub input_id: String,
    #[cfg(feature = "e2e-observe")]
    pub request_index: u32,
}

pub struct LiveTurn {
    pub(super) completion_background: Option<bool>,
    pub turn: TurnRef,
    pub started: tokio::time::Instant,
    pub started_ms: u64,
    pub requests: u32,
    pub phase: Phase,
    /// Index in the transcript where this turn's blocks start.
    pub start: usize,
    /// The Agent dropped events while this turn ran.
    pub gap: bool,
    /// The tool running now and its detail, for the status line.
    pub running_tool: Option<(String, String)>,
    /// Esc asked for cancellation and `turn.wait` has not answered yet.
    pub interrupting: bool,
    pub accounted_usage: Usage,
    pub usage_final: bool,
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
            completion_background: None,
            turn,
            started: tokio::time::Instant::now(),
            started_ms: crate::protocol::stamp::now_millis(),
            requests: 1,
            phase: Phase::Thinking,
            start,
            gap: false,
            running_tool: None,
            interrupting: false,
            accounted_usage: Usage::default(),
            usage_final: false,
        }
    }
}

/// How long a prompt waits after it appears, or after a stray key, before it takes an answer.
pub const ARM_AFTER: Duration = Duration::from_millis(500);
pub const TEXT_ANSWER_MAX_BYTES: usize = 8192;

#[derive(Clone)]
pub struct PendingInteraction {
    pub turn: TurnRef,
    pub interaction_id: String,
    /// The call asking; its card says what would run.
    pub tool_call_id: String,
    pub tool_name: String,
    pub prompt: String,
    pub kind: InteractionKind,
    pub selected: usize,
}

#[derive(Clone)]
pub enum InteractionKind {
    Approval,
    Choice(Vec<String>),
    Text,
}

impl PendingInteraction {
    /// The interaction a snapshot carries, with the turn and request that asked.
    pub fn from_snapshot(snapshot: &SessionSnapshot, pending: &SnapshotInteraction) -> Self {
        let turn = TurnRef::new(&snapshot.session_id, &pending.loop_id);
        Self::from_wire(turn, &pending.interaction)
    }

    pub fn from_wire(turn: TurnRef, interaction: &crate::protocol::PendingInteraction) -> Self {
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
                    .filter_map(|choice| choice.get("text")?.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let kind = match interaction.kind.get("type").and_then(|kind| kind.as_str()) {
            Some("approval") => InteractionKind::Approval,
            Some("tool_input") => match field("answer_kind").and_then(|kind| kind.as_str()) {
                Some("text") => InteractionKind::Text,
                _ => InteractionKind::Choice(choices),
            },
            _ => InteractionKind::Approval,
        };
        Self {
            turn,
            interaction_id: interaction.interaction_id.clone(),
            tool_call_id: interaction.tool_call_id.clone(),
            tool_name: interaction.tool_name.clone(),
            prompt,
            kind,
            selected: 0,
        }
    }

    /// The rows the prompt offers; for an approval, row 0 allows once, row 1
    /// denies and row 2 allows the tool for the session.
    pub fn options(&self) -> Vec<String> {
        match &self.kind {
            InteractionKind::Approval => vec![
                "Allow once".into(),
                "Deny".into(),
                format!("Allow {} this session", self.tool_name),
            ],
            InteractionKind::Choice(choices) => choices.clone(),
            InteractionKind::Text => Vec::new(),
        }
    }
}

impl Session {
    pub fn new(info: SessionInfo, epoch: u64) -> Self {
        Self {
            #[cfg(feature = "e2e-observe")]
            observed_installs: Vec::new(),
            info,
            epoch,
            transcript: Transcript::default(),
            live: None,
            ended_live: VecDeque::new(),
            auto_title_asked: false,
            queue: Queue::default(),
            early: VecDeque::new(),
            early_bytes: 0,
            #[cfg(feature = "e2e-observe")]
            early_peak_bytes: 0,
            early_gap: false,
            usage: UsageTotals::default(),
            presentation: None,
            presentation_asked: None,
            presentation_in_flight: false,
            context: None,
            control_in_flight: false,
            control_due: false,
            load: None,
            recovery: crate::app::recovery::RecoveryState::default(),
            recovery_tasks: crate::app::recovery::RecoveryTasks::default(),
            body_recovery: VecDeque::new(),
            execution: None,
            catalog: crate::protocol::CompletionDirectory::default(),
            closed: VecDeque::new(),
            delivered: VecDeque::new(),
            completion_order: VecDeque::new(),
            adopted: None,
            ordering_gap: false,
            block_reason: None,
            context_asked: None,
            auto_seen: None,
            compaction: None,
            summary_coverage: None,
            interaction: None,
            interaction_editor: super::composer::Composer::new(),
            armed_at: tokio::time::Instant::now(),
            allowed_tools: BTreeSet::new(),
            first_item: 0,
            loaded_from: 0,
            loaded_end: 0,
            loading_older: false,
            backfill: 0,
            reconcile_from: Vec::new(),
            draft: None,
            returned: Vec::new(),
            unseen: false,
            scroll: Scroll::Follow,
        }
    }

    pub fn is_busy(&self) -> bool {
        self.live.is_some()
            || self.execution.as_ref().is_some_and(|execution| {
                !matches!(execution, crate::protocol::SessionExecution::None)
            })
    }

    pub(super) fn note_body_recovery(
        &mut self,
        loop_id: &str,
        status: super::recovery::RecoveryStatus,
        message: String,
    ) {
        self.body_recovery.retain(|note| note.loop_id != loop_id);
        self.body_recovery.push_back(BodyRecovery {
            loop_id: loop_id.to_owned(),
            status,
            message,
        });
        if self.body_recovery.len() > 256 {
            self.body_recovery.pop_front();
        }
        let at =
            self.transcript.blocks().iter().position(
                |block| matches!(block,Block::TurnEnd { loop_id:id,.. } if id == loop_id),
            );
        if let Some(at) = at {
            self.transcript.block_mut(at);
        }
    }

    pub(super) fn incomplete_completion(
        &mut self,
        turn: &crate::protocol::CompletedTurn,
        status: super::recovery::RecoveryStatus,
        message: String,
    ) -> Option<(usize, isize, usize)> {
        let exists = self.transcript.blocks().iter().any(
            |block| matches!(block, Block::TurnEnd { loop_id, .. } if *loop_id == turn.loop_id),
        );
        let moved = if exists {
            if !self
                .reconcile_from
                .iter()
                .any(|(id, _)| id == &turn.loop_id)
            {
                let range = turn_bounds(self.transcript.blocks(), &turn.loop_id)
                    .expect("an incomplete turn has its terminal");
                self.record_region(&turn.loop_id, range.start);
            }
            None
        } else {
            let start = self
                .reconcile_from
                .iter()
                .find(|(id, _)| *id == turn.loop_id)
                .map(|(_, at)| *at)
                .or_else(|| self.completion_position(turn))
                .unwrap_or_else(|| self.transcript.len());
            let before = self.transcript.len();
            self.record_region(&turn.loop_id, start);
            let insertion = self.region_end(&turn.loop_id);
            self.place_early(&turn.loop_id);
            let outcome = match (turn.completion_kind, turn.outcome_kind) {
                (crate::protocol::CompletionKind::InternalError, _) => {
                    crate::protocol::LoopOutcome::Unknown
                }
                (_, Some(crate::protocol::CompletionOutcome::Completed)) => {
                    crate::protocol::LoopOutcome::Completed
                }
                (_, Some(crate::protocol::CompletionOutcome::Cancelled)) => {
                    crate::protocol::LoopOutcome::Cancelled {
                        reason: crate::protocol::CancelReason::Unknown,
                    }
                }
                (_, Some(crate::protocol::CompletionOutcome::Failed)) => {
                    crate::protocol::LoopOutcome::Failed {
                        kind: "unknown".into(),
                        model_error: None,
                    }
                }
                _ => crate::protocol::LoopOutcome::Unknown,
            };
            self.push_in_turn(
                &turn.loop_id,
                Block::TurnEnd {
                    loop_id: turn.loop_id.clone(),
                    outcome,
                    duration: None,
                    requests: 0,
                    tools: 0,
                    usage: Usage::default(),
                    changes: None,
                    saving: None,
                },
            );
            Some((
                insertion,
                (self.transcript.len() - before) as isize,
                insertion,
            ))
        };
        let message = match turn.persistence {
            Some(persistence) => {
                self.saving_note(&turn.loop_id, persistence, turn.persistence_uncertain);
                message
            }
            None => format!("{message} · save unknown"),
        };
        self.completion_order.retain(|(_, id)| id != &turn.loop_id);
        self.completion_order
            .push_back((turn.seq, turn.loop_id.clone()));
        if self.completion_order.len() > CLOSED_TURNS {
            self.completion_order.pop_front();
        }
        let message = if self.ordering_gap {
            format!("{message} · history anchor unavailable; region position is unresolved")
        } else {
            message
        };
        self.note_body_recovery(&turn.loop_id, status, message);
        moved
    }

    pub fn compacting(&self) -> bool {
        self.compaction.is_some()
    }

    /// The Git branch the footer shows, from the last presentation.
    pub fn branch(&self) -> Option<&str> {
        let branch = self.presentation.as_ref()?.git_branch.as_deref();
        branch.filter(|branch| !branch.trim().is_empty())
    }

    pub fn status_label(&self) -> Option<&'static str> {
        if self.compacting() {
            Some("Compacting context")
        } else if self
            .interaction
            .as_ref()
            .is_some_and(|pending| matches!(pending.kind, InteractionKind::Text))
        {
            Some("Waiting for input")
        } else if self.interaction.is_some() {
            Some("Waiting for approval")
        } else {
            self.phase_label()
        }
    }

    pub fn live_status(&self) -> LiveStatus {
        self.control_status()
    }

    /// The status line while a compaction runs. Inside a turn Esc ends the
    /// whole turn, which is the only way to stop that compaction.
    pub fn compaction_label(&self) -> Option<String> {
        let progress = self.compaction.as_ref()?;
        let key = match self.is_busy() {
            true => "interrupt",
            false => "cancel",
        };
        Some(format!(
            "Compacting context · {} (esc to {key})",
            progress.phase.label()
        ))
    }

    /// Whether the footer's figures may be asked for again mid-turn: no answer
    /// is on its way and the last ask is old enough.
    pub fn presentation_due(&self) -> bool {
        !self.presentation_in_flight
            && self
                .presentation_asked
                .is_none_or(|asked| asked.elapsed() >= PRESENTATION_EVERY)
    }

    /// The status-line phase while a turn runs.
    pub fn phase_label(&self) -> Option<&'static str> {
        self.live
            .as_ref()
            .map(|live| match live.interrupting {
                true => "Interrupting",
                false => live.phase.label(),
            })
            .or_else(|| self.execution.as_ref()?.phase().map(|phase| phase.label()))
    }

    /// The first prompt the user typed, as loaded.
    pub fn first_prompt(&self) -> Option<&str> {
        self.transcript
            .blocks()
            .iter()
            .find_map(|block| match block {
                Block::User {
                    text,
                    kind: UserKind::Prompt,
                    ..
                } => Some(text.as_str()),
                _ => None,
            })
    }

    /// The title an untitled session gets from its first prompt.
    pub fn auto_title(&self) -> Option<String> {
        let title = self.info.title.as_deref();
        let untitled = title.is_none_or(|title| title.trim().is_empty());
        untitled.then(|| self.first_prompt().and_then(auto_title))?
    }

    fn is_live(&self, turn: &TurnRef) -> bool {
        self.live
            .as_ref()
            .is_some_and(|live| live.turn.loop_id == turn.loop_id)
    }

    /// Applies one event for this session. Events are hints that may arrive in
    /// any order; a turn starts only from an RPC answer, never from an event.
    pub fn apply_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::SessionOpened { session, .. } => self.info = *session,
            // Control state arrives only through `apply_control`.
            AgentEvent::SessionState { .. } => {}
            AgentEvent::RequestUsage { turn, usage, .. } if self.is_live(&turn) => {
                let live = self.live.as_mut().unwrap();
                if !live.usage_final {
                    self.usage.add(&usage);
                    live.accounted_usage.add(&usage);
                }
            }
            AgentEvent::RequestStarted {
                turn,
                request_index,
                model,
                ..
            } if self.is_live(&turn) => {
                if let Some(live) = self.live.as_mut() {
                    live.requests = live.requests.max(request_index + 1);
                    live.phase = Phase::Waiting;
                }
                self.info.model = model;
            }
            // A turn whose canonical body is installed takes no more deltas.
            AgentEvent::OutputDelta { turn, .. } if self.is_closed(&turn.loop_id) => {}
            // A successor starts streaming before the wait that names it
            // answers, so its deltas are keyed by its own turn and kept even
            // while the previous turn is still the live one.
            AgentEvent::OutputDelta {
                turn,
                request_index,
                reasoning,
                delta,
                ..
            } if self.knows_turn(&turn.loop_id) => {
                self.ensure_assistant(&turn.loop_id, request_index);
                let key = RequestKey {
                    loop_id: turn.loop_id,
                    request_index,
                };
                if let Block::Assistant {
                    reasoning: thinking,
                    text,
                    streaming,
                    ..
                } = self.transcript.assistant_mut(&key)
                {
                    match reasoning {
                        true => thinking.push_str(&delta),
                        false => text.push_str(&delta),
                    }
                    *streaming = true;
                }
                if let Some(live) = self
                    .live
                    .as_mut()
                    .filter(|live| live.turn.loop_id == key.loop_id)
                {
                    live.phase = match reasoning {
                        true => Phase::Thinking,
                        false => Phase::Writing,
                    };
                }
            }
            // The queue arrives only through `apply_control`.
            AgentEvent::InputQueue { .. } => {}
            AgentEvent::InputDelivered { turn, items, .. } => self.deliver(turn, items),
            AgentEvent::ToolStarted {
                turn,
                request_index,
                tool_call_id,
                tool_name,
                ..
            } if self.knows_turn(&turn.loop_id) => {
                let active = self.is_live(&turn);
                let key = ToolRef {
                    session_id: turn.session_id,
                    loop_id: turn.loop_id,
                    request_index,
                    tool_call_id,
                };
                match self.transcript.find_tool_mut(&key) {
                    // A preview or the invocation made the card; it runs from here.
                    Some(call) => {
                        call.started_at.get_or_insert_with(Instant::now);
                    }
                    None => self.push_running_tool(key, tool_name.clone()),
                }
                if active && let Some(live) = self.live.as_mut() {
                    live.phase = Phase::Thinking;
                    live.running_tool = Some((tool_name, String::new()));
                }
            }
            AgentEvent::ToolPresentation {
                call,
                tool_name,
                display,
                ..
            } if self.knows_turn(&call.turn.loop_id) => {
                let key = call.key();
                if self.transcript.find_tool(&key).is_none() {
                    self.push_running_tool(key.clone(), tool_name);
                }
                if let Some(card) = self.transcript.find_tool_mut(&key) {
                    card.apply_display(&display);
                }
                if let Some((_, detail)) = self
                    .live
                    .as_mut()
                    .filter(|live| live.turn.loop_id == key.loop_id)
                    .and_then(|live| live.running_tool.as_mut())
                {
                    *detail = display.detail;
                }
            }
            AgentEvent::ToolArgumentsPreview {
                call,
                tool_name,
                state,
                display,
                ..
            } if self.knows_turn(&call.turn.loop_id) => {
                let key = call.key();
                match self.transcript.find_tool_mut(&key) {
                    Some(card) => card.apply_display(&display),
                    // The preview can come before `tool_started`; the card
                    // shows the arguments while they generate.
                    None if state == PreviewState::Generating => {
                        let mut card = ToolCall::new(key, tool_name);
                        card.apply_display(&display);
                        self.push_in_turn(&call.turn.loop_id, Block::Tool(Box::new(card)));
                    }
                    None => {}
                }
            }
            AgentEvent::ToolInvocation { invocation, .. } => {
                if self.is_closed(&invocation.tool_ref.loop_id)
                    && self.transcript.find_tool(&invocation.tool_ref).is_none()
                {
                    return;
                }
                match self.transcript.find_tool_mut(&invocation.tool_ref) {
                    Some(card) => card.apply_invocation(&invocation),
                    None => {
                        let mut card =
                            ToolCall::new(invocation.tool_ref.clone(), invocation.name.clone());
                        card.detail = invocation.detail();
                        self.push_in_turn(
                            &invocation.tool_ref.loop_id,
                            Block::Tool(Box::new(card)),
                        );
                    }
                }
            }
            AgentEvent::ToolExecution { execution, .. } => {
                if let Some(card) = self.transcript.find_tool_mut(&execution.tool_ref) {
                    card.apply_execution(&execution);
                    if ToolState::from_execution(execution.state).is_terminal() {
                        self.finish_tool(&execution.tool_ref);
                    }
                }
            }
            AgentEvent::ToolProcess { data, .. } => {
                let Some(card) = self.transcript.find_tool_mut(&data.tool_ref) else {
                    return;
                };
                if let Some(command) = &data.command {
                    card.apply_command(command);
                }
                if let Some(chunk) = &data.chunk {
                    let bytes = match chunk.encoding.as_str() {
                        "base64" => {
                            use base64::Engine;
                            base64::engine::general_purpose::STANDARD
                                .decode(chunk.data.as_bytes())
                                .unwrap_or_default()
                        }
                        _ => chunk.data.clone().into_bytes(),
                    };
                    let text = String::from_utf8_lossy(&bytes).into_owned();
                    if let Some(text) = card.accept_chunk(chunk.stream, chunk, text) {
                        card.push_output(&text);
                    }
                }
            }
            AgentEvent::ToolFinished { call, result, .. } => {
                let key = call.key();
                self.finish_tool(&key);
                if let Some(card) = self.transcript.find_tool_mut(&key) {
                    card.set_state(match result.outcome {
                        ToolOutcome::Success | ToolOutcome::InputProvided => ToolState::Succeeded,
                        ToolOutcome::Failed => ToolState::Failed,
                        ToolOutcome::Denied => ToolState::Denied,
                        ToolOutcome::Cancelled => ToolState::Cancelled,
                        ToolOutcome::Unknown => card.state,
                    });
                    card.truncated |= result.content_truncated;
                    if let Some(content) = result.content {
                        card.apply_result(&content);
                    }
                }
            }
            // An interaction arrives and resolves with the control snapshot; an
            // older resolution must not clear a newer interaction.
            AgentEvent::InteractionRequested { .. } | AgentEvent::InteractionResolved { .. } => {}
            AgentEvent::TurnFinished { turn, .. } if self.is_live(&turn) => {
                if let Some(live) = self.live.as_mut() {
                    live.phase = Phase::Waiting;
                }
            }
            _ => {}
        }
    }

    fn push_running_tool(&mut self, key: ToolRef, name: String) {
        let mut card = ToolCall::new(key, name);
        card.state = ToolState::Running;
        card.started_at = Some(Instant::now());
        let loop_id = card.key.loop_id.clone();
        self.push_in_turn(&loop_id, Block::Tool(Box::new(card)));
    }

    /// Tool calls shown from `start` on, which is what the turn's counts mean
    /// to a reader; the Agent's `tool_rounds` counts requests with tools.
    pub fn tools_since(&self, start: usize) -> u64 {
        self.transcript.blocks()[start.min(self.transcript.len())..]
            .iter()
            .filter(|block| matches!(block, Block::Tool(_)))
            .count() as u64
    }

    /// Records a tool's end on its card and in the live turn.
    fn finish_tool(&mut self, key: &ToolRef) {
        if let Some(card) = self.transcript.find_tool_mut(key)
            && card.duration.is_none()
        {
            card.duration = card.started_at.map(|start| start.elapsed());
        }
        if let Some(live) = self.live.as_mut()
            && live.turn.loop_id == key.loop_id
        {
            live.running_tool = None;
            live.phase = Phase::Thinking;
        }
    }

    /// Trims the output of the finished cards from block `from` on.
    pub fn settle_cards(&mut self, from: usize) {
        let long: Vec<usize> = (from..self.transcript.len())
            .filter(|index| {
                matches!(&self.transcript.blocks()[*index], Block::Tool(call)
                    if call.output_content().len() > EXPANDED_ROWS)
            })
            .collect();
        for index in long {
            if let Some(Block::Tool(call)) = self.transcript.block_mut(index) {
                call.settle();
            }
        }
    }

    /// Ends the live turn when `wait` is about it, appending the turn-end line.
    /// Returns whether the durable items must replace what was streamed.
    /// Remembers where a turn's streamed region begins, so a completion that
    /// arrives for it later replaces that region instead of duplicating it.
    pub fn record_region(&mut self, loop_id: &str, start: usize) {
        self.reconcile_from.retain(|(id, _)| id != loop_id);
        self.reconcile_from.push((loop_id.to_owned(), start));
        if self.reconcile_from.len() > RECONCILE_REGIONS {
            self.reconcile_from.remove(0);
        }
    }

    pub fn end_turn(&mut self, wait: &TurnWaitResult) -> Option<bool> {
        let loop_id = &wait.turn.loop_id;
        if self.transcript.blocks().iter().any(|block| matches!(block, Block::TurnEnd { loop_id: id, duration: Some(_), .. } if id == loop_id)) {
            return None;
        }
        let live = self
            .live
            .as_ref()
            .filter(|live| &live.turn.loop_id == loop_id)
            .or_else(|| {
                self.ended_live
                    .iter()
                    .find(|live| &live.turn.loop_id == loop_id)
            });
        let start = live.map(|live| live.start).or_else(|| {
            self.reconcile_from
                .iter()
                .find(|(id, _)| id == loop_id)
                .map(|(_, start)| *start)
        })?;
        let elapsed = live.map(|live| live.started.elapsed());
        let requests = wait
            .requests
            .unwrap_or_else(|| live.map_or(0, |live| live.requests));
        let gap = live.is_some_and(|live| live.gap);
        let accounted_usage = live.map(|live| live.accounted_usage.clone());
        let end = self.region_end(loop_id);
        let finishing: Vec<usize> = (start..end)
            .filter(|index| {
                self.transcript.blocks()[*index].is_streaming()
                    || matches!(&self.transcript.blocks()[*index], Block::Tool(call)
                        if call.output_content().len() > EXPANDED_ROWS)
            })
            .collect();
        for index in finishing {
            match self.transcript.block_mut(index) {
                Some(Block::Assistant { streaming, .. }) => *streaming = false,
                Some(Block::Tool(call)) => call.settle(),
                _ => {}
            }
        }
        let usage = wait
            .usage
            .clone()
            .or_else(|| accounted_usage.clone())
            .unwrap_or_default();
        if wait.usage.is_some()
            && let Some(previous) = accounted_usage
        {
            self.usage.replace_contribution(&previous, &usage);
            if let Some(live) = self
                .live
                .iter_mut()
                .chain(&mut self.ended_live)
                .find(|live| live.turn.loop_id == *loop_id)
            {
                live.accounted_usage = usage.clone();
                live.usage_final = true;
            }
        }
        let tools = self.transcript.blocks()[start..end]
            .iter()
            .filter(|block| matches!(block, Block::Tool(_)))
            .count() as u64;
        // A completion that arrived before this wait already closed the turn
        // with its own line; the wait only fills in what it alone knows.
        match self.transcript.blocks().iter().rposition(
            |block| matches!(block, Block::TurnEnd { loop_id, .. } if loop_id == &wait.turn.loop_id),
        ) {
            Some(at) => {
                if let Some(Block::TurnEnd { duration, requests, tools: t, usage: u, .. }) =
                    self.transcript.block_mut(at)
                {
                    *duration = elapsed.or(Some(Duration::ZERO));
                    *requests = (*requests).max(wait.requests.unwrap_or(0));
                    *t = (*t).max(tools);
                    if u.input_tokens.is_none() && u.output_tokens.is_none() {
                        u.clone_from(&usage);
                    }
                }
            }
            None => {
                self.push_in_turn(loop_id, Block::TurnEnd {
                    loop_id: wait.turn.loop_id.clone(),
                    outcome: wait.outcome.clone(),
                    duration: elapsed.or(Some(Duration::ZERO)),
                    requests,
                    tools,
                    usage,
                    changes: None,
                    saving: None,
                });
            }
        }
        Some(gap || !wait.outcome.is_completed())
    }

    /// Installs a completion's verified body: the region the turn streamed into
    /// is replaced by its canonical items, and the turn-end line states the
    /// saving fact the result reported. `turn` is the directory's description,
    /// which also places a turn the client never streamed.
    pub fn reconcile_completion(
        &mut self,
        turn: &crate::protocol::CompletedTurn,
        outcome: &crate::protocol::LoopOutcome,
        items: &[RawHistoryItem],
        interrupted: Option<&InterruptedRequest>,
        persistence: TurnPersistence,
        persistence_uncertain: bool,
    ) -> Option<(usize, isize, usize)> {
        // A turn's canonical body is installed once; a second read of it, from
        // the completion directory or a late outcome, changes nothing.
        if self.is_closed(&turn.loop_id) {
            self.saving_note(&turn.loop_id, persistence, persistence_uncertain);
            return None;
        }
        let streamed = self.transcript.blocks().iter().any(
            |block| matches!(block, Block::TurnEnd { loop_id, .. } if *loop_id == turn.loop_id),
        );
        if !self
            .reconcile_from
            .iter()
            .any(|(id, _)| id == &turn.loop_id)
            && (self
                .live
                .iter()
                .chain(&self.ended_live)
                .any(|live| live.turn.loop_id == turn.loop_id))
        {
            return None;
        }
        if streamed
            && !self
                .reconcile_from
                .iter()
                .any(|(id, _)| id == &turn.loop_id)
        {
            let range = turn_bounds(self.transcript.blocks(), &turn.loop_id)
                .expect("a streamed turn has its end");
            self.record_region(&turn.loop_id, range.start);
        }
        let mut early_end = None;
        if !streamed
            && !self
                .reconcile_from
                .iter()
                .any(|(id, _)| id == &turn.loop_id)
        {
            let start = self.completion_position(turn)?;
            let before = self.transcript.len();
            self.record_region(&turn.loop_id, start);
            self.place_early(&turn.loop_id);
            early_end = Some(start + self.transcript.len() - before);
        }
        let placed = match streamed {
            true => self.reconcile_turn(&turn.loop_id, items, interrupted),
            false => Some(self.place_completion(turn, outcome, items, interrupted, early_end)),
        };
        // A completion that installed nothing leaves the turn repairable and
        // states no saving fact: the region it would have replaced is gone, so
        // closing it here would freeze the streamed body and still label it
        // saved.
        if let Some((start, _, _)) = placed {
            #[cfg(feature = "e2e-observe")]
            for index in start..self.transcript.len() {
                if matches!(
                    self.transcript.blocks()[index],
                    Block::User {
                        kind: UserKind::Prompt,
                        ..
                    }
                ) && self
                    .transcript
                    .user_identity(index)
                    .is_some_and(|user| user.loop_id == turn.loop_id)
                {
                    self.transcript
                        .identify_user(index, &turn.loop_id, turn.input_id.as_deref());
                }
            }
            for live in &mut self.ended_live {
                if live.turn.loop_id == turn.loop_id {
                    live.start = start;
                }
            }
            self.saving_note(&turn.loop_id, persistence, persistence_uncertain);
            self.close_turn(&turn.loop_id);
            self.completion_order
                .push_back((turn.seq, turn.loop_id.clone()));
            if self.completion_order.len() > CLOSED_TURNS {
                self.completion_order.pop_front();
            }
        }
        placed
    }

    /// Inserts a turn the client never streamed where the directory says it
    /// belongs: after the last turn that was committed before it.
    fn place_completion(
        &mut self,
        turn: &crate::protocol::CompletedTurn,
        outcome: &crate::protocol::LoopOutcome,
        items: &[RawHistoryItem],
        interrupted: Option<&InterruptedRequest>,
        early_end: Option<usize>,
    ) -> (usize, isize, usize) {
        let mut durable = Transcript::default();
        for item in items {
            append_history_item(&mut durable, item, &self.info.session_id);
        }
        if let Some(partial) = interrupted {
            push_interrupted(&mut durable, &turn.loop_id, partial);
        }
        durable.push(Block::TurnEnd {
            loop_id: turn.loop_id.clone(),
            outcome: outcome.clone(),
            duration: None,
            requests: 0,
            tools: 0,
            usage: Usage::default(),
            changes: None,
            saving: None,
        });
        // A turn this client began has a recorded region even when its line
        // never arrived (a dropped event gap); replacing that region keeps the
        // prompt and the partial answer from being duplicated.
        let start = self
            .reconcile_from
            .iter()
            .find(|(id, _)| id == &turn.loop_id)
            .expect("a confirmed completion has its region")
            .1;
        let end = early_end.unwrap_or_else(|| self.region_end(&turn.loop_id));
        let merged = Transcript::merged(durable, &self.transcript, start..end);
        let grown = merged.blocks.len() as isize - (end - start) as isize;
        self.transcript.splice_merged(start..end, merged);
        self.reconcile_from.retain(|(id, _)| id != &turn.loop_id);
        self.rebase_range(start, end, grown);
        (start, grown, end)
    }

    /// Locates an unseen completion from confirmed sequence and saved-history anchors.
    fn completion_position(&mut self, turn: &crate::protocol::CompletedTurn) -> Option<usize> {
        let mut start = match &turn.after_saved_loop_id {
            Some(anchor) => match self.transcript.blocks().iter().position(
                |block| matches!(block, Block::TurnEnd { loop_id, .. } if loop_id == anchor),
            ) {
                Some(at) => at + 1,
                None => {
                    self.ordering_gap = true;
                    return None;
                }
            },
            None if self.loaded_from > 0 => {
                self.ordering_gap = true;
                return None;
            }
            None => self
                .transcript
                .blocks()
                .iter()
                .position(|block| {
                    !matches!(
                        block,
                        Block::EarlierHistory { .. } | Block::Compaction { .. }
                    )
                })
                .unwrap_or(self.transcript.len()),
        };
        for (_, prior) in self
            .completion_order
            .iter()
            .filter(|(seq, _)| *seq < turn.seq)
        {
            if let Some(at) =
                self.transcript.blocks().iter().position(
                    |block| matches!(block,Block::TurnEnd { loop_id,.. } if loop_id == prior),
                )
            {
                start = start.max(at + 1);
            }
        }
        let next = self
            .catalog
            .entries
            .iter()
            .chain(self.recovery.entries.values().map(|entry| &entry.turn))
            .filter(|entry| entry.seq > turn.seq)
            .filter_map(|next| {
                turn_bounds(self.transcript.blocks(), &next.loop_id)
                    .map(|range| range.start)
                    .or_else(|| {
                        self.reconcile_from
                            .iter()
                            .find(|(id, _)| id == &next.loop_id)
                            .map(|(_, at)| *at)
                    })
            })
            .min();
        if next.is_some_and(|next| start > next) {
            self.ordering_gap = true;
            return None;
        }
        Some(start)
    }

    /// Records on the turn-end line whether the result was saved, and whether
    /// an uncertain write is still in doubt.
    fn saving_note(
        &mut self,
        loop_id: &str,
        persistence: TurnPersistence,
        persistence_uncertain: bool,
    ) {
        let index = self.transcript.blocks().iter().rposition(
            |block| matches!(block, Block::TurnEnd { loop_id: id, .. } if id == loop_id),
        );
        let Some(Block::TurnEnd { saving: slot, .. }) =
            index.and_then(|index| self.transcript.block_mut(index))
        else {
            return;
        };
        *slot = Some(match (persistence, persistence_uncertain) {
            (TurnPersistence::Persisted, _) => crate::transcript::Saving::Saved,
            (TurnPersistence::Failed, true) => crate::transcript::Saving::Uncertain,
            (TurnPersistence::Failed, false) => crate::transcript::Saving::Unsaved,
        });
    }

    /// Closes every body entry of a turn whose canonical content is installed,
    /// so a late delta cannot duplicate it.
    fn close_turn(&mut self, loop_id: &str) {
        self.closed.retain(|id| id != loop_id);
        self.closed.push_back(loop_id.to_owned());
        if self.closed.len() > CLOSED_TURNS {
            self.closed.pop_front();
        }
    }

    /// Whether a turn's body is already authoritative.
    pub fn is_closed(&self, loop_id: &str) -> bool {
        self.closed.iter().any(|id| id == loop_id)
    }

    /// The loop the last control snapshot saw executing; it differs from
    /// `live` while a waited turn has not answered yet.
    pub fn observed_loop(&self) -> Option<&str> {
        self.execution
            .as_ref()
            .and_then(|execution| execution.loop_id())
    }

    /// Replaces the region a finished turn streamed into, up to its turn-end
    /// line, with its durable items and what only live events knew. Blocks
    /// after the line, such as a follow-up already sent, stay as they are.
    /// Returns where the region ended and how much it grew.
    pub fn reconcile_turn(
        &mut self,
        loop_id: &str,
        items: &[RawHistoryItem],
        interrupted: Option<&InterruptedRequest>,
    ) -> Option<(usize, isize, usize)> {
        let at = self
            .reconcile_from
            .iter()
            .position(|(id, _)| id == loop_id)?;
        let start = self.reconcile_from[at].1;
        // The region ends at this turn's line, so a successor already started
        // after it keeps its place; the line itself is replaced because the
        // The region ends before this turn's own line, which stays: it is where
        // the wait recorded the outcome, duration, counts and usage, and where
        // `saving_note` states what the result reported. A successor that
        // already started keeps its blocks beyond it.
        let end = self.transcript.blocks()[start..]
            .iter()
            .position(|block| matches!(block, Block::TurnEnd { loop_id: id, .. } if id == loop_id))
            .map(|at| start + at)?;
        self.reconcile_from.remove(at);
        let mut durable = Transcript::default();
        for item in items {
            append_history_item(&mut durable, item, &self.info.session_id);
        }
        if let Some(partial) = interrupted {
            push_interrupted(&mut durable, loop_id, partial);
        }
        let merged = Transcript::merged(durable, &self.transcript, start..end);
        let grown = merged.blocks.len() as isize - (end - start) as isize;
        self.transcript.splice_merged(start..end, merged);
        // The region is replaced in place, so a boundary inside it keeps the
        // region's first block instead of moving past the new content.
        self.rebase_range(start, end, grown);
        Some((start, grown, end))
    }

    /// Moves every boundary for a region `start..end` replaced by `grown` more
    /// blocks: before it stays, inside it lands on `start`, after it moves.
    fn rebase_range(&mut self, start: usize, end: usize, grown: isize) {
        let shift = |index: &mut usize| {
            *index = match *index {
                // Strictly inside the replaced region: the region's first block
                // is the nearest boundary that still exists.
                inside if inside > start && inside < end => start,
                // At or after the region's end: a successor that started there
                // keeps its own blocks, which now sit after the new body.
                after if after >= end => after.saturating_add_signed(grown),
                before => before,
            };
        };
        if let Some(live) = self.live.as_mut() {
            shift(&mut live.start);
        }
        for live in &mut self.ended_live {
            shift(&mut live.start);
        }
        for (_, at) in &mut self.reconcile_from {
            shift(at);
        }
    }

    /// Makes the inputs a running turn took steering blocks, or holds them for a turn not live yet.
    fn deliver(&mut self, turn: TurnRef, items: Vec<DeliveredInput>) {
        if self.knows_turn(&turn.loop_id) {
            self.place_steers(&turn.loop_id, items);
        } else {
            let bytes = items
                .iter()
                .map(|item| item.text.len() + item.id.len() + 128)
                .sum();
            self.cache_early(
                AgentEvent::InputDelivered {
                    meta: crate::protocol::event::EventMeta {
                        session_id: turn.session_id.clone(),
                        load: self.load.clone(),
                        dropped_before: 0,
                    },
                    turn,
                    items,
                },
                bytes,
            );
        }
    }

    /// Places what was delivered before the turn `loop_id` became live.
    pub fn place_early(&mut self, loop_id: &str) {
        let held = std::mem::take(&mut self.early);
        for (event, bytes) in held {
            if event.loop_id() == Some(loop_id) {
                self.early_bytes -= bytes;
                self.apply_event(event);
            } else {
                self.early.push_back((event, bytes));
            }
        }
    }

    pub(super) fn knows_turn(&self, loop_id: &str) -> bool {
        self.is_closed(loop_id)
            || self
                .live
                .as_ref()
                .is_some_and(|live| live.turn.loop_id == loop_id)
            || self.reconcile_from.iter().any(|(id, _)| id == loop_id)
    }

    pub(super) fn cache_early(&mut self, event: AgentEvent, bytes: usize) {
        if bytes > EARLY_BYTES {
            self.early_gap = true;
            self.recovery.behind = true;
            return;
        }
        while self.early_bytes + bytes > EARLY_BYTES || {
            let loops: BTreeSet<_> = self
                .early
                .iter()
                .filter_map(|(event, _)| event.loop_id())
                .collect();
            !event
                .loop_id()
                .is_some_and(|loop_id| loops.contains(loop_id))
                && loops.len() >= EARLY_TURNS
        } {
            self.evict_early();
        }
        self.early_bytes += bytes;
        self.early.push_back((event, bytes));
        #[cfg(feature = "e2e-observe")]
        {
            self.early_peak_bytes = self.early_peak_bytes.max(self.early_bytes);
        }
    }

    pub(super) fn evict_early(&mut self) {
        if let Some((_, bytes)) = self.early.pop_front() {
            self.early_bytes -= bytes;
            self.early_gap = true;
            self.recovery.behind = true;
        }
    }

    pub(super) fn replay_early(&mut self) {
        let loops: Vec<String> = self
            .early
            .iter()
            .filter_map(|(event, _)| event.loop_id())
            .filter(|id| self.knows_turn(id))
            .map(str::to_owned)
            .collect();
        for loop_id in loops {
            self.place_early(&loop_id);
        }
    }

    fn region_end(&self, loop_id: &str) -> usize {
        let start = self
            .reconcile_from
            .iter()
            .find(|(id, _)| id == loop_id)
            .map(|(_, start)| *start)
            .or_else(|| turn_bounds(self.transcript.blocks(), loop_id).map(|range| range.start))
            .unwrap_or(self.transcript.len());
        self.reconcile_from
            .iter()
            .filter(|(id, at)| id != loop_id && *at >= start)
            .map(|(_, at)| *at)
            .chain(self.transcript.blocks()[start..].iter().position(|block| matches!(block, Block::TurnEnd { loop_id: id, .. } if id == loop_id)).map(|at| start + at))
            .min()
            .unwrap_or(self.transcript.len())
    }

    fn push_in_turn(&mut self, loop_id: &str, block: Block) {
        let end = self.region_end(loop_id);
        let start = self
            .reconcile_from
            .iter()
            .find(|(id, _)| id == loop_id)
            .map_or(end, |(_, at)| *at);
        let at = match &block {
            Block::Assistant { key, .. } => {
                self.request_position(loop_id, start..end, key.request_index, true)
            }
            Block::Tool(call) => {
                self.request_position(loop_id, start..end, call.key.request_index, false)
            }
            _ => end,
        };
        self.insert_in_turn(loop_id, at, block);
    }

    fn request_position(
        &self,
        loop_id: &str,
        range: std::ops::Range<usize>,
        request: u32,
        assistant: bool,
    ) -> usize {
        range
            .clone()
            .find(|index| match &self.transcript.blocks()[*index] {
                Block::User {
                    kind: UserKind::Steering,
                    ..
                } => self.transcript.user_identity(*index).is_some_and(|user| {
                    user.loop_id == loop_id
                        && user
                            .request_index
                            .is_some_and(|boundary| boundary > request)
                }),
                Block::Assistant { key, .. } => {
                    key.loop_id == loop_id && key.request_index > request
                }
                Block::Tool(call) => {
                    call.key.loop_id == loop_id
                        && (call.key.request_index > request
                            || assistant && call.key.request_index == request)
                }
                _ => false,
            })
            .unwrap_or(range.end)
    }

    fn insert_in_turn(&mut self, loop_id: &str, at: usize, block: Block) {
        for (id, start) in &mut self.reconcile_from {
            if id != loop_id && *start >= at {
                *start += 1;
            }
        }
        if let Some(live) = &mut self.live
            && live.turn.loop_id != loop_id
            && live.start >= at
        {
            live.start += 1;
        }
        for live in &mut self.ended_live {
            if live.turn.loop_id != loop_id && live.start >= at {
                live.start += 1;
            }
        }
        self.transcript.insert(at, block);
    }

    fn ensure_assistant(&mut self, loop_id: &str, request_index: u32) {
        let key = RequestKey {
            loop_id: loop_id.to_owned(),
            request_index,
        };
        if !self.transcript.blocks().iter().any(
            |block| matches!(block, Block::Assistant { key: existing, .. } if *existing == key),
        ) {
            self.push_in_turn(
                loop_id,
                Block::Assistant {
                    key,
                    text: String::new(),
                    reasoning: String::new(),
                    streaming: true,
                    expanded: None,
                },
            );
        }
    }

    fn place_steers(&mut self, loop_id: &str, items: Vec<DeliveredInput>) {
        for item in items {
            if self.delivered.iter().any(|known| known.input_id == item.id) {
                continue;
            }
            let end = self.region_end(loop_id);
            let start = self
                .reconcile_from
                .iter()
                .find(|(id, _)| id == loop_id)
                .map_or(end, |(_, at)| *at);
            let at = (start..end)
                .find(|index| match &self.transcript.blocks()[*index] {
                    Block::User {
                        kind: UserKind::Steering,
                        ..
                    } => self.transcript.user_identity(*index).is_some_and(|user| {
                        user.loop_id == loop_id
                            && user
                                .request_index
                                .is_some_and(|boundary| boundary > item.request_index)
                    }),
                    Block::Assistant { key, .. } => {
                        key.loop_id == loop_id && key.request_index >= item.request_index
                    }
                    Block::Tool(call) => {
                        call.key.loop_id == loop_id && call.key.request_index >= item.request_index
                    }
                    _ => false,
                })
                .unwrap_or(end);
            self.delivered.push_back(DeliveredIdentity {
                #[cfg(feature = "e2e-observe")]
                loop_id: loop_id.into(),
                input_id: item.id.clone(),
                #[cfg(feature = "e2e-observe")]
                request_index: item.request_index,
            });
            if self.delivered.len() > 256 {
                self.delivered.pop_front();
            }
            self.insert_in_turn(
                loop_id,
                at,
                Block::User {
                    text: item.text,
                    kind: UserKind::Steering,
                    at: Some(accepted_millis(item.accepted_at.as_deref())),
                },
            );
            self.transcript
                .identify_steer(at, loop_id, &item.id, item.request_index);
        }
    }

    /// The source items not loaded as themselves. The summary stands in for
    /// the item at the display floor, which still has to be read.
    pub fn hidden_items(&self) -> usize {
        self.loaded_from + usize::from(self.loaded_from == self.first_item)
    }

    /// Replaces the transcript with the history tail.
    pub fn install_tail(&mut self, page: History) -> Vec<(std::ops::Range<usize>, usize, bool)> {
        let old = std::mem::take(&mut self.transcript);
        let pending = std::mem::take(&mut self.reconcile_from);
        let mut regions = pending.clone();
        let mut from = 0;
        for (at, block) in old.blocks().iter().enumerate() {
            if let Block::TurnEnd { loop_id, .. } = block {
                if !regions.iter().any(|(id, _)| id == loop_id) {
                    regions.push((loop_id.clone(), from));
                }
                from = at + 1;
            }
        }
        if let Some(live) = &self.live
            && !regions.iter().any(|(id, _)| id == &live.turn.loop_id)
        {
            regions.push((live.turn.loop_id.clone(), live.start));
        }
        regions.sort_by_key(|(_, start)| *start);
        let mut full_turns = BTreeSet::new();
        for (index, item) in page.items.iter().enumerate() {
            if let RuntimeItem::User(user) = &item.item
                && user.kind == UserKind::Prompt
            {
                let bounded_end = page.items[index+1..].iter().any(|item| matches!(&item.item, RuntimeItem::User(next) if next.kind == UserKind::Prompt)) || page.end == page.pin.total;
                if bounded_end
                    && page
                        .records
                        .iter()
                        .any(|record| record.loop_id == user.loop_id)
                {
                    full_turns.insert(user.loop_id.clone());
                }
            }
        }
        if !page.canonical {
            self.first_item = page.floor;
        }
        if page.from <= self.loaded_end && self.loaded_from <= page.end {
            self.loaded_from = self.loaded_from.min(page.from);
            self.loaded_end = self.loaded_end.max(page.end);
        } else {
            self.loaded_from = page.from;
            self.loaded_end = page.end;
        }
        self.transcript =
            Transcript::from_history(&page.items, &page.records, &self.info.session_id, page.from)
                .with_previous_history(&old, page.from..page.end);
        for item in &page.items {
            if matches!(item.item, RuntimeItem::ToolResult(_)) {
                append_history_item(&mut self.transcript, item, &self.info.session_id);
            }
        }
        // With the whole display window loaded, its turns and the summary's
        // cover are every turn there is; anything less would be a wrong total.
        let covered = match page.floor {
            0 => Some(Usage::default()),
            _ => page.covered_usage,
        };
        if page.from <= page.floor
            && self.usage == UsageTotals::default()
            && let Some(mut total) = covered
        {
            for record in &page.records {
                total.add(&record.usage);
            }
            self.usage.add(&total);
        }
        if let Some(coverage) = self.summary_coverage {
            self.cover_replayed_compaction(coverage);
        }
        // `from` counts even when an automatic compaction covers the whole
        // display tail: the history behind it is still there to read.
        if self.loaded_from > 0 {
            let hidden_items = self.hidden_items();
            self.transcript
                .insert(0, Block::EarlierHistory { hidden_items });
        }
        let mut placements: Vec<(String, usize)> = Vec::new();
        for (index, (loop_id, start)) in regions.iter().enumerate() {
            let end = regions
                .get(index + 1)
                .map_or(old.len(), |(_, start)| *start);
            let existing = turn_bounds(self.transcript.blocks(), loop_id);
            if (full_turns.contains(loop_id)
                || (*start..end).any(|at| old.history_source(at).is_some()))
                && let Some(range) = existing
            {
                let mut durable = Transcript::default();
                durable.splice_copy(0..0, &self.transcript, range.clone());
                let merged = Transcript::merged(durable, &old, *start..end);
                let delta = merged.blocks.len() as isize - range.len() as isize;
                for (_, at) in &mut placements {
                    if *at >= range.end {
                        *at = at.saturating_add_signed(delta);
                    }
                }
                placements.push((loop_id.clone(), range.start));
                self.transcript.splice_merged(range, merged);
            } else {
                if let Some(range) = existing {
                    for (_, at) in &mut placements {
                        if *at >= range.end {
                            *at -= range.len();
                        }
                    }
                    self.transcript.splice(range, Vec::new());
                }
                let description = self
                    .catalog
                    .entries
                    .iter()
                    .find(|turn| turn.loop_id == *loop_id)
                    .cloned()
                    .or_else(|| {
                        self.recovery
                            .entries
                            .values()
                            .find(|entry| entry.turn.loop_id == *loop_id)
                            .map(|entry| entry.turn.clone())
                    });
                let at = if self
                    .live
                    .as_ref()
                    .is_some_and(|live| live.turn.loop_id == *loop_id)
                {
                    self.transcript.len()
                } else if let Some(turn) = description {
                    self.completion_position(&turn).unwrap_or_else(|| {
                        self.ordering_gap = true;
                        self.transcript.len()
                    })
                } else {
                    // A retained earlier region is not covered by a tail page.
                    placements
                        .last()
                        .and_then(|(id, _)| {
                            turn_bounds(self.transcript.blocks(), id).map(|range| range.end)
                        })
                        .unwrap_or_else(|| {
                            self.transcript
                                .blocks()
                                .iter()
                                .position(|block| !matches!(block, Block::EarlierHistory { .. }))
                                .unwrap_or(self.transcript.len())
                        })
                };
                for (_, placed) in &mut placements {
                    if *placed >= at {
                        *placed += end - *start;
                    }
                }
                placements.push((loop_id.clone(), at));
                self.transcript.splice_copy(at..at, &old, *start..end);
            }
        }
        if regions.is_empty() && !old.is_empty() {
            let durable = std::mem::take(&mut self.transcript);
            let merged = Transcript::merged(durable, &old, 0..old.len());
            self.transcript.splice_merged(0..0, merged);
        }
        let mut mapping = Vec::new();
        for (index, (loop_id, start)) in regions.iter().enumerate() {
            let end = regions
                .get(index + 1)
                .map_or(old.len(), |(_, start)| *start);
            let new_start = placements
                .iter()
                .find(|(id, _)| id == loop_id)
                .expect("every retained region was placed")
                .1;
            if let Some(live) = &mut self.live
                && live.turn.loop_id == *loop_id
            {
                live.start = new_start;
            }
            for live in &mut self.ended_live {
                if live.turn.loop_id == *loop_id {
                    live.start = new_start;
                }
            }
            mapping.push((*start..end, new_start, full_turns.contains(loop_id)));
        }
        self.reconcile_from = pending
            .into_iter()
            .map(|(id, _)| {
                let start = placements
                    .iter()
                    .find(|(placed, _)| placed == &id)
                    .expect("every pending region was placed")
                    .1;
                (id, start)
            })
            .collect();
        mapping
    }

    /// Puts an older page in front of the transcript in place of the marker;
    /// returns how much the front grew, negative when the marker went alone,
    /// and the cards of a canonical page, which carry no detail of their own.
    pub fn prepend_page(&mut self, page: History) -> (isize, Vec<ToolRef>) {
        self.loading_older = false;
        // A turn cut by the page's end already has its line from the later window.
        let records: Vec<TurnRecord> = page
            .records
            .iter()
            .filter(|record| {
                !self.transcript.blocks().iter().any(
                    |block| matches!(block, Block::TurnEnd { loop_id, .. } if *loop_id == record.loop_id),
                )
            })
            .cloned()
            .collect();
        let scratch =
            Transcript::from_history(&page.items, &records, &self.info.session_id, page.from);
        let bare_cards = match page.canonical {
            true => scratch.tool_calls().map(|call| call.key.clone()).collect(),
            false => Vec::new(),
        };
        let added = scratch.len();
        let replaced = usize::from(matches!(
            self.transcript.blocks().first(),
            Some(Block::EarlierHistory { .. })
        ));
        self.transcript.remove_front(replaced);
        self.transcript.splice_transcript(0..0, scratch);
        let exhausted = added == 0 && (page.canonical || replaced == 0);
        // A compaction can raise the display floor above the loaded rows; the
        // read position never moves past them, or the items between are lost.
        self.loaded_from = match exhausted {
            true => 0,
            false => page.from.min(self.loaded_from),
        };
        if !page.canonical {
            self.first_item = page.floor;
        }
        let marker = self.loaded_from > 0
            && (page.canonical || page.has_more || self.loaded_from <= self.first_item);
        if marker {
            let hidden_items = self.hidden_items();
            self.transcript
                .insert(0, Block::EarlierHistory { hidden_items });
        }
        let grown = added as isize - replaced as isize + isize::from(marker);
        self.rebase(0, grown);
        (grown, bare_cards)
    }

    /// Moves every boundary at or after `at` by `delta` blocks inserted or
    /// removed before it; a page or a reconcile can land at any point around
    /// a turn.
    fn rebase(&mut self, at: usize, delta: isize) {
        let shift = |index: &mut usize| {
            if *index >= at {
                *index = index.saturating_add_signed(delta);
            }
        };
        if let Some(live) = self.live.as_mut() {
            shift(&mut live.start);
        }
        for live in &mut self.ended_live {
            shift(&mut live.start);
        }
        for (_, start) in &mut self.reconcile_from {
            shift(start);
        }
    }

    /// Takes a `session.context` report: compaction progress, the last result
    /// as a marker, and the coverage of the current summary. Returns the outcome
    /// of a turn-start compaction that failed since the previous report.
    pub fn apply_context(&mut self, context: SessionContext) -> Option<String> {
        let automatic = context.automatic.as_ref();
        // The first report only sets the baseline: an earlier attempt is
        // already in the history that was read.
        let mut failure = None;
        if let Some(last) = automatic.and_then(|view| view.last.as_ref())
            && self.auto_seen.replace(last.operation_id.clone()).as_ref()
                != Some(&last.operation_id)
            && self.context.is_some()
        {
            if last.outcome == "compacted" {
                let result = CompactionResult {
                    operation_id: last.operation_id.clone(),
                    status: CompactStatus::Compacted,
                    before_tokens: last.before_tokens,
                    after_tokens: last.after_tokens,
                    covered_loop_count: None,
                    covered_item_count: None,
                    retained_item_count: None,
                    failure_kind: None,
                    next: None,
                };
                self.push_compaction(&result, None);
            } else if !last.outcome.starts_with("compacted") && last.outcome != "cancelled" {
                failure = Some(last.outcome.clone());
            }
        }
        let coverage = context
            .coverage
            .as_ref()
            .filter(|coverage| coverage.covered_item_count > 0)
            .map(|coverage| (coverage.covered_item_count, coverage.retained_item_count));
        if let Some(result) = &context.last_result {
            self.push_compaction(result, coverage);
        }
        if let Some(coverage) = coverage {
            self.cover_replayed_compaction(coverage);
        }
        self.context = Some(context);
        failure
    }

    /// Adds the one-line compaction marker, once per operation: the manual
    /// response and later context reads can report the same compaction.
    pub fn push_compaction(&mut self, result: &CompactionResult, coverage: Option<(usize, usize)>) {
        let known = self.transcript.blocks().iter().any(|block| {
            matches!(block, Block::Compaction { operation_id, .. } if *operation_id == result.operation_id)
        });
        if result.status != CompactStatus::Compacted || known {
            return;
        }
        self.transcript.push(Block::Compaction {
            operation_id: result.operation_id.clone(),
            before: result.before_tokens,
            after: result.after_tokens,
            coverage: result
                .covered_item_count
                .map(|covered| (Some(covered), result.retained_item_count))
                .or(coverage.map(|(covered, kept)| (Some(covered), Some(kept)))),
            summary: None,
        });
    }

    /// Gives the summary replayed from history the counts `session.context`
    /// reports for it.
    fn cover_replayed_compaction(&mut self, coverage: (usize, usize)) {
        self.summary_coverage = Some(coverage);
        let index = self.transcript.blocks().iter().rposition(|block| {
            matches!(block, Block::Compaction { operation_id, coverage: None, .. } if operation_id.is_empty())
        });
        if let Some(Block::Compaction { coverage: slot, .. }) =
            index.and_then(|index| self.transcript.block_mut(index))
        {
            *slot = Some((Some(coverage.0), Some(coverage.1)));
        }
    }
}

fn turn_bounds(blocks: &[Block], loop_id: &str) -> Option<std::ops::Range<usize>> {
    let end = blocks
        .iter()
        .position(|block| matches!(block, Block::TurnEnd { loop_id: id, .. } if id == loop_id))?
        + 1;
    let start = blocks[..end - 1]
        .iter()
        .rposition(|block| matches!(block, Block::TurnEnd { .. }))
        .map_or(0, |at| at + 1);
    Some(start..end)
}

/// The Agent's acceptance time of an input in Unix milliseconds, else now.
pub(super) fn accepted_millis(stamp: Option<&str>) -> u64 {
    stamp.and_then(unix_millis).unwrap_or_else(now_millis)
}

/// The first meaningful line of a prompt, cut at a word boundary near 60
/// cells.
pub fn auto_title(prompt: &str) -> Option<String> {
    let first = prompt.lines().find(|line| !line.trim().is_empty())?.trim();
    let first = first.trim_start_matches(['#', '>', '-', '*', ' ']);
    if first.is_empty() {
        return None;
    }
    let mut title = String::new();
    let mut cells = 0;
    for word in first.split_whitespace() {
        let width = unicode_width::UnicodeWidthStr::width(word) + usize::from(!title.is_empty());
        if cells + width > 60 {
            break;
        }
        if !title.is_empty() {
            title.push(' ');
        }
        title.push_str(word);
        cells += width;
    }
    Some(if title.is_empty() {
        first.chars().take(60).collect()
    } else if title == first {
        title
    } else {
        format!("{title}…")
    })
}

pub(crate) fn input_budget(context: Option<&SessionContext>) -> Option<u64> {
    context?
        .budget
        .as_ref()?
        .input_budget_tokens
        .filter(|budget| *budget > 0)
}

/// The last reported context usage as a percentage of its input budget or model window.
pub fn context_percent(session: &Session) -> Option<f64> {
    let usage = session.presentation.as_ref()?.context.as_ref()?;
    reported_percent(usage, session.context.as_ref())
}

/// Reported usage as a percentage of the input budget in `context`, else of the model window.
pub fn reported_percent(usage: &ContextUsage, context: Option<&SessionContext>) -> Option<f64> {
    let tokens = usage.tokens?;
    match input_budget(context) {
        Some(budget) => Some(tokens as f64 / budget as f64 * 100.0),
        None => usage.percent.or_else(|| {
            usage
                .window
                .filter(|window| *window > 0)
                .map(|window| tokens as f64 / window as f64 * 100.0)
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{InputMode, QueueItem};

    fn session() -> Session {
        Session::new(
            serde_json::from_value(crate::testing::session_info("s")).unwrap(),
            1,
        )
    }

    fn item(id: &str, mode: InputMode, text: &str) -> QueueItem {
        QueueItem {
            id: id.into(),
            mode,
            text: text.into(),
            accepted_at: None,
        }
    }

    fn delivered(id: &str, text: &str) -> DeliveredInput {
        DeliveredInput {
            id: id.into(),
            text: text.into(),
            accepted_at: None,
            request_index: 1,
        }
    }

    fn steering(session: &Session) -> Vec<&str> {
        session
            .transcript
            .blocks()
            .iter()
            .filter_map(|block| match block {
                Block::User {
                    text,
                    kind: UserKind::Steering,
                    ..
                } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn delivered_inputs_project_body_without_writing_the_snapshot_queue() {
        let mut session = session();
        session.live = Some(LiveTurn::new(TurnRef::new("s", "l"), 0));
        session.queue = Queue {
            items: vec![
                item("a", InputMode::Steer, "first"),
                item("b", InputMode::Steer, "second"),
                item("c", InputMode::FollowUp, "later"),
            ],
            paused: false,
        };
        session.deliver(
            TurnRef::new("s", "l"),
            vec![delivered("a", "first"), delivered("b", "second")],
        );
        assert_eq!(steering(&session), ["first", "second"]);
        let queued: Vec<&str> = session.queue.items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(queued, ["a", "b", "c"]);
    }

    #[test]
    fn inputs_delivered_before_the_turn_is_live_are_placed_when_it_starts() {
        let mut session = session();
        session.deliver(TurnRef::new("s", "l"), vec![delivered("a", "early")]);
        assert!(steering(&session).is_empty());
        session.live = Some(LiveTurn::new(TurnRef::new("s", "l"), 0));
        session.place_early("other");
        assert!(steering(&session).is_empty(), "another turn's inputs wait");
        session.place_early("l");
        assert_eq!(steering(&session), ["early"]);
        session.place_early("l");
        assert_eq!(steering(&session), ["early"], "placed once");
    }

    #[test]
    fn a_turn_ends_once_and_leaves_the_queue_to_the_agent() {
        let mut session = session();
        session.live = Some(LiveTurn::new(TurnRef::new("s", "l"), 0));
        session.queue = Queue {
            items: vec![item("a", InputMode::Steer, "steer")],
            paused: false,
        };
        let wait: TurnWaitResult = serde_json::from_value(serde_json::json!({
            "turn": {"session_id": "s", "loop_id": "l"}, "outcome": {"type": "completed"}
        }))
        .unwrap();
        assert_eq!(session.end_turn(&wait), Some(false));
        assert_eq!(session.end_turn(&wait), None, "the turn ends once");
        assert_eq!(session.queue.items.len(), 1);
    }

    #[test]
    fn titles_take_the_first_meaningful_line() {
        assert_eq!(
            auto_title("\n\n# Fix the parser\nand more"),
            Some("Fix the parser".into())
        );
        assert_eq!(auto_title("   \n \n"), None);
        assert_eq!(auto_title(&"a".repeat(200)).unwrap().chars().count(), 60);
    }

    #[test]
    fn long_titles_cut_at_a_word_boundary() {
        let title = auto_title(
            "Refactor the parser so that it can recover from a missing brace and report the line",
        )
        .unwrap();
        assert!(title.starts_with("Refactor the parser"), "{title:?}");
        assert!(title.ends_with('…'), "{title:?}");
        assert!(unicode_width::UnicodeWidthStr::width(title.as_str()) <= 61);
    }

    #[test]
    fn only_an_untitled_session_gets_an_auto_title() {
        let mut session = session();
        session.transcript.push(Block::User {
            text: "fix it".into(),
            kind: UserKind::Prompt,
            at: None,
        });
        assert_eq!(session.auto_title().as_deref(), Some("fix it"));
        session.info.title = Some("named".into());
        assert_eq!(session.auto_title(), None);
        session.info.title = Some("  ".into());
        assert_eq!(session.auto_title().as_deref(), Some("fix it"));
    }
}
