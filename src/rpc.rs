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
pub const MAX_RPC_FRAME_BYTES: usize = 8 * 1024 * 1024;
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

/// Owns the agent child and its stdio tasks.
///
/// The child is owned by a dedicated waiter task. `send` enforces the
/// outbound request-line bound and hands the serialized line to the single
/// writer task; `recv` yields events from the reader tasks. Dropping the
/// process closes the channels and aborts the tasks, dropping the child
/// (`kill_on_drop`), so no agent process is left behind. Must be created
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
}

impl RpcProcess {
    /// Spawns `agent_bin --config <agent_config> --stdio` and starts the four
    /// background tasks. All fallible steps run before any task is started,
    /// so a spawn failure leaves nothing behind to clean up.
    pub fn spawn(agent_bin: &Path, agent_config: &Path) -> Result<Self, RpcError> {
        if !agent_config.is_file() {
            return Err(RpcError::ConfigMissing(agent_config.to_path_buf()));
        }
        let mut command = Command::new(agent_bin);
        command
            .arg("--config")
            .arg(agent_config)
            .arg("--stdio")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(RpcError::Spawn)?;
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
        self.events.recv().await
    }

    /// Requests the waiter task to kill the agent child; the
    /// `RpcEvent::Exited` event follows. This is the control path for the
    /// future shutdown sequence: send `agent.shutdown`, wait for its
    /// response, then wait a bounded time for the child and call this when
    /// the deadline passes.
    pub fn kill_child(&self) {
        let _ = self.kill.try_send(());
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
        biased;
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

        let event = r#"{"jsonrpc":"2.0","method":"agent.event","params":{"type":"output_delta","data":{"turn":{"session_id":"ses_1","instance_id":"ins_1","turn_id":"trn_1"},"channel":"text","delta":"hi","meta":{"session_id":"ses_1","instance_id":"ins_1","dropped_before":0}}}}"#;
        let response =
            |id: u64| format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{{"version":"0.2.0"}}}}"#);
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
}
