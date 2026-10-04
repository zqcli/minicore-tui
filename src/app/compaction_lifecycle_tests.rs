use super::*;
use crate::ui::testapp::{self, respond, respond_rpc_error, take_requests};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};

fn app() -> App {
    testapp::open_empty(ThemeKind::Dark, "ses_1", None, "high")
}

fn context(operation: Option<&str>, result: Option<(&str, &str)>) -> Value {
    json!({
        "session_id": "ses_1",
        "current_operation": operation.map(|id| json!({
            "operation_id": id, "phase": "summarizing",
            "covered_item_count": 2, "retained_item_count": 0
        })),
        "last_result": result.map(|(id, status)| json!({"operation_id": id, "status": status})),
        "coverage": {"covered_loop_count": 1, "covered_item_count": 2, "retained_item_count": 0},
        "budget": {}, "automatic": {"current": null, "last": null}
    })
}

fn clock(app: &mut App) -> Arc<AtomicU64> {
    let elapsed = Arc::new(AtomicU64::new(0));
    let copy = elapsed.clone();
    let base = Instant::now();
    app.monotonic_now =
        Arc::new(move || base + Duration::from_millis(copy.load(Ordering::Relaxed)));
    elapsed
}

#[test]
fn only_new_successful_compaction_refreshes_reported_context_presentation() {
    for origin in ["manual", "automatic"] {
        for status in ["compacted", "noop", "failed", "unknown_write"] {
            let mut app = app();
            app.sessions.known.get_mut("ses_1").unwrap().presentation = Some(
                serde_json::from_value(json!({
                    "session_id": "ses_1",
                    "context": {"kind": "reported", "tokens": 5000, "window": 100000, "percent": 5.0}
                }))
                .unwrap(),
            );
            let mut snapshot = context(None, Some(("compact-done", status)));
            snapshot["last_result"]["origin"] = json!(origin);
            let request = take_requests(
                app.arm_context_poll(
                    &"ses_1".into(),
                    ContextQueryOwner::Operation("compact-done".into()),
                    true,
                )
                .into_iter()
                .collect(),
            )
            .remove(0);
            let refresh = take_requests(respond(&mut app, &request, snapshot.clone()));
            if status == "compacted" {
                assert_eq!(refresh.len(), 1, "{origin} {status}");
                assert_eq!(refresh[0].method, "session.presentation");
                assert!(
                    take_requests(respond(
                        &mut app,
                        &refresh[0],
                        json!({
                            "session_id": "ses_1", "context": {"kind": "unknown"}
                        })
                    ))
                    .is_empty()
                );
                let presentation = app.active_view().unwrap().presentation.as_ref().unwrap();
                assert_eq!(
                    presentation.context.kind,
                    crate::protocol::ContextKindWire::Unknown
                );
                assert_eq!(presentation.context.percent, None);
            } else {
                assert!(refresh.is_empty(), "{origin} {status}");
                assert_eq!(
                    app.active_view()
                        .unwrap()
                        .presentation
                        .as_ref()
                        .unwrap()
                        .context
                        .percent,
                    Some(5.0)
                );
            }
            // Re-reading the same terminal result is not another invalidation.
            let request = take_requests(app.open_context()).remove(0);
            assert!(take_requests(respond(&mut app, &request, snapshot)).is_empty());
            assert!(!app.active_view().unwrap().presentation_refresh_pending);
        }
    }
}

