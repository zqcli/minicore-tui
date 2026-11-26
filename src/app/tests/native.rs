use crossterm::event::KeyCode;
use minicore_host_protocol::{
    CompactRequest, CompactionOutcome, CompactionState, ExecutionSettings, MessageMode,
    OperationId, SessionConfigPatch, SessionOverrides, SessionRef, TurnId, TurnOutcome,
};

use super::harness::Harness;
use crate::app::{Action, App, Msg};
use crate::history::HistoryWindow;
use crate::testing::{ModelCall, real_bounded};
use crate::transcript::Block;

async fn compaction_session(h: &mut Harness) -> SessionRef {
    let mut execution = ExecutionSettings::default();
    execution.compaction.target_percent = 1;
    let target = real_bounded(crate::app::tasks::create_session(
        &h.native.client,
        h.native.target.agent_id.clone(),
        SessionOverrides(SessionConfigPatch {
            execution: Some(execution),
            ..Default::default()
        }),
    ))
    .await
    .unwrap();
    h.open(&target).await;
    target
}

async fn compaction_history(h: &mut Harness, target: &SessionRef) {
    for count in 1..=2 {
        submit(h, &"historical context to summarize\n".repeat(400), false);
        let call = h.next_call().await;
        call.text("recorded");
        call.finish();
        h.until(|app| terminal_count(app, target) == count && !app.is_busy())
            .await;
    }
}

async fn request_compaction(h: &mut Harness, target: &SessionRef) {
    real_bounded(h.native.client.session(target.clone()).compact(
        CompactRequest {
            operation_id: OperationId::new().unwrap(),
        },
        Default::default(),
    ))
    .await
    .expect("native compaction admission");
}

fn compaction_result<'a>(app: &'a App, target: &SessionRef) -> Option<&'a CompactionOutcome> {
    match &app
        .resident
        .find(target)?
        .state
        .data
        .compaction
        .as_ref()?
        .state
    {
        CompactionState::Finished { result } => Some(result),
        _ => None,
    }
}

#[tokio::test(start_paused = true)]
async fn compaction_failure_is_visible_once_without_moving_the_reading_anchor() {
    let mut h = Harness::opened().await;
    let target = compaction_session(&mut h).await;
    compaction_history(&mut h, &target).await;
    h.app.notices.clear();
    request_compaction(&mut h, &target).await;
    let call = h.next_call().await;
    h.until(App::compacting).await;
    assert!(h.notices().is_empty(), "admission is not a terminal result");
    let anchor = crate::app::Scroll::Anchor {
        block: 0,
        offset: 1,
    };
    h.app.scroll = anchor;
    let epoch = h.app.resident.find(&target).unwrap().epoch;
    let preparing = h.app.resident.find(&target).unwrap().state.clone();
    // A finished utility response with no summary is a real asynchronous Host failure.
    call.finish();
    h.until(|app| {
        matches!(
            compaction_result(app, &target),
            Some(CompactionOutcome::Failed { .. })
        )
    })
    .await;
    let Some(CompactionOutcome::Failed { fault }) = compaction_result(&h.app, &target) else {
        unreachable!()
    };
    let expected = format!("Compaction failed: {fault}");
    assert_eq!(h.notices(), [expected.as_str()]);
    assert_eq!(h.app.scroll, anchor);
    assert!(matches!(h.app.overlay, crate::app::Overlay::None));
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(180, 30)).unwrap();
    terminal
        .draw(|frame| {
            crate::ui::draw(frame, &mut h.app);
        })
        .unwrap();
    let screen: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(
        screen.contains(&expected),
        "ordinary conversation must show the native fault"
    );
    assert_eq!(h.app.scroll, anchor);

    let finished = h.app.resident.find(&target).unwrap().state.clone();
    h.app.apply_snapshot(epoch, finished.clone());
    h.app.apply_snapshot(epoch, preparing);
    assert_eq!(h.notices(), [expected.as_str()]);
    h.app.notices.clear();
    h.app.apply_snapshot(epoch, finished.clone());
    h.app.on_action(Action::Pause);
    h.until(|app| {
        app.resident
            .find(&target)
            .unwrap()
            .state
            .data
            .controller_revision
            > finished.data.controller_revision
    })
    .await;
    assert!(
        h.notices().is_empty(),
        "a new revision with the same failure must stay quiet"
    );
    let other = h.native.target.clone();
    h.open(&other).await;
    let other_epoch = h.app.resident.current().unwrap().epoch;
    h.app.apply_snapshot(other_epoch, finished);
    assert!(
        h.app
            .resident
            .current()
            .unwrap()
            .state
            .data
            .compaction
            .is_none()
    );
    assert!(
        h.notices().is_empty(),
        "a mismatched Session must reject the snapshot"
    );
    h.open(&target).await;
    assert!(
        h.notices().is_empty(),
        "reopening must not replay the failure"
    );
    assert_eq!(h.app.scroll, anchor);
    h.finish().await;
}

