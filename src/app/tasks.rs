//! Async operations. Each function performs one RPC exchange (or a small chain)
//! and posts exactly one [`OpDone`]; nothing here mutates `App`.

use std::sync::Arc;

use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::{BUSY_RETRY, Msg, OpDone, OpKind};
use crate::protocol::{
    self, CompactionResult, ModelListResult, OkResult, PingResult, ProfileListResult,
    ReadSessionResult, SessionContext, SessionInfo, SessionListResult, SessionPresentation,
    SessionResult, SessionState, SessionUpdateResult, SteerResult, ToolRef, TurnRef,
    TurnResultPage, TurnSendResult, TurnWaitResult, changes::ChangeList, tool::ToolOutputPage,
    tool::ToolReadResult,
};
use crate::rpc::Client;
use crate::transcript::ChangeSummary;

pub type Tx = mpsc::UnboundedSender<Msg>;

fn post(tx: &Tx, session: Option<&str>, kind: OpKind) {
    let _ = tx.send(Msg::Op(Box::new(OpDone {
        session: session.map(str::to_owned),
        epoch: 0,
        kind,
    })));
}

/// One assembled history page.
pub struct History {
    /// Source item index paired with its decoded body, in ascending order.
    pub items: Vec<(usize, protocol::read::RawHistoryItem)>,
    pub from: usize,
    pub to: usize,
    /// The display projection's `first_item`: the boundary the Agent itself
    /// reports, which is where the visible history starts. It is not the same
    /// as `from`, the first item this page happens to contain.
    pub floor: usize,
    pub has_more: bool,
}

/// Handshake, capability check and catalogs. Posts `Msg::Ready`, or a terminal
/// `Msg::AgentClosed` when the backend cannot be used.
pub async fn bootstrap(client: Arc<Client>, tx: Tx) {
    let ping = match client
        .call::<Value, PingResult>(protocol::METHOD_PING, &Value::Null)
        .await
    {
        Ok(ping) => ping,
        Err(error) => return closed(&tx, error.message),
    };
    if let Err(error) = protocol::validate_backend(&ping) {
        return closed(&tx, error.to_string());
    }
    let models = match client
        .call::<Value, ModelListResult>(protocol::METHOD_MODELS, &Value::Null)
        .await
    {
        Ok(list) => list.models,
        Err(error) => return closed(&tx, error.message),
    };
    let profiles = match client
        .call::<Value, ProfileListResult>(protocol::METHOD_PROFILES, &Value::Null)
        .await
    {
        Ok(list) => list.profiles,
        Err(error) => return closed(&tx, error.message),
    };
    let sessions = match list_sessions(client.as_ref()).await {
        Ok(sessions) => sessions,
        Err(error) => return closed(&tx, error.message),
    };
    let _ = tx.send(Msg::Ready(Box::new(super::Ready {
        models,
        profiles,
        sessions,
    })));
}

fn closed(tx: &Tx, reason: String) {
    let _ = tx.send(Msg::AgentClosed(reason));
}

pub async fn list_sessions(client: &Client) -> Result<Vec<SessionInfo>, protocol::RpcError> {
    let list: SessionListResult = client
        .call::<Value, SessionListResult>(protocol::METHOD_SESSION_LIST, &Value::Null)
        .await?;
    Ok(list.sessions)
}

pub fn reload_sessions(client: Arc<Client>, tx: Tx) {
    tokio::spawn(async move {
        match list_sessions(client.as_ref()).await {
            Ok(sessions) => {
                let _ = tx.send(Msg::Sessions(sessions));
            }
            Err(error) => {
                let _ = tx.send(Msg::Notice(
                    crate::transcript::block::NoticeLevel::Error,
                    error.message,
                ));
            }
        }
    });
}

pub fn open_session(client: Arc<Client>, tx: Tx, session_id: String) {
    tokio::spawn(async move {
        let params = json!({ "session_id": session_id });
        let result: Result<SessionResult, _> = client
            .call::<Value, SessionResult>(protocol::METHOD_SESSION_OPEN, &params)
            .await;
        post(
            &tx,
            Some(&session_id),
            OpKind::Opened(result.map(|result| result.session)),
        );
    });
}

