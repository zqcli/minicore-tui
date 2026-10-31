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

fn bounded_new_history() -> Value {
    let mut page = page(
        160,
        0,
        (0..160)
            .map(|index| {
                user(
                    index,
                    &format!("new-history-{index}"),
                    &format!("new-prefix-{index}"),
                )
            })
            .collect(),
    );
    page["_fixture_one_item"] = json!(true);
    page["history_revision"] = json!("new-history-revision");
    page["captured_end"] = json!(160_000);
    page
}

async fn history_catchup_at_its_page_limit() -> Harness {
    let mut old = answered(80, 0);
    old["history_revision"] = json!("old-history-revision");
    old["captured_end"] = json!(80);
    let mut h = Harness::opened(old).await;
    h.fake
        .script_held("session.state", json!({"session_id":"s1"}));
    h.fake.script("session.read", bounded_new_history());
    h.control_event(json!({"completions":{"through_seq":1,"retained_from_seq":2,"entries":[],"truncated":true}})).await;
    h.until(|app| {
        app.session.as_ref().unwrap().recovery.history_status
            == Some(crate::app::recovery::RecoveryStatus::Exhausted)
    })
    .await;
    h
}

#[tokio::test(start_paused = true)]
async fn a_bounded_history_catchup_keeps_its_verified_window_and_continues_the_missing_suffix() {
    let mut h = history_catchup_at_its_page_limit().await;
    assert!(
        h.texts().iter().any(|text| text == "new-prefix-159"),
        "the verified newer window must not be discarded at the round limit"
    );
    assert_eq!(h.app.session.as_ref().unwrap().recovery.settled_through, 0);
    h.app.notices.clear();
    assert!(
        h.screen(120, 30).contains("/recover"),
        "the continuation is persistent, not a transient notice"
    );
    for _ in 0..8 {
        if h.app.session.as_ref().unwrap().recovery.settled_through == 1 {
            break;
        }
        h.action(Action::RetryRecovery);
        h.until(|app| {
            app.session.as_ref().unwrap().recovery.settled_through == 1
                || app.session.as_ref().unwrap().recovery.history_status
                    == Some(crate::app::recovery::RecoveryStatus::Exhausted)
        })
        .await;
    }
    assert_eq!(
        h.app.session.as_ref().unwrap().recovery.settled_through,
        1,
        "every bounded continuation must make real coverage progress"
    );
    assert!(h.texts().iter().any(|text| text == "new-prefix-0"));
}

#[tokio::test(start_paused = true)]
async fn older_pagination_to_zero_cannot_settle_an_unread_newer_history_suffix() {
    let mut h = history_catchup_at_its_page_limit().await;
    let mut old = page(80, 0, vec![user(0, "old-history", "older visible content")]);
    old["history_revision"] = json!("old-history-revision");
    old["captured_end"] = json!(80);
    h.fake.script_sequence("session.read", vec![]);
    h.fake.script("session.read", old);
    let read = crate::app::tasks::read_history(
        h.fake.client.clone(),
        "s1".into(),
        crate::app::tasks::Window::Before(20),
    )
    .await
    .unwrap();
    assert!(matches!(&read,crate::app::tasks::HistoryRead::Complete(page) if page.from == 0));
    h.op(crate::app::SessionOp::Page(Ok(read)));
    assert_eq!(
        h.app.session.as_ref().unwrap().recovery.settled_through,
        0,
        "old H0 coverage cannot stand in for the unread H1 suffix"
    );
    h.app.notices.clear();
    assert!(h.screen(120, 30).contains("/recover"));
}

