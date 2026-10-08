//! Streaming text follows the same Markdown, copy and identity contract as history.

use super::*;
use crate::event::{AppEvent, RpcEvent};
use crate::protocol::{IncomingFrame, RpcNotification};
use crate::state::transcript::{AssistantBlock, AssistantPart};
use crate::state::view::{ScrollAnchor, SelectionGranularity, SelectionPoint};
use crate::theme::ThemeKind;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use serde_json::json;

const SESSION: &str = "ses_1";
const LOOP: &str = "stream-markdown";

fn notify(app: &mut App, value: serde_json::Value) {
    app.update(AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Notification(
        RpcNotification::AgentEvent(serde_json::from_value(value).unwrap()),
    ))));
}

fn start_stream(theme: ThemeKind) -> App {
    let mut app = crate::ui::testapp::open_empty(theme, SESSION, None, "high");
    app.update(AppEvent::SubmitTurn {
        session_id: SESSION.into(),
        text: "Render this incrementally".into(),
    });
    let turn = json!({"session_id": SESSION, "loop_id": LOOP});
    let meta = json!({"session_id": SESSION, "dropped_before": 0});
    notify(
        &mut app,
        json!({"type": "turn_started", "data": {"turn": turn, "meta": meta}}),
    );
    notify(
        &mut app,
        json!({"type": "request_started", "data": {
            "turn": turn, "meta": meta, "request_index": 0, "config_revision": 0,
            "model": "deep", "reasoning": "high"
        }}),
    );
    app
}

fn delta(app: &mut App, text: &str) {
    notify(
        app,
        json!({"type": "output_delta", "data": {
            "turn": {"session_id": SESSION, "loop_id": LOOP},
            "meta": {"session_id": SESSION, "dropped_before": 0},
            "request_index": 0, "channel": "text", "delta": text
        }}),
    );
}

fn text_section(prepared: &PreparedConversation) -> crate::state::view::SectionView {
    prepared
        .sections
        .iter()
        .find(|section| {
            section.id.loop_id.as_deref() == Some(LOOP)
                && section.id.kind == SectionKind::AssistantText
        })
        .unwrap()
}

fn selection(section: &crate::state::view::SectionView, width: usize) -> ConversationSelection {
    ConversationSelection {
        session_id: SESSION.into(),
        anchor: SelectionPoint {
            row: section.rows.start,
            column: 0,
            section_id: Some(section.id.clone()),
            section_row: 0,
        },
        focus: SelectionPoint {
            row: section.rows.end - 1,
            column: width - 1,
            section_id: Some(section.id.clone()),
            section_row: section.rows.len() - 1,
        },
        granularity: SelectionGranularity::Character,
        dragged: true,
    }
}

#[test]
fn live_markdown_renders_structures_before_stream_completion() {
    for theme in [ThemeKind::Dark, ThemeKind::Light] {
        let mut app = start_stream(theme);
        let chunks = [
            "## 修复结果 / Result\n\n",
            "- first **item**\n- second",
            "\n\n```rust\n  let 中文 = 1;",
            "\n```\n\n| Key | Value |\n| --- | --- |\n| a | 中",
        ];
        for (index, chunk) in chunks.iter().enumerate() {
            delta(&mut app, chunk);
            let prepared = prepare_conversation(&app, 77);
            let section = text_section(&prepared);
            let lines = prepared.window(section.rows.start, section.rows.len());
            let text = lines
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            assert!(!text.contains("##"));
            assert!(lines.iter().flat_map(|line| &line.spans).any(|span| {
                span.content.contains("修复结果")
                    && span.style.fg == Some(theme.theme().md_heading)
                    && span.style.add_modifier.contains(Modifier::BOLD)
            }));
            if index >= 1 {
                assert!(text.contains("• first item"));
                assert!(!text.contains("**"));
            }
            if index >= 2 {
                assert!(
                    text.contains("╭"),
                    "an unfinished fence still has code geometry"
                );
                assert!(text.contains("let 中文 = 1;"));
                assert!(!text.contains("```"));
            }
            if index >= 3 {
                assert!(text.contains("Key"));
                assert!(
                    text.contains("─┼─"),
                    "formed table renders before its last row finishes"
                );
                assert!(!text.contains("---"));
            }
            assert!(!app.active_view().unwrap().live.as_ref().unwrap().waiting);
        }
    }
}

