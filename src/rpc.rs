//! The agent child process and its JSON-RPC client.
//!
//! Four background tasks own the pipes: one writer, one stdout reader, one
//! stderr reader (bounded tail) and one child waiter. Requests are routed by id
//! to a `oneshot`; events and child loss are posted to the app channel.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Semaphore, mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::protocol::{
    AgentEvent, IncomingFrame, OutgoingRequest, RequestId, RpcError, RpcResponse,
};

/// One incoming frame from the child, in arrival order.
pub enum Incoming {
    Event(Box<AgentEvent>),
    /// The child is gone: no further frames will arrive.
    Closed {
        status: Option<String>,
    },
}

/// Upper bound for one inbound line. The Agent states no limit of its own.
const MAX_FRAME_BYTES: usize = 32 * 1024 * 1024;
/// The Agent terminates the process on an oversized request line, so the
/// client must refuse to send one.
const MAX_REQUEST_BYTES: usize = 1024 * 1024;
const STDERR_TAIL_LINES: usize = 200;
const OUTBOUND_CAPACITY: usize = 64;
/// The Agent admits 4 concurrent read queries; the rest are retried on
/// `-32019`, which is worse than pacing them here.
const QUERY_PERMITS: usize = 3;

pub struct Client {
    outbound: mpsc::Sender<Vec<u8>>,
    pending: Arc<Mutex<HashMap<RequestId, oneshot::Sender<RpcResponse>>>>,
    next_id: AtomicU64,
    queries: Arc<Semaphore>,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    closed: Arc<Mutex<Option<String>>>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl Client {
    /// Spawns the agent and starts its pipe tasks. `incoming` receives events
    /// and the terminal close notice.
    pub fn spawn(
        agent_bin: &Path,
        agent_config: &Path,
        incoming: mpsc::Sender<Incoming>,
    ) -> Result<Arc<Self>, RpcError> {
        let mut child = Command::new(agent_bin)
            .arg("--config")
            .arg(agent_config)
            .arg("--stdio")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| {
                RpcError::closed(format!("could not start {}: {error}", agent_bin.display()))
            })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| RpcError::closed("agent stdin is unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| RpcError::closed("agent stdout is unavailable"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| RpcError::closed("agent stderr is unavailable"))?;

        let (outbound, outbound_rx) = mpsc::channel::<Vec<u8>>(OUTBOUND_CAPACITY);
        let pending: Arc<Mutex<HashMap<RequestId, oneshot::Sender<RpcResponse>>>> = Arc::default();
        let stderr_tail: Arc<Mutex<VecDeque<String>>> = Arc::default();
        let closed: Arc<Mutex<Option<String>>> = Arc::default();

        let writer = tokio::spawn(writer_task(stdin, outbound_rx));
        let reader = tokio::spawn(reader_task(
            stdout,
            pending.clone(),
            incoming.clone(),
            closed.clone(),
        ));
        let stderr_reader = tokio::spawn(stderr_task(stderr, stderr_tail.clone()));
        let waiter = tokio::spawn(waiter_task(
            child,
            closed.clone(),
            pending.clone(),
            incoming,
        ));

        Ok(Arc::new(Self {
            outbound,
            pending,
            next_id: AtomicU64::new(1),
            queries: Arc::new(Semaphore::new(QUERY_PERMITS)),
            stderr_tail,
            closed,
            tasks: Mutex::new(vec![writer, reader, stderr_reader, waiter]),
        }))
    }

    /// Sends one request and awaits its response. Query methods take one of
    /// the Agent's read-query slots; everything else is always admitted.
    pub async fn call<P: serde::Serialize, R: DeserializeOwned>(
        &self,
        method: &'static str,
        params: &P,
        query: bool,
    ) -> Result<R, RpcError> {
        let _permit = if query {
            Some(self.queries.acquire().await.map_err(|_| closed())?)
        } else {
            None
        };
        let response = self.call_raw(method, params).await?;
        response.decode()
    }

    pub async fn call_raw<P: serde::Serialize>(
        &self,
        method: &'static str,
        params: &P,
    ) -> Result<RpcResponse, RpcError> {
        if let Some(reason) = self.closed.lock().unwrap().clone() {
            return Err(RpcError::closed(reason));
        }
        let id = RequestId(self.next_id.fetch_add(1, Ordering::Relaxed));
        let mut params = serde_json::to_value(params)
            .map_err(|error| RpcError::new(-32602, "invalid_params", error.to_string()))?;
        // The Agent accepts omitted params or an object, never JSON null.
        if params.is_null() {
            params = Value::Object(Default::default());
        }
        let line = encode_request(OutgoingRequest::new(id, method, params))?;
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        match self.outbound.send(line).await {
            Ok(()) => {}
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                return Err(RpcError::closed("agent is not accepting requests"));
            }
        }
        rx.await.map_err(|_| {
            self.pending.lock().unwrap().remove(&id);
            RpcError::closed("agent closed before answering")
        })
    }