pub fn create_session(client: Arc<Client>, tx: Tx, params: Value) {
    tokio::spawn(async move {
        let result: Result<SessionResult, _> = client
            .call::<Value, SessionResult>(protocol::METHOD_SESSION_CREATE, &params)
            .await;
        post(
            &tx,
            None,
            OpKind::Created(result.map(|result| result.session)),
        );
    });
}

/// Closes (if the Agent still holds it) and deletes one stored session. The
/// picker only deletes sessions the TUI is not running, so a close here is
/// normally a no-op; it is what makes delete work for a session another client
/// left loaded.
pub fn delete_session(client: Arc<Client>, tx: Tx, session_id: String) {
    tokio::spawn(async move {
        let params = json!({ "session_id": session_id });
        let result = async {
            match client
                .call::<Value, OkResult>(protocol::METHOD_SESSION_CLOSE, &params)
                .await
            {
                // A closed row is what the picker deletes, so this is normal.
                Ok(_) => {}
                Err(error) if error.code == protocol::SESSION_NOT_LOADED => {}
                Err(error) => return Err(error),
            }
            client
                .call::<Value, OkResult>(protocol::METHOD_SESSION_DELETE, &params)
                .await
                .map(|_| ())
        }
        .await;
        post(&tx, None, OpKind::Deleted(session_id, result));
    });
}

pub fn session_state(client: Arc<Client>, tx: Tx, session_id: String, epoch: u64) {
    tokio::spawn(async move {
        let params = json!({ "session_id": session_id });
        let result = client
            .call::<Value, SessionState>(protocol::METHOD_SESSION_STATE, &params)
            .await;
        let _ = tx.send(Msg::Op(Box::new(OpDone {
            session: Some(session_id),
            epoch,
            kind: OpKind::State(result),
        })));
    });
}

pub fn presentation(client: Arc<Client>, tx: Tx, session_id: String, epoch: u64) {
    tokio::spawn(async move {
        let params = json!({ "session_id": session_id });
        let result = client
            .call::<Value, SessionPresentation>(protocol::METHOD_SESSION_PRESENTATION, &params)
            .await;
        let _ = tx.send(Msg::Op(Box::new(OpDone {
            session: Some(session_id),
            epoch,
            kind: OpKind::Presentation(result),
        })));
    });
}

pub fn context(client: Arc<Client>, tx: Tx, session_id: String) {
    tokio::spawn(async move {
        let params = json!({ "session_id": session_id });
        let result = client
            .call::<Value, SessionContext>(protocol::METHOD_SESSION_CONTEXT, &params)
            .await;
        post(&tx, Some(&session_id), OpKind::Context(result));
    });
}

pub fn rename(client: Arc<Client>, tx: Tx, session_id: String, title: String) {
    tokio::spawn(async move {
        let params = json!({"session_id": session_id, "title": title});
        let result: Result<SessionResult, _> = client
            .call::<Value, SessionResult>(protocol::METHOD_SESSION_RENAME, &params)
            .await;
        post(
            &tx,
            Some(&session_id),
            OpKind::Renamed(result.map(|result| result.session)),
        );
    });
}

pub fn update_session(client: Arc<Client>, tx: Tx, params: Value, session_id: String) {
    tokio::spawn(async move {
        let result: Result<SessionUpdateResult, _> = client
            .call::<Value, SessionUpdateResult>(protocol::METHOD_SESSION_UPDATE, &params)
            .await;
        post(
            &tx,
            Some(&session_id),
            OpKind::Updated(result.map(|result| result.session)),
        );
    });
}