#[test]
fn post_turn_discovery_retries_errors_and_operation_survives_panel_close() {
    let mut app = app();
    let time = clock(&mut app);
    let request = take_requests(
        app.arm_context_poll(
            &"ses_1".into(),
            ContextQueryOwner::PostTurn("loop_done".into()),
            true,
        )
        .into_iter()
        .collect(),
    )
    .remove(0);
    respond_rpc_error(&mut app, &request, -32603, "temporary read error");
    assert!(matches!(
        app.context_polls["ses_1"].owner,
        ContextQueryOwner::PostTurn(_)
    ));
    time.store(500, Ordering::Relaxed);
    let request = take_requests(app.update(AppEvent::Tick)).remove(0);
    respond(&mut app, &request, context(Some("auto-loop_done"), None));
    assert_eq!(
        app.context_polls["ses_1"].owner,
        ContextQueryOwner::Operation("auto-loop_done".into())
    );
    let revision = app.active_view().unwrap().transcript.render_revision;
    let scroll = app.active_view().unwrap().scroll.clone();
    app.composer.type_text("draft remains");
    let request = take_requests(app.open_context()).remove(0);
    respond(&mut app, &request, context(Some("auto-loop_done"), None));
    app.close_main_detail();
    assert!(app.context_polls.contains_key("ses_1"));
    time.store(1000, Ordering::Relaxed);
    let request = take_requests(app.update(AppEvent::Tick)).remove(0);
    respond(
        &mut app,
        &request,
        context(None, Some(("auto-loop_done", "failed"))),
    );
    assert!(!app.context_polls.contains_key("ses_1"));
    assert!(!app.active_view().unwrap().is_preparing());
    assert_eq!(app.composer.content(), "draft remains");
    assert_eq!(
        app.active_view().unwrap().transcript.render_revision,
        revision
    );
    let after = &app.active_view().unwrap().scroll;
    assert_eq!(
        (
            after.offset,
            after.follow_tail,
            after.new_content,
            after.fold_pinned
        ),
        (
            scroll.offset,
            scroll.follow_tail,
            scroll.new_content,
            scroll.fold_pinned
        )
    );
    assert_eq!(after.anchor, scroll.anchor);
    assert!(
        app.notices
            .iter()
            .any(|notice| notice.text.contains("auto-loop_done failed"))
    );
}

#[test]
fn instant_post_turn_result_is_discovered_without_a_running_notification() {
    let mut app = app();
    let request = take_requests(
        app.arm_context_poll(
            &"ses_1".into(),
            ContextQueryOwner::PostTurn("loop_done".into()),
            true,
        )
        .into_iter()
        .collect(),
    )
    .remove(0);
    respond(
        &mut app,
        &request,
        context(None, Some(("auto-loop_done", "unknown_write"))),
    );
    assert!(app.context_polls.is_empty());
    assert_eq!(
        app.active_view()
            .unwrap()
            .context
            .as_ref()
            .unwrap()
            .last_result
            .as_ref()
            .unwrap()
            .status,
        crate::protocol::CompactStatusWire::UnknownWrite
    );
    assert!(
        app.notices
            .iter()
            .any(|notice| notice.text.contains("unknown write outcome"))
    );
}

#[test]
fn failed_submit_restores_text_without_clearing_an_independent_operation() {
    let mut app = app();
    let time = clock(&mut app);
    app.composer.type_text("prompt");
    let requests = take_requests(app.update(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "prompt".into(),
    }));
    let send = requests
        .iter()
        .find(|request| request.method == "turn.send")
        .unwrap();
    time.store(500, Ordering::Relaxed);
    let request = take_requests(app.update(AppEvent::Tick)).remove(0);
    respond(&mut app, &request, context(Some("auto-previous"), None));
    let commands = respond_rpc_error(&mut app, send, -32000, "SessionBusy");
    assert!(
        take_requests(commands)
            .iter()
            .all(|request| request.method != "turn.send")
    );
    assert_eq!(app.composer.content(), "prompt");
    assert!(app.active_view().unwrap().live.is_none());
    assert_eq!(
        app.context_polls["ses_1"].owner,
        ContextQueryOwner::Operation("auto-previous".into())
    );
    assert_eq!(
        app.active_view()
            .unwrap()
            .state
            .as_ref()
            .unwrap()
            .compaction
            .as_ref()
            .unwrap()
            .operation_id,
        "auto-previous"
    );
}

