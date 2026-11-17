//! Projection of native Host state, accepted records, and exact output ranges.

use minicore_host_core::*;
use minicore_host_protocol::*;
use minicore_host_runtime::model::Usage;
use minicore_host_transport::OutputBuffer;

use super::{App, LiveTurn, NoticeLevel, Overlay, Scroll, session::Output, tasks::OutputRead};
use crate::transcript::{Block, HistorySource, RequestKey, ToolCall, ToolKey};

const OUTPUT_PAGE_BYTES: u64 = 64 * 1024;

impl App {
    pub(super) fn on_host_state(&mut self, event: StateEvent) {
        match event {
            StateEvent::Host {
                host_id,
                host_incarnation,
                data,
            } => {
                let initial = self.client.initial_info();
                if host_id != initial.host_id || host_incarnation != initial.host_incarnation {
                    return;
                }
                if let Some(fault) = &data.shutdown_fault {
                    self.error(fault);
                }
                self.host_state = Some(data);
            }
            StateEvent::Session { source, data } => {
                let initial = self.client.initial_info();
                if source.host_id != initial.host_id
                    || source.host_incarnation != initial.host_incarnation
                {
                    return;
                }
                let target = SessionRef {
                    agent_id: source.agent_id.clone(),
                    session_id: source.session_id.clone(),
                };
                let snapshot = session_snapshot(source, data);
                if let Some(epoch) = self.resident.find(&target).map(|session| session.epoch) {
                    self.apply_snapshot(epoch, snapshot);
                } else {
                    let old_catalog = self.pending_states.get(&target).map(catalog_facts);
                    let changed = match self.pending_states.get_mut(&target) {
                        Some(current) => super::session::merge_snapshot(current, snapshot),
                        None => {
                            self.pending_states.insert(target.clone(), snapshot);
                            true
                        }
                    };
                    if changed {
                        let snapshot = self.pending_states[&target].clone();
                        self.project_catalog_state(&snapshot);
                        if old_catalog.as_ref() != Some(&catalog_facts(&snapshot))
                            || !self.sessions.iter().any(|session| {
                                session.agent_id == target.agent_id
                                    && session.id == target.session_id
                            })
                        {
                            self.refresh_catalogs();
                        }
                    }
                }
            }
            StateEvent::Turn { ref source, .. } => {
                let target = SessionRef {
                    agent_id: source.session.agent_id.clone(),
                    session_id: source.session.session_id.clone(),
                };
                if let Some(epoch) = self.resident.find(&target).map(|session| session.epoch) {
                    self.on_session_event(epoch, ObservationEvent::State(event));
                }
            }
            StateEvent::Toolcall { ref source, .. } => {
                let target = SessionRef {
                    agent_id: source.turn.session.agent_id.clone(),
                    session_id: source.turn.session.session_id.clone(),
                };
                if let Some(epoch) = self.resident.find(&target).map(|session| session.epoch) {
                    self.on_session_event(epoch, ObservationEvent::State(event));
                }
            }
        }
    }

    pub(super) fn install_baseline(&mut self, epoch: u64, baseline: SubscribeResult<StateEvent>) {
        self.session_mut(epoch).observation_error = None;
        for state in baseline.initial_states {
            self.on_session_event(epoch, ObservationEvent::State(state));
        }
        for position in baseline.output_positions {
            if !self.output_source_matches(epoch, &position.source) {
                continue;
            }
            if self.session_mut(epoch).closed_outputs.iter().any(|closed| {
                closed.source == position.source
                    && closed.channel == position.channel
                    && closed.end >= position.end
            }) {
                continue;
            }
            let final_position = source_is_canonical(self.session_mut(epoch), &position.source);
            let index = self.output_index(epoch, &position.source, &position.channel);
            let output = &mut self.session_mut(epoch).outputs[index];
            output.fixed_final |= final_position;
            output.failed = false;
            output.buffer.position(&position);
        }
        self.advance_outputs(epoch);
    }

    pub(super) fn apply_snapshot(&mut self, epoch: u64, snapshot: SessionSnapshot) {
        let old_catalog = self
            .resident
            .get(epoch)
            .map(|session| catalog_facts(&session.state));
        let old_revision = self
            .resident
            .get(epoch)
            .map(|session| session.state.data.controller_revision);
        let previous_interaction = self
            .resident
            .get(epoch)
            .and_then(|session| session.interaction.as_ref())
            .map(|pending| pending.data.interaction_id.clone());
        let previous_compaction = self
            .resident
            .get(epoch)
            .and_then(|session| session.state.data.compaction.as_ref())
            .map(|activity| serde_json::to_value(activity).expect("native compaction serializes"));
        if !self.session_mut(epoch).apply_state(snapshot) {
            return;
        }
        let snapshot = self.session_mut(epoch).state.clone();
        self.project_catalog_state(&snapshot);
        if old_catalog.as_ref() != Some(&catalog_facts(&snapshot)) {
            self.refresh_catalogs();
        }
        if old_revision != Some(snapshot.data.controller_revision) {
            self.refresh_inputs(epoch);
            self.refresh_context_for(epoch);
        }
        let current_interaction = self
            .session_mut(epoch)
            .interaction
            .as_ref()
            .map(|pending| pending.data.interaction_id.clone());
        if current_interaction.is_some()
            && current_interaction != previous_interaction
            && !self.resident.is_current(epoch)
        {
            self.background_approval(epoch);
        }
        let compaction =
            snapshot.data.compaction.as_ref().map(|activity| {
                serde_json::to_value(activity).expect("native compaction serializes")
            });
        if compaction != previous_compaction {
            self.refresh_history(epoch);
            self.refresh_context_for(epoch);
            if let Some(activity) = &snapshot.data.compaction
                && let CompactionState::Finished {
                    result: CompactionOutcome::Failed { fault },
                } = &activity.state
            {
                let target = self.session_mut(epoch).address();
                self.with_notice_for(&target, |app| {
                    app.notice(NoticeLevel::Error, format!("Compaction failed: {fault}"));
                });
            }
        }
        if let Some(result) = snapshot.data.last_result
            && result.session_id == self.session_mut(epoch).info.id
            && !self.session_mut(epoch).completed.contains(&result.turn_id)
        {
            self.read_turn(epoch, result.turn_id);
        }
    }