pub fn send_turn(client: Arc<Client>, tx: Tx, session_id: String, text: String, epoch: u64) {
    tokio::spawn(async move {
        let params = json!({"session_id": session_id, "text": text});
        let result = client
            .call::<Value, TurnSendResult>(protocol::METHOD_TURN_SEND, &params)
            .await
            .map(|sent| sent.turn);
        let _ = tx.send(Msg::Op(Box::new(OpDone {
            session: Some(session_id),
            epoch,
            kind: OpKind::Sent(text, result),
        })));
    });
}

/// Waits for the turn to end. A full deferred queue or a busy session is a
/// condition, not an outcome, so the wait is registered again.
pub fn wait_turn(client: Arc<Client>, tx: Tx, turn: TurnRef, epoch: u64) {
    tokio::spawn(async move {
        let params = json!({"session_id": turn.session_id, "loop_id": turn.loop_id});
        let result = loop {
            match client
                .call::<Value, TurnWaitResult>(protocol::METHOD_TURN_WAIT, &params)
                .await
            {
                Err(error) if error.is_capacity() || error.is_busy() => {
                    tokio::time::sleep(BUSY_RETRY).await;
                }
                result => break result,
            }
        };
        let _ = tx.send(Msg::Op(Box::new(OpDone {
            session: Some(turn.session_id),
            epoch,
            kind: OpKind::Waited(Box::new(result)),
        })));
    });
}

pub fn steer(client: Arc<Client>, tx: Tx, turn: TurnRef, text: String) {
    tokio::spawn(async move {
        let params = json!({
            "session_id": turn.session_id.clone(),
            "loop_id": turn.loop_id.clone(),
            "text": text,
        });
        let result: Result<SteerResult, _> = client
            .call::<Value, SteerResult>(protocol::METHOD_TURN_STEER, &params)
            .await;
        let _ = tx.send(Msg::Op(Box::new(OpDone {
            session: Some(turn.session_id),
            epoch: 0,
            kind: OpKind::Steered(result.map(|result| result.steer_index.unwrap_or(0))),
        })));
    });
}

pub fn cancel_turn(client: Arc<Client>, tx: Tx, turn: TurnRef) {
    tokio::spawn(async move {
        let params = json!({"session_id": turn.session_id, "loop_id": turn.loop_id});
        let result: Result<protocol::CancelledResult, _> = client
            .call::<Value, protocol::CancelledResult>(protocol::METHOD_TURN_CANCEL, &params)
            .await;
        let _ = tx.send(Msg::Op(Box::new(OpDone {
            session: Some(turn.session_id),
            epoch: 0,
            kind: OpKind::Cancelled(result.map(|_| ())),
        })));
    });
}

pub fn answer(client: Arc<Client>, tx: Tx, params: Value, session_id: String) {
    tokio::spawn(async move {
        let result: Result<OkResult, _> = client
            .call::<Value, OkResult>(protocol::METHOD_INTERACTION_ANSWER, &params)
            .await;
        post(&tx, Some(&session_id), OpKind::Answered(result.map(|_| ())));
    });
}

pub fn compact(client: Arc<Client>, tx: Tx, session_id: String, operation_id: String) {
    tokio::spawn(async move {
        let params = json!({"session_id": session_id, "operation_id": operation_id});
        let result: Result<CompactionResult, _> = client
            .call::<Value, CompactionResult>(protocol::METHOD_SESSION_COMPACT, &params)
            .await;
        post(&tx, Some(&session_id), OpKind::Compacted(result));
    });
}

/// Reads the tail of the display projection. A fresh probe always comes first,
/// so a pin is never cached across operations.
pub fn load_tail(client: Arc<Client>, tx: Tx, session_id: String) {
    tokio::spawn(async move {
        let result = tail(client.as_ref(), &session_id).await;
        post(&tx, Some(&session_id), OpKind::History(result));
    });
}

/// The tail window, retried once with a fresh pin: a page continuation can be
/// refused with a stale pin while the Agent is appending, and restarting with
/// a new probe is the documented recovery.
async fn tail(client: &Client, session_id: &str) -> Result<History, protocol::RpcError> {
    match read_tail(client, session_id).await {
        Err(error) if error.code == protocol::INVALID_STATE => read_tail(client, session_id).await,
        result => result,
    }
}