#[tokio::test(start_paused = true)]
async fn compaction_failure_after_navigation_names_its_original_session() {
    let mut h = Harness::opened().await;
    let target = compaction_session(&mut h).await;
    compaction_history(&mut h, &target).await;
    let other = h.native.target.clone();
    for _ in 0..2 {
        h.open(&target).await;
        request_compaction(&mut h, &target).await;
        let call = h.next_call().await;
        h.until(App::compacting).await;
        h.open(&other).await;
        h.app.composer.set_text("unrelated draft");
        h.app.notices.clear();
        call.finish();
        for _ in 1..4 {
            h.next_call().await.finish();
        }
        h.until(|app| {
            matches!(
                compaction_result(app, &target),
                Some(CompactionOutcome::Failed { .. })
            )
        })
        .await;
        let Some(CompactionOutcome::Failed { fault }) = compaction_result(&h.app, &target) else {
            unreachable!()
        };
        let session = h.app.resident.find(&target).unwrap();
        let owner = session
            .info
            .title
            .as_deref()
            .unwrap_or(target.session_id.as_str());
        let expected = format!("\"{owner}\": Compaction failed: {fault}");
        assert_eq!(
            h.notices(),
            [expected.as_str()],
            "each new operation reports its own failure"
        );
        assert_eq!(h.app.resident.current().unwrap().address(), other);
        assert_eq!(h.app.composer.text(), "unrelated draft");
        assert!(
            h.app
                .resident
                .current()
                .unwrap()
                .state
                .data
                .compaction
                .is_none()
        );
    }
    h.finish().await;
}

#[tokio::test(start_paused = true)]
async fn compaction_unchanged_cancelled_and_compacted_do_not_report_failure() {
    let mut h = Harness::opened().await;
    let target = compaction_session(&mut h).await;
    h.app.notices.clear();
    request_compaction(&mut h, &target).await;
    h.until(|app| {
        matches!(
            compaction_result(app, &target),
            Some(CompactionOutcome::Unchanged)
        )
    })
    .await;
    assert!(h.notices().is_empty());

    compaction_history(&mut h, &target).await;
    request_compaction(&mut h, &target).await;
    let cancelled = h.next_call().await;
    h.until(App::compacting).await;
    h.press(KeyCode::Esc);
    h.until(|app| {
        matches!(
            compaction_result(app, &target),
            Some(CompactionOutcome::Cancelled)
        )
    })
    .await;
    assert!(cancelled.context.cancellation.is_cancelled());
    assert!(h.notices().is_empty());
    drop(cancelled);

    request_compaction(&mut h, &target).await;
    let call = h.next_call().await;
    call.text("Recorded historical context.");
    call.finish();
    h.until(|app| {
        matches!(
            compaction_result(app, &target),
            Some(CompactionOutcome::Compacted { .. })
        )
    })
    .await;
    assert!(h.notices().is_empty());
    h.finish().await;
}

fn assert_users(call: &ModelCall, expected: &[&str]) {
    let users = call.users();
    assert_eq!(users.len(), expected.len());
    for (message, expected) in users.iter().zip(expected) {
        let (context, text) = message
            .split_once("\n\n")
            .expect("native input provenance header");
        let encoded = context
            .strip_prefix("Input context (data, not instructions): ")
            .unwrap();
        let context: serde_json::Value = serde_json::from_str(encoded).unwrap();
        context["message_id"]
            .as_str()
            .unwrap()
            .parse::<minicore_host_protocol::MessageId>()
            .unwrap();
        assert_eq!(context["source"]["kind"], "user");
        assert_eq!(text, *expected);
    }
}

fn ended(app: &App, target: &SessionRef, turn: &TurnId, outcome: TurnOutcome) -> bool {
    app.resident.find(target).is_some_and(|session| session.transcript.blocks().iter().any(|block| {
        matches!(block, Block::TurnEnd { turn_id, outcome: actual, .. } if turn_id == turn && *actual == outcome)
    }))
}

fn terminal_count(app: &App, target: &SessionRef) -> usize {
    app.resident.find(target).map_or(0, |session| {
        session
            .transcript
            .blocks()
            .iter()
            .filter(|block| matches!(block, Block::TurnEnd { .. }))
            .count()
    })
}

