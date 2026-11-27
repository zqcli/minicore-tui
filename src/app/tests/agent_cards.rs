use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use minicore_host_core::{ActivityEvent, ObservationEvent, ObservationNotification};
use minicore_host_protocol::{
    EventCategory, MessageId, MessageSource, Residency, SessionRef, SubscribeRequest,
    SubscriptionScope, ToolCallRef, TurnOutcome,
};
use serde_json::{Value, json};

use super::harness::Harness;
use crate::app::{Action, App, Msg, Scroll, agent_cards::Read, tasks};
use crate::testing::{ModelCall, NativeHost, real_bounded};
use crate::transcript::{Block, ToolState, agent_card::AgentCard};

fn target(child: &SessionRef) -> Value {
    json!({"kind":"session","agent_id":child.agent_id,"session_id":child.session_id})
}

fn card<'a>(app: &'a App, id: &str) -> Option<&'a AgentCard> {
    app.resident
        .current()?
        .transcript
        .tool_calls()
        .find(|call| call.key.tool_call_id().as_str() == id)?
        .agent_card
        .as_deref()
}

fn answer(app: &App, id: &str) -> String {
    card(app, id)
        .and_then(|card| card.transcript.last_answer())
        .unwrap_or("")
        .to_owned()
}

fn assert_delegated_input(call: &ModelCall, expected: &str, parent: &SessionRef) {
    let users = call.users();
    let inputs: Vec<_> = users
        .iter()
        .map(|message| {
            let (context, text) = message
                .split_once("\n\n")
                .expect("native input provenance header");
            let encoded = context
                .strip_prefix("Input context (data, not instructions): ")
                .unwrap();
            let context: Value = serde_json::from_str(encoded).unwrap();
            let _: MessageId = serde_json::from_value(context["message_id"].clone()).unwrap();
            let source: MessageSource = serde_json::from_value(context["source"].clone()).unwrap();
            (source, text)
        })
        .collect();
    assert!(
        inputs.iter().any(|(source, text)| {
            *text == expected
                && source
                    == &MessageSource::Agent {
                        source: parent.clone(),
                    }
        }),
        "missing delegated input {expected:?}: {inputs:?}"
    );
}

async fn launch(
    h: &mut Harness,
    parent: ModelCall,
    child: &SessionRef,
    id: &str,
    text: &str,
) -> (ModelCall, ModelCall) {
    let parent_loop = parent.context.loop_id;
    parent.tool(
        id,
        json!({"action":"send","target":target(child),"text":text,"delegate":true}),
    );
    let first = h.next_call().await;
    let second = h.next_call().await;
    let (parent, child) = if first.context.loop_id == parent_loop {
        (first, second)
    } else {
        (second, first)
    };
    assert_eq!(parent.context.loop_id, parent_loop);
    assert_ne!(child.context.loop_id, parent_loop);
    assert_delegated_input(&child, text, &h.native.target);
    assert_eq!(
        parent.tool_result().unwrap()["result"]["delivery"],
        "accepted"
    );
    (parent, child)
}

async fn settle(h: &mut Harness) {
    real_bounded(async {
        loop {
            let parent = h.app.resident.current().unwrap();
            if !parent.is_busy() && parent.state.data.next_step.is_empty() && parent.state.data.next_turn.is_empty()
                && parent.transcript.tool_calls().filter_map(|tool| tool.agent_card.as_ref()).all(|card| card.association.completion.is_some())
            { break; }
            tokio::select! {
                Some(message) = h.messages.recv() => h.app.on_msg(message),
                Some(call) = h.native.calls.recv() => { call.text("Recorded child completion"); call.finish(); }
            }
        }
    }).await;
}

