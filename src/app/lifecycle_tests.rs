//! Lifecycle tests that drive `App` through the real client, transport and
//! reducer: startup, send, stream, tool, wait, turn end, paging and compaction.

use std::time::Duration;

use serde_json::{Value, json};

use crate::app::{Action, App, Msg, OpDone, OpKind, Overlay, Scroll};
use crate::protocol::read::UserKind;
use crate::testing::{FakeAgent, display_read, session_info, turn};
use crate::transcript::Block;

/// Runs the app's message pump until `check` holds or the budget expires.
async fn pump(
    app: &mut App,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<Msg>,
    check: impl Fn(&App) -> bool,
) {
    for _ in 0..400 {
        while let Ok(Some(message)) =
            tokio::time::timeout(Duration::from_millis(2), rx.recv()).await
        {
            app.on_msg(message);
        }
        if check(app) {
            return;
        }
    }
    panic!(
        "the app never reached the expected state (error: {:?}, busy: {}, session: {}, ready: {})",
        app.last_error,
        app.is_busy(),
        app.session.is_some(),
        app.ready
    );
}

/// Bootstraps and opens `s1`, leaving the transcript loaded.
async fn open(
    app: &mut App,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<Msg>,
    fake: &FakeAgent,
    tx: &tokio::sync::mpsc::UnboundedSender<Msg>,
) {
    crate::app::tasks::bootstrap(fake.client.clone(), tx.clone()).await;
    pump(app, rx, |app| app.ready).await;
    app.on_action(Action::OpenSession("s1".into()));
    pump(app, rx, |app| app.session.is_some()).await;
    settle(app, rx).await;
}

/// Drains messages until the channel has been quiet for `QUIET`, so spawned
/// ops (including deferred replies) settle.
async fn settle(app: &mut App, rx: &mut tokio::sync::mpsc::UnboundedReceiver<Msg>) {
    while let Ok(Some(message)) = tokio::time::timeout(QUIET, rx.recv()).await {
        app.on_msg(message);
    }
}

/// How long the message channel must stay quiet to call the app settled.
const QUIET: Duration = Duration::from_millis(120);

/// A fake Agent answering the catalog calls a bootstrap makes.
fn boot(fake: &FakeAgent) {
    fake.script(
        "agent.ping",
        json!({
            "version": "0.7.0",
            "protocol_version": 1,
            "capabilities": [
                "session.read", "session.read.display", "session.context",
                "turn.result", "tool.read", "tool.read.display", "tool.output",
                "workspace.files", "workspace.search", "workspace.status",
                "changes.list", "changes.diff"
            ],
        }),
    );
    fake.script(
        "model.list",
        json!({"models": [{
            "id": "mock", "model_ref": "mock/large", "context_window": 200000,
            "supports_tools": true,
            "supported_reasoning": ["low", "medium", "high"],
        }]}),
    );
    fake.script(
        "profile.list",
        json!({"profiles": [{"id": "coding", "model": "mock", "reasoning": "medium"}]}),
    );
    fake.script("session.list", json!({"sessions": [session_info("s1")]}));
    fake.script(
        "session.presentation",
        json!({"session_id": "s1", "model_label": "mock", "git_branch": "main",
               "context": {"tokens": 1200, "window": 200000, "percent": 0.6, "kind": "reported"}}),
    );
    fake.script(
        "session.state",
        json!({"session_id": "s1", "status": "idle"}),
    );
}

