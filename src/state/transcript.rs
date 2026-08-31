//! Durable transcript blocks (spec 18). Blocks are built from
//! `session.transcript` pages; the live turn is never folded into them.

use serde_json::Value;

use crate::protocol::{TurnTerminalWire, UsageWire};
use crate::state::tool::ToolStatus;

/// One displayed transcript entry (spec 18.1). Tool call arguments are
/// never stored: assistant entries carry no arguments on the wire.
#[derive(Debug, Clone, PartialEq)]
pub enum TranscriptBlock {
    User(UserBlock),
    Assistant(AssistantBlock),
    Tool(ToolBlock),
    Summary(SummaryBlock),
    Terminal(TerminalBlock),
}

impl TranscriptBlock {
    /// The durable sequence number, when this block came from a
    /// `session.transcript` entry; `None` for locally created blocks.
    pub fn seq(&self) -> Option<u64> {
        match self {
            Self::User(block) => block.seq,
            Self::Assistant(block) => Some(block.seq),
            Self::Tool(_) => None,
            Self::Summary(block) => Some(block.seq),
            Self::Terminal(block) => Some(block.seq),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct UserBlock {
    pub seq: Option<u64>,
    pub turn_id: Option<String>,
    pub text: String,
    /// The local card shown before the turn's durable entry arrives.
    pub pending: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AssistantBlock {
    pub seq: u64,
    pub turn_id: String,
    pub model: String,
    pub parts: Vec<AssistantPart>,
    pub terminal_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AssistantPart {
    Text(String),
    Reasoning(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolBlock {
    pub tool_call_id: String,
    pub turn_id: String,
    pub name: String,
    pub arguments: Option<Value>,
    pub result: Option<String>,
    pub outcome: Option<String>,
    pub live_status: Option<ToolStatus>,
    pub progress: Option<String>,
    pub expanded: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SummaryBlock {
    pub seq: u64,
    pub through: u64,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TerminalBlock {
    pub seq: u64,
    pub turn_id: String,
    pub terminal: TurnTerminalWire,
    pub usage: UsageWire,
}

/// The accumulated durable transcript of one session (spec 12.8).
#[derive(Debug, Default)]
pub struct TranscriptState {
    pub blocks: Vec<TranscriptBlock>,
    /// Highest durable entry sequence actually merged into `blocks`; the
    /// start point of incremental fetches. Never advanced by the wire's
    /// `observed_head`, which may sit above the last merged entry.
    pub last_seq: Option<u64>,
    /// The next page's `after` cursor while pagination is incomplete.
    pub next_after: Option<u64>,
    /// The last fetched page reported `complete`.
    pub complete: bool,
    /// Per-block line caches for the render phase; empty until Phase 3.
    pub render_cache: TranscriptRenderCache,
}

/// Placeholder for render-time block caches; populated by the render phase.
#[derive(Debug, Default)]
pub struct TranscriptRenderCache {}