#[test]
fn busy_response_preserves_later_draft_and_pending_esc_does_not_cancel_previous_auto() {
    let mut app = app();
    let time = clock(&mut app);
    app.composer.set_text("rejected prompt");
    let requests = take_requests(app.submit_composer());
    let send = requests
        .iter()
        .find(|request| request.method == "turn.send")
        .unwrap();
    assert!(app.composer.is_empty());
    time.store(500, Ordering::Relaxed);
    let request = take_requests(app.update(AppEvent::Tick)).remove(0);
    respond(&mut app, &request, context(Some("auto-previous"), None));
    assert!(
        app.submissions
            .values()
            .all(|submission| submission.preparation.is_none())
    );
    let cancel = take_requests(app.update(AppEvent::CancelTurn {
        session_id: "ses_1".into(),
    }));
    assert!(
        cancel.is_empty(),
        "a pending send does not own the previous automatic operation"
    );
    app.composer.set_text("newer draft");
    let commands = respond_rpc_error(&mut app, send, -32000, "SessionBusy");
    assert_eq!(app.composer.content(), "newer draft\nrejected prompt");
    assert!(
        take_requests(commands)
            .iter()
            .all(|request| request.method != "turn.send")
    );
    assert_eq!(
        app.context_polls["ses_1"].owner,
        ContextQueryOwner::Operation("auto-previous".into())
    );
    assert!(
        app.active_view()
            .unwrap()
            .state
            .as_ref()
            .unwrap()
            .compaction
            .is_some()
    );
}

#[test]
fn unsent_submit_does_not_erase_new_draft_or_operation_owner() {
    let mut app = app();
    let requests = take_requests(app.update(AppEvent::SubmitTurn {
        session_id: "ses_1".into(),
        text: "unsent".into(),
    }));
    let send = requests
        .iter()
        .find(|request| request.method == "turn.send")
        .unwrap();
    let local_id = match &app.pending_requests[&send.id] {
        RequestKind::SendTurn {
            local_submission, ..
        } => *local_submission,
        _ => unreachable!(),
    };
    let request = take_requests(
        app.arm_context_poll(
            &"ses_1".into(),
            ContextQueryOwner::PostTurn("previous".into()),
            true,
        )
        .into_iter()
        .collect(),
    )
    .remove(0);
    respond(&mut app, &request, context(Some("auto-previous"), None));
    app.composer.type_text("new draft");
    app.restore_unsent_turn(&"ses_1".into(), local_id);
    assert_eq!(app.composer.content(), "new draft\nunsent");
    assert!(matches!(
        app.context_polls["ses_1"].owner,
        ContextQueryOwner::Operation(_)
    ));
    assert!(
        app.active_view()
            .unwrap()
            .state
            .as_ref()
            .unwrap()
            .compaction
            .is_some()
    );
}

#[test]
fn retired_turn_notification_discovers_operation_without_reviving_turn_state() {
    let mut app = app();
    app.sessions.known.get_mut("ses_1").unwrap().retired_loop = Some(TurnRef {
        session_id: "ses_1".into(),
        loop_id: "old".into(),
    });
    let previous = app.active_view().unwrap().state.clone();
    let state: SessionStateWire = serde_json::from_value(json!({
        "session_id": "ses_1", "status": "idle", "active_loop": null,
        "compaction": {"operation_id": "auto-old", "phase": "summarizing",
            "covered_item_count": 2, "retained_item_count": 0}
    }))
    .unwrap();
    let request = take_requests(app.apply_session_state(
        &state,
        Some(&"old".into()),
        SessionStateSource::Notification,
    ))
    .remove(0);
    assert_eq!(app.active_view().unwrap().state, previous);
    respond(&mut app, &request, context(Some("auto-old"), None));
    assert_eq!(
        app.context_polls["ses_1"].owner,
        ContextQueryOwner::Operation("auto-old".into())
    );
    assert!(app.active_view().unwrap().live.is_none());
}