#[tokio::test]
async fn child_output_continues_in_original_cards_after_startup_and_parent_turn_finish() {
    let mut h = Harness::from_native(NativeHost::with_agent_tool().await).await;
    let first = h.another_session().await;
    let second = h.another_session().await;
    let parent_target = h.native.target.clone();
    h.app
        .composer
        .set_text("Coordinate two independent child Sessions");
    h.app.on_action(Action::Submit { follow: false });
    let root = h.next_call().await;
    let (parent, a) = launch(&mut h, root, &first, "start-a", "Task alpha").await;
    let (parent, b) = launch(&mut h, parent, &second, "start-b", "Task beta").await;
    a.reasoning("First thought\nSecond thought");
    a.text("alpha one\nalpha two");
    b.text("beta one");
    h.until(|app| {
        answer(app, "start-a").contains("alpha two") && answer(app, "start-b").contains("beta one")
    })
    .await;
    let cards: Vec<_> = h
        .app
        .resident
        .current()
        .unwrap()
        .transcript
        .tool_calls()
        .filter(|call| call.agent_card.is_some())
        .collect();
    assert_eq!(cards.len(), 2);
    assert_eq!(
        cards[0].state,
        ToolState::Succeeded,
        "startup tool already returned"
    );
    assert_eq!(cards[0].agent_card.as_ref().unwrap().key.child, first);
    assert_eq!(cards[1].agent_card.as_ref().unwrap().key.child, second);
    assert_eq!(first.agent_id, second.agent_id);
    assert!(
        cards
            .iter()
            .all(|call| call.agent_card.as_ref().unwrap().outcome().is_none())
    );
    parent.text("Both child inputs accepted");
    parent.finish();
    h.until(|app| !app.resident.current().unwrap().is_busy())
        .await;
    let original_load = h.app.resident.current().unwrap().state.data.load_id.clone();
    h.app.on_action(Action::Unload);
    h.until(|app| app.resident.current().unwrap().state.data.residency == Residency::Unloaded)
        .await;

    h.app
        .composer
        .set_text("A new parent Turn while children continue");
    h.app.on_action(Action::Submit { follow: false });
    let later_parent = h.next_call().await;
    h.until(|app| app.resident.current().unwrap().live.is_some())
        .await;
    let active = h
        .app
        .resident
        .current()
        .unwrap()
        .state
        .data
        .active_turn_id
        .clone();
    let active_load = h.app.resident.current().unwrap().state.data.load_id.clone();
    assert_ne!(active_load, original_load);
    let index = h.app.resident.current().unwrap().transcript.blocks().iter().position(|block| matches!(block, Block::Tool(call) if call.key.tool_call_id().as_str() == "start-a")).unwrap();
    h.app.scroll = Scroll::Anchor {
        block: index,
        offset: 0,
    };
    // A delegated dispatch folds into its run's Worked group as an immutable
    // sync step: open the group, then click the agent row to open the Ctrl+B
    // Async Tasks Inspector focused on that child.
    h.app
        .resident
        .current_mut()
        .unwrap()
        .transcript
        .toggle_group(index);
    h.screen(80, 22);
    let row = (0..22)
        .find(|row| {
            h.app
                .layout
                .hit(3, *row)
                .is_some_and(|(block, _)| block == index)
                && h.app
                    .layout
                    .row_text(*row)
                    .is_some_and(|text| text.trim_start().starts_with("✓ agent"))
        })
        .unwrap();
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        h.app.on_mouse(MouseEvent {
            kind,
            column: 3,
            row,
            modifiers: KeyModifiers::NONE,
        });
    }
    assert!(matches!(
        h.app.overlay,
        crate::app::Overlay::Delegations { .. }
    ));
    let inspector_wide = h.render_text(120, 24);
    assert!(inspector_wide.contains("Async Tasks"), "{inspector_wide}");
    assert!(inspector_wide.contains("Task alpha"), "{inspector_wide}");
    assert!(inspector_wide.contains("Task beta"), "{inspector_wide}");
    // Typing filters the Ctrl+B task list immediately; Ctrl+U clears it.
    for ch in "beta".chars() {
        h.app.on_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char(ch),
            KeyModifiers::NONE,
        ));
    }
    let filtered_screen = h.render_text(120, 24);
    assert!(filtered_screen.contains("showing 1/2"), "{filtered_screen}");
    assert!(filtered_screen.contains("Task beta"), "{filtered_screen}");
    assert!(!filtered_screen.contains("Task alpha"), "{filtered_screen}");
    h.app.on_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('u'),
        KeyModifiers::CONTROL,
    ));
    let cleared_screen = h.render_text(120, 24);
    assert!(cleared_screen.contains("Task alpha"), "{cleared_screen}");
    // Esc closes the inspector back to the main transcript.
    h.app.on_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Esc,
        KeyModifiers::NONE,
    ));
    assert!(h.app.overlay.is_none());

    h.app.composer.set_text("Parent draft 日本語");
    // Anchor at the top of the transcript (above the tail) so the reader stays
    // detached while children continue streaming in the background.
    h.app.scroll = Scroll::Anchor {
        block: 0,
        offset: 0,
    };
    h.screen(80, 22);
    h.app.selection = Some(crate::app::selection::Drag {
        from: (3, 3),
        to: (8, 3),
    });
    let selection = h.app.selection;
    let scroll = h.app.scroll;
    a.text("\nalpha final line");
    b.text("\nbeta partial retained");
    h.until(|app| {
        answer(app, "start-a").contains("final line")
            && answer(app, "start-b").contains("partial retained")
    })
    .await;
    h.screen(80, 22);
    assert_eq!(h.app.composer.text(), "Parent draft 日本語");
    assert_eq!(h.app.scroll, scroll);
    assert_eq!(h.app.selection, selection);
    a.finish();
    b.fail("Controlled child provider failure");
    for _ in 0..3 {
        let retry = real_bounded(h.native.calls.recv()).await.unwrap();
        retry.text("beta partial retained");
        retry.fail("Controlled child provider failure");
    }
    h.until(|app| {
        card(app, "start-a").is_some_and(|card| card.outcome() == Some(TurnOutcome::Completed))
            && card(app, "start-b").is_some_and(|card| card.outcome() == Some(TurnOutcome::Failed))
    })
    .await;
    assert_eq!(
        h.app.resident.current().unwrap().state.data.active_turn_id,
        active
    );
    assert_eq!(
        h.app.resident.current().unwrap().state.data.load_id,
        active_load
    );
    assert_eq!(
        answer(&h.app, "start-a"),
        "alpha one\nalpha two\nalpha final line"
    );
    assert!(answer(&h.app, "start-b").contains("beta partial retained"));
    assert!(
        card(&h.app, "start-b")
            .unwrap()
            .fault()
            .unwrap()
            .contains("Controlled child provider failure")
    );
    assert_eq!(h.app.composer.text(), "Parent draft 日本語");

    h.open(&first).await;
    h.open(&parent_target).await;
    assert_eq!(h.app.composer.text(), "Parent draft 日本語");
    assert_eq!(h.app.scroll, scroll);
    later_parent.text("Parent continues independently");
    later_parent.finish();
    settle(&mut h).await;
    h.finish().await;
}

