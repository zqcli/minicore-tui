//! Spec 61 Real-Agent E2E test suite.
//!
//! All tests are self-contained and run against a loopback mock HTTP server
//! simulating the OpenAI Responses API. They use isolated temporary directories
//! and require no external network, real API credentials, or parent-directory traversal.
//!
//! To run these tests:
//!   MINICORE_AGENT_BIN=/path/to/minicore-agent cargo test --test agent_e2e -- --ignored

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use crossterm::event::{Event as CrosstermEvent, KeyCode, KeyEvent, KeyModifiers};
use minicore_tui::app::{App, ConnectionState, RequestKind};
use minicore_tui::command::AppCommand;
use minicore_tui::event::{AppEvent, RpcEvent};
use minicore_tui::protocol::{
    AgentEventWire, CancelReasonWire, HistoryItemWire, IncomingFrame, LoopOutcomeWire,
    OutgoingRequest, Reasoning, RequestId, RpcNotification, ToolCallViewWire, TurnPersistenceWire,
    TurnRef, TurnResultViewWire, UserMessageKindWire,
};
use minicore_tui::rpc::RpcProcess;
use minicore_tui::state::session::ConfigUpdateState;
use minicore_tui::state::transcript::AssistantPart;
use minicore_tui::state::turn::{LivePart, PendingSteerState};
use minicore_tui::state::{FoldOverride, ToolKey};
use minicore_tui::theme::ThemeKind;
use serde_json::json;

// Bounded harness deadlines. Under a default-parallel run (ten real Agent
// processes spawned at once) spawn contention can stretch any single wait
// well past a per-request window, so the official run is serial and these
// remain generous upper bounds rather than product timing assumptions.
const TIMEOUT: Duration = Duration::from_secs(60);
const MOCK_API_KEY_ENV: &str = "MINICORE_E2E_MOCK_API_KEY";
const MOCK_API_KEY_VAL: &str = "mock-key-spec61-round7-e2e";
const MAX_HTTP_HEADER_SIZE: usize = 64 * 1024;
const MAX_HTTP_BODY_SIZE: usize = 1024 * 1024;

fn require_agent_bin() -> String {
    std::env::var("MINICORE_AGENT_BIN")
        .expect("MINICORE_AGENT_BIN must be set to run agent_e2e tests; cannot silently pass")
}

// ============================================================================
// Loopback Mock Server for OpenAI Responses Provider
// ============================================================================

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct RecordedRequest {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: String,
    json: serde_json::Value,
    model: Option<String>,
}

struct MockResponse {
    body: String,
    gate: Option<Arc<AtomicBool>>,
    expected_model: Option<String>,
    /// When set, write the body one SSE event at a time with a short delay so
    /// long-lived (live) frames are observable instead of arriving at once.
    chunked_delay_ms: Option<u64>,
}

struct MockHttpServer {
    port: u16,
    running: Arc<AtomicBool>,
    server_thread: Option<JoinHandle<()>>,
    responses: Arc<Mutex<VecDeque<MockResponse>>>,
    recorded_requests: Arc<Mutex<Vec<RecordedRequest>>>,
}

impl MockHttpServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback mock server");
        let port = listener.local_addr().expect("local addr").port();
        listener
            .set_nonblocking(true)
            .expect("set nonblocking listener");

        let running = Arc::new(AtomicBool::new(true));
        let responses = Arc::new(Mutex::new(VecDeque::new()));
        let recorded_requests = Arc::new(Mutex::new(Vec::new()));

        let running_clone = running.clone();
        let responses_clone = responses.clone();
        let recorded_clone = recorded_requests.clone();

        let server_thread = thread::spawn(move || {
            while running_clone.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        handle_connection(
                            &mut stream,
                            &responses_clone,
                            &recorded_clone,
                            &running_clone,
                        );
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            port,
            running,
            server_thread: Some(server_thread),
            responses,
            recorded_requests,
        }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/v1", self.port)
    }

    fn enqueue_sse(&self, sse_body: String) {
        self.responses.lock().unwrap().push_back(MockResponse {
            body: sse_body,
            gate: None,
            expected_model: None,
            chunked_delay_ms: None,
        });
    }

    fn enqueue_chunked_sse(&self, sse_body: String) {
        self.responses.lock().unwrap().push_back(MockResponse {
            body: sse_body,
            gate: None,
            expected_model: None,
            chunked_delay_ms: Some(25),
        });
    }

    fn enqueue_sse_with_model(&self, sse_body: String, expected_model: &str) {
        self.responses.lock().unwrap().push_back(MockResponse {
            body: sse_body,
            gate: None,
            expected_model: Some(expected_model.to_string()),
            chunked_delay_ms: None,
        });
    }

    fn enqueue_gated(&self, sse_body: String, gate: Arc<AtomicBool>, expected_model: Option<&str>) {
        self.responses.lock().unwrap().push_back(MockResponse {
            body: sse_body,
            gate: Some(gate),
            expected_model: expected_model.map(|s| s.to_string()),
            chunked_delay_ms: None,
        });
    }

    fn recorded_requests(&self) -> Vec<RecordedRequest> {
        self.recorded_requests.lock().unwrap().clone()
    }
}

impl Drop for MockHttpServer {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        // Poke listener to unblock accept if pending
        let _ = TcpStream::connect(format!("127.0.0.1:{}", self.port));
        if let Some(thread) = self.server_thread.take() {
            let _ = thread.join();
        }
    }
}

type RawHttpRequest = (String, String, Vec<(String, String)>, Vec<u8>);

fn read_http_request(stream: &mut TcpStream) -> Result<RawHttpRequest, String> {
    let mut buf = [0u8; 4096];
    let mut total_read = Vec::new();

    let header_end = loop {
        let n = stream.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("early EOF before headers ended".into());
        }
        total_read.extend_from_slice(&buf[..n]);
        if total_read.len() > MAX_HTTP_HEADER_SIZE {
            return Err("HTTP header exceeds maximum allowed size".into());
        }
        if let Some(pos) = total_read.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
    };

    let head_str = String::from_utf8_lossy(&total_read[..header_end]);
    let mut lines = head_str.lines();
    let request_line = lines.next().ok_or("missing request line")?;
    let mut req_parts = request_line.split_whitespace();
    let method = req_parts.next().unwrap_or("").to_string();
    let path = req_parts.next().unwrap_or("").to_string();

    let mut headers = Vec::new();
    let mut content_length: usize = 0;
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            let key = k.trim().to_string();
            let val = v.trim().to_string();
            if key.eq_ignore_ascii_case("content-length") {
                content_length = val.parse().unwrap_or(0);
            }
            headers.push((key, val));
        }
    }

    if content_length > MAX_HTTP_BODY_SIZE {
        return Err("HTTP content-length exceeds maximum allowed size".into());
    }

    let body_start = header_end + 4;
    let mut body = total_read[body_start..].to_vec();
    if body.len() < content_length {
        let remaining = content_length - body.len();
        let mut rem_buf = vec![0u8; remaining];
        stream.read_exact(&mut rem_buf).map_err(|e| e.to_string())?;
        body.extend_from_slice(&rem_buf);
    }

    Ok((method, path, headers, body))
}

fn handle_connection(
    stream: &mut TcpStream,
    responses: &Arc<Mutex<VecDeque<MockResponse>>>,
    recorded_requests: &Arc<Mutex<Vec<RecordedRequest>>>,
    running: &Arc<AtomicBool>,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    // The listener is non-blocking so the accept loop can poll; on macOS an
    // accepted socket inherits O_NONBLOCK, and a non-blocking read returns
    // WouldBlock (os error 35) before the agent's request bytes arrive,
    // silently dropping the request (agent observes `request_outcome_unknown`).
    // Revert the accepted stream to blocking so reads wait for the payload.
    let _ = stream.set_nonblocking(false);

    let (method, path, headers, body_bytes) = match read_http_request(stream) {
        Ok(res) => res,
        Err(_) => return,
    };

    let body_str = String::from_utf8_lossy(&body_bytes).into_owned();
    let body_json: serde_json::Value =
        serde_json::from_str(&body_str).unwrap_or(serde_json::Value::Null);
    let model = body_json
        .get("model")
        .and_then(|m| m.as_str())
        .map(|s| s.to_string());

    let recorded = RecordedRequest {
        method,
        path,
        headers,
        body: body_str,
        json: body_json,
        model,
    };

    recorded_requests.lock().unwrap().push(recorded.clone());

    let next_resp = {
        let mut queue = responses.lock().unwrap();
        queue.pop_front()
    };

    let resp = match next_resp {
        Some(r) => r,
        None => {
            // Strict mock server: reject unexpected extra requests
            let err_body = "HTTP/1.1 500 Internal Server Error\r\nContent-Type: text/plain\r\nConnection: close\r\nContent-Length: 31\r\n\r\nUnexpected extra HTTP request.";
            let _ = stream.write_all(err_body.as_bytes());
            let _ = stream.flush();
            return;
        }
    };

    if let Some(expected) = &resp.expected_model {
        if recorded.model.as_deref() != Some(expected.as_str()) {
            let err_body = format!(
                "HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\nExpected model {} but got {:?}",
                expected, recorded.model
            );
            let _ = stream.write_all(err_body.as_bytes());
            let _ = stream.flush();
            return;
        }
    }

    if let Some(gate) = &resp.gate {
        let wait_start = Instant::now();
        let gate_timeout = Duration::from_secs(10);
        while !gate.load(Ordering::Relaxed) && running.load(Ordering::Relaxed) {
            if wait_start.elapsed() > gate_timeout {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    let http_response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
        resp.body.len(),
        resp.body
    );

    match resp.chunked_delay_ms {
        Some(delay) => {
            // Headers first (without the body), then stream the body one SSE
            // event at a time with a short delay so live frames are actually
            // observable instead of all arriving in a single write.
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
                resp.body.len()
            );
            stream.write_all(headers.as_bytes()).ok();
            stream.flush().ok();
            let mut written = 0usize;
            while written < resp.body.len() {
                let next_event = resp.body[written..]
                    .find("\n\n")
                    .map(|pos| pos + 2)
                    .unwrap_or(resp.body.len() - written);
                let chunk = &resp.body[written..written + next_event];
                stream.write_all(chunk.as_bytes()).ok();
                stream.flush().ok();
                written += next_event;
                thread::sleep(Duration::from_millis(delay));
            }
        }
        None => {
            let _ = stream.write_all(http_response.as_bytes());
            let _ = stream.flush();
        }
    }
}

fn sse_text_response(text: &str) -> String {
    format!(
        "data: {}\n\ndata: {}\n\n",
        json!({"type": "response.output_text.delta", "delta": text}),
        json!({"type": "response.completed", "response": {
            "status": "completed",
            "usage": {
                "input_tokens": 10,
                "output_tokens": 10,
                "total_tokens": 20,
                "input_tokens_details": {"cached_tokens": 0, "cache_write_tokens": 0},
                "output_tokens_details": {"reasoning_tokens": 0}
            }
        }})
    )
}

fn sse_text_response_with_usage(text: &str, input: u64, output: u64, reasoning: u64) -> String {
    format!(
        "data: {}\n\ndata: {}\n\n",
        json!({"type": "response.output_text.delta", "delta": text}),
        json!({"type": "response.completed", "response": {
            "status": "completed",
            "usage": {
                "input_tokens": input,
                "output_tokens": output,
                "total_tokens": input + output,
                "input_tokens_details": {"cached_tokens": 0, "cache_write_tokens": 0},
                "output_tokens_details": {"reasoning_tokens": reasoning}
            }
        }})
    )
}

fn sse_tool_call_response(call_id: &str, tool_name: &str, arguments: &str) -> String {
    format!(
        "data: {}\n\ndata: {}\n\n",
        json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "item": {
                "type": "function_call",
                "call_id": call_id,
                "name": tool_name,
                "arguments": arguments
            }
        }),
        json!({"type": "response.completed", "response": {
            "status": "completed",
            "usage": {
                "input_tokens": 10,
                "output_tokens": 10,
                "total_tokens": 20,
                "input_tokens_details": {"cached_tokens": 0, "cache_write_tokens": 0},
                "output_tokens_details": {"reasoning_tokens": 0}
            }
        }})
    )
}