#[tokio::test]
async fn a_turn_streams_through_to_its_turn_end_line() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (mut fake, mut app) = FakeAgent::start(tx.clone());
    boot(&fake);
    fake.script("session.open", json!({"session": session_info("s1")}));
    fake.script("session.read", display_read(0, 0));
    fake.script(
        "session.presentation",
        json!({"session_id": "s1", "model_label": "mock", "git_branch": "main",
               "context": {"tokens": 1200, "window": 200000, "percent": 0.6, "kind": "reported"}}),
    );
    fake.script(
        "turn.send",
        json!({"turn": turn("l1"), "accepted_at": "2024-01-01T00:00:01Z"}),
    );
    fake.script_held(
        "turn.wait",
        json!({
            "turn": turn("l1"),
            "outcome": {"type": "completed"},
            "usage": {"input_tokens": 100, "output_tokens": 20},
            "requests": 2,
            "tool_rounds": 1,
            "completed_at": "2024-01-01T00:00:09Z",
        }),
    );
    fake.script(
        "changes.list",
        json!({"records": [], "total": 0, "complete": true}),
    );

    crate::app::tasks::bootstrap(fake.client.clone(), tx.clone()).await;
    pump(&mut app, &mut rx, |app| app.ready).await;
    assert_eq!(app.models.len(), 1);
    assert_eq!(app.sessions.len(), 1);

    app.on_action(Action::OpenSession("s1".into()));
    pump(&mut app, &mut rx, |app| app.session.is_some()).await;
    settle(&mut app, &mut rx).await;

    app.composer = "fix the greeting".into();
    app.on_action(Action::Submit);
    pump(&mut app, &mut rx, |app| app.is_busy()).await;

    // Tool call facts: started, then display, then process output.
    fake.emit(json!({
        "type": "tool_started",
        "data": {"turn": turn("l1"), "request_index": 0, "tool_call_id": "c1",
                 "tool_name": "bash", "meta": {"session_id": "s1", "dropped_before": 0}}
    }))
    .await;
    fake.emit(json!({
        "type": "tool_presentation",
        "data": {"turn": turn("l1"), "request_index": 0, "tool_call_id": "c1",
                 "tool_name": "bash",
                 "display": {"detail": "$ cargo test", "truncated": false},
                 "meta": {"session_id": "s1", "dropped_before": 0}}
    }))
    .await;
    fake.emit(json!({
        "type": "tool_execution",
        "data": {"tool_ref": {"session_id": "s1", "loop_id": "l1",
                              "request_index": 0, "tool_call_id": "c1"},
                 "name": "bash",
                 "state": "succeeded",
                 "result_truncated": false,
                 "meta": {"session_id": "s1", "dropped_before": 0}}
    }))
    .await;
    pump(&mut app, &mut rx, |app| {
        app.transcript().is_some_and(|transcript| {
            transcript
                .find_tool_by_call_id("c1")
                .is_some_and(|call| call.state == crate::transcript::ToolState::Succeeded)
        })
    })
    .await;

    fake.release("turn.wait");
    pump(&mut app, &mut rx, |app| !app.is_busy()).await;
    settle(&mut app, &mut rx).await;

    let session = app.session.as_ref().expect("a resident session");
    let blocks = session.transcript.blocks();
    assert!(
        matches!(&blocks[0], Block::User { text, .. } if text == "fix the greeting"),
        "the prompt is the first block"
    );
    let end = blocks
        .iter()
        .find_map(|block| match block {
            Block::TurnEnd {
                outcome,
                requests,
                tool_rounds,
                usage,
                ..
            } => Some((outcome.clone(), *requests, *tool_rounds, usage.clone())),
            _ => None,
        })
        .expect("a turn-end block");
    assert!(end.0.is_completed());
    assert_eq!(end.1, 2);
    assert_eq!(end.2, 1, "the finished tool is counted in the turn summary");
    assert_eq!(end.3.output_tokens, Some(20));
    assert_eq!(session.usage.output, 20);

    let calls = fake.methods();
    assert!(calls.contains(&"turn.send".to_owned()));
    assert!(calls.contains(&"turn.wait".to_owned()));
    let waited = fake.calls("turn.wait");
    assert_eq!(waited[0]["loop_id"], json!("l1"));
}