#[tokio::test]
async fn followups_keep_one_card_and_a_reused_session_gets_a_new_frozen_boundary() {
    let mut h = Harness::from_native(NativeHost::with_agent_tool().await).await;
    let child = h.another_session().await;
    h.app
        .composer
        .set_text("Delegate, add a follow-up, then reuse the child");
    h.app.on_action(Action::Submit { follow: false });
    let root = h.next_call().await;
    let (parent, first) = launch(&mut h, root, &child, "original", "first task").await;
    parent.tool("supplement", json!({"action":"send","target":target(&child),"text":"follow-up detail","mode":"follow_up"}));
    let parent = h.next_call().await;
    assert!(parent.tool_result().is_some());
    h.until(|app| {
        app.resident
            .current()
            .unwrap()
            .transcript
            .tool_calls()
            .any(|call| {
                call.key.tool_call_id().as_str() == "supplement" && !call.delegations.is_empty()
            })
    })
    .await;
    let receipt = h
        .app
        .resident
        .current()
        .unwrap()
        .transcript
        .tool_calls()
        .find(|call| call.key.tool_call_id().as_str() == "supplement")
        .unwrap();
    assert!(receipt.is_delegation_receipt());
    assert_eq!(
        receipt.delegations[0].origin.tool_call_id.as_str(),
        "original"
    );
    first.text("first answer");
    first.finish();
    let followup = h.next_call().await;
    assert_delegated_input(&followup, "follow-up detail", &h.native.target);
    followup.text("follow-up answer");
    followup.finish();
    h.until(|app| {
        card(app, "original").is_some_and(|card| card.outcome() == Some(TurnOutcome::Completed))
    })
    .await;
    let original = card(&h.app, "original").unwrap();
    assert_eq!(original.association.accepted_message_ids.len(), 2);
    assert_eq!(original.association.inputs.len(), 1);
    assert_eq!(
        original.association.inputs[0].delegated_message_ids.len(),
        2
    );
    let old_key = original.key.clone();
    let old_answer = answer(&h.app, "original");
    let (parent, second) = launch(
        &mut h,
        parent,
        &child,
        "new-delegation",
        "unrelated new task",
    )
    .await;
    second.text("new task output");
    h.until(|app| answer(app, "new-delegation") == "new task output")
        .await;
    assert_ne!(card(&h.app, "new-delegation").unwrap().key, old_key);
    assert_eq!(
        card(&h.app, "new-delegation").unwrap().key.child,
        old_key.child
    );
    assert_eq!(answer(&h.app, "original"), old_answer);
    second.finish();
    h.until(|app| {
        card(app, "new-delegation")
            .is_some_and(|card| card.outcome() == Some(TurnOutcome::Completed))
    })
    .await;
    parent.text("Finished delegation coordination");
    parent.finish();
    settle(&mut h).await;
    h.finish().await;
}

