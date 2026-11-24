//! Native child observation without terminal navigation or a second execution owner.

use std::collections::{HashMap, VecDeque};

use minicore_host_core::*;
use minicore_host_protocol::*;
use minicore_host_transport::{OutputBuffer, SessionClient};

use super::{
    App, Msg,
    tasks::{self, OutputRead},
};
use crate::history::{HistoryError, HistoryWindow};
use crate::transcript::agent_card::{self, AgentCard, DelegationKey, PREVIEW_BYTES};
use crate::transcript::{Block, RequestKey, ToolCall};

const PENDING_CARDS: usize = 32;
const OUTPUT_PAGE_BYTES: u64 = 16 * 1024;

#[derive(Default)]
pub(super) struct Cards {
    observers: HashMap<DelegationKey, Observer>,
    pending: VecDeque<DelegationKey>,
}

struct Observer {
    location: Option<(u64, ToolCallRef)>,
    card: Option<Box<AgentCard>>,
    outputs: Vec<Output>,
    closed: Vec<(OutputSource, String)>,
    history_reading: bool,
    history_loaded: bool,
    history_due: bool,
    // One native response watermark per Turn survives preview body clipping.
    model_floor: HashMap<TurnId, u64>,
    turn_revisions: HashMap<TurnId, (Option<LoadId>, u64)>,
    tool_revisions: HashMap<ToolCallRef, (Option<LoadId>, u64)>,
    finished_turns: Vec<TurnId>,
}

struct Output {
    buffer: OutputBuffer,
    channel: String,
    rendered: u64,
    reading: bool,
    failed: bool,
}

pub enum Read {
    Baseline(Result<SubscribeResult<StateEvent>, CallError>),
    History(Result<HistoryWindow, HistoryError>),
    Output {
        source: OutputSource,
        channel: String,
        result: Box<Result<OutputRead, String>>,
    },
}

#[cfg(test)]
impl Cards {
    pub(super) fn is_reading(&self) -> bool {
        self.observers.values().any(|observer| {
            observer.history_reading || observer.outputs.iter().any(|output| output.reading)
        })
    }

    pub(super) fn buffered_outputs(&self) -> usize {
        self.observers
            .values()
            .map(|observer| observer.outputs.len())
            .sum()
    }
}

impl App {
    pub(super) fn refresh_agent_names(&mut self) {
        let changed: Vec<_> = self
            .agent_cards
            .observers
            .iter()
            .filter_map(|(key, observer)| {
                let card = self.agent_card(observer)?;
                let name = &self
                    .agents
                    .iter()
                    .find(|agent| agent.id == key.child.agent_id)?
                    .name;
                (card.agent_name.as_ref() != Some(name)).then(|| (key.clone(), name.clone()))
            })
            .collect();
        for (key, name) in changed {
            if let Some((observer, mut card)) = self.take_agent_card(&key) {
                card.agent_name = Some(name);
                self.put_agent_card(key, observer, card);
            }
        }
    }

    pub(super) fn agent_observation_error(&mut self, error: String) {
        let keys: Vec<_> = self.agent_cards.observers.keys().cloned().collect();
        for key in keys {
            if let Some((observer, mut card)) = self.take_agent_card(&key) {
                if card.association.completion.is_none()
                    || !observer.outputs.is_empty()
                    || observer.history_reading
                {
                    card.observation_error = Some(error.clone());
                }
                self.put_agent_card(key, observer, card);
            }
        }
        self.notice(super::NoticeLevel::Error, error);
    }

    pub(super) fn locate_agent_card(&mut self, parent: SessionRef, origin: ToolCallRef) {
        self.open_session(parent.clone());
        self.agent_location = Some((parent, origin));
        self.resolve_agent_location();
    }

