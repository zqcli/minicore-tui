//! 0.2.3 focused regressions for the acknowledged-settings footer contract and
//! the accepted-steer user-card rendering. Tests drive the real App through
//! selector confirm / response ack / steer response / history reconciliation
//! events and assert the rendered terminal, never abstract helpers.

use crate::app::App;
use crate::event::{AppEvent, RpcEvent};
use crate::protocol::{IncomingFrame, Reasoning, RpcNotification};
use crate::state::turn::PendingSteerState;
use crate::theme::ThemeKind;
use crate::ui::component_tests::{draw, text};
use crate::ui::testapp;

fn agent_event(app: &mut App, value: serde_json::Value) {
    app.update(AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Notification(
        RpcNotification::AgentEvent(serde_json::from_value(value).unwrap()),
    ))));
}

/// Starts a live loop on the luna session whose request 0 metadata is the
/// historical `high`/`luna` (the value a prior turn actually ran with).
fn luna_live_loop(app: &mut App) {
    let commands = testapp::take_requests(app.update(AppEvent::SubmitTurn {
        session_id: "ses_main".to_owned(),
        text: "stream me".to_owned(),
    }));
    assert_eq!(commands.len(), 1);
    agent_event(
        app,
        serde_json::json!({
            "type": "turn_started",
            "data": {"turn": {"session_id": "ses_main", "loop_id": "loop_live"},
                     "meta": {"session_id": "ses_main", "dropped_before": 0}}
        }),
    );
    agent_event(
        app,
        serde_json::json!({
            "type": "request_started",
            "data": {
                "turn": {"session_id": "ses_main", "loop_id": "loop_live"},
                "request_index": 0,
                "config_revision": 0,
                "model": "luna",
                "reasoning": "high",
                "meta": {"session_id": "ses_main", "dropped_before": 0}
            }
        }),
    );
}

/// RED contract: after a session.update ack the footer must show the
/// acknowledged session reasoning immediately (idle AND live) without a new
/// turn, while the running request's own metadata stays immutable.
#[test]
fn footer_reflects_acknowledged_reasoning_immediately_while_live() {
    let mut app = testapp::luna_session(ThemeKind::Dark);
    luna_live_loop(&mut app);

    app.update(AppEvent::OpenReasoningSelector);
    app.update(AppEvent::MoveSelector { delta: 2 });
    let commands = testapp::take_requests(app.update(AppEvent::ConfirmDock));
    let update = commands
        .iter()
        .find(|request| request.method == "session.update")
        .expect("session.update issued");
    testapp::respond(
        &mut app,
        update,
        serde_json::json!({
            "session": {
                "session_id": "ses_main", "title": null, "profile": "coding",
                "workspace": "/work/cli", "model": "luna", "reasoning": "max",
                "loaded": true, "created_at": "2027-01-15T07:55:00Z",
                "updated_at": "2027-01-15T07:55:00Z"
            },
            "active_revision": null
        }),
    );

    let content = text(&draw(&app, 80, 24));
    assert!(
        content.contains(" · max · "),
        "footer must show the acknowledged max while live: {content}"
    );
    assert!(
        !content.contains(" · high · "),
        "footer must not keep the stale live-request reasoning: {content}"
    );
    let live = app.sessions.known["ses_main"].live.as_ref().unwrap();
    assert_eq!(
        live.requests[0].reasoning,
        Reasoning::High,
        "running request metadata must stay immutable"
    );
}