fn sse_two_tool_calls_response(call0: &str, call1: &str) -> String {
    format!(
        "data: {}\n\ndata: {}\n\ndata: {}\n\n",
        json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "item": {
                "type": "function_call",
                "call_id": call0,
                "name": "read",
                "arguments": r#"{"path": "data.txt"}"#
            }
        }),
        json!({
            "type": "response.output_item.done",
            "output_index": 1,
            "item": {
                "type": "function_call",
                "call_id": call1,
                "name": "read",
                "arguments": r#"{"path": "data.txt"}"#
            }
        }),
        json!({"type": "response.completed", "response": {
            "status": "completed",
            "usage": {
                "input_tokens": 10,
                "output_tokens": 10,
                "total_tokens": 20,
                "input_tokens_details": {"cached_tokens": 0, "cache_write_tokens": 0},
                "output_tokens_details": {"reasoning_tokens": 0}
            }
        }})
    )
}

/// SSE that streams three distinct reasoning summary items (`summary_index`
/// 0/1/2) with fragments inside item 0 and no newline at the item boundaries,
/// exactly as the provider emits them. 0.2.4 boundary repro: the summary item
/// boundary is only signaled by `summary_index`; the text carries no separator.
fn sse_multi_summary_reasoning_response() -> String {
    let events = [
        json!({"type":"response.output_item.added","output_index":0,"item":{"type":"reasoning","id":"rs_boundary","summary":[]}}),
        json!({"type":"response.reasoning_summary_text.delta","item_id":"rs_boundary","output_index":0,"summary_index":0,"delta":"Plan"}),
        json!({"type":"response.reasoning_summary_text.delta","item_id":"rs_boundary","output_index":0,"summary_index":0,"delta":"ning ... caveats"}),
        json!({"type":"response.reasoning_summary_text.delta","item_id":"rs_boundary","output_index":0,"summary_index":1,"delta":"Detailing ... timeline"}),
        json!({"type":"response.reasoning_summary_text.delta","item_id":"rs_boundary","output_index":0,"summary_index":2,"delta":"Analyzing ..."}),
        json!({"type":"response.output_item.done","output_index":0,"item":{
            "type":"reasoning",
            "id":"rs_boundary",
            "status":"completed",
            "summary":[{"type":"summary_text","text":"Planning ... caveatsDetailing ... timelineAnalyzing ..."}],
            "provider":{"name":"loopback-openai","trace_id":"reasoning-boundary"}
        }}),
        json!({"type":"response.completed","response":{"status":"completed","usage":{
            "input_tokens":10,"output_tokens":10,"total_tokens":20,
            "input_tokens_details":{"cached_tokens":0,"cache_write_tokens":0},
            "output_tokens_details":{"reasoning_tokens":10}
        }}}),
    ];
    let mut body = String::new();
    for event in events {
        body.push_str(&format!("data: {}\n\n", event));
    }
    body
}

// ============================================================================
// Test Environment & Process Management
// ============================================================================

struct E2eEnvironment {
    temp_dir: PathBuf,
    config_path: PathBuf,
    workspace_path: PathBuf,
    _server: MockHttpServer,
}

impl E2eEnvironment {
    fn setup() -> (Self, String) {
        let server = MockHttpServer::start();
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let temp_dir =
            std::env::temp_dir().join(format!("minicore_tui_e2e_{}_{}", std::process::id(), nanos));
        let workspace_path = temp_dir.join("workspace");
        let data_dir = temp_dir.join("agent_data");
        std::fs::create_dir_all(&workspace_path).expect("create workspace");
        std::fs::create_dir_all(&data_dir).expect("create agent_data");

        let config_path = temp_dir.join("agent.toml");
        let server_url = server.url();
        let config_toml = format!(
            r#"data_dir = {:?}
event_capacity = 64
default_profile = "coding"

[profiles.coding]
model = "deep"
reasoning = "high"
system_prompt = "You are a test assistant."
tools = ["read", "write"]
max_tool_rounds = 4
approval = "auto"

[profiles.fast]
model = "fast"
reasoning = "low"
system_prompt = "You are a fast test assistant."
tools = ["read", "write"]
max_tool_rounds = 4
approval = "auto"

[models.deep]
provider = "open_ai_responses"
model = "deep-model"
base_url = "{server_url}"
api_key_env = "{MOCK_API_KEY_ENV}"
physical_context_window = 32000
output_budget_tokens = 2048
safety_margin_tokens = 1000
supported_reasoning = ["auto", "low", "medium", "high"]
supports_tools = true
request_timeout_seconds = 30

[models.fast]
provider = "open_ai_responses"
model = "fast-model"
base_url = "{server_url}"
api_key_env = "{MOCK_API_KEY_ENV}"
physical_context_window = 16000
output_budget_tokens = 1024
safety_margin_tokens = 1000
supported_reasoning = ["auto", "low", "high"]
supports_tools = true
request_timeout_seconds = 30

[profiles.luna]
model = "luna"
reasoning = "high"
system_prompt = "You are a deep reasoning test assistant."
tools = ["read", "write"]
max_tool_rounds = 4
approval = "auto"

[models.luna]
provider = "open_ai_responses"
model = "luna-model"
base_url = "{server_url}"
api_key_env = "{MOCK_API_KEY_ENV}"
physical_context_window = 372000
output_budget_tokens = 128000
safety_margin_tokens = 4000
supported_reasoning = ["auto", "disabled", "low", "medium", "high", "xhigh", "max", "ultra"]
supports_tools = true
request_timeout_seconds = 30
"#,
            data_dir
        );
        std::fs::write(&config_path, config_toml).expect("write agent.toml");

        let env = Self {
            temp_dir,
            config_path,
            workspace_path,
            _server: server,
        };
        (env, server_url)
    }

    fn spawn_agent(&self, agent_bin: &str) -> RpcProcess {
        RpcProcess::spawn_with_env(
            Path::new(agent_bin),
            &self.config_path,
            &[(MOCK_API_KEY_ENV, MOCK_API_KEY_VAL)],
        )
        .expect("spawn Agent with mock env")
    }
}

impl Drop for E2eEnvironment {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.temp_dir);
    }
}

// ============================================================================
// Driver & Dispatch Helpers
// ============================================================================

async fn pump_step(process: &mut RpcProcess, app: &mut App) -> Result<(), String> {
    let event = tokio::time::timeout(Duration::from_secs(10), process.recv()).await;
    let event = match event {
        // A silent window is scheduling contention, not a product stall: the
        // caller's overall deadline (bounded) decides whether the pump timed
        // out. Aborting on a single 10s window turned the parallel spawn of
        // ten real Agent processes into flaky "recv timed out" failures.
        Ok(event) => event.ok_or("agent process stream ended")?,
        Err(_) => return Ok(()),
    };

    let commands = app.update(AppEvent::Rpc(event));
    for command in commands {
        match command {
            AppCommand::Rpc(req) => {
                process.send(req).await.map_err(|e| e.to_string())?;
            }
            AppCommand::KillChild => process.kill_child(),
            AppCommand::CopySelection(_) => {}
            AppCommand::Exit => return Ok(()),
        }
    }
    Ok(())
}

async fn wait_for_request0_and_wait_turn(
    env: &E2eEnvironment,
    process: &mut RpcProcess,
    app: &mut App,
    session_id: &str,
) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if Instant::now() >= deadline {
            return Err("Timed out waiting for Request 0 and wait_turn registration".into());
        }

        let has_req = !env._server.recorded_requests().is_empty();
        let has_wait = app
            .sessions
            .known
            .get(session_id)
            .is_some_and(|v| v.live.as_ref().is_some_and(|l| l.reference.is_some()))
            && app
                .pending_requests
                .values()
                .any(|k| matches!(k, RequestKind::WaitTurn(_)));

        if has_req && has_wait {
            return Ok(());
        }

        match tokio::time::timeout(Duration::from_millis(20), process.recv()).await {
            Ok(Some(event)) => {
                let commands = app.update(AppEvent::Rpc(event));
                for command in commands {
                    match command {
                        AppCommand::Rpc(req) => {
                            process.send(req).await.map_err(|e| e.to_string())?;
                        }
                        AppCommand::KillChild => process.kill_child(),
                        AppCommand::CopySelection(_) => {}
                        AppCommand::Exit => return Ok(()),
                    }
                }
            }
            Ok(None) => return Err("Agent process stdout EOF".into()),
            Err(_) => {
                // Short timeout elapsed, check conditions again
            }
        }
    }
}

async fn wait_for_session_ready(
    process: &mut RpcProcess,
    app: &mut App,
    session_id: &str,
) -> Result<(), String> {
    pump_until(process, app, |a| {
        a.sessions
            .known
            .get(session_id)
            .is_some_and(|view| view.info.loaded && view.transcript.complete && !view.loading)
    })
    .await
}

/// Pumps until a submitted turn has fully landed: no live loop remains and the
/// durable transcript contains at least one User item (so the ready state is
/// not conflated with a brand-new empty session).
async fn wait_turn_landed(
    process: &mut RpcProcess,
    app: &mut App,
    session_id: &str,
) -> Result<(), String> {
    pump_until(process, app, |a| {
        a.sessions.known.get(session_id).is_some_and(|view| {
            view.live.is_none()
                && view
                    .transcript
                    .items
                    .iter()
                    .any(|entry| matches!(&entry.item, HistoryItemWire::User(_)))
        })
    })
    .await
}

async fn wait_for_active_session(
    process: &mut RpcProcess,
    app: &mut App,
) -> Result<String, String> {
    pump_until(process, app, |a| a.sessions.active.is_some()).await?;
    let session_id = app
        .sessions
        .active
        .clone()
        .ok_or_else(|| "active session disappeared while waiting for it".to_owned())?;
    wait_for_session_ready(process, app, &session_id).await?;
    Ok(session_id)
}

async fn pump_until(
    process: &mut RpcProcess,
    app: &mut App,
    predicate: impl Fn(&App) -> bool,
) -> Result<(), String> {
    let deadline = Instant::now() + TIMEOUT;
    while !predicate(app) {
        if Instant::now() >= deadline {
            return Err(format!("e2e pump timed out: {:?}", app.connection));
        }
        pump_step(process, app).await?;
    }
    Ok(())
}

async fn dispatch(process: &mut RpcProcess, app: &mut App, event: AppEvent) -> Result<(), String> {
    let commands = app.update(event);
    for command in commands {
        match command {
            AppCommand::Rpc(req) => {
                process.send(req).await.map_err(|e| e.to_string())?;
            }
            AppCommand::KillChild => process.kill_child(),
            AppCommand::CopySelection(_) => {}
            AppCommand::Exit => return Ok(()),
        }
    }
    Ok(())
}

struct StrictShutdownReport {
    shutdown_ok: bool,
    cancelled_waits: Vec<TurnResultViewWire>,
    seen_eof: bool,
    seen_exit: bool,
}

async fn drain_shutdown_strict(
    process: &mut RpcProcess,
    app: &mut App,
) -> Result<StrictShutdownReport, String> {
    dispatch(process, app, AppEvent::ShutdownRequested).await?;

    let deadline = Instant::now() + Duration::from_secs(15);
    let mut shutdown_ok = false;
    let mut cancelled_waits = Vec::new();
    let mut seen_eof = false;
    let mut seen_exit = false;

    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(remaining, process.recv()).await {
            Ok(Some(event)) => {
                match &event {
                    RpcEvent::Frame(IncomingFrame::Response(resp)) => {
                        // Check if this is shutdown response
                        if let Some(res) = &resp.result {
                            if res.get("ok").and_then(|v| v.as_bool()) == Some(true) {
                                shutdown_ok = true;
                            }
                            if let Ok(turn_res) =
                                serde_json::from_value::<TurnResultViewWire>(res.clone())
                            {
                                cancelled_waits.push(turn_res);
                            }
                        }
                    }
                    RpcEvent::ConnectionClosed => {
                        seen_eof = true;
                    }
                    RpcEvent::Exited(_) => {
                        seen_exit = true;
                    }
                    _ => {}
                }

                dispatch(process, app, AppEvent::Rpc(event)).await?;
            }
            Ok(None) => {
                seen_eof = true;
                break;
            }
            Err(_) => {
                return Err("Strict shutdown timed out waiting for process events".into());
            }
        }

        if shutdown_ok && seen_eof && seen_exit {
            break;
        }
    }

    if !shutdown_ok {
        return Err("Strict shutdown failed: shutdown response never confirmed ok".into());
    }
    if !seen_eof {
        return Err("Strict shutdown failed: agent stdout never reached EOF".into());
    }
    if !seen_exit {
        return Err("Strict shutdown failed: agent child process never reported exit".into());
    }

    Ok(StrictShutdownReport {
        shutdown_ok,
        cancelled_waits,
        seen_eof,
        seen_exit,
    })
}

