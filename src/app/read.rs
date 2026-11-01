//! Verified, bounded reads of immutable turn results and pinned history windows.

use std::sync::Arc;

use serde_json::{Value, json};
use tokio::sync::OwnedSemaphorePermit;

use crate::protocol::UnavailableReason;
use crate::protocol::read::{
    InterruptedRequest, MAX_AUTO_ITEM_BYTES, RawHistoryItem, ReadChunk, RuntimeItem,
    TurnAvailability, TurnPersistence, TurnRecord,
};
use crate::protocol::{
    self, ChunkAssembler, ChunkOutcome, ReadCursor, ReadSessionResult, RpcError, SnapshotPin,
    TurnRef, TurnResultPage, TurnWaitResult, Usage,
};
use crate::rpc::Client;

const READ_WORKSPACE_BYTES: usize = 64 * 1024 * 1024;
const PAGE_WORKSPACE_RESERVE: usize = protocol::PAGE_MAX_BYTES * 8;
pub const HISTORY_CATCHUP_MAX_PAGES: usize = 64;
pub const HISTORY_CATCHUP_MAX_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug)]
struct ReadWorkspace {
    #[cfg(feature = "e2e-observe")]
    observation: crate::rpc::metrics::Workspace,
    _permit: OwnedSemaphorePermit,
    used: usize,
    limit: usize,
}

impl ReadWorkspace {
    async fn acquire(client: &Client) -> Self {
        let permit = client
            .read_workspaces()
            .acquire_owned()
            .await
            .expect("reader slots stay open");
        Self {
            _permit: permit,
            #[cfg(feature = "e2e-observe")]
            observation: client.metrics.workspace(PAGE_WORKSPACE_RESERVE),
            used: PAGE_WORKSPACE_RESERVE,
            limit: READ_WORKSPACE_BYTES,
        }
    }

    fn charge(&mut self, bytes: usize) -> Result<(), ReadPartial> {
        self.used = self
            .used
            .checked_add(bytes)
            .filter(|used| *used <= self.limit)
            .ok_or(ReadPartial::WorkspaceLimit)?;
        #[cfg(feature = "e2e-observe")]
        self.observation.charge(bytes);
        Ok(())
    }

    fn release(&mut self, bytes: usize) {
        self.used -= bytes;
        #[cfg(feature = "e2e-observe")]
        self.observation.release(bytes);
    }

    fn reset(&mut self) {
        self.release(self.used - PAGE_WORKSPACE_RESERVE);
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ReadPartial {
    Oversized { index: usize, total_bytes: usize },
    WorkspaceLimit,
    HistoryLimit,
}

impl ReadPartial {
    pub fn message(&self) -> String {
        match self {
            Self::Oversized { index, total_bytes } => format!(
                "Item {index} is too large to restore ({total_bytes} bytes); existing content was kept"
            ),
            Self::WorkspaceLimit => {
                "The read is too large to restore; existing content was kept".into()
            }
            Self::HistoryLimit => {
                "More history remains to recover; load older history to continue".into()
            }
        }
    }
}

#[derive(Debug)]
pub enum TurnRead {
    Pending,
    /// The result is known to exist but its body cannot be read; the reason
    /// says why, and existing content stays as it is.
    Unavailable(UnavailableRead),
    /// The Agent answered with an error; nothing was installed.
    Failed(RpcError),
    Partial(Box<ReadPartial>),
    Complete(Box<TurnContent>),
}

/// Why a completed turn's body cannot be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnavailableRead {
    pub reason: UnavailableReason,
    pub persistence: Option<TurnPersistence>,
    pub persistence_uncertain: bool,
}

impl UnavailableRead {
    pub fn message(self) -> String {
        let reason = match self.reason {
            UnavailableReason::Expired => "the retained result expired",
            UnavailableReason::TooLarge => "it is too large to restore",
            UnavailableReason::ResourceLimit => "the Agent is out of read budget",
            UnavailableReason::InternalError => "the Agent hit an internal error",
            UnavailableReason::NotRetained | UnavailableReason::Unknown => {
                "the Agent no longer retains it"
            }
        };
        let saving = match (self.persistence, self.persistence_uncertain) {
            (Some(TurnPersistence::Persisted), _) => " · saved",
            (Some(TurnPersistence::Failed), true) => " · its save is unconfirmed",
            (Some(TurnPersistence::Failed), false) => " · not saved",
            (None, _) => "",
        };
        format!("This turn's content is unavailable: {reason}{saving}")
    }
}

/// Terminal facts shared by wait receipts and verified result pages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnDetails {
    pub outcome: crate::protocol::LoopOutcome,
    pub usage: Option<Usage>,
    pub requests: Option<u32>,
    pub tool_rounds: Option<u64>,
}

impl From<&TurnWaitResult> for TurnDetails {
    fn from(wait: &TurnWaitResult) -> Self {
        Self {
            outcome: wait.outcome.clone(),
            usage: wait.usage.clone(),
            requests: wait.requests,
            tool_rounds: wait.tool_rounds,
        }
    }
}

#[derive(Debug)]
pub struct TurnContent {
    pub turn: TurnRef,
    pub availability: TurnAvailability,
    /// The immutable identity of the body, which every page must agree with.
    pub result_id: String,
    pub details: TurnDetails,
    pub persistence: TurnPersistence,
    pub persistence_uncertain: bool,
    pub items: Vec<RawHistoryItem>,
    pub interrupted: Option<InterruptedRequest>,
    pub tool_facts: Vec<(crate::protocol::ToolRef, crate::protocol::ToolReadResult)>,
    _workspace: ReadWorkspace,
}

