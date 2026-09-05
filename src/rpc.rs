//! The minicore-agent stdio RPC process: child lifecycle, one stdin writer,
//! one stdout reader, one stderr reader, and a bounded event channel
//! (development spec 10). Background tasks only emit `RpcEvent`s; they never
//! touch app state.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::event::RpcEvent;
use crate::protocol::{FrameError, FrameErrorKind, OutgoingRequest, parse_frame};

/// Upper bound for one incoming RPC frame, excluding the trailing newline
/// (spec 10.7). The reader enforces the bound before any byte is appended, so
/// a malicious or corrupted line can never grow unbounded in memory.
pub const MAX_RPC_FRAME_BYTES: usize = 32 * 1024 * 1024;
/// Upper bound for one captured agent stderr line (spec 10.8).
pub const MAX_AGENT_LOG_LINE_BYTES: usize = 4096;
/// The agent contract bounds one request line at 1 MiB including the
/// terminating newline (docs/rpc-contract.md). `RpcProcess::send` enforces it
/// on the serialized bytes and rejects oversized requests locally with
/// `RpcError::RequestTooLarge`, so the agent never answers them with a
/// null-id parse error and the connection never turns fatal for them.
pub const MAX_REQUEST_LINE_BYTES: usize = 1024 * 1024;

const REQUESTS_CHANNEL_CAPACITY: usize = 64;
const EVENTS_CHANNEL_CAPACITY: usize = 128;
const READ_CHUNK_BYTES: usize = 8192;
const KILL_CHANNEL_CAPACITY: usize = 1;
/// Bounded wait after a kill request for the waiter task to reap the child.
pub const TERMINATE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Owns the agent child and its stdio tasks.
///
/// The child is owned by a dedicated waiter task. `send` enforces the
/// outbound request-line bound and hands the serialized line to the single
/// writer task; `recv` yields events from the reader tasks. Dropping the
/// process aborts the background tasks and lets Tokio's `kill_on_drop` child
/// fallback terminate the agent. Drop cannot synchronously reap an
/// asynchronous child; normal success and error paths must call
/// [`RpcProcess::terminate`] before terminal restoration. Must be created
/// inside a Tokio runtime.
///
/// Request ids are chosen by the caller (the app state in `App::update`);
/// this type only forwards already-numbered requests.
#[derive(Debug)]
pub struct RpcProcess {
    requests: mpsc::Sender<Vec<u8>>,
    events: mpsc::Receiver<RpcEvent>,
    kill: mpsc::Sender<()>,
    tasks: Vec<JoinHandle<()>>,
    /// Latched when an `Exited` event was consumed; the waiter task reaped
    /// the child by then, so `terminate` has nothing left to wait for.
    seen_exit: bool,
}

impl RpcProcess {
    /// Spawns `agent_bin --config <agent_config> --stdio` and starts the four
    /// background tasks. All fallible steps run before any task is started,
    /// so a spawn failure leaves nothing behind to clean up.
    pub fn spawn(agent_bin: &Path, agent_config: &Path) -> Result<Self, RpcError> {
        Self::spawn_with_env(agent_bin, agent_config, &[])
    }

    /// Spawns the agent process with optional additional environment variables.
    pub fn spawn_with_env(
        agent_bin: &Path,
        agent_config: &Path,
        envs: &[(&str, &str)],
    ) -> Result<Self, RpcError> {
        if !agent_config.is_file() {
            return Err(RpcError::ConfigMissing(agent_config.to_path_buf()));
        }
        let mut command = Self::spawn_command(agent_bin, agent_config);
        for &(key, val) in envs {
            command.env(key, val);
        }
        let child = command.spawn().map_err(RpcError::Spawn)?;
        Self::from_child(child)
    }

    /// The exact production launch shape (spec 10.5): the agent is always
    /// started as `agent --config <path> --stdio` with piped stdio. Test
    /// harnesses never route through this — they drive
    /// [`RpcProcess::from_child`] directly so production assembly stays
    /// untouched.
    fn spawn_command(agent_bin: &Path, agent_config: &Path) -> Command {
        let mut command = Command::new(agent_bin);
        command
            .arg("--config")
            .arg(agent_config)
            .arg("--stdio")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command
    }

    /// Wires the four background tasks around an already-spawned child.
    /// All fallible steps run before any task is started. Unit tests drive
    /// the full production stdin/stdout/stderr/waiter pipeline through this
    /// with a fake-agent child (see the `tests` module).
    fn from_child(mut child: Child) -> Result<Self, RpcError> {
        let stdin = child.stdin.take().expect("agent stdin is piped");
        let stdout = child.stdout.take().expect("agent stdout is piped");
        let stderr = child.stderr.take().expect("agent stderr is piped");

        let (requests_tx, requests_rx) = mpsc::channel(REQUESTS_CHANNEL_CAPACITY);
        let (events_tx, events_rx) = mpsc::channel(EVENTS_CHANNEL_CAPACITY);
        let (kill_tx, kill_rx) = mpsc::channel(KILL_CHANNEL_CAPACITY);

        let tasks = vec![
            tokio::spawn(stdin_writer(stdin, requests_rx, events_tx.clone())),
            tokio::spawn(stdout_reader(stdout, events_tx.clone())),
            tokio::spawn(stderr_reader(stderr, events_tx.clone())),
            tokio::spawn(child_waiter(child, kill_rx, events_tx)),
        ];

        Ok(Self {
            requests: requests_tx,
            events: events_rx,
            kill: kill_tx,
            tasks,
            seen_exit: false,
        })
    }

    /// Sends one already-numbered request as a single NDJSON line. The id
    /// was allocated by the caller and must already be registered in its
    /// pending map before this returns, so a response can never beat the
    /// registration.
    ///
    /// The serialized line (including the trailing newline) must fit the
    /// agent's 1 MiB request bound; oversized requests fail with the typed
    /// `RpcError::RequestTooLarge` (actual and maximum byte counts only, no
    /// content) and are never written to the child. Channel or serialization
    /// failures are synchronous too; the caller reports them back to the
    /// app.
    pub async fn send(&self, request: OutgoingRequest) -> Result<(), RpcError> {
        let bytes = serde_json::to_vec(&request)?;
        let line_bytes = bytes.len() + 1;
        if line_bytes > MAX_REQUEST_LINE_BYTES {
            return Err(RpcError::RequestTooLarge {
                actual_bytes: line_bytes,
                max_bytes: MAX_REQUEST_LINE_BYTES,
            });
        }
        self.requests
            .send(bytes)
            .await
            .map_err(|_| RpcError::Closed)?;
        Ok(())
    }

    /// The next event from the agent; `None` once every background task has
    /// ended. Events originate from four tasks concurrently: no total order
    /// is promised across `Frame`, `ConnectionClosed`, `Exited`, and
    /// `ProtocolError` (see `crate::event`).
    pub async fn recv(&mut self) -> Option<RpcEvent> {
        let event = self.events.recv().await;
        if matches!(&event, Some(RpcEvent::Exited(_))) {
            self.mark_exit_seen();
        }
        event
    }