#[test]
fn live_markdown_partial_unicode_layout_matches_durable_metadata() {
    let source = "## 中文🙂 cafe\u{301}\n\n- **first**\n  - next\n\n```rust\n  │literal╭─╮ 中文🙂 cafe\u{301}\n```\n\n| 名 | 值 |\n| -- | -- |\n| 一 | 二 |\n\n[链接](https://example.test)";
    let app = crate::ui::testapp::open_empty(ThemeKind::Dark, SESSION, None, "high");
    for theme in [Theme::dark(), Theme::light()] {
        for width in [1, 2, 3, 8, 12, 24, 77] {
            let context = LiveRenderContext {
                theme: &theme,
                view: app.active_view().unwrap(),
                session_id: SESSION,
                loop_id: LOOP,
                request_index: 0,
                width,
                reasoning_visible: true,
                durable_tool_keys: None,
            };
            // Every valid text-delta boundary, including an open fence/list,
            // a combining sequence in progress, and an unfinished table/link.
            for end in source
                .char_indices()
                .map(|(end, _)| end)
                .chain([source.len()])
            {
                let partial = &source[..end];
                let input = assistant::AssistantSectionInput {
                    source: Arc::from(partial),
                    kind: SectionKind::AssistantText,
                    ordinal: 0,
                    collapsible: false,
                    folded: false,
                    tool_call: None,
                    in_hidden_run: false,
                };
                let durable = assistant::render_section(&theme, &input, width, true);
                let mut lines = vec![Line::from("previous"), Line::default()];
                let mut links = vec![Vec::new(), Vec::new()];
                let mut ranges = Vec::new();
                let mut copies = Vec::new();
                crate::markdown::reset_parse_count();
                context.append_text(
                    &mut lines,
                    &mut Some(&mut ranges),
                    &mut links,
                    &mut copies,
                    partial,
                    0,
                );
                assert_eq!(crate::markdown::parse_count(), 1);
                assert_eq!(lines.len(), links.len());
                let skip = usize::from(durable.lines.first().is_some_and(layout::line_is_blank));
                assert_eq!(lines[2..], durable.lines[skip..]);
                assert_eq!(links[2..], durable.link_cells[skip..]);
                if durable.lines.is_empty() {
                    assert!(ranges.is_empty());
                    assert!(copies.is_empty());
                    continue;
                }
                let range = SectionRange {
                    id: ranges[0].id.clone(),
                    rows: 0..durable.lines.len(),
                    content_columns: content_columns_for_kind(&SectionKind::AssistantText, width),
                    collapsible: false,
                    folded: false,
                };
                let (_, source_map, expected) = section_copy_metadata(
                    &durable.lines,
                    &range,
                    true,
                    Some(partial),
                    durable.hard_breaks.as_deref(),
                    durable.copy_cells.as_deref(),
                );
                assert_eq!(copies.len(), expected.len() - skip);
                for (live, saved) in copies.iter().zip(expected.iter().skip(skip)) {
                    assert_eq!(live.row, saved.row + 2 - skip);
                    assert_eq!(live.columns, saved.columns);
                    assert_eq!(live.text(), saved.text());
                    assert_eq!(live.source_offset, saved.source_offset);
                    assert_eq!(live.hard_break_after, saved.hard_break_after);
                    assert_eq!(live.decorative, saved.decorative);
                    assert_eq!(live.table_fragments, saved.table_fragments);
                    assert!(partial.is_char_boundary(live.source_offset));
                }
                assert!(source_map.rows.iter().all(|row| {
                    partial.is_char_boundary(row.source_range.start)
                        && partial.is_char_boundary(row.source_range.end)
                }));
            }
        }
    }
}

