//! The authoritative control snapshot: one sampling of a resident Session.

use serde::Deserialize;

use super::{CompactionProgress, PendingInteraction, Queue, QueueItem};

/// The completion discovery window of one load, oldest first.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct CompletionDirectory {
    /// The highest completion sequence published for this load; 0 before any.
    pub through_seq: u64,
    /// The sequence of the first retained entry, or `through_seq + 1` when empty.
    pub retained_from_seq: u64,
    /// The retained suffix, ascending, without gaps.
    pub entries: Vec<CompletedTurn>,
    /// Whether entries older than the window were dropped.
    pub truncated: bool,
}

/// What one turn did when it reached the Agent's completion boundary.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CompletionKind {
    Reported,
    InternalError,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CompletionOutcome {
    Completed,
    Cancelled,
    Failed,
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ResultAvailability {
    Live,
    Stored,
    Unavailable,
    /// A value this client does not know; the entry stays unsettled and the
    /// read itself decides what it is.
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum UnavailableReason {
    Expired,
    TooLarge,
    ResourceLimit,
    InternalError,
    NotRetained,
    #[serde(other)]
    Unknown,
}

/// One finished turn, as the directory describes it; never its body.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct CompletedTurn {
    pub seq: u64,
    pub loop_id: String,
    pub input_id: Option<String>,
    pub completion_kind: CompletionKind,
    pub outcome_kind: Option<CompletionOutcome>,
    pub persistence: Option<super::read::TurnPersistence>,
    #[serde(default)]
    pub persistence_uncertain: bool,
    pub availability: ResultAvailability,
    pub unavailable_reason: Option<UnavailableReason>,
    pub next_loop_id: Option<String>,
    /// The last committed-history turn before this one, for unsaved ordering.
    pub after_saved_loop_id: Option<String>,
}

/// What the Session is doing, with the identity of the input that started it.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionExecution {
    None,
    Starting {
        input: QueueItem,
    },
    Running {
        loop_id: String,
        input: Option<QueueItem>,
        request_index: u32,
        phase: ExecutionPhase,
    },
    Finishing {
        loop_id: String,
        input: Option<QueueItem>,
        request_index: u32,
    },
}

impl SessionExecution {
    pub fn loop_id(&self) -> Option<&str> {
        match self {
            Self::None => None,
            Self::Starting { .. } => None,
            Self::Running { loop_id, .. } | Self::Finishing { loop_id, .. } => Some(loop_id),
        }
    }

    pub fn input(&self) -> Option<&QueueItem> {
        match self {
            Self::None => None,
            Self::Starting { input } => Some(input),
            Self::Running { input, .. } | Self::Finishing { input, .. } => input.as_ref(),
        }
    }

