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

/// Pumps until `check` holds, allowing for work that retries after a delay.
async fn step_until(
    app: &mut App,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<Msg>,
    check: impl Fn(&App) -> bool,
) {
    for _ in 0..60 {
        while let Ok(Some(message)) =
            tokio::time::timeout(Duration::from_millis(5), rx.recv()).await
        {
            app.on_msg(message);
        }
        if check(app) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the app never settled (error: {:?})", app.last_error);
}

/// Pumps for a fixed window, for work that retries on a timer.
async fn pump_for(
    app: &mut App,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<Msg>,
    window: Duration,
) {
    let deadline = tokio::time::Instant::now() + window;
    while tokio::time::Instant::now() < deadline {
        while let Ok(Some(message)) =
            tokio::time::timeout(Duration::from_millis(5), rx.recv()).await
        {
            app.on_msg(message);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

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
    fake.script("session.context", json!({"session_id": "s1"}));
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
    fake.script(
        "changes.list",
        json!({"records": [], "total": 0, "complete": true}),
    );

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
    // The projection reports its floor as 0 here, so this page was the oldest
    // history; the boundary is exhausted rather than behind a summary.
    let session = app.session.as_ref().unwrap();
    assert_eq!(session.loaded_from, 0);
    assert_eq!(session.first_item, 0);
    assert!(!session.reached_floor);
}

#[tokio::test]
async fn reading_below_the_display_floor_uses_a_canonical_read() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (fake, mut app) = FakeAgent::start(tx.clone());
    boot(&fake);
    fake.script("session.open", json!({"session": session_info("s1")}));
    // The display view starts at item 10: everything below it was collapsed
    // into a compaction summary and is only reachable in the canonical view.
    fake.script("session.read", display_read(30, 10));

    open(&mut app, &mut rx, &fake, &tx).await;
    let session = app.session.as_ref().unwrap();
    // The floor is the projection's own boundary, and the tail starts at it
    // because everything above it is what the display view shows.
    assert_eq!(session.first_item, 10);
    assert_eq!(session.loaded_from, 10);
    // Nothing is left uncovered, but the items behind the floor are still
    // there, so the transcript says so instead of looking empty.
    assert!(
        matches!(
            session.transcript.blocks(),
            [Block::EarlierHistory { hidden_items: 10 }]
        ),
        "an all-covering summary still advertises the history below it: {:?}",
        session.transcript.blocks()
    );

    let canonical = crate::testing::chunk(
        9,
        json!({"type": "user", "data": {"loop_id": "l0", "kind": "prompt",
                                        "input": {"text": "collapsed question"}}}),
    );
    fake.script_sequence(
        "session.read",
        vec![
            // The canonical probe learns the raw boundary.
            json!({
                "session": session_info("s1"), "items": [], "next_cursor": null,
                "total": 12, "history_revision": "h2", "captured_end": 1,
                "projection": {"revision": "p2", "first_item": 0, "covered_item_count": 0},
            }),
            json!({
                "session": session_info("s1"),
                "items": [canonical], "next_cursor": null, "total": 12,
                "history_revision": "h2", "captured_end": 1,
                "projection": {"revision": "p2", "first_item": 0, "covered_item_count": 0},
            }),
        ],
    );
    app.on_action(Action::LoadOlder);
    settle(&mut app, &mut rx).await;

    let reads = fake.calls("session.read");
    assert!(
        app.session
            .as_ref()
            .is_some_and(|session| session.reached_floor),
        "the display floor was passed"
    );
    assert!(
        reads
            .iter()
            .any(|params| params["view"] == json!("canonical")),
        "a page below the floor is read in the canonical view: {reads:?}"
    );
    let session = app.session.as_ref().unwrap();
    assert!(
        session
            .transcript
            .blocks()
            .iter()
            .any(|block| matches!(block, Block::User { text, .. } if text == "collapsed question")),
        "{:?}",
        session.transcript.blocks()
    );
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

    app.composer.set_text("change it");
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

    app.composer.set_text("change it");
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

    app.composer.set_text("change it");
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

#[tokio::test]
async fn reconciling_a_gapped_turn_keeps_the_history_loaded_before_it() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (fake, mut app) = FakeAgent::start(tx.clone());
    boot(&fake);
    fake.script("session.open", json!({"session": session_info("s1")}));
    // An already-loaded earlier turn that reconciliation must not touch. The
    // probe asks with `limit:1`, the page asks for the window, so the fixture is
    // a sequence: probe first, then the page.
    fake.script_sequence(
        "session.read",
        vec![
            display_read(2, 0),
            json!({
                "session": session_info("s1"),
                "items": [
                    crate::testing::chunk(0, json!({"type": "user", "data": {"loop_id": "l0",
                        "kind": "prompt", "input": {"text": "earlier question"}}})),
                    crate::testing::chunk(1, json!({"type": "assistant", "data": {"loop_id": "l0",
                        "request_index": 0, "model": "mock", "reasoning": "medium",
                        "content": [{"type": "text", "data": "earlier answer"}]}})),
                ],
                "next_cursor": null,
                "total": 2,
                "history_revision": "h1",
                "captured_end": 2,
                "projection": {"revision": "p1", "first_item": 0, "covered_item_count": 0},
            }),
        ],
    );
    fake.script("turn.send", json!({"turn": turn("l1")}));
    fake.script(
        "turn.wait",
        json!({"turn": turn("l1"), "outcome": {"type": "failed", "kind": "model"}, "requests": 1}),
    );
    let item = json!({
        "item": {"type": "user", "data": {"loop_id": "l1", "kind": "prompt",
                                          "input": {"text": "gapped prompt"}}}
    })
    .to_string();
    fake.script(
        "turn.result",
        json!({
            "turn": turn("l1"),
            "outcome": {"type": "failed", "kind": "model"},
            "total": 1,
            "items": [{"index": 2, "offset": 0, "total_bytes": item.len(),
                       "encoding": "utf8_json", "data": item, "complete": true}],
            "next_cursor": null,
        }),
    );
    fake.script(
        "changes.list",
        json!({"records": [], "total": 0, "complete": true}),
    );
    open(&mut app, &mut rx, &fake, &tx).await;
    app.composer.set_text("gapped prompt");
    app.on_action(Action::Submit);
    settle(&mut app, &mut rx).await;

    let session = app.session.as_ref().unwrap();
    let prompts: Vec<&str> = session
        .transcript
        .blocks()
        .iter()
        .filter_map(|block| match block {
            Block::User { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        prompts,
        vec!["earlier question", "gapped prompt"],
        "reconciliation must replace only the finished turn's region"
    );
    let answers: Vec<&str> = session
        .transcript
        .blocks()
        .iter()
        .filter_map(|block| match block {
            Block::Assistant { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(answers, vec!["earlier answer"], "loaded history is kept");
    // The turn-end line belongs after the reconciled items.
    let last = session.transcript.blocks().last().unwrap();
    assert!(
        matches!(last, Block::TurnEnd { loop_id, .. } if loop_id == "l1"),
        "the turn ends last: {last:?}"
    );
}

#[tokio::test]
async fn an_automatic_compaction_after_a_turn_is_reported() {
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
    fake.script(
        "changes.list",
        json!({"records": [], "total": 0, "complete": true}),
    );
    // The Agent compacts automatically during the turn; it has no response of
    // its own, so only the context refresh after the turn can reveal it.
    let auto = json!({
        "session_id": "s1",
        "last_result": {"origin": "auto", "operation_id": "lup_1",
                        "status": "compacted", "before_tokens": 41000,
                        "after_tokens": 12000},
        "budget": {"estimated_history_tokens": 12000},
    });
    fake.script("session.context", auto);
    open(&mut app, &mut rx, &fake, &tx).await;

    app.composer.set_text("compact me");
    app.on_action(Action::Submit);
    settle(&mut app, &mut rx).await;

    let markers: Vec<String> = app
        .session
        .as_ref()
        .unwrap()
        .transcript
        .blocks()
        .iter()
        .filter_map(|block| match block {
            Block::Compaction { operation_id, .. } => Some(operation_id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        markers,
        vec!["auto-lup_1"],
        "an automatic compaction is reported after the turn"
    );
}

#[tokio::test]
async fn one_compaction_is_reported_once_however_often_it_is_seen() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (fake, mut app) = FakeAgent::start(tx.clone());
    boot(&fake);
    fake.script("session.open", json!({"session": session_info("s1")}));
    fake.script("session.read", display_read(0, 0));
    let auto = json!({
        "session_id": "s1",
        "last_result": {"origin": "auto", "operation_id": "lup_1",
                        "status": "compacted", "before_tokens": 41000,
                        "after_tokens": 12000},
        "budget": {"estimated_history_tokens": 12000},
    });
    fake.script("session.context", auto.clone());
    fake.script(
        "session.compact",
        json!({
            "origin": "manual", "operation_id": "tui-1", "status": "compacted",
            "before_tokens": 12000, "after_tokens": 4000,
        }),
    );
    open(&mut app, &mut rx, &fake, &tx).await;

    app.on_action(Action::Compact);
    settle(&mut app, &mut rx).await;
    // A later refresh reports the same automatic compaction again.
    app.on_msg(Msg::Op(Box::new(OpDone {
        session: Some("s1".into()),
        epoch: app.session.as_ref().unwrap().epoch,
        kind: OpKind::Context(Ok(serde_json::from_value(auto).unwrap())),
    })));

    let markers: Vec<String> = app
        .session
        .as_ref()
        .unwrap()
        .transcript
        .blocks()
        .iter()
        .filter_map(|block| match block {
            Block::Compaction { operation_id, .. } => Some(operation_id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        markers,
        vec!["auto-lup_1".to_owned(), "tui-1".to_owned()],
        "one marker per compaction, manual before automatic on the same open"
    );
}

#[tokio::test]
async fn a_refused_send_takes_back_its_prompt_and_queues_the_text() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (fake, mut app) = FakeAgent::start(tx.clone());
    boot(&fake);
    fake.script("session.open", json!({"session": session_info("s1")}));
    fake.script("session.read", display_read(0, 0));
    // Post-turn compaction still owns the session when the send arrives.
    fake.script_error("turn.send", -32003, "session busy", "session_busy");
    fake.script(
        "session.state",
        json!({"session_id": "s1", "status": "busy"}),
    );
    open(&mut app, &mut rx, &fake, &tx).await;

    app.composer.set_text("next thing");
    app.on_action(Action::Submit);
    settle(&mut app, &mut rx).await;

    let session = app.session.as_ref().unwrap();
    assert!(
        !session
            .transcript
            .blocks()
            .iter()
            .any(|block| matches!(block, Block::User { .. })),
        "a refused send must not stay in the transcript: {:?}",
        session.transcript.blocks()
    );
    assert_eq!(
        session
            .queue
            .iter()
            .map(|queued| queued.text.as_str())
            .collect::<Vec<_>>(),
        vec!["next thing"],
        "the text is queued rather than lost"
    );
    assert!(app.waiting_for_compaction);
}

#[tokio::test]
async fn a_send_refused_for_another_reason_returns_the_text_to_the_composer() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (fake, mut app) = FakeAgent::start(tx.clone());
    boot(&fake);
    fake.script("session.open", json!({"session": session_info("s1")}));
    fake.script("session.read", display_read(0, 0));
    fake.script_error(
        "turn.send",
        -32015,
        "history too large",
        "history_too_large",
    );
    open(&mut app, &mut rx, &fake, &tx).await;

    app.composer.set_text("too much");
    app.on_action(Action::Submit);
    settle(&mut app, &mut rx).await;

    assert_eq!(app.composer.text(), "too much", "the draft comes back");
    let session = app.session.as_ref().unwrap();
    assert!(
        !session
            .transcript
            .blocks()
            .iter()
            .any(|block| matches!(block, Block::User { .. }))
    );
    assert_eq!(app.last_error.as_deref(), Some("history too large"));
}

#[tokio::test]
async fn a_busy_then_busy_then_idle_session_retries_until_it_sends_once() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (fake, mut app) = FakeAgent::start(tx.clone());
    boot(&fake);
    fake.script("session.open", json!({"session": session_info("s1")}));
    fake.script("session.read", display_read(0, 0));
    // Post-turn compaction owns the session for two polls, then frees it.
    fake.script_sequence(
        "session.state",
        vec![
            json!({"session_id": "s1", "status": "busy",
                   "compaction": {"operation_id": "auto-l1", "phase": "summarizing",
                                  "covered_item_count": 4, "retained_item_count": 1}}),
            json!({"session_id": "s1", "status": "busy",
                   "compaction": {"operation_id": "auto-l1", "phase": "committing",
                                  "covered_item_count": 4, "retained_item_count": 1}}),
            json!({"session_id": "s1", "status": "idle"}),
        ],
    );
    fake.script("turn.send", json!({"turn": turn("l2")}));
    fake.script(
        "turn.wait",
        json!({"turn": turn("l2"), "outcome": {"type": "completed"}}),
    );
    fake.script(
        "changes.list",
        json!({"records": [], "total": 0, "complete": true}),
    );
    open(&mut app, &mut rx, &fake, &tx).await;

    // The first send is refused: post-turn compaction still runs.
    fake.script_error("turn.send", -32003, "session busy", "session_busy");
    app.composer.set_text("the important text");
    app.on_action(Action::Submit);
    settle(&mut app, &mut rx).await;
    assert!(app.waiting_for_compaction, "the retry loop is running");

    // The Agent frees the session; the queue must flush exactly once.
    fake.script("turn.send", json!({"turn": turn("l2")}));
    app.on_msg(Msg::Op(Box::new(OpDone {
        session: Some("s1".into()),
        epoch: app.session.as_ref().unwrap().epoch,
        kind: OpKind::State(Ok(serde_json::from_value(json!({
            "session_id": "s1", "status": "idle"
        }))
        .unwrap())),
    })));
    settle(&mut app, &mut rx).await;

    let sends: Vec<Value> = fake.calls("turn.send");
    let texts: Vec<&str> = sends
        .iter()
        .filter_map(|params| params["text"].as_str())
        .collect();
    assert_eq!(
        texts,
        vec!["the important text", "the important text"],
        "one refused send, then exactly one real send"
    );
    assert!(!app.waiting_for_compaction, "the latch is released");
    let session = app.session.as_ref().unwrap();
    let prompts = session
        .transcript
        .blocks()
        .iter()
        .filter(|block| matches!(block, Block::User { .. }))
        .count();
    assert_eq!(prompts, 1, "one prompt for one turn, not a phantom");
}

#[tokio::test]
async fn a_running_loop_is_adopted_from_state_and_ends_through_turn_wait() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (fake, mut app) = FakeAgent::start(tx.clone());
    boot(&fake);
    fake.script("session.open", json!({"session": session_info("s1")}));
    fake.script("session.read", display_read(0, 0));
    // The session was already running a loop when it was opened.
    fake.script(
        "session.state",
        json!({"session_id": "s1", "status": "running",
               "active_loop": {"loop_id": "l9", "request_index": 1}}),
    );
    fake.script(
        "turn.wait",
        json!({"turn": turn("l9"), "outcome": {"type": "completed"}, "requests": 2}),
    );
    fake.script(
        "changes.list",
        json!({"records": [], "total": 0, "complete": true}),
    );
    open(&mut app, &mut rx, &fake, &tx).await;

    // A state refresh is what reveals the running loop on adopt.
    app.on_msg(Msg::Op(Box::new(OpDone {
        session: Some("s1".into()),
        epoch: app.session.as_ref().unwrap().epoch,
        kind: OpKind::State(Ok(serde_json::from_value(json!({
            "session_id": "s1", "status": "running",
            "active_loop": {"loop_id": "l9", "request_index": 1}
        }))
        .unwrap())),
    })));
    settle(&mut app, &mut rx).await;

    assert!(
        fake.calls("turn.wait")
            .iter()
            .any(|params| params["loop_id"] == json!("l9")),
        "an adopted loop registers its own wait"
    );
    let session = app.session.as_ref().unwrap();
    assert!(!session.is_busy(), "the adopted turn ended");
    assert!(
        matches!(
            session.transcript.blocks().last(),
            Some(Block::TurnEnd { loop_id, .. }) if loop_id == "l9"
        ),
        "{:?}",
        session.transcript.blocks()
    );
}

#[tokio::test]
async fn a_capacity_refusal_on_turn_wait_is_retried() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (fake, mut app) = FakeAgent::start(tx.clone());
    boot(&fake);
    fake.script("session.open", json!({"session": session_info("s1")}));
    fake.script("session.read", display_read(0, 0));
    fake.script("turn.send", json!({"turn": turn("l1")}));
    // Every deferred slot was taken when the wait was registered.
    fake.script_error_then(
        "turn.wait",
        -32019,
        "no deferred slot",
        "resource_exhausted",
        json!({"turn": turn("l1"), "outcome": {"type": "completed"}}),
    );
    fake.script(
        "changes.list",
        json!({"records": [], "total": 0, "complete": true}),
    );
    open(&mut app, &mut rx, &fake, &tx).await;

    app.composer.set_text("retry the wait");
    app.on_action(Action::Submit);
    // The refusal is a condition, not a verdict: the retry registers the wait
    // again and the turn then ends normally.
    step_until(&mut app, &mut rx, |app| {
        !app.session
            .as_ref()
            .is_some_and(|session| session.is_busy())
    })
    .await;
    assert!(
        fake.calls("turn.wait").len() >= 2,
        "the wait was registered again"
    );
    let session = app.session.as_ref().unwrap();
    assert!(matches!(
        session.transcript.blocks().last(),
        Some(Block::TurnEnd { loop_id, .. }) if loop_id == "l1"
    ));
}

#[tokio::test]
async fn a_stale_pin_during_the_tail_load_restarts_with_a_fresh_probe() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (fake, mut app) = FakeAgent::start(tx.clone());
    boot(&fake);
    fake.script("session.open", json!({"session": session_info("s1")}));
    // The probe succeeds; the page continuation is refused as stale, so the
    // retry probes again and reads the window.
    let error = json!({"error": {"code": -32005, "message": "stale pin",
                                 "data": {"kind": "invalid_state"}}});
    fake.script_sequence(
        "session.read",
        vec![
            display_read(6, 0),
            error,
            display_read(6, 0),
            json!({
                "session": session_info("s1"),
                "items": [crate::testing::chunk(0, json!({"type": "user", "data": {
                    "loop_id": "l0", "kind": "prompt", "input": {"text": "recovered"},
                }}))],
                "next_cursor": null, "total": 6,
                "history_revision": "h2", "captured_end": 2,
                "projection": {"revision": "p2", "first_item": 0, "covered_item_count": 0},
            }),
        ],
    );

    open(&mut app, &mut rx, &fake, &tx).await;

    assert_eq!(app.last_error, None, "a stale pin is never surfaced");
    let reads = fake.calls("session.read");
    assert_eq!(reads.len(), 4, "probe, refused page, fresh probe, page");
    assert!(
        reads[2].get("history_revision").is_none(),
        "the retry probes again rather than reusing the stale pin: {reads:?}"
    );
    let session = app.session.as_ref().unwrap();
    assert!(
        session
            .transcript
            .blocks()
            .iter()
            .any(|block| matches!(block, Block::User { text, .. } if text == "recovered")),
        "{:?}",
        session.transcript.blocks()
    );
}

#[tokio::test]
async fn a_compaction_reported_while_idle_keeps_its_progress() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (fake, mut app) = FakeAgent::start(tx.clone());
    boot(&fake);
    fake.script("session.open", json!({"session": session_info("s1")}));
    fake.script("session.read", display_read(0, 0));
    open(&mut app, &mut rx, &fake, &tx).await;

    // The Agent publishes `idle` while a post-turn compaction still owns the
    // session; the progress must not be erased.
    app.on_msg(Msg::Op(Box::new(OpDone {
        session: Some("s1".into()),
        epoch: app.session.as_ref().unwrap().epoch,
        kind: OpKind::State(Ok(serde_json::from_value(json!({
            "session_id": "s1", "status": "idle",
            "compaction": {"operation_id": "auto-l1", "phase": "merging",
                           "covered_item_count": 8, "retained_item_count": 2}
        }))
        .unwrap())),
    })));

    let session = app.session.as_ref().unwrap();
    let progress = session.compaction.as_ref().expect("the progress is kept");
    assert_eq!(progress.phase, crate::protocol::CompactionPhase::Merging);
    assert_eq!(app.status_label(), Some("Compacting context"));
}

#[tokio::test]
async fn recalling_a_queued_message_never_overwrites_the_draft() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (fake, mut app) = FakeAgent::start(tx.clone());
    boot(&fake);
    fake.script("session.open", json!({"session": session_info("s1")}));
    fake.script("session.read", display_read(0, 0));
    fake.script("turn.send", json!({"turn": turn("l1")}));
    fake.script_held("turn.wait", json!({"turn": turn("l1")}));
    open(&mut app, &mut rx, &fake, &tx).await;

    app.composer.set_text("first");
    app.on_action(Action::Submit);
    pump(&mut app, &mut rx, |app| app.is_busy()).await;

    // Queue one message, then start a fresh draft.
    app.composer.set_text("queued");
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
    app.on_action(Action::QueueFollowUp);
    app.composer.set_text("half-typed thought");

    app.on_action(Action::RecallQueued);

    assert_eq!(
        app.composer.text(),
        "half-typed thought",
        "the draft is what the user is writing, not what the queue holds"
    );
    assert_eq!(
        app.session.as_ref().unwrap().queue.len(),
        1,
        "the message stays queued for when there is room"
    );
    assert!(
        app.notices
            .iter()
            .any(|notice| notice.text.contains("Clear the draft")),
        "the refusal says why: {:?}",
        app.notices
    );
}

#[tokio::test]
async fn a_busy_send_behind_a_queued_follow_up_keeps_fifo_and_sends_each_text_once() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (fake, mut app) = FakeAgent::start(tx.clone());
    boot(&fake);
    fake.script("session.open", json!({"session": session_info("s1")}));
    fake.script("session.read", display_read(0, 0));
    // Every send is refused as busy while a compaction owns the session: the
    // refused prompt is queued, never dropped and never duplicated. Once the
    // compaction and the turn are done the queue drains in order.
    let busy = json!({"error": {"code": -32003, "message": "session is busy",
                                 "data": {"kind": "busy"}}});
    // Two refusals, then the drain is accepted once compaction ends.
    fake.script_sequence(
        "turn.send",
        vec![
            busy.clone(),
            busy,
            json!({"turn": turn("l1")}),
            json!({"turn": turn("l2")}),
            json!({"turn": turn("l3")}),
        ],
    );
    fake.script(
        "turn.wait",
        json!({"turn": {"loop_id": "l1", "status": "completed"}}),
    );
    fake.script("session.rename", json!({"session": session_info("s1")}));

    open(&mut app, &mut rx, &fake, &tx).await;

    // Two messages queued while busy, then one more that is refused: three
    // items wait, and the drain must take them in the order they were queued.
    app.composer.set_text("queued first");
    app.on_action(Action::QueueFollowUp);
    app.composer.set_text("queued second");
    app.on_action(Action::QueueFollowUp);
    app.composer.set_text("typed third");
    app.on_action(Action::Submit);
    pump_for(&mut app, &mut rx, Duration::from_millis(1600)).await;

    // Both texts reach the Agent exactly once, oldest first.
    let sends: Vec<String> = fake
        .calls("turn.send")
        .iter()
        .filter_map(|call| call["text"].as_str().map(str::to_owned))
        .collect();
    assert_eq!(
        sends,
        ["queued first", "queued second", "typed third"],
        "the queue drains oldest first and the refused message joins the back"
    );

    // The compaction finishes: the queued message drains without another key
    // press, and the flag that gated it is released.
    let idle = json!({"session_id": "s1", "status": "idle"});
    app.on_msg(Msg::Op(Box::new(OpDone {
        session: Some("s1".into()),
        epoch: app.session.as_ref().unwrap().epoch,
        kind: OpKind::State(Ok(serde_json::from_value(idle).unwrap())),
    })));
    step_until(&mut app, &mut rx, |_| fake.calls("turn.send").len() >= 3).await;
    let all = fake.calls("turn.send");
    let attempts: Vec<&str> = all
        .iter()
        .filter_map(|call| call["text"].as_str())
        .collect();
    assert_eq!(
        attempts[..3],
        ["queued first", "queued second", "typed third"],
        "the drain and the refusal keep the order the user typed: {attempts:?}"
    );
    assert!(
        attempts
            .iter()
            .all(|text| ["queued first", "queued second", "typed third"].contains(text)),
        "only the three real messages are ever sent: {attempts:?}"
    );
    let queued = app.session.as_ref().unwrap().queue.len();
    assert!(
        queued <= 2,
        "the queue never duplicates a refusal: {queued}"
    );
    assert!(
        !app.waiting_for_compaction,
        "the gate is released once an idle state arrives"
    );

    // A refused send never leaves a prompt behind, so the transcript cannot
    // claim a turn the Agent did not accept.
    let prompts: Vec<&str> = app
        .session
        .as_ref()
        .unwrap()
        .transcript
        .blocks()
        .iter()
        .filter_map(|block| match block {
            Block::User { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    // Only the accepted send left a prompt: the two refusals left none, and
    // the same text never appears twice.
    let mut unique = prompts.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(
        unique.len(),
        prompts.len(),
        "no message is claimed twice: {prompts:?}"
    );
    assert!(
        prompts.len() <= 1,
        "at most the one accepted send is a prompt: {prompts:?}"
    );
}

/// The earlier-history marker claims `home` loads the missing page, so the key
/// path itself must request it - not just the action the reducer handles.
#[tokio::test]
async fn ctrl_home_loads_the_earlier_page_the_marker_promises() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (fake, mut app) = FakeAgent::start(tx.clone());
    boot(&fake);
    fake.script("session.open", json!({"session": session_info("s1")}));
    fake.script("session.read", display_read(80, 0));
    open(&mut app, &mut rx, &fake, &tx).await;

    assert_eq!(app.session.as_ref().unwrap().loaded_from, 20, "tail loaded");
    let reads = fake.calls("session.read").len();

    // No layout rows exist for a one-screen display page, which is exactly the
    // case where the older items were unreachable.
    app.on_key(KeyEvent::new(KeyCode::Home, KeyModifiers::CONTROL));
    pump(&mut app, &mut rx, |app| {
        !app.session.as_ref().unwrap().loading_older
    })
    .await;

    assert!(
        fake.calls("session.read").len() > reads,
        "the key asks for the page instead of only moving the view: {:?}",
        fake.methods()
    );
}

/// A page refused for a stale pin must not cost the reader what is already
/// loaded: one retry against a fresh pin recovers the page, and if it cannot,
/// the transcript stays as it was.
#[tokio::test]
async fn a_stale_older_page_retries_and_keeps_the_loaded_history() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (fake, mut app) = FakeAgent::start(tx.clone());
    boot(&fake);
    fake.script("session.open", json!({"session": session_info("s1")}));
    fake.script("session.read", display_read(80, 0));
    open(&mut app, &mut rx, &fake, &tx).await;

    // The reader is looking at something in the loaded window.
    app.scroll = Scroll::Anchor {
        block: 2,
        offset: 1,
    };
    let before = app.session.as_ref().unwrap().transcript.blocks().to_vec();
    assert!(!before.is_empty(), "the tail is on screen");

    // The probe succeeds, the window read finds the pin stale, and the retry
    // sees a fresh pin: the page arrives anyway. `script_error_then` answers with
    // the refusal and then every later call with the page, which is the retry.
    let page = json!({
        "session": session_info("s1"),
        "items": [crate::testing::chunk(0, json!({"type": "user", "data": {
            "loop_id": "l0", "kind": "prompt", "input": {"text": "recovered item"},
        }}))],
        "next_cursor": null, "total": 80,
        "history_revision": "h1", "captured_end": 1,
        "projection": {"revision": "p1", "first_item": 0, "covered_item_count": 0},
    });
    // The probe is refused, then the retried probe and its window both succeed.
    fake.script_sequence(
        "session.read",
        vec![
            json!({"error": {"code": crate::protocol::INVALID_STATE,
                             "message": "stale pin", "data": {"kind": "invalid_state"}}}),
            page.clone(),
            page,
        ],
    );
    app.on_action(Action::LoadOlder);
    pump(&mut app, &mut rx, |app| {
        !app.session.as_ref().unwrap().loading_older
    })
    .await;

    assert!(
        !app.session.as_ref().unwrap().loading_older,
        "the flag is released so paging can be tried again"
    );
    // The recovered item was prepended and it was the last one, so no marker
    // goes above it: the row that was replaced and the row that arrived cancel
    // out, and the reader's block is still the third one on screen.
    assert_eq!(
        app.scroll,
        Scroll::Anchor {
            block: 2,
            offset: 1
        },
        "the reader's anchor follows the block it was reading"
    );
    assert!(
        !matches!(
            app.session.as_ref().unwrap().transcript.blocks().first(),
            Some(Block::EarlierHistory { .. })
        ),
        "nothing is left behind the window, so nothing claims to be: {:?}",
        app.session.as_ref().unwrap().transcript.blocks()
    );
    let text = format!("{:?}", app.session.as_ref().unwrap().transcript.blocks());
    assert!(
        text.contains("recovered item"),
        "the retried page lands in the transcript: {text}"
    );
}

/// With the retry spent, a still-stale page leaves the transcript alone and says
/// so, instead of reloading the tail over the reader's position.
#[tokio::test]
async fn an_exhausted_stale_probe_keeps_the_transcript_and_notices() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (fake, mut app) = FakeAgent::start(tx.clone());
    boot(&fake);
    fake.script("session.open", json!({"session": session_info("s1")}));
    fake.script("session.read", display_read(80, 0));
    open(&mut app, &mut rx, &fake, &tx).await;

    app.scroll = Scroll::Anchor {
        block: 1,
        offset: 0,
    };
    let before = format!("{:?}", app.session.as_ref().unwrap().transcript.blocks());

    fake.script_error(
        "session.read",
        crate::protocol::INVALID_STATE,
        "stale pin",
        "invalid_state",
    );
    let reads = fake.calls("session.read").len();
    app.on_action(Action::LoadOlder);
    pump(&mut app, &mut rx, |app| {
        !app.session.as_ref().unwrap().loading_older
    })
    .await;

    assert_eq!(
        format!("{:?}", app.session.as_ref().unwrap().transcript.blocks()),
        before,
        "the loaded window is kept, not replaced by a tail reload"
    );
    assert_eq!(
        app.scroll,
        Scroll::Anchor {
            block: 1,
            offset: 0
        }
    );
    assert_eq!(
        fake.calls("session.read").len() - reads,
        2,
        "one probe and one retry, not an unbounded loop: {:?}",
        fake.methods()
    );
    assert!(
        app.notices
            .iter()
            .any(|notice| notice.text.contains("older history")),
        "the reader is told why nothing came: {:?}",
        app.notices.iter().map(|n| &n.text).collect::<Vec<_>>()
    );
}

/// A real automatic compaction reports coverage but no token figures. The
/// transcript must still say something, and the history below the floor must
/// stay reachable instead of the session looking empty.
#[tokio::test]
async fn an_automatic_compaction_that_covers_everything_still_advertises_history() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (fake, mut app) = FakeAgent::start(tx.clone());
    boot(&fake);
    fake.script("session.open", json!({"session": session_info("s1")}));
    // The display projection starts at the floor: 177 items were all absorbed.
    fake.script("session.read", display_read(177, 177));
    open(&mut app, &mut rx, &fake, &tx).await;

    let session = app.session.as_ref().unwrap();
    assert!(
        matches!(
            session.transcript.blocks(),
            [Block::EarlierHistory { hidden_items: 177 }]
        ),
        "an empty display tail still points at the history: {:?}",
        session.transcript.blocks()
    );
    assert_eq!(session.loaded_from, 177, "the next page starts below it");
}

