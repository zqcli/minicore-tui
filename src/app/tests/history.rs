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

/// A tail whose last item is an answer, so opening it reads no further.
fn answered(total: usize, floor: usize) -> Value {
    page(total, floor, vec![assistant(total - 1, "l9", "latest")])
}

/// The kind of every block, to compare order.
fn shape(h: &Harness) -> Vec<&'static str> {
    h.blocks()
        .iter()
        .map(|block| match block {
            Block::EarlierHistory { .. } => "earlier",
            Block::User { .. } => "user",
            Block::Assistant { .. } => "assistant",
            Block::Tool(_) => "tool",
            Block::Compaction { .. } => "compaction",
            _ => "other",
        })
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
    let mut h = Harness::opened(answered(80, 0)).await;
    assert_eq!(cursors(&h), [20], "the tail is the last sixty items");
    assert_eq!(h.marker(), Some(20));

    h.fake
        .script("session.read", page(80, 0, vec![user(0, "l0", "first")]));
    h.action(Action::LoadOlder);
    h.settle().await;
    assert_eq!(cursors(&h), [20, 0]);
    assert_eq!(h.marker(), None, "nothing is older than item 0");
    assert_eq!(h.texts(), ["first", "latest"]);
}

#[tokio::test(start_paused = true)]
async fn the_marker_states_the_next_read_position_and_goes_at_the_start() {
    let mut h = Harness::opened(answered(90, 0)).await;
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
    assert_eq!(
        h.texts(),
        ["the earliest question", "a middle question", "latest"]
    );
}

#[tokio::test(start_paused = true)]
async fn ctrl_home_loads_the_page_the_marker_promises() {
    let mut h = Harness::opened(answered(80, 0)).await;
    let reads = h.calls("session.read").len();
    h.ctrl(KeyCode::Home);
    h.settle().await;
    assert!(h.calls("session.read").len() > reads);
}

#[tokio::test(start_paused = true)]
async fn history_below_the_display_floor_is_read_in_the_canonical_view() {
    let tail = page(30, 10, vec![summary(10), assistant(29, "l9", "latest")]);
    let mut h = Harness::opened(tail).await;
    assert_eq!(
        h.marker(),
        Some(11),
        "the summary stands in for item 10, which is still unread"
    );

    h.fake.script(
        "session.read",
        page(
            30,
            10,
            vec![
                user(9, "l0", "collapsed question"),
                assistant(10, "l0", "covered answer"),
            ],
        ),
    );
    h.action(Action::LoadOlder);
    h.settle().await;
    assert_eq!(read_views(&h)[2..], ["canonical", "canonical"]);
    assert_eq!(cursors(&h).last(), Some(&0));
    assert_eq!(
        shape(&h),
        ["user", "assistant", "compaction", "assistant"],
        "the covered items come before their summary"
    );
    assert_eq!(h.marker(), None);
}

#[tokio::test(start_paused = true)]
async fn a_floor_raised_past_the_loaded_window_still_reads_every_item() {
    let mut h = Harness::opened(answered(100, 10)).await;
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
    assert_eq!(h.texts(), ["deep item", "latest"]);
}

#[tokio::test(start_paused = true)]
async fn every_canonical_page_replaces_the_marker_at_the_top() {
    let mut h = Harness::opened(answered(42, 40)).await;
    assert_eq!(h.marker(), Some(41));
    h.app.scroll = Scroll::Anchor {
        block: 0,
        offset: 0,
    };
    let mut loaded = Vec::new();
    for (from, text) in [(21, "page two"), (1, "page one"), (0, "page zero")] {
        h.fake
            .script("session.read", page(42, 40, vec![user(from, "l0", text)]));
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
        assert_eq!(h.texts()[..loaded.len()], loaded);
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
        (answered(80, 0), "display item"),
        (answered(32, 30), "canonical item"),
    ] {
        let mut h = Harness::opened(tail).await;
        let page = page(80, 0, vec![user(0, "l0", text)]);
        h.fake.script_sequence(
            "session.read",
            vec![refusal(STALE, "invalid_state"), page.clone(), page],
        );
        h.action(Action::LoadOlder);
        h.settle().await;
        assert_eq!(h.texts(), [text, "latest"]);
        assert!(h.notices().is_empty(), "{:?}", h.notices());
    }
}