/// RED contract: an acknowledged model update shows immediately while live;
/// a rejected update must not claim success (footer keeps the old model).
#[test]
fn footer_reflects_acknowledged_model_and_rejected_update_keeps_old() {
    let mut app = testapp::luna_session(ThemeKind::Dark);
    luna_live_loop(&mut app);

    // Switch model luna -> deep through the real model selector.
    app.update(AppEvent::OpenModelSelector);
    app.update(AppEvent::MoveSelector { delta: 1 });
    let commands = testapp::take_requests(app.update(AppEvent::ConfirmDock));
    let update = commands
        .iter()
        .find(|request| request.method == "session.update")
        .expect("session.update issued for the model");
    assert_eq!(update.params["model"], serde_json::json!("deep"));
    testapp::respond(
        &mut app,
        update,
        serde_json::json!({
            "session": {
                "session_id": "ses_main", "title": null, "profile": "coding",
                "workspace": "/work/cli", "model": "deep", "reasoning": "high",
                "loaded": true, "created_at": "2027-01-15T07:55:00Z",
                "updated_at": "2027-01-15T07:55:00Z"
            },
            "active_revision": null
        }),
    );

    let content = text(&draw(&app, 80, 24));
    assert!(
        content.contains(" · deep · "),
        "footer must show the acknowledged model deep while live: {content}"
    );
    assert!(
        !content.contains(" · luna · "),
        "footer must not keep the stale live-request model: {content}"
    );
    let live = app.sessions.known["ses_main"].live.as_ref().unwrap();
    assert_eq!(live.requests[0].model, "luna", "request metadata immutable");

    // A rejected update leaves the acknowledged model in place.
    app.update(AppEvent::OpenModelSelector);
    let commands = testapp::take_requests(app.update(AppEvent::ConfirmDock));
    let update = commands
        .iter()
        .find(|request| request.method == "session.update")
        .expect("second session.update issued");
    testapp::respond_rpc_error(&mut app, update, -32603, "model rejected");
    let content = text(&draw(&app, 80, 24));
    assert!(
        content.contains(" · deep · "),
        "rejected update must not rewrite the acknowledged model: {content}"
    );
}

/// RED contract: a successfully accepted pending steer renders as a shared
/// user card (rail surface, wrap, accepted_at timestamp) with a subtle
/// awaiting-history marker, not the "⠸ Steering (…)" banner.
#[test]
fn accepted_pending_steer_renders_as_user_card_with_subtle_awaiting_marker() {
    let mut app = testapp::live_turn(ThemeKind::Dark);
    let commands = testapp::take_requests(app.update(AppEvent::SteerTurn {
        session_id: "ses_1".to_owned(),
        text: "change direction now".to_owned(),
    }));
    let steer = commands
        .iter()
        .find(|request| request.method == "turn.steer")
        .expect("turn.steer issued")
        .clone();
    testapp::respond(
        &mut app,
        &steer,
        serde_json::json!({
            "ok": true,
            "accepted_at": "2026-01-02T03:04:05.000Z",
            "steer_index": 1
        }),
    );

    // 0.2.4 contract: while accepted but not yet applied, the steer lives in
    // the gray dock queue, NOT as a transcript user card.
    let content = text(&draw(&app, 80, 24));
    assert!(
        content.contains("Steering (accepted): change direction now"),
        "accepted steer must render as a gray dock queue row: {content}"
    );
    assert!(
        !content.contains("↪ change direction now"),
        "accepted (not applied) steer must not render as a user card: {content}"
    );
    let live = app.sessions.known["ses_1"].live.as_ref().unwrap();
    assert_eq!(live.pending_steers[0].state, PendingSteerState::Queued);
    assert_eq!(live.pending_steers[0].steer_index, Some(1));

    // A receipt proves the steer entered a prepared prompt history: it leaves
    // the gray queue and renders as the actual applied Steering user card.
    apply_steer_receipt(&mut app, 1);
    let content = text(&draw(&app, 80, 24));
    assert!(
        content.contains("↪ change direction now"),
        "applied steer must render as the real user card: {content}"
    );
    assert!(
        !content.contains("Steering (accepted): change direction now"),
        "applied steer must leave the gray dock queue: {content}"
    );
    let view = &app.sessions.known["ses_1"];
    assert!(view.live.as_ref().unwrap().pending_steers.is_empty());
    assert_eq!(view.applied_steers.len(), 1);
}

