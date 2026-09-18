//! C2b structural performance probes (Spec §11 and §25).
//!
//! The baseline helpers intentionally use the synchronous test harness. The
//! production path is exercised separately through `LocalJobs`' single owned
//! layout worker. Run the ignored acceptance probes with:
//!
//! ```text
//! cargo test --release --locked --test performance -- --ignored --nocapture
//! ```
//!
//! The structural numbers here feed `docs/performance.md`. Counts are measured
//! through public behavior (prepared row counts, live-delta preparation row
//! counts), never through a hidden telemetry hook.
//!
//! Two paths are distinguished deliberately:
//!
//! * **production frame path** — `main::prepare_frame_with_jobs` requests a
//!   bounded worker result and installs it through `AppEvent::DurableLayoutPrepared`;
//!   a live-only delta reuses the immutable durable cache and composes only the
//!   live tail.
//! * **diagnostic helper** — `ui::transcript::all_lines` clones the already
//!   prepared rows. It is not the per-frame cost; it is a measurement and test
//!   helper. Do not cite it as the production frame cost.

use std::path::PathBuf;

use minicore_tui::app::{App, ConnectionState};
use minicore_tui::event::{AppEvent, RpcEvent};
use minicore_tui::jobs::LocalJobs;
use minicore_tui::protocol::{IncomingFrame, RpcNotification, TurnRef};
use minicore_tui::state::session::SessionView;
use minicore_tui::state::transcript::{AssistantBlock, AssistantPart, TranscriptBlock};
use minicore_tui::ui::transcript::{all_lines, prepare_conversation, total_lines};
use serde_json::json;

const WIDTH: u16 = 79;

/// Builds an active, loaded session whose durable transcript has `messages`
/// assistant blocks, each a Markdown paragraph of roughly `bytes_per_message`.
fn app_with_history(messages: usize, bytes_per_message: usize) -> App {
    let mut app = App::new(PathBuf::from("/project"));
    app.connection = ConnectionState::Ready;
    let info = serde_json::from_value(json!({
        "session_id": "ses_perf",
        "title": "Performance",
        "profile": "coding",
        "workspace": "/project",
        "model": "deep",
        "reasoning": "high",
        "loaded": true,
        "created_at": "2026-01-01T00:00:00Z",
        "updated_at": "2026-01-01T00:00:00Z"
    }))
    .unwrap();
    let mut view = SessionView::new(info);
    view.state = Some(
        serde_json::from_value(json!({
            "session_id": "ses_perf",
            "status": "idle",
            "active_loop": null,
            "block_reason": null
        }))
        .unwrap(),
    );
    let body = "lorem ipsum dolor sit amet ".repeat(bytes_per_message / 27 + 1);
    for index in 0..messages {
        view.transcript
            .push_block(TranscriptBlock::Assistant(AssistantBlock {
                index,
                loop_id: format!("lup_perf_{index}"),
                request_index: 0,
                model: "deep".into(),
                reasoning_level: minicore_tui::protocol::Reasoning::High,
                parts: vec![AssistantPart::Text(format!("## msg {index}\n\n{body}"))],
                tool_calls: vec![],
                usage: Default::default(),
                finish_reason: "stop".into(),
                terminal_error: None,
            }));
    }
    view.transcript.complete = true;
    view.transcript.invalidate();
    app.sessions.known.insert("ses_perf".into(), view);
    app.sessions.active = Some("ses_perf".into());
    app
}

/// Appends `count` real `output_delta` events to the active live loop.
fn push_live_deltas(app: &mut App, turn: &TurnRef, count: usize) {
    for index in 0..count {
        let part = format!("delta-{index} ");
        let event = json!({
            "type": "output_delta",
            "data": {
                "turn": {"session_id": turn.session_id, "loop_id": turn.loop_id},
                "request_index": 0,
                "channel": "text",
                "delta": part,
                "meta": {"session_id": turn.session_id, "dropped_before": 0}
            }
        });
        app.update(AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Notification(
            RpcNotification::AgentEvent(serde_json::from_value(event).unwrap()),
        ))));
    }
}

