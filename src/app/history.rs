//! The authoritative Protocol v1 history window and its paging state
//! (spec §5.3, §6). This module owns the `session.read` pin/assembler chain so
//! it does not keep growing `app.rs`.
//!
//! It deliberately does **not** know about the display model. It returns
//! decoded Runtime items; `app.rs` projects them into the short-term
//! `TranscriptBlock` bridge, which stage C replaces with shared sections.

use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::Arc;

use crate::protocol::TurnRef;

use super::*;
use crate::protocol::read::{
    Assembled, ChunkAssembler, RawHistoryItem, ReadCursor, ReadError, RuntimeItem, SnapshotPin,
    TurnResultPage,
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
    large_items: BTreeMap<usize, usize>,
    pending_large_items: BTreeMap<usize, ()>,
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

    pub fn large_item(&self, index: usize) -> Option<usize> {
        self.large_items.get(&index).copied()
    }

    pub fn items(&self) -> impl Iterator<Item = (&usize, &Arc<RawHistoryItem>)> {
        self.items.iter()
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

    /// Inserts one decoded item, returning its `Arc` for the display bridge.
    pub fn insert(&mut self, index: usize, item: RawHistoryItem) -> Arc<RawHistoryItem> {
        let item = Arc::new(item);
        if let Some(previous) = self.items.remove(&index) {
            self.bytes = self.bytes.saturating_sub(item_bytes(&previous));
        }
        self.large_items.remove(&index);
        self.pending_large_items.remove(&index);
        self.bytes += item_bytes(&item);
        self.items.insert(index, item.clone());
        self.merge_range(index);
        item
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

/// A paged `turn.result` body. Its indexes remain local to the Turn and are
/// never inserted into the session-global history window.
#[derive(Debug)]
pub struct TurnResultWindow {
    pub turn: TurnRef,
    pub cursor: ReadCursor,
    pub assembler: ChunkAssembler,
    pub total: Option<usize>,
    pub items: BTreeMap<usize, Arc<RawHistoryItem>>,
    pub large_items: BTreeMap<usize, usize>,
    pub pending_large_items: BTreeMap<usize, ()>,
    pub explicit_large_item: bool,
    pub complete: bool,
}

impl TurnResultWindow {
    pub fn new(turn: TurnRef) -> Self {
        Self {
            turn,
            cursor: ReadCursor::start(),
            assembler: ChunkAssembler::new(),
            total: None,
            items: BTreeMap::new(),
            large_items: BTreeMap::new(),
            pending_large_items: BTreeMap::new(),
            explicit_large_item: false,
            complete: false,
        }
    }

    pub fn apply_page(&mut self, page: &TurnResultPage) -> Result<(), ReadError> {
        if page.turn != self.turn {
            return Err(ReadError::NonContiguous {
                expected: self.cursor.item,
                found: page.total,
            });
        }
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
                Assembled::Item { index, item } => {
                    if index != expected {
                        return Err(ReadError::NonContiguous {
                            expected,
                            found: index,
                        });
                    }
                    expected = index.saturating_add(1);
                    if let Some(existing) = self.items.get(&index) {
                        if existing.as_ref() != &item {
                            return Err(ReadError::ItemChanged { index });
                        }
                    } else {
                        self.large_items.remove(&index);
                        self.pending_large_items.remove(&index);
                        self.items.insert(index, Arc::new(item));
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
            self.complete = terminal && self.pending_large_items.is_empty();
        }
        Ok(())
    }
}

/// What one applied page contributed.
#[derive(Debug)]
pub struct AppliedPage {
    /// Newly decoded items in page order.
    pub inserted: Vec<(usize, Arc<RawHistoryItem>)>,
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

    let mut inserted = Vec::new();
    let mut placeholders = Vec::new();
    let mut explicit_large_item = false;
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
                let item = window.insert(index, item);
                inserted.push((index, item));
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
    pub(super) fn request_history(&mut self, session_id: &SessionId) -> Option<AppCommand> {
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
            && view
                .transcript
                .window
                .items()
                .any(|(_, item)| item.item.loop_id() == Some(loop_id))
            && !view.transcript.window.items().any(|(_, item)| {
                matches!(
                    &item.item,
                    crate::protocol::read::RuntimeItem::User(user)
                        if user.loop_id == loop_id
                            && user.kind == crate::protocol::read::RuntimeUserKind::Steering
                            && user.input.text == steer_text
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
                    for (index, item) in &applied.inserted {
                        install_history_item(view, *index, item);
                    }
                    for (index, total_bytes) in &applied.placeholders {
                        install_history_placeholder(view, *index, *total_bytes);
                    }
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

        // Project newly decoded Runtime items into the display bridge.
        for (index, item) in &applied.inserted {
            install_history_item(view, *index, item);
        }
        for (index, total_bytes) in &applied.placeholders {
            install_history_placeholder(view, *index, *total_bytes);
        }

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
                .any(|(_, item)| item.item.loop_id() == Some(id.as_str()))
        });

        let loop_contained_in_history = match &live_loop_id {
            Some(id) => view.transcript.blocks.iter().any(|b| match b {
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
            .filter_map(|block| match block {
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

        let apply_result = self
            .turn_results
            .entry(turn.clone())
            .or_insert_with(|| crate::app::history::TurnResultWindow::new(turn.clone()))
            .apply_page(&page);
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
