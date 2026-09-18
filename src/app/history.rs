//! The authoritative Protocol v1 history window and its paging state
//! (spec §5.3, §6). This module owns the `session.read` pin/assembler chain so
//! it does not keep growing `app.rs`.
//!
//! It deliberately does **not** know about the display model. It returns
//! decoded Runtime items; `app.rs` projects them into the short-term
//! `TranscriptBlock` bridge, which stage C replaces with shared sections.

use std::collections::{BTreeMap, HashSet};
use std::ops::Range;
use std::sync::Arc;

use crate::protocol::read::{
    Assembled, ChunkAssembler, RawHistoryItem, ReadCursor, ReadError, RuntimeItem, SnapshotPin,
};

/// Why the read chain cannot continue with the pin it holds.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PinError {
    #[error("history revision changed: the pinned prefix is no longer valid")]
    RevisionChanged,
    #[error("history total {found} is below the previously loaded {known}")]
    TotalRegressed { known: usize, found: usize },
    #[error("history page did not advance from cursor item {item}")]
    CursorStalled { item: usize },
}

/// A stable, byte-bounded window over one pinned history prefix. `items` is a
/// sparse map because the default open for a long session starts at the tail
/// and earlier ranges are loaded on demand (spec §6.3, §6.5).
#[derive(Debug, Clone, Default)]
pub struct HistoryWindow {
    pin: Option<SnapshotPin>,
    items: BTreeMap<usize, Arc<RawHistoryItem>>,
    loaded_ranges: Vec<Range<usize>>,
    bytes: usize,
    pub trailing_incomplete: bool,
    pub records_truncated: bool,
}

impl HistoryWindow {
    pub fn pin(&self) -> Option<&SnapshotPin> {
        self.pin.as_ref()
    }

    pub fn total(&self) -> usize {
        self.pin.as_ref().map_or(0, |pin| pin.total)
    }

    pub fn item(&self, index: usize) -> Option<&Arc<RawHistoryItem>> {
        self.items.get(&index)
    }

