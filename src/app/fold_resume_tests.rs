use super::*;
use crate::state::transcript::{AssistantBlock, AssistantPart, ToolBlock, UserBlock};
use crate::state::view::SectionKind;
use crate::ui::{testapp, transcript};
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend, layout::Rect};
use serde_json::json;

fn screen(app: &App) -> crate::ui::layout::ScreenLayout {
    crate::ui::layout::screen_layout(app, Rect::new(0, 0, 80, 24))
}

fn frame(app: &mut App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|frame| crate::ui::render(frame, app))
        .unwrap();
    let buffer = terminal.backend().buffer();
    app.remember_transcript_frame(buffer);
    buffer.content.iter().map(|cell| cell.symbol()).collect()
}

fn prepare(app: &mut App) {
    let area = screen(app).transcript;
    let prepared = transcript::prepare_conversation(app, area.width);
    let total = prepared.total_rows();
    app.update(AppEvent::ConversationPrepared(prepared));
    app.update(AppEvent::Viewport {
        total_lines: total,
        visible_rows: area.height as usize,
    });
    frame(app);
}

fn fixture(kind: SectionKind, prior: usize, asynchronous: bool) -> App {
    let mut app = testapp::open_empty(ThemeKind::Dark, "ses_1", None, "high");
    app.terminal_size = (80, 24);
    let view = app.active_session_mut().unwrap();
    for index in 0..prior {
        view.transcript.push_block(TranscriptBlock::User(UserBlock {
            index: Some(index),
            loop_id: Some(format!("prior-{index}")),
            kind: crate::protocol::UserMessageKindWire::Prompt,
            text: format!("Earlier prompt {index}"),
            pending: false,
        }));
    }
    if kind == SectionKind::Tool {
        use std::fmt::Write;
        let mut result = String::new();
        for i in 0..90 {
            writeln!(result, "result line {i}").unwrap();
        }
        view.transcript.push_block(TranscriptBlock::Tool(ToolBlock {
            index: Some(prior),
            loop_id: "fold-target".into(),
            request_index: 0,
            tool_call_id: "call-target".into(),
            name: "read".into(),
            result: Some(result.into()),
            outcome: Some(crate::protocol::ToolOutcomeWire::Success),
            live_status: None,
            progress: None,
            expanded: false,
        }));
        Arc::make_mut(&mut view.tool_folds).insert(
            ToolKey::new("ses_1", "fold-target", 0, "call-target"),
            FoldOverride::Collapsed,
        );
    } else {
        view.transcript
            .push_block(TranscriptBlock::Assistant(AssistantBlock {
                index: prior,
                loop_id: "fold-target".into(),
                request_index: 0,
                model: "model".into(),
                reasoning_level: crate::protocol::Reasoning::High,
                parts: vec![AssistantPart::Reasoning("thinking carefully ".repeat(300))],
                tool_calls: Vec::new(),
                usage: Default::default(),
                finish_reason: "stop".into(),
                terminal_error: None,
            }));
    }
    if asynchronous {
        app.enable_async_layout();
    }
    prepare(&mut app);
    app
}

fn target(app: &App, kind: SectionKind) -> crate::state::view::SectionView {
    app.prepared_conversation(screen(app).content.width)
        .unwrap()
        .sections
        .iter()
        .find(|section| section.id.kind == kind)
        .unwrap()
}

fn click_header(app: &mut App, kind: SectionKind) -> usize {
    let area = screen(app).transcript;
    let section = target(app, kind);
    let prepared = app.prepared_conversation(area.width).unwrap();
    let position = transcript::scroll_position(app, prepared.total_rows(), area.height as usize);
    let local = section.rows.start - position.offset;
    assert!(local < area.height as usize);
    let column = area.x + section.content_columns.start as u16;
    app.last_click = None;
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        app.update(AppEvent::Terminal(CrosstermEvent::Mouse(MouseEvent {
            kind,
            column,
            row: area.y + local as u16,
            modifiers: KeyModifiers::NONE,
        })));
    }
    local
}

#[test]
fn fold_resume_clicked_headers_stay_fixed_at_tail_and_in_long_history() {
    for kind in [SectionKind::Tool, SectionKind::Thinking] {
        for prior in [0, 250] {
            for asynchronous in [false, true] {
                let mut app = fixture(kind, prior, asynchronous);
                let original_folded = target(&app, kind).folded;
                assert!(original_folded);
                let row = click_header(&mut app, kind);
                assert!(app.active_view().unwrap().scroll.fold_pinned);
                // The production frame feedback may repeat the OLD geometry
                // while a new asynchronous layout is not ready yet.
                app.clamp_transcript_scroll();
                assert!(!app.active_view().unwrap().scroll.follow_tail);
                prepare(&mut app);
                assert!(!target(&app, kind).folded);
                let position = transcript::scroll_position(&app, app.viewport.0, app.viewport.1);
                assert_eq!(target(&app, kind).rows.start - position.offset, row);
                assert_eq!(click_header(&mut app, kind), row);
                prepare(&mut app);
                assert!(target(&app, kind).folded);
                let position = transcript::scroll_position(&app, app.viewport.0, app.viewport.1);
                assert_eq!(target(&app, kind).rows.start - position.offset, row);
                assert!(!app.active_view().unwrap().scroll.follow_tail);
                app.transcript_scroll_bottom();
                assert!(app.active_view().unwrap().scroll.follow_tail);
                assert!(!app.active_view().unwrap().scroll.fold_pinned);
            }
        }
    }
}

