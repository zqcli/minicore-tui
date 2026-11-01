use std::time::Duration;

use crossterm::event::{KeyCode, KeyModifiers, MouseEvent, MouseEventKind};
use serde_json::json;

use super::harness::*;
use crate::app::{FLASH_TTL, Msg, NoticeLevel, Overlay, Scroll};
use crate::protocol::read::UserKind;
use crate::transcript::{Block, RequestKey};

#[tokio::test(start_paused = true)]
async fn answering_an_interaction_keeps_it_until_the_authoritative_snapshot_resolves_it() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake
        .script_held("interaction.answer", json!({"accepted":true}));
    h.fake
        .script_held("session.state@s1", json!({"session_id":"s1"}));
    h.control_event(json!({"pending_interaction":{"loop_id":"l1","request_index":0,"interaction_id":"first","tool_call_id":"call","tool_name":"bash","kind":{"type":"approval","data":{"prompt":"Allow?"}}}})).await;
    h.until(|app| app.has_interaction()).await;
    h.action(crate::app::Action::Answer(crate::app::Answer::Allow));
    assert_eq!(
        h.session().interaction.as_ref().unwrap().interaction_id,
        "first"
    );
    h.control_event(json!({"pending_interaction":{"loop_id":"l1","request_index":0,"interaction_id":"newer","tool_call_id":"call2","tool_name":"bash","kind":{"type":"approval","data":{"prompt":"New approval?"}}}})).await;
    h.until(|app| {
        app.resident
            .current()
            .unwrap()
            .interaction
            .as_ref()
            .unwrap()
            .interaction_id
            == "newer"
    })
    .await;
    h.fake.release("interaction.answer");
    h.settle().await;
    assert_eq!(
        h.session().interaction.as_ref().unwrap().interaction_id,
        "newer"
    );
}

fn text_interaction(id: &str) -> serde_json::Value {
    json!({"loop_id":"l1","request_index":0,"interaction_id":id,"tool_call_id":"call","tool_name":"question","kind":{"type":"tool_input","data":{"prompt":"Describe the branch to use","answer_kind":"text","choices":[{"index":0,"text":"not a choice prompt"}]}}})
}

#[tokio::test(start_paused = true)]
async fn persistence_blocking_stays_visible_without_hiding_an_actionable_text_interaction() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake
        .script_held("interaction.answer", json!({"accepted":true}));
    h.control_event(json!({"block_reason":"persistence"})).await;
    h.until(|app| app.resident.current().unwrap().block_reason.is_some())
        .await;
    h.app.notices.clear();
    assert!(h.screen(120, 30).contains("Persistence blocked"));
    h.app.composer.set_text("separate conversation draft");
    h.control_event(json!({"pending_interaction":text_interaction("blocked-question"),"block_reason":"persistence"})).await;
    h.until(|app| app.has_interaction()).await;
    h.type_text("the answer");
    let screen = h.screen(120, 30);
    for text in [
        "Persistence blocked",
        "Waiting for input",
        "Describe the branch to use",
        "the answer",
    ] {
        assert!(screen.contains(text), "{text}: {screen}");
    }
    tokio::time::advance(Duration::from_millis(500)).await;
    h.press(KeyCode::Enter);
    h.fake.wait_for_calls("interaction.answer", 1).await;
    assert_eq!(
        h.calls("interaction.answer")[0]["interaction_id"],
        "blocked-question"
    );
    assert_eq!(
        h.calls("interaction.answer")[0]["answer"],
        json!({"type":"text","text":"the answer"})
    );
    assert_eq!(h.app.composer.text(), "separate conversation draft");
    let mut long_question = text_interaction("long-blocked-question");
    long_question["kind"]["data"]["prompt"] = json!("question line\n".repeat(100));
    h.control_event(json!({"pending_interaction":long_question,"block_reason":"persistence"}))
        .await;
    h.until(|app| {
        app.resident
            .current()
            .unwrap()
            .interaction
            .as_ref()
            .unwrap()
            .interaction_id
            == "long-blocked-question"
    })
    .await;
    h.type_text("narrow answer");
    let narrow = h.screen(70, 12);
    for text in ["Persistence blocked", "Text answer", "narrow answer"] {
        assert!(narrow.contains(text), "{text}: {narrow}");
    }
}

