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
type Controls = HashMap<String, Control>;

/// A scripted reply: what the Agent answers and how long it takes to answer.
#[derive(Clone)]
struct Reply {
    frame: Value,
    delay: Duration,
    /// Set when the answer waits for an explicit release.
    hold: Option<Arc<Hold>>,
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
    fn result(result: Value) -> Self {
        Self {
            frame: json!({"result": result}),
            delay: Duration::ZERO,
            hold: None,
        }
    }
}

/// What the scripted Agent reports as its control state.
///
/// It follows the calls it answered, so a test drives it with receipts and
/// events as it would drive the real Agent; a test that needs a hand-written
/// observation scripts `session.state` or emits `session_snapshot`.
struct Control {
    load: String,
    revision: u64,
    execution: Value,
    queue: Value,
    interaction: Option<Value>,
    compaction: Value,
    through_seq: u64,
    completions: Vec<Value>,
    saved: Option<String>,
    results: HashMap<String, Value>,
}

impl Default for Control {
    fn default() -> Self {
        Self {
            load: LOAD.into(),
            revision: 0,
            execution: json!({"type": "none"}),
            queue: json!({"items": [], "paused": false}),
            interaction: None,
            compaction: Value::Null,
            through_seq: 0,
            completions: Vec::new(),
            saved: None,
            results: HashMap::new(),
        }
    }
}

impl Control {
    fn snapshot(&mut self, session_id: &str) -> Value {
        self.revision += 1;
        json!({
            "session_id": session_id,
            "load": self.load,
            "revision": self.revision,
            "execution": self.execution.clone(),
            "pending_interaction": self.interaction.clone(),
            "queue": self.queue.clone(),
            "compaction": self.compaction,
            "block_reason": Value::Null,
            "completions": {
                "through_seq": self.through_seq,
                "retained_from_seq": self.through_seq.saturating_sub(255) + 1,
                "entries": self.completions,
                "truncated": false,
            }
        })
    }

    /// Records what a receipt says about execution and the queue.
    fn on_receipt(&mut self, method: &str, result: &Value) {
        if let Some(queue) = result.get("queue") {
            self.queue = queue.clone();
        }
        if let Some(started) = result.get("started").filter(|started| !started.is_null()) {
            self.execution = json!({
                "type": "running",
                "loop_id": started["turn"]["loop_id"],
                "input": started["input"],
                "request_index": 0,
                "phase": "running_model",
            });
        }
        if method == "session.compact" {
            self.compaction = Value::Null;
            if let Some(next) = result.get("next").filter(|next| !next.is_null()) {
                self.execution = json!({"type":"running","loop_id":next["turn"]["loop_id"],"input":next["input"],"request_index":0,"phase":"running_model"});
                if let Some(items) = self.queue["items"].as_array_mut() {
                    items.retain(|item| item["id"] != next["input"]["id"]);
                }
            }
        }
    }

    /// Ends a turn a `turn.wait` answered and registers its completion.
    fn on_wait(&mut self, result: &Value) {
        let input = self.execution["input"].clone();
        if let Some(queue) = result.get("queue").filter(|queue| !queue.is_null()) {
            self.queue = queue.clone();
        }
        let loop_id = result["turn"]["loop_id"].clone();
        let items = input.get("text").map(|text| vec![chunk(0, json!({"type":"user","data":{"loop_id":loop_id,"kind":"prompt","input":{"text":text}}}))]).unwrap_or_default();
        self.results.insert(loop_id.as_str().unwrap().into(), json!({
            "turn":result["turn"],"load":self.load,"result_id":format!("r1:{:064x}",1),
            "availability":"stored","persistence":"persisted","items":items,"total":items.len(),"next_cursor":null,
            "outcome":result["outcome"],"usage":result["usage"],"requests":result["requests"],"tool_rounds":result["tool_rounds"]
        }));
        let outcome: crate::protocol::LoopOutcome =
            serde_json::from_value(result["outcome"].clone())
                .unwrap_or(crate::protocol::LoopOutcome::Unknown);
        self.execution = json!({"type": "none"});
        self.through_seq += 1;
        let kind = match outcome.is_completed() {
            true => "completed",
            false => "failed",
        };
        self.completions.push(json!({
            "seq": self.through_seq,
            "loop_id": loop_id,
            "completion_kind": "reported",
            "outcome_kind": kind,
            "persistence": "persisted",
            "availability": "stored",
            "after_saved_loop_id": self.saved,
            "next_loop_id": result["next"]["turn"]["loop_id"],
        }));
        if self.completions.len() > 255 {
            let removed = self.completions.remove(0);
            self.results.remove(removed["loop_id"].as_str().unwrap());
        }
        self.saved = Some(loop_id.as_str().unwrap_or_default().to_owned());
        if let Some(next) = result.get("next").filter(|next| !next.is_null()) {
            self.execution = json!({
                "type": "running",
                "loop_id": next["turn"]["loop_id"],
                "input": next["input"],
                "request_index": 0,
                "phase": "running_model",
            });
        }
    }