    pub(super) fn project_catalog_state(&mut self, snapshot: &SessionSnapshot) {
        if snapshot
            .data
            .deletion
            .as_ref()
            .is_some_and(|deletion| deletion.data.removed())
        {
            let keep = |row: &SessionSummary| {
                row.agent_id != snapshot.agent_id || row.id != snapshot.session_id
            };
            self.sessions.retain(keep);
            for agent in &mut self.agents {
                agent.sessions.retain(keep);
            }
            if let Overlay::Sessions {
                rows,
                selected,
                armed,
                renaming,
                ..
            } = &mut self.overlay
            {
                rows.retain(keep);
                *selected = (*selected).min(rows.len().saturating_sub(1));
                let target = SessionRef {
                    agent_id: snapshot.agent_id.clone(),
                    session_id: snapshot.session_id.clone(),
                };
                if armed.as_ref() == Some(&target) {
                    *armed = None;
                }
                if renaming.as_ref().is_some_and(|(known, _)| known == &target) {
                    *renaming = None;
                }
            }
            return;
        }
        let apply = |row: &mut SessionSummary| {
            if row.agent_id != snapshot.agent_id || row.id != snapshot.session_id {
                return;
            }
            let state = &snapshot.data;
            row.lifecycle = state.lifecycle;
            row.scheduling = state.scheduling;
            row.pause_reasons.clone_from(&state.pause_reasons);
            row.residency = state.residency;
            row.active_turn_id.clone_from(&state.active_turn_id);
            row.next_step = state.next_step.len();
            row.next_turn = state.next_turn.len();
            row.pending_interactions = state.pending_interactions.len();
            row.owned_work.clone_from(&state.owned_work);
            row.block_reason.clone_from(&state.block_reason);
            row.metadata_fault.clone_from(&state.metadata_fault);
            row.end_reason.clone_from(&state.end_reason);
            row.deletion = state
                .deletion
                .as_ref()
                .map(|deletion| deletion.data.clone());
        };
        for row in &mut self.sessions {
            apply(row);
        }
        for agent in &mut self.agents {
            for row in &mut agent.sessions {
                apply(row);
            }
        }
        if let Overlay::Sessions { rows, .. } = &mut self.overlay {
            for row in rows {
                apply(row);
            }
        }
    }

    /// Run after one complete message reduction, so a final baseline can finish
    /// installing before its observation tasks and resident view are dropped.
    pub(super) fn retire_removed_sessions(&mut self) {
        let removed: Vec<_> = self
            .resident
            .iter()
            .filter(|session| {
                session
                    .state
                    .data
                    .deletion
                    .as_ref()
                    .is_some_and(|deletion| deletion.data.removed())
            })
            .map(|session| (session.epoch, session.address(), session.state.clone()))
            .collect();
        for (epoch, target, snapshot) in removed {
            let current = self.resident.is_current(epoch);
            let session = self.session_mut(epoch);
            let drafts = session
                .draft
                .take()
                .into_iter()
                .chain(std::mem::take(&mut session.returned));
            let mut recovered = drafts.collect::<Vec<_>>().join("\n\n");
            if current && !self.draft_text.is_empty() {
                let neutral = std::mem::take(&mut self.draft_text);
                recovered = if recovered.is_empty() {
                    neutral
                } else {
                    format!("{neutral}\n\n{recovered}")
                };
            }
            if !recovered.is_empty() {
                if current || self.resident.current().is_none() {
                    if !self.composer.is_empty() {
                        recovered.insert_str(0, "\n\n");
                    }
                    self.composer
                        .place_cursor(self.composer.text().len(), false);
                    self.composer.insert(&recovered);
                } else {
                    if !self.draft_text.is_empty() {
                        self.draft_text.push_str("\n\n");
                    }
                    self.draft_text.push_str(&recovered);
                }
            }
            if current {
                // The composer keeps its existing text, paste tokens, and undo
                // history. Local sends remain in local_inputs with their
                // original immutable target, independent of this view.
                self.resident.select(None);
                self.scroll = Scroll::Follow;
                self.layout.clear();
                self.selection = None;
                if !matches!(
                    self.overlay,
                    Overlay::Unconfirmed { .. }
                        | Overlay::Sessions { .. }
                        | Overlay::Agents { .. }
                        | Overlay::Help { .. }
                        | Overlay::Error { .. }
                ) {
                    self.overlay = Overlay::None;
                }
            }
            self.project_catalog_state(&snapshot);
            self.pending_states.insert(target.clone(), snapshot);
            self.resident.remove(&target);
            self.notice(
                NoticeLevel::Info,
                format!("Session {} was removed", target.session_id),
            );
        }
    }