// ============================================================================
// Spec 61 Scenarios A - F
// ============================================================================

/// Spec 61.2 E2E-A: Discovery
/// Tests agent.ping (0.3.x gate), model.list, profile.list, session.list discovery.
#[test]
#[ignore = "requires MINICORE_AGENT_BIN; runs against self-contained loopback mock HTTP server"]
fn e2e_scenario_a_discovery() {
    let agent_bin = require_agent_bin();
    let (env, _) = E2eEnvironment::setup();

    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async move {
        let mut process = env.spawn_agent(&agent_bin);
        let mut app = App::new(env.workspace_path.clone());
        app.update(AppEvent::SetTheme(ThemeKind::Dark));

        dispatch(&mut process, &mut app, AppEvent::Bootstrap)
            .await
            .unwrap();
        pump_until(&mut process, &mut app, |a| {
            a.connection == ConnectionState::Ready
        })
        .await
        .unwrap();

        assert_eq!(app.connection, ConnectionState::Ready);
        assert!(!app.catalogs.models.is_empty());
        assert!(!app.catalogs.profiles.is_empty());

        let rep = drain_shutdown_strict(&mut process, &mut app).await.unwrap();
        assert!(rep.shutdown_ok);
        assert!(rep.seen_eof);
        assert!(rep.seen_exit);
        process.terminate().await;
    });
}

/// Real-Agent session lifecycle coverage: drive the Session panel through the
/// public App event path and verify the same session.list response that the
/// current Agent persists after rename and delete.
#[test]
#[ignore = "requires MINICORE_AGENT_BIN; runs against self-contained loopback mock HTTP server"]
fn e2e_session_panel_rename_and_delete_against_current_agent() {
    let agent_bin = require_agent_bin();
    let (env, _) = E2eEnvironment::setup();

    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async move {
        let mut process = env.spawn_agent(&agent_bin);
        let mut app = App::new(env.workspace_path.clone());

        let key_with_modifiers = |code, modifiers| {
            AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(code, modifiers)))
        };
        let key = |code| key_with_modifiers(code, KeyModifiers::empty());

        dispatch(&mut process, &mut app, AppEvent::Bootstrap)
            .await
            .unwrap();
        pump_until(&mut process, &mut app, |a| {
            a.connection == ConnectionState::Ready
        })
        .await
        .unwrap();

        dispatch(
            &mut process,
            &mut app,
            AppEvent::CreateSession {
                workspace: env.workspace_path.to_string_lossy().into_owned(),
                profile: Some("coding".to_owned()),
                model: None,
                reasoning: None,
                title: Some("Panel lifecycle original".to_owned()),
            },
        )
        .await
        .unwrap();
        let session_id = wait_for_active_session(&mut process, &mut app)
            .await
            .unwrap();
        pump_until(&mut process, &mut app, |a| {
            a.sessions.known.get(&session_id).is_some_and(|view| {
                view.state.as_ref().is_some_and(|state| {
                    state.status == minicore_tui::protocol::SessionStatusWire::Idle
                })
            })
        })
        .await
        .unwrap();

        dispatch(&mut process, &mut app, AppEvent::OpenSessionSelector)
            .await
            .unwrap();
        pump_until(&mut process, &mut app, |a| {
            !a.pending_requests
                .values()
                .any(|kind| matches!(kind, RequestKind::RefreshSessions { .. }))
        })
        .await
        .unwrap();

        dispatch(&mut process, &mut app, key(KeyCode::F(2)))
            .await
            .unwrap();
        dispatch(
            &mut process,
            &mut app,
            key_with_modifiers(KeyCode::Char('u'), KeyModifiers::CONTROL),
        )
        .await
        .unwrap();
        dispatch(
            &mut process,
            &mut app,
            AppEvent::Terminal(CrosstermEvent::Paste("Panel lifecycle renamed".to_owned())),
        )
        .await
        .unwrap();
        dispatch(&mut process, &mut app, key(KeyCode::Enter))
            .await
            .unwrap();
        pump_until(&mut process, &mut app, |a| {
            a.sessions
                .known
                .get(&session_id)
                .is_some_and(|view| view.info.title.as_deref() == Some("Panel lifecycle renamed"))
        })
        .await
        .unwrap();

        dispatch(&mut process, &mut app, key(KeyCode::F(5)))
            .await
            .unwrap();
        pump_until(&mut process, &mut app, |a| {
            !a.pending_requests
                .values()
                .any(|kind| matches!(kind, RequestKind::RefreshSessions { .. }))
                && a.sessions.list.iter().any(|session| {
                    session.session_id == session_id
                        && session.title.as_deref() == Some("Panel lifecycle renamed")
                })
        })
        .await
        .unwrap();

        dispatch(&mut process, &mut app, key(KeyCode::Delete))
            .await
            .unwrap();
        dispatch(&mut process, &mut app, key(KeyCode::Enter))
            .await
            .unwrap();
        pump_until(&mut process, &mut app, |a| {
            a.sessions.closed.contains(&session_id)
                && a.sessions
                    .known
                    .get(&session_id)
                    .is_some_and(|view| !view.info.loaded)
        })
        .await
        .unwrap();

        // Permanent deletion defaults to Cancel: Enter alone must return to
        // Browse without sending session.delete.
        dispatch(&mut process, &mut app, key(KeyCode::Enter))
            .await
            .unwrap();
        assert!(!app.pending_requests.values().any(|kind| {
            matches!(kind, RequestKind::DeleteSession { session_id: pending } if pending == &session_id)
        }));
        assert!(!app.sessions.deleted.contains(&session_id));

        // Re-enter the confirmation and explicitly choose Delete.
        dispatch(&mut process, &mut app, key(KeyCode::Delete))
            .await
            .unwrap();
        dispatch(&mut process, &mut app, key(KeyCode::Tab))
            .await
            .unwrap();
        dispatch(&mut process, &mut app, key(KeyCode::Enter))
            .await
            .unwrap();
        pump_until(&mut process, &mut app, |a| {
            a.sessions.deleted.contains(&session_id) && !a.sessions.known.contains_key(&session_id)
        })
        .await
        .unwrap();

        dispatch(&mut process, &mut app, key(KeyCode::F(5)))
            .await
            .unwrap();
        pump_until(&mut process, &mut app, |a| {
            !a.pending_requests
                .values()
                .any(|kind| matches!(kind, RequestKind::RefreshSessions { .. }))
                && !a
                    .sessions
                    .list
                    .iter()
                    .any(|session| session.session_id == session_id)
        })
        .await
        .unwrap();

        let rep = drain_shutdown_strict(&mut process, &mut app).await.unwrap();
        assert!(rep.shutdown_ok && rep.seen_eof && rep.seen_exit);
        process.terminate().await;
    });
}

/// Spec 61.2 E2E-B: Basic Turn Flow
/// Tests session.create, turn.send, turn.wait, and session.history reconciliation.
#[test]
#[ignore = "requires MINICORE_AGENT_BIN; runs against self-contained loopback mock HTTP server"]
fn e2e_scenario_b_basic_turn() {
    let agent_bin = require_agent_bin();
    let (env, _) = E2eEnvironment::setup();
    env._server
        .enqueue_sse(sse_text_response("Hello from mock Agent loopback!"));

    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async move {
        let mut process = env.spawn_agent(&agent_bin);
        let mut app = App::new(env.workspace_path.clone());

        dispatch(&mut process, &mut app, AppEvent::Bootstrap)
            .await
            .unwrap();
        pump_until(&mut process, &mut app, |a| {
            a.connection == ConnectionState::Ready
        })
        .await
        .unwrap();

        dispatch(
            &mut process,
            &mut app,
            AppEvent::CreateSession {
                workspace: env.workspace_path.to_string_lossy().into_owned(),
                profile: Some("coding".to_owned()),
                model: None,
                reasoning: None,
                title: Some("E2E Basic".to_owned()),
            },
        )
        .await
        .unwrap();
        let session_id = wait_for_active_session(&mut process, &mut app)
            .await
            .unwrap();

        dispatch(
            &mut process,
            &mut app,
            AppEvent::SubmitTurn {
                session_id: session_id.clone(),
                text: "Say hello".to_owned(),
            },
        )
        .await
        .unwrap();

        pump_until(&mut process, &mut app, |a| {
            a.sessions
                .known
                .get(&session_id)
                .is_some_and(|v| v.live.is_none() && v.transcript.complete)
        })
        .await
        .unwrap();

        let reqs = env._server.recorded_requests();
        assert_eq!(reqs.len(), 1, "Expected exactly 1 HTTP request");

        let view = &app.sessions.known[&session_id];
        assert!(!view.transcript.items.is_empty());

        let rep = drain_shutdown_strict(&mut process, &mut app).await.unwrap();
        assert!(rep.shutdown_ok);
        assert!(rep.seen_eof);
        assert!(rep.seen_exit);
        process.terminate().await;
    });
}

/// 0.2.2 max reasoning end-to-end: the TUI parses the Agent's advertised max
/// level, ships the literal `max` on session creation, and the single running
/// request reaches the provider body as `max` while the session keeps it.
#[test]
#[ignore = "requires MINICORE_AGENT_BIN; runs against self-contained loopback mock HTTP server"]
fn e2e_max_reasoning_ships_literal_max_and_provider_body_carries_it() {
    let agent_bin = require_agent_bin();
    let (env, _) = E2eEnvironment::setup();
    env._server
        .enqueue_sse_with_model(sse_text_response("max reasoning answer."), "luna-model");

    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async move {
        let mut process = env.spawn_agent(&agent_bin);
        let mut app = App::new(env.workspace_path.clone());
        dispatch(&mut process, &mut app, AppEvent::Bootstrap)
            .await
            .unwrap();
        pump_until(&mut process, &mut app, |a| {
            a.connection == ConnectionState::Ready
        })
        .await
        .unwrap();

        let luna = app
            .catalogs
            .models
            .iter()
            .find(|model| model.id == "luna")
            .expect("luna model is listed");
        assert!(
            luna.supported_reasoning.contains(&Reasoning::Max),
            "luna must advertise max in the catalog"
        );

        dispatch(
            &mut process,
            &mut app,
            AppEvent::CreateSession {
                workspace: env.workspace_path.to_string_lossy().into_owned(),
                profile: Some("luna".to_owned()),
                model: None,
                reasoning: Some(Reasoning::Max),
                title: Some("E2E Max".to_owned()),
            },
        )
        .await
        .unwrap();
        let session_id = wait_for_active_session(&mut process, &mut app)
            .await
            .unwrap();
        assert_eq!(
            app.sessions.known[&session_id].info.reasoning,
            Reasoning::Max,
            "new session retains max"
        );

        dispatch(
            &mut process,
            &mut app,
            AppEvent::SubmitTurn {
                session_id: session_id.clone(),
                text: "Think maximally".to_owned(),
            },
        )
        .await
        .unwrap();
        pump_until(&mut process, &mut app, |a| {
            a.sessions
                .known
                .get(&session_id)
                .is_some_and(|view| view.live.is_none() && view.transcript.complete)
        })
        .await
        .unwrap();

        let reqs = env._server.recorded_requests();
        let provider: Vec<_> = reqs
            .iter()
            .filter(|request| request.path.ends_with("/responses"))
            .collect();
        assert_eq!(provider.len(), 1, "exactly one provider request");
        assert_eq!(
            provider[0]
                .json
                .get("reasoning")
                .and_then(|value| value.get("effort"))
                .and_then(serde_json::Value::as_str),
            Some("max"),
            "provider body must carry the literal `reasoning.effort: max`: {}",
            provider[0].body
        );
        assert_eq!(
            app.sessions.known[&session_id].info.reasoning,
            Reasoning::Max,
            "session keeps max after the loop"
        );

        let rep = drain_shutdown_strict(&mut process, &mut app).await.unwrap();
        assert!(rep.shutdown_ok);
        assert!(rep.seen_eof);
        assert!(rep.seen_exit);
        process.terminate().await;
    });
}

