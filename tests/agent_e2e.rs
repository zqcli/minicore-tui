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

use minicore_tui::app::{App, ConnectionState, RequestKind};
use minicore_tui::command::AppCommand;
use minicore_tui::event::{AppEvent, RpcEvent};
use minicore_tui::protocol::{
    CancelReasonWire, HistoryItemWire, IncomingFrame, LoopOutcomeWire, TurnPersistenceWire,
    TurnResultViewWire, UserMessageKindWire,
};
use minicore_tui::rpc::RpcProcess;
use minicore_tui::state::session::ConfigUpdateState;
use minicore_tui::state::turn::PendingSteerState;
use minicore_tui::theme::ThemeKind;
use serde_json::json;

const TIMEOUT: Duration = Duration::from_secs(30);
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
        });
    }

    fn enqueue_sse_with_model(&self, sse_body: String, expected_model: &str) {
        self.responses.lock().unwrap().push_back(MockResponse {
            body: sse_body,
            gate: None,
            expected_model: Some(expected_model.to_string()),
        });
    }

    fn enqueue_gated(&self, sse_body: String, gate: Arc<AtomicBool>, expected_model: Option<&str>) {
        self.responses.lock().unwrap().push_back(MockResponse {
            body: sse_body,
            gate: Some(gate),
            expected_model: expected_model.map(|s| s.to_string()),
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

    let _ = stream.write_all(http_response.as_bytes());
    let _ = stream.flush();
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
    let event = tokio::time::timeout(Duration::from_secs(10), process.recv())
        .await
        .map_err(|_| "recv timed out")?
        .ok_or("agent process stream ended")?;

    let commands = app.update(AppEvent::Rpc(event));
    for command in commands {
        match command {
            AppCommand::Rpc(req) => {
                process.send(req).await.map_err(|e| e.to_string())?;
            }
            AppCommand::KillChild => process.kill_child(),
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
    let deadline = Instant::now() + Duration::from_secs(15);
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

    let deadline = Instant::now() + Duration::from_secs(6);
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
        pump_until(&mut process, &mut app, |a| a.sessions.active.is_some())
            .await
            .unwrap();
        let session_id = app.sessions.active.clone().unwrap();

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
        pump_until(&mut process, &mut app, |a| a.sessions.active.is_some())
            .await
            .unwrap();
        let session_id = app.sessions.active.clone().unwrap();

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
        pump_until(&mut process, &mut app, |a| a.sessions.active.is_some())
            .await
            .unwrap();
        let session_id = app.sessions.active.clone().unwrap();

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
        pump_until(&mut process, &mut app, |a| a.sessions.active.is_some())
            .await
            .unwrap();
        let session_id = app.sessions.active.clone().unwrap();

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
        pump_until(&mut process, &mut app, |a| a.sessions.active.is_some())
            .await
            .unwrap();
        let session_id = app.sessions.active.clone().unwrap();

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
