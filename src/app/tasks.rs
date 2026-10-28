//! Async operations. Each spawns, awaits the Agent and posts one message;
//! nothing here touches `App` state.

use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::{App, BUSY_RETRY, Msg, NoticeLevel, SessionOp};
use crate::protocol::changes::{ChangeList, DiffPage};
use crate::protocol::read::{InterruptedRequest, RawHistoryItem, TurnRecord};
use crate::protocol::{
    self, ChunkAssembler, ChunkOutcome, ModelListResult, PingResult, ProfileListResult, ReadCursor,
    ReadSessionResult, RpcError, SessionInfo, SessionListResult, SessionResult, SnapshotPin,
    TurnRef, TurnResultPage, TurnWaitResult, Usage,
};
use crate::rpc::Client;
use crate::transcript::ChangeSummary;

pub type Tx = mpsc::UnboundedSender<Msg>;

/// An assembled history window.
pub struct History {
    /// Decoded items in source order.
    pub items: Vec<RawHistoryItem>,
    /// The first source item of the window.
    pub from: usize,
    /// The display projection's `first_item`: older items are behind the
    /// compaction summary.
    pub floor: usize,
    pub has_more: bool,
    pub canonical: bool,
    /// How the turns with items in the window ended.
    pub records: Vec<TurnRecord>,
    /// The usage behind the summary, when the Agent counted all of it.
    pub covered_usage: Option<Usage>,
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
    Ok(page.outcome.map(|outcome| TurnWaitResult {
        turn: turn.clone(),
        outcome,
        usage: page.usage,
        requests: page.requests,
        tool_rounds: page.tool_rounds,
    }))
}

/// Reads one history window. A stale pin means the history moved while it was
/// paged; the read starts over once with a fresh probe.
pub async fn read_history(
    client: Arc<Client>,
    session_id: String,
    window: Window,
) -> Result<History, RpcError> {
    let read = async || -> Result<History, RpcError> {
        let canonical = matches!(window, Window::CanonicalBefore(_));
        let view = if canonical { "canonical" } else { "display" };
        let pin = probe(&client, &session_id, view).await?;
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
        let (items, records) = read_window(&client, &session_id, view, &pin, start, end).await?;
        Ok(History {
            items,
            from: start,
            floor: pin.first_item,
            has_more: start > pin.first_item,
            canonical,
            records,
            covered_usage: pin
                .covered_usage
                .filter(|covered| !covered.partial)
                .map(|covered| covered.usage),
        })
    };
    match read().await {
        Err(error) if error.is_stale_pin() => read().await,
        result => result,
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
    Ok(page.pin())
}

/// Reads `[start, end)` against `pin` and decodes every complete item, with
/// the record of each turn that has one.
async fn read_window(
    client: &Client,
    session_id: &str,
    view: &str,
    pin: &SnapshotPin,
    start: usize,
    end: usize,
) -> Result<(Vec<RawHistoryItem>, Vec<TurnRecord>), RpcError> {
    let mut assembler = ChunkAssembler::new();
    let mut items = Vec::new();
    let mut records: Vec<TurnRecord> = Vec::new();
    let mut cursor = Some(ReadCursor {
        item: start,
        offset: 0,
    });
    while let Some(position) = cursor.filter(|position| position.item < end) {
        let mut params = json!({
            "session_id": session_id,
            "limit": protocol::PAGE_LIMIT,
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
        for record in page.records {
            if !records.iter().any(|known| known.loop_id == record.loop_id) {
                records.push(record);
            }
        }
        for chunk in page.items {
            let index = chunk.index;
            if let Ok(ChunkOutcome::Item(item)) = assembler.push(chunk)
                && index < end
                && let Ok(decoded) = protocol::read::decode_item(&item.data)
            {
                items.push(decoded);
            }
        }
        cursor = page.next_cursor.as_ref().and_then(ReadCursor::from_value);
    }
    Ok((items, records))
}

/// One turn's durable items and its interrupted partial reply, read after a
/// gap or an unfinished outcome.
pub async fn read_turn(
    client: Arc<Client>,
    turn: TurnRef,
) -> Result<(Vec<RawHistoryItem>, Option<InterruptedRequest>), RpcError> {
    let mut assembler = ChunkAssembler::new();
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
        let page: TurnResultPage = client.call(protocol::METHOD_TURN_RESULT, &params).await?;
        for chunk in page.items {
            if let Ok(ChunkOutcome::Item(item)) = assembler.push(chunk)
                && let Ok(decoded) = protocol::read::decode_item(&item.data)
            {
                items.push(decoded);
            }
        }
        cursor = page.next_cursor;
        // Every page carries it.
        if cursor.is_none() {
            return Ok((items, page.interrupted));
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
        let items = async || {
            let pin = probe(&client, &session_id, "canonical").await?;
            let (items, _) =
                read_window(&client, &session_id, "canonical", &pin, 0, pin.total).await?;
            Ok::<_, RpcError>(items)
        };
        let items = match items().await {
            Err(error) if error.is_stale_pin() => items().await,
            result => result,
        }
        .map_err(|error| error.message)?;
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
    #[test]
    fn the_default_export_path_names_the_session_and_the_day() {
        assert_eq!(super::civil_from_days(0), (1970, 1, 1));
        assert_eq!(super::civil_from_days(20_454), (2026, 1, 1));
        let path = super::default_export_path("ses_abc123");
        assert!(path.starts_with("minicore-abc123-20"), "{path}");
        assert!(path.ends_with(".md"), "{path}");
    }
}