/// Spec 61.2 E2E-C: Tool Execution Flow
/// Tests model tool call `read`, Agent tool execution, 2nd request, and turn persistence.
#[test]
#[ignore = "requires MINICORE_AGENT_BIN; runs against self-contained loopback mock HTTP server"]
fn e2e_scenario_c_tool_execution() {
    let agent_bin = require_agent_bin();
    let (env, _) = E2eEnvironment::setup();

    let test_file = env.workspace_path.join("data.txt");
    std::fs::write(&test_file, "contents of data.txt").expect("write test file");

    env._server.enqueue_sse(sse_tool_call_response(
        "call_1",
        "read",
        "{\"path\": \"data.txt\"}",
    ));
    env._server.enqueue_sse(sse_text_response(
        "File contents received: contents of data.txt",
    ));

    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async move {
        let mut process = env.spawn_agent(&agent_bin);
        let mut app = App::new(env.workspace_path.clone());

        dispatch(&mut process, &mut app, AppEvent::Bootstrap)
            .await
            .unwrap();
        pump_until(&mut process, &mut app, |a| {
            a.connection == ConnectionState::Ready
        })
        .await
        .unwrap();

        dispatch(
            &mut process,
            &mut app,
            AppEvent::CreateSession {
                workspace: env.workspace_path.to_string_lossy().into_owned(),
                profile: Some("coding".to_owned()),
                model: None,
                reasoning: None,
                title: Some("E2E Tool".to_owned()),
            },
        )
        .await
        .unwrap();
        let session_id = wait_for_active_session(&mut process, &mut app)
            .await
            .unwrap();

        dispatch(
            &mut process,
            &mut app,
            AppEvent::SubmitTurn {
                session_id: session_id.clone(),
                text: "Read data.txt".to_owned(),
            },
        )
        .await
        .unwrap();

        pump_until(&mut process, &mut app, |a| {
            a.sessions
                .known
                .get(&session_id)
                .is_some_and(|v| v.live.is_none() && v.transcript.complete)
        })
        .await
        .unwrap();

        let reqs = env._server.recorded_requests();
        assert_eq!(reqs.len(), 2, "Expected exactly 2 HTTP requests");

        let view = &app.sessions.known[&session_id];
        assert!(
            view.transcript
                .items
                .iter()
                .any(|i| matches!(i.item, HistoryItemWire::ToolResult(_)))
        );

        let rep = drain_shutdown_strict(&mut process, &mut app).await.unwrap();
        assert!(rep.shutdown_ok);
        assert!(rep.seen_eof);
        assert!(rep.seen_exit);
        process.terminate().await;
    });
}

/// Spec 61.2 E2E-D: Steering Flow
/// Tests gating Request 0 until turn.send confirms registered wait, sending steer,
/// awaiting steer acceptance, releasing gate, and verifying kind="steering" in History.
#[test]
#[ignore = "requires MINICORE_AGENT_BIN; runs against self-contained loopback mock HTTP server"]
fn e2e_scenario_d_steer_turn() {
    let agent_bin = require_agent_bin();
    let (env, _) = E2eEnvironment::setup();

    let test_file = env.workspace_path.join("data.txt");
    std::fs::write(&test_file, "contents").unwrap();

    let req0_gate = Arc::new(AtomicBool::new(false));
    // Request 0 is gated at arrival
    env._server.enqueue_gated(
        sse_tool_call_response("call_1", "read", "{\"path\": \"data.txt\"}"),
        req0_gate.clone(),
        Some("deep-model"),
    );
    // Request 1 responds with final text
    env._server
        .enqueue_sse(sse_text_response("Steered successfully."));

    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async move {
        let mut process = env.spawn_agent(&agent_bin);
        let mut app = App::new(env.workspace_path.clone());

        dispatch(&mut process, &mut app, AppEvent::Bootstrap)
            .await
            .unwrap();
        pump_until(&mut process, &mut app, |a| {
            a.connection == ConnectionState::Ready
        })
        .await
        .unwrap();

        dispatch(
            &mut process,
            &mut app,
            AppEvent::CreateSession {
                workspace: env.workspace_path.to_string_lossy().into_owned(),
                profile: Some("coding".to_owned()),
                model: None,
                reasoning: None,
                title: Some("E2E Steer".to_owned()),
            },
        )
        .await
        .unwrap();
        let session_id = wait_for_active_session(&mut process, &mut app)
            .await
            .unwrap();

        dispatch(
            &mut process,
            &mut app,
            AppEvent::SubmitTurn {
                session_id: session_id.clone(),
                text: "Initial command".to_owned(),
            },
        )
        .await
        .unwrap();

        // 1. Wait until Request 0 reaches mock server and App has registered wait_turn
        wait_for_request0_and_wait_turn(&env, &mut process, &mut app, &session_id)
            .await
            .unwrap();

        // 2. Dispatch steer
        dispatch(
            &mut process,
            &mut app,
            AppEvent::SteerTurn {
                session_id: session_id.clone(),
                text: "Steer instruction".to_owned(),
            },
        )
        .await
        .unwrap();

        // 3. Wait until steer is confirmed accepted/queued by the agent
        pump_until(&mut process, &mut app, |a| {
            a.sessions.known.get(&session_id).is_some_and(|v| {
                v.live.as_ref().is_some_and(|l| {
                    l.pending_steers
                        .iter()
                        .any(|s| s.state == PendingSteerState::Queued)
                })
            })
        })
        .await
        .unwrap();

        // 4. Release request 0 gate
        req0_gate.store(true, Ordering::Relaxed);

        // 5. Wait until turn completes
        pump_until(&mut process, &mut app, |a| {
            a.sessions
                .known
                .get(&session_id)
                .is_some_and(|v| v.live.is_none() && v.transcript.complete)
        })
        .await
        .unwrap();

        let reqs = env._server.recorded_requests();
        assert_eq!(
            reqs.len(),
            2,
            "Expected exactly 2 HTTP requests for steered turn"
        );

        let view = &app.sessions.known[&session_id];
        assert!(view.transcript.items.iter().any(|i| match &i.item {
            HistoryItemWire::User(u) => u.kind == UserMessageKindWire::Steering,
            _ => false,
        }));

        let rep = drain_shutdown_strict(&mut process, &mut app).await.unwrap();
        assert!(rep.shutdown_ok);
        assert!(rep.seen_eof);
        assert!(rep.seen_exit);
        process.terminate().await;
    });
}

/// 0.2.4 boundary repro (reasoning summaries): the provider streams three
/// distinct reasoning summary items (summary_index 0/1/2, fragments inside
/// item 0, no newline at item boundaries). The TUI must show each summary on
/// its own line while fragments within an item stay concatenated. RED until
/// the summary-item boundary is preserved through the Agent's parser.
#[test]
#[ignore = "requires MINICORE_AGENT_BIN; runs against self-contained loopback mock HTTP server"]
fn e2e_reasoning_summary_item_boundaries_survive_to_the_tui() {
    let agent_bin = require_agent_bin();
    let (env, _) = E2eEnvironment::setup();
    env._server
        .enqueue_chunked_sse(sse_multi_summary_reasoning_response());

    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async move {
        let mut process = env.spawn_agent(&agent_bin);
        let mut app = App::new(env.workspace_path.clone());
        dispatch(&mut process, &mut app, AppEvent::Bootstrap)
            .await
            .unwrap();
        pump_until(&mut process, &mut app, |a| {
            a.connection == ConnectionState::Ready
        })
        .await
        .unwrap();
        dispatch(
            &mut process,
            &mut app,
            AppEvent::CreateSession {
                workspace: env.workspace_path.to_string_lossy().into_owned(),
                profile: Some("coding".to_owned()),
                model: None,
                reasoning: Some(Reasoning::High),
                title: Some("E2E Reasoning Boundary".to_owned()),
            },
        )
        .await
        .unwrap();
        let session_id = wait_for_active_session(&mut process, &mut app)
            .await
            .unwrap();
        dispatch(
            &mut process,
            &mut app,
            AppEvent::SubmitTurn {
                session_id: session_id.clone(),
                text: "plan and detail".to_owned(),
            },
        )
        .await
        .unwrap();
        // Capture the raw Agent->TUI output_delta events while the turn
        // streams (the live view is gone once it completes). This records the
        // first actual wire/raw delta values that reach the TUI.
        let wire_deltas: std::cell::RefCell<Vec<String>> = std::cell::RefCell::new(Vec::new());
        loop {
            let event = tokio::time::timeout(Duration::from_millis(300), process.recv()).await;
            match event {
                Ok(Some(event)) => {
                    if let RpcEvent::Frame(IncomingFrame::Notification(
                        RpcNotification::AgentEvent(AgentEventWire::OutputDelta { data }),
                    )) = &event
                    {
                        wire_deltas
                            .borrow_mut()
                            .push(format!("{:?} {:?}", data.channel, data.delta));
                    }
                    let commands = app.update(AppEvent::Rpc(event));
                    for command in commands {
                        if let AppCommand::Rpc(request) = command {
                            process.send(request).await.unwrap();
                        }
                    }
                }
                Ok(None) => panic!("agent process stream ended"),
                Err(_) => {}
            }
            if app
                .sessions
                .known
                .get(&session_id)
                .is_some_and(|v| v.live.is_none() && v.transcript.complete)
            {
                break;
            }
        }

        let view = &app.sessions.known[&session_id];
        let live_reasoning: String = view
            .live
            .as_ref()
            .map(|live| {
                live.requests
                    .iter()
                    .flat_map(|request| request.parts.iter())
                    .filter_map(|part| match part {
                        LivePart::Reasoning(text) => Some(text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .concat()
            })
            .unwrap_or_default();
        // The durable assistant part(s) carry what the final/history render uses.
        let durable_reasoning: String = view
            .transcript
            .blocks
            .iter()
            .filter_map(|block| match block {
                minicore_tui::state::transcript::TranscriptBlock::Assistant(assistant) => {
                    assistant.parts.iter().find_map(|part| {
                        if let AssistantPart::Reasoning(text) = part {
                            Some(text.clone())
                        } else {
                            None
                        }
                    })
                }
                _ => None,
            })
            .collect();
        let reasoning = if !live_reasoning.is_empty() {
            live_reasoning
        } else {
            durable_reasoning.clone()
        };
        assert!(!reasoning.is_empty(), "a reasoning part must reach the TUI");
        assert!(
            reasoning.contains("caveats\nDetailing") && reasoning.contains("timeline\nAnalyzing"),
            "summary item boundaries must survive as line breaks, got the flattened value above"
        );
        assert!(
            reasoning.contains("Planning ... caveats"),
            "fragments within one summary item stay concatenated"
        );

        let rep = drain_shutdown_strict(&mut process, &mut app).await.unwrap();
        assert!(rep.shutdown_ok);
        assert!(rep.seen_eof);
        assert!(rep.seen_exit);
        process.terminate().await;
    });
}

/// 0.2.4 direct-RPC batch contract demonstration (the parent-requested RED-phase
/// test): when BOTH steering instructions are handed to the Agent runtime at one
/// request boundary (here via DIRECT turn.steer RPCs, bypassing the TUI FIFO
/// pacing), the runtime batches them into ONE next provider request. This
/// preserves the Agent/Runtime batch semantics independently of the TUI queue.
#[test]
#[ignore = "requires MINICORE_AGENT_BIN; runs against self-contained loopback mock HTTP server"]
fn e2e_two_consecutive_steers_both_reach_the_provider() {
    let agent_bin = require_agent_bin();
    let (env, _) = E2eEnvironment::setup();
    let test_file = env.workspace_path.join("data.txt");
    std::fs::write(&test_file, "contents").unwrap();

    let req0_gate = Arc::new(AtomicBool::new(false));
    env._server.enqueue_gated(
        sse_tool_call_response("call_1", "read", "{\"path\": \"data.txt\"}"),
        req0_gate.clone(),
        Some("deep-model"),
    );
    env._server
        .enqueue_sse(sse_text_response("handled the batched steers"));

    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async move {
        let mut process = env.spawn_agent(&agent_bin);
        let mut app = App::new(env.workspace_path.clone());
        dispatch(&mut process, &mut app, AppEvent::Bootstrap)
            .await
            .unwrap();
        pump_until(&mut process, &mut app, |a| {
            a.connection == ConnectionState::Ready
        })
        .await
        .unwrap();
        dispatch(
            &mut process,
            &mut app,
            AppEvent::CreateSession {
                workspace: env.workspace_path.to_string_lossy().into_owned(),
                profile: Some("coding".to_owned()),
                model: Some("deep".to_owned()),
                reasoning: Some(Reasoning::High),
                title: Some("E2E Direct Batch".to_owned()),
            },
        )
        .await
        .unwrap();
        let session_id = wait_for_active_session(&mut process, &mut app)
            .await
            .unwrap();
        dispatch(
            &mut process,
            &mut app,
            AppEvent::SubmitTurn {
                session_id: session_id.clone(),
                text: "Initial command".to_owned(),
            },
        )
        .await
        .unwrap();
        wait_for_request0_and_wait_turn(&env, &mut process, &mut app, &session_id)
            .await
            .unwrap();

        let loop_id = app.sessions.known[&session_id]
            .live
            .as_ref()
            .and_then(|live| live.reference.as_ref())
            .map(|r| r.loop_id.clone())
            .expect("active loop");
        let turn = TurnRef {
            session_id: session_id.clone(),
            loop_id: loop_id.clone(),
        };

        // Hand BOTH steers directly to the runtime before releasing Request 0,
        // exactly like the RED-phase demonstration: they both queue while the
        // request is held and batch into the single next provider request.
        let steer_a = OutgoingRequest::steer_turn(RequestId(9001), &turn, "taskA");
        let steer_b = OutgoingRequest::steer_turn(RequestId(9002), &turn, "taskB");
        process.send(steer_a).await.unwrap();
        process.send(steer_b).await.unwrap();

        // Read the two steer ACKs from the stream (the raw ids are dispatched
        // to the App too; its unknown-id notices are harmless).
        let ack_a = RequestId(9001);
        let ack_b = RequestId(9002);
        let mut saw_a = false;
        let mut saw_b = false;
        let deadline = Instant::now() + Duration::from_secs(30);
        while !(saw_a && saw_b) {
            let event = tokio::time::timeout(Duration::from_millis(300), process.recv())
                .await
                .map_err(|_| ())
                .and_then(|event| event.ok_or(()));
            match event {
                Ok(RpcEvent::Frame(IncomingFrame::Response(response))) => {
                    if response.id == ack_a {
                        saw_a = true;
                    } else if response.id == ack_b {
                        saw_b = true;
                    }
                    let commands = app.update(AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Response(
                        response,
                    ))));
                    for command in commands {
                        if let AppCommand::Rpc(request) = command {
                            process.send(request).await.unwrap();
                        }
                    }
                }
                Ok(other) => {
                    let commands = app.update(AppEvent::Rpc(other));
                    for command in commands {
                        if let AppCommand::Rpc(request) = command {
                            process.send(request).await.unwrap();
                        }
                    }
                }
                Err(_) => {
                    if Instant::now() >= deadline {
                        panic!("timed out waiting for both direct steer ACKs");
                    }
                }
            }
        }
        assert!(saw_a && saw_b, "both direct steer RPCs must be ACKed");

        req0_gate.store(true, Ordering::Relaxed);
        pump_until(&mut process, &mut app, |a| {
            a.sessions
                .known
                .get(&session_id)
                .is_some_and(|v| v.live.is_none() && v.transcript.complete)
        })
        .await
        .unwrap();

        let reqs = env._server.recorded_requests();
        let provider: Vec<_> = reqs
            .iter()
            .filter(|request| request.path.ends_with("/responses"))
            .collect();
        let user_texts_of = |request: &RecordedRequest| -> Vec<String> {
            request
                .json
                .get("input")
                .and_then(serde_json::Value::as_array)
                .map(|inputs| {
                    inputs
                        .iter()
                        .filter_map(|input| {
                            if input.get("role").and_then(serde_json::Value::as_str) != Some("user")
                            {
                                return None;
                            }
                            input.get("content").and_then(|content| {
                                content.as_array().and_then(|rows| {
                                    rows.iter().find_map(|row| {
                                        row.get("text")
                                            .and_then(serde_json::Value::as_str)
                                            .map(str::to_owned)
                                    })
                                })
                            })
                        })
                        .collect()
                })
                .unwrap_or_default()
        };
        assert!(
            provider.len() >= 2,
            "gated request0 + one batched request carrying both steers"
        );
        let batched = user_texts_of(provider.last().unwrap());
        assert!(
            batched.iter().any(|text| text == "taskA") && batched.iter().any(|text| text == "taskB"),
            "the single batched provider request must carry BOTH steers: {batched:?}"
        );

        let view = &app.sessions.known[&session_id];
        let steering_in_history = view
            .transcript
            .items
            .iter()
            .filter(|entry| {
                matches!(&entry.item, HistoryItemWire::User(u) if u.kind == UserMessageKindWire::Steering)
            })
            .count();
        assert_eq!(steering_in_history, 2, "both steers persist in history once");

        let rep = drain_shutdown_strict(&mut process, &mut app).await.unwrap();
        assert!(rep.shutdown_ok);
        assert!(rep.seen_eof);
        assert!(rep.seen_exit);
        process.terminate().await;
    });
}