#[tokio::test]
async fn a_busy_send_keeps_the_text_and_retries_when_idle() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (fake, mut app) = FakeAgent::start(tx.clone());
    boot(&fake);
    fake.script("session.open", json!({"session": session_info("s1")}));
    fake.script("session.read", display_read(0, 0));
    fake.script(
        "session.presentation",
        json!({"session_id": "s1", "model_label": "mock", "git_branch": "main",
               "context": {"tokens": 1200, "window": 200000, "percent": 0.6, "kind": "reported"}}),
    );
    fake.script_error("turn.send", -32003, "session is busy", "session_busy");
    fake.script(
        "session.state",
        json!({"state": {"session_id": "s1", "status": "idle"}}),
    );

    open(&mut app, &mut rx, &fake, &tx).await;

    app.composer = "hello".into();
    app.on_action(Action::Submit);
    // The send was refused: the text is queued, not lost, and the app retries.
    pump(&mut app, &mut rx, |app| {
        app.queue_len() > 0 || app.waiting_for_compaction
    })
    .await;
    assert!(
        app.queue_len() > 0 || app.waiting_for_compaction,
        "a busy send is retained rather than dropped"
    );
    assert!(fake.methods().contains(&"turn.send".to_owned()));
}

#[tokio::test]
async fn a_gapped_turn_reconciles_against_turn_result() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (fake, mut app) = FakeAgent::start(tx.clone());
    boot(&fake);
    fake.script("session.open", json!({"session": session_info("s1")}));
    fake.script("session.read", display_read(0, 0));
    fake.script(
        "session.presentation",
        json!({"session_id": "s1", "model_label": "mock", "git_branch": "main",
               "context": {"tokens": 1200, "window": 200000, "percent": 0.6, "kind": "reported"}}),
    );
    fake.script("turn.send", json!({"turn": turn("l1")}));
    fake.script(
        "turn.wait",
        json!({"turn": turn("l1"), "outcome": {"type": "failed", "kind": "model"}, "requests": 1}),
    );
    let item = json!({
        "item": {"type": "user", "data": {"loop_id": "l1", "kind": "prompt",
                                          "input": {"text": "canonical prompt"}}}
    })
    .to_string();
    fake.script(
        "turn.result",
        json!({
            "turn": turn("l1"),
            "outcome": {"type": "failed", "kind": "model"},
            "total": 1,
            "items": [{
                "index": 0, "offset": 0, "total_bytes": item.len(),
                "encoding": "utf8_json", "data": item, "complete": true,
            }],
            "next_cursor": null,
        }),
    );
    fake.script(
        "changes.list",
        json!({"records": [], "total": 0, "complete": true}),
    );

    open(&mut app, &mut rx, &fake, &tx).await;

    app.composer = "canonical prompt".into();
    app.on_action(Action::Submit);
    // The turn is durable-only here: it may resolve before the frame is drawn.
    settle(&mut app, &mut rx).await;

    assert!(fake.methods().contains(&"turn.result".to_owned()));
    let session = app.session.as_ref().unwrap();
    let prompts: Vec<&str> = session
        .transcript
        .blocks()
        .iter()
        .filter_map(|block| match block {
            Block::User { text, kind } if *kind == UserKind::Prompt => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        prompts,
        vec!["canonical prompt"],
        "the durable items replace the optimistic block instead of duplicating it"
    );
}

#[tokio::test]
async fn older_pages_prepend_and_keep_the_anchor_block() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (fake, mut app) = FakeAgent::start(tx.clone());
    boot(&fake);
    fake.script("session.open", json!({"session": session_info("s1")}));
    fake.script("session.read", display_read(80, 0));

    open(&mut app, &mut rx, &fake, &tx).await;

    let session = app.session.as_ref().unwrap();
    // The tail starts at total-60, and the earlier-history marker counts what
    // is behind it.
    assert_eq!(session.loaded_from, 20);
    assert!(matches!(
        session.transcript.blocks().first(),
        Some(Block::EarlierHistory { hidden_items: 20 })
    ));

    // Reading one older page moves the boundary and re-renders the marker.
    fake.script(
        "session.read",
        json!({
            "session": session_info("s1"),
            "items": [], "next_cursor": null, "total": 80,
            "history_revision": "h1", "captured_end": 1,
            "projection": {"revision": "p1", "first_item": 0, "covered_item_count": 0},
        }),
    );
    app.on_action(Action::LoadOlder);
    pump(&mut app, &mut rx, |app| {
        app.session
            .as_ref()
            .is_some_and(|session| session.loaded_from == 0)
    })
    .await;
    let session = app.session.as_ref().unwrap();
    assert!(!matches!(
        session.transcript.blocks().first(),
        Some(Block::EarlierHistory { .. })
    ));
}

