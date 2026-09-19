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
use minicore_tui::app::{App, CliPrefs, ConnectionState, RequestKind, StartupSession};
use minicore_tui::command::AppCommand;
use minicore_tui::event::{AppEvent, JobOutcome, RpcEvent};
use minicore_tui::protocol::{
    AgentEventWire, CancelReasonWire, CompactStatusWire, IncomingFrame, LoopOutcomeWire,
    OutgoingRequest, Reasoning, RequestId, RpcNotification, ToolCallViewWire, TurnPersistenceWire,
    TurnRef, TurnResultViewWire,
};
use minicore_tui::rpc::RpcProcess;
use minicore_tui::state::export::ExportPhase;
use minicore_tui::state::selection::{Dock, SessionPanelMode};
use minicore_tui::state::session::ConfigUpdateState;
use minicore_tui::state::transcript::{AssistantPart, TranscriptBlock};
use minicore_tui::state::turn::{LivePart, PendingSteerState};
use minicore_tui::state::{FoldOverride, ToolKey};
use minicore_tui::theme::ThemeKind;
use serde_json::json;

/// A deleted session is absent from both the known views and the catalog
/// list; the catalog generation, not a tombstone set, keeps it deleted.
fn session_absent(app: &App, session_id: &str) -> bool {
    !app.sessions.known.contains_key(session_id)
        && !app
            .sessions
            .list
            .iter()
            .any(|session| session.session_id == session_id)
}

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

