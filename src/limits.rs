//! Central capacity budgets (spec §21).
//!
//! Every bound lives here so the owners charge the same number: the history
//! window evicts decoded bodies, the layout cache will evict prepared
//! sections, and the live loop stops accepting deltas. A budget that is only
//! mentioned in a comment is not a budget; the values below are the ones the
//! accounting code uses.

/// The fixed Agent's whole-file read bound; no unbounded preview accumulation.
pub const FILE_PREVIEW_BYTES: usize = 512 * 1024;
pub const WORKSPACE_CANDIDATE_BYTES: usize = 1024 * 1024;
pub const WORKSPACE_CANDIDATES: usize = 500;

/// Decoded history bodies retained across all sessions.
pub const HISTORY_BODY_BYTES: usize = 32 * 1024 * 1024;
/// Prepared conversation sections retained in the layout cache.
pub const LAYOUT_CACHE_BYTES: usize = 48 * 1024 * 1024;
/// Maximum source handed to one Markdown/layout section. Larger durable
/// bodies remain readable through the history owner but render as a bounded
/// explicit placeholder instead of creating an oversized row vector.
pub const LAYOUT_SECTION_BYTES: usize = 4 * 1024 * 1024;
/// Retained live output for one loop.
pub const LIVE_LOOP_BYTES: usize = 4 * 1024 * 1024;
/// Retained live output for every loop of one session.
pub const LIVE_TOTAL_BYTES: usize = 16 * 1024 * 1024;
/// Retained presentation output for one tool stream.
pub const TOOL_STREAM_BYTES: usize = 1024 * 1024;
/// Raw stream bytes requested per detail page.
pub const TOOL_PAGE_BYTES: usize = 16 * 1024;
/// Metadata is bounded independently of payload size; tiny process events
/// coalesce into pages instead of allocating one retained Arc per byte.
pub const TOOL_STREAM_CHUNKS: usize = 128;
/// Retained presentation output for all tools in one session.
pub const TOOL_TOTAL_BYTES: usize = 16 * 1024 * 1024;
/// One Composer draft.
pub const COMPOSER_DRAFT_BYTES: usize = 256 * 1024;
/// Every Composer draft held by the app.
pub const COMPOSER_ALL_DRAFTS_BYTES: usize = 8 * 1024 * 1024;
/// Maximum UTF-8 bytes accepted when an external editor returns a draft.
pub const EDITOR_READ_BYTES: usize = 256 * 1024;
/// Hard cumulative admission ceiling for editor readback work.
pub const EDITOR_TOTAL_BYTES: usize = 8 * 1024 * 1024;
/// Items protected near the active viewport before history eviction may
/// remove the oldest confirmed items. This protects the viewport and its
/// recent neighbourhood, not the whole active session.
pub const HISTORY_PROTECT_TAIL_ITEMS: usize = 64;
/// Background sessions protect no tail: only the active viewport and its
/// recent neighbourhood are pinned, so an inactive session releases its
/// bodies instead of holding the global budget.
pub const HISTORY_PROTECT_TAIL_ITEMS_BACKGROUND: usize = 0;
/// Evictions one UI pass may perform, so a very large window cannot stall
/// `App::update`; the next pass continues where this one stopped.
pub const HISTORY_EVICTIONS_PER_PASS: usize = 4096;

#[cfg(test)]
mod tests {
    use super::*;

    /// The budgets are the spec targets, spelled in one place. A drift here
    /// is a product decision, not an implementation detail.
    #[test]
    fn budgets_match_the_spec_targets() {
        assert_eq!(HISTORY_BODY_BYTES, 32 * 1024 * 1024);
        assert_eq!(LAYOUT_CACHE_BYTES, 48 * 1024 * 1024);
        assert_eq!(LAYOUT_SECTION_BYTES, 4 * 1024 * 1024);
        assert_eq!(LIVE_LOOP_BYTES, 4 * 1024 * 1024);
        assert_eq!(LIVE_TOTAL_BYTES, 16 * 1024 * 1024);
        assert_eq!(TOOL_STREAM_BYTES, 1024 * 1024);
        assert_eq!(TOOL_TOTAL_BYTES, 16 * 1024 * 1024);
        assert_eq!(COMPOSER_DRAFT_BYTES, 256 * 1024);
        assert_eq!(COMPOSER_ALL_DRAFTS_BYTES, 8 * 1024 * 1024);
        assert_eq!(EDITOR_READ_BYTES, 256 * 1024);
        assert_eq!(EDITOR_TOTAL_BYTES, 8 * 1024 * 1024);
        assert_eq!(HISTORY_PROTECT_TAIL_ITEMS_BACKGROUND, 0);
    }
}
