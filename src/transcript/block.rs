//! The block types of a conversation.

use std::time::Duration;

use super::ToolCall;
use crate::protocol::read::UserKind;
use crate::protocol::{LoopOutcome, Usage};

/// One rendered unit of the conversation.
#[derive(Clone, Debug)]
pub enum Block {
    User {
        text: String,
        kind: UserKind,
    },
    Assistant {
        key: RequestKey,
        reasoning: String,
        text: String,
        streaming: bool,
        /// The reader's thinking fold; `None` follows the global Ctrl+O.
        expanded: Option<bool>,
    },
    Tool(Box<ToolCall>),
    TurnEnd {
        loop_id: String,
        outcome: LoopOutcome,
        duration: Option<Duration>,
        requests: u32,
        tools: u64,
        usage: Usage,
        changes: Option<ChangeSummary>,
    },
    Compaction {
        /// The Agent's operation id, so a duplicate report is recognised.
        operation_id: String,
        before: Option<u64>,
        after: Option<u64>,
        /// Items covered and kept, each when reported; shown when there are no token figures.
        coverage: Option<(Option<usize>, Option<usize>)>,
        summary: Option<String>,
    },
    EarlierHistory {
        hidden_items: usize,
    },
}

/// Identity of one assistant response inside a turn.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RequestKey {
    pub loop_id: String,
    pub request_index: u32,
}

impl Block {
    pub fn is_streaming(&self) -> bool {
        matches!(
            self,
            Block::Assistant {
                streaming: true,
                ..
            }
        )
    }

    pub fn loop_id(&self) -> Option<&str> {
        match self {
            Block::Assistant { key, .. } => Some(&key.loop_id),
            Block::Tool(call) => Some(&call.key.loop_id),
            Block::TurnEnd { loop_id, .. } => Some(loop_id),
            _ => None,
        }
    }
}

/// What one turn changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChangeSummary {
    pub files: usize,
    /// Added and removed lines; `None` unless every file's diff was counted.
    pub lines: Option<(usize, usize)>,
}

impl ChangeSummary {
    pub fn is_empty(&self) -> bool {
        self.files == 0
    }
}
