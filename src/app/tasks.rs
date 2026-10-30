//! Async operations. Each spawns, awaits the Agent and posts one message;
//! nothing here touches `App` state.

use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::sync::{OwnedSemaphorePermit, mpsc};

use super::{App, BUSY_RETRY, Msg, NoticeLevel, SessionOp};
use crate::protocol::changes::{ChangeList, DiffPage};
use crate::protocol::read::{
    InterruptedRequest, MAX_AUTO_ITEM_BYTES, RawHistoryItem, ReadChunk, RuntimeItem,
    TurnAvailability, TurnPersistence, TurnRecord,
};
use crate::protocol::{
    self, ChunkAssembler, ChunkOutcome, ModelListResult, PingResult, ProfileListResult, ReadCursor,
    ReadSessionResult, RpcError, SessionInfo, SessionListResult, SessionResult, SnapshotPin,
    TurnRef, TurnResultPage, TurnWaitResult, Usage,
};
use crate::rpc::Client;
use crate::transcript::ChangeSummary;

pub type Tx = mpsc::UnboundedSender<Msg>;

const READ_WORKSPACE_BYTES: usize = 64 * 1024 * 1024;
const PAGE_WORKSPACE_RESERVE: usize = protocol::PAGE_MAX_BYTES * 8;

#[derive(Debug)]
struct ReadWorkspace {
    _permit: OwnedSemaphorePermit,
    used: usize,
    limit: usize,
}

impl ReadWorkspace {
    async fn acquire(client: &Client) -> Self {
        Self {
            _permit: client
                .read_workspaces()
                .acquire_owned()
                .await
                .expect("reader slots stay open"),
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
        Ok(())
    }

    fn release(&mut self, bytes: usize) {
        self.used -= bytes;
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ReadPartial {
    Oversized { index: usize, total_bytes: usize },
    WorkspaceLimit,
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
        }
    }
}

#[derive(Debug)]
pub enum TurnRead {
    Pending,
    Unavailable(RpcError),
    Partial(ReadPartial),
    Complete(TurnContent),
}

#[derive(Debug)]
pub struct TurnContent {
    pub turn: TurnRef,
    pub availability: TurnAvailability,
    pub outcome: crate::protocol::LoopOutcome,
    pub persistence: TurnPersistence,
    pub items: Vec<RawHistoryItem>,
    pub interrupted: Option<InterruptedRequest>,
    _workspace: ReadWorkspace,
}

#[derive(Debug)]
pub enum HistoryRead {
    Partial(ReadPartial),
    Complete(Box<History>),
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

/// Which part of the history to read.
#[derive(Clone, Copy)]
pub enum Window {
    Tail,
    /// The display page just before this item.
    Before(usize),
    /// The canonical page just before this item, below the display floor.
    CanonicalBefore(usize),
}

impl App {
    /// Runs `work` and posts its result for the foreground session.
    pub(super) fn spawn(&self, work: impl Future<Output = SessionOp> + Send + 'static) {
        let Some(epoch) = self.session.as_ref().map(|session| session.epoch) else {
            return;
        };
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let _ = tx.send(Msg::Session(epoch, Box::new(work.await)));
        });
    }

    /// One call for the foreground session, whose id joins `fields`; `wrap` turns
    /// the result into the op to post.
    pub(super) fn call<R: DeserializeOwned + Send + 'static>(
        &self,
        method: &'static str,
        mut fields: Value,
        wrap: impl FnOnce(Result<R, RpcError>) -> SessionOp + Send + 'static,
    ) {
        if let Some(session) = self.session.as_ref() {
            fields["session_id"] = json!(session.info.session_id);
        }
        let client = self.client.clone();
        self.spawn(async move { wrap(client.call(method, &fields).await) });
    }

    /// Runs `work` and posts the message it produces, whatever session is open.
    pub(super) fn spawn_msg(&self, work: impl Future<Output = Msg> + Send + 'static) {
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let _ = tx.send(work.await);
        });
    }
}

