//! The live (provisional) view of one running loop (turn) and multi-request states (spec r2).

use std::sync::Arc;

use crate::protocol::{Reasoning, RequestId, TurnRef, TurnResultViewWire};
use crate::state::tool::LiveTool;

/// App-local id correlating a submitted turn with its send response; the
/// wire only carries `TurnRef`s.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LocalSubmissionId(pub u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationRef {
    pub session_id: String,
    pub operation_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submission {
    pub request_id: Option<RequestId>,
    pub local_id: LocalSubmissionId,
    pub session_epoch: u64,
    pub editor_revision: u64,
    pub text: Arc<str>,
    pub preparation: Option<OperationRef>,
    pub cancel_requested: bool,
}

/// One pending steering instruction queued or in flight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingSteer {
    pub local_id: u64,
    pub text: String,
    pub state: PendingSteerState,
    pub accepted_at: Option<String>,
    /// 1-based FIFO acceptance index from the Agent's steer ACK (absent on
    /// older Agents); receipts compare `applied_count` against it.
    pub steer_index: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingSteerState {
    Sending,
    Queued,
    Persisted,
    NotRecorded,
    Unconfirmed,
}

/// One locally admitted, not-yet-sent steering instruction in the per-session
/// FIFO queue. It lives OUTSIDE `LiveLoop` so a finished loop never drops it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteerQueueItem {
    pub local_id: u64,
    pub text: String,
    pub state: SteerQueueState,
    /// Composer revision at admission; the late-ACK guard compares it so new
    /// editor content is never cleared by an old steer response.
    pub editor_revision: Option<u64>,
    /// True while this queued message is being re-submitted as a fresh turn
    /// after its loop sealed (race fallback). The entry is kept until the
    /// turn.send ACK so a send failure cannot drop the text.
    pub handoff: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SteerQueueState {
    /// Admitted locally; not yet handed to any RPC.
    Unsent,
    /// The request reached the Agent but its response could not be decoded
    /// (outcome uncertain): never auto-resend; only deliberate withdrawal can
    /// move it back to an editable state.
    Unconfirmed,
}

/// A steering instruction proven applied because the Agent held it in a
/// prepared prompt history (receipt); rendered as a provisional Steering User
/// card until the durable history replaces it exactly once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedSteer {
    pub local_id: u64,
    pub text: String,
    pub accepted_at: Option<String>,
    pub request_index: u32,
}

/// Monotonic steering receipt observed for the CURRENT loop: the highest
/// `applied_count` seen and the request at which it was first observed. Used
/// to pair ACK `steer_index` values with real receipts (identity, never queue
/// position), and to recover dropped receipts from `session.presentation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SteerReceiptObserved {
    pub request_index: u32,
    pub applied_count: u64,
}

/// One model/tool iteration within a live loop.
#[derive(Debug, Clone, PartialEq)]
pub struct LiveRequest {
    pub request_index: u32,
    pub config_revision: u64,
    pub model: String,
    pub reasoning: Reasoning,
    pub text: String,
    pub reasoning_text: String,
    /// Arrival order of visible model parts. The flattened fields above are
    /// retained for compatibility and accounting, but rendering uses this
    /// sequence whenever it is populated.
    pub parts: Vec<LivePart>,
    pub tools: Vec<LiveTool>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LivePart {
    Text(String),
    Reasoning(String),
    Tool { tool_call_id: String },
}

impl LiveRequest {
    pub fn new(
        request_index: u32,
        config_revision: u64,
        model: String,
        reasoning: Reasoning,
    ) -> Self {
        Self {
            request_index,
            config_revision,
            model,
            reasoning,
            text: String::new(),
            reasoning_text: String::new(),
            parts: Vec::new(),
            tools: Vec::new(),
        }
    }
}

/// Everything known about a running turn (loop) before durable history confirms it.
#[derive(Debug, Clone, PartialEq)]
pub struct LiveLoop {
    /// The exact wire identity: session_id + loop_id.
    pub reference: Option<TurnRef>,
    pub local_submission: LocalSubmissionId,
    pub user_text: String,
    pub requests: Vec<LiveRequest>,
    pub pending_steers: Vec<PendingSteer>,
    /// True after the turn.wait response (or a wait failure) is observed;
    /// while false, the running composer may submit a steer.
    pub waiting: bool,
    /// Esc-style cancellation was requested.
    pub cancel_requested: bool,
    /// `dropped_before > 0` was observed while this turn was live.
    pub event_gap: bool,
    /// Final loop result if received while live.
    pub last_result: Option<TurnResultViewWire>,
}

impl LiveLoop {
    pub fn new(local_submission: LocalSubmissionId, user_text: String) -> Self {
        Self {
            reference: None,
            local_submission,
            user_text,
            requests: Vec::new(),
            pending_steers: Vec::new(),
            waiting: false,
            cancel_requested: false,
            event_gap: false,
            last_result: None,
        }
    }

    /// Finds or creates a request slot by `request_index`.
    pub fn ensure_request_mut(
        &mut self,
        request_index: u32,
        config_revision: u64,
        model: String,
        reasoning: Reasoning,
    ) -> &mut LiveRequest {
        if let Some(pos) = self
            .requests
            .iter()
            .position(|request| request.request_index == request_index)
        {
            &mut self.requests[pos]
        } else {
            let position = self
                .requests
                .iter()
                .position(|request| request.request_index > request_index)
                .unwrap_or(self.requests.len());
            self.requests.insert(
                position,
                LiveRequest::new(request_index, config_revision, model, reasoning),
            );
            &mut self.requests[position]
        }
    }
}

/// Preserved loop data when persistence fails or session is blocked.
#[derive(Debug, Clone, PartialEq)]
pub struct UnsavedLoop {
    pub turn: TurnRef,
    pub user_text: String,
    pub requests: Vec<LiveRequest>,
    pub result: Option<TurnResultViewWire>,
    pub event_gap: bool,
}