    /// Yields an event only if one is already buffered; `Ok(None)` means the
    /// channel is still open but empty, while `Err(Closed)` means every
    /// producer has ended. The distinction lets the main loop disable its RPC
    /// select arm instead of busy-looping after channel closure.
    pub fn try_recv(&mut self) -> Result<Option<RpcEvent>, RpcError> {
        let event = match self.events.try_recv() {
            Ok(event) => Some(event),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => None,
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                return Err(RpcError::Closed);
            }
        };
        if matches!(&event, Some(RpcEvent::Exited(_))) {
            self.mark_exit_seen();
        }
        Ok(event)
    }

    /// Stops the stdin writer after the child has exited. Without this, the
    /// writer would wait forever on its still-owned request sender and the
    /// event channel could never become fully closed.
    fn mark_exit_seen(&mut self) {
        if self.seen_exit {
            return;
        }
        self.seen_exit = true;
        let (closed, receiver) = mpsc::channel::<Vec<u8>>(1);
        drop(receiver);
        self.requests = closed;
    }

    /// Requests the waiter task to kill the agent child; the
    /// `RpcEvent::Exited` event follows. This is the control path for the
    /// future shutdown sequence: send `agent.shutdown`, wait for its
    /// response, then wait a bounded time for the child and call this when
    /// the deadline passes.
    pub fn kill_child(&self) {
        let _ = self.kill.try_send(());
    }

    /// Requests the waiter to kill the child and waits (bounded) for the
    /// `Exited` event, so the child is reaped before this returns. Idempotent:
    /// after a natural exit the waiter has already ended and the kill channel
    /// is closed, so this returns almost immediately. Safe to call on every
    /// shutdown path, including a clean one.
    pub async fn terminate(&mut self) {
        // The waiter already reaped the child (a clean exit, or a caller
        // consumed `Exited`); do not wait out the full timeout.
        if self.seen_exit {
            return;
        }
        self.kill_child();
        let _ = tokio::time::timeout(TERMINATE_TIMEOUT, async {
            while let Some(event) = self.recv().await {
                if matches!(event, RpcEvent::Exited(_)) {
                    break;
                }
            }
        })
        .await;
    }

    /// Kills the child, observes the exit, and then drains the independent
    /// stdout/stderr readers until the event channel closes or a bounded
    /// deadline expires. Unlike [`RpcProcess::terminate`], this deliberately
    /// continues after `Exited`, so final stderr lines and buffered frames are
    /// not discarded. The observer must not issue new RPCs from the returned
    /// app commands.
    pub async fn terminate_with_observer<F>(&mut self, mut observe: F)
    where
        F: FnMut(RpcEvent),
    {
        if !self.seen_exit {
            self.kill_child();
        }
        self.terminate_observing(&mut observe).await;
        if self.drain_events(&mut observe).await {
            self.join_tasks().await;
        } else {
            // A broken reader must not make shutdown unbounded. Preserve the
            // events already queued, then abort only tasks that failed to
            // finish inside the explicit drain deadline.
            while let Ok(Some(event)) = self.try_recv() {
                observe(event);
            }
            for task in &self.tasks {
                task.abort();
            }
            self.join_tasks().await;
        }
    }

    /// The observer version of the ordinary terminate wait. It retains the
    /// existing bounded wait and exit condition while ensuring events consumed
    /// on the way to `Exited` are delivered to the caller.
    async fn terminate_observing<F>(&mut self, observe: &mut F)
    where
        F: FnMut(RpcEvent),
    {
        // The waiter already reaped the child (a clean exit, or a caller
        // consumed `Exited`); do not wait out the full timeout.
        if self.seen_exit {
            return;
        }
        self.kill_child();
        let _ = tokio::time::timeout(TERMINATE_TIMEOUT, async {
            while let Some(event) = self.recv().await {
                let exited = matches!(event, RpcEvent::Exited(_));
                observe(event);
                if exited {
                    break;
                }
            }
        })
        .await;
    }

    /// Drains all events after the child exit, including final stderr output.
    /// `None` proves every event producer has ended; the timeout keeps a
    /// malfunctioning reader from blocking terminal restoration forever.
    async fn drain_events<F>(&mut self, observe: &mut F) -> bool
    where
        F: FnMut(RpcEvent),
    {
        let deadline = tokio::time::Instant::now() + TERMINATE_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return false;
            }
            match tokio::time::timeout(remaining, self.recv()).await {
                Ok(Some(event)) => observe(event),
                Ok(None) => return true,
                Err(_) => return false,
            }
        }
    }

    /// All senders have been dropped when the event channel closes, so the
    /// task handles are already complete. Joining them makes that fact
    /// explicit without extending the bounded drain.
    async fn join_tasks(&mut self) {
        while let Some(task) = self.tasks.pop() {
            let _ = task.await;
        }
    }

    /// Whether an `Exited` event was consumed; the child was reaped.
    pub fn child_reaped(&self) -> bool {
        self.seen_exit
    }
}

impl Drop for RpcProcess {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RpcError {
    #[error("agent config file does not exist: {0}")]
    ConfigMissing(PathBuf),
    #[error("failed to spawn the agent process: {0}")]
    Spawn(io::Error),
    #[error("the RPC process is closed")]
    Closed,
    #[error("failed to serialize the request: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error(
        "request line of {actual_bytes} bytes (including the newline) exceeds the {max_bytes} byte limit"
    )]
    RequestTooLarge {
        actual_bytes: usize,
        max_bytes: usize,
    },
}

/// Writes one NDJSON request line and flushes it (spec 10.5).
async fn write_ndjson_line<W>(writer: &mut W, line: &[u8]) -> io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    writer.write_all(line).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await
}

/// The only task that writes to the agent's stdin. Lines arrive already
/// serialized and bound-checked by `RpcProcess::send`.
async fn stdin_writer<W>(
    mut stdin: W,
    mut requests: mpsc::Receiver<Vec<u8>>,
    events: mpsc::Sender<RpcEvent>,
) where
    W: AsyncWrite + Unpin,
{
    while let Some(line) = requests.recv().await {
        if let Err(error) = write_ndjson_line(&mut stdin, &line).await {
            emit(
                &events,
                RpcEvent::ProtocolError(FrameError::new(
                    FrameErrorKind::Io,
                    format!("stdin write failed: {error}"),
                )),
            )
            .await;
            return;
        }
    }
}