#[test]
fn cancel_after_turn_completion_targets_only_the_independent_operation() {
    let mut app = app();
    let request = take_requests(
        app.arm_context_poll(
            &"ses_1".into(),
            ContextQueryOwner::PostTurn("done".into()),
            true,
        )
        .into_iter()
        .collect(),
    )
    .remove(0);
    respond(&mut app, &request, context(Some("auto-done"), None));
    let requests = take_requests(app.update(AppEvent::CancelTurn {
        session_id: "ses_1".into(),
    }));
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "session.compact.cancel");
    assert_eq!(requests[0].params["operation_id"], "auto-done");
    assert!(app.compaction_cancelling("ses_1", "auto-done"));
    assert!(app.active_view().unwrap().live.is_none());
    respond(&mut app, &requests[0], json!({"cancelled": false}));
    assert!(!app.compaction_cancelling("ses_1", "auto-done"));
}

#[test]
fn stale_operation_read_cannot_retire_new_owner_or_cross_session_epoch() {
    let mut app = app();
    let old = take_requests(
        app.arm_context_poll(
            &"ses_1".into(),
            ContextQueryOwner::Operation("auto-old".into()),
            true,
        )
        .into_iter()
        .collect(),
    )
    .remove(0);
    app.arm_context_poll(
        &"ses_1".into(),
        ContextQueryOwner::Operation("auto-new".into()),
        false,
    );
    respond(
        &mut app,
        &old,
        context(None, Some(("auto-old", "compacted"))),
    );
    assert_eq!(
        app.context_polls["ses_1"].owner,
        ContextQueryOwner::Operation("auto-new".into())
    );
    assert!(app.active_view().unwrap().context.is_none());
    assert!(app.active_view().unwrap().compaction_feedback.is_empty());
    let request = take_requests(
        app.arm_context_poll(
            &"ses_1".into(),
            ContextQueryOwner::Operation("auto-new".into()),
            true,
        )
        .into_iter()
        .collect(),
    )
    .remove(0);
    app.sessions
        .known
        .get_mut("ses_1")
        .unwrap()
        .context_query_generation += 1;
    respond(&mut app, &request, context(Some("auto-new"), None));
    assert!(app.active_view().unwrap().context.is_none());
    assert!(app.active_view().unwrap().compaction_feedback.is_empty());
}

#[test]
fn pre_completion_context_read_cannot_discharge_post_turn_discovery() {
    let mut app = app();
    let time = clock(&mut app);
    let old = take_requests(
        app.arm_context_poll(
            &"ses_1".into(),
            ContextQueryOwner::Turn("done".into()),
            true,
        )
        .into_iter()
        .collect(),
    )
    .remove(0);
    app.arm_context_poll(
        &"ses_1".into(),
        ContextQueryOwner::PostTurn("done".into()),
        false,
    );
    // This snapshot was captured before the completion reservation. Accepting
    // it as a discovery result would miss an instant post-turn operation.
    respond(&mut app, &old, context(None, None));
    assert!(app.context_polls.contains_key("ses_1"));
    assert!(app.active_view().unwrap().context.is_none());
    assert!(app.active_view().unwrap().compaction_feedback.is_empty());
    time.store(500, Ordering::Relaxed);
    let request = take_requests(app.update(AppEvent::Tick)).remove(0);
    respond(
        &mut app,
        &request,
        context(None, Some(("auto-done", "compacted"))),
    );
    assert!(app.context_polls.is_empty());
    let view = app.active_view().unwrap();
    assert!(view.context.as_ref().unwrap().last_result.is_some());
}