/// Emits a `steer_progress` receipt for the live `loop_live` request, moving
/// up to `applied_count` FIFO accepted entries into `applied_steers`.
fn apply_steer_receipt(app: &mut App, applied_count: u64) {
    app.update(AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Notification(
        RpcNotification::AgentEvent(
            serde_json::from_value(serde_json::json!({
                "type": "steer_progress",
                "data": {
                    "turn": {"session_id": "ses_1", "loop_id": "loop_live"},
                    "request_index": 0,
                    "applied_count": applied_count,
                    "meta": {"session_id": "ses_1", "dropped_before": 0}
                }
            }))
            .unwrap(),
        ),
    ))));
}

/// RED contract: a persisted steer is owned by the durable history card; the
/// provisional card/banner is removed exactly once, and the footer no longer
/// shows a stale `queued` marker once nothing is actually pending.
#[test]
fn persisted_steer_removes_provisional_and_stale_queued() {
    let mut app = testapp::live_turn(ThemeKind::Dark);
    // Accepted then confirmed by durable history (state -> Persisted).
    let commands = testapp::take_requests(app.update(AppEvent::SteerTurn {
        session_id: "ses_1".to_owned(),
        text: "confirmed steer".to_owned(),
    }));
    let steer = commands
        .iter()
        .find(|request| request.method == "turn.steer")
        .expect("turn.steer issued")
        .clone();
    testapp::respond(
        &mut app,
        &steer,
        serde_json::json!({"ok": true, "accepted_at": "2026-01-02T03:04:05.000Z", "steer_index": 1}),
    );
    if let Some(view) = app.sessions.known.get_mut("ses_1") {
        if let Some(live) = view.live.as_mut() {
            for pending in &mut live.pending_steers {
                pending.state = PendingSteerState::Persisted;
            }
        }
    }

    let content = text(&draw(&app, 80, 24));
    assert!(
        !content.contains("⠸ Steering ("),
        "no provisional banner for a persisted steer: {content}"
    );
    assert!(
        !content.contains("queued"),
        "footer must not show a stale queued marker after reconciliation: {content}"
    );
}

// ============================================================================
// 0.2.3 review P1: folded-thinking copy excludes the actual hint row (end-2),
// and the accepted-steer "awaiting history" decoration is not copied.
// ============================================================================

use crate::state::view::{
    ConversationSelection, SectionId, SectionKind, SelectionGranularity, SelectionPoint,
};
use crate::ui::transcript;

fn assistant_entry(index: usize, loop_id: &str, text: &str, reasoning: &str) -> serde_json::Value {
    let mut content = Vec::new();
    if !reasoning.is_empty() {
        content.push(serde_json::json!({"type": "reasoning", "data": {"text": reasoning}}));
    }
    if !text.is_empty() {
        content.push(serde_json::json!({"type": "text", "data": text}));
    }
    serde_json::json!({
        "index": index,
        "item": {
            "type": "assistant",
            "data": {
                "loop_id": loop_id,
                "request_index": 0,
                "model": "deep",
                "reasoning": "high",
                "content": content,
                "usage": {},
                "finish_reason": "stop"
            }
        }
    })
}

fn section_copy(app: &App, predicate: impl Fn(&SectionId) -> bool) -> String {
    let prepared = transcript::prepare_conversation(app, 77);
    let section = prepared
        .sections
        .iter()
        .find(|range| predicate(&range.id))
        .expect("matching section present");
    let selection = ConversationSelection {
        session_id: prepared.session_id.clone().expect("active session"),
        anchor: SelectionPoint {
            row: section.rows.start,
            column: 0,
            section_id: Some(section.id.clone()),
            section_row: 0,
        },
        focus: SelectionPoint {
            row: section.rows.end.saturating_sub(1),
            column: 4096,
            section_id: Some(section.id.clone()),
            section_row: section.rows.len().saturating_sub(1),
        },
        granularity: SelectionGranularity::Character,
        dragged: false,
    };
    transcript::selection_text(&prepared, &selection)
}