    pub(super) fn on_session_event(&mut self, epoch: u64, event: ObservationEvent) {
        if let Some(source) = event_session(&event)
            && !self
                .resident
                .get(epoch)
                .is_some_and(|session| session.identity_matches(source))
        {
            return;
        }
        if let Some(source) = event_turn(&event)
            && !self
                .resident
                .get(epoch)
                .is_some_and(|session| session.turn_matches(source))
        {
            return;
        }
        if let ObservationEvent::Activity(
            ActivityEvent::MessageAccepted { data, .. } | ActivityEvent::MessageBound { data, .. },
        ) = &event
        {
            for association in &data.delegations {
                self.register_delegation(association);
            }
        }
        self.observe_output(epoch, &event);
        match event {
            ObservationEvent::State(StateEvent::Host { .. }) => {}
            ObservationEvent::State(StateEvent::Session { source, data }) => {
                self.apply_snapshot(epoch, session_snapshot(source, data))
            }
            ObservationEvent::State(StateEvent::Turn { source, data }) => {
                self.apply_turn_state(epoch, source, data)
            }
            ObservationEvent::State(StateEvent::Toolcall { source, data }) => {
                self.apply_tool_state(epoch, source, data)
            }
            ObservationEvent::Activity(ActivityEvent::MessageAccepted { .. }) => {
                self.refresh_inputs(epoch)
            }
            ObservationEvent::Activity(ActivityEvent::MessageBound {
                source,
                message_id,
                data,
            }) => {
                if let (Some(position), Some(recorded_at)) =
                    (data.history_position, data.recorded_at)
                {
                    self.install_records(
                        epoch,
                        &[HistoryRecordData {
                            delegations: data.delegations,
                            position,
                            recorded_at,
                            turn_id: Some(source.turn_id),
                            response_index: None,
                            item: HistoryItemData::User {
                                received_at: Some(data.received_at),
                                text: data.text,
                                message_id: Some(message_id),
                                source: Some(data.source),
                                mode: Some(data.mode),
                                in_reply_to: data.in_reply_to,
                            },
                        }],
                    );
                } else {
                    self.refresh_history(epoch);
                }
                self.refresh_inputs(epoch);
            }
            ObservationEvent::Activity(ActivityEvent::ModelOutput { .. }) => {}
            ObservationEvent::Activity(ActivityEvent::ToolOutput { source, .. }) => {
                let key = tool_ref(&source);
                if self.session_mut(epoch).transcript.find_tool(&key).is_none() {
                    self.read_tool(epoch, key);
                }
            }
            ObservationEvent::Activity(ActivityEvent::ModelFinished { source, data }) => {
                self.install_records(
                    epoch,
                    &[HistoryRecordData {
                        delegations: Vec::new(),
                        position: data.history_position,
                        recorded_at: data.recorded_at,
                        turn_id: Some(source.turn.turn_id),
                        response_index: Some(source.response_index),
                        item: HistoryItemData::Assistant {
                            response: data.result,
                        },
                    }],
                );
            }
            ObservationEvent::Activity(ActivityEvent::ToolStarted { source, data }) => {
                let key = tool_ref(&source);
                let session = self.session_mut(epoch);
                if session.transcript.find_tool(&key).is_none() {
                    session.transcript.push(Block::Tool(Box::new(ToolCall::new(
                        key.clone(),
                        &data.name,
                    ))));
                }
                session
                    .transcript
                    .find_tool_mut(&key)
                    .expect("tool card inserted")
                    .apply_invocation(&data);
            }
            ObservationEvent::Activity(ActivityEvent::ToolFinished { source, data }) => {
                let key = tool_ref(&source);
                let invocation = self
                    .session_mut(epoch)
                    .transcript
                    .find_tool(&key)
                    .map(|call| (call.name.clone(), call.response_index));
                if let Some((name, response_index)) = invocation {
                    self.install_records(
                        epoch,
                        &[HistoryRecordData {
                            delegations: Vec::new(),
                            position: data.history_position,
                            recorded_at: data.recorded_at,
                            turn_id: Some(source.turn.turn_id),
                            response_index,
                            item: HistoryItemData::ToolResult {
                                tool_call_id: source.tool_call_id,
                                name,
                                result: data.result,
                            },
                        }],
                    );
                } else {
                    self.read_tool(epoch, key);
                    self.refresh_history(epoch);
                }
            }
            ObservationEvent::Activity(ActivityEvent::TurnFinished { source, data }) => {
                self.apply_turn_state(epoch, source, data)
            }
            ObservationEvent::Activity(ActivityEvent::SessionClosed { source, data }) => {
                self.apply_snapshot(epoch, session_snapshot(source, data))
            }
            ObservationEvent::Activity(
                ActivityEvent::InteractionRequested { .. }
                | ActivityEvent::InteractionResolved { .. },
            ) => self.refresh_control_for(epoch),
        }
        self.advance_outputs(epoch);
    }

    fn apply_turn_state(&mut self, epoch: u64, source: TurnIdentity, data: TurnStateData) {
        let session = self.session_mut(epoch);
        if !session.turn_matches(&source) {
            return;
        }
        if session
            .turn_revisions
            .get(&source.turn_id)
            .is_some_and(|(load, revision)| {
                load == &source.load_id && *revision >= data.runner_revision
            })
        {
            return;
        }
        session.turn_revisions.insert(
            source.turn_id.clone(),
            (source.load_id.clone(), data.runner_revision),
        );
        if session.state.data.active_turn_id.as_ref() == Some(&source.turn_id)
            && session.state.data.load_id == source.load_id
        {
            match &mut session.live {
                Some(live) if live.turn_id == source.turn_id && live.load_id == source.load_id => {
                    live.state = data.clone();
                    live.interrupting = data.state == TurnState::Cancelling;
                }
                _ => {
                    session.live = Some(LiveTurn {
                        turn_id: source.turn_id.clone(),
                        load_id: source.load_id.clone(),
                        state: data.clone(),
                        started: tokio::time::Instant::now(),
                        running_tool: session
                            .state
                            .data
                            .owned_work
                            .tools
                            .iter()
                            .find(|tool| tool.state == ToolCallState::Running)
                            .map(|tool| tool.name.clone()),
                        start: session.transcript.len(),
                        interrupting: data.state == TurnState::Cancelling,
                    })
                }
            }
        }
        if data.state != TurnState::Finished {
            return;
        }
        let fresh = session.completed.insert(source.turn_id.clone());
        if let Some(outcome) = data.outcome {
            if let (Some(position), Some(recorded_at), Some(finished_at)) =
                (data.history_position, data.recorded_at, data.finished_at)
            {
                self.install_records(
                    epoch,
                    &[HistoryRecordData {
                        delegations: Vec::new(),
                        position,
                        recorded_at,
                        turn_id: Some(source.turn_id.clone()),
                        response_index: None,
                        item: HistoryItemData::TurnFinished {
                            terminal: TurnTerminalData {
                                finished_at,
                                requests: data.requests,
                                tool_rounds: data.tool_rounds,
                                model_attempts: data.model_attempts.clone(),
                                outcome,
                                reason: data.fault.as_ref().map(|fault| fault.message().to_owned()),
                            },
                        },
                    }],
                );
            }
            self.set_terminal_facts(epoch, &source.turn_id, &data);
            if fresh && !self.resident.is_current(epoch) {
                self.background_turn_ended(epoch, outcome);
            }
        }
        if fresh {
            self.read_turn(epoch, source.turn_id);
            self.refresh_history(epoch);
            self.refresh_inputs(epoch);
            self.refresh_session_details_for(epoch);
            self.refresh_workspace_for(epoch);
        }
    }

    fn apply_tool_state(&mut self, epoch: u64, source: ToolCallIdentity, data: ToolCallStateData) {
        let key = tool_ref(&source);
        let session = self.session_mut(epoch);
        if !session.turn_matches(&source.turn) {
            return;
        }
        if session
            .tool_revisions
            .get(&key)
            .is_some_and(|(load, revision)| {
                load == &source.turn.load_id && *revision >= data.runner_revision
            })
        {
            return;
        }
        session
            .tool_revisions
            .insert(key.clone(), (source.turn.load_id, data.runner_revision));
        if session.transcript.find_tool(&key).is_none() {
            session.transcript.push(Block::Tool(Box::new(ToolCall::new(
                key.clone(),
                &data.name,
            ))));
        }
        let call = session
            .transcript
            .find_tool_mut(&key)
            .expect("tool card inserted");
        call.apply_state(&data);
        let needs_arguments = call.arguments.is_none();
        for association in &data.delegations {
            self.register_delegation(association);
        }
        if needs_arguments {
            self.read_tool(epoch, key);
        }
    }