fn durable_assistant_text(item: &minicore_tui::state::transcript::AssistantBlock) -> String {
    item.parts
        .iter()
        .filter_map(|part| match part {
            AssistantPart::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
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
        let gate_timeout = Duration::from_secs(120);
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
    let commands = drain_editor_jobs(app).await?;
    dispatch_commands(process, app, commands).await?;
    let wait = if app.tool_detail().is_some() {
        app.next_tick()
            .unwrap_or(Duration::from_millis(500))
            .min(Duration::from_millis(500))
    } else if editor_job_in_flight() {
        Duration::from_millis(20)
    } else {
        Duration::from_secs(10)
    };
    let event = tokio::time::timeout(wait, process.recv()).await;
    let event = match event {
        // A silent window is scheduling contention, not a product stall: the
        // caller's overall deadline (bounded) decides whether the pump timed
        // out. Aborting on a single 10s window turned the parallel spawn of
        // ten real Agent processes into flaky "recv timed out" failures.
        Ok(event) => event.ok_or("agent process stream ended")?,
        // A silent window is not a stalled app: the real main loop ticks while
        // it waits for the next frame. The tick is what retries a queued read
        // slot or a parked export record.
        Err(_) => {
            // Only the scan/export chains need an idle pass to retry a read
            // slot or a parked record; other flows must see exactly the events
            // the Agent sent, or a preparation retry would run too early.
            let editor_commands = drain_editor_jobs(app).await?;
            dispatch_commands(process, app, editor_commands).await?;
            if app.export_running()
                || matches!(app.dock, Dock::Search(_))
                || editor_job_in_flight()
                || app.tool_detail().is_some()
            {
                let commands = app.update(AppEvent::Tick);
                dispatch_commands(process, app, commands).await?;
            }
            return Ok(());
        }
    };

    let commands = app.update(AppEvent::Rpc(event));
    dispatch_commands(process, app, commands).await
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
                        AppCommand::LocalScan(request) => handle_local_scan(app, &request),
                        AppCommand::StartExport(request) => handle_start_export(*request),
                        AppCommand::StartEditor(request) => handle_start_editor(*request),
                        AppCommand::PersistConfig(request) => {
                            let request = *request;
                            let result =
                                minicore_tui::config::persist(&request.path, &request.config)
                                    .map_err(|error| error.to_string());
                            let _ = app.update(AppEvent::JobFinished(JobOutcome::Config {
                                path: request.path,
                                config: request.config,
                                result,
                            }));
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
        a.sessions.known.get(session_id).is_some_and(|view| {
            view.info.loaded && view.transcript.complete && !view.history_read.is_loading()
        })
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
                    .window
                    .items()
                    .any(|(_, entry)| matches!(entry.as_ref(), TranscriptBlock::User(_)))
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
            return Err(format!(
                "e2e pump timed out: {:?} (active {:?}, pending {:?}, list {:?}, dock {:?})",
                app.connection,
                app.sessions.active,
                app.pending_requests
                    .iter()
                    .map(|(id, kind)| format!("{}:{kind:?}", id.0))
                    .collect::<Vec<_>>(),
                app.sessions
                    .list
                    .iter()
                    .map(|session| (session.session_id.clone(), session.loaded))
                    .collect::<Vec<_>>(),
                app.dock,
            ));
        }
        pump_step(process, app).await?;
    }
    Ok(())
}

/// Creates a session while another one may already be active and waits for the
/// new session's own view (the create ACK is what activates it).
async fn create_additional_session(
    process: &mut RpcProcess,
    app: &mut App,
    workspace: &std::path::Path,
    title: &str,
) -> String {
    let before = app.sessions.active.clone();
    dispatch(
        process,
        app,
        AppEvent::CreateSession {
            workspace: workspace.to_string_lossy().into_owned(),
            profile: Some("coding".to_owned()),
            model: Some("deep".to_owned()),
            reasoning: Some(Reasoning::High),
            title: Some(title.to_owned()),
        },
    )
    .await
    .unwrap();
    pump_until(process, app, |a| {
        a.sessions.active.is_some() && a.sessions.active != before
    })
    .await
    .unwrap();
    let session_id = app.sessions.active.clone().unwrap();
    wait_for_session_ready(process, app, &session_id)
        .await
        .unwrap();
    session_id
}

/// Runs the owned loaded-content scan inline: the production worker executes
/// this exact body, so the E2E assertions observe the real matcher.
fn handle_local_scan(app: &mut App, request: &minicore_tui::jobs::LocalScanRequest) {
    let outcome = minicore_tui::state::search::run_local_scan(request);
    let _ = app.update(AppEvent::LocalScanFinished(Box::new(outcome)));
}

/// The owned export writers this test process started. The real job runs on
/// the blocking pool, exactly as `main.rs` starts it; the handles are awaited
/// by the pump so the completion event reaches the App.
static EXPORT_HANDLES: std::sync::Mutex<
    Vec<(
        minicore_tui::jobs::ExportCapture,
        tokio::task::JoinHandle<minicore_tui::jobs::ExportOutcome>,
    )>,
> = std::sync::Mutex::new(Vec::new());

static EDITOR_HANDLES: std::sync::Mutex<
    Vec<(
        minicore_tui::jobs::EditorCapture,
        tokio::task::JoinHandle<minicore_tui::jobs::EditorOutcome>,
    )>,
> = std::sync::Mutex::new(Vec::new());

fn handle_start_export(request: minicore_tui::command::StartExportRequest) {
    let capture = request.capture.clone();
    let cancel = request.cancel.clone();
    let handle = tokio::task::spawn_blocking(move || {
        minicore_tui::jobs::run_export_job(&request.target, request.overwrite, request.rx, cancel)
    });
    EXPORT_HANDLES
        .lock()
        .expect("export handle lock")
        .push((capture, handle));
}

fn handle_start_editor(request: minicore_tui::command::StartEditorRequest) {
    let capture = request.capture.clone();
    let cancel = Arc::new(AtomicBool::new(false));
    let cancel_for_job = Arc::clone(&cancel);
    let handle = tokio::task::spawn_blocking(move || {
        minicore_tui::jobs::run_editor_job(&request.editor, &request.draft, &cancel_for_job)
    });
    EDITOR_HANDLES
        .lock()
        .expect("editor handle lock")
        .push((capture, handle));
}

fn editor_job_in_flight() -> bool {
    !EDITOR_HANDLES
        .lock()
        .expect("editor handle lock")
        .is_empty()
}

/// Feeds every finished export job's outcome back to the reducer.
async fn drain_export_jobs(app: &mut App) -> Result<Vec<AppCommand>, String> {
    let finished: Vec<_> = {
        let mut guard = EXPORT_HANDLES.lock().expect("export handle lock");
        let mut finished = Vec::new();
        let mut index = 0;
        while index < guard.len() {
            if guard[index].1.is_finished() {
                finished.push(guard.remove(index));
            } else {
                index += 1;
            }
        }
        finished
    };
    let mut commands = Vec::new();
    for (capture, handle) in finished {
        let outcome = handle.await.map_err(|error| error.to_string())?;
        commands.extend(app.update(AppEvent::JobFinished(JobOutcome::Export {
            capture,
            outcome,
        })));
    }
    Ok(commands)
}

async fn drain_editor_jobs(app: &mut App) -> Result<Vec<AppCommand>, String> {
    let finished: Vec<_> = {
        let mut guard = EDITOR_HANDLES.lock().expect("editor handle lock");
        let mut finished = Vec::new();
        let mut index = 0;
        while index < guard.len() {
            if guard[index].1.is_finished() {
                finished.push(guard.remove(index));
            } else {
                index += 1;
            }
        }
        finished
    };
    let mut commands = Vec::new();
    for (capture, handle) in finished {
        let outcome = handle.await.map_err(|error| error.to_string())?;
        commands.extend(app.update(AppEvent::JobFinished(JobOutcome::Editor {
            capture,
            outcome,
        })));
    }
    Ok(commands)
}

async fn dispatch(process: &mut RpcProcess, app: &mut App, event: AppEvent) -> Result<(), String> {
    let commands = app.update(event);

    dispatch_commands(process, app, commands).await
}

/// The reload path is deliberately exercised against the real Agent process:
/// the RPC acknowledgement and the three catalog reads must settle before the
/// TUI reports success; the active session view is deliberately not re-read.
#[test]
#[ignore = "requires MINICORE_AGENT_BIN; runs against self-contained loopback mock HTTP server"]
fn e2e_configuration_reload_refreshes_catalogs_only() {
    let agent_bin = require_agent_bin();
    let (env, _) = E2eEnvironment::setup();
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
                title: Some("Reload E2E".to_owned()),
            },
        )
        .await
        .unwrap();
        let session_id = wait_for_active_session(&mut process, &mut app)
            .await
            .unwrap();

        let config = std::fs::read_to_string(&env.config_path).unwrap();
        let config = config.replace(
            "[profiles.fast]\nmodel = \"fast\"\nreasoning = \"low\"",
            "[profiles.fast]\nmodel = \"fast\"\nreasoning = \"high\"",
        );
        std::fs::write(&env.config_path, config).unwrap();

        dispatch(&mut process, &mut app, AppEvent::Reload)
            .await
            .unwrap();
        pump_until(&mut process, &mut app, |a| {
            !a.pending_requests.values().any(|kind| {
                matches!(
                    kind,
                    RequestKind::Reload { .. }
                        | RequestKind::ReloadModels { .. }
                        | RequestKind::ReloadProfiles { .. }
                        | RequestKind::ReloadSessions { .. }
                )
            }) && a.notices().back().is_some_and(|notice| {
                notice.text == "Agent configuration and session metadata reloaded"
            })
        })
        .await
        .unwrap();

        assert_eq!(app.sessions.active.as_deref(), Some(session_id.as_str()));
        assert!(
            app.pending_requests.values().all(|kind| !matches!(
                kind,
                RequestKind::SessionState { .. }
                    | RequestKind::SessionPresentation { .. }
                    | RequestKind::History { .. }
            )),
            "a catalog reload must not stage a session view read"
        );
        let view = app.sessions.known.get(&session_id).unwrap();
        assert!(view.info.loaded);
        assert!(view.transcript.complete);
        assert_eq!(
            view.result_confirmation,
            minicore_tui::state::session::ResultConfirmation::Confirmed,
            "reload must not mark the settled result as needing a read"
        );
        assert!(app.catalogs.loaded);
        assert_eq!(
            app.catalogs
                .profiles
                .iter()
                .find(|profile| profile.id == "fast")
                .map(|profile| profile.reasoning),
            Some(Reasoning::High)
        );

        let report = drain_shutdown_strict(&mut process, &mut app).await.unwrap();
        assert!(report.shutdown_ok && report.seen_eof && report.seen_exit);
        process.terminate().await;
    });
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
        assert!(!session_absent(&app, &session_id));

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
            session_absent(a, &session_id)
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
/// Tests session.create, turn.send, turn.wait, and session.read reconciliation.
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
        assert!(!view.transcript.window.is_empty());

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
                .window
                .items()
                .any(|(_, i)| matches!(i.as_ref(), TranscriptBlock::Tool(_)))
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
        assert!(view.transcript.window.items().any(|(_, i)| {
            matches!(
                i.as_ref(),
                TranscriptBlock::User(u) if u.kind == minicore_tui::protocol::UserMessageKindWire::Steering
            )
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
            .filter_map(|block| match block.as_ref() {
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
                    let commands = app.update(AppEvent::Rpc(RpcEvent::Frame(
                        IncomingFrame::Response(response),
                    )));
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
            batched.iter().any(|text| text == "taskA")
                && batched.iter().any(|text| text == "taskB"),
            "the single batched provider request must carry BOTH steers: {batched:?}"
        );

        let view = &app.sessions.known[&session_id];
        let steering_in_history = view
            .transcript
            .window
            .items()
            .filter(|(_, entry)| {
                matches!(
                    entry.as_ref(),
                    TranscriptBlock::User(u)
                        if u.kind == minicore_tui::protocol::UserMessageKindWire::Steering
                )
            })
            .count();
        assert_eq!(
            steering_in_history, 2,
            "both steers persist in history once"
        );

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
                    && v.live
                        .as_ref()
                        .is_some_and(|l| l.pending_steers.iter().any(|s| s.text == "taskA"))
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
            request1.iter().any(|text| text == "taskA")
                && !request1.iter().any(|text| text == "taskB"),
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
            .window
            .items()
            .filter(|(_, entry)| {
                matches!(
                    entry.as_ref(),
                    TranscriptBlock::User(u)
                        if u.kind == minicore_tui::protocol::UserMessageKindWire::Steering
                )
            })
            .count();
        assert_eq!(
            steering_in_history, 2,
            "both steers persist in history once"
        );

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
            .window
            .items()
            .filter(|(_, entry)| {
                matches!(
                    entry.as_ref(),
                    TranscriptBlock::User(u)
                        if u.kind == minicore_tui::protocol::UserMessageKindWire::Steering
                )
            })
            .count();
        assert_eq!(
            steering_in_history, 2,
            "both duplicate texts persist once each"
        );

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
            last_result.requests,
            Some(2),
            "Must execute exactly 2 requests in this turn"
        );
        assert_eq!(
            last_result.tool_rounds,
            Some(1),
            "Must execute exactly 1 tool round"
        );
        assert_eq!(
            last_result.final_config_revision,
            Some(assigned_revision),
            "final_config_revision must match the updated revision"
        );

        // 3. Verify history sequence and labels
        assert_eq!(
            view.transcript.window.len(),
            4,
            "Transcript must contain 4 items"
        );
        let item = |index: usize| view.transcript.window.item(index).unwrap();

        assert!(matches!(item(0).as_ref(), TranscriptBlock::User(_)));

        // Request 0: must retain original model label 'deep'
        match item(1).as_ref() {
            TranscriptBlock::Assistant(a) => {
                assert_eq!(a.request_index, 0);
                assert_eq!(a.model, "deep");
                assert_eq!(a.tool_calls.len(), 1);
            }
            other => panic!("Expected Assistant for item 1, got {:?}", other),
        }

        assert!(matches!(item(2).as_ref(), TranscriptBlock::Tool(_)));

        // Request 1: must show updated model label 'fast'
        match item(3).as_ref() {
            TranscriptBlock::Assistant(a) => {
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
        assert_eq!(t1_res.requests, Some(1), "Turn 1 must not be extended");
        assert_eq!(t1_res.tool_rounds, Some(0));

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
            Some(TurnPersistenceWire::Persisted),
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
            .window
            .items()
            .filter_map(|(_, entry)| match entry.as_ref() {
                TranscriptBlock::Assistant(assistant) => {
                    let text = durable_assistant_text(assistant);
                    (!text.is_empty()).then_some(text)
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
        let first_calls: Vec<ToolCallViewWire> = view
            .transcript
            .window
            .items()
            .filter_map(|(_, entry)| match entry.as_ref() {
                TranscriptBlock::Assistant(assistant) if assistant.request_index == 0 => {
                    Some(assistant.tool_calls.clone())
                }
                _ => None,
            })
            .flatten()
            .collect();
        assert_eq!(first_calls.len(), 2, "expected two tool calls in request 0");
        let first_loop_id = view
            .transcript
            .window
            .items()
            .find_map(|(_, entry)| match entry.as_ref() {
                TranscriptBlock::Assistant(assistant) => Some(assistant.loop_id.clone()),
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
        assert_eq!(result.usage.as_ref().unwrap().input_tokens, Some(40));
        assert_eq!(result.usage.as_ref().unwrap().output_tokens, Some(50));
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
                    matches!(block.as_ref(), minicore_tui::state::transcript::TranscriptBlock::Assistant(assistant) if assistant.loop_id == loop_id)
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
                    .window
                    .items()
                    .any(|(_, entry)| matches!(entry.as_ref(), TranscriptBlock::Assistant(_)))
            })
        })
        .await
        .unwrap();

        let view = &app.sessions.known[&session_a];
        let first_loop_id = view
            .transcript
            .window
            .items()
            .find_map(|(_, entry)| match entry.as_ref() {
                TranscriptBlock::Assistant(assistant) => Some(assistant.loop_id.clone()),
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

/// Types a slash command into the composer and presses Enter through the
/// same key events a user sends.
async fn submit_slash_command(
    process: &mut RpcProcess,
    app: &mut App,
    command: &str,
) -> Result<(), String> {
    for character in command.chars() {
        dispatch(
            process,
            app,
            AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(
                KeyCode::Char(character),
                KeyModifiers::empty(),
            ))),
        )
        .await?;
    }
    dispatch(
        process,
        app,
        AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::empty(),
        ))),
    )
    .await
}

/// Pumps until `predicate` holds while also running the decode worker's exact
/// body, so read chains that need decoded items (full-session scans, exports)
/// progress exactly as they do behind `main.rs`'s owned worker.
async fn pump_until_with_decode(
    process: &mut RpcProcess,
    app: &mut App,
    predicate: impl Fn(&App) -> bool,
) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let commands = drain_pending_decode(app);
        dispatch_commands(process, app, commands).await?;
        if predicate(app) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!("e2e decode pump timed out: {:?}", app.dock));
        }
        pump_step(process, app).await?;
        let commands = drain_pending_decode(app);
        dispatch_commands(process, app, commands).await?;
        if predicate(app) {
            return Ok(());
        }
    }
}

