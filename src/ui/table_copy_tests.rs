//! Table selection uses the exact rendered fragments, including during reflow.

use super::*;
use crate::markdown::TableCopyFragment;
use crate::state::view::{SelectionGranularity, SelectionPoint};

const SESSION: &str = "table-copy";
const WRAPPED: &str = "| A | B |\n| --- | --- |\n| xxxxxxxxxxxx | y |";

fn prepare(sources: &[&str], width: usize) -> PreparedConversation {
    let mut lines = Vec::new();
    let mut sections = Vec::new();
    let mut copies = Vec::new();
    for (ordinal, source) in sources.iter().enumerate() {
        let rendered =
            assistant::render_text_section(&Theme::dark(), source, width, ordinal as u32);
        let mut range = SectionRange {
            id: SectionId {
                session_id: Arc::from(SESSION),
                loop_id: Some(Arc::from("loop")),
                request_index: Some(0),
                kind: SectionKind::AssistantText,
                ordinal: ordinal as u32,
                tool_call_id: None,
                history_index: None,
            },
            rows: 0..rendered.lines.len(),
            content_columns: content_columns_for_kind(&SectionKind::AssistantText, width),
            collapsible: false,
            folded: false,
        };
        let (_, _, section_copies) = section_copy_metadata(
            &rendered.lines,
            &range,
            true,
            Some(source),
            rendered.hard_breaks.as_deref(),
            rendered.copy_cells.as_deref(),
        );
        let offset = lines.len();
        copies.extend(section_copies.into_iter().map(|mut copy| {
            copy.row += offset;
            copy
        }));
        range.rows = offset..offset + rendered.lines.len();
        sections.push(range);
        lines.extend(rendered.lines);
    }
    PreparedConversation {
        width: width as u16,
        session_id: Some(SESSION.into()),
        live: lines,
        sections: SectionIndex {
            live: Arc::new(sections),
            ..SectionIndex::default()
        },
        copy_ranges: CopyIndex {
            live: Arc::new(copies),
            ..CopyIndex::default()
        },
        ..PreparedConversation::default()
    }
}

fn selected(prepared: &PreparedConversation, start: (usize, usize), end: (usize, usize)) -> String {
    let point = |(row, column)| {
        let section = prepared.sections.at_row(row).unwrap();
        SelectionPoint {
            row,
            column,
            section_id: Some(section.id),
            section_row: row - section.rows.start,
        }
    };
    selection_text(
        prepared,
        &ConversationSelection {
            session_id: SESSION.into(),
            anchor: point(start),
            focus: point(end),
            granularity: SelectionGranularity::Character,
            dragged: true,
        },
    )
}

fn all(prepared: &PreparedConversation) -> String {
    selected(
        prepared,
        (0, 0),
        (prepared.total_rows() - 1, prepared.width as usize - 1),
    )
}

fn fragments(prepared: &PreparedConversation) -> Vec<(usize, TableCopyFragment)> {
    prepared
        .copy_ranges
        .iter()
        .flat_map(|copy| {
            copy.table_fragments
                .into_iter()
                .flatten()
                .map(move |fragment| (copy.row, fragment.clone()))
        })
        .collect()
}

#[test]
fn table_copy_joins_wrapped_cells_in_source_order_at_width_25() {
    // The shared assistant helper contributes one inset column.
    let prepared = prepare(&[WRAPPED], 26);
    assert_eq!(all(&prepared), "A|B\nxxxxxxxxxxxx|y");
    let body_offset = WRAPPED.find("| x").unwrap();
    let body: Vec<_> = fragments(&prepared)
        .into_iter()
        .filter(|(_, fragment)| fragment.row_offset == body_offset)
        .collect();
    assert_eq!(
        body.iter()
            .filter(|(_, fragment)| fragment.cell_index == 0)
            .count(),
        2
    );
    assert_eq!(body[0].1.chunk_offset, 0);
    assert_eq!(body.last().unwrap().1.chunk_offset, 11);
    assert!(!all(&prepared).contains(['│', '─']));
}

#[test]
fn table_copy_intersects_one_glyph_without_widening_to_a_cell_or_row() {
    for width in [5, 9, 12, 26, 80] {
        let prepared = prepare(&[WRAPPED], width);
        for (row, fragment) in fragments(&prepared) {
            for column in fragment.columns {
                assert_eq!(
                    selected(&prepared, (row, column), (row, column)).len(),
                    1,
                    "width {width}, row {row}, column {column}"
                );
            }
        }
        for copy in prepared.copy_ranges.iter() {
            let visible = copy.table_fragments.unwrap_or_default();
            for column in 0..width {
                if !visible
                    .iter()
                    .any(|fragment| fragment.columns.contains(&column))
                {
                    assert_eq!(
                        selected(&prepared, (copy.row, column), (copy.row, column)),
                        "",
                        "layout decoration at width {width}, row {}, column {column}",
                        copy.row
                    );
                }
            }
        }
    }
}

