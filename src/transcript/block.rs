//! Presentation blocks identified by native Host facts.

use std::time::Duration;

use minicore_host_protocol::{PersistenceState, Timestamp, TurnId, TurnOutcome};
use minicore_host_runtime::model::Usage;

use super::ToolCall;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UserKind {
    Prompt,
    Steering,
    FollowUp,
    /// A child Session's settled result, delivered to its delegating parent.
    Delegated,
}

#[derive(Clone, Debug)]
pub enum Block {
    User {
        text: String,
        kind: UserKind,
        at: Option<Timestamp>,
    },
    Assistant {
        key: RequestKey,
        reasoning: String,
        text: String,
        streaming: bool,
        partial: bool,
        expanded: Option<bool>,
    },
    Tool(Box<ToolCall>),
    TurnEnd {
        turn_id: TurnId,
        outcome: TurnOutcome,
        reason: Option<String>,
        duration: Option<Duration>,
        requests: u64,
        tool_rounds: u64,
        usage: Usage,
        changes: Option<ChangeSummary>,
        saving: Option<PersistenceState>,
    },
    Summary {
        text: String,
    },
    Compaction {
        source_position: u64,
        covered_items: u64,
        summary: String,
    },
    EarlierHistory {
        hidden_items: u64,
    },
}

/// Seed history has a native position but no fabricated execution identity.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum RequestKey {
    Response {
        turn_id: TurnId,
        response_index: u64,
    },
    History {
        position: u64,
    },
}

impl RequestKey {
    pub fn response(turn_id: TurnId, response_index: u64) -> Self {
        Self::Response {
            turn_id,
            response_index,
        }
    }

    pub fn turn_id(&self) -> Option<&TurnId> {
        match self {
            Self::Response { turn_id, .. } => Some(turn_id),
            Self::History { .. } => None,
        }
    }
}

impl Block {
    pub fn turn_id(&self) -> Option<&TurnId> {
        match self {
            Self::Assistant { key, .. } => key.turn_id(),
            Self::Tool(call) => call.key.turn_id(),
            Self::TurnEnd { turn_id, .. } => Some(turn_id),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChangeSummary {
    pub files: usize,
    pub lines: Option<(usize, usize)>,
}

impl ChangeSummary {
    pub fn is_empty(&self) -> bool {
        self.files == 0
    }
}
