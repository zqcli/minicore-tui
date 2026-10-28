use std::time::Duration;

use serde_json::json;

use super::harness::*;
use crate::app::Action;
use crate::transcript::Block;

fn compacted(operation_id: &str) -> serde_json::Value {
    json!({"session_id": "s1",
           "last_result": {"operation_id": operation_id, "status": "compacted",
                           "before_tokens": 41000, "after_tokens": 12000},
           "budget": {"estimated_history_tokens": 12000}})
}

#[tokio::test(start_paused = true)]
async fn a_manual_compaction_adds_one_marker_and_keeps_what_is_loaded() {
    let mut h = Harness::opened(page(1, 0, vec![user(0, "l0", "loaded")])).await;
    h.fake.script(
        "session.compact",
        json!({"operation_id": "tui-1", "status": "compacted",
               "before_tokens": 32200, "after_tokens": 2700, "covered_loop_count": 3,
               "covered_item_count": 12, "retained_item_count": 2}),
    );
    h.action(Action::Compact);
    assert_eq!(h.app.status_label(), Some("Compacting context"));
    h.settle().await;
    assert_eq!(h.calls("session.compact")[0]["operation_id"], "tui-1");
    assert_eq!(h.texts(), ["loaded"]);
    assert_eq!(h.compactions(), ["tui-1"]);
    assert!(
        h.screen(80, 12)
            .contains("Compacted from 32.2k to 2.7k tokens")
    );

    h.fake.script(
        "session.compact",
        json!({"operation_id": "tui-2", "status": "noop"}),
    );
    h.action(Action::Compact);
    h.settle().await;
    assert_eq!(h.calls("session.compact")[1]["operation_id"], "tui-2");
    assert_eq!(h.compactions(), ["tui-1"]);
    assert_eq!(h.notices(), ["Nothing to compact yet"]);
}

#[tokio::test(start_paused = true)]
async fn a_manual_compaction_the_context_reports_again_adds_no_second_marker() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script(
        "session.compact",
        json!({"operation_id": "tui-1", "status": "compacted",
               "before_tokens": 41000, "after_tokens": 12000}),
    );
    h.action(Action::Compact);
    h.settle().await;
    assert_eq!(h.compactions(), ["tui-1"]);

    h.context(compacted("tui-1"));
    assert_eq!(h.compactions(), ["tui-1"]);
}

#[tokio::test(start_paused = true)]
async fn a_replayed_summary_gets_the_counts_of_the_context_ledger() {
    let mut h = Harness::opened(page(
        1,
        0,
        vec![chunk(
            0,
            json!({"type": "summary", "data": {"content": "the earlier work, summarised"}}),
        )],
    ))
    .await;
    h.context(json!({"session_id": "s1",
                     "coverage": {"covered_loop_count": 4, "covered_item_count": 332,
                                  "retained_item_count": 2}}));
    let coverage: Vec<_> = h
        .blocks()
        .iter()
        .filter_map(|block| match block {
            Block::Compaction { coverage, .. } => Some(*coverage),
            _ => None,
        })
        .collect();
    assert_eq!(coverage, [Some((Some(332), Some(2)))]);
}

#[tokio::test(start_paused = true)]
async fn an_idle_state_with_a_compaction_still_shows_it() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.state(json!({"session_id": "s1", "status": "idle",
                   "compaction": {"operation_id": "tui-1", "phase": "merging",
                                  "covered_item_count": 8, "retained_item_count": 2}}));
    assert_eq!(h.app.status_label(), Some("Compacting context"));
    h.state(json!({"session_id": "s1", "status": "idle"}));
    assert_eq!(h.app.status_label(), None);
}

fn presentation(percent: f64) -> serde_json::Value {
    json!({"session_id": "s1",
           "context": {"tokens": 1000, "window": 200000, "percent": percent, "kind": "reported"}})
}

fn footer_percent(h: &Harness) -> Option<f64> {
    crate::app::session::context_percent(h.app.session.as_ref().expect("a session"))
}