#[tokio::test]
async fn a_fast_released_memory_child_keeps_actual_partial_output_when_history_is_gone() {
    let mut h = Harness::from_native(NativeHost::with_agent_tool().await).await;
    h.app.composer.set_text("Create a short memory child");
    h.app.on_action(Action::Submit { follow: false });
    let root = h.next_call().await;
    let parent_loop = root.context.loop_id;
    let parent_target = h.native.target.clone();
    let turn_id = h
        .native
        .client
        .session(parent_target.clone())
        .state(tasks::options())
        .await
        .unwrap()
        .data
        .active_turn_id
        .unwrap();
    let key = ToolCallRef {
        turn_id,
        tool_call_id: "memory-start".to_owned().into(),
    };
    let mut events = tasks::native_session_subscription(&h.native.client, parent_target.clone())
        .await
        .unwrap();
    root.tool(
        "memory-start",
        json!({"action":"create","text":"short child task"}),
    );
    // Deliberately do not reduce App messages until native source retirement.
    let first = real_bounded(h.native.calls.recv()).await.unwrap();
    let second = real_bounded(h.native.calls.recv()).await.unwrap();
    let (parent, child) = if first.context.loop_id == parent_loop {
        (first, second)
    } else {
        (second, first)
    };
    assert_eq!(parent.context.loop_id, parent_loop);
    assert_ne!(child.context.loop_id, parent_loop);
    assert_delegated_input(&child, "short child task", &parent_target);
    let receipt = parent.tool_result().unwrap();
    let child_target = SessionRef {
        agent_id: serde_json::from_value(receipt["agent_id"].clone()).unwrap(),
        session_id: serde_json::from_value(receipt["session_id"].clone()).unwrap(),
    };
    child.reasoning("captured thought\nsecond thought");
    child.text("retained before failure\nlast real bytes");
    child.fail("Actual short-child failure");
    for _ in 0..3 {
        let retry = real_bounded(h.native.calls.recv()).await.unwrap();
        retry.reasoning("captured thought\nsecond thought");
        retry.text("retained before failure\nlast real bytes");
        retry.fail("Actual short-child failure");
    }
    real_bounded(async {
        loop {
            if let Some(ObservationNotification::Event(push)) = events.recv().await.unwrap()
                && matches!(push.event, ObservationEvent::Activity(ActivityEvent::MessageAccepted { data, .. }) if data.delegations.iter().any(|association| association.origin == key && association.completion.is_some()))
            { break; }
        }
    }).await;
    assert!(
        h.native
            .client
            .session(child_target)
            .info(tasks::options())
            .await
            .is_err()
    );
    h.until(|app| {
        card(app, "memory-start").is_some_and(|card| {
            card.history_unavailable && card.outcome() == Some(TurnOutcome::Failed)
        }) && answer(app, "memory-start").contains("last real bytes")
    })
    .await;
    let retained = card(&h.app, "memory-start").unwrap();
    assert!(
        retained
            .fault()
            .unwrap()
            .contains("Actual short-child failure")
    );
    assert!(retained.transcript.blocks().iter().any(|block| matches!(block, Block::Assistant { reasoning, .. } if reasoning == "captured thought\nsecond thought")));
    parent.text("Observed memory child result");
    parent.finish();
    settle(&mut h).await;
    drop(events);
    h.finish().await;
}