/// RED contract: a folded durable Thinking section copies its content rows
/// only; the actual hint row is decorative and must never enter the copy.
#[test]
fn folded_thinking_copy_excludes_the_hint_row_in_durable() {
    let app = testapp::open_with(
        ThemeKind::Dark,
        "ses_1",
        Some("Task"),
        "high",
        vec![
            testapp::user_entry(0, "loop_1", "prompt"),
            assistant_entry(1, "loop_1", "answer", "one\ntwo\nthree\nfour"),
        ],
    );
    let text = section_copy(&app, |id| id.kind == SectionKind::Thinking);
    assert!(
        !text.contains("earlier lines") && !text.contains("ctrl+o"),
        "fold hint must be excluded from the copy: {text:?}"
    );
    assert!(
        text.contains("one") && text.contains("three") && !text.contains("four"),
        "the visible preview rows copy (hidden line stays out): {text:?}"
    );
}

/// RED contract: the same holds for the live folded Thinking tail.
#[test]
fn folded_thinking_copy_excludes_the_hint_row_in_live() {
    let mut app = testapp::open_empty(ThemeKind::Dark, "ses_1", Some("Task"), "high");
    let commands = testapp::take_requests(app.update(AppEvent::SubmitTurn {
        session_id: "ses_1".to_owned(),
        text: "stream me".to_owned(),
    }));
    assert_eq!(commands.len(), 1);
    agent_event(
        &mut app,
        serde_json::json!({
            "type": "turn_started",
            "data": {"turn": {"session_id": "ses_1", "loop_id": "loop_live"},
                     "meta": {"session_id": "ses_1", "dropped_before": 0}}
        }),
    );
    agent_event(
        &mut app,
        serde_json::json!({
            "type": "request_started",
            "data": {
                "turn": {"session_id": "ses_1", "loop_id": "loop_live"},
                "request_index": 0,
                "config_revision": 0,
                "model": "deep", "reasoning": "high",
                "meta": {"session_id": "ses_1", "dropped_before": 0}
            }
        }),
    );
    for delta in ["first\n", "second\n", "third\n", "fourth"] {
        agent_event(
            &mut app,
            serde_json::json!({
                "type": "output_delta",
                "data": {
                    "turn": {"session_id": "ses_1", "loop_id": "loop_live"},
                    "request_index": 0,
                    "channel": "reasoning",
                    "delta": delta,
                    "meta": {"session_id": "ses_1", "dropped_before": 0}
                }
            }),
        );
    }
    let text = section_copy(&app, |id| id.kind == SectionKind::Thinking);
    assert!(
        !text.contains("earlier lines") && !text.contains("ctrl+o"),
        "live fold hint must be excluded from the copy: {text:?}"
    );
    assert!(
        text.contains("first") && text.contains("third") && !text.contains("fourth"),
        "the live preview rows copy (hidden line stays out): {text:?}"
    );
}

/// RED contract: the accepted-steer awaiting-history marker is UI decoration
/// and must not enter the copy of the provisional user card.
#[test]
fn accepted_steer_awaiting_marker_is_excluded_from_the_copy() {
    let mut app = testapp::live_turn(ThemeKind::Dark);
    let commands = testapp::take_requests(app.update(AppEvent::SteerTurn {
        session_id: "ses_1".to_owned(),
        text: "change direction now".to_owned(),
    }));
    let steer = commands
        .iter()
        .find(|request| request.method == "turn.steer")
        .expect("turn.steer issued")
        .clone();
    testapp::respond(
        &mut app,
        &steer,
        serde_json::json!({"ok": true, "accepted_at": "2026-01-02T03:04:05.000Z", "steer_index": 1}),
    );
    // Applied by receipt so it renders as the provisional user card.
    apply_steer_receipt(&mut app, 1);
    let text = section_copy(&app, |id| {
        id.kind == SectionKind::User && id.history_index.is_none() && id.ordinal > 0
    });
    assert!(
        !text.contains("⠸ applied"),
        "the applied decoration must be excluded from the copy: {text:?}"
    );
    assert!(
        text.contains("change direction now"),
        "the steer body stays copyable: {text:?}"
    );
}

// ============================================================================
// 0.2.3 review P2: provisional accepted-steer selection identity. Prove the
// rebase never lands on a wrong durable User block when a global steer id
// equals another durable user's history index.
// ============================================================================

use crate::state::transcript::{TranscriptBlock, UserBlock};