    pub fn items(&self) -> impl Iterator<Item = (&usize, &Arc<RawHistoryItem>)> {
        self.items.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn loaded_ranges(&self) -> &[Range<usize>] {
        &self.loaded_ranges
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Highest contiguous loaded prefix length, used as the incremental read
    /// start when a new turn is persisted (spec §6.4).
    pub fn confirmed_prefix(&self) -> usize {
        self.loaded_ranges
            .iter()
            .find(|range| range.start == 0)
            .map_or(0, |range| range.end)
    }

    /// True when every item in `[0, total)` is present. An empty session
    /// (total 0) is trivially complete.
    pub fn complete(&self) -> bool {
        let total = self.total();
        total == 0 || self.confirmed_prefix() >= total
    }

    pub fn has_items(&self) -> bool {
        !self.items.is_empty()
    }

    pub fn reset(&mut self) {
        self.pin = None;
        self.items.clear();
        self.loaded_ranges.clear();
        self.bytes = 0;
        self.trailing_incomplete = false;
        self.records_truncated = false;
    }

    /// Installs a pin without dropping already-loaded items. The caller decides
    /// whether the view is replaced.
    pub fn install_pin(&mut self, pin: SnapshotPin) {
        self.pin = Some(pin);
    }

    /// Replaces the window wholesale.
    pub fn replace_pin(&mut self, pin: SnapshotPin) {
        self.reset();
        self.pin = Some(pin);
    }

    /// Inserts one decoded item, returning its `Arc` for the display bridge.
    pub fn insert(&mut self, index: usize, item: RawHistoryItem) -> Arc<RawHistoryItem> {
        let item = Arc::new(item);
        self.bytes += item_bytes(&item);
        self.items.insert(index, item.clone());
        self.merge_range(index);
        item
    }

    fn merge_range(&mut self, index: usize) {
        let mut start = index;
        let mut end = index + 1;
        let mut merged: Vec<Range<usize>> = Vec::new();
        for range in self.loaded_ranges.drain(..) {
            if range.end < start || range.start > end {
                merged.push(range);
            } else {
                start = start.min(range.start);
                end = end.max(range.end);
            }
        }
        merged.push(start..end);
        merged.sort_by_key(|range| range.start);
        self.loaded_ranges = merged;
    }
}

fn item_bytes(item: &RawHistoryItem) -> usize {
    // Charge the visible text once; this is a budget estimate, not RSS
    // (spec §11.6).
    match &item.item {
        RuntimeItem::User(user) => user.input.text.len(),
        RuntimeItem::Assistant(assistant) => assistant
            .content
            .iter()
            .map(|part| part.visible_bytes())
            .sum(),
        RuntimeItem::ToolResult(result) => result.output.content.len(),
        RuntimeItem::Summary(summary) => summary.content.len(),
    }
}

/// One in-flight `session.read` page. It owns the chunk assembler so a page
/// that lands outside its requested window can discard a partial item instead
/// of mistaking it for loaded content (spec §6.3).
#[derive(Debug)]
pub struct ReadPage {
    pub cursor: ReadCursor,
    pub want_pin: Option<SnapshotPin>,
    pub assembler: ChunkAssembler,
    /// The lowest index this page may contribute to the window. A reused
    /// first-page chunk below the window start is dropped, not faked.
    pub window_start: usize,
    /// Whether this page replaces the view rather than appending.
    pub replacement: bool,
    /// Whether this page is a stale-check or gap reconcile.
    pub reconcile: bool,
}

impl ReadPage {
    pub fn new(cursor: ReadCursor, want_pin: Option<SnapshotPin>, window_start: usize) -> Self {
        Self {
            cursor,
            want_pin,
            assembler: ChunkAssembler::new(),
            window_start,
            replacement: false,
            reconcile: false,
        }
    }
}

/// What one applied page contributed.
#[derive(Debug)]
pub struct AppliedPage {
    /// Newly decoded items in page order.
    pub inserted: Vec<(usize, Arc<RawHistoryItem>)>,
    /// The backend-provided cursor for the next page, if any.
    pub next: Option<ReadCursor>,
    /// A `ReadChunk` that was refused (protocol error), with its index.
    pub error: Option<ReadError>,
}

/// The result of validating and feeding one page.
#[derive(Debug)]
pub enum ReadApply {
    /// The page's pin disagrees with the window's; the view is stale.
    Stale(PinError),
    /// The page decoded (or partially decoded) cleanly.
    Ok(AppliedPage),
}

/// Validates one page against the pinned window and feeds its chunks through
/// the assembler. Items below `page.window_start` are dropped. The caller owns
/// the display projection.
pub fn apply_page(
    window: &mut HistoryWindow,
    page: &mut ReadPage,
    result: &crate::protocol::ReadSessionResult,
) -> Result<ReadApply, ReadError> {
    // A continuation carries the pin it was issued with and must see the same
    // prefix. A fresh chain has no pin by design (spec §6.4): it opens a new
    // generation, so it may not regress the known total but is allowed a new
    // revision.
    if let Some(want) = page.want_pin.as_ref() {
        if want.captured_end != result.captured_end
            || want.history_revision != result.history_revision
        {
            return Ok(ReadApply::Stale(PinError::RevisionChanged));
        }
        if result.total < want.total {
            return Ok(ReadApply::Stale(PinError::TotalRegressed {
                known: want.total,
                found: result.total,
            }));
        }
    } else if let Some(existing) = window.pin() {
        if result.total < existing.total {
            return Ok(ReadApply::Stale(PinError::TotalRegressed {
                known: existing.total,
                found: result.total,
            }));
        }
    }

    let mut inserted = Vec::new();
    // The page must be contiguous from the requested cursor, and its items must
    // advance by exactly one; a gap is a protocol violation, never spliced.
    let mut expected = page.cursor.item;
    for chunk in &result.items {
        match page.assembler.push(chunk.clone()) {
            Ok(Assembled::Pending) => {}
            Ok(Assembled::Item { index, item }) => {
                // An already-loaded index must never silently change: a durable
                // item is immutable, so different bytes are a protocol conflict.
                // This is checked before ordering so an overlapping changed item
                // is reported as a conflict rather than a mere gap.
                if let Some(existing) = window.item(index) {
                    if existing.as_ref() != &item {
                        return Ok(ReadApply::Ok(AppliedPage {
                            inserted,
                            next: None,
                            error: Some(ReadError::ItemChanged { index }),
                        }));
                    }
                }
                if index != expected {
                    return Ok(ReadApply::Ok(AppliedPage {
                        inserted,
                        next: None,
                        error: Some(ReadError::NonContiguous {
                            expected,
                            found: index,
                        }),
                    }));
                }
                expected = index.saturating_add(1);
                if index < page.window_start || window.item(index).is_some() {
                    continue;
                }
                let item = window.insert(index, item);
                inserted.push((index, item));
            }
            Ok(Assembled::LargeItem { index, total_bytes }) => {
                // A visible placeholder belongs to the display model; stage C
                // renders it. The bytes are intentionally not retained.
                let _ = (index, total_bytes);
                expected = index.saturating_add(1);
            }
            Err(error) => {
                return Ok(ReadApply::Ok(AppliedPage {
                    inserted,
                    next: result.next_cursor,
                    error: Some(error),
                }));
            }
        }
    }
    // The backend cursor must agree with the items just delivered: it is the
    // position after the last complete item. A disagreement would loop forever
    // without loading anything.
    if let Some(next) = result.next_cursor {
        if next.item != expected {
            return Ok(ReadApply::Ok(AppliedPage {
                inserted,
                next: None,
                error: Some(ReadError::CursorStalled {
                    item: page.cursor.item,
                }),
            }));
        }
    }
    window.install_pin(result.pin());
    window.trailing_incomplete |= result.trailing_incomplete;
    window.records_truncated |= result.records_truncated;
    Ok(ReadApply::Ok(AppliedPage {
        inserted,
        next: result.next_cursor,
        error: None,
    }))
}

/// A bounded body budget across sessions (spec §6.5). The active session's
/// window is pinned; inactive sessions evict their least-recently-touched
/// items first. Eviction never calls `session.close` and never changes backend
/// history.
#[derive(Debug, Default)]
pub struct HistoryBudget {
    limit: usize,
    /// (session_id, index) in touch order, oldest first.
    order: Vec<(String, usize)>,
    pinned: HashSet<String>,
}

impl HistoryBudget {
    pub fn new(limit: usize) -> Self {
        Self {
            limit,
            order: Vec::new(),
            pinned: HashSet::new(),
        }
    }

    pub fn touch(&mut self, session_id: &str, index: usize) {
        let entry = (session_id.to_owned(), index);
        self.order.retain(|existing| existing != &entry);
        self.order.push(entry);
    }

    pub fn pin_session(&mut self, session_id: &str) {
        self.pinned.insert(session_id.to_owned());
    }

    pub fn unpin_session(&mut self, session_id: &str) {
        self.pinned.remove(session_id);
    }

    pub fn forget_session(&mut self, session_id: &str) {
        self.unpin_session(session_id);
        self.order.retain(|(id, _)| id != session_id);
    }

    /// Returns the items that must be evicted to fit `entries` items.
    pub fn evicted(&mut self, entries: usize) -> Vec<(String, usize)> {
        let mut evicted = Vec::new();
        let mut remaining = entries;
        while remaining > self.limit {
            let Some(position) = self
                .order
                .iter()
                .position(|(session_id, _)| !self.pinned.contains(session_id))
            else {
                break;
            };
            let entry = self.order.remove(position);
            remaining -= 1;
            evicted.push(entry);
        }
        evicted
    }
}

/// Convenience re-export so callers do not import the protocol path directly.
pub use crate::protocol::read::{ReadChunk as ReadChunkWire, ReadCursor as ReadCursorWire};