#[tokio::test]
async fn fresh_baselines_do_not_replay_trimmed_outputs_or_retain_finished_buffers() {
    let mut h = Harness::from_native(NativeHost::with_agent_tool().await).await;
    let child = h.another_session().await;
    h.app
        .composer
        .set_text("Delegate a long child conversation");
    h.app.on_action(Action::Submit { follow: false });
    let root = h.next_call().await;
    let (mut parent, mut working) = launch(&mut h, root, &child, "long-child", "long task").await;
    for index in 0..40 {
        working.text(&format!("Intermediate child response {index}"));
        working.tool(
            &format!("inspect-{index}"),
            json!({"action":"inspect","target":target(&child)}),
        );
        working = h.next_call().await;
        assert!(working.tool_result().is_some());
    }
    let current_answer = "Current child response before more delegated inputs";
    working.text(current_answer);
    h.until(|app| answer(app, "long-child") == current_answer && !app.agent_cards.is_reading())
        .await;
    assert!(card(&h.app, "long-child").unwrap().truncated);
    let request = SubscribeRequest {
        scope: SubscriptionScope::Host {
            host_id: h.native.client.initial_info().host_id.clone(),
        },
        categories: [EventCategory::State, EventCategory::Output].into(),
    };
    let baseline = h
        .native
        .client
        .subscribe(request.clone(), tasks::options())
        .await
        .unwrap();
    assert!(baseline.result().output_positions.len() > 64);
    h.app.on_msg(Msg::HostBaseline(baseline.result().clone()));
    assert!(
        h.app.agent_cards.buffered_outputs() <= 2,
        "only the live response channels remain"
    );
    h.until(|app| !app.agent_cards.is_reading()).await;
    assert_eq!(answer(&h.app, "long-child"), current_answer);
    drop(baseline);

    // More than one history window of plain responses leaves every old tool
    // outside the retained preview; baseline tool states must not restore them.
    let final_answer = "The actual final child answer";
    for index in 0..12 {
        let input = format!("Continue the delegated task {index}");
        parent.tool(
            &format!("continue-{index}"),
            json!({"action":"send","target":target(&child),"text":&input,"mode":"follow_up"}),
        );
        parent = h.next_call().await;
        assert!(parent.tool_result().is_some());
        working.finish();
        working = h.next_call().await;
        assert_delegated_input(&working, &input, &h.native.target);
        if index == 11 {
            working.text(final_answer);
        } else {
            working.text(&format!("Continuation answer {index}"));
        }
    }
    working.finish();
    h.until(|app| {
        card(app, "long-child").is_some_and(|card| card.outcome() == Some(TurnOutcome::Completed))
            && !app.agent_cards.is_reading()
    })
    .await;
    let baseline = h
        .native
        .client
        .subscribe(request, tasks::options())
        .await
        .unwrap();
    h.app.on_msg(Msg::HostBaseline(baseline.result().clone()));
    h.until(|app| !app.agent_cards.is_reading()).await;
    assert_eq!(h.app.agent_cards.buffered_outputs(), 0);
    assert_eq!(answer(&h.app, "long-child"), final_answer);
    drop(baseline);

    // Reattach the actual retained association without any prior live finish
    // events. Its native baseline supplies State::Turn(Finished), not activity.
    let association = card(&h.app, "long-child").unwrap().association.clone();
    h.app.agent_cards = Default::default();
    h.app.register_delegation(&association);
    let mut baseline_seen = false;
    real_bounded(async {
        while !baseline_seen || h.app.agent_cards.is_reading() {
            let message = h.messages.recv().await.unwrap();
            baseline_seen |= matches!(&message, Msg::AgentCardRead(_, read) if matches!(read.as_ref(), Read::Baseline(_)));
            h.app.on_msg(message);
        }
    })
    .await;
    assert_eq!(h.app.agent_cards.buffered_outputs(), 0);
    assert_eq!(answer(&h.app, "long-child"), final_answer);
    assert!(card(&h.app, "long-child").unwrap().transcript.len() <= 24);
    assert_eq!(
        card(&h.app, "long-child")
            .unwrap()
            .transcript
            .tool_calls()
            .count(),
        0
    );
    parent.text("Long child work recorded");
    parent.finish();
    settle(&mut h).await;
    h.finish().await;
}

