//! The agent child process and its JSON-RPC client. By default the child is this
//! same executable running the built-in Agent.
//!
//! Four background tasks own the pipes: one writer, one stdout reader, one
//! stderr reader (bounded tail) and one child waiter. Requests are routed by id
//! to a `oneshot`; events and child loss are posted to the app channel.

use std::collections::{HashMap, VecDeque};
use std::path::Path;
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

#[cfg(feature = "e2e-observe")]
pub mod metrics;

/// One incoming frame from the child, in arrival order.
pub enum Incoming {
    Event {
        event: Box<AgentEvent>,
        /// Encoded size of the line this event was decoded from.
        bytes: usize,
    },
    /// The child is gone: no further frames will arrive.
    Closed { status: Option<String> },
}

/// Upper bound for one frame the Agent sends (a `session_snapshot` notification
/// or a receipt): 64 MiB fits a full snapshot of 32 queued 256 KiB inputs, the
/// execution input, a maximum interaction and a 256-entry catalogue.
pub const CONTROL_FRAME_MAX_BYTES: usize = 64 * 1024 * 1024;
/// Upper bound for one line from the Agent, envelope and newline included.
const MAX_FRAME_BYTES: usize = CONTROL_FRAME_MAX_BYTES;
/// Upper bound for one line to the Agent. It admits an 8 MiB request; a legal
/// `input.push` carrying one
/// 256 KiB maximum input escapes to 1,573,049 bytes, so this clears the largest
/// legal request with room for the method, the id and the load.
pub const MAX_REQUEST_BYTES: usize = 8 * 1024 * 1024;
const STDERR_TAIL_LINES: usize = 200;
const OUTBOUND_CAPACITY: usize = 64;
/// The Agent admits 4 concurrent read queries; the rest are retried on
/// `-32019`, which is worse than pacing them here.
const QUERY_PERMITS: usize = 3;
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);
const TERMINATE_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Default)]
struct Shutdown {
    child: Option<ChildOwner>,
    complete: bool,
}

struct ChildOwner {
    task: JoinHandle<()>,
    terminate: Option<oneshot::Sender<()>>,
}

pub struct Client {
    #[cfg(feature = "e2e-observe")]
    pub metrics: Arc<metrics::Metrics>,
    outbound: mpsc::Sender<Vec<u8>>,
    pending: Arc<Mutex<HashMap<RequestId, oneshot::Sender<RpcResponse>>>>,
    next_id: AtomicU64,
    queries: Arc<Semaphore>,
    read_workspaces: Arc<Semaphore>,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    closed: Arc<Mutex<Option<String>>>,
    incoming: mpsc::Sender<Incoming>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    shutdown: tokio::sync::Mutex<Shutdown>,
}

/// The command serving the Agent on stdio: `binary` when set, else the Agent
/// built into this executable as its `agent` subcommand.
pub fn agent_command(binary: Option<&Path>, config: &Path) -> std::io::Result<Command> {
    let mut command = match binary {
        Some(binary) => Command::new(binary),
        None => {
            let mut command = Command::new(std::env::current_exe()?);
            command.arg(AGENT_SUBCOMMAND);
            command
        }
    };
    command.arg("--config").arg(config).arg("--stdio");
    #[cfg(feature = "e2e-observe")]
    command
        .env_remove("MINICORE_E2E_TRACE")
        .env_remove("MINICORE_E2E_APPLY_GATE")
        .env_remove("MINICORE_E2E_PHONE_GATE");
    Ok(command)
}

/// The first argument that turns this executable into the Agent.
pub const AGENT_SUBCOMMAND: &str = "agent";