#[test]
fn live_markdown_copy_keeps_code_source_without_frames_or_soft_wraps() {
    let code = format!(
        "  │literal╭─╮  \n{}\n\n  end  ",
        "中文🙂cafe\u{301} ".repeat(12)
    );
    for width in [8, 24, 77] {
        for closed in [false, true] {
            let mut app = start_stream(ThemeKind::Dark);
            delta(
                &mut app,
                &format!("```rust\n{code}{}", if closed { "\n```" } else { "" }),
            );
            let prepared = prepare_conversation(&app, width);
            let section = text_section(&prepared);
            assert_eq!(
                selection_text(&prepared, &selection(&section, width as usize)),
                code
            );
        }
    }
}

#[test]
fn live_table_copy_survives_delimiter_append_resize_and_persistence() {
    let mut app = start_stream(ThemeKind::Dark);
    delta(&mut app, "| A | B |\n| --- |");
    assert!(
        prepare_conversation(&app, 26)
            .copy_ranges
            .iter()
            .all(|copy| copy.table_fragments.is_none())
    );
    delta(&mut app, " --- |\n| xxxxxxxxxxxx | y");
    let initial = prepare_conversation(&app, 26);
    let section = text_section(&initial);
    assert_eq!(
        selection_text(&initial, &selection(&section, 26)),
        "A|B\nxxxxxxxxxxxx|y"
    );
    let source_ids = |prepared: &PreparedConversation| {
        let mut ids = prepared
            .copy_ranges
            .iter()
            .flat_map(|copy| {
                copy.table_fragments.into_iter().flatten().map(|fragment| {
                    (
                        fragment.row_offset,
                        fragment.cell_offset,
                        fragment.cell_index,
                    )
                })
            })
            .collect::<Vec<_>>();
        ids.sort_unstable();
        ids.dedup();
        ids
    };
    let initial_ids = source_ids(&initial);
    delta(&mut app, " |\n| next | row |");
    for width in [9, 26, 77] {
        let prepared = prepare_conversation(&app, width);
        let section = text_section(&prepared);
        assert_eq!(
            selection_text(&prepared, &selection(&section, width as usize)),
            "A|B\nxxxxxxxxxxxx|y\nnext|row"
        );
        let appended_ids = source_ids(&prepared);
        assert!(initial_ids.iter().all(|id| appended_ids.contains(id)));
    }
    let source = "| A | B |\n| --- | --- |\n| xxxxxxxxxxxx | y |\n| next | row |";
    let view = app.sessions.known.get_mut(SESSION).unwrap();
    view.live = None;
    view.transcript
        .push_block(TranscriptBlock::Assistant(AssistantBlock {
            index: 1,
            loop_id: LOOP.into(),
            request_index: 0,
            model: "deep".into(),
            reasoning_level: crate::protocol::Reasoning::High,
            parts: vec![AssistantPart::Text(source.into())],
            tool_calls: Vec::new(),
            usage: Default::default(),
            finish_reason: "stop".into(),
            terminal_error: None,
        }));
    view.transcript.invalidate();
    for width in [9, 26, 77] {
        let prepared = prepare_conversation(&app, width);
        let section = text_section(&prepared);
        assert_eq!(
            selection_text(&prepared, &selection(&section, width as usize)),
            "A|B\nxxxxxxxxxxxx|y\nnext|row"
        );
        assert!(
            initial_ids
                .iter()
                .all(|id| source_ids(&prepared).contains(id))
        );
    }
}

#[test]
fn live_markdown_links_follow_shared_boundaries_and_text_ordinals() {
    let mut app = start_stream(ThemeKind::Dark);
    let request = &mut app
        .sessions
        .known
        .get_mut(SESSION)
        .unwrap()
        .live
        .as_mut()
        .unwrap()
        .requests[0];
    request.parts = vec![
        crate::state::turn::LivePart::Text("[first](https://example.test/one)".into()),
        crate::state::turn::LivePart::Reasoning("between".into()),
        crate::state::turn::LivePart::Text("[中文 second](https://example.test/two)".into()),
    ];
    let prepared = prepare_conversation(&app, 24);
    let sections: Vec<_> = prepared
        .sections
        .iter()
        .filter(|section| section.id.kind == SectionKind::AssistantText)
        .collect();
    assert_eq!(sections.len(), 2);
    for (ordinal, section) in sections.iter().enumerate() {
        assert_eq!(section.id.ordinal, ordinal as u32);
        assert!(section.rows.clone().any(|row| {
            prepared
                .links_at(row)
                .iter()
                .any(|range| range.contains(&2))
        }));
        assert!(section.rows.clone().all(|row| {
            prepared
                .links_at(row)
                .iter()
                .all(|range| range.start >= 1 && range.end <= 24)
        }));
    }
}

