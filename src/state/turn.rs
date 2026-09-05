//! The live (provisional) view of one running loop (turn) and multi-request states (spec r2).

use crate::protocol::{Reasoning, TurnRef, TurnResultViewWire};
use crate::state::tool::LiveTool;

/// App-local id correlating a submitted turn with its send response; the
/// wire only carries `TurnRef`s.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LocalSubmissionId(pub u64);

/// One pending steering instruction queued or in flight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingSteer {
    pub local_id: u64,
    pub text: String,
    pub state: PendingSteerState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingSteerState {
    Sending,
    Queued,
    Persisted,
    NotRecorded,
    Unconfirmed,
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
    pub tools: Vec<LiveTool>,
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