#[tokio::test]
async fn a_stale_pin_reloads_the_tail_instead_of_failing() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (fake, mut app) = FakeAgent::start(tx.clone());
    boot(&fake);
    fake.script("session.open", json!({"session": session_info("s1")}));
    fake.script("session.read", display_read(100, 40));

    open(&mut app, &mut rx, &fake, &tx).await;
    assert_eq!(app.session.as_ref().unwrap().loaded_from, 40);

    // An older read below first_item is refused; the tail is re-read.
    fake.script(
        "session.read",
        json!({"error": {"code": -32602, "message": "cursor is below first_item"}}),
    );
    let before = fake.calls("session.read").len();
    app.on_action(Action::LoadOlder);
    settle(&mut app, &mut rx).await;
    assert!(
        fake.calls("session.read").len() > before,
        "the failed page is followed by a fresh probe"
    );
    assert!(app.last_error.is_none() || app.session.is_some());
}

#[tokio::test]
async fn compaction_appends_one_marker_and_keeps_loaded_blocks() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (fake, mut app) = FakeAgent::start(tx.clone());
    boot(&fake);
    fake.script("session.open", json!({"session": session_info("s1")}));
    fake.script("session.read", display_read(4, 0));
    fake.script(
        "session.compact",
        json!({
            "origin": "manual", "operation_id": "op",
            "status": "compacted", "before_tokens": 32200, "after_tokens": 2700,
            "covered_loop_count": 3, "covered_item_count": 12, "retained_item_count": 2,
        }),
    );

    open(&mut app, &mut rx, &fake, &tx).await;
    let loaded = app.session.as_ref().unwrap().transcript.len();

    app.on_action(Action::Compact);
    settle(&mut app, &mut rx).await;

    let session = app.session.as_ref().unwrap();
    let markers: Vec<&Block> = session
        .transcript
        .blocks()
        .iter()
        .filter(|block| matches!(block, Block::Compaction { .. }))
        .collect();
    assert_eq!(markers.len(), 1, "exactly one marker per compaction");
    assert!(matches!(
        markers[0],
        Block::Compaction {
            before: Some(32200),
            after: Some(2700),
            ..
        }
    ));
    assert!(
        session.transcript.len() >= loaded,
        "loaded content survives the marker"
    );
}

#[tokio::test]
async fn steering_only_leaves_the_queue_when_the_agent_applies_it() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (mut fake, mut app) = FakeAgent::start(tx.clone());
    boot(&fake);
    fake.script("session.open", json!({"session": session_info("s1")}));
    fake.script("session.read", display_read(0, 0));
    fake.script(
        "session.presentation",
        json!({"session_id": "s1", "model_label": "mock", "git_branch": "main",
               "context": {"tokens": 1200, "window": 200000, "percent": 0.6, "kind": "reported"}}),
    );
    fake.script("turn.send", json!({"turn": turn("l1")}));
    fake.script("turn.steer", json!({"steer_index": 0}));
    fake.script_held(
        "turn.wait",
        json!({"turn": turn("l1"), "outcome": {"type": "completed"}}),
    );

    open(&mut app, &mut rx, &fake, &tx).await;

    app.composer = "start".into();
    app.on_action(Action::Submit);
    pump(&mut app, &mut rx, |app| app.is_busy()).await;

    // While running, the next prompt becomes a steer.
    app.composer = "also the tests".into();
    app.on_action(Action::Submit);
    let session = app.session.as_ref().unwrap();
    assert!(
        session
            .queue
            .iter()
            .any(|queued| queued.kind == crate::app::session::QueuedKind::Steer),
        "a running turn takes the text as a steer"
    );

    // Applying it is confirmed by the boundary fact only.
    fake.emit(json!({
        "type": "steer_progress",
        "data": {"turn": turn("l1"), "applied_count": 1,
                 "meta": {"session_id": "s1", "dropped_before": 0}}
    }))
    .await;

    pump(&mut app, &mut rx, |app| {
        app.transcript().is_some_and(|transcript| {
            transcript.blocks().iter().any(|block| {
                matches!(
                    block,
                    Block::User {
                        kind: UserKind::Steering,
                        ..
                    }
                )
            })
        })
    })
    .await;
    let session = app.session.as_ref().unwrap();
    assert!(
        session
            .queue
            .iter()
            .all(|queued| queued.kind != crate::app::session::QueuedKind::Steer)
    );
}

