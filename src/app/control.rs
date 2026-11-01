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
        if let Some(live) = self.live.take() {
            self.ended_live.push_back(live);
            if self.ended_live.len() > 256 {
                self.ended_live.pop_front();
            }
        }
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
            if event.meta().is_some_and(|meta| meta.dropped_before > 0)
                && let Some(live) = self
                    .live
                    .as_mut()
                    .filter(|live| live.turn.loop_id == loop_id)
            {
                live.gap = true;
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