/// Builds a session with `count` durable prompt cards already reconciled for
/// the SAME live loop (realistic mid-loop history), plus one accepted steer
/// whose local id equals `steer_id` (a global value numerically equal to the
/// last durable user's history index).
fn steer_identity_app(count: u32, steer_id: u32) -> crate::app::App {
    let mut app = testapp::open_empty(ThemeKind::Dark, "ses_1", Some("Task"), "high");
    let commands = testapp::take_requests(app.update(AppEvent::SubmitTurn {
        session_id: "ses_1".to_owned(),
        text: "stream me".to_owned(),
    }));
    assert_eq!(commands.len(), 1);
    agent_event(
        &mut app,
        serde_json::json!({
            "type": "turn_started",
            "data": {"turn": {"session_id": "ses_1", "loop_id": "loop_live"},
                     "meta": {"session_id": "ses_1", "dropped_before": 0}}
        }),
    );
    agent_event(
        &mut app,
        serde_json::json!({
            "type": "request_started",
            "data": {
                "turn": {"session_id": "ses_1", "loop_id": "loop_live"},
                "request_index": 0,
                "config_revision": 0,
                "model": "deep", "reasoning": "high",
                "meta": {"session_id": "ses_1", "dropped_before": 0}
            }
        }),
    );
    // Same-loop durable history already reconciled (indices 0..count-1).
    if let Some(view) = app.sessions.known.get_mut("ses_1") {
        for index in 0..count {
            view.transcript.push_block(TranscriptBlock::User(UserBlock {
                index: Some(index as usize),
                loop_id: Some("loop_live".to_owned()),
                kind: crate::protocol::UserMessageKindWire::Prompt,
                text: format!("prompt {index}"),
                pending: false,
            }));
        }
    }
    let commands = testapp::take_requests(app.update(AppEvent::SteerTurn {
        session_id: "ses_1".to_owned(),
        text: "steer body".to_owned(),
    }));
    let steer = commands
        .iter()
        .find(|request| request.method == "turn.steer")
        .expect("turn.steer issued")
        .clone();
    testapp::respond(
        &mut app,
        &steer,
        serde_json::json!({"ok": true, "accepted_at": "2026-01-02T03:04:05.000Z", "steer_index": 1}),
    );
    if let Some(view) = app.sessions.known.get_mut("ses_1") {
        if let Some(live) = view.live.as_mut() {
            for pending in &mut live.pending_steers {
                pending.local_id = u64::from(steer_id);
            }
        }
    }
    // Applied by receipt so the steer renders as the provisional user card
    // whose ordinal equals `steer_id` (the P2 collision scaffold).
    apply_steer_receipt(&mut app, 1);
    app
}