async fn install_worker_layout(app: &mut App, jobs: &mut LocalJobs, width: u16) {
    let request = app
        .layout_request(width)
        .expect("the durable cache must miss before the worker request");
    let identity = request.identity.clone();
    assert!(jobs.try_schedule_layout(request));
    app.mark_layout_pending(identity);
    loop {
        let event = jobs.events().recv().await.expect("layout worker event");
        let complete = matches!(
            &event,
            AppEvent::DurableLayoutPrepared(result) if result.complete
        );
        app.update(event);
        if complete {
            break;
        }
    }
    assert!(app.prepared_conversation(width).is_some());
}

/// Production-path smoke test: the first durable layout is prepared by the
/// owned worker, while the App only installs the result and composes the
/// small live tail. No synchronous durable fallback is permitted in this
/// mode.
#[tokio::test]
async fn production_layout_worker_installs_current_width_only() {
    let mut app = app_with_history(64, 240);
    app.enable_async_layout();
    assert!(app.prepared_conversation(WIDTH).is_none());
    assert_eq!(total_lines(&app, WIDTH), 0);

    let mut jobs = LocalJobs::new();
    install_worker_layout(&mut app, &mut jobs, WIDTH).await;
    assert_eq!(app.prepared_conversation(WIDTH).unwrap().width, WIDTH);
    assert!(app.prepared_conversation(WIDTH + 1).is_none());
    assert!(app.layout_request(WIDTH + 1).is_some());

    app.update(AppEvent::Terminal(crossterm::event::Event::Key(
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('x'),
            crossterm::event::KeyModifiers::empty(),
        ),
    )));
    assert_eq!(app.composer.content(), "x");
    assert!(app.prepared_conversation(WIDTH).is_some());

    app.update(AppEvent::TerminalSize {
        width: 100,
        height: 24,
    });
    assert!(app.prepared_conversation(WIDTH).is_none());
    let resized = WIDTH - 1;
    install_worker_layout(&mut app, &mut jobs, resized).await;
    assert_eq!(app.prepared_conversation(resized).unwrap().width, resized);
    assert!(app.prepared_conversation(WIDTH).is_none());
    jobs.shutdown().await;
}

#[test]
fn durable_layout_snapshot_shares_tool_presentation_storage() {
    let app = app_with_history(1, 240);
    let view = app.active_view().expect("active performance view");
    let snapshot = minicore_tui::ui::transcript::DurableLayoutSnapshot::from_view(view);
    assert!(std::sync::Arc::ptr_eq(
        &snapshot.tool_presentations,
        &view.tool_presentations
    ));
}

#[tokio::test]
async fn production_layout_worker_fences_stale_theme_result() {
    let mut app = app_with_history(64, 240);
    app.enable_async_layout();
    let mut jobs = LocalJobs::new();

    let first = app.layout_request(WIDTH).expect("initial layout request");
    let first_identity = first.identity.clone();
    let first_cancel = std::sync::Arc::clone(&first.cancel);
    assert!(jobs.try_schedule_layout(first));
    app.mark_layout_pending(first_identity);
    let stale = jobs.events().recv().await.expect("first layout result");

    app.update(AppEvent::SetTheme(minicore_tui::theme::ThemeKind::Light));
    let second = app
        .layout_request(WIDTH)
        .expect("theme change must request a new generation");
    let second_identity = second.identity.clone();
    assert!(jobs.try_schedule_layout(second));
    app.mark_layout_pending(second_identity);
    assert!(first_cancel.load(std::sync::atomic::Ordering::Relaxed));

    app.update(stale);
    assert!(app.prepared_conversation(WIDTH).is_none());
    let current = jobs.events().recv().await.expect("current layout result");
    assert!(matches!(current, AppEvent::DurableLayoutPrepared(_)));
    app.update(current);
    assert!(app.prepared_conversation(WIDTH).is_some());
    assert_eq!(app.theme, minicore_tui::theme::ThemeKind::Light);
    jobs.shutdown().await;
}