    fn result(&self, params: &Value) -> Value {
        self.results.get(params["loop_id"].as_str().unwrap()).cloned().unwrap_or_else(|| json!({
            "turn":{"session_id":params["session_id"],"loop_id":params["loop_id"]},"load":self.load,"availability":"pending","items":[],"total":0,"next_cursor":null
        }))
    }

    fn on_event(&mut self, event: &Value) {
        match event["type"].as_str() {
            Some("session_snapshot") => {
                let snapshot = &event["data"]["snapshot"];
                if snapshot["load"].as_str() == Some(self.load.as_str())
                    && snapshot["revision"]
                        .as_u64()
                        .is_some_and(|revision| revision > self.revision)
                {
                    self.revision = snapshot["revision"].as_u64().unwrap();
                    self.execution = snapshot["execution"].clone();
                    self.queue = snapshot["queue"].clone();
                    self.compaction = snapshot["compaction"].clone();
                    self.interaction = snapshot
                        .get("pending_interaction")
                        .filter(|pending| !pending.is_null())
                        .cloned();
                }
            }
            Some("input_queue") => self.queue = event["data"]["queue"].clone(),
            Some("interaction_requested") => {
                let interaction = &event["data"]["interaction"];
                self.interaction = Some(json!({
                    "loop_id": event["data"]["turn"]["loop_id"],
                    "request_index": 0,
                    "interaction_id": interaction["interaction_id"],
                    "tool_call_id": interaction["tool_call_id"],
                    "tool_name": interaction["tool_name"],
                    "kind": interaction["kind"],
                }));
            }
            Some("interaction_resolved")
                if self.interaction.as_ref().is_some_and(|pending| {
                    pending["interaction_id"] == event["data"]["interaction_id"]
                }) =>
            {
                self.interaction = None
            }
            Some("input_delivered") => {
                if let Some(items) = self.queue["items"].as_array_mut() {
                    items.retain(|item| {
                        !event["data"]["items"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|delivered| delivered["id"] == item["id"])
                    });
                }
            }
            _ => {}
        }
    }
}

/// The resident instance identity the scripted Agent reports.
pub const LOAD: &str = "load_1";

pub struct FakeAgent {
    pub client: Arc<Client>,
    calls: Arc<Mutex<Vec<Served>>>,
    calls_ready: Arc<Notify>,
    control: Arc<Mutex<Controls>>,
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
    pub fn control_revision(&self, session_id: &str) -> u64 {
        self.control
            .lock()
            .unwrap()
            .get(session_id)
            .map_or(0, |control| control.revision)
    }

    /// What the scripted Agent would report as its control snapshot now.
    pub fn control(&self) -> Value {
        self.control
            .lock()
            .unwrap()
            .entry("s1".into())
            .or_default()
            .snapshot("s1")
    }

    /// Sets the queue the scripted Agent holds.
    pub fn set_queue(&self, queue: Value) {
        self.control
            .lock()
            .unwrap()
            .entry("s1".into())
            .or_default()
            .queue = queue;
    }

    /// Pushes the current control snapshot as the Agent's own event.
    pub async fn emit_control(&mut self) {
        let snapshot = self.control();
        self.emit(json!({"type": "session_snapshot", "data": {
            "meta": {"session_id": "s1", "dropped_before": 0},
            "snapshot": snapshot,
        }}))
        .await;
    }