    pub(super) fn resolve_agent_location(&mut self) {
        if self.loading_session {
            return;
        }
        let Some((target, origin)) = self.agent_location.clone() else {
            return;
        };
        let Some(session) = self.resident.current() else {
            self.agent_location = None;
            return;
        };
        if session.address() != target {
            self.agent_location = None;
            return;
        }
        if let Some(block) = session.transcript.blocks().iter().position(
            |block| matches!(block, Block::Tool(call) if call.key.call_ref() == Some(&origin)),
        ) {
            self.scroll = super::Scroll::Anchor { block, offset: 0 };
            self.selection = None;
            self.agent_location = None;
        } else if !session.loading_older {
            if session
                .history
                .as_ref()
                .is_some_and(HistoryWindow::has_more)
            {
                self.load_older();
            } else {
                self.agent_location = None;
                self.notice(
                    super::NoticeLevel::Info,
                    "The original Agent card is unavailable in this Session's retained history",
                );
            }
        }
    }

    pub(super) fn host_observed(&mut self, event: ObservationEvent) {
        self.observe_agent_event(&event);
        if let ObservationEvent::State(state) = event {
            self.on_host_state(state);
        }
    }

    pub(super) fn host_baseline(&mut self, baseline: SubscribeResult<StateEvent>) {
        let positions = live_positions(&baseline);
        for state in baseline.initial_states {
            self.host_observed(ObservationEvent::State(state));
        }
        for position in positions {
            let source = output_turn(&position.source);
            let Some(key) = self.agent_key(source) else {
                continue;
            };
            let Some((mut observer, mut card)) = self.take_agent_card(&key) else {
                continue;
            };
            if !closed(&observer, &position.source, &position.channel)
                && !canonical(&card, &position.source)
            {
                output(&mut observer, &position.source, &position.channel)
                    .buffer
                    .position(&position);
            }
            self.advance_agent_outputs(&key, &mut observer, &mut card);
            self.put_agent_card(key, observer, card);
        }
        let recovery: Vec<_> = self
            .agent_cards
            .observers
            .iter()
            .filter_map(|(key, observer)| {
                let card = self.agent_card(observer)?;
                (card.observation_error.is_some()
                    && !card.history_unavailable
                    && !card.association.inputs.is_empty())
                .then(|| key.clone())
            })
            .collect();
        for key in recovery {
            if let Some((mut observer, card)) = self.take_agent_card(&key) {
                self.read_agent_history(&key, &mut observer, &card);
                self.put_agent_card(key, observer, card);
            }
        }
        self.ready = true;
    }

