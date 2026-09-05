//! Test fixture builders that construct app state exclusively through
//! `App::update` with real wire-shaped events (no production back doors).
//! Used by the component and snapshot tests for the Phase 3 renderer.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::app::{App, ConnectionState};
use crate::command::AppCommand;
use crate::event::{AppEvent, RpcEvent};
use crate::protocol::{
    IncomingFrame, OutgoingRequest, RpcError as WireError, RpcNotification, RpcResponse,
};
use crate::theme::ThemeKind;

fn fixed_now() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_800_000_000)
}

pub fn take_requests(commands: Vec<AppCommand>) -> Vec<OutgoingRequest> {
    commands
        .into_iter()
        .filter_map(|command| match command {
            AppCommand::Rpc(request) => Some(request),
            _ => None,
        })
        .collect()
}

pub fn respond(app: &mut App, request: &OutgoingRequest, result: Value) -> Vec<AppCommand> {
    app.update(AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Response(
        RpcResponse {
            id: request.id,
            result: Some(result),
            error: None,
        },
    ))))
}

pub fn respond_rpc_error(
    app: &mut App,
    request: &OutgoingRequest,
    code: i64,
    message: &str,
) -> Vec<AppCommand> {
    app.update(AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Response(
        RpcResponse {
            id: request.id,
            result: None,
            error: Some(WireError {
                code,
                message: message.to_owned(),
                data: None,
            }),
        },
    ))))
}

fn session_info(session_id: &str, title: Option<&str>, reasoning: &str) -> Value {
    json!({
        "session_id": session_id,
        "title": title,
        "profile": "coding",
        "workspace": "/project",
        "model": "deep",
        "reasoning": reasoning,
        "loaded": true,
        "created_at": "2026-01-02T03:04:05.006Z",
        "updated_at": "2026-01-02T03:04:05.006Z"
    })
}

fn state_json(session_id: &str, status: &str) -> Value {
    json!({
        "session_id": session_id,
        "status": status,
        "active_loop": null,
        "block_reason": null
    })
}

fn page_json(items: Vec<Value>, complete: bool) -> Value {
    let next_offset = if complete { None } else { Some(items.len()) };
    let total = items.len();
    json!({
        "items": items,
        "next_offset": next_offset,
        "total": total
    })
}

fn user_entry(index: usize, loop_id: &str, text: &str) -> Value {
    json!({
        "index": index,
        "item": {
            "type": "user",
            "data": {
                "loop_id": loop_id,
                "kind": "prompt",
                "text": text
            }
        }
    })
}

fn assistant_entry_with(
    index: usize,
    loop_id: &str,
    text: &str,
    reasoning: Option<&str>,
    tool_calls: Value,
) -> Value {
    json!({
        "index": index,
        "item": {
            "type": "assistant",
            "data": {
                "loop_id": loop_id,
                "request_index": 0,
                "model": "deep",
                "reasoning_level": "high",
                "text": text,
                "reasoning": reasoning.unwrap_or(""),
                "tool_calls": tool_calls,
                "usage": {},
                "finish_reason": "stop"
            }
        }
    })
}

fn tool_result_entry(
    index: usize,
    loop_id: &str,
    call_id: &str,
    name: &str,
    outcome: &str,
    content: &str,
) -> Value {
    json!({
        "index": index,
        "item": {
            "type": "tool_result",
            "data": {
                "loop_id": loop_id,
                "request_index": 0,
                "tool_call_id": call_id,
                "tool_name": name,
                "outcome": outcome,
                "content": content
            }
        }
    })
}

/// An empty app in `Starting` with the given palette.
pub fn fresh(theme: ThemeKind) -> App {
    let mut app = App::new(PathBuf::from("/project"));
    app.update(AppEvent::SetTheme(theme));
    app
}

