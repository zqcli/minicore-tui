//! Focused regression tests for the Rail spacing contract (0.2.2):
//! exactly one transparent external spacer between the user card and a
//! following thinking section, reasoning raw newlines preserved as visual
//! line breaks, like-live/history consistency for consecutive reasoning
//! parts, and one clear blank above the busy Working status row. All tests
//! run against the real renderer/App paths, never abstract helpers.

use ratatui::text::Line;

use crate::state::transcript::{AssistantBlock, AssistantPart};
use crate::theme::{Theme, ThemeKind};
use crate::ui::testapp;
use crate::ui::{assistant, reasoning, transcript};

fn content_rows(lines: &[Line<'_>]) -> Vec<String> {
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        })
        .collect()
}

/// RED contract: a raw single newline inside a thinking run is a visual line
/// break, not a markdown soft-break space.
#[test]
fn thinking_single_newline_is_a_visual_break_not_flattened() {
    let theme = Theme::dark();
    let lines =
        reasoning::reasoning_lines_with_fold(&theme, "first\nsecond", 40, true, false, None);
    let rows = content_rows(&lines);
    assert!(
        rows.iter().any(|row| row.contains("first")),
        "first line must appear in its own row: {rows:?}"
    );
    assert!(
        rows.iter().any(|row| row.contains("second")),
        "second line must appear in its own row: {rows:?}"
    );
    let first = rows.iter().position(|row| row.contains("first")).unwrap();
    let second = rows.iter().position(|row| row.contains("second")).unwrap();
    assert!(
        first != second,
        "first and second must not share one visual row (SoftBreak flattening): {rows:?}"
    );
    assert!(
        second == first + 1,
        "single newline yields exactly two adjacent content rows: {rows:?}"
    );
}

/// RED contract: consecutive durable reasoning parts join without forcing
/// paragraph breaks, matching how live SSE deltas accumulate.
#[test]
fn consecutive_reasoning_parts_join_like_live_accumulation() {
    let theme = Theme::dark();
    let lines = assistant::lines_with_folds(
        &theme,
        &AssistantBlock {
            index: 1,
            loop_id: "turn".to_owned(),
            request_index: 0,
            model: "model".to_owned(),
            reasoning_level: crate::protocol::Reasoning::High,
            parts: vec![
                AssistantPart::Reasoning("first".to_owned()),
                AssistantPart::Reasoning("\nsecond".to_owned()),
            ],
            tool_calls: vec![],
            usage: Default::default(),
            finish_reason: "stop".to_owned(),
            terminal_error: None,
        },
        40,
        true,
        &std::collections::HashMap::new(),
    );
    let rows = content_rows(&lines);
    let first = rows.iter().position(|row| row.contains("first")).unwrap();
    let second = rows.iter().position(|row| row.contains("second")).unwrap();
    assert_eq!(
        second,
        first + 1,
        "consecutive parts must not force a blank paragraph between them: {rows:?}"
    );
}

/// RED contract: a user card's internal slate padding is not an external
/// spacer; exactly one transparent blank separates the card from the first
/// thinking row.
#[test]
fn user_card_then_thinking_has_exactly_one_transparent_spacer() {
    let app = testapp::chat_with_reasoning(ThemeKind::Dark);
    let prepared = transcript::prepare_conversation(&app, 77);
    let rows = content_rows(&prepared.lines());
    let thinking_first = rows
        .iter()
        .position(|row| row.contains("carefully thinking out loud"))
        .expect("thinking content present");
    // The row immediately before the first thinking row must be a transparent
    // blank (no rail glyph, no card fill), and the row before that must be the
    // user card's own filled padding (unchanged internal surface).
    assert!(
        rows[thinking_first - 1].is_empty(),
        "exactly one transparent spacer before thinking: {:?}",
        rows[thinking_first - 2..=thinking_first].to_vec()
    );
    assert!(
        rows[thinking_first - 2].contains("▎") && !rows[thinking_first - 2].trim().is_empty(),
        "the user card's internal padding row is unchanged: {:?}",
        rows[thinking_first - 2..=thinking_first].to_vec()
    );
    // And the thinking section also carries its own trailing transparent blank
    // so the next section boundary stays single-spaced.
    assert!(
        rows[thinking_first + 1].is_empty() || rows[thinking_first + 2].is_empty(),
        "thinking keeps a trailing spacer: {:?}",
        rows[thinking_first..thinking_first + 3].to_vec()
    );
}