#[test]
fn live_markdown_completion_preserves_geometry_selection_and_scroll_anchor() {
    let source = format!(
        "## Result\n\n{}\n\n[link](https://example.test)",
        (0..30)
            .map(|index| format!("paragraph {index:02} 中文 cafe\u{301}\n\n"))
            .collect::<String>()
    );
    let mut app = start_stream(ThemeKind::Dark);
    app.update(AppEvent::TerminalSize {
        width: 80,
        height: 24,
    });
    delta(&mut app, &source);
    let live = prepare_conversation(&app, 77);
    let section = text_section(&live);
    let target = live
        .copy_ranges
        .iter()
        .find(|copy| section.rows.contains(&copy.row) && copy.text.contains("paragraph 05"))
        .unwrap();
    let anchor = ScrollAnchor {
        section_id: section.id.clone(),
        source_offset: target.source_offset,
        screen_row: 0,
    };
    let selected = selection(&section, 77);
    let copied = selection_text(&live, &selected);
    app.update(AppEvent::ConversationPrepared(live.clone()));
    app.update(AppEvent::Viewport {
        total_lines: live.total_rows(),
        visible_rows: 15,
    });
    let view = app.sessions.known.get_mut(SESSION).unwrap();
    view.scroll.follow_tail = false;
    view.scroll.offset = target.row;
    view.scroll.anchor = Some(anchor.clone());
    app.selection = Some(selected);
    // Model completion replaces the same logical section with its durable
    // identity; the history index is the only identity field that changes.
    let view = app.sessions.known.get_mut(SESSION).unwrap();
    view.live = None;
    view.transcript
        .push_block(TranscriptBlock::Assistant(AssistantBlock {
            index: 1,
            loop_id: LOOP.into(),
            request_index: 0,
            model: "deep".into(),
            reasoning_level: crate::protocol::Reasoning::High,
            parts: vec![AssistantPart::Text(source)],
            tool_calls: Vec::new(),
            usage: Default::default(),
            finish_reason: "stop".into(),
            terminal_error: None,
        }));
    view.transcript.invalidate();
    let saved = prepare_conversation(&app, 77);
    let saved_section = text_section(&saved);
    assert_eq!(
        live.window(section.rows.start, section.rows.len()),
        saved.window(saved_section.rows.start, saved_section.rows.len())
    );
    assert!(saved.has_scroll_anchor_section(&anchor));
    let saved_row = saved.row_for_scroll_anchor(&anchor).unwrap();
    assert!(
        saved
            .copy_row(saved_row)
            .unwrap()
            .text
            .contains("paragraph 05")
    );
    app.update(AppEvent::ConversationPrepared(saved.clone()));
    assert!(!app.active_view().unwrap().scroll.follow_tail);
    assert_eq!(app.active_view().unwrap().scroll.offset, saved_row);
    assert_eq!(
        selection_text(&saved, app.selection.as_ref().unwrap()),
        copied
    );
}