#[tokio::test(start_paused = true)]
async fn local_unknown_input_retains_its_owner_and_requires_a_duplicate_warning_after_editing() {
    use crate::app::{
        Action, Overlay,
        unconfirmed::{ReviewMode, Status},
    };
    let mut h = Harness::opened(display_read(0, 0)).await;
    #[cfg(feature = "e2e-observe")]
    let trace = tempfile::NamedTempFile::new().unwrap();
    #[cfg(feature = "e2e-observe")]
    h.app.start_observer(trace.reopen().unwrap());
    h.fake.script_error_with(
        "input.push",
        -32603,
        "acceptance unknown",
        "transport_uncertain",
        Some("unknown"),
    );
    h.app.composer.set_text("original unknown");
    h.action(Action::QueueFollowUp);
    let original_id = h.app.local_inputs[0].id;
    h.app.composer.set_text("new independent draft");
    h.settle().await;
    assert_eq!(
        h.app.local_inputs.len(),
        1,
        "a generic notice and history are not unconfirmed operation ownership"
    );
    let original = &h.app.local_inputs[0];
    assert_eq!(original.id, original_id);
    assert_eq!(original.status, Status::Unconfirmed);
    assert_eq!(original.entry.text(), "original unknown");
    assert_eq!(original.target.session_id, "s1");
    assert_eq!(original.target.load, crate::testing::LOAD);
    assert_eq!(original.target.epoch, h.session().epoch);
    assert_eq!(original.mode, crate::protocol::InputMode::FollowUp);
    assert!(
        !h.app.composer.history_prev(),
        "uncertain input is not ordinary recallable prompt history"
    );
    h.app.notices.clear();
    tokio::time::advance(Duration::from_secs(60)).await;
    h.app.tick();
    assert!(h.screen(120, 24).contains("F8 review"));
    h.press(KeyCode::F(8));
    h.press(KeyCode::Char('e'));
    assert!(matches!(
        h.app.overlay,
        Overlay::Unconfirmed {
            mode: ReviewMode::Edit(_),
            ..
        }
    ));
    h.press(KeyCode::Home);
    h.type_text("edited ");
    assert_eq!(h.app.composer.text(), "new independent draft");
    #[cfg(feature = "e2e-observe")]
    {
        h.app.observe_checkpoint(false);
        let rows: Vec<serde_json::Value> = std::fs::read_to_string(trace.path())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let state = rows
            .iter()
            .rev()
            .find(|row| row["event"] == "canonical_view")
            .unwrap()["local_input_state"]
            .clone();
        assert_eq!(
            state["operations"][0]["operation_id"],
            format!("tui-{original_id}")
        );
        assert_eq!(state["operations"][0]["status"], "unconfirmed");
        assert_eq!(state["operations"].as_array().unwrap().len(), 1);
        assert_eq!(
            state["review"]["operation_id"],
            format!("tui-{original_id}")
        );
        assert_eq!(state["review"]["mode"], "edit");
        assert_ne!(
            state["operations"][0]["body"],
            state["review"]["edited_body"]
        );
        assert!(
            !std::fs::read_to_string(trace.path())
                .unwrap()
                .contains("original unknown")
        );
    }
    assert_eq!(
        h.app.local_inputs[0].entry.text(),
        "original unknown",
        "editing cannot rewrite the original acceptance uncertainty"
    );
    h.press(KeyCode::Enter);
    let warning = h.screen(140, 24).to_lowercase();
    assert!(
        warning.contains("may already") && warning.contains("duplicate"),
        "{warning}"
    );
    h.key(KeyCode::Char('y'), KeyModifiers::CONTROL);
    h.key(KeyCode::Char('y'), KeyModifiers::ALT);
    assert_eq!(
        h.calls("input.push").len(),
        1,
        "redo or modified keys are not an explicit resend decision"
    );
    h.press(KeyCode::Enter);
    assert!(matches!(
        h.app.overlay,
        Overlay::Unconfirmed {
            mode: ReviewMode::Edit(_),
            ..
        }
    ));
    assert_eq!(
        h.calls("input.push").len(),
        1,
        "Enter cancels the warning, never confirms an uncertain resend"
    );
    h.press(KeyCode::Esc);
    h.press(KeyCode::Char('r'));
    h.press(KeyCode::Esc);
    assert_eq!(h.app.local_inputs[0].status, Status::Unconfirmed);
    assert_eq!(h.calls("input.push").len(), 1);
    h.press(KeyCode::Char('e'));
    h.press(KeyCode::Home);
    h.type_text("edited ");
    h.press(KeyCode::Enter);
    h.fake.script_held("input.push", accepted("s1", "i1"));
    h.press(KeyCode::Char('y'));
    h.fake.wait_for_calls("input.push", 2).await;
    assert_eq!(h.app.local_inputs.len(), 2);
    let retry = &h.app.local_inputs[1];
    assert_ne!(retry.id, original_id);
    assert_eq!(retry.retry_of, Some(original_id));
    assert_eq!(retry.target, h.app.local_inputs[0].target);
    assert_eq!(retry.mode, crate::protocol::InputMode::FollowUp);
    assert_eq!(h.calls("input.push")[1]["text"], "edited original unknown");
    assert_eq!(
        h.app.local_inputs[0].status,
        Status::Unconfirmed,
        "a new operation's acceptance cannot resolve the old operation"
    );
    assert_eq!(h.app.composer.text(), "new independent draft");
    h.press(KeyCode::Char('d'));
    h.press(KeyCode::Esc);
    assert_eq!(h.app.local_inputs.len(), 2);
    h.press(KeyCode::Char('d'));
    h.press(KeyCode::Char('y'));
    assert_eq!(
        h.app.local_inputs.len(),
        1,
        "forget is an explicit decision and never a cancellation RPC"
    );
    assert_eq!(h.calls("input.remove").len(), 0);
    assert_eq!(h.calls("turn.cancel").len(), 0);
}

#[tokio::test(start_paused = true)]
async fn local_unknown_inputs_survive_view_and_load_changes_without_retargeting_or_automatic_recall()
 {
    use crate::app::{Action, Panel, unconfirmed::Status};
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script_error_with(
        "input.push",
        -32603,
        "no receipt",
        "transport_uncertain",
        Some("unknown"),
    );
    h.submit("retained original");
    let target = h.app.local_inputs[0].target.clone();
    h.action(Action::NewSession);
    h.settle().await;
    assert!(h.app.resident.current().is_none());
    assert_eq!(h.app.local_inputs[0].target, target);
    assert_eq!(h.app.local_inputs[0].status, Status::Unconfirmed);
    h.app.resident = Default::default();
    h.fake.script("session.open",json!({"session":session_info("s1"),"load":"new-load","snapshot":snapshot("s1","new-load",1)}));
    h.fake.script(
        "session.state@s1",
        state_response(snapshot("s1", "new-load", 2)),
    );
    h.action(Action::OpenSession("s1".into()));
    h.until(|app| {
        app.resident
            .current()
            .is_some_and(|session| session.load == "new-load")
    })
    .await;
    h.app.open_panel(Panel::Unconfirmed);
    h.press(KeyCode::Char('r'));
    h.press(KeyCode::Char('y'));
    h.settle().await;
    assert_eq!(
        h.calls("input.push").len(),
        1,
        "a known old load must not be retargeted to the reopened session"
    );
    assert_eq!(h.app.local_inputs[0].target, target);
    assert_eq!(h.app.local_inputs[0].entry.text(), "retained original");
    h.fake.close();
    h.until(|app| app.fatal.is_some()).await;
    h.press(KeyCode::Esc);
    h.press(KeyCode::F(8));
    assert!(
        h.screen(120, 24).contains("retained original"),
        "the fatal screen must not make the retained operation inaccessible"
    );
    assert!(!h.app.composer.history_prev());
}

#[tokio::test(start_paused = true)]
async fn unknown_input_confirmation_keeps_the_same_operation_when_an_earlier_receipt_removes_a_row()
{
    use crate::app::{Action, unconfirmed::Status};
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script_held("input.push@s1", accepted("s1", "i1"));
    h.submit("same");
    let first = h.app.local_inputs[0].id;
    h.fake
        .script("session.open@s2", opened_session(session_info("s2")));
    h.fake
        .script("session.read@s2", history(display_read(0, 0)));
    h.action(Action::OpenSession("s2".into()));
    h.until(|app| {
        app.resident
            .current()
            .is_some_and(|session| session.info.session_id == "s2")
    })
    .await;
    h.fake.script_error_with(
        "input.push@s2",
        -32603,
        "unknown",
        "transport_uncertain",
        Some("unknown"),
    );
    h.submit("same");
    h.settle().await;
    let original = h.app.local_inputs[1].id;
    assert_ne!(first, original);
    assert_eq!(h.app.local_inputs[1].status, Status::Unconfirmed);
    h.press(KeyCode::F(8));
    h.press(KeyCode::Down);
    h.press(KeyCode::Char('r'));
    h.fake.release("input.push@s1");
    h.until(|app| app.local_inputs.len() == 1).await;
    assert_eq!(
        h.app.selected_local_input().map(|input| input.id),
        Some(original),
        "the confirmation owns an ID, not a shifted list index or matching text"
    );
    h.fake.script_held("input.push@s2", accepted("s2", "i1"));
    h.press(KeyCode::Char('y'));
    h.fake.wait_for_calls("input.push", 3).await;
    let retry = h.app.local_inputs.back().unwrap();
    assert_eq!(retry.retry_of, Some(original));
    assert_eq!(retry.target.session_id, "s2");
    assert!(retry.id > original);
    assert_eq!(h.calls("input.push")[2]["session_id"], "s2");
    assert_eq!(h.calls("input.push")[2]["text"], "same");
    assert_eq!(h.app.local_inputs[0].id, original);
    assert_eq!(h.app.local_inputs[0].status, Status::Unconfirmed);
}

