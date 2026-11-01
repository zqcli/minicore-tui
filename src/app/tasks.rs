//! Async operations. Each spawns, awaits the Agent and posts one message;
//! nothing here touches `App` state.

use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::operation::{Operation, Outcome};
use super::read::{export_text, invalid_read, stored_outcome};
use super::{App, BUSY_RETRY, Msg, NoticeLevel, SessionOp};
use crate::protocol::changes::{ChangeList, DiffPage};
use crate::protocol::{
    self, ModelListResult, OpenedSession, PingResult, ProfileListResult, RpcError, SessionInfo,
    SessionListResult, TurnRef, TurnWaitResult,
};
use crate::rpc::Client;
use crate::transcript::ChangeSummary;

pub type Tx = mpsc::UnboundedSender<Msg>;

#[derive(Default)]
pub struct Tasks(std::cell::RefCell<Vec<tokio::task::AbortHandle>>);

impl Tasks {
    pub fn spawn(&self, work: impl Future<Output = ()> + Send + 'static) {
        let mut handles = self.0.borrow_mut();
        handles.retain(|handle| !handle.is_finished());
        handles.push(tokio::spawn(work).abort_handle());
    }
}

impl Drop for Tasks {
    fn drop(&mut self) {
        for task in self.0.get_mut() {
            task.abort();
        }
    }
}

impl App {
    pub(super) fn spawn_for(
        &self,
        epoch: u64,
        work: impl Future<Output = SessionOp> + Send + 'static,
    ) {
        let tx = self.tx.clone();
        self.resident
            .get(epoch)
            .expect("a resident task target")
            .tasks
            .spawn(async move {
                let _ = tx.send(Msg::Session(epoch, Box::new(work.await)));
            });
    }

    pub(super) fn spawn(&self, work: impl Future<Output = SessionOp> + Send + 'static) {
        if let Some(session) = self.resident.current() {
            self.spawn_for(session.epoch, work);
        }
    }

    pub(super) fn call_for<R: DeserializeOwned + Send + 'static>(
        &self,
        epoch: u64,
        method: &'static str,
        mut fields: Value,
        wrap: impl FnOnce(Result<R, RpcError>) -> SessionOp + Send + 'static,
    ) {
        let session = self
            .resident
            .get(epoch)
            .expect("a resident operation target");
        fields["session_id"] = json!(session.info.session_id);
        if matches!(
            method,
            protocol::METHOD_INPUT_RECALL | protocol::METHOD_INPUT_RESUME
        ) {
            fields["load"] = json!(session.load);
        }
        let client = self.client.clone();
        self.spawn_for(
            epoch,
            async move { wrap(client.call(method, &fields).await) },
        );
    }

    pub(super) fn call<R: DeserializeOwned + Send + 'static>(
        &self,
        method: &'static str,
        fields: Value,
        wrap: impl FnOnce(Result<R, RpcError>) -> SessionOp + Send + 'static,
    ) {
        if let Some(session) = self.resident.current() {
            self.call_for(session.epoch, method, fields, wrap);
        }
    }

    pub(super) fn spawn_msg(&self, work: impl Future<Output = Msg> + Send + 'static) {
        let tx = self.tx.clone();
        self.tasks.spawn(async move {
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

/// `session.open` or `session.create`; both answer with the session, the
/// resident instance identity and its first control snapshot.
pub async fn session(
    client: Arc<Client>,
    method: &'static str,
    params: Value,
) -> Result<Adopted, RpcError> {
    let result: OpenedSession = client.call(method, &params).await?;
    let load = result.load;
    let snapshot = result.snapshot;
    if snapshot.session_id != result.session.session_id || snapshot.load != load {
        return Err(invalid_read(
            "the opened instance snapshot has another identity",
        ));
    }
    Ok(Adopted {
        info: result.session,
        load,
        snapshot,
    })
}

/// The session a rename returned; a rename creates no instance of its own.
pub async fn renamed_session(
    client: Arc<Client>,
    session_id: String,
    title: String,
    load: Option<String>,
) -> Result<SessionInfo, RpcError> {
    let Outcome::Info(info) = Operation::Rename(title)
        .execute(&client, &session_id, load.as_deref())
        .await?
    else {
        unreachable!("a rename returns session info")
    };
    Ok(info)
}

/// A session this client just opened or created.
pub struct Adopted {
    pub info: SessionInfo,
    pub load: String,
    pub snapshot: protocol::SessionSnapshot,
}

/// Closes a resident instance before deleting its stored session.
pub async fn delete_session(client: Arc<Client>, session_id: String, load: Option<String>) -> Msg {
    let result = Operation::Delete
        .execute(&client, &session_id, load.as_deref())
        .await
        .map(drop);
    Msg::Deleted(session_id, result)
}

/// Waits for the turn to end. A full deferred queue or a busy session is a
/// condition, not an outcome, so the wait is registered again.
pub async fn wait_turn(client: Arc<Client>, turn: TurnRef, load: String) -> SessionOp {
    let params = json!({"session_id": turn.session_id, "loop_id": turn.loop_id});
    let mut retries = 0;
    loop {
        match client
            .call::<_, TurnWaitResult>(protocol::METHOD_TURN_WAIT, &params)
            .await
        {
            Err(error) if (error.is_capacity() || error.is_busy()) && retries < 3 => {
                retries += 1;
                tokio::time::sleep(BUSY_RETRY).await;
            }
            Ok(wait) if wait.turn == turn => return SessionOp::Waited(Ok(wait)),
            Ok(_) => {
                return SessionOp::Waited(Err(invalid_read(
                    "wait response belongs to another turn",
                )));
            }
            // The waiter is gone but the turn may have ended: its stored result
            // ends it, so a lost wait never leaves the turn running.
            Err(error) => match stored_outcome(&client, &turn, &load).await {
                Ok(Some(wait)) => return SessionOp::Waited(Ok(wait)),
                Ok(None) => return SessionOp::Waited(Err(error)),
                Err(_) => return SessionOp::Waited(Err(error)),
            },
        }
    }
}

/// Whether an error says this client's instance identity is out of date.
pub fn is_stale_load(error: &RpcError) -> bool {
    matches!(
        error.code,
        protocol::MISSING_LOAD | protocol::STALE_LOAD | protocol::STALE_RESULT
    )
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
        let text = export_text(&client, &session_id).await?;
        let file = path.clone();
        tokio::task::spawn_blocking(move || std::fs::write(file, text))
            .await
            .map_err(|error| error.to_string())?
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

/// An accepted input must name the exact instance that received the request.
pub(super) fn accepted_input(
    receipt: protocol::InputPushResult,
    session_id: &str,
    load: &str,
) -> Result<protocol::InputPushResult, RpcError> {
    if receipt.accepted
        && receipt.session_id == session_id
        && receipt.load == load
        && !receipt.id.is_empty()
    {
        Ok(receipt)
    } else {
        Err(RpcError::new(
            -32603,
            "unconfirmed_receipt",
            "The acceptance receipt did not confirm this operation's target",
        ))
    }
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
