use serde_json::json;

use super::harness::*;
use crate::app::Action;
use crate::transcript::Block;

fn automatic() -> serde_json::Value {
    json!({"session_id": "s1",
           "last_result": {"origin": "automatic", "operation_id": "auto-lup_1",
                           "status": "compacted", "before_tokens": 41000, "after_tokens": 12000},
           "budget": {"estimated_history_tokens": 12000}})
}

#[tokio::test(start_paused = true)]
async fn a_manual_compaction_adds_one_marker_and_keeps_what_is_loaded() {
    let mut h = Harness::opened(page(1, 0, vec![user(0, "l0", "loaded")])).await;
    h.fake.script(
        "session.compact",
        json!({"origin": "manual", "operation_id": "tui-1", "status": "compacted",
               "before_tokens": 32200, "after_tokens": 2700, "covered_loop_count": 3,
               "covered_item_count": 12, "retained_item_count": 2}),
    );
    h.action(Action::Compact);
    assert_eq!(h.app.status_label(), Some("Compacting context"));
    h.settle().await;
    assert_eq!(h.calls("session.compact")[0]["operation_id"], "tui-1");
    assert_eq!(h.texts(), ["loaded"]);
    assert_eq!(h.compactions(), ["tui-1"]);
    assert!(h.screen(80, 12).contains("32.2k → 2.7k"));

    h.fake.script(
        "session.compact",
        json!({"origin": "manual", "operation_id": "tui-2", "status": "noop"}),
    );
    h.action(Action::Compact);
    h.settle().await;
    assert_eq!(h.calls("session.compact")[1]["operation_id"], "tui-2");
    assert_eq!(h.compactions(), ["tui-1"]);
    assert_eq!(h.notices(), ["Nothing to compact yet"]);
}

#[tokio::test(start_paused = true)]
async fn an_automatic_compaction_is_reported_once_after_the_turn() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script("turn.send", json!({"turn": turn("l1")}));
    h.fake.script("turn.wait", waited("l1", completed()));
    h.fake.script("session.context", automatic());
    h.submit("compact me");
    h.settle().await;
    assert_eq!(h.compactions(), ["auto-lup_1"]);

    h.context(automatic());
    h.fake.script(
        "session.compact",
        json!({"origin": "manual", "operation_id": "tui-1", "status": "compacted",
               "before_tokens": 12000, "after_tokens": 4000}),
    );
    h.action(Action::Compact);
    h.settle().await;
    h.context(automatic());
    assert_eq!(h.compactions(), ["auto-lup_1", "tui-1"]);
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
                   "compaction": {"operation_id": "auto-l1", "phase": "merging",
                                  "covered_item_count": 8, "retained_item_count": 2}}));
    assert_eq!(h.app.status_label(), Some("Compacting context"));
    h.state(json!({"session_id": "s1", "status": "idle"}));
    assert_eq!(h.app.status_label(), None);
}