/// Handshake, capability check and catalogs. Posts `Msg::Ready`, or
/// `Msg::AgentClosed` when the backend cannot be used.
pub async fn bootstrap(client: Arc<Client>, tx: Tx) {
    let ready = async {
        let ping: PingResult = client.call(protocol::METHOD_PING, &Value::Null).await?;
        protocol::validate_backend(&ping).map_err(|error| RpcError::closed(error.to_string()))?;
        let models: ModelListResult = client.call(protocol::METHOD_MODELS, &Value::Null).await?;
        let profiles: ProfileListResult =
            client.call(protocol::METHOD_PROFILES, &Value::Null).await?;
        let sessions: SessionListResult = client
            .call(protocol::METHOD_SESSION_LIST, &Value::Null)
            .await?;
        Ok::<_, RpcError>(super::Ready {
            models: models.models,
            profiles: profiles.profiles,
            sessions: sessions.sessions,
        })
    };
    let _ = tx.send(match ready.await {
        Ok(ready) => Msg::Ready(Box::new(ready)),
        Err(error) => Msg::AgentClosed(error.message),
    });
}

/// `session.open` or `session.create`; both answer with the session.
pub async fn session(
    client: Arc<Client>,
    method: &'static str,
    params: Value,
) -> Result<SessionInfo, RpcError> {
    let result: SessionResult = client.call(method, &params).await?;
    Ok(result.session)
}

/// Closes (when another client left it loaded) and deletes a stored session.
pub async fn delete_session(client: Arc<Client>, session_id: String) -> Msg {
    let params = json!({ "session_id": session_id });
    let result = async {
        match client
            .call::<_, Value>(protocol::METHOD_SESSION_CLOSE, &params)
            .await
        {
            Err(error) if error.code != protocol::SESSION_NOT_LOADED => return Err(error),
            _ => {}
        }
        client
            .call::<_, Value>(protocol::METHOD_SESSION_DELETE, &params)
            .await
            .map(drop)
    }
    .await;
    Msg::Deleted(session_id, result)
}

/// Waits for the turn to end. A full deferred queue or a busy session is a
/// condition, not an outcome, so the wait is registered again.
pub async fn wait_turn(client: Arc<Client>, turn: TurnRef) -> SessionOp {
    let params = json!({"session_id": turn.session_id, "loop_id": turn.loop_id});
    loop {
        match client
            .call::<_, TurnWaitResult>(protocol::METHOD_TURN_WAIT, &params)
            .await
        {
            Err(error) if error.is_capacity() || error.is_busy() => {
                tokio::time::sleep(BUSY_RETRY).await;
            }
            Ok(wait) => return SessionOp::Waited(Ok(wait)),
            // The waiter is gone but the turn may have ended: its stored result
            // ends it, so a lost wait never leaves the turn running.
            Err(error) => match stored_outcome(&client, &turn).await {
                Ok(Some(wait)) => return SessionOp::Waited(Ok(wait)),
                Ok(None) => tokio::time::sleep(BUSY_RETRY).await,
                Err(_) => return SessionOp::Waited(Err(error)),
            },
        }
    }
}