    pub(super) fn install_tool(&mut self, epoch: u64, snapshot: ToolCallSnapshot) {
        if !self.session_mut(epoch).turn_matches(&snapshot.source.turn) {
            return;
        }
        let key = tool_ref(&snapshot.source);
        if self.session_mut(epoch).transcript.find_tool(&key).is_none() {
            self.session_mut(epoch)
                .transcript
                .push(Block::Tool(Box::new(ToolCall::new(
                    key.clone(),
                    &snapshot.data.state.name,
                ))));
        }
        self.session_mut(epoch)
            .transcript
            .find_tool_mut(&key)
            .expect("tool card inserted")
            .set_arguments(&snapshot.data.arguments);
        self.apply_tool_state(epoch, snapshot.source, snapshot.data.state.clone());
        if let Some(result) = &snapshot.data.result
            && let Some(call) = self.session_mut(epoch).transcript.find_tool_mut(&key)
        {
            call.apply_result(result);
        }
        self.advance_outputs(epoch);
    }

    /// Explicit user recovery retries reads of their original native objects.
    /// Completed state and all mutation receipts remain untouched.
    pub(super) fn retry_failed_reads(&mut self, epoch: u64) {
        let session = self.session_mut(epoch);
        let turns: Vec<_> = session.failed_turn_reads.keys().cloned().collect();
        let tools: Vec<_> = session.failed_tool_reads.keys().cloned().collect();
        for turn in turns {
            self.read_turn(epoch, turn);
        }
        for tool in tools {
            self.read_tool(epoch, tool);
        }
    }

    pub(super) fn install_turn(&mut self, epoch: u64, result: TurnResultSnapshot) {
        if !self.session_mut(epoch).turn_matches(&result.source) {
            return;
        }
        let turn_id = result.source.turn_id.clone();
        let mut records: Vec<_> = result
            .data
            .history
            .into_iter()
            .map(|record| HistoryRecordData {
                delegations: record.delegations,
                position: record.position,
                recorded_at: record.recorded_at,
                turn_id: Some(turn_id.clone()),
                response_index: record.response_index,
                item: record.item,
            })
            .chain(
                result
                    .data
                    .partials
                    .into_iter()
                    .map(|partial| HistoryRecordData {
                        delegations: Vec::new(),
                        position: partial.position,
                        recorded_at: partial.recorded_at,
                        turn_id: Some(turn_id.clone()),
                        response_index: Some(partial.response_index),
                        item: HistoryItemData::Partial {
                            partial: partial.partial,
                        },
                    }),
            )
            .collect();
        if let Some(saved) = result.data.saved_terminal {
            records.push(HistoryRecordData {
                delegations: Vec::new(),
                position: saved.position,
                recorded_at: saved.recorded_at,
                turn_id: Some(turn_id.clone()),
                response_index: None,
                item: HistoryItemData::TurnFinished {
                    terminal: saved.terminal,
                },
            });
            self.session_mut(epoch).completed.insert(turn_id.clone());
        }
        records.sort_by_key(|record| record.position);
        self.install_records(epoch, &records);
        if let Some(info) = result.data.info {
            self.apply_turn_state(epoch, result.source, info);
        } else {
            let transcript = &mut self.session_mut(epoch).transcript;
            if let Some(at) = transcript.blocks().iter().position(
                |block| matches!(block, Block::TurnEnd { turn_id: known, .. } if known == &turn_id),
            ) && let Some(Block::TurnEnd { saving, .. }) = transcript.block_mut(at)
            {
                *saving = Some(PersistenceState::Persisted);
            }
        }
        self.advance_outputs(epoch);
    }

    fn set_terminal_facts(&mut self, epoch: u64, turn_id: &TurnId, data: &TurnStateData) {
        let Some(outcome) = data.outcome else { return };
        let session = self.session_mut(epoch);
        let duration = None;
        let transcript = &mut session.transcript;
        let at = transcript.blocks().iter().position(
            |block| matches!(block, Block::TurnEnd { turn_id: known, .. } if known == turn_id),
        );
        if let Some(at) = at {
            if let Some(Block::TurnEnd {
                saving,
                duration: known_duration,
                ..
            }) = transcript.block_mut(at)
            {
                *saving = Some(data.persistence);
                *known_duration = known_duration.or(duration);
            }
        } else {
            transcript.push(Block::TurnEnd {
                turn_id: turn_id.clone(),
                outcome,
                reason: data.fault.as_ref().map(|fault| fault.message().to_owned()),
                duration,
                requests: data.requests,
                tool_rounds: data.tool_rounds,
                usage: Usage::default(),
                changes: None,
                saving: Some(data.persistence),
            });
        }
    }

    pub(super) fn install_records(&mut self, epoch: u64, records: &[HistoryRecordData]) {
        let anchor = match self.scroll_for(epoch) {
            Scroll::Anchor { block, offset } => {
                anchor_for(&self.session_mut(epoch).transcript, block)
                    .map(|anchor| (anchor, offset))
            }
            Scroll::Follow => None,
        };
        let session = self.session_mut(epoch);
        session.transcript.install_history_records(records);
        for record in records {
            if matches!(record.item, HistoryItemData::TurnFinished { .. })
                && let Some(turn) = &record.turn_id
            {
                session.completed.insert(turn.clone());
            }
        }
        if let Some(live) = &mut session.live {
            live.start = session
                .transcript
                .blocks()
                .iter()
                .position(|block| block.turn_id() == Some(&live.turn_id))
                .unwrap_or(session.transcript.len());
        }
        if let Some((anchor, offset)) = anchor
            && let Some(block) = find_anchor(&session.transcript, &anchor)
        {
            self.set_scroll_for(epoch, Scroll::Anchor { block, offset });
        }
        self.install_delegation_records(epoch, records);
        self.advance_outputs(epoch);
    }

    fn output_source_matches(&self, epoch: u64, source: &OutputSource) -> bool {
        self.resident
            .get(epoch)
            .is_some_and(|session| session.turn_matches(output_turn(source)))
    }

    fn output_index(&mut self, epoch: u64, source: &OutputSource, channel: &str) -> usize {
        let session = self.session_mut(epoch);
        if let Some(at) = session
            .outputs
            .iter()
            .position(|output| output.buffer.source() == source && output.channel == channel)
        {
            return at;
        }
        session.outputs.push(Output {
            buffer: OutputBuffer::new(source.clone(), channel.to_owned()),
            channel: channel.to_owned(),
            rendered: 0,
            reading: false,
            failed: false,
            fixed_final: false,
        });
        session.outputs.len() - 1
    }