#[tokio::test(start_paused = true)]
async fn local_uncertainty_cannot_be_bypassed_through_undo_external_edit_or_history() {
    use crate::app::{Action, Msg, unconfirmed::Status};
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.app.composer.insert("not yet accepted");
    h.action(Action::EditDraft);
    assert!(
        !h.app.composer.history_prev(),
        "opening an external editor is not acceptance"
    );
    let original = h.app.edit_request.take().unwrap();
    h.app.on_msg(Msg::DraftEdited(Some(original)));
    h.fake
        .script_error_with("input.push", -32603, "unknown", "transport_uncertain", None);
    h.action(Action::Submit);
    h.settle().await;
    assert_eq!(h.app.local_inputs[0].status, Status::Unconfirmed);
    assert!(
        !h.app.composer.undo(),
        "the uncertain submission's old undo payload is not an ordinary draft"
    );
    assert!(!h.app.composer.history_prev());
    h.press(KeyCode::F(8));
    h.press(KeyCode::Char('e'));
    h.app.on_paste("edited ");
    h.press(KeyCode::Enter);
    h.press(KeyCode::Esc);
    assert_eq!(h.calls("input.push").len(), 1);
    assert_eq!(h.app.local_inputs[0].entry.text(), "not yet accepted");
}

#[tokio::test(start_paused = true)]
async fn local_start_failure_is_accepted_history_but_a_rejection_never_overwrites_an_edited_equal_draft()
 {
    use crate::app::{Action, unconfirmed::Status};
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script("input.push",json!({"session_id":"s1","load":crate::testing::LOAD,"accepted":true,"id":"accepted-input","start_error":{"input_id":"older-input","kind":"runtime_start_failed","message":"The older queued input could not start"}}));
    let payload = "accepted paste\n".repeat(12);
    h.app.on_paste(&payload);
    h.action(Action::Submit);
    h.settle().await;
    assert!(h.app.local_inputs.is_empty());
    assert!(h.app.composer.is_empty());
    assert!(h.app.composer.history_prev());
    assert_eq!(h.app.composer.text(), payload);
    assert!(h.app.composer.display().contains("[paste"));
    assert!(
        h.notices()
            .iter()
            .any(|notice| notice.contains("was accepted"))
    );
    h.fake.script_error(
        "input.push",
        -32602,
        "rejected before acceptance",
        "invalid_params",
    );
    h.submit("same");
    h.app.composer.set_text("same");
    h.settle().await;
    assert_eq!(h.app.composer.text(), "same");
    assert_eq!(h.app.local_inputs.len(), 1);
    assert_eq!(h.app.local_inputs[0].status, Status::Rejected);
    assert_eq!(h.app.local_inputs[0].entry.text(), "same");
}