#[test]
fn operation_read_send_failure_and_queue_full_keep_the_single_owner() {
    for queue_full in [false, true] {
        let mut app = app();
        let time = clock(&mut app);
        let request = take_requests(
            app.arm_context_poll(
                &"ses_1".into(),
                ContextQueryOwner::PostTurn("done".into()),
                true,
            )
            .into_iter()
            .collect(),
        )
        .remove(0);
        app.arm_context_poll(
            &"ses_1".into(),
            ContextQueryOwner::Operation("auto-done".into()),
            false,
        );
        if queue_full {
            app.update(AppEvent::RpcQueueFull {
                request,
                class: SendClass::Normal,
            });
        } else {
            app.update(AppEvent::RpcSendFailed {
                id: request.id,
                error: RpcError::Closed,
            });
        }
        assert_eq!(
            app.context_polls["ses_1"].owner,
            ContextQueryOwner::Operation("auto-done".into())
        );
        assert!(
            !app.active_view().unwrap().event_gap,
            "context backpressure is not a turn-history gap"
        );
        time.store(500, Ordering::Relaxed);
        let request = take_requests(app.update(AppEvent::Tick)).remove(0);
        assert_eq!(request.method, "session.context");
        respond(
            &mut app,
            &request,
            context(None, Some(("auto-done", "compacted"))),
        );
        assert!(app.context_polls.is_empty());
    }
}

#[test]
fn compaction_cancel_failure_is_exact_and_queue_full_retains_intent() {
    for failure in ["rpc", "unsupported", "send", "queue"] {
        let mut app = app();
        let request = take_requests(
            app.arm_context_poll(
                &"ses_1".into(),
                ContextQueryOwner::PostTurn("done".into()),
                true,
            )
            .into_iter()
            .collect(),
        )
        .remove(0);
        respond(&mut app, &request, context(Some("auto-done"), None));
        let cancel = take_requests(app.update(AppEvent::CancelTurn {
            session_id: "ses_1".into(),
        }))
        .remove(0);
        assert!(app.compaction_cancelling("ses_1", "auto-done"));
        match failure {
            "rpc" => {
                respond_rpc_error(&mut app, &cancel, -32603, "not cancelled");
            }
            "unsupported" => {
                respond_rpc_error(&mut app, &cancel, -32601, "not supported");
            }
            "send" => {
                app.update(AppEvent::RpcSendFailed {
                    id: cancel.id,
                    error: RpcError::Closed,
                });
            }
            "queue" => {
                app.update(AppEvent::RpcQueueFull {
                    request: cancel,
                    class: SendClass::Control,
                });
            }
            _ => unreachable!(),
        }
        assert_eq!(
            app.compaction_cancelling("ses_1", "auto-done"),
            failure == "queue"
        );
        assert!(app.context_polls.contains_key("ses_1"));
    }

    let mut app = app();
    let request = take_requests(
        app.arm_context_poll(
            &"ses_1".into(),
            ContextQueryOwner::PostTurn("old".into()),
            true,
        )
        .into_iter()
        .collect(),
    )
    .remove(0);
    respond(&mut app, &request, context(Some("auto-old"), None));
    let old_cancel = take_requests(app.update(AppEvent::CancelTurn {
        session_id: "ses_1".into(),
    }))
    .remove(0);
    let request = take_requests(
        app.arm_context_poll(
            &"ses_1".into(),
            ContextQueryOwner::Operation("auto-new".into()),
            true,
        )
        .into_iter()
        .collect(),
    )
    .remove(0);
    respond(&mut app, &request, context(Some("auto-new"), None));
    app.update(AppEvent::CancelTurn {
        session_id: "ses_1".into(),
    });
    assert!(app.compaction_cancelling("ses_1", "auto-new"));
    app.update(AppEvent::RpcSendFailed {
        id: old_cancel.id,
        error: RpcError::Closed,
    });
    assert!(app.compaction_cancelling("ses_1", "auto-new"));
}