#[tokio::test]
async fn an_approval_prompt_is_answered_over_the_wire() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (mut fake, mut app) = FakeAgent::start(tx.clone());
    boot(&fake);
    fake.script("session.open", json!({"session": session_info("s1")}));
    fake.script("session.read", display_read(0, 0));
    fake.script(
        "session.presentation",
        json!({"session_id": "s1", "model_label": "mock", "git_branch": "main",
               "context": {"tokens": 1200, "window": 200000, "percent": 0.6, "kind": "reported"}}),
    );
    fake.script("turn.send", json!({"turn": turn("l1")}));
    fake.script("interaction.answer", json!({}));
    fake.script_held(
        "turn.wait",
        json!({"turn": turn("l1"), "outcome": {"type": "completed"}}),
    );

    open(&mut app, &mut rx, &fake, &tx).await;
    app.composer = "write a file".into();
    app.on_action(Action::Submit);
    pump(&mut app, &mut rx, |app| app.is_busy()).await;

    fake.emit(json!({
        "type": "interaction_requested",
        "data": {"turn": turn("l1"),
                 "interaction": {"interaction_id": "i1", "tool_call_id": "c1",
                                 "tool_name": "write",
                                 "kind": {"type": "approval", "data": {"prompt": "Allow write?"}}},
                 "meta": {"session_id": "s1", "dropped_before": 0}}
    }))
    .await;
    pump(&mut app, &mut rx, |app| app.has_interaction()).await;
    let interaction = app.session.as_ref().unwrap().interaction.as_ref().unwrap();
    assert_eq!(interaction.options(), vec!["Allow once", "Deny"]);
    assert_eq!(app.status_label(), Some("Waiting for approval"));

    app.on_action(Action::Answer(crate::app::Answer::Allow));
    settle(&mut app, &mut rx).await;
    let answered = fake.calls("interaction.answer");
    assert_eq!(answered.len(), 1);
    assert_eq!(answered[0]["interaction_id"], json!("i1"));
    assert_eq!(answered[0]["answer"]["decision"], json!("allow_once"));
    assert!(!app.has_interaction());
}

#[tokio::test]
async fn a_closed_agent_shows_a_fatal_screen_with_its_stderr_tail() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (fake, mut app) = FakeAgent::start(tx.clone());
    boot(&fake);
    crate::app::tasks::bootstrap(fake.client.clone(), tx.clone()).await;
    pump(&mut app, &mut rx, |app| app.ready).await;

    drop(fake);
    pump(&mut app, &mut rx, |app| app.fatal.is_some()).await;
    let fatal = app.fatal.as_ref().unwrap();
    assert!(!fatal.reason.is_empty());
}