/// Runs the one decode worker's exact body inline and feeds every outcome
/// back, so read chains, full-session scans and exports progress in E2E the
/// way they do behind `main.rs`'s owned worker.
fn drain_pending_decode(app: &mut App) -> Vec<AppCommand> {
    let mut commands = Vec::new();
    while let Some(request) = app.pending_decode_request() {
        let identity = request.identity.clone();
        let fingerprint = request.fingerprint;
        let scan = request.scan.as_ref().map(|scan| (**scan).clone());
        let export = request.export.as_ref().map(|spec| **spec);
        app.mark_decode_scheduled();
        let (result, scan_outcome, export_outcome) =
            match minicore_tui::protocol::read::decode_item(&request.item.data) {
                Ok(decoded) => {
                    let scan_outcome = scan.map(|scan| {
                        let mut plan = minicore_tui::state::search::ScanPlan::new(
                            &scan.needle,
                            scan.include_thinking,
                        );
                        plan.scan_item(request.item.index, &decoded);
                        Box::new(minicore_tui::jobs::ScanItemOutcome {
                            index: request.item.index,
                            matches: plan.collector.matches,
                        })
                    });
                    let export_outcome = export.map(|spec| {
                        let rendered = minicore_tui::state::export::item_markdown(&decoded, spec);
                        Box::new(minicore_tui::jobs::ExportItemOutcome {
                            index: request.item.index,
                            markdown: rendered.markdown,
                            opaque_parts: rendered.opaque_parts,
                        })
                    });
                    (Ok(decoded), scan_outcome, export_outcome)
                }
                Err(error) => (Err(error), None, None),
            };
        let outcome = minicore_tui::jobs::DecodeOutcome {
            identity,
            fingerprint,
            result,
            cancelled: false,
            scan: scan_outcome,
            export: export_outcome,
        };
        commands.extend(app.update(AppEvent::HistoryItemDecoded(Box::new(outcome))));
    }
    commands
}

/// Presses one unmodified key through the reducer path the TUI uses.
async fn press_key(process: &mut RpcProcess, app: &mut App, code: KeyCode) -> Result<(), String> {
    dispatch(
        process,
        app,
        AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(
            code,
            KeyModifiers::empty(),
        ))),
    )
    .await
}

/// Presses one Ctrl chord.
async fn press_ctrl_key(
    process: &mut RpcProcess,
    app: &mut App,
    character: char,
) -> Result<(), String> {
    dispatch(
        process,
        app,
        AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(
            KeyCode::Char(character),
            KeyModifiers::CONTROL,
        ))),
    )
    .await
}

/// Sends already-reduced commands exactly as the main loop would.
async fn dispatch_commands(
    process: &mut RpcProcess,
    app: &mut App,
    commands: Vec<AppCommand>,
) -> Result<(), String> {
    for command in commands {
        match command {
            AppCommand::Rpc(req) => {
                process.send(req).await.map_err(|e| e.to_string())?;
            }
            AppCommand::LocalScan(request) => handle_local_scan(app, &request),
            AppCommand::StartExport(request) => handle_start_export(*request),
            AppCommand::StartEditor(request) => handle_start_editor(*request),
            AppCommand::PersistConfig(request) => {
                let request = *request;
                let result = minicore_tui::config::persist(&request.path, &request.config)
                    .map_err(|error| error.to_string());
                let _ = app.update(AppEvent::JobFinished(JobOutcome::Config {
                    path: request.path,
                    config: request.config,
                    result,
                }));
            }
            AppCommand::KillChild => process.kill_child(),
            AppCommand::CopySelection(_) => {}
            AppCommand::Exit => return Ok(()),
        }
    }
    Ok(())
}

/// Runs a slash line through the composer entry point. The command is set
/// directly because some of these cases intentionally run while another dock
/// owns the keyboard; the reducer path after `submit_composer` is the same one
/// the Enter key uses.
async fn run_slash_command(
    process: &mut RpcProcess,
    app: &mut App,
    command: &str,
) -> Result<(), String> {
    app.composer_mut().set_text(command);
    let commands = app.submit_composer();
    dispatch_commands(process, app, commands).await
}

async fn type_draft(process: &mut RpcProcess, app: &mut App, text: &str) -> Result<(), String> {
    dispatch(
        process,
        app,
        AppEvent::Terminal(CrosstermEvent::Paste(text.to_owned())),
    )
    .await
}

fn enable_bash_profile(env: &E2eEnvironment) {
    let mut config: toml::Value =
        toml::from_str(&std::fs::read_to_string(&env.config_path).unwrap()).unwrap();
    config["profiles"]["coding"]["tools"] =
        toml::Value::Array(vec![toml::Value::String("bash".into())]);
    std::fs::write(&env.config_path, toml::to_string(&config).unwrap()).unwrap();
}