#[tokio::test(start_paused = true)]
async fn an_older_page_that_stays_stale_keeps_the_loaded_window() {
    let tail = page(
        80,
        0,
        vec![user(70, "l7", "on screen"), assistant(79, "l7", "latest")],
    );
    let mut h = Harness::opened(tail).await;
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
            answered(32, 30)
        } else {
            answered(100, 0)
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
            vec!["history one", "latest", "streaming question"]
        } else {
            vec!["history one", "history two", "latest", "streaming question"]
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
    let mut h = Harness::opened(answered(62, 60)).await;
    h.fake.script("turn.send", json!({"turn": turn("l1")}));
    h.fake.script_held("turn.wait", waited("l1", cancelled()));
    h.fake.script(
        "turn.result",
        turn_result("l1", vec![user(60, "l1", "streaming question")]),
    );
    h.submit("streaming question");
    h.until(|app| app.is_busy()).await;

    h.fake.script("session.read", page(62, 60, vec![]));
    h.action(Action::LoadOlder);
    h.settle().await;
    assert_eq!(h.marker(), None);

    h.fake.release("turn.wait");
    h.settle().await;
    assert_eq!(h.texts(), ["latest", "streaming question"]);
    assert!(matches!(h.blocks().last(), Some(Block::TurnEnd { .. })));
}

/// Every `session.read` the fake answers, in call order: each window is a
/// probe followed by the page.
fn reads(windows: Vec<(usize, usize, Vec<Value>)>) -> Vec<Value> {
    windows
        .into_iter()
        .flat_map(|(total, floor, items)| [display_read(total, floor), page(total, floor, items)])
        .collect()
}

#[tokio::test(start_paused = true)]
async fn a_session_that_opens_on_its_summary_loads_the_latest_answer_behind_it() {
    let mut h = Harness::new();
    h.fake.script_sequence(
        "session.read",
        reads(vec![
            (218, 217, vec![summary(217)]),
            (
                218,
                217,
                vec![
                    user(216, "l9", "the last question"),
                    assistant(217, "l9", "the final answer"),
                ],
            ),
        ]),
    );
    h.open().await;
    assert_eq!(cursors(&h), [217, 198]);
    assert_eq!(read_views(&h)[2..], ["canonical", "canonical"]);
    assert_eq!(shape(&h), ["earlier", "user", "assistant", "compaction"]);
    assert_eq!(h.marker(), Some(198));
    assert_eq!(h.app.scroll, Scroll::Follow);
    let screen = h.screen(100, 30);
    let (answer, marker) = (screen.find("the final answer"), screen.find("[compaction]"));
    assert!(answer.is_some() && answer < marker, "{screen}");
}

#[tokio::test(start_paused = true)]
async fn reading_behind_the_summary_is_bounded_and_scrolling_up_still_loads() {
    let mut h = Harness::new();
    let mut windows = vec![(218, 217, vec![summary(217)])];
    for (from, text) in [
        (210, "p1"),
        (190, "p2"),
        (170, "p3"),
        (150, "p4"),
        (130, "p5"),
    ] {
        windows.push((218, 217, vec![user(from, "l0", text)]));
    }
    h.fake.script_sequence("session.read", reads(windows));
    h.open().await;
    assert_eq!(cursors(&h), [217, 198, 178, 158, 138]);
    assert_eq!(h.texts(), ["p4", "p3", "p2", "p1"], "oldest first");
    assert_eq!(h.marker(), Some(138));

    h.action(Action::LoadOlder);
    h.settle().await;
    assert_eq!(cursors(&h).last(), Some(&118));
    assert_eq!(h.texts(), ["p5", "p4", "p3", "p2", "p1"]);
    assert_eq!(h.marker(), Some(118));
}

