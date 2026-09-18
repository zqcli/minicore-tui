//! Durable history/transcript blocks (spec r2). Blocks are built from
//! `session.history` pages; the live loop is never folded into them.
//! There are no synthetic terminal blocks: past history is rendered strictly
//! from durable items.

use crate::protocol::{
    Reasoning, ToolCallViewWire, ToolOutcomeWire, UsageWire, UserMessageKindWire,
};
use crate::state::tool::ToolStatus;

/// One displayed transcript/history entry. Tool call arguments are
/// never stored: assistant entries carry no arguments on the wire.
#[derive(Debug, Clone, PartialEq)]
pub enum TranscriptBlock {
    User(UserBlock),
    Assistant(AssistantBlock),
    Tool(ToolBlock),
    Summary(SummaryBlock),
}

impl TranscriptBlock {
    /// The durable item index, when this block came from a
    /// `session.history` entry; `None` for locally created blocks.
    pub fn index(&self) -> Option<usize> {
        match self {
            Self::User(block) => block.index,
            Self::Assistant(block) => Some(block.index),
            Self::Tool(block) => block.index,
            Self::Summary(block) => Some(block.index),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct UserBlock {
    pub index: Option<usize>,
    pub loop_id: Option<String>,
    pub kind: UserMessageKindWire,
    pub text: String,
    /// The local card shown before the turn's durable entry arrives.
    pub pending: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AssistantBlock {
    pub index: usize,
    pub loop_id: String,
    pub request_index: u32,
    pub model: String,
    pub reasoning_level: Reasoning,
    pub parts: Vec<AssistantPart>,
    pub tool_calls: Vec<ToolCallViewWire>,
    pub usage: UsageWire,
    pub finish_reason: String,
    pub terminal_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AssistantPart {
    Text(String),
    Reasoning(String),
    ToolCall(ToolCallViewWire),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolBlock {
    pub index: Option<usize>,
    pub loop_id: String,
    pub request_index: u32,
    pub tool_call_id: String,
    pub name: String,
    pub result: Option<String>,
    pub outcome: Option<ToolOutcomeWire>,
    pub live_status: Option<ToolStatus>,
    pub progress: Option<String>,
    pub expanded: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SummaryBlock {
    pub index: usize,
    pub content: String,
}

/// The accumulated durable history of one session.
#[derive(Debug, Default)]
pub struct TranscriptState {
    pub blocks: Vec<TranscriptBlock>,
    /// The authoritative pinned window of decoded Runtime items. Render blocks
    /// may expand one item into several cards, so their length is unrelated.
    pub window: crate::app::history::HistoryWindow,
    /// The backend-provided cursor for the next read page, if the window is
    /// not yet complete (spec §6.3). Never computed from local item count.
    pub next_cursor: Option<crate::protocol::ReadCursor>,
    /// Number of contiguous durable items loaded so far. Mirrors
    /// `window.confirmed_prefix()` for the display bridge.
    pub loaded_count: usize,
    /// Total readable items in the pinned prefix (mirrors `window.total()`).
    pub total: usize,
    /// True when the whole prefix is loaded and no page remains.
    pub complete: bool,
    /// Durable content/display generation: history, folds and tool metadata
    /// invalidate it; live deltas and viewport-only events do not.
    pub render_revision: u64,
    pub render_cache: Option<std::sync::Arc<crate::state::view::PreparedDurable>>,
}

impl TranscriptState {
    /// Refreshes the display-bridge counters from the authoritative window.
    pub fn sync_from_window(&mut self) {
        self.loaded_count = self.window.confirmed_prefix();
        self.total = self.window.total();
        self.complete = self.next_cursor.is_none() && self.window.complete();
    }

    /// Increments the render generation used by prepared conversation
    /// snapshots.
    pub fn invalidate(&mut self) {
        self.render_revision = self.render_revision.wrapping_add(1);
        self.render_cache = None;
    }

    /// Clears blocks and invalidates prepared conversation metadata.
    pub fn clear_blocks(&mut self) {
        self.blocks.clear();
        self.window.reset();
        self.next_cursor = None;
        self.loaded_count = 0;
        self.total = 0;
        self.complete = false;
        self.invalidate();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolExpansion {
    Expanded,
    Collapsed,
}
