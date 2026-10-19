use crossterm::event::KeyCode;
use serde_json::{Value, json};

use super::harness::*;
use crate::app::{Action, Scroll};
use crate::transcript::Block;

const STALE: i64 = crate::protocol::INVALID_STATE;

/// The `item` of every paged `session.read` cursor, in call order.
fn cursors(h: &Harness) -> Vec<u64> {
    h.calls("session.read")
        .iter()
        .filter_map(|params| params["cursor"]["item"].as_u64())
        .collect()
}

fn read_views(h: &Harness) -> Vec<String> {
    h.calls("session.read")
        .iter()
        .map(|params| params["view"].as_str().unwrap_or_default().to_owned())
        .collect()
}

#[tokio::test(start_paused = true)]
async fn the_tail_window_and_each_older_page_move_the_marker() {
    let mut h = Harness::opened(display_read(80, 0)).await;
    assert_eq!(cursors(&h), [20], "the tail is the last sixty items");
    assert_eq!(h.marker(), Some(20));

    h.fake
        .script("session.read", page(80, 0, vec![user(0, "l0", "first")]));
    h.action(Action::LoadOlder);
    h.settle().await;
    assert_eq!(cursors(&h), [20, 0]);
    assert_eq!(h.marker(), None, "nothing is older than item 0");
    assert_eq!(h.texts(), ["first"]);
}

#[tokio::test(start_paused = true)]
async fn the_marker_states_the_next_read_position_and_goes_at_the_start() {
    let mut h = Harness::opened(display_read(90, 0)).await;
    assert_eq!(h.marker(), Some(30));

    h.fake.script(
        "session.read",
        page(90, 0, vec![user(10, "l0", "a middle question")]),
    );
    h.action(Action::LoadOlder);
    h.settle().await;
    assert_eq!(h.marker(), Some(10));
    assert!(matches!(h.blocks()[0], Block::EarlierHistory { .. }));

    h.fake.script(
        "session.read",
        page(90, 0, vec![user(0, "l0", "the earliest question")]),
    );
    h.action(Action::LoadOlder);
    h.settle().await;
    assert_eq!(h.marker(), None);
    assert_eq!(h.texts(), ["the earliest question", "a middle question"]);
}

#[tokio::test(start_paused = true)]
async fn ctrl_home_loads_the_page_the_marker_promises() {
    let mut h = Harness::opened(display_read(80, 0)).await;
    let reads = h.calls("session.read").len();
    h.ctrl(KeyCode::Home);
    h.settle().await;
    assert!(h.calls("session.read").len() > reads);
}

#[tokio::test(start_paused = true)]
async fn history_below_the_display_floor_is_read_in_the_canonical_view() {
    let mut h = Harness::opened(display_read(30, 10)).await;
    assert!(
        matches!(h.blocks(), [Block::EarlierHistory { hidden_items: 10 }]),
        "an all-covering summary still points at the history below it: {:?}",
        h.blocks()
    );

    h.fake.script(
        "session.read",
        page(12, 0, vec![user(9, "l0", "collapsed question")]),
    );
    h.action(Action::LoadOlder);
    h.settle().await;
    assert_eq!(read_views(&h)[2..], ["canonical", "canonical"]);
    assert_eq!(h.texts(), ["collapsed question"]);
}

#[tokio::test(start_paused = true)]
async fn a_floor_raised_past_the_loaded_window_still_reads_every_item() {
    let mut h = Harness::opened(display_read(100, 10)).await;
    assert_eq!(h.marker(), Some(40));

    // The page reports a floor above the reader: nothing arrives and the read
    // position stays where the loaded rows end.
    h.fake.script("session.read", page(100, 60, vec![]));
    h.action(Action::LoadOlder);
    h.settle().await;
    assert_eq!(h.marker(), Some(40));

    h.fake.script(
        "session.read",
        page(100, 60, vec![user(20, "l0", "deep item")]),
    );
    h.action(Action::LoadOlder);
    h.settle().await;
    assert_eq!(read_views(&h).last().map(String::as_str), Some("canonical"));
    assert_eq!(cursors(&h).last(), Some(&20));
    assert_eq!(h.texts(), ["deep item"]);
}

#[tokio::test(start_paused = true)]
async fn every_canonical_page_replaces_the_marker_at_the_top() {
    let mut h = Harness::opened(display_read(60, 60)).await;
    assert_eq!(h.marker(), Some(60));
    h.app.scroll = Scroll::Anchor {
        block: 0,
        offset: 0,
    };
    let mut loaded = Vec::new();
    for (from, text) in [(40, "page two"), (20, "page one"), (0, "page zero")] {
        h.fake
            .script("session.read", page(60, 60, vec![user(from, "l0", text)]));
        h.action(Action::LoadOlder);
        h.settle().await;
        loaded.insert(0, text);
        let markers: Vec<_> = h
            .blocks()
            .iter()
            .filter(|block| matches!(block, Block::EarlierHistory { .. }))
            .collect();
        assert_eq!(markers.len(), usize::from(from > 0), "{:?}", h.blocks());
        assert_eq!(h.marker(), (from > 0).then_some(from));
        assert_eq!(h.texts(), loaded);
    }
}

#[tokio::test(start_paused = true)]
async fn a_stale_pin_on_the_tail_restarts_with_a_fresh_probe() {
    let mut h = Harness::new();
    h.fake.script_sequence(
        "session.read",
        vec![
            display_read(6, 0),
            refusal(STALE, "invalid_state"),
            display_read(6, 0),
            page(6, 0, vec![user(0, "l0", "recovered")]),
        ],
    );
    h.open().await;
    assert_eq!(h.texts(), ["recovered"]);
    assert!(h.notices().is_empty(), "a stale pin is never surfaced");
    let reads = h.calls("session.read");
    assert_eq!(reads.len(), 4);
    assert!(reads[2].get("history_revision").is_none(), "{reads:?}");
}