#[tokio::test]
async fn ctrl_b_inspector_loads_real_child_conversation_filters_scrolls_and_messages_directly() {
    use crate::app::{Overlay, Panel, overlay::TaskFocus};
    use crossterm::event::{KeyCode, KeyEvent};

    let mut h = Harness::from_native(NativeHost::with_agent_tool().await).await;
    let child = h.another_session().await;
    let parent_target = h.native.target.clone();

    h.app.composer.set_text("Delegate a check to child");
    h.app.on_action(Action::Submit { follow: false });
    let root = h.next_call().await;
    let (parent, working) = launch(&mut h, root, &child, "start-1", "Check API routes").await;
    working.text("Found 3 API routes in src/routes.rs");
    working.usage(minicore_host_runtime::model::Usage::from_optional(
        Some(12300),
        Some(850),
        None,
    ));
    working.finish();
    parent.text("Delegated API route check.");
    parent.finish();
    real_bounded(async {
        loop {
            let parent = h.app.resident.current().unwrap();
            let has_delegated = parent.transcript.blocks().iter().any(|b| {
                matches!(
                    b,
                    Block::User {
                        kind: crate::transcript::UserKind::Delegated,
                        ..
                    }
                )
            });
            if has_delegated
                && !parent.is_busy()
                && parent.state.data.next_step.is_empty()
                && parent.state.data.next_turn.is_empty()
            {
                break;
            }
            tokio::select! {
                Some(message) = h.messages.recv() => h.app.on_msg(message),
                Some(call) = h.native.calls.recv() => {
                    call.text("Recorded child completion");
                    call.finish();
                }
            }
        }
    })
    .await;

    // 1. Delegated arrival blocks are hidden from the main transcript.
    let parent_session = h.app.resident.current().unwrap();
    let delegated_indices: Vec<usize> = parent_session
        .transcript
        .blocks()
        .iter()
        .enumerate()
        .filter_map(|(i, b)| {
            matches!(
                b,
                Block::User {
                    kind: crate::transcript::UserKind::Delegated,
                    ..
                }
            )
            .then_some(i)
        })
        .collect();
    assert!(!delegated_indices.is_empty(), "expected delegated delivery");
    for idx in delegated_indices {
        assert!(
            parent_session.transcript.absorbed(idx),
            "agent completed banner must stay hidden from main transcript"
        );
    }

    // 2. Keep a draft in parent composer to verify complete isolation.
    h.app.composer.set_text("Parent draft stays intact");

    // 3. Open Ctrl+B; child session loads in background without switching parent.
    h.app.open_panel(Panel::Delegations);
    assert!(matches!(
        h.app.overlay,
        Overlay::Delegations {
            focus: TaskFocus::List,
            ..
        }
    ));
    h.until(|app| app.resident.find(&child).is_some()).await;
    assert_eq!(
        h.app.resident.current().unwrap().address(),
        parent_target,
        "opening Ctrl+B must not switch away from the parent session"
    );
    assert!(
        h.app
            .inspected_session()
            .unwrap()
            .transcript
            .last_answer()
            .unwrap_or("")
            .contains("Found 3 API routes")
    );

    let selected = h.app.inspected_session().unwrap();
    let expected_model = format!("Model {}", selected.info.config.model);
    let expected_reasoning = format!(
        "Reasoning {}",
        crate::ui::sheets::reasoning_label(selected.info.config.reasoning)
    );
    let expected_mode = format!(
        "Storage {}",
        crate::ui::sheets::storage_label(selected.state.data.mode)
    );
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(110, 24)).unwrap();
    for (width, focus) in [(110, TaskFocus::List), (62, TaskFocus::Detail)] {
        terminal.backend_mut().resize(width, 24);
        if let Overlay::Delegations { focus: current, .. } = &mut h.app.overlay {
            *current = focus;
        }
        terminal
            .draw(|frame| {
                crate::ui::draw(frame, &mut h.app);
            })
            .unwrap();
        let screen = terminal.backend().buffer();
        let lines: Vec<String> = screen
            .content
            .chunks(width as usize)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect())
            .collect();
        let facts = lines
            .iter()
            .position(|line| line.contains(&expected_model))
            .unwrap();
        assert!(lines[facts].contains(&expected_reasoning));
        assert!(lines[facts + 1].contains(&expected_mode));
        assert!(lines[facts + 2].contains("Last response tokens: ↑12.3k ↓850"));
        assert!(
            lines
                .iter()
                .skip(facts + 2)
                .any(|line| line.contains("Found 3 API"))
        );
    }
    if let Overlay::Delegations { focus, .. } = &mut h.app.overlay {
        *focus = TaskFocus::List;
    }

    // 4. Filter by typing in List, then clear with Ctrl+U.
    for ch in "routes".chars() {
        h.app
            .on_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
    }
    assert!(matches!(
        &h.app.overlay,
        Overlay::Delegations { filter, .. } if filter == "routes"
    ));
    h.app
        .on_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    assert!(matches!(
        &h.app.overlay,
        Overlay::Delegations { filter, .. } if filter.is_empty()
    ));

    // 5. Right focuses Detail; Esc returns to List; Enter focuses Composer.
    h.app
        .on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    assert!(matches!(
        h.app.overlay,
        Overlay::Delegations {
            focus: TaskFocus::Detail,
            ..
        }
    ));
    h.app
        .on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(
        h.app.overlay,
        Overlay::Delegations {
            focus: TaskFocus::List,
            ..
        }
    ));
    h.app
        .on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(
        h.app.overlay,
        Overlay::Delegations {
            focus: TaskFocus::Composer,
            ..
        }
    ));

    // 6. Send a direct follow-up message from Ctrl+B bottom composer to child.
    for ch in "Check auth too".chars() {
        h.app
            .on_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
    }
    assert_eq!(h.app.task_inspector.composer.text(), "Check auth too");
    assert_eq!(h.app.composer.text(), "Parent draft stays intact");
    h.app
        .on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(h.app.task_inspector.composer.is_empty());

    let child_followup = h.next_call().await;
    assert!(
        child_followup
            .users()
            .iter()
            .any(|text| text.contains("Check auth too")),
        "child must receive the message sent from Ctrl+B: {:?}",
        child_followup.users()
    );
    child_followup.text("Auth middleware verified");
    child_followup.finish();
    h.until(|app| {
        app.inspected_session().is_some_and(|session| {
            !session.is_busy()
                && session
                    .transcript
                    .last_answer()
                    .unwrap_or("")
                    .contains("Auth middleware verified")
        })
    })
    .await;
    assert_eq!(
        h.app.composer.text(),
        "Parent draft stays intact",
        "parent draft must remain untouched after messaging child from Ctrl+B"
    );
    h.finish().await;
}