    fn observe_output(&mut self, epoch: u64, event: &ObservationEvent) {
        let (source, channels) = match event {
            ObservationEvent::Activity(ActivityEvent::ModelOutput { source, data }) => (
                OutputSource::Model {
                    source: source.clone(),
                },
                vec![data.channel.clone()],
            ),
            ObservationEvent::Activity(ActivityEvent::ToolOutput { source, data }) => (
                OutputSource::Toolcall {
                    source: source.clone(),
                },
                vec![data.channel.clone()],
            ),
            ObservationEvent::Activity(ActivityEvent::ModelFinished { source, data }) => (
                OutputSource::Model {
                    source: source.clone(),
                },
                data.output_positions
                    .iter()
                    .map(|position| position.channel.clone())
                    .collect(),
            ),
            ObservationEvent::Activity(ActivityEvent::ToolFinished { source, data }) => (
                OutputSource::Toolcall {
                    source: source.clone(),
                },
                data.output_positions
                    .iter()
                    .map(|position| position.channel.clone())
                    .collect(),
            ),
            _ => return,
        };
        for channel in channels {
            if self
                .session_mut(epoch)
                .closed_outputs
                .iter()
                .any(|closed| closed.source == source && closed.channel == channel)
            {
                continue;
            }
            let index = self.output_index(epoch, &source, &channel);
            let output = &mut self.session_mut(epoch).outputs[index];
            if let Err(error) = output.buffer.event(event) {
                output.failed = true;
                self.session_mut(epoch).observation_error = Some(error.to_string());
                self.error(error);
            }
        }
    }

    pub(super) fn output_read_result(
        &mut self,
        epoch: u64,
        source: OutputSource,
        channel: String,
        result: Result<OutputRead, String>,
    ) {
        if !self.output_source_matches(epoch, &source) {
            return;
        }
        let Some(at) = self
            .session_mut(epoch)
            .outputs
            .iter()
            .position(|output| output.buffer.source() == &source && output.channel == channel)
        else {
            return;
        };
        let output = &mut self.session_mut(epoch).outputs[at];
        output.reading = false;
        let result = result.and_then(|read| {
            match read {
                OutputRead::Model(read) => output.buffer.model_read(&read),
                OutputRead::Tool(read) => output.buffer.tool_read(&read),
            }
            .map_err(|error| error.to_string())
        });
        if let Err(error) = result {
            output.failed = true;
            self.session_mut(epoch).observation_error = Some(error.clone());
            self.notice(NoticeLevel::Error, error);
        }
        self.advance_outputs(epoch);
    }

    fn advance_outputs(&mut self, epoch: u64) {
        let session = self.session_mut(epoch);
        let mut reads = Vec::new();
        for output in &mut session.outputs {
            let source = output.buffer.source();
            let text = output.buffer.text();
            if output.rendered < text.len() {
                let delta = &text[output.rendered..];
                match source {
                    OutputSource::Model { source } => {
                        let key = RequestKey::response(
                            source.turn.turn_id.clone(),
                            source.response_index,
                        );
                        if !session.transcript.has_canonical_response(&key)
                            && let Block::Assistant {
                                reasoning, text, ..
                            } = session.transcript.assistant_mut(&key)
                        {
                            match output.channel.as_str() {
                                "text" => text.push_str(delta),
                                "reasoning" => reasoning.push_str(delta),
                                _ => {}
                            }
                        }
                        output.rendered = output.buffer.text().len();
                    }
                    OutputSource::Toolcall { source } => {
                        if let Some(call) = session.transcript.find_tool_mut(&tool_ref(source)) {
                            call.push_output(delta);
                            output.rendered = output.buffer.text().len();
                        }
                    }
                }
            }
            if !output.reading
                && !output.failed
                && let Some(request) = output.buffer.next_read(OUTPUT_PAGE_BYTES)
            {
                output.reading = true;
                reads.push((source.clone(), request));
            }
        }
        let mut closed = Vec::new();
        session.outputs.retain(|output| {
            let canonical_model = matches!(output.buffer.source(), OutputSource::Model { source } if session.transcript.has_canonical_response(&RequestKey::response(source.turn.turn_id.clone(), source.response_index)));
            let complete = (output.fixed_final || output.buffer.is_complete() || canonical_model)
                && !output.reading && output.buffer.next_read(OUTPUT_PAGE_BYTES).is_none();
            if complete && output.rendered == output.buffer.text().len() {
                closed.push(OutputPosition { source: output.buffer.source().clone(), channel: output.channel.clone(), end: output.buffer.text().len() as u64 });
                false
            } else { true }
        });
        session.closed_outputs.extend(closed);
        for (source, request) in reads {
            self.read_output(epoch, source, request);
        }
    }
}

pub(super) fn session_snapshot(source: SessionIdentity, data: SessionStateData) -> SessionSnapshot {
    SessionSnapshot {
        host_id: source.host_id,
        host_incarnation: source.host_incarnation,
        agent_id: source.agent_id,
        session_id: source.session_id,
        data,
    }
}
fn tool_ref(source: &ToolCallIdentity) -> ToolCallRef {
    ToolCallRef {
        turn_id: source.turn.turn_id.clone(),
        tool_call_id: source.tool_call_id.clone(),
    }
}
fn output_turn(source: &OutputSource) -> &TurnIdentity {
    match source {
        OutputSource::Model { source } => &source.turn,
        OutputSource::Toolcall { source } => &source.turn,
    }
}
pub(super) fn event_turn(event: &ObservationEvent) -> Option<&TurnIdentity> {
    match event {
        ObservationEvent::State(StateEvent::Turn { source, .. }) => Some(source),
        ObservationEvent::State(StateEvent::Toolcall { source, .. }) => Some(&source.turn),
        ObservationEvent::Activity(
            ActivityEvent::ModelOutput { source, .. } | ActivityEvent::ModelFinished { source, .. },
        ) => Some(&source.turn),
        ObservationEvent::Activity(
            ActivityEvent::ToolOutput { source, .. }
            | ActivityEvent::ToolStarted { source, .. }
            | ActivityEvent::ToolFinished { source, .. }
            | ActivityEvent::InteractionRequested { source, .. }
            | ActivityEvent::InteractionResolved { source, .. },
        ) => Some(&source.turn),
        ObservationEvent::Activity(
            ActivityEvent::MessageBound { source, .. } | ActivityEvent::TurnFinished { source, .. },
        ) => Some(source),
        _ => None,
    }
}
pub(super) fn event_session(event: &ObservationEvent) -> Option<&SessionIdentity> {
    event_turn(event).map(|turn| &turn.session).or(match event {
        ObservationEvent::State(StateEvent::Session { source, .. })
        | ObservationEvent::Activity(
            ActivityEvent::MessageAccepted { source, .. }
            | ActivityEvent::SessionClosed { source, .. },
        ) => Some(source),
        _ => None,
    })
}
enum Anchor {
    History(HistorySource),
    Response(RequestKey),
    Tool(ToolKey),
    Turn(TurnId),
}
fn anchor_for(transcript: &crate::transcript::Transcript, at: usize) -> Option<Anchor> {
    if let Some(source) = transcript.history_source(at) {
        return Some(Anchor::History(source.clone()));
    }
    match transcript.blocks().get(at)? {
        Block::Assistant { key, .. } => Some(Anchor::Response(key.clone())),
        Block::Tool(call) => Some(Anchor::Tool(call.key.clone())),
        Block::TurnEnd { turn_id, .. } => Some(Anchor::Turn(turn_id.clone())),
        _ => None,
    }
}
fn find_anchor(transcript: &crate::transcript::Transcript, anchor: &Anchor) -> Option<usize> {
    match anchor {
        Anchor::History(source) => transcript.find_history_source(source),
        Anchor::Response(key) => transcript
            .blocks()
            .iter()
            .position(|block| matches!(block, Block::Assistant { key: known, .. } if known == key)),
        Anchor::Tool(key) => transcript
            .blocks()
            .iter()
            .position(|block| matches!(block, Block::Tool(call) if &call.key == key)),
        Anchor::Turn(turn) => transcript
            .blocks()
            .iter()
            .position(|block| matches!(block, Block::TurnEnd { turn_id, .. } if turn_id == turn)),
    }
}