#[test]
fn fold_resume_collapse_keeps_bottom_padding_until_explicit_scroll() {
    let mut app = fixture(SectionKind::Tool, 250, false);
    click_header(&mut app, SectionKind::Tool);
    prepare(&mut app);
    // Put the expanded header near the top, then collapse a body much taller
    // than the viewport. A normal bottom clamp would pull the header downward.
    let row = target(&app, SectionKind::Tool).rows.start;
    app.set_transcript_offset(row - 2, app.viewport.0, app.viewport.1);
    frame(&mut app);
    assert_eq!(click_header(&mut app, SectionKind::Tool), 2);
    prepare(&mut app);
    let position = transcript::scroll_position(&app, app.viewport.0, app.viewport.1);
    assert_eq!(
        target(&app, SectionKind::Tool).rows.start - position.offset,
        2
    );
    assert!(position.offset > app.viewport.0.saturating_sub(app.viewport.1));
    app.transcript_scroll(-1);
    assert!(!app.active_view().unwrap().scroll.fold_pinned);
}

fn page(app: &App, start: usize, end: usize, total: usize) -> serde_json::Value {
    json!({
        "session": app.active_view().unwrap().info,
        "items": (start..end).map(|index| {
            let data = json!({"item":{"type":"user","data":{
                "loop_id":format!("loop-{index}"), "kind":"prompt",
                "input":{"text":format!("RESUMED-{index:03}")}
            }}}).to_string();
            json!({"index":index,"offset":0,"total_bytes":data.len(),
                "encoding":"utf8_json","data":data,"complete":true})
        }).collect::<Vec<_>>(),
        "total":total,"records":[],"records_truncated":false,
        "history_revision":"a".repeat(64),"captured_end":90_000,
        "trailing_incomplete":false,
        "next_cursor":if end < total { json!({"item":end,"offset":0}) } else { serde_json::Value::Null }
    })
}

fn loading_app(asynchronous: bool) -> (App, crate::protocol::OutgoingRequest) {
    let mut app = testapp::open_empty(ThemeKind::Dark, "ses_1", None, "high");
    let info = app.active_view().unwrap().info.clone();
    app.sessions
        .known
        .insert("ses_1".into(), app.new_session_view(info));
    app.terminal_size = (80, 24);
    app.enable_async_layout();
    if asynchronous {
        app.enable_async_decode();
    }
    app.active_session_mut()
        .unwrap()
        .history_read
        .begin(HistoryTrigger::Refresh);
    let command = app.request_history(&"ses_1".into()).unwrap();
    (app, testapp::take_requests(vec![command]).remove(0))
}

fn drain_decode(app: &mut App, mut commands: Vec<AppCommand>) -> Vec<AppCommand> {
    while let Some(request) = app.pending_decode_request() {
        assert!(app.history_tail_loading());
        assert!(app.layout_request(77).is_none());
        assert!(!frame(app).contains("RESUMED-"));
        app.mark_decode_scheduled();
        commands.extend(
            app.update(AppEvent::HistoryItemDecoded(Box::new(
                crate::jobs::DecodeOutcome {
                    identity: request.identity,
                    fingerprint: request.fingerprint,
                    result: crate::protocol::read::decode_item(&request.item.data)
                        .map_err(|error| error.to_string()),
                    cancelled: false,
                    scan: None,
                    export: None,
                },
            ))),
        );
    }
    commands
}

#[test]
fn fold_resume_paged_initial_history_publishes_only_final_tail() {
    for asynchronous in [false, true] {
        for total in [6, 225] {
            let (mut app, probe) = loading_app(asynchronous);
            let response = page(&app, 0, 1, total);
            let commands = testapp::respond(&mut app, &probe, response);
            let mut requests = testapp::take_requests(drain_decode(&mut app, commands));
            let mut expected_start = total
                .saturating_sub(crate::protocol::READ_TAIL_ITEMS)
                .max(1);
            while !requests.is_empty() {
                assert_eq!(requests.len(), 1);
                assert!(app.history_tail_loading());
                assert!(!frame(&mut app).contains("RESUMED-"));
                assert!(app.layout_request(77).is_none());
                let request = requests.remove(0);
                let start = request.params["cursor"]["item"].as_u64().unwrap() as usize;
                assert_eq!(start, expected_start);
                let end = (start + 3).min(total);
                let response = page(&app, start, end, total);
                let commands = testapp::respond(&mut app, &request, response);
                requests = testapp::take_requests(drain_decode(&mut app, commands));
                expected_start = end;
            }
            assert_eq!(expected_start, total);
            assert!(!app.history_tail_loading());
            assert!(!app.active_view().unwrap().initial_history_pending);
            prepare(&mut app);
            assert!(frame(&mut app).contains(&format!("RESUMED-{:03}", total - 1)));
            assert!(app.active_view().unwrap().scroll.follow_tail);
            // A subsequent refresh is not initial Resume hydration.
            let view = app.active_session_mut().unwrap();
            view.history_read.begin(HistoryTrigger::Refresh);
            view.transcript.complete = false;
            assert!(!app.history_tail_loading());
        }
    }
}