/// C2b acceptance probe: a real 50k-row durable history and 1000 real
/// `output_delta` notifications use the installed immutable layout. The
/// ignored marker keeps the normal suite quick; this is the command-line
/// acceptance evidence for the worker path.
#[tokio::test]
#[ignore = "C2b acceptance; run with --ignored --nocapture"]
async fn measure_c2b_worker_over_50k_rows_and_1000_output_deltas() {
    const HEIGHT: usize = 40;
    let mut app = app_with_history(7300, 240);
    app.enable_async_layout();
    let mut jobs = LocalJobs::new();
    install_worker_layout(&mut app, &mut jobs, WIDTH).await;
    let initial = app.prepared_conversation(WIDTH).unwrap();
    let initial_rows = initial.total_rows();
    assert!(initial_rows >= 50_000);

    app.update(AppEvent::SubmitTurn {
        session_id: "ses_perf".into(),
        text: "live turn".into(),
    });
    // The local pending user card is a durable section change. Settle that
    // one worker result before measuring the following live-only deltas.
    let turn = TurnRef {
        session_id: "ses_perf".into(),
        loop_id: "lup_live".into(),
    };
    install_worker_layout(&mut app, &mut jobs, WIDTH).await;
    // The first event binds the pending card to the real Loop and therefore
    // is also a durable identity change. Settle that transition out of band.
    push_live_deltas(&mut app, &turn, 1);
    install_worker_layout(&mut app, &mut jobs, WIDTH).await;
    let base = minicore_tui::perf::snapshot();
    let mut window_rows = 0usize;
    for index in 0..1000 {
        push_live_deltas(&mut app, &turn, 1);
        let prepared = minicore_tui::ui::transcript::prepare_conversation_with_durable(
            &app,
            WIDTH,
            app.cached_durable(WIDTH).expect("durable cache"),
        );
        window_rows += prepared
            .window(prepared.total_rows().saturating_sub(HEIGHT), HEIGHT)
            .len();
        app.update(AppEvent::ConversationPrepared(prepared));
        if index % 250 == 0 {
            assert!(app.cached_durable(WIDTH).is_some());
        }
    }
    let after = minicore_tui::perf::snapshot();
    println!(
        "c2b_worker: durable_rows={initial_rows} deltas=1000 layout_calls={} history_bytes_cloned={} viewport_rows={} viewport_bytes={}",
        after.layout_calls - base.layout_calls,
        after.historical_text_bytes_cloned - base.historical_text_bytes_cloned,
        after.viewport_rows_materialized - base.viewport_rows_materialized,
        after.viewport_text_bytes_cloned - base.viewport_text_bytes_cloned,
    );
    assert_eq!(after.layout_calls - base.layout_calls, 0);
    assert_eq!(
        after.historical_text_bytes_cloned - base.historical_text_bytes_cloned,
        0
    );
    assert_eq!(
        window_rows as u64,
        after.viewport_rows_materialized - base.viewport_rows_materialized
    );
    assert!(after.viewport_rows_materialized <= 1000 * HEIGHT as u64);
    jobs.shutdown().await;
}

/// Defect: the total prepared row count for a long history grows with the
/// number of messages. A viewport-only composition (stage C) must produce a
/// row count proportional to the viewport, not this total.
#[test]
fn baseline_prepared_rows_scale_with_total_history() {
    let small = app_with_history(20, 240);
    let large = app_with_history(200, 240);
    let small_rows = total_lines(&small, WIDTH);
    let large_rows = total_lines(&large, WIDTH);
    assert!(small_rows > 0);
    assert!(
        large_rows > small_rows * 5,
        "BASELINE: prepared rows grow with history ({small_rows} -> {large_rows})"
    );
    println!("baseline rows: 20 msgs={small_rows} 200 msgs={large_rows}");
}