#[tokio::test(start_paused = true)]
async fn local_unconfirmed_capacity_refuses_before_sending_and_keeps_the_new_draft() {
    use crate::app::{
        Action,
        unconfirmed::{MAX_BYTES, MAX_OPERATIONS, Status},
    };
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script_error_with(
        "input.push",
        -32603,
        "unknown",
        "transport_uncertain",
        Some("unknown"),
    );
    for index in 0..MAX_OPERATIONS {
        h.submit(&format!("input {index}"));
        h.settle().await;
    }
    assert_eq!(h.app.local_inputs.len(), MAX_OPERATIONS);
    assert!(
        h.app
            .local_inputs
            .iter()
            .all(|input| input.status == Status::Unconfirmed)
    );
    assert!(h.app.local_input_bytes() <= MAX_BYTES);
    let operation = h.app.op_seq;
    h.submit("kept at capacity");
    h.settle().await;
    assert_eq!(h.calls("input.push").len(), MAX_OPERATIONS);
    assert_eq!(h.app.op_seq, operation);
    assert_eq!(h.app.composer.text(), "kept at capacity");
    let mut empty = Harness::opened(display_read(0, 0)).await;
    empty.app.composer.set_text(&"x".repeat(MAX_BYTES + 1));
    let operation = empty.app.op_seq;
    empty.action(Action::Submit);
    assert!(empty.app.local_inputs.is_empty());
    assert_eq!(empty.app.op_seq, operation);
    assert_eq!(empty.app.composer.text().len(), MAX_BYTES + 1);
    assert!(empty.calls("input.push").is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_first_input_that_becomes_unconfirmed_keeps_its_paste_and_the_draft_typed_during_creation()
 {
    use crate::app::{Action, unconfirmed::Status};
    let mut h = Harness::new();
    h.boot().await;
    h.fake
        .script_held("session.create", opened_session(session_info("s1")));
    h.fake.script_error_with(
        "input.push",
        -32603,
        "unknown",
        "transport_uncertain",
        Some("unknown"),
    );
    let payload = "first paste\n".repeat(12);
    h.app.on_paste(&payload);
    h.action(Action::Submit);
    h.fake.wait_for_calls("session.create", 1).await;
    assert!(
        h.app.local_input_bytes() >= payload.len() * 2,
        "the creation copy and submission are both owned"
    );
    h.app.composer.set_text("next draft while creating");
    h.fake.release("session.create");
    h.until(|app| {
        app.local_inputs
            .iter()
            .any(|input| input.status == Status::Unconfirmed)
    })
    .await;
    assert_eq!(h.app.composer.text(), "next draft while creating");
    assert_eq!(h.app.local_inputs[0].entry.text(), payload);
    assert!(!h.app.composer.history_prev());
    h.press(KeyCode::F(8));
    h.press(KeyCode::Char('e'));
    h.press(KeyCode::Home);
    h.press(KeyCode::End);
    let original = h.app.local_inputs[0].id;
    h.app.unconfirmed_editor.set_text("");
    h.press(KeyCode::Enter);
    h.press(KeyCode::Char('y'));
    assert_eq!(
        h.calls("input.push").len(),
        1,
        "an empty edit is not a new send or resolution"
    );
    assert_eq!(h.app.local_inputs[0].id, original);
    assert_eq!(h.app.local_inputs[0].status, Status::Unconfirmed);
}

#[tokio::test(start_paused = true)]
async fn text_input_uses_its_own_editor_and_explicit_answer_kind_without_consuming_the_conversation_draft()
 {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.app.composer.set_text("conversation draft stays here");
    h.fake
        .script_held("interaction.answer", json!({"accepted":true}));
    h.control_event(json!({"pending_interaction":text_interaction("text-1")}))
        .await;
    h.until(|app| app.has_interaction()).await;
    assert!(matches!(
        h.session().interaction.as_ref().unwrap().kind,
        crate::app::InteractionKind::Text
    ));
    h.type_text("y an answer n");
    h.app.on_paste(" second portion");
    h.press(KeyCode::Left);
    h.press(KeyCode::Backspace);
    h.press(KeyCode::End);
    assert_eq!(h.app.composer.text(), "conversation draft stays here");
    assert!(h.calls("interaction.answer").is_empty());
    let expected = h.session().interaction_editor.text().to_owned();
    assert!(expected.starts_with("y an answer n "));
    let screen = h.screen(100, 25);
    assert!(screen.contains("Describe the branch to use"), "{screen}");
    assert!(screen.contains("Text answer"), "{screen}");
    assert!(!screen.contains("Allow once"), "{screen}");
    assert!(
        h.app
            .on_key(crossterm::event::KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE
            ))
            .is_none(),
        "the new text prompt preserves the answer arming window"
    );
    arm_prompt().await;
    h.press(KeyCode::Enter);
    h.fake.wait_for_calls("interaction.answer", 1).await;
    let request = h.calls("interaction.answer").remove(0);
    assert_eq!(request["answer"], json!({"type":"text","text":expected}));
    assert_eq!(request["interaction_id"], "text-1");
    assert_eq!(request["loop_id"], "l1");
    assert_eq!(request["load"], crate::testing::LOAD);
    assert_eq!(h.app.composer.text(), "conversation draft stays here");
}

#[tokio::test(start_paused = true)]
async fn text_editor_is_preserved_by_same_id_revisions_but_old_answers_cannot_replace_a_new_prompt()
{
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake
        .script_held("interaction.answer", json!({"accepted":true}));
    h.control_event(json!({"pending_interaction":text_interaction("first")}))
        .await;
    h.until(|app| app.has_interaction()).await;
    h.type_text("first answer");
    h.control_event(json!({"pending_interaction":text_interaction("first")}))
        .await;
    h.settle().await;
    assert_eq!(h.session().interaction_editor.text(), "first answer");
    arm_prompt().await;
    h.press(KeyCode::Enter);
    h.fake.wait_for_calls("interaction.answer", 1).await;
    h.control_event(json!({"pending_interaction":text_interaction("second")}))
        .await;
    h.until(|app| {
        app.resident
            .current()
            .unwrap()
            .interaction
            .as_ref()
            .unwrap()
            .interaction_id
            == "second"
    })
    .await;
    assert!(h.session().interaction_editor.is_empty());
    h.type_text("new answer");
    h.fake.release("interaction.answer");

    h.settle().await;
    let session = h.session();
    assert_eq!(
        session.interaction.as_ref().unwrap().interaction_id,
        "second"
    );
    assert_eq!(session.interaction_editor.text(), "new answer");
    assert_eq!(h.calls("interaction.answer").len(), 1);
}

#[tokio::test(start_paused = true)]
async fn empty_or_oversized_text_answers_are_rejected_without_truncating_or_sending_them() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake
        .script_held("interaction.answer", json!({"accepted":true}));
    h.control_event(json!({"pending_interaction":text_interaction("text")}))
        .await;
    h.until(|app| app.has_interaction()).await;
    for text in [String::new(), "bad\ntext".to_owned(), "界".repeat(2731)] {
        h.action(crate::app::Action::Answer(crate::app::Answer::Text(text)));
    }
    h.settle().await;
    assert!(h.calls("interaction.answer").is_empty());
    let text = "é".repeat(crate::app::session::TEXT_ANSWER_MAX_BYTES / 2);
    h.action(crate::app::Action::Answer(crate::app::Answer::Text(
        text.clone(),
    )));
    h.fake.wait_for_calls("interaction.answer", 1).await;
    assert_eq!(h.calls("interaction.answer")[0]["answer"]["text"], text);
    h.action(crate::app::Action::Answer(crate::app::Answer::Text(
        "   ".into(),
    )));
    h.fake.wait_for_calls("interaction.answer", 2).await;
    assert_eq!(
        h.calls("interaction.answer")[1]["answer"]["text"],
        "   ",
        "Agent-legal whitespace must not be trimmed or rejected"
    );
}

/// `count` prompts, each followed by a three-line answer.
fn conversation(h: &mut Harness, count: usize) {
    let transcript = &mut h.session_mut().transcript;
    for index in 0..count {
        transcript.push(Block::User {
            text: format!("prompt {index}"),
            kind: UserKind::Prompt,
            at: None,
        });
        transcript.push(Block::Assistant {
            key: RequestKey {
                loop_id: format!("l{index}"),
                request_index: 0,
            },
            reasoning: String::new(),
            text: format!("answer {index}\n\nline two\n\nline three"),
            streaming: false,
            expanded: None,
        });
    }
}

/// The first transcript row with text; an anchored block may start with the
/// blank row that separates it from the one above, or a card's pad row.
fn first_row(h: &mut Harness) -> String {
    let screen = h.screen(60, 24);
    // The scrollbar fills the right edge for a moment after a scroll.
    let bare = |row: &str| row.trim_end_matches(['│', '┃', ' ']).to_owned();
    screen
        .lines()
        .map(bare)
        .find(|row| !matches!(row.trim(), "" | "▎"))
        .unwrap_or_default()
}

/// A running turn that has just asked to run `bash`.
async fn asking() -> Harness {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake
        .script("input.push", starting_reply("l1", h.next_revision()));
    h.fake.script_held("turn.wait", waited("l1", completed()));
    h.submit("write a file");
    h.until(|app| app.is_busy()).await;
    let mut waiting = running_snapshot(h.next_revision(), "l1", "write a file");
    waiting["execution"]["phase"] = json!("waiting_for_input");
    waiting["pending_interaction"] = json!({"loop_id":"l1","request_index":0,
        "interaction_id":"i1","tool_call_id":"c1","tool_name":"bash",
        "kind":{"type":"approval","data":{"prompt":"Allow bash?"}}});
    h.fake
        .script("session.state@s1", state_response(waiting.clone()));
    h.fake.script(
        "interaction.answer",
        after_snapshot(
            json!({"ok":true}),
            running_snapshot(h.next_revision() + 1, "l1", "write a file"),
        ),
    );
    h.snapshot_event(waiting).await;
    h.until(|app| app.has_interaction()).await;
    h
}