#[test]
fn fold_resume_initial_empty_and_failed_reads_release_loading() {
    let (mut app, probe) = loading_app(false);
    let response = page(&app, 0, 0, 0);
    testapp::respond(&mut app, &probe, response);
    assert!(!app.history_tail_loading());
    assert!(!app.active_view().unwrap().initial_history_pending);

    let (mut app, probe) = loading_app(false);
    testapp::respond_rpc_error(&mut app, &probe, -32603, "read failed");
    assert!(!app.history_tail_loading());
    assert!(!app.active_view().unwrap().initial_history_pending);
}

#[test]
fn fold_resume_search_releases_header_pin_and_shows_deep_match() {
    use crate::state::search::{SearchMatch, SearchSource};

    let mut app = fixture(SectionKind::Tool, 250, false);
    click_header(&mut app, SectionKind::Tool);
    prepare(&mut app);
    let view = app.active_session_mut().unwrap();
    let owner = view.transcript.blocks.last().unwrap().clone();
    let TranscriptBlock::Tool(tool) = owner.as_ref() else {
        panic!("expected tool");
    };
    let needle = "result line 35";
    let source_offset = tool.result.as_ref().unwrap().find(needle).unwrap();
    view.transcript.window.insert_owner(250, owner, 0, 2_000);
    // A terminal search action invalidates the preparation before it calls
    // jump_to_match. Keep that same ordering here, then exercise real reflow.
    app.prepared_conversation = None;
    assert!(
        app.jump_to_match(&SearchMatch {
            index: Some(250),
            source: SearchSource::ToolResult,
            loop_id: Some("fold-target".into()),
            request_index: Some(0),
            ordinal: 0,
            tool_call_id: Some("call-target".into()),
            preview: needle.into(),
            source_offset,
            byte_range: source_offset..source_offset + needle.len(),
        })
        .is_empty()
    );
    assert!(!app.active_view().unwrap().scroll.fold_pinned);
    prepare(&mut app);
    assert!(frame(&mut app).contains(needle));
}

#[test]
fn fold_resume_final_chunk_is_not_published_until_complete() {
    for asynchronous in [false, true] {
        let (mut app, probe) = loading_app(asynchronous);
        let mut first = page(&app, 0, 1, 1);
        let data = first["items"][0]["data"].as_str().unwrap().to_owned();
        let split = data.len() / 2;
        first["items"][0]["data"] = json!(&data[..split]);
        first["items"][0]["complete"] = json!(false);
        first["next_cursor"] = json!({"item":0,"offset":split});
        let requests = testapp::take_requests(testapp::respond(&mut app, &probe, first));
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].params["cursor"],
            json!({"item":0,"offset":split})
        );
        assert!(app.history_tail_loading());
        assert!(!frame(&mut app).contains("RESUMED-000"));
        let mut last = page(&app, 0, 1, 1);
        last["items"][0]["data"] = json!(&data[split..]);
        last["items"][0]["offset"] = json!(split);
        let commands = testapp::respond(&mut app, &requests[0], last);
        assert!(drain_decode(&mut app, commands).is_empty());
        assert!(!app.history_tail_loading());
        prepare(&mut app);
        assert!(frame(&mut app).contains("RESUMED-000"));
    }
}

#[test]
fn fold_resume_switching_away_and_back_does_not_replay_prefix() {
    let (mut app, probe) = loading_app(false);
    let response = page(&app, 0, 1, 6);
    let requests = testapp::take_requests(testapp::respond(&mut app, &probe, response));
    assert_eq!(requests.len(), 1);
    let mut info = app.active_view().unwrap().info.clone();
    info.session_id = "ses_other".into();
    let mut other = app.new_session_view(info);
    other.transcript.complete = true;
    other.initial_history_pending = false;
    other
        .transcript
        .push_block(TranscriptBlock::User(UserBlock {
            index: Some(0),
            loop_id: Some("other-loop".into()),
            kind: crate::protocol::UserMessageKindWire::Prompt,
            text: "OTHER-SESSION-CONTENT".into(),
            pending: false,
        }));
    app.sessions.known.insert("ses_other".into(), other);
    app.set_active_session(Some("ses_other".into()));
    prepare(&mut app);
    assert!(frame(&mut app).contains("OTHER-SESSION-CONTENT"));
    app.set_active_session(Some("ses_1".into()));
    let loading = frame(&mut app);
    assert!(!loading.contains("RESUMED-"));
    assert!(!loading.contains("OTHER-SESSION-CONTENT"));
    let response = page(&app, 1, 6, 6);
    assert!(testapp::respond(&mut app, &requests[0], response).is_empty());
    prepare(&mut app);
    assert!(frame(&mut app).contains("RESUMED-005"));
}