/// 0.2.4 FIFO pacing contract: the TUI issues at most ONE in-flight steer per
/// session until a receipt (steer_progress applied_count) proves the previous
/// one entered a prepared prompt history. Request 1 carries A (NOT B);
/// request 2 carries A (as its earlier user/assistant turn) + B; the final
/// history shows both steering items exactly once.
#[test]
#[ignore = "requires MINICORE_AGENT_BIN; runs against self-contained loopback mock HTTP server"]
fn e2e_fifo_steers_are_paced_until_receipt() {
    let agent_bin = require_agent_bin();
    let (env, _) = E2eEnvironment::setup();
    let test_file = env.workspace_path.join("data.txt");
    std::fs::write(&test_file, "contents").unwrap();

    let req0_gate = Arc::new(AtomicBool::new(false));
    env._server.enqueue_gated(
        sse_tool_call_response("call_1", "read", "{\"path\": \"data.txt\"}"),
        req0_gate.clone(),
        Some("deep-model"),
    );
    env._server.enqueue_sse(sse_text_response("handled A"));
    env._server.enqueue_sse(sse_text_response("handled B"));

    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async move {
        let mut process = env.spawn_agent(&agent_bin);
        let mut app = App::new(env.workspace_path.clone());
        dispatch(&mut process, &mut app, AppEvent::Bootstrap)
            .await
            .unwrap();
        pump_until(&mut process, &mut app, |a| {
            a.connection == ConnectionState::Ready
        })
        .await
        .unwrap();
        dispatch(
            &mut process,
            &mut app,
            AppEvent::CreateSession {
                workspace: env.workspace_path.to_string_lossy().into_owned(),
                profile: Some("coding".to_owned()),
                model: None,
                reasoning: None,
                title: Some("E2E Paced Steers".to_owned()),
            },
        )
        .await
        .unwrap();
        let session_id = wait_for_active_session(&mut process, &mut app)
            .await
            .unwrap();
        dispatch(
            &mut process,
            &mut app,
            AppEvent::SubmitTurn {
                session_id: session_id.clone(),
                text: "Initial command".to_owned(),
            },
        )
        .await
        .unwrap();
        wait_for_request0_and_wait_turn(&env, &mut process, &mut app, &session_id)
            .await
            .unwrap();

        // Steer A: issued immediately, ACKed.
        dispatch(
            &mut process,
            &mut app,
            AppEvent::SteerTurn {
                session_id: session_id.clone(),
                text: "taskA".to_owned(),
            },
        )
        .await
        .unwrap();
        pump_until(&mut process, &mut app, |a| {
            a.sessions.known.get(&session_id).is_some_and(|v| {
                v.live.as_ref().is_some_and(|l| {
                    l.pending_steers
                        .iter()
                        .any(|s| s.text == "taskA" && s.state == PendingSteerState::Queued)
                })
            })
        })
        .await
        .unwrap();

        // Steer B admitted immediately (rapid second, textbook FIFO): it stays
        // in the local unsent queue while A is accepted-but-unconfirmed.
        dispatch(
            &mut process,
            &mut app,
            AppEvent::SteerTurn {
                session_id: session_id.clone(),
                text: "taskB".to_owned(),
            },
        )
        .await
        .unwrap();
        pump_until(&mut process, &mut app, |a| {
            a.sessions.known.get(&session_id).is_some_and(|v| {
                v.steer_queue.iter().any(|item| item.text == "taskB")
                    && v.live.as_ref().is_some_and(|l| {
                        l.pending_steers.iter().any(|s| s.text == "taskA")
                    })
            })
        })
        .await
        .unwrap();
        {
            let view = &app.sessions.known[&session_id];
            assert_eq!(
                view.live.as_ref().unwrap().pending_steers.len(),
                1,
                "only ONE steer RPC in flight while A is accepted-but-unconfirmed"
            );
            assert_eq!(view.steer_queue.len(), 1);
        }

        // Release gate: request 1 (with A, without B) flows, B is sent only
        // after A's receipt, then request 2 carries both A's turn and B.
        req0_gate.store(true, Ordering::Relaxed);
        pump_until(&mut process, &mut app, |a| {
            a.sessions
                .known
                .get(&session_id)
                .is_some_and(|v| v.live.is_none() && v.transcript.complete)
        })
        .await
        .unwrap();

        let reqs = env._server.recorded_requests();
        let provider: Vec<_> = reqs
            .iter()
            .filter(|request| request.path.ends_with("/responses"))
            .collect();
        assert_eq!(provider.len(), 3, "gated request0 + A + B");
        let user_texts = |request: &RecordedRequest| -> Vec<String> {
            request
                .json
                .get("input")
                .and_then(serde_json::Value::as_array)
                .map(|inputs| {
                    inputs
                        .iter()
                        .filter_map(|input| {
                            if input.get("role").and_then(serde_json::Value::as_str) != Some("user")
                            {
                                return None;
                            }
                            input.get("content").and_then(|content| {
                                content.as_array().and_then(|rows| {
                                    rows.iter().find_map(|row| {
                                        row.get("text")
                                            .and_then(serde_json::Value::as_str)
                                            .map(str::to_owned)
                                    })
                                })
                            })
                        })
                        .collect()
                })
                .unwrap_or_default()
        };
        let request1 = user_texts(provider[1]);
        assert!(
            request1.iter().any(|text| text == "taskA") && !request1.iter().any(|text| text == "taskB"),
            "request 1 must carry A and NOT B (pacing), got: {request1:?}"
        );
        let request2 = user_texts(provider[2]);
        assert!(
            request2.iter().any(|text| text == "taskB"),
            "request 2 must carry B, got: {request2:?}"
        );

        let view = &app.sessions.known[&session_id];
        let steering_in_history = view
            .transcript
            .items
            .iter()
            .filter(|entry| {
                matches!(&entry.item, HistoryItemWire::User(u) if u.kind == UserMessageKindWire::Steering)
            })
            .count();
        assert_eq!(steering_in_history, 2, "both steers persist in history once");

        let rep = drain_shutdown_strict(&mut process, &mut app).await.unwrap();
        assert!(rep.shutdown_ok);
        assert!(rep.seen_eof);
        assert!(rep.seen_exit);
        process.terminate().await;
    });
}

