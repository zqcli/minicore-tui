//! Stage-A performance baseline measurement (Spec §25).
//!
//! These tests measure the *current* v0.2.8 structure. They are not pass/fail
//! budgets for the old code; every assertion pins a fact the stage C
//! performance work must preserve or improve. Run the ignored timing tests
//! with:
//!
//! ```text
//! cargo test --locked --test performance -- --ignored --nocapture
//! ```
//!
//! The structural numbers here feed `docs/performance.md`. Counts are
//! measured through public behavior (prepared row counts, clone byte totals),
//! never through a hidden telemetry hook.

use std::path::PathBuf;

use minicore_tui::app::App;
use minicore_tui::state::session::SessionView;
use minicore_tui::state::transcript::{AssistantBlock, AssistantPart, TranscriptBlock};
use minicore_tui::ui::transcript::{all_lines, prepare_conversation, total_lines};
use serde_json::json;

const WIDTH: u16 = 79;

/// Builds an active session whose durable transcript has `messages`
/// assistant blocks, each a Markdown paragraph of roughly `bytes_per_message`.
fn app_with_history(messages: usize, bytes_per_message: usize) -> App {
    let mut app = App::new(PathBuf::from("/project"));
    app.connection = minicore_tui::app::ConnectionState::Ready;
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

/// Baseline: the total prepared row count for a long history grows with the
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

/// Baseline: `all_lines` materializes the full transcript as owned rows.
/// Stage C must keep viewport preparation proportional to the viewport.
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

/// Ignored timing probe: repeated `all_lines` on a large history. Records
/// wall-clock per call; the number itself is machine-dependent and is not a
/// CI assertion. It exists so `docs/performance.md` can be reproduced.
#[test]
#[ignore = "manual performance measurement; run with --ignored --nocapture"]
fn measure_all_lines_rebuild_latency() {
    let app = app_with_history(1000, 240);
    let theme = minicore_tui::theme::Theme::dark();
    // Warm the durable cache once.
    let _ = all_lines(&theme, &app, WIDTH as usize);
    let start = std::time::Instant::now();
    let mut rows = 0;
    for _ in 0..20 {
        rows += all_lines(&theme, &app, WIDTH as usize).len();
    }
    let elapsed = start.elapsed();
    println!(
        "measure_all_lines_rebuild_latency: 20 rebuilds rows_total={rows} per_call_us={}",
        elapsed.as_micros() / 20
    );
}

/// Ignored timing probe: append one live delta to a large history and count
/// how many rows are rebuilt. Stage C should rebuild only the live tail.
#[test]
#[ignore = "manual performance measurement; run with --ignored --nocapture"]
fn measure_live_delta_rebuild_cost() {
    let mut app = app_with_history(1000, 240);
    let theme = minicore_tui::theme::Theme::dark();
    let before = all_lines(&theme, &app, WIDTH as usize).len();
    app.update(minicore_tui::event::AppEvent::SubmitTurn {
        session_id: "ses_perf".into(),
        text: "live".into(),
    });
    let after = all_lines(&theme, &app, WIDTH as usize).len();
    println!(
        "measure_live_delta_rebuild_cost: rows_before={before} rows_after={after} delta={}",
        after as i64 - before as i64
    );
}