#[test]
fn live_markdown_deltas_leave_scrolled_history_and_editor_responsive() {
    let mut app = crate::ui::testapp::scrolled(ThemeKind::Dark);
    app.update(AppEvent::SubmitTurn {
        session_id: SESSION.into(),
        text: "start".into(),
    });
    notify(
        &mut app,
        json!({"type": "turn_started", "data": {
            "turn": {"session_id": SESSION, "loop_id": LOOP},
            "meta": {"session_id": SESSION, "dropped_before": 0}
        }}),
    );
    app.update(AppEvent::TerminalSize {
        width: 80,
        height: 24,
    });
    delta(&mut app, "## Streaming\n\n");
    let prepared = prepare_conversation(&app, 77);
    app.update(AppEvent::ConversationPrepared(prepared));
    app.update(AppEvent::Terminal(Event::Key(KeyEvent::new(
        KeyCode::Home,
        KeyModifiers::CONTROL,
    ))));
    let offset = app.active_view().unwrap().scroll.offset;
    let anchor_row = app
        .prepared_conversation(77)
        .unwrap()
        .row(offset)
        .unwrap()
        .clone();
    let focus = app.focus;
    for _ in 0..20 {
        delta(&mut app, "- 中文 **bold** and `code`\n");
        app.update(AppEvent::ConversationPrepared(prepare_conversation(
            &app, 77,
        )));
        assert!(!app.active_view().unwrap().scroll.follow_tail);
        assert_eq!(app.active_view().unwrap().scroll.offset, offset);
        assert_eq!(
            app.prepared_conversation(77).unwrap().row(offset),
            Some(&anchor_row)
        );
        crate::markdown::reset_parse_count();
        app.update(AppEvent::Terminal(Event::Key(KeyEvent::new(
            KeyCode::Char('x'),
            KeyModifiers::NONE,
        ))));
        assert_eq!(
            crate::markdown::parse_count(),
            0,
            "typing reuses the prepared Markdown frame"
        );
        assert_eq!(app.focus, focus);
    }
    assert_eq!(app.composer().lines().join("\n"), "x".repeat(20));
}

/// Observes cached-durable preparation plus the actual TestBackend draw. This
/// excludes terminal transport/input queuing and is not a hardware latency gate.
#[test]
#[ignore = "manual streaming/full-frame timing probe; run with --ignored --nocapture"]
fn measure_live_markdown_preparation_and_editor_input() {
    use ratatui::{Terminal, backend::TestBackend};
    use std::time::Instant;
    fn draw_frame(app: &mut App, terminal: &mut Terminal<TestBackend>) {
        app.update(AppEvent::TerminalSize {
            width: 80,
            height: 24,
        });
        let screen = layout::screen_layout(app, Rect::new(0, 0, 80, 24));
        let width = screen.content.width;
        if app.prepared_conversation(width).is_none() {
            let prepared =
                prepare_conversation_from_cache(app, width, app.cached_durable(width).unwrap());
            app.update(AppEvent::ConversationPrepared(prepared));
        }
        let total = app.prepared_conversation(width).unwrap().total_rows();
        app.update(AppEvent::Viewport {
            total_lines: total,
            visible_rows: visible_rows(app, total, screen.transcript.height),
        });
        terminal
            .draw(|frame| crate::ui::render(frame, app))
            .unwrap();
        app.update(AppEvent::Rendered);
    }
    for repeats in [240, 1920] {
        let mut app = start_stream(ThemeKind::Dark);
        app.update(AppEvent::TerminalSize {
            width: 80,
            height: 24,
        });
        let seed = "## Result 中文\n\nA **bold** paragraph with café and [link](https://example.test).\n\n- first\n- second\n\n```rust\n  let value = 1;\n```\n\n".repeat(repeats);
        delta(&mut app, &seed);
        app.update(AppEvent::ConversationPrepared(prepare_conversation(
            &app, 77,
        )));
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        draw_frame(&mut app, &mut terminal);
        let mut delta_frames = Vec::new();
        let mut input_frames = Vec::new();
        for _ in 0..100 {
            let before = Instant::now();
            delta(&mut app, "- next 中文🙂\n");
            draw_frame(&mut app, &mut terminal);
            delta_frames.push(before.elapsed());
            crate::markdown::reset_parse_count();
            let before = Instant::now();
            app.update(AppEvent::Terminal(Event::Key(KeyEvent::new(
                KeyCode::Char('x'),
                KeyModifiers::NONE,
            ))));
            draw_frame(&mut app, &mut terminal);
            input_frames.push(before.elapsed());
            assert_eq!(
                crate::markdown::parse_count(),
                0,
                "the complete typing draw path reuses live Markdown layout"
            );
        }
        delta_frames.sort();
        input_frames.sort();
        println!(
            "live_markdown_frame: profile={} seed_bytes={} updates=100 delta_prepare_draw_p95_us={} delta_prepare_draw_p99_us={} input_prepare_draw_p95_us={} input_prepare_draw_p99_us={}",
            if cfg!(debug_assertions) {
                "optimized_debug"
            } else {
                "release"
            },
            seed.len(),
            delta_frames[94].as_micros(),
            delta_frames[98].as_micros(),
            input_frames[94].as_micros(),
            input_frames[98].as_micros()
        );
        assert_eq!(app.composer().lines().join("\n"), "x".repeat(100));
    }
}