#[test]
#[ignore = "requires MINICORE_AGENT_BIN; real non-PTY Bash streams and tool detail"]
fn e2e_tool_detail_drains_real_nonpty_stdout_and_stderr_after_terminal() {
    use minicore_tui::protocol::ToolDataStreamWire as Stream;
    let agent_bin = require_agent_bin();
    let (env, _) = E2eEnvironment::setup();
    enable_bash_profile(&env);
    let command = "if test -t 1; then printf PTY; else printf PIPE; fi; i=0; while [ $i -lt 8000 ]; do printf '中🙂\\n'; i=$((i+1)); done; printf ERR >&2; exit 7";
    env._server.enqueue_sse(sse_tool_call_response(
        "bash_detail",
        "bash",
        &json!({"command": command}).to_string(),
    ));
    env._server
        .enqueue_sse(sse_text_response("completed command"));
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(async move {
            let mut process = env.spawn_agent(&agent_bin);
            let mut app = App::new(env.workspace_path.clone());
            dispatch(&mut process, &mut app, AppEvent::Bootstrap)
                .await
                .unwrap();
            pump_until(&mut process, &mut app, |app| {
                app.connection == ConnectionState::Ready
            })
            .await
            .unwrap();
            let session = create_additional_session(
                &mut process,
                &mut app,
                &env.workspace_path,
                "tool detail",
            )
            .await;
            dispatch(
                &mut process,
                &mut app,
                AppEvent::SubmitTurn {
                    session_id: session.clone(),
                    text: "run synthetic command".into(),
                },
            )
            .await
            .unwrap();
            pump_until_with_decode(&mut process, &mut app, |app| {
                app.active_view()
                    .is_some_and(|view| view.live.is_none() && view.transcript.complete)
            })
            .await
            .unwrap();
            let key = app
                .active_view()
                .unwrap()
                .tool_presentations
                .keys()
                .find(|key| key.tool_call_id == "bash_detail")
                .unwrap()
                .clone();
            app.composer_mut().set_text("independent draft");
            let commands = app.open_tool_detail(key.clone());
            dispatch_commands(&mut process, &mut app, commands)
                .await
                .unwrap();
            pump_until_with_decode(&mut process, &mut app, |app| {
                app.tool_detail()
                    .is_some_and(|detail| detail.tab == Stream::Stdout && detail.stream().eof)
            })
            .await
            .unwrap();
            let stream = app.tool_detail().unwrap().stream();
            assert_eq!(stream.next_offset, 4 + ("中🙂\n".len() * 8000) as u64);
            assert_eq!(
                stream.display_text(),
                format!("PIPE{}", "中🙂\n".repeat(8000))
            );
            let command = app.tool_facts().unwrap().command.as_ref().unwrap();
            assert_eq!(command.exit_code, Some(7));
            assert!(command.output_complete);
            assert!(
                app.tool_detail().unwrap().error.is_none(),
                "nonzero exit is not RPC failure"
            );
            press_key(&mut process, &mut app, KeyCode::Tab)
                .await
                .unwrap();
            pump_until_with_decode(&mut process, &mut app, |app| {
                app.tool_detail()
                    .is_some_and(|detail| detail.tab == Stream::Stderr && detail.stream().eof)
            })
            .await
            .unwrap();
            assert_eq!(app.tool_detail().unwrap().stream().display_text(), "ERR");
            press_key(&mut process, &mut app, KeyCode::Esc)
                .await
                .unwrap();
            assert!(app.tool_detail().is_none());
            assert_eq!(app.composer().content(), "independent draft");
            assert!(
                drain_shutdown_strict(&mut process, &mut app)
                    .await
                    .unwrap()
                    .shutdown_ok
            );
            process.terminate().await;
        });
}

#[test]
#[ignore = "requires MINICORE_AGENT_BIN; real Bash cancellation by exact LoopRef"]
fn e2e_tool_detail_close_does_not_cancel_then_exact_turn_cancel_drains() {
    use minicore_tui::protocol::ToolDataStreamWire as Stream;
    let agent_bin = require_agent_bin();
    let (env, _) = E2eEnvironment::setup();
    enable_bash_profile(&env);
    env._server.enqueue_sse(sse_tool_call_response(
        "bash_cancel",
        "bash",
        &json!({"command": "printf running; sleep 20; printf should-not-run"}).to_string(),
    ));
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(async move {
            let mut process = env.spawn_agent(&agent_bin);
            let mut app = App::new(env.workspace_path.clone());
            dispatch(&mut process, &mut app, AppEvent::Bootstrap)
                .await
                .unwrap();
            pump_until(&mut process, &mut app, |app| {
                app.connection == ConnectionState::Ready
            })
            .await
            .unwrap();
            let session = create_additional_session(
                &mut process,
                &mut app,
                &env.workspace_path,
                "tool cancellation",
            )
            .await;
            dispatch(
                &mut process,
                &mut app,
                AppEvent::SubmitTurn {
                    session_id: session.clone(),
                    text: "run cancellable command".into(),
                },
            )
            .await
            .unwrap();
            pump_until(&mut process, &mut app, |app| {
                app.active_view().is_some_and(|view| {
                    view.tool_presentations.iter().any(|(key, facts)| {
                        key.tool_call_id == "bash_cancel" && facts.command.is_some()
                    })
                })
            })
            .await
            .unwrap();
            let key = app
                .active_view()
                .unwrap()
                .tool_presentations
                .keys()
                .find(|key| key.tool_call_id == "bash_cancel")
                .unwrap()
                .clone();
            let commands = app.open_tool_detail(key.clone());
            dispatch_commands(&mut process, &mut app, commands)
                .await
                .unwrap();
            pump_until(&mut process, &mut app, |app| {
                app.tool_detail()
                    .is_some_and(|detail| detail.streams[Stream::Stdout.index()].next_offset >= 7)
            })
            .await
            .unwrap();
            let close = app.update(AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(
                KeyCode::Esc,
                KeyModifiers::NONE,
            ))));
            assert!(
                close.is_empty(),
                "closing a read-only detail must not cancel"
            );
            assert!(app.active_view().unwrap().live.is_some());
            let commands = app.update(AppEvent::CancelTurn {
                session_id: session.clone(),
            });
            let cancel = commands
                .iter()
                .find_map(|command| match command {
                    AppCommand::Rpc(request) if request.method == "turn.cancel" => Some(request),
                    _ => None,
                })
                .unwrap();
            assert_eq!(cancel.params["session_id"], key.session_id);
            assert_eq!(cancel.params["loop_id"], key.loop_id);
            dispatch_commands(&mut process, &mut app, commands)
                .await
                .unwrap();
            pump_until_with_decode(&mut process, &mut app, |app| {
                app.active_view()
                    .is_some_and(|view| view.live.is_none() && view.transcript.complete)
            })
            .await
            .unwrap();
            let commands = app.open_tool_detail(key);
            dispatch_commands(&mut process, &mut app, commands)
                .await
                .unwrap();
            pump_until_with_decode(&mut process, &mut app, |app| {
                app.tool_detail()
                    .is_some_and(|detail| detail.tab == Stream::Stdout && detail.stream().eof)
            })
            .await
            .unwrap();
            assert_eq!(
                app.tool_detail().unwrap().stream().display_text(),
                "running"
            );
            let command = app.tool_facts().unwrap().command.as_ref().unwrap();
            assert_eq!(
                command.status,
                minicore_tui::protocol::CommandStatusWire::Cancelled
            );
            assert!(command.termination_confirmed);
            assert!(
                drain_shutdown_strict(&mut process, &mut app)
                    .await
                    .unwrap()
                    .shutdown_ok
            );
            process.terminate().await;
        });
}

fn compact_status(app: &App, session_id: &str) -> Option<CompactStatusWire> {
    app.sessions
        .known
        .get(session_id)
        .and_then(|view| view.manual_compact.as_ref())
        .and_then(|compact| compact.result.as_ref())
        .map(|result| result.status)
}

async fn create_compact_session(
    process: &mut RpcProcess,
    app: &mut App,
    workspace: &std::path::Path,
    title: &str,
) -> String {
    dispatch(
        process,
        app,
        AppEvent::CreateSession {
            workspace: workspace.to_string_lossy().into_owned(),
            profile: Some("coding".to_owned()),
            model: Some("deep".to_owned()),
            reasoning: Some(Reasoning::High),
            title: Some(title.to_owned()),
        },
    )
    .await
    .unwrap();
    wait_for_active_session(process, app).await.unwrap()
}

/// Manual compaction on a session with no history is a real `noop`: the Agent
/// answers without any provider call and the TUI returns to idle.
#[test]
#[ignore = "requires MINICORE_AGENT_BIN; runs against self-contained loopback mock HTTP server"]
fn e2e_manual_compact_without_history_is_a_noop() {
    let agent_bin = require_agent_bin();
    let (env, _) = E2eEnvironment::setup();
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
        let session_id = create_compact_session(
            &mut process,
            &mut app,
            &env.workspace_path,
            "Compact noop E2E",
        )
        .await;

        submit_slash_command(&mut process, &mut app, "/compact")
            .await
            .unwrap();
        pump_until(&mut process, &mut app, |a| {
            compact_status(a, &session_id).is_some()
        })
        .await
        .unwrap();

        assert_eq!(
            compact_status(&app, &session_id),
            Some(CompactStatusWire::Noop),
            "an empty session cannot compact any history"
        );
        assert!(!app.sessions.known[&session_id].is_preparing());
        assert_eq!(
            env._server.recorded_requests().len(),
            0,
            "a noop compaction must not call the provider"
        );

        let report = drain_shutdown_strict(&mut process, &mut app).await.unwrap();
        assert!(report.shutdown_ok && report.seen_eof && report.seen_exit);
        process.terminate().await;
    });
}