#[tokio::test(start_paused = true)]
async fn saved_fallback_for_a_truncated_catalog_keeps_unknown_unsaved_turns_visibly_missing() {
    let mut h = Harness::opened(display_read(0, 0)).await;
    let mut saved = page(
        2,
        0,
        vec![
            user(0, "saved-B", "saved prompt B"),
            assistant(1, "saved-B", "saved answer B"),
        ],
    );
    saved["records"] = json!([{"loop_id":"saved-B","outcome":completed()}]);
    h.fake.script("session.read", saved);
    h.fake
        .script_held("session.state", json!({"session_id":"s1"}));
    h.control_event(json!({"completions":{"through_seq":2,"retained_from_seq":3,"truncated":true,"entries":[]}})).await;
    h.until(|app| {
        app.session.as_ref().unwrap().recovery.history_status
            == Some(crate::app::recovery::RecoveryStatus::Partial)
    })
    .await;
    assert_eq!(h.texts(), ["saved prompt B", "saved answer B"]);
    let session = h.app.session.as_ref().unwrap();
    assert_eq!(
        (
            session.recovery.observed_through,
            session.recovery.settled_through
        ),
        (2, 2),
        "settled includes an explicit missing range, never proof of two canonical turns"
    );
    assert!(session.recovery.truncated_gap);
    assert!(!session.is_closed("unknown-unsaved-A"));
    assert!(
        h.calls("turn.result").is_empty(),
        "missing identities cannot be guessed from the saved tail"
    );
    h.app.notices.clear();
    tokio::time::advance(std::time::Duration::from_secs(60)).await;
    h.app.tick();
    let screen = h.screen(120, 24).to_lowercase();
    assert!(
        screen.contains("unsaved") && screen.contains("missing"),
        "the persistent dock must identify the unknown unsaved gap after the transient fallback notice: {screen}"
    );
    assert!(!screen.contains("all history recovered"));
}

#[tokio::test(start_paused = true)]
async fn chunked_large_item_catchup_retains_the_new_suffix_and_resumes_its_exact_coverage() {
    use crate::app::tasks::{HistoryRead, Window};
    let h = Harness::opened(answered(80, 0)).await;
    let text = "x".repeat(1_000_000);
    let mut history = page(
        30,
        0,
        (0..30)
            .map(|index| user(index, &format!("byte-{index}"), &text))
            .collect(),
    );
    history["history_revision"] = json!("byte-limited-new-history");
    history["captured_end"] = json!(30_000_000);
    h.fake.script("session.read", history);
    let before = h.calls("session.read").len();
    let first =
        crate::app::tasks::read_history(h.fake.client.clone(), "s1".into(), Window::CatchUp(None))
            .await
            .unwrap();
    let HistoryRead::Limited(first, crate::app::tasks::ReadPartial::HistoryLimit) = first else {
        panic!("a round must stop at its read budget");
    };
    assert_eq!(first.end, 30);
    assert!(first.from > 0 && !first.items.is_empty());
    assert!(
        h.calls("session.read").len() - before <= crate::app::tasks::HISTORY_CATCHUP_MAX_PAGES,
        "probes and chunk continuations count toward the page limit"
    );
    let mut covered = first.coverage();
    assert_eq!(first.items.len(), covered.end - covered.from);
    drop(first);
    for _ in 0..8 {
        if covered.complete() {
            break;
        }
        let next = crate::app::tasks::read_history(
            h.fake.client.clone(),
            "s1".into(),
            Window::CatchUp(Some(covered.clone())),
        )
        .await
        .unwrap();
        let next = match next {
            HistoryRead::Complete(page) | HistoryRead::Limited(page, _) => page,
            HistoryRead::Partial(reason) => panic!("{reason:?}"),
        };
        assert_eq!(
            next.end, covered.from,
            "a continuation reads only the actual missing prefix of the newer pin"
        );
        assert!(next.from < next.end, "each continuation must make progress");
        assert_eq!(next.items.len(), next.end - next.from);
        covered.from = next.from;
    }
    assert!(covered.complete());
}