#[derive(Debug)]
pub enum HistoryRead {
    Partial(ReadPartial),
    Limited(Box<History>, ReadPartial),
    Complete(Box<History>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryCoverage {
    pub captured_end: u64,
    pub history_revision: String,
    pub total: usize,
    pub from: usize,
    pub end: usize,
}

impl HistoryCoverage {
    pub fn matches(&self, pin: &SnapshotPin) -> bool {
        self.captured_end == pin.captured_end
            && self.history_revision == pin.history_revision
            && self.total == pin.total
    }

    pub fn complete(&self) -> bool {
        self.from == 0 && self.end == self.total
    }
}

/// An assembled history window.
#[derive(Debug)]
pub struct History {
    /// Decoded items in source order.
    pub items: Vec<RawHistoryItem>,
    /// The first source item of the window.
    pub from: usize,
    /// The exclusive end of the verified source range.
    pub end: usize,
    pub pin: SnapshotPin,
    /// The display projection's `first_item`: older items are behind the
    /// compaction summary.
    pub floor: usize,
    pub has_more: bool,
    pub canonical: bool,
    /// How the turns with items in the window ended.
    pub records: Vec<TurnRecord>,
    /// The usage behind the summary, when the Agent counted all of it.
    pub covered_usage: Option<Usage>,
    _workspace: ReadWorkspace,
}

impl History {
    pub fn coverage(&self) -> HistoryCoverage {
        HistoryCoverage {
            captured_end: self.pin.captured_end,
            history_revision: self.pin.history_revision.clone(),
            total: self.pin.total,
            from: self.from,
            end: self.end,
        }
    }
}

/// Which part of the history to read.
#[derive(Clone)]
pub enum Window {
    Tail,
    CatchUp(Option<HistoryCoverage>),
    /// The display page just before this item.
    Before(usize),
    /// The canonical page just before this item, below the display floor.
    CanonicalBefore(usize),
}

struct HistoryBudget {
    pages: usize,
    bytes: usize,
}

fn history_page_bound(budget: &mut Option<HistoryBudget>, default: usize) -> Option<usize> {
    let Some(budget) = budget else {
        return Some(default);
    };
    if budget.pages == 0 || budget.bytes < protocol::PROBE_MAX_BYTES {
        return None;
    }
    budget.pages -= 1;
    Some(default.min(budget.bytes))
}

fn charge_history_bytes(budget: &mut Option<HistoryBudget>, bytes: usize) {
    if let Some(budget) = budget {
        budget.bytes -= bytes;
    }
}

pub(super) async fn stored_outcome(
    client: &Client,
    turn: &TurnRef,
    load: &str,
) -> Result<Option<TurnWaitResult>, RpcError> {
    let params =
        json!({"session_id": turn.session_id, "loop_id": turn.loop_id,"load":load, "limit": 1});
    let page: TurnResultPage = client.call(protocol::METHOD_TURN_RESULT, &params).await?;
    validate_turn_page(&page, turn, load)?;
    if matches!(
        page.availability,
        TurnAvailability::Pending | TurnAvailability::Unavailable
    ) {
        return Ok(None);
    }
    if page.outcome.is_none() || page.persistence.is_none() {
        return Err(invalid_read("completed turn metadata is missing"));
    }
    Ok(page.outcome.map(|outcome| TurnWaitResult {
        turn: turn.clone(),
        outcome,
        usage: page.usage,
        requests: page.requests,
        tool_rounds: page.tool_rounds,
        next: None,
        queue: None,
    }))
}

pub(super) fn invalid_read(reason: impl Into<String>) -> RpcError {
    RpcError::new(protocol::INTERNAL_ERROR, "invalid_read", reason)
}

fn validate_page_chunks(chunks: &[ReadChunk], max_bytes: usize) -> Result<(), RpcError> {
    let bytes = chunks
        .iter()
        .try_fold(0usize, |bytes, chunk| bytes.checked_add(chunk.data.len()));
    if bytes.is_none_or(|bytes| bytes > max_bytes) {
        return Err(invalid_read("read page exceeds the requested byte budget"));
    }
    Ok(())
}

fn validate_turn_page(page: &TurnResultPage, turn: &TurnRef, load: &str) -> Result<(), RpcError> {
    validate_page_chunks(&page.items, protocol::PAGE_MAX_BYTES)?;
    if page.turn != *turn {
        return Err(invalid_read("turn identity changed"));
    }
    if page.load.as_deref() != Some(load) {
        return Err(invalid_read("the result belongs to another load"));
    }
    match page.availability {
        TurnAvailability::Pending
            if !page.items.is_empty()
                || page.next_cursor.is_some()
                || page.outcome.is_some()
                || page.persistence.is_some()
                || page.interrupted.is_some()
                || page.total != 0
                || page.result_id.is_some() =>
        {
            return Err(invalid_read("pending turn contains completed result data"));
        }
        TurnAvailability::Unavailable => {
            if !page.items.is_empty() || page.next_cursor.is_some() || page.total != 0 {
                return Err(invalid_read("an unavailable turn carries a body"));
            }
            if page.result_id.is_some() {
                return Err(invalid_read("an unavailable turn names a readable body"));
            }
        }
        _ => {}
    }
    Ok(())
}

/// One assembled sequence of contiguous source items.
struct Assembly {
    assembler: ChunkAssembler,
    position: ReadCursor,
    items: Vec<RawHistoryItem>,
    oversized: Option<ReadPartial>,
    reserved_item: usize,
}

impl Assembly {
    fn new(start: usize) -> Self {
        Self {
            assembler: ChunkAssembler::new(),
            position: ReadCursor {
                item: start,
                offset: 0,
            },
            items: Vec::new(),
            oversized: None,
            reserved_item: 0,
        }
    }

    fn reserve_items(
        &mut self,
        count: usize,
        workspace: &mut ReadWorkspace,
    ) -> Result<(), ReadPartial> {
        let bytes = count
            .checked_mul(std::mem::size_of::<RawHistoryItem>())
            .ok_or(ReadPartial::WorkspaceLimit)?;
        workspace.charge(bytes)?;
        self.items
            .try_reserve_exact(count)
            .map_err(|_| ReadPartial::WorkspaceLimit)?;
        workspace.charge(
            (self.items.capacity() - count).saturating_mul(std::mem::size_of::<RawHistoryItem>()),
        )
    }

    fn push(
        &mut self,
        chunk: ReadChunk,
        end: usize,
        workspace: &mut ReadWorkspace,
    ) -> Result<Option<ReadPartial>, RpcError> {
        if chunk.index != self.position.item
            || chunk.offset != self.position.offset
            || chunk.index >= end
        {
            return Err(invalid_read("item chunks are not contiguous"));
        }
        if chunk.offset == 0 {
            let bytes = if chunk.total_bytes > MAX_AUTO_ITEM_BYTES {
                512
            } else {
                chunk
                    .total_bytes
                    .checked_mul(4)
                    .and_then(|bytes| bytes.checked_add(2 * std::mem::size_of::<RawHistoryItem>()))
                    .unwrap_or(usize::MAX)
            };
            if let Err(partial) = workspace.charge(bytes) {
                return Ok(Some(partial));
            }
            self.reserved_item = bytes;
        }
        let result = self
            .assembler
            .push(chunk)
            .map_err(|error| invalid_read(error.to_string()))?;
        match result {
            ChunkOutcome::Pending => {
                self.position = self
                    .assembler
                    .next_cursor()
                    .expect("a pending item has a cursor");
            }
            ChunkOutcome::Item(item) => {
                let structures = json_structures(&item.data);
                if let Err(partial) = workspace.charge(structures.saturating_mul(512)) {
                    return Ok(Some(partial));
                }
                self.reserved_item += structures.saturating_mul(512);
                let decoded = protocol::read::decode_item(&item.data).map_err(invalid_read)?;
                let retained = decoded.heap_bytes();
                if retained > self.reserved_item {
                    return Ok(Some(ReadPartial::WorkspaceLimit));
                }
                self.items.push(decoded);
                drop(item);
                workspace.release(self.reserved_item - retained);
                self.reserved_item = 0;
                self.position = ReadCursor {
                    item: self.position.item + 1,
                    offset: 0,
                };
            }
            ChunkOutcome::Oversized { index, total_bytes } => {
                self.oversized
                    .get_or_insert(ReadPartial::Oversized { index, total_bytes });
                self.reserved_item = 0;
                self.position = ReadCursor {
                    item: self.position.item + 1,
                    offset: 0,
                };
            }
        }
        Ok(None)
    }

    fn page_end(
        &self,
        start: ReadCursor,
        next: Option<ReadCursor>,
        total: usize,
    ) -> Result<(), RpcError> {
        if self.position == start && start.item < total {
            return Err(invalid_read("read page made no progress"));
        }
        let expected = (self.position.item < total).then_some(self.position);
        if next != expected || next.is_some_and(|next| next <= start) {
            return Err(invalid_read("cursor does not match the assembled items"));
        }
        if next.is_none() && self.assembler.next_cursor().is_some() {
            return Err(invalid_read("the final item is incomplete"));
        }
        Ok(())
    }
}

#[derive(Default)]
struct JsonCounter {
    bytes: usize,
    structures: usize,
    quoted: bool,
    escaped: bool,
}

impl JsonCounter {
    fn observe(&mut self, data: &[u8]) {
        self.bytes = self.bytes.saturating_add(data.len());
        for &byte in data {
            if self.quoted {
                if self.escaped {
                    self.escaped = false;
                } else if byte == b'\\' {
                    self.escaped = true;
                } else if byte == b'"' {
                    self.quoted = false;
                }
            } else if byte == b'"' {
                self.quoted = true;
            } else if matches!(byte, b'{' | b'[' | b',') {
                self.structures = self.structures.saturating_add(1);
            }
        }
    }
}

impl std::io::Write for JsonCounter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.observe(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

// Each JSON container or separator reserves room for decoded structs and vector slack.
fn json_structures(raw: &str) -> usize {
    let mut counter = JsonCounter::default();
    counter.observe(raw.as_bytes());
    counter.structures.saturating_add(1)
}

fn charge_metadata<T: serde::Serialize>(
    workspace: &mut ReadWorkspace,
    value: &T,
) -> Result<(), ReadPartial> {
    workspace.charge(metadata_charge(value))
}

fn metadata_charge<T: serde::Serialize>(value: &T) -> usize {
    let mut counter = JsonCounter::default();
    serde_json::to_writer(&mut counter, value).expect("decoded metadata serializes");
    counter
        .bytes
        .saturating_mul(2)
        .saturating_add(counter.structures.saturating_add(1).saturating_mul(512))
}

/// Reads one history window and retries once when its pin becomes stale.
pub async fn read_history(
    client: Arc<Client>,
    session_id: String,
    window: Window,
) -> Result<HistoryRead, RpcError> {
    let mut workspace = ReadWorkspace::acquire(&client).await;
    let mut budget = matches!(&window, Window::CatchUp(_)).then_some(HistoryBudget {
        pages: HISTORY_CATCHUP_MAX_PAGES,
        bytes: HISTORY_CATCHUP_MAX_BYTES,
    });
    let read = async |workspace: &mut ReadWorkspace,
                      budget: &mut Option<HistoryBudget>,
                      probe_bound: usize|
           -> Result<_, RpcError> {
        let canonical = matches!(&window, Window::CanonicalBefore(_) | Window::CatchUp(_));
        let view = if canonical { "canonical" } else { "display" };
        let (pin, bytes) = probe(&client, &session_id, view, probe_bound).await?;
        charge_history_bytes(budget, bytes);
        if let Err(partial) = charge_metadata(workspace, &pin) {
            return Ok((WindowRead::Partial(partial), 0, 0, pin, canonical));
        }
        let (start, end) = match &window {
            Window::CatchUp(Some(covered)) if covered.matches(&pin) => (0, covered.from),
            Window::CatchUp(_) => (0, pin.total),
            Window::Tail => (
                pin.total
                    .saturating_sub(protocol::TAIL_ITEMS)
                    .max(pin.first_item),
                pin.total,
            ),
            Window::Before(end) => (
                end.saturating_sub(protocol::PAGE_LIMIT)
                    .max(pin.first_item)
                    .min(*end),
                *end,
            ),
            Window::CanonicalBefore(end) => (end.saturating_sub(protocol::PAGE_LIMIT), *end),
        };
        if end > pin.total {
            return Err(invalid_read("history window exceeds its pinned total"));
        }
        let result = if matches!(&window, Window::CatchUp(_)) {
            read_catchup(&client, &session_id, &pin, end, workspace, budget).await?
        } else {
            read_window(
                &client,
                &session_id,
                view,
                &pin,
                start..end,
                workspace,
                budget,
            )
            .await?
        };
        Ok((result, start, end, pin, canonical))
    };
    let probe_bound = history_page_bound(&mut budget, protocol::PROBE_MAX_BYTES)
        .expect("a new round admits its probe");
    let result = match read(&mut workspace, &mut budget, probe_bound).await {
        Err(error) if error.is_stale_pin() => {
            workspace.reset();
            let Some(bound) = history_page_bound(&mut budget, protocol::PROBE_MAX_BYTES) else {
                return Ok(HistoryRead::Partial(ReadPartial::HistoryLimit));
            };
            read(&mut workspace, &mut budget, bound).await
        }
        result => result,
    }?;
    let (result, start, end, pin, canonical) = result;
    let (items, records, start, end, limit) = match result {
        WindowRead::Partial(partial) => return Ok(HistoryRead::Partial(partial)),
        WindowRead::Complete(items, records) => (items, records, start, end, None),
        WindowRead::Limited(items, records, from, end, reason) => {
            (items, records, from, end, Some(reason))
        }
    };
    let page = Box::new(History {
        items,
        from: start,
        end,
        floor: pin.first_item,
        has_more: start > pin.first_item,
        canonical,
        records,
        covered_usage: pin
            .covered_usage
            .clone()
            .filter(|covered| !covered.partial)
            .map(|covered| covered.usage),
        pin,
        _workspace: workspace,
    });
    Ok(match limit {
        Some(reason) => HistoryRead::Limited(page, reason),
        None => HistoryRead::Complete(page),
    })
}

/// A one-item read that pins the snapshot the window is read from.
async fn probe(
    client: &Client,
    session_id: &str,
    view: &str,
    max_bytes: usize,
) -> Result<(SnapshotPin, usize), RpcError> {
    let params = json!({
        "session_id": session_id,
        "limit": protocol::PROBE_LIMIT,
        "max_bytes": max_bytes,
        "view": view,
    });
    let page: ReadSessionResult = client.call(protocol::METHOD_SESSION_READ, &params).await?;
    validate_page_chunks(&page.items, max_bytes)?;
    if !page.records.is_empty() {
        let mut counter = JsonCounter::default();
        serde_json::to_writer(&mut counter, &page.records).expect("decoded metadata serializes");
        if counter.bytes > protocol::PROBE_MAX_BYTES {
            return Err(invalid_read(
                "history probe exceeds the requested byte budget",
            ));
        }
    }
    if page.session.session_id != session_id {
        return Err(invalid_read("history session identity changed"));
    }
    if (view == "display") != page.projection.is_some() {
        return Err(invalid_read(
            "history projection does not match the requested view",
        ));
    }
    if page.pin().first_item > page.total {
        return Err(invalid_read("history projection starts past its total"));
    }
    if page.items.len() > 1 {
        return Err(invalid_read("history probe returned more than one item"));
    }
    let expected = page.pin().first_item;
    match page.items.first() {
        Some(chunk)
            if chunk.index != expected || chunk.offset != 0 || chunk.encoding != "utf8_json" =>
        {
            return Err(invalid_read(
                "history probe contains an invalid first chunk",
            ));
        }
        None if expected < page.total => {
            return Err(invalid_read("history probe made no progress"));
        }
        _ => {}
    }
    let bytes = page.items.iter().map(|chunk| chunk.data.len()).sum();
    Ok((page.pin(), bytes))
}

enum WindowRead {
    Complete(Vec<RawHistoryItem>, Vec<TurnRecord>),
    Limited(
        Vec<RawHistoryItem>,
        Vec<TurnRecord>,
        usize,
        usize,
        ReadPartial,
    ),
    Partial(ReadPartial),
}

async fn read_catchup(
    client: &Client,
    session_id: &str,
    pin: &SnapshotPin,
    end: usize,
    workspace: &mut ReadWorkspace,
    budget: &mut Option<HistoryBudget>,
) -> Result<WindowRead, RpcError> {
    let mut items = Vec::new();
    let count = budget.as_ref().expect("catch-up has a round budget").pages * protocol::PAGE_LIMIT;
    if let Err(partial) = workspace.charge(count * std::mem::size_of::<RawHistoryItem>()) {
        return Ok(WindowRead::Partial(partial));
    }
    if items.try_reserve_exact(count).is_err() {
        return Ok(WindowRead::Partial(ReadPartial::WorkspaceLimit));
    }
    if let Err(partial) =
        workspace.charge((items.capacity() - count) * std::mem::size_of::<RawHistoryItem>())
    {
        return Ok(WindowRead::Partial(partial));
    }
    let mut records: Vec<TurnRecord> = Vec::new();
    let mut at = end;
    while at > 0 {
        let charged = workspace.used;
        let available = budget.as_ref().unwrap().pages;
        budget.as_mut().unwrap().pages = available.min(1);
        let start = at.saturating_sub(protocol::PAGE_LIMIT);
        let preview = read_window(
            client,
            session_id,
            "canonical",
            pin,
            start..at,
            workspace,
            budget,
        )
        .await;
        budget.as_mut().unwrap().pages += available - available.min(1);
        let (window, start) = match preview? {
            complete @ WindowRead::Complete(..) => (complete, start),
            WindowRead::Limited(..) | WindowRead::Partial(_) => {
                workspace.release(workspace.used - charged);
                (
                    read_window(
                        client,
                        session_id,
                        "canonical",
                        pin,
                        at - 1..at,
                        workspace,
                        budget,
                    )
                    .await?,
                    at - 1,
                )
            }
        };
        let (decoded, metadata) = match window {
            WindowRead::Complete(items, records) => (items, records),
            WindowRead::Limited(_, _, _, _, reason) | WindowRead::Partial(reason) => {
                workspace.release(workspace.used - charged);
                items.reverse();
                return Ok(WindowRead::Limited(items, records, at, end, reason));
            }
        };
        let mut retained = decoded
            .iter()
            .map(RawHistoryItem::heap_bytes)
            .sum::<usize>();
        for record in metadata {
            if let Some(known) = records.iter().find(|known| known.loop_id == record.loop_id) {
                if known != &record {
                    return Err(invalid_read("history turn metadata changed between pages"));
                }
            } else {
                retained += metadata_charge(&record);
                records.push(record);
            }
        }
        workspace.release(workspace.used - charged);
        workspace
            .charge(retained)
            .expect("retained source item is a subset of its read workspace");
        items.extend(decoded.into_iter().rev());
        at = start;
    }
    items.reverse();
    Ok(WindowRead::Complete(items, records))
}

async fn read_window(
    client: &Client,
    session_id: &str,
    view: &str,
    pin: &SnapshotPin,
    range: std::ops::Range<usize>,
    workspace: &mut ReadWorkspace,
    budget: &mut Option<HistoryBudget>,
) -> Result<WindowRead, RpcError> {
    let (start, end) = (range.start, range.end);
    let mut assembly = Assembly::new(start);
    let count = budget.as_ref().map_or(end - start, |budget| {
        (end - start).min(budget.pages * protocol::PAGE_LIMIT)
    });
    if let Err(partial) = assembly.reserve_items(count, workspace) {
        return Ok(WindowRead::Partial(partial));
    }
    let mut records: Vec<TurnRecord> = Vec::new();
    while assembly.position.item < end {
        let Some(max_bytes) = history_page_bound(budget, protocol::PAGE_MAX_BYTES) else {
            if let Some(partial) = assembly.oversized {
                return Ok(WindowRead::Partial(partial));
            }
            workspace.release(assembly.reserved_item);
            let through = start + assembly.items.len();
            return Ok(WindowRead::Limited(
                assembly.items,
                records,
                start,
                through,
                ReadPartial::HistoryLimit,
            ));
        };
        let position = assembly.position;
        let mut params = json!({
            "session_id": session_id,
            "limit": protocol::PAGE_LIMIT.min(end - position.item),
            "max_bytes": max_bytes,
            "view": view,
            "cursor": position.value(),
            "captured_end": pin.captured_end,
            "history_revision": pin.history_revision,
        });
        if let Some(projection) = &pin.projection_revision {
            params["projection_revision"] = json!(projection);
        }
        let page: ReadSessionResult = client.call(protocol::METHOD_SESSION_READ, &params).await?;
        validate_page_chunks(&page.items, max_bytes)?;
        charge_history_bytes(
            budget,
            page.items.iter().map(|chunk| chunk.data.len()).sum(),
        );
        if page.session.session_id != session_id || page.pin() != *pin {
            return Err(invalid_read(
                "history page does not match its pinned snapshot",
            ));
        }
        for record in page.records {
            if let Some(known) = records.iter().find(|known| known.loop_id == record.loop_id) {
                if known != &record {
                    return Err(invalid_read("history turn metadata changed between pages"));
                }
            } else {
                if let Err(partial) = charge_metadata(workspace, &record) {
                    return Ok(WindowRead::Partial(partial));
                }
                records.push(record);
            }
        }
        for chunk in page.items {
            if let Some(partial) = assembly.push(chunk, end, workspace)? {
                return Ok(WindowRead::Partial(partial));
            }
        }
        assembly.page_end(position, page.next_cursor, pin.total)?;
    }
    if let Some(partial) = assembly.oversized {
        Ok(WindowRead::Partial(partial))
    } else {
        Ok(WindowRead::Complete(assembly.items, records))
    }
}

/// Reads and validates a completion's whole result before returning installable content.
pub async fn read_turn(
    client: Arc<Client>,
    turn: TurnRef,
    load: String,
) -> Result<TurnRead, RpcError> {
    let mut workspace = ReadWorkspace::acquire(&client).await;
    let mut assembly = Assembly::new(0);
    let mut expected: Option<TurnContentMeta> = None;
    let (mut usage, mut requests, mut tool_rounds) = (None, None, None);
    loop {
        let position = assembly.position;
        let mut params = json!({
            "session_id": turn.session_id,
            "loop_id": turn.loop_id,
            "load": load,
            "limit": protocol::PAGE_LIMIT,
            "max_bytes": protocol::PAGE_MAX_BYTES,
        });
        if position != ReadCursor::start() {
            params["cursor"] = position.value();
            params["result_id"] = json!(
                expected
                    .as_ref()
                    .expect("a continuation has a page identity")
                    .result_id
            );
        }
        let page: TurnResultPage = match client.call(protocol::METHOD_TURN_RESULT, &params).await {
            // A result that expired or changed identity between pages is a
            // terminal state for this read, not a retryable error.
            Err(error) if error.kind == "turn_not_found" || error.kind == "not_retained" => {
                return Ok(TurnRead::Unavailable(UnavailableRead {
                    reason: UnavailableReason::NotRetained,
                    persistence: None,
                    persistence_uncertain: false,
                }));
            }
            Err(error) if error.kind == "stale_result" => {
                return Ok(TurnRead::Failed(error));
            }
            result => result?,
        };
        validate_turn_page(&page, &turn, &load)?;
        match page.availability {
            TurnAvailability::Pending => {
                if expected.is_some() {
                    return Err(invalid_read("pending turn contains completed result data"));
                }
                return Ok(TurnRead::Pending);
            }
            TurnAvailability::Unavailable => {
                return Ok(TurnRead::Unavailable(UnavailableRead {
                    reason: page
                        .unavailable_reason
                        .unwrap_or(UnavailableReason::NotRetained),
                    persistence: page
                        .persistence
                        .or_else(|| expected.as_ref().map(|meta| meta.persistence)),
                    persistence_uncertain: page.persistence_uncertain
                        || expected
                            .as_ref()
                            .is_some_and(|meta| meta.persistence_uncertain),
                }));
            }
            TurnAvailability::Live | TurnAvailability::Stored => {}
        }
        let outcome = page
            .outcome
            .ok_or_else(|| invalid_read("completed turn outcome is missing"))?;
        let persistence = page
            .persistence
            .ok_or_else(|| invalid_read("completed turn persistence is missing"))?;
        let result_id = page
            .result_id
            .clone()
            .filter(|id| {
                id.strip_prefix("r1:").is_some_and(|digest| {
                    digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
                })
            })
            .ok_or_else(|| invalid_read("completed turn result identity is missing"))?;
        let metadata = TurnContentMeta {
            total: page.total,
            outcome,
            persistence,
            persistence_uncertain: page.persistence_uncertain,
            interrupted: page.interrupted,
            result_id,
        };
        match &expected {
            Some(expected) if expected != &metadata => {
                return Err(invalid_read("turn metadata changed between pages"));
            }
            Some(_) => {}
            None => {
                if let Err(partial) = charge_metadata(
                    &mut workspace,
                    &(&turn, &metadata.outcome, &metadata.interrupted),
                ) {
                    return Ok(TurnRead::Partial(Box::new(partial)));
                }
                if let Err(partial) = assembly.reserve_items(page.total, &mut workspace) {
                    return Ok(TurnRead::Partial(Box::new(partial)));
                }
                expected = Some(metadata.clone());
            }
        }
        // A durable page may omit statistics a retained result supplied.
        // Keep known facts without extending the body's identity contract.
        usage = usage.or(page.usage);
        requests = requests.or(page.requests);
        tool_rounds = tool_rounds.or(page.tool_rounds);
        for chunk in page.items {
            if let Some(partial) = assembly.push(chunk, page.total, &mut workspace)? {
                return Ok(TurnRead::Partial(Box::new(partial)));
            }
        }
        assembly.page_end(position, page.next_cursor, page.total)?;
        if page.next_cursor.is_none() {
            for item in &assembly.items {
                let loop_id = match &item.item {
                    RuntimeItem::User(item) => Some(&item.loop_id),
                    RuntimeItem::Assistant(item) => Some(&item.loop_id),
                    RuntimeItem::ToolResult(item) => Some(&item.loop_id),
                    RuntimeItem::Summary(_) => None,
                };
                if loop_id.is_some_and(|loop_id| loop_id != &turn.loop_id) {
                    return Err(invalid_read("item belongs to another turn"));
                }
            }
            if let Some(partial) = assembly.oversized {
                return Ok(TurnRead::Partial(Box::new(partial)));
            }
            let meta = expected.expect("completed pages have metadata");
            let final_count = assembly
                .items
                .iter()
                .filter(|item| matches!(item.item, RuntimeItem::ToolResult(_)))
                .count();
            if let Err(partial) = workspace.charge(final_count.saturating_mul(128)) {
                return Ok(TurnRead::Partial(Box::new(partial)));
            }
            let final_calls: std::collections::BTreeSet<_> = assembly
                .items
                .iter()
                .filter_map(|item| match &item.item {
                    RuntimeItem::ToolResult(result) => {
                        Some((result.request_index, result.call_id.as_str()))
                    }
                    _ => None,
                })
                .collect();
            let mut tool_facts = Vec::new();
            let mut asked = 0;
            'tools: for item in &assembly.items {
                let RuntimeItem::Assistant(assistant) = &item.item else {
                    continue;
                };
                for part in &assistant.content {
                    let crate::protocol::read::RuntimeAssistantPart::ToolCall {
                        tool_call_id, ..
                    } = part
                    else {
                        continue;
                    };
                    if final_calls.contains(&(assistant.request_index, tool_call_id.as_str())) {
                        continue;
                    }
                    if asked == 64 {
                        break 'tools;
                    }
                    asked += 1;
                    let key = crate::protocol::ToolRef {
                        session_id: turn.session_id.clone(),
                        loop_id: turn.loop_id.clone(),
                        request_index: assistant.request_index,
                        tool_call_id: tool_call_id.clone(),
                    };
                    let params = json!({"session_id":turn.session_id,"loop_id":turn.loop_id,"load":load,"request_index":key.request_index,"tool_call_id":key.tool_call_id,"display":true});
                    let Ok(facts) = client
                        .call::<_, Value>(protocol::METHOD_TOOL_READ, &params)
                        .await
                    else {
                        continue;
                    };
                    if let Err(partial) = charge_metadata(&mut workspace, &facts) {
                        return Ok(TurnRead::Partial(Box::new(partial)));
                    }
                    let facts = serde_json::from_value(facts)
                        .map_err(|error| invalid_read(format!("tool metadata: {error}")))?;
                    tool_facts.push((key, facts));
                }
            }
            return Ok(TurnRead::Complete(Box::new(TurnContent {
                turn,
                availability: page.availability,
                result_id: meta.result_id,
                details: TurnDetails {
                    outcome: meta.outcome,
                    usage,
                    requests,
                    tool_rounds,
                },
                persistence: meta.persistence,
                persistence_uncertain: meta.persistence_uncertain,
                items: assembly.items,
                interrupted: meta.interrupted,
                tool_facts,
                _workspace: workspace,
            })));
        }
    }
}

/// The page metadata that must stay identical across a result's pages.
#[derive(Clone, Debug, PartialEq, Eq)]
struct TurnContentMeta {
    total: usize,
    outcome: crate::protocol::LoopOutcome,
    persistence: TurnPersistence,
    persistence_uncertain: bool,
    interrupted: Option<InterruptedRequest>,
    result_id: String,
}
/// Canonical text for an export, with the same pin and byte budgets as recovery.
pub(super) async fn export_text(client: &Client, session_id: &str) -> Result<String, String> {
    let mut workspace = ReadWorkspace::acquire(client).await;
    let items = async |workspace: &mut ReadWorkspace| {
        let (pin, _) = probe(client, session_id, "canonical", protocol::PROBE_MAX_BYTES).await?;
        read_window(
            client,
            session_id,
            "canonical",
            &pin,
            0..pin.total,
            workspace,
            &mut None,
        )
        .await
    };
    let items = match items(&mut workspace).await {
        Err(error) if error.is_stale_pin() => {
            workspace.reset();
            items(&mut workspace).await
        }
        result => result,
    }
    .map_err(|error| error.message)?;
    let items = match items {
        WindowRead::Complete(items, _) => items,
        WindowRead::Partial(partial) | WindowRead::Limited(_, _, _, _, partial) => {
            return Err(partial.message());
        }
    };
    Ok(crate::transcript::to_markdown(&items))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::{Value, json};
    use tokio::sync::mpsc;

    use super::{
        Assembly, HistoryRead, PAGE_WORKSPACE_RESERVE, ReadPartial, ReadWorkspace, TurnRead,
        UnavailableRead, Window, read_history, read_turn,
    };
    use crate::protocol::read::TurnAvailability;
    use crate::protocol::{ReadChunk, TurnRef, UnavailableReason};
    use crate::testing::{FakeAgent, chunk, session_info};

    const TEST_TIMEOUT: Duration = Duration::from_secs(10);

    const RESULT_ID: &str = "r1:0000000000000000000000000000000000000000000000000000000000000002";

    fn result_page(turn: Value, items: Vec<Value>, total: usize, next: Value) -> Value {
        json!({
            "turn": turn,
            "load": crate::testing::LOAD,
            "result_id": RESULT_ID,
            "availability": "stored",
            "outcome": {"type": "completed"},
            "persistence": "persisted",
            "items": items,
            "total": total,
            "next_cursor": next,
        })
    }

    fn user(index: usize, text: &str) -> Value {
        chunk(
            index,
            json!({"type": "user", "data": {
                "loop_id": "l1", "kind": "prompt", "input": {"text": text}
            }}),
        )
    }

    fn assistant(index: usize, text: &str) -> Value {
        chunk(
            index,
            json!({"type": "assistant", "data": {
                "loop_id": "l1", "request_index": 0,
                "content": [{"type": "text", "data": text}]
            }}),
        )
    }

    fn turn(session_id: &str, loop_id: &str) -> Value {
        json!({"session_id": session_id, "loop_id": loop_id})
    }

    async fn read_pages(
        pages: Vec<Value>,
    ) -> (Result<TurnRead, crate::protocol::RpcError>, Vec<Value>) {
        let (tx, _rx) = mpsc::unbounded_channel();
        let (fake, _app) = FakeAgent::start(tx);
        fake.script_sequence("turn.result", pages);
        let result = tokio::time::timeout(
            TEST_TIMEOUT,
            read_turn(
                fake.client.clone(),
                TurnRef::new("s1", "l1"),
                crate::testing::LOAD.into(),
            ),
        )
        .await
        .expect("turn read timed out");
        (result, fake.calls("turn.result"))
    }

    fn malformed_chunk(data: &str, offset: usize, total: usize, complete: bool) -> Value {
        json!({"index": 0, "offset": offset, "total_bytes": total,
               "encoding": "utf8_json", "data": data, "complete": complete})
    }

    #[tokio::test(start_paused = true)]
    async fn complete_pages_keep_terminal_facts_and_allow_an_empty_body() {
        let mut first = result_page(
            turn("s1", "l1"),
            vec![user(0, "one")],
            2,
            json!({"item": 1, "offset": 0}),
        );
        let mut last = result_page(turn("s1", "l1"), vec![assistant(1, "two")], 2, Value::Null);
        for page in [&mut first, &mut last] {
            page["usage"] = json!({"input_tokens":90, "output_tokens":12});
            page["requests"] = json!(3);
            page["tool_rounds"] = json!(2);
        }
        let (result, calls) = read_pages(vec![first, last]).await;
        let TurnRead::Complete(content) = result.expect("complete pages must be readable") else {
            panic!("a complete result was expected");
        };
        assert_eq!(content.items.len(), 2);
        assert_eq!(
            content.details.usage.as_ref().unwrap().output_tokens,
            Some(12)
        );
        assert_eq!(content.details.requests, Some(3));
        assert_eq!(content.details.tool_rounds, Some(2));
        assert!(content.interrupted.is_none());
        drop(content);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1]["cursor"], json!({"item": 1, "offset": 0}));
        assert_eq!(
            calls[1]["result_id"], RESULT_ID,
            "a continuation names the immutable body it continues"
        );

        let empty = result_page(turn("s1", "l1"), vec![], 0, Value::Null);
        let (result, calls) = read_pages(vec![empty]).await;
        let TurnRead::Complete(content) = result.expect("a complete empty turn is valid") else {
            panic!("a complete empty result was expected");
        };
        assert!(content.items.is_empty());
        assert_eq!(calls.len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn w0_read_turn_does_not_accept_pending_as_a_complete_empty_turn() {
        let mut pending = result_page(turn("s1", "l1"), vec![], 0, Value::Null);
        pending["availability"] = json!("pending");
        pending.as_object_mut().unwrap().remove("outcome");
        pending.as_object_mut().unwrap().remove("persistence");
        pending.as_object_mut().unwrap().remove("result_id");
        let (result, _) = read_pages(vec![pending]).await;
        assert!(
            matches!(result, Ok(TurnRead::Pending)),
            "pending must not be returned as a complete turn: {result:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn live_to_stored_pages_keep_known_statistics_when_either_page_omits_them() {
        for statistics_first in [true, false] {
            let mut first = result_page(
                turn("s1", "l1"),
                vec![user(0, "one")],
                2,
                json!({"item":1,"offset":0}),
            );
            first["availability"] = json!("live");
            let mut last = result_page(turn("s1", "l1"), vec![assistant(1, "two")], 2, Value::Null);
            let statistics = if statistics_first {
                &mut first
            } else {
                &mut last
            };
            statistics["usage"] = json!({"input_tokens":90,"output_tokens":12});
            statistics["requests"] = json!(3);
            statistics["tool_rounds"] = json!(2);
            let (result, calls) = read_pages(vec![first, last]).await;
            let Ok(TurnRead::Complete(content)) = result else {
                panic!("live-to-stored lost its immutable body: {result:?}");
            };
            assert_eq!(content.items.len(), 2);
            assert_eq!(content.availability, TurnAvailability::Stored);
            assert_eq!(
                content.details.usage.as_ref().unwrap().output_tokens,
                Some(12)
            );
            assert_eq!(content.details.requests, Some(3));
            assert_eq!(content.details.tool_rounds, Some(2));
            assert_eq!(calls[1]["result_id"], RESULT_ID);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_changed_result_identity_or_load_never_installs_a_verified_prefix() {
        let first = result_page(
            turn("s1", "l1"),
            vec![user(0, "one")],
            2,
            json!({"item":1,"offset":0}),
        );
        for (field, value) in [
            ("result_id", json!(format!("r1:{:064x}", 3))),
            ("load", json!("reopened")),
        ] {
            let mut last = result_page(turn("s1", "l1"), vec![assistant(1, "two")], 2, Value::Null);
            last[field] = value;
            let (result, _) = read_pages(vec![first.clone(), last]).await;
            assert!(
                result.is_err(),
                "changed {field} installed a prefix: {result:?}"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn expiry_between_pages_is_unavailable_not_a_complete_prefix() {
        let first = result_page(
            turn("s1", "l1"),
            vec![user(0, "one")],
            2,
            json!({"item":1,"offset":0}),
        );
        let last = json!({"turn":turn("s1","l1"),"load":crate::testing::LOAD,"availability":"unavailable","unavailable_reason":"expired","total":0,"items":[],"next_cursor":null});
        let (result, calls) = read_pages(vec![first, last]).await;
        assert!(
            matches!(
                result,
                Ok(TurnRead::Unavailable(UnavailableRead {
                    reason: UnavailableReason::Expired,
                    ..
                }))
            ),
            "{result:?}"
        );
        assert_eq!(calls.len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn canonical_tools_without_final_results_use_bounded_metadata_reads_not_output_downloads()
    {
        let (tx, _rx) = mpsc::unbounded_channel();
        let (fake, _app) = FakeAgent::start(tx);
        let item = chunk(
            0,
            json!({"type":"assistant","data":{"loop_id":"l1","request_index":0,"content":[{"type":"tool_call","data":{"tool_call_id":"call1","name":"bash"}}]}}),
        );
        fake.script(
            "turn.result",
            result_page(turn("s1", "l1"), vec![item], 1, Value::Null),
        );
        fake.script("tool.read",json!({"execution":{"tool_ref":{"session_id":"s1","loop_id":"l1","request_index":0,"tool_call_id":"call1"},"state":"succeeded","result_truncated":false}}));
        let result = read_turn(
            fake.client.clone(),
            TurnRef::new("s1", "l1"),
            crate::testing::LOAD.into(),
        )
        .await
        .unwrap();
        let TurnRead::Complete(content) = result else {
            panic!("canonical tool body failed: {result:?}");
        };
        assert_eq!(content.tool_facts.len(), 1);
        assert_eq!(
            content.tool_facts[0].1.execution.as_ref().unwrap().state,
            crate::protocol::ToolExecutionState::Succeeded
        );
        assert_eq!(fake.calls("tool.read").len(), 1);
        assert_eq!(fake.calls("tool.read")[0]["display"], true);
        assert!(fake.calls("tool.output").is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn w0_read_turn_rejects_an_invalid_chunk_offset() {
        let page = result_page(
            turn("s1", "l1"),
            vec![malformed_chunk("x", 1, 2, true)],
            1,
            Value::Null,
        );
        let (result, _) = read_pages(vec![page]).await;
        assert!(
            result.is_err(),
            "a discontinuous chunk must fail: {result:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn w0_read_turn_rejects_an_invalid_chunk_encoding() {
        let mut item = user(0, "one");
        item["encoding"] = json!("base64");
        let page = result_page(turn("s1", "l1"), vec![item], 1, Value::Null);
        let (result, _) = read_pages(vec![page]).await;
        assert!(
            result.is_err(),
            "an unsupported encoding must fail: {result:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn w0_read_turn_rejects_invalid_item_json() {
        let page = result_page(
            turn("s1", "l1"),
            vec![malformed_chunk("{", 0, 1, true)],
            1,
            Value::Null,
        );
        let (result, _) = read_pages(vec![page]).await;
        assert!(result.is_err(), "invalid item JSON must fail: {result:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn w0_read_turn_rejects_an_unfinished_last_item() {
        let page = result_page(
            turn("s1", "l1"),
            vec![malformed_chunk("{", 0, 2, false)],
            1,
            Value::Null,
        );
        let (result, _) = read_pages(vec![page]).await;
        assert!(
            result.is_err(),
            "an unfinished item must not be installed: {result:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn w0_read_turn_rejects_a_total_that_exceeds_the_read_items() {
        let page = result_page(turn("s1", "l1"), vec![user(0, "one")], 2, Value::Null);
        let (result, _) = read_pages(vec![page]).await;
        assert!(
            result.is_err(),
            "a short result must not be called complete: {result:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn w0_read_turn_rejects_a_gap_between_complete_item_indices() {
        let page = result_page(
            turn("s1", "l1"),
            vec![user(0, "one"), assistant(2, "three")],
            2,
            Value::Null,
        );
        let (result, _) = read_pages(vec![page]).await;
        assert!(
            result.is_err(),
            "a missing item index must fail: {result:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn w0_read_turn_rejects_another_loop() {
        let page = result_page(turn("s1", "other"), vec![user(0, "one")], 1, Value::Null);
        let (result, _) = read_pages(vec![page]).await;
        assert!(
            result.is_err(),
            "a page from another loop must fail: {result:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn w0_read_turn_rejects_another_session() {
        let page = result_page(turn("other", "l1"), vec![user(0, "one")], 1, Value::Null);
        let (result, _) = read_pages(vec![page]).await;
        assert!(
            result.is_err(),
            "a page from another session must fail: {result:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn w0_read_turn_rejects_a_different_turn_on_a_later_page() {
        let first = result_page(
            turn("s1", "l1"),
            vec![user(0, "one")],
            2,
            json!({"item": 1, "offset": 0}),
        );
        let second = result_page(
            turn("s1", "other"),
            vec![assistant(1, "two")],
            2,
            Value::Null,
        );
        let (result, _) = read_pages(vec![first, second]).await;
        assert!(
            result.is_err(),
            "a later page must belong to the requested turn: {result:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn w0_read_turn_stops_before_repeating_a_cursor() {
        let cursor = json!({"item": 1, "offset": 0});
        let first = result_page(turn("s1", "l1"), vec![user(0, "one")], 2, cursor.clone());
        let stuck = result_page(turn("s1", "l1"), vec![], 2, cursor);
        let sentinel = json!({"error": {"code": -32603, "message": "unexpected third read", "data": {"kind": "sentinel"}}});
        let (result, calls) = read_pages(vec![first, stuck, sentinel]).await;
        assert!(result.is_err(), "a repeated cursor must fail");
        assert_eq!(
            calls.len(),
            2,
            "the repeated cursor must be rejected before a third read: {calls:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_turn_item_split_across_pages_is_assembled_before_installation() {
        let item = user(0, "split answer");
        let raw = item["data"].as_str().unwrap();
        let split = raw.len() / 2;
        let first = result_page(
            turn("s1", "l1"),
            vec![malformed_chunk(&raw[..split], 0, raw.len(), false)],
            1,
            json!({"item": 0, "offset": split}),
        );
        let last = result_page(
            turn("s1", "l1"),
            vec![malformed_chunk(&raw[split..], split, raw.len(), true)],
            1,
            Value::Null,
        );
        let (result, calls) = read_pages(vec![first, last]).await;
        let TurnRead::Complete(content) = result.unwrap() else {
            panic!("expected a complete turn")
        };
        assert_eq!(content.items.len(), 1);
        assert_eq!(calls[1]["cursor"], json!({"item": 0, "offset": split}));
    }

    #[tokio::test(start_paused = true)]
    async fn a_turn_read_rejects_a_bad_second_page_instead_of_returning_its_prefix() {
        let first = result_page(
            turn("s1", "l1"),
            vec![user(0, "one")],
            2,
            json!({"item": 1, "offset": 0}),
        );
        let mut bad = assistant(1, "two");
        bad["offset"] = json!(1);
        let last = result_page(turn("s1", "l1"), vec![bad], 2, Value::Null);
        let (result, calls) = read_pages(vec![first, last]).await;
        assert!(
            result.is_err(),
            "a valid prefix must not escape a later error: {result:?}"
        );
        assert_eq!(calls.len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_turn_read_rejects_missing_required_page_fields() {
        for field in ["turn", "availability", "total", "items", "next_cursor"] {
            let mut page = result_page(turn("s1", "l1"), vec![], 0, Value::Null);
            page.as_object_mut().unwrap().remove(field);
            let (result, _) = read_pages(vec![page]).await;
            assert!(
                result.is_err(),
                "missing {field} must not be a complete result: {result:?}"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_turn_read_rejects_an_item_owned_by_another_loop() {
        let item = chunk(
            0,
            json!({"type":"user", "data": {
                "loop_id":"other", "kind":"prompt", "input":{"text":"one"}
            }}),
        );
        let (result, _) = read_pages(vec![result_page(
            turn("s1", "l1"),
            vec![item],
            1,
            Value::Null,
        )])
        .await;
        assert!(
            result.is_err(),
            "an item from another loop must not be installed: {result:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_turn_not_found_is_explicitly_unavailable() {
        let error = json!({"error": {"code": -32007, "message": "turn not found", "data": {"kind": "turn_not_found"}}});
        let (result, _) = read_pages(vec![error]).await;
        assert!(matches!(result, Ok(TurnRead::Unavailable(reason))
                if reason.reason == crate::protocol::UnavailableReason::NotRetained));
    }

    #[tokio::test(start_paused = true)]
    async fn an_oversized_item_is_an_explicit_partial_turn() {
        let item = user(0, &"x".repeat(crate::protocol::read::MAX_AUTO_ITEM_BYTES));
        let raw = item["data"].as_str().unwrap();
        let total = raw.len();
        let mut pages = Vec::new();
        for (index, bytes) in raw.as_bytes().chunks(200_000).enumerate() {
            let offset = index * 200_000;
            let end = offset + bytes.len();
            let next = if end == total {
                Value::Null
            } else {
                json!({"item":0, "offset":end})
            };
            let data = std::str::from_utf8(bytes).unwrap();
            pages.push(result_page(
                turn("s1", "l1"),
                vec![malformed_chunk(data, offset, total, end == total)],
                1,
                next,
            ));
        }
        let (result, calls) = read_pages(pages).await;
        assert!(calls.len() > 1);
        assert!(
            matches!(result, Ok(TurnRead::Partial(partial)) if matches!(*partial, ReadPartial::Oversized { index: 0, total_bytes } if total_bytes == total))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_item_that_exceeds_the_workspace_is_partial_before_decoding() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let (fake, _app) = FakeAgent::start(tx);
        let mut workspace = ReadWorkspace::acquire(&fake.client).await;
        workspace.limit = 64;
        let mut assembly = Assembly::new(0);
        let item: ReadChunk =
            serde_json::from_value(user(0, "too large for the test budget")).unwrap();
        assert_eq!(
            assembly.push(item, 1, &mut workspace).unwrap(),
            Some(ReadPartial::WorkspaceLimit)
        );
        assert!(assembly.items.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn completed_items_release_temporary_decode_reservations() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let (fake, _app) = FakeAgent::start(tx);
        let mut workspace = ReadWorkspace::acquire(&fake.client).await;
        let mut assembly = Assembly::new(0);
        assembly.reserve_items(1, &mut workspace).unwrap();
        let item: ReadChunk = serde_json::from_value(user(0, &"x".repeat(1_000))).unwrap();
        assert!(assembly.push(item, 1, &mut workspace).unwrap().is_none());
        let retained = assembly.items[0].heap_bytes()
            + assembly.items.capacity() * std::mem::size_of::<crate::protocol::RawHistoryItem>();
        assert_eq!(workspace.used, PAGE_WORKSPACE_RESERVE + retained);
    }

    #[tokio::test(start_paused = true)]
    async fn completed_reads_hold_both_workspace_slots_until_released() {
        tokio::time::timeout(TEST_TIMEOUT, async {
            let (tx, _rx) = mpsc::unbounded_channel();
            let (fake, _app) = FakeAgent::start(tx);
            fake.script(
                "turn.result",
                result_page(turn("s1", "l1"), vec![user(0, "one")], 1, Value::Null),
            );
            let first = read_turn(
                fake.client.clone(),
                TurnRef::new("s1", "l1"),
                crate::testing::LOAD.into(),
            )
            .await
            .unwrap();
            let second = read_turn(
                fake.client.clone(),
                TurnRef::new("s1", "l1"),
                crate::testing::LOAD.into(),
            )
            .await
            .unwrap();
            assert!(
                matches!(first, TurnRead::Complete(_)) && matches!(second, TurnRead::Complete(_))
            );
            assert!(fake.client.read_workspaces().try_acquire_owned().is_err());
            drop(first);
            assert!(fake.client.read_workspaces().try_acquire_owned().is_ok());
            drop(second);
        })
        .await
        .expect("workspace test timed out");
    }

    #[cfg(feature = "e2e-observe")]
    #[tokio::test(start_paused = true)]
    async fn recovery_metrics_follow_actual_queries_cancellation_and_result_workspace_ownership() {
        crate::testing::real_bounded(async {
            let (tx, _rx) = mpsc::unbounded_channel();
            let (fake, _app) = FakeAgent::start(tx);
            fake.script_held(
                "turn.result",
                result_page(turn("s1", "l1"), vec![user(0, "body")], 1, Value::Null),
            );
            let first = tokio::spawn(crate::rpc::metrics::recovery_scope(read_turn(
                fake.client.clone(),
                TurnRef::new("s1", "l1"),
                crate::testing::LOAD.into(),
            )));
            let second = tokio::spawn(crate::rpc::metrics::recovery_scope(read_turn(
                fake.client.clone(),
                TurnRef::new("s1", "l1"),
                crate::testing::LOAD.into(),
            )));
            fake.wait_for_calls("turn.result", 2).await;
            let active = fake.client.metrics.snapshot();
            assert_eq!(active.query_requests, 2);
            assert_eq!(active.read_pages, 2);
            assert_eq!(active.recovery_queries, 2);
            assert_eq!(active.recovery_pages, 2);
            assert_eq!(active.query_in_flight, 2);
            assert_eq!(active.query_peak, 2);
            assert_eq!(active.recovery_workspaces, 2);
            assert_eq!(
                active.recovery_workspace_reserved_bytes,
                2 * PAGE_WORKSPACE_RESERVE
            );
            first.abort();
            assert!(first.await.unwrap_err().is_cancelled());
            let cancelled = fake.client.metrics.snapshot();
            assert_eq!(cancelled.query_in_flight, 1);
            assert_eq!(cancelled.recovery_workspaces, 1);
            assert_eq!(
                cancelled.recovery_workspace_reserved_bytes,
                PAGE_WORKSPACE_RESERVE
            );
            fake.release("turn.result");
            let result = second.await.unwrap().unwrap();
            let TurnRead::Complete(content) = result else {
                panic!("reader failed: {result:?}");
            };
            let assembled = fake.client.metrics.snapshot();
            assert_eq!(assembled.query_in_flight, 0);
            assert_eq!(
                assembled.workspaces, 1,
                "assembled body awaiting reducer still owns its workspace"
            );
            assert!(assembled.workspace_reserved_bytes > PAGE_WORKSPACE_RESERVE);
            assert!(assembled.workspace_reserved_peak_bytes >= 2 * PAGE_WORKSPACE_RESERVE);
            drop(content);
            let settled = fake.client.metrics.snapshot();
            assert_eq!(settled.workspaces, 0);
            assert_eq!(settled.recovery_workspaces, 0);
            assert_eq!(settled.workspace_reserved_bytes, 0);
            assert_eq!(settled.recovery_workspace_reserved_bytes, 0);
            assert_eq!(settled.query_requests, 2);
            assert_eq!(settled.recovery_workspaces_peak, 2);
        })
        .await;
    }

    fn history_page(total: usize, items: Vec<Value>, next: Value) -> Value {
        json!({"session": session_info("s1"), "items": items, "next_cursor": next,
            "total": total, "records": [], "history_revision": "h1", "captured_end": 1,
            "projection": {"revision":"p1", "first_item":0, "covered_item_count":0,
                "covered_usage":{"usage":{}, "partial":true}}})
    }

    #[tokio::test(start_paused = true)]
    async fn byte_limited_catchup_keeps_a_verified_suffix_and_continues_without_rereading_it() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let (fake, _app) = FakeAgent::start(tx);
        let text = "x".repeat(200_000);
        let items = (0..8).map(|index|chunk(index,json!({"type":"user","data":{"loop_id":format!("turn-{index}"),"kind":"prompt","input":{"text":format!("item-{index} {text}")}}}))).collect();
        let fixture = history_page(8, items, Value::Null);
        fake.script(
            "session.read",
            crate::testing::Reply::result(fixture).history(),
        );
        let mut end = 8;
        for round in 0..8 {
            if end == 0 {
                break;
            }
            let mut workspace = ReadWorkspace::acquire(&fake.client).await;
            let mut budget = Some(super::HistoryBudget {
                pages: super::HISTORY_CATCHUP_MAX_PAGES,
                bytes: 1024 * 1024,
            });
            let bound =
                super::history_page_bound(&mut budget, crate::protocol::PROBE_MAX_BYTES).unwrap();
            let (pin, bytes) = super::probe(&fake.client, "s1", "canonical", bound)
                .await
                .unwrap();
            super::charge_history_bytes(&mut budget, bytes);
            let result =
                super::read_catchup(&fake.client, "s1", &pin, end, &mut workspace, &mut budget)
                    .await
                    .unwrap();
            let (items, from) = match result {
                super::WindowRead::Limited(items, _, from, through, ReadPartial::HistoryLimit) => {
                    assert_eq!(through, end);
                    assert!(
                        budget.as_ref().unwrap().pages > 0,
                        "this round exhausted bytes, not pages"
                    );
                    (items, from)
                }
                super::WindowRead::Complete(items, _) => (items, 0),
                _ => panic!("byte-bounded recovery must return its complete verified source range"),
            };
            assert_eq!(items.len(), end - from);
            assert!(
                !items.is_empty(),
                "round {round} must make real coverage progress"
            );
            for (index, item) in (from..end).zip(items) {
                let crate::protocol::read::RuntimeItem::User(item) = item.item else {
                    panic!("a user item");
                };
                assert_eq!(item.loop_id, format!("turn-{index}"));
                assert!(item.input.text.starts_with(&format!("item-{index} ")));
            }
            end = from;
        }
        assert_eq!(
            end, 0,
            "every missing range must be recoverable with bounded continuations"
        );
    }

    async fn history_pages(
        pages: Vec<Value>,
        window: Window,
    ) -> Result<HistoryRead, crate::protocol::RpcError> {
        let (tx, _rx) = mpsc::unbounded_channel();
        let (fake, _app) = FakeAgent::start(tx);
        fake.script_sequence("session.read", pages);
        tokio::time::timeout(
            TEST_TIMEOUT,
            read_history(fake.client.clone(), "s1".into(), window),
        )
        .await
        .expect("history read timed out")
    }

    #[tokio::test(start_paused = true)]
    async fn a_history_window_validates_continuations_and_rejects_a_late_error() {
        let probe = history_page(2, vec![user(0, "one")], json!({"item":1,"offset":0}));
        let first = probe.clone();
        let last = history_page(2, vec![assistant(1, "two")], Value::Null);
        let result = history_pages(vec![probe.clone(), first.clone(), last], Window::Tail)
            .await
            .unwrap();
        let HistoryRead::Complete(history) = result else {
            panic!("expected a complete history")
        };
        assert_eq!(history.items.len(), 2);
        drop(history);

        let mut bad = assistant(1, "two");
        bad["encoding"] = json!("wrong");
        let last = history_page(2, vec![bad], Value::Null);
        let result = history_pages(vec![probe, first, last], Window::Tail).await;
        assert!(
            result.is_err(),
            "a bad second page must not return its prefix: {result:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_history_window_rejects_changed_identity_or_pin() {
        let probe = history_page(1, vec![user(0, "one")], Value::Null);
        for field in ["session", "history_revision", "captured_end", "total"] {
            let mut page = probe.clone();
            match field {
                "session" => page["session"]["session_id"] = json!("other"),
                "history_revision" => page["history_revision"] = json!("h2"),
                "captured_end" => page["captured_end"] = json!(2),
                "total" => page["total"] = json!(2),
                _ => unreachable!(),
            }
            let result = history_pages(vec![probe.clone(), page], Window::Tail).await;
            assert!(result.is_err(), "changed {field} must fail: {result:?}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_history_page_rejects_a_stalled_cursor_and_an_unfinished_item() {
        let probe = history_page(1, vec![user(0, "one")], Value::Null);
        let stalled = history_page(1, vec![], json!({"item":0,"offset":0}));
        let incomplete = history_page(1, vec![malformed_chunk("{", 0, 2, false)], Value::Null);
        for page in [stalled, incomplete] {
            let result = history_pages(vec![probe.clone(), page], Window::Tail).await;
            assert!(
                result.is_err(),
                "an incomplete history page must fail: {result:?}"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_history_window_beyond_the_pin_fails_without_a_zero_progress_loop() {
        let result = history_pages(
            vec![history_page(0, vec![], Value::Null)],
            Window::Before(20),
        )
        .await;
        assert!(
            result.is_err(),
            "a range beyond the pinned total must fail: {result:?}"
        );
    }
}