/// 0.2.4 duplicate-text FIFO: two IDENTICAL steering texts are still paced
/// one at a time and BOTH persist in the final history exactly once each.
#[test]
#[ignore = "requires MINICORE_AGENT_BIN; runs against self-contained loopback mock HTTP server"]
fn e2e_fifo_duplicate_texts_are_paced_and_both_persist() {
    let agent_bin = require_agent_bin();
    let (env, _) = E2eEnvironment::setup();
    let test_file = env.workspace_path.join("data.txt");
    std::fs::write(&test_file, "contents").unwrap();

    let req0_gate = Arc::new(AtomicBool::new(false));
    env._server.enqueue_gated(
        sse_tool_call_response("call_1", "read", "{\"path\": \"data.txt\"}"),
        req0_gate.clone(),
        Some("deep-model"),
    );
    env._server.enqueue_sse(sse_text_response("handled A"));
    env._server.enqueue_sse(sse_text_response("handled B"));

    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async move {
        let mut process = env.spawn_agent(&agent_bin);
        let mut app = App::new(env.workspace_path.clone());
        dispatch(&mut process, &mut app, AppEvent::Bootstrap)
            .await
            .unwrap();
        pump_until(&mut process, &mut app, |a| {
            a.connection == ConnectionState::Ready
        })
        .await
        .unwrap();
        dispatch(
            &mut process,
            &mut app,
            AppEvent::CreateSession {
                workspace: env.workspace_path.to_string_lossy().into_owned(),
                profile: Some("coding".to_owned()),
                model: None,
                reasoning: None,
                title: Some("E2E Dup Steers".to_owned()),
            },
        )
        .await
        .unwrap();
        let session_id = wait_for_active_session(&mut process, &mut app)
            .await
            .unwrap();
        dispatch(
            &mut process,
            &mut app,
            AppEvent::SubmitTurn {
                session_id: session_id.clone(),
                text: "Initial command".to_owned(),
            },
        )
        .await
        .unwrap();
        wait_for_request0_and_wait_turn(&env, &mut process, &mut app, &session_id)
            .await
            .unwrap();

        for _ in 0..2 {
            dispatch(
                &mut process,
                &mut app,
                AppEvent::SteerTurn {
                    session_id: session_id.clone(),
                    text: "same text twice".to_owned(),
                },
            )
            .await
            .unwrap();
        }
        // First issue immediately; the duplicate waits in the unsent queue.
        pump_until(&mut process, &mut app, |a| {
            a.sessions.known.get(&session_id).is_some_and(|v| {
                v.live.as_ref().is_some_and(|l| {
                    l.pending_steers
                        .iter()
                        .any(|s| s.state == PendingSteerState::Queued)
                }) && v.steer_queue.len() == 1
            })
        })
        .await
        .unwrap();

        req0_gate.store(true, Ordering::Relaxed);
        pump_until(&mut process, &mut app, |a| {
            a.sessions
                .known
                .get(&session_id)
                .is_some_and(|v| v.live.is_none() && v.transcript.complete)
        })
        .await
        .unwrap();

        let view = &app.sessions.known[&session_id];
        let steering_in_history = view
            .transcript
            .items
            .iter()
            .filter(|entry| {
                matches!(&entry.item, HistoryItemWire::User(u) if u.kind == UserMessageKindWire::Steering)
            })
            .count();
        assert_eq!(steering_in_history, 2, "both duplicate texts persist once each");

        let rep = drain_shutdown_strict(&mut process, &mut app).await.unwrap();
        assert!(rep.shutdown_ok);
        assert!(rep.seen_eof);
        assert!(rep.seen_exit);
        process.terminate().await;
    });
}

/// Spec 61.2 E2E-E: Same-Loop Dynamic Update (Spec 12 & 33 & 61 Deterministic)
/// 1. Gated Model A request 0 HTTP arrival; parses request JSON to verify model == "deep-model".
/// 2. Dispatches update to Model B via UI selector while request 0 is held.
/// 3. Awaits update response containing active_revision.
/// 4. Releases request 0 gate -> returns read ToolCall -> read executes.
/// 5. Subsequent request 1 uses Model B ("fast-model") in the same loop.
/// 6. Verifies:
///    - Requests: exactly 2 (A then B, 1 each; no extraneous requests or default fallbacks).
///    - Turn statistics: requests == 2, tool_rounds == 1, final_config_revision == active_revision.
///    - Identical session_id and loop_id.
///    - History sequence: Prompt, Assistant (request 0, deep), ToolResult, Assistant (request 1, fast).
///    - Old request labels are not rewritten to B; no cancel+send occurred.
#[test]
#[ignore = "requires MINICORE_AGENT_BIN; runs against self-contained loopback mock HTTP server"]
fn e2e_scenario_e_same_loop_update() {
    let agent_bin = require_agent_bin();
    let (env, _) = E2eEnvironment::setup();

    let test_file = env.workspace_path.join("data.txt");
    std::fs::write(&test_file, "contents").unwrap();

    let req0_gate = Arc::new(AtomicBool::new(false));

    // Request 0 must be deep-model and returns read tool call
    env._server.enqueue_gated(
        sse_tool_call_response("call_1", "read", "{\"path\": \"data.txt\"}"),
        req0_gate.clone(),
        Some("deep-model"),
    );

    // Request 1 must be fast-model and returns final answer
    env._server.enqueue_sse_with_model(
        sse_text_response("Response produced by model fast."),
        "fast-model",
    );

    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async move {
        let mut process = env.spawn_agent(&agent_bin);
        let mut app = App::new(env.workspace_path.clone());

        dispatch(&mut process, &mut app, AppEvent::Bootstrap)
            .await
            .unwrap();
        pump_until(&mut process, &mut app, |a| {
            a.connection == ConnectionState::Ready
        })
        .await
        .unwrap();

        dispatch(
            &mut process,
            &mut app,
            AppEvent::CreateSession {
                workspace: env.workspace_path.to_string_lossy().into_owned(),
                profile: Some("coding".to_owned()),
                model: Some("deep".to_owned()),
                reasoning: None,
                title: Some("E2E Update Same Loop".to_owned()),
            },
        )
        .await
        .unwrap();
        pump_until(&mut process, &mut app, |a| a.sessions.active.is_some())
            .await
            .unwrap();
        let session_id = app.sessions.active.clone().unwrap();

        dispatch(
            &mut process,
            &mut app,
            AppEvent::SubmitTurn {
                session_id: session_id.clone(),
                text: "Read data.txt with model switch".to_owned(),
            },
        )
        .await
        .unwrap();

        // 1. Wait until Request 0 reaches mock server and App has registered wait_turn
        wait_for_request0_and_wait_turn(&env, &mut process, &mut app, &session_id)
            .await
            .unwrap();

        let initial_reqs = env._server.recorded_requests();
        assert_eq!(
            initial_reqs.len(),
            1,
            "Request 0 must arrive at mock server"
        );
        assert_eq!(
            initial_reqs[0].model.as_deref(),
            Some("deep-model"),
            "Request 0 must use Model A (deep-model)"
        );

        let initial_loop_id = app.sessions.known[&session_id]
            .live
            .as_ref()
            .unwrap()
            .reference
            .as_ref()
            .unwrap()
            .loop_id
            .clone();

        // Trigger dynamic model update to 'fast' via UI selector
        dispatch(&mut process, &mut app, AppEvent::OpenModelSelector)
            .await
            .unwrap();
        dispatch(
            &mut process,
            &mut app,
            AppEvent::SetSelectorQuery {
                query: "fast".to_owned(),
            },
        )
        .await
        .unwrap();
        dispatch(&mut process, &mut app, AppEvent::ConfirmDock)
            .await
            .unwrap();

        // Wait until App receives update response with active revision and marks WaitingBoundary
        pump_until(&mut process, &mut app, |a| {
            a.sessions.known.get(&session_id).is_some_and(|v| {
                v.config_update.as_ref().is_some_and(|u| {
                    u.state == ConfigUpdateState::WaitingBoundary && u.revision.is_some()
                })
            })
        })
        .await
        .unwrap();

        let assigned_revision = app.sessions.known[&session_id]
            .config_update
            .as_ref()
            .unwrap()
            .revision
            .unwrap();
        assert!(assigned_revision >= 1);

        // Now release Request 0 gate -> Agent reads tool call, executes read tool, and reaches boundary
        req0_gate.store(true, Ordering::Relaxed);

        // Wait until turn completes
        pump_until(&mut process, &mut app, |a| {
            a.sessions
                .known
                .get(&session_id)
                .is_some_and(|v| v.live.is_none() && v.transcript.complete)
        })
        .await
        .unwrap();

        // 1. Verify exact HTTP requests and their JSON models
        let all_reqs = env._server.recorded_requests();
        assert_eq!(
            all_reqs.len(),
            2,
            "Expected exactly 2 HTTP requests (one for A, one for B)"
        );
        assert_eq!(all_reqs[0].model.as_deref(), Some("deep-model"));
        assert_eq!(all_reqs[1].model.as_deref(), Some("fast-model"));

        // 2. Verify turn outcome and loop statistics
        let view = &app.sessions.known[&session_id];
        let last_result = view
            .last_result
            .as_ref()
            .expect("last_result must be recorded after completion");
        assert_eq!(last_result.turn.session_id, session_id);
        assert_eq!(
            last_result.turn.loop_id, initial_loop_id,
            "Loop ID must remain identical throughout dynamic update (no cancel+send)"
        );
        assert_eq!(
            last_result.requests, 2,
            "Must execute exactly 2 requests in this turn"
        );
        assert_eq!(
            last_result.tool_rounds, 1,
            "Must execute exactly 1 tool round"
        );
        assert_eq!(
            last_result.final_config_revision, assigned_revision,
            "final_config_revision must match the updated revision"
        );

        // 3. Verify history sequence and labels
        let items = &view.transcript.items;
        assert_eq!(items.len(), 4, "Transcript must contain 4 items");

        assert!(matches!(&items[0].item, HistoryItemWire::User(_)));

        // Request 0: must retain original model label 'deep'
        match &items[1].item {
            HistoryItemWire::Assistant(a) => {
                assert_eq!(a.request_index, 0);
                assert_eq!(a.model, "deep");
                assert_eq!(a.tool_calls.len(), 1);
            }
            other => panic!("Expected Assistant for item 1, got {:?}", other),
        }

        assert!(matches!(&items[2].item, HistoryItemWire::ToolResult(_)));

        // Request 1: must show updated model label 'fast'
        match &items[3].item {
            HistoryItemWire::Assistant(a) => {
                assert_eq!(a.request_index, 1);
                assert_eq!(a.model, "fast");
            }
            other => panic!("Expected Assistant for item 3, got {:?}", other),
        }

        let rep = drain_shutdown_strict(&mut process, &mut app).await.unwrap();
        assert!(rep.shutdown_ok);
        assert!(rep.seen_eof);
        assert!(rep.seen_exit);
        process.terminate().await;
    });
}

/// Spec 61.2 E2E-E2: Dynamic Update Not Extending Single-Request Turn
/// Verifies updating config during a single-request turn does not synthesize extra requests,
/// and the next turn cleanly picks up the new model.
#[test]
#[ignore = "requires MINICORE_AGENT_BIN; runs against self-contained loopback mock HTTP server"]
fn e2e_scenario_e2_update_single_request_then_next_turn() {
    let agent_bin = require_agent_bin();
    let (env, _) = E2eEnvironment::setup();

    let req0_gate = Arc::new(AtomicBool::new(false));

    // Turn 1 Request 0: deep-model, gated, returns direct final text without tool call
    env._server.enqueue_gated(
        sse_text_response("Turn 1 direct answer with model deep."),
        req0_gate.clone(),
        Some("deep-model"),
    );

    // Turn 2 Request 0: fast-model, returns final text
    env._server.enqueue_sse_with_model(
        sse_text_response("Turn 2 direct answer with model fast."),
        "fast-model",
    );

    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async move {
        let mut process = env.spawn_agent(&agent_bin);
        let mut app = App::new(env.workspace_path.clone());

        dispatch(&mut process, &mut app, AppEvent::Bootstrap)
            .await
            .unwrap();
        pump_until(&mut process, &mut app, |a| {
            a.connection == ConnectionState::Ready
        })
        .await
        .unwrap();

        dispatch(
            &mut process,
            &mut app,
            AppEvent::CreateSession {
                workspace: env.workspace_path.to_string_lossy().into_owned(),
                profile: Some("coding".to_owned()),
                model: Some("deep".to_owned()),
                reasoning: None,
                title: Some("E2E Single Request Update".to_owned()),
            },
        )
        .await
        .unwrap();
        let session_id = wait_for_active_session(&mut process, &mut app)
            .await
            .unwrap();

        // Submit Turn 1
        dispatch(
            &mut process,
            &mut app,
            AppEvent::SubmitTurn {
                session_id: session_id.clone(),
                text: "Turn 1 prompt".to_owned(),
            },
        )
        .await
        .unwrap();

        // Wait until Request 0 is held by gate and wait_turn is in flight
        wait_for_request0_and_wait_turn(&env, &mut process, &mut app, &session_id)
            .await
            .unwrap();

        // Update model to fast via selector
        dispatch(&mut process, &mut app, AppEvent::OpenModelSelector)
            .await
            .unwrap();
        dispatch(
            &mut process,
            &mut app,
            AppEvent::SetSelectorQuery {
                query: "fast".to_owned(),
            },
        )
        .await
        .unwrap();
        dispatch(&mut process, &mut app, AppEvent::ConfirmDock)
            .await
            .unwrap();

        // Wait for update response confirmation
        pump_until(&mut process, &mut app, |a| {
            a.sessions.known.get(&session_id).is_some_and(|v| {
                v.config_update
                    .as_ref()
                    .is_some_and(|u| u.revision.is_some())
            })
        })
        .await
        .unwrap();

        // Release Turn 1 Request 0 gate
        req0_gate.store(true, Ordering::Relaxed);

        // Turn 1 must finish immediately with 1 request (no tool round extension)
        pump_until(&mut process, &mut app, |a| {
            a.sessions
                .known
                .get(&session_id)
                .is_some_and(|v| v.live.is_none() && v.transcript.complete)
        })
        .await
        .unwrap();

        let t1_res = app.sessions.known[&session_id]
            .last_result
            .as_ref()
            .unwrap();
        assert_eq!(t1_res.requests, 1, "Turn 1 must not be extended");
        assert_eq!(t1_res.tool_rounds, 0);

        // Submit Turn 2
        dispatch(
            &mut process,
            &mut app,
            AppEvent::SubmitTurn {
                session_id: session_id.clone(),
                text: "Turn 2 prompt".to_owned(),
            },
        )
        .await
        .unwrap();

        // Wait for Turn 2 completion
        pump_until(&mut process, &mut app, |a| {
            a.sessions
                .known
                .get(&session_id)
                .is_some_and(|v| v.live.is_none() && v.transcript.complete)
        })
        .await
        .unwrap();

        let reqs = env._server.recorded_requests();
        assert_eq!(
            reqs.len(),
            2,
            "Expected exactly 2 total requests across turns"
        );
        assert_eq!(reqs[0].model.as_deref(), Some("deep-model"));
        assert_eq!(reqs[1].model.as_deref(), Some("fast-model"));

        let rep = drain_shutdown_strict(&mut process, &mut app).await.unwrap();
        assert!(rep.shutdown_ok);
        assert!(rep.seen_eof);
        assert!(rep.seen_exit);
        process.terminate().await;
    });
}