#[tokio::test(start_paused = true)]
async fn a_request_refreshes_the_footer_context_at_most_once_per_window() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    let usage = || json!({"turn": turn("l1"), "usage": {"input_tokens": 10}});
    let asked = |h: &Harness| h.calls("session.presentation").len();
    h.until(|app| !app.session.as_ref().unwrap().presentation_in_flight)
        .await;
    tokio::time::advance(Duration::from_secs(3)).await;
    let opened = asked(&h);

    h.fake.script("session.presentation", presentation(40.0));
    h.emit("request_usage", usage()).await;
    h.until(|app| {
        app.session
            .as_ref()
            .unwrap()
            .presentation
            .as_ref()
            .is_some_and(|presentation| {
                presentation
                    .context
                    .as_ref()
                    .and_then(|usage| usage.percent)
                    == Some(40.0)
            })
    })
    .await;
    assert_eq!(asked(&h), opened + 1);

    h.fake.script("session.presentation", presentation(50.0));
    h.emit("request_usage", usage()).await;
    h.run_for(Duration::from_millis(500)).await;
    assert_eq!(
        asked(&h),
        opened + 1,
        "a second request inside the window waits"
    );
    assert_eq!(footer_percent(&h), Some(40.0));

    tokio::time::advance(Duration::from_secs(2)).await;
    h.emit("request_usage", usage()).await;
    h.until(|app| {
        app.session
            .as_ref()
            .unwrap()
            .presentation
            .as_ref()
            .is_some_and(|presentation| {
                presentation
                    .context
                    .as_ref()
                    .and_then(|usage| usage.percent)
                    == Some(50.0)
            })
    })
    .await;
    assert_eq!(asked(&h), opened + 2);
}

#[tokio::test(start_paused = true)]
async fn the_end_of_a_compaction_refreshes_the_footer_context_inside_the_window() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.settle().await;
    let state = |compaction: serde_json::Value| json!({"state": {"session_id": "s1", "status": "idle", "compaction": compaction}});
    let working = json!({"operation_id": "tui-1", "phase": "merging",
                         "covered_item_count": 8, "retained_item_count": 2});
    h.emit("session_state", state(working)).await;
    h.run_for(Duration::from_millis(100)).await;
    let before = h.calls("session.presentation").len();

    h.fake.script("session.presentation", presentation(7.0));
    h.emit("session_state", state(serde_json::Value::Null))
        .await;
    h.until(|app| {
        app.session
            .as_ref()
            .unwrap()
            .presentation
            .as_ref()
            .is_some_and(|presentation| {
                presentation
                    .context
                    .as_ref()
                    .and_then(|usage| usage.percent)
                    == Some(7.0)
            })
    })
    .await;
    assert_eq!(h.calls("session.presentation").len(), before + 1);
    assert_eq!(footer_percent(&h), Some(7.0));
}

fn automatic_context(current: serde_json::Value, last: serde_json::Value) -> serde_json::Value {
    json!({"session_id": "s1", "automatic": {"current": current, "last": last}})
}

#[tokio::test(start_paused = true)]
async fn a_compaction_at_turn_start_shows_its_phase_and_leaves_a_marker() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script("turn.send", json!({"turn": turn("l1")}));
    h.fake.script_held("turn.wait", waited("l1", completed()));
    h.submit("go");
    h.until(|app| app.is_busy()).await;
    h.settle().await;

    let running = json!({"operation_id": "auto-boundary-l1-1", "loop_id": "l1",
                         "before_tokens": 32100, "outcome": "summarizing"});
    h.fake
        .script("session.context", automatic_context(running, json!(null)));
    tokio::time::advance(Duration::from_secs(3)).await;
    h.app.tick();
    h.until(|app| app.compacting()).await;
    let screen = h.screen(100, 14);
    assert!(
        screen.contains("Compacting context · summarizing (esc to interrupt)"),
        "{screen}"
    );
    assert!(h.compactions().is_empty());

    let done = json!({"operation_id": "auto-boundary-l1-1", "loop_id": "l1",
                      "before_tokens": 32100, "after_tokens": 4100, "outcome": "compacted"});
    h.fake
        .script("session.context", automatic_context(json!(null), done));
    tokio::time::advance(Duration::from_secs(3)).await;
    h.app.tick();
    h.until(|app| !app.compacting()).await;
    assert_eq!(h.compactions(), ["auto-boundary-l1-1"]);
    assert!(
        h.screen(100, 14)
            .contains("Compacted from 32.1k to 4.1k tokens")
    );

    tokio::time::advance(Duration::from_secs(3)).await;
    h.app.tick();
    h.run_for(Duration::from_secs(1)).await;
    assert_eq!(h.compactions().len(), 1, "one marker per operation");
}