/// RED contract: the live SSE reasoning deltas with a raw newline keep that
/// newline as a visual row break in the prepared transcript.
#[test]
fn prepared_transcript_preserves_thinking_newlines_from_wire_entry() {
    let mut app = testapp::open_empty(ThemeKind::Dark, "ses_1", Some("Task"), "high");
    app.update(crate::event::AppEvent::SubmitTurn {
        session_id: "ses_1".to_owned(),
        text: "stream me".to_owned(),
    });
    let notification = |value: serde_json::Value| {
        crate::event::AppEvent::Rpc(crate::event::RpcEvent::Frame(
            crate::protocol::IncomingFrame::Notification(
                crate::protocol::RpcNotification::AgentEvent(
                    serde_json::from_value(value).unwrap(),
                ),
            ),
        ))
    };
    app.update(notification(serde_json::json!({
        "type": "turn_started",
        "data": {"turn": {"session_id": "ses_1", "loop_id": "loop_live"},
                 "meta": {"session_id": "ses_1", "dropped_before": 0}}
    })));
    app.update(notification(serde_json::json!({
        "type": "request_started",
        "data": {
            "turn": {"session_id": "ses_1", "loop_id": "loop_live"},
            "request_index": 0,
            "config_revision": 0,
            "model": "deep",
            "reasoning": "high",
            "meta": {"session_id": "ses_1", "dropped_before": 0}
        }
    })));
    let delta = |channel: &str, delta_text: &str| {
        notification(serde_json::json!({
            "type": "output_delta",
            "data": {
                "turn": {"session_id": "ses_1", "loop_id": "loop_live"},
                "request_index": 0,
                "channel": channel,
                "delta": delta_text,
                "meta": {"session_id": "ses_1", "dropped_before": 0}
            }
        }))
    };
    app.update(delta("reasoning", "first line"));
    app.update(delta("reasoning", "\nsecond line"));
    let prepared = transcript::prepare_conversation(&app, 77);
    let rows = content_rows(&prepared.lines());
    let first = rows
        .iter()
        .position(|row| row.contains("first line"))
        .expect("first reasoning row");
    let second = rows
        .iter()
        .position(|row| row.contains("second line"))
        .expect("second reasoning row");
    assert!(
        second == first + 1
            && rows[first].contains("first line")
            && rows[second].contains("second line"),
        "live reasoning newline must stay a visual row break: {rows:?}"
    );
}

/// RED contract: while busy, one clear transparent blank always separates
/// the last transcript row from the Working status row, even when content
/// fills the viewport.
fn busy_with_filled_transcript() -> crate::app::App {
    // A long durable transcript of user cards (fills any of the three
    // viewports) that then enters a running state with no live output yet,
    // so the transcript tail is a user card directly above Working.
    let mut items = Vec::new();
    for index in 0..24 {
        items.push(testapp::user_entry(
            index,
            &format!("loop_{index}"),
            &format!("user message {index}"),
        ));
    }
    let mut app = testapp::open_with(ThemeKind::Dark, "ses_1", Some("Task"), "high", items);
    testapp::set_session_running(&mut app, "ses_1", "loop_last");
    app
}

#[test]
fn busy_working_keeps_one_clear_blank_above_it_at_every_size() {
    for (width, height) in [(60u16, 16u16), (80, 24), (120, 40)] {
        let app = busy_with_filled_transcript();
        let terminal = crate::ui::component_tests::draw(&app, width, height);
        let rows = crate::ui::component_tests::buffer_lines(&terminal);
        let status = rows
            .iter()
            .rposition(|row| row.contains("Working") || row.contains("Running"))
            .expect("busy status row present");
        assert!(
            rows[status - 1][..width as usize - 1].trim().is_empty(),
            "{width}x{height}: row above the Working status must be blank at {status} (scrollbar column allowed): {:?}",
            rows[status - 3..status + 1].to_vec()
        );
    }
}