/// Manual compaction over real persisted history performs one summary utility
/// call and reports `compacted`.
#[test]
#[ignore = "requires MINICORE_AGENT_BIN; runs against self-contained loopback mock HTTP server"]
fn e2e_manual_compact_summarizes_history() {
    let agent_bin = require_agent_bin();
    let (env, _) = E2eEnvironment::setup();
    env._server.enqueue_sse(sse_text_response("first answer"));
    env._server
        .enqueue_sse(sse_text_response("history summary"));

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
        let session_id = create_compact_session(
            &mut process,
            &mut app,
            &env.workspace_path,
            "Compact history E2E",
        )
        .await;

        dispatch(
            &mut process,
            &mut app,
            AppEvent::SubmitTurn {
                session_id: session_id.clone(),
                text: "Say hello before compacting".to_owned(),
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

        submit_slash_command(&mut process, &mut app, "/compact")
            .await
            .unwrap();
        pump_until(&mut process, &mut app, |a| {
            compact_status(a, &session_id) == Some(CompactStatusWire::Compacted)
                && !a.sessions.known[&session_id].is_preparing()
        })
        .await
        .unwrap();

        assert_eq!(
            compact_status(&app, &session_id),
            Some(CompactStatusWire::Compacted),
            "real history is summarized"
        );
        assert!(!app.sessions.known[&session_id].is_preparing());
        let requests = env._server.recorded_requests();
        assert!(
            requests.len() >= 2,
            "compaction adds one summary provider call, got {}",
            requests.len()
        );
        assert!(
            app.sessions.known[&session_id].transcript.complete,
            "compaction preserves readable history"
        );

        let report = drain_shutdown_strict(&mut process, &mut app).await.unwrap();
        assert!(report.shutdown_ok && report.seen_eof && report.seen_exit);
        process.terminate().await;
    });
}

/// A manual compaction whose summary call is still in flight is cancelled by
/// its known operation id: the deferred result must never claim success and
/// the session returns to idle.
#[test]
#[ignore = "requires MINICORE_AGENT_BIN; runs against self-contained loopback mock HTTP server"]
fn e2e_manual_compact_deferred_cancel() {
    let agent_bin = require_agent_bin();
    let (env, _) = E2eEnvironment::setup();
    let gate = Arc::new(AtomicBool::new(false));
    env._server.enqueue_sse(sse_text_response("first answer"));
    env._server
        .enqueue_gated(sse_text_response("late summary"), gate.clone(), None);

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
        let session_id = create_compact_session(
            &mut process,
            &mut app,
            &env.workspace_path,
            "Compact cancel E2E",
        )
        .await;

        dispatch(
            &mut process,
            &mut app,
            AppEvent::SubmitTurn {
                session_id: session_id.clone(),
                text: "Say hello before cancelling".to_owned(),
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

        submit_slash_command(&mut process, &mut app, "/compact")
            .await
            .unwrap();
        // Wait until the summary provider call is in flight (recorded and
        // gated), then cancel by the locally known operation id.
        let deadline = Instant::now() + TIMEOUT;
        while env._server.recorded_requests().len() < 2 && Instant::now() < deadline {
            pump_step(&mut process, &mut app)
                .await
                .expect("pump while waiting for the summary call");
        }
        assert!(
            env._server.recorded_requests().len() >= 2,
            "the summary provider call never arrived"
        );
        assert!(
            app.sessions.known[&session_id].manual_compact.is_some(),
            "the deferred compaction is still owned locally"
        );
        dispatch(
            &mut process,
            &mut app,
            AppEvent::CancelTurn {
                session_id: session_id.clone(),
            },
        )
        .await
        .unwrap();
        gate.store(true, Ordering::Relaxed);

        pump_until(&mut process, &mut app, |a| {
            compact_status(a, &session_id).is_some()
                && !a.sessions.known[&session_id].is_preparing()
        })
        .await
        .unwrap();

        assert_ne!(
            compact_status(&app, &session_id),
            Some(CompactStatusWire::Compacted),
            "a cancelled compaction must not claim it summarized the history"
        );

        let report = drain_shutdown_strict(&mut process, &mut app).await.unwrap();
        assert!(report.shutdown_ok && report.seen_eof && report.seen_exit);
        process.terminate().await;
    });
}

/// Automatic admission prepares before a loop exists. A large first answer
/// pushes the estimated history over the automatic-compaction trigger, so the
/// second submit really runs a summary utility call; the call is gated to keep
/// the preparation observable. The app polls `session.context`, records the
/// operation id and Esc cancels by that exact id.
#[test]
#[ignore = "requires MINICORE_AGENT_BIN; runs against self-contained loopback mock HTTP server"]
fn e2e_automatic_preparation_is_observable_and_cancellable() {
    let agent_bin = require_agent_bin();
    let (env, _) = E2eEnvironment::setup();
    let gate = Arc::new(AtomicBool::new(false));
    env._server
        .enqueue_sse(sse_text_response(&"h".repeat(160 * 1024)));
    env._server
        .enqueue_gated(sse_text_response("prepared summary"), gate.clone(), None);

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
        let session_id = create_compact_session(
            &mut process,
            &mut app,
            &env.workspace_path,
            "Preparation E2E",
        )
        .await;

        dispatch(
            &mut process,
            &mut app,
            AppEvent::SubmitTurn {
                session_id: session_id.clone(),
                text: "fill the history".to_owned(),
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

        // The next submit must prepare: the agent starts a summary utility
        // call before any loop exists, and the gate keeps it in flight.
        dispatch(
            &mut process,
            &mut app,
            AppEvent::SubmitTurn {
                session_id: session_id.clone(),
                text: "prepared turn".to_owned(),
            },
        )
        .await
        .unwrap();
        let deadline = Instant::now() + TIMEOUT;
        while env._server.recorded_requests().len() < 2 && Instant::now() < deadline {
            pump_step(&mut process, &mut app)
                .await
                .expect("pump while the preparation summary is in flight");
        }
        assert!(
            env._server.recorded_requests().len() >= 2,
            "automatic admission never started a summary call"
        );

        // The submission-owned context poll observes the live operation and
        // records its exact identity.
        let deadline = Instant::now() + TIMEOUT;
        while app.sessions.known[&session_id]
            .state
            .as_ref()
            .and_then(|state| state.compaction.as_ref())
            .is_none()
            && Instant::now() < deadline
        {
            pump_step(&mut process, &mut app)
                .await
                .expect("pump while observing the preparation operation");
        }
        let observed = app.sessions.known[&session_id]
            .state
            .as_ref()
            .and_then(|state| state.compaction.clone())
            .expect("the app must observe the preparation operation from session.context");
        assert!(
            !observed.operation_id.is_empty(),
            "the observed preparation carries the real operation id"
        );
        assert!(
            app.sessions.known[&session_id].is_preparing(),
            "the session stays preparing until the preparation settles"
        );

        // Esc marks the submission cancelled; the next context poll that still
        // sees `observed` routes session.compact.cancel by that exact id.
        dispatch(
            &mut process,
            &mut app,
            AppEvent::CancelTurn {
                session_id: session_id.clone(),
            },
        )
        .await
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            pump_step(&mut process, &mut app)
                .await
                .expect("pump while routing the preparation cancel");
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        gate.store(true, Ordering::Relaxed);

        // The cancelled preparation fails the deferred send, and the prompt
        // returns to the composer instead of being lost.
        pump_until(&mut process, &mut app, |a| {
            let view = &a.sessions.known[&session_id];
            view.live.is_none()
                && !view.is_preparing()
                && a.composer.content().contains("prepared turn")
        })
        .await
        .unwrap();
        assert!(
            app.composer.content().contains("prepared turn"),
            "a cancelled preparation must not lose the prompt"
        );

        let report = drain_shutdown_strict(&mut process, &mut app).await.unwrap();
        assert!(report.shutdown_ok && report.seen_eof && report.seen_exit);
        process.terminate().await;
    });
}

// ============================================================================
// D1 real-Agent coverage: browse, startup selection, drafts, command paths.
// ============================================================================