#[tokio::test]
async fn a_manual_compaction_records_the_operation_it_started() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (fake, mut app) = FakeAgent::start(tx.clone());
    boot(&fake);
    fake.script("session.open", json!({"session": session_info("s1")}));
    fake.script("session.read", display_read(0, 0));
    fake.script(
        "session.presentation",
        json!({"session_id": "s1", "model_label": "mock", "git_branch": "main",
               "context": {"tokens": 1200, "window": 200000, "percent": 0.6, "kind": "reported"}}),
    );
    fake.script(
        "session.compact",
        json!({"operation_id": "x", "status": "noop"}),
    );
    open(&mut app, &mut rx, &fake, &tx).await;
    app.on_action(Action::Compact);
    settle(&mut app, &mut rx).await;
    let calls = fake.calls("session.compact");
    assert_eq!(calls.len(), 1);
    let operation = calls[0]["operation_id"].as_str().unwrap().to_owned();
    assert!(operation.starts_with("tui-"), "{operation}");
    assert!(
        fake.calls("session.compact")
            .iter()
            .all(|call| call["operation_id"] == json!(operation)),
        "an operation id is never reused within a session"
    );
}

#[tokio::test]
async fn scrolling_leaves_follow_mode_and_returns_to_it() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (fake, mut app) = FakeAgent::start(tx.clone());
    boot(&fake);
    fake.script("session.open", json!({"session": session_info("s1")}));
    fake.script("session.read", display_read(10, 0));
    open(&mut app, &mut rx, &fake, &tx).await;

    // Ten blocks of ten rows each; the viewport shows five rows.
    for index in 0..10 {
        app.session.as_mut().unwrap().transcript.push(Block::User {
            text: index.to_string(),
            kind: UserKind::Prompt,
        });
    }
    app.layout.prepare(
        app.session.as_ref().unwrap().transcript.blocks(),
        app.session.as_ref().unwrap().transcript.revisions(),
        40,
        crate::transcript::layout::Fold {
            expanded_tools: false,
            show_reasoning: false,
        },
        |_, _| vec![ratatui::text::Line::raw("row"); 10],
    );
    app.viewport = 5;
    assert_eq!(app.layout.total_rows(), 100);
    app.scroll_by(-5, 5);
    assert_ne!(app.scroll, Scroll::Follow);
    app.scroll_by(100, 5);
    assert_eq!(app.scroll, Scroll::Follow);
    assert_eq!(app.overlay, Overlay::None);
}

#[tokio::test]
async fn an_op_for_another_session_is_ignored() {
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let (_fake, mut app) = FakeAgent::start(tx);
    app.on_msg(Msg::Op(Box::new(OpDone {
        session: Some("other".into()),
        epoch: 0,
        kind: OpKind::History(Err(crate::protocol::RpcError::new(
            0,
            "unexpected",
            "should not surface",
        ))),
    })));
    assert!(app.last_error.is_none());
}

/// A turn-scope change list with one record and a countable diff.
fn one_changed_file() -> Value {
    json!({
        "records": [{
            "change_ref": "tool:abc", "path": "app.py", "kind": "modified",
            "details_available": true,
        }],
        "total": 1,
        "complete": true,
    })
}

fn diff_with(added: usize, removed: usize) -> Value {
    let mut lines = Vec::new();
    for index in 0..removed {
        lines.push(
            json!({"kind": "removed", "old_index": index, "new_index": index,
                          "text": "-old", "line_complete": true}),
        );
    }
    for index in 0..added {
        lines.push(
            json!({"kind": "added", "old_index": 100 + index, "new_index": index,
                          "text": "+new", "line_complete": true}),
        );
    }
    json!({
        "change_ref": "tool:abc", "path": "app.py", "kind": "modified",
        "availability": "available", "complete": true, "truncated": false,
        "hunks": [{"old_start": 1, "old_count": removed, "new_start": 1,
                   "new_count": added, "lines": lines}],
        "next_cursor": null,
    })
}