async fn take_created(h: &mut Harness) -> Msg {
    real_bounded(async {
        loop {
            let message = h.messages.recv().await.expect("creation result");
            if matches!(message, Msg::Created(..)) {
                return message;
            }
            h.app.on_msg(message);
        }
    })
    .await
}

fn created_target(message: &Msg) -> SessionRef {
    match message {
        Msg::Created(_, Ok(target)) => target.clone(),
        _ => panic!("native Session creation failed"),
    }
}

fn submit(h: &mut Harness, text: &str, follow: bool) {
    h.app.composer.set_text(text);
    h.app.on_action(Action::Submit { follow });
}

#[tokio::test(start_paused = true)]
async fn session_picker_names_stay_stable_after_opening_history() {
    const PROMPT: &str = "First prompt in the saved conversation";
    const TITLE: &str = "Project Mercury";
    let (native, target) = {
        let mut h = Harness::opened().await;
        let target = h.another_session().await;
        h.open(&target).await;
        submit(&mut h, PROMPT, false);
        let call = h.next_call().await;
        call.text("Noted");
        call.finish();
        h.until(|app| terminal_count(app, &target) == 1).await;
        (h.native, target)
    };

    // A fresh frontend sees the catalog before it has opened the target's history.
    let mut h = Harness::from_native(native).await;
    let sheet = |app: &App| crate::ui::sheets::sheet(app, app.styles).unwrap();
    let id = target.session_id.as_str();
    let short = &id[..6];
    assert!(h.app.live(&target).is_none());
    h.app.composer.set_text("/sessions");
    h.press(KeyCode::Enter);
    h.app.on_paste(id);
    assert_eq!(sheet(&h.app).rows[0].label, short);
    h.press(KeyCode::Enter);
    h.until(|app| app.live(&target).is_some()).await;
    let opened = h.app.resident.current().unwrap();
    assert_eq!(opened.address(), target);
    assert_eq!(opened.info.title, None);
    assert_eq!(opened.auto_title().as_deref(), Some(PROMPT));

    h.ctrl(KeyCode::Char('r'));
    assert_eq!(sheet(&h.app).rows[0].label, short);
    h.app.on_paste(PROMPT);
    assert!(sheet(&h.app).rows.is_empty());
    h.press(KeyCode::Esc);
    h.ctrl(KeyCode::Char('r'));
    h.ctrl(KeyCode::Char('d'));
    assert!(matches!(
        &h.app.overlay,
        crate::app::Overlay::Confirm { lines, .. } if lines[0].starts_with(short)
    ));
    h.press(KeyCode::Char('n'));
    h.ctrl(KeyCode::Char('r'));
    h.ctrl(KeyCode::Char('n'));
    assert_eq!(sheet(&h.app).status.as_deref(), Some(short));
    h.type_text(TITLE);
    h.press(KeyCode::Enter);
    h.until(|app| {
        app.sessions
            .iter()
            .any(|s| s.id == target.session_id && s.title.as_deref() == Some(TITLE))
    })
    .await;
    h.app.on_paste("prj mrc");
    assert_eq!(sheet(&h.app).rows[0].label, TITLE);
    h.finish().await;
}

#[tokio::test(start_paused = true)]
async fn first_input_stays_with_its_created_session_after_navigation() {
    let mut h = Harness::opened().await;
    let existing = h.native.target.clone();
    h.app.select_session(None);
    submit(&mut h, "first input for a new session", false);
    assert!(h.app.creating_session());
    assert_eq!(
        h.app.local_input_bytes(),
        h.app
            .pending_creations
            .values()
            .next()
            .unwrap()
            .entry
            .as_ref()
            .unwrap()
            .retained_bytes()
    );
    let created = take_created(&mut h).await;
    let target = created_target(&created);
    h.open(&existing).await;
    assert!(!h.app.creating_session());
    h.app.composer.set_text("draft in existing session");
    h.app.on_msg(created);
    let call = h.next_call().await;
    assert_users(&call, &["first input for a new session"]);
    call.text("background answer");
    call.finish();
    h.until(|app| terminal_count(app, &target) == 1 && app.local_inputs.is_empty())
        .await;
    assert_eq!(h.app.resident.current().unwrap().address(), existing);
    assert_eq!(h.app.composer.text(), "draft in existing session");
    assert!(h.app.resident.find(&target).unwrap().unseen);
    let history = HistoryWindow::tail(&h.native.client.session(existing), 60)
        .await
        .unwrap();
    assert!(
        history.items.is_empty(),
        "navigation must not receive the pending first input"
    );
    assert!(h.app.pending_creations.is_empty());
    h.finish().await;
}