async fn read_tail(client: &Client, session_id: &str) -> Result<History, protocol::RpcError> {
    let probe = probe(client, session_id, "display").await?;
    let total = probe.total;
    let start = total
        .saturating_sub(protocol::TAIL_ITEMS)
        .max(probe.first_item);
    read_window(client, session_id, "display", probe, start, total).await
}

/// Reads the page immediately older than `before`.
pub fn load_older(client: Arc<Client>, tx: Tx, session_id: String, before: usize) {
    tokio::spawn(async move {
        let result = older(client.as_ref(), &session_id, before).await;
        post(&tx, Some(&session_id), OpKind::Older(result));
    });
}

/// An older page, retried once with a fresh pin: a turn appending while the page
/// is read moves the captured end, and a refused pin is the documented signal to
/// probe again rather than to give up on what is already loaded.
async fn older(
    client: &Client,
    session_id: &str,
    before: usize,
) -> Result<History, protocol::RpcError> {
    match read_older(client, session_id, before).await {
        Err(error) if error.code == protocol::INVALID_STATE => {
            read_older(client, session_id, before).await
        }
        result => result,
    }
}

async fn read_older(
    client: &Client,
    session_id: &str,
    before: usize,
) -> Result<History, protocol::RpcError> {
    let probe = probe(client, session_id, "display").await?;
    let start = before
        .saturating_sub(protocol::PAGE_LIMIT)
        .max(probe.first_item);
    read_window(client, session_id, "display", probe, start, before).await
}

/// Reads the page immediately older than `before` in the canonical view: the
/// items the display projection collapsed into the compaction summary. A fresh
/// pin with its own boundary is required, because the canonical history has
/// items below the display `first_item`.
pub fn load_canonical(client: Arc<Client>, tx: Tx, session_id: String, before: usize) {
    tokio::spawn(async move {
        let result = canonical(client.as_ref(), &session_id, before).await;
        post(&tx, Some(&session_id), OpKind::Canonical(result));
    });
}

/// A page below the display floor, retried once with a fresh pin like the
/// display pages: the same stale-pin race applies to the canonical view.
async fn canonical(
    client: &Client,
    session_id: &str,
    before: usize,
) -> Result<History, protocol::RpcError> {
    match read_canonical(client, session_id, before).await {
        Err(error) if error.code == protocol::INVALID_STATE => {
            read_canonical(client, session_id, before).await
        }
        result => result,
    }
}

async fn read_canonical(
    client: &Client,
    session_id: &str,
    before: usize,
) -> Result<History, protocol::RpcError> {
    let probe = probe(client, session_id, "canonical").await?;
    let start = before.saturating_sub(protocol::PAGE_LIMIT);
    read_window(client, session_id, "canonical", probe, start, before).await
}

/// Result of the one-item probe that establishes a pin and the window bounds.
#[derive(Clone)]
struct Probe {
    pin: protocol::SnapshotPin,
    total: usize,
    first_item: usize,
}

async fn probe(client: &Client, session_id: &str, view: &str) -> Result<Probe, protocol::RpcError> {
    let params = json!({
        "session_id": session_id,
        "limit": protocol::PROBE_LIMIT,
        "max_bytes": protocol::PROBE_MAX_BYTES,
        "view": view,
    });
    let page: ReadSessionResult = client
        .call::<Value, ReadSessionResult>(protocol::METHOD_SESSION_READ, &params)
        .await?;
    let pin = page.pin();
    Ok(Probe {
        total: pin.total,
        first_item: pin.first_item,
        pin,
    })
}