#[tokio::test(start_paused = true)]
async fn an_approval_prompt_owns_the_keys_and_answers_over_the_wire() {
    let mut h = asking().await;
    let screen = h.screen(80, 20);
    for text in [
        "Allow bash?",
        "Allow once",
        "Deny",
        "Waiting for approval",
        "esc to deny",
    ] {
        assert!(screen.contains(text), "{text}:\n{screen}");
    }

    h.type_text("oops");
    h.ctrl(KeyCode::Char('j'));
    h.app.on_paste("a whole pasted prompt");
    assert_eq!(
        h.app.composer.text(),
        "",
        "the draft behind the sheet stays empty"
    );

    h.press(KeyCode::Down);
    arm_prompt().await;
    h.press(KeyCode::Enter);
    h.settle().await;
    let answered = h.calls("interaction.answer");
    assert_eq!(answered.len(), 1);
    assert_eq!(answered[0]["interaction_id"], "i1");
    assert_eq!(answered[0]["answer"]["decision"], "deny");
    assert!(!h.app.has_interaction());
}

#[tokio::test(start_paused = true)]
async fn answer_keys_pressed_as_the_prompt_appears_do_nothing_until_it_is_armed() {
    let mut h = asking().await;
    for key in [KeyCode::Char('y'), KeyCode::Char('a'), KeyCode::Char('n')] {
        h.press(key);
    }
    h.press(KeyCode::Enter);
    h.run_for(Duration::from_millis(50)).await;
    assert!(h.calls("interaction.answer").is_empty());
    assert!(h.app.has_interaction());

    arm_prompt().await;
    h.press(KeyCode::Char('y'));
    h.settle().await;
    let answered = h.calls("interaction.answer");
    assert_eq!(answered.len(), 1);
    assert_eq!(answered[0]["answer"]["decision"], "allow_once");
}

#[tokio::test(start_paused = true)]
async fn typing_on_keeps_a_prompt_unarmed_until_a_pause() {
    let mut h = asking().await;
    for _ in 0..5 {
        tokio::time::advance(Duration::from_millis(400)).await;
        h.type_text("a sentence");
    }
    h.press(KeyCode::Enter);
    h.run_for(Duration::from_millis(50)).await;
    assert!(h.calls("interaction.answer").is_empty());

    arm_prompt().await;
    h.press(KeyCode::Char('n'));
    h.settle().await;
    let answered = h.calls("interaction.answer");
    assert_eq!(answered.len(), 1);
    assert_eq!(answered[0]["answer"]["decision"], "deny");
}

#[tokio::test(start_paused = true)]
async fn escape_denies_a_prompt_that_is_not_armed_yet() {
    let mut h = asking().await;
    h.press(KeyCode::Esc);
    h.settle().await;
    let answered = h.calls("interaction.answer");
    assert_eq!(answered.len(), 1);
    assert_eq!(answered[0]["answer"]["decision"], "deny");
}

#[tokio::test(start_paused = true)]
async fn a_shifted_end_does_not_select_in_the_draft_behind_a_prompt() {
    let mut h = asking().await;
    h.app.composer.set_text("draft");
    h.app.composer.move_home();
    h.key(KeyCode::End, KeyModifiers::SHIFT);
    assert!(h.app.composer.selection().is_none());
}

#[tokio::test(start_paused = true)]
async fn the_transcript_scrolls_while_a_prompt_waits() {
    let mut h = asking().await;
    conversation(&mut h, 12);
    h.screen(60, 24);
    h.press(KeyCode::PageUp);
    assert_ne!(h.app.scroll, Scroll::Follow);
    h.press(KeyCode::End);
    assert_eq!(h.app.scroll, Scroll::Follow);
    h.screen(60, 24);
    h.ctrl(KeyCode::Up);
    assert_ne!(h.app.scroll, Scroll::Follow);
    let expanded = h.app.tools_expanded;
    h.ctrl(KeyCode::Char('o'));
    assert_ne!(h.app.tools_expanded, expanded);
    assert!(h.app.has_interaction(), "the prompt stays");
    assert!(h.calls("interaction.answer").is_empty());
}

#[tokio::test(start_paused = true)]
async fn shift_enter_adds_a_line_and_alt_enter_queues_follow_ups_behind_a_running_turn() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    let one = entry("i2", "follow_up", "one");
    let two = entry("i3", "follow_up", "two");
    h.fake.script_sequence(
        "input.push",
        vec![
            starting_reply("l1", 2),
            after_snapshot(
                accepted("s1", "i2"),
                running_queue(3, "l1", "first line\nxy", vec![one.clone()]),
            ),
            after_snapshot(
                accepted("s1", "i3"),
                running_queue(4, "l1", "first line\nxy", vec![one, two]),
            ),
        ],
    );
    h.fake.script_held("turn.wait", waited("l1", completed()));

    h.app.composer.set_text("first line");
    h.key(KeyCode::Enter, KeyModifiers::SHIFT);
    h.type_text("xy");
    assert_eq!(h.app.composer.text(), "first line\nxy");
    h.press(KeyCode::Enter);
    h.until(|app| app.is_busy()).await;
    assert_eq!(h.app.composer.text(), "");

    for text in ["one", "two"] {
        h.app.composer.set_text(text);
        h.key(KeyCode::Enter, KeyModifiers::ALT);
    }
    h.settle().await;
    assert_eq!(h.queue(), ["one", "two"]);
    assert_eq!(
        h.pushes()[1..],
        [
            ("follow_up".to_owned(), "one".to_owned()),
            ("follow_up".to_owned(), "two".to_owned())
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn enter_pushes_a_steer_alt_and_ctrl_enter_a_follow_up_and_the_dock_follows_the_agents_order()
{
    let mut h = Harness::opened(display_read(0, 0)).await;
    let alt = entry("i2", "follow_up", "by alt enter");
    let ctrl = entry("i3", "follow_up", "by ctrl enter");
    let now = entry("i4", "steer", "now");
    h.fake.script_sequence(
        "input.push",
        vec![
            starting_reply("l1", 2),
            after_snapshot(
                accepted("s1", "i2"),
                running_queue(3, "l1", "start", vec![alt.clone()]),
            ),
            after_snapshot(
                accepted("s1", "i3"),
                running_queue(4, "l1", "start", vec![alt.clone(), ctrl.clone()]),
            ),
            // The Agent runs steers first.
            after_snapshot(
                accepted("s1", "i4"),
                running_queue(5, "l1", "start", vec![now, alt, ctrl]),
            ),
        ],
    );
    h.fake.script_held("turn.wait", waited("l1", completed()));
    h.submit("start");
    h.until(|app| app.is_busy()).await;

    h.key(KeyCode::Enter, KeyModifiers::ALT);
    assert_eq!(h.sends(), ["start"], "an empty draft pushes nothing");

    h.type_text("by alt enter");
    h.key(KeyCode::Enter, KeyModifiers::ALT);
    h.type_text("by ctrl enter");
    h.key(KeyCode::Enter, KeyModifiers::CONTROL);
    h.type_text("now");
    h.press(KeyCode::Enter);
    h.settle().await;
    assert_eq!(
        h.pushes(),
        [
            ("steer", "start"),
            ("follow_up", "by alt enter"),
            ("follow_up", "by ctrl enter"),
            ("steer", "now"),
        ]
        .map(|(mode, text)| (mode.to_owned(), text.to_owned()))
    );
    assert_eq!(h.queue(), ["now", "by alt enter", "by ctrl enter"]);
    let screen = h.screen(60, 14);
    let at = |needle: &str| {
        screen
            .find(needle)
            .unwrap_or_else(|| panic!("{needle}\n{screen}"))
    };
    assert!(
        at("↗ steer  now") < at("↳ follow-up 1  by alt enter"),
        "{screen}"
    );
    assert!(
        at("follow-up 1") < at("↳ follow-up 2  by ctrl enter"),
        "{screen}"
    );
}

#[tokio::test(start_paused = true)]
async fn tab_does_nothing_in_the_composer() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake
        .script("input.push", starting_reply("l1", h.next_revision()));
    h.fake.script_held("turn.wait", waited("l1", completed()));
    h.type_text("draft");
    h.press(KeyCode::Tab);
    assert_eq!(h.app.composer.text(), "draft");
    h.press(KeyCode::Enter);
    h.until(|app| app.is_busy()).await;

    h.type_text("busy");
    h.press(KeyCode::Tab);
    assert_eq!(h.app.composer.text(), "busy");
    assert!(h.queue().is_empty());
}

#[tokio::test(start_paused = true)]
async fn tab_with_a_completion_list_open_completes() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.type_text("/mode");
    h.press(KeyCode::Tab);
    assert_eq!(h.app.composer.text(), "/model ");
}