#[tokio::test(start_paused = true)]
async fn concurrent_creations_keep_separate_entries_when_results_arrive_out_of_order() {
    let mut h = Harness::opened().await;
    let existing = h.native.target.clone();
    h.app.select_session(None);
    submit(&mut h, "first creation input", false);
    let first = take_created(&mut h).await;
    let first_target = created_target(&first);
    h.open(&existing).await;
    h.app.select_session(None);
    submit(&mut h, "second creation input", true);
    let second = take_created(&mut h).await;
    let second_target = created_target(&second);
    assert_ne!(first_target, second_target);
    h.app.on_msg(second);
    let second_call = h.next_call().await;
    assert_users(&second_call, &["second creation input"]);
    second_call.text("second creation result");
    second_call.finish();
    h.until(|app| terminal_count(app, &second_target) == 1)
        .await;
    h.app.on_msg(first);
    let first_call = h.next_call().await;
    assert_users(&first_call, &["first creation input"]);
    first_call.text("first creation result");
    first_call.finish();
    h.until(|app| terminal_count(app, &first_target) == 1 && app.local_inputs.is_empty())
        .await;
    assert_eq!(h.app.resident.current().unwrap().address(), second_target);
    assert!(h.app.pending_creations.is_empty());
    assert_eq!(terminal_count(&h.app, &first_target), 1);
    assert_eq!(terminal_count(&h.app, &second_target), 1);
    h.finish().await;
}

#[tokio::test(start_paused = true)]
async fn launch_model_override_is_consumed_by_only_the_initial_open() {
    let mut h = Harness::opened().await;
    let initial = h.another_session().await;
    let later = h.another_session().await;
    h.app.launch.model = Some("fixture/initial-override".parse().unwrap());
    h.open(&initial).await;
    let first = h
        .native
        .client
        .session(initial)
        .info(Default::default())
        .await
        .unwrap();
    assert_eq!(first.config.model.as_str(), "fixture/initial-override");
    h.open(&later).await;
    let later = h
        .native
        .client
        .session(later)
        .info(Default::default())
        .await
        .unwrap();
    assert_eq!(later.config.model.as_str(), "fixture/gated");
    assert!(h.app.launch.model.is_none());
    h.finish().await;
}