/// Bootstraps to Ready and opens `session_id` with `items` delivered on
/// the first history chain.
fn open_with(
    theme: ThemeKind,
    session_id: &str,
    title: Option<&str>,
    reasoning: &str,
    items: Vec<Value>,
) -> App {
    let mut app = fresh(theme);
    let requests = take_requests(app.update(AppEvent::Bootstrap));
    for request in &requests {
        let result = match request.method {
            "agent.ping" => json!({"version": "0.3.0"}),
            "model.list" => json!({"models": []}),
            "profile.list" => json!({"profiles": []}),
            "session.list" => json!({"sessions": []}),
            other => panic!("unexpected bootstrap request: {other}"),
        };
        take_requests(respond(&mut app, request, result));
    }
    let open = take_requests(app.update(AppEvent::OpenSession {
        session_id: session_id.into(),
    }));
    let requests = take_requests(respond(
        &mut app,
        &open[0],
        json!({"session": session_info(session_id, title, reasoning)}),
    ));
    let state = requests
        .iter()
        .find(|r| r.method == "session.state")
        .unwrap();
    let history = requests
        .iter()
        .find(|r| r.method == "session.history")
        .unwrap();
    take_requests(respond(&mut app, state, state_json(session_id, "idle")));
    let commands = respond(&mut app, history, page_json(items, true));
    assert!(take_requests(commands).is_empty());
    app
}

/// Bootstraps to Ready with one empty session open and complete.
pub fn open_empty(theme: ThemeKind, session_id: &str, title: Option<&str>, reasoning: &str) -> App {
    open_with(theme, session_id, title, reasoning, Vec::new())
}

pub const CHAT_MARKDOWN: &str = "# Heading\n\nA paragraph with **bold**, *italic*, and `code`.\n\n- first item\n- second item\n\n> quoted wisdom\n\n```\nfn hello() {\n    println!(\"hi\");\n}\n```\n\n[link](https://example.com) at the end.\n\n---\n";

/// A session with a durable user/assistant exchange (markdown).
pub fn chat(theme: ThemeKind) -> App {
    let items = vec![
        user_entry(0, "loop_1", "Hello **world** with `code`."),
        assistant_entry_with(1, "loop_1", CHAT_MARKDOWN, None, json!([])),
    ];
    open_with(theme, "ses_1", Some("Task"), "high", items)
}

/// A session whose assistant carries a reasoning run (plus text ordering).
pub fn chat_with_reasoning(theme: ThemeKind) -> App {
    let items = vec![
        user_entry(0, "loop_1", "hello"),
        assistant_entry_with(
            1,
            "loop_1",
            "answer text",
            Some("carefully thinking out loud"),
            json!([]),
        ),
    ];
    open_with(theme, "ses_1", Some("Task"), "high", items)
}

/// A session with durable tool cards in success/denied/failed states and an
/// expanded preview with more than 40 lines (toggled via `ToggleTools`).
pub fn tools(theme: ThemeKind) -> App {
    let big_result = (0..60)
        .map(|i| format!("line {i:02} of a long file"))
        .collect::<Vec<_>>()
        .join("\n");
    let items = vec![
        user_entry(0, "loop_1", "run the tools"),
        assistant_entry_with(
            1,
            "loop_1",
            "using tools now",
            None,
            json!([
                {"tool_call_id": "call-1", "name": "read", "call_index": 0},
                {"tool_call_id": "call-2", "name": "bash", "call_index": 1},
                {"tool_call_id": "call-3", "name": "edit", "call_index": 2}
            ]),
        ),
        tool_result_entry(2, "loop_1", "call-1", "read", "success", &big_result),
        tool_result_entry(3, "loop_1", "call-2", "bash", "denied", "not allowed"),
        tool_result_entry(4, "loop_1", "call-3", "edit", "failed", "edit rejected"),
    ];
    let mut app = open_with(theme, "ses_1", Some("Task"), "high", items);
    app.update(AppEvent::ToggleTools {
        session_id: "ses_1".into(),
    });
    app
}

