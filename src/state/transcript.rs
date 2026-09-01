//! Durable transcript blocks (spec 18). Blocks are built from
//! `session.transcript` pages; the live turn is never folded into them.

use ratatui::text::Line;
use serde_json::Value;

use crate::protocol::{TurnTerminalWire, UsageWire};
use crate::state::tool::ToolStatus;
use crate::theme::ThemeKind;

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
            tool_expansions: tool_expansions(&self.blocks),
        }
    }

    /// Invalidates every prepared durable line set after a block mutation.
    pub(crate) fn invalidate(&mut self) {
        self.render_revision = self.render_revision.saturating_add(1);
        self.render_cache.clear();
    }

    /// Clears durable blocks and invalidates their prepared rendering.
    pub(crate) fn clear_blocks(&mut self) {
        if !self.blocks.is_empty() {
            self.blocks.clear();
            self.invalidate();
        } else {
            self.render_cache.clear();
        }
    }
}

/// The identity of one prepared durable transcript line set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TranscriptCacheKey {
    pub revision: u64,
    pub width: u16,
    pub theme: ThemeKind,
    pub reasoning_visible: bool,
    pub tools_expanded: bool,
    /// Individual tool expansion is part of the key even though block
    /// mutations also advance `revision`; this makes the rendering contract
    /// explicit and prevents a stale per-tool view from being reused.
    pub tool_expansions: Vec<ToolExpansion>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolExpansion {
    pub turn_id: String,
    pub tool_call_id: String,
    pub expanded: bool,
}

/// A read-only result produced by `ui::transcript::prepare_cache`. It is
/// installed only after `App::update` verifies the active session and key.
#[derive(Clone, Debug)]
pub struct PreparedTranscriptCache {
    pub session_id: String,
    pub key: TranscriptCacheKey,
    pub lines: Vec<Line<'static>>,
}

/// Prepared durable lines for one session. Header, notices and live streaming
/// content are intentionally not stored here.
#[derive(Debug, Default)]
pub struct TranscriptRenderCache {
    prepared: Option<PreparedTranscriptCache>,
}

impl TranscriptRenderCache {
    pub(crate) fn clear(&mut self) {
        self.prepared = None;
    }

    pub fn matches(&self, key: &TranscriptCacheKey) -> bool {
        self.prepared
            .as_ref()
            .is_some_and(|prepared| prepared.key == *key)
    }

    pub fn lines(&self, key: &TranscriptCacheKey) -> Option<&[Line<'static>]> {
        self.prepared
            .as_ref()
            .filter(|prepared| prepared.key == *key)
            .map(|prepared| prepared.lines.as_slice())
    }

    pub(crate) fn install(&mut self, prepared: PreparedTranscriptCache) {
        self.prepared = Some(prepared);
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.prepared.is_none()
    }
}

/// Extracts the individual expansion state used by the cache key.
pub fn tool_expansions(blocks: &[TranscriptBlock]) -> Vec<ToolExpansion> {
    blocks
        .iter()
        .filter_map(|block| match block {
            TranscriptBlock::Tool(tool) => Some(ToolExpansion {
                turn_id: tool.turn_id.clone(),
                tool_call_id: tool.tool_call_id.clone(),
                expanded: tool.expanded,
            }),
            _ => None,
        })
        .collect()
}
