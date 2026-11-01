//! Applying the authoritative control snapshot to a live session.
//!
//! Nothing else writes execution, queue, pause, interaction, compaction or
//! blocking: command receipts may notify, return text and schedule reads, but
//! they no longer decide control state.

use crate::protocol::{
    AgentEvent, BlockReason, ExecutionPhase, SessionExecution, SessionSnapshot, TurnRef,
};

use super::{LiveStatus, LiveTurn, PendingInteraction, Phase, Session};
use crate::protocol::read::UserKind;
use crate::transcript::Block;

impl Session {
    /// Replaces every control field with one snapshot's observation.
    pub fn apply_control(&mut self, snapshot: &SessionSnapshot) {
        self.execution = Some(snapshot.execution.clone());
        self.catalog = snapshot.completions.clone();
        let started = self.apply_execution(&snapshot.execution);
        self.queue = snapshot.queue.clone();
        self.compaction = snapshot.compaction.clone();
        self.block_reason = snapshot.block_reason;
        let unchanged = self
            .interaction
            .as_ref()
            .zip(snapshot.pending_interaction.as_ref())
            .is_some_and(|(known, pending)| {
                known.interaction_id == pending.interaction.interaction_id
                    && known.turn.loop_id == pending.loop_id
            });
        if !unchanged {
            self.interaction_editor = super::composer::Composer::new();
            self.interaction = snapshot
                .pending_interaction
                .as_ref()
                .map(|pending| PendingInteraction::from_snapshot(snapshot, pending));
            if self.interaction.is_some() {
                self.armed_at = tokio::time::Instant::now() + super::session::ARM_AFTER;
            }
        }
        if let Some(turn) = started {
            self.adopted = Some(turn);
        }
    }

    /// Makes the snapshot's execution the live turn, or clears it. Returns the
    /// turn this observation started, which is what the client waits on.
    fn apply_execution(&mut self, execution: &SessionExecution) -> Option<TurnRef> {
        let observed = execution.loop_id();
        if let Some(live) = self.live.as_mut() {
            match observed {
                Some(loop_id) if live.turn.loop_id == loop_id => {
                    if let Some(phase) = execution.phase() {
                        live.phase = phase_of(phase);
                    } else {
                        live.phase = Phase::Waiting;
                    }
                    return None;
                }
                Some(_) | None => {}
            }
        }
        self.retire_live();
        let loop_id = observed?;
        let start = self.transcript.len();
        if let Some(input) = execution.input() {
            self.transcript.push(Block::User {
                text: input.text.clone(),
                kind: UserKind::Prompt,
                at: Some(super::session::accepted_millis(
                    input.accepted_at.as_deref(),
                )),
            });
        }
        #[cfg(feature = "e2e-observe")]
        if let Some(input) = execution.input() {
            self.transcript
                .identify_user(start, loop_id, Some(&input.id));
        }
        self.record_region(loop_id, start);
        let mut live = LiveTurn::new(
            TurnRef::new(self.info.session_id.clone(), loop_id.to_owned()),
            start,
        );
        if let Some(phase) = execution.phase() {
            live.phase = phase_of(phase);
        }
        self.live = Some(live);
        Some(TurnRef::new(
            self.info.session_id.clone(),
            loop_id.to_owned(),
        ))
    }

    /// What the dock and the tab title say the session is doing.
    pub fn control_status(&self) -> LiveStatus {
        if self.interaction.is_some() {
            LiveStatus::Approval
        } else if self.compaction.is_some() {
            LiveStatus::Compacting
        } else if self.is_busy() {
            LiveStatus::Running
        } else {
            LiveStatus::Idle
        }
    }

    /// Why the session cannot start a turn, for the status line.
    pub fn block_label(&self) -> Option<&'static str> {
        self.block_reason.map(BlockReason::label)
    }

    /// Applies one event to a live session: body content only. Control state
    /// arrives through [`Session::apply_control`].
    pub fn apply_body_event(&mut self, event: AgentEvent, bytes: usize) {
        if matches!(
            event,
            AgentEvent::InteractionRequested { .. }
                | AgentEvent::InteractionResolved { .. }
                | AgentEvent::TurnStarted { .. }
                | AgentEvent::TurnFinished { .. }
        ) {
            self.apply_event(event);
            return;
        }
        if let Some(loop_id) = event.loop_id().map(str::to_owned) {
            if self.is_closed(&loop_id) && !event.is_tool_metadata() {
                return;
            }
            if !self.knows_turn(&loop_id) {
                self.cache_early(event, bytes);
                return;
            }
        }
        self.apply_event(event);
    }
}