    pub(super) fn register_delegation(&mut self, association: &DelegationAssociation) {
        let Some(epoch) = self
            .resident
            .find(&association.parent)
            .map(|session| session.epoch)
        else {
            return;
        };
        let key = DelegationKey {
            incarnation: self.client.initial_info().host_incarnation.clone(),
            child: association.child.clone(),
            task_message_id: association.task_message_id.clone(),
        };
        if !self.agent_cards.observers.contains_key(&key) {
            let mut card = AgentCard::new(key.incarnation.clone(), association.clone());
            card.agent_name = self
                .agents
                .iter()
                .find(|agent| agent.id == key.child.agent_id)
                .map(|agent| agent.name.clone());
            // The child may be gone (a released temporary Session); the
            // delegating call still names its task.
            card.task =
                self.resident
                    .find(&association.parent)
                    .and_then(|session| {
                        session.transcript.tool_calls().find(|call| {
                            call.key.tool_call_id() == &association.origin.tool_call_id
                        })
                    })
                    .and_then(|call| call.arguments.as_ref()?.get("text")?.as_str())
                    .map(|text| agent_card::head(text, 512).to_owned())
                    .unwrap_or_default();
            if let Some(snapshot) = self.pending_states.get(&key.child)
                && let Some(current) = &snapshot.data.delegation
                && current.task_message_id == key.task_message_id
                && current.parent == association.parent
            {
                card.merge(current);
                card.state = Some(snapshot.clone());
            }
            self.agent_cards.observers.insert(
                key.clone(),
                Observer {
                    location: None,
                    card: Some(Box::new(card)),
                    outputs: Vec::new(),
                    closed: Vec::new(),
                    history_reading: false,
                    history_loaded: false,
                    history_due: false,
                    model_floor: HashMap::new(),
                    turn_revisions: HashMap::new(),
                    tool_revisions: HashMap::new(),
                    finished_turns: Vec::new(),
                },
            );
            self.agent_cards.pending.push_back(key.clone());
            let client = self.client.clone();
            let target = key.child.clone();
            let read_key = key.clone();
            self.spawn_msg(async move {
                let result = tasks::native_session_subscription(&client, target)
                    .await
                    .map(|subscription| subscription.result().clone());
                Msg::AgentCardRead(read_key, Box::new(Read::Baseline(result)))
            });
        }
        let Some((mut observer, mut card)) = self.take_agent_card(&key) else {
            return;
        };
        let was_complete = card.association.completion.is_some();
        card.merge(association);
        if self
            .session_mut(epoch)
            .transcript
            .find_tool(&association.origin)
            .is_some()
        {
            observer.location = Some((epoch, association.origin.clone()));
            self.agent_cards.pending.retain(|pending| pending != &key);
        }
        if !card.association.inputs.is_empty()
            && (!observer.history_loaded
                || (!was_complete && card.association.completion.is_some()))
        {
            self.read_agent_history(&key, &mut observer, &card);
        }
        self.put_agent_card(key, observer, card);
        while self.agent_cards.pending.len() > PENDING_CARDS {
            if let Some(expired) = self.agent_cards.pending.pop_front() {
                self.agent_cards.observers.remove(&expired);
            }
        }
    }

    pub(super) fn install_delegation_records(&mut self, epoch: u64, records: &[HistoryRecordData]) {
        let parent = self.session_mut(epoch).address();
        for association in records.iter().flat_map(|record| &record.delegations) {
            if association.parent != parent {
                continue;
            }
            if let Some(call) = self
                .session_mut(epoch)
                .transcript
                .find_tool_mut(&association.origin)
                && !call.delegations.iter().any(|known| {
                    known.child == association.child
                        && known.task_message_id == association.task_message_id
                })
            {
                call.delegations.push(association.clone());
            }
            self.register_delegation(association);
        }
        // A later native notification may have arrived before the original page.
        let pending: Vec<_> = self
            .agent_cards
            .pending
            .iter()
            .filter_map(|key| {
                let card = self.agent_cards.observers.get(key)?.card.as_ref()?;
                (card.association.parent == parent).then(|| card.association.clone())
            })
            .collect();
        for association in pending {
            self.register_delegation(&association);
        }
    }

    fn agent_key(&self, source: &TurnIdentity) -> Option<DelegationKey> {
        self.agent_cards
            .observers
            .iter()
            .find_map(|(key, observer)| {
                if key.child.agent_id != source.session.agent_id
                    || key.child.session_id != source.session.session_id
                {
                    return None;
                }
                let card = self.agent_card(observer)?;
                card.contains_turn(source).then(|| key.clone())
            })
    }

    fn agent_card<'a>(&'a self, observer: &'a Observer) -> Option<&'a AgentCard> {
        match &observer.location {
            Some((epoch, key)) => self
                .resident
                .get(*epoch)?
                .transcript
                .find_tool(key)?
                .agent_card
                .as_deref(),
            None => observer.card.as_deref(),
        }
    }

    fn take_agent_card(&mut self, key: &DelegationKey) -> Option<(Observer, Box<AgentCard>)> {
        let mut observer = self.agent_cards.observers.remove(key)?;
        let card = match &observer.location {
            Some((epoch, origin)) => self
                .resident
                .get_mut(*epoch)?
                .transcript
                .find_tool_mut(origin)?
                .agent_card
                .take()?,
            None => observer.card.take()?,
        };
        Some((observer, card))
    }