/// P2 proof: a selection anchored on the provisional accepted-steer card with
/// a global id numerically equal to another durable user's history index must
/// be cleared at live->history reconciliation, never rebased onto any wrong
/// durable User block (durable User ordinal is always 0 while steer ordinal
/// is the local id, so section identity can never conflate them).
#[test]
fn provisional_steer_selection_never_rebases_onto_a_wrong_durable_user() {
    // 8 durable prompt cards in the SAME loop, steer global id 7.
    let mut app = steer_identity_app(8, 7);

    // Anchor a selection on the live steer card (kind User, ordinal 7).
    let prepared = transcript::prepare_conversation(&app, 77);
    let steer_section = prepared
        .sections
        .iter()
        .find(|range| {
            range.id.kind == SectionKind::User
                && range.id.ordinal == 7
                && range.id.history_index.is_none()
        })
        .expect("live steer card section");
    let steer_id: SectionId = steer_section.id.clone();
    assert_eq!(
        prepared
            .sections
            .iter()
            .filter(|range| range.id.kind == SectionKind::User && range.id.history_index.is_some())
            .count(),
        8,
        "durable prompt cards present"
    );
    app.selection = Some(ConversationSelection {
        session_id: "ses_1".to_owned(),
        anchor: SelectionPoint {
            row: steer_section.rows.start,
            column: 2,
            section_id: Some(steer_id.clone()),
            section_row: 0,
        },
        focus: SelectionPoint {
            row: steer_section.rows.start + 1,
            column: 20,
            section_id: Some(steer_id.clone()),
            section_row: 1,
        },
        granularity: SelectionGranularity::Paragraph,
        dragged: false,
    });
    let prepared = transcript::prepare_conversation(&app, 77);
    app.install_conversation(prepared);
    assert!(
        app.selection.is_some(),
        "while the steer is still pending the live selection survives"
    );

    // Reconciliation: the durable steering card appears at history index 8
    // (same loop) exactly once, exactly as in the live->history reconcile: the
    // matching applied (receipt-proven) provisional card is removed.
    if let Some(view) = app.sessions.known.get_mut("ses_1") {
        view.transcript.push_block(TranscriptBlock::User(UserBlock {
            index: Some(8),
            loop_id: Some("loop_live".to_owned()),
            kind: crate::protocol::UserMessageKindWire::Steering,
            text: "steer body".to_owned(),
            pending: false,
        }));
        view.applied_steers
            .retain(|applied| applied.text != "steer body");
        view.transcript.invalidate();
    }
    let prepared = transcript::prepare_conversation(&app, 77);
    assert_eq!(
        prepared
            .sections
            .iter()
            .filter(|range| range.id.kind == SectionKind::User && range.id.history_index.is_some())
            .count(),
        9,
        "8 prompts + 1 steering card"
    );
    app.install_conversation(prepared);

    match app.selection.take() {
        None => {
            // Identity cannot be proven safely: the selection is cleared
            // rather than guessed (documented limitation).
        }
        Some(selection) => {
            // If it survived, it must point at the steering card itself, not
            // at any other durable user (never a wrong rebase target).
            for point in [&selection.anchor, &selection.focus] {
                let id = point
                    .section_id
                    .clone()
                    .expect("rebased point has identity");
                assert_eq!(
                    id.history_index,
                    Some(8),
                    "the surviving selection must resolve to the steering card: {id:?}"
                );
                assert_eq!(
                    id.ordinal, 0,
                    "durable steering card ordinal is 0, not the steer local id: {id:?}"
                );
            }
        }
    }
}

// ============================================================================
// 0.2.4 desired queue UI (RED until implemented): a pending accepted steer
// is shown as a gray `Steering:` queue row near the Working status, NOT as a
// user-card surface. Durable history Steering User items keep User style.
// ============================================================================

/// RED contract: while a steer is accepted and pending history, the area near
/// the Working status row must carry a gray `Steering:` queue indicator and
/// must not render the steer as a user card.
#[test]
fn pending_steer_uses_gray_queue_not_a_user_card() {
    let mut app = testapp::live_turn(ThemeKind::Dark);
    let commands = testapp::take_requests(app.update(AppEvent::SteerTurn {
        session_id: "ses_1".to_owned(),
        text: "change direction now".to_owned(),
    }));
    let steer = commands
        .iter()
        .find(|request| request.method == "turn.steer")
        .expect("turn.steer issued")
        .clone();
    testapp::respond(
        &mut app,
        &steer,
        serde_json::json!({"ok": true, "accepted_at": "2026-01-02T03:04:05.000Z", "steer_index": 1}),
    );

    let rows = crate::ui::component_tests::buffer_lines(&draw(&app, 80, 24));
    let working = rows
        .iter()
        .rposition(|row| row.contains("Working") || row.contains("Running"))
        .expect("busy status row");
    // The gray queue sits ABOVE the Working status row in the dock, separated
    // by exactly one blank gap (queue.bottom < status.y).
    let near = &rows[working.saturating_sub(5)..working];
    assert!(
        near.iter()
            .any(|row| row.contains("Steering (accepted): change direction now")),
        "pending steer must be surfaced as a gray Steering queue row ABOVE Working: {near:?}"
    );
    assert!(
        near.last().is_some_and(|row| row.trim().is_empty()),
        "exactly one blank gap between the queue and the Working status: {near:?}"
    );
    assert!(
        !rows
            .iter()
            .any(|row| row.contains("↪ change direction now")),
        "pending steer must not render as a user-card surface"
    );
}