/// The only task that reads the agent's stdout. One line at a time, with a
/// hard bound enforced before any byte is buffered; malformed or oversized
/// frames are fatal and reading stops (spec 10.6, 10.7).
async fn stdout_reader<R>(mut stdout: R, events: mpsc::Sender<RpcEvent>)
where
    R: AsyncRead + Unpin,
{
    let mut chunk = [0u8; READ_CHUNK_BYTES];
    let mut line = Vec::new();
    loop {
        let read = match stdout.read(&mut chunk).await {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) => {
                emit(
                    &events,
                    RpcEvent::ProtocolError(FrameError::new(
                        FrameErrorKind::Io,
                        format!("stdout read failed: {error}"),
                    )),
                )
                .await;
                return;
            }
        };
        for &byte in &chunk[..read] {
            if byte == b'\n' {
                let line = std::mem::take(&mut line);
                match parse_frame(&line) {
                    Ok(frame) => emit(&events, RpcEvent::Frame(frame)).await,
                    Err(error) => {
                        emit(&events, RpcEvent::ProtocolError(error)).await;
                        return;
                    }
                }
            } else if line.len() == MAX_RPC_FRAME_BYTES {
                emit(
                    &events,
                    RpcEvent::ProtocolError(FrameError::new(
                        FrameErrorKind::TooLarge,
                        format!("frame exceeds the {MAX_RPC_FRAME_BYTES} byte limit"),
                    )),
                )
                .await;
                return;
            } else {
                line.push(byte);
            }
        }
    }
    if !line.is_empty() {
        emit(
            &events,
            RpcEvent::ProtocolError(FrameError::new(
                FrameErrorKind::PartialFrame,
                "stdout closed mid-frame",
            )),
        )
        .await;
    }
    emit(&events, RpcEvent::ConnectionClosed).await;
}

/// The only task that reads the agent's stderr. Lines are capped at
/// `MAX_AGENT_LOG_LINE_BYTES` on a UTF-8 boundary; the agent's exit is
/// reported by the waiter task, so stderr errors end this task silently.
async fn stderr_reader<R>(mut stderr: R, events: mpsc::Sender<RpcEvent>)
where
    R: AsyncRead + Unpin,
{
    let mut chunk = [0u8; READ_CHUNK_BYTES];
    let mut line = Vec::new();
    loop {
        let read = match stderr.read(&mut chunk).await {
            Ok(0) => break,
            Ok(read) => read,
            Err(_) => break,
        };
        for &byte in &chunk[..read] {
            if byte == b'\n' {
                emit(&events, RpcEvent::AgentLogLine(agent_log_line(&line))).await;
                line.clear();
            } else if line.len() < MAX_AGENT_LOG_LINE_BYTES {
                line.push(byte);
            }
            // Bytes beyond the cap are dropped; memory stays bounded.
        }
    }
    if !line.is_empty() {
        emit(&events, RpcEvent::AgentLogLine(agent_log_line(&line))).await;
    }
}

/// The owner of the child process. A kill request interrupts the wait,
/// terminates the child and reaps it before the exit event is emitted.
async fn child_waiter(
    mut child: Child,
    mut kill: mpsc::Receiver<()>,
    events: mpsc::Sender<RpcEvent>,
) {
    let wait = tokio::select! {
        status = child.wait() => Some(status),
        _ = kill.recv() => None,
    };
    let status = match wait {
        Some(Ok(status)) => Some(status),
        _ => {
            let _ = child.start_kill();
            child.wait().await.ok()
        }
    };
    emit(&events, RpcEvent::Exited(status)).await;
}

/// Bounded stderr line conversion (spec 10.8): caps the bytes, never splits a
/// code point at the cap, and replaces remaining invalid bytes lossily.
fn agent_log_line(bytes: &[u8]) -> String {
    let capped = &bytes[..bytes.len().min(MAX_AGENT_LOG_LINE_BYTES)];
    let slice = match std::str::from_utf8(capped) {
        Ok(_) => capped,
        Err(error) => {
            let tail = &capped[error.valid_up_to()..];
            if partial_utf8_tail(tail) {
                &capped[..error.valid_up_to()]
            } else {
                capped
            }
        }
    };
    String::from_utf8_lossy(slice).into_owned()
}

/// True when `tail` is an incomplete multi-byte sequence at the end of a
/// line: continuation bytes whose lead was cut, or a lead byte whose
/// continuations were cut off by the cap.
fn partial_utf8_tail(tail: &[u8]) -> bool {
    let Some(lead) = tail.first() else {
        return false;
    };
    match lead_sequence_len(*lead) {
        None => tail.iter().all(|byte| byte & 0xC0 == 0x80),
        Some(total) => tail.len() < total && tail[1..].iter().all(|byte| byte & 0xC0 == 0x80),
    }
}

/// Expected total sequence length for a UTF-8 lead byte.
fn lead_sequence_len(byte: u8) -> Option<usize> {
    match byte {
        0xC2..=0xDF => Some(2),
        0xE0..=0xEF => Some(3),
        0xF0..=0xF4 => Some(4),
        _ => None,
    }
}