    fn put_agent_card(&mut self, key: DelegationKey, mut observer: Observer, card: Box<AgentCard>) {
        match &observer.location {
            Some((epoch, origin)) => {
                let Some(session) = self.resident.get_mut(*epoch) else {
                    return;
                };
                let Some(call) = session.transcript.find_tool_mut(origin) else {
                    return;
                };
                call.agent_card = Some(card);
            }
            None => observer.card = Some(card),
        }
        self.agent_cards.observers.insert(key, observer);
    }

    fn observe_agent_event(&mut self, event: &ObservationEvent) {
        let initial = self.client.initial_info();
        if let Some(source) = super::control::event_session(event)
            && (source.host_id != initial.host_id
                || source.host_incarnation != initial.host_incarnation)
        {
            return;
        }
        if let ObservationEvent::State(StateEvent::Session { source, data }) = event
            && let Some(association) = &data.delegation
        {
            self.register_delegation(association);
            let key = DelegationKey {
                incarnation: source.host_incarnation.clone(),
                child: association.child.clone(),
                task_message_id: association.task_message_id.clone(),
            };
            if let Some((observer, mut card)) = self.take_agent_card(&key) {
                let snapshot = super::control::session_snapshot(source.clone(), data.clone());
                if card.association.completion.is_none() {
                    card.historical = false;
                    match &mut card.state {
                        Some(current) => {
                            super::session::merge_snapshot(current, snapshot);
                        }
                        None => card.state = Some(snapshot),
                    }
                }
                self.put_agent_card(key, observer, card);
            }
        }
        if let ObservationEvent::Activity(
            ActivityEvent::MessageAccepted { data, .. } | ActivityEvent::MessageBound { data, .. },
        ) = event
        {
            for association in &data.delegations {
                self.register_delegation(association);
            }
        }
        let Some(source) = super::control::event_turn(event) else {
            return;
        };
        let Some(key) = self.agent_key(source) else {
            return;
        };
        let Some((mut observer, mut card)) = self.take_agent_card(&key) else {
            return;
        };
        match event {
            ObservationEvent::State(StateEvent::Turn { source, data })
            | ObservationEvent::Activity(ActivityEvent::TurnFinished { source, data }) => {
                if data.state == TurnState::Finished
                    && !observer.finished_turns.contains(&source.turn_id)
                {
                    observer.finished_turns.push(source.turn_id.clone());
                }
                if newer(
                    &mut observer.turn_revisions,
                    source.turn_id.clone(),
                    &source.load_id,
                    data.runner_revision,
                ) {
                    if let Some(last) = data.requests.checked_sub(1) {
                        advance_model_floor(&mut observer, &source.turn_id, last);
                    }
                    card.turn = Some((source.clone(), data.clone()));
                    if data.state == TurnState::Finished {
                        if data.outcome != Some(TurnOutcome::Completed) {
                            for index in 0..card.transcript.len() {
                                if matches!(&card.transcript.blocks()[index], Block::Assistant { key, streaming: true, .. } if key.turn_id() == Some(&source.turn_id))
                                    && let Some(Block::Assistant {
                                        streaming, partial, ..
                                    }) = card.transcript.block_mut(index)
                                {
                                    *streaming = false;
                                    *partial = true;
                                }
                            }
                        }
                        self.read_agent_history(&key, &mut observer, &card);
                    }
                }
            }
            ObservationEvent::State(StateEvent::Toolcall { source, data }) => {
                let tool = tool_ref(source);
                if newer(
                    &mut observer.tool_revisions,
                    tool.clone(),
                    &source.turn.load_id,
                    data.runner_revision,
                ) && (data.state != ToolCallState::Finished
                    || card.transcript.find_tool(&tool).is_some())
                {
                    ensure_tool(&mut card, &tool, &data.name).apply_state(data);
                }
            }
            ObservationEvent::Activity(ActivityEvent::MessageBound {
                message_id, data, ..
            }) => {
                if message_id == &key.task_message_id && card.task.is_empty() {
                    card.task = agent_card::head(&data.text, 512).to_owned();
                }
            }
            ObservationEvent::Activity(ActivityEvent::ToolStarted { source, data }) => {
                let call = ensure_tool(&mut card, &tool_ref(source), &data.name);
                call.set_preview_arguments(&data.arguments);
                call.set_state(crate::transcript::ToolState::Running);
            }
            ObservationEvent::Activity(ActivityEvent::ModelFinished { source, data }) => {
                advance_model_floor(
                    &mut observer,
                    &source.turn.turn_id,
                    source.response_index.saturating_add(1),
                );
                card.transcript.install_preview_response(source, data);
            }
            ObservationEvent::Activity(ActivityEvent::ToolFinished { source, data }) => {
                let call = ensure_tool(&mut card, &tool_ref(source), "tool");
                call.apply_preview_result(&data.result);
                close_output(
                    &mut observer,
                    OutputSource::Toolcall {
                        source: source.clone(),
                    },
                );
            }
            ObservationEvent::Activity(ActivityEvent::ModelOutput { source, data }) => {
                let source = OutputSource::Model {
                    source: source.clone(),
                };
                observe_output(&mut observer, &mut card, source, &data.channel, event);
            }
            ObservationEvent::Activity(ActivityEvent::ToolOutput { source, data }) => {
                let key = tool_ref(source);
                ensure_tool(&mut card, &key, "tool");
                let source = OutputSource::Toolcall {
                    source: source.clone(),
                };
                observe_output(&mut observer, &mut card, source, &data.channel, event);
            }
            _ => {}
        }
        self.advance_agent_outputs(&key, &mut observer, &mut card);
        card.trim();
        self.put_agent_card(key, observer, card);
    }