/// D1 (spec §10.1): a closed session stays readable through `session.read`
/// alone after its workspace directory is deleted and its model's provider is
/// unreachable. No `session.open` is issued, so neither the workspace nor the
/// model is required to browse.
#[test]
#[ignore = "requires MINICORE_AGENT_BIN; runs against self-contained loopback mock HTTP server"]
fn e2e_browse_closed_session_survives_deleted_workspace_and_dead_model() {
    let agent_bin = require_agent_bin();
    let (env, _) = E2eEnvironment::setup();
    env._server
        .enqueue_sse(sse_text_response("browsed durable answer."));

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
                title: Some("Browsed after teardown".to_owned()),
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
                text: "the durable prompt".to_owned(),
            },
        )
        .await
        .unwrap();
        wait_turn_landed(&mut process, &mut app, &session_id)
            .await
            .unwrap();

        run_slash_command(&mut process, &mut app, "/close confirm")
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

        // Remove the workspace and point the model at a dead provider.
        std::fs::remove_dir_all(&env.workspace_path).unwrap();
        let config = std::fs::read_to_string(&env.config_path).unwrap();
        let config = config.replace(
            &format!("base_url = \"{}\"", env._server.url()),
            "base_url = \"http://127.0.0.1:1\"",
        );
        std::fs::write(&env.config_path, config).unwrap();
        dispatch(&mut process, &mut app, AppEvent::Reload)
            .await
            .unwrap();
        pump_until(&mut process, &mut app, |a| {
            !a.pending_requests.values().any(|kind| {
                matches!(
                    kind,
                    RequestKind::Reload { .. }
                        | RequestKind::ReloadModels { .. }
                        | RequestKind::ReloadProfiles { .. }
                        | RequestKind::ReloadSessions { .. }
                )
            })
        })
        .await
        .unwrap();
        assert!(
            !std::path::Path::new(&env.workspace_path).exists(),
            "the workspace is really gone"
        );

        // Panel: the only row is the closed session in this workspace.
        dispatch(&mut process, &mut app, AppEvent::OpenSessionSelector)
            .await
            .unwrap();
        pump_until(&mut process, &mut app, |a| {
            !a.pending_requests.values().any(|kind| {
                matches!(
                    kind,
                    RequestKind::RefreshSessions { .. } | RequestKind::ListSessions
                )
            }) && matches!(&a.dock, Dock::SessionSelector(state) if state
                .selected_session_id
                .as_deref()
                == Some(session_id.as_str()))
        })
        .await
        .unwrap();

        dispatch(
            &mut process,
            &mut app,
            AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(
                KeyCode::Char('b'),
                KeyModifiers::CONTROL,
            ))),
        )
        .await
        .unwrap();
        pump_until(&mut process, &mut app, |a| {
            a.sessions.known.get(&session_id).is_some_and(|view| {
                view.browsing
                    && view
                        .transcript
                        .window
                        .items()
                        .any(|(_, entry)| matches!(entry.as_ref(), TranscriptBlock::User(_)))
            })
        })
        .await
        .unwrap();

        assert!(
            !app.pending_requests.values().any(|kind| {
                matches!(kind, RequestKind::OpenSession { session_id: pending, .. } if pending == &session_id)
            }),
            "browse must not open the session"
        );
        let view = &app.sessions.known[&session_id];
        assert!(view.browsing, "the view stays read-only");
        assert_eq!(view.info.model, "deep");

        let rep = drain_shutdown_strict(&mut process, &mut app).await.unwrap();
        assert!(rep.shutdown_ok && rep.seen_eof && rep.seen_exit);
        process.terminate().await;
    });
}

/// D1 (spec §6.1, §10.2): `--session <id>` opens exactly that id and
/// `--continue` matches only the current workspace; neither path ever sends a
/// prompt. A `--continue` miss falls back to the selector without guessing.
#[test]
#[ignore = "requires MINICORE_AGENT_BIN; runs against self-contained loopback mock HTTP server"]
fn e2e_startup_selection_opens_without_auto_prompt() {
    let agent_bin = require_agent_bin();
    let (env, _) = E2eEnvironment::setup();
    let other_workspace = env.temp_dir.join("other_workspace");
    std::fs::create_dir_all(&other_workspace).unwrap();

    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async move {
        // Seed the store with one session per workspace.
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
        let here =
            create_compact_session(&mut process, &mut app, &env.workspace_path, "Startup here")
                .await;
        let elsewhere = create_compact_session(
            &mut process,
            &mut app,
            &other_workspace,
            "Startup elsewhere",
        )
        .await;
        let rep = drain_shutdown_strict(&mut process, &mut app).await.unwrap();
        assert!(rep.shutdown_ok && rep.seen_eof && rep.seen_exit);
        process.terminate().await;

        // `--session <id>`: the exact id, no selector, no prompt.
        let mut process = env.spawn_agent(&agent_bin);
        let prefs = CliPrefs {
            startup_session: Some(StartupSession::Exact(elsewhere.clone())),
            ..CliPrefs::default()
        };
        let mut app = App::with_cli_prefs(env.workspace_path.clone(), prefs);
        dispatch(&mut process, &mut app, AppEvent::Bootstrap)
            .await
            .unwrap();
        pump_until(&mut process, &mut app, |a| {
            a.sessions.active.as_deref() == Some(elsewhere.as_str())
                && a.connection == ConnectionState::Ready
        })
        .await
        .unwrap();
        assert!(!matches!(app.dock, Dock::SessionSelector(_)));
        assert!(
            !app.pending_requests.values().any(|kind| matches!(
                kind,
                RequestKind::SendTurn { .. } | RequestKind::SteerTurn { .. }
            )),
            "--session never sends a prompt"
        );
        assert!(
            env._server.recorded_requests().is_empty(),
            "--session never reaches the provider"
        );
        let rep = drain_shutdown_strict(&mut process, &mut app).await.unwrap();
        assert!(rep.shutdown_ok && rep.seen_eof && rep.seen_exit);
        process.terminate().await;

        // `--continue` in this workspace: the session that lives here, never
        // the newer one in the other project.
        let mut process = env.spawn_agent(&agent_bin);
        let prefs = CliPrefs {
            startup_session: Some(StartupSession::ContinueCurrentWorkspace),
            ..CliPrefs::default()
        };
        let mut app = App::with_cli_prefs(env.workspace_path.clone(), prefs);
        dispatch(&mut process, &mut app, AppEvent::Bootstrap)
            .await
            .unwrap();
        pump_until(&mut process, &mut app, |a| {
            a.sessions.active.as_deref() == Some(here.as_str())
                && a.connection == ConnectionState::Ready
        })
        .await
        .unwrap();
        assert!(
            !app.pending_requests.values().any(|kind| matches!(
                kind,
                RequestKind::SendTurn { .. } | RequestKind::SteerTurn { .. }
            )),
            "--continue never sends a prompt"
        );
        assert!(env._server.recorded_requests().is_empty());
        let rep = drain_shutdown_strict(&mut process, &mut app).await.unwrap();
        assert!(rep.shutdown_ok && rep.seen_eof && rep.seen_exit);
        process.terminate().await;

        // A `--continue` miss opens the selector instead of guessing.
        let empty_workspace = env.temp_dir.join("empty_workspace");
        std::fs::create_dir_all(&empty_workspace).unwrap();
        let mut process = env.spawn_agent(&agent_bin);
        let prefs = CliPrefs {
            startup_session: Some(StartupSession::ContinueCurrentWorkspace),
            ..CliPrefs::default()
        };
        let mut app = App::with_cli_prefs(empty_workspace, prefs);
        dispatch(&mut process, &mut app, AppEvent::Bootstrap)
            .await
            .unwrap();
        pump_until(&mut process, &mut app, |a| {
            a.connection == ConnectionState::Ready
                && matches!(a.dock, Dock::SessionSelector(_))
                && !a.pending_requests.values().any(|kind| {
                    matches!(
                        kind,
                        RequestKind::RefreshSessions { .. } | RequestKind::ListSessions
                    )
                })
        })
        .await
        .unwrap();
        assert!(
            app.sessions.active.is_none(),
            "no cross-project guess opens"
        );
        assert!(
            !app.pending_requests.values().any(|kind| {
                matches!(
                    kind,
                    RequestKind::OpenSession { .. } | RequestKind::SendTurn { .. }
                )
            }),
            "a miss opens nothing"
        );
        assert!(env._server.recorded_requests().is_empty());
        let rep = drain_shutdown_strict(&mut process, &mut app).await.unwrap();
        assert!(rep.shutdown_ok && rep.seen_eof && rep.seen_exit);
        process.terminate().await;
    });
}