/// Spec 61.2 E2E-F: Shutdown Cancels In-Flight Turn.Wait
/// Tests shutting down while turn.wait is pending: verifies turn.wait receives cancelled outcome
/// with reason=user and persistence record, shutdown receives ok response, and process exits with EOF.
#[test]
#[ignore = "requires MINICORE_AGENT_BIN; runs against self-contained loopback mock HTTP server"]
fn e2e_scenario_f_shutdown_cancels_active_wait() {
    let agent_bin = require_agent_bin();
    let (env, _) = E2eEnvironment::setup();

    let held_gate = Arc::new(AtomicBool::new(false));
    // Request 0 is held indefinitely until shutdown arrives
    env._server.enqueue_gated(
        sse_text_response("Should not be delivered before shutdown."),
        held_gate.clone(),
        Some("deep-model"),
    );

    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async move {
        let mut process = env.spawn_agent(&agent_bin);
        let mut app = App::new(env.workspace_path.clone());

        dispatch(&mut process, &mut app, AppEvent::Bootstrap)
            .await
            .unwrap();
        pump_until(&mut process, &mut app, |a| {
            a.connection == ConnectionState::Ready
        })
        .await
        .unwrap();

        dispatch(
            &mut process,
            &mut app,
            AppEvent::CreateSession {
                workspace: env.workspace_path.to_string_lossy().into_owned(),
                profile: Some("coding".to_owned()),
                model: None,
                reasoning: None,
                title: Some("E2E Shutdown Cancel".to_owned()),
            },
        )
        .await
        .unwrap();
        let session_id = wait_for_active_session(&mut process, &mut app)
            .await
            .unwrap();

        dispatch(
            &mut process,
            &mut app,
            AppEvent::SubmitTurn {
                session_id: session_id.clone(),
                text: "Long running prompt to be cancelled by shutdown".to_owned(),
            },
        )
        .await
        .unwrap();

        // 1. Wait until Request 0 reaches mock server and App has registered wait_turn
        wait_for_request0_and_wait_turn(&env, &mut process, &mut app, &session_id)
            .await
            .unwrap();

        // Now initiate strict shutdown while wait_turn is pending
        let rep = drain_shutdown_strict(&mut process, &mut app).await.unwrap();
        assert!(rep.shutdown_ok, "Shutdown response must be confirmed ok");
        assert!(rep.seen_eof, "Process stdout must reach EOF");
        assert!(rep.seen_exit, "Process child must report exit");

        // Verify cancelled wait outcome
        assert!(
            !rep.cancelled_waits.is_empty(),
            "Expected at least 1 turn.wait result received during shutdown"
        );
        let wait_res = &rep.cancelled_waits[0];
        assert_eq!(
            wait_res.outcome,
            LoopOutcomeWire::Cancelled {
                reason: CancelReasonWire::User
            },
            "Outcome must be cancelled with reason user"
        );
        assert_eq!(
            wait_res.persistence,
            TurnPersistenceWire::Persisted,
            "Cancelled turn must report persistence record"
        );

        held_gate.store(true, Ordering::Relaxed);
        process.terminate().await;
    });
}

/// Stress: six loops across ten provider requests, each with distinct final
/// text. The transcript must order the six finals exactly once each and the
/// mock must have received exactly ten requests (four tool loops at two
/// requests each plus two text-only loops).
#[test]
#[ignore = "requires MINICORE_AGENT_BIN; runs against self-contained loopback mock HTTP server"]
fn e2e_stress_six_loops_ten_requests_no_repeated_final_text() {
    let agent_bin = require_agent_bin();
    let (env, _) = E2eEnvironment::setup();
    let test_file = env.workspace_path.join("data.txt");
    std::fs::write(&test_file, "numbered stress fixture contents").unwrap();

    // Loops 0..3 run a read call (two requests each); loops 4..5 are
    // text-only (one request each). 4*2 + 2 = 10 requests total.
    for i in 0..4 {
        env._server.enqueue_sse(sse_tool_call_response(
            &format!("stress-call-{i}"),
            "read",
            r#"{"path": "data.txt"}"#,
        ));
        env._server
            .enqueue_sse(sse_text_response(&format!("stress-final-loop-{i}")));
    }
    env._server
        .enqueue_sse(sse_text_response("stress-final-loop-4"));
    env._server
        .enqueue_sse(sse_text_response("stress-final-loop-5"));

    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async move {
        let mut process = env.spawn_agent(&agent_bin);
        let mut app = App::new(env.workspace_path.clone());

        dispatch(&mut process, &mut app, AppEvent::Bootstrap)
            .await
            .unwrap();
        pump_until(&mut process, &mut app, |a| {
            a.connection == ConnectionState::Ready
        })
        .await
        .unwrap();
        dispatch(
            &mut process,
            &mut app,
            AppEvent::CreateSession {
                workspace: env.workspace_path.to_string_lossy().into_owned(),
                profile: Some("coding".to_owned()),
                model: None,
                reasoning: None,
                title: Some("E2E Stress 6x10".to_owned()),
            },
        )
        .await
        .unwrap();
        let session_id = wait_for_active_session(&mut process, &mut app)
            .await
            .unwrap();

        for index in 0..6 {
            dispatch(
                &mut process,
                &mut app,
                AppEvent::SubmitTurn {
                    session_id: session_id.clone(),
                    text: format!("stress turn {index}"),
                },
            )
            .await
            .unwrap();
            wait_turn_landed(&mut process, &mut app, &session_id)
                .await
                .unwrap();
        }

        assert_eq!(
            env._server.recorded_requests().len(),
            10,
            "expected exactly 10 provider requests across six loops"
        );

        let view = &app.sessions.known[&session_id];
        let finals: Vec<String> = view
            .transcript
            .items
            .iter()
            .filter_map(|entry| match &entry.item {
                HistoryItemWire::Assistant(text) if !text.text.is_empty() => {
                    Some(text.text.clone())
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            finals.len(),
            6,
            "expected six distinct finals, got {finals:?}"
        );
        let mut seen = std::collections::HashSet::new();
        for (index, final_text) in finals.iter().enumerate() {
            assert_eq!(
                final_text.as_str(),
                format!("stress-final-loop-{index}"),
                "final text order mismatch"
            );
            assert!(
                seen.insert(final_text.clone()),
                "duplicate repeated final text {final_text:?}"
            );
        }

        let rep = drain_shutdown_strict(&mut process, &mut app).await.unwrap();
        assert!(rep.shutdown_ok && rep.seen_eof && rep.seen_exit);
        process.terminate().await;
    });
}

/// Stress: keep the second tool card expanded while a new loop starts
/// generating in the background. The per-session fold override must survive
/// the new loop's live section rebasing.
#[test]
#[ignore = "requires MINICORE_AGENT_BIN; runs against self-contained loopback mock HTTP server"]
fn e2e_stress_second_tool_expansion_survives_background_generation() {
    let agent_bin = require_agent_bin();
    let (env, _) = E2eEnvironment::setup();
    let test_file = env.workspace_path.join("data.txt");
    std::fs::write(&test_file, "contents for the two-tool stress loop").unwrap();

    env._server.enqueue_sse(sse_two_tool_calls_response(
        "stress-tool-1",
        "stress-tool-2",
    ));
    env._server
        .enqueue_sse(sse_text_response("stress two-tool loop complete"));
    env._server
        .enqueue_sse(sse_text_response("background generation finished"));

    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async move {
        let mut process = env.spawn_agent(&agent_bin);
        let mut app = App::new(env.workspace_path.clone());

        dispatch(&mut process, &mut app, AppEvent::Bootstrap)
            .await
            .unwrap();
        pump_until(&mut process, &mut app, |a| {
            a.connection == ConnectionState::Ready
        })
        .await
        .unwrap();
        dispatch(
            &mut process,
            &mut app,
            AppEvent::CreateSession {
                workspace: env.workspace_path.to_string_lossy().into_owned(),
                profile: Some("coding".to_owned()),
                model: None,
                reasoning: None,
                title: Some("E2E Stress Expand".to_owned()),
            },
        )
        .await
        .unwrap();
        let session_id = wait_for_active_session(&mut process, &mut app)
            .await
            .unwrap();

        dispatch(
            &mut process,
            &mut app,
            AppEvent::SubmitTurn {
                session_id: session_id.clone(),
                text: "run two tools".to_owned(),
            },
        )
        .await
        .unwrap();
        wait_turn_landed(&mut process, &mut app, &session_id)
            .await
            .unwrap();

        let view = &app.sessions.known[&session_id];
        let first_calls: Vec<&ToolCallViewWire> = view
            .transcript
            .items
            .iter()
            .filter_map(|entry| match &entry.item {
                HistoryItemWire::Assistant(assistant) if assistant.request_index == 0 => {
                    Some(assistant.tool_calls.as_slice())
                }
                _ => None,
            })
            .flatten()
            .collect();
        assert_eq!(first_calls.len(), 2, "expected two tool calls in request 0");
        let first_loop_id = view
            .transcript
            .items
            .iter()
            .find_map(|entry| match &entry.item {
                HistoryItemWire::Assistant(assistant) => Some(assistant.loop_id.clone()),
                _ => None,
            })
            .unwrap();
        let second_tool_id = first_calls[1].tool_call_id.clone();

        // Toggle until the per-tool override records Expanded (the read card
        // may already render expanded by default, so the first press can
        // collapse it; the second press is the deterministic expand).
        for _ in 0..2 {
            dispatch(
                &mut process,
                &mut app,
                AppEvent::ToggleTool {
                    session_id: session_id.clone(),
                    loop_id: first_loop_id.clone(),
                    request_index: 0,
                    tool_call_id: second_tool_id.clone(),
                },
            )
            .await
            .unwrap();
            let key = ToolKey::new(&session_id, &first_loop_id, 0, &second_tool_id);
            if app.sessions.known[&session_id].tool_folds.get(&key) == Some(&FoldOverride::Expanded)
            {
                break;
            }
        }
        let key = ToolKey::new(&session_id, &first_loop_id, 0, &second_tool_id);
        assert_eq!(
            app.sessions.known[&session_id].tool_folds.get(&key),
            Some(&FoldOverride::Expanded),
            "second tool must be expanded before background generation"
        );

        // Start the background loop and assert the override stays while it is
        // live (mid-generation), not just after it settles.
        dispatch(
            &mut process,
            &mut app,
            AppEvent::SubmitTurn {
                session_id: session_id.clone(),
                text: "background generation".to_owned(),
            },
        )
        .await
        .unwrap();
        pump_until(&mut process, &mut app, |a| {
            a.sessions
                .known
                .get(&session_id)
                .is_some_and(|v| v.live.is_some())
        })
        .await
        .unwrap();
        assert_eq!(
            app.sessions.known[&session_id].tool_folds.get(&key),
            Some(&FoldOverride::Expanded),
            "second tool expansion lost while a new loop generates"
        );
        wait_turn_landed(&mut process, &mut app, &session_id)
            .await
            .unwrap();
        assert_eq!(
            app.sessions.known[&session_id].tool_folds.get(&key),
            Some(&FoldOverride::Expanded),
            "second tool expansion lost after background loop completed"
        );

        assert_eq!(env._server.recorded_requests().len(), 3);
        let rep = drain_shutdown_strict(&mut process, &mut app).await.unwrap();
        assert!(rep.shutdown_ok && rep.seen_eof && rep.seen_exit);
        process.terminate().await;
    });
}