/// A running live turn with text, reasoning, a running tool, and an event gap.
pub fn live_turn(theme: ThemeKind) -> App {
    let mut app = open_empty(theme, "ses_1", Some("Task"), "high");
    let send = take_requests(app.update(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "stream me".into(),
    }));
    assert_eq!(send.len(), 1);
    let started = serde_json::from_value(json!({
        "type": "turn_started",
        "data": {"turn": {"session_id": "ses_1", "loop_id": "loop_live"},
                 "meta": {"session_id": "ses_1", "dropped_before": 0}}
    }))
    .unwrap();
    app.update(AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Notification(
        RpcNotification::AgentEvent(started),
    ))));
    let req_started = serde_json::from_value(json!({
        "type": "request_started",
        "data": {
            "turn": {"session_id": "ses_1", "loop_id": "loop_live"},
            "request_index": 0,
            "config_revision": 0,
            "model": "deep",
            "reasoning": "high",
            "meta": {"session_id": "ses_1", "dropped_before": 0}
        }
    }))
    .unwrap();
    app.update(AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Notification(
        RpcNotification::AgentEvent(req_started),
    ))));
    for (channel, delta) in [
        ("text", "Streaming "),
        ("text", "content"),
        ("reasoning", "thinking "),
        ("reasoning", "hard"),
    ] {
        let output = serde_json::from_value(json!({
            "type": "output_delta",
            "data": {
                "turn": {"session_id": "ses_1", "loop_id": "loop_live"},
                "request_index": 0,
                "channel": channel,
                "delta": delta,
                "meta": {"session_id": "ses_1", "dropped_before": 0}
            }
        }))
        .unwrap();
        app.update(AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Notification(
            RpcNotification::AgentEvent(output),
        ))));
    }
    for event in [
        json!({"type": "tool_started", "data": {
            "turn": {"session_id": "ses_1", "loop_id": "loop_live"},
            "request_index": 0,
            "tool_call_id": "c1", "tool_name": "read",
            "meta": {"session_id": "ses_1", "dropped_before": 0}}}),
        json!({"type": "tool_progress", "data": {
            "turn": {"session_id": "ses_1", "loop_id": "loop_live"},
            "request_index": 0,
            "tool_call_id": "c1",
            "progress": {"message": "reading…", "completed": null, "total": null},
            "meta": {"session_id": "ses_1", "dropped_before": 0}}}),
        json!({"type": "output_delta", "data": {
            "turn": {"session_id": "ses_1", "loop_id": "loop_live"},
            "request_index": 0,
            "channel": "text", "delta": "more",
            "meta": {"session_id": "ses_1", "dropped_before": 2}}}),
    ] {
        let event = serde_json::from_value(event).unwrap();
        app.update(AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Notification(
            RpcNotification::AgentEvent(event),
        ))));
    }
    app
}

/// Sets deterministic clock functions so notice timestamps and spinner
/// frames don't drift across runs.
#[allow(dead_code)]
pub fn freeze_time(app: &mut App) {
    app.now = || UNIX_EPOCH + Duration::from_secs(1_700_000_000);
}

/// A ready app populated with standard catalogs.
pub fn ready_catalog(
    theme: ThemeKind,
    models: Vec<Value>,
    profiles: Vec<Value>,
    sessions: Vec<Value>,
) -> App {
    let mut app = fresh(theme);
    app.now = fixed_now;
    let requests = take_requests(app.update(AppEvent::Bootstrap));
    for request in &requests {
        let result = match request.method {
            "agent.ping" => json!({"version": "0.3.0"}),
            "model.list" => json!({"models": models.clone()}),
            "profile.list" => json!({"profiles": profiles.clone()}),
            "session.list" => json!({"sessions": sessions.clone()}),
            other => panic!("unexpected bootstrap request: {other}"),
        };
        take_requests(respond(&mut app, request, result));
    }
    assert_eq!(app.connection, ConnectionState::Ready);
    app
}

pub fn standard_catalog() -> (Vec<Value>, Vec<Value>, Vec<Value>) {
    let models = vec![
        json!({"id": "deep", "model_ref": "minicore/deep:v1", "context_window": 128000, "supports_tools": true, "supported_reasoning": ["auto", "low", "medium", "high"]}),
        json!({"id": "fast", "model_ref": "minicore/fast:v1", "context_window": 32000, "supports_tools": false, "supported_reasoning": ["low", "medium"]}),
        json!({"id": "tiny", "model_ref": "minicore/tiny:v1", "context_window": 8000, "supports_tools": true, "supported_reasoning": ["disabled", "low"]}),
    ];
    let profiles = vec![
        json!({"id": "coding", "model": "deep", "reasoning": "high", "tools": ["read", "edit", "bash"]}),
        json!({"id": "review", "model": "fast", "reasoning": "medium", "tools": ["read"]}),
    ];
    let sessions = vec![
        json!({"session_id": "ses_old", "title": "Rust port", "profile": "coding", "workspace": "/work/rust", "model": "deep", "reasoning": "high", "loaded": true, "created_at": "2027-01-14T08:00:00Z", "updated_at": "2027-01-14T08:00:00Z"}),
        json!({"session_id": "ses_recent", "title": "Web app", "profile": "review", "workspace": "/work/web", "model": "fast", "reasoning": "medium", "loaded": false, "created_at": "2027-01-15T07:45:00Z", "updated_at": "2027-01-15T07:45:00Z"}),
        json!({"session_id": "ses_main", "title": null, "profile": "coding", "workspace": "/work/cli", "model": "deep", "reasoning": "high", "loaded": true, "created_at": "2027-01-15T07:55:00Z", "updated_at": "2027-01-15T07:55:00Z"}),
    ];
    (models, profiles, sessions)
}

