//! A scripted stand-in for the Agent process: real pipes, real JSON-RPC
//! framing and the real event stream, so the app can be driven through a whole
//! turn without the backend.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Notify, mpsc, oneshot};

use crate::app::{App, Msg};
use crate::rpc::{Client, Incoming};

/// Bounds a test wait even when Tokio's clock is paused or kept runnable.
pub async fn real_bounded<T>(work: impl Future<Output = T>) -> T {
    let (cancel, cancelled) = std::sync::mpsc::channel::<()>();
    let (expired, timeout) = oneshot::channel();
    std::thread::spawn(move || {
        if matches!(
            cancelled.recv_timeout(Duration::from_secs(5)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ) {
            let _ = expired.send(());
        }
    });
    let result = tokio::select! {
        result = work => result,
        _ = timeout => panic!("test wait exceeded five real seconds"),
    };
    drop(cancel);
    result
}

/// One request the fake Agent answered.
#[derive(Clone, Debug)]
pub struct Served {
    pub method: String,
    pub params: Value,
}

type Script = Arc<Mutex<HashMap<String, Reply>>>;

/// A scripted reply: what the Agent answers and how long it takes to answer.
#[derive(Clone)]
pub struct Reply {
    frame: Value,
    delay: Duration,
    /// Set when the answer waits for an explicit release.
    hold: Option<Arc<Hold>>,
    events: Vec<Value>,
    settings: Vec<(String, Value)>,
    history: bool,
    request_text: bool,
}

/// Reads are paged, so one method can need a different answer per call: the
/// probe asks with `limit:1`, the page asks for a window.
type Replies = std::collections::VecDeque<Reply>;

/// A gate a deferred answer waits behind. Releasing it lets every waiter go.
#[derive(Default)]
struct Hold {
    released: std::sync::atomic::AtomicBool,
    notify: tokio::sync::Notify,
}

impl Hold {
    async fn wait(&self) {
        while !self.released.load(std::sync::atomic::Ordering::SeqCst) {
            self.notify.notified().await;
        }
    }

    fn release(&self) {
        self.released
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.notify.notify_waiters();
    }
}

impl Reply {
    pub fn result(result: Value) -> Self {
        Self {
            frame: json!({"result": result}),
            delay: Duration::ZERO,
            hold: None,
            events: Vec::new(),
            settings: Vec::new(),
            history: false,
            request_text: false,
        }
    }

    /// Publishes these literal events immediately before this reply.
    pub fn events(mut self, events: Vec<Value>) -> Self {
        self.events = events;
        self
    }

    /// Replaces a later query's script when this reply is delivered.
    pub fn setting(mut self, method: impl Into<String>, result: Value) -> Self {
        self.settings.push((method.into(), result));
        self
    }

    /// Substitutes `$text` in this explicit reply and its events with the request text.
    pub fn with_request_text(mut self) -> Self {
        self.request_text = true;
        self
    }

    /// Serves a history fixture through its requested cursor and byte budget.
    pub fn history(mut self) -> Self {
        self.history = true;
        self
    }
}

impl From<Value> for Reply {
    fn from(value: Value) -> Self {
        if value.get("error").is_some() {
            Self {
                frame: value,
                ..Self::result(Value::Null)
            }
        } else {
            Self::result(value)
        }
    }
}

/// The resident instance identity the scripted Agent reports.
pub const LOAD: &str = "load_1";

pub struct FakeAgent {
    pub client: Arc<Client>,
    calls: Arc<Mutex<Vec<Served>>>,
    calls_ready: Arc<Notify>,
    unexpected: Arc<Mutex<Vec<String>>>,
    script: Script,
    /// Per-call answers for a method that is asked more than once.
    sequences: Arc<Mutex<HashMap<String, Replies>>>,
    /// Frames the fake pushes to the client (events, and unreadable input).
    /// Taken on drop to close the pipe.
    outbox: Option<mpsc::UnboundedSender<Vec<u8>>>,
    /// The Agent-side tasks, stopped on drop so the pipe ends with the fake.
    responder: Option<Responder>,
}

impl FakeAgent {
    /// Starts the fake behind a real [`Client`] and returns it with a fresh App
    /// wired to the same message channel.
    pub fn start(tx: mpsc::UnboundedSender<Msg>) -> (Self, App) {
        let (agent_side, client_side) = tokio::io::duplex(1024 * 1024);
        let (client_reader, client_writer) = tokio::io::split(client_side);
        let (agent_reader, mut agent_writer) = tokio::io::split(agent_side);
        let (events, mut incoming) = mpsc::channel::<Incoming>(64);
        let client = Client::start(client_writer, client_reader, tokio::io::empty(), events);
        let pump = tx.clone();
        tokio::spawn(async move {
            while let Some(incoming) = incoming.recv().await {
                let message = match incoming {
                    Incoming::Event { event, bytes } => Msg::Agent { event, bytes },
                    Incoming::Closed { status } => {
                        Msg::AgentClosed(status.unwrap_or_else(|| "closed".into()))
                    }
                };
                if pump.send(message).is_err() {
                    return;
                }
            }
        });

        let calls: Arc<Mutex<Vec<Served>>> = Arc::default();
        let calls_ready = Arc::new(Notify::new());
        let unexpected: Arc<Mutex<Vec<String>>> = Arc::default();
        let script: Script = Arc::default();
        let (outbox, mut outbox_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        // The Agent side is two tasks: one reads requests, one writes frames.
        // Splitting them keeps a pending read from being cancelled by a write.
        let (stop, mut stopped) = oneshot::channel::<()>();
        let writer_task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut stopped => return,
                    frame = outbox_rx.recv() => match frame {
                        Some(frame) => {
                            if agent_writer.write_all(&frame).await.is_err()
                                || agent_writer.flush().await.is_err()
                            {
                                return;
                            }
                        }
                        None => return,
                    },
                }
            }
        });
        let replies = outbox.clone();
        let sequences = Arc::new(Mutex::new(HashMap::<String, Replies>::new()));
        let reader_task = {
            let calls = calls.clone();
            let calls_ready = calls_ready.clone();
            let unexpected = unexpected.clone();
            let script = script.clone();
            let sequences = sequences.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(agent_reader);
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line).await {
                        Ok(0) | Err(_) => return,
                        Ok(_) => {}
                    }
                    let Ok(request) = serde_json::from_str::<Value>(line.trim_end()) else {
                        return;
                    };
                    let method = request["method"].as_str().unwrap_or_default().to_owned();
                    calls.lock().unwrap().push(Served {
                        method: method.clone(),
                        params: request["params"].clone(),
                    });
                    calls_ready.notify_waiters();
                    // A `method@session_id` script answers only that session's calls.
                    let mut keys: Vec<String> = request["params"]["session_id"]
                        .as_str()
                        .map(|id| format!("{method}@{id}"))
                        .into_iter()
                        .collect();
                    if let (Some(session), Some(loop_id)) = (
                        request["params"]["session_id"].as_str(),
                        request["params"]["loop_id"].as_str(),
                    ) {
                        keys.insert(0, format!("{method}@{session}/{loop_id}"));
                    }
                    keys.push(method.clone());
                    let mut reply = {
                        let mut sequences = sequences.lock().unwrap();
                        let script = script.lock().unwrap();
                        keys.iter()
                            .find_map(|key| {
                                let queued = sequences.get_mut(key).and_then(Replies::pop_front);
                                queued.or_else(|| script.get(key).cloned())
                            })
                            .unwrap_or_else(|| {
                                unexpected.lock().unwrap().push(method.clone());
                                Reply::from(json!({"error": {
                                    "code": -32601, "message": format!("unscripted request: {method}"),
                                    "data": {"kind": "unscripted_request"}
                                }}))
                            })
                    };
                    if reply.request_text {
                        let text = &request["params"]["text"];
                        replace_text(&mut reply.frame, text);
                        for event in &mut reply.events {
                            replace_text(event, text);
                        }
                        for (_, result) in &mut reply.settings {
                            replace_text(result, text);
                        }
                    }
                    let mut frame = json!({"jsonrpc": "2.0", "id": request["id"]});
                    for (key, value) in reply.frame.as_object().unwrap() {
                        frame[key] = value.clone();
                    }
                    if reply.history
                        && let Some(result) = frame.get_mut("result")
                    {
                        history_fixture(result, &request["params"]);
                    }
                    let replies = replies.clone();
                    let script = script.clone();
                    tokio::spawn(async move {
                        if !reply.delay.is_zero() {
                            tokio::time::sleep(reply.delay).await;
                        }
                        if let Some(hold) = reply.hold {
                            hold.wait().await;
                        }
                        for (method, result) in reply.settings {
                            script.lock().unwrap().insert(method, Reply::result(result));
                        }
                        for event in reply.events {
                            let event =
                                json!({"jsonrpc": "2.0", "method": "agent.event", "params": event});
                            let _ = replies.send(format!("{event}\n").into_bytes());
                        }
                        let _ = replies.send(format!("{frame}\n").into_bytes());
                    });
                }
            })
        };
        // Dropping the fake stops the writer, which closes the Agent side of
        // the pipe exactly like a dying child process.
        let responder = Some(Responder {
            stop,
            writer_task,
            reader_task,
        });

        let app = App::new(client.clone(), tx, std::env::temp_dir());
        (
            Self {
                client,
                calls,
                calls_ready,
                unexpected,
                script,
                sequences,
                outbox: Some(outbox),
                responder,
            },
            app,
        )
    }

    /// Scripts a literal reply; scoped keys are `method@session` or `method@session/loop`.
    pub fn script(&self, method: &str, reply: impl Into<Reply>) {
        self.script
            .lock()
            .unwrap()
            .insert(method.to_owned(), reply.into());
    }

    /// Holds a scripted reply until [`FakeAgent::release`].
    pub fn script_held(&self, method: &str, reply: impl Into<Reply>) {
        let mut reply = reply.into();
        reply.hold = Some(Arc::new(Hold::default()));
        self.script(method, reply);
    }

    /// Releases a held answer; every waiter completes.
    pub fn release(&self, method: &str) {
        let script = self.script.lock().unwrap();
        script
            .get(method)
            .and_then(|reply| reply.hold.as_ref())
            .unwrap_or_else(|| panic!("no held script for {method}"))
            .release();
    }

    /// Sets a JSON-RPC error answer, the way a refused call arrives.
    pub fn script_error(&self, method: &str, code: i64, message: &str, kind: &str) {
        self.script_error_with(
            method,
            code,
            message,
            kind,
            (method.split('@').next() == Some("input.push")).then_some("rejected"),
        );
    }

    /// A JSON-RPC error carrying the acceptance classification the Agent sends
    /// when a refusal says whether the operation had already been applied.
    pub fn script_error_with(
        &self,
        method: &str,
        code: i64,
        message: &str,
        kind: &str,
        acceptance: Option<&str>,
    ) {
        let mut data = json!({"kind": kind});
        if let Some(acceptance) = acceptance {
            data["acceptance"] = json!(acceptance);
        }
        self.script(
            method,
            json!({"error": {"code": code, "message": message, "data": data}}),
        );
    }

    /// A JSON-RPC error that waits for [`FakeAgent::release`].
    pub fn script_error_held(&self, method: &str, code: i64, message: &str, kind: &str) {
        let acceptance = (method.split('@').next() == Some("input.push")).then_some("rejected");
        self.script_held(
            method,
            json!({"error": {
                "code": code, "message": message, "data": {"kind": kind, "acceptance": acceptance}
            }}),
        );
    }

    /// Answers once per entry, then falls back to the persistent script.
    pub fn script_sequence<R: Into<Reply>>(&self, method: &str, replies: Vec<R>) {
        self.sequences.lock().unwrap().insert(
            method.to_owned(),
            replies.into_iter().map(Into::into).collect(),
        );
    }

    /// Answers each request after its explicit delay, allowing later calls to finish first.
    pub fn script_sequence_later<R: Into<Reply>>(&self, method: &str, replies: Vec<(R, Duration)>) {
        self.script_sequence(
            method,
            replies
                .into_iter()
                .map(|(reply, delay)| {
                    let mut reply = reply.into();
                    reply.delay = delay;
                    reply
                })
                .collect(),
        );
    }

    /// Refuses the first call and answers the next with `result`.
    pub fn script_error_then(
        &self,
        method: &str,
        code: i64,
        message: &str,
        kind: &str,
        result: impl Into<Reply>,
    ) {
        self.script_sequence(
            method,
            vec![
                Reply::from(
                    json!({"error": {"code": code, "message": message, "data": {"kind": kind}}}),
                ),
                result.into(),
            ],
        );
    }

    /// Sends exactly one literal Agent event on the response stream.
    pub async fn emit(&mut self, event: Value) {
        let frame = json!({"jsonrpc": "2.0", "method": "agent.event", "params": event});
        self.push(format!("{frame}\n").into_bytes()).await;
    }

    /// Writes a raw frame, for tests about unreadable input.
    pub async fn push(&mut self, frame: Vec<u8>) {
        if let Some(outbox) = &self.outbox {
            let _ = outbox.send(frame);
        }
        tokio::task::yield_now().await;
    }

    pub fn assert_scripted(&self) {
        let unexpected = self.unexpected.lock().unwrap();
        assert!(unexpected.is_empty(), "unscripted requests: {unexpected:?}");
    }

    pub fn methods(&self) -> Vec<String> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .map(|call| call.method.clone())
            .collect()
    }

    /// Waits until the fake has received `count` calls to `method`.
    pub async fn wait_for_calls(&self, method: &str, count: usize) {
        real_bounded(async {
            loop {
                let notified = self.calls_ready.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if self.calls(method).len() >= count {
                    return;
                }
                notified.await;
            }
        })
        .await
    }

    pub fn calls(&self, method: &str) -> Vec<Value> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call.method == method)
            .map(|call| call.params.clone())
            .collect()
    }
}

