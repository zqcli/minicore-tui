//! The live (provisional) view of one running loop (turn) and multi-request states (spec r2).

use std::sync::Arc;

use crate::protocol::{Reasoning, RequestId, TurnRef, TurnResultViewWire};
use crate::state::tool::LiveTool;

/// App-local id correlating a submitted turn with its send response; the
/// wire only carries `TurnRef`s.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
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
    /// Arrival order of visible model parts. This is the sole retained live
    /// output body; flattened text/reasoning mirrors are intentionally absent.
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
            parts: Vec::new(),
            tools: Vec::new(),
        }
    }

    pub fn visible_text(&self) -> String {
        self.parts
            .iter()
            .filter_map(|part| match part {
                LivePart::Text(text) => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    pub fn reasoning_text(&self) -> String {
        self.parts
            .iter()
            .filter_map(|part| match part {
                LivePart::Reasoning(text) => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    fn retained_bytes(&self) -> usize {
        let mut bytes = self.model.len();
        for part in &self.parts {
            bytes += match part {
                LivePart::Text(text) | LivePart::Reasoning(text) => text.len(),
                LivePart::Tool { tool_call_id } => tool_call_id.len(),
            };
        }
        for tool in &self.tools {
            bytes += tool.tool_call_id.len() + tool.name.len();
            bytes += tool.progress.as_ref().map_or(0, String::len);
            bytes += tool.result.as_ref().map_or(0, |result| result.len());
            if let Some(display) = &tool.display {
                bytes += display.detail.len();
                bytes += display.expanded_input.as_ref().map_or(0, String::len);
            }
        }
        bytes
    }

    fn trim_to_bytes(&mut self, budget: usize, used: &mut usize) {
        trim_string(&mut self.model, budget, used);
        for part in &mut self.parts {
            match part {
                LivePart::Text(text) | LivePart::Reasoning(text) => {
                    trim_string(text, budget, used);
                }
                LivePart::Tool { tool_call_id } => {
                    trim_string(tool_call_id, budget, used);
                }
            }
        }
        for tool in &mut self.tools {
            trim_string(&mut tool.tool_call_id, budget, used);
            trim_string(&mut tool.name, budget, used);
            if let Some(progress) = &mut tool.progress {
                trim_string(progress, budget, used);
            }
            if let Some(result) = &mut tool.result {
                let available = budget.saturating_sub(*used);
                if result.len() > available {
                    let mut end = available;
                    while end > 0 && !result.is_char_boundary(end) {
                        end -= 1;
                    }
                    *result = std::sync::Arc::<str>::from(&result[..end]);
                    tool.result_truncated = true;
                }
                *used = (*used).saturating_add(result.len());
            }
            if let Some(display) = &mut tool.display {
                let display = Arc::make_mut(display);
                trim_string(&mut display.detail, budget, used);
                if let Some(input) = &mut display.expanded_input {
                    trim_string(input, budget, used);
                }
            }
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

    pub fn retained_bytes(&self) -> usize {
        let mut bytes = self.user_text.len();
        bytes += self
            .requests
            .iter()
            .map(LiveRequest::retained_bytes)
            .sum::<usize>();
        bytes
    }

    pub fn trim_to_bytes(&mut self, budget: usize) {
        let mut used = 0;
        trim_string(&mut self.user_text, budget, &mut used);
        for request in &mut self.requests {
            request.trim_to_bytes(budget, &mut used);
        }
    }
}

fn trim_string(text: &mut String, budget: usize, used: &mut usize) {
    let available = budget.saturating_sub(*used);
    if text.len() > available {
        let mut end = available;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    *used = (*used).saturating_add(text.len());
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

impl UnsavedLoop {
    pub fn retained_bytes(&self) -> usize {
        self.user_text.len()
            + self
                .requests
                .iter()
                .map(LiveRequest::retained_bytes)
                .sum::<usize>()
    }

    pub fn trim_to_bytes(&mut self, budget: usize) {
        let mut used = 0;
        trim_string(&mut self.user_text, budget, &mut used);
        for request in &mut self.requests {
            request.trim_to_bytes(budget, &mut used);
        }
    }
}