/// Baseline: `all_lines` materializes the full transcript as owned rows. This
/// is the **diagnostic helper**, not the production frame path; it is recorded
/// so stage C removes the full clone from both.
#[test]
fn baseline_all_lines_materializes_full_transcript() {
    let app = app_with_history(200, 240);
    let lines = all_lines(&minicore_tui::theme::Theme::dark(), &app, WIDTH as usize);
    let prepared = prepare_conversation(&app, WIDTH);
    assert_eq!(
        lines.len(),
        prepared.total_rows(),
        "BASELINE: all_lines owns exactly the full prepared row set"
    );
    assert!(
        lines.len() > 500,
        "long history materializes {} rows",
        lines.len()
    );
    println!("baseline all_lines rows: {}", lines.len());
}

/// Ignored diagnostic: synchronous preparation over a 50,000-row history.
/// Production C2b uses the worker acceptance probe below; this helper remains
/// useful for comparing the explicit test-only fallback.
#[test]
#[ignore = "manual performance measurement; run with --ignored --nocapture"]
fn measure_prepare_frame_path_over_50k_rows() {
    // ~7 rows per 240-byte message; 7200 messages reaches ~50k rows.
    let app = app_with_history(7300, 240);
    let first = prepare_conversation(&app, WIDTH);
    assert!(
        first.total_rows() >= 50_000,
        "fixture must reach 50k rows, got {}",
        first.total_rows()
    );
    let start = std::time::Instant::now();
    let mut rows = 0;
    for _ in 0..5 {
        rows += prepare_conversation(&app, WIDTH).total_rows();
    }
    let elapsed = start.elapsed();
    println!(
        "measure_prepare_frame_path_over_50k_rows: total_rows={} per_call_ms={:.2}",
        first.total_rows(),
        elapsed.as_secs_f64() * 1000.0 / 5.0
    );
    let _ = rows;
}

/// Diagnostic live-delta baseline: start a loop, then push 1000 real
/// `output_delta` events and measure the synchronous helper. The production
/// worker/cache contract is asserted by the C2b probe above.
#[test]
#[ignore = "manual performance measurement; run with --ignored --nocapture"]
fn measure_live_delta_rebuild_cost() {
    let mut app = app_with_history(7300, 240);
    let theme = minicore_tui::theme::Theme::dark();
    let before_rows = all_lines(&theme, &app, WIDTH as usize).len();

    app.update(AppEvent::SubmitTurn {
        session_id: "ses_perf".into(),
        text: "live turn".into(),
    });
    let turn = TurnRef {
        session_id: "ses_perf".into(),
        loop_id: "lup_live".into(),
    };
    let start = std::time::Instant::now();
    push_live_deltas(&mut app, &turn, 1000);
    let elapsed = start.elapsed();
    let after_rows = all_lines(&theme, &app, WIDTH as usize).len();

    println!(
        "measure_live_delta_rebuild_cost: deltas=1000 history_rows_before={before_rows} \
         history_rows_after={after_rows} delta_rows={} push_ms={:.2}",
        after_rows as i64 - before_rows as i64,
        elapsed.as_secs_f64() * 1000.0
    );
}

