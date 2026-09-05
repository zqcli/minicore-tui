//! Durable history/transcript blocks (spec r2). Blocks are built from
//! `session.history` pages; the live loop is never folded into them.
//! There are no synthetic terminal blocks: past history is rendered strictly
//! from durable items.

use ratatui::text::Line;

use crate::protocol::{
    Reasoning, ToolCallViewWire, ToolOutcomeWire, UsageWire, UserMessageKindWire,
};
use crate::state::tool::ToolStatus;
use crate::theme::ThemeKind;

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
    /// Raw indexed history items are the pagination authority. Render blocks
    /// may expand one item into several cards, so their length is unrelated.
    pub items: Vec<crate::protocol::IndexedHistoryItemWire>,
    /// Number of contiguous durable items loaded so far. Used as `offset` in pagination.
    pub loaded_count: usize,
    /// The next page's `offset` cursor while pagination is incomplete.
    pub next_offset: Option<usize>,
    /// Total items count reported by the last `session.history` page.
    pub total: usize,
    /// The last fetched page reported `complete` or loaded all items.
    pub complete: bool,
    /// Monotonic generation of the durable blocks used by render preparation.
    pub render_revision: u64,
    /// Read-only durable line cache; only `App::update` installs or clears it.
    pub render_cache: TranscriptRenderCache,
}

impl TranscriptState {
    /// Builds the complete identity of a durable render preparation.
    pub fn cache_key(
        &self,
        width: u16,
        theme: ThemeKind,
        reasoning_visible: bool,
        tools_expanded: bool,
    ) -> TranscriptCacheKey {
        TranscriptCacheKey {
            revision: self.render_revision,
            width,
            theme,
            reasoning_visible,
            tools_expanded,
        }
    }

    /// Increments the render generation, invalidating any prepared line cache.
    pub fn invalidate(&mut self) {
        self.render_revision = self.render_revision.wrapping_add(1);
        self.render_cache.clear();
    }

    /// Clears blocks and invalidates cache.
    pub fn clear_blocks(&mut self) {
        self.blocks.clear();
        self.items.clear();
        self.loaded_count = 0;
        self.next_offset = None;
        self.total = 0;
        self.complete = false;
        self.invalidate();
    }
}

/// The identity of one prepared durable transcript line set.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TranscriptCacheKey {
    pub revision: u64,
    pub width: u16,
    pub theme: ThemeKind,
    pub reasoning_visible: bool,
    pub tools_expanded: bool,
}

/// A read-only result produced by `ui::transcript::prepare_cache`. It is
/// installed exclusively by `App::update`.
#[derive(Debug, Default)]
pub struct PreparedTranscriptCache {
    pub session_id: String,
    pub key: Option<TranscriptCacheKey>,
    pub lines: Vec<Line<'static>>,
}

/// The installed durable transcript render cache.
#[derive(Debug, Default)]
pub struct TranscriptRenderCache {
    entry: Option<PreparedTranscriptCache>,
}

impl TranscriptRenderCache {
    pub fn install(&mut self, prepared: PreparedTranscriptCache) {
        self.entry = Some(prepared);
    }

    pub fn clear(&mut self) {
        self.entry = None;
    }

    pub fn matches(&self, key: &TranscriptCacheKey) -> bool {
        self.entry
            .as_ref()
            .and_then(|entry| entry.key.as_ref())
            .map(|installed| installed == key)
            .unwrap_or(false)
    }

    pub fn lines(&self, key: &TranscriptCacheKey) -> Option<&[Line<'static>]> {
        if self.matches(key) {
            self.entry.as_ref().map(|entry| entry.lines.as_slice())
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolExpansion {
    Expanded,
    Collapsed,
}
