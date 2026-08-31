//! The live (provisional) view of one submitted turn (spec 12.6).

use crate::protocol::TurnRef;
use crate::state::tool::LiveTool;

/// App-local id correlating a submitted turn with its send response; the
/// wire only carries `TurnRef`s.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LocalSubmissionId(pub u64);

/// Everything known about a running turn before its durable transcript
/// exists. Never authoritative: the reconciled `TranscriptBlock`s replace it.
#[derive(Debug, Clone, PartialEq)]
pub struct LiveTurn {
    /// The exact wire identity, filled by `turn_started` or by the
    /// `turn.send` response. All further turn-scoped events must match it
    /// exactly (session + instance + turn).
    pub reference: Option<TurnRef>,
    pub local_submission: LocalSubmissionId,
    pub user_text: String,
    pub text: String,
    pub reasoning: String,
    pub tools: Vec<LiveTool>,
    /// True once the turn.wait response arrived and the durable
    /// reconciliation is running.
    pub waiting: bool,
    /// Esc-style cancellation was requested; the cancel goes out with the
    /// exact reference as soon as one is known.
    pub cancel_requested: bool,
    /// `dropped_before > 0` was observed while this turn was live.
    pub event_gap: bool,
}