#[tokio::test(start_paused = true)]
async fn submitted_input_streams_then_installs_one_native_terminal_result() {
    let mut h = Harness::opened().await;
    let target = h.native.target.clone();
    submit(&mut h, "stream this exact input αβ", false);
    let call = h.next_call().await;
    assert_users(&call, &["stream this exact input αβ"]);
    call.text("partial αβ");
    h.until(|app| {
        app.resident.find(&target).unwrap().transcript.blocks().iter().any(|block| {
        matches!(block, Block::Assistant { text, streaming: true, .. } if text == "partial αβ")
    })
    })
    .await;
    let turn = h
        .app
        .resident
        .find(&target)
        .unwrap()
        .state
        .data
        .active_turn_id
        .clone()
        .unwrap();
    call.text("🙂 completed");
    call.finish();
    h.until(|app| {
        let session = app.resident.find(&target).unwrap();
        ended(app, &target, &turn, TurnOutcome::Completed)
            && app.local_inputs.is_empty()
            && session.turn_reads.is_empty()
            && session.transcript.blocks().iter().any(|block| {
                matches!(block, Block::Assistant { key, streaming: false, partial: false, .. }
                    if key.turn_id() == Some(&turn))
            })
    })
    .await;
    let session = h.app.resident.find(&target).unwrap();
    let answers = session
        .transcript
        .blocks()
        .iter()
        .filter_map(|block| match block {
            Block::Assistant {
                text,
                streaming,
                partial,
                ..
            } => Some((text.as_str(), *streaming, *partial)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(answers, [("partial αβ🙂 completed", false, false)]);
    assert_eq!(terminal_count(&h.app, &target), 1);
    let result = h
        .native
        .client
        .session(target)
        .turn_result(turn, Default::default())
        .await
        .unwrap();
    assert_eq!(
        result.data.info.unwrap().outcome,
        Some(TurnOutcome::Completed)
    );
    assert!(result.data.partials.is_empty());
    h.finish().await;
}

#[tokio::test(start_paused = true)]
async fn steer_and_follow_up_use_the_hosts_distinct_queues_and_turn_boundaries() {
    let mut h = Harness::opened().await;
    let target = h.native.target.clone();
    submit(&mut h, "initial", false);
    let first = h.next_call().await;
    h.until(|app| app.resident.find(&target).unwrap().is_busy())
        .await;
    let first_loop = first.context.loop_id;
    submit(&mut h, "steer next request", false);
    submit(&mut h, "follow next turn", true);
    h.until(|app| {
        app.local_inputs.is_empty()
            && app
                .resident
                .find(&target)
                .unwrap()
                .queue
                .as_ref()
                .is_some_and(|queue| queue.next_step.len() == 1 && queue.next_turn.len() == 1)
    })
    .await;
    let queue = h
        .native
        .client
        .session(target.clone())
        .inputs(Default::default())
        .await
        .unwrap();
    assert_eq!(queue.next_step[0].input.text, "steer next request");
    assert_eq!(queue.next_step[0].input.mode, MessageMode::Steer);
    assert_eq!(queue.next_turn[0].input.text, "follow next turn");
    assert_eq!(queue.next_turn[0].input.mode, MessageMode::FollowUp);
    assert_ne!(
        queue.next_step[0].input.message_id,
        queue.next_turn[0].input.message_id
    );
    first.text("first answer");
    first.finish();
    let steering = h.next_call().await;
    assert_eq!(steering.context.loop_id, first_loop);
    assert_eq!(steering.context.request_index, 1);
    assert_users(&steering, &["initial", "steer next request"]);
    steering.text("steered answer");
    steering.finish();
    let follow = h.next_call().await;
    assert_ne!(follow.context.loop_id, first_loop);
    assert_eq!(follow.context.request_index, 0);
    assert_users(
        &follow,
        &["initial", "steer next request", "follow next turn"],
    );
    follow.text("follow-up answer");
    follow.finish();
    h.until(|app| {
        terminal_count(app, &target) == 2 && !app.resident.find(&target).unwrap().is_busy()
    })
    .await;
    let queue = h
        .native
        .client
        .session(target)
        .inputs(Default::default())
        .await
        .unwrap();
    assert!(queue.next_step.is_empty() && queue.next_turn.is_empty());
    h.finish().await;
}

#[tokio::test(start_paused = true)]
async fn cancellation_keeps_its_original_turn_and_preserves_accepted_follow_up() {
    let mut h = Harness::opened().await;
    let original = h.native.target.clone();
    submit(&mut h, "cancel this turn", false);
    let original_call = h.next_call().await;
    h.until(|app| {
        app.resident
            .find(&original)
            .unwrap()
            .state
            .data
            .active_turn_id
            .is_some()
    })
    .await;
    let original_turn = h
        .app
        .resident
        .find(&original)
        .unwrap()
        .state
        .data
        .active_turn_id
        .clone()
        .unwrap();
    submit(&mut h, "accepted successor", true);
    h.until(|app| {
        app.local_inputs.is_empty()
            && app
                .resident
                .find(&original)
                .unwrap()
                .state
                .data
                .next_turn
                .len()
                == 1
    })
    .await;
    let other = h.another_session().await;
    h.open(&other).await;
    submit(&mut h, "other session keeps running", false);
    let other_call = h.next_call().await;
    h.until(|app| {
        app.resident
            .find(&other)
            .unwrap()
            .state
            .data
            .active_turn_id
            .is_some()
    })
    .await;
    let other_turn = h
        .app
        .resident
        .find(&other)
        .unwrap()
        .state
        .data
        .active_turn_id
        .clone()
        .unwrap();
    h.open(&original).await;
    h.press(KeyCode::Esc);
    h.open(&other).await;
    let successor = h.next_call().await;
    assert_users(&successor, &["cancel this turn", "accepted successor"]);
    h.until(|app| ended(app, &original, &original_turn, TurnOutcome::Cancelled))
        .await;
    assert!(original_call.context.cancellation.is_cancelled());
    assert!(!other_call.context.cancellation.is_cancelled());
    assert_eq!(h.app.resident.current().unwrap().address(), other);
    assert_eq!(
        h.app
            .resident
            .current()
            .unwrap()
            .state
            .data
            .active_turn_id
            .as_ref(),
        Some(&other_turn)
    );
    successor.text("successor survived cancellation");
    successor.finish();
    other_call.text("other session completed");
    other_call.finish();
    h.until(|app| {
        terminal_count(app, &original) == 2
            && ended(app, &other, &other_turn, TurnOutcome::Completed)
    })
    .await;
    assert!(h.app.resident.find(&original).unwrap().unseen);
    h.finish().await;
}