    /// Ends the turn the Agent has running, as its own completion would.
    pub async fn complete_control(&mut self, outcome: Value) {
        {
            let mut control = self.control.lock().unwrap();
            let wait = serde_json::json!({
                "turn": {"session_id": "s1", "loop_id": "l1"},
                "outcome": outcome,
            });
            control.entry("s1".into()).or_default().on_wait(&wait);
        }
        self.emit_control().await;
    }
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
        let control: Arc<Mutex<Controls>> = Arc::default();
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
            let script = script.clone();
            let sequences = sequences.clone();
            let control = control.clone();
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
                    let reply = {
                        let mut sequences = sequences.lock().unwrap();
                        let script = script.lock().unwrap();
                        keys.iter()
                            .find_map(|key| {
                                let queued = sequences.get_mut(key).and_then(Replies::pop_front);
                                queued.or_else(|| script.get(key).cloned())
                            })
                            .unwrap_or_else(|| {
                                let mut controls = control.lock().unwrap();
                                let owner = controls
                                    .entry(
                                        request["params"]["session_id"]
                                            .as_str()
                                            .unwrap_or("s1")
                                            .into(),
                                    )
                                    .or_default();
                                Reply::result(if method == "turn.result" {
                                    owner.result(&request["params"])
                                } else {
                                    json!({})
                                })
                            })
                    };
                    let mut frame = json!({"jsonrpc": "2.0", "id": request["id"]});
                    if let Value::Object(fields) = reply.frame {
                        for (key, mut value) in fields {
                            echo(&mut value, &request["params"]["text"]);
                            if method == "session.read" && key == "result" {
                                history_fixture(&mut value, &request["params"]);
                            }
                            if key == "result"
                                && matches!(
                                    method.as_str(),
                                    "session.state" | "session.open" | "session.create"
                                )
                            {
                                control_answers(&control, &method, &request["params"], &mut value);
                            }
                            frame[key] = value;
                        }
                    }
                    let replies = replies.clone();
                    let control = control.clone();
                    let params = request["params"].clone();
                    if method == "session.compact" {
                        let mut controls = control.lock().unwrap();
                        let id = params["session_id"].as_str().unwrap();
                        let owner = controls.entry(id.into()).or_default();
                        owner.compaction = json!({"operation_id":params["operation_id"],"phase":"preparing","covered_item_count":0,"retained_item_count":0});
                        let snapshot = owner.snapshot(id);
                        let event = json!({"jsonrpc":"2.0","method":"agent.event","params":{"type":"session_snapshot","data":{"meta":{"session_id":id,"load":LOAD,"dropped_before":0},"snapshot":snapshot}}});
                        let _ = replies.send(format!("{event}\n").into_bytes());
                    }
                    tokio::spawn(async move {
                        if !reply.delay.is_zero() {
                            tokio::time::sleep(reply.delay).await;
                        }
                        if let Some(hold) = reply.hold {
                            hold.wait().await;
                        }
                        if frame.get("result").is_some()
                            && !matches!(
                                method.as_str(),
                                "session.state" | "session.open" | "session.create"
                            )
                        {
                            control_answers(&control, &method, &params, &mut frame["result"]);
                        }
                        if matches!(
                            method.as_str(),
                            "turn.wait"
                                | "input.push"
                                | "input.resume"
                                | "input.remove"
                                | "input.recall"
                                | "interaction.answer"
                                | "session.compact"
                                | "session.context"
                        ) && frame.get("result").is_some()
                        {
                            let mut controls = control.lock().unwrap();
                            let id = params["session_id"].as_str().unwrap_or("s1");
                            let owner = controls.entry(id.into()).or_default();
                            let snapshot = owner.snapshot(id);
                            let event = json!({"jsonrpc":"2.0","method":"agent.event","params":{
                                "type":"session_snapshot","data":{"meta":{"session_id":snapshot["session_id"],"load":LOAD,"dropped_before":0},"snapshot":snapshot}
                            }});
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
                control,
                script,
                sequences,
                outbox: Some(outbox),
                responder,
            },
            app,
        )
    }

    /// Sets the result one method answers with. Unscripted methods answer `{}`;
    /// a `method@session_id` key answers only that session's calls.
    pub fn script(&self, method: &str, result: Value) {
        self.script
            .lock()
            .unwrap()
            .insert(method.to_owned(), Reply::result(result));
    }

    /// Answers `method` only after [`FakeAgent::release`] is called, the way a
    /// deferred call (`turn.wait`) resolves when the Agent is done.
    pub fn script_held(&self, method: &str, result: Value) {
        self.script.lock().unwrap().insert(
            method.to_owned(),
            Reply {
                frame: json!({"result": result}),
                delay: Duration::ZERO,
                hold: Some(Arc::new(Hold::default())),
            },
        );
    }

    /// Releases a held answer; every waiter completes.
    pub fn release(&self, method: &str) {
        if let Some(reply) = self.script.lock().unwrap().get(method)
            && let Some(hold) = &reply.hold
        {
            hold.release();
        }
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
        self.script.lock().unwrap().insert(
            method.to_owned(),
            Reply {
                frame: json!({"error": {"code": code, "message": message, "data": data}}),
                delay: Duration::ZERO,
                hold: None,
            },
        );
    }

