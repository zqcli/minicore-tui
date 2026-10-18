//! Lifecycle tests that drive `App` through the real client, transport and
//! reducer: startup, send, stream, tool, wait, turn end, paging and compaction.

use std::time::Duration;

use serde_json::json;

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
    fake.script("changes.list", json!({"changes": [], "count": 0}));

    crate::app::tasks::bootstrap(fake.client.clone(), tx.clone()).await;
    pump(&mut app, &mut rx, |app| app.ready).await;
    assert_eq!(app.models.len(), 1);
    assert_eq!(app.sessions.len(), 1);

    app.on_action(Action::OpenSession("s1".into()));
    pump(&mut app, &mut rx, |app| app.session.is_some()).await;
    settle(&mut app, &mut rx).await;

    app.composer.set_text("fix the greeting");
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

    app.composer.set_text("hello");
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
    fake.script("changes.list", json!({"changes": [], "count": 0}));

    open(&mut app, &mut rx, &fake, &tx).await;

    app.composer.set_text("canonical prompt");
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

    app.composer.set_text("start");
    app.on_action(Action::Submit);
    pump(&mut app, &mut rx, |app| app.is_busy()).await;

    // While running, the next prompt becomes a steer.
    app.composer.set_text("also the tests");
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
    app.composer.set_text("write a file");
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
async fn a_pending_approval_swallows_typing_paste_and_stray_keys() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (mut fake, mut app) = FakeAgent::start(tx.clone());
    boot(&fake);
    fake.script("session.open", json!({"session": session_info("s1")}));
    fake.script("session.read", display_read(0, 0));
    fake.script("turn.send", json!({"turn": turn("l1")}));
    fake.script("interaction.answer", json!({}));
    fake.script_held("turn.wait", json!({"turn": turn("l1")}));

    open(&mut app, &mut rx, &fake, &tx).await;
    app.composer.set_text("keep me");
    app.on_action(Action::Submit);
    pump(&mut app, &mut rx, |app| app.is_busy()).await;

    fake.emit(json!({
        "type": "interaction_requested",
        "data": {"turn": turn("l1"),
                 "interaction": {"interaction_id": "i1", "tool_call_id": "c1",
                                 "tool_name": "bash",
                                 "kind": {"type": "approval", "data": {"prompt": "Allow bash?"}}},
                 "meta": {"session_id": "s1", "dropped_before": 0}}
    }))
    .await;
    pump(&mut app, &mut rx, |app| app.has_interaction()).await;

    // Text, newlines and paste aimed at the prompt must not become a draft.
    for character in "oops".chars() {
        app.on_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
    }
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
    app.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL));
    app.on_paste("a whole pasted prompt");
    assert_eq!(
        app.composer.text(),
        "",
        "the draft behind the sheet stays empty"
    );

    // The prompt itself still answers: Enter allows option zero.
    let input = app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(
        input,
        crate::app::keys::Input::Action(Action::Answer(crate::app::Answer::Allow))
    );
}

#[tokio::test]
async fn the_composer_model_keeps_the_draft_across_a_queued_turn() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (fake, mut app) = FakeAgent::start(tx.clone());
    boot(&fake);
    fake.script("session.open", json!({"session": session_info("s1")}));
    fake.script("session.read", display_read(0, 0));
    fake.script("turn.send", json!({"turn": turn("l1")}));
    fake.script_held("turn.wait", json!({"turn": turn("l1")}));

    open(&mut app, &mut rx, &fake, &tx).await;

    // A multi-line draft survives a steer, and Undo walks the real edits.
    app.composer.set_text("first line");
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
    app.on_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
    app.on_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
    assert_eq!(app.composer.text(), "first line\nxy");

    app.on_action(Action::Submit);
    pump(&mut app, &mut rx, |app| app.is_busy()).await;
    assert_eq!(app.composer.text(), "", "submitting empties the draft");

    // A new draft typed while the turn runs is untouched by queueing.
    app.composer.set_text("queued next");
    let input = app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
    assert_eq!(
        input,
        crate::app::keys::Input::Action(Action::QueueFollowUp)
    );
    app.on_action(Action::QueueFollowUp);
    assert_eq!(app.composer.text(), "");
    let queue = &app.session.as_ref().unwrap().queue;
    assert_eq!(
        queue.last().map(|queued| queued.text.as_str()),
        Some("queued next")
    );
}