/// D1 (spec §10.3): switching sessions keeps each draft (text, cursor, paste
/// markers) and leaves a running loop alive in the background.
#[test]
#[ignore = "requires MINICORE_AGENT_BIN; runs against self-contained loopback mock HTTP server"]
fn e2e_session_switch_keeps_drafts_and_running_background_loop() {
    let agent_bin = require_agent_bin();
    let (env, _) = E2eEnvironment::setup();
    let gate = Arc::new(AtomicBool::new(false));
    env._server
        .enqueue_gated(sse_text_response("background answer."), gate.clone(), None);

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

        let first =
            create_compact_session(&mut process, &mut app, &env.workspace_path, "Draft A").await;
        dispatch(
            &mut process,
            &mut app,
            AppEvent::SubmitTurn {
                session_id: first.clone(),
                text: "start the background turn".to_owned(),
            },
        )
        .await
        .unwrap();
        pump_until(&mut process, &mut app, |a| {
            a.sessions
                .known
                .get(&first)
                .is_some_and(|view| view.live.is_some())
                && !env._server.recorded_requests().is_empty()
        })
        .await
        .unwrap();

        // Draft in the running session, then create and draft in a second one.
        type_draft(&mut process, &mut app, "alpha draft")
            .await
            .unwrap();
        assert_eq!(app.composer().content(), "alpha draft");

        let second =
            create_additional_session(&mut process, &mut app, &env.workspace_path, "Draft B").await;
        assert_eq!(
            app.composer().content(),
            "",
            "a new session starts with an empty draft"
        );
        type_draft(&mut process, &mut app, "beta draft")
            .await
            .unwrap();
        assert_eq!(app.composer().content(), "beta draft");

        // Switching back restores the first session's draft and leaves its
        // loop running in the background.
        dispatch(
            &mut process,
            &mut app,
            AppEvent::OpenSession {
                session_id: first.clone(),
            },
        )
        .await
        .unwrap();
        pump_until(&mut process, &mut app, |a| {
            a.sessions.active.as_deref() == Some(first.as_str())
        })
        .await
        .unwrap();
        assert_eq!(
            app.composer().content(),
            "alpha draft",
            "the first session's draft follows the switch"
        );
        assert!(
            app.sessions.known[&first].live.is_some(),
            "the background loop survives the switch"
        );

        // And the second draft is still its own.
        dispatch(
            &mut process,
            &mut app,
            AppEvent::OpenSession {
                session_id: second.clone(),
            },
        )
        .await
        .unwrap();
        pump_until(&mut process, &mut app, |a| {
            a.sessions.active.as_deref() == Some(second.as_str())
        })
        .await
        .unwrap();
        assert_eq!(app.composer().content(), "beta draft");

        // Release the provider and let the background turn land.
        gate.store(true, Ordering::Relaxed);
        pump_until(&mut process, &mut app, |a| {
            a.sessions.known.get(&first).is_some_and(|view| {
                view.live.is_none()
                    && view
                        .transcript
                        .window
                        .items()
                        .any(|(_, entry)| matches!(entry.as_ref(), TranscriptBlock::Assistant(_)))
            })
        })
        .await
        .unwrap();

        let rep = drain_shutdown_strict(&mut process, &mut app).await.unwrap();
        assert!(rep.shutdown_ok && rep.seen_eof && rep.seen_exit);
        process.terminate().await;
    });
}

/// D1 (spec §10.4): `/new` creates quickly in this workspace with the recent
/// explicit configuration, `/new form` still opens the custom form, and
/// `/rename`, `/close` and `/delete` work end to end against the real Agent.
#[test]
#[ignore = "requires MINICORE_AGENT_BIN; runs against self-contained loopback mock HTTP server"]
fn e2e_new_form_rename_close_delete_commands() {
    let agent_bin = require_agent_bin();
    let (env, _) = E2eEnvironment::setup();

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

        // `/new` without an active session: quick create, no form.
        run_slash_command(&mut process, &mut app, "/new")
            .await
            .unwrap();
        let session_id = wait_for_active_session(&mut process, &mut app)
            .await
            .unwrap();
        assert!(app.new_session().is_none(), "/new never opens the form");
        let view = &app.sessions.known[&session_id];
        assert_eq!(
            view.info.workspace,
            env.workspace_path.to_string_lossy()
        );
        assert_eq!(view.info.model, "deep");
        assert_eq!(view.info.profile, "coding");

        // `/new form` still reaches the custom form.
        run_slash_command(&mut process, &mut app, "/new form")
            .await
            .unwrap();
        assert!(app.new_session().is_some(), "/new form opens the custom form");
        dispatch(
            &mut process,
            &mut app,
            AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(
                KeyCode::Esc,
                KeyModifiers::empty(),
            ))),
        )
        .await
        .unwrap();
        assert!(!matches!(app.dock, Dock::NewSession(_)));

        // `/rename <title>` through the safe mutation path.
        run_slash_command(&mut process, &mut app, "/rename E2E renamed")
            .await
            .unwrap();
        pump_until(&mut process, &mut app, |a| {
            a.sessions
                .known
                .get(&session_id)
                .is_some_and(|view| view.info.title.as_deref() == Some("E2E renamed"))
        })
        .await
        .unwrap();

        // `/close` keeps the view; `/delete` needs the closed session and an
        // explicit confirm.
        run_slash_command(&mut process, &mut app, "/close confirm")
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

        dispatch(&mut process, &mut app, AppEvent::OpenSessionSelector)
            .await
            .unwrap();
        pump_until(&mut process, &mut app, |a| {
            !a.pending_requests.values().any(|kind| {
                matches!(
                    kind,
                    RequestKind::RefreshSessions { .. } | RequestKind::ListSessions
                )
            })
        })
        .await
        .unwrap();
        // The close removed the row from the last catalog snapshot; F5 makes
        // the closed session selectable again (the panel's own refresh).
        dispatch(
            &mut process,
            &mut app,
            AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(
                KeyCode::F(5),
                KeyModifiers::empty(),
            ))),
        )
        .await
        .unwrap();
        pump_until(&mut process, &mut app, |a| {
            !a.pending_requests.values().any(|kind| {
                matches!(
                    kind,
                    RequestKind::RefreshSessions { .. } | RequestKind::ListSessions
                )
            }) && matches!(&a.dock, Dock::SessionSelector(state) if state
                .selected_session_id
                .as_deref()
                == Some(session_id.as_str()))
        })
        .await
        .unwrap();

        run_slash_command(&mut process, &mut app, "/delete")
            .await
            .unwrap();
        assert!(
            !app.pending_requests.values().any(|kind| {
                matches!(kind, RequestKind::DeleteSession { session_id: pending } if pending == &session_id)
            }),
            "delete waits for the explicit confirm"
        );
        run_slash_command(&mut process, &mut app, "/delete confirm")
            .await
            .unwrap();
        pump_until(&mut process, &mut app, |a| session_absent(a, &session_id))
            .await
            .unwrap();

        // The rename survives the reloaded catalog and the row is gone.
        run_slash_command(&mut process, &mut app, "/sessions")
            .await
            .unwrap();
        pump_until(&mut process, &mut app, |a| {
            !a.pending_requests.values().any(|kind| {
                matches!(
                    kind,
                    RequestKind::RefreshSessions { .. } | RequestKind::ListSessions
                )
            }) && !a
                .sessions
                .list
                .iter()
                .any(|session| session.session_id == session_id)
        })
        .await
        .unwrap();
        match &app.dock {
            Dock::SessionSelector(state) => assert!(matches!(
                state.mode,
                SessionPanelMode::Browse
            )),
            dock => panic!("expected the session selector, got {dock:?}"),
        }

        let rep = drain_shutdown_strict(&mut process, &mut app).await.unwrap();
        assert!(rep.shutdown_ok && rep.seen_eof && rep.seen_exit);
        process.terminate().await;
    });
}

