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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{
        CompletionDirectory, Queue, QueueItem, ResultAvailability, SessionSnapshot,
    };
    use serde_json::json;

    fn session() -> Session {
        Session::new(
            serde_json::from_value(crate::testing::session_info("s")).unwrap(),
            1,
        )
    }

    fn snapshot(execution: serde_json::Value, queue: serde_json::Value) -> SessionSnapshot {
        serde_json::from_value(json!({
            "session_id": "s", "load": "load_1", "revision": 1,
            "execution": execution, "queue": queue,
            "completions": {"through_seq": 0, "retained_from_seq": 1, "entries": [],
                            "truncated": false}
        }))
        .unwrap()
    }

    fn item(id: &str, text: &str) -> serde_json::Value {
        json!({"id": id, "mode": "steer", "text": text})
    }

    #[test]
    fn a_running_execution_opens_the_turn_region_with_its_own_input() {
        let mut session = session();
        let snapshot = snapshot(
            json!({"type": "running", "loop_id": "l1", "input": item("i1", "first"),
                   "request_index": 0, "phase": "running_model"}),
            json!({"items": [], "paused": false}),
        );
        session.apply_control(&snapshot);
        let live = session.live.as_ref().unwrap();
        assert_eq!(live.turn.loop_id, "l1");
        assert_eq!(live.phase, Phase::Waiting);
        assert!(matches!(
            session.transcript.blocks().first(),
            Some(Block::User { text, kind: UserKind::Prompt, .. }) if text == "first"
        ));
        assert_eq!(session.control_status(), LiveStatus::Running);
    }

    #[test]
    fn a_claimed_input_is_busy_without_creating_a_speculative_loop_or_queue_entry() {
        let mut session = session();
        session.apply_control(&snapshot(
            json!({"type":"starting","input":item("claimed","older input")}),
            json!({"items":[],"paused":false}),
        ));
        assert!(session.is_busy());
        assert!(session.live.is_none());
        assert_eq!(session.status_label(), Some("Thinking"));
        assert!(session.queue.items.is_empty());
        assert!(session.transcript.is_empty());
    }

    #[test]
    fn a_finishing_execution_is_not_a_running_phase_and_keeps_the_live_turn() {
        let mut session = session();
        session.apply_control(&snapshot(
            json!({"type": "running", "loop_id": "l1", "input": item("i1", "first"),
                   "request_index": 1, "phase": "waiting_for_input"}),
            json!({"items": [], "paused": false}),
        ));
        session.apply_control(&snapshot(
            json!({"type": "finishing", "loop_id": "l1", "input": item("i1", "first"),
                   "request_index": 1}),
            json!({"items": [], "paused": false}),
        ));
        assert_eq!(
            session.live.as_ref().unwrap().turn.loop_id,
            "l1",
            "a completion still reported by the directory ends it"
        );
        assert_eq!(session.control_status(), LiveStatus::Running);
    }

    #[test]
    fn a_new_loop_replaces_the_observed_execution_identity() {
        let mut session = session();
        session.apply_control(&snapshot(
            json!({"type": "running", "loop_id": "l1", "input": item("i1", "first"),
                   "request_index": 0, "phase": "running_model"}),
            json!({"items": [], "paused": false}),
        ));
        // A delayed wait does not hold the current execution back.
        session.apply_control(&snapshot(
            json!({"type": "running", "loop_id": "l2", "input": item("i2", "second"),
                   "request_index": 0, "phase": "running_tools"}),
            json!({"items": [], "paused": false}),
        ));
        assert_eq!(session.live.as_ref().unwrap().turn.loop_id, "l2");
        assert_eq!(session.observed_loop(), Some("l2"));
    }

    #[test]
    fn a_queue_and_pause_come_only_from_the_snapshot() {
        let mut session = session();
        session.queue = Queue {
            items: vec![QueueItem {
                id: "old".into(),
                mode: crate::protocol::InputMode::Steer,
                text: "old".into(),
                accepted_at: None,
            }],
            paused: false,
        };
        session.apply_control(&snapshot(
            json!({"type": "none"}),
            json!({"items": [item("i2", "queued")], "paused": true}),
        ));
        let ids: Vec<&str> = session
            .queue
            .items
            .iter()
            .map(|item| item.id.as_str())
            .collect();
        assert_eq!(ids, ["i2"]);
        assert!(session.queue.paused);
        assert_eq!(session.control_status(), LiveStatus::Idle);
    }

    #[test]
    fn a_snapshot_interaction_is_operable_without_a_tool_card() {
        let mut session = session();
        let snapshot: SessionSnapshot = serde_json::from_value(json!({
            "session_id": "s", "load": "load_1", "revision": 1,
            "execution": {"type": "running", "loop_id": "l1",
                          "input": item("i1", "go"), "request_index": 2,
                          "phase": "waiting_for_input"},
            "pending_interaction": {
                "loop_id": "l1", "request_index": 2, "interaction_id": "int_1",
                "tool_call_id": "call_1", "tool_name": "shell",
                "kind": {"type": "tool_input", "data": {
                    "prompt": "Which branch?", "answer_kind": "single_choice",
                    "choices": [{"index": 0, "text": "main"}, {"index": 1, "text": "next"}]
                }}
            },
            "queue": {"items": [], "paused": false},
            "completions": {"through_seq": 0, "retained_from_seq": 1, "entries": [],
                            "truncated": false}
        }))
        .unwrap();
        session.apply_control(&snapshot);
        let interaction = session.interaction.as_ref().unwrap();
        assert_eq!(interaction.turn.loop_id, "l1");
        assert_eq!(interaction.prompt, "Which branch?");
        assert_eq!(interaction.options(), ["main", "next"]);
        assert_eq!(session.control_status(), LiveStatus::Approval);
        // A later snapshot without the interaction resolves it.
        session.apply_control(&snapshot_json_without_interaction());
        assert!(session.interaction.is_none());
    }

    fn snapshot_json_without_interaction() -> SessionSnapshot {
        serde_json::from_value(json!({
            "session_id": "s", "load": "load_1", "revision": 2,
            "execution": {"type": "running", "loop_id": "l1",
                          "input": item("i1", "go"), "request_index": 2,
                          "phase": "running_tools"},
            "queue": {"items": [], "paused": false},
            "completions": {"through_seq": 0, "retained_from_seq": 1, "entries": [],
                            "truncated": false}
        }))
        .unwrap()
    }

    #[test]
    fn a_block_reason_is_visible_without_hiding_an_interaction() {
        let mut session = session();
        let snapshot: SessionSnapshot = serde_json::from_value(json!({
            "session_id": "s", "load": "load_1", "revision": 1,
            "execution": {"type": "none"},
            "pending_interaction": {
                "loop_id": "l1", "request_index": 0, "interaction_id": "int_1",
                "tool_call_id": "call_1", "tool_name": "shell",
                "kind": {"type": "approval", "data": {"prompt": "Run?", "risk": "low"}}
            },
            "queue": {"items": [], "paused": true},
            "block_reason": "persistence",
            "completions": {"through_seq": 1, "retained_from_seq": 1, "entries": [{
                "seq": 1, "loop_id": "l0", "completion_kind": "reported",
                "outcome_kind": "completed", "persistence": "failed",
                "availability": "unavailable", "unavailable_reason": "not_retained"
            }], "truncated": false}
        }))
        .unwrap();
        session.apply_control(&snapshot);
        assert_eq!(
            session.block_label(),
            Some("Persistence blocked: reopen the session to reconcile the last save")
        );
        assert!(session.interaction.is_some(), "the prompt stays answerable");
        let directory: &CompletionDirectory = &snapshot.completions;
        assert_eq!(
            directory.entries[0].availability,
            ResultAvailability::Unavailable
        );
        let _ = TurnRef::new("s", "l1");
    }
}