#[tokio::test(start_paused = true)]
async fn a_message_sent_during_a_compaction_waits_in_the_agents_queue_without_a_turn() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script("input.push", accepted("s1", "i1"));
    h.control_event(json!({"compaction": {
        "operation_id": "auto-boundary-l1-1", "phase": "summarizing",
        "covered_item_count": 8, "retained_item_count": 2,
    }}))
    .await;
    h.until(|app| app.compacting()).await;
    let observed = h.control(json!({"queue":queue(vec![entry("i1","follow_up","later")],false),
        "compaction":{"operation_id":"auto-boundary-l1-1","phase":"summarizing","covered_item_count":8,"retained_item_count":2}}));
    h.fake
        .script("input.push", after_snapshot(accepted("s1", "i1"), observed));
    h.app.composer.set_text("later");
    h.action(crate::app::Action::QueueFollowUp);
    h.settle().await;
    assert_eq!(h.queue(), ["later"]);
    assert_eq!(h.pushes(), [("follow_up".to_owned(), "later".to_owned())]);
    assert!(!h.is_busy());
    assert!(h.calls("turn.wait").is_empty(), "the TUI starts nothing");
}

#[tokio::test(start_paused = true)]
async fn ctrl_c_clears_a_draft_and_a_second_press_within_half_a_second_quits() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.type_text("x");
    h.ctrl(KeyCode::Char('c'));
    assert_eq!(h.app.composer.text(), "");
    assert!(!h.app.quit);
    h.ctrl(KeyCode::Char('c'));
    assert!(h.app.quit, "the second press is inside the window");

    let mut h = Harness::opened(display_read(0, 0)).await;
    h.ctrl(KeyCode::Char('c'));
    assert!(!h.app.quit, "the first press only arms the quit");
    assert_eq!(h.notices(), ["Press Ctrl+C again to quit"]);
    tokio::time::advance(std::time::Duration::from_millis(600)).await;
    h.ctrl(KeyCode::Char('c'));
    assert!(!h.app.quit, "a late press arms again");
    h.type_text("x");
    h.ctrl(KeyCode::Char('c'));
    assert!(!h.app.quit, "typing disarmed the quit");
    h.ctrl(KeyCode::Char('c'));
    assert!(h.app.quit);
}

#[tokio::test(start_paused = true)]
async fn the_palette_delivers_the_typed_argument_to_the_accepted_command() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    let mut updated = session_info("s1");
    updated["reasoning"] = json!("max");
    h.fake.script("session.update", json!({"session": updated}));

    h.type_text("/rename a better title");
    h.press(KeyCode::Enter);
    h.settle().await;
    assert_eq!(h.calls("session.rename")[0]["title"], "a better title");
    assert_eq!(h.app.composer.text(), "");

    // `/res` lists resume before reasoning; the argument belongs to the row
    // the user accepts.
    h.type_text("/res max");
    let screen = h.screen(80, 24);
    let resume = screen.find("resume").expect("resume row");
    let reasoning = screen.find("reasoning").expect("reasoning row");
    assert!(resume < reasoning, "{screen}");
    h.press(KeyCode::Down);
    h.press(KeyCode::Enter);
    h.settle().await;
    assert_eq!(h.calls("session.update")[0]["reasoning"], "max");
    assert_eq!(h.app.composer.text(), "");
    assert!(h.app.overlay.is_none(), "{:?}", h.app.overlay);
}

#[tokio::test(start_paused = true)]
async fn a_slash_line_that_names_no_command_is_a_prompt() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script_sequence(
        "input.push",
        vec![
            starting_reply("l1", 2),
            after_snapshot(
                accepted("s1", "i2"),
                running_queue(
                    3,
                    "l1",
                    "/usr/bin/env is broken",
                    vec![entry("i2", "steer", "/var/log is huge")],
                ),
            ),
        ],
    );
    h.fake.script_held("turn.wait", waited("l1", completed()));

    h.type_text("/usr/bin/env is broken");
    h.press(KeyCode::Enter);
    h.until(|app| app.is_busy()).await;
    assert_eq!(h.sends(), ["/usr/bin/env is broken"]);
    assert!(h.app.overlay.is_none());
    assert!(h.notices().is_empty());

    // Without the palette (a recalled or edited draft) the line still goes out.
    h.app.composer.set_text("/var/log is huge");
    h.press(KeyCode::Enter);
    h.settle().await;
    assert_eq!(h.sends()[1], "/var/log is huge");
}

#[tokio::test(start_paused = true)]
async fn slash_tool_is_an_unknown_command() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    assert!(crate::app::commands::execute("tool", "").is_none());
    assert!(crate::app::commands::filter("tool", &[]).is_empty());
    h.type_text("/tool");
    h.press(KeyCode::Enter);
    assert_eq!(
        h.notices(),
        ["Unknown command /tool · enter again sends it as text"]
    );
    assert!(h.app.overlay.is_none());
    assert_eq!(h.app.composer.text(), "/tool");
    h.settle().await;
    assert!(h.calls("tool.read").is_empty());
    assert!(h.sends().is_empty());
}