#[tokio::test]
async fn the_turn_end_line_counts_real_added_and_removed_lines() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (fake, mut app) = FakeAgent::start(tx.clone());
    boot(&fake);
    fake.script("session.open", json!({"session": session_info("s1")}));
    fake.script("session.read", display_read(0, 0));
    fake.script("turn.send", json!({"turn": turn("l1")}));
    fake.script(
        "turn.wait",
        json!({"turn": turn("l1"), "outcome": {"type": "completed"}}),
    );
    fake.script("changes.list", one_changed_file());
    fake.script("changes.diff", diff_with(40, 7));
    open(&mut app, &mut rx, &fake, &tx).await;

    app.composer = "change it".into();
    app.on_action(Action::Submit);
    settle(&mut app, &mut rx).await;

    let summary = app
        .session
        .as_ref()
        .unwrap()
        .transcript
        .blocks()
        .iter()
        .find_map(|block| match block {
            Block::TurnEnd { changes, .. } => *changes,
            _ => None,
        })
        .expect("the turn has a change summary");
    assert_eq!(summary.files, 1);
    assert_eq!(summary.lines, Some((40, 7)));
    let line = crate::app::turn_end_line(
        &crate::protocol::LoopOutcome::Completed,
        None,
        1,
        0,
        Some(&summary),
    );
    assert!(line.contains("1 file changed (+40 −7)"), "{line}");
}

#[tokio::test]
async fn an_unknown_line_count_is_reported_rather_than_as_zero() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (fake, mut app) = FakeAgent::start(tx.clone());
    boot(&fake);
    fake.script("session.open", json!({"session": session_info("s1")}));
    fake.script("session.read", display_read(0, 0));
    fake.script("turn.send", json!({"turn": turn("l1")}));
    fake.script(
        "turn.wait",
        json!({"turn": turn("l1"), "outcome": {"type": "completed"}}),
    );
    // The record exists but its body is gone: the file count is known and the
    // line counts are not.
    fake.script(
        "changes.list",
        json!({
            "records": [{"change_ref": "tool:gone", "path": "app.py",
                         "kind": "modified", "details_available": false}],
            "total": 1,
        }),
    );
    open(&mut app, &mut rx, &fake, &tx).await;

    app.composer = "change it".into();
    app.on_action(Action::Submit);
    settle(&mut app, &mut rx).await;

    let summary = app
        .session
        .as_ref()
        .unwrap()
        .transcript
        .blocks()
        .iter()
        .find_map(|block| match block {
            Block::TurnEnd { changes, .. } => *changes,
            _ => None,
        })
        .expect("the turn has a change summary");
    assert_eq!(summary.files, 1);
    assert_eq!(summary.lines, None, "an unavailable diff has no count");
    let line = crate::app::turn_end_line(
        &crate::protocol::LoopOutcome::Completed,
        None,
        1,
        0,
        Some(&summary),
    );
    assert!(line.contains("1 file changed"), "{line}");
    assert!(
        !line.contains("+0 −0"),
        "unknown counts are never printed: {line}"
    );
    assert!(!line.contains("(+"), "{line}");
}

#[tokio::test]
async fn three_tool_calls_on_one_file_report_one_file_and_summed_lines() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (fake, mut app) = FakeAgent::start(tx.clone());
    boot(&fake);
    fake.script("session.open", json!({"session": session_info("s1")}));
    fake.script("session.read", display_read(0, 0));
    fake.script("turn.send", json!({"turn": turn("l1")}));
    fake.script(
        "turn.wait",
        json!({"turn": turn("l1"), "outcome": {"type": "completed"}}),
    );
    // write + edit + apply_patch on one path: three records, one file.
    fake.script(
        "changes.list",
        json!({
            "records": [
                {"change_ref": "tool:w", "path": "app.py", "details_available": true},
                {"change_ref": "tool:e", "path": "app.py", "details_available": true},
                {"change_ref": "tool:p", "path": "app.py", "details_available": true},
            ],
            "total": 3,
        }),
    );
    fake.script("changes.diff", diff_with(2, 1));
    open(&mut app, &mut rx, &fake, &tx).await;

    app.composer = "change it".into();
    app.on_action(Action::Submit);
    settle(&mut app, &mut rx).await;

    let summary = app
        .session
        .as_ref()
        .unwrap()
        .transcript
        .blocks()
        .iter()
        .find_map(|block| match block {
            Block::TurnEnd { changes, .. } => *changes,
            _ => None,
        })
        .expect("the turn has a change summary");
    assert_eq!(summary.files, 1, "one path, three calls");
    assert_eq!(
        summary.lines,
        Some((6, 3)),
        "each record's diff is counted on its own"
    );
}

