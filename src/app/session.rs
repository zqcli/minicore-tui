//! The resident session: transcript, live turn, queue, usage and status.

use std::time::{Duration, Instant};

use crate::protocol::event::{PreviewState, ToolOutcome};
use crate::protocol::read::{RawHistoryItem, UserKind};
use crate::protocol::{
    AgentEvent, CompactStatus, CompactionProgress, CompactionResult, ModelInfo, SessionContext,
    SessionInfo, SessionPresentation, ToolRef, TurnRef, TurnWaitResult, UsageTotals,
};
use crate::transcript::block::append_history_item;
use crate::transcript::{Block, RequestKey, ToolCall, ToolState, Transcript};

use super::tasks::History;

/// How soon the footer's figures may be asked for again while a turn runs.
const PRESENTATION_EVERY: Duration = Duration::from_secs(2);

pub struct Session {
    pub info: SessionInfo,
    /// Identifies this residency; results posted for another one are stale.
    pub epoch: u64,
    pub transcript: Transcript,
    pub live: Option<LiveTurn>,
    pub queue: Vec<Queued>,
    pub usage: UsageTotals,
    pub presentation: Option<SessionPresentation>,
    /// When `presentation` was last asked for, and whether that answer is still due.
    pub presentation_asked: Option<tokio::time::Instant>,
    pub presentation_in_flight: bool,
    pub context: Option<SessionContext>,
    pub compaction: Option<CompactionProgress>,
    /// Coverage of the current summary from `session.context`; a summary
    /// replayed from history carries none of its own.
    pub summary_coverage: Option<(usize, usize)>,
    pub interaction: Option<PendingInteraction>,
    /// The display projection's first item; older items are behind the
    /// compaction summary.
    pub first_item: usize,
    /// The oldest source item loaded.
    pub loaded_from: usize,
    pub loading_older: bool,
    /// Pages still to load behind the summary until the latest answer shows.
    pub backfill: u8,
    pub applied_steers: u64,
    /// The finished turn whose durable items will replace its streamed region.
    pub reconcile_from: Option<(String, usize)>,
    /// Where the next turn's region starts, captured before its prompt block.
    pub next_turn_start: Option<usize>,
}

pub struct LiveTurn {
    pub turn: TurnRef,
    pub started: Instant,
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
            phase: Phase::Thinking,
            start,
            gap: false,
            running_tool: None,
            interrupting: false,
        }
    }
}

/// A steer (sent, awaiting apply) or a follow-up (sent after the turn).
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
            QueuedKind::Steer => "Steering",
            QueuedKind::Follow => "Follow-up",
        }
    }
}

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

pub enum InteractionKind {
    Approval,
    Choice(Vec<String>),
}

impl PendingInteraction {
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
        let approval =
            interaction.kind.get("type").and_then(|kind| kind.as_str()) == Some("approval");
        Self {
            turn,
            interaction_id: interaction.interaction_id.clone(),
            tool_call_id: interaction.tool_call_id.clone(),
            tool_name: interaction.tool_name.clone(),
            prompt,
            kind: if approval || choices.is_empty() {
                InteractionKind::Approval
            } else {
                InteractionKind::Choice(choices)
            },
            selected: 0,
        }
    }

    /// The rows the prompt offers; for an approval, row 0 allows.
    pub fn options(&self) -> Vec<String> {
        match &self.kind {
            InteractionKind::Approval => vec!["Allow once".into(), "Deny".into()],
            InteractionKind::Choice(choices) => choices.clone(),
        }
    }
}

impl Session {
    pub fn new(info: SessionInfo, epoch: u64) -> Self {
        Self {
            info,
            epoch,
            transcript: Transcript::default(),
            live: None,
            queue: Vec::new(),
            usage: UsageTotals::default(),
            presentation: None,
            presentation_asked: None,
            presentation_in_flight: false,
            context: None,
            compaction: None,
            summary_coverage: None,
            interaction: None,
            first_item: 0,
            loaded_from: 0,
            loading_older: false,
            backfill: 0,
            applied_steers: 0,
            reconcile_from: None,
            next_turn_start: None,
        }
    }

    pub fn is_busy(&self) -> bool {
        self.live.is_some()
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
        self.live.as_ref().map(|live| match live.interrupting {
            true => "Interrupting",
            false => live.phase.label(),
        })
    }