fn phase_of(phase: ExecutionPhase) -> Phase {
    match phase {
        ExecutionPhase::Unknown | ExecutionPhase::Starting => Phase::Thinking,
        ExecutionPhase::RunningModel => Phase::Waiting,
        ExecutionPhase::RunningTools => Phase::Thinking,
        ExecutionPhase::WaitingForInput => Phase::Waiting,
        ExecutionPhase::Finishing | ExecutionPhase::Finished => Phase::Waiting,
    }
}

impl super::App {
    /// Applies one authoritative control snapshot; it is the only entry that
    /// writes execution, queue, pause, interaction, compaction and blocking.
    pub fn apply_snapshot(&mut self, snapshot: SessionSnapshot) {
        let id = snapshot.session_id.clone();
        let Some(epoch) = self.live(&id).map(|session| session.epoch) else {
            return;
        };
        let observed_before = self.resident.get(epoch).unwrap().recovery.observed_through;
        let applied = self.apply_to_session(epoch, &snapshot);
        if applied {
            let background = !self.resident.is_current(epoch);
            let mut ring = false;
            for turn in snapshot
                .completions
                .entries
                .iter()
                .filter(|turn| turn.seq > observed_before)
            {
                let session = self.session_mut(epoch);
                if let Some(live) = session.tracked_turn_mut(&turn.loop_id)
                    && live.completion_background.is_none()
                {
                    live.completion_background = Some(background);
                    ring |= live.started.elapsed() >= crate::title::LONG_TURN;
                }
            }
            self.ring |= ring;
            let other: usize = self
                .resident
                .iter()
                .filter(|session| session.epoch != epoch)
                .map(|session| session.recovery.entries.len())
                .sum();
            self.session_mut(epoch)
                .recovery
                .trim(1024usize.saturating_sub(other));
            if snapshot.completions.through_seq > observed_before {
                self.stamp_activity(epoch);
            }
            if let Some(title) = self
                .resident
                .get(epoch)
                .filter(|session| !session.auto_title_asked)
                .and_then(Session::auto_title)
            {
                self.session_mut(epoch).auto_title_asked = true;
                self.rename_for(epoch, &title);
            }
            #[cfg(feature = "e2e-observe")]
            self.observe_control(&snapshot);
            self.start_adopted_turn_for(epoch);
            // A newly observed approval is announced from the snapshot itself,
            // whether or not its event reached the client first.
            let pending = self
                .live(&id)
                .and_then(|session| session.interaction.as_ref())
                .map(|interaction| interaction.interaction_id.clone());
            if pending.is_some() && pending != self.resident.get(epoch).unwrap().approval_seen {
                self.session_mut(epoch).approval_seen = pending;
                self.on_approval_requested(epoch);
            }
            self.recover_completions();
        }
    }

    /// Holds a snapshot whose session is not open yet.
    pub(super) fn hold_snapshot(&mut self, snapshot: SessionSnapshot, bytes: usize) {
        self.pre_install.hold(snapshot, bytes);
    }

    fn apply_to_session(&mut self, epoch: u64, snapshot: &SessionSnapshot) -> bool {
        let session = self.session_mut(epoch);
        if session.load != snapshot.load || !session.recovery.accept_revision(snapshot.revision) {
            return false;
        }
        session.apply_control(snapshot);
        session.recovery.discover(&snapshot.completions);
        session.replay_early();
        true
    }

    /// Asks for the current control snapshot; receipts get their queue and
    /// execution from here, never from their own answer.
    pub(super) fn refresh_control_for(&mut self, epoch: u64) {
        let Some(session) = self.resident.get_mut(epoch) else {
            return;
        };
        if session.control_in_flight {
            session.control_due = true;
            return;
        }
        session.control_due = false;
        session.control_in_flight = true;

        self.call_for(
            epoch,
            crate::protocol::METHOD_SESSION_STATE,
            serde_json::json!({}),
            super::SessionOp::State,
        );
    }

    pub(super) fn refresh_control(&mut self) {
        if let Some(epoch) = self.resident.current().map(|session| session.epoch) {
            self.refresh_control_for(epoch);
        }
    }

    /// Fetches the snapshot behind control hints, at most one read at a time.
    pub(super) fn pull_control_for(&mut self, epoch: u64) {
        if self
            .resident
            .get(epoch)
            .is_some_and(|session| session.control_due)
        {
            self.refresh_control_for(epoch);
        }
    }

    /// Starts waiting on an execution a control snapshot adopted, so its
    /// outcome arrives even though no receipt named it.
    pub(super) fn start_adopted_turn_for(&mut self, epoch: u64) {
        let Some(session) = self.resident.get_mut(epoch) else {
            return;
        };
        let Some(turn) = session.adopted.take() else {
            return;
        };
        let load = session.load.clone();
        self.spawn_for(
            epoch,
            super::tasks::wait_turn(self.client.clone(), turn, load),
        );
    }
}
