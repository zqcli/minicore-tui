//! Stage-A performance baseline measurement (Spec §25).
//!
//! These tests measure the *current* v0.2.8 structure. They are not pass/fail
//! budgets for the old code; every assertion pins a fact the stage C
//! performance work must preserve or improve. Run the ignored timing tests
//! with:
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
//! * **production frame path** — `main::prepare_frame` calls
//!   `ui::transcript::prepare_conversation` once per changed frame and installs
//!   the result through `AppEvent::ConversationPrepared`. A live delta therefore
//!   rebuilds a full `PreparedConversation` when the durable revision changes.
//! * **diagnostic helper** — `ui::transcript::all_lines` clones the already
//!   prepared rows. It is not the per-frame cost; it is a measurement and test
//!   helper. Do not cite it as the production frame cost.
//!
//! Stage C must make both proportional to the viewport, not the total history.

use std::path::PathBuf;

use minicore_tui::app::{App, ConnectionState};
use minicore_tui::event::{AppEvent, RpcEvent};
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
            .blocks
            .push(TranscriptBlock::Assistant(AssistantBlock {
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

/// Ignored: the production frame path over a 50,000-row history. For each
/// changed frame `prepare_frame` rebuilds a full `PreparedConversation`, so
/// preparation scales with total history. The measured per-call time and row
/// count are recorded in `docs/performance.md`.
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

/// Real live-delta baseline: start a loop, then push 1000 real `output_delta`
/// events and measure how the prepared row count/preparation changes. Stage C
/// must rebuild only the live tail.
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