#[tokio::test]
async fn a_new_session_leaves_the_profile_to_the_agent_default() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (fake, mut app) = FakeAgent::start(tx.clone());
    boot(&fake);
    // The catalog sorts profiles by id, so `profiles.first()` is not the
    // configured default; a new session must not name one at all.
    fake.script(
        "profile.list",
        json!({"profiles": [
            {"id": "ask", "model": "mock", "reasoning": "medium"},
            {"id": "coding", "model": "mock", "reasoning": "medium"},
        ]}),
    );
    fake.script("session.create", json!({"session": session_info("s1")}));
    fake.script("session.read", display_read(0, 0));
    crate::app::tasks::bootstrap(fake.client.clone(), tx.clone()).await;
    pump(&mut app, &mut rx, |app| app.ready).await;
    app.on_action(Action::NewSession);
    pump(&mut app, &mut rx, |app| app.session.is_some()).await;

    let create = fake
        .calls("session.create")
        .into_iter()
        .next()
        .expect("a session was created");
    assert_eq!(
        create["workspace"],
        json!(app.workspace.display().to_string())
    );
    assert!(
        create.get("profile").is_none(),
        "no profile means the Agent's configured default: {create}"
    );
}

#[tokio::test]
async fn launch_flags_reach_the_new_session() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (fake, mut app) = FakeAgent::start(tx.clone());
    boot(&fake);
    app.launch = crate::app::LaunchSettings {
        profile: Some("coding".into()),
        model: Some("deep".into()),
        reasoning: Some(crate::protocol::Reasoning::High),
    };
    fake.script("session.create", json!({"session": session_info("s1")}));
    fake.script("session.read", display_read(0, 0));
    crate::app::tasks::bootstrap(fake.client.clone(), tx.clone()).await;
    pump(&mut app, &mut rx, |app| app.ready).await;
    app.on_action(Action::NewSession);
    pump(&mut app, &mut rx, |app| app.session.is_some()).await;

    let create = fake
        .calls("session.create")
        .into_iter()
        .next()
        .expect("a session was created");
    assert_eq!(create["profile"], json!("coding"));
    assert_eq!(create["model"], json!("deep"));
    assert_eq!(create["reasoning"], json!("high"));
}

#[tokio::test]
async fn launch_flags_apply_to_an_opened_session_through_session_update() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (fake, mut app) = FakeAgent::start(tx.clone());
    boot(&fake);
    app.launch = crate::app::LaunchSettings {
        profile: None,
        model: Some("deep".into()),
        reasoning: Some(crate::protocol::Reasoning::Max),
    };
    fake.script("session.open", json!({"session": session_info("s1")}));
    fake.script("session.read", display_read(0, 0));
    fake.script(
        "session.update",
        json!({"session": {"session_id": "s1", "model": "deep", "reasoning": "max"},
               "active_revision": null}),
    );
    crate::app::tasks::bootstrap(fake.client.clone(), tx.clone()).await;
    pump(&mut app, &mut rx, |app| app.ready).await;
    app.on_action(Action::OpenSession("s1".into()));
    pump(&mut app, &mut rx, |app| {
        app.session.as_ref().is_some_and(|s| s.info.model == "deep")
    })
    .await;

    let update = fake
        .calls("session.update")
        .into_iter()
        .next()
        .expect("the flags were applied to the opened session");
    assert_eq!(update["model"], json!("deep"));
    assert_eq!(update["reasoning"], json!("max"));
    assert!(app.launch.is_empty(), "the flags apply once");
}