#[tokio::test(start_paused = true)]
async fn a_stale_pin_on_an_older_page_retries_once() {
    for (tail, text) in [
        (display_read(80, 0), "display item"),
        (display_read(30, 30), "canonical item"),
    ] {
        let mut h = Harness::opened(tail).await;
        let page = page(80, 0, vec![user(0, "l0", text)]);
        h.fake.script_sequence(
            "session.read",
            vec![refusal(STALE, "invalid_state"), page.clone(), page],
        );
        h.action(Action::LoadOlder);
        h.settle().await;
        assert_eq!(h.texts(), [text]);
        assert!(h.notices().is_empty(), "{:?}", h.notices());
    }
}

#[tokio::test(start_paused = true)]
async fn an_older_page_that_stays_stale_keeps_the_loaded_window() {
    let mut h = Harness::opened(page(80, 0, vec![user(70, "l7", "on screen")])).await;
    h.app.scroll = Scroll::Anchor {
        block: 1,
        offset: 0,
    };
    let before = format!("{:?}", h.blocks());
    let reads = h.calls("session.read").len();
    h.fake
        .script_error("session.read", STALE, "stale pin", "invalid_state");
    h.action(Action::LoadOlder);
    h.settle().await;
    assert_eq!(format!("{:?}", h.blocks()), before);
    assert_eq!(
        h.app.scroll,
        Scroll::Anchor {
            block: 1,
            offset: 0
        }
    );
    assert_eq!(h.calls("session.read").len() - reads, 2, "one retry");
    assert_eq!(
        h.notices(),
        ["Could not load older history yet; try again in a moment"]
    );
}

/// When a page lands during a turn.
#[derive(Clone, Copy, Debug)]
enum Timing {
    WhileStreaming,
    BeforeTheSendAnswers,
    BeforeTheDurableItems,
}

#[tokio::test(start_paused = true)]
async fn history_paged_in_during_a_turn_survives_its_reconcile() {
    for timing in [
        Timing::WhileStreaming,
        Timing::BeforeTheSendAnswers,
        Timing::BeforeTheDurableItems,
    ] {
        let canonical = matches!(timing, Timing::BeforeTheSendAnswers);
        let tail = if canonical {
            display_read(30, 30)
        } else {
            display_read(100, 0)
        };
        let mut h = Harness::opened(tail).await;
        let older: Vec<Value> = if canonical {
            vec![user(0, "l0", "history one")]
        } else {
            vec![
                user(38, "l0", "history one"),
                assistant(39, "l0", "history two"),
            ]
        };
        let expected: Vec<&str> = if canonical {
            vec!["history one", "streaming question"]
        } else {
            vec!["history one", "history two", "streaming question"]
        };
        let result = turn_result("l1", vec![user(100, "l1", "streaming question")]);
        match timing {
            Timing::BeforeTheSendAnswers => {
                h.fake.script_held("turn.send", json!({"turn": turn("l1")}))
            }
            _ => h.fake.script("turn.send", json!({"turn": turn("l1")})),
        }
        match timing {
            Timing::BeforeTheDurableItems => {
                h.fake.script("turn.wait", waited("l1", cancelled()));
                h.fake.script_held("turn.result", result);
            }
            _ => {
                h.fake.script_held("turn.wait", waited("l1", cancelled()));
                h.fake.script("turn.result", result);
            }
        }
        h.submit("streaming question");
        match timing {
            Timing::WhileStreaming => h.until(|app| app.is_busy()).await,
            Timing::BeforeTheSendAnswers => h.settle().await,
            Timing::BeforeTheDurableItems => {
                h.until(|app| {
                    app.transcript_blocks()
                        .iter()
                        .any(|block| matches!(block, Block::TurnEnd { .. }))
                })
                .await
            }
        }
        h.app.scroll = Scroll::Anchor {
            block: 1,
            offset: 1,
        };
        h.fake.script("session.read", page(100, 0, older));
        h.action(Action::LoadOlder);
        h.settle().await;
        if !canonical {
            assert_eq!(
                h.app.scroll,
                Scroll::Anchor {
                    block: 3,
                    offset: 1
                },
                "{timing:?}: the anchored block keeps its place"
            );
        }
        h.fake.release("turn.send");
        h.fake.release("turn.wait");
        h.fake.release("turn.result");
        h.settle().await;
        assert_eq!(h.texts(), expected, "{timing:?}: {:?}", h.blocks());
    }
}

#[tokio::test(start_paused = true)]
async fn an_empty_canonical_page_removes_the_marker_and_the_turn_still_reconciles() {
    let mut h = Harness::opened(display_read(60, 60)).await;
    h.fake.script("turn.send", json!({"turn": turn("l1")}));
    h.fake.script_held("turn.wait", waited("l1", cancelled()));
    h.fake.script(
        "turn.result",
        turn_result("l1", vec![user(60, "l1", "streaming question")]),
    );
    h.submit("streaming question");
    h.until(|app| app.is_busy()).await;

    h.fake.script("session.read", page(60, 60, vec![]));
    h.action(Action::LoadOlder);
    h.settle().await;
    assert_eq!(h.marker(), None);

    h.fake.release("turn.wait");
    h.settle().await;
    assert_eq!(h.texts(), ["streaming question"]);
    assert!(matches!(h.blocks().last(), Some(Block::TurnEnd { .. })));
}
