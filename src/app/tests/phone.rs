//! Phone adapters for reducer tests; the Agent remains explicitly scripted.

use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot, watch};

use super::harness::*;
use crate::app::Msg;
use crate::remote::{Connection, RemoteEvent, auth::Device, wire::Intent};

static NEXT_OP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

pub(super) fn phone(
    h: &mut Harness,
    id: u64,
    device: &str,
) -> (
    mpsc::Receiver<Value>,
    watch::Receiver<Option<&'static str>>,
    oneshot::Receiver<bool>,
) {
    let (outbound, output) = mpsc::channel(512);
    let (close, closing) = watch::channel(None);
    let (accepted, admission) = oneshot::channel();
    h.app.on_msg(Msg::Remote(RemoteEvent::Connect {
        connection: Connection {
            id,
            generation: h.app.remote.generation,
            device: Device {
                name: device.into(),
                token_sha256: device.into(),
                paired_at: "2026-01-01T00:00:00Z".into(),
            },
            outbound,
            close,
        },
        accepted,
    }));
    (output, closing, admission)
}

pub(super) async fn connect_phone(
    h: &mut Harness,
    id: u64,
    device: &str,
) -> (mpsc::Receiver<Value>, watch::Receiver<Option<&'static str>>) {
    let (output, closing, admission) = phone(h, id, device);
    assert!(admission.await.unwrap());
    (output, closing)
}

pub(super) fn enable_remote(h: &mut Harness) {
    h.app.remote.config = Some(config(h.app.workspace.clone()));
    h.app.remote.url = Some("http://localhost:8765".into());
}

pub(super) async fn request(h: &mut Harness, intent: Value) -> String {
    request_from(h, 1, intent).await
}

