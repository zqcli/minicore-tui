use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use minicore_host_core::{ActivityEvent, ObservationEvent, ObservationNotification};
use minicore_host_protocol::{Residency, SessionRef, ToolCallRef, TurnOutcome};
use serde_json::{Value, json};

use super::harness::Harness;
use crate::app::{Action, App, Scroll, tasks};
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

async fn launch(
    h: &mut Harness,
    parent: ModelCall,
    child: &SessionRef,
    id: &str,
    text: &str,
) -> (ModelCall, ModelCall) {
    parent.tool(
        id,
        json!({"action":"send","target":target(child),"text":text,"delegate":true}),
    );
    let first = h.next_call().await;
    let second = h.next_call().await;
    let (parent, child) = if first.users().contains(&text) {
        (second, first)
    } else {
        (first, second)
    };
    assert!(child.users().contains(&text));
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
    h.screen(80, 22);
    let row = (0..22)
        .find(|row| h.app.layout.hit(3, *row) == Some((index, 0)))
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
    h.app.composer.set_text("Parent draft 日本語");
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
    assert!(
        h.app
            .resident
            .current()
            .unwrap()
            .transcript
            .tool_calls()
            .find(|call| call.key.tool_call_id().as_str() == "start-a")
            .unwrap()
            .is_expanded(false)
    );
    a.finish();
    b.fail("Controlled child provider failure");
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
    assert!(followup.users().contains(&"follow-up detail"));
    followup.text("follow-up answer");
    followup.finish();
    h.until(|app| {
        card(app, "original").is_some_and(|card| card.outcome() == Some(TurnOutcome::Completed))
    })
    .await;
    let original = card(&h.app, "original").unwrap();
    assert_eq!(original.association.accepted_message_ids.len(), 2);
    assert_eq!(original.association.inputs.len(), 2);
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
    root.tool("memory-start", json!({"action":"create","resource":"session","target":{"host_id":h.native.client.initial_info().host_id,"agent_id":parent_target.agent_id},"mode":"memory","text":"short child task"}));
    // Deliberately do not reduce App messages until native source retirement.
    let first = real_bounded(h.native.calls.recv()).await.unwrap();
    let second = real_bounded(h.native.calls.recv()).await.unwrap();
    let (parent, child) = if first.users().contains(&"short child task") {
        (second, first)
    } else {
        (first, second)
    };
    let receipt = parent.tool_result().unwrap();
    let child_target = SessionRef {
        agent_id: serde_json::from_value(receipt["agent_id"].clone()).unwrap(),
        session_id: serde_json::from_value(receipt["session_id"].clone()).unwrap(),
    };
    child.reasoning("captured thought\nsecond thought");
    child.text("retained before failure\nlast real bytes");
    child.fail("Actual short-child failure");
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