#[tokio::test(start_paused = true)]
async fn at_opens_the_file_picker_only_at_the_start_of_a_word() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.type_text("mail me@example.com");
    assert!(h.app.overlay.is_none());
    assert_eq!(h.app.composer.text(), "mail me@example.com");

    h.type_text(" @");
    assert!(matches!(h.app.overlay, Overlay::Files { .. }));
    h.press(KeyCode::Esc);
    h.app.composer.clear();
    h.type_text("@");
    assert!(matches!(h.app.overlay, Overlay::Files { .. }));
}

#[tokio::test(start_paused = true)]
async fn ctrl_and_alt_move_and_delete_by_word() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    for modifier in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
        h.app.composer.set_text("one two three");
        h.key(KeyCode::Left, modifier);
        h.key(KeyCode::Left, modifier);
        h.type_text("[");
        h.key(KeyCode::Right, modifier);
        h.type_text("]");
        assert_eq!(h.app.composer.text(), "one [two ]three");

        h.press(KeyCode::End);
        h.key(KeyCode::Backspace, modifier);
        assert_eq!(h.app.composer.text(), "one [two ");
    }
    h.app.composer.set_text("one two");
    h.ctrl(KeyCode::Char('h'));
    assert_eq!(h.app.composer.text(), "one ", "legacy terminals send ^H");
    h.press(KeyCode::Backspace);
    assert_eq!(h.app.composer.text(), "one");
}

#[tokio::test(start_paused = true)]
async fn alt_d_and_alt_delete_delete_the_next_word_and_ctrl_x_copies_the_answer() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.app.composer.set_text("one two three");
    h.press(KeyCode::Home);
    h.key(KeyCode::Char('d'), KeyModifiers::ALT);
    assert_eq!(
        h.app.composer.text(),
        " two three",
        "typed nothing, deleted a word"
    );
    h.key(KeyCode::Delete, KeyModifiers::ALT);
    assert_eq!(h.app.composer.text(), " three");

    h.ctrl(KeyCode::Char('x'));
    assert_eq!(h.notices(), ["No answer to copy yet"]);
    assert_eq!(
        h.app.composer.text(),
        " three",
        "Ctrl+X leaves the draft alone"
    );
}

fn wheel(h: &mut Harness, kind: MouseEventKind) {
    h.app.on_mouse(MouseEvent {
        kind,
        column: 5,
        row: 5,
        modifiers: KeyModifiers::NONE,
    });
}

#[tokio::test(start_paused = true)]
async fn the_wheel_scrolls_the_open_overlay_never_the_hidden_transcript() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    conversation(&mut h, 12);
    h.screen(60, 24);

    h.press(KeyCode::F(1));
    wheel(&mut h, MouseEventKind::ScrollDown);
    assert!(matches!(h.app.overlay, Overlay::Help { scroll: 3 }));
    wheel(&mut h, MouseEventKind::ScrollUp);
    assert!(matches!(h.app.overlay, Overlay::Help { scroll: 0 }));
    h.press(KeyCode::Esc);

    h.type_text("/");
    wheel(&mut h, MouseEventKind::ScrollDown);
    assert!(matches!(h.app.overlay, Overlay::Palette { selected: 1 }));
    h.press(KeyCode::Esc);
    h.app.composer.clear();

    h.type_text("/context");
    h.press(KeyCode::Enter);
    wheel(&mut h, MouseEventKind::ScrollUp);
    assert!(matches!(h.app.overlay, Overlay::Context));
    assert_eq!(h.app.scroll, Scroll::Follow, "the transcript did not move");

    h.press(KeyCode::Esc);
    wheel(&mut h, MouseEventKind::ScrollUp);
    assert!(matches!(h.app.scroll, Scroll::Anchor { .. }));
}

#[tokio::test(start_paused = true)]
async fn escape_closes_a_picker_and_takes_only_its_token() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script(
        "workspace.files",
        json!({"entries": [{"path": "src/main.rs", "kind": "file"},
                           {"path": "README.md", "kind": "file"}],
               "truncated": false, "scan_complete": true}),
    );
    h.type_text("/di");
    h.press(KeyCode::Esc);
    assert!(h.app.overlay.is_none());
    assert_eq!(h.app.composer.text(), "/di", "the draft stays");

    h.app.composer.clear();
    h.type_text("/usr/local/bin/foo is a path");
    h.press(KeyCode::Esc);
    assert_eq!(h.app.composer.text(), "/usr/local/bin/foo is a path");

    h.app.composer.set_text("fix the bug in ");
    h.type_text("@ap");
    h.press(KeyCode::Esc);
    assert!(h.app.overlay.is_none());
    assert_eq!(h.app.composer.text(), "fix the bug in @ap");

    h.app.composer.set_text("fix the bug in ");
    h.type_text("@zzz");
    h.settle().await;
    h.press(KeyCode::Enter);
    assert!(!h.app.overlay.is_none(), "no match keeps the picker open");
    h.press(KeyCode::Esc);
    h.app.composer.set_text("fix the bug in ");
    h.type_text("@main");
    h.settle().await;
    h.press(KeyCode::Enter);
    assert_eq!(h.app.composer.text(), "fix the bug in @src/main.rs ");
}

#[tokio::test(start_paused = true)]
async fn the_command_list_filters_wraps_and_applies() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.type_text("/");
    assert!(matches!(h.app.overlay, Overlay::Palette { selected: 0 }));
    h.press(KeyCode::Up);
    let last = crate::app::commands::COMMANDS.len() - 1;
    assert!(matches!(h.app.overlay, Overlay::Palette { selected } if selected == last));
    h.press(KeyCode::Down);
    assert!(matches!(h.app.overlay, Overlay::Palette { selected: 0 }));

    h.press(KeyCode::Backspace);
    assert!(h.app.overlay.is_none(), "backspace past the slash closes");
    assert_eq!(h.app.composer.text(), "");

    h.type_text("/mo");
    h.press(KeyCode::Tab);
    assert!(h.app.overlay.is_none());
    assert_eq!(
        h.app.composer.text(),
        "/model ",
        "tab completes without running"
    );

    h.app.composer.clear();
    h.type_text("/mo");
    h.press(KeyCode::Enter);
    assert!(matches!(h.app.overlay, Overlay::Models { .. }));
    assert_eq!(h.app.composer.text(), "");
}