fn source_is_canonical(session: &super::Session, source: &OutputSource) -> bool {
    session.completed.contains(&output_turn(source).turn_id)
        || match source {
            OutputSource::Model { source } => {
                session
                    .transcript
                    .has_canonical_response(&RequestKey::response(
                        source.turn.turn_id.clone(),
                        source.response_index,
                    ))
            }
            OutputSource::Toolcall { source } => session
                .transcript
                .find_tool(&tool_ref(source))
                .is_some_and(|call| call.state.is_terminal()),
        }
}

fn catalog_facts(
    snapshot: &SessionSnapshot,
) -> (
    SessionLifecycle,
    Scheduling,
    Vec<PauseReason>,
    Option<SessionDeletionState>,
    bool,
) {
    (
        snapshot.data.lifecycle,
        snapshot.data.scheduling,
        snapshot.data.pause_reasons.clone(),
        snapshot.data.deletion.clone(),
        snapshot.data.metadata_saving,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use minicore_host_runtime::model::{AssistantPart, ModelFinishReason};
    use minicore_host_transport::HostClient;
    use std::sync::Arc;

    struct NoExecutionResources;
    impl ResourceResolver for NoExecutionResources {
        fn validate(&self, _: &SessionConfig) -> Result<(), Fault> {
            Ok(())
        }
        fn resolve<'a>(&'a self, _: &'a SessionConfig) -> ResourceFuture<'a> {
            Box::pin(async {
                Err(Fault::new(
                    FaultKind::SessionResourceMissing,
                    "test fixture does not execute models",
                ))
            })
        }
    }

    async fn app() -> App {
        let host = Host::memory(Arc::new(NoExecutionResources)).unwrap();
        let client = HostClient::in_process(host).unwrap();
        let created = client
            .create_agent(
                AgentCreate {
                    operation_id: OperationId::new().unwrap(),
                    name: "projection fixture".into(),
                    mode: StorageMode::Memory,
                    config: AgentConfig::default(),
                    default_session: None,
                    initial_message: None,
                },
                CallOptions::default(),
            )
            .await
            .unwrap();
        let session = client.session(SessionRef {
            agent_id: created.agent_id.unwrap(),
            session_id: created.session_id.unwrap(),
        });
        let info = session.info(CallOptions::default()).await.unwrap();
        let state = session.state(CallOptions::default()).await.unwrap();
        let (tx, _) = tokio::sync::mpsc::channel(crate::app::tasks::MESSAGE_CAPACITY);
        let mut app = App::new(client, tx, std::path::PathBuf::from("."));
        app.resident
            .insert(super::super::Session::new(info, state, 1, session));
        app.resident.select(Some(1));
        app
    }

    fn source(app: &App) -> ModelOutputIdentity {
        let session = app.resident.current().unwrap();
        ModelOutputIdentity {
            turn: TurnIdentity {
                session: SessionIdentity {
                    host_id: session.state.host_id.clone(),
                    host_incarnation: session.state.host_incarnation.clone(),
                    agent_id: session.info.agent_id.clone(),
                    session_id: session.info.id.clone(),
                },
                turn_id: "00000000000000000001".parse().unwrap(),
                load_id: None,
            },
            response_index: 0,
        }
    }

    fn stamp() -> Timestamp {
        Timestamp::from_unix_milliseconds(42).unwrap()
    }

    fn delta(source: &ModelOutputIdentity, start: u64, end: u64, text: &str) -> ObservationEvent {
        ObservationEvent::Activity(ActivityEvent::ModelOutput {
            source: source.clone(),
            data: OutputDelta {
                channel: "text".into(),
                start,
                end,
                text: text.into(),
            },
        })
    }

    #[tokio::test(start_paused = true)]
    async fn canonical_completion_closes_streams_after_unicode_gap_recovery() {
        let mut app = app().await;
        let source = source(&app);
        app.on_session_event(1, delta(&source, 4, 8, "🙂"));
        assert!(app.session_mut(1).transcript.last_answer().is_none());
        app.output_read_result(
            1,
            OutputSource::Model {
                source: source.clone(),
            },
            "text".into(),
            Ok(OutputRead::Model(ModelTextSnapshot {
                source: source.clone(),
                data: TextRangeData {
                    channel: "text".into(),
                    start: 0,
                    end: 4,
                    text: "αβ".into(),
                    has_more: false,
                },
            })),
        );
        assert_eq!(app.session_mut(1).transcript.last_answer(), Some("αβ🙂"));
        app.on_session_event(
            1,
            ObservationEvent::Activity(ActivityEvent::ModelFinished {
                source: source.clone(),
                data: ModelFinishedData {
                    history_position: 0,
                    recorded_at: stamp(),
                    result: ModelResponseData {
                        content: vec![AssistantPart::Text("αβ🙂".into())],
                        finish_reason: ModelFinishReason::Stop,
                        usage: Usage::default(),
                    },
                    output_positions: vec![ChannelPosition {
                        channel: "text".into(),
                        end: 8,
                    }],
                },
            }),
        );
        assert!(app.session_mut(1).outputs.is_empty());
        app.on_session_event(1, delta(&source, 0, 8, "αβ🙂"));
        assert_eq!(app.session_mut(1).transcript.last_answer(), Some("αβ🙂"));
        assert_eq!(app.session_mut(1).transcript.len(), 1);
        assert!(app.session_mut(1).outputs.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn foreign_session_output_does_not_create_a_block_or_read() {
        let mut app = app().await;
        let mut source = source(&app);
        source.turn.session.host_incarnation = "00000000000000000009".parse().unwrap();
        app.on_session_event(1, delta(&source, 0, 4, "text"));
        assert!(app.session_mut(1).transcript.is_empty());
        assert!(app.session_mut(1).outputs.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn active_turn_output_requires_its_authoritative_load_before_a_turn_event_arrives() {
        let mut app = app().await;
        let mut source = source(&app);
        app.session_mut(1).state.data.active_turn_id = Some(source.turn.turn_id.clone());
        app.session_mut(1).state.data.load_id = Some("00000000000000000005".parse().unwrap());
        source.turn.load_id = Some("00000000000000000006".parse().unwrap());
        app.on_session_event(1, delta(&source, 0, 4, "text"));
        assert!(app.session_mut(1).transcript.is_empty());
        assert!(app.session_mut(1).outputs.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn late_turn_completion_does_not_replace_the_successor() {
        let mut app = app().await;
        let source = source(&app).turn;
        let successor: TurnId = "00000000000000000002".parse().unwrap();
        app.session_mut(1).state.data.active_turn_id = Some(successor.clone());
        let data: TurnStateData = serde_json::from_value(serde_json::json!({
            "history_position":0,"recorded_at":"2026-11-13T00:00:00Z","finished_at":"2026-11-13T00:00:00Z",
            "state":"finished","runner_revision":5,"config_revision":0,"outcome":"completed","persistence":"not_applicable",
            "requests":1,"tool_rounds":0,"phase":null,"model_retry":null,"model_attempts":[1],"fault":null,"persistence_fault":null
        })).unwrap();
        app.on_session_event(
            1,
            ObservationEvent::Activity(ActivityEvent::TurnFinished {
                source: source.clone(),
                data,
            }),
        );
        assert_eq!(
            app.session_mut(1).state.data.active_turn_id.as_ref(),
            Some(&successor)
        );
        assert!(app.session_mut(1).transcript.blocks().iter().any(|block| matches!(block, Block::TurnEnd { turn_id, saving: Some(PersistenceState::NotApplicable), .. } if turn_id == &source.turn_id)));
    }

    #[tokio::test(start_paused = true)]
    async fn repeated_bound_messages_are_identified_by_record_position() {
        let mut app = app().await;
        let source = source(&app).turn;
        let event = |position| {
            ObservationEvent::Activity(ActivityEvent::MessageBound {
                source: source.clone(),
                message_id: "00000000000000000003".parse().unwrap(),
                data: MessageEventData {
                    delegations: Vec::new(),
                    received_at: stamp(),
                    history_position: Some(position),
                    recorded_at: Some(stamp()),
                    text: "identical".into(),
                    mode: MessageMode::Steer,
                    source: MessageSource::User,
                    queue: InputQueue::NextStep,
                    order: position,
                    in_reply_to: None,
                    controller_revision: 1,
                },
            })
        };
        app.on_session_event(1, event(0));
        app.on_session_event(1, event(1));
        app.on_session_event(1, event(0));
        assert_eq!(app.session_mut(1).transcript.len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn finished_tool_output_is_not_appended_again_after_buffer_release() {
        let mut app = app().await;
        let model = source(&app);
        let source = ToolCallIdentity {
            turn: model.turn,
            tool_call_id: ToolCallId("call".into()),
        };
        let key = tool_ref(&source);
        app.on_session_event(
            1,
            ObservationEvent::Activity(ActivityEvent::ToolStarted {
                source: source.clone(),
                data: ToolInvocationData {
                    name: "bash".into(),
                    arguments: serde_json::json!({"command":"echo hello","timeout_seconds":5}),
                },
            }),
        );
        let output = || {
            ObservationEvent::Activity(ActivityEvent::ToolOutput {
                source: source.clone(),
                data: OutputDelta {
                    channel: "stdout".into(),
                    start: 0,
                    end: 6,
                    text: "hello\n".into(),
                },
            })
        };
        app.on_session_event(1, output());
        app.on_session_event(
            1,
            ObservationEvent::Activity(ActivityEvent::ToolFinished {
                source: source.clone(),
                data: ToolFinishedData {
                    history_position: 0,
                    recorded_at: stamp(),
                    result: ToolResultData {
                        outcome: minicore_host_runtime::tools::ToolResultOutcome::Success,
                        output: minicore_host_runtime::tools::ToolOutput::new("model result")
                            .unwrap(),
                    },
                    output_positions: vec![ChannelPosition {
                        channel: "stdout".into(),
                        end: 6,
                    }],
                },
            }),
        );
        assert!(app.session_mut(1).outputs.is_empty());
        app.on_session_event(1, output());
        let call = app.session_mut(1).transcript.find_tool(&key).unwrap();
        assert_eq!(call.output_content(), ["hello"]);
        assert_eq!(call.arguments.as_ref().unwrap()["timeout_seconds"], 5);
        assert_eq!(call.state, crate::transcript::ToolState::Succeeded);
        assert!(app.session_mut(1).outputs.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn reading_a_retained_partial_does_not_claim_persistence_or_load_the_session() {
        let mut app = app().await;
        let mut source = source(&app).turn;
        source.load_id = Some("00000000000000000005".parse().unwrap());
        let info: TurnStateData = serde_json::from_value(serde_json::json!({
            "history_position":1,"recorded_at":"2026-11-13T00:00:00Z","finished_at":"2026-11-13T00:00:00Z",
            "state":"finished","runner_revision":5,"config_revision":0,"outcome":"cancelled","persistence":"failed",
            "requests":1,"tool_rounds":0,"phase":null,"model_retry":null,"model_attempts":[1],"fault":null,"persistence_fault":null
        })).unwrap();
        let result = TurnResultSnapshot {
            source,
            data: TurnResultData {
                info: Some(info),
                config: Some(app.session_mut(1).info.config.clone()),
                saved_terminal: None,
                history: Vec::new(),
                partials: vec![TurnPartialData {
                    position: 0,
                    recorded_at: stamp(),
                    response_index: 0,
                    partial: ModelPartialData {
                        content: vec![AssistantPart::Text("unfinished actual text".into())],
                        incomplete_tool_calls: Vec::new(),
                        usage: None,
                    },
                }],
            },
        };
        app.install_turn(1, result);
        assert!(app.session_mut(1).state.data.load_id.is_none());
        assert!(
            matches!(&app.session_mut(1).transcript.blocks()[0], Block::Assistant { partial: true, text, .. } if text == "unfinished actual text")
        );
        assert!(matches!(
            &app.session_mut(1).transcript.blocks()[1],
            Block::TurnEnd {
                saving: Some(PersistenceState::Failed),
                ..
            }
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn a_new_load_discards_context_from_the_previous_residency() {
        let mut app = app().await;
        let client = app.session_mut(1).client.clone();
        let context = client.context(CallOptions::default()).await.unwrap();
        app.session_mut(1).context = Some(context);
        let mut state = app.session_mut(1).state.clone();
        state.data.controller_revision += 1;
        state.data.load_id = Some("00000000000000000005".parse().unwrap());
        state.data.residency = Residency::Loaded;
        app.apply_snapshot(1, state);
        assert!(app.session_mut(1).context.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn native_removed_session_releases_its_view_and_keeps_uncertain_input_target() {
        let mut app = app().await;
        let target = app.session_mut(1).address();
        let original = app.session_mut(1).target();
        let client = app.session_mut(1).client.clone();
        let stale_catalog =
            super::super::tasks::catalogs(&app.client, Some(target.agent_id.clone()))
                .await
                .unwrap();
        let mut events = super::super::tasks::subscribe_session(&app.client, target.clone())
            .await
            .unwrap();
        assert!(app.push_local_input(
            1,
            super::super::composer::Entry::new("unconfirmed original input".into()),
            MessageMode::Steer,
            None,
        ));
        let id = app.local_inputs[0].id;
        // This is an acknowledgement-loss fixture; deletion and all state
        // observations below come from the actual in-process Host.
        app.on_local_pushed(id, Err(CallError::WaitTimeout { wait_timeout_ms: 1 }));
        app.composer.set_text("an unsent draft");
        client
            .delete(
                SessionDeleteRequest {
                    operation_id: OperationId::new().unwrap(),
                    close: true,
                },
                CallOptions::default(),
            )
            .await
            .unwrap();
        crate::testing::real_bounded(async {
            while app.resident.get(1).is_some() {
                let notification = events.recv().await.unwrap().expect("native removal fact");
                if let ObservationNotification::Event(push) = notification {
                    app.on_msg(super::super::Msg::Session(
                        1,
                        Box::new(super::super::SessionOp::Observed {
                            generation: 0,
                            event: Box::new(push.event),
                        }),
                    ));
                }
            }
        })
        .await;
        assert!(app.resident.current().is_none());
        assert_eq!(app.composer.text(), "an unsent draft");
        assert_eq!(app.local_inputs.len(), 1);
        assert_eq!(app.local_inputs[0].target, original);
        assert_eq!(
            app.local_inputs[0].status,
            super::super::unconfirmed::Status::Unconfirmed
        );
        app.on_msg(super::super::Msg::Catalogs(Ok(stale_catalog)));
        assert!(!app.sessions.iter().any(|row| row.id == target.session_id));
        assert!(
            !app.agents
                .iter()
                .flat_map(|agent| &agent.sessions)
                .any(|row| row.id == target.session_id)
        );
        assert!(
            app.pending_states[&target]
                .data
                .deletion
                .as_ref()
                .unwrap()
                .data
                .removed()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn explicit_recovery_retries_a_failed_read_of_an_already_finished_native_turn() {
        use crate::testing::{NativeHost, real_bounded};
        let mut fixture = NativeHost::new().await;
        let client = fixture.client.session(fixture.target.clone());
        let mut events =
            super::super::tasks::subscribe_session(&fixture.client, fixture.target.clone())
                .await
                .unwrap();
        client
            .send(
                MessageInput {
                    message_id: MessageId::new().unwrap(),
                    mode: MessageMode::Steer,
                    text: "recover this actual result".into(),
                    in_reply_to: None,
                },
                CallOptions::default(),
            )
            .await
            .unwrap();
        let call = real_bounded(fixture.calls.recv()).await.unwrap();
        call.text("retained complete result");
        call.finish();
        let event = real_bounded(async {
            loop {
                let notification = events.recv().await.unwrap().unwrap();
                if let ObservationNotification::Event(push) = notification
                    && matches!(
                        push.event,
                        ObservationEvent::Activity(ActivityEvent::TurnFinished { .. })
                    )
                {
                    break push.event;
                }
            }
        })
        .await;
        let turn = event_turn(&event).unwrap().turn_id.clone();
        let info = client.info(CallOptions::default()).await.unwrap();
        let state = client.state(CallOptions::default()).await.unwrap();
        let (tx, mut messages) = tokio::sync::mpsc::channel(super::super::tasks::MESSAGE_CAPACITY);
        let mut app = App::new(fixture.client.clone(), tx, std::path::PathBuf::from("."));
        app.resident
            .insert(super::super::Session::new(info, state, 1, client));
        app.resident.select(Some(1));
        app.on_session_event(1, event);
        real_bounded(async {
            loop {
                let message = messages.recv().await.unwrap();
                if let super::super::Msg::Session(1, op) = message {
                    match *op {
                        super::super::SessionOp::Turn(known, Ok(result)) if known == turn => {
                            assert_eq!(result.source.turn_id, turn);
                            // Replace only this read's delivered result to
                            // model a failed read; the Turn itself is real.
                            app.on_op(
                                1,
                                super::super::SessionOp::Turn(
                                    known,
                                    Err("injected read failure".into()),
                                ),
                            );
                            break;
                        }
                        other => app.on_op(1, other),
                    }
                }
            }
        })
        .await;
        assert!(app.session_mut(1).completed.contains(&turn));
        assert!(app.session_mut(1).failed_turn_reads.contains_key(&turn));
        assert!(app.session_mut(1).turn_reads.is_empty());
        app.on_action(super::super::Action::RetryRecovery);
        assert!(app.session_mut(1).turn_reads.contains(&turn));
        real_bounded(async {
            while app.session_mut(1).failed_turn_reads.contains_key(&turn) {
                app.on_msg(messages.recv().await.unwrap());
            }
        })
        .await;
        assert!(app.session_mut(1).completed.contains(&turn));
        assert_eq!(
            app.session_mut(1).transcript.last_answer(),
            Some("retained complete result")
        );
        assert!(app.session_mut(1).observation_error.is_none());
        messages.close();
        drop(app);
        drop(events);
        fixture.finish().await;
    }
}