#[tokio::test(start_paused = true)]
async fn the_tail_window_and_each_older_page_move_the_marker() {
    let mut h = Harness::opened(answered(80, 0)).await;
    assert_eq!(
        cursors(&h),
        [20, 40, 60],
        "the tail is the last sixty items"
    );
    assert_eq!(h.marker(), Some(20));

    h.fake
        .script("session.read", page(80, 0, vec![user(0, "l0", "first")]));
    h.action(Action::LoadOlder);
    h.settle().await;
    assert_eq!(cursors(&h), [20, 40, 60, 0]);
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
    for (tail, total, index, text) in [
        (answered(80, 0), 80, 0, "display item"),
        (answered(32, 30), 32, 11, "canonical item"),
    ] {
        let mut h = Harness::opened(tail).await;
        let page = page(total, 0, vec![user(index, "l0", text)]);
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

#[tokio::test(start_paused = true)]
async fn a_late_history_page_error_keeps_the_existing_transcript_and_anchor() {
    let mut h = Harness::opened(answered(80, 0)).await;
    h.app.scroll = Scroll::Anchor {
        block: 1,
        offset: 0,
    };
    let before = format!("{:?}", h.blocks());
    let mut first = page(80, 0, vec![user(0, "l0", "unverified prefix")]);
    first.as_object_mut().unwrap().remove("_fixture_history");
    first["next_cursor"] = json!({"item": 1, "offset": 0});
    let mut bad_item = assistant(1, "l0", "bad");
    bad_item["encoding"] = json!("wrong");
    let mut last = page(80, 0, vec![bad_item]);
    last.as_object_mut().unwrap().remove("_fixture_history");
    last["next_cursor"] = Value::Null;
    h.fake
        .script_sequence("session.read", vec![display_read(80, 0), first, last]);
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
    assert!(
        h.notices().iter().any(|notice| notice.contains("encoding")),
        "{:?}",
        h.notices()
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
async fn a_saved_history_tail_preserves_unsaved_a_live_c_new_output_draft_and_anchor() {
    use crate::app::tasks::{self, HistoryRead, Window};
    use crate::protocol::read::{TurnPersistence, decode_item};
    use crate::protocol::{CompletedTurn, CompletionDirectory, LoopOutcome};
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script_held("turn.wait", waited("C", completed()));
    h.running_state("C", entry("iC", "steer", "prompt C"));
    let entries:Vec<CompletedTurn> = ["A","B"].into_iter().enumerate().map(|(i,id)|serde_json::from_value(json!({"seq":i+1,"loop_id":id,"completion_kind":"reported","outcome_kind":"completed","persistence":if id == "A" {"failed"} else {"persisted"},"availability":if id == "A" {"live"} else {"stored"}})).unwrap()).collect();
    let a = [json!({"item":{"type":"user","data":{"loop_id":"A","kind":"prompt","input":{"text":"prompt A"}}}}),json!({"item":{"type":"assistant","data":{"loop_id":"A","request_index":0,"content":[{"type":"text","data":"answer A"}]}}})].into_iter().map(|item|decode_item(&item.to_string()).unwrap()).collect::<Vec<_>>();
    let session = h.app.session.as_mut().unwrap();
    session.catalog = CompletionDirectory {
        through_seq: 2,
        retained_from_seq: 1,
        entries: entries.clone(),
        truncated: false,
    };
    session
        .reconcile_completion(
            &entries[0],
            &LoopOutcome::Completed,
            &a,
            None,
            TurnPersistence::Failed,
            false,
        )
        .unwrap();
    let mut saved = page(
        2,
        0,
        vec![user(0, "B", "prompt B"), assistant(1, "B", "answer B")],
    );
    saved["records"] = json!([{"loop_id":"B","outcome":completed()}]);
    h.fake.script("session.read", saved);
    let history = tasks::read_history(h.fake.client.clone(), "s1".into(), Window::Tail)
        .await
        .unwrap();
    assert!(matches!(&history, HistoryRead::Complete(_)));
    h.emit(
        "output_delta",
        json!({"turn":turn("C"),"request_index":0,"channel":"text","delta":"new C tail"}),
    )
    .await;
    h.until(|app| {
        app.transcript_blocks()
            .iter()
            .any(|block| matches!(block,Block::Assistant { text, .. } if text == "new C tail"))
    })
    .await;
    h.app.composer.set_text("keep this draft");
    let c = h.app.session.as_ref().unwrap().live.as_ref().unwrap().start;
    h.app.scroll = Scroll::Anchor {
        block: c + 1,
        offset: 1,
    };
    h.op(crate::app::SessionOp::Tail(Ok(history)));
    assert_eq!(
        h.texts(),
        [
            "prompt A",
            "answer A",
            "prompt B",
            "answer B",
            "prompt C",
            "new C tail"
        ]
    );
    assert_eq!(h.app.composer.text(), "keep this draft");
    assert_eq!(
        h.app.session.as_ref().unwrap().live.as_ref().unwrap().start,
        6
    );
    assert_eq!(
        h.app.scroll,
        Scroll::Anchor {
            block: 7,
            offset: 1
        }
    );
    assert!(h.blocks().iter().any(|block|matches!(block,Block::TurnEnd { loop_id,saving:Some(crate::transcript::Saving::Unsaved),.. } if loop_id == "A")));
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
            vec![user(11, "l0", "history one")]
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
            Timing::BeforeTheSendAnswers => h.fake.script_held("input.push", started("l1")),
            _ => h.fake.script("input.push", started("l1")),
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
                    app.transcript_blocks().iter().any(
                        |block| matches!(block, Block::TurnEnd { loop_id, .. } if loop_id == "l1"),
                    )
                })
                .await
            }
        }
        h.app.scroll = Scroll::Anchor {
            block: 1,
            offset: 1,
        };
        let total = if canonical { 32 } else { 100 };
        h.fake.script("session.read", page(total, 0, older));
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
        h.fake.release("input.push");
        h.fake.release("turn.wait");
        h.fake.release("turn.result");
        h.settle().await;
        assert_eq!(h.texts(), expected, "{timing:?}: {:?}", h.blocks());
    }
}