impl Client {
    /// Spawns the agent `command` and starts its pipe tasks. `incoming` receives
    /// events and the terminal close notice.
    pub fn spawn(
        mut command: Command,
        incoming: mpsc::Sender<Incoming>,
    ) -> Result<Arc<Self>, RpcError> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| {
                let program = Path::new(command.as_std().get_program());
                RpcError::closed(format!("could not start {}: {error}", program.display()))
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

        let client = Self::start(stdin, stdout, stderr, incoming);
        let (terminate, termination) = oneshot::channel();
        let task = tokio::spawn(waiter_task(
            child,
            termination,
            client.closed.clone(),
            client.pending.clone(),
            client.incoming.clone(),
        ));
        client.shutdown.try_lock().unwrap().child = Some(ChildOwner {
            task,
            terminate: Some(terminate),
        });
        Ok(client)
    }

    /// Wires a client to already-open pipes. The child-process launcher and the
    /// tests that drive a scripted Agent both go through here.
    pub fn start<W, R, E>(
        stdin: W,
        stdout: R,
        stderr: E,
        incoming: mpsc::Sender<Incoming>,
    ) -> Arc<Self>
    where
        W: AsyncWrite + Unpin + Send + 'static,
        R: AsyncRead + Unpin + Send + 'static,
        E: AsyncRead + Unpin + Send + 'static,
    {
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
        Arc::new(Self {
            #[cfg(feature = "e2e-observe")]
            metrics: Arc::default(),
            outbound,
            pending,
            next_id: AtomicU64::new(1),
            queries: Arc::new(Semaphore::new(QUERY_PERMITS)),
            read_workspaces: Arc::new(Semaphore::new(2)),
            stderr_tail,
            closed,
            incoming,
            tasks: Mutex::new(vec![writer, reader, stderr_reader]),
            shutdown: tokio::sync::Mutex::new(Shutdown::default()),
        })
    }

    /// Sends one request and awaits its response. Read queries wait for one of
    /// the client's query permits first.
    pub async fn call<P: serde::Serialize, R: DeserializeOwned>(
        &self,
        method: &'static str,
        params: &P,
    ) -> Result<R, RpcError> {
        let _permit = if is_query(method) {
            Some(self.queries.acquire().await.map_err(|_| closed())?)
        } else {
            None
        };
        #[cfg(feature = "e2e-observe")]
        let _observation = _permit.as_ref().map(|_| self.metrics.query());
        let response = self.call_raw(method, params).await?;
        response.decode()
    }

    pub async fn call_raw<P: serde::Serialize>(
        &self,
        method: &'static str,
        params: &P,
    ) -> Result<RpcResponse, RpcError> {
        let id = RequestId(self.next_id.fetch_add(1, Ordering::Relaxed));
        let mut params = serde_json::to_value(params).map_err(|error| {
            before_write_error(
                method,
                RpcError::new(-32602, "invalid_params", error.to_string()),
            )
        })?;
        // The Agent accepts omitted params or an object, never JSON null.
        if params.is_null() {
            params = Value::Object(Default::default());
        }
        let line = encode_request(OutgoingRequest::new(id, method, params))
            .map_err(|error| before_write_error(method, error))?;
        let rx = {
            let closed = self.closed.lock().unwrap();
            if let Some(reason) = closed.as_ref() {
                return Err(before_write_error(method, RpcError::closed(reason.clone())));
            }
            let (tx, rx) = oneshot::channel();
            self.pending.lock().unwrap().insert(id, tx);
            rx
        };
        match self.outbound.send(line).await {
            Ok(()) =>
            {
                #[cfg(feature = "e2e-observe")]
                if is_query(method) {
                    self.metrics.sent(method);
                }
            }
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                return Err(before_write_error(
                    method,
                    RpcError::closed("agent is not accepting requests"),
                ));
            }
        }
        rx.await.map_err(|_| {
            self.pending.lock().unwrap().remove(&id);
            RpcError::closed("agent closed before answering")
        })
    }

    pub fn stderr_tail(&self) -> Vec<String> {
        self.stderr_tail.lock().unwrap().iter().cloned().collect()
    }

    pub(crate) fn read_workspaces(&self) -> Arc<Semaphore> {
        self.read_workspaces.clone()
    }

    /// Sends `agent.shutdown` and gives the child a bounded moment to exit
    /// before it is killed. Safe to call more than once.
    pub async fn shutdown(&self) {
        let mut shutdown = self.shutdown.lock().await;
        if shutdown.complete {
            return;
        }
        let _ = tokio::time::timeout(
            SHUTDOWN_TIMEOUT,
            self.call_raw("agent.shutdown", &Value::Null),
        )
        .await;
        if let Some(child) = &mut shutdown.child
            && tokio::time::timeout(SHUTDOWN_TIMEOUT, &mut child.task)
                .await
                .is_err()
        {
            if let Some(terminate) = child.terminate.take() {
                let _ = terminate.send(());
            }
            if tokio::time::timeout(TERMINATE_TIMEOUT, &mut child.task)
                .await
                .is_err()
            {
                child.task.abort();
                let _ = (&mut child.task).await;
            }
        }
        for task in self.tasks.lock().unwrap().drain(..) {
            task.abort();
        }
        shutdown.child = None;
        shutdown.complete = true;
    }
}