#[test]
fn table_copy_does_not_pull_unselected_column_wraps_between_source_offsets() {
    let prepared = prepare(&[WRAPPED], 26);
    let body_offset = WRAPPED.find("| x").unwrap();
    let body: Vec<_> = fragments(&prepared)
        .into_iter()
        .filter(|(_, fragment)| fragment.row_offset == body_offset)
        .collect();
    let second = body
        .iter()
        .find(|(_, fragment)| fragment.cell_index == 1)
        .unwrap();
    let continuation = body
        .iter()
        .find(|(_, fragment)| fragment.chunk_offset > 0)
        .unwrap();
    let start = (second.0, second.1.columns.start);
    let end = (continuation.0, continuation.1.columns.start);
    // The first eleven x's are outside this display selection. Reconstructing
    // a min..max raw interval would accidentally include them.
    assert_eq!(selected(&prepared, start, end), "x|y");
    assert_eq!(selected(&prepared, end, start), "x|y");
    assert_eq!(selected(&prepared, end, end), "x");
}

#[test]
fn table_copy_narrow_labels_and_padding_are_never_source_content() {
    let source = "| First long header | Other long header |\n| --- | --- |\n| value abc | second |\n| third | fourth |";
    for width in [2, 5, 9, 12, 26, 80] {
        let prepared = prepare(&[source], width);
        assert_eq!(
            all(&prepared),
            "First long header|Other long header\nvalue abc|second\nthird|fourth",
            "width {width}"
        );
        assert_eq!(all(&prepared).matches("First long header").count(), 1);
        for (row, fragment) in fragments(&prepared) {
            if fragment.text.starts_with('v') {
                let point = (row, fragment.columns.start);
                assert_eq!(selected(&prepared, point, point), "v");
            }
        }
    }
}

#[test]
fn table_copy_preserves_literal_pipes_and_unicode_glyphs() {
    let source = "| A | B |\n| --- | --- |\n| a\\|b │ 中🙂e\u{301}界 | `c\\|d` |";
    for width in [5, 9, 12, 26, 80] {
        let prepared = prepare(&[source], width);
        assert_eq!(
            all(&prepared),
            "A|B\na|b │ 中🙂e\u{301}界|c|d",
            "width {width}"
        );
        for (row, fragment) in fragments(&prepared) {
            let mut column = fragment.columns.start;
            for glyph in fragment.text.graphemes(true) {
                let width = UnicodeWidthStr::width(glyph);
                if width > 0 {
                    let point = (row, column);
                    assert_eq!(selected(&prepared, point, point), glyph);
                }
                column += width;
            }
        }
    }
}

#[test]
fn table_copy_respects_source_rows_and_section_boundaries() {
    let prepared = prepare(&[WRAPPED, WRAPPED], 26);
    assert_eq!(all(&prepared), "A|B\nxxxxxxxxxxxx|y\nA|B\nxxxxxxxxxxxx|y");
    let source = "before\n\n| A | B |\n| --- | --- |\n| xxxxxxxxxxxx | y |\n\nafter";
    for width in [9, 26, 80] {
        assert_eq!(
            all(&prepare(&[source], width)),
            "before\n\nA|B\nxxxxxxxxxxxx|y\n\nafter"
        );
    }
}

#[test]
fn table_copy_nested_list_and_assistant_insets_shift_every_fragment() {
    let source = "- before\n\n  | A | B |\n  | --- | --- |\n  | xxxxxxxxxxxx | y |\n\n  after";
    for width in [12, 30, 80] {
        let prepared = prepare(&[source], width);
        for (row, fragment) in fragments(&prepared) {
            assert!(fragment.columns.start >= 3, "list plus assistant inset");
            assert_eq!(
                selected(
                    &prepared,
                    (row, fragment.columns.start),
                    (row, fragment.columns.end - 1)
                ),
                fragment.text
            );
        }
        assert!(all(&prepared).contains("A|B\nxxxxxxxxxxxx|y"));
    }
}