    fn advance_agent_outputs(
        &self,
        key: &DelegationKey,
        observer: &mut Observer,
        card: &mut AgentCard,
    ) {
        for output in &mut observer.outputs {
            let source = output.buffer.source();
            if retired_model(
                observer.model_floor.get(&output_turn(source).turn_id),
                source,
            ) || canonical(card, source)
            {
                continue;
            }
            let start = output.buffer.end() - output.buffer.text().len() as u64;
            if output.rendered < start {
                output.rendered = start;
                card.truncated = true;
            }
            let delta = &output.buffer.text()[(output.rendered - start) as usize..];
            if !delta.is_empty() {
                match source {
                    OutputSource::Model { source } => {
                        let response = RequestKey::response(
                            source.turn.turn_id.clone(),
                            source.response_index,
                        );
                        if let Block::Assistant {
                            reasoning, text, ..
                        } = card.transcript.assistant_mut(&response)
                        {
                            card.truncated |= match output.channel.as_str() {
                                "text" => agent_card::append(text, delta),
                                "reasoning" => agent_card::append(reasoning, delta),
                                _ => false,
                            };
                        }
                    }
                    OutputSource::Toolcall { source } => {
                        if let Some(call) = card.transcript.find_tool_mut(&tool_ref(source)) {
                            call.push_output(delta);
                        }
                    }
                }
                output.rendered = output.buffer.end();
            }
            output.buffer.retain_tail(PREVIEW_BYTES);
            if !output.reading
                && !output.failed
                && let Some(request) = output.buffer.next_read(OUTPUT_PAGE_BYTES)
            {
                output.reading = true;
                let source = output.buffer.source().clone();
                let client = self.client.session(key.child.clone());
                let key = key.clone();
                self.spawn_msg(async move {
                    let channel = request.channel.clone();
                    let result = tasks::output(&client, &source, request).await;
                    Msg::AgentCardRead(
                        key,
                        Box::new(Read::Output {
                            source,
                            channel,
                            result: Box::new(result),
                        }),
                    )
                });
            }
        }
        observer.outputs.retain(|output| {
            let finished = canonical(card, output.buffer.source())
                || retired_model(
                    observer
                        .model_floor
                        .get(&output_turn(output.buffer.source()).turn_id),
                    output.buffer.source(),
                )
                || output.buffer.is_complete()
                || (output.failed && card.association.completion.is_some())
                || (observer
                    .finished_turns
                    .contains(&output_turn(output.buffer.source()).turn_id)
                    && output.buffer.next_read(OUTPUT_PAGE_BYTES).is_none());
            if finished && !output.reading {
                observer
                    .closed
                    .push((output.buffer.source().clone(), output.channel.clone()));
                false
            } else {
                true
            }
        });
        if observer.closed.len() > 64 {
            observer.closed.drain(..observer.closed.len() - 64);
        }
    }