    /// Whether the child process is still answering. `None` is a live agent.
    pub fn closed_reason(&self) -> Option<String> {
        self.closed.lock().unwrap().clone()
    }

    pub fn stderr_tail(&self) -> Vec<String> {
        self.stderr_tail.lock().unwrap().iter().cloned().collect()
    }

    /// Sends `agent.shutdown` and gives the child a bounded moment to exit
    /// before it is killed. Safe to call more than once.
    pub async fn shutdown(&self) {
        let _ = tokio::time::timeout(
            Duration::from_secs(3),
            self.call_raw("agent.shutdown", &Value::Null),
        )
        .await;
        for task in self.tasks.lock().unwrap().drain(..) {
            task.abort();
        }
    }
}

fn closed() -> RpcError {
    RpcError::closed("rpc client is shutting down")
}

fn encode_request(request: OutgoingRequest) -> Result<Vec<u8>, RpcError> {
    let mut line = serde_json::to_vec(&request)
        .map_err(|error| RpcError::new(-32602, "invalid_params", error.to_string()))?;
    if line.len() + 1 > MAX_REQUEST_BYTES {
        return Err(RpcError::new(
            -32602,
            "request_too_large",
            format!(
                "request of {} bytes exceeds the {MAX_REQUEST_BYTES} byte limit",
                line.len() + 1
            ),
        ));
    }
    line.push(b'\n');
    Ok(line)
}

async fn writer_task<W: AsyncWrite + Unpin>(mut stdin: W, mut outbound: mpsc::Receiver<Vec<u8>>) {
    while let Some(line) = outbound.recv().await {
        if stdin.write_all(&line).await.is_err() {
            break;
        }
        if stdin.flush().await.is_err() {
            break;
        }
    }
}