/// Reads `[start, end)` and assembles every complete item in it.
async fn read_window(
    client: &Client,
    session_id: &str,
    view: &str,
    probe: Probe,
    start: usize,
    end: usize,
) -> Result<History, protocol::RpcError> {
    let mut assembler = protocol::ChunkAssembler::new();
    let mut items = Vec::new();
    let mut cursor = Some(protocol::ReadCursor {
        item: start,
        offset: 0,
    });
    while let Some(position) = cursor {
        if position.item >= end {
            break;
        }
        let mut params = json!({
            "session_id": session_id,
            "limit": protocol::PAGE_LIMIT,
            "max_bytes": protocol::PAGE_MAX_BYTES,
            "view": view,
            "cursor": position.value(),
            "captured_end": probe.pin.captured_end,
            "history_revision": probe.pin.history_revision,
        });
        if let Some(projection) = &probe.pin.projection_revision {
            params["projection_revision"] = json!(projection);
        }
        let page: ReadSessionResult = client
            .call::<Value, ReadSessionResult>(protocol::METHOD_SESSION_READ, &params)
            .await?;
        let mut advanced = false;
        for chunk in page.items {
            let index = chunk.index;
            match assembler.push(chunk) {
                Ok(protocol::ChunkOutcome::Item(item)) => {
                    if index < end
                        && let Ok(decoded) = protocol::read::decode_item(&item.data)
                    {
                        items.push((index, decoded));
                    }
                    advanced = true;
                }
                Ok(protocol::ChunkOutcome::Pending | protocol::ChunkOutcome::Oversized { .. }) => {}
                Err(_) => {
                    advanced = true;
                }
            }
        }
        let next = page
            .next_cursor
            .as_ref()
            .and_then(protocol::ReadCursor::from_value);
        cursor = next;
        if !advanced && cursor.is_none() {
            break;
        }
    }
    Ok(History {
        from: start,
        to: end,
        floor: probe.first_item,
        has_more: start > probe.first_item,
        items,
    })
}

/// Re-reads one turn's durable items, used after a gap or a non-completed
/// outcome. The live region is replaced by these.
pub fn load_turn_result(client: Arc<Client>, tx: Tx, turn: TurnRef, epoch: u64) {
    tokio::spawn(async move {
        let session_id = turn.session_id.clone();
        let result = read_turn_result(client.as_ref(), &turn).await;
        let _ = tx.send(Msg::Op(Box::new(OpDone {
            session: Some(session_id),
            epoch,
            kind: OpKind::TurnItems(result),
        })));
    });
}