    fn read_agent_history(&self, key: &DelegationKey, observer: &mut Observer, card: &AgentCard) {
        if observer.history_reading {
            observer.history_due = true;
            return;
        }
        observer.history_reading = true;
        observer.history_loaded = true;
        let client = self.client.session(key.child.clone());
        let key = key.clone();
        let turns: Vec<_> = card
            .association
            .inputs
            .iter()
            .map(|range| range.turn.turn_id.clone())
            .collect();
        self.spawn_msg(async move {
            Msg::AgentCardRead(
                key,
                Box::new(Read::History(preview_history(&client, &turns).await)),
            )
        });
    }

    pub(super) fn agent_card_read(&mut self, key: DelegationKey, read: Read) {
        if let Read::Baseline(result) = read {
            self.agent_baseline(key, result);
            return;
        }
        let Some((mut observer, mut card)) = self.take_agent_card(&key) else {
            return;
        };
        match read {
            Read::Baseline(_) => unreachable!("baseline handled above"),
            Read::History(result) => {
                observer.history_reading = false;
                match result {
                    Ok(window) if window.source.host_incarnation == key.incarnation => {
                        if !window.items.is_empty()
                            && observer
                                .outputs
                                .iter()
                                .all(|output| canonical(&card, output.buffer.source()))
                        {
                            card.observation_error = None;
                            card.history_unavailable = false;
                        }
                        for record in &window.items {
                            if let (Some(turn), Some(response)) =
                                (&record.turn_id, record.response_index)
                                && matches!(
                                    record.item,
                                    HistoryItemData::Assistant { .. }
                                        | HistoryItemData::Partial { .. }
                                        | HistoryItemData::ToolResult { .. }
                                )
                            {
                                advance_model_floor(
                                    &mut observer,
                                    turn,
                                    response.saturating_add(1),
                                );
                            }
                            if let HistoryItemData::User {
                                message_id: Some(message_id),
                                text,
                                ..
                            } = &record.item
                                && message_id == &key.task_message_id
                                && card.task.is_empty()
                            {
                                card.task = agent_card::head(text, 512).to_owned();
                            }
                        }
                        card.transcript.install_preview_records(&window.items);
                    }
                    Ok(_) => {}
                    Err(error) => {
                        card.history_unavailable = matches!(&error, HistoryError::Host(CallError::Business(fault)) if fault.kind() == Some(FaultKind::AgentSessionNotFound));
                        card.observation_error = Some(error.to_string());
                    }
                }
                if std::mem::take(&mut observer.history_due) {
                    self.read_agent_history(&key, &mut observer, &card);
                }
            }
            Read::Output {
                source,
                channel,
                result,
            } => {
                if let Some(output) = observer
                    .outputs
                    .iter_mut()
                    .find(|output| output.buffer.source() == &source && output.channel == channel)
                {
                    output.reading = false;
                    let result = (*result).and_then(|read| {
                        match read {
                            OutputRead::Model(read) => output.buffer.model_read(&read),
                            OutputRead::Tool(read) => output.buffer.tool_read(&read),
                        }
                        .map_err(|error| error.to_string())
                    });
                    if let Err(error) = result {
                        output.failed = true;
                        card.observation_error = Some(error);
                    }
                }
            }
        }
        self.advance_agent_outputs(&key, &mut observer, &mut card);
        card.trim();
        self.put_agent_card(key, observer, card);
    }