#[tokio::test(start_paused = true)]
async fn paging_keeps_four_rows_and_ctrl_arrows_jump_between_prompts() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    conversation(&mut h, 12);
    let before = h.screen(60, 24);
    let rows = h.viewport();
    assert!(before.contains("line three"), "{before}");

    h.press(KeyCode::PageUp);
    let after = h.screen(60, 24);
    let bare = |row: &str| row.trim_end_matches(['│', '┃', ' ']).to_owned();
    let old: Vec<String> = before.lines().take(rows).map(bare).collect();
    let new: Vec<String> = after.lines().take(rows).map(bare).collect();
    assert_eq!(
        new[rows - 4],
        old[0],
        "four rows of the last page stay:\n{after}"
    );
    h.press(KeyCode::PageDown);
    assert_eq!(h.app.scroll, Scroll::Follow);

    h.ctrl(KeyCode::Up);
    let top = first_row(&mut h);
    let index: usize = top
        .rsplit(' ')
        .next()
        .and_then(|number| number.parse().ok())
        .unwrap_or_else(|| panic!("a prompt at the top: {top:?}"));
    h.ctrl(KeyCode::Up);
    assert!(first_row(&mut h).ends_with(&format!("prompt {}", index - 1)));
    h.ctrl(KeyCode::Down);
    assert!(first_row(&mut h).ends_with(&format!("prompt {index}")));
    for _ in index..12 {
        h.ctrl(KeyCode::Down);
    }
    assert_eq!(
        h.app.scroll,
        Scroll::Follow,
        "past the last prompt the view follows"
    );

    h.ctrl(KeyCode::Home);
    assert!(
        first_row(&mut h).ends_with(concat!("minicore-tui v", env!("CARGO_PKG_VERSION"))),
        "the header tops the transcript"
    );
    assert!(h.screen(60, 24).contains("prompt 0"));
    h.ctrl(KeyCode::End);
    assert_eq!(h.app.scroll, Scroll::Follow);
    h.press(KeyCode::Home);
    assert!(
        first_row(&mut h).ends_with(concat!("minicore-tui v", env!("CARGO_PKG_VERSION"))),
        "home with no draft goes to the top"
    );
    h.press(KeyCode::End);
    assert_eq!(h.app.scroll, Scroll::Follow, "end follows the tail again");
}

#[tokio::test(start_paused = true)]
async fn the_find_bar_moves_the_view_with_every_step() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    conversation(&mut h, 12);
    h.screen(60, 24);
    h.ctrl(KeyCode::Char('f'));
    h.type_text("prompt 1");
    let screen = h.screen(60, 24);
    assert!(screen.contains("1 of 3"), "prompt 1, 10 and 11:\n{screen}");
    assert!(
        first_row(&mut h).ends_with("prompt 1"),
        "typing shows match 1"
    );
    h.press(KeyCode::Down);
    assert!(first_row(&mut h).ends_with("prompt 10"));
    // The last match cannot reach the top: the view stops at the end.
    h.press(KeyCode::Tab);
    let screen = h.screen(60, 24);
    assert!(
        screen.contains("3 of 3") && screen.contains("prompt 11"),
        "{screen}"
    );
    h.press(KeyCode::Up);
    assert!(first_row(&mut h).ends_with("prompt 10"));
    h.press(KeyCode::Enter);
    let screen = h.screen(60, 24);
    assert!(
        screen.contains("3 of 3") && screen.contains("prompt 11"),
        "{screen}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_match_in_block_0_is_shown_below_the_welcome_header() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    conversation(&mut h, 12);
    h.screen(60, 24);
    h.ctrl(KeyCode::Char('f'));
    h.type_text("prompt 0");
    assert!(
        first_row(&mut h).ends_with("prompt 0"),
        "the view lands on the match, not on the header:\n{}",
        h.screen(60, 24)
    );
}

#[tokio::test(start_paused = true)]
async fn errors_stay_until_a_key_and_info_notices_expire() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.app.notice(NoticeLevel::Error, "boom");
    tokio::time::advance(Duration::from_secs(60)).await;
    h.app.tick();
    assert_eq!(h.notices(), ["boom"]);
    assert!(
        h.app.next_expiry().is_none(),
        "an error alone does not keep the clock running"
    );
    h.press(KeyCode::Left);
    assert!(h.notices().is_empty());

    h.app.notice(NoticeLevel::Info, "copied");
    tokio::time::advance(Duration::from_secs(6)).await;
    h.app.tick();
    assert!(h.notices().is_empty());
    assert!(h.app.next_expiry().is_none());
}

#[tokio::test(start_paused = true)]
async fn a_flash_and_the_scrollbar_wake_the_clock_once_at_their_end() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    let flashed = tokio::time::Instant::now();
    h.app.on_msg(Msg::Flash("Copied!".into()));
    let step = Duration::from_millis(300);
    tokio::time::advance(step).await;
    h.app.scrolled_at = Some(tokio::time::Instant::now());
    assert!(!h.app.animating(), "nothing moves on every frame");
    assert_eq!(h.app.next_expiry(), Some(flashed + FLASH_TTL));

    tokio::time::advance(FLASH_TTL - step).await;
    h.app.tick();
    assert_eq!(h.app.next_expiry(), Some(flashed + step + FLASH_TTL));
    tokio::time::advance(step).await;
    h.app.tick();
    assert!(h.app.next_expiry().is_none() && !h.app.scrollbar_shown());
}

#[tokio::test(start_paused = true)]
async fn r_in_the_context_sheet_reads_the_ledger_again() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.type_text("/context");
    h.press(KeyCode::Enter);
    h.settle().await;
    let before = h.calls("session.context").len();
    h.press(KeyCode::Char('r'));
    h.settle().await;
    assert_eq!(h.calls("session.context").len(), before + 1);
    assert!(h.screen(80, 20).contains("r refresh · c copy · esc close"));
}

#[tokio::test(start_paused = true)]
async fn pointer_motion_asks_for_a_redraw_only_over_the_scrollbar() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    conversation(&mut h, 12);
    h.screen(60, 24);
    let moved = |h: &mut Harness, column| {
        h.app.on_mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column,
            row: 5,
            modifiers: KeyModifiers::NONE,
        })
    };
    assert!(!moved(&mut h, 20), "motion over text changes nothing");
    assert!(moved(&mut h, 59), "motion over the scrollbar shows it");
}

#[tokio::test(start_paused = true)]
async fn a_local_session_tool_grant_is_recorded_only_after_a_successful_answer() {
    for accepted in [false, true] {
        let mut h = Harness::opened(display_read(0, 0)).await;
        if accepted {
            h.fake
                .script_held("interaction.answer", json!({"accepted":true}));
        } else {
            h.fake.script_error_held(
                "interaction.answer",
                -32000,
                "already resolved",
                "invalid_state",
            );
        }
        h.control_event(
            json!({"pending_interaction":{"loop_id":"l1","request_index":0,
            "interaction_id":"approval","tool_call_id":"call","tool_name":"bash",
            "kind":{"type":"approval","data":{"prompt":"Allow?"}}}}),
        )
        .await;
        h.until(|app| app.has_interaction()).await;
        h.action(crate::app::Action::Answer(crate::app::Answer::AllowTool));
        h.fake.wait_for_calls("interaction.answer", 1).await;
        assert!(
            h.session().allowed_tools.is_empty(),
            "sending is not acceptance"
        );
        h.fake.release("interaction.answer");
        h.settle().await;
        assert_eq!(h.session().allowed_tools.contains("bash"), accepted);
        assert_eq!(h.calls("interaction.answer").len(), 1);
    }
}