    /// A JSON-RPC error that is answered only after [`FakeAgent::release`].
    pub fn script_error_held(&self, method: &str, code: i64, message: &str, kind: &str) {
        let acceptance = (method.split('@').next() == Some("input.push")).then_some("rejected");
        self.script.lock().unwrap().insert(
            method.to_owned(),
            Reply {
                frame: json!({"error": {"code": code, "message": message, "data": {"kind": kind,"acceptance":acceptance}}}),
                delay: Duration::ZERO,
                hold: Some(Arc::new(Hold::default())),
            },
        );
    }

    /// Sets a result that arrives after `delay`, like a deferred call
    /// (`turn.wait`) or a slow read.
    /// Answers `method` with each result in turn, one per call, then falls back
    /// to the plain script. Paged reads need this: the probe and the page ask
    /// the same method with different cursors.
    /// A value carrying an `error` object is used as the whole reply, so a
    /// sequence can mix results and refusals.
    pub fn script_sequence(&self, method: &str, results: Vec<Value>) {
        let queue: Replies = results
            .into_iter()
            .map(|value| {
                if value.get("error").is_some() {
                    Reply {
                        frame: value,
                        delay: Duration::ZERO,
                        hold: None,
                    }
                } else {
                    Reply::result(value)
                }
            })
            .collect();
        self.sequences
            .lock()
            .unwrap()
            .insert(method.to_owned(), queue);
    }

    /// Answers `method` with each (result, delay) pair in turn, so a test can
    /// make a later request finish before an earlier one.
    pub fn script_sequence_later(&self, method: &str, results: Vec<(Value, Duration)>) {
        let queue: Replies = results
            .into_iter()
            .map(|(result, delay)| Reply {
                frame: json!({"result": result}),
                delay,
                hold: None,
            })
            .collect();
        self.sequences
            .lock()
            .unwrap()
            .insert(method.to_owned(), queue);
    }

    /// Answers `method` with a JSON-RPC error on the first call, then with
    /// `result`: the shape of a refusal that a retry overcomes.
    pub fn script_error_then(
        &self,
        method: &str,
        code: i64,
        message: &str,
        kind: &str,
        result: Value,
    ) {
        let error = json!({"error": {"code": code, "message": message, "data": {"kind": kind}}});
        self.sequences.lock().unwrap().insert(
            method.to_owned(),
            vec![
                Reply {
                    frame: error,
                    delay: Duration::ZERO,
                    hold: None,
                },
                Reply::result(result),
            ]
            .into(),
        );
    }

    /// Sends one Agent event on the same stream the Agent uses; the fake's own
    /// control state follows the events a real Agent would derive from.
    pub async fn emit(&mut self, mut event: Value) {
        if event["data"]["meta"].is_object() && event["data"]["meta"]["load"].is_null() {
            event["data"]["meta"]["load"] = json!(LOAD);
        }
        let delivered = event["type"] == "input_delivered";
        let id = event["data"]["meta"]["session_id"]
            .as_str()
            .unwrap_or("s1")
            .to_owned();
        if let Some(id) = event["data"]["meta"]["session_id"].as_str() {
            self.control
                .lock()
                .unwrap()
                .entry(id.into())
                .or_default()
                .on_event(&event);
        }
        let frame = json!({"jsonrpc": "2.0", "method": "agent.event", "params": event});
        self.push(format!("{frame}\n").into_bytes()).await;
        if delivered {
            let snapshot = self
                .control
                .lock()
                .unwrap()
                .entry(id.clone())
                .or_default()
                .snapshot(&id);
            let event = json!({"jsonrpc":"2.0","method":"agent.event","params":{
                "type":"session_snapshot","data":{"meta":{"session_id":id,"load":LOAD,"dropped_before":0},"snapshot":snapshot}
            }});
            self.push(format!("{event}\n").into_bytes()).await;
        }
    }

