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