#[test]
fn table_copy_raw_cell_ids_survive_escaping_resize_and_stream_growth() {
    let incomplete = "控制\u{1b}prefix\n\n| A | B |\n| --- | --";
    // No table exists until its delimiter row has the required final column.
    let partial = prepare(&[incomplete.trim_end_matches('-')], 26);
    assert!(fragments(&partial).is_empty());
    let formed = format!("{incomplete}- |\n| xxxxxxxxxxxx | y");
    let extended = format!("{formed} |\n| next | row |\n");
    let row = formed.find("| A").unwrap();
    let first_cell = formed.find(" A ").unwrap();
    let second_cell = formed.find(" B ").unwrap();
    let body = formed.find("| x").unwrap();
    let cell_ids = |prepared: &PreparedConversation| {
        let mut ids = fragments(prepared)
            .into_iter()
            .map(|(_, fragment)| {
                assert!(extended.is_char_boundary(fragment.row_offset));
                assert!(extended.is_char_boundary(fragment.cell_offset));
                if let Some(separator) = fragment.separator_before {
                    assert_eq!(extended.as_bytes()[separator], b'|');
                }
                (
                    fragment.row_offset,
                    fragment.cell_offset,
                    fragment.cell_index,
                )
            })
            .filter(|(offset, _, _)| *offset <= body)
            .collect::<Vec<_>>();
        ids.sort_unstable();
        ids.dedup();
        ids
    };
    let expected = cell_ids(&prepare(&[&formed], 26));
    assert!(expected.contains(&(row, first_cell, 0)));
    assert!(expected.contains(&(row, second_cell, 1)));
    for width in [5, 9, 12, 26, 80] {
        assert_eq!(cell_ids(&prepare(&[&formed], width)), expected);
        assert_eq!(cell_ids(&prepare(&[&extended], width)), expected);
    }
}

#[test]
fn table_copy_fragment_storage_is_linear_and_excludes_repeated_labels() {
    let value = "abcdefghij".repeat(100);
    let source =
        format!("| Long header one | Long header two |\n| --- | --- |\n| {value} | tail |");
    let expected_bytes = "Long header oneLong header two".len() + value.len() + "tail".len();
    for width in [5, 9, 26, 80] {
        let prepared = prepare(&[&source], width);
        let fragments = fragments(&prepared);
        assert_eq!(
            fragments
                .iter()
                .map(|(_, fragment)| fragment.text.len())
                .sum::<usize>(),
            expected_bytes
        );
        assert!(fragments.len() <= expected_bytes);
        assert_eq!(
            all(&prepared),
            format!("Long header one|Long header two\n{value}|tail")
        );
    }
}

#[test]
fn table_copy_fragment_allocations_are_charged_once_to_layout_budget() {
    let prepared = prepare(&[WRAPPED], 26);
    let section = prepared.sections.first().unwrap();
    let mut copies = prepared.copy_ranges.live.as_ref().clone();
    // The estimator must count String capacity, rather than only logical bytes.
    let owned = copies
        .iter_mut()
        .find(|copy| copy.table_fragments.is_some())
        .unwrap();
    Arc::make_mut(owned.table_fragments.as_mut().unwrap())[0]
        .text
        .reserve(4096);
    let expected = copies
        .iter()
        .filter_map(|copy| copy.table_fragments.as_ref())
        .map(|fragments| {
            std::mem::size_of_val(fragments.as_ref())
                + 2 * std::mem::size_of::<usize>()
                + fragments
                    .iter()
                    .map(|fragment| fragment.text.capacity())
                    .sum::<usize>()
        })
        .sum::<usize>();
    let duplicate = copies
        .iter()
        .find(|copy| copy.table_fragments.is_some())
        .unwrap()
        .clone();
    let mut layout = SectionLayout {
        key: LayoutKey {
            section: section.id,
            revision: 0,
            width: 26,
            theme: crate::theme::ThemeKind::Dark,
            folded: false,
            reasoning_visible: true,
        },
        order: 0,
        rows: Arc::new(prepared.live),
        source: Arc::from(WRAPPED),
        source_map: Arc::new(SourceMap {
            source: Arc::from(WRAPPED),
            rows: Arc::new(Vec::new()),
        }),
        copy_ranges: Arc::new(copies),
        link_cells: Arc::new(Vec::new()),
        content_columns: 1..26,
        collapsible: false,
        folded: false,
    };
    let before = layout.retained_bytes();
    let duplicate_text = duplicate.text().len();
    Arc::make_mut(&mut layout.copy_ranges).push(duplicate);
    assert_eq!(
        layout.retained_bytes(),
        before + duplicate_text,
        "another reference must not double-charge the fragment allocation"
    );
    for copy in Arc::make_mut(&mut layout.copy_ranges) {
        copy.table_fragments = None;
    }
    assert_eq!(before + duplicate_text - layout.retained_bytes(), expected);
}

#[test]
fn table_copy_preserves_empty_middle_edge_and_whole_rows() {
    let source = "| A | B | C |\n| --- | --- | --- |\n| x | | z |\n| | y | |\n| | | |\n| end | | |";
    for width in [5, 9, 26, 80] {
        let prepared = prepare(&[source], width);
        assert_eq!(
            all(&prepared),
            "A|B|C\nx||z\n|y|\n||\nend||",
            "width {width}"
        );
        for (row, fragment) in fragments(&prepared) {
            if fragment.text.is_empty() {
                // An empty place marker never turns a one-glyph selection of
                // padding or a repeated label into other source-cell content.
                let point = (row, fragment.columns.start);
                assert_eq!(selected(&prepared, point, point), "");
            }
        }
    }
}