#[test]
fn stale_manual_send_failure_cannot_clear_a_new_automatic_owner() {
    let mut app = app();
    let requests = take_requests(app.start_manual_compact());
    let compact = requests
        .iter()
        .find(|request| request.method == "session.compact")
        .unwrap();
    let initial_read = requests
        .iter()
        .find(|request| request.method == "session.context")
        .unwrap();
    respond(&mut app, initial_read, context(Some("auto-new"), None));
    app.update(AppEvent::RpcSendFailed {
        id: compact.id,
        error: RpcError::Closed,
    });
    assert_eq!(
        app.context_polls["ses_1"].owner,
        ContextQueryOwner::Operation("auto-new".into())
    );
    assert!(
        app.active_view()
            .unwrap()
            .state
            .as_ref()
            .unwrap()
            .compaction
            .is_some()
    );
}

#[test]
fn wide_round_counts_decode_across_result_history_and_recovery_pages() {
    for count in [65535_u64, 65536, 70000] {
        let result: crate::protocol::TurnResultViewWire = serde_json::from_value(json!({
            "turn": {"session_id": "ses_1", "loop_id": "loop_done"},
            "outcome": {"type": "completed"}, "tool_rounds": count, "persistence": "persisted"
        }))
        .unwrap();
        assert_eq!(result.tool_rounds, Some(count));
        let summary: crate::protocol::read::ReadTurnSummary = serde_json::from_value(json!({
            "loop_id": "loop_done", "outcome": {"type": "completed"},
            "requests": 1, "tool_rounds": count, "final_config_revision": 0,
            "completed_at": "2026-10-03T00:00:00Z"
        }))
        .unwrap();
        assert_eq!(summary.tool_rounds, count);
        let page: crate::protocol::read::TurnResultPage = serde_json::from_value(json!({
            "turn": {"session_id": "ses_1", "loop_id": "loop_done"},
            "availability": "stored", "tool_rounds": count,
            "items": [], "cursor": {"item_index": 0, "byte_offset": 0},
            "next_cursor": null, "total": 0, "captured_end": 0,
            "history_revision": "revision"
        }))
        .unwrap();
        assert_eq!(page.tool_rounds, Some(count));
    }
}

#[test]
fn terminal_compaction_feedback_is_bounded_deduplicated_and_survives_reopen() {
    let mut app = app();
    for n in 0..20 {
        let request = take_requests(
            app.arm_context_poll(&"ses_1".into(), ContextQueryOwner::Explicit, true)
                .into_iter()
                .collect(),
        )
        .remove(0);
        respond(
            &mut app,
            &request,
            context(None, Some((&format!("auto-{n}"), "compacted"))),
        );
    }
    for _ in 0..2 {
        let request = take_requests(
            app.arm_context_poll(&"ses_1".into(), ContextQueryOwner::Explicit, true)
                .into_iter()
                .collect(),
        )
        .remove(0);
        respond(&mut app, &request, context(None, Some(("auto-19", "noop"))));
    }
    let view = app.active_view().unwrap();
    assert_eq!(view.compaction_feedback.len(), 16);
    assert_eq!(view.compaction_feedback[0].operation_id, "auto-4");
    assert_eq!(
        view.compaction_feedback[15].status,
        crate::protocol::CompactStatusWire::Noop
    );
    assert!(view.transcript.blocks.is_empty());
    let expected = view.compaction_feedback.clone();
    let mut other = view.info.clone();
    other.session_id = "ses_other".into();
    let mut other = crate::state::session::SessionView::new(other);
    other.record_compaction_result(
        serde_json::from_value(json!({"operation_id":"auto-19", "status":"failed"})).unwrap(),
    );
    app.sessions.known.insert("ses_other".into(), other);
    app.set_active_session(Some("ses_other".into()));
    assert_eq!(app.active_view().unwrap().compaction_feedback.len(), 1);
    app.set_active_session(Some("ses_1".into()));
    let response = RpcResponse {
        id: crate::protocol::RequestId(1),
        error: None,
        result: Some(json!({"session": app.active_view().unwrap().info})),
    };
    app.on_open_response("ses_1".into(), None, &response);
    assert_eq!(app.active_view().unwrap().compaction_feedback, expected);
    assert!(app.active_view().unwrap().transcript.blocks.is_empty());
}