/// C2 structural acceptance (spec §11.2/§11.7/§25.1): over a 50k-row
/// history, live deltas must not rebuild the durable layout or copy history
/// text, and each installed frame must materialize only its viewport window.
/// Counters are read from the real call points; the window byte count proves
/// they are not constants.
#[test]
#[ignore = "manual performance measurement; run with --ignored --nocapture"]
fn measure_c2_stable_layout_and_viewport_ownership() {
    const HEIGHT: usize = 40;
    let mut app = app_with_history(7300, 240);
    app.update(AppEvent::ConversationPrepared(prepare_conversation(
        &app, WIDTH,
    )));
    let first = prepare_conversation(&app, WIDTH);
    assert!(
        first.total_rows() >= 50_000,
        "fixture must reach 50k rows, got {}",
        first.total_rows()
    );
    // The turn-start durable change is settled before the measurement, so
    // the 1000 frames below measure only delta frames.
    app.update(AppEvent::SubmitTurn {
        session_id: "ses_perf".into(),
        text: "live turn".into(),
    });
    let turn = TurnRef {
        session_id: "ses_perf".into(),
        loop_id: "lup_live".into(),
    };
    // The first unattributed delta materializes a durable live block; it and
    // the turn-start change are settled before the measurement.
    push_live_deltas(&mut app, &turn, 1);
    app.update(AppEvent::ConversationPrepared(prepare_conversation(
        &app, WIDTH,
    )));
    let base = minicore_tui::perf::snapshot();
    let start = std::time::Instant::now();
    let mut window_rows = 0usize;
    let mut live_rows = 0usize;
    let mut rebuild_frames: Vec<usize> = Vec::new();
    for index in 0..1000 {
        if index % 4 == 0 {
            push_live_deltas(&mut app, &turn, 1);
        }
        let before_frame = minicore_tui::perf::snapshot().layout_calls;
        let prepared = prepare_conversation(&app, WIDTH);
        if minicore_tui::perf::snapshot().layout_calls != before_frame {
            rebuild_frames.push(index);
        }
        live_rows = prepared.live_rows();
        let offset = prepared.total_rows().saturating_sub(HEIGHT);
        window_rows += prepared.window(offset, HEIGHT).len();
        app.update(AppEvent::ConversationPrepared(prepared));
    }
    let elapsed = start.elapsed();
    let after = minicore_tui::perf::snapshot();
    let layout_delta = after.layout_calls - base.layout_calls;
    let history_bytes = after.historical_text_bytes_cloned - base.historical_text_bytes_cloned;
    let viewport_rows = after.viewport_rows_materialized - base.viewport_rows_materialized;
    let viewport_bytes = after.viewport_text_bytes_cloned - base.viewport_text_bytes_cloned;
    println!(
        "measure_c2: frames=1000 deltas=250 live_rows={live_rows} layout_calls_delta={layout_delta}          history_bytes_cloned={history_bytes} viewport_rows={viewport_rows}          viewport_bytes={viewport_bytes} window_rows={window_rows} rebuild_frames={rebuild_frames:?} elapsed_ms={:.2}",
        elapsed.as_secs_f64() * 1000.0
    );
    assert_eq!(
        layout_delta, 0,
        "live deltas must not rebuild the stable durable layout"
    );
    assert_eq!(
        history_bytes, 0,
        "live deltas must not clone history text into a frame"
    );
    assert_eq!(
        window_rows as u64, viewport_rows,
        "each frame materializes exactly its requested window"
    );
    assert!(
        viewport_rows <= 1000 * HEIGHT as u64,
        "viewport materialization is bounded by height, not history"
    );
    assert!(
        viewport_bytes > 0,
        "the window clone is measured at the real call point, not a constant"
    );
}

/// Ignored timing probe retained for the diagnostic helper, clearly labelled
/// as `all_lines` (not the production frame path).
#[test]
#[ignore = "manual performance measurement; run with --ignored --nocapture"]
fn measure_all_lines_clone_latency() {
    let app = app_with_history(1000, 240);
    let theme = minicore_tui::theme::Theme::dark();
    let _ = all_lines(&theme, &app, WIDTH as usize);
    let start = std::time::Instant::now();
    let mut rows = 0;
    for _ in 0..20 {
        rows += all_lines(&theme, &app, WIDTH as usize).len();
    }
    let elapsed = start.elapsed();
    println!(
        "measure_all_lines_clone_latency: helper 20 clones rows_total={rows} per_call_ms={:.2}",
        elapsed.as_secs_f64() * 1000.0 / 20.0
    );
}