async fn stored_outcome(
    client: &Client,
    turn: &TurnRef,
) -> Result<Option<TurnWaitResult>, RpcError> {
    let params = json!({"session_id": turn.session_id, "loop_id": turn.loop_id, "limit": 1});
    let page: TurnResultPage = client.call(protocol::METHOD_TURN_RESULT, &params).await?;
    validate_page_chunks(&page.items, protocol::PAGE_MAX_BYTES)?;
    if page.turn != *turn {
        return Err(invalid_read("turn identity changed"));
    }
    if page.availability == TurnAvailability::Pending {
        if !page.items.is_empty()
            || page.next_cursor.is_some()
            || page.total != 0
            || page.outcome.is_some()
            || page.persistence.is_some()
            || page.interrupted.is_some()
        {
            return Err(invalid_read("pending turn contains completed result data"));
        }
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

fn invalid_read(reason: impl Into<String>) -> RpcError {
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
    let mut counter = JsonCounter::default();
    serde_json::to_writer(&mut counter, value).expect("decoded metadata serializes");
    let bytes = counter
        .bytes
        .saturating_mul(2)
        .saturating_add(counter.structures.saturating_add(1).saturating_mul(512));
    workspace.charge(bytes)
}

/// Reads one history window and retries once when its pin becomes stale.
pub async fn read_history(
    client: Arc<Client>,
    session_id: String,
    window: Window,
) -> Result<HistoryRead, RpcError> {
    let mut workspace = ReadWorkspace::acquire(&client).await;
    let read = async |workspace: &mut ReadWorkspace| -> Result<_, RpcError> {
        let canonical = matches!(window, Window::CanonicalBefore(_));
        let view = if canonical { "canonical" } else { "display" };
        let pin = probe(&client, &session_id, view).await?;
        if let Err(partial) = charge_metadata(workspace, &pin) {
            return Ok((WindowRead::Partial(partial), 0, 0, pin, canonical));
        }
        let (start, end) = match window {
            Window::Tail => (
                pin.total
                    .saturating_sub(protocol::TAIL_ITEMS)
                    .max(pin.first_item),
                pin.total,
            ),
            Window::Before(end) => (
                end.saturating_sub(protocol::PAGE_LIMIT).max(pin.first_item),
                end,
            ),
            Window::CanonicalBefore(end) => (end.saturating_sub(protocol::PAGE_LIMIT), end),
        };
        if end > pin.total {
            return Err(invalid_read("history window exceeds its pinned total"));
        }
        let result = read_window(&client, &session_id, view, &pin, start, end, workspace).await?;
        Ok((result, start, end, pin, canonical))
    };
    let result = match read(&mut workspace).await {
        Err(error) if error.is_stale_pin() => {
            workspace.used = PAGE_WORKSPACE_RESERVE;
            read(&mut workspace).await
        }
        result => result,
    }?;
    let (result, start, end, pin, canonical) = result;
    match result {
        WindowRead::Partial(partial) => Ok(HistoryRead::Partial(partial)),
        WindowRead::Complete(items, records) => Ok(HistoryRead::Complete(Box::new(History {
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
        }))),
    }
}

/// A one-item read that pins the snapshot the window is read from.
async fn probe(client: &Client, session_id: &str, view: &str) -> Result<SnapshotPin, RpcError> {
    let params = json!({
        "session_id": session_id,
        "limit": protocol::PROBE_LIMIT,
        "max_bytes": protocol::PROBE_MAX_BYTES,
        "view": view,
    });
    let page: ReadSessionResult = client.call(protocol::METHOD_SESSION_READ, &params).await?;
    validate_page_chunks(&page.items, protocol::PROBE_MAX_BYTES)?;
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
    Ok(page.pin())
}

enum WindowRead {
    Complete(Vec<RawHistoryItem>, Vec<TurnRecord>),
    Partial(ReadPartial),
}

async fn read_window(
    client: &Client,
    session_id: &str,
    view: &str,
    pin: &SnapshotPin,
    start: usize,
    end: usize,
    workspace: &mut ReadWorkspace,
) -> Result<WindowRead, RpcError> {
    let mut assembly = Assembly::new(start);
    if let Err(partial) = assembly.reserve_items(end.saturating_sub(start), workspace) {
        return Ok(WindowRead::Partial(partial));
    }
    let mut records: Vec<TurnRecord> = Vec::new();
    while assembly.position.item < end {
        let position = assembly.position;
        let mut params = json!({
            "session_id": session_id,
            "limit": protocol::PAGE_LIMIT.min(end - position.item),
            "max_bytes": protocol::PAGE_MAX_BYTES,
            "view": view,
            "cursor": position.value(),
            "captured_end": pin.captured_end,
            "history_revision": pin.history_revision,
        });
        if let Some(projection) = &pin.projection_revision {
            params["projection_revision"] = json!(projection);
        }
        let page: ReadSessionResult = client.call(protocol::METHOD_SESSION_READ, &params).await?;
        validate_page_chunks(&page.items, protocol::PAGE_MAX_BYTES)?;
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

/// Reads and validates a turn before returning any installable content.
pub async fn read_turn(client: Arc<Client>, turn: TurnRef) -> Result<TurnRead, RpcError> {
    let mut workspace = ReadWorkspace::acquire(&client).await;
    let mut assembly = Assembly::new(0);
    let mut expected: Option<(
        usize,
        crate::protocol::LoopOutcome,
        TurnPersistence,
        Option<InterruptedRequest>,
    )> = None;
    loop {
        let position = assembly.position;
        let mut params = json!({
            "session_id": turn.session_id,
            "loop_id": turn.loop_id,
            "limit": protocol::PAGE_LIMIT,
            "max_bytes": protocol::PAGE_MAX_BYTES,
        });
        if position != ReadCursor::start() {
            params["cursor"] = position.value();
        }
        let page: TurnResultPage = match client.call(protocol::METHOD_TURN_RESULT, &params).await {
            Err(error) if error.kind == "turn_not_found" => {
                return Ok(TurnRead::Unavailable(error));
            }
            result => result?,
        };
        validate_page_chunks(&page.items, protocol::PAGE_MAX_BYTES)?;
        if page.turn != turn {
            return Err(invalid_read("turn identity changed"));
        }
        if page.availability == TurnAvailability::Pending {
            if expected.is_some()
                || !page.items.is_empty()
                || page.next_cursor.is_some()
                || page.outcome.is_some()
                || page.persistence.is_some()
                || page.interrupted.is_some()
                || page.total != 0
            {
                return Err(invalid_read("pending turn contains completed result data"));
            }
            return Ok(TurnRead::Pending);
        }
        let outcome = page
            .outcome
            .ok_or_else(|| invalid_read("completed turn outcome is missing"))?;
        let persistence = page
            .persistence
            .ok_or_else(|| invalid_read("completed turn persistence is missing"))?;
        let metadata = (page.total, outcome, persistence, page.interrupted);
        if let Some(expected) = &expected {
            if expected != &metadata {
                return Err(invalid_read("turn metadata changed between pages"));
            }
        } else {
            if let Err(partial) =
                charge_metadata(&mut workspace, &(&turn, &metadata.1, &metadata.3))
            {
                return Ok(TurnRead::Partial(partial));
            }
            if let Err(partial) = assembly.reserve_items(page.total, &mut workspace) {
                return Ok(TurnRead::Partial(partial));
            }
            expected = Some(metadata);
        }
        for chunk in page.items {
            if let Some(partial) = assembly.push(chunk, page.total, &mut workspace)? {
                return Ok(TurnRead::Partial(partial));
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
                return Ok(TurnRead::Partial(partial));
            }
            let (_, outcome, persistence, interrupted) =
                expected.expect("completed pages have metadata");
            return Ok(TurnRead::Complete(TurnContent {
                turn,
                availability: page.availability,
                outcome,
                persistence,
                items: assembly.items,
                interrupted,
                _workspace: workspace,
            }));
        }
    }
}

/// How many files a turn changed and, when every diff can be counted, its
/// added and removed lines.
pub async fn turn_changes(
    client: Arc<Client>,
    session_id: String,
    loop_id: String,
) -> Result<ChangeSummary, RpcError> {
    let params = json!({
        "session_id": session_id,
        "scope": {"turn": {"loop_id": loop_id}},
        "limit": 100,
        "max_bytes": 65_536,
    });
    let list: ChangeList = client.call(protocol::METHOD_CHANGES_LIST, &params).await?;
    let mut lines = Some((0, 0));
    for record in &list.records {
        let Some((added, removed)) = lines else {
            break;
        };
        lines = None;
        if record.details_available {
            let page = read_diff(&client, &session_id, &record.change_ref).await?;
            let available = page
                .availability
                .as_deref()
                .is_none_or(|state| state == "available");
            // An uncountable record makes the total unknown rather than partial.
            if available && !page.binary && !page.truncated && !page.stale {
                let (more, fewer) = protocol::changes::count_lines(std::slice::from_ref(&page));
                lines = Some((added + more, removed + fewer));
            }
        }
    }
    Ok(ChangeSummary {
        files: list.files(),
        lines,
    })
}

/// Pages of one path's change records, one merged page per record.
pub async fn read_diffs(
    client: Arc<Client>,
    session_id: String,
    change_refs: Vec<String>,
) -> Result<Vec<DiffPage>, RpcError> {
    let mut pages = Vec::with_capacity(change_refs.len());
    for change_ref in change_refs {
        pages.push(read_diff(&client, &session_id, &change_ref).await?);
    }
    Ok(pages)
}

/// One change record's diff, paged to the end; a capacity refusal is retried
/// briefly.
async fn read_diff(
    client: &Client,
    session_id: &str,
    change_ref: &str,
) -> Result<DiffPage, RpcError> {
    let mut hunks = Vec::new();
    let mut cursor: Option<Value> = None;
    loop {
        let params = json!({
            "session_id": session_id,
            "change_ref": change_ref,
            "cursor": cursor,
            "max_bytes": protocol::PAGE_MAX_BYTES,
        });
        let mut attempt = 0;
        let mut page: DiffPage = loop {
            match client.call(protocol::METHOD_CHANGES_DIFF, &params).await {
                Err(error) if error.is_capacity() && attempt < DIFF_RETRIES => {
                    attempt += 1;
                    tokio::time::sleep(BUSY_RETRY * attempt).await;
                }
                result => break result?,
            }
        };
        hunks.append(&mut page.hunks);
        cursor.clone_from(&page.next_cursor);
        if cursor.is_none() {
            page.hunks = hunks;
            return Ok(page);
        }
    }
}

const DIFF_RETRIES: u32 = 2;

/// Writes the canonical conversation as Markdown and reports where.
pub async fn export(client: Arc<Client>, session_id: String, path: Option<String>) -> Msg {
    let path = path.unwrap_or_else(|| default_export_path(&session_id));
    let written = async {
        let mut workspace = ReadWorkspace::acquire(&client).await;
        let items = async |workspace: &mut ReadWorkspace| {
            let pin = probe(&client, &session_id, "canonical").await?;
            read_window(
                &client,
                &session_id,
                "canonical",
                &pin,
                0,
                pin.total,
                workspace,
            )
            .await
        };
        let items = match items(&mut workspace).await {
            Err(error) if error.is_stale_pin() => {
                workspace.used = PAGE_WORKSPACE_RESERVE;
                items(&mut workspace).await
            }
            result => result,
        }
        .map_err(|error| error.message)?;
        let items = match items {
            WindowRead::Complete(items, _) => items,
            WindowRead::Partial(partial) => return Err(partial.message()),
        };
        std::fs::write(&path, crate::transcript::to_markdown(&items))
            .map_err(|error| format!("{path}: {error}"))
    };
    match written.await {
        Ok(()) => Msg::Notice(NoticeLevel::Info, format!("Exported to {path}")),
        Err(error) => Msg::Notice(NoticeLevel::Error, error),
    }
}

/// `minicore-<session>-<date>.md`, in the directory the user started from.
fn default_export_path(session_id: &str) -> String {
    let short = session_id.strip_prefix("ses_").unwrap_or(session_id);
    format!("minicore-{short}-{}.md", today())
}

/// Today as `YYYY-MM-DD` in UTC.
fn today() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let (year, month, day) = civil_from_days((seconds / 86_400) as i64);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Days since the Unix epoch to a civil date (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::{Value, json};
    use tokio::sync::mpsc;

    use super::{
        Assembly, HistoryRead, PAGE_WORKSPACE_RESERVE, ReadPartial, ReadWorkspace, TurnRead,
        Window, read_history, read_turn,
    };
    use crate::protocol::{ReadChunk, TurnRef};
    use crate::testing::{FakeAgent, chunk, session_info};

    const TEST_TIMEOUT: Duration = Duration::from_secs(10);

    fn result_page(turn: Value, items: Vec<Value>, total: usize, next: Value) -> Value {
        json!({
            "turn": turn,
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
            read_turn(fake.client.clone(), TurnRef::new("s1", "l1")),
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
    async fn w0_read_turn_accepts_complete_pages_and_an_empty_result() {
        let first = result_page(
            turn("s1", "l1"),
            vec![user(0, "one")],
            2,
            json!({"item": 1, "offset": 0}),
        );
        let last = result_page(turn("s1", "l1"), vec![assistant(1, "two")], 2, Value::Null);
        let (result, calls) = read_pages(vec![first, last]).await;
        let TurnRead::Complete(content) = result.expect("complete pages must be readable") else {
            panic!("a complete result was expected");
        };
        assert_eq!(content.items.len(), 2);
        assert!(content.interrupted.is_none());
        drop(content);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1]["cursor"], json!({"item": 1, "offset": 0}));

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
        let (result, _) = read_pages(vec![pending]).await;
        assert!(
            matches!(result, Ok(TurnRead::Pending)),
            "pending must not be returned as a complete turn: {result:?}"
        );
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
        assert!(
            matches!(result, Ok(TurnRead::Unavailable(error)) if error.kind == "turn_not_found")
        );
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
            matches!(result, Ok(TurnRead::Partial(ReadPartial::Oversized { index: 0, total_bytes })) if total_bytes == total)
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
            let first = read_turn(fake.client.clone(), TurnRef::new("s1", "l1"))
                .await
                .unwrap();
            let second = read_turn(fake.client.clone(), TurnRef::new("s1", "l1"))
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

    fn history_page(total: usize, items: Vec<Value>, next: Value) -> Value {
        json!({"session": session_info("s1"), "items": items, "next_cursor": next,
            "total": total, "records": [], "history_revision": "h1", "captured_end": 1,
            "projection": {"revision":"p1", "first_item":0, "covered_item_count":0,
                "covered_usage":{"usage":{}, "partial":true}}})
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

    #[test]
    fn the_default_export_path_names_the_session_and_the_day() {
        assert_eq!(super::civil_from_days(0), (1970, 1, 1));
        assert_eq!(super::civil_from_days(20_454), (2026, 1, 1));
        let path = super::default_export_path("ses_abc123");
        assert!(path.starts_with("minicore-abc123-20"), "{path}");
        assert!(path.ends_with(".md"), "{path}");
    }
}
