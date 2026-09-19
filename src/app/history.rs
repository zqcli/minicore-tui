//! The authoritative Protocol v1 history window and its paging state
//! (spec §5.3, §6). This module owns the `session.read` pin/assembler chain so
//! it does not keep growing `app.rs`.
//!
//! It deliberately does **not** know about the display model. It validates
//! pages and stages canonical item bodies; the owned decode worker returns
//! transient `RawHistoryItem` values to `app.rs`, which immediately projects
//! them into the shared `TranscriptBlock` owner.

use std::collections::{BTreeMap, VecDeque};
use std::ops::Range;
use std::sync::Arc;

use crate::protocol::TurnRef;
use crate::state::transcript::{
    AssistantBlock, AssistantPart, SummaryBlock, ToolBlock, TranscriptBlock, UserBlock,
};

use super::*;
use crate::protocol::read::{
    Assembled, ChunkAssembler, RawHistoryItem, ReadCursor, ReadError, SnapshotPin, TurnResultPage,
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
    items: BTreeMap<usize, HistorySlot>,
    large_items: BTreeMap<usize, usize>,
    pending_large_items: BTreeMap<usize, ()>,
    loaded_ranges: Vec<Range<usize>>,
    bytes: usize,
    pub trailing_incomplete: bool,
    pub records_truncated: bool,
}

#[derive(Debug, Clone)]
struct HistorySlot {
    owner: Arc<TranscriptBlock>,
    fingerprint: u64,
    bytes: usize,
}

impl HistoryWindow {
    pub fn pin(&self) -> Option<&SnapshotPin> {
        self.pin.as_ref()
    }

    pub fn total(&self) -> usize {
        self.pin.as_ref().map_or(0, |pin| pin.total)
    }

    pub fn item(&self, index: usize) -> Option<&Arc<TranscriptBlock>> {
        self.items.get(&index).map(|slot| &slot.owner)
    }

    pub fn large_item(&self, index: usize) -> Option<usize> {
        self.large_items.get(&index).copied()
    }

    pub fn items(&self) -> impl Iterator<Item = (&usize, &Arc<TranscriptBlock>)> {
        self.items.iter().map(|(index, slot)| (index, &slot.owner))
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty() && self.large_items.is_empty()
    }

    pub fn len(&self) -> usize {
        self.items.len() + self.large_items.len()
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
        let end = self
            .loaded_ranges
            .iter()
            .find(|range| range.start == 0)
            .map_or(0, |range| range.end);
        self.pending_large_items
            .keys()
            .next()
            .map_or(end, |pending| end.min(*pending))
    }

    /// True when every item in `[0, total)` is present. An empty session
    /// (total 0) is trivially complete.
    pub fn complete(&self) -> bool {
        let total = self.total();
        total == 0 || (self.pending_large_items.is_empty() && self.confirmed_prefix() >= total)
    }

    pub fn has_items(&self) -> bool {
        !self.items.is_empty() || !self.large_items.is_empty()
    }

