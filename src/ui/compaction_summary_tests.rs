use super::*;

fn summary_layout(source: &str, folded: bool) -> Arc<SectionLayout> {
    let width = 72;
    let (lines, links, breaks) = compaction_summary_lines(&Theme::dark(), width, source, folded);
    make_section_layout(
        LayoutKey {
            section: SectionId {
                session_id: Arc::from("summary-test"),
                loop_id: None,
                request_index: None,
                kind: SectionKind::Summary,
                ordinal: 0,
                tool_call_id: None,
                history_index: Some(0),
            },
            revision: 1,
            width: width as u16,
            theme: crate::theme::ThemeKind::Dark,
            folded,
            reasoning_visible: true,
        },
        lines,
        links,
        true,
        folded,
        0,
        Some(source),
        Some(&breaks),
        None,
    )
    .unwrap()
}

#[test]
fn compaction_summary_collapsed_header_is_decorative_but_source_is_complete() {
    let source = "# Decisions\n\nKeep **all** of this summary.";
    let layout = summary_layout(source, true);
    assert_eq!(layout.rows.len(), 3);
    assert!(layout.rows[1].to_string().contains("[compaction]"));
    assert!(layout.rows[1].to_string().contains("expand"));
    assert!(!layout.rows[1].to_string().contains("Decisions"));
    assert!(layout.copy_ranges.iter().all(|row| row.decorative));
    assert_eq!(layout.source_map.source.as_ref(), source);
    assert!(layout.collapsible && layout.folded);
}

#[test]
fn compaction_summary_expansion_renders_markdown_links_and_excludes_header_from_copy() {
    let source = "# Decisions\n\nKeep **everything**.\n\n[reference](https://example.test)\n\n```rust\nlet x = 1;\n```";
    let layout = summary_layout(source, false);
    let copied = layout
        .copy_ranges
        .iter()
        .filter(|row| !row.decorative)
        .map(|row| row.text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(copied.contains("Decisions"));
    assert!(copied.contains("everything"));
    assert!(copied.contains("let x = 1;"));
    assert!(!copied.contains("[compaction]"));
    assert!(!copied.contains("click to collapse"));
    assert!(!copied.contains(crate::ui::rail::RAIL_GLYPH));
    assert!(layout.link_cells.iter().flatten().next().is_some());
    assert!(
        layout
            .link_cells
            .iter()
            .flatten()
            .all(|range| range.start >= 1)
    );
    assert_eq!(layout.source_map.source.as_ref(), source);
    assert!(!layout.folded);
    assert_eq!(summary_layout(source, true).rows.len(), 3);
}

#[test]
fn compaction_summary_large_body_never_enters_markdown_layout() {
    let source = "*".repeat(crate::limits::LAYOUT_SECTION_BYTES + 1);
    for folded in [true, false] {
        let layout = summary_layout(&source, folded);
        assert_eq!(layout.rows.len(), 3);
        assert!(layout.copy_ranges.iter().all(|row| row.decorative));
        assert_eq!(layout.source_map.source.len(), source.len());
    }
}

#[test]
fn compaction_summary_dark_light_and_narrow_header_keep_one_line_and_neutral_surface() {
    for theme in [Theme::dark(), Theme::light()] {
        for width in [8, 24, 80] {
            let (lines, links, breaks) =
                compaction_summary_lines(&theme, width, "hidden body", true);
            assert_eq!(lines.len(), 3);
            assert_eq!(links.len(), lines.len());
            assert_eq!(breaks.len(), lines.len());
            assert_eq!(crate::markdown::line_width(&lines[1]), width);
            assert!(lines[1].to_string().contains('▸'));
            assert_eq!(lines[1].spans[0].style.fg, Some(theme.rail_thinking));
            assert!(
                lines[1]
                    .spans
                    .iter()
                    .all(|span| span.style.bg == Some(theme.card_bg))
            );
        }
    }
}

#[test]
fn compaction_summary_success_feedback_does_not_stack_and_current_unknown_stays_visible() {
    let mut app =
        crate::ui::testapp::open_empty(crate::theme::ThemeKind::Dark, "ses_1", None, "high");
    for index in 0..16 {
        app.sessions
            .known
            .get_mut("ses_1")
            .unwrap()
            .record_compaction_result(
            serde_json::from_value(
                serde_json::json!({"operation_id": format!("op-{index}"), "status": "compacted"}),
            )
            .unwrap(),
        );
        let (rows, _, _) = build_live_tail(&Theme::dark(), &app, 100, None, None, None);
        assert!(
            !rows
                .iter()
                .any(|row| row.to_string().contains("compaction"))
        );
    }
    let mut context: serde_json::Value = serde_json::from_str::<serde_json::Value>(
        &std::fs::read_to_string("tests/fixtures/agent-v1/session-context-idle.json").unwrap(),
    )
    .unwrap()["result"]
        .clone();
    context["session_id"] = "ses_1".into();
    context["last_result"] = serde_json::json!({"operation_id": "unknown", "status": "unknown_write", "origin": "automatic"});
    let view = app.sessions.known.get_mut("ses_1").unwrap();
    view.context = Some(serde_json::from_value(context).unwrap());
    view.state.as_mut().unwrap().status = crate::protocol::SessionStatusWire::Blocked;
    app.notices.clear();
    for status in [
        crate::protocol::SessionStatusWire::Blocked,
        crate::protocol::SessionStatusWire::Idle,
        crate::protocol::SessionStatusWire::Blocked,
    ] {
        app.sessions
            .known
            .get_mut("ses_1")
            .unwrap()
            .state
            .as_mut()
            .unwrap()
            .status = status;
        let (rows, _, _) = build_live_tail(&Theme::dark(), &app, 100, None, None, None);
        assert!(
            !rows
                .iter()
                .any(|row| row.to_string().contains("Compaction write outcome unknown")),
            "old automatic result must not be attributed to a new block"
        );
        if status == crate::protocol::SessionStatusWire::Blocked {
            let backend = ratatui::backend::TestBackend::new(100, 2);
            let mut terminal = ratatui::Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| crate::ui::status::render(frame, frame.area(), &app, &Theme::dark()))
                .unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(
                text.contains("Blocked"),
                "current authoritative block survives notice expiry"
            );
        }
    }
}