#[tokio::test(start_paused = true)]
async fn an_earlier_automatic_compaction_is_not_added_when_the_session_opens() {
    let done = json!({"operation_id": "auto-boundary-l0-1", "before_tokens": 32100,
                      "after_tokens": 4100, "outcome": "compacted"});
    let mut h = Harness::new();
    h.fake
        .script("session.context", automatic_context(json!(null), done));
    h.open().await;
    h.settle().await;
    assert!(h.compactions().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_compaction_seen_through_session_state_shows_its_phase_and_leaves_a_marker_when_it_ends()
{
    let mut h = Harness::opened(display_read(0, 0)).await;
    let state = |compaction: serde_json::Value| json!({"state": {"session_id": "s1", "status": "idle", "compaction": compaction}});
    let working = json!({"operation_id": "tui-1", "phase": "merging",
                         "covered_item_count": 8, "retained_item_count": 2});
    h.emit("session_state", state(working)).await;
    h.until(|app| app.compacting()).await;
    let screen = h.screen(100, 14);
    assert!(
        screen.contains("Compacting context · merging (esc to cancel)"),
        "{screen}"
    );

    h.fake.script("session.context", compacted("tui-1"));
    h.emit("session_state", state(serde_json::Value::Null))
        .await;
    h.until(|app| !app.compacting()).await;
    h.settle().await;
    assert_eq!(h.compactions(), ["tui-1"]);
}

#[tokio::test(start_paused = true)]
async fn esc_cancels_the_compaction_that_owns_the_session() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake
        .script("session.compact.cancel", json!({"cancelled": true}));
    h.emit(
        "session_state",
        json!({"state": {"session_id": "s1", "status": "idle",
                         "compaction": {"operation_id": "tui-1", "phase": "summarizing",
                                        "covered_item_count": 8, "retained_item_count": 2}}}),
    )
    .await;
    h.until(|app| app.compacting()).await;
    h.app.composer.set_text("a draft");
    h.press(crossterm::event::KeyCode::Esc);
    h.settle().await;
    assert_eq!(
        h.calls("session.compact.cancel"),
        [json!({"session_id": "s1", "operation_id": "tui-1"})]
    );
    assert_eq!(h.app.composer.text(), "a draft", "esc did one thing");
}

#[tokio::test(start_paused = true)]
async fn a_failed_automatic_compaction_is_reported_once_and_a_cancelled_one_not_at_all() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    let failed = automatic_context(
        json!(null),
        json!({"operation_id": "auto-boundary-l1-1", "outcome": "model_failure"}),
    );
    h.context(failed.clone());
    h.context(failed);
    let cancelled = json!({"operation_id": "auto-boundary-l2-1", "outcome": "cancelled"});
    h.context(automatic_context(json!(null), cancelled));
    assert_eq!(h.notices(), ["Automatic compaction failed: model_failure"]);
}

#[tokio::test(start_paused = true)]
async fn a_cancelled_manual_compaction_says_so() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script_held(
        "session.compact",
        json!({"operation_id": "tui-1", "status": "failed",
               "failure_kind": "cancelled"}),
    );
    h.fake
        .script("session.compact.cancel", json!({"cancelled": true}));
    h.action(Action::Compact);
    h.press(crossterm::event::KeyCode::Esc);
    h.run_for(Duration::from_millis(100)).await;
    assert_eq!(
        h.calls("session.compact.cancel")[0]["operation_id"],
        "tui-1"
    );
    h.fake.release("session.compact");
    h.settle().await;
    assert!(h.notices().is_empty());
    assert_eq!(h.app.flash.as_ref().unwrap().0, "Compaction cancelled");
}