async fn emit(events: &mpsc::Sender<RpcEvent>, event: RpcEvent) {
    let _ = events.send(event).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{IncomingFrame, RequestId, RpcNotification};
    use serde_json::{Value, json};
    use std::time::Instant;
    use tokio::io::{DuplexStream, duplex};
    use tokio::time::{Duration, timeout};

    const TEST_TIMEOUT: Duration = Duration::from_secs(5);

    async fn next_event(events: &mut mpsc::Receiver<RpcEvent>) -> RpcEvent {
        timeout(TEST_TIMEOUT, events.recv())
            .await
            .expect("timed out waiting for an RPC event")
            .expect("RPC event channel closed")
    }

    /// Asserts that the reader produced no trailing event: either the channel
    /// stays silent or closes because the reader task ended (both mean the
    /// connection terminated). Only an actual event fails the assertion.
    async fn assert_no_further_events(events: &mut mpsc::Receiver<RpcEvent>) {
        if let Ok(Some(event)) = timeout(Duration::from_millis(200), events.recv()).await {
            panic!("unexpected trailing event: {event:?}");
        }
    }

    fn request(id: u64, method: &'static str) -> OutgoingRequest {
        OutgoingRequest::new(RequestId(id), method, json!({}))
    }

    fn request_with_params(id: u64, params: Value) -> OutgoingRequest {
        OutgoingRequest::new(RequestId(id), "agent.ping", params)
    }

    /// Builds a process whose writer task is fed from the returned server
    /// half, so tests can observe exactly what would reach the child's stdin.
    fn test_process() -> (RpcProcess, DuplexStream) {
        let (client, server) = duplex(2 * 1024 * 1024);
        let (requests_tx, requests_rx) = mpsc::channel(8);
        let (events_tx, events_rx) = mpsc::channel(8);
        let (kill_tx, _kill_rx) = mpsc::channel(1);
        let process = RpcProcess {
            requests: requests_tx,
            events: events_rx,
            kill: kill_tx,
            tasks: vec![tokio::spawn(stdin_writer(client, requests_rx, events_tx))],
            seen_exit: false,
        };
        (process, server)
    }

    /// Builds a `params` value whose full request line (including the
    /// trailing newline) is exactly `target` bytes when serialized with
    /// request id 1.
    fn params_with_line_bytes(target: usize) -> Value {
        let mut fill = String::new();
        loop {
            let params = json!({ "x": fill.clone() });
            let line_bytes = serde_json::to_vec(&OutgoingRequest::new(
                RequestId(1),
                "agent.ping",
                params.clone(),
            ))
            .unwrap()
            .len()
                + 1;
            match line_bytes.cmp(&target) {
                std::cmp::Ordering::Equal => return params,
                std::cmp::Ordering::Less => fill.push_str(&"a".repeat(target - line_bytes)),
                std::cmp::Ordering::Greater => panic!("target below the fixed header size"),
            }
        }
    }

    #[tokio::test]
    async fn writer_emits_one_flushed_ndjson_line_per_request() {
        let (client, mut server) = duplex(1024);
        let (requests_tx, requests_rx) = mpsc::channel(4);
        let (events_tx, _events_rx) = mpsc::channel(4);
        tokio::spawn(stdin_writer(client, requests_rx, events_tx));

        requests_tx
            .send(serde_json::to_vec(&request(1, "agent.ping")).unwrap())
            .await
            .unwrap();
        requests_tx
            .send(serde_json::to_vec(&request(2, "model.list")).unwrap())
            .await
            .unwrap();
        drop(requests_tx);

        let mut expected = serde_json::to_vec(&request(1, "agent.ping")).unwrap();
        expected.push(b'\n');
        expected.extend_from_slice(&serde_json::to_vec(&request(2, "model.list")).unwrap());
        expected.push(b'\n');

        let mut all = Vec::new();
        timeout(TEST_TIMEOUT, server.read_to_end(&mut all))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(all, expected);
    }

    #[tokio::test]
    async fn reader_emits_frames_in_arrival_order_with_preserved_ids() {
        let (mut client, server) = duplex(16 * 1024);
        let (events_tx, mut events_rx) = mpsc::channel(16);
        tokio::spawn(stdout_reader(server, events_tx));

        let event = r#"{"jsonrpc":"2.0","method":"agent.event","params":{"type":"output_delta","data":{"turn":{"session_id":"ses_1","loop_id":"loop_1"},"request_index":0,"channel":"text","delta":"hi","meta":{"session_id":"ses_1","dropped_before":0}}}}"#;
        let response =
            |id: u64| format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{{"version":"0.3.0"}}}}"#);
        // Interleaved frames, with response ids arriving out of order. The
        // transport never reorders or correlates: each frame is emitted as
        // its bytes arrive and ids are preserved for the app to correlate
        // (spec 10.6); no ordering promise exists across the writer, readers,
        // and waiter tasks either.
        let payload = format!("{event}\n{}\n{}\n{event}\n", response(2), response(1));
        client.write_all(payload.as_bytes()).await.unwrap();

        let first = next_event(&mut events_rx).await;
        assert!(matches!(
            first,
            RpcEvent::Frame(IncomingFrame::Notification(RpcNotification::AgentEvent(_)))
        ));
        let second = next_event(&mut events_rx).await;
        match second {
            RpcEvent::Frame(IncomingFrame::Response(response)) => {
                assert_eq!(response.id, RequestId(2));
            }
            _ => panic!("expected response with id 2"),
        }
        let third = next_event(&mut events_rx).await;
        match third {
            RpcEvent::Frame(IncomingFrame::Response(response)) => {
                assert_eq!(response.id, RequestId(1));
            }
            _ => panic!("expected response with id 1"),
        }
        let fourth = next_event(&mut events_rx).await;
        assert!(matches!(
            fourth,
            RpcEvent::Frame(IncomingFrame::Notification(RpcNotification::AgentEvent(_)))
        ));

        drop(client);
        match next_event(&mut events_rx).await {
            RpcEvent::ConnectionClosed => {}
            _ => panic!("expected clean connection close"),
        }
    }

    #[tokio::test]
    async fn reader_accepts_exactly_max_size_frames() {
        let (mut client, server) = duplex(8192);
        let (events_tx, mut events_rx) = mpsc::channel(8);
        tokio::spawn(stdout_reader(server, events_tx));

        // A JSON string payload of exactly MAX_RPC_FRAME_BYTES in total
        // (quotes, filler, newline) is within the bound and fails only
        // envelope validation.
        let mut boundary = vec![b'a'; MAX_RPC_FRAME_BYTES];
        boundary[0] = b'"';
        boundary[MAX_RPC_FRAME_BYTES - 2] = b'"';
        boundary[MAX_RPC_FRAME_BYTES - 1] = b'\n';
        client.write_all(&boundary).await.unwrap();
        match next_event(&mut events_rx).await {
            RpcEvent::ProtocolError(error) if error.kind == FrameErrorKind::InvalidEnvelope => {}
            other => panic!("expected an envelope error, got: {other:?}"),
        }

        // The envelope error is fatal: the reader stopped and never parses
        // the valid frame written afterwards (the write may or may not
        // succeed depending on pipe state).
        let _ = client
            .write_all(br#"{"jsonrpc":"2.0","id":1,"result":{}}"#)
            .await;
        assert_no_further_events(&mut events_rx).await;
    }

    #[tokio::test]
    async fn reader_rejects_the_first_byte_beyond_the_max() {
        let (mut client, server) = duplex(8192);
        let (events_tx, mut events_rx) = mpsc::channel(8);
        tokio::spawn(stdout_reader(server, events_tx));

        let mut oversized = vec![b'x'; MAX_RPC_FRAME_BYTES + 1];
        oversized.push(b'\n');
        client.write_all(&oversized).await.unwrap();
        match next_event(&mut events_rx).await {
            RpcEvent::ProtocolError(error) if error.kind == FrameErrorKind::TooLarge => {}
            other => panic!("expected a size error, got: {other:?}"),
        }
        // The reader terminated before EOF: no close event follows.
        assert_no_further_events(&mut events_rx).await;
    }

    #[tokio::test]
    async fn reader_treats_malformed_frames_as_fatal_and_stops() {
        let (mut client, server) = duplex(4096);
        let (events_tx, mut events_rx) = mpsc::channel(8);
        tokio::spawn(stdout_reader(server, events_tx));

        client.write_all(b"not json\n").await.unwrap();
        match next_event(&mut events_rx).await {
            RpcEvent::ProtocolError(error) if error.kind == FrameErrorKind::InvalidJson => {}
            _ => panic!("expected an invalid JSON error"),
        }
        // A valid frame written afterwards is never read: no recovery. The
        // reader already dropped its half, so the write may fail.
        let _ = client
            .write_all(br#"{"jsonrpc":"2.0","id":1,"result":{}}"#)
            .await;
        assert_no_further_events(&mut events_rx).await;
    }

    #[tokio::test]
    async fn reader_reports_clean_eof_and_partial_frames() {
        let (mut client, server) = duplex(4096);
        let (events_tx, mut events_rx) = mpsc::channel(8);
        tokio::spawn(stdout_reader(server, events_tx));

        client
            .write_all(br#"{"jsonrpc":"2.0","id":1,"result":{}}{"#)
            .await
            .unwrap();
        drop(client);
        match next_event(&mut events_rx).await {
            RpcEvent::ProtocolError(error) if error.kind == FrameErrorKind::PartialFrame => {}
            _ => panic!("expected a partial frame error"),
        }
        match next_event(&mut events_rx).await {
            RpcEvent::ConnectionClosed => {}
            _ => panic!("expected the connection close after EOF"),
        }
    }

    #[tokio::test]
    async fn stderr_reader_emits_bounded_utf8_safe_lines() {
        let (mut client, server) = duplex(16 * 1024);
        let (events_tx, mut events_rx) = mpsc::channel(8);
        tokio::spawn(stderr_reader(server, events_tx));

        // A short line, a line beyond the cap, and a line where the cap cuts
        // inside a three-byte CJK code point.
        client.write_all(b"short line\n").await.unwrap();
        let mut long = vec![b'a'; MAX_AGENT_LOG_LINE_BYTES + 500];
        long.push(b'\n');
        let mut split = vec![b'b'; MAX_AGENT_LOG_LINE_BYTES - 1];
        split.extend_from_slice("你".as_bytes());
        split.push(b'\n');
        client.write_all(&long).await.unwrap();
        client.write_all(&split).await.unwrap();

        match next_event(&mut events_rx).await {
            RpcEvent::AgentLogLine(line) => assert_eq!(line, "short line"),
            _ => panic!("expected a log line"),
        }
        let long_line = match next_event(&mut events_rx).await {
            RpcEvent::AgentLogLine(line) => line,
            _ => panic!("expected a log line"),
        };
        assert_eq!(long_line.len(), MAX_AGENT_LOG_LINE_BYTES);
        let split_line = match next_event(&mut events_rx).await {
            RpcEvent::AgentLogLine(line) => line,
            _ => panic!("expected a log line"),
        };
        assert!(std::str::from_utf8(split_line.as_bytes()).is_ok());
        assert_eq!(split_line.len(), MAX_AGENT_LOG_LINE_BYTES - 1);
        assert!(!split_line.ends_with('\u{FFFD}'));
    }

    #[test]
    fn stderr_line_replaces_interior_invalid_bytes_lossily() {
        let mut bytes = b"ab".to_vec();
        bytes.push(0xFF);
        bytes.extend_from_slice(b"cd");
        assert_eq!(agent_log_line(&bytes), "ab\u{FFFD}cd");

        // A lead byte with a non-continuation neighbour is not a partial tail.
        let mut bytes = b"ab".to_vec();
        bytes.push(0xE4);
        bytes.push(b'A');
        assert_eq!(agent_log_line(&bytes), "ab\u{FFFD}A");
    }

    #[tokio::test]
    async fn send_forwards_pre_numbered_requests_without_touching_ids() {
        let (process, mut server) = test_process();

        // Ids come from the caller and pass through untouched.
        process.send(request(7, "agent.ping")).await.unwrap();
        process.send(request(8, "model.list")).await.unwrap();

        let mut lines = Vec::new();
        timeout(TEST_TIMEOUT, read_lines(&mut server, &mut lines, 2))
            .await
            .unwrap();
        assert_eq!(lines.len(), 2);
        let first: Value = serde_json::from_slice(&lines[0]).unwrap();
        let second: Value = serde_json::from_slice(&lines[1]).unwrap();
        assert_eq!(first["id"], json!(7));
        assert_eq!(second["id"], json!(8));
        assert_eq!(second["method"], "model.list");

        // The kill control path is idempotent from the caller's side.
        process.kill_child();
        process.kill_child();
        drop(process);
    }

    #[tokio::test]
    async fn send_accepts_request_lines_up_to_one_megabyte() {
        let (process, _server) = test_process();
        // Exactly 1 MiB including the terminating newline is accepted.
        let params = params_with_line_bytes(MAX_REQUEST_LINE_BYTES);
        process.send(request_with_params(1, params)).await.unwrap();
    }

    #[tokio::test]
    async fn send_rejects_oversized_lines_without_writing_them() {
        let (process, mut server) = test_process();
        // One byte over the bound: rejected with the typed error.
        let params = params_with_line_bytes(MAX_REQUEST_LINE_BYTES + 1);
        let error = process
            .send(request_with_params(1, params))
            .await
            .unwrap_err();
        match error {
            RpcError::RequestTooLarge {
                actual_bytes,
                max_bytes,
            } => {
                assert_eq!(actual_bytes, MAX_REQUEST_LINE_BYTES + 1);
                assert_eq!(max_bytes, MAX_REQUEST_LINE_BYTES);
            }
            other => panic!("expected a size error, got: {other:?}"),
        }
        assert!(error.to_string().contains("byte limit"));
        assert!(!error.to_string().contains("aaaa"));

        // Nothing reached the writer: the agent never sees the oversized line.
        let mut buf = [0u8; 64];
        assert!(
            timeout(Duration::from_millis(200), server.read(&mut buf))
                .await
                .is_err()
        );

        // UTF-8 counts in bytes, not characters: 400k three-byte CJK chars
        // exceed the bound despite a small character count.
        let (process, _server) = test_process();
        let error = process
            .send(request_with_params(1, json!({ "x": "你".repeat(400_000) })))
            .await
            .unwrap_err();
        match error {
            RpcError::RequestTooLarge { actual_bytes, .. } => {
                assert!(actual_bytes > MAX_REQUEST_LINE_BYTES);
            }
            other => panic!("expected a size error, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn recv_ends_when_all_sender_tasks_are_gone() {
        let (client, _server) = duplex(1024);
        let (requests_tx, requests_rx) = mpsc::channel(4);
        let (events_tx, events_rx) = mpsc::channel(4);
        let (kill_tx, _kill_rx) = mpsc::channel(1);
        // The writer is the only holder of the events sender. It watches the
        // requests channel; a surrogate sender sits in the process so the
        // writer's input can be closed without dropping the process.
        let writer = tokio::spawn(stdin_writer(client, requests_rx, events_tx));
        let (surrogate_tx, _surrogate_rx) = mpsc::channel(4);
        let mut process = RpcProcess {
            requests: surrogate_tx,
            events: events_rx,
            kill: kill_tx,
            tasks: vec![writer],
            seen_exit: false,
        };
        drop(requests_tx);
        assert!(
            timeout(TEST_TIMEOUT, process.recv())
                .await
                .unwrap()
                .is_none()
        );
        drop(process);
    }

    #[tokio::test]
    async fn try_recv_drains_all_buffered_events_before_reporting_closed() {
        let (events_tx, events_rx) = mpsc::channel(EVENTS_CHANNEL_CAPACITY);
        for index in 0..EVENTS_CHANNEL_CAPACITY {
            events_tx
                .send(RpcEvent::AgentLogLine(format!("event-{index}")))
                .await
                .unwrap();
        }
        drop(events_tx);
        let (requests_tx, _requests_rx) = mpsc::channel(1);
        let (kill_tx, _kill_rx) = mpsc::channel(1);
        let mut process = RpcProcess {
            requests: requests_tx,
            events: events_rx,
            kill: kill_tx,
            tasks: Vec::new(),
            seen_exit: false,
        };

        let mut received = 0;
        while let Ok(Some(RpcEvent::AgentLogLine(_))) = process.try_recv() {
            received += 1;
        }
        assert_eq!(received, EVENTS_CHANNEL_CAPACITY);
        assert!(matches!(process.try_recv(), Err(RpcError::Closed)));
    }

    #[tokio::test]
    async fn writer_failure_emits_a_protocol_error() {
        let (client, server) = duplex(1024);
        // The agent side is gone before the write: the writer must report a
        // typed pipe failure instead of silently losing the request.
        drop(server);
        let (requests_tx, requests_rx) = mpsc::channel(4);
        let (events_tx, mut events_rx) = mpsc::channel(4);
        tokio::spawn(stdin_writer(client, requests_rx, events_tx));
        requests_tx
            .send(serde_json::to_vec(&request(1, "agent.ping")).unwrap())
            .await
            .unwrap();
        match next_event(&mut events_rx).await {
            RpcEvent::ProtocolError(error) => assert_eq!(error.kind, FrameErrorKind::Io),
            other => panic!("expected an io protocol error, got: {other:?}"),
        }
    }

    /// Spawns this test binary restricted to one helper test via libtest's
    /// `--exact` switch, with stdio detached. The helpers live in this module
    /// and their full names are part of the invocation.
    fn spawn_helper(exact_test: &str) -> Child {
        let mut command = Command::new(std::env::current_exe().expect("test executable"));
        command
            .arg("--exact")
            .arg(exact_test)
            .arg("--nocapture")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        command
            .spawn()
            .expect("failed to spawn the helper test binary")
    }

    #[test]
    fn helper_quiet_exit() {
        // Spawned by `child_waiter_reports_natural_exit`; exits 0 at once.
    }

    #[test]
    fn helper_hangs() {
        // Sleeps only when spawned with libtest's `--exact` flag, so the
        // regular suite run returns immediately while the killed worker gets
        // a chance to hang.
        if std::env::args().any(|arg| arg == "--exact") {
            std::thread::sleep(Duration::from_secs(30));
        }
    }

    #[tokio::test]
    async fn child_waiter_reports_natural_exit() {
        let child = spawn_helper("rpc::tests::helper_quiet_exit");
        // Keep the kill sender alive: the waiter must only react to an
        // actual kill request.
        let (_kill_tx, kill_rx) = mpsc::channel(1);
        let (events_tx, mut events_rx) = mpsc::channel(4);
        let waiter = tokio::spawn(child_waiter(child, kill_rx, events_tx));

        match next_event(&mut events_rx).await {
            RpcEvent::Exited(Some(status)) => {
                assert!(status.success(), "the helper should exit 0");
            }
            other => panic!("expected a natural exit event, got: {other:?}"),
        }
        waiter.await.expect("child waiter task finished");
    }

    #[tokio::test]
    async fn child_waiter_kills_and_reaps_a_hanging_child() {
        let child = spawn_helper("rpc::tests::helper_hangs");
        let (kill_tx, kill_rx) = mpsc::channel(1);
        let (events_tx, mut events_rx) = mpsc::channel(4);
        let waiter = tokio::spawn(child_waiter(child, kill_rx, events_tx));

        // Give the helper a moment to start up, then kill it through the
        // control channel; the waiter must terminate and reap the process.
        tokio::time::sleep(Duration::from_millis(150)).await;
        kill_tx.send(()).await.expect("kill channel open");

        match next_event(&mut events_rx).await {
            RpcEvent::Exited(Some(status)) => {
                assert!(!status.success(), "a killed child must not report success");
            }
            other => panic!("expected a killed exit event, got: {other:?}"),
        }
        waiter.await.expect("child waiter task finished");
    }

    async fn read_lines<R>(reader: &mut R, out: &mut Vec<Vec<u8>>, count: usize)
    where
        R: AsyncRead + Unpin,
    {
        let mut buf = [0u8; 4096];
        let mut line = Vec::new();
        while out.len() < count {
            let read = reader.read(&mut buf).await.unwrap();
            assert_ne!(read, 0, "unexpected EOF while reading lines");
            for &byte in &buf[..read] {
                if byte == b'\n' {
                    out.push(std::mem::take(&mut line));
                } else {
                    line.push(byte);
                }
            }
        }
    }

    #[test]
    fn spawn_rejects_missing_config_before_spawning() {
        let error = RpcProcess::spawn(
            Path::new("minicore-agent"),
            Path::new("/nonexistent/agent.toml"),
        )
        .unwrap_err();
        assert!(matches!(error, RpcError::ConfigMissing(_)));
        assert!(error.to_string().contains("/nonexistent/agent.toml"));
    }

    #[tokio::test]
    async fn spawn_reports_an_unusable_agent_binary() {
        let config = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let error =
            RpcProcess::spawn(Path::new("/nonexistent/minicore-agent-bin"), &config).unwrap_err();
        assert!(matches!(error, RpcError::Spawn(_)));
        assert!(error.to_string().contains("failed to spawn"));
    }
    // ---- fake-agent harness ------------------------------------------
    //
    // The full OS pipe/lifecycle pipeline is exercised through the PRODUCTION
    // `RpcProcess::spawn` against a scripted agent built as a non-installable
    // `harness = false` test target (`tests/agent_process.rs`, so it never ships
    // as a `[[bin]]`). Production exclusively assembles `agent --config
    // <path> --stdio`; the config file content selects the fake's behavior.
    // The same piped stdin/stdout/stderr/waiter pipeline is used as with a
    // real agent; libtest output is never on the child's stdout because the
    // fake is a plain `main`, not a `#[test]`.

    use std::sync::atomic::{AtomicU64, Ordering};

    /// RAII scratch config file for the fake agent; removed on drop,
    /// including panic unwinds. Kept beside its process for the test's
    /// lifetime so the child can read the mode file at startup.
    struct TempConfigFile {
        path: PathBuf,
    }

    impl TempConfigFile {
        fn new(mode: &str) -> Self {
            static SEQ: AtomicU64 = AtomicU64::new(0);
            let dir = std::env::temp_dir().join(format!("mct-rpc-{}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("create temp dir");
            let path = dir.join(format!(
                "agent-{}.toml",
                SEQ.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::write(&path, mode).expect("write fake config");
            Self { path }
        }
    }

    impl Drop for TempConfigFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
            let _ = std::fs::remove_dir(self.path.parent().expect("temp dir path"));
        }
    }

    /// Locates the harness=false `agent_process` test target binary. Cargo may
    /// expose it through `CARGO_BIN_EXE_agent_process` or place a hashed
    /// executable beside this test binary; both paths are supported on Unix
    /// and Windows.
    fn agent_process_bin() -> PathBuf {
        if let Some(path) = std::env::var_os("CARGO_BIN_EXE_agent_process").map(PathBuf::from) {
            if is_regular_executable(&path) {
                return path;
            }
        }
        // `current_exe` may be relative; canonicalize so the deps directory
        // is always an absolute path.
        let executable = std::env::current_exe()
            .expect("test executable")
            .canonicalize()
            .unwrap_or_else(|_| std::env::current_exe().expect("test executable"));
        let dir = executable.parent().expect("deps directory").to_path_buf();
        let candidates = std::fs::read_dir(&dir)
            .expect("read the deps directory")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| agent_process_name_candidate(name, cfg!(windows)))
                    && is_regular_executable(path)
            })
            .collect::<Vec<_>>();
        candidates
            .into_iter()
            .max_by_key(|path| {
                std::fs::metadata(path)
                    .and_then(|meta| meta.modified())
                    .ok()
            })
            .expect("the agent_process test target must be built (cargo test --all-targets)")
    }

    /// Pure name filter used by the filesystem scan. Dep-info, pdb, rmeta,
    /// and other companion files are rejected by their actual executable
    /// metadata in `agent_process_bin`; this function only recognizes Cargo's
    /// base/hashed target name with the Windows `.exe` suffix allowed.
    fn agent_process_name_candidate(name: &str, windows: bool) -> bool {
        let name = if windows {
            let Some((stem, extension)) = name.rsplit_once('.') else {
                return false;
            };
            if !extension.eq_ignore_ascii_case("exe") {
                return false;
            }
            stem
        } else {
            if name.contains('.') {
                return false;
            }
            name
        };
        name == "agent_process"
            || name.strip_prefix("agent_process-").is_some_and(|suffix| {
                !suffix.is_empty()
                    && suffix
                        .chars()
                        .all(|character| character.is_ascii_hexdigit())
            })
    }

    fn is_regular_executable(path: &Path) -> bool {
        let Ok(metadata) = std::fs::metadata(path) else {
            return false;
        };
        if !metadata.is_file() {
            return false;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            metadata.permissions().mode() & 0o111 != 0
        }
        #[cfg(windows)]
        {
            path.extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
        }
        #[cfg(not(any(unix, windows)))]
        {
            true
        }
    }

    #[test]
    fn agent_process_name_selection_accepts_windows_executables_only() {
        assert!(agent_process_name_candidate("agent_process-abc", false));
        assert!(agent_process_name_candidate("agent_process-abc.exe", true));
        assert!(agent_process_name_candidate("agent_process.exe", true));
        assert!(!agent_process_name_candidate(
            "agent_process-abc.exe.d",
            true
        ));
        assert!(!agent_process_name_candidate("agent_process-abc.pdb", true));
        assert!(!agent_process_name_candidate(
            "agent_process-abc.rmeta",
            true
        ));
        assert!(!agent_process_name_candidate(
            "agent_process-abc.d.exe",
            true
        ));
        assert!(!agent_process_name_candidate(
            "agent_process-not-a-hash.exe",
            true
        ));
        assert!(!agent_process_name_candidate("agent_process-abc", true));
        assert!(!agent_process_name_candidate(
            "agent_process-abc.exe",
            false
        ));
        assert!(!agent_process_name_candidate(
            "not_agent_process-abc.exe",
            true
        ));
    }

    #[test]
    fn agent_process_bare_run_is_quiet_and_successful() {
        let output = std::process::Command::new(agent_process_bin())
            .output()
            .expect("spawn the bare harness target");
        assert!(output.status.success());
        assert!(output.stdout.is_empty(), "bare harness wrote RPC stdout");
        assert!(output.stderr.is_empty(), "bare harness wrote stderr");
    }

    /// A live fake agent plus its scratch config, kept alive for the test's
    /// lifetime; derefs to the `RpcProcess`.
    struct FakeAgent {
        process: RpcProcess,
        _config: TempConfigFile,
    }

    impl std::ops::Deref for FakeAgent {
        type Target = RpcProcess;
        fn deref(&self) -> &RpcProcess {
            &self.process
        }
    }

    impl std::ops::DerefMut for FakeAgent {
        fn deref_mut(&mut self) -> &mut RpcProcess {
            &mut self.process
        }
    }

    fn spawn_fake(mode: &str) -> FakeAgent {
        let config = TempConfigFile::new(mode);
        let binary = agent_process_bin();
        let process = RpcProcess::spawn(&binary, &config.path)
            .unwrap_or_else(|error| panic!("spawn agent_process ({mode}): {error}"));
        FakeAgent {
            process,
            _config: config,
        }
    }

    /// The fake-agent tests read events from a whole `RpcProcess` (unlike
    /// the duplex unit helpers, which read a bare channel).
    async fn next_process_event(process: &mut RpcProcess) -> RpcEvent {
        timeout(TEST_TIMEOUT, process.recv())
            .await
            .expect("timed out waiting for an RPC event")
            .expect("RPC event channel closed")
    }

    #[tokio::test]
    async fn fake_ping_then_clean_shutdown_reaps_the_child() {
        let mut process = spawn_fake("serve");
        process
            .send(OutgoingRequest::ping(RequestId(1)))
            .await
            .expect("ping sends");
        match next_process_event(&mut process).await {
            RpcEvent::Frame(IncomingFrame::Response(response)) => {
                assert_eq!(response.id, RequestId(1));
            }
            other => panic!("expected a ping response, got {other:?}"),
        }

        process
            .send(OutgoingRequest::shutdown(RequestId(2)))
            .await
            .expect("shutdown sends");
        loop {
            match next_process_event(&mut process).await {
                RpcEvent::Frame(IncomingFrame::Response(response)) => {
                    assert_eq!(response.id, RequestId(2));
                    assert!(response.parse_shutdown().expect("ok").ok);
                }
                RpcEvent::ConnectionClosed => {}
                RpcEvent::Exited(Some(status)) => {
                    assert!(status.success(), "a clean shutdown exits 0");
                    break;
                }
                other => panic!("unexpected event: {other:?}"),
            }
        }
        assert!(process.child_reaped(), "the exit was consumed and reaped");
        let start = Instant::now();
        process.terminate().await;
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "terminate is a fast no-op after a clean exit"
        );
    }

    #[tokio::test]
    async fn fake_responses_are_out_of_order_but_ids_never_reorder() {
        let mut process = spawn_fake("out_of_order");
        process
            .send(OutgoingRequest::ping(RequestId(7)))
            .await
            .expect("ping sends");
        process
            .send(OutgoingRequest::list_models(RequestId(8)))
            .await
            .expect("model.list sends");

        let first = match next_process_event(&mut process).await {
            RpcEvent::Frame(IncomingFrame::Response(response)) => response.id,
            other => panic!("expected a response, got {other:?}"),
        };
        let second = match next_process_event(&mut process).await {
            RpcEvent::Frame(IncomingFrame::Response(response)) => response.id,
            other => panic!("expected a response, got {other:?}"),
        };
        assert_eq!(
            first,
            RequestId(8),
            "model.list answered first in reverse mode"
        );
        assert_eq!(second, RequestId(7), "ping answered second");
    }

    #[tokio::test]
    async fn fake_events_arrive_before_the_send_response() {
        let mut process = spawn_fake("events_first");
        process
            .send(OutgoingRequest::send_turn(RequestId(3), "ses_1", "hello"))
            .await
            .expect("turn.send sends");
        for _ in 0..4 {
            let event = next_process_event(&mut process).await;
            assert!(
                matches!(
                    event,
                    RpcEvent::Frame(IncomingFrame::Notification(RpcNotification::AgentEvent(_)))
                ),
                "expected an agent event, got {event:?}"
            );
        }
        match next_process_event(&mut process).await {
            RpcEvent::Frame(IncomingFrame::Response(response)) => {
                assert_eq!(response.id, RequestId(3));
            }
            other => panic!("expected the send response, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn fake_serve_mode_speaks_the_full_contract() {
        let mut process = spawn_fake("serve");
        for (id, method) in [
            (RequestId(1), "agent.ping"),
            (RequestId(2), "model.list"),
            (RequestId(3), "profile.list"),
            (RequestId(4), "session.list"),
        ] {
            let request = OutgoingRequest::new(id, method, json!({}));
            process.send(request).await.expect("request sends");
            match next_process_event(&mut process).await {
                RpcEvent::Frame(IncomingFrame::Response(response)) => {
                    assert_eq!(response.id, id, "{method} answered with its id");
                }
                other => panic!("{method} expected a response, got {other:?}"),
            }
        }

        process
            .send(OutgoingRequest::session_create(
                RequestId(5),
                "/ws/fake",
                None,
                None,
                None,
                None,
            ))
            .await
            .expect("session.create sends");
        match next_process_event(&mut process).await {
            RpcEvent::Frame(IncomingFrame::Response(response)) => {
                assert_eq!(response.id, RequestId(5));
            }
            other => panic!("expected session.create, got {other:?}"),
        }

        process
            .send(OutgoingRequest::send_turn(RequestId(6), "ses_fake_1", "hi"))
            .await
            .expect("turn.send sends");
        match next_process_event(&mut process).await {
            RpcEvent::Frame(IncomingFrame::Response(response)) => {
                assert_eq!(response.id, RequestId(6));
            }
            other => panic!("expected turn.send, got {other:?}"),
        }
        for _ in 0..4 {
            let event = next_process_event(&mut process).await;
            assert!(matches!(
                event,
                RpcEvent::Frame(IncomingFrame::Notification(RpcNotification::AgentEvent(_)))
            ));
        }

        process
            .send(OutgoingRequest::shutdown(RequestId(9)))
            .await
            .expect("shutdown sends");
        loop {
            match next_process_event(&mut process).await {
                RpcEvent::Frame(IncomingFrame::Response(response)) => {
                    assert_eq!(response.id, RequestId(9));
                }
                RpcEvent::ConnectionClosed => {}
                RpcEvent::Exited(Some(status)) => {
                    assert!(status.success());
                    break;
                }
                other => panic!("unexpected trailing event: {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn fake_unknown_method_reports_a_top_level_jsonrpc_error() {
        let mut process = spawn_fake("serve");
        process
            .send(OutgoingRequest::new(
                RequestId(9),
                "no.such.method",
                json!({}),
            ))
            .await
            .expect("unknown method sends");
        match next_process_event(&mut process).await {
            RpcEvent::Frame(IncomingFrame::Response(response)) => {
                assert_eq!(response.id, RequestId(9));
                let error = response
                    .error
                    .as_ref()
                    .expect("the error sits at the top level, never inside result");
                assert_eq!(error.code, -32601);
                assert!(response.result.is_none());
            }
            other => panic!("expected a top-level error response, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn fake_crashing_agent_reports_termination_and_a_failed_exit() {
        let mut process = spawn_fake("crash");
        process
            .send(OutgoingRequest::ping(RequestId(1)))
            .await
            .expect("ping sends");
        let mut saw_closed = false;
        let mut saw_exit = false;
        while !(saw_closed && saw_exit) {
            match next_process_event(&mut process).await {
                RpcEvent::ConnectionClosed => saw_closed = true,
                RpcEvent::Exited(Some(status)) => {
                    assert!(!status.success(), "the crashed child must not succeed");
                    saw_exit = true;
                }
                other => panic!("unexpected event after crash: {other:?}"),
            }
        }
        // stdout EOF and the waiter exit are emitted by independent tasks;
        // their relative order is intentionally not part of the contract.
        assert!(saw_closed && saw_exit);
    }

    #[tokio::test]
    async fn fake_terminate_kills_and_reaps_a_hanging_agent() {
        let mut process = spawn_fake("hang");
        process
            .send(OutgoingRequest::ping(RequestId(1)))
            .await
            .expect("ping sends");
        match next_process_event(&mut process).await {
            RpcEvent::Frame(IncomingFrame::Response(response)) => {
                assert_eq!(response.id, RequestId(1));
            }
            other => panic!("expected a ping response, got {other:?}"),
        }
        let start = Instant::now();
        process.terminate().await;
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "terminate must force-kill a child that ignores shutdown"
        );
        assert!(process.child_reaped(), "the killed child must be reaped");
        let start = Instant::now();
        process.terminate().await;
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn fake_hanging_agent_stderr_survives_until_kill_and_reap() {
        let mut process = spawn_fake("hang_stderr");
        process
            .send(OutgoingRequest::ping(RequestId(1)))
            .await
            .expect("ping sends");
        let mut saw_log = false;
        let mut saw_ping = false;
        while !(saw_log && saw_ping) {
            match next_process_event(&mut process).await {
                RpcEvent::AgentLogLine(line) => {
                    assert_eq!(line, "fake agent stderr before forced termination");
                    saw_log = true;
                }
                RpcEvent::Frame(IncomingFrame::Response(response)) => {
                    assert_eq!(response.id, RequestId(1));
                    saw_ping = true;
                }
                other => panic!("unexpected event before forced termination: {other:?}"),
            }
        }
        process.kill_child();
        process.terminate().await;
        assert!(process.child_reaped(), "the forced child must be reaped");
    }

    #[tokio::test]
    async fn fake_oversized_request_lines_are_rejected_without_writing() {
        let process = spawn_fake("serve");
        let params = json!({ "x": "a".repeat(1024 * 1024) });
        let error = process
            .send(OutgoingRequest::new(RequestId(1), "agent.ping", params))
            .await
            .expect_err("oversized lines never reach the agent");
        assert!(matches!(error, RpcError::RequestTooLarge { .. }));
    }
}