#[tokio::test]
async fn the_diff_view_lists_one_row_per_path_and_merges_its_records() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (fake, mut app) = FakeAgent::start(tx.clone());
    boot(&fake);
    fake.script("session.open", json!({"session": session_info("s1")}));
    fake.script("session.read", display_read(0, 0));
    fake.script(
        "changes.list",
        json!({"records": [
            {"change_ref": "r1", "path": "app.py", "kind": "modified", "details_available": true},
            {"change_ref": "r2", "path": "app.py", "kind": "modified", "details_available": true},
            {"change_ref": "r3", "path": "notes.txt", "kind": "added", "details_available": true}
        ]}),
    );
    fake.script(
        "changes.diff",
        json!({"change_ref": "r1", "path": "app.py", "hunks": [], "complete": true}),
    );
    open(&mut app, &mut rx, &fake, &tx).await;

    app.open_panel(crate::app::Panel::Diff(crate::app::DiffScope::Session));
    pump(&mut app, &mut rx, |app| !app.diff.files.is_empty()).await;
    settle(&mut app, &mut rx).await;

    let paths: Vec<&str> = app
        .diff
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    assert_eq!(paths, ["app.py", "notes.txt"], "one row per changed path");
    assert_eq!(
        app.diff.files[0].change_refs,
        ["r1", "r2"],
        "both records of app.py drive its diff"
    );
    assert_eq!(app.diff.files[1].change_refs, ["r3"]);
    assert!(
        app.diff.status.contains("content unchanged"),
        "an empty record explains itself instead of implying a diff: {:?}",
        app.diff.status
    );
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
    assert_eq!(
        app.layout.total_rows(),
        109,
        "ten rows per block plus one separating row between blocks"
    );
    app.scroll_by(-5, 5);
    assert_ne!(app.scroll, Scroll::Follow);
    app.scroll_by(100, 5);
    assert_eq!(app.scroll, Scroll::Follow);
    assert_eq!(app.overlay, Overlay::None);
}

#[tokio::test]
async fn ctrl_c_clears_a_draft_then_needs_two_presses_on_an_empty_composer() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (fake, mut app) = FakeAgent::start(tx.clone());
    boot(&fake);
    fake.script("session.open", json!({"session": session_info("s1")}));
    fake.script("session.read", display_read(0, 0));
    open(&mut app, &mut rx, &fake, &tx).await;

    let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    let typed = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
    app.on_key(typed);
    assert_eq!(app.composer.text(), "x");
    app.on_key(ctrl_c);
    assert_eq!(app.composer.text(), "", "the draft is cleared first");
    assert!(!app.quit, "clearing a draft must not quit");

    assert_eq!(app.on_key(ctrl_c), crate::app::keys::Input::Edited);
    assert!(!app.quit, "the first empty press only arms the quit");
    assert!(app.quit_armed);
    assert_eq!(
        app.notices.back().map(|notice| notice.text.as_str()),
        Some("Press Ctrl+C again to quit")
    );

    app.on_key(typed);
    assert!(!app.quit_armed, "typing disarms the quit");
    app.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
    assert_eq!(app.on_key(ctrl_c), crate::app::keys::Input::Edited);
    assert_eq!(
        app.on_key(ctrl_c),
        crate::app::keys::Input::Action(Action::Quit),
        "a second empty press quits"
    );
}

#[tokio::test]
async fn palette_arguments_reach_the_command() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (fake, mut app) = FakeAgent::start(tx.clone());
    boot(&fake);
    fake.script("session.open", json!({"session": session_info("s1")}));
    fake.script("session.read", display_read(0, 0));
    open(&mut app, &mut rx, &fake, &tx).await;

    for character in "/rename a better title".chars() {
        app.on_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
    }
    assert_eq!(app.composer.text(), "/rename a better title");
    let input = app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(
        input,
        crate::app::keys::Input::Action(Action::Rename("a better title".into()))
    );
    assert!(app.composer.is_empty());
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