#[tokio::test(start_paused = true)]
async fn a_canonical_page_without_visible_content_removes_the_marker_and_the_turn_still_reconciles()
{
    let mut h = Harness::opened(answered(62, 60)).await;
    h.fake.script("input.push", started("l1"));
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

/// Every `session.read` the fake answers, including the tail's continuation pages.
fn reads(windows: Vec<(usize, usize, Vec<Value>)>) -> Vec<Value> {
    windows
        .into_iter()
        .enumerate()
        .flat_map(|(index, (total, floor, items))| {
            let pages = if index == 0 {
                total
                    .saturating_sub(floor)
                    .min(crate::protocol::TAIL_ITEMS)
                    .div_ceil(crate::protocol::PAGE_LIMIT)
            } else {
                1
            };
            std::iter::once(display_read(total, floor))
                .chain(std::iter::repeat_n(page(total, floor, items), pages))
        })
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
            (32, 30, vec![assistant_tool(11, "l1", "c1", "bash")]),
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
    assert!(screen.contains("▎ $ ls -la"), "{screen}");
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
                    assistant_tool(11, "l1", "c1", "bash"),
                    assistant_tool(12, "l1", "c2", "read"),
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

fn partial(request_index: u32, reasoning: &str, text: &str) -> Value {
    json!({"stop_reason": "aborted", "request_index": request_index, "model": "mock",
           "reasoning": reasoning, "text": text})
}

/// The `(request_index, reasoning, text)` of every assistant block of `loop_id`.
fn replies(h: &Harness, loop_id: &str) -> Vec<(u32, String, String)> {
    h.blocks()
        .iter()
        .filter_map(|block| match block {
            Block::Assistant {
                key,
                reasoning,
                text,
                ..
            } if key.loop_id == loop_id => {
                Some((key.request_index, reasoning.clone(), text.clone()))
            }
            _ => None,
        })
        .collect()
}

#[tokio::test(start_paused = true)]
async fn a_reopened_session_shows_the_interrupted_reply_before_the_turn_end_line() {
    let items = vec![user(0, "l1", "go"), assistant_tool(1, "l1", "c1", "bash")];
    let records = json!([{"loop_id": "l1", "outcome": cancelled(), "requests": 2,
                          "interrupted": partial(1, "weighing it", "I will start by readi")}]);
    let mut h = Harness::opened(with_records(page(2, 0, items), records)).await;
    assert_eq!(shape(&h), ["user", "tool", "assistant", "other"]);
    assert_eq!(
        replies(&h, "l1"),
        [(1, "weighing it".into(), "I will start by readi".into())]
    );
    assert_eq!(turn_end_lines(&mut h), ["interrupted · 1 tool"]);
}

#[tokio::test(start_paused = true)]
async fn a_turn_interrupted_in_its_first_request_still_ends_with_its_partial_reply() {
    let records = json!([{"loop_id": "l1", "outcome": cancelled(), "requests": 1,
                          "interrupted": partial(0, "", "Let me che")}]);
    let mut h = Harness::opened(with_records(page(1, 0, vec![user(0, "l1", "go")]), records)).await;
    assert_eq!(shape(&h), ["user", "assistant", "other"]);
    assert_eq!(h.texts(), ["go", "Let me che"]);
    assert_eq!(turn_end_lines(&mut h), ["interrupted"]);
}

#[tokio::test(start_paused = true)]
async fn a_turn_without_an_interrupted_reply_gets_no_extra_block_or_line() {
    let records = json!([{"loop_id": "l1", "outcome": cancelled(), "requests": 1}]);
    let h = Harness::opened(with_records(page(1, 0, vec![user(0, "l1", "go")]), records)).await;
    assert_eq!(shape(&h), ["user"]);
}

#[tokio::test(start_paused = true)]
async fn a_record_repeated_on_an_older_page_adds_its_interrupted_reply_once() {
    let records = json!([{"loop_id": "l1", "outcome": cancelled(), "requests": 3,
                          "interrupted": partial(2, "", "half an answer")}]);
    let tail = page(80, 0, vec![assistant_tool(79, "l1", "c2", "bash")]);
    let mut h = Harness::opened(with_records(tail, records.clone())).await;
    assert_eq!(replies(&h, "l1").len(), 1);

    let older = vec![user(18, "l1", "go"), assistant_tool(19, "l1", "c1", "bash")];
    h.fake
        .script("session.read", with_records(page(80, 0, older), records));
    h.action(Action::LoadOlder);
    h.settle().await;
    let halves = h.texts().iter().filter(|t| *t == "half an answer").count();
    assert_eq!(halves, 1, "{:?}", h.blocks());
    assert_eq!(turn_end_lines(&mut h).len(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_turn_cut_by_the_window_leaves_its_tool_count_out() {
    // The window starts after the turn's prompt and first cards.
    let items = vec![
        assistant_tool(7, "l1", "c9", "bash"),
        assistant(8, "l1", "a1"),
    ];
    let records = json!([{"loop_id": "l1", "outcome": completed(), "requests": 9}]);
    let mut h = Harness::opened(with_records(page(9, 7, items), records)).await;
    assert_eq!(turn_end_lines(&mut h), ["completed · 9 requests"]);
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

#[tokio::test(start_paused = true)]
async fn an_opened_session_shows_the_times_the_agent_stored_and_none_for_old_prompts() {
    let h = &mut Harness::opened(page(
        4,
        0,
        vec![
            user(0, "l0", "old prompt"),
            assistant(1, "l0", "old answer"),
            user_at(2, "l1", "new prompt", "2026-10-20T21:38:00Z"),
            assistant(3, "l1", "new answer"),
        ],
    ))
    .await;
    assert_eq!(h.sent_at("old prompt"), None);
    let screen = h.screen(60, 20);
    assert_eq!(screen.matches("· 10/20/2026").count(), 1, "{screen}");
    assert!(screen.contains("21:38 · 10/20/2026"), "{screen}");
}

#[tokio::test(start_paused = true)]
async fn a_request_that_only_called_tools_adds_no_row_between_its_neighbours_cards() {
    let request = |index: usize, tool: &str| {
        chunk(
            index,
            json!({"type": "assistant", "data": {"loop_id": "l1", "request_index": index - 1,
                "model": "mock", "content": [
                    {"type": "reasoning", "data": {"text": null, "summary": null}},
                    {"type": "tool_call", "data": {"tool_call_id": tool, "name": tool}}]}}),
        )
    };
    let items = vec![
        user(0, "l1", "go"),
        request(1, "bash"),
        request(2, "read"),
        request(3, "grep"),
    ];
    let mut h = Harness::opened(page(4, 0, items)).await;
    let screen = h.screen(60, 30);
    let row = |name: &str| {
        screen
            .lines()
            .position(|line| line.starts_with(&format!(" ▎ {name}")))
            .unwrap_or_else(|| panic!("no {name} card:\n{screen}"))
    };
    let gap = |from: &str, to: &str| row(to) - row(from) - 1;
    assert_eq!(
        (gap("bash", "read"), gap("read", "grep")),
        (1, 1),
        "{screen}"
    );
}