#[tokio::test]
async fn delegated_sessions_hide_from_sessions_picker_and_flatten_grandchildren_with_names() {
    use crate::app::overlay::{async_tasks, matching_sessions};
    use std::collections::HashMap;

    let mut h = Harness::from_native(NativeHost::with_agent_tool().await).await;
    let root_addr = h.native.target.clone();

    h.app.composer.set_text("Coordinate nested work");
    h.app.on_action(Action::Submit { follow: false });
    let root_call = h.next_call().await;
    let root_loop = root_call.context.loop_id;

    // Root explicitly grants the L1 worker permission to launch a helper.
    root_call.tool(
        "create-l1",
        json!({
            "action": "create",
            "resource": "session",
            "name": "worker-a",
            "allow_delegation": true,
            "text": "Inspect repo and spawn helper"
        }),
    );
    let first = h.next_call().await;
    let second = h.next_call().await;
    let (root_after_l1, l1_call) = if first.context.loop_id == root_loop {
        (first, second)
    } else {
        (second, first)
    };
    let l1_loop = l1_call.context.loop_id;
    let l1_result = root_after_l1.tool_result().unwrap();
    let l1_session_id: minicore_host_protocol::SessionId = l1_result["session_id"]
        .as_str()
        .unwrap_or_else(|| panic!("expected session_id in {l1_result}"))
        .parse()
        .unwrap();

    // L1 delegates to L2 grandchild with an explicit short name.
    l1_call.tool(
        "create-l2",
        json!({
            "action": "create",
            "resource": "session",
            "name": "helper-b",
            "text": "Check git log"
        }),
    );
    let first2 = h.next_call().await;
    let second2 = h.next_call().await;
    let (l1_after_l2, l2_call) = if first2.context.loop_id == l1_loop {
        (first2, second2)
    } else {
        (second2, first2)
    };
    let l2_result = l1_after_l2.tool_result().unwrap();
    let l2_session_id: minicore_host_protocol::SessionId = l2_result["session_id"]
        .as_str()
        .unwrap_or_else(|| panic!("expected session_id in {l2_result}"))
        .parse()
        .unwrap();

    // Wait for Host catalog and root-scoped delegation events to land in TUI state.
    h.app.refresh_catalogs();
    h.until(|app| {
        let tasks = async_tasks(app);
        app.sessions.iter().any(|s| s.id == l1_session_id)
            && app.sessions.iter().any(|s| s.id == l2_session_id)
            && tasks
                .iter()
                .any(|t| t.session_id() == l1_session_id.as_str())
            && tasks
                .iter()
                .any(|t| t.session_id() == l2_session_id.as_str())
    })
    .await;

    // 1. Both delegated sessions have provenance and are hidden from `/sessions`.
    let l1_summary = h
        .app
        .sessions
        .iter()
        .find(|s| s.id == l1_session_id)
        .unwrap();
    let l2_summary = h
        .app
        .sessions
        .iter()
        .find(|s| s.id == l2_session_id)
        .unwrap();
    assert_eq!(l1_summary.delegated_by.as_ref(), Some(&root_addr));
    assert_eq!(l1_summary.delegation_root.as_ref(), Some(&root_addr));
    assert_eq!(
        l2_summary.delegated_by.as_ref().map(|r| &r.session_id),
        Some(&l1_session_id)
    );
    assert_eq!(l2_summary.delegation_root.as_ref(), Some(&root_addr));
    let visible = matching_sessions(&h.app.sessions, "");
    assert!(visible.iter().any(|s| s.id == root_addr.session_id));
    assert!(
        visible.iter().all(|s| s.delegated_by.is_none()),
        "delegated child/grandchild sessions must be hidden from /sessions"
    );

    // 2. Flat Async Tasks list contains both L1 and L2 with names and dispatcher attribution.
    let tasks = async_tasks(&h.app);
    let mut names: HashMap<String, String> = HashMap::new();
    for item in &tasks {
        if let Some(name) = item.task_name() {
            names.insert(item.session_id().to_owned(), name.to_owned());
        }
    }
    let l1_item = tasks
        .iter()
        .find(|t| t.session_id() == l1_session_id.as_str())
        .unwrap();
    let l2_item = tasks
        .iter()
        .find(|t| t.session_id() == l2_session_id.as_str())
        .unwrap();
    assert_eq!(l1_item.task_name(), Some("worker-a"));
    assert_eq!(
        l1_item.dispatched_by(Some(&root_addr), &names).as_deref(),
        Some("from main")
    );
    assert_eq!(l2_item.task_name(), Some("helper-b"));
    assert!(
        l2_item
            .dispatched_by(Some(&root_addr), &names)
            .as_deref()
            .is_some_and(|s| s.starts_with("from worker-a (")),
        "expected dispatcher attribution for grandchild, got: {:?}",
        l2_item.dispatched_by(Some(&root_addr), &names)
    );

    // Finish cleanly in bottom-up order: L2 finishes and delivers its completion
    // to L1 while L1 is still active, then L1 finishes its turn and notifies root.
    l2_call.text("git log clean");
    l2_call.finish();
    h.until(|app| {
        async_tasks(app).iter().any(|item| {
            item.session_id() == l2_session_id.as_str()
                && matches!(
                    item,
                    crate::app::overlay::AsyncTaskItem::Agent(card)
                        if card.association.completion.is_some()
                )
        })
    })
    .await;
    l1_after_l2.text("helper finished");
    l1_after_l2.finish();
    let l1_followup = h.next_call().await;
    assert_eq!(l1_followup.context.loop_id, l1_loop);
    l1_followup.text("summarized helper output");
    l1_followup.finish();
    root_after_l1.text("all delegated");
    root_after_l1.finish();
    real_bounded(async {
        loop {
            let parent = h.app.resident.current().unwrap();
            let tasks = async_tasks(&h.app);
            let all_done = tasks
                .iter()
                .all(|item| matches!(item, crate::app::overlay::AsyncTaskItem::Agent(card) if card.association.completion.is_some()));
            if !parent.is_busy()
                && parent.state.data.next_step.is_empty()
                && parent.state.data.next_turn.is_empty()
                && all_done
            {
                break;
            }
            tokio::select! {
                Some(message) = h.messages.recv() => h.app.on_msg(message),
                Some(call) = h.native.calls.recv() => {
                    call.text("Recorded completion");
                    call.finish();
                }
            }
        }
    })
    .await;
    h.finish().await;
}

