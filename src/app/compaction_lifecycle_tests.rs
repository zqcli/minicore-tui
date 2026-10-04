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

fn state_event(app: &mut App, operation: Option<&str>) -> Vec<AppCommand> {
    let state: SessionStateWire = serde_json::from_value(json!({
        "session_id": "ses_1", "status": "idle", "active_loop": null,
        "compaction": operation.map(|id| json!({"operation_id": id, "phase": "summarizing",
            "covered_item_count": 2, "retained_item_count": 0}))
    }))
    .unwrap();
    app.apply_session_state(&state, None, SessionStateSource::Notification)
}

fn observed_operation(app: &App) -> Option<&str> {
    app.active_view()?
        .context
        .as_ref()?
        .current_operation
        .as_ref()
        .map(|operation| operation.operation_id.as_str())
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
                app.queue_context_read(
                    &"ses_1".into(),
                    ContextQueryOwner::Operation("compact-done".into()),
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
fn post_turn_read_does_not_retry_errors_and_operation_survives_panel_close() {
    let mut app = app();
    let time = clock(&mut app);
    let request = take_requests(
        app.queue_context_read(
            &"ses_1".into(),
            ContextQueryOwner::PostTurn("loop_done".into()),
        )
        .into_iter()
        .collect(),
    )
    .remove(0);
    respond_rpc_error(&mut app, &request, -32603, "temporary read error");
    assert!(app.context_reads.is_empty());
    time.store(500, Ordering::Relaxed);
    assert!(take_requests(app.update(AppEvent::Tick)).is_empty());
    let request = take_requests(state_event(&mut app, Some("auto-loop_done"))).remove(0);
    respond(&mut app, &request, context(Some("auto-loop_done"), None));
    assert_eq!(observed_operation(&app), Some("auto-loop_done"));
    let revision = app.active_view().unwrap().transcript.render_revision;
    let scroll = app.active_view().unwrap().scroll.clone();
    app.composer.type_text("draft remains");
    let request = take_requests(app.open_context()).remove(0);
    respond(&mut app, &request, context(Some("auto-loop_done"), None));
    app.close_main_detail();
    assert!(app.context_reads.is_empty());
    assert!(app.active_view().unwrap().is_preparing());
    time.store(1000, Ordering::Relaxed);
    assert!(take_requests(app.update(AppEvent::Tick)).is_empty());
    let request = take_requests(state_event(&mut app, None)).remove(0);
    respond(
        &mut app,
        &request,
        context(None, Some(("auto-loop_done", "failed"))),
    );
    assert!(!app.context_reads.contains_key("ses_1"));
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
        app.queue_context_read(
            &"ses_1".into(),
            ContextQueryOwner::PostTurn("loop_done".into()),
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
    assert!(app.context_reads.is_empty());
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
    assert!(take_requests(app.update(AppEvent::Tick)).is_empty());
    let request = take_requests(state_event(&mut app, Some("auto-previous"))).remove(0);
    respond(&mut app, &request, context(Some("auto-previous"), None));
    let commands = respond_rpc_error(&mut app, send, -32000, "SessionBusy");
    assert!(
        take_requests(commands)
            .iter()
            .all(|request| request.method != "turn.send")
    );
    assert_eq!(app.composer.content(), "prompt");
    assert!(app.active_view().unwrap().live.is_none());
    assert_eq!(observed_operation(&app), Some("auto-previous"));
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
    assert!(take_requests(app.update(AppEvent::Tick)).is_empty());
    let request = take_requests(state_event(&mut app, Some("auto-previous"))).remove(0);
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
    assert_eq!(observed_operation(&app), Some("auto-previous"));
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
        app.queue_context_read(
            &"ses_1".into(),
            ContextQueryOwner::PostTurn("previous".into()),
        )
        .into_iter()
        .collect(),
    )
    .remove(0);
    respond(&mut app, &request, context(Some("auto-previous"), None));
    app.composer.type_text("new draft");
    app.restore_unsent_turn(&"ses_1".into(), local_id);
    assert_eq!(app.composer.content(), "new draft\nunsent");
    assert_eq!(observed_operation(&app), Some("auto-previous"));
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
    assert_eq!(observed_operation(&app), Some("auto-old"));
    assert!(app.active_view().unwrap().live.is_none());
}

#[test]
fn cancel_after_turn_completion_targets_only_the_independent_operation() {
    let mut app = app();
    let request = take_requests(
        app.queue_context_read(&"ses_1".into(), ContextQueryOwner::PostTurn("done".into()))
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
        app.queue_context_read(
            &"ses_1".into(),
            ContextQueryOwner::Operation("auto-old".into()),
        )
        .into_iter()
        .collect(),
    )
    .remove(0);
    app.queue_context_read(
        &"ses_1".into(),
        ContextQueryOwner::Operation("auto-new".into()),
    );
    let request = take_requests(respond(
        &mut app,
        &old,
        context(None, Some(("auto-old", "compacted"))),
    ))
    .remove(0);
    assert!(app.context_reads.is_empty());
    assert!(app.active_view().unwrap().context.is_none());
    assert!(app.active_view().unwrap().compaction_feedback.is_empty());
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
    let old = take_requests(
        app.queue_context_read(&"ses_1".into(), ContextQueryOwner::Turn("done".into()))
            .into_iter()
            .collect(),
    )
    .remove(0);
    app.queue_context_read(&"ses_1".into(), ContextQueryOwner::PostTurn("done".into()));
    // This snapshot was captured before the completion reservation. Accepting
    // it as a discovery result would miss an instant post-turn operation.
    let request = take_requests(respond(&mut app, &old, context(None, None))).remove(0);
    assert!(app.context_reads.is_empty());
    assert!(app.active_view().unwrap().context.is_none());
    assert!(app.active_view().unwrap().compaction_feedback.is_empty());
    respond(
        &mut app,
        &request,
        context(None, Some(("auto-done", "compacted"))),
    );
    assert!(app.context_reads.is_empty());
    let view = app.active_view().unwrap();
    assert!(view.context.as_ref().unwrap().last_result.is_some());
}

#[test]
fn operation_read_send_failure_is_silent_and_queue_full_retains_one_intent() {
    for queue_full in [false, true] {
        let mut app = app();
        let time = clock(&mut app);
        let request = take_requests(state_event(&mut app, Some("auto-done"))).remove(0);
        let commands = if queue_full {
            app.update(AppEvent::RpcQueueFull {
                request,
                class: SendClass::Normal,
            })
        } else {
            app.update(AppEvent::RpcSendFailed {
                id: request.id,
                error: RpcError::Closed,
            })
        };
        assert!(take_requests(commands).is_empty());
        assert_eq!(app.context_reads.contains_key("ses_1"), queue_full);
        assert!(!app.active_view().unwrap().event_gap);
        time.store(50_000, Ordering::Relaxed);
        assert!(take_requests(app.update(AppEvent::Tick)).is_empty());
        // A real unrelated response can admit the retained unsent read only.
        let progress = app.request_session_state(&"ses_1".into());
        let progress = take_requests(vec![progress]).remove(0);
        let reads = take_requests(respond(
            &mut app,
            &progress,
            json!({"session_id":"ses_1", "status":"idle", "active_loop":null}),
        ));
        assert_eq!(
            reads
                .iter()
                .filter(|r| r.method == "session.context")
                .count(),
            usize::from(queue_full)
        );
        if queue_full {
            let read = reads
                .iter()
                .find(|r| r.method == "session.context")
                .unwrap();
            respond(&mut app, read, context(None, Some(("auto-done", "noop"))));
        }
        assert!(app.context_reads.is_empty());
    }
}

#[test]
fn compaction_cancel_failure_is_exact_and_queue_full_retains_intent() {
    for failure in ["rpc", "unsupported", "send", "queue"] {
        let mut app = app();
        let request = take_requests(
            app.queue_context_read(&"ses_1".into(), ContextQueryOwner::PostTurn("done".into()))
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
        assert_eq!(observed_operation(&app), Some("auto-done"));
    }

    let mut app = app();
    let request = take_requests(
        app.queue_context_read(&"ses_1".into(), ContextQueryOwner::PostTurn("old".into()))
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
        app.queue_context_read(
            &"ses_1".into(),
            ContextQueryOwner::Operation("auto-new".into()),
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
    assert_eq!(observed_operation(&app), Some("auto-new"));
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
            app.queue_context_read(&"ses_1".into(), ContextQueryOwner::Explicit)
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
            app.queue_context_read(&"ses_1".into(), ContextQueryOwner::Explicit)
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

#[test]
fn every_context_owner_is_one_shot_after_active_success_or_failure() {
    for owner in [
        ContextQueryOwner::Submission(LocalSubmissionId(1)),
        ContextQueryOwner::Turn("loop".into()),
        ContextQueryOwner::PostTurn("loop".into()),
        ContextQueryOwner::Operation("auto".into()),
        ContextQueryOwner::ManualCompact("manual".into()),
        ContextQueryOwner::Explicit,
        ContextQueryOwner::Panel(0),
    ] {
        for failure in ["active", "rpc", "parse", "session"] {
            for foreground in [true, false] {
                let mut app = app();
                let time = clock(&mut app);
                let request = if matches!(owner, ContextQueryOwner::Panel(_)) {
                    take_requests(app.open_context()).remove(0)
                } else {
                    take_requests(
                        app.queue_context_read(&"ses_1".into(), owner.clone())
                            .into_iter()
                            .collect(),
                    )
                    .remove(0)
                };
                let commands = match failure {
                    "rpc" => respond_rpc_error(&mut app, &request, -32603, "read failed"),
                    "parse" => respond(&mut app, &request, json!({})),
                    "session" => {
                        let mut snapshot = context(None, None);
                        snapshot["session_id"] = json!("wrong-session");
                        respond(&mut app, &request, snapshot)
                    }
                    _ => respond(&mut app, &request, context(Some("active-operation"), None)),
                };
                assert!(take_requests(commands).is_empty(), "{owner:?} {failure}");
                assert!(app.context_reads.is_empty());
                if !foreground {
                    app.set_active_session(None);
                }
                app.close_main_detail();
                // No context deadline is added even with a cached active operation.
                app.notices.clear();
                assert_eq!(
                    app.next_tick(),
                    (failure == "active").then_some(SPINNER_INTERVAL),
                    "only the existing visual spinner can request a wake"
                );
                for elapsed in [500, 2_000, 50_000] {
                    time.store(elapsed, Ordering::Relaxed);
                    assert!(
                        take_requests(app.update(AppEvent::Tick)).is_empty(),
                        "{owner:?} {failure} foreground={foreground}"
                    );
                }
            }
        }
    }
}

#[test]
fn terminal_session_events_discover_all_results_without_start_or_live_turn() {
    for status in ["compacted", "noop", "failed", "unknown_write"] {
        for retired in [false, true] {
            let mut app = app();
            if retired {
                app.sessions.known.get_mut("ses_1").unwrap().retired_loop = Some(TurnRef {
                    session_id: "ses_1".into(),
                    loop_id: "old".into(),
                });
            }
            let state: SessionStateWire = serde_json::from_value(json!({
                "session_id":"ses_1", "status":"idle", "active_loop":null,
                "compaction":null,
            }))
            .unwrap();
            let old_loop = "old".to_owned();
            let request = take_requests(app.apply_session_state(
                &state,
                retired.then_some(&old_loop),
                SessionStateSource::Notification,
            ))
            .remove(0);
            assert_eq!(request.method, "session.context");
            respond(
                &mut app,
                &request,
                context(None, Some(("auto-finished", status))),
            );
            assert!(app.active_view().unwrap().live.is_none());
            assert_eq!(
                app.active_view()
                    .unwrap()
                    .context
                    .as_ref()
                    .unwrap()
                    .last_result
                    .as_ref()
                    .unwrap()
                    .operation_id,
                "auto-finished"
            );
        }
    }
}

#[test]
fn terminal_event_behind_old_read_is_coalesced_and_cannot_accept_old_snapshot() {
    let mut app = app();
    let old = take_requests(state_event(&mut app, Some("auto"))).remove(0);
    assert!(state_event(&mut app, None).is_empty());
    assert!(state_event(&mut app, None).is_empty());
    assert_eq!(app.context_reads.len(), 1);
    let reads = take_requests(respond(&mut app, &old, context(Some("auto"), None)));
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0].method, "session.context");
    assert!(app.active_view().unwrap().context.is_none());
    assert!(app.context_reads.is_empty());
    assert!(
        take_requests(respond(
            &mut app,
            &reads[0],
            context(None, Some(("auto", "noop")))
        ))
        .is_empty()
    );
}

#[test]
fn context_intent_survives_query_slots_waiting_queue_and_deferred_budget_saturation() {
    use crate::app::queries::{QueryAdmission, QueryKey, QuerySlots};
    for saturation in ["slots", "waiting", "deferred"] {
        let mut app = app();
        let time = clock(&mut app);
        let mut blockers = Vec::new();
        let count = if saturation == "deferred" {
            MAX_DEFERRED_REQUESTS
        } else {
            2
        };
        for n in 0..count {
            let kind = if saturation == "deferred" {
                RequestKind::WaitTurn(TurnRef {
                    session_id: format!("other-{n}"),
                    loop_id: "loop".into(),
                })
            } else {
                RequestKind::StaleRead
            };
            let request = take_requests(vec![
                app.request(kind, |id| OutgoingRequest::session_state(id, "other")),
            ])
            .remove(0);
            if saturation != "deferred" {
                assert_eq!(
                    app.queries.request_query(
                        QueryKey::History {
                            session_id: format!("other-{n}"),
                            generation: 0,
                        },
                        request.id
                    ),
                    QueryAdmission::Admitted
                );
            }
            blockers.push(request);
        }
        if saturation == "waiting" {
            for n in 0..QuerySlots::MAX_WAITING {
                assert_eq!(
                    app.queries.request_query(
                        QueryKey::History {
                            session_id: format!("queued-{n}"),
                            generation: 0,
                        },
                        RequestId(1000 + n as u64)
                    ),
                    QueryAdmission::Busy
                );
            }
        }
        assert!(state_event(&mut app, None).is_empty());
        assert!(state_event(&mut app, None).is_empty());
        assert_eq!(app.context_reads.len(), 1);
        assert!(
            app.next_tick().is_none(),
            "a parked context read has no wake deadline"
        );
        time.store(60_000, Ordering::Relaxed);
        assert!(take_requests(app.update(AppEvent::Tick)).is_empty());
        if saturation == "waiting" {
            // Other views close and invalidate their queued reads. The context
            // intent did not fit QuerySlots, but must survive that refusal.
            for n in 0..QuerySlots::MAX_WAITING {
                app.invalidate_query_scope(&queries::QueryScope::Session(format!("queued-{n}")));
            }
        }
        let requests = take_requests(respond_rpc_error(
            &mut app,
            &blockers[0],
            -32603,
            "finished",
        ));
        let reads: Vec<_> = requests
            .iter()
            .filter(|r| r.method == "session.context")
            .collect();
        assert_eq!(reads.len(), 1, "{saturation}");
        assert!(app.context_reads.is_empty());
        respond(&mut app, reads[0], context(None, Some(("auto", "noop"))));
        time.store(120_000, Ordering::Relaxed);
        assert!(take_requests(app.update(AppEvent::Tick)).is_empty());
    }
}

#[test]
fn lost_terminal_event_stays_stale_until_context_refresh_or_reported_gap() {
    for recovery in ["refresh", "gap"] {
        let mut app = app();
        let time = clock(&mut app);
        let request = take_requests(state_event(&mut app, Some("auto"))).remove(0);
        respond(&mut app, &request, context(Some("auto"), None));
        time.store(600_000, Ordering::Relaxed);
        assert!(take_requests(app.update(AppEvent::Tick)).is_empty());
        assert_eq!(observed_operation(&app), Some("auto"));
        let requests = if recovery == "refresh" {
            let initial = take_requests(app.open_context()).remove(0);
            respond(&mut app, &initial, context(Some("auto"), None));
            take_requests(app.refresh_context_panel())
        } else {
            let event: AgentEventWire = serde_json::from_value(json!({
                "type": "session_state",
                "data": {"meta": {"session_id":"ses_1", "dropped_before":1},
                    "state":{"session_id":"ses_1", "status":"idle", "active_loop":null, "compaction":null}}
            })).unwrap();
            take_requests(app.on_agent_event(event))
        };
        let request = requests
            .iter()
            .find(|r| r.method == "session.context")
            .unwrap();
        let mut next = take_requests(respond(
            &mut app,
            request,
            context(None, Some(("auto", "noop"))),
        ));
        // A simultaneous terminal event plus gap coalesces a fresh follow-up.
        if let Some(read) = next.iter_mut().find(|r| r.method == "session.context") {
            respond(&mut app, read, context(None, Some(("auto", "noop"))));
        }
        assert!(!app.active_view().unwrap().is_preparing());
    }
}

#[test]
fn newer_terminal_intent_survives_old_send_failure_without_retrying_failed_reads() {
    let mut app = app();
    let time = clock(&mut app);
    let old = take_requests(state_event(&mut app, Some("auto"))).remove(0);
    assert!(state_event(&mut app, None).is_empty());
    // An unknown failure and a timer do not establish queue progress.
    assert!(
        take_requests(app.update(AppEvent::RpcSendFailed {
            id: RequestId(u64::MAX),
            error: RpcError::Closed,
        }))
        .is_empty()
    );
    assert!(take_requests(app.update(AppEvent::Tick)).is_empty());
    let fresh = take_requests(app.update(AppEvent::RpcSendFailed {
        id: old.id,
        error: RpcError::Closed,
    }));
    assert_eq!(fresh.len(), 1);
    assert_eq!(fresh[0].method, "session.context");
    assert!(app.active_view().unwrap().context.is_none());
    assert!(app.context_reads.is_empty());
    // Failure consumes this newer read; it must not generate another demand.
    assert!(
        take_requests(app.update(AppEvent::RpcSendFailed {
            id: fresh[0].id,
            error: RpcError::Closed,
        }))
        .is_empty()
    );
    time.store(50_000, Ordering::Relaxed);
    assert!(take_requests(app.update(AppEvent::Tick)).is_empty());
}

#[test]
fn retired_manual_read_cannot_keep_a_later_closed_panel_intent_alive() {
    let mut app = app();
    let requests = take_requests(app.start_manual_compact());
    let compact = requests
        .iter()
        .find(|r| r.method == "session.compact")
        .unwrap();
    let old = requests
        .iter()
        .find(|r| r.method == "session.context")
        .unwrap();
    assert!(respond_rpc_error(&mut app, compact, -32603, "failed before start").is_empty());
    assert!(app.active_view().unwrap().manual_compact.is_none());
    assert!(app.open_context().is_empty());
    assert!(matches!(
        app.context_reads.get("ses_1"),
        Some(ContextQueryOwner::Panel(_))
    ));
    app.close_main_detail();
    assert!(app.context_reads.is_empty());
    assert!(take_requests(respond(&mut app, old, context(Some("retired"), None))).is_empty());
    assert!(app.active_view().unwrap().context.is_none());
}

#[test]
fn cancelled_operation_terminal_event_clears_exact_intent_from_fresh_result() {
    let mut app = app();
    let read = take_requests(state_event(&mut app, Some("auto-cancelled"))).remove(0);
    respond(&mut app, &read, context(Some("auto-cancelled"), None));
    let cancel = take_requests(app.update(AppEvent::CancelTurn {
        session_id: "ses_1".into(),
    }))
    .remove(0);
    let read = take_requests(respond(&mut app, &cancel, json!({"cancelled":true}))).remove(0);
    respond(&mut app, &read, context(Some("auto-cancelled"), None));
    assert!(app.compaction_cancelling("ses_1", "auto-cancelled"));
    let terminal = take_requests(state_event(&mut app, None)).remove(0);
    let mut snapshot = context(None, Some(("auto-cancelled", "failed")));
    snapshot["last_result"]["failure_kind"] = json!("cancelled");
    assert!(take_requests(respond(&mut app, &terminal, snapshot)).is_empty());
    assert!(!app.compaction_cancelling("ses_1", "auto-cancelled"));
    assert!(!app.active_view().unwrap().is_preparing());
    assert_eq!(
        app.active_view()
            .unwrap()
            .context
            .as_ref()
            .unwrap()
            .last_result
            .as_ref()
            .unwrap()
            .failure_kind
            .as_deref(),
        Some("cancelled")
    );
}