    pub fn reset(&mut self) {
        self.pin = None;
        self.items.clear();
        self.large_items.clear();
        self.pending_large_items.clear();
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

    /// Installs the one semantic owner shared by history and the transcript
    /// projection. Raw wire items never remain in this window.
    pub fn insert_owner(
        &mut self,
        index: usize,
        owner: Arc<TranscriptBlock>,
        fingerprint: u64,
        bytes: usize,
    ) {
        if let Some(previous) = self.items.remove(&index) {
            self.bytes = self.bytes.saturating_sub(previous.bytes);
        }
        self.large_items.remove(&index);
        self.pending_large_items.remove(&index);
        self.bytes += bytes;
        self.items.insert(
            index,
            HistorySlot {
                owner,
                fingerprint,
                bytes,
            },
        );
        self.merge_range(index);
    }

    pub(crate) fn fingerprint(&self, index: usize) -> Option<u64> {
        self.items.get(&index).map(|slot| slot.fingerprint)
    }

    #[cfg(test)]
    pub fn insert(&mut self, index: usize, item: RawHistoryItem) {
        let bytes = item_bytes(&item);
        let fingerprint = raw_item_fingerprint(&item);
        self.insert_owner(
            index,
            Arc::new(TranscriptBlock::HistoryPlaceholder(
                crate::state::transcript::HistoryPlaceholderBlock {
                    index,
                    total_bytes: bytes,
                },
            )),
            fingerprint,
            bytes,
        );
    }

    pub fn insert_placeholder(&mut self, index: usize, total_bytes: usize) {
        self.insert_large_placeholder(index, total_bytes, true);
    }

    pub fn insert_large_placeholder(&mut self, index: usize, total_bytes: usize, complete: bool) {
        if self.items.contains_key(&index) {
            return;
        }
        self.large_items.insert(index, total_bytes);
        if complete {
            self.pending_large_items.remove(&index);
        } else {
            self.pending_large_items.insert(index, ());
        }
        self.merge_range(index);
    }

    /// Removes the oldest decoded item unless it is protected. Returns the
    /// bytes released. `loaded_ranges` is split so the evicted index reads as
    /// a real gap again (spec §6.5): a later read re-fetches it instead of
    /// trusting a range that no longer has content.
    fn evict_oldest_entry(&mut self, protect_from: usize) -> Option<(usize, usize)> {
        let (&index, _) = self.items.iter().next()?;
        if index >= protect_from {
            return None;
        }
        self.remove_entry(index)
    }

    fn remove_entry(&mut self, index: usize) -> Option<(usize, usize)> {
        let item = self.items.remove(&index)?;
        let bytes = item.bytes;
        self.bytes = self.bytes.saturating_sub(bytes);
        self.forget_loaded(index);
        Some((index, bytes))
    }

    /// Evicts the retained item farthest from the protected source ranges.
    /// This is the head-browse case: when the viewport is near item zero, the
    /// tail is allowed to release first instead of making the whole suffix
    /// effectively protected by one lower-bound index.
    pub fn evict_farthest_entry(&mut self, protected: &[Range<usize>]) -> Option<(usize, usize)> {
        if protected.is_empty() {
            return self.evict_oldest_entry(usize::MAX);
        }
        let candidate = self
            .items
            .keys()
            .copied()
            .filter(|index| !protected.iter().any(|range| range.contains(index)))
            .max_by_key(|index| {
                let distance = protected
                    .iter()
                    .map(|range| {
                        if *index < range.start {
                            range.start.saturating_sub(*index)
                        } else {
                            index.saturating_sub(range.end).saturating_add(1)
                        }
                    })
                    .min()
                    .unwrap_or(0);
                (distance, *index)
            })?;
        self.remove_entry(candidate)
    }

    pub fn evict_oldest(&mut self, protect_from: usize) -> Option<usize> {
        self.evict_oldest_entry(protect_from)
            .map(|(_, bytes)| bytes)
    }

    /// Evicts unprotected oldest items until the retained bytes fit `budget`
    /// (or nothing left can be evicted). Returns the released bytes.
    pub fn evict_to_budget(&mut self, budget: usize, protect_from: usize) -> usize {
        let mut released = 0;
        while self.bytes > budget {
            match self.evict_oldest_entry(protect_from) {
                Some((_, bytes)) => released += bytes,
                None => break,
            }
        }
        released
    }

    /// The lowest index that eviction must keep for a protected tail of
    /// `tail` items. The pinned total is authoritative; when no pin is
    /// installed yet, the highest loaded index still gives a real bound so a
    /// content-free window cannot protect everything.
    pub fn protect_from(&self, tail: usize) -> usize {
        let last_loaded = self
            .items
            .keys()
            .next_back()
            .copied()
            .into_iter()
            .chain(self.large_items.keys().next_back().copied())
            .max()
            .map_or(0, |last| last + 1);
        self.total().max(last_loaded).saturating_sub(tail)
    }

    /// True when the oldest decoded item is outside the protected tail, so
    /// the budget pass can skip sessions that can no longer give anything up.
    pub fn can_evict(&self, protect_from: usize) -> bool {
        self.items
            .keys()
            .next()
            .is_some_and(|&index| index < protect_from)
    }

    /// Drops `index` from the loaded ranges, splitting the range it was in so
    /// the hole is reported honestly.
    fn forget_loaded(&mut self, index: usize) {
        let mut rebuilt = Vec::with_capacity(self.loaded_ranges.len() + 1);
        for range in self.loaded_ranges.drain(..) {
            if index < range.start || index >= range.end {
                rebuilt.push(range);
                continue;
            }
            if range.start < index {
                rebuilt.push(range.start..index);
            }
            if index + 1 < range.end {
                rebuilt.push(index + 1..range.end);
            }
        }
        self.loaded_ranges = rebuilt;
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

#[cfg(test)]
fn item_bytes(item: &RawHistoryItem) -> usize {
    use crate::protocol::read::RuntimeItem;
    // The raw item only exists at the reducer boundary, so its wire strings
    // are charged by length. Once projected, `owner_bytes` charges retained
    // String capacities instead of pretending a reallocation has no cost.
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

pub(crate) fn owner_bytes(owner: &TranscriptBlock) -> usize {
    match owner {
        TranscriptBlock::User(user) => {
            user.text.capacity() + user.loop_id.as_ref().map_or(0, String::capacity)
        }
        TranscriptBlock::Assistant(assistant) => {
            assistant.loop_id.capacity()
                + assistant.model.capacity()
                + assistant.finish_reason.capacity()
                + assistant
                    .parts
                    .iter()
                    .map(|part| match part {
                        AssistantPart::Text(text) | AssistantPart::Reasoning(text) => {
                            text.capacity()
                        }
                        AssistantPart::ToolCall(call) => {
                            call.tool_call_id.capacity() + call.name.capacity()
                        }
                    })
                    .sum::<usize>()
        }
        TranscriptBlock::Tool(tool) => {
            tool.loop_id.capacity()
                + tool.tool_call_id.capacity()
                + tool.name.capacity()
                + tool.result.as_ref().map_or(0, |result| result.len())
                + tool.progress.as_ref().map_or(0, String::capacity)
        }
        TranscriptBlock::Summary(summary) => summary.content.capacity(),
        TranscriptBlock::HistoryPlaceholder(placeholder) => placeholder.total_bytes,
    }
}

#[cfg(test)]
fn raw_item_fingerprint(item: &RawHistoryItem) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    format!("{item:?}").hash(&mut hasher);
    hasher.finish()
}

pub(crate) fn encoded_item_fingerprint(item: &crate::protocol::read::EncodedHistoryItem) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    item.data.hash(&mut hasher);
    hasher.finish()
}

/// One in-flight `session.read` page. It owns the chunk assembler so a page
/// that lands outside its requested window can discard a partial item instead
/// of mistaking it for loaded content (spec §6.3).
#[derive(Debug)]
pub struct ReadPage {
    pub cursor: ReadCursor,
    pub want_pin: Option<SnapshotPin>,
    pub assembler: ChunkAssembler,
    /// Complete canonical items waiting for the single decode worker. The page
    /// size is bounded by the remote `READ_PAGE_MAX_BYTES` limit, and only the
    /// front item is ever submitted.
    pub pending_encoded: VecDeque<crate::protocol::read::EncodedHistoryItem>,
    /// Page metadata is held until every encoded item has been decoded.
    pub pending_page: Option<PendingPage>,
    /// The lowest index this page may contribute to the window. A reused
    /// first-page chunk below the window start is dropped, not faked.
    pub window_start: usize,
    /// Whether this page replaces the view rather than appending.
    pub replacement: bool,
    /// Whether this page is a stale-check or gap reconcile.
    pub reconcile: bool,
    /// Gap revision captured when this chain started; late event drops during
    /// decode must still force reconciliation at page completion.
    pub gap_revision: u64,
}

#[derive(Debug, Clone)]
pub struct PendingPage {
    pub next: Option<ReadCursor>,
    pub explicit_large_item: bool,
    pub error: Option<ReadError>,
}

/// Converts one decoded result item into its semantic owner. This conversion
/// is shared by the session history reducer and the turn-result recovery
/// window; neither window retains the raw wire item after this boundary.
pub(super) fn raw_item_owner(index: usize, item: &RawHistoryItem) -> Arc<TranscriptBlock> {
    use crate::protocol::read::{RuntimeAssistantPart, RuntimeItem, RuntimeUserKind};

    match &item.item {
        RuntimeItem::User(user) => Arc::new(TranscriptBlock::User(UserBlock {
            index: Some(index),
            loop_id: Some(user.loop_id.clone()),
            kind: match user.kind {
                RuntimeUserKind::Prompt => crate::protocol::UserMessageKindWire::Prompt,
                RuntimeUserKind::Steering => crate::protocol::UserMessageKindWire::Steering,
            },
            text: user.input.text.clone(),
            pending: false,
        })),
        RuntimeItem::Assistant(assistant) => {
            let mut parts = Vec::new();
            let mut tool_calls = Vec::new();
            for part in &assistant.content {
                match part {
                    RuntimeAssistantPart::Text(text) if !text.is_empty() => {
                        parts.push(AssistantPart::Text(text.clone()));
                    }
                    RuntimeAssistantPart::Reasoning { text, summary, .. } => {
                        let body = text.clone().or_else(|| summary.clone()).unwrap_or_default();
                        if !body.is_empty() {
                            parts.push(AssistantPart::Reasoning(body));
                        }
                    }
                    RuntimeAssistantPart::Text(_) => {}
                    RuntimeAssistantPart::ToolCall {
                        tool_call_id,
                        name,
                        call_index,
                        ..
                    } => {
                        let call = crate::protocol::ToolCallViewWire {
                            tool_call_id: tool_call_id.clone(),
                            name: name.clone(),
                            call_index: *call_index,
                            display: None,
                        };
                        parts.push(AssistantPart::ToolCall(call.clone()));
                        tool_calls.push(call);
                    }
                }
            }
            let reasoning_level = assistant
                .reasoning
                .as_deref()
                .and_then(|value| {
                    serde_json::from_value::<crate::protocol::Reasoning>(serde_json::Value::String(
                        value.to_owned(),
                    ))
                    .ok()
                })
                .unwrap_or_default();
            Arc::new(TranscriptBlock::Assistant(AssistantBlock {
                index,
                loop_id: assistant.loop_id.clone(),
                request_index: assistant.request_index,
                model: assistant.model.clone(),
                reasoning_level,
                parts,
                tool_calls,
                usage: assistant.usage,
                finish_reason: assistant.finish_reason.clone(),
                terminal_error: None,
            }))
        }
        RuntimeItem::ToolResult(result) => Arc::new(TranscriptBlock::Tool(ToolBlock {
            index: Some(index),
            loop_id: result.loop_id.clone(),
            request_index: result.request_index,
            tool_call_id: result.call_id.clone(),
            name: result.tool_name.clone(),
            result: Some(Arc::<str>::from(result.output.content.as_str())),
            outcome: serde_json::from_value::<crate::protocol::ToolOutcomeWire>(
                serde_json::Value::String(result.outcome.clone()),
            )
            .ok(),
            live_status: None,
            progress: None,
            expanded: false,
        })),
        RuntimeItem::Summary(summary) => Arc::new(TranscriptBlock::Summary(SummaryBlock {
            index,
            content: summary.content.clone(),
        })),
    }
}

impl ReadPage {
    pub fn new(cursor: ReadCursor, want_pin: Option<SnapshotPin>, window_start: usize) -> Self {
        Self {
            cursor,
            want_pin,
            assembler: ChunkAssembler::new(),
            pending_encoded: VecDeque::new(),
            pending_page: None,
            window_start,
            replacement: false,
            reconcile: false,
            gap_revision: 0,
        }
    }
}

/// A paged `turn.result` body. Its indexes remain local to the Turn and are
/// never inserted into the session-global history window.
#[derive(Debug)]
pub struct TurnResultWindow {
    pub turn: TurnRef,
    pub cursor: ReadCursor,
    pub assembler: ChunkAssembler,
    pub total: Option<usize>,
    pub read_chain: u64,
    pub pending_encoded: VecDeque<crate::protocol::read::EncodedHistoryItem>,
    pub pending_page: Option<PendingTurnPage>,
    pub items: BTreeMap<usize, Arc<TranscriptBlock>>,
    pub fingerprints: BTreeMap<usize, u64>,
    pub large_items: BTreeMap<usize, usize>,
    pub pending_large_items: BTreeMap<usize, ()>,
    pub explicit_large_item: bool,
    pub complete: bool,
}

#[derive(Debug, Clone)]
pub struct PendingTurnPage {
    pub availability: crate::protocol::read::TurnAvailability,
    pub outcome: Option<crate::protocol::LoopOutcomeWire>,
    pub persistence: Option<crate::protocol::TurnPersistenceWire>,
    pub usage: Option<crate::protocol::UsageWire>,
    pub requests: Option<u32>,
    pub tool_rounds: Option<u16>,
    pub final_config_revision: Option<u64>,
    pub completed_at: Option<String>,
    pub total: usize,
    pub next_cursor: Option<ReadCursor>,
    pub terminal: bool,
}

impl TurnResultWindow {
    pub fn new(turn: TurnRef) -> Self {
        Self {
            turn,
            cursor: ReadCursor::start(),
            assembler: ChunkAssembler::new(),
            total: None,
            read_chain: 0,
            pending_encoded: VecDeque::new(),
            pending_page: None,
            items: BTreeMap::new(),
            fingerprints: BTreeMap::new(),
            large_items: BTreeMap::new(),
            pending_large_items: BTreeMap::new(),
            explicit_large_item: false,
            complete: false,
        }
    }

    /// Validates a page and stages its complete canonical items without
    /// deserializing them. Production calls this method and drains
    /// `pending_encoded` through the decode worker.
    pub fn stage_page(&mut self, page: &TurnResultPage) -> Result<(), ReadError> {
        if page.turn != self.turn {
            return Err(ReadError::NonContiguous {
                expected: self.cursor.item,
                found: page.total,
            });
        }
        self.pending_encoded.clear();
        self.pending_page = None;
        let terminal = !matches!(
            page.availability,
            crate::protocol::read::TurnAvailability::Pending
        );
        if terminal {
            if let Some(expected) = self.total {
                if expected != page.total {
                    return Err(ReadError::TurnTotalMismatch {
                        expected,
                        found: page.total,
                    });
                }
            }
            self.total = Some(page.total);
        }

        let mut expected = self.cursor.item;
        for chunk in &page.items {
            match self.assembler.push(chunk.clone())? {
                Assembled::Pending => {}
                Assembled::EncodedItem { item } => {
                    let index = item.index;
                    if index != expected {
                        return Err(ReadError::NonContiguous {
                            expected,
                            found: index,
                        });
                    }
                    expected = index.saturating_add(1);
                    let fingerprint = encoded_item_fingerprint(&item);
                    if let Some(existing) = self.fingerprints.get(&index) {
                        if *existing != fingerprint {
                            return Err(ReadError::ItemChanged { index });
                        }
                    } else {
                        self.large_items.remove(&index);
                        self.pending_large_items.remove(&index);
                        self.fingerprints.insert(index, fingerprint);
                        self.pending_encoded.push_back(item);
                    }
                }
                Assembled::LargeItem { index, total_bytes } => {
                    if index != expected {
                        return Err(ReadError::NonContiguous {
                            expected,
                            found: index,
                        });
                    }
                    expected = index.saturating_add(1);
                    if !self.items.contains_key(&index) {
                        self.large_items.insert(index, total_bytes);
                        self.pending_large_items.remove(&index);
                    }
                }
                Assembled::LargeItemPending { index, total_bytes } => {
                    if index != expected {
                        return Err(ReadError::NonContiguous {
                            expected,
                            found: index,
                        });
                    }
                    self.large_items.insert(index, total_bytes);
                    self.pending_large_items.insert(index, ());
                    self.explicit_large_item = true;
                }
            }
        }

        if let Some(next) = page.next_cursor {
            if next.item != expected {
                return Err(ReadError::CursorStalled {
                    item: self.cursor.item,
                });
            }
            if let Some(partial) = self.assembler.next_cursor() {
                if next != partial {
                    return Err(ReadError::CursorOffsetMismatch {
                        expected: partial.offset,
                        found: next.offset,
                    });
                }
            } else if next.offset != 0 {
                return Err(ReadError::CursorOffsetMismatch {
                    expected: 0,
                    found: next.offset,
                });
            }
            if next == self.cursor && !self.explicit_large_item {
                return Err(ReadError::CursorStalled {
                    item: self.cursor.item,
                });
            }
            self.cursor = next;
        } else {
            if self.assembler.current_index().is_some() {
                return Err(ReadError::CursorStalled { item: expected });
            }
            if terminal && expected != page.total {
                return Err(ReadError::NonContiguous {
                    expected,
                    found: page.total,
                });
            }
            self.cursor = ReadCursor {
                item: expected,
                offset: 0,
            };
        }
        self.pending_page = Some(PendingTurnPage {
            availability: page.availability,
            outcome: page.outcome.clone(),
            persistence: page.persistence,
            usage: page.usage,
            requests: page.requests,
            tool_rounds: page.tool_rounds,
            final_config_revision: page.final_config_revision,
            completed_at: page.completed_at.clone(),
            total: page.total,
            next_cursor: page.next_cursor,
            terminal,
        });
        self.complete = self.pending_encoded.is_empty()
            && terminal
            && self.pending_large_items.is_empty()
            && page.next_cursor.is_none();
        Ok(())
    }

    /// Installs one worker-decoded item and returns whether the staged page is
    /// now ready for its metadata/cursor to be committed.
    pub fn apply_decoded(
        &mut self,
        index: usize,
        item: RawHistoryItem,
        fingerprint: u64,
    ) -> Result<bool, ReadError> {
        if self
            .pending_encoded
            .front()
            .is_none_or(|encoded| encoded.index != index)
            || self.fingerprints.get(&index).copied() != Some(fingerprint)
        {
            return Err(ReadError::ItemChanged { index });
        }
        self.pending_encoded.pop_front();
        self.items
            .entry(index)
            .or_insert_with(|| raw_item_owner(index, &item));
        let ready = self.pending_encoded.is_empty();
        if ready {
            if let Some(page) = self.pending_page.as_ref() {
                self.complete = page.terminal
                    && page.next_cursor.is_none()
                    && self.pending_large_items.is_empty();
            }
            self.pending_page = None;
        }
        Ok(ready)
    }

    /// Compatibility/test-only synchronous drain. Production result handling
    /// uses `stage_page` plus `apply_decoded` from the owned worker.
    pub fn apply_page(&mut self, page: &TurnResultPage) -> Result<(), ReadError> {
        self.stage_page(page)?;
        while let Some(encoded) = self.pending_encoded.pop_front() {
            let item = crate::protocol::read::decode_item(&encoded.data).map_err(|detail| {
                ReadError::MalformedItem {
                    index: encoded.index,
                    detail,
                }
            })?;
            self.items
                .insert(encoded.index, raw_item_owner(encoded.index, &item));
        }
        self.pending_page = None;
        let terminal = !matches!(page.availability, TurnAvailability::Pending);
        self.complete =
            terminal && page.next_cursor.is_none() && self.pending_large_items.is_empty();
        Ok(())
    }
}

/// What one applied page contributed.
#[derive(Debug)]
pub struct AppliedPage {
    /// Complete canonical items in page order. JSON decoding is performed by
    /// the owned worker after this page has passed chunk validation.
    pub inserted: Vec<crate::protocol::read::EncodedHistoryItem>,
    /// Items over the automatic decode budget. Their bytes are not retained,
    /// but their indexes remain visible as bounded placeholders.
    pub placeholders: Vec<(usize, usize)>,
    /// The backend-provided cursor for the next page, if any.
    pub next: Option<ReadCursor>,
    /// A large item was surfaced before its bytes were exhausted. Ordinary
    /// pagination must stop until an explicit continuation is requested.
    pub explicit_large_item: bool,
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
    result.pin().validate()?;
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
        if result.total != want.total {
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

    page.pending_encoded.clear();
    page.pending_page = None;
    let mut inserted = Vec::new();
    let mut placeholders = Vec::new();
    let mut explicit_large_item = false;
    // The page must be contiguous from the requested cursor, and its items must
    // advance by exactly one; a gap is a protocol violation, never spliced.
    let mut expected = page.cursor.item;
    for chunk in &result.items {
        match page.assembler.push(chunk.clone()) {
            Ok(Assembled::Pending) => {}
            Ok(Assembled::EncodedItem { item }) => {
                let index = item.index;
                // An already-loaded index must never silently change: a durable
                // item is immutable, so different canonical bytes are a
                // protocol conflict. Comparing the encoded owner avoids a
                // second JSON decode on the App thread.
                if let Some(existing) = window.fingerprint(index) {
                    if existing != encoded_item_fingerprint(&item) {
                        return Ok(ReadApply::Ok(AppliedPage {
                            inserted,
                            placeholders,
                            next: None,
                            explicit_large_item,
                            error: Some(ReadError::ItemChanged { index }),
                        }));
                    }
                }
                if index != expected {
                    return Ok(ReadApply::Ok(AppliedPage {
                        inserted,
                        placeholders,
                        next: None,
                        explicit_large_item,
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
                inserted.push(item);
            }
            Ok(Assembled::LargeItem { index, total_bytes }) => {
                if index != expected {
                    return Ok(ReadApply::Ok(AppliedPage {
                        inserted,
                        placeholders,
                        next: None,
                        explicit_large_item,
                        error: Some(ReadError::NonContiguous {
                            expected,
                            found: index,
                        }),
                    }));
                }
                if index >= page.window_start && window.item(index).is_none() {
                    window.insert_large_placeholder(index, total_bytes, true);
                    placeholders.push((index, total_bytes));
                }
                expected = index.saturating_add(1);
            }
            Ok(Assembled::LargeItemPending { index, total_bytes }) => {
                if index != expected {
                    return Ok(ReadApply::Ok(AppliedPage {
                        inserted,
                        placeholders,
                        next: None,
                        explicit_large_item,
                        error: Some(ReadError::NonContiguous {
                            expected,
                            found: index,
                        }),
                    }));
                }
                if index >= page.window_start && window.item(index).is_none() {
                    window.insert_large_placeholder(index, total_bytes, false);
                    placeholders.push((index, total_bytes));
                }
                explicit_large_item = true;
            }
            Err(error) => {
                return Ok(ReadApply::Ok(AppliedPage {
                    inserted,
                    placeholders,
                    next: result.next_cursor,
                    explicit_large_item,
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
                placeholders,
                next: None,
                explicit_large_item,
                error: Some(ReadError::CursorStalled {
                    item: page.cursor.item,
                }),
            }));
        }
        if next == page.cursor && !explicit_large_item {
            return Ok(ReadApply::Ok(AppliedPage {
                inserted,
                placeholders,
                next: None,
                explicit_large_item,
                error: Some(ReadError::CursorStalled {
                    item: page.cursor.item,
                }),
            }));
        }
        if let Some(partial) = page.assembler.next_cursor() {
            if next != partial {
                return Ok(ReadApply::Ok(AppliedPage {
                    inserted,
                    placeholders,
                    next: None,
                    explicit_large_item,
                    error: Some(ReadError::CursorOffsetMismatch {
                        expected: partial.offset,
                        found: next.offset,
                    }),
                }));
            }
        } else if next.offset != 0 {
            return Ok(ReadApply::Ok(AppliedPage {
                inserted,
                placeholders,
                next: None,
                explicit_large_item,
                error: Some(ReadError::CursorOffsetMismatch {
                    expected: 0,
                    found: next.offset,
                }),
            }));
        }
    } else {
        if page.assembler.current_index().is_some() {
            return Ok(ReadApply::Ok(AppliedPage {
                inserted,
                placeholders,
                next: None,
                explicit_large_item,
                error: Some(ReadError::CursorStalled { item: expected }),
            }));
        }
        if expected != result.total {
            return Ok(ReadApply::Ok(AppliedPage {
                inserted,
                placeholders,
                next: None,
                explicit_large_item,
                error: Some(ReadError::NonContiguous {
                    expected,
                    found: result.total,
                }),
            }));
        }
    }
    window.install_pin(result.pin());
    window.trailing_incomplete |= result.trailing_incomplete;
    window.records_truncated |= result.records_truncated;
    page.pending_encoded = inserted.iter().cloned().collect();
    page.pending_page = Some(PendingPage {
        next: result.next_cursor,
        explicit_large_item,
        error: None,
    });
    Ok(ReadApply::Ok(AppliedPage {
        inserted,
        placeholders,
        next: result.next_cursor,
        explicit_large_item,
        error: None,
    }))
}

/// Convenience re-export so callers do not import the protocol path directly.
pub use crate::protocol::read::{ReadChunk as ReadChunkWire, ReadCursor as ReadCursorWire};

impl App {
    pub(crate) fn enforce_live_budget(&mut self) {
        for view in self.sessions.known.values_mut() {
            if let Some(live) = view.live.as_mut() {
                if live.retained_bytes() > crate::limits::LIVE_LOOP_BYTES {
                    live.trim_to_bytes(crate::limits::LIVE_LOOP_BYTES);
                    live.event_gap = true;
                    view.event_gap = true;
                }
            }
            if let Some(unsaved) = view.unsaved_loop.as_mut() {
                if unsaved.retained_bytes() > crate::limits::LIVE_LOOP_BYTES {
                    unsaved.trim_to_bytes(crate::limits::LIVE_LOOP_BYTES);
                    view.event_gap = true;
                }
            }
        }
        for view in self.sessions.known.values_mut() {
            let total = view.live.as_ref().map_or(0, LiveLoop::retained_bytes)
                + view
                    .unsaved_loop
                    .as_ref()
                    .map_or(0, UnsavedLoop::retained_bytes);
            if total > crate::limits::LIVE_TOTAL_BYTES {
                if let Some(live) = view.live.as_mut() {
                    let allowed = live.retained_bytes().min(crate::limits::LIVE_TOTAL_BYTES);
                    live.trim_to_bytes(allowed);
                    live.event_gap = true;
                }
                if let Some(unsaved) = view.unsaved_loop.as_mut() {
                    let remaining = crate::limits::LIVE_TOTAL_BYTES
                        .saturating_sub(view.live.as_ref().map_or(0, LiveLoop::retained_bytes));
                    unsaved.trim_to_bytes(remaining);
                }
                view.event_gap = true;
            }
        }
    }

    pub(crate) fn enforce_tool_budget(&mut self) {
        for view in self.sessions.known.values_mut() {
            let presentations = std::sync::Arc::make_mut(&mut view.tool_presentations);
            let mut changed = false;
            for state in presentations.values_mut() {
                let state = std::sync::Arc::make_mut(state);
                if state.retained_bytes() > crate::limits::TOOL_STREAM_BYTES {
                    state.truncate_to_bytes(crate::limits::TOOL_STREAM_BYTES);
                    changed = true;
                }
            }
            let mut remaining = crate::limits::TOOL_TOTAL_BYTES;
            for state in presentations.values_mut() {
                let state = std::sync::Arc::make_mut(state);
                let before = state.retained_bytes();
                let allowed = before.min(remaining);
                if before > allowed {
                    state.truncate_to_bytes(allowed);
                    changed = true;
                }
                remaining = remaining.saturating_sub(state.retained_bytes());
            }
            if changed {
                view.transcript.invalidate();
                self.prepared_conversation = None;
            }
        }
    }

    fn history_protection_ranges(
        view: &SessionView,
        viewport: (usize, usize),
        tail: usize,
        active: bool,
        frame_header_rows: usize,
        durable_skip: usize,
    ) -> Vec<Range<usize>> {
        let mut ranges = Vec::new();
        if active && tail > 0 {
            let scroll_offset = if view.scroll.follow_tail {
                view.transcript.render_cache.as_ref().map_or_else(
                    || {
                        view.transcript
                            .window
                            .items()
                            .last()
                            .map_or(0, |(index, _)| *index)
                    },
                    |durable| {
                        durable
                            .layout
                            .total_rows
                            .saturating_sub(viewport.1.max(1))
                            .saturating_add(frame_header_rows.saturating_sub(durable_skip))
                    },
                )
            } else {
                view.scroll.offset
            };
            let durable_offset = scroll_offset
                .saturating_sub(frame_header_rows)
                .saturating_add(durable_skip);
            let viewport_start = durable_offset.saturating_sub(tail);
            let viewport_end = durable_offset
                .saturating_add(viewport.1.max(1))
                .saturating_add(tail);
            if let Some(durable) = view.transcript.render_cache.as_ref() {
                for placement in durable.layout.sections.iter() {
                    if placement.rows.end <= viewport_start || placement.rows.start >= viewport_end
                    {
                        continue;
                    }
                    if let Some(index) = placement.layout.key.section.history_index {
                        ranges.push(index.saturating_sub(tail)..index.saturating_add(tail + 1));
                    }
                }
            }
            if ranges.is_empty() {
                let indices = view
                    .transcript
                    .window
                    .items()
                    .map(|(index, _)| *index)
                    .collect::<Vec<_>>();
                if let Some((position, _)) = indices
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, index)| (**index).abs_diff(durable_offset))
                {
                    let start = position.saturating_sub(tail);
                    let end = position.saturating_add(tail + 1).min(indices.len());
                    if let (Some(first), Some(last)) = (indices.get(start), indices.get(end - 1)) {
                        ranges.push(*first..last.saturating_add(1));
                    }
                }
            }
        }

        let recent_loop = view
            .last_result
            .as_ref()
            .map(|result| result.turn.loop_id.as_str())
            .or_else(|| {
                view.live
                    .as_ref()
                    .and_then(|live| live.reference.as_ref())
                    .map(|turn| turn.loop_id.as_str())
            });
        if let Some(loop_id) = recent_loop {
            let result_indices = view
                .transcript
                .window
                .items()
                .filter_map(|(index, block)| {
                    let matches = match block.as_ref() {
                        TranscriptBlock::User(block) => block.loop_id.as_deref() == Some(loop_id),
                        TranscriptBlock::Assistant(block) => block.loop_id == loop_id,
                        TranscriptBlock::Tool(block) => block.loop_id == loop_id,
                        TranscriptBlock::Summary(_) | TranscriptBlock::HistoryPlaceholder(_) => {
                            false
                        }
                    };
                    matches.then_some(*index)
                })
                .collect::<Vec<_>>();
            if let Some(last) = result_indices.last() {
                let radius = tail.min(8);
                ranges.push(last.saturating_sub(radius)..last.saturating_add(radius + 1));
            }
        }

        ranges.sort_by_key(|range| range.start);
        let mut merged: Vec<Range<usize>> = Vec::new();
        for range in ranges {
            if let Some(previous) = merged.last_mut() {
                if range.start <= previous.end {
                    previous.end = previous.end.max(range.end);
                    continue;
                }
            }
            merged.push(range);
        }
        merged
    }

    pub(super) fn retain_result_summary(&mut self, result: crate::protocol::TurnResultViewWire) {
        let turn = result.turn.clone();
        if !self.retained_results.contains_key(&turn) {
            self.retained_result_order.push_back(turn.clone());
        }
        self.retained_results.insert(turn, result);
        while self.retained_result_order.len() > MAX_RETAINED_TURN_RESULTS {
            if let Some(oldest) = self.retained_result_order.pop_front() {
                self.retained_results.remove(&oldest);
                self.turn_results.remove(&oldest);
            }
        }
    }

    pub(super) fn request_turn_result_page(
        &mut self,
        turn: TurnRef,
        cursor: crate::protocol::ReadCursor,
    ) -> Option<AppCommand> {
        if self.turn_result_decode_pending(&turn) {
            return None;
        }
        let kind = RequestKind::TurnResult(turn.clone());
        if let Some(retry) = Self::retry_key(&kind) {
            if self.retry_pending(&retry) {
                return None;
            }
        }
        if !self.deferred_admission_ok() {
            self.defer_request(kind, |id| {
                OutgoingRequest::turn_result(
                    id,
                    &turn,
                    Some(cursor),
                    READ_PAGE_LIMIT,
                    READ_PAGE_MAX_BYTES,
                )
            });
            return None;
        }
        self.turn_results
            .entry(turn.clone())
            .or_insert_with(|| crate::app::history::TurnResultWindow::new(turn.clone()));
        let needs_read_chain = self
            .turn_results
            .get(&turn)
            .is_some_and(|window| window.read_chain == 0);
        if needs_read_chain {
            self.next_read_chain = self
                .next_read_chain
                .checked_add(1)
                .expect("read chains exhausted");
            let chain = self.next_read_chain;
            if let Some(window) = self.turn_results.get_mut(&turn) {
                window.read_chain = chain;
            }
        }
        let key = crate::app::queries::QueryKey::TurnResult {
            session_id: turn.session_id.clone(),
            loop_id: turn.loop_id.clone(),
        };
        let id = self.next_request_id();
        if self.queries.request_query(key, id) != crate::app::queries::QueryAdmission::Admitted {
            return None;
        }
        let request = OutgoingRequest::turn_result(
            id,
            &turn,
            Some(cursor),
            READ_PAGE_LIMIT,
            READ_PAGE_MAX_BYTES,
        );
        self.pending_requests.insert(id, kind);
        Some(AppCommand::Rpc(request))
    }

    pub(super) fn pending_history(&self, session_id: &SessionId) -> bool {
        self.pending_requests.values().any(|kind| {
            matches!(
                kind,
                RequestKind::History { session_id: pending, .. } if pending == session_id
            )
        })
    }

    /// The next read for a chain that is either starting fresh or continuing
    /// from the backend's own cursor. A fresh chain never carries the old window
    /// pin: after a new turn the revision has moved and §6.4 requires a new pin.
    /// A fresh window starts with the one-item §6.3 probe.
    pub(super) fn history_decode_pending(&self, session_id: &SessionId) -> bool {
        let page_pending = self
            .sessions
            .known
            .get(session_id)
            .and_then(|view| view.read_page.as_ref())
            .is_some_and(|page| !page.pending_encoded.is_empty());
        let request_pending = self.pending_decode.as_ref().is_some_and(|request| {
            matches!(
                &request.identity.target,
                crate::jobs::DecodeTarget::History { session_id: pending, .. }
                    if pending == session_id
            )
        });
        let worker_pending = self.decode_in_flight.as_ref().is_some_and(|identity| {
            matches!(
                &identity.target,
                crate::jobs::DecodeTarget::History { session_id: pending, .. }
                    if pending == session_id
            )
        });
        page_pending || request_pending || worker_pending
    }

    pub(super) fn invalidate_decode_for_session(&mut self, session_id: &SessionId) {
        let target_matches = |target: &crate::jobs::DecodeTarget| match target {
            crate::jobs::DecodeTarget::History {
                session_id: pending,
                ..
            } => pending == session_id,
            crate::jobs::DecodeTarget::TurnResult { turn, .. } => turn.session_id == *session_id,
            crate::jobs::DecodeTarget::SearchScan {
                session_id: pending,
                ..
            } => pending == session_id,
        };
        if self
            .pending_decode
            .as_ref()
            .is_some_and(|request| target_matches(&request.identity.target))
        {
            if let Some(request) = self.pending_decode.take() {
                request
                    .cancel
                    .store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }
        if self
            .decode_in_flight
            .as_ref()
            .is_some_and(|identity| target_matches(&identity.target))
        {
            self.decode_in_flight = None;
        }
    }

    pub(super) fn request_history(&mut self, session_id: &SessionId) -> Option<AppCommand> {
        if self.history_decode_pending(session_id) {
            return None;
        }
        if let Some(view) = self.sessions.known.get_mut(session_id) {
            if view.read_page.is_none() && view.history_read.is_loading() {
                view.history_query_generation = view
                    .history_query_generation
                    .checked_add(1)
                    .expect("history query generations exhausted");
            }
        }
        let (cursor, pin, window_start, replacement, reconcile, probe) = self
            .sessions
            .known
            .get(session_id)
            .map(|view| {
                let next = view.transcript.next_cursor;
                let pin = next.and(view.transcript.window.pin().cloned());
                (
                    next.unwrap_or(crate::protocol::ReadCursor::start()),
                    pin.clone(),
                    view.transcript.window.confirmed_prefix(),
                    view.transcript.window.is_empty(),
                    view.event_gap,
                    // Probe whenever this request carries no pin: the read then
                    // has to establish the prefix before any windowed read.
                    pin.is_none(),
                )
            })
            .unwrap_or((
                crate::protocol::ReadCursor::start(),
                None,
                0,
                true,
                false,
                true,
            ));
        let gap_revision = self
            .sessions
            .known
            .get(session_id)
            .map_or(0, |view| view.gap_revision);
        self.request_read(
            session_id,
            ReadRequest {
                cursor,
                pin,
                window_start,
                replacement,
                reconcile,
                probe,
                gap_revision,
            },
        )
    }

    fn queue_history_decode(&mut self, session_id: &SessionId) {
        if self.pending_decode.is_some() || self.decode_in_flight.is_some() {
            return;
        }
        let Some((session_epoch, read_chain, item)) =
            self.sessions.known.get(session_id).and_then(|view| {
                view.read_page.as_ref().and_then(|page| {
                    page.pending_encoded
                        .front()
                        .cloned()
                        .map(|item| (view.session_epoch, view.history_query_generation, item))
                })
            })
        else {
            return;
        };
        let identity = crate::jobs::DecodeIdentity {
            session_epoch,
            read_chain,
            target: crate::jobs::DecodeTarget::History {
                session_id: session_id.clone(),
                index: item.index,
            },
        };
        let request = crate::jobs::DecodeRequest {
            identity: identity.clone(),
            fingerprint: encoded_item_fingerprint(&item),
            item,
            cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            scan: None,
        };
        self.decode_in_flight = Some(identity);
        self.pending_decode = Some(request);
    }

    pub(super) fn turn_result_decode_pending(&self, turn: &TurnRef) -> bool {
        let window_pending = self
            .turn_results
            .get(turn)
            .is_some_and(|window| !window.pending_encoded.is_empty());
        let request_pending = self.pending_decode.as_ref().is_some_and(|request| {
            matches!(
                &request.identity.target,
                crate::jobs::DecodeTarget::TurnResult { turn: pending, .. }
                    if pending == turn
            )
        });
        let worker_pending = self.decode_in_flight.as_ref().is_some_and(|identity| {
            matches!(
                &identity.target,
                crate::jobs::DecodeTarget::TurnResult { turn: pending, .. }
                    if pending == turn
            )
        });
        window_pending || request_pending || worker_pending
    }

    fn queue_turn_result_decode(&mut self, turn: &TurnRef) {
        if self.pending_decode.is_some() || self.decode_in_flight.is_some() {
            return;
        }
        let Some((session_epoch, read_chain, item)) =
            self.turn_results.get(turn).and_then(|window| {
                window.pending_encoded.front().cloned().map(|item| {
                    let epoch = self
                        .sessions
                        .known
                        .get(&turn.session_id)
                        .map_or(0, |view| view.session_epoch);
                    (epoch, window.read_chain, item)
                })
            })
        else {
            return;
        };
        let identity = crate::jobs::DecodeIdentity {
            session_epoch,
            read_chain,
            target: crate::jobs::DecodeTarget::TurnResult {
                turn: turn.clone(),
                index: item.index,
            },
        };
        let request = crate::jobs::DecodeRequest {
            identity: identity.clone(),
            fingerprint: encoded_item_fingerprint(&item),
            item,
            cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            scan: None,
        };
        self.decode_in_flight = Some(identity);
        self.pending_decode = Some(request);
    }

    pub(super) fn pending_open_or_history(&self, session_id: &SessionId) -> bool {
        self.pending_requests.values().any(|kind| {
            matches!(
                kind,
                RequestKind::OpenSession { session_id: pending, .. } if pending == session_id
            ) || matches!(
                kind,
                RequestKind::History { session_id: pending, .. } if pending == session_id
            )
        })
    }

    pub(super) fn resume_deferred_reconcile(&mut self, session_id: &SessionId) -> Vec<AppCommand> {
        if !self.can_send_requests()
            || self.session_pending_deletion(session_id)
            || self.sessions.closed.contains(session_id)
        {
            return Vec::new();
        }
        let state_needed = {
            let Some(view) = self.sessions.known.get(session_id) else {
                return Vec::new();
            };
            let terminal_live = view
                .live
                .as_ref()
                .is_some_and(|live| live.waiting && live.last_result.is_some());
            if !view.history_read.post_wait_pending()
                || !view.info.loaded
                || view.closing
                || view.history_read.is_loading()
                || (view.live.is_some() && !terminal_live)
                || view.unsaved_loop.is_some()
                || self.pending_history(session_id)
            {
                return Vec::new();
            }
            view.latest_state_query.is_none()
        };
        if let Some(view) = self.sessions.known.get_mut(session_id) {
            view.history_read.take_pending();
            view.history_read.begin(HistoryTrigger::Gap);
        }
        let mut commands = Vec::new();
        if state_needed {
            commands.push(self.request_session_state(session_id));
        }
        commands.extend(self.request_history(session_id));
        commands
    }

    pub(super) fn history_proves_steer_not_recorded(
        view: &SessionView,
        loop_id: &str,
        steer_text: &str,
    ) -> bool {
        view.transcript.complete
            && (view.last_result.as_ref().is_some_and(|result| {
                result.turn.loop_id == loop_id
                    && result.persistence == Some(TurnPersistenceWire::Persisted)
            }) || view.live.as_ref().is_some_and(|live| {
                live.last_result.as_ref().is_some_and(|result| {
                    result.turn.loop_id == loop_id
                        && result.persistence == Some(TurnPersistenceWire::Persisted)
                })
            }))
            && view.transcript.window.items().any(|(_, item)| {
                matches!(
                    item.as_ref(),
                    TranscriptBlock::User(user) if user.loop_id.as_deref() == Some(loop_id)
                ) || matches!(
                    item.as_ref(),
                    TranscriptBlock::Assistant(assistant) if assistant.loop_id == loop_id
                ) || matches!(
                    item.as_ref(),
                    TranscriptBlock::Tool(tool) if tool.loop_id == loop_id
                )
            })
            && !view.transcript.window.items().any(|(_, item)| {
                matches!(
                    item.as_ref(),
                    TranscriptBlock::User(user)
                        if user.loop_id.as_deref() == Some(loop_id)
                            && user.kind == crate::protocol::UserMessageKindWire::Steering
                            && user.text == steer_text
                )
            })
    }

    pub(super) fn mark_history_unconfirmed(view: &mut SessionView) {
        view.read_page = None;
        view.history_read.finish();
        view.event_gap = true;
        view.transcript.complete = false;
        if let Some(live) = view.live.as_mut() {
            live.event_gap = true;
        }
        Self::mark_pending_steers_unconfirmed(view);
    }

    pub(super) fn on_history_response(
        &mut self,
        session_id: &SessionId,
        read: &ReadRequest,
        response: &RpcResponse,
    ) -> Vec<AppCommand> {
        if self.session_pending_deletion(session_id)
            || self.sessions.pending_deletes.contains(session_id)
        {
            return Vec::new();
        }
        let page = match response.parse_session_read() {
            Ok(page) => page,
            Err(error) => {
                self.notice(
                    NoticeLevel::Error,
                    format!("malformed history for {session_id}: {error}"),
                );
                if let Some(view) = self.sessions.known.get_mut(session_id) {
                    Self::mark_history_unconfirmed(view);
                }
                return Vec::new();
            }
        };
        self.continue_read_chain(session_id, read, &page)
    }

    /// Applies one `session.read` page to a session's window and reconciles the
    /// live loop when the chain completes. The chunk assembler lives on the
    /// view so an item spanning pages is never rebuilt from scratch.
    pub(super) fn continue_read_chain(
        &mut self,
        session_id: &SessionId,
        read: &ReadRequest,
        page: &crate::protocol::ReadSessionResult,
    ) -> Vec<AppCommand> {
        // A pending search/prompt jump resumes as soon as its target item
        // window is resident, on both the async-decode and fixture paths.
        let mut commands = self.continue_read_chain_inner(session_id, read, page);
        commands.extend(self.on_search_history_progress(session_id));
        commands
    }

    fn continue_read_chain_inner(
        &mut self,
        session_id: &SessionId,
        read: &ReadRequest,
        page: &crate::protocol::ReadSessionResult,
    ) -> Vec<AppCommand> {
        if self.reload.is_some() {
            return Vec::new();
        }
        if !self.can_send_requests() {
            return Vec::new();
        }
        if self.session_pending_deletion(session_id)
            || self.sessions.pending_deletes.contains(session_id)
        {
            return Vec::new();
        }

        let Some(view) = self.sessions.known.get_mut(session_id) else {
            return Vec::new();
        };

        // A fresh read chain must start from a clean assembler; a re-pin
        // replaces the window rather than merging two generations.
        let mut page_state = view
            .read_page
            .take()
            .unwrap_or_else(|| crate::app::history::ReadPage::new(read.cursor, None, 0));
        page_state.cursor = read.cursor;
        page_state.want_pin = read.pin.clone();
        page_state.window_start = read.window_start;
        page_state.replacement = read.replacement;
        page_state.reconcile = read.reconcile;
        page_state.gap_revision = read.gap_revision;
        if read.replacement && read.cursor == crate::protocol::ReadCursor::start() {
            view.transcript.window.reset();
        }

        // §6.3 step 1: the first read of a fresh window is a one-item probe.
        // It establishes the pin and `total`; the probe item is kept only when
        // the chosen window starts at it, otherwise its assembler is discarded
        // so a stray chunk below the window is never faked as loaded (§6.3
        // step 3).
        if read.probe {
            let pin = page.pin();
            let window_start = pin.total.saturating_sub(crate::protocol::READ_TAIL_ITEMS);
            view.transcript.window.replace_pin(pin);
            view.read_page = None;
            view.transcript.next_cursor = None;
            page_state.window_start = window_start;
            let apply =
                crate::app::history::apply_page(&mut view.transcript.window, &mut page_state, page);
            match apply {
                Ok(crate::app::history::ReadApply::Ok(applied)) => {
                    if let Some(error) = &applied.error {
                        view.read_page = None;
                        Self::mark_history_unconfirmed(view);
                        let message = match error {
                            crate::protocol::ReadError::NonContiguous { expected, .. } => {
                                format!(
                                    "history for {session_id} is not contiguous at item {expected}"
                                )
                            }
                            crate::protocol::ReadError::CursorStalled { item } => {
                                format!("history for {session_id} did not advance from item {item}")
                            }
                            crate::protocol::ReadError::ItemChanged { index } => {
                                format!(
                                    "history for {session_id} changed at an existing item index {index}"
                                )
                            }
                            other => {
                                format!("history for {session_id} is not decodable: {other}")
                            }
                        };
                        self.notice(NoticeLevel::Error, message);
                        return Vec::new();
                    }
                    for (index, total_bytes) in &applied.placeholders {
                        install_history_placeholder(view, *index, *total_bytes);
                    }
                    if self.async_decode && !applied.inserted.is_empty() {
                        page_state.pending_encoded = applied.inserted.iter().cloned().collect();
                        view.read_page = Some(page_state);
                        view.transcript.next_cursor = applied.next;
                        view.transcript.sync_from_window();
                        self.queue_history_decode(session_id);
                        return Vec::new();
                    }
                    for encoded in &applied.inserted {
                        let item = match crate::protocol::read::decode_item(&encoded.data) {
                            Ok(item) => item,
                            Err(detail) => {
                                view.read_page = None;
                                Self::mark_history_unconfirmed(view);
                                self.notice(
                                    NoticeLevel::Error,
                                    format!("history for {session_id} is not decodable: {detail}"),
                                );
                                return Vec::new();
                            }
                        };
                        if let Some(owner) = install_history_item(view, encoded.index, &item) {
                            let bytes = owner_bytes(&owner);
                            view.transcript.window.insert_owner(
                                encoded.index,
                                owner,
                                encoded_item_fingerprint(encoded),
                                bytes,
                            );
                        }
                    }
                    page_state.pending_encoded.clear();
                    page_state.pending_page = None;
                    if applied.explicit_large_item && window_start == 0 {
                        view.read_page = Some(page_state);
                        view.transcript.next_cursor = applied.next;
                        view.transcript.sync_from_window();
                        view.history_read.finish();
                        return Vec::new();
                    }
                    // A probe that already delivered the whole prefix (a short
                    // history) needs no further read.
                    if applied.next.is_none() {
                        view.transcript.sync_from_window();
                        view.history_read.finish();
                        view.read_page = None;
                        let next = Self::finish_read_chain(view, session_id, read);
                        view.recompute_usage_projection();
                        return match next {
                            NextChain::Page | NextChain::Reconcile => {
                                self.request_history(session_id).into_iter().collect()
                            }
                            NextChain::LoopNotContained(loop_id) => {
                                self.notice(
                                    NoticeLevel::Warning,
                                    format!(
                                        "history sync warning: loop {loop_id} not contained in history response"
                                    ),
                                );
                                Vec::new()
                            }
                            NextChain::Done => Vec::new(),
                        };
                    }
                    if window_start == 0 {
                        // The probe item belongs to the window; continue from
                        // the backend's own next cursor under the new pin.
                        view.read_page = Some(page_state);
                        view.transcript.next_cursor = applied.next;
                        view.transcript.sync_from_window();
                        view.history_read.continue_loading();
                        return self.request_history(session_id).into_iter().collect();
                    }
                }
                _ => {
                    view.read_page = None;
                }
            }
            // A long session opens at its tail: read forward from the window
            // start under the fresh pin, never sending a cursor without one.
            let mut next_request = read.clone();
            next_request.cursor = crate::protocol::ReadCursor {
                item: window_start,
                offset: 0,
            };
            next_request.window_start = window_start;
            next_request.replacement = false;
            next_request.probe = false;
            next_request.pin = view.transcript.window.pin().cloned();
            view.transcript.sync_from_window();
            view.history_read.continue_loading();
            return self
                .request_read(session_id, next_request)
                .into_iter()
                .collect();
        }

        let applied = match crate::app::history::apply_page(
            &mut view.transcript.window,
            &mut page_state,
            page,
        ) {
            Err(error) => {
                view.read_page = None;
                Self::mark_history_unconfirmed(view);
                self.notice(
                    NoticeLevel::Error,
                    format!("history for {session_id} is not decodable: {error}"),
                );
                return Vec::new();
            }
            Ok(crate::app::history::ReadApply::Stale(error)) => {
                view.read_page = None;
                // The pinned prefix is gone; do not splice two generations.
                view.event_gap = true;
                view.history_read.finish();
                self.notice(
                    NoticeLevel::Warning,
                    format!("history for {session_id} became stale: {error}; reload to continue"),
                );
                return Vec::new();
            }
            Ok(crate::app::history::ReadApply::Ok(applied)) => applied,
        };

        for (index, total_bytes) in &applied.placeholders {
            install_history_placeholder(view, *index, *total_bytes);
        }
        if self.async_decode && !applied.inserted.is_empty() {
            page_state.pending_encoded = applied.inserted.iter().cloned().collect();
            view.read_page = Some(page_state);
            view.transcript.next_cursor = applied.next;
            view.transcript.sync_from_window();
            self.queue_history_decode(session_id);
            return Vec::new();
        }
        for encoded in &applied.inserted {
            let item = match crate::protocol::read::decode_item(&encoded.data) {
                Ok(item) => item,
                Err(detail) => {
                    view.read_page = None;
                    Self::mark_history_unconfirmed(view);
                    self.notice(
                        NoticeLevel::Error,
                        format!("history for {session_id} is not decodable: {detail}"),
                    );
                    return Vec::new();
                }
            };
            if let Some(owner) = install_history_item(view, encoded.index, &item) {
                let bytes = owner_bytes(&owner);
                view.transcript.window.insert_owner(
                    encoded.index,
                    owner,
                    encoded_item_fingerprint(encoded),
                    bytes,
                );
            }
        }

        page_state.pending_encoded.clear();
        page_state.pending_page = None;
        if let Some(error) = applied.error {
            view.read_page = None;
            Self::mark_history_unconfirmed(view);
            let message = match &error {
                crate::protocol::ReadError::NonContiguous { expected, .. } => {
                    format!("history for {session_id} is not contiguous at item {expected}")
                }
                crate::protocol::ReadError::CursorStalled { item } => {
                    format!("history for {session_id} did not advance from item {item}")
                }
                crate::protocol::ReadError::ItemChanged { index } => {
                    format!("history for {session_id} changed at an existing item index {index}")
                }
                _ => format!("history for {session_id} is not decodable: {error}"),
            };
            self.notice(NoticeLevel::Error, message);
            return Vec::new();
        }

        // Persist the assembler for the next page (it may hold a partial item).
        view.read_page = Some(page_state);
        view.transcript.next_cursor = applied.next;

        if applied.explicit_large_item {
            view.transcript.sync_from_window();
            view.history_read.finish();
            return Vec::new();
        }

        let next = match applied.next {
            Some(_) => {
                view.transcript.sync_from_window();
                view.history_read.continue_loading();
                NextChain::Page
            }
            None => {
                view.transcript.sync_from_window();
                view.history_read.finish();
                let next = Self::finish_read_chain(view, session_id, read);
                view.read_page = None;
                view.recompute_usage_projection();
                next
            }
        };

        match next {
            NextChain::Page | NextChain::Reconcile => {
                self.request_history(session_id).into_iter().collect()
            }
            NextChain::LoopNotContained(loop_id) => {
                self.notice(
                    NoticeLevel::Warning,
                    format!(
                        "history sync warning: loop {loop_id} not contained in history response"
                    ),
                );
                Vec::new()
            }
            NextChain::Done => Vec::new(),
        }
    }

    fn queue_any_pending_decode(&mut self) {
        if self.pending_decode.is_some() || self.decode_in_flight.is_some() {
            return;
        }
        if let Some(session_id) = self.sessions.known.iter().find_map(|(session_id, view)| {
            view.read_page
                .as_ref()
                .filter(|page| !page.pending_encoded.is_empty())
                .map(|_| session_id.clone())
        }) {
            self.queue_history_decode(&session_id);
            return;
        }
        if let Some(turn) = self
            .turn_results
            .iter()
            .find_map(|(turn, window)| (!window.pending_encoded.is_empty()).then(|| turn.clone()))
        {
            self.queue_turn_result_decode(&turn);
            return;
        }
        if self
            .search_scan
            .as_ref()
            .is_some_and(crate::app::search::SearchScan::has_pending_decode)
        {
            self.queue_search_decode();
        }
    }

    pub(super) fn on_history_item_decoded(
        &mut self,
        outcome: crate::jobs::DecodeOutcome,
    ) -> Vec<AppCommand> {
        if self.decode_in_flight.as_ref() != Some(&outcome.identity) {
            // A cancelled/stale completion still consumed its worker slot; it
            // must not touch a newer page or read chain.
            return Vec::new();
        }
        self.decode_in_flight = None;
        if outcome.cancelled {
            self.queue_any_pending_decode();
            return Vec::new();
        }
        let target = outcome.identity.target.clone();
        let commands = match target {
            crate::jobs::DecodeTarget::History { session_id, index } => self
                .finish_history_item_decode(
                    &session_id,
                    outcome.identity.session_epoch,
                    outcome.identity.read_chain,
                    index,
                    outcome.fingerprint,
                    outcome.result,
                ),
            crate::jobs::DecodeTarget::TurnResult { turn, index } => self
                .finish_turn_result_item_decode(
                    &turn,
                    outcome.identity.session_epoch,
                    outcome.identity.read_chain,
                    index,
                    outcome.fingerprint,
                    outcome.result,
                ),
            crate::jobs::DecodeTarget::SearchScan {
                session_id,
                generation,
                index,
            } => self.finish_search_item_decoded(
                &session_id,
                generation,
                index,
                outcome.fingerprint,
                &outcome,
            ),
        };
        self.queue_any_pending_decode();
        commands
    }

    fn finish_history_item_decode(
        &mut self,
        session_id: &SessionId,
        session_epoch: u64,
        read_chain: u64,
        index: usize,
        fingerprint: u64,
        result: Result<RawHistoryItem, String>,
    ) -> Vec<AppCommand> {
        let Some(view) = self.sessions.known.get_mut(session_id) else {
            return Vec::new();
        };
        if view.session_epoch != session_epoch || view.history_query_generation != read_chain {
            return Vec::new();
        }
        let item = match result {
            Ok(item) => item,
            Err(detail) => {
                view.read_page = None;
                Self::mark_history_unconfirmed(view);
                self.notice(
                    NoticeLevel::Error,
                    format!("history for {session_id} is not decodable: {detail}"),
                );
                return Vec::new();
            }
        };
        let (has_more, pending) = {
            let Some(page) = view.read_page.as_mut() else {
                return Vec::new();
            };
            let Some(front) = page.pending_encoded.front() else {
                return Vec::new();
            };
            if front.index != index || encoded_item_fingerprint(front) != fingerprint {
                return Vec::new();
            }
            page.pending_encoded.pop_front();
            let has_more = !page.pending_encoded.is_empty();
            let pending = (!has_more).then(|| page.pending_page.take()).flatten();
            (has_more, pending)
        };
        if let Some(owner) = install_history_item(view, index, &item) {
            let bytes = owner_bytes(&owner);
            view.transcript
                .window
                .insert_owner(index, owner, fingerprint, bytes);
        }
        if has_more {
            let commands = self.on_search_history_progress(session_id);
            self.queue_history_decode(session_id);
            return commands;
        }
        let page_state = view.read_page.take().expect("decode page remains owned");
        let Some(pending) = pending else {
            return Vec::new();
        };
        if let Some(error) = pending.error {
            Self::mark_history_unconfirmed(view);
            self.notice(
                NoticeLevel::Error,
                format!("history for {session_id} is not decodable: {error}"),
            );
            return Vec::new();
        }
        view.transcript.next_cursor = pending.next;
        if pending.explicit_large_item {
            view.transcript.sync_from_window();
            view.history_read.finish();
            if page_state.window_start == 0 {
                view.read_page = Some(page_state);
            }
            return Vec::new();
        }
        let next = match pending.next {
            Some(_) => {
                view.read_page = Some(page_state);
                view.transcript.sync_from_window();
                view.history_read.continue_loading();
                NextChain::Page
            }
            None => {
                view.transcript.sync_from_window();
                view.history_read.finish();
                let read = ReadRequest {
                    cursor: page_state.cursor,
                    pin: page_state.want_pin.clone(),
                    window_start: page_state.window_start,
                    replacement: page_state.replacement,
                    reconcile: page_state.reconcile,
                    probe: false,
                    gap_revision: page_state.gap_revision,
                };
                Self::finish_read_chain(view, session_id, &read)
            }
        };
        view.recompute_usage_projection();
        match next {
            NextChain::Page | NextChain::Reconcile => {
                self.request_history(session_id).into_iter().collect()
            }
            NextChain::LoopNotContained(loop_id) => {
                self.notice(
                    NoticeLevel::Warning,
                    format!(
                        "history sync warning: loop {loop_id} not contained in history response"
                    ),
                );
                Vec::new()
            }
            NextChain::Done => Vec::new(),
        }
    }

    fn finish_turn_result_item_decode(
        &mut self,
        turn: &TurnRef,
        session_epoch: u64,
        read_chain: u64,
        index: usize,
        fingerprint: u64,
        result: Result<RawHistoryItem, String>,
    ) -> Vec<AppCommand> {
        let current_epoch = self
            .sessions
            .known
            .get(&turn.session_id)
            .map_or(0, |view| view.session_epoch);
        let Some(window) = self.turn_results.get_mut(turn) else {
            return Vec::new();
        };
        if current_epoch != session_epoch || window.read_chain != read_chain {
            return Vec::new();
        }
        let item = match result {
            Ok(item) => item,
            Err(detail) => {
                if let Some(view) = self.sessions.known.get_mut(&turn.session_id) {
                    view.result_confirmation = ResultConfirmation::NeedsRead;
                    Self::mark_pending_steers_unconfirmed(view);
                }
                self.notice(
                    NoticeLevel::Warning,
                    format!(
                        "result read-back for {}/{} is not decodable: {detail}",
                        turn.session_id, turn.loop_id
                    ),
                );
                return Vec::new();
            }
        };
        if window
            .pending_encoded
            .front()
            .is_none_or(|encoded| encoded.index != index)
        {
            return Vec::new();
        }
        match window.apply_decoded(index, item, fingerprint) {
            Ok(false) => {
                self.queue_turn_result_decode(turn);
                Vec::new()
            }
            Ok(true) => self.finish_turn_result_page(turn),
            Err(error) => {
                if let Some(view) = self.sessions.known.get_mut(&turn.session_id) {
                    view.result_confirmation = ResultConfirmation::NeedsRead;
                    Self::mark_pending_steers_unconfirmed(view);
                }
                self.notice(
                    NoticeLevel::Warning,
                    format!(
                        "result read-back for {}/{} is not decodable ({error})",
                        turn.session_id, turn.loop_id
                    ),
                );
                Vec::new()
            }
        }
    }

    fn finish_turn_result_page(&mut self, turn: &TurnRef) -> Vec<AppCommand> {
        let Some(page) = self
            .turn_results
            .get_mut(turn)
            .and_then(|window| window.pending_page.take())
        else {
            return Vec::new();
        };
        let (complete, next_cursor, explicit_large_item) = self
            .turn_results
            .get(turn)
            .map(|window| {
                (
                    window.complete,
                    (!window.complete).then_some(window.cursor),
                    window.explicit_large_item,
                )
            })
            .unwrap_or((false, None, false));
        if !complete && !explicit_large_item {
            if let Some(cursor) = next_cursor {
                return self
                    .request_turn_result_page(turn.clone(), cursor)
                    .into_iter()
                    .collect();
            }
        }
        if explicit_large_item {
            self.notice(
                NoticeLevel::Info,
                format!(
                    "result for {}/{} contains a large item; read it explicitly to continue",
                    turn.session_id, turn.loop_id
                ),
            );
            return Vec::new();
        }
        if page.availability == TurnAvailability::Pending {
            if let Some(view) = self.sessions.known.get_mut(&turn.session_id) {
                if Self::wait_targets_current_turn(view, turn) {
                    view.result_confirmation = ResultConfirmation::NeedsRead;
                    if let Some(live) = view.live.as_mut() {
                        live.waiting = true;
                    }
                }
            }
            if !self.pending_wait_for(turn) {
                return self.request_wait(turn.clone()).into_iter().collect();
            }
            return Vec::new();
        }
        let Some(outcome) = page.outcome else {
            return Vec::new();
        };
        let result = crate::protocol::TurnResultViewWire {
            turn: turn.clone(),
            outcome,
            usage: page.usage,
            requests: page.requests,
            tool_rounds: page.tool_rounds,
            final_config_revision: page.final_config_revision,
            persistence: page.persistence,
            accepted_at: None,
            completed_at: page.completed_at,
        };
        self.retain_result_summary(result.clone());
        if result.persistence.is_none() {
            if let Some(view) = self.sessions.known.get_mut(&turn.session_id) {
                view.result_confirmation = ResultConfirmation::NeedsRead;
            }
            self.notice(
                NoticeLevel::Warning,
                format!(
                    "result read-back for {}/{} omitted persistence; outcome remains unconfirmed",
                    turn.session_id, turn.loop_id
                ),
            );
            return Vec::new();
        }
        if result.persistence == Some(TurnPersistenceWire::Failed) {
            self.project_turn_result_into_live(turn, &result);
            self.retain_failed_result_on_view(turn, &result);
            self.notice(
                NoticeLevel::Error,
                format!(
                    "Turn {}/{} completed but persistence is still unconfirmed.",
                    turn.session_id, turn.loop_id
                ),
            );
            return Vec::new();
        }
        self.reconcile_after_wait(turn)
    }

    /// Reconciles the live loop once the read chain is complete (spec §6.4,
    /// §7.1). Returns the next chain step, if any.
    pub(super) fn finish_read_chain(
        view: &mut SessionView,
        session_id: &SessionId,
        read: &ReadRequest,
    ) -> NextChain {
        let live_loop_id = view
            .live
            .as_ref()
            .and_then(|live| live.reference.as_ref())
            .map(|r| r.loop_id.clone());

        let raw_items_contain_loop = live_loop_id.as_ref().is_some_and(|id| {
            view.transcript
                .window
                .items()
                .any(|(_, item)| match item.as_ref() {
                    TranscriptBlock::User(user) => user.loop_id.as_deref() == Some(id.as_str()),
                    TranscriptBlock::Assistant(assistant) => assistant.loop_id == *id,
                    TranscriptBlock::Tool(tool) => tool.loop_id == *id,
                    _ => false,
                })
        });

        let loop_contained_in_history = match &live_loop_id {
            Some(id) => view.transcript.blocks.iter().any(|b| match b.as_ref() {
                TranscriptBlock::User(u) => !u.pending && u.loop_id.as_deref() == Some(id),
                TranscriptBlock::Assistant(a) => a.loop_id.as_str() == id.as_str(),
                TranscriptBlock::Tool(t) => t.loop_id.as_str() == id.as_str(),
                _ => false,
            }),
            None => false,
        };

        let same_turn_persisted = match &live_loop_id {
            Some(id) => {
                view.last_result.as_ref().is_some_and(|r| {
                    r.persistence == Some(TurnPersistenceWire::Persisted) && r.turn.loop_id == *id
                }) || view
                    .live
                    .as_ref()
                    .and_then(|l| l.last_result.as_ref())
                    .is_some_and(|r| {
                        r.persistence == Some(TurnPersistenceWire::Persisted)
                            && r.turn.loop_id == *id
                    })
            }
            None => true,
        };

        let turn_satisfied = if live_loop_id.is_some() {
            same_turn_persisted && raw_items_contain_loop
        } else {
            view.live.is_none()
        };

        let gap_rev_matches = read.gap_revision == view.gap_revision;
        let needs_gap_reconcile = view.unsaved_loop.is_none()
            && view.event_gap
            && !gap_rev_matches
            && view
                .live
                .as_ref()
                .is_none_or(|live| live.last_result.is_some());

        if view.unsaved_loop.is_none() && view.event_gap && turn_satisfied && gap_rev_matches {
            view.event_gap = false;
        }

        view.history_read.finish();

        let loop_id = live_loop_id.as_deref();
        let mut persisted_steers: Vec<String> = view
            .transcript
            .blocks
            .iter()
            .filter_map(|block| match block.as_ref() {
                TranscriptBlock::User(user)
                    if user.kind == UserMessageKindWire::Steering
                        && loop_id
                            .is_some_and(|loop_id| user.loop_id.as_deref() == Some(loop_id)) =>
                {
                    Some(user.text.clone())
                }
                _ => None,
            })
            .collect();
        let blocked = view.is_blocked();
        let persistence_unconfirmed = view.unsaved_loop.is_some();
        let terminal = view
            .live
            .as_ref()
            .is_some_and(|live| live.last_result.is_some());
        if let Some(live) = view.live.as_mut() {
            for steer in &mut live.pending_steers {
                if matches!(
                    steer.state,
                    PendingSteerState::Sending
                        | PendingSteerState::Queued
                        | PendingSteerState::Unconfirmed
                ) {
                    if let Some(position) =
                        persisted_steers.iter().position(|text| text == &steer.text)
                    {
                        persisted_steers.remove(position);
                        steer.state = if persistence_unconfirmed {
                            PendingSteerState::Unconfirmed
                        } else {
                            PendingSteerState::Persisted
                        };
                    } else if steer.state == PendingSteerState::Queued && terminal {
                        steer.state = if blocked {
                            PendingSteerState::Unconfirmed
                        } else {
                            PendingSteerState::NotRecorded
                        };
                    } else if steer.state == PendingSteerState::Sending && terminal {
                        steer.state = PendingSteerState::Unconfirmed;
                    }
                }
            }
        }
        view.applied_steers.retain(|applied| {
            if let Some(position) = persisted_steers
                .iter()
                .position(|text| text == &applied.text)
            {
                persisted_steers.remove(position);
                false
            } else {
                true
            }
        });

        if view.unsaved_loop.is_none()
            && !view.is_blocked()
            && loop_contained_in_history
            && view
                .live
                .as_ref()
                .is_some_and(|live| live.last_result.is_some())
        {
            if let Some(live) = view.live.take() {
                let current_loop = live
                    .reference
                    .as_ref()
                    .map(|r| r.loop_id.clone())
                    .unwrap_or_default();
                for steer in live.pending_steers {
                    view.completed_steers
                        .push(crate::state::session::CompletedSteerNotice {
                            session_id: session_id.clone(),
                            loop_id: current_loop.clone(),
                            local_id: steer.local_id,
                            text: steer.text,
                            state: steer.state,
                            accepted_at: steer.accepted_at,
                        });
                }
            }
        }

        if needs_gap_reconcile {
            view.history_read.take_pending();
            view.history_read.begin(HistoryTrigger::Gap);
            NextChain::Reconcile
        } else if view.history_read.take_pending() == Some(HistoryTrigger::PostWait) {
            if !loop_contained_in_history && view.live.is_some() {
                view.history_read.begin(HistoryTrigger::PostWait);
                NextChain::Reconcile
            } else {
                NextChain::Done
            }
        } else if !loop_contained_in_history
            && view.live.as_ref().is_some_and(|l| l.last_result.is_some())
        {
            NextChain::LoopNotContained(live_loop_id.unwrap_or_default())
        } else {
            NextChain::Done
        }
    }

    pub(super) fn turn_result_view(
        page: &crate::protocol::read::TurnResultPage,
    ) -> Option<crate::protocol::TurnResultViewWire> {
        Some(crate::protocol::TurnResultViewWire {
            turn: page.turn.clone(),
            outcome: page.outcome.clone()?,
            usage: page.usage,
            requests: page.requests,
            tool_rounds: page.tool_rounds,
            final_config_revision: page.final_config_revision,
            persistence: page.persistence,
            // `turn.result.completed_at` is not prompt acceptance time.
            accepted_at: None,
            completed_at: page.completed_at.clone(),
        })
    }

    pub(super) fn retain_failed_result_on_view(
        &mut self,
        turn: &TurnRef,
        result: &crate::protocol::TurnResultViewWire,
    ) {
        self.project_turn_result_into_live(turn, result);
        let Some(view) = self.sessions.known.get_mut(&turn.session_id) else {
            return;
        };
        if !Self::wait_targets_current_turn(view, turn) {
            return;
        }
        // The read-back itself reported `persistence = failed`: the failure is
        // known, so the outcome is Confirmed and only the save is unconfirmed.
        view.result_confirmation = ResultConfirmation::Confirmed;
        if let Some(state) = view.state.as_mut() {
            state.status = SessionStatusWire::Blocked;
            state.active_loop = None;
            state.block_reason = Some(crate::protocol::SessionBlockReasonWire::Persistence);
        } else {
            view.state = Some(SessionStateWire {
                session_id: turn.session_id.clone(),
                status: SessionStatusWire::Blocked,
                active_loop: None,
                block_reason: Some(crate::protocol::SessionBlockReasonWire::Persistence),
                compaction: None,
            });
        }
        if view.unsaved_loop.is_none() {
            if let Some(live) = view.live.as_ref() {
                view.unsaved_loop = Some(UnsavedLoop {
                    turn: turn.clone(),
                    user_text: live.user_text.clone(),
                    requests: live.requests.clone(),
                    result: Some(result.clone()),
                    event_gap: live.event_gap,
                });
            }
        } else if let Some(unsaved) = view.unsaved_loop.as_mut() {
            unsaved.result = Some(result.clone());
        }
        Self::mark_pending_steers_unconfirmed(view);
        view.recompute_usage_projection();
    }

    /// Rebuilds the readable provisional body from the authoritative
    /// turn-local result window. This is deliberately kept in `LiveLoop` only
    /// as a display bridge; the local item indexes never enter the session
    /// history window. In particular, a persistence failure with dropped live
    /// deltas still exposes the complete retained report before the user
    /// closes or reopens the session.
    pub(super) fn project_turn_result_into_live(
        &mut self,
        turn: &TurnRef,
        result: &crate::protocol::TurnResultViewWire,
    ) {
        let Some(window) = self.turn_results.get(turn) else {
            return;
        };
        if !window.complete {
            return;
        }
        let (local_submission, fallback_text, cancel_requested, event_gap, pending_steers) = self
            .sessions
            .known
            .get(&turn.session_id)
            .and_then(|view| view.live.as_ref())
            .map(|live| {
                (
                    live.local_submission,
                    live.user_text.clone(),
                    live.cancel_requested,
                    live.event_gap,
                    live.pending_steers.clone(),
                )
            })
            .unwrap_or((
                LocalSubmissionId(u64::MAX),
                String::new(),
                false,
                false,
                Vec::new(),
            ));
        let mut projected =
            live_loop_from_turn_result(turn, window, local_submission, fallback_text);
        projected.waiting = true;
        projected.cancel_requested = cancel_requested;
        projected.event_gap = event_gap;
        projected.pending_steers = pending_steers;
        projected.last_result = Some(result.clone());
        if let Some(view) = self.sessions.known.get_mut(&turn.session_id) {
            view.live = Some(projected);
            view.transcript.invalidate();
        }
    }

    /// Settles an authoritative `turn.result` read-back (spec §7.2). Pages
    /// are assembled in a turn-local window; only the final retained summary
    /// may be projected into a SessionView. A late response can therefore
    /// survive reload/reopen without confusing its local item indexes with the
    /// session-global indexes returned by `session.read`.
    pub(super) fn on_turn_result_response(
        &mut self,
        turn: &TurnRef,
        response: &RpcResponse,
    ) -> Vec<AppCommand> {
        let page = match response.parse_turn_result_page() {
            Ok(page) => page,
            Err(error) => {
                if let Some(view) = self.sessions.known.get_mut(&turn.session_id) {
                    Self::mark_pending_steers_unconfirmed(view);
                }
                self.notice(
                    NoticeLevel::Warning,
                    format!(
                        "result read-back for {}/{} failed ({error}); outcome remains unconfirmed",
                        turn.session_id, turn.loop_id
                    ),
                );
                return Vec::new();
            }
        };
        if page.turn != *turn {
            self.connection_terminated("turn.result response does not match the requested turn");
            return Vec::new();
        }

        let apply_result = if self.async_decode {
            self.turn_results
                .entry(turn.clone())
                .or_insert_with(|| crate::app::history::TurnResultWindow::new(turn.clone()))
                .stage_page(&page)
        } else {
            self.turn_results
                .entry(turn.clone())
                .or_insert_with(|| crate::app::history::TurnResultWindow::new(turn.clone()))
                .apply_page(&page)
        };
        if let Err(error) = apply_result {
            if let Some(view) = self.sessions.known.get_mut(&turn.session_id) {
                view.result_confirmation = ResultConfirmation::NeedsRead;
                Self::mark_pending_steers_unconfirmed(view);
            }
            self.notice(
                NoticeLevel::Warning,
                format!(
                    "result read-back for {}/{} is not decodable ({error}); outcome remains unconfirmed",
                    turn.session_id, turn.loop_id
                ),
            );
            return Vec::new();
        }

        if let Some(result) = Self::turn_result_view(&page) {
            self.retain_result_summary(result.clone());
            if let Some(view) = self.sessions.known.get_mut(&turn.session_id) {
                if Self::wait_targets_current_turn(view, turn) {
                    view.last_result = Some(result.clone());
                    // Any reported persistence (including `failed`) is
                    // evidence about the outcome; a page that omits it is not.
                    view.result_confirmation = if result.persistence.is_none() {
                        ResultConfirmation::NeedsRead
                    } else {
                        ResultConfirmation::Confirmed
                    };
                    if let Some(live) = view.live.as_mut() {
                        if live.reference.as_ref() == Some(turn) {
                            live.last_result = Some(result.clone());
                            live.waiting = true;
                        }
                    }
                    view.recompute_usage_projection();
                }
            }
        }

        if self.async_decode
            && self
                .turn_results
                .get(turn)
                .is_some_and(|window| !window.pending_encoded.is_empty())
        {
            self.queue_turn_result_decode(turn);
            return Vec::new();
        }

        let (complete, next_cursor, explicit_large_item) = self
            .turn_results
            .get(turn)
            .map(|window| {
                (
                    window.complete,
                    (!window.complete).then_some(window.cursor),
                    window.explicit_large_item,
                )
            })
            .unwrap_or((false, None, false));
        if !complete && !explicit_large_item {
            if let Some(cursor) = next_cursor {
                return self
                    .request_turn_result_page(turn.clone(), cursor)
                    .into_iter()
                    .collect();
            }
        }

        if explicit_large_item {
            self.notice(
                NoticeLevel::Info,
                format!(
                    "result for {}/{} contains a large item; read it explicitly to continue",
                    turn.session_id, turn.loop_id
                ),
            );
            return Vec::new();
        }

        if page.availability == TurnAvailability::Pending {
            if let Some(view) = self.sessions.known.get_mut(&turn.session_id) {
                if Self::wait_targets_current_turn(view, turn) {
                    view.result_confirmation = ResultConfirmation::NeedsRead;
                    if let Some(live) = view.live.as_mut() {
                        live.waiting = true;
                    }
                }
            }
            if !self.pending_wait_for(turn) {
                return self.request_wait(turn.clone()).into_iter().collect();
            }
            return Vec::new();
        }

        let Some(result) = Self::turn_result_view(&page) else {
            return Vec::new();
        };
        if result.persistence.is_none() {
            if let Some(view) = self.sessions.known.get_mut(&turn.session_id) {
                view.result_confirmation = ResultConfirmation::NeedsRead;
            }
            self.notice(
                NoticeLevel::Warning,
                format!(
                    "result read-back for {}/{} omitted persistence; outcome remains unconfirmed",
                    turn.session_id, turn.loop_id
                ),
            );
            return Vec::new();
        }
        if result.persistence == Some(TurnPersistenceWire::Failed) {
            self.project_turn_result_into_live(turn, &result);
            self.retain_failed_result_on_view(turn, &result);
            self.notice(
                NoticeLevel::Error,
                format!(
                    "Turn {}/{} completed but persistence is still unconfirmed.",
                    turn.session_id, turn.loop_id
                ),
            );
            return Vec::new();
        }
        self.reconcile_after_wait(turn)
    }

    pub(super) fn reconcile_after_wait(&mut self, turn: &TurnRef) -> Vec<AppCommand> {
        if self.reload.is_some() {
            if let Some(view) = self.sessions.known.get_mut(&turn.session_id) {
                view.history_read.defer(HistoryTrigger::PostWait);
            }
            return Vec::new();
        }
        if !self.can_send_requests() {
            return Vec::new();
        }
        // Closed or not-loaded session view must not issue session.state or session.read.
        // Wait itself has already recorded the result and kept live temporarily visible.
        if let Some(view) = self.sessions.known.get(&turn.session_id) {
            if !view.info.loaded || view.closing {
                return Vec::new();
            }
        } else {
            return Vec::new();
        }
        let mut commands = vec![self.request_session_state(&turn.session_id)];
        let pending_history = self.pending_history(&turn.session_id);
        let fetch = {
            let Some(view) = self.sessions.known.get_mut(&turn.session_id) else {
                return commands;
            };
            if view.history_read.is_loading() || pending_history {
                // If a history fetch is already in flight, flag that a post-wait
                // reconcile is required once the in-flight fetch completes (spec scenario B).
                view.history_read.defer(HistoryTrigger::PostWait);
                false
            } else {
                view.history_read.begin(HistoryTrigger::PostWait);
                true
            }
        };
        if fetch {
            commands.extend(self.request_history(&turn.session_id));
        }
        commands
    }

    /// A read-back request could not be issued at all: a live loop still needs
    /// a read (the chain retries when the transport returns), while a retired
    /// loop can only be settled by a fresh read after reconnecting.
    pub(super) fn mark_result_read_failed(&mut self, session_id: &SessionId) {
        if let Some(view) = self.sessions.known.get_mut(session_id) {
            view.result_confirmation = if view.live.is_some() {
                ResultConfirmation::NeedsRead
            } else {
                ResultConfirmation::Unknown
            };
            Self::mark_pending_steers_unconfirmed(view);
        }
    }

    pub(super) fn start_gap_reconcile(&mut self, session_id: &SessionId) -> Vec<AppCommand> {
        if self.reload.is_some() {
            return Vec::new();
        }
        if !self.can_send_requests() {
            return Vec::new();
        }
        if self.session_pending_deletion(session_id)
            || self.sessions.pending_deletes.contains(session_id)
            || self.sessions.closed.contains(session_id)
        {
            return Vec::new();
        }
        let history_pending = self.pending_history(session_id);
        let (state_needed, history_needed, defer_history) = {
            let Some(view) = self.sessions.known.get(session_id) else {
                return Vec::new();
            };
            if !view.event_gap && !view.history_read.post_wait_pending() {
                return Vec::new();
            }
            if view.closing {
                return Vec::new();
            }
            let state_needed = (view.state.is_none() || view.steer_state_unconfirmed)
                && view.latest_state_query.is_none();
            (
                state_needed,
                !view.history_read.is_loading()
                    && !view.history_read.is_reconciling()
                    && !history_pending,
                view.live.is_some() || view.unsaved_loop.is_some(),
            )
        };
        let mut commands = Vec::new();
        if state_needed {
            commands.push(self.request_session_state(session_id));
        }
        if history_needed && !defer_history {
            if let Some(view) = self.sessions.known.get_mut(session_id) {
                view.history_read.begin(HistoryTrigger::Gap);
            }
            commands.extend(self.request_history(session_id));
        }
        commands
    }

    pub(super) fn mark_gap(&mut self, meta: &EventMetaWire) {
        if meta.dropped_before == 0 {
            return;
        }
        if self.session_pending_deletion(&meta.session_id)
            || self.sessions.pending_deletes.contains(&meta.session_id)
        {
            return;
        }
        self.fence_pending_session_state(&meta.session_id);
        if let Some(view) = self.sessions.known.get_mut(&meta.session_id) {
            view.event_gap = true;
            view.gap_revision = view.gap_revision.wrapping_add(1);
            if view.live.as_ref().is_some_and(|live| {
                meta.loop_id.as_ref().is_none_or(|loop_id| {
                    live.reference
                        .as_ref()
                        .is_none_or(|reference| reference.loop_id == *loop_id)
                })
            }) {
                if let Some(live) = view.live.as_mut() {
                    live.event_gap = true;
                }
            }
        }
    }

    /// Legacy entry point used by presentation recovery: cache the receipt,
    /// then apply whatever the ACK indexes now cover.
    pub(super) fn reconcile_steer_receipt(
        &mut self,
        session_id: &SessionId,
        loop_id: &str,
        request_index: u32,
        applied_count: u64,
    ) {
        let turn = TurnRef {
            session_id: session_id.clone(),
            loop_id: loop_id.to_owned(),
        };
        self.on_steer_progress(&turn, request_index, applied_count);
    }
}

impl App {
    /// Enforces the all-drafts budget (spec §21). Runs only for a pass that
    /// changed a draft, trims undo/redo/paste-history capacity in one step and
    /// reports a single warning while the over-budget condition persists.
    pub(crate) fn enforce_draft_budget(&mut self) -> usize {
        self.enforce_draft_budget_with(crate::limits::COMPOSER_ALL_DRAFTS_BYTES)
    }

    /// Budget-parameterized form used by tests; production always passes
    /// [`crate::limits::COMPOSER_ALL_DRAFTS_BYTES`].
    pub fn enforce_draft_budget_with(&mut self, budget: usize) -> usize {
        // One measurement per pass. `draft_bytes` only sums cached lengths,
        // paste metadata and recalled messages; it never joins a buffer.
        let before = self.draft_bytes();
        if before <= budget {
            self.draft_budget_warned = false;
            return 0;
        }
        // One step: drop every composer to the smallest undo capacity, then
        // report only if the draft text itself still exceeds the budget.
        for view in self.sessions.known.values_mut() {
            view.composer.set_undo_capacity(1);
        }
        self.composer.set_undo_capacity(1);
        let retained = self.draft_bytes();
        if retained > budget {
            if !self.draft_budget_warned {
                self.draft_budget_warned = true;
                self.notice(
                    NoticeLevel::Warning,
                    format!(
                        "draft budget: {} KiB of un-sent text is retained across sessions; send or discard a draft to free space (input is refused until then)",
                        retained / 1024
                    ),
                );
            }
        } else {
            self.draft_budget_warned = false;
        }
        before.saturating_sub(retained)
    }
}

impl App {
    /// Retained decoded history bytes across every session.
    pub(crate) fn history_body_bytes(&self) -> usize {
        self.sessions
            .known
            .values()
            .map(|view| view.transcript.window.bytes())
            .sum()
    }

    pub(crate) fn layout_cache_bytes(&self) -> usize {
        let cached = self
            .sessions
            .known
            .values()
            .filter_map(|view| view.transcript.render_cache.as_ref())
            .map(|cache| cache.retained_bytes())
            .sum::<usize>();
        cached
            + self
                .layout_partial
                .as_ref()
                .map_or(0, |(_, layout)| layout.retained_bytes())
    }

    pub(crate) fn enforce_layout_budget(&mut self) -> usize {
        let mut total = self.layout_cache_bytes();
        if total <= crate::limits::LAYOUT_CACHE_BYTES {
            return 0;
        }
        let mut released = 0;
        if let Some((_, layout)) = self.layout_partial.take() {
            let bytes = layout.retained_bytes();
            self.prepared_conversation = None;
            total = total.saturating_sub(bytes);
            released += bytes;
            if total <= crate::limits::LAYOUT_CACHE_BYTES {
                return released;
            }
        }
        let active = self.sessions.active.clone();
        while total > crate::limits::LAYOUT_CACHE_BYTES {
            let victim = self
                .sessions
                .known
                .iter()
                .filter(|(_, view)| view.transcript.render_cache.is_some())
                .max_by_key(|(id, view)| {
                    let active_rank = usize::from(Some(id.as_str()) == active.as_deref());
                    (
                        usize::MAX - active_rank,
                        view.transcript
                            .render_cache
                            .as_ref()
                            .map_or(0, |cache| cache.retained_bytes()),
                    )
                })
                .map(|(id, _)| id.clone());
            let Some(id) = victim else {
                break;
            };
            let bytes = self.sessions.known[&id]
                .transcript
                .render_cache
                .as_ref()
                .map_or(0, |cache| cache.retained_bytes());
            if let Some(view) = self.sessions.known.get_mut(&id) {
                view.transcript.render_cache = None;
                view.transcript.invalidate();
            }
            self.prepared_conversation = None;
            total = total.saturating_sub(bytes);
            released += bytes;
        }
        released
    }

    /// Enforces the global history body budget (spec §21). Background
    /// sessions are evicted before the active one, and the protected tail is
    /// the viewport plus its neighbourhood, never the whole session. Work is
    /// capped per pass so `App::update` cannot stall; the next event
    /// continues where this one stopped.
    pub(crate) fn enforce_history_budget(&mut self) -> usize {
        self.enforce_history_budget_with(crate::limits::HISTORY_BODY_BYTES)
    }

    pub(crate) fn enforce_history_budget_with(&mut self, budget: usize) -> usize {
        let mut total = self.history_body_bytes();
        crate::perf::set(crate::perf::Counter::HistoryBodyBytes, total as u64);
        if total <= budget {
            return 0;
        }
        let active = self.sessions.active.clone();
        let active_durable_offset = self
            .prepared_conversation
            .as_ref()
            .filter(|prepared| prepared.session_id.as_deref() == active.as_deref())
            .map_or((0, 0), |prepared| {
                (prepared.header_rows(), prepared.durable_skip)
            });
        let mut released = 0;
        for _ in 0..crate::limits::HISTORY_EVICTIONS_PER_PASS {
            if total <= budget {
                break;
            }
            // Most-retained session wins; the active session is ranked last so
            // background sessions give up their bodies first, and sessions
            // whose protected tail leaves nothing to evict are skipped.
            let victim = self
                .sessions
                .known
                .iter()
                .filter(|(_, view)| view.transcript.window.bytes() > 0)
                .filter(|(id, view)| {
                    let is_active = Some(id.as_str()) == active.as_deref();
                    let protect = if is_active {
                        crate::limits::HISTORY_PROTECT_TAIL_ITEMS
                    } else {
                        crate::limits::HISTORY_PROTECT_TAIL_ITEMS_BACKGROUND
                    };
                    let (header_rows, durable_skip) = if is_active {
                        active_durable_offset
                    } else {
                        (0, 0)
                    };
                    let ranges = Self::history_protection_ranges(
                        view,
                        self.viewport,
                        protect,
                        is_active,
                        header_rows,
                        durable_skip,
                    );
                    view.transcript
                        .window
                        .items()
                        .any(|(index, _)| !ranges.iter().any(|range| range.contains(index)))
                })
                .max_by_key(|(id, view)| {
                    let active_rank = usize::from(Some(id.as_str()) == active.as_deref());
                    (usize::MAX - active_rank, view.transcript.window.bytes())
                })
                .map(|(id, _)| id.clone());
            let Some(id) = victim else {
                break;
            };
            let is_active = Some(id.as_str()) == active.as_deref();
            let protect = if is_active {
                crate::limits::HISTORY_PROTECT_TAIL_ITEMS
            } else {
                crate::limits::HISTORY_PROTECT_TAIL_ITEMS_BACKGROUND
            };
            let (header_rows, durable_skip) = if is_active {
                active_durable_offset
            } else {
                (0, 0)
            };
            let ranges = Self::history_protection_ranges(
                &self.sessions.known[&id],
                self.viewport,
                protect,
                is_active,
                header_rows,
                durable_skip,
            );
            let evicted = self
                .sessions
                .known
                .get_mut(&id)
                .expect("victim session exists")
                .transcript
                .window
                .evict_farthest_entry(&ranges);
            match evicted {
                Some((index, bytes)) => {
                    if let Some(view) = self.sessions.known.get_mut(&id) {
                        view.transcript
                            .blocks_mut()
                            .retain(|block| block.index() != Some(index));
                        view.transcript.invalidate();
                    }
                    self.prepared_conversation = None;
                    released += bytes;
                    total = total.saturating_sub(bytes);
                }
                None => break,
            }
        }
        crate::perf::set(crate::perf::Counter::HistoryBodyBytes, total as u64);
        released
    }
}

#[cfg(test)]
mod budget_tests {
    use super::*;
    use crate::theme::ThemeKind;

    fn item(bytes: usize) -> RawHistoryItem {
        RawHistoryItem {
            item: serde_json::from_value(serde_json::json!({
                "type": "user",
                "data": {
                    "loop_id": "lup_1",
                    "kind": "prompt",
                    "input": {"text": "x".repeat(bytes)}
                }
            }))
            .expect("valid runtime user item"),
            timestamp: None,
        }
    }

    fn window_with(count: usize, bytes: usize) -> HistoryWindow {
        let mut window = HistoryWindow::default();
        for index in 0..count {
            window.insert(index, item(bytes));
        }
        window
    }

    /// Eviction removes the oldest unprotected items, keeps the protected
    /// tail, accounts the released bytes, and reopens the evicted indexes as
    /// real gaps so a later read re-fetches them.
    #[test]
    fn eviction_releases_body_bytes_and_reopens_the_loaded_range() {
        let mut window = window_with(8, 100);
        assert_eq!(window.bytes(), 800);
        assert_eq!(window.loaded_ranges().to_vec(), vec![0..8]);

        let released = window.evict_to_budget(300, 6);
        assert_eq!(released, 500, "the five oldest items are released");
        assert_eq!(window.bytes(), 300);
        assert_eq!(window.loaded_ranges().to_vec(), vec![5..8]);
        assert_eq!(
            window.confirmed_prefix(),
            0,
            "the prefix restarts at the gap"
        );
        assert!(window.item(7).is_some(), "the protected tail stays");
        assert!(window.item(0).is_none());
    }

    #[test]
    fn evicting_an_owner_releases_the_last_arc_body_reference() {
        let owner = Arc::new(TranscriptBlock::User(UserBlock {
            index: Some(0),
            loop_id: Some("loop".to_owned()),
            kind: crate::protocol::UserMessageKindWire::Prompt,
            text: "retained body".to_owned(),
            pending: false,
        }));
        let weak = Arc::downgrade(&owner);
        let mut window = HistoryWindow::default();
        window.insert_owner(0, Arc::clone(&owner), 7, owner_bytes(&owner));
        drop(owner);
        assert!(weak.upgrade().is_some(), "the window owns the body");
        assert!(window.bytes() > 0);
        let bytes = window.bytes();
        assert_eq!(window.evict_oldest(1), Some(bytes));
        assert!(weak.upgrade().is_none(), "eviction releases the body owner");
        assert_eq!(window.bytes(), 0);
    }

    #[test]
    fn eviction_never_removes_the_protected_tail() {
        let mut window = window_with(4, 100);
        let released = window.evict_to_budget(0, 1);
        assert_eq!(released, 100);
        assert_eq!(window.bytes(), 300);
        assert_eq!(window.loaded_ranges().to_vec(), vec![1..4]);
        assert_eq!(
            window.evict_oldest(1),
            None,
            "the protected tail is bounded"
        );
    }

    /// The app-level pass evicts background sessions before the active one
    /// and leaves the active protected tail alone.
    #[test]
    fn app_budget_evicts_background_first_and_protects_the_active_tail() {
        let mut app = crate::ui::testapp::open_with(
            ThemeKind::Dark,
            "ses_1",
            Some("Active"),
            "high",
            Vec::new(),
        );
        crate::ui::testapp::open_session(&mut app, "ses_2");
        app.sessions.active = Some("ses_1".to_owned());
        for (session, count) in [("ses_1", 100usize), ("ses_2", 10usize)] {
            let window = &mut app
                .sessions
                .known
                .get_mut(session)
                .unwrap()
                .transcript
                .window;
            for index in 0..count {
                window.insert(index, item(100));
            }
        }
        assert_eq!(app.history_body_bytes(), 11_000);

        let released = app.enforce_history_budget_with(6_500);
        assert_eq!(released, 4_500, "background first, then the active oldest");
        assert!(app.history_body_bytes() <= 6_500, "the budget is enforced");
        let active_window = &app.sessions.known["ses_1"].transcript.window;
        assert!(
            active_window.item(99).is_some(),
            "the active viewport tail is protected"
        );
        assert!(
            active_window.item(0).is_none(),
            "the active session is not pinned wholesale"
        );
        let background_window = &app.sessions.known["ses_2"].transcript.window;
        assert_eq!(
            background_window.bytes(),
            0,
            "the background session gave up its bodies first"
        );
    }

    #[test]
    fn active_head_browse_releases_the_far_suffix_within_the_global_budget() {
        let mut app = crate::ui::testapp::open_with(
            ThemeKind::Dark,
            "ses_1",
            Some("Active"),
            "high",
            Vec::new(),
        );
        app.sessions.active = Some("ses_1".to_owned());
        let window = &mut app
            .sessions
            .known
            .get_mut("ses_1")
            .unwrap()
            .transcript
            .window;
        for index in 0..200 {
            window.insert(index, item(1_000));
        }
        app.viewport = (0, 6);
        app.active_session_mut().unwrap().scroll.follow_tail = false;
        app.active_session_mut().unwrap().scroll.offset = 0;

        app.enforce_history_budget_with(80_000);

        assert!(app.history_body_bytes() <= 80_000);
        assert!(
            app.sessions.known["ses_1"]
                .transcript
                .window
                .item(0)
                .is_some()
        );
        assert!(
            app.sessions.known["ses_1"]
                .transcript
                .window
                .item(199)
                .is_none(),
            "head browsing must release far suffix bodies"
        );
    }

    #[test]
    fn layout_eviction_releases_the_last_prepared_layout_owner() {
        use crate::state::view::{
            ConversationLayout, DurableCacheKey, LayoutKey, SectionId, SectionKind, SectionLayout,
        };
        use ratatui::text::{Line, Span};

        let mut app = crate::ui::testapp::open_with(
            ThemeKind::Dark,
            "ses_1",
            Some("Active"),
            "high",
            Vec::new(),
        );
        let id = SectionId {
            session_id: Arc::from("ses_1"),
            loop_id: None,
            request_index: None,
            kind: SectionKind::Summary,
            ordinal: 0,
            tool_call_id: None,
            history_index: Some(0),
        };
        let section = Arc::new(SectionLayout {
            key: LayoutKey {
                section: id,
                revision: 1,
                width: 79,
                theme: ThemeKind::Dark,
                folded: false,
                reasoning_visible: true,
            },
            order: 0,
            rows: Arc::new(vec![Line::from(Span::raw(
                "x".repeat(crate::limits::LAYOUT_CACHE_BYTES + 1),
            ))]),
            source: Arc::from("x"),
            source_map: Arc::new(crate::state::view::SourceMap {
                source: Arc::from("x"),
                rows: Arc::new(Vec::new()),
            }),
            copy_ranges: Arc::new(Vec::new()),
            link_cells: Arc::new(Vec::new()),
            content_columns: 0..1,
            collapsible: false,
            folded: false,
        });
        let prepared = Arc::new(crate::state::view::PreparedDurable {
            key: DurableCacheKey {
                revision: 0,
                width: 79,
                theme: ThemeKind::Dark,
                reasoning_visible: true,
                tools_expanded: false,
            },
            layout: Arc::new(ConversationLayout::from_sections(vec![section])),
        });
        let weak = Arc::downgrade(&prepared);
        app.sessions
            .known
            .get_mut("ses_1")
            .unwrap()
            .transcript
            .render_cache = Some(Arc::clone(&prepared));
        drop(prepared);
        app.enforce_layout_budget();
        assert!(weak.upgrade().is_none(), "layout owner must be released");
    }
}

#[cfg(test)]
mod decode_tests {
    use crate::jobs::LocalJobs;
    use crate::state::session::HistoryTrigger;
    use serde_json::json;
    use std::path::PathBuf;

    fn session_info() -> crate::protocol::SessionInfo {
        serde_json::from_value(json!({
            "session_id": "ses_decode",
            "title": null,
            "profile": "coding",
            "workspace": "/project",
            "model": "deep",
            "reasoning": "high",
            "loaded": true,
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z"
        }))
        .unwrap()
    }

    fn page() -> crate::protocol::ReadSessionResult {
        let data = r#"{"item":{"type":"user","data":{"loop_id":"loop","kind":"prompt","input":{"text":"hello"}}},"timestamp":"2026-01-01T00:00:00Z"}"#;
        serde_json::from_value(json!({
            "session": session_info(),
            "items": [{
                "index": 0,
                "offset": 0,
                "total_bytes": data.len(),
                "encoding": "utf8_json",
                "data": data,
                "complete": true
            }],
            "total": 1,
            "records": [],
            "records_truncated": false,
            "history_revision": "0000000000000000000000000000000000000000000000000000000000000000",
            "captured_end": 1,
            "trailing_incomplete": false
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn async_history_waits_for_the_decode_worker_before_installing_owner() {
        let mut app = crate::app::App::new(PathBuf::from("/project"));
        app.connection = crate::app::ConnectionState::Ready;
        app.enable_async_decode();
        app.sessions.known.insert(
            "ses_decode".to_owned(),
            crate::state::session::SessionView::new(session_info()),
        );
        app.sessions.active = Some("ses_decode".to_owned());
        app.sessions
            .known
            .get_mut("ses_decode")
            .unwrap()
            .history_read
            .begin(HistoryTrigger::Refresh);
        let request = app
            .request_history(&"ses_decode".to_owned())
            .expect("history request");
        let read = match request {
            crate::command::AppCommand::Rpc(request) => match app.pending_requests.get(&request.id)
            {
                Some(crate::app::RequestKind::History { read, .. }) => read.clone(),
                other => panic!("unexpected pending kind: {other:?}"),
            },
            _ => panic!("history request is RPC"),
        };
        assert!(
            app.continue_read_chain(&"ses_decode".to_owned(), &read, &page())
                .is_empty()
        );
        assert!(
            app.sessions.known["ses_decode"]
                .transcript
                .blocks
                .is_empty()
        );
        let decode = app.pending_decode_request().expect("one decode handoff");
        let mut jobs = LocalJobs::new();
        assert!(jobs.try_schedule_decode(decode));
        app.mark_decode_scheduled();
        let event = jobs.events().recv().await.expect("decode event");
        app.update(event);
        assert_eq!(app.sessions.known["ses_decode"].transcript.blocks.len(), 1);
        assert!(app.sessions.known["ses_decode"].transcript.complete);
        jobs.shutdown().await;
    }

    #[tokio::test]
    async fn stale_decode_result_clears_worker_identity_without_installing_new_epoch() {
        let mut app = crate::app::App::new(PathBuf::from("/project"));
        app.connection = crate::app::ConnectionState::Ready;
        app.enable_async_decode();
        app.sessions.known.insert(
            "ses_decode".to_owned(),
            crate::state::session::SessionView::new(session_info()),
        );
        app.sessions.active = Some("ses_decode".to_owned());
        app.sessions
            .known
            .get_mut("ses_decode")
            .unwrap()
            .history_read
            .begin(HistoryTrigger::Refresh);
        let request = app.request_history(&"ses_decode".to_owned()).unwrap();
        let read = match request {
            crate::command::AppCommand::Rpc(request) => match app.pending_requests.get(&request.id)
            {
                Some(crate::app::RequestKind::History { read, .. }) => read.clone(),
                other => panic!("unexpected pending kind: {other:?}"),
            },
            _ => panic!("history request is RPC"),
        };
        app.continue_read_chain(&"ses_decode".to_owned(), &read, &page());
        let decode = app.pending_decode_request().unwrap();
        let mut jobs = LocalJobs::new();
        assert!(jobs.try_schedule_decode(decode));
        app.mark_decode_scheduled();
        let view = app.sessions.known.get_mut("ses_decode").unwrap();
        view.session_epoch += 1;
        view.read_page = None;
        app.update(jobs.events().recv().await.unwrap());
        assert!(
            app.sessions.known["ses_decode"]
                .transcript
                .blocks
                .is_empty()
        );
        assert!(app.decode_in_flight.is_none());
        jobs.shutdown().await;
    }
}