fn closed() -> RpcError {
    RpcError::closed("rpc client is shutting down")
}

fn before_write_error(method: &str, mut error: RpcError) -> RpcError {
    if method == crate::protocol::METHOD_INPUT_PUSH {
        error.acceptance = Some(crate::protocol::Acceptance::Rejected);
    }
    error
}

/// Methods that hold one of the Agent's read-query slots.
fn is_query(method: &str) -> bool {
    method == crate::protocol::METHOD_SESSION_READ
        || method == crate::protocol::METHOD_TURN_RESULT
        || ["tool.", "workspace.", "changes."]
            .iter()
            .any(|prefix| method.starts_with(prefix))
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
                let event = Incoming::Event {
                    event: Box::new(event),
                    bytes: line.len(),
                };
                if incoming.send(event).await.is_err() {
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
    termination: oneshot::Receiver<()>,
    closed: Arc<Mutex<Option<String>>>,
    pending: Arc<Mutex<HashMap<RequestId, oneshot::Sender<RpcResponse>>>>,
    incoming: mpsc::Sender<Incoming>,
) {
    let result = tokio::select! {
        result = child.wait() => result.map(|status|status.to_string()),
        _ = termination => child.kill().await.map(|()|"terminated".to_owned()),
    };
    let status = match result {
        Ok(status) => status,
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

#[cfg(test)]
mod tests {
    #[cfg(feature = "e2e-observe")]
    #[test]
    fn acceptance_trace_and_application_gate_are_not_inherited_by_agent_tools() {
        let command = super::agent_command(
            Some(std::path::Path::new("test-agent")),
            std::path::Path::new("config.toml"),
        )
        .unwrap();
        let environment: std::collections::HashMap<_, _> = command.as_std().get_envs().collect();
        for name in [
            "MINICORE_E2E_TRACE",
            "MINICORE_E2E_APPLY_GATE",
            "MINICORE_E2E_PHONE_GATE",
        ] {
            assert_eq!(environment.get(std::ffi::OsStr::new(name)), Some(&None));
        }
    }
    use super::*;
    use serde_json::json;

    #[tokio::test(start_paused = true)]
    async fn shutdown_keeps_a_full_child_exit_grace_after_a_delayed_ack_application() {
        use std::{future::Future, task::Poll};
        let (tx, _rx) = mpsc::unbounded_channel();
        let (fake, _app) = crate::testing::FakeAgent::start(tx);
        fake.script("agent.shutdown", json!({"shutdown":true}));
        fake.script("test.fence", json!({"fenced":true}));
        let (terminate, mut termination) = oneshot::channel();
        let (release, released) = oneshot::channel();
        let task = tokio::spawn(async move {
            released.await.unwrap();
        });
        fake.client.shutdown.lock().await.child = Some(ChildOwner {
            task,
            terminate: Some(terminate),
        });
        let mut shutdown = Box::pin(fake.client.shutdown());
        std::future::poll_fn(|context| {
            assert!(shutdown.as_mut().poll(context).is_pending());
            Poll::Ready(())
        })
        .await;
        fake.wait_for_calls("agent.shutdown", 1).await;
        let _: Value = fake.client.call("test.fence", &json!({})).await.unwrap();
        tokio::time::advance(SHUTDOWN_TIMEOUT + Duration::from_millis(1)).await;
        std::future::poll_fn(|context| {
            assert!(shutdown.as_mut().poll(context).is_pending());
            Poll::Ready(())
        })
        .await;
        assert!(
            matches!(
                termination.try_recv(),
                Err(oneshot::error::TryRecvError::Empty)
            ),
            "acknowledgement latency must not consume the actual child owner's exit grace"
        );
        release.send(()).unwrap();
        shutdown.await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_push_closed_during_encoding_is_rejected_before_enqueue_without_an_orphaned_call() {
        struct CloseWhileEncoding(Arc<Client>);
        impl serde::Serialize for CloseWhileEncoding {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                *self.0.closed.lock().unwrap() = Some("closed during encoding".into());
                serde::Serialize::serialize(&json!({"text":"retained draft"}), serializer)
            }
        }
        let (tx, _rx) = mpsc::unbounded_channel();
        let (fake, _app) = crate::testing::FakeAgent::start(tx);
        let error = tokio::time::timeout(
            Duration::from_secs(1),
            fake.client.call_raw(
                crate::protocol::METHOD_INPUT_PUSH,
                &CloseWhileEncoding(fake.client.clone()),
            ),
        )
        .await
        .expect("closure before enqueue must not leave an orphaned pending call")
        .unwrap_err();
        assert_eq!(
            error.acceptance,
            Some(crate::protocol::Acceptance::Rejected)
        );
        assert!(fake.client.pending.lock().unwrap().is_empty());
        assert!(fake.calls("input.push").is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn concurrent_shutdown_callers_share_one_ack_and_completed_shutdown_is_idempotent() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let (fake, _app) = crate::testing::FakeAgent::start(tx);
        fake.script_held("agent.shutdown", json!({"shutdown":true}));
        let first = fake.client.clone();
        let second = first.clone();
        let first = tokio::spawn(async move { first.shutdown().await });
        let second = tokio::spawn(async move { second.shutdown().await });
        fake.wait_for_calls("agent.shutdown", 1).await;
        fake.release("agent.shutdown");
        crate::testing::real_bounded(async {
            first.await.unwrap();
            second.await.unwrap();
        })
        .await;
        fake.client.shutdown().await;
        assert_eq!(fake.calls("agent.shutdown").len(), 1);
        assert!(fake.client.tasks.lock().unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn an_acknowledged_child_is_only_forced_at_the_deadline_and_its_owner_is_joined() {
        use std::{future::Future, task::Poll};
        let (tx, _rx) = mpsc::unbounded_channel();
        let (fake, _app) = crate::testing::FakeAgent::start(tx);
        fake.script("agent.shutdown", json!({"shutdown":true}));
        fake.script("test.fence", json!({"fenced":true}));
        let (terminate, termination) = oneshot::channel();
        let (signalled, signal) = oneshot::channel();
        let (release, released) = oneshot::channel();
        let owner = tokio::spawn(async move {
            termination.await.unwrap();
            signalled.send(()).unwrap();
            released.await.unwrap();
        });
        let owner_finished = owner.abort_handle();
        fake.client.shutdown.lock().await.child = Some(ChildOwner {
            task: owner,
            terminate: Some(terminate),
        });
        let mut shutdown = Box::pin(fake.client.shutdown());
        std::future::poll_fn(|context| {
            assert!(shutdown.as_mut().poll(context).is_pending());
            Poll::Ready(())
        })
        .await;
        fake.wait_for_calls("agent.shutdown", 1).await;
        let _: Value = fake.client.call("test.fence", &json!({})).await.unwrap();
        std::future::poll_fn(|context| {
            assert!(shutdown.as_mut().poll(context).is_pending());
            Poll::Ready(())
        })
        .await;
        assert!(!owner_finished.is_finished());
        tokio::time::advance(SHUTDOWN_TIMEOUT).await;
        std::future::poll_fn(|context| {
            assert!(shutdown.as_mut().poll(context).is_pending());
            Poll::Ready(())
        })
        .await;
        crate::testing::real_bounded(signal).await.unwrap();
        assert!(
            !owner_finished.is_finished(),
            "shutdown still owns the forced-exit join"
        );
        release.send(()).unwrap();
        shutdown.await;
        assert!(owner_finished.is_finished());
        assert!(fake.client.tasks.lock().unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_child_owner_that_ignores_forced_exit_is_aborted_after_the_bounded_fallback() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let (fake, _app) = crate::testing::FakeAgent::start(tx);
        fake.script("agent.shutdown", json!({"shutdown":true}));
        let (terminate, _termination) = oneshot::channel();
        let task = tokio::spawn(std::future::pending::<()>());
        let finished = task.abort_handle();
        fake.client.shutdown.lock().await.child = Some(ChildOwner {
            task,
            terminate: Some(terminate),
        });
        let started = tokio::time::Instant::now();
        fake.client.shutdown().await;
        assert!(finished.is_finished());
        assert_eq!(started.elapsed(), SHUTDOWN_TIMEOUT + TERMINATE_TIMEOUT);
    }

    /// The escape-heavy input whose `input.push` frame integration measured at
    /// 1,573,049 bytes: 262,144 NUL bytes, each escaped to `\u0000`.
    fn maximum_input() -> String {
        "\u{0}".repeat(262_144)
    }

    #[test]
    fn a_legal_maximum_input_frame_is_written_in_full() {
        let text = maximum_input();
        let request = OutgoingRequest::new(
            RequestId(7),
            crate::protocol::METHOD_INPUT_PUSH,
            json!({"session_id": "ses_1", "load": "load_1", "mode": "steer", "text": text}),
        );
        let line = encode_request(request).expect("a legal maximum input must be sendable");
        // Integration measured a legal maximum push at 1,573,049 bytes; the
        // bound must clear it with room for a richer envelope than this one.
        // A legal maximum input must fit with room, not just barely.
        let measured = 1_573_049;
        assert!(
            MAX_REQUEST_BYTES >= 2 * measured,
            "headroom for the envelope"
        );
        assert_eq!(line.last(), Some(&b'\n'), "the line is LF-terminated");
        assert!(
            line.len() < MAX_REQUEST_BYTES,
            "the bound must leave room for the line and its newline"
        );
        // Nothing sliced the body: the input survives whole.
        let frame: serde_json::Value =
            serde_json::from_slice(&line[..line.len() - 1]).expect("a JSON frame");
        assert_eq!(frame["params"]["text"].as_str(), Some(text.as_str()));
    }

    #[test]
    fn a_request_one_byte_over_the_bound_is_refused_before_writing() {
        // The encoded frame is exactly the bound, so one more byte is illegal.
        let envelope = serde_json::to_vec(&OutgoingRequest::new(
            RequestId(7),
            crate::protocol::METHOD_INPUT_PUSH,
            json!({"text": ""}),
        ))
        .expect("a frame");
        // `encode_request` counts the newline it appends, so the largest
        // sendable frame is one byte under the bound before that newline.
        let over = MAX_REQUEST_BYTES - 1 - envelope.len();
        let request = OutgoingRequest::new(
            RequestId(7),
            crate::protocol::METHOD_INPUT_PUSH,
            json!({"text": "x".repeat(over)}),
        );
        let line = serde_json::to_vec(&request).expect("a frame");
        assert!(
            line.len() < MAX_REQUEST_BYTES,
            "the at-the-bound frame must be sendable"
        );
        encode_request(request).expect("a frame at the bound is written");
        let error = encode_request(OutgoingRequest::new(
            RequestId(8),
            crate::protocol::METHOD_INPUT_PUSH,
            json!({"text": "x".repeat(over + 1)}),
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
        let Some(Incoming::Event { event, .. }) = event_rx.recv().await else {
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

#[cfg(test)]
mod frame_limit_tests {
    use super::*;
    use serde_json::json;
    use tokio::io::AsyncWriteExt;

    /// Builds the control frame integration measured at 52,237,130 bytes: a
    /// full snapshot with 32 queued maximum inputs, the execution input, a
    /// maximum interaction and a 256-entry catalogue.
    fn measured_snapshot_frame() -> Vec<u8> {
        let text = "\u{0}".repeat(262_144);
        let items: Vec<serde_json::Value> = (0..32)
            .map(|index| json!({"id": format!("inp_{index}"), "mode": "steer", "text": text}))
            .collect();
        let entries: Vec<serde_json::Value> = (0..256)
            .map(|seq| {
                json!({"seq": seq, "loop_id": format!("lup_{seq}"),
                       "input_id": format!("inp_{seq}"), "completion_kind": "reported",
                       "outcome_kind": "completed", "persistence": "persisted",
                       "availability": "stored"})
            })
            .collect();
        let mut line = serde_json::to_vec(&json!({
            "jsonrpc": "2.0", "method": "agent.event",
            "params": {"type": "session_snapshot", "data": {
                "meta": {"session_id": "ses_1", "load": "load_1", "dropped_before": 0},
                "snapshot": {
                    "session_id": "ses_1", "load": "load_1", "revision": 9,
                    "execution": {"type": "running", "loop_id": "lup_1",
                                  "input": {"id": "inp_1", "mode": "steer", "text": text},
                                  "request_index": 0, "phase": "running_model"},
                    "pending_interaction": {
                        "loop_id": "lup_1", "request_index": 0,
                        "interaction_id": "int_1", "tool_call_id": "cal_1",
                        "tool_name": "bash", "kind": {"type": "approval",
                                                      "data": {"prompt": text}}},
                    "queue": {"items": items, "paused": false},
                    "compaction": null, "block_reason": null,
                    "completions": {"through_seq": 256, "retained_from_seq": 1,
                                    "entries": entries, "truncated": false}
                }
            }}
        }))
        .expect("a frame");
        line.push(b'\n');
        line
    }

    #[tokio::test]
    async fn a_full_control_frame_is_read_and_decoded_whole() {
        let frame = measured_snapshot_frame();
        // The measurement integration reported is what the bound must admit;
        // this fixture adds the envelope fields the client itself writes.
        assert!(
            frame.len() >= 52_237_130,
            "this fixture is smaller than the measured legal snapshot"
        );
        assert!(
            frame.len() <= MAX_FRAME_BYTES,
            "a measured legal control frame must fit the reader bound"
        );
        let (mut writer, reader) = tokio::io::duplex(64 * 1024);
        let feed = frame.clone();
        let pump = tokio::spawn(async move {
            writer.write_all(&feed).await.expect("the frame is written");
            writer.shutdown().await.expect("the stream ends");
        });
        let mut buffered = tokio::io::BufReader::new(reader);
        let mut line = Vec::new();
        let read = read_line(&mut buffered, &mut line)
            .await
            .expect("a readable line");
        assert_eq!(read, frame.len(), "the whole line is read, not a prefix");
        assert_eq!(line.len(), frame.len());
        let decoded = crate::protocol::parse_frame(&line[..line.len() - 1])
            .expect("the whole control frame decodes");
        let crate::protocol::IncomingFrame::Event(event) = decoded else {
            panic!("a snapshot notification must decode as an event");
        };
        let crate::protocol::AgentEvent::SessionSnapshot { snapshot, .. } = event else {
            panic!("a snapshot event");
        };
        assert_eq!(snapshot.queue.items.len(), 32, "no queued input was lost");
        assert_eq!(snapshot.queue.items[31].text.len(), 262_144);
        assert_eq!(snapshot.completions.entries.len(), 256);
        pump.await.expect("the pump finished");
    }
}
