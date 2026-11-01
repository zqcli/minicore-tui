//! Streamed body events, early content and steering placement.

use std::collections::BTreeSet;
use std::time::Instant;

use super::{DeliveredIdentity, EARLY_BYTES, EARLY_TURNS, Phase, Session, accepted_millis};
use crate::protocol::event::{PreviewState, ToolOutcome};
use crate::protocol::read::UserKind;
use crate::protocol::{AgentEvent, DeliveredInput, ToolRef, TurnRef};
use crate::transcript::{Block, RequestKey, ToolCall, ToolState};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum BodyOrder {
    Steering,
    Assistant,
    Tool,
}

impl Session {
    /// Applies one event to a live session: body content only. Control state
    /// arrives through [`Session::apply_control`].
    pub fn apply_body_event(&mut self, event: AgentEvent, bytes: usize) {
        if let Some(loop_id) = event.loop_id() {
            if self.is_closed(loop_id) && !event.is_tool_metadata() {
                return;
            }
            if !self.knows_turn(loop_id) {
                self.cache_early(event, bytes);
                return;
            }
        }
        self.apply_event(event);
    }

    fn is_live(&self, turn: &TurnRef) -> bool {
        self.live
            .as_ref()
            .is_some_and(|live| live.turn.loop_id == turn.loop_id)
    }

    /// Applies body hints to the session's known turn regions.
    pub fn apply_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::SessionOpened { session, .. } => self.info = *session,
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
            // A successor can stream before the predecessor's wait answers;
            // its own turn identity determines where those deltas belong.
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

    /// Makes the inputs a running turn took steering blocks, or holds them for a turn not live yet.
    pub(super) fn deliver(&mut self, turn: TurnRef, items: Vec<DeliveredInput>) {
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
                        load: Some(self.load.clone()),
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

    pub(in crate::app) fn knows_turn(&self, loop_id: &str) -> bool {
        self.is_closed(loop_id)
            || self
                .live
                .as_ref()
                .is_some_and(|live| live.turn.loop_id == loop_id)
            || self.region_start(loop_id).is_some()
    }

    fn cache_early(&mut self, event: AgentEvent, bytes: usize) {
        if bytes > EARLY_BYTES {
            self.early_gap = true;
            self.recovery.behind = true;
            return;
        }
        while self.early_bytes + bytes > EARLY_BYTES
            || (!event.loop_id().is_some_and(|loop_id| {
                self.early
                    .iter()
                    .rev()
                    .any(|(held, _)| held.loop_id() == Some(loop_id))
            }) && self
                .early
                .iter()
                .filter_map(|(event, _)| event.loop_id())
                .collect::<BTreeSet<_>>()
                .len()
                >= EARLY_TURNS)
        {
            self.evict_early();
        }
        self.early_bytes += bytes;
        self.early.push_back((event, bytes));
        #[cfg(feature = "e2e-observe")]
        {
            self.early_peak_bytes = self.early_peak_bytes.max(self.early_bytes);
        }
    }

    pub(in crate::app) fn evict_early(&mut self) {
        if let Some((_, bytes)) = self.early.pop_front() {
            self.early_bytes -= bytes;
            self.early_gap = true;
            self.recovery.behind = true;
        }
    }

    pub(in crate::app) fn replay_early(&mut self) {
        let mut seen = BTreeSet::new();
        let loops: Vec<String> = self
            .early
            .iter()
            .filter_map(|(event, _)| event.loop_id())
            .filter(|id| seen.insert(*id) && self.knows_turn(id))
            .map(str::to_owned)
            .collect();
        for loop_id in loops {
            self.place_early(&loop_id);
        }
    }

    pub(super) fn push_in_turn(&mut self, loop_id: &str, block: Block) {
        let end = self.region_end(loop_id);
        let start = self.region_start(loop_id).unwrap_or(end);
        let at = match &block {
            Block::Assistant { key, .. } => {
                self.request_position(loop_id, start..end, key.request_index, BodyOrder::Assistant)
            }
            Block::Tool(call) => {
                self.request_position(loop_id, start..end, call.key.request_index, BodyOrder::Tool)
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
        kind: BodyOrder,
    ) -> usize {
        range
            .clone()
            .find(|index| {
                let position = match &self.transcript.blocks()[*index] {
                    Block::User {
                        kind: UserKind::Steering,
                        ..
                    } => self
                        .transcript
                        .user_identity(*index)
                        .filter(|user| user.loop_id == loop_id)
                        .and_then(|user| user.request_index)
                        .map(|request| (request, BodyOrder::Steering)),
                    Block::Assistant { key, .. } if key.loop_id == loop_id => {
                        Some((key.request_index, BodyOrder::Assistant))
                    }
                    Block::Tool(call) if call.key.loop_id == loop_id => {
                        Some((call.key.request_index, BodyOrder::Tool))
                    }
                    _ => None,
                };
                position.is_some_and(|position| position > (request, kind))
            })
            .unwrap_or(range.end)
    }

    pub(super) fn insert_in_turn(&mut self, loop_id: &str, at: usize, block: Block) {
        for (id, start) in self.boundaries_mut() {
            if id != loop_id && *start >= at {
                *start += 1;
            }
        }
        self.transcript.insert(at, block);
    }

    fn ensure_assistant(&mut self, loop_id: &str, request_index: u32) {
        if !self.transcript.blocks().iter().rev().any(|block| {
            matches!(block, Block::Assistant { key, .. }
                if key.loop_id == loop_id && key.request_index == request_index)
        }) {
            self.push_in_turn(
                loop_id,
                Block::Assistant {
                    key: RequestKey {
                        loop_id: loop_id.to_owned(),
                        request_index,
                    },
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
            let start = self.region_start(loop_id).unwrap_or(end);
            let at =
                self.request_position(loop_id, start..end, item.request_index, BodyOrder::Steering);
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
}