pub fn open_session(app: &mut App, session_id: &str) {
    let open = take_requests(app.update(AppEvent::OpenSession {
        session_id: session_id.into(),
    }));
    let requests = if let Some(open) = open.iter().find(|request| request.method == "session.open")
    {
        take_requests(respond(
            app,
            open,
            json!({"session": session_info(session_id, Some("Task"), "high")}),
        ))
    } else {
        open
    };
    let state = requests
        .iter()
        .find(|r| r.method == "session.state")
        .unwrap();
    let history = requests
        .iter()
        .find(|r| r.method == "session.history")
        .unwrap();
    take_requests(respond(app, state, state_json(session_id, "idle")));
    let commands = respond(app, history, page_json(Vec::new(), true));
    assert!(take_requests(commands).is_empty());
}

/// Marks a listed session as running through a real `session_state` event.
pub fn set_session_running(app: &mut App, session_id: &str, loop_id: &str) {
    let event = serde_json::from_value(json!({
        "type": "session_state",
        "data": {
            "state": {
                "session_id": session_id,
                "status": "running",
                "active_loop": {
                    "loop_id": loop_id,
                    "status": "running_model",
                    "request_index": 0,
                    "config_revision": 0,
                    "model": "deep",
                    "pending_interaction": null
                },
                "block_reason": null
            },
            "meta": {
                "session_id": session_id,
                "loop_id": loop_id,
                "dropped_before": 0
            }
        }
    }))
    .expect("session_state fixture parses");
    app.update(AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Notification(
        RpcNotification::AgentEvent(event),
    ))));
}

/// The new-session form on the standard catalog.
pub fn new_session(theme: ThemeKind) -> App {
    let (models, profiles, sessions) = standard_catalog();
    let mut app = ready_catalog(theme, models, profiles, sessions);
    app.update(AppEvent::OpenNewSession);
    app
}

/// The model selector with an active session, so its current model is marked.
pub fn model_selector(theme: ThemeKind) -> App {
    let (models, profiles, sessions) = standard_catalog();
    let mut app = ready_catalog(theme, models, profiles, sessions);
    // Keep this fixture on the first-open path; production reuses a loaded
    // SessionView instead of issuing a redundant session.open request.
    app.sessions.known.get_mut("ses_main").unwrap().info.loaded = false;
    open_session(&mut app, "ses_main");
    app.update(AppEvent::OpenModelSelector);
    app
}

/// The reasoning selector for the draft's selected model.
pub fn reasoning_selector(theme: ThemeKind) -> App {
    let (models, profiles, sessions) = standard_catalog();
    let mut app = ready_catalog(theme, models, profiles, sessions);
    app.update(AppEvent::OpenReasoningSelector);
    app
}

/// The session selector with running, loaded, and known-but-unloaded markers.
pub fn session_selector(theme: ThemeKind) -> App {
    let (models, profiles, sessions) = standard_catalog();
    let mut app = ready_catalog(theme, models, profiles, sessions);
    set_session_running(&mut app, "ses_main", "loop_main");
    app.update(AppEvent::OpenSessionSelector);
    app
}

/// The profile selector.
pub fn profile_selector(theme: ThemeKind) -> App {
    let (models, profiles, sessions) = standard_catalog();
    let mut app = ready_catalog(theme, models, profiles, sessions);
    app.update(AppEvent::OpenProfileSelector);
    app
}

/// A model selector with a query that matches nothing.
pub fn empty_model_search(theme: ThemeKind) -> App {
    let mut app = model_selector(theme);
    app.update(AppEvent::SetSelectorQuery {
        query: "zzzz".to_owned(),
    });
    app
}

/// The model selector with its cursor moved on a short terminal fixture.
pub fn narrow_selector(theme: ThemeKind) -> App {
    let mut app = model_selector(theme);
    app.update(AppEvent::MoveSelector { delta: 1 });
    app
}