#[tokio::test(start_paused = true)]
async fn a_failed_read_behind_the_summary_ends_the_backfill() {
    let mut h = Harness::new();
    h.fake.script_sequence(
        "session.read",
        vec![
            display_read(218, 217),
            page(218, 217, vec![summary(217)]),
            refusal(crate::protocol::INTERNAL_ERROR, "internal"),
        ],
    );
    h.open().await;
    assert_eq!(h.calls("session.read").len(), 3);
    assert_eq!(h.marker(), Some(218));
    assert_eq!(h.notices().len(), 1);
}

fn tool_read(state: &str, exit_code: i64) -> Value {
    json!({
        "display": {"detail": "ls -la", "expanded_input": "ls -la\n", "input_line_count": 1},
        "execution": {
            "tool_ref": {"session_id": "s1", "loop_id": "l1", "request_index": 0, "tool_call_id": "c1"},
            "state": state,
            "started_at": "2026-09-14T00:00:00.000Z",
            "finished_at": "2026-09-14T00:00:01.500Z",
            "result_truncated": false,
            "command": {"status": "exited", "exit_code": exit_code},
        },
    })
}

fn cards(h: &Harness) -> Vec<&crate::transcript::ToolCall> {
    h.app
        .session
        .as_ref()
        .expect("a session")
        .transcript
        .tool_calls()
        .collect()
}

#[tokio::test(start_paused = true)]
async fn canonical_cards_take_their_detail_and_facts_from_tool_read() {
    let mut h = Harness::new();
    h.fake.script("tool.read", tool_read("failed", 2));
    h.fake.script_sequence(
        "session.read",
        reads(vec![
            (32, 30, vec![summary(30), assistant(31, "l9", "latest")]),
            (32, 30, vec![assistant_tool(8, "l1", "c1", "bash")]),
        ]),
    );
    h.open().await;
    assert!(
        h.calls("tool.read").is_empty(),
        "display cards have their detail"
    );
    h.action(Action::LoadOlder);
    h.settle().await;

    assert_eq!(
        h.calls("tool.read"),
        [
            json!({"session_id": "s1", "loop_id": "l1", "request_index": 0,
                "tool_call_id": "c1", "display": true})
        ]
    );
    let card = cards(&h)[0];
    assert_eq!(card.detail, "ls -la");
    assert_eq!(card.expanded_input.as_deref(), Some("ls -la\n"));
    assert_eq!(card.input_lines, Some(1));
    assert_eq!(card.state, crate::transcript::ToolState::Failed);
    assert_eq!(card.exit_code, Some(2));
    assert_eq!(card.duration, Some(std::time::Duration::from_millis(1500)));
    let screen = h.screen(100, 40);
    assert!(screen.contains("▎ ls -la"), "{screen}");
    assert!(screen.contains("1.5s · exit 2"), "{screen}");
}

