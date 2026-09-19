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
        let mut bytes = self.model.capacity();
        for part in &self.parts {
            bytes += match part {
                LivePart::Text(text) | LivePart::Reasoning(text) => text.capacity(),
                LivePart::Tool { tool_call_id } => tool_call_id.capacity(),
            };
        }
        for tool in &self.tools {
            bytes += tool.tool_call_id.capacity() + tool.name.capacity();
            bytes += tool.progress.as_ref().map_or(0, String::capacity);
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
            // Tool result/display Arcs are charged and bounded by ToolFacts,
            // their semantic owner. COW-trimming them here would duplicate
            // the shared body when a live view and the facts map both retain it.
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
        let mut bytes = self.user_text.capacity();
        bytes += self
            .requests
            .iter()
            .map(LiveRequest::retained_bytes)
            .sum::<usize>();
        bytes += self
            .pending_steers
            .iter()
            .map(|steer| {
                steer.text.capacity() + steer.accepted_at.as_ref().map_or(0, String::capacity)
            })
            .sum::<usize>();
        bytes += self.last_result.as_ref().map_or(0, turn_result_bytes);
        bytes
    }

    pub fn trim_to_bytes(&mut self, budget: usize) {
        let mut used = 0;
        trim_string(&mut self.user_text, budget, &mut used);
        for request in &mut self.requests {
            request.trim_to_bytes(budget, &mut used);
        }
        for steer in &mut self.pending_steers {
            trim_string(&mut steer.text, budget, &mut used);
            if let Some(accepted_at) = &mut steer.accepted_at {
                trim_string(accepted_at, budget, &mut used);
            }
        }
        if self
            .last_result
            .as_ref()
            .is_some_and(|result| !reserve_result_bytes(result, budget, &mut used))
        {
            // A TurnRef is an identity, not display text: truncating it would
            // manufacture a different result. The SessionView result remains
            // authoritative, so discard only this provisional duplicate.
            self.last_result = None;
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
    if text.capacity() > available {
        text.shrink_to_fit();
    }
    *used = (*used).saturating_add(text.capacity());
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
        self.user_text.capacity()
            + self
                .requests
                .iter()
                .map(LiveRequest::retained_bytes)
                .sum::<usize>()
            + self.result.as_ref().map_or(0, turn_result_bytes)
    }

    pub fn trim_to_bytes(&mut self, budget: usize) {
        let mut used = 0;
        trim_string(&mut self.user_text, budget, &mut used);
        for request in &mut self.requests {
            request.trim_to_bytes(budget, &mut used);
        }
        if self
            .result
            .as_ref()
            .is_some_and(|result| !reserve_result_bytes(result, budget, &mut used))
        {
            self.result = None;
        }
    }
}

fn turn_result_bytes(result: &TurnResultViewWire) -> usize {
    result.turn.session_id.capacity()
        + result.turn.loop_id.capacity()
        + result.accepted_at.as_ref().map_or(0, String::capacity)
        + result.completed_at.as_ref().map_or(0, String::capacity)
}

fn reserve_result_bytes(result: &TurnResultViewWire, budget: usize, used: &mut usize) -> bool {
    let bytes = turn_result_bytes(result);
    if bytes > budget.saturating_sub(*used) {
        return false;
    }
    *used += bytes;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_budget_uses_string_capacity_and_releases_trimmed_capacity() {
        let mut live = LiveLoop::new(LocalSubmissionId(1), "prompt".to_owned());
        let mut request = LiveRequest::new(0, 0, String::new(), Reasoning::Auto);
        let mut text = String::with_capacity(1024);
        text.push_str("reply");
        request.parts.push(LivePart::Text(text));
        live.requests.push(request);

        assert!(live.retained_bytes() >= 1024);
        live.trim_to_bytes(4);
        assert!(live.retained_bytes() <= 4);
    }

    #[test]
    fn live_budget_drops_unfit_result_metadata_without_truncating_its_identity() {
        let mut live = LiveLoop::new(LocalSubmissionId(1), String::new());
        live.last_result = Some(TurnResultViewWire {
            turn: TurnRef {
                session_id: "session".repeat(256),
                loop_id: "loop".repeat(256),
            },
            outcome: crate::protocol::LoopOutcomeWire::Completed,
            usage: None,
            requests: None,
            tool_rounds: None,
            final_config_revision: None,
            persistence: None,
            accepted_at: None,
            completed_at: None,
        });

        live.trim_to_bytes(4);

        assert!(live.last_result.is_none());
        assert!(live.retained_bytes() <= 4);
    }
}