    fn agent_baseline(
        &mut self,
        key: DelegationKey,
        result: Result<SubscribeResult<StateEvent>, CallError>,
    ) {
        let Some((observer, mut card)) = self.take_agent_card(&key) else {
            return;
        };
        let baseline = match result {
            Ok(baseline) => baseline,
            Err(error) => {
                card.history_unavailable = matches!(&error, CallError::Business(fault) if fault.kind() == Some(FaultKind::AgentSessionNotFound));
                card.observation_error = Some(error.to_string());
                self.put_agent_card(key, observer, card);
                return;
            }
        };
        card.historical = !baseline.initial_states.iter().any(|event| matches!(event, StateEvent::Session { data, .. } if data.delegation.as_ref().is_some_and(|current| current.task_message_id == key.task_message_id && current.parent == card.association.parent)));
        let positions = live_positions(&baseline);
        self.put_agent_card(key.clone(), observer, card);
        for state in baseline.initial_states {
            self.host_observed(ObservationEvent::State(state));
        }
        let Some((mut observer, mut card)) = self.take_agent_card(&key) else {
            return;
        };
        for position in positions {
            if card.contains_turn(output_turn(&position.source))
                && !closed(&observer, &position.source, &position.channel)
                && !canonical(&card, &position.source)
            {
                output(&mut observer, &position.source, &position.channel)
                    .buffer
                    .position(&position);
            }
        }
        self.advance_agent_outputs(&key, &mut observer, &mut card);
        self.put_agent_card(key, observer, card);
    }

    pub(super) fn retire_agent_observers(&mut self) {
        let resident = &self.resident;
        self.agent_cards
            .observers
            .retain(|_, observer| match &observer.location {
                Some((epoch, key)) => resident
                    .get(*epoch)
                    .is_some_and(|session| session.transcript.find_tool(key).is_some()),
                None => observer
                    .card
                    .as_ref()
                    .is_some_and(|card| resident.find(&card.association.parent).is_some()),
            });
        self.agent_cards
            .pending
            .retain(|key| self.agent_cards.observers.contains_key(key));
    }
}

// Completed sources are recovered from positioned history, never replayed as
// fresh streaming blocks after their bounded preview has been discarded.
fn live_positions(baseline: &SubscribeResult<StateEvent>) -> Vec<OutputPosition> {
    let sources: Vec<_> = baseline
        .initial_states
        .iter()
        .filter_map(|state| match state {
            StateEvent::Turn { source, data }
                if data.state != TurnState::Finished && data.phase == Some(TurnPhase::Model) =>
            {
                data.requests
                    .checked_sub(1)
                    .map(|response_index| OutputSource::Model {
                        source: ModelOutputIdentity {
                            turn: source.clone(),
                            response_index,
                        },
                    })
            }
            StateEvent::Toolcall { source, data } if data.state != ToolCallState::Finished => {
                Some(OutputSource::Toolcall {
                    source: source.clone(),
                })
            }
            _ => None,
        })
        .collect();
    baseline
        .output_positions
        .iter()
        .filter(|position| sources.contains(&position.source))
        .cloned()
        .collect()
}

fn advance_model_floor(observer: &mut Observer, turn: &TurnId, floor: u64) {
    let current = observer.model_floor.entry(turn.clone()).or_default();
    *current = (*current).max(floor);
}

fn retired_model(floor: Option<&u64>, source: &OutputSource) -> bool {
    matches!(source, OutputSource::Model { source }
        if floor.is_some_and(|floor| source.response_index < *floor))
}

fn closed(observer: &Observer, source: &OutputSource, channel: &str) -> bool {
    observer
        .finished_turns
        .contains(&output_turn(source).turn_id)
        || retired_model(
            observer.model_floor.get(&output_turn(source).turn_id),
            source,
        )
        || observer
            .closed
            .iter()
            .any(|(known, name)| known == source && name == channel)
}