fn replace_text(value: &mut Value, text: &Value) {
    match value {
        Value::String(value) if value == "$text" => *value = text.as_str().unwrap().to_owned(),
        Value::Array(items) => items.iter_mut().for_each(|item| replace_text(item, text)),
        Value::Object(fields) => fields
            .values_mut()
            .for_each(|field| replace_text(field, text)),
        _ => {}
    }
}

fn history_fixture(value: &mut Value, params: &Value) {
    let Some(fields) = value.as_object_mut() else {
        return;
    };
    fields["session"]["session_id"] = params["session_id"].clone();
    let total = fields["total"].as_u64().unwrap() as usize;
    let canonical = params["view"] == "canonical";
    let floor = if canonical {
        fields["projection"] = Value::Null;
        0
    } else {
        fields["projection"]
            .get("first_item")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize
    };
    let start = params["cursor"]["item"]
        .as_u64()
        .map_or(floor, |item| item as usize);
    let one_item = fields.remove("_fixture_one_item") == Some(Value::Bool(true));
    let limit = if one_item {
        1
    } else {
        params["limit"].as_u64().unwrap() as usize
    };
    let end = start.saturating_add(limit).min(total);
    let known: HashMap<usize, Value> = fields["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| (item["index"].as_u64().unwrap() as usize, item.clone()))
        .collect();
    let mut remaining = params["max_bytes"].as_u64().unwrap() as usize;
    let mut offset = params["cursor"]["offset"].as_u64().unwrap_or(0) as usize;
    let mut cursor = start;
    let mut items = Vec::new();
    while cursor < end && remaining > 0 {
        let mut item = known.get(&cursor).cloned().unwrap_or_else(|| chunk(cursor, json!({
            "type": "assistant", "data": {"loop_id": "fixture", "request_index": 0, "content": []}
        })));
        let data = item["data"].as_str().unwrap();
        let mut through = data.len().min(offset + remaining);
        while !data.is_char_boundary(through) {
            through -= 1;
        }
        if through == offset {
            break;
        }
        let complete = through == data.len();
        let text = data[offset..through].to_owned();
        remaining -= text.len();
        item["data"] = json!(text);
        item["offset"] = json!(offset);
        item["complete"] = json!(complete);
        items.push(item);
        if complete {
            cursor += 1;
            offset = 0;
        } else {
            offset = through;
            break;
        }
    }
    fields.insert("items".into(), json!(items));
    fields.insert(
        "next_cursor".into(),
        if cursor < total {
            json!({"item":cursor,"offset":offset})
        } else {
            Value::Null
        },
    );
}

/// The Agent-side tasks of a fake: the frame writer, its stop signal and the
/// pipes, which must be dropped for the client to see a close.
struct Responder {
    stop: oneshot::Sender<()>,
    writer_task: tokio::task::JoinHandle<()>,
    reader_task: tokio::task::JoinHandle<()>,
}

impl FakeAgent {
    /// Ends the Agent side of the pipe, the way a dying child process does.
    pub fn close(&mut self) {
        if let Some(responder) = self.responder.take() {
            let _ = responder.stop.send(());
            responder.writer_task.abort();
            responder.reader_task.abort();
            // Closing the outbox ends the frame writer, and the writer owns the
            // only handle to the Agent side of the pipe.
            self.outbox.take();
        }
    }
}

impl Drop for FakeAgent {
    fn drop(&mut self) {
        self.close();
    }
}

/// A session catalog entry the fake Agent returns.
pub fn session_info(session_id: &str) -> Value {
    json!({
        "session_id": session_id,
        "title": null,
        "profile": "coding",
        "workspace": "/tmp/ws",
        "model": "mock",
        "reasoning": "medium",
        "loaded": true,
        "created_at": "2024-01-01T00:00:00Z",
        "updated_at": "2024-01-01T00:00:00Z",
    })
}

pub fn turn(loop_id: &str) -> Value {
    json!({"session_id": "s1", "loop_id": loop_id})
}

/// A control snapshot with nothing running and an empty queue.
pub fn snapshot(session_id: &str, load: &str, revision: u64) -> Value {
    json!({
        "session_id": session_id,
        "load": load,
        "revision": revision,
        "execution": {"type": "none"},
        "queue": {"items": [], "paused": false},
        "pending_interaction": null, "compaction": null, "block_reason": null,
        "completions": {
            "through_seq": 0, "retained_from_seq": 1, "entries": [], "truncated": false
        }
    })
}

/// A control snapshot with one running turn.
pub fn snapshot_running(
    session_id: &str,
    load: &str,
    revision: u64,
    loop_id: &str,
    input: Value,
) -> Value {
    let mut state = snapshot(session_id, load, revision);
    state["execution"] = json!({
        "type": "running", "loop_id": loop_id, "input": input, "request_index": 0,
        "phase": "running_model"
    });
    state
}

/// One history chunk as the Agent frames it: the item JSON lives in `data`, and
/// a small item is a single complete chunk.
pub fn chunk(index: usize, item: Value) -> Value {
    chunk_envelope(index, json!({"item": item}))
}

/// A history chunk whose item JSON is the whole `envelope`, e.g. with a timestamp.
pub fn chunk_envelope(index: usize, envelope: Value) -> Value {
    let data = envelope.to_string();
    json!({
        "index": index,
        "offset": 0,
        "total_bytes": data.len(),
        "encoding": "utf8_json",
        "data": data,
        "complete": true,
    })
}

/// A history projection with `first_item` older items covered by its summary.
pub fn display_read(total: usize, first_item: usize) -> Value {
    json!({
        "session": session_info("s1"),
        "items": [],
        "next_cursor": null,
        "total": total,
        "records": [],
        "history_revision": "h1",
        "captured_end": 1,
        "projection": {
            "revision": "p1",
            "first_item": first_item,
            "covered_item_count": first_item,
            "covered_usage": {"usage": {}, "partial": true},
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_fake_answers_concurrent_and_held_calls() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let (mut fake, mut app) = FakeAgent::start(tx);
        fake.script("echo", json!({"ok": true}));
        fake.script_held("waiting", json!({"done": true}));
        let client = fake.client.clone();
        let quick =
            tokio::spawn(async move { client.call::<Value, Value>("echo", &json!({})).await });
        let client = fake.client.clone();
        let slow =
            tokio::spawn(async move { client.call::<Value, Value>("waiting", &json!({})).await });
        let quick = tokio::time::timeout(Duration::from_secs(1), quick)
            .await
            .expect("the quick call is not blocked by the held one")
            .unwrap()
            .unwrap();
        assert_eq!(quick, json!({"ok": true}));
        assert!(!slow.is_finished(), "the held call waits");
        fake.release("waiting");
        let slow = tokio::time::timeout(Duration::from_secs(1), slow)
            .await
            .expect("the held call resolves after release")
            .unwrap()
            .unwrap();
        assert_eq!(slow, json!({"done": true}));

        fake.emit(json!({
            "type": "session_closed",
            "data": {"session_id": "s", "meta": {"session_id": "s", "load": LOAD, "dropped_before": 0}}
        }))
        .await;
        let message = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("the event arrives")
            .unwrap();
        assert!(matches!(message, Msg::Agent { .. }));
        let _ = &mut app;
    }

    #[tokio::test(start_paused = true)]
    async fn literal_replies_preserve_snapshot_identity_and_do_not_publish_control() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let (fake, _app) = FakeAgent::start(tx);
        let state = json!({"snapshot": snapshot("s1", "different-load", 77)});
        fake.script("session.state", state.clone());
        fake.script("input.push", json!({"id":"input", "text":"$text"}));
        let observed: Value = fake
            .client
            .call("session.state", &json!({"session_id":"s1"}))
            .await
            .unwrap();
        assert_eq!(observed, state);
        let receipt: Value = fake
            .client
            .call("input.push", &json!({"session_id":"s1","text":"hello"}))
            .await
            .unwrap();
        assert_eq!(receipt, json!({"id":"input","text":"$text"}));
        tokio::task::yield_now().await;
        assert!(
            rx.try_recv().is_err(),
            "a receipt publishes no implicit event"
        );
        let observed: Value = fake
            .client
            .call("session.state", &json!({"session_id":"s1"}))
            .await
            .unwrap();
        assert_eq!(
            observed, state,
            "a receipt cannot rewrite a scripted observation"
        );
        let error = fake
            .client
            .call::<Value, Value>(
                "turn.result",
                &json!({"session_id":"s1","loop_id":"unknown"}),
            )
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unscripted request: turn.result")
        );
    }
}