/// Spec 9.4/12.4: per-request usage reported by the real Agent appears in
/// the TUI while the loop is still running (no fake zero), then the persisted
/// loop total replaces the live rows without double-counting — including
/// after history is loaded.
#[test]
#[ignore = "requires MINICORE_AGENT_BIN; runs against self-contained loopback mock HTTP server"]
fn e2e_live_request_usage_shows_during_loop_and_persisted_total_replaces_it() {
    use minicore_tui::state::session::UsageCompleteness;

    let agent_bin = require_agent_bin();
    let (env, _) = E2eEnvironment::setup();
    let test_file = env.workspace_path.join("data.txt");
    std::fs::write(&test_file, "contents").unwrap();

    let req1_gate = Arc::new(AtomicBool::new(false));
    // Request 0: tool call with usage 10/10, answered immediately.
    env._server.enqueue_sse_with_model(
        sse_tool_call_response("call_1", "read", r#"{"path": "data.txt"}"#),
        "deep-model",
    );
    // Request 1: text with a distinct usage 30/40, gated until we assert the
    // live footer state.
    env._server.enqueue_gated(
        sse_text_response_with_usage("final usage answer", 30, 40, 0),
        req1_gate.clone(),
        Some("deep-model"),
    );

    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async move {
        let mut process = env.spawn_agent(&agent_bin);
        let mut app = App::new(env.workspace_path.clone());

        dispatch(&mut process, &mut app, AppEvent::Bootstrap)
            .await
            .unwrap();
        pump_until(&mut process, &mut app, |a| {
            a.connection == ConnectionState::Ready
        })
        .await
        .unwrap();
        dispatch(
            &mut process,
            &mut app,
            AppEvent::CreateSession {
                workspace: env.workspace_path.to_string_lossy().into_owned(),
                profile: Some("coding".to_owned()),
                model: Some("deep".to_owned()),
                reasoning: None,
                title: Some("E2E Live Usage".to_owned()),
            },
        )
        .await
        .unwrap();
        let session_id = wait_for_active_session(&mut process, &mut app)
            .await
            .unwrap();

        dispatch(
            &mut process,
            &mut app,
            AppEvent::SubmitTurn {
                session_id: session_id.clone(),
                text: "read data.txt".to_owned(),
            },
        )
        .await
        .unwrap();

        // Request 0's real usage lands while the loop is still running
        // (request 1 is gated on the mock). The footer projection must show
        // ↑10 ↓10 — a known value, not a fake zero.
        pump_until(&mut process, &mut app, |app| {
            app.sessions
                .known
                .get(&session_id)
                .is_some_and(|view| {
                    let Some(loop_id) = view
                        .live
                        .as_ref()
                        .and_then(|live| live.reference.as_ref())
                        .map(|reference| reference.loop_id.clone())
                    else {
                        return false;
                    };
                    view.live_request_usage
                        .get(&(loop_id, 0))
                        .is_some_and(|usage| usage.input_tokens == Some(10))
                })
        })
        .await
        .unwrap();

        let view = &app.sessions.known[&session_id];
        let loop_id = view
            .live
            .as_ref()
            .and_then(|live| live.reference.as_ref())
            .expect("loop must still be live")
            .loop_id
            .clone();
        assert_eq!(
            view.live_request_usage.len(),
            1,
            "only request 0 may have reported usage while request 1 is gated"
        );
        assert_eq!(view.usage_projection.usage.input_tokens, Some(10));
        assert_eq!(view.usage_projection.usage.output_tokens, Some(10));
        assert_eq!(
            view.usage_projection.completeness,
            UsageCompleteness::Partial,
            "a live loop with a partially-known total stays Partial, never fake Complete"
        );
        assert!(view.live.is_some(), "loop must still be running");

        req1_gate.store(true, Ordering::Relaxed);
        pump_until(&mut process, &mut app, |app| {
            app.sessions.known.get(&session_id).is_some_and(|view| {
                view.live.is_none() && view.transcript.complete
            })
        })
        .await
        .unwrap();

        // The persisted total (10+30 / 10+40) replaces the live rows: no
        // double counting of request 0.
        let result = app.sessions.known[&session_id]
            .last_result
            .as_ref()
            .expect("loop result")
            .clone();
        assert_eq!(result.usage.input_tokens, Some(40));
        assert_eq!(result.usage.output_tokens, Some(50));
        assert_eq!(
            app.sessions.known[&session_id]
                .usage_projection
                .usage
                .input_tokens,
            Some(40)
        );
        assert_eq!(
            app.sessions.known[&session_id]
                .usage_projection
                .usage
                .output_tokens,
            Some(50)
        );
        assert_eq!(
            app.sessions.known[&session_id]
                .usage_projection
                .completeness,
            UsageCompleteness::Complete
        );

        // History loads per-request usage rows for the same loop; the total
        // stays 40/50 (persisted total wins over both live rows and history).
        pump_until(&mut process, &mut app, |app| {
            app.sessions.known.get(&session_id).is_some_and(|view| {
                view.transcript.blocks.iter().any(|block| {
                    matches!(block, minicore_tui::state::transcript::TranscriptBlock::Assistant(assistant) if assistant.loop_id == loop_id)
                })
            })
        })
        .await
        .unwrap();
        assert_eq!(
            app.sessions.known[&session_id]
                .usage_projection
                .usage
                .input_tokens,
            Some(40),
            "history reload must not double-count the completed loop"
        );

        let rep = drain_shutdown_strict(&mut process, &mut app).await.unwrap();
        assert!(rep.shutdown_ok && rep.seen_eof && rep.seen_exit);
        process.terminate().await;
    });
}

/// Stress: an expanded tool card in session A survives switching to session B
/// and back to A (per-session fold state is not shared or reset by activation).
#[test]
#[ignore = "requires MINICORE_AGENT_BIN; runs against self-contained loopback mock HTTP server"]
fn e2e_stress_session_switch_preserves_tool_fold() {
    let agent_bin = require_agent_bin();
    let (env, _) = E2eEnvironment::setup();
    let test_file = env.workspace_path.join("data.txt");
    std::fs::write(&test_file, "contents").unwrap();

    env._server.enqueue_sse(sse_tool_call_response(
        "switch-session-tool",
        "read",
        r#"{"path": "data.txt"}"#,
    ));
    env._server
        .enqueue_sse(sse_text_response("session-a loop complete"));
    env._server
        .enqueue_sse(sse_text_response("session-b loop complete"));

    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async move {
        let mut process = env.spawn_agent(&agent_bin);
        let mut app = App::new(env.workspace_path.clone());

        dispatch(&mut process, &mut app, AppEvent::Bootstrap)
            .await
            .unwrap();
        pump_until(&mut process, &mut app, |a| {
            a.connection == ConnectionState::Ready
        })
        .await
        .unwrap();
        dispatch(
            &mut process,
            &mut app,
            AppEvent::CreateSession {
                workspace: env.workspace_path.to_string_lossy().into_owned(),
                profile: Some("coding".to_owned()),
                model: None,
                reasoning: None,
                title: Some("E2E Stress Switch A".to_owned()),
            },
        )
        .await
        .unwrap();
        let session_a = wait_for_active_session(&mut process, &mut app)
            .await
            .unwrap();

        dispatch(
            &mut process,
            &mut app,
            AppEvent::SubmitTurn {
                session_id: session_a.clone(),
                text: "read the file in session A".to_owned(),
            },
        )
        .await
        .unwrap();
        wait_turn_landed(&mut process, &mut app, &session_a)
            .await
            .unwrap();

        // `wait_turn_landed` only guarantees the live loop is gone and some
        // User item arrived; the durable history may page in the Assistant
        // (tool-call) item afterwards. This stress case genuinely needs that
        // Assistant item of the completed loop before deriving its loop id
        // and toggling the derived Tool fold, so wait for the actual item.
        pump_until(&mut process, &mut app, |a| {
            a.sessions.known.get(&session_a).is_some_and(|view| {
                view.transcript
                    .items
                    .iter()
                    .any(|entry| matches!(&entry.item, HistoryItemWire::Assistant(_)))
            })
        })
        .await
        .unwrap();

        let view = &app.sessions.known[&session_a];
        let first_loop_id = view
            .transcript
            .items
            .iter()
            .find_map(|entry| match &entry.item {
                HistoryItemWire::Assistant(assistant) => Some(assistant.loop_id.clone()),
                _ => None,
            })
            .unwrap();

        for _ in 0..2 {
            dispatch(
                &mut process,
                &mut app,
                AppEvent::ToggleTool {
                    session_id: session_a.clone(),
                    loop_id: first_loop_id.clone(),
                    request_index: 0,
                    tool_call_id: "switch-session-tool".to_owned(),
                },
            )
            .await
            .unwrap();
            let key = ToolKey::new(&session_a, &first_loop_id, 0, "switch-session-tool");
            if app.sessions.known[&session_a].tool_folds.get(&key) == Some(&FoldOverride::Expanded)
            {
                break;
            }
        }
        let key = ToolKey::new(&session_a, &first_loop_id, 0, "switch-session-tool");
        assert_eq!(
            app.sessions.known[&session_a].tool_folds.get(&key),
            Some(&FoldOverride::Expanded)
        );

        // Second session (a separate workspace) becomes active; run one loop.
        let workspace_b = env.temp_dir.join("workspace-b");
        std::fs::create_dir_all(&workspace_b).unwrap();
        dispatch(
            &mut process,
            &mut app,
            AppEvent::CreateSession {
                workspace: workspace_b.to_string_lossy().into_owned(),
                profile: Some("coding".to_owned()),
                model: None,
                reasoning: None,
                title: Some("E2E Stress Switch B".to_owned()),
            },
        )
        .await
        .unwrap();
        pump_until(&mut process, &mut app, |a| {
            a.sessions
                .active
                .as_deref()
                .is_some_and(|id| id != session_a.as_str())
        })
        .await
        .unwrap();
        let session_b = app
            .sessions
            .active
            .clone()
            .ok_or_else(|| "session B never became active".to_owned())
            .unwrap();
        assert_ne!(session_b, session_a);
        wait_for_session_ready(&mut process, &mut app, &session_b)
            .await
            .unwrap();
        dispatch(
            &mut process,
            &mut app,
            AppEvent::SubmitTurn {
                session_id: session_b.clone(),
                text: "read the file in session B".to_owned(),
            },
        )
        .await
        .unwrap();
        wait_turn_landed(&mut process, &mut app, &session_b)
            .await
            .unwrap();

        // Back to A: the expansion must still be in effect.
        dispatch(
            &mut process,
            &mut app,
            AppEvent::OpenSession {
                session_id: session_a.clone(),
            },
        )
        .await
        .unwrap();
        pump_until(&mut process, &mut app, |a| {
            a.sessions.active.as_deref() == Some(session_a.as_str())
        })
        .await
        .unwrap();
        assert_eq!(
            app.sessions.known[&session_a].tool_folds.get(&key),
            Some(&FoldOverride::Expanded),
            "session-switch reset session A's tool fold"
        );

        assert_eq!(env._server.recorded_requests().len(), 3);

        let rep = drain_shutdown_strict(&mut process, &mut app).await.unwrap();
        assert!(rep.shutdown_ok && rep.seen_eof && rep.seen_exit);
        process.terminate().await;
    });
}