#[test]
fn live_markdown_source_anchor_survives_delta_and_width_change() {
    let lines = (0..80)
        .map(|i| format!("line-{i:03} some 中文 cafe\u{301} words\n"))
        .collect::<String>();
    let cases = [
        format!("```rust\n{lines}"),
        lines.clone(),
        lines.lines().map(|line| format!("    {line}\n")).collect(),
        format!(
            "- item\n\n  ```rust\n{}",
            lines
                .lines()
                .map(|line| format!("  {line}\n"))
                .collect::<String>()
        ),
        format!("prefix\u{1b}\n\n```\n{lines}"),
    ];
    for source in cases {
        let mut app = start_stream(ThemeKind::Dark);
        app.update(AppEvent::TerminalSize {
            width: 80,
            height: 24,
        });
        delta(&mut app, &source);
        let prepared = prepare_conversation(&app, 77);
        let section = text_section(&prepared);
        let target = prepared
            .copy_ranges
            .iter()
            .find(|copy| section.rows.contains(&copy.row) && copy.text.contains("line-020"))
            .unwrap();
        let anchor = ScrollAnchor {
            section_id: section.id.clone(),
            source_offset: target.source_offset,
            screen_row: 0,
        };
        let original_row = target.row;
        let original_text = target.text.to_owned();
        assert!(source.is_char_boundary(anchor.source_offset));
        assert!(
            anchor.source_offset >= source.find("line-017").unwrap(),
            "anchor must refer to this source row, not the first source line"
        );
        app.update(AppEvent::ConversationPrepared(prepared.clone()));
        app.update(AppEvent::Viewport {
            total_lines: prepared.total_rows(),
            visible_rows: 15,
        });
        let view = app.sessions.known.get_mut(SESSION).unwrap();
        view.scroll.follow_tail = false;
        view.scroll.offset = original_row;
        view.scroll.anchor = Some(anchor.clone());
        delta(&mut app, "line-080 tail\n");
        let extended = prepare_conversation(&app, 77);
        let resolved = extended.row_for_scroll_anchor(&anchor).unwrap();
        assert_eq!(resolved, original_row);
        assert_eq!(extended.copy_row(resolved).unwrap().text, original_text);
        app.update(AppEvent::ConversationPrepared(extended));
        assert_eq!(app.active_view().unwrap().scroll.offset, original_row);
        assert!(!app.active_view().unwrap().scroll.follow_tail);
        app.update(AppEvent::TerminalSize {
            width: 60,
            height: 24,
        });
        let narrower = prepare_conversation(&app, 57);
        let row = narrower.row_for_scroll_anchor(&anchor).unwrap();
        let copy = narrower.copy_row(row).unwrap();
        assert!(
            copy.source_offset.abs_diff(anchor.source_offset) < 100,
            "a width change must stay within the source neighborhood"
        );
        assert!(source.is_char_boundary(copy.source_offset));
        app.update(AppEvent::ConversationPrepared(narrower));
        assert_eq!(app.active_view().unwrap().scroll.offset, row);
        assert!(!app.active_view().unwrap().scroll.follow_tail);
    }
}