#[tokio::test]
async fn the_tasks_stop_shortcut_interrupts_the_child_not_its_parent() {
    use crate::app::{Overlay, Panel, overlay::TaskFocus};
    use crossterm::event::{KeyCode, KeyEvent};

    let mut h = Harness::from_native(NativeHost::with_agent_tool().await).await;
    let child = h.another_session().await;
    h.app.composer.set_text("Coordinate a child");
    h.app.on_action(Action::Submit { follow: false });
    let root = h.next_call().await;
    let (parent, _working) = launch(&mut h, root, &child, "stop-child", "Check routes").await;
    h.until(|app| card(app, "stop-child").is_some()).await;
    h.app.open_panel(Panel::Delegations);
    h.until(|app| app.inspected_session().is_some_and(|s| s.is_busy()))
        .await;

    h.app
        .on_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    assert!(matches!(
        h.app.overlay,
        Overlay::Delegations {
            focus: TaskFocus::Detail,
            ..
        }
    ));
    h.app
        .on_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL));
    h.until(|app| {
        card(app, "stop-child").and_then(AgentCard::outcome) == Some(TurnOutcome::Cancelled)
    })
    .await;
    assert!(h.app.resident.current().unwrap().is_busy());
    parent.finish();
    settle(&mut h).await;
}