    /// The phases Runtime reports while a turn runs.
    pub fn phase(&self) -> Option<ExecutionPhase> {
        match self {
            Self::Running { phase, .. } => Some(*phase),
            Self::Starting { .. } => Some(ExecutionPhase::Starting),
            Self::Finishing { .. } => None,
            Self::None => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionPhase {
    Unknown,
    Starting,
    RunningModel,
    RunningTools,
    WaitingForInput,
    Finishing,
    Finished,
}

impl ExecutionPhase {
    /// The Working line's text for this observation.
    pub fn label(self) -> &'static str {
        match self {
            Self::Unknown | Self::Starting => "Thinking",
            Self::RunningModel => "Waiting for model",
            Self::RunningTools => "Running tools",
            Self::WaitingForInput => "Waiting for approval",
            Self::Finishing | Self::Finished => "Finishing",
        }
    }
}

/// A waiting interaction with the turn and request that asked for it.
#[derive(Clone, Debug, Deserialize)]
pub struct SnapshotInteraction {
    pub loop_id: String,
    pub request_index: u32,
    #[serde(flatten)]
    pub interaction: PendingInteraction,
}

/// The single authoritative control state of one resident Session.
#[derive(Clone, Debug, Deserialize)]
pub struct SessionSnapshot {
    pub session_id: String,
    /// The resident instance; stable until the Session is closed and reopened.
    pub load: String,
    /// Monotonic within one `load`.
    pub revision: u64,
    pub execution: SessionExecution,
    pub pending_interaction: Option<SnapshotInteraction>,
    pub queue: Queue,
    #[serde(default)]
    pub compaction: Option<CompactionProgress>,
    pub block_reason: Option<BlockReason>,
    pub completions: CompletionDirectory,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum BlockReason {
    Persistence,
    Internal,
}

impl BlockReason {
    pub fn label(self) -> &'static str {
        match self {
            Self::Persistence => {
                "Persistence blocked: reopen the session to reconcile the last save"
            }
            Self::Internal => "Blocked: the Agent hit an internal error",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The whole frame the Agent sends for a control change, decoded the way
    /// the client's reader decodes it. A snapshot that only round-trips in
    /// isolation would hide an envelope or field-name mismatch.
    #[test]
    fn a_snapshot_event_decodes_from_its_wire_frame() {
        let line = json!({
            "jsonrpc": "2.0",
            "method": "agent.event",
            "params": {
                "type": "session_snapshot",
                "data": {
                    "meta": {"session_id": "ses_1", "load": "load_1", "dropped_before": 0},
                    "snapshot": {
                        "session_id": "ses_1", "load": "load_1", "revision": 7,
                        "execution": {"type": "running", "loop_id": "lop_1",
                                      "input": {"id": "inp_1", "mode": "steer", "text": "hi"},
                                      "request_index": 0, "phase": "running_tools"},
                        "pending_interaction": {
                            "loop_id": "lop_1", "request_index": 0,
                            "interaction_id": "int_1", "tool_call_id": "cal_1",
                            "tool_name": "bash",
                            "kind": {"type": "approval", "data": {"prompt": "Allow bash?"}}
                        },
                        "queue": {"items": [], "paused": false},
                        "block_reason": null,
                        "completions": {"through_seq": 0, "retained_from_seq": 1,
                                        "entries": [], "truncated": false}
                    }
                }
            }
        })
        .to_string();
        let frame = crate::protocol::parse_frame(line.as_bytes()).expect("a readable frame");
        let crate::protocol::IncomingFrame::Event(event) = frame else {
            panic!("a notification must decode as an event");
        };
        let crate::protocol::AgentEvent::SessionSnapshot { meta, snapshot } = event else {
            panic!("session_snapshot must decode as a snapshot event: {event:?}");
        };
        assert_eq!(meta.session_id, "ses_1");
        assert_eq!(snapshot.load, "load_1");
        assert_eq!(snapshot.revision, 7);
        assert_eq!(snapshot.execution.loop_id(), Some("lop_1"));
        let pending = snapshot.pending_interaction.expect("a pending interaction");
        assert_eq!(pending.interaction.interaction_id, "int_1");
        assert_eq!(pending.request_index, 0);
        assert_eq!(pending.interaction.tool_name, "bash");
        // Field names are the contract the Agent writes and the chain harness
        // asserts: dropping a required one must fail here first.
        for field in [
            "session_id",
            "load",
            "revision",
            "execution",
            "queue",
            "completions",
        ] {
            let mut payload = snapshot_payload();
            payload.as_object_mut().expect("an object").remove(field);
            assert!(
                serde_json::from_value::<SessionSnapshot>(payload).is_err(),
                "the snapshot wire shape no longer requires `{field}`"
            );
        }
        // An optional control field may be absent and still mean "nothing".
        for field in ["pending_interaction", "compaction", "block_reason"] {
            let mut payload = snapshot_payload();
            payload.as_object_mut().expect("an object").remove(field);
            assert!(
                serde_json::from_value::<SessionSnapshot>(payload).is_ok(),
                "the snapshot wire shape must accept a missing `{field}`"
            );
        }
    }

    #[test]
    fn a_full_legal_snapshot_frame_fits_the_control_ceiling() {
        let text = "\u{0}".repeat(262_144);
        let items: Vec<serde_json::Value> = (0..32)
            .map(|index| json!({"id": format!("inp_{index}"), "mode": "steer", "text": text}))
            .collect();
        let entries: Vec<serde_json::Value> = (0..256)
            .map(|seq| {
                json!({"seq": seq, "loop_id": format!("lup_{seq}"),
                       "input_id": format!("inp_{seq}"), "completion_kind": "reported",
                       "outcome_kind": "completed", "persistence": "persisted",
                       "availability": "stored"})
            })
            .collect();
        let mut frame = serde_json::to_vec(&json!({
            "jsonrpc": "2.0", "method": "agent.event",
            "params": {"type": "session_snapshot", "data": {
                "meta": {"session_id": "ses_1", "load": "load_1", "dropped_before": 0},
                "snapshot": {
                    "session_id": "ses_1", "load": "load_1", "revision": 9,
                    "execution": {"type": "running", "loop_id": "lup_1",
                                  "input": {"id": "inp_1", "mode": "steer", "text": text},
                                  "request_index": 0, "phase": "running_model"},
                    "pending_interaction": {
                        "loop_id": "lup_1", "request_index": 0,
                        "interaction_id": "int_1", "tool_call_id": "cal_1",
                        "tool_name": "bash",
                        "kind": {"type": "approval", "data": {"prompt": text}}},
                    "queue": {"items": items, "paused": false},
                    "compaction": null, "block_reason": null,
                    "completions": {"through_seq": 256, "retained_from_seq": 1,
                                    "entries": entries, "truncated": false}
                }
            }}
        }))
        .expect("a frame");
        frame.push(b'\n');
        assert!(
            frame.len() >= 52_237_130,
            "this fixture must be at least the measured legal frame, got {}",
            frame.len()
        );
        assert!(
            frame.len() <= crate::rpc::CONTROL_FRAME_MAX_BYTES,
            "a legal full snapshot frame must fit the control ceiling: {} > {}",
            frame.len(),
            crate::rpc::CONTROL_FRAME_MAX_BYTES
        );
        let decoded = crate::protocol::parse_frame(&frame[..frame.len() - 1])
            .expect("the full frame decodes");
        let crate::protocol::IncomingFrame::Event(crate::protocol::AgentEvent::SessionSnapshot {
            snapshot,
            ..
        }) = decoded
        else {
            panic!("a snapshot event");
        };
        assert_eq!(snapshot.queue.items.len(), 32);
        assert_eq!(snapshot.completions.entries.len(), 256);
    }

    fn snapshot_payload() -> serde_json::Value {
        json!({
            "session_id": "ses_1", "load": "load_1", "revision": 1,
            "execution": {"type": "none"},
            "pending_interaction": null,
            "queue": {"items": [], "paused": false},
            "compaction": null, "block_reason": null,
            "completions": {"through_seq": 0, "retained_from_seq": 1,
                            "entries": [], "truncated": false}
        })
    }

    #[test]
    fn a_snapshot_decodes_every_execution_shape() {
        let base = |execution: serde_json::Value| {
            json!({
                "session_id": "ses_1", "load": "load_1", "revision": 3,
                "execution": execution, "queue": {"items": [], "paused": false},
                "completions": {"through_seq": 0, "retained_from_seq": 1, "entries": [],
                                "truncated": false}
            })
        };
        let none: SessionSnapshot = serde_json::from_value(base(json!({"type": "none"}))).unwrap();
        assert_eq!(none.execution.loop_id(), None);
        assert_eq!(none.revision, 3);

        let item = json!({"id": "i1", "mode": "steer", "text": "hi"});
        let starting: SessionSnapshot = serde_json::from_value(base(json!({
            "type": "starting", "input": item
        })))
        .unwrap();
        assert_eq!(starting.execution.input().unwrap().id, "i1");

        let running: SessionSnapshot = serde_json::from_value(base(json!({
            "type": "running", "loop_id": "lup_1", "input": item, "request_index": 2,
            "phase": "running_tools"
        })))
        .unwrap();
        assert_eq!(running.execution.loop_id(), Some("lup_1"));
        assert_eq!(
            running.execution.phase().map(ExecutionPhase::label),
            Some("Running tools")
        );
        assert!(running.pending_interaction.is_none());

        let finishing: SessionSnapshot = serde_json::from_value(base(json!({
            "type": "finishing", "loop_id": "lup_1", "input": item, "request_index": 0
        })))
        .unwrap();
        assert_eq!(finishing.execution.phase(), None);
    }

    #[test]
    fn a_completed_turn_carries_its_saved_and_unsaved_anchors() {
        let turn: CompletedTurn = serde_json::from_value(json!({
            "seq": 7, "loop_id": "lup_a", "input_id": "inp_1",
            "completion_kind": "reported", "outcome_kind": "cancelled",
            "persistence": "failed", "availability": "live",
            "next_loop_id": "lup_b", "after_saved_loop_id": "lup_z"
        }))
        .unwrap();
        assert_eq!(turn.seq, 7);
        assert!(!turn.persistence_uncertain);
        assert_eq!(turn.availability, ResultAvailability::Live);
        assert_eq!(turn.unavailable_reason, None);

        let expired: CompletedTurn = serde_json::from_value(json!({
            "seq": 8, "loop_id": "lup_c", "completion_kind": "reported",
            "outcome_kind": "completed", "persistence": "failed",
            "persistence_uncertain": true, "availability": "unavailable",
            "unavailable_reason": "expired"
        }))
        .unwrap();
        assert!(expired.persistence_uncertain);
        assert_eq!(expired.unavailable_reason, Some(UnavailableReason::Expired));
    }

    #[test]
    fn a_future_completion_kind_or_reason_degrades_instead_of_failing() {
        let turn: CompletedTurn = serde_json::from_value(json!({
            "seq": 1, "loop_id": "lup_a", "completion_kind": "teleported",
            "availability": "elsewhere", "unavailable_reason": "quota"
        }))
        .unwrap();
        assert_eq!(turn.completion_kind, CompletionKind::Unknown);
        assert_eq!(turn.availability, ResultAvailability::Unknown);
        assert_eq!(turn.unavailable_reason, Some(UnavailableReason::Unknown));
    }

    #[test]
    fn a_pending_interaction_keeps_its_turn_identity_and_full_content() {
        let snapshot: SessionSnapshot = serde_json::from_value(json!({
            "session_id": "ses_1", "load": "load_1", "revision": 1,
            "execution": {"type": "none"},
            "pending_interaction": {
                "loop_id": "lup_1", "request_index": 2, "interaction_id": "int_1",
                "tool_call_id": "call_1", "tool_name": "shell",
                "kind": {"type": "approval", "data": {"prompt": "Run rm?", "risk": "high"}}
            },
            "queue": {"items": [], "paused": true},
            "block_reason": "persistence",
            "completions": {"through_seq": 0, "retained_from_seq": 1, "entries": [],
                            "truncated": false}
        }))
        .unwrap();
        let interaction = snapshot.pending_interaction.unwrap();
        assert_eq!(interaction.loop_id, "lup_1");
        assert_eq!(interaction.request_index, 2);
        assert_eq!(interaction.interaction.tool_name, "shell");
        assert!(snapshot.queue.paused);
        assert_eq!(snapshot.block_reason, Some(BlockReason::Persistence));
    }
}