async fn reader_task<R: AsyncRead + Unpin>(
    stdout: R,
    pending: Arc<Mutex<HashMap<RequestId, oneshot::Sender<RpcResponse>>>>,
    incoming: mpsc::Sender<Incoming>,
    closed: Arc<Mutex<Option<String>>>,
) {
    let mut reader = BufReader::with_capacity(64 * 1024, stdout);
    let mut line = Vec::new();
    loop {
        line.clear();
        match read_line(&mut reader, &mut line).await {
            Ok(0) => {
                fail(&closed, &pending, &incoming, "agent closed its stdout");
                return;
            }
            Ok(_) => {}
            Err(reason) => {
                fail(&closed, &pending, &incoming, &reason);
                return;
            }
        }
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        match crate::protocol::parse_frame(&line) {
            Ok(IncomingFrame::Response(response)) => {
                let target = pending.lock().unwrap().remove(&response.id);
                if let Some(target) = target {
                    let _ = target.send(response);
                }
            }
            Ok(IncomingFrame::Event(event)) => {
                if incoming
                    .send(Incoming::Event(Box::new(event)))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            Ok(IncomingFrame::UnknownNotification(_)) => {}
            Err(error) => {
                // A malformed frame means the stream cannot be trusted; the
                // child is stopped rather than decoded around.
                fail(
                    &closed,
                    &pending,
                    &incoming,
                    &format!("agent sent an unreadable frame: {error}"),
                );
                return;
            }
        }
    }
}

/// Reads one LF-terminated line with a hard size bound.
async fn read_line<R: AsyncBufReadExt + Unpin>(
    reader: &mut R,
    line: &mut Vec<u8>,
) -> Result<usize, String> {
    loop {
        let available = match reader.fill_buf().await {
            Ok(available) => available,
            Err(error) => return Err(format!("agent stdout read failed: {error}")),
        };
        if available.is_empty() {
            return Ok(line.len());
        }
        match available.iter().position(|byte| *byte == b'\n') {
            Some(end) => {
                line.extend_from_slice(&available[..=end]);
                reader.consume(end + 1);
                return Ok(line.len());
            }
            None => {
                line.extend_from_slice(available);
                let consumed = available.len();
                reader.consume(consumed);
                if line.len() > MAX_FRAME_BYTES {
                    return Err(format!(
                        "agent frame exceeded the {MAX_FRAME_BYTES} byte limit"
                    ));
                }
            }
        }
    }
}

async fn stderr_task<R: AsyncRead + Unpin>(stderr: R, tail: Arc<Mutex<VecDeque<String>>>) {
    let mut reader = BufReader::new(stderr);
    let mut buffer = String::new();
    loop {
        buffer.clear();
        match reader.read_line(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let line = buffer.trim_end();
        if line.is_empty() {
            continue;
        }
        let mut tail = tail.lock().unwrap();
        if tail.len() == STDERR_TAIL_LINES {
            tail.pop_front();
        }
        tail.push_back(line.to_owned());
    }
}

async fn waiter_task(
    mut child: Child,
    closed: Arc<Mutex<Option<String>>>,
    pending: Arc<Mutex<HashMap<RequestId, oneshot::Sender<RpcResponse>>>>,
    incoming: mpsc::Sender<Incoming>,
) {
    let status = match child.wait().await {
        Ok(status) => format!("{}", status),
        Err(error) => format!("wait failed: {error}"),
    };
    fail(
        &closed,
        &pending,
        &incoming,
        &format!("agent exited ({status})"),
    );
}

fn fail(
    closed: &Arc<Mutex<Option<String>>>,
    pending: &Arc<Mutex<HashMap<RequestId, oneshot::Sender<RpcResponse>>>>,
    incoming: &mpsc::Sender<Incoming>,
    reason: &str,
) {
    let first = {
        let mut slot = closed.lock().unwrap();
        if slot.is_some() {
            false
        } else {
            *slot = Some(reason.to_owned());
            true
        }
    };
    if !first {
        return;
    }
    pending.lock().unwrap().clear();
    let _ = incoming.try_send(Incoming::Closed {
        status: Some(reason.to_owned()),
    });
}

/// Default agent binary name resolved through `PATH`.
pub fn default_agent_bin() -> PathBuf {
    PathBuf::from("minicore-agent")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn oversized_requests_are_refused_before_writing() {
        let big = "x".repeat(MAX_REQUEST_BYTES);
        let error = encode_request(OutgoingRequest::new(
            RequestId(1),
            "turn.send",
            json!({"text": big}),
        ))
        .unwrap_err();
        assert_eq!(error.kind, "request_too_large");
    }

    #[test]
    fn a_failure_keeps_the_first_honest_reason() {
        let closed: Arc<Mutex<Option<String>>> = Arc::default();
        let pending: Arc<Mutex<HashMap<RequestId, oneshot::Sender<RpcResponse>>>> = Arc::default();
        let (tx, mut rx) = mpsc::channel(4);
        fail(&closed, &pending, &tx, "agent exited (0)");
        fail(&closed, &pending, &tx, "agent closed its stdout");
        assert_eq!(closed.lock().unwrap().as_deref(), Some("agent exited (0)"));
        assert!(matches!(rx.try_recv(), Ok(Incoming::Closed { .. })));
    }

    #[tokio::test]
    async fn reader_routes_responses_by_id_and_forwards_events() {
        let (client_side, mut agent_side) = tokio::io::duplex(64 * 1024);
        let pending: Arc<Mutex<HashMap<RequestId, oneshot::Sender<RpcResponse>>>> = Arc::default();
        let closed: Arc<Mutex<Option<String>>> = Arc::default();
        let (events, mut event_rx) = mpsc::channel(8);
        let reader = tokio::spawn(reader_task(
            client_side,
            pending.clone(),
            events,
            closed.clone(),
        ));

        let (tx, rx) = oneshot::channel();
        pending.lock().unwrap().insert(RequestId(7), tx);
        let reply = json!({"jsonrpc": "2.0", "id": 7, "result": {"ok": true}});
        let event = json!({"jsonrpc": "2.0", "method": "agent.event", "params": {
            "type": "session_closed",
            "data": {"session_id": "s", "meta": {"session_id": "s", "dropped_before": 0}}
        }});
        agent_side
            .write_all(format!("{reply}\n{event}\n").as_bytes())
            .await
            .unwrap();
        agent_side.flush().await.unwrap();

        let response = rx.await.unwrap();
        assert_eq!(response.id, RequestId(7));
        let Some(Incoming::Event(event)) = event_rx.recv().await else {
            panic!("expected an event");
        };
        assert!(matches!(*event, AgentEvent::SessionClosed { .. }));

        // EOF closes the client with a visible reason and wakes the waiter.
        let (waiting, _waiting_rx) = oneshot::channel();
        pending.lock().unwrap().insert(RequestId(8), waiting);
        drop(agent_side);
        reader.await.unwrap();
        assert!(closed.lock().unwrap().is_some());
        assert!(pending.lock().unwrap().is_empty());
        assert!(matches!(
            event_rx.recv().await,
            Some(Incoming::Closed { .. })
        ));
    }

    #[tokio::test]
    async fn a_malformed_frame_stops_the_stream() {
        let (client_side, mut agent_side) = tokio::io::duplex(4096);
        let pending: Arc<Mutex<HashMap<RequestId, oneshot::Sender<RpcResponse>>>> = Arc::default();
        let closed: Arc<Mutex<Option<String>>> = Arc::default();
        let (events, _event_rx) = mpsc::channel(8);
        let reader = tokio::spawn(reader_task(client_side, pending, events, closed.clone()));
        agent_side.write_all(b"not json\n").await.unwrap();
        agent_side.flush().await.unwrap();
        reader.await.unwrap();
        assert!(
            closed
                .lock()
                .unwrap()
                .as_deref()
                .unwrap()
                .contains("unreadable frame")
        );
    }
}