    pub fn model<'a>(&self, models: &'a [ModelInfo]) -> Option<&'a ModelInfo> {
        models.iter().find(|model| model.id == self.info.model)
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
    /// any order; returns a turn the Agent started that is not live yet.
    pub fn apply_event(&mut self, event: AgentEvent) -> Option<TurnRef> {
        if event.meta().is_some_and(|meta| meta.dropped_before > 0)
            && let Some(live) = self.live.as_mut()
        {
            live.gap = true;
        }
        match event {
            AgentEvent::SessionOpened { session, .. } => self.info = *session,
            // The Agent reports `idle` while a compaction still owns the
            // session, so the progress is taken as it comes.
            AgentEvent::SessionState { state, .. } => self.compaction = state.compaction,
            AgentEvent::TurnStarted { turn, .. } => return Some(turn),
            AgentEvent::RequestStarted {
                request_index,
                model,
                ..
            } => {
                if let Some(live) = self.live.as_mut() {
                    live.requests = live.requests.max(request_index + 1);
                    live.phase = Phase::Waiting;
                }
                self.info.model = model;
            }
            AgentEvent::OutputDelta {
                turn,
                request_index,
                reasoning,
                delta,
                ..
            } if self.is_live(&turn) => {
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
                if let Some(live) = self.live.as_mut() {
                    live.phase = match reasoning {
                        true => Phase::Thinking,
                        false => Phase::Writing,
                    };
                }
            }
            AgentEvent::SteerProgress { applied_count, .. } => {
                if let Some(loop_id) = self.live.as_ref().map(|live| live.turn.loop_id.clone()) {
                    self.apply_steer_progress(&loop_id, applied_count);
                }
            }
            AgentEvent::ToolStarted {
                turn,
                request_index,
                tool_call_id,
                tool_name,
                ..
            } if self.is_live(&turn) => {
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
                if let Some(live) = self.live.as_mut() {
                    live.phase = Phase::Thinking;
                    live.running_tool = Some((tool_name, String::new()));
                }
            }
            AgentEvent::ToolPresentation {
                call,
                tool_name,
                display,
                ..
            } if self.is_live(&call.turn) => {
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
            } if self.is_live(&call.turn) => {
                let key = call.key();
                match self.transcript.find_tool_mut(&key) {
                    Some(card) => card.apply_display(&display),
                    // The preview can come before `tool_started`; the card
                    // shows the arguments while they generate.
                    None if state == PreviewState::Generating => {
                        let mut card = ToolCall::new(key, tool_name);
                        card.apply_display(&display);
                        self.transcript.push(Block::Tool(Box::new(card)));
                    }
                    None => {}
                }
            }
            AgentEvent::ToolInvocation { invocation, .. } => {
                match self.transcript.find_tool_mut(&invocation.tool_ref) {
                    Some(card) => card.apply_invocation(&invocation),
                    None => {
                        let mut card =
                            ToolCall::new(invocation.tool_ref.clone(), invocation.name.clone());
                        card.detail = invocation.detail();
                        self.transcript.push(Block::Tool(Box::new(card)));
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
                let card = self.transcript.find_tool_mut(&data.tool_ref)?;
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
                        ToolOutcome::Success => ToolState::Succeeded,
                        ToolOutcome::Failed => ToolState::Failed,
                        ToolOutcome::Denied => ToolState::Denied,
                        ToolOutcome::Cancelled => ToolState::Cancelled,
                        ToolOutcome::InputProvided | ToolOutcome::Unknown => card.state,
                    });
                    card.truncated |= result.content_truncated;
                    if let Some(content) = result.content {
                        card.apply_result(&content);
                    }
                }
            }
            AgentEvent::InteractionRequested {
                turn, interaction, ..
            } => self.interaction = Some(PendingInteraction::from_wire(turn, &interaction)),
            AgentEvent::InteractionResolved { .. } => self.interaction = None,
            // Only `turn.wait` ends a turn; this just stops the phase text.
            AgentEvent::TurnFinished { .. } => {
                if let Some(live) = self.live.as_mut() {
                    live.phase = Phase::Waiting;
                }
            }
            _ => {}
        }
        None
    }

    fn push_running_tool(&mut self, key: ToolRef, name: String) {
        let mut card = ToolCall::new(key, name);
        card.state = ToolState::Running;
        card.started_at = Some(Instant::now());
        self.transcript.push(Block::Tool(Box::new(card)));
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

    /// Ends the live turn when `wait` is about it, appending the turn-end line.
    /// Returns whether the durable items must replace what was streamed.
    pub fn end_turn(&mut self, wait: &TurnWaitResult) -> Option<bool> {
        let live = self
            .live
            .take_if(|live| live.turn.loop_id == wait.turn.loop_id)?;
        self.abandon_steers();
        let streaming: Vec<usize> = (live.start..self.transcript.len())
            .filter(|index| self.transcript.blocks()[*index].is_streaming())
            .collect();
        for index in streaming {
            if let Some(Block::Assistant { streaming, .. }) = self.transcript.block_mut(index) {
                *streaming = false;
            }
        }
        let usage = wait.usage.clone().unwrap_or_default();
        self.usage.add(&usage);
        let tools = self.tools_since(live.start);
        self.transcript.push(Block::TurnEnd {
            loop_id: wait.turn.loop_id.clone(),
            outcome: wait.outcome.clone(),
            duration: Some(live.started.elapsed()),
            requests: wait.requests.unwrap_or(live.requests),
            tools,
            usage,
            changes: None,
        });
        let reconcile = live.gap || !wait.outcome.is_completed();
        if reconcile {
            self.reconcile_from = Some((wait.turn.loop_id.clone(), live.start));
        }
        Some(reconcile)
    }

    /// Replaces the region a finished turn streamed into with its durable
    /// items, keeping what only live events knew and the turn-end line last.
    pub fn reconcile_turn(&mut self, items: &[RawHistoryItem]) {
        let Some((loop_id, start)) = self.reconcile_from.take() else {
            return;
        };
        let turn_end = self
            .transcript
            .blocks()
            .iter()
            .position(|block| matches!(block, Block::TurnEnd { loop_id: id, .. } if *id == loop_id))
            .map(|index| {
                let block = self.transcript.blocks()[index].clone();
                self.transcript.remove(index);
                block
            });
        let live: Vec<ToolCall> = self.transcript.blocks()[start.min(self.transcript.len())..]
            .iter()
            .filter_map(|block| match block {
                Block::Tool(call) => Some(call.as_ref().clone()),
                _ => None,
            })
            .collect();
        self.transcript.truncate(start);
        for item in items {
            append_history_item(&mut self.transcript, item, &self.info.session_id);
        }
        for index in start..self.transcript.len() {
            let known = match &self.transcript.blocks()[index] {
                Block::Tool(call) => live.iter().find(|live| live.key == call.key),
                _ => None,
            };
            if let Some(known) = known.cloned()
                && let Some(Block::Tool(call)) = self.transcript.block_mut(index)
            {
                call.keep_live_facts(&known);
            }
        }
        if let Some(turn_end) = turn_end {
            self.transcript.push(turn_end);
        }
    }

    /// A refused send: the optimistic prompt block goes again.
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

    /// The oldest follow-up, the next one to send.
    pub fn take_follow_up(&mut self) -> Option<String> {
        let index = self
            .queue
            .iter()
            .position(|queued| queued.kind == QueuedKind::Follow)?;
        Some(self.queue.remove(index).text)
    }

    /// The newest follow-up, taken back out of the queue.
    pub fn recall_follow_up(&mut self) -> Option<String> {
        let index = self
            .queue
            .iter()
            .rposition(|queued| queued.kind == QueuedKind::Follow)?;
        Some(self.queue.remove(index).text)
    }

    /// The Agent discards steers it has not applied; they wait as follow-ups.
    pub fn abandon_steers(&mut self) {
        for queued in &mut self.queue {
            queued.kind = QueuedKind::Follow;
        }
        self.applied_steers = 0;
    }

    /// A refused steer waits for the next turn instead.
    pub fn demote_last_steer(&mut self) {
        if let Some(index) = self
            .queue
            .iter()
            .rposition(|queued| queued.kind == QueuedKind::Steer)
        {
            let mut queued = self.queue.remove(index);
            queued.kind = QueuedKind::Follow;
            self.queue.push(queued);
        }
    }

    /// Steers the Agent applied leave the queue and appear in the transcript,
    /// in acceptance order.
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
            let at = self
                .transcript
                .blocks()
                .iter()
                .rposition(|block| block.loop_id() == Some(loop_id))
                .map_or(self.transcript.len(), |index| index + 1);
            self.transcript.insert(
                at,
                Block::User {
                    text: queued.text,
                    kind: UserKind::Steering,
                },
            );
            self.applied_steers += 1;
        }
    }

    /// The source items not loaded as themselves. The summary stands in for
    /// the item at the display floor, which still has to be read.
    pub fn hidden_items(&self) -> usize {
        self.loaded_from + usize::from(self.loaded_from == self.first_item)
    }

    /// Replaces the transcript with the history tail.
    pub fn install_tail(&mut self, page: History) {
        self.transcript.clear();
        self.first_item = page.floor;
        self.loaded_from = page.from;
        for item in &page.items {
            append_history_item(&mut self.transcript, item, &self.info.session_id);
        }
        if let Some(coverage) = self.summary_coverage {
            self.cover_replayed_compaction(coverage);
        }
        // `from` counts even when an automatic compaction covers the whole
        // display tail: the history behind it is still there to read.
        if page.from > 0 {
            let hidden_items = self.hidden_items();
            self.transcript
                .insert(0, Block::EarlierHistory { hidden_items });
        }
    }

    /// Puts an older page in front of the transcript in place of the marker;
    /// returns how much the front grew, negative when the marker went alone,
    /// and the cards of a canonical page, which carry no detail of their own.
    pub fn prepend_page(&mut self, page: History) -> (isize, Vec<ToolRef>) {
        self.loading_older = false;
        let mut scratch = Transcript::default();
        for item in &page.items {
            append_history_item(&mut scratch, item, &self.info.session_id);
        }
        let bare_cards = match page.canonical {
            true => scratch.tool_calls().map(|call| call.key.clone()).collect(),
            false => Vec::new(),
        };
        let blocks = scratch.into_blocks();
        let added = blocks.len();
        let replaced = usize::from(matches!(
            self.transcript.blocks().first(),
            Some(Block::EarlierHistory { .. })
        ));
        self.transcript.remove_front(replaced);
        for block in blocks.into_iter().rev() {
            self.transcript.insert(0, block);
        }
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
        self.rebase(grown);
        (grown, bare_cards)
    }

    /// Moves every boundary stated as a transcript index by `delta` blocks
    /// put in front of it; a page can land at any point around a turn.
    fn rebase(&mut self, delta: isize) {
        let shift = |index: &mut usize| *index = index.saturating_add_signed(delta);
        if let Some(live) = self.live.as_mut() {
            shift(&mut live.start);
        }
        if let Some(start) = self.next_turn_start.as_mut() {
            shift(start);
        }
        if let Some((_, start)) = self.reconcile_from.as_mut() {
            shift(start);
        }
    }

    /// Takes a `session.context` report: compaction progress, the last result
    /// as a marker, and the coverage of the current summary.
    pub fn apply_context(&mut self, context: SessionContext) {
        self.compaction = context.current_operation.clone();
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

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Session {
        Session::new(
            serde_json::from_value(crate::testing::session_info("s")).unwrap(),
            1,
        )
    }

    fn queue(session: &mut Session, text: &str, kind: QueuedKind) {
        session.queue.push(Queued {
            text: text.into(),
            kind,
        });
    }

    #[test]
    fn applied_steers_become_steering_blocks_in_order_once() {
        let mut session = session();
        session.live = Some(LiveTurn::new(TurnRef::new("s", "l"), 0));
        queue(&mut session, "first", QueuedKind::Steer);
        queue(&mut session, "second", QueuedKind::Steer);
        queue(&mut session, "later", QueuedKind::Follow);
        session.apply_steer_progress("l", 1);
        session.apply_steer_progress("l", 2);
        session.apply_steer_progress("l", 2);
        let steered: Vec<&str> = session
            .transcript
            .blocks()
            .iter()
            .filter_map(|block| match block {
                Block::User {
                    text,
                    kind: UserKind::Steering,
                } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(steered, ["first", "second"]);
        assert_eq!(session.take_follow_up().as_deref(), Some("later"));
    }

    #[test]
    fn a_turn_end_turns_unapplied_steers_into_follow_ups() {
        let mut session = session();
        session.live = Some(LiveTurn::new(TurnRef::new("s", "l"), 0));
        queue(&mut session, "steer", QueuedKind::Steer);
        let wait: TurnWaitResult = serde_json::from_value(serde_json::json!({
            "turn": {"session_id": "s", "loop_id": "l"}, "outcome": {"type": "completed"}
        }))
        .unwrap();
        assert_eq!(session.end_turn(&wait), Some(false));
        assert_eq!(session.end_turn(&wait), None, "the turn ends once");
        assert_eq!(session.take_follow_up().as_deref(), Some("steer"));
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
        });
        assert_eq!(session.auto_title().as_deref(), Some("fix it"));
        session.info.title = Some("named".into());
        assert_eq!(session.auto_title(), None);
        session.info.title = Some("  ".into());
        assert_eq!(session.auto_title().as_deref(), Some("fix it"));
    }
}
