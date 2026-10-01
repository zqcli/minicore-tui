//! Durable history/transcript blocks (spec r2). Blocks are projected from
//! authoritative `session.read` items; the live loop is never folded into them.
//! There are no synthetic terminal blocks: past history is rendered strictly
//! from durable items.

use std::sync::Arc;

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
    HistoryPlaceholder(HistoryPlaceholderBlock),
}

impl TranscriptBlock {
    /// The durable item index, when this block came from a
    /// `session.read` entry; `None` for locally created blocks.
    pub fn index(&self) -> Option<usize> {
        match self {
            Self::User(block) => block.index,
            Self::Assistant(block) => Some(block.index),
            Self::Tool(block) => block.index,
            Self::Summary(block) => Some(block.index),
            Self::HistoryPlaceholder(block) => Some(block.index),
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
    pub result: Option<Arc<str>>,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryPlaceholderBlock {
    pub index: usize,
    pub total_bytes: usize,
}

/// The accumulated durable history of one session.
#[derive(Debug, Default)]
pub struct TranscriptState {
    /// Durable transcript blocks. `Arc` so a layout request can snapshot the
    /// whole transcript with one refcount bump instead of copying every
    /// string; mutation is copy-on-write through [`Self::blocks_mut`].
    pub blocks: Arc<Vec<Arc<TranscriptBlock>>>,
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

    /// Copy-on-write access to the durable block list. A layout job that holds
    /// a snapshot `Arc` keeps its own view; the next mutation clones only the
    /// `Arc` vector, never the block text.
    pub fn blocks_mut(&mut self) -> &mut Vec<Arc<TranscriptBlock>> {
        Arc::make_mut(&mut self.blocks)
    }

    /// Installs an authoritative item in session-global order. Earlier window
    /// reads can arrive after the tail; their arrival order is not chronology.
    /// Pending local cards stay after indexed history, and a reread replaces
    /// the indexed owner rather than adding a duplicate.
    pub(crate) fn insert_history_owner(&mut self, owner: Arc<TranscriptBlock>) {
        let index = owner.index().expect("durable history item index");
        let blocks = self.blocks_mut();
        if blocks
            .last()
            .is_none_or(|block| block.index().is_some_and(|last| last < index))
        {
            blocks.push(owner);
            return;
        }
        blocks.retain(|block| block.index() != Some(index));
        let position = blocks
            .iter()
            .position(|block| block.index().is_none_or(|other| other > index))
            .unwrap_or(blocks.len());
        blocks.insert(position, owner);
    }

    /// Appends one block without copying any existing block text.
    pub fn push_block(&mut self, block: TranscriptBlock) {
        Arc::make_mut(&mut self.blocks).push(Arc::new(block));
    }

    /// Inserts one block without copying any existing block text.
    pub fn insert_block(&mut self, index: usize, block: TranscriptBlock) {
        Arc::make_mut(&mut self.blocks).insert(index, Arc::new(block));
    }

    /// Mutable access to one block; only that block's body is cloned when a
    /// snapshot still references it.
    pub fn block_mut(&mut self, index: usize) -> &mut TranscriptBlock {
        let blocks = Arc::make_mut(&mut self.blocks);
        Arc::make_mut(&mut blocks[index])
    }

    /// Clears blocks and invalidates prepared conversation metadata.
    pub fn clear_blocks(&mut self) {
        self.blocks_mut().clear();
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