async fn read_turn_result(
    client: &Client,
    turn: &TurnRef,
) -> Result<Vec<(usize, protocol::read::RawHistoryItem)>, protocol::RpcError> {
    let mut assembler = protocol::ChunkAssembler::new();
    let mut items = Vec::new();
    let mut cursor: Option<Value> = None;
    loop {
        let mut params = json!({
            "session_id": turn.session_id,
            "loop_id": turn.loop_id,
            "limit": protocol::PAGE_LIMIT,
            "max_bytes": protocol::PAGE_MAX_BYTES,
        });
        if let Some(cursor) = cursor.take() {
            params["cursor"] = cursor;
        }
        let page: TurnResultPage = client
            .call::<Value, TurnResultPage>(protocol::METHOD_TURN_RESULT, &params)
            .await?;
        for chunk in page.items {
            let index = chunk.index;
            match assembler.push(chunk) {
                Ok(protocol::ChunkOutcome::Item(item)) => {
                    if let Ok(decoded) = protocol::read::decode_item(&item.data) {
                        items.push((index, decoded));
                    }
                }
                Ok(protocol::ChunkOutcome::Pending | protocol::ChunkOutcome::Oversized { .. }) => {}
                Err(_) => break,
            }
        }
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    Ok(items)
}

/// One turn's change list and its line counts, for the turn-end summary line.
pub fn load_changes(client: Arc<Client>, tx: Tx, session_id: String, loop_id: String) {
    tokio::spawn(async move {
        let result = session_changes(client.as_ref(), &session_id, &loop_id).await;
        post(&tx, Some(&session_id), OpKind::Changes(loop_id, result));
    });
}

async fn session_changes(
    client: &Client,
    session_id: &str,
    loop_id: &str,
) -> Result<ChangeSummary, protocol::RpcError> {
    let params = json!({
        "session_id": session_id,
        "scope": {"turn": {"loop_id": loop_id}},
        "limit": 100,
        "max_bytes": 65536,
    });
    let list: ChangeList = client
        .call::<Value, ChangeList>(protocol::METHOD_CHANGES_LIST, &params)
        .await?;
    let files = list.files();
    if files == 0 {
        return Ok(ChangeSummary {
            files: 0,
            lines: Some((0, 0)),
        });
    }
    // Line counts come from the per-file diffs. Each record is counted on its
    // own, because line indexes are per file. One uncountable record makes the
    // pair unknown: a partial sum would read as if only some files changed.
    let (mut added, mut removed) = (0, 0);
    for record in &list.records {
        if !record.details_available {
            return Ok(ChangeSummary { files, lines: None });
        }
        let Some((record_added, record_removed)) =
            read_diff(client, session_id, &record.change_ref).await?
        else {
            return Ok(ChangeSummary { files, lines: None });
        };
        added += record_added;
        removed += record_removed;
    }
    Ok(ChangeSummary {
        files,
        lines: Some((added, removed)),
    })
}

/// Counts one file's added and removed lines, paging its diff to exhaustion.
/// `None` means the diff exists but cannot be counted (unavailable, partial,
/// truncated, stale or binary), which is reported as an unknown count rather
/// than as zero. A capacity refusal is retried briefly: the Agent admits a
/// bounded number of concurrent reads and other clients share that allowance.
async fn read_diff(
    client: &Client,
    session_id: &str,
    change_ref: &str,
) -> Result<Option<(usize, usize)>, protocol::RpcError> {
    let mut pages = Vec::new();
    let mut cursor: Option<Value> = None;
    loop {
        let params = json!({
            "session_id": session_id,
            "change_ref": change_ref,
            "cursor": cursor,
            "max_bytes": protocol::PAGE_MAX_BYTES,
        });
        let mut attempt = 0;
        let page = loop {
            let result = client
                .call::<Value, protocol::changes::DiffPage>(protocol::METHOD_CHANGES_DIFF, &params)
                .await;
            match result {
                Ok(page) => break page,
                Err(error) if error.is_capacity() && attempt < DIFF_RETRIES => {
                    attempt += 1;
                    tokio::time::sleep(BUSY_RETRY * attempt).await;
                }
                Err(error) => return Err(error),
            }
        };
        let unavailable = page
            .availability
            .as_deref()
            .is_some_and(|state| state != "available");
        if page.binary || unavailable || page.truncated || page.stale {
            return Ok(None);
        }
        cursor = page.next_cursor.clone();
        pages.push(page);
        if cursor.is_none() {
            return Ok(Some(protocol::changes::count_lines(&pages)));
        }
    }
}

/// Attempts at one diff page before a capacity refusal is reported.
const DIFF_RETRIES: u32 = 2;

/// Lists the change records of one diff scope; `wire` is that scope as
/// `changes.list` takes it. The diff view gets files first, then hunks, so a
/// huge workspace never blocks the panel from opening.
pub fn list_changes(
    client: Arc<Client>,
    tx: Tx,
    session_id: String,
    scope: crate::app::DiffScope,
    wire: protocol::ChangeScope,
) {
    tokio::spawn(async move {
        let params = json!({
            "session_id": session_id,
            "scope": wire.value(),
            "limit": 200,
            "max_bytes": 262_144,
        });
        let result: Result<ChangeList, _> = client
            .call::<Value, ChangeList>(protocol::METHOD_CHANGES_LIST, &params)
            .await;
        post(
            &tx,
            Some(&session_id),
            OpKind::ChangeFiles(scope, result.map(|list| list.records)),
        );
    });
}

/// Pages every change record of one path for the diff view, one page per
/// record in list order. The records stay separate: each one is its own
/// before/after comparison, so their hunks do not form one diff.
pub fn load_diff(
    client: Arc<Client>,
    tx: Tx,
    session_id: String,
    query: crate::app::DiffQuery,
    change_refs: Vec<String>,
) {
    tokio::spawn(async move {
        let mut pages = Vec::with_capacity(change_refs.len());
        for change_ref in change_refs {
            match page_diff(&client, &session_id, change_ref).await {
                Ok(page) => pages.push(page),
                Err(error) => {
                    post(
                        &tx,
                        Some(&session_id),
                        OpKind::DiffFile(query.clone(), Err(error)),
                    );
                    return;
                }
            }
        }
        post(&tx, Some(&session_id), OpKind::DiffFile(query, Ok(pages)));
    });
}

/// One record's hunks, following `next_cursor` to the end.
async fn page_diff(
    client: &Client,
    session_id: &str,
    change_ref: String,
) -> Result<protocol::changes::DiffPage, protocol::RpcError> {
    let mut hunks = Vec::new();
    let mut cursor: Option<Value> = None;
    loop {
        let params = json!({
            "session_id": session_id,
            "change_ref": change_ref,
            "cursor": cursor,
            "max_bytes": 262_144,
        });
        let mut page: protocol::changes::DiffPage = client
            .call::<Value, protocol::changes::DiffPage>(protocol::METHOD_CHANGES_DIFF, &params)
            .await?;
        cursor.clone_from(&page.next_cursor);
        hunks.append(&mut page.hunks);
        if cursor.is_none() {
            page.hunks = hunks;
            return Ok(page);
        }
    }
}

/// Fetches workspace paths for the `@` picker: one recursive `workspace.files`
/// page filtered by the current query, capped by the Agent's own limits.
pub fn list_files(client: Arc<Client>, tx: Tx, session_id: String, query: String) {
    tokio::spawn(async move {
        let params = json!({
            "session_id": session_id,
            "recursive": true,
            "query": query,
            "limit": 500,
            "max_bytes": 262_144,
        });
        let result: Result<protocol::FilesPage, _> = client
            .call::<Value, protocol::FilesPage>(protocol::METHOD_WORKSPACE_FILES, &params)
            .await;
        post(
            &tx,
            Some(&session_id),
            OpKind::Files(result.map(|page| {
                page.entries
                    .into_iter()
                    .filter(|entry| entry.kind == protocol::FileKind::File)
                    .map(|entry| entry.path)
                    .collect()
            })),
        );
    });
}

pub fn read_tool(client: Arc<Client>, tx: Tx, key: ToolRef) {
    tokio::spawn(async move {
        let params = json!({
            "session_id": key.session_id,
            "loop_id": key.loop_id,
            "request_index": key.request_index,
            "tool_call_id": key.tool_call_id,
            "display": true,
        });
        let result: Result<ToolReadResult, _> = client
            .call::<Value, ToolReadResult>(protocol::METHOD_TOOL_READ, &params)
            .await;
        let session_id = key.session_id.clone();
        post(&tx, Some(&session_id), OpKind::ToolRead(key, result));
    });
}

/// Pages the full output of one tool call for the tool view.
pub fn read_tool_output(client: Arc<Client>, tx: Tx, key: ToolRef, stream: String) {
    tokio::spawn(async move {
        let result = read_output(client.as_ref(), &key, &stream).await;
        let session_id = key.session_id.clone();
        post(
            &tx,
            Some(&session_id),
            OpKind::ToolOutput(key, stream, result),
        );
    });
}

async fn read_output(
    client: &Client,
    key: &ToolRef,
    stream: &str,
) -> Result<StreamText, protocol::RpcError> {
    let mut bytes = Vec::new();
    let mut result = StreamText::default();
    let mut offset = 0u64;
    loop {
        let params = json!({
            "session_id": key.session_id,
            "loop_id": key.loop_id,
            "request_index": key.request_index,
            "tool_call_id": key.tool_call_id,
            "stream": stream,
            "offset": offset,
            "max_bytes": 262144,
        });
        let page: ToolOutputPage = client
            .call::<Value, ToolOutputPage>(protocol::METHOD_TOOL_OUTPUT, &params)
            .await?;
        // The Agent keeps a bounded tail: a first page that starts past zero
        // means the head of the stream is gone.
        if offset == 0 && (page.base_offset > 0 || page.truncated) {
            result.dropped_head = true;
        }
        result.expired |= page.availability.as_deref() == Some("expired");
        bytes.extend_from_slice(&page.bytes());
        if bytes.len() >= MAX_TOOL_VIEW_BYTES {
            result.cut = true;
            break;
        }
        if page.eof || page.next_offset <= offset {
            break;
        }
        offset = page.next_offset;
    }
    // Decoded once: a multi-byte character can straddle two pages.
    result.text = String::from_utf8_lossy(&bytes).into_owned();
    Ok(result)
}

/// One stream of a tool call's full output, as the tool view shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StreamText {
    pub text: String,
    /// The Agent no longer retains the start of the stream.
    pub dropped_head: bool,
    /// The retained bytes were evicted; empty text does not mean no output.
    pub expired: bool,
    /// Reading stopped at [`MAX_TOOL_VIEW_BYTES`].
    pub cut: bool,
}

