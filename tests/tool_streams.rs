use base64::{Engine, engine::general_purpose::STANDARD};
use minicore_tui::protocol::{
    ToolDataAvailabilityWire as Availability, ToolDataStreamWire as Stream, ToolRefWire,
    tool::{ToolOutputPage, ToolReadResult},
};
use minicore_tui::state::tool::StreamView;

fn page(base: u64, bytes: &[u8], eof: bool) -> ToolOutputPage {
    ToolOutputPage {
        tool_ref: ToolRefWire {
            session_id: "s".into(),
            loop_id: "l".into(),
            request_index: 2,
            tool_call_id: "c".into(),
        },
        stream: Stream::Stdout,
        encoding: "base64".into(),
        base_offset: base,
        next_offset: base + bytes.len() as u64,
        observed_end: base + bytes.len() as u64,
        eof,
        truncated: false,
        availability: Availability::Available,
        data: STANDARD.encode(bytes),
    }
}
#[test]
fn utf8_cross_page_invalid_bytes_and_controls_keep_raw_cursor() {
    let mut view = StreamView::new(Stream::Stdout);
    view.accept_page(&page(0, &[0xe4, 0xb8], false)).unwrap();
    assert_eq!(view.display_text(), "");
    view.accept_page(&page(2, &[0xad, 0xff, 0x1b, b'\r'], true))
        .unwrap();
    assert_eq!(view.next_offset, 6);
    let text = view.display_text();
    assert!(text.starts_with("中�"));
    assert!(!text.contains('\x1b'));
    assert!(!text.contains('\r'));
}
#[test]
fn overlap_event_query_does_not_duplicate_raw_bytes() {
    let mut view = StreamView::new(Stream::Stdout);
    let chunk = minicore_tui::protocol::ToolProcessChunkWire {
        stream: Stream::Stdout,
        encoding: "base64".into(),
        data: STANDARD.encode(b"abc"),
        base_offset: 0,
        next_offset: 3,
        observed_end: 3,
        dropped: false,
        expired: false,
    };
    view.accept_event(&chunk).unwrap();
    view.accept_page(&page(0, b"abcdef", true)).unwrap();
    assert_eq!(view.display_text(), "abcdef");
    assert_eq!(view.next_offset, 6);
}
#[test]
fn empty_gap_is_not_eof_and_clears_unicode_tail() {
    let mut view = StreamView::new(Stream::Stdout);
    view.accept_page(&page(0, &[0xe4], false)).unwrap();
    view.accept_page(&page(20, b"", false)).unwrap();
    assert_eq!(view.next_offset, 20);
    assert!(!view.eof);
    assert!(view.gap);
    view.accept_page(&page(20, b"tail", true)).unwrap();
    assert_eq!(view.display_text(), "tail");
}
#[test]
fn true_empty_eof_is_available_not_unavailable() {
    let mut view = StreamView::new(Stream::Stdout);
    view.accept_page(&page(0, b"", true)).unwrap();
    assert!(view.eof);
    assert_eq!(view.availability, Availability::Available);
}
#[test]
fn window_capacity_is_bounded_and_offsets_survive_head_eviction() {
    let mut view = StreamView::new(Stream::Stdout);
    for i in 0..80 {
        view.accept_page(&page(i * 16384, &vec![b'x'; 16384], false))
            .unwrap();
    }
    assert_eq!(view.retained_bytes, 1024 * 1024);
    assert_eq!(view.base_offset, 16 * 16384);
    assert_eq!(view.next_offset, 80 * 16384);
    assert!(view.truncated && view.gap);
}
#[test]
fn pinned_read_fixtures_preserve_policy_and_recording_facts() {
    for file in [
        "tool-read-awaiting-policy.json",
        "tool-read-running.json",
        "tool-read-terminal.json",
        "tool-read-clean-empty.json",
    ] {
        let value: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(format!("tests/fixtures/agent-v1/{file}")).unwrap(),
        )
        .unwrap();
        let read: ToolReadResult =
            serde_json::from_value(value.get("result").unwrap_or(&value).clone()).unwrap();
        if file.contains("awaiting") {
            assert!(read.execution.started_at.is_none());
            assert!(!read.execution.state.is_terminal());
        }
    }
}
#[test]
fn every_pinned_output_fixture_decodes_using_raw_offsets() {
    for file in [
        "tool-output-input.json",
        "tool-output-output.json",
        "tool-output-stdout.json",
        "tool-output-stderr.json",
        "tool-output-expired.json",
        "tool-output-partial.json",
        "tool-output-stdout-pending.json",
        "tool-output-clean-empty-eof-stdout.json",
    ] {
        let value: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(format!("tests/fixtures/agent-v1/{file}")).unwrap(),
        )
        .unwrap();
        let page: ToolOutputPage =
            serde_json::from_value(value.get("result").unwrap_or(&value).clone()).unwrap();
        let mut view = StreamView::new(page.stream);
        view.accept_page(&page).unwrap();
        assert_eq!(view.next_offset, page.next_offset);
        assert_eq!(view.availability, page.availability);
    }
}
#[test]
fn stream_debug_never_logs_content() {
    let page = page(0, b"secret-content", false);
    let mut view = StreamView::new(Stream::Stdout);
    view.accept_page(&page).unwrap();
    assert!(!format!("{page:?} {view:?}").contains("secret-content"));
}

#[test]
fn late_page_eof_cannot_hide_newer_event_bytes() {
    let mut view = StreamView::new(Stream::Stdout);
    view.accept_page(&page(0, b"newer", false)).unwrap();
    let mut stale = page(0, b"", true);
    stale.availability = Availability::Pending;
    view.accept_page(&stale).unwrap();
    assert!(!view.eof);
    assert_eq!(view.availability, Availability::Available);
    assert_eq!(view.next_offset, 5);
    assert_eq!(view.display_text(), "newer");
}

#[test]
fn detail_result_pages_share_the_existing_toolfacts_body() {
    use minicore_tui::state::tool::StreamChunk;
    use std::sync::Arc;
    let body: Arc<str> = Arc::from("中🙂".repeat(8000));
    let mut view = StreamView::new(Stream::Output);
    let mut page = page(0, b"", true);
    page.stream = Stream::Output;
    page.encoding = "utf8".into();
    page.data = body.to_string();
    page.next_offset = body.len() as u64;
    page.observed_end = page.next_offset;
    view.accept_page_with_result(&page, Some(&body)).unwrap();
    assert!(view.chunks.iter().all(
        |chunk| matches!(chunk, StreamChunk::Result { source, .. } if Arc::ptr_eq(source, &body))
    ));
    assert_eq!(view.display_text(), body.as_ref());
}

#[test]
fn event_gap_does_not_skip_bytes_that_an_authoritative_page_can_recover() {
    let mut view = StreamView::new(Stream::Stdout);
    let event = minicore_tui::protocol::ToolProcessChunkWire {
        stream: Stream::Stdout,
        encoding: "base64".into(),
        data: STANDARD.encode(b"tail"),
        base_offset: 4,
        next_offset: 8,
        observed_end: 8,
        dropped: false,
        expired: false,
    };
    view.accept_event(&event).unwrap();
    assert_eq!(view.next_offset, 0);
    view.accept_page(&page(0, b"headtail", true)).unwrap();
    assert_eq!(view.display_text(), "headtail");
}