fn newer<K: Eq + std::hash::Hash>(
    revisions: &mut HashMap<K, (Option<LoadId>, u64)>,
    key: K,
    load: &Option<LoadId>,
    revision: u64,
) -> bool {
    if revisions
        .get(&key)
        .is_some_and(|(known, old)| known == load && *old >= revision)
    {
        return false;
    }
    revisions.insert(key, (load.clone(), revision));
    true
}

fn ensure_tool<'a>(card: &'a mut AgentCard, key: &ToolCallRef, name: &str) -> &'a mut ToolCall {
    if card.transcript.find_tool(key).is_none() {
        card.transcript
            .push(Block::Tool(Box::new(ToolCall::new(key.clone(), name))));
    }
    let tool = card
        .transcript
        .find_tool_mut(key)
        .expect("preview tool inserted");
    if name != "tool" {
        tool.name = name.to_owned();
    }
    tool
}

fn output<'a>(observer: &'a mut Observer, source: &OutputSource, channel: &str) -> &'a mut Output {
    let at = observer
        .outputs
        .iter()
        .position(|output| output.buffer.source() == source && output.channel == channel)
        .unwrap_or_else(|| {
            observer.outputs.push(Output {
                buffer: OutputBuffer::new(source.clone(), channel.into()),
                channel: channel.into(),
                rendered: 0,
                reading: false,
                failed: false,
            });
            observer.outputs.len() - 1
        });
    &mut observer.outputs[at]
}

fn observe_output(
    observer: &mut Observer,
    card: &mut AgentCard,
    source: OutputSource,
    channel: &str,
    event: &ObservationEvent,
) {
    if canonical(card, &source) || closed(observer, &source, channel) {
        return;
    }
    let output = output(observer, &source, channel);
    if output.failed {
        return;
    }
    if let Err(error) = output.buffer.event(event) {
        output.failed = true;
        card.observation_error = Some(error.to_string());
    }
}

fn close_output(observer: &mut Observer, source: OutputSource) {
    observer.outputs.retain(|output| {
        if output.buffer.source() != &source {
            return true;
        }
        observer
            .closed
            .push((source.clone(), output.channel.clone()));
        false
    });
}

fn canonical(card: &AgentCard, source: &OutputSource) -> bool {
    match source {
        OutputSource::Model { source } => {
            card.transcript
                .has_canonical_response(&RequestKey::response(
                    source.turn.turn_id.clone(),
                    source.response_index,
                ))
        }
        OutputSource::Toolcall { source } => card
            .transcript
            .find_tool(&tool_ref(source))
            .is_some_and(ToolCall::has_result),
    }
}

fn output_turn(source: &OutputSource) -> &TurnIdentity {
    match source {
        OutputSource::Model { source } => &source.turn,
        OutputSource::Toolcall { source } => &source.turn,
    }
}

fn tool_ref(source: &ToolCallIdentity) -> ToolCallRef {
    ToolCallRef {
        turn_id: source.turn.turn_id.clone(),
        tool_call_id: source.tool_call_id.clone(),
    }
}

async fn preview_history(
    client: &SessionClient,
    turns: &[TurnId],
) -> Result<HistoryWindow, HistoryError> {
    let mut window = HistoryWindow::tail(client, 60).await?;
    let mut records = Vec::new();
    loop {
        records.extend(
            std::mem::take(&mut window.items)
                .into_iter()
                .filter(|record| {
                    record
                        .turn_id
                        .as_ref()
                        .is_some_and(|turn| turns.contains(turn))
                }),
        );
        if records.len() >= agent_card::PREVIEW_BLOCKS || !window.has_more() {
            break;
        }
        window.items = window.older(client, 60).await?;
    }
    records.sort_by_key(|record| record.position);
    let discard = records.len().saturating_sub(agent_card::PREVIEW_BLOCKS);
    records.drain(..discard);
    window.items = records;
    Ok(window)
}