    /// Writes a raw frame, for tests about unreadable input.
    pub async fn push(&mut self, frame: Vec<u8>) {
        if let Some(outbox) = &self.outbox {
            let _ = outbox.send(frame);
        }
        tokio::task::yield_now().await;
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

/// Replaces every `"$text"` in a scripted answer with the request's text, so an answer can name what was pushed.
/// Makes the scripted answer reflect the receipt or wait it came from, and
/// answers `session.state` from the fake's own control state.
fn control_answers(controls: &Mutex<Controls>, method: &str, params: &Value, result: &mut Value) {
    let id = params["session_id"]
        .as_str()
        .or_else(|| result["session"]["session_id"].as_str())
        .unwrap_or("s1")
        .to_owned();
    let mut controls = controls.lock().unwrap();
    let control = controls.entry(id.clone()).or_default();
    match method {
        // The scripted answer states the same observation its own control
        // state now holds, so later readers and events stay consistent.
        "session.state" => {
            if result
                .as_object_mut()
                .unwrap()
                .remove("_fixture_exact_snapshot")
                == Some(Value::Bool(true))
            {
                return;
            }
            // A test that scripts an observation gets those fields; the fake
            // stays authoritative for what it owns as live control state, so a
            // scripted snapshot never blanks a queue an earlier receipt set.
            let scripted = result.get("snapshot").and_then(Value::as_object).cloned();
            let mut snapshot = control.snapshot(&id);
            // A receipt or wait knows the queue the Agent holds after it, which
            // an older snapshot cannot describe; with no such record the
            // scripted queue is the observation the test wants.
            for (key, value) in scripted.unwrap_or_default() {
                let rejected = matches!(key.as_str(), "revision" | "session_id" | "load");
                if !rejected {
                    snapshot[&key] = value;
                }
            }
            if snapshot["execution"]["loop_id"]
                .as_str()
                .is_some_and(|id| control.results.contains_key(id))
            {
                snapshot["execution"] = json!({"type":"none"});
            }
            // The v2 answer derives its own fields from the same observation.
            let running = !matches!(snapshot["execution"]["type"].as_str(), Some("none"));
            result["session_id"] = json!(id);
            result["status"] = json!(if running { "running" } else { "idle" });
            result["active_loop"] = match running {
                true => json!({"loop_id": snapshot["execution"]["loop_id"]}),
                false => Value::Null,
            };
            result["compaction"] = snapshot["compaction"].clone();
            result["snapshot"] = snapshot;
        }
        // A recall also states the queue it left behind, so the fake must forget
        // the entries it returned or the next read would restore them.
        "input.push" | "input.resume" | "input.remove" | "input.recall" | "session.compact" => {
            control.on_receipt(method, result)
        }
        "interaction.answer"
            if control
                .interaction
                .as_ref()
                .is_some_and(|pending| pending["interaction_id"] == params["interaction_id"]) =>
        {
            control.interaction = None
        }
        "turn.wait" if result["turn"]["loop_id"] == params["loop_id"] => control.on_wait(result),
        "turn.result"
            if matches!(result["availability"].as_str(), Some("stored" | "live"))
                && result["turn"]["loop_id"].is_string()
                && control.execution["loop_id"] == result["turn"]["loop_id"] =>
        {
            control.on_wait(result)
        }
        "session.context"
            if result.get("current_operation").is_some() || result.get("automatic").is_some() =>
        {
            control.compaction = result["current_operation"].clone();
            if control.compaction.is_null()
                && let Some(current) = result["automatic"]
                    .get("current")
                    .filter(|current| !current.is_null())
            {
                control.compaction = json!({"operation_id":current["operation_id"],"phase":"summarizing","covered_item_count":0,"retained_item_count":0});
            }
        }
        "session.open" | "session.create" => {
            if result["load"].is_null() {
                result["load"] = json!(LOAD);
            }
            if result.get("snapshot").is_none() {
                result["snapshot"] = control.snapshot(&id);
            }
        }
        _ => {}
    }
}

fn echo(value: &mut Value, text: &Value) {
    if value.as_str() == Some("$text") {
        *value = text.clone();
        return;
    }
    match value {
        Value::Array(items) => items.iter_mut().for_each(|item| echo(item, text)),
        Value::Object(fields) => fields.values_mut().for_each(|field| echo(field, text)),
        _ => {}
    }
}

fn history_fixture(value: &mut Value, params: &Value) {
    let Some(fields) = value.as_object_mut() else {
        return;
    };
    if fields.remove("_fixture_history") != Some(Value::Bool(true)) {
        return;
    }
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

/// A `session.read` result: `total` items, the first `first_item` covered by a
/// compaction summary.
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

pub fn display_read(total: usize, first_item: usize) -> Value {
    json!({
        "_fixture_history": true,
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
            "data": {"session_id": "s", "meta": {"session_id": "s", "dropped_before": 0}}
        }))
        .await;
        let message = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("the event arrives")
            .unwrap();
        assert!(matches!(message, Msg::Agent { .. }));
        let _ = &mut app;
    }
}