/// Delivers an intent the way the socket does, naming the view and instance
/// this phone holds unless the test supplies its own.
pub(super) async fn request_from(h: &mut Harness, connection: u64, mut intent: Value) -> String {
    let phone = h
        .app
        .remote
        .phones
        .iter()
        .find(|phone| phone.connection.id == connection);
    let session = phone
        .and_then(|phone| phone.view.session_id())
        .and_then(|id| h.app.live(id));
    let fields = intent.as_object_mut().unwrap();
    fields
        .entry("view_epoch")
        .or_insert_with(|| json!(phone.map_or("", |phone| phone.projection.view_epoch())));
    // Match the client envelope without replacing explicit stale identities.
    let acts_on_view = !matches!(
        fields["kind"].as_str(),
        Some("open_session" | "new_session" | "rename_session" | "delete_session")
    );
    if acts_on_view && let Some(session) = session {
        fields
            .entry("session_id")
            .or_insert_with(|| json!(session.info.session_id));
        if fields["session_id"] == session.info.session_id {
            fields.entry("load").or_insert_with(|| json!(session.load));
        }
    }
    let cancel = fields["kind"] == "cancel";
    if (cancel || fields["kind"] == "answer")
        && !fields.contains_key("loop_id")
        && let Some(session) = session
    {
        let turn = if cancel {
            session.live.as_ref().map(|live| &live.turn)
        } else {
            session.interaction.as_ref().map(|pending| &pending.turn)
        };
        if let Some(turn) = turn {
            fields.insert("loop_id".into(), json!(turn.loop_id));
        }
        if cancel && let Some(operation) = &session.compaction {
            fields
                .entry("operation_id")
                .or_insert_with(|| json!(operation.operation_id));
        }
    }
    // A fresh operation each time: an answer names only its operation, so a
    // live id cannot be reused.
    let id = format!(
        "request{}",
        NEXT_OP.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let (done, _finished) = oneshot::channel();
    h.app.on_msg(Msg::Remote(RemoteEvent::Request {
        connection,
        id: id.clone(),
        intent: serde_json::from_value::<Intent>(intent).unwrap(),
        done,
    }));
    // A submit, queue or unqueue is answered from the Agent's own result, so the
    // app must run before the answer exists. Nothing is drained here: the caller
    // reads the frames this produces in order.
    h.settle().await;
    id
}

pub(super) fn config(root: std::path::PathBuf) -> crate::config::Remote {
    crate::config::Remote {
        bind: vec!["127.0.0.1:0".parse().unwrap()],
        url: None,
        roots: vec![root],
        allow_insecure: false,
        tls: None,
    }
}

pub(super) fn resident_session() -> Harness {
    let mut h = Harness::new();
    h.app.workspace = h.app.workspace.canonicalize().unwrap();
    let mut info: crate::protocol::SessionInfo =
        serde_json::from_value(session_info("s1")).unwrap();
    info.workspace = h.app.workspace.to_string_lossy().into_owned();
    h.app.sessions.push(info.clone());
    h.app.epoch = 1;
    let session = crate::app::Session::new(info, 1, crate::testing::LOAD.into());
    h.app.show_session(Some(session));
    h.app.ready = true;
    h
}

/// The projection identity the server sent this connection last.
pub(super) fn view_epoch(h: &mut Harness, connection: u64) -> String {
    h.app
        .remote
        .phones
        .iter()
        .find(|phone| phone.connection.id == connection)
        .expect("an admitted phone")
        .projection
        .view_epoch()
        .to_owned()
}

/// A v3 intent naming the projection and resident instance the phone holds.
pub(super) fn phone_intent(h: &mut Harness, connection: u64, mut intent: Value) -> Value {
    let epoch = view_epoch(h, connection);
    intent["view_epoch"] = json!(epoch);
    intent["session_id"] = json!("s1");
    intent["load"] = json!("load_1");
    intent
}

/// The message types waiting in `output`.
pub(super) fn received(output: &mut mpsc::Receiver<Value>) -> Vec<Value> {
    messages(output)
        .into_iter()
        .map(|message| message["t"].clone())
        .collect()
}

/// The last state the next sync sends, if it sends one.
pub(super) fn synced_state(h: &mut Harness, output: &mut mpsc::Receiver<Value>) -> Option<Value> {
    h.app.sync_remote();
    messages(output)
        .into_iter()
        .rev()
        .find_map(|message| message.get("state").cloned())
}

pub(super) fn messages(output: &mut mpsc::Receiver<Value>) -> Vec<Value> {
    std::iter::from_fn(|| output.try_recv().ok()).collect()
}

/// The next frame the phone receives, running the app until it is produced.
///
/// A frame that never comes fails the test instead of hanging the suite.
pub(super) async fn next(h: &mut Harness, output: &mut mpsc::Receiver<Value>) -> Value {
    for _ in 0..64 {
        if let Ok(message) = output.try_recv() {
            return message;
        }
        if output.is_closed() {
            break;
        }
        h.settle().await;
    }
    panic!("the phone never received the frame");
}

/// The phone's answer to an intent, skipping the projections that precede it.
pub(super) async fn ack_next(h: &mut Harness, output: &mut mpsc::Receiver<Value>) -> Value {
    crate::testing::real_bounded(async {
        loop {
            let message = next(h, output).await;
            if message["t"] == "ack" {
                return message;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
}

pub(super) fn projected(output: &mut mpsc::Receiver<Value>) -> Value {
    messages(output)
        .into_iter()
        .rev()
        .find_map(|message| message.get("state").cloned())
        .expect("a projection")
}

pub(super) async fn connected_session() -> (Harness, mpsc::Receiver<Value>) {
    let mut h = resident_session();
    h.app.remote.config = Some(config(h.app.workspace.clone()));
    h.app.remote.url = Some("http://localhost:8765".into());
    h.fake.script(
        "session.state@s1",
        state_response(snapshot("s1", crate::testing::LOAD, 1)),
    );
    let (mut output, _closing, admission) = phone(&mut h, 1, "first");
    assert!(admission.await.unwrap());
    h.app.sync_remote();
    messages(&mut output);
    (h, output)
}

pub(super) async fn called(h: &Harness, method: &str, count: usize) {
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        h.fake.wait_for_calls(method, count),
    )
    .await
    .expect("the Agent call did not reach its gate");
}
