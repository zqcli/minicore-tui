//! Test fixture builders that construct app state exclusively through
//! `App::update` with real wire-shaped events (no production back doors).
//! Used by the component and snapshot tests for the Phase 3 renderer.

use std::path::PathBuf;

use serde_json::{Value, json};

use crate::app::App;
use crate::command::AppCommand;
use crate::event::{AppEvent, RpcEvent};
use crate::protocol::{
    IncomingFrame, OutgoingRequest, RpcError as WireError, RpcErrorData, RpcNotification,
    RpcResponse,
};
use crate::theme::ThemeKind;

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

pub fn respond_error(
    app: &mut App,
    request: &OutgoingRequest,
    kind: &str,
    message: &str,
) -> Vec<AppCommand> {
    app.update(AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Response(
        RpcResponse {
            id: request.id,
            result: None,
            error: Some(WireError {
                code: -32000,
                message: message.to_owned(),
                data: Some(RpcErrorData {
                    kind: kind.to_owned(),
                    retryable: false,
                }),
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
        "instance_id": "ins_1",
        "created_at": "2026-01-02T03:04:05.006Z",
        "updated_at": "2026-01-02T03:04:05.006Z"
    })
}

fn state_json(session_id: &str, status: &str) -> Value {
    json!({
        "session_id": session_id,
        "instance_id": "ins_1",
        "status": status,
        "health": "healthy",
        "active_turn": null,
        "pending_interaction": null,
        "conversation_seq": 0,
        "last_terminal": null
    })
}

fn page_json(entries: Vec<Value>, complete: bool) -> Value {
    json!({
        "entries": entries,
        "next_after": null,
        "observed_head": entries.len() as u64,
        "complete": complete
    })
}

fn user_entry(seq: u64, turn_id: &str, text: &str) -> Value {
    json!({"user_message": {
        "seq": seq,
        "turn_id": turn_id,
        "text": text,
        "execution": {"model": "deep", "reasoning": "high", "max_tool_rounds": 8},
        "created_at": "2026-01-02T03:04:05.006Z"
    }})
}

fn assistant_entry_with(
    seq: u64,
    turn_id: &str,
    text: &str,
    reasoning: Option<&str>,
    tool_calls: Value,
) -> Value {
    json!({"assistant_message": {
        "seq": seq,
        "turn_id": turn_id,
        "model": "deep",
        "text": text,
        "reasoning": reasoning,
        "tool_calls": tool_calls,
        "usage": {},
        "finish_reason": "stop",
        "created_at": "2026-01-02T03:04:05.006Z"
    }})
}

fn tool_result_entry(seq: u64, call_id: &str, name: &str, outcome: &str, content: &str) -> Value {
    json!({"tool_result": {
        "seq": seq,
        "turn_id": "trn_1",
        "tool_call_id": call_id,
        "tool_name": name,
        "outcome": outcome,
        "content": content,
        "created_at": "2026-01-02T03:04:05.006Z"
    }})
}

/// An empty app in `Starting` with the given palette.
pub fn fresh(theme: ThemeKind) -> App {
    let mut app = App::new(PathBuf::from("/project"));
    app.update(AppEvent::SetTheme(theme));
    app
}

/// Bootstraps to Ready and opens `session_id` with `entries` delivered on
/// the first transcript chain.
fn open_with(
    theme: ThemeKind,
    session_id: &str,
    title: Option<&str>,
    reasoning: &str,
    entries: Vec<Value>,
) -> App {
    let mut app = fresh(theme);
    let requests = take_requests(app.update(AppEvent::Bootstrap));
    for request in &requests {
        let result = match request.method {
            "agent.ping" => json!({"version": "0.2.0"}),
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
    let transcript = requests
        .iter()
        .find(|r| r.method == "session.transcript")
        .unwrap();
    take_requests(respond(&mut app, state, state_json(session_id, "idle")));
    let commands = respond(&mut app, transcript, page_json(entries, true));
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
    let entries = vec![
        user_entry(1, "trn_1", "Hello **world** with `code`."),
        assistant_entry_with(2, "trn_1", CHAT_MARKDOWN, None, json!([])),
        // A completed terminal stays invisible (spec 18.6).
        json!({"turn_terminal": {
            "seq": 3, "turn_id": "trn_1", "terminal": "completed",
            "usage": {}, "created_at": "2026-01-02T03:04:05.006Z"
        }}),
    ];
    open_with(theme, "ses_1", Some("Task"), "high", entries)
}

/// A session whose assistant carries a reasoning run (plus text ordering).
pub fn chat_with_reasoning(theme: ThemeKind) -> App {
    let entries = vec![
        user_entry(1, "trn_1", "hello"),
        assistant_entry_with(
            2,
            "trn_1",
            "answer text",
            Some("carefully thinking out loud"),
            json!([]),
        ),
    ];
    open_with(theme, "ses_1", Some("Task"), "high", entries)
}

/// A session with durable tool cards in success/denied/failed states and an
/// expanded preview with more than 40 lines (toggled via `ToggleTools`).
pub fn tools(theme: ThemeKind) -> App {
    let big_result = (0..60)
        .map(|i| format!("line {i:02} of a long file"))
        .collect::<Vec<_>>()
        .join("\n");
    let entries = vec![
        user_entry(1, "trn_1", "run the tools"),
        assistant_entry_with(
            2,
            "trn_1",
            "using tools now",
            None,
            json!([
                {"tool_call_id": "call-1", "name": "read", "call_index": 0},
                {"tool_call_id": "call-2", "name": "bash", "call_index": 1},
                {"tool_call_id": "call-3", "name": "edit", "call_index": 2}
            ]),
        ),
        tool_result_entry(3, "call-1", "read", "success", &big_result),
        tool_result_entry(4, "call-2", "bash", "denied", "not allowed"),
        tool_result_entry(5, "call-3", "edit", "failed", "edit rejected"),
    ];
    let mut app = open_with(theme, "ses_1", Some("Task"), "high", entries);
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
        "data": {"turn": {"session_id": "ses_1", "instance_id": "ins_1", "turn_id": "trn_live"},
                 "meta": {"session_id": "ses_1", "instance_id": "ins_1", "dropped_before": 0}}
    }))
    .unwrap();
    app.update(AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Notification(
        RpcNotification::AgentEvent(started),
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
                "turn": {"session_id": "ses_1", "instance_id": "ins_1", "turn_id": "trn_live"},
                "channel": channel,
                "delta": delta,
                "meta": {"session_id": "ses_1", "instance_id": "ins_1", "dropped_before": 0}
            }
        }))
        .unwrap();
        app.update(AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Notification(
            RpcNotification::AgentEvent(output),
        ))));
    }
    for event in [
        json!({"type": "tool_started", "data": {
            "turn": {"session_id": "ses_1", "instance_id": "ins_1", "turn_id": "trn_live"},
            "tool_call_id": "c1", "tool_name": "read",
            "meta": {"session_id": "ses_1", "instance_id": "ins_1", "dropped_before": 0}}}),
        json!({"type": "tool_progress", "data": {
            "turn": {"session_id": "ses_1", "instance_id": "ins_1", "turn_id": "trn_live"},
            "tool_call_id": "c1",
            "progress": {"message": "reading…", "completed": null, "total": null},
            "meta": {"session_id": "ses_1", "instance_id": "ins_1", "dropped_before": 0}}}),
        json!({"type": "output_delta", "data": {
            "turn": {"session_id": "ses_1", "instance_id": "ins_1", "turn_id": "trn_live"},
            "channel": "text", "delta": "more",
            "meta": {"session_id": "ses_1", "instance_id": "ins_1", "dropped_before": 2}}}),
    ] {
        let event = serde_json::from_value(event).unwrap();
        app.update(AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Notification(
            RpcNotification::AgentEvent(event),
        ))));
    }
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
    let entries = vec![
        user_entry(1, "trn_1", "你好，世界 👋 请解释一下"),
        assistant_entry_with(
            2,
            "trn_1",
            "这是 **中文** 测试：`你好`。宽度计算对 CJK 每字占 2 列，emoji 😀 也占 2 列。",
            None,
            json!([]),
        ),
    ];
    open_with(theme, "ses_1", Some("Task"), "high", entries)
}