#[test]
fn live_markdown_repeated_code_lines_have_distinct_source_anchors() {
    let source = format!("```\n{}", "same 中文\tline\n".repeat(60));
    let mut app = start_stream(ThemeKind::Dark);
    delta(&mut app, &source);
    let prepared = prepare_conversation(&app, 77);
    let section = text_section(&prepared);
    let copies: Vec<_> = prepared
        .copy_ranges
        .iter()
        .filter(|copy| section.rows.contains(&copy.row) && copy.text.starts_with("same 中文"))
        .collect();
    assert_eq!(copies.len(), 60);
    let anchor = ScrollAnchor {
        section_id: section.id,
        source_offset: copies[20].source_offset,
        screen_row: 0,
    };
    assert_eq!(
        anchor.source_offset,
        source.match_indices("same 中文").nth(20).unwrap().0
    );
    delta(&mut app, "same 中文\tline\n");
    let extended = prepare_conversation(&app, 77);
    let row = extended.row_for_scroll_anchor(&anchor).unwrap();
    assert_eq!(
        extended.copy_row(row).unwrap().source_offset,
        anchor.source_offset
    );
}

#[test]
fn non_table_markdown_copy_retains_existing_list_and_quote_prefixes() {
    for (source, expected) in [
        ("- first\n- second", "• first\n• second"),
        ("> quote", "▍ quote"),
        (
            "- first\n  - second\n  - third",
            "• first\n  • second\n  • third",
        ),
        ("- item\n\n  ```\n  code│  \n  ```", "• item\n\ncode│  "),
    ] {
        let mut app = start_stream(ThemeKind::Dark);
        delta(&mut app, source);
        let prepared = prepare_conversation(&app, 77);
        assert_eq!(
            selection_text(&prepared, &selection(&text_section(&prepared), 77)),
            expected
        );
    }
}

#[test]
fn live_table_wrapped_cell_anchor_survives_append_and_resize() {
    let long = (0..240)
        .map(|i| format!("p{i:03}中文&amp; "))
        .collect::<String>();
    for (left, right) in [(long.as_str(), "short"), ("short", long.as_str())] {
        let source =
            format!("escaped\u{1b} prefix\n\n| A | B |\n| --- | --- |\n| {left} | {right} |\n");
        let mut app = start_stream(ThemeKind::Dark);
        app.update(AppEvent::TerminalSize {
            width: 80,
            height: 24,
        });
        delta(&mut app, &source);
        let prepared = prepare_conversation(&app, 77);
        let section = text_section(&prepared);
        let target = prepared
            .copy_ranges
            .iter()
            .find(|copy| {
                copy.table_fragments.is_some_and(|fragments| {
                    fragments
                        .iter()
                        .any(|fragment| fragment.chunk_offset > 300 && !fragment.text.is_empty())
                })
            })
            .unwrap();
        let original_row = target.row;
        let original_text = target.text.to_owned();
        let anchor = ScrollAnchor {
            section_id: section.id,
            source_offset: target.source_offset,
            screen_row: 0,
        };
        assert!(source.is_char_boundary(anchor.source_offset));
        assert!(anchor.source_offset > source.find("p000").unwrap() + 100);
        app.update(AppEvent::ConversationPrepared(prepared.clone()));
        app.update(AppEvent::Viewport {
            total_lines: prepared.total_rows(),
            visible_rows: 15,
        });
        let view = app.sessions.known.get_mut(SESSION).unwrap();
        view.scroll.follow_tail = false;
        view.scroll.offset = original_row;
        view.scroll.anchor = Some(anchor.clone());
        delta(&mut app, "| later | row |\n");
        let extended = prepare_conversation(&app, 77);
        let resolved = extended.row_for_scroll_anchor(&anchor).unwrap();
        assert_eq!(resolved, original_row);
        assert_eq!(extended.copy_row(resolved).unwrap().text, original_text);
        app.update(AppEvent::ConversationPrepared(extended));
        assert_eq!(app.active_view().unwrap().scroll.offset, original_row);
        app.update(AppEvent::TerminalSize {
            width: 60,
            height: 24,
        });
        let narrower = prepare_conversation(&app, 57);
        let resolved = narrower.row_for_scroll_anchor(&anchor).unwrap();
        let resolved_copy = narrower.copy_row(resolved).unwrap();
        assert!(resolved_copy.source_offset.abs_diff(anchor.source_offset) < 100);
        assert!(source.is_char_boundary(resolved_copy.source_offset));
        app.update(AppEvent::ConversationPrepared(narrower));
        assert_eq!(app.active_view().unwrap().scroll.offset, resolved);
        assert!(!app.active_view().unwrap().scroll.follow_tail);
    }
}