/// Loading older history while a turn streams must not move the boundary that
/// turn's durable items replace: the region is stated as an index, so every
/// prepend has to rebase it or the reconcile truncates what the user just loaded.
#[tokio::test]
async fn older_history_loaded_during_a_turn_survives_the_turns_reconcile() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (fake, mut app) = FakeAgent::start(tx.clone());
    boot(&fake);
    fake.script("session.open", json!({"session": session_info("s1")}));
    // The tail starts at item 40, so 40 items wait behind it.
    fake.script("session.read", display_read(100, 0));
    open(&mut app, &mut rx, &fake, &tx).await;
    assert_eq!(app.session.as_ref().unwrap().loaded_from, 40);

    // A prompt is on screen and its turn is streaming into the transcript.
    fake.script("turn.send", json!({"turn": turn("l1")}));
    fake.script("session.rename", json!({"session": session_info("s1")}));
    app.composer.set_text("streaming question");
    app.on_action(Action::Submit);
    pump(&mut app, &mut rx, |app| {
        app.session.as_ref().unwrap().live.is_some()
    })
    .await;
    pump_for(&mut app, &mut rx, Duration::from_millis(120)).await;
    let start = app.session.as_ref().unwrap().live.as_ref().unwrap().start;
    let live_blocks = app.session.as_ref().unwrap().transcript.len() - start;
    assert!(live_blocks > 0, "the live turn occupies rows");

    // The user is reading something while the page arrives.
    app.scroll = Scroll::Anchor {
        block: 2,
        offset: 1,
    };

    // The user scrolls up and loads a page of older history mid-turn.
    let older = json!({
        "session": session_info("s1"),
        "items": [
            crate::testing::chunk(0, json!({"type": "user", "data": {
                "loop_id": "l0", "kind": "prompt", "input": {"text": "history one"}}})),
            crate::testing::chunk(1, json!({"type": "assistant", "data": {
                "loop_id": "l0", "request_index": 0, "model": "mock",
                "reasoning": "medium",
                "content": [{"type": "text", "data": "history two"}]}})),
        ],
        "next_cursor": null, "total": 100,
        "history_revision": "h1", "captured_end": 1,
        "projection": {"revision": "p1", "first_item": 0, "covered_item_count": 0},
    });
    fake.script("session.read", older);
    app.on_action(Action::LoadOlder);
    pump(&mut app, &mut rx, |app| {
        !app.session.as_ref().unwrap().loading_older
    })
    .await;
    assert_eq!(
        app.scroll,
        Scroll::Anchor {
            block: 4,
            offset: 1
        },
        "the anchored row keeps its place across the prepend"
    );
    let loaded: Vec<String> = app
        .session
        .as_ref()
        .unwrap()
        .transcript
        .blocks()
        .iter()
        .filter_map(|block| match block {
            Block::User { text, .. } => Some(text.clone()),
            Block::Assistant { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        loaded,
        ["history one", "history two", "streaming question"],
        "the older page is in front of the live turn"
    );

    // The event stream dropped something, so the turn ends incomplete and its
    // durable items replace the live region.
    app.session.as_mut().unwrap().live.as_mut().unwrap().gap = true;
    app.on_msg(Msg::Op(Box::new(OpDone {
        session: Some("s1".into()),
        epoch: app.session.as_ref().unwrap().epoch,
        kind: OpKind::Waited(Box::new(Ok(crate::protocol::TurnWaitResult {
            turn: crate::protocol::TurnRef::new("s1", "l1"),
            outcome: crate::protocol::LoopOutcome::Cancelled {
                reason: crate::protocol::CancelReason::User,
            },
            usage: None,
            requests: None,
            tool_rounds: None,
            final_config_revision: None,
            persistence: None,
        }))),
    })));
    fake.script(
        "turn.result",
        json!({"turn": turn("l1"), "items": [
            crate::testing::chunk(0, json!({"type": "user", "data": {
                "loop_id": "l1", "kind": "prompt", "input": {"text": "streaming question"}}})),
        ]}),
    );
    settle(&mut app, &mut rx).await;
    assert!(
        app.session.as_ref().unwrap().reconcile_from.is_none(),
        "the turn's region was reconciled"
    );

    let after: Vec<String> = app
        .session
        .as_ref()
        .unwrap()
        .transcript
        .blocks()
        .iter()
        .filter_map(|block| match block {
            Block::User { text, .. } => Some(text.clone()),
            Block::Assistant { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        after,
        ["history one", "history two", "streaming question"],
        "the reconcile replaces only the turn, never the loaded history: {:?}",
        app.session.as_ref().unwrap().transcript.blocks()
    );
}

/// The canonical page read below a compaction floor is prepended too, so the
/// streaming turn's region has to move by that page as well.
#[tokio::test]
async fn a_canonical_page_loaded_during_a_turn_survives_the_turns_reconcile() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (fake, mut app) = FakeAgent::start(tx.clone());
    boot(&fake);
    fake.script("session.open", json!({"session": session_info("s1")}));
    // Everything above item 30 is behind the compaction summary.
    fake.script("session.read", display_read(30, 30));
    open(&mut app, &mut rx, &fake, &tx).await;
    assert_eq!(app.session.as_ref().unwrap().loaded_from, 30);

    // The send is held, so the turn's region is recorded but not yet streaming:
    // the page arrives in exactly that window.
    fake.script_held("turn.send", json!({"turn": turn("l1")}));
    fake.script("session.rename", json!({"session": session_info("s1")}));
    app.composer.set_text("streaming question");
    app.on_action(Action::Submit);
    pump(&mut app, &mut rx, |app| {
        app.session.as_ref().unwrap().next_turn_start.is_some()
    })
    .await;
    let pending = app.session.as_ref().unwrap().next_turn_start.unwrap();
    assert!(pending > 0, "the region starts after the placeholder");

    // A canonical page arrives mid-turn: no placeholder is removed, so the whole
    // page lands in front of the live region.
    let canonical = json!({
        "session": session_info("s1"),
        "items": [crate::testing::chunk(0, json!({"type": "user", "data": {
            "loop_id": "l0", "kind": "prompt", "input": {"text": "collapsed question"}}}))],
        "next_cursor": null, "total": 30,
        "history_revision": "h2", "captured_end": 1,
        "projection": {"revision": "p2", "first_item": 0, "covered_item_count": 0},
    });
    fake.script_sequence("session.read", vec![canonical.clone(), canonical]);
    app.on_action(Action::LoadOlder);
    pump(&mut app, &mut rx, |app| {
        !app.session.as_ref().unwrap().loading_older
    })
    .await;

    assert!(
        app.session
            .as_ref()
            .unwrap()
            .transcript
            .blocks()
            .iter()
            .any(|block| matches!(block, Block::User { text, .. } if text == "collapsed question")),
        "the canonical page is on screen before the turn ends: {:?}",
        app.session.as_ref().unwrap().transcript.blocks()
    );
    assert!(
        app.session.as_ref().unwrap().reached_floor,
        "the below-floor read is what was used: {:?}",
        fake.methods()
    );

    // The send answers now, so the live turn starts from the rebased region.
    fake.release("turn.send");
    pump(&mut app, &mut rx, |app| {
        app.session.as_ref().unwrap().live.is_some()
    })
    .await;
    let start = app.session.as_ref().unwrap().live.as_ref().unwrap().start;
    assert_eq!(
        start,
        pending + 1,
        "the streaming region moved with the page in front of it"
    );
    pump_for(&mut app, &mut rx, Duration::from_millis(60)).await;

    app.session.as_mut().unwrap().live.as_mut().unwrap().gap = true;
    app.on_msg(Msg::Op(Box::new(OpDone {
        session: Some("s1".into()),
        epoch: app.session.as_ref().unwrap().epoch,
        kind: OpKind::Waited(Box::new(Ok(crate::protocol::TurnWaitResult {
            turn: crate::protocol::TurnRef::new("s1", "l1"),
            outcome: crate::protocol::LoopOutcome::Cancelled {
                reason: crate::protocol::CancelReason::User,
            },
            usage: None,
            requests: None,
            tool_rounds: None,
            final_config_revision: None,
            persistence: None,
        }))),
    })));
    fake.script(
        "turn.result",
        json!({"turn": turn("l1"), "items": [
            crate::testing::chunk(0, json!({"type": "user", "data": {
                "loop_id": "l1", "kind": "prompt", "input": {"text": "streaming question"}}})),
        ]}),
    );
    settle(&mut app, &mut rx).await;

    let text: Vec<String> = app
        .session
        .as_ref()
        .unwrap()
        .transcript
        .blocks()
        .iter()
        .filter_map(|block| match block {
            Block::User { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        text,
        ["collapsed question", "streaming question"],
        "the canonical page is kept: {:?}",
        app.session.as_ref().unwrap().transcript.blocks()
    );
}

/// A compaction can raise the display floor above where the reader had paged to.
/// The next request is then a canonical read, and it must start from the raw
/// boundary rather than skipping the items between the two offsets.
#[tokio::test]
async fn a_floor_raised_past_the_loaded_window_still_reads_every_item() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (fake, mut app) = FakeAgent::start(tx.clone());
    boot(&fake);
    fake.script("session.open", json!({"session": session_info("s1")}));
    fake.script("session.read", display_read(100, 10));
    open(&mut app, &mut rx, &fake, &tx).await;
    assert_eq!(app.session.as_ref().unwrap().loaded_from, 40);

    // A page lands, and its reply reports a floor that has moved past where the
    // reader is: the compaction absorbed what was already on screen.
    fake.script(
        "session.read",
        json!({
            "session": session_info("s1"), "items": [], "next_cursor": null,
            "total": 100, "history_revision": "h2", "captured_end": 1,
            "projection": {"revision": "p2", "first_item": 60, "covered_item_count": 60},
        }),
    );
    app.on_action(Action::LoadOlder);
    pump(&mut app, &mut rx, |app| {
        !app.session.as_ref().unwrap().loading_older
    })
    .await;

    // Nothing arrived, so the reader is told, and the window is untouched. The
    // marker counts what is behind the window, so it agrees with the position
    // the next read starts from.
    let session = app.session.as_ref().unwrap();
    assert_eq!(
        session.hidden_items, session.loaded_from,
        "the marker and the read position describe the same boundary"
    );
    let marker = session
        .transcript
        .blocks()
        .first()
        .and_then(|block| match block {
            Block::EarlierHistory { hidden_items } => Some(*hidden_items),
            _ => None,
        });
    assert_eq!(
        marker,
        Some(session.loaded_from),
        "the rendered count is the read position"
    );
    assert!(
        session.loaded_from <= 40,
        "a floor above the loaded window must not advance the read position past it"
    );

    // The next request therefore reads canonical below the floor it now knows.
    fake.script_sequence(
        "session.read",
        vec![
            json!({
                "session": session_info("s1"), "items": [], "next_cursor": null,
                "total": 100, "history_revision": "h3", "captured_end": 1,
                "projection": {"revision": "p3", "first_item": 60, "covered_item_count": 60},
            }),
            json!({
                "session": session_info("s1"),
                "items": [crate::testing::chunk(0, json!({"type": "user", "data": {
                    "loop_id": "l0", "kind": "prompt", "input": {"text": "deep item"}}}))],
                "next_cursor": null, "total": 100, "history_revision": "h3",
                "captured_end": 1,
                "projection": {"revision": "p3", "first_item": 60, "covered_item_count": 60},
            }),
        ],
    );
    app.on_action(Action::LoadOlder);
    pump(&mut app, &mut rx, |app| {
        !app.session.as_ref().unwrap().loading_older
    })
    .await;
    let reads = fake.calls("session.read");
    assert!(
        reads
            .iter()
            .any(|params| params["view"] == json!("canonical")),
        "below the raised floor the read is canonical: {reads:?}"
    );
    assert!(
        app.session
            .as_ref()
            .unwrap()
            .transcript
            .blocks()
            .iter()
            .any(|block| matches!(block, Block::User { text, .. } if text == "deep item")),
        "{:?}",
        app.session.as_ref().unwrap().transcript.blocks()
    );
}

/// The canonical view pages through the same stale-pin race as the display view,
/// so it retries once too, and an unrecoverable one keeps the transcript.
#[tokio::test]
async fn a_stale_canonical_page_retries_and_keeps_the_loaded_history() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (fake, mut app) = FakeAgent::start(tx.clone());
    boot(&fake);
    fake.script("session.open", json!({"session": session_info("s1")}));
    fake.script("session.read", display_read(30, 30));
    open(&mut app, &mut rx, &fake, &tx).await;

    let page = json!({
        "session": session_info("s1"),
        "items": [crate::testing::chunk(0, json!({"type": "user", "data": {
            "loop_id": "l0", "kind": "prompt", "input": {"text": "canonical item"}}}))],
        "next_cursor": null, "total": 30, "history_revision": "h2",
        "captured_end": 1,
        "projection": {"revision": "p2", "first_item": 0, "covered_item_count": 0},
    });
    fake.script_sequence(
        "session.read",
        vec![
            json!({"error": {"code": crate::protocol::INVALID_STATE,
                             "message": "stale pin", "data": {"kind": "invalid_state"}}}),
            page.clone(),
            page,
        ],
    );
    app.on_action(Action::LoadOlder);
    pump(&mut app, &mut rx, |app| {
        !app.session.as_ref().unwrap().loading_older
    })
    .await;

    assert!(
        app.session
            .as_ref()
            .unwrap()
            .transcript
            .blocks()
            .iter()
            .any(|block| matches!(block, Block::User { text, .. } if text == "canonical item")),
        "the retried canonical page lands: {:?}",
        app.session.as_ref().unwrap().transcript.blocks()
    );
}

/// The third window: a page can land after a gapped end has queued the turn's
/// reconcile but before its durable items arrive. The queued region is an index
/// too, so the prepend has to move it or the reconcile erases the page.
#[tokio::test]
async fn a_page_landing_before_the_reconcile_still_survives_it() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (fake, mut app) = FakeAgent::start(tx.clone());
    boot(&fake);
    fake.script("session.open", json!({"session": session_info("s1")}));
    fake.script("session.read", display_read(100, 0));
    open(&mut app, &mut rx, &fake, &tx).await;

    fake.script("turn.send", json!({"turn": turn("l1")}));
    fake.script("session.rename", json!({"session": session_info("s1")}));
    app.composer.set_text("streaming question");
    app.on_action(Action::Submit);
    pump(&mut app, &mut rx, |app| {
        app.session.as_ref().unwrap().live.is_some()
    })
    .await;

    // The turn ends incomplete, so the region to replace is remembered now.
    app.session.as_mut().unwrap().live.as_mut().unwrap().gap = true;
    // The durable items are held: the read is in flight while the page arrives.
    fake.script_held(
        "turn.result",
        json!({"turn": turn("l1"), "items": [
            crate::testing::chunk(0, json!({"type": "user", "data": {
                "loop_id": "l1", "kind": "prompt", "input": {"text": "streaming question"}}})),
        ]}),
    );
    app.on_msg(Msg::Op(Box::new(OpDone {
        session: Some("s1".into()),
        epoch: app.session.as_ref().unwrap().epoch,
        kind: OpKind::Waited(Box::new(Ok(crate::protocol::TurnWaitResult {
            turn: crate::protocol::TurnRef::new("s1", "l1"),
            outcome: crate::protocol::LoopOutcome::Cancelled {
                reason: crate::protocol::CancelReason::User,
            },
            usage: None,
            requests: None,
            tool_rounds: None,
            final_config_revision: None,
            persistence: None,
        }))),
    })));
    let queued = app.session.as_ref().unwrap().reconcile_from.clone();
    assert!(queued.is_some(), "the reconcile is waiting for its items");

    // A page arrives in that window.
    let older = json!({
        "session": session_info("s1"),
        "items": [
            crate::testing::chunk(0, json!({"type": "user", "data": {
                "loop_id": "l0", "kind": "prompt", "input": {"text": "history one"}}})),
            crate::testing::chunk(1, json!({"type": "assistant", "data": {
                "loop_id": "l0", "request_index": 0, "model": "mock",
                "reasoning": "medium",
                "content": [{"type": "text", "data": "history two"}]}})),
        ],
        "next_cursor": null, "total": 100,
        "history_revision": "h1", "captured_end": 1,
        "projection": {"revision": "p1", "first_item": 0, "covered_item_count": 0},
    });
    fake.script("session.read", older);
    app.on_action(Action::LoadOlder);
    pump(&mut app, &mut rx, |app| {
        !app.session.as_ref().unwrap().loading_older
    })
    .await;

    // The queued region moved with whatever ended up in front of it: the two
    // blocks, minus the placeholder they replaced, plus one if it came back.
    let moved = app.session.as_ref().unwrap().reconcile_from.clone();
    assert_eq!(
        moved.as_ref().map(|(_, start)| *start),
        queued.as_ref().map(|(_, start)| start + 2),
        "the reconcile region followed the prepended blocks: {queued:?} -> {moved:?}"
    );

    // Now the durable items land: only the turn is replaced.
    fake.release("turn.result");
    pump(&mut app, &mut rx, |app| {
        app.session.as_ref().unwrap().reconcile_from.is_none()
    })
    .await;

    let text: Vec<String> = app
        .session
        .as_ref()
        .unwrap()
        .transcript
        .blocks()
        .iter()
        .filter_map(|block| match block {
            Block::User { text, .. } => Some(text.clone()),
            Block::Assistant { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        text,
        ["history one", "history two", "streaming question"],
        "the reconcile kept the page it landed behind: {:?}",
        app.session.as_ref().unwrap().transcript.blocks()
    );
}

/// Each canonical page replaces the marker instead of stacking under it: the
/// label states how much history is behind the window, so a page that lands
/// below it has to bring the marker down too or the reader sees a count that
/// no longer describes anything, sitting in the middle of the transcript.
#[tokio::test]
async fn every_canonical_page_refreshes_the_marker_at_the_top() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (fake, mut app) = FakeAgent::start(tx.clone());
    boot(&fake);
    fake.script("session.open", json!({"session": session_info("s1")}));
    // An all-covering compaction: the tail is empty and 60 items wait behind it.
    fake.script("session.read", display_read(60, 60));
    open(&mut app, &mut rx, &fake, &tx).await;
    assert_eq!(app.session.as_ref().unwrap().loaded_from, 60);
    assert!(
        matches!(
            app.session.as_ref().unwrap().transcript.blocks(),
            [Block::EarlierHistory { hidden_items: 60 }]
        ),
        "the reopen advertises the history below the floor"
    );

    // The reader is looking at the marker itself when the first page lands.
    app.scroll = Scroll::Anchor {
        block: 0,
        offset: 0,
    };

    for (step, (from, text)) in [(40, "page two"), (20, "page one"), (0, "page zero")]
        .into_iter()
        .enumerate()
    {
        let page = json!({
            "session": session_info("s1"),
            "items": [crate::testing::chunk(from, json!({"type": "user", "data": {
                "loop_id": "l0", "kind": "prompt", "input": {"text": text}}}))],
            "next_cursor": null, "total": 60, "history_revision": "h2",
            "captured_end": 1,
            "projection": {"revision": "p2", "first_item": 60, "covered_item_count": 60},
        });
        fake.script("session.read", page);
        app.on_action(Action::LoadOlder);
        pump(&mut app, &mut rx, |app| {
            !app.session.as_ref().unwrap().loading_older
        })
        .await;

        let session = app.session.as_ref().unwrap();
        assert_eq!(
            session.loaded_from, from,
            "step {step}: the window moved down to {text}"
        );
        // One marker, at the top, counting the boundary the page just set.
        let markers: Vec<usize> = session
            .transcript
            .blocks()
            .iter()
            .filter_map(|block| match block {
                Block::EarlierHistory { hidden_items } => Some(*hidden_items),
                _ => None,
            })
            .collect();
        assert_eq!(
            markers,
            if from == 0 { vec![] } else { vec![from] },
            "step {step}: exactly one marker, counting the new boundary: {:?}",
            session.transcript.blocks()
        );
        if from > 0 {
            assert!(
                matches!(
                    session.transcript.blocks().first(),
                    Some(Block::EarlierHistory { hidden_items }) if *hidden_items == from
                ),
                "step {step}: the fresh marker sits at the top: {:?}",
                session.transcript.blocks()
            );
        }
        // The page itself is on screen, and the marker did not displace it.
        assert!(
            session
                .transcript
                .blocks()
                .iter()
                .any(|block| matches!(block, Block::User { text: t, .. } if t == text)),
            "step {step}: the loaded page is kept: {:?}",
            session.transcript.blocks()
        );
    }
}

/// The marker is a row like any other, so a page that ends the history without
/// bringing items still removes one: the turn's region has to move back with it
/// instead of pointing one row past where it now starts.
#[tokio::test]
async fn an_exhausted_canonical_page_moves_the_turn_region_back_with_its_marker() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (fake, mut app) = FakeAgent::start(tx.clone());
    boot(&fake);
    fake.script("session.open", json!({"session": session_info("s1")}));
    fake.script("session.read", display_read(60, 60));
    open(&mut app, &mut rx, &fake, &tx).await;
    assert!(
        matches!(
            app.session.as_ref().unwrap().transcript.blocks(),
            [Block::EarlierHistory { hidden_items: 60 }]
        ),
        "the reopen put a marker above an empty tail"
    );

    // A turn is streaming while the reader pages below the floor.
    fake.script("turn.send", json!({"turn": turn("l1")}));
    fake.script("session.rename", json!({"session": session_info("s1")}));
    app.composer.set_text("streaming question");
    app.on_action(Action::Submit);
    pump(&mut app, &mut rx, |app| {
        app.session.as_ref().unwrap().live.is_some()
    })
    .await;
    let started = app.session.as_ref().unwrap().live.as_ref().unwrap().start;

    // The page comes back empty, which ends the history and drops the marker.
    fake.script(
        "session.read",
        json!({
            "session": session_info("s1"), "items": [], "next_cursor": null,
            "total": 60, "history_revision": "h2", "captured_end": 1,
            "projection": {"revision": "p2", "first_item": 60, "covered_item_count": 60},
        }),
    );
    app.on_action(Action::LoadOlder);
    pump(&mut app, &mut rx, |app| {
        !app.session.as_ref().unwrap().loading_older
    })
    .await;

    let session = app.session.as_ref().unwrap();
    assert!(
        !matches!(
            session.transcript.blocks().first(),
            Some(Block::EarlierHistory { .. })
        ),
        "nothing is left behind the window: {:?}",
        session.transcript.blocks()
    );
    assert_eq!(
        session.live.as_ref().unwrap().start,
        started - 1,
        "the turn moved back with the row that was removed"
    );
}

/// The bottom of history is not an affordance: with nothing older to load, a
/// marker would say "0 earlier items, home to load", offering a key that cannot
/// do anything. The reader has the earliest turn and that is the whole story.
#[tokio::test]
async fn the_earliest_turn_does_not_claim_earlier_items() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (fake, mut app) = FakeAgent::start(tx.clone());
    boot(&fake);
    fake.script("session.open", json!({"session": session_info("s1")}));
    // The last page of history: it starts at item 0 and has nothing behind it.
    fake.script("session.read", display_read(80, 0));
    open(&mut app, &mut rx, &fake, &tx).await;
    assert_eq!(app.session.as_ref().unwrap().loaded_from, 20);

    fake.script_sequence(
        "session.read",
        vec![
            // The probe reports the raw start of history.
            json!({
                "session": session_info("s1"), "items": [], "next_cursor": null,
                "total": 80, "history_revision": "h1", "captured_end": 1,
                "projection": {"revision": "p1", "first_item": 0, "covered_item_count": 0},
            }),
            json!({
                "session": session_info("s1"),
                "items": [crate::testing::chunk(0, json!({"type": "user", "data": {
                    "loop_id": "l0", "kind": "prompt",
                    "input": {"text": "the earliest question"}}}))],
                "next_cursor": null, "total": 80, "history_revision": "h1",
                "captured_end": 1,
                "projection": {"revision": "p1", "first_item": 0, "covered_item_count": 0},
            }),
        ],
    );
    app.on_action(Action::LoadOlder);
    pump(&mut app, &mut rx, |app| {
        app.session
            .as_ref()
            .is_some_and(|session| session.loaded_from == 0)
    })
    .await;

    let session = app.session.as_ref().unwrap();
    assert_eq!(session.loaded_from, 0, "the window reached the start");
    let blocks = session.transcript.blocks();
    assert!(
        !blocks
            .iter()
            .any(|block| matches!(block, Block::EarlierHistory { .. })),
        "no marker remains at the bottom of history: {blocks:?}"
    );
    assert!(
        blocks.iter().any(
            |block| matches!(block, Block::User { text, .. } if text == "the earliest question")
        ),
        "the earliest turn is on screen: {blocks:?}"
    );
}