/// The Help panel.
pub fn help(theme: ThemeKind) -> App {
    let mut app = fresh(theme);
    app.update(AppEvent::Terminal(crossterm::event::Event::Key(
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::F(1),
            crossterm::event::KeyModifiers::empty(),
        ),
    )));
    app
}

/// The Logs panel with bounded captured stderr lines.
pub fn logs(theme: ThemeKind) -> App {
    let mut app = fresh(theme);
    for line in [
        "agent 12:34:12  loaded profile coding",
        "agent 12:34:13  session ses_1 opened",
        "agent 12:34:20  tool read: 128 lines",
    ] {
        app.update(AppEvent::Rpc(RpcEvent::AgentLogLine(line.to_owned())));
    }
    for c in "/logs".chars() {
        app.update(char_event(c));
    }
    app.update(AppEvent::Terminal(crossterm::event::Event::Key(
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::empty(),
        ),
    )));
    app
}

/// A multiline composer buffer inserted through the paste path.
pub fn multiline_composer(theme: ThemeKind) -> App {
    let mut app = chat(theme);
    app.update(AppEvent::Terminal(crossterm::event::Event::Paste(
        "line one\nline two 你好😀\nline three\nline four\nline five\nline six".to_owned(),
    )));
    app
}

/// A model selector with a visible search query.
pub fn search_query(theme: ThemeKind) -> App {
    let mut app = model_selector(theme);
    app.update(AppEvent::SetSelectorQuery {
        query: "fast".to_owned(),
    });
    app
}

/// A durable transcript scrolled away from the tail with new output.
pub fn new_output_marker(theme: ThemeKind) -> App {
    let mut items = Vec::new();
    for i in 0..24 {
        items.push(user_entry(i * 2, &format!("loop_{i}"), "a user turn"));
        items.push(assistant_entry_with(
            i * 2 + 1,
            &format!("loop_{i}"),
            &format!("A substantial assistant reply for block {i} with enough text to wrap over several rows."),
            None,
            json!([]),
        ));
    }
    let mut app = open_with(theme, "ses_1", Some("Task"), "high", items);
    let total = crate::ui::transcript::total_lines(&app, 80);
    app.update(AppEvent::Viewport {
        total_lines: total,
        visible_rows: 10,
    });
    app.update(AppEvent::Terminal(crossterm::event::Event::Key(
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::PageUp,
            crossterm::event::KeyModifiers::empty(),
        ),
    )));
    app.update(AppEvent::Viewport {
        total_lines: total + 2,
        visible_rows: 10,
    });
    app
}

/// A chat app whose transcript scroll state points away from the tail.
pub fn scrolled(theme: ThemeKind) -> App {
    let mut app = chat(theme);
    if let Some(view) = app.sessions.known.get_mut("ses_1") {
        view.scroll.offset = 0;
        view.scroll.follow_tail = false;
        view.scroll.new_content = true;
    }
    app
}

/// A CJK/emoji exchange on a full footer.
pub fn cjk(theme: ThemeKind) -> App {
    let items = vec![
        user_entry(0, "loop_cjk", "你好，世界 👋 请解释一下"),
        assistant_entry_with(
            1,
            "loop_cjk",
            "这是 **中文** 测试：`你好`。宽度计算对 CJK 每字占 2 列，emoji 😀 也占 2 列。",
            None,
            json!([]),
        ),
    ];
    open_with(theme, "ses_1", Some("Task"), "high", items)
}

pub fn unsaved_gap(theme: ThemeKind) -> App {
    let mut app = chat(theme);
    if let Some(view) = app.sessions.known.get_mut("ses_1") {
        let result = crate::protocol::TurnResultViewWire {
            turn: crate::protocol::TurnRef {
                session_id: "ses_1".to_string(),
                loop_id: "loop_unsaved".to_string(),
            },
            outcome: crate::protocol::LoopOutcomeWire::Completed,
            persistence: crate::protocol::TurnPersistenceWire::Failed,
            usage: crate::protocol::UsageWire::default(),
            requests: 1,
            tool_rounds: 0,
            final_config_revision: 0,
        };
        view.event_gap = true;
        view.last_result = Some(result.clone());
        view.unsaved_loop = Some(crate::state::turn::UnsavedLoop {
            turn: result.turn.clone(),
            event_gap: true,
            user_text: "Process critical batch data".to_string(),
            requests: vec![],
            result: Some(result),
        });
        if let Some(state) = &mut view.state {
            state.status = crate::protocol::SessionStatusWire::Blocked;
            state.block_reason = Some(crate::protocol::SessionBlockReasonWire::Persistence);
        }
    }
    app
}