/// D2 (spec §17.1/§17.4): a real Agent read chain feeds both the pinned
/// full-session search and the local export writer. The export is refused
/// until the existing target is confirmed, commits atomically, keeps its
/// unsaved live turn behind the explicit choice, and removes its temp file
/// when cancelled.
#[test]
#[ignore = "requires MINICORE_AGENT_BIN; runs against self-contained loopback mock HTTP server"]
fn e2e_full_search_and_export_real_agent_chain() {
    let agent_bin = require_agent_bin();
    let (env, _) = E2eEnvironment::setup();
    // Eleven completed turns produce 22+ saved history items, forcing the
    // real session.read search/export chains across the 20-item page limit.
    // The combined saved history exceeds one read page, and the final answer
    // contains a multi-byte needle in the streamed body.
    for index in 0..11 {
        let text = if index == 10 {
            format!(
                "{}café ☕ cross-chunk needle{}",
                "x".repeat(12_000),
                "y".repeat(12_000)
            )
        } else {
            format!("durable answer {index}")
        };
        env._server.enqueue_sse(sse_text_response(&text));
    }
    let gate = Arc::new(AtomicBool::new(false));
    let dir = std::env::temp_dir().join(format!(
        "mctui-e2e-export-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let target = dir.join("chat.md");

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
        let session =
            create_compact_session(&mut process, &mut app, &env.workspace_path, "Export A").await;
        for index in 0..11 {
            dispatch(
                &mut process,
                &mut app,
                AppEvent::SubmitTurn {
                    session_id: session.clone(),
                    text: format!("durable question {index}"),
                },
            )
            .await
            .unwrap();
            let expected_items = (index + 1) * 2;
            pump_until(&mut process, &mut app, |a| {
                a.sessions.known.get(&session).is_some_and(|view| {
                    view.live.is_none() && view.transcript.total >= expected_items
                })
            })
            .await
            .unwrap_or_else(|error| {
                let view = app.sessions.known.get(&session).unwrap();
                panic!(
                    "turn {index} expected {expected_items} items, got total={} loaded={} complete={} live={:?}: {error}",
                    view.transcript.total,
                    view.transcript.loaded_count,
                    view.transcript.complete,
                    view.live.as_ref().map(|live| &live.reference)
                )
            });
        }
        pump_until(&mut process, &mut app, |a| {
            a.sessions
                .known
                .get(&session)
                .is_some_and(|view| view.transcript.total >= 22)
        })
        .await
        .unwrap();
        env._server
            .enqueue_gated(sse_text_response("live unsaved body"), gate.clone(), None);

        // The explicit full-session scan runs on the real read chain and
        // finds the literal in the durable body.
        run_slash_command(&mut process, &mut app, "/search full needle")
            .await
            .unwrap();
        pump_until_with_decode(&mut process, &mut app, |a| {
            a.search_panel()
                .is_some_and(|panel| !panel.matches.is_empty())
        })
        .await
        .unwrap();
        let panel = app.search_panel().expect("search panel");
        assert_eq!(panel.matches.len(), 1, "{:?}", panel.matches);
        assert!(panel.coverage.scanned_items > 20, "{:?}", panel.coverage);
        assert!(panel.coverage.complete, "{:?}", panel.coverage);
        press_key(&mut process, &mut app, KeyCode::Esc)
            .await
            .unwrap();

        // A live turn that is not in saved history: appended only after the
        // explicit Ctrl+N choice.
        dispatch(
            &mut process,
            &mut app,
            AppEvent::SubmitTurn {
                session_id: session.clone(),
                text: "unsaved question".to_owned(),
            },
        )
        .await
        .unwrap();
        pump_until(&mut process, &mut app, |a| {
            a.sessions
                .known
                .get(&session)
                .is_some_and(|view| view.live.is_some())
        })
        .await
        .unwrap();

        // The explicit Ctrl+N choice appends the live turn; without it only
        // saved history is written.
        run_slash_command(
            &mut process,
            &mut app,
            &format!("/export {}", target.display()),
        )
        .await
        .unwrap();
        press_ctrl_key(&mut process, &mut app, 'n').await.unwrap();
        press_key(&mut process, &mut app, KeyCode::Enter)
            .await
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let commands = drain_export_jobs(&mut app).await.unwrap();
            dispatch_commands(&mut process, &mut app, commands)
                .await
                .unwrap();
            if app
                .export_form()
                .is_some_and(|form| form.phase == ExportPhase::Done)
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "export did not finish: phase={:?} notice={:?} running={} owner={} pending_requests={:?} decode_pending={} dock={:?}",
                app.export_form().map(|form| form.phase),
                app.export_form().and_then(|form| form.notice.clone()),
                app.export_running(),
                app.export_owner_busy(),
                app.pending_requests
                    .iter()
                    .map(|(id, kind)| format!("{}:{kind:?}", id.0))
                    .collect::<Vec<_>>(),
                app.pending_decode_request().is_some(),
                app.dock
            );
            pump_step(&mut process, &mut app).await.unwrap();
            let commands = drain_pending_decode(&mut app);
            dispatch_commands(&mut process, &mut app, commands)
                .await
                .unwrap();
        }
        let written = std::fs::read_to_string(&target).expect("exported file");
        assert!(written.contains("Conversation export"), "{written}");
        assert!(written.contains("café ☕ cross-chunk needle"), "{written}");
        assert!(written.contains("saved history plus"), "{written}");
        assert!(written.contains("Unconfirmed live turn"), "{written}");
        assert!(written.contains("unsaved question"), "{written}");
        assert!(written.contains("unconfirmed:"), "{written}");
        let leftovers: Vec<String> = std::fs::read_dir(&dir)
            .expect("readable scratch dir")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name != "chat.md")
            .collect();
        assert!(leftovers.is_empty(), "temp files left: {leftovers:?}");
        gate.store(true, Ordering::Relaxed);
        let rep = drain_shutdown_strict(&mut process, &mut app).await.unwrap();
        assert!(rep.shutdown_ok && rep.seen_eof && rep.seen_exit);
        process.terminate().await;
        let _ = std::fs::remove_dir_all(&dir);
    });
}

/// D3 (spec §12.4/REF-47): the direct editor owns only its temporary draft
/// file while the real Agent RPC reader and background turn continue to make
/// progress. The return is routed through the same session/revision fence as
/// production and leaves no workspace file behind.
#[test]
#[ignore = "requires MINICORE_AGENT_BIN; runs against self-contained loopback mock HTTP server"]
fn e2e_external_editor_coexists_with_a_real_background_turn() {
    let agent_bin = require_agent_bin();
    let (env, _) = E2eEnvironment::setup();
    env._server
        .enqueue_chunked_sse(sse_text_response("background answer"));
    let editor_scratch = env.temp_dir.join("editor-scratch");
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
        let session =
            create_compact_session(&mut process, &mut app, &env.workspace_path, "Editor A").await;
        let editor = minicore_tui::config::EditorConfig {
            executable: "/bin/sh".to_owned(),
            args: vec![
                "-c".to_owned(),
                format!(
                    "sleep 0.30; printf 'editor draft' > \"$1\"; test ! -e '{}'",
                    editor_scratch.display()
                ),
                "editor".to_owned(),
            ],
        };
        let config = minicore_tui::config::TuiConfig {
            editor: Some(editor),
            ..minicore_tui::config::TuiConfig::default()
        };
        app.apply_tui_config(config);

        dispatch(
            &mut process,
            &mut app,
            AppEvent::SubmitTurn {
                session_id: session.clone(),
                text: "background question".to_owned(),
            },
        )
        .await
        .unwrap();
        wait_for_request0_and_wait_turn(&env, &mut process, &mut app, &session)
            .await
            .unwrap();

        app.composer_mut().set_text("draft before editor");
        run_slash_command(&mut process, &mut app, "/editor")
            .await
            .unwrap();
        assert!(app.editor_active());
        pump_until(&mut process, &mut app, |a| {
            !a.editor_active()
                && a.sessions
                    .known
                    .get(&session)
                    .is_some_and(|view| view.live.is_none() && view.transcript.total >= 2)
        })
        .await
        .unwrap();
        assert_eq!(
            app.composer().content(),
            "editor draft",
            "editor notices: {:?}",
            app.notices
                .iter()
                .map(|notice| &notice.text)
                .collect::<Vec<_>>()
        );
        assert!(
            !editor_scratch.exists(),
            "editor used a workspace path instead of the private temp file"
        );
        let view = app.sessions.known.get(&session).expect("session view");
        assert!(view.transcript.total >= 2, "background turn did not land");

        let rep = drain_shutdown_strict(&mut process, &mut app).await.unwrap();
        assert!(rep.shutdown_ok && rep.seen_eof && rep.seen_exit);
        process.terminate().await;
    });
}