#[tokio::test(start_paused = true)]
async fn every_card_of_a_page_is_read_once_and_a_missing_call_stays_bare() {
    let mut h = Harness::new();
    h.fake
        .script_error("tool.read", -32021, "tool not found", "tool_not_found");
    h.fake.script_sequence(
        "session.read",
        reads(vec![
            (32, 30, vec![summary(30), assistant(31, "l9", "latest")]),
            (
                32,
                30,
                vec![
                    assistant_tool(8, "l1", "c1", "bash"),
                    assistant_tool(9, "l1", "c2", "read"),
                ],
            ),
        ]),
    );
    h.open().await;
    h.action(Action::LoadOlder);
    h.settle().await;

    let asked: Vec<String> = h
        .calls("tool.read")
        .iter()
        .map(|params| params["tool_call_id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(asked, ["c1", "c2"]);
    assert!(h.notices().is_empty(), "{:?}", h.notices());
    assert!(cards(&h).iter().all(|card| card.detail.is_empty()));
}

#[tokio::test(start_paused = true)]
async fn find_matches_follow_their_blocks_when_a_page_is_put_in_front() {
    let mut h = Harness::opened(answered(80, 0)).await;
    h.ctrl(KeyCode::Char('f'));
    h.type_text("latest");
    let matched = |h: &Harness| match &h.app.overlay {
        crate::app::Overlay::Find { matches, .. } => {
            matches.iter().map(|found| found.block).collect::<Vec<_>>()
        }
        other => panic!("find is not open: {other:?}"),
    };
    assert_eq!(matched(&h), [1], "after the earlier-history marker");

    h.fake.script(
        "session.read",
        page(
            80,
            0,
            vec![user(18, "l0", "first"), assistant(19, "l0", "second")],
        ),
    );
    h.app.scroll_by(-1);
    h.action(Action::LoadOlder);
    h.settle().await;
    assert_eq!(h.texts(), ["first", "second", "latest"]);
    assert_eq!(matched(&h), [2]);
    h.press(KeyCode::Enter);
    assert_eq!(
        h.app.scroll,
        Scroll::Anchor {
            block: 2,
            offset: 0
        }
    );
}

fn with_records(mut read: Value, records: Value) -> Value {
    read["records"] = records;
    read
}

fn turn_end_lines(h: &mut Harness) -> Vec<String> {
    h.screen(100, 20)
        .lines()
        .map(|row| row.trim().to_owned())
        .filter(|row| row.starts_with("completed") || row.starts_with("interrupted"))
        .collect()
}

#[tokio::test(start_paused = true)]
async fn a_reopened_session_gets_its_turn_end_lines_and_totals_from_the_read() {
    let items = vec![
        user(0, "l1", "q1"),
        assistant_tool(1, "l1", "c1", "bash"),
        assistant(2, "l1", "a1"),
        user(3, "l2", "q2"),
        assistant(4, "l2", "a2"),
    ];
    let records = json!([
        {"loop_id": "l1", "outcome": completed(), "requests": 2,
         "usage": {"input_tokens": 100, "output_tokens": 10, "cache_read_tokens": 40}},
        {"loop_id": "l2", "outcome": cancelled(), "requests": 1,
         "usage": {"input_tokens": 50, "output_tokens": 5}},
    ]);
    let mut h = Harness::opened(with_records(page(5, 0, items), records)).await;
    assert_eq!(
        shape(&h),
        [
            "user",
            "assistant",
            "tool",
            "assistant",
            "other",
            "user",
            "assistant",
            "other"
        ]
    );
    assert_eq!(
        turn_end_lines(&mut h),
        ["completed · 2 requests · 1 tool", "interrupted"]
    );
    let usage = &h.app.session.as_ref().unwrap().usage;
    assert_eq!((usage.input, usage.output, usage.cached), (150, 15, 40));
}

#[tokio::test(start_paused = true)]
async fn totals_are_left_out_when_the_window_does_not_reach_the_summary() {
    let records = json!([{"loop_id": "l1", "outcome": completed(), "requests": 1,
                          "usage": {"input_tokens": 100}}]);
    let mut read = with_records(
        page(80, 0, vec![user(70, "l1", "q"), assistant(71, "l1", "a")]),
        records,
    );
    read["projection"]["first_item"] = json!(0);
    let h = Harness::opened(read).await;
    assert_eq!(h.marker(), Some(20));
    assert_eq!(h.app.session.as_ref().unwrap().usage.input, 0);

    let mut summarised = with_records(page(5, 3, vec![summary(3), user(4, "l1", "q")]), json!([]));
    summarised["projection"]["covered_usage"] =
        json!({"usage": {"input_tokens": 900, "output_tokens": 90}, "partial": false});
    let h = Harness::opened(summarised).await;
    assert_eq!(h.app.session.as_ref().unwrap().usage.input, 900);

    let mut partial = page(5, 3, vec![summary(3), user(4, "l1", "q")]);
    partial["projection"]["covered_usage"] =
        json!({"usage": {"input_tokens": 900}, "partial": true});
    let h = Harness::opened(partial).await;
    assert_eq!(h.app.session.as_ref().unwrap().usage.input, 0);
}