pub fn steering(theme: ThemeKind) -> App {
    let mut app = live_turn(theme);
    if let Some(view) = app.sessions.known.get_mut("ses_1") {
        if let Some(live) = &mut view.live {
            live.pending_steers.push(crate::state::turn::PendingSteer {
                local_id: 1,
                text: "Focus on memory safety instead".to_string(),
                state: crate::state::turn::PendingSteerState::Queued,
            });
        }
    }
    app
}

pub fn pending_model(theme: ThemeKind) -> App {
    let mut app = live_turn(theme);
    if let Some(view) = app.sessions.known.get_mut("ses_1") {
        view.config_update = Some(crate::state::session::PendingConfigUpdate {
            loop_id: Some("loop_live".to_string()),
            model: Some("claude-3-7-sonnet".to_string()),
            reasoning: Some(crate::protocol::Reasoning::High),
            revision: Some(2),
            state: crate::state::session::ConfigUpdateState::WaitingBoundary,
        });
    }
    app.notice(
        crate::app::NoticeLevel::Info,
        "Saved · applies at next model request (rev 2)",
    );
    app
}

pub fn finishing(theme: ThemeKind) -> App {
    let mut app = live_turn(theme);
    if let Some(view) = app.sessions.known.get_mut("ses_1") {
        if let Some(state) = &mut view.state {
            state.status = crate::protocol::SessionStatusWire::Finishing;
        }
    }
    app
}

pub fn close_user(theme: ThemeKind) -> App {
    let mut app = live_turn(theme);
    if let Some(view) = app.sessions.known.get_mut("ses_1") {
        view.last_result = Some(crate::protocol::TurnResultViewWire {
            turn: crate::protocol::TurnRef {
                session_id: "ses_1".to_string(),
                loop_id: "loop_live".to_string(),
            },
            outcome: crate::protocol::LoopOutcomeWire::Cancelled {
                reason: crate::protocol::CancelReasonWire::User,
            },
            persistence: crate::protocol::TurnPersistenceWire::Persisted,
            usage: crate::protocol::UsageWire::default(),
            requests: 1,
            tool_rounds: 1,
            final_config_revision: 0,
        });
    }
    app.update(AppEvent::CloseSession {
        session_id: "ses_1".to_string(),
        confirm: false,
    });
    app
}

fn result_only(theme: ThemeKind, loop_id: &str, reason: crate::protocol::CancelReasonWire) -> App {
    let mut app = open_empty(theme, "ses_1", Some("Task"), "high");
    if let Some(view) = app.sessions.known.get_mut("ses_1") {
        view.last_result = Some(crate::protocol::TurnResultViewWire {
            turn: crate::protocol::TurnRef {
                session_id: "ses_1".to_string(),
                loop_id: loop_id.to_string(),
            },
            outcome: crate::protocol::LoopOutcomeWire::Cancelled { reason },
            persistence: crate::protocol::TurnPersistenceWire::Persisted,
            usage: crate::protocol::UsageWire::default(),
            requests: 1,
            tool_rounds: 0,
            final_config_revision: 0,
        });
    }
    app
}

pub fn unknown_cancel_result(theme: ThemeKind) -> App {
    result_only(
        theme,
        "loop_unknown",
        crate::protocol::CancelReasonWire::Unknown("sandbox_evicted".to_string()),
    )
}

pub fn shutdown_cancel_result(theme: ThemeKind) -> App {
    result_only(
        theme,
        "loop_shutdown",
        crate::protocol::CancelReasonWire::Shutdown,
    )
}

pub fn store_error(theme: ThemeKind) -> App {
    let mut app = chat(theme);
    let cmds = app.update(AppEvent::OpenSession {
        session_id: "ses_corrupt".to_string(),
    });
    let reqs = take_requests(cmds);
    if let Some(req) = reqs.first() {
        respond_rpc_error(
            &mut app,
            req,
            crate::protocol::STORE_ERROR,
            "store read failed",
        );
    }
    app
}

fn char_event(c: char) -> AppEvent {
    AppEvent::Terminal(crossterm::event::Event::Key(
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char(c),
            crossterm::event::KeyModifiers::empty(),
        ),
    ))
}
