//! A scripted stand-in for the Agent process: real pipes, real JSON-RPC
//! framing and the real event stream, so the app can be driven through a whole
//! turn without the backend.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

use crate::app::{App, Msg};
use crate::rpc::{Client, Incoming};

/// One request the fake Agent answered.
#[derive(Clone, Debug)]
pub struct Served {
    pub method: String,
    pub params: Value,
}

type Script = Arc<Mutex<HashMap<String, Reply>>>;

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

pub struct FakeAgent {
    pub client: Arc<Client>,
    calls: Arc<Mutex<Vec<Served>>>,
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
                    Incoming::Event(event) => Msg::Agent(event),
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
                    // A `method@session_id` script answers only that session's calls.
                    let mut keys: Vec<String> = request["params"]["session_id"]
                        .as_str()
                        .map(|id| format!("{method}@{id}"))
                        .into_iter()
                        .collect();
                    keys.push(method.clone());
                    let reply = {
                        let mut sequences = sequences.lock().unwrap();
                        let script = script.lock().unwrap();
                        keys.iter()
                            .find_map(|key| {
                                let queued = sequences.get_mut(key).and_then(Replies::pop_front);
                                queued.or_else(|| script.get(key).cloned())
                            })
                            .unwrap_or_else(|| Reply::result(json!({})))
                    };
                    let mut frame = json!({"jsonrpc": "2.0", "id": request["id"]});
                    if let Value::Object(fields) = reply.frame {
                        for (key, value) in fields {
                            frame[key] = value;
                        }
                    }
                    let replies = replies.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(reply.delay).await;
                        if let Some(hold) = reply.hold {
                            hold.wait().await;
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
        self.script.lock().unwrap().insert(
            method.to_owned(),
            Reply {
                frame: json!({"error": {"code": code, "message": message, "data": {"kind": kind}}}),
                delay: Duration::ZERO,
                hold: None,
            },
        );
    }

    /// A JSON-RPC error that is answered only after [`FakeAgent::release`].
    pub fn script_error_held(&self, method: &str, code: i64, message: &str, kind: &str) {
        self.script.lock().unwrap().insert(
            method.to_owned(),
            Reply {
                frame: json!({"error": {"code": code, "message": message, "data": {"kind": kind}}}),
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

    /// Sends one Agent event on the same stream the Agent uses.
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

    pub fn methods(&self) -> Vec<String> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .map(|call| call.method.clone())
            .collect()
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
        "session": session_info("s1"),
        "items": [],
        "next_cursor": null,
        "total": total,
        "history_revision": "h1",
        "captured_end": 1,
        "projection": {
            "revision": "p1",
            "first_item": first_item,
            "covered_item_count": first_item,
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
        assert!(matches!(message, Msg::Agent(_)));
        let _ = &mut app;
    }
}