/// Ceiling for one tool view; the Agent keeps 1 MiB per stream anyway.
pub const MAX_TOOL_VIEW_BYTES: usize = 1024 * 1024;

/// Exports the conversation from canonical history as Markdown.
pub fn export(client: Arc<Client>, tx: Tx, session_id: String, path: Option<String>) {
    tokio::spawn(async move {
        let session_id_for_post = session_id.clone();
        let result = export_markdown(client.as_ref(), &session_id, path).await;
        post(&tx, Some(&session_id_for_post), OpKind::Exported(result));
    });
}

async fn export_markdown(
    client: &Client,
    session_id: &str,
    path: Option<String>,
) -> Result<String, String> {
    let probe: ReadSessionResult = client
        .call::<Value, ReadSessionResult>(
            protocol::METHOD_SESSION_READ,
            &json!({
                "session_id": session_id,
                "limit": protocol::PROBE_LIMIT,
                "max_bytes": protocol::PROBE_MAX_BYTES,
            }),
        )
        .await
        .map_err(|error| error.message)?;
    let pin = probe.pin();
    let mut assembler = protocol::ChunkAssembler::new();
    let mut items = Vec::new();
    let mut cursor = Some(protocol::ReadCursor::start());
    while let Some(position) = cursor {
        let params = json!({
            "session_id": session_id,
            "limit": protocol::PAGE_LIMIT,
            "max_bytes": protocol::PAGE_MAX_BYTES,
            "cursor": position.value(),
            "captured_end": pin.captured_end,
            "history_revision": pin.history_revision,
        });
        let page: ReadSessionResult = client
            .call::<Value, ReadSessionResult>(protocol::METHOD_SESSION_READ, &params)
            .await
            .map_err(|error| error.message)?;
        for chunk in page.items {
            if let Ok(protocol::ChunkOutcome::Item(item)) = assembler.push(chunk)
                && let Ok(decoded) = protocol::read::decode_item(&item.data)
            {
                items.push(decoded);
            }
        }
        cursor = page
            .next_cursor
            .as_ref()
            .and_then(protocol::ReadCursor::from_value);
    }
    let path = path.unwrap_or_else(|| default_export_path(session_id));
    std::fs::write(&path, crate::transcript::block::to_markdown(&items))
        .map_err(|error| format!("{path}: {error}"))?;
    Ok(path)
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
    #[test]
    fn the_default_export_path_names_the_session_and_the_day() {
        assert_eq!(super::civil_from_days(0), (1970, 1, 1));
        assert_eq!(super::civil_from_days(20_454), (2026, 1, 1));
        let path = super::default_export_path("ses_abc123");
        assert!(path.starts_with("minicore-abc123-20"), "{path}");
        assert!(path.ends_with(".md"), "{path}");
    }
}
