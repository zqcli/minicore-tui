//! The `/diff` Git diff workspace: a compact 1-line searchable file list beside
//! the scrollable hunks of the selected file, built on the shared [`super::split`]
//! two-pane engine so its layout, focus model and mouse behavior match `Ctrl+B`.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::app::diff::{DiffFile, DiffRow, DiffState, sign};
use minicore_host_workspace::{ChangeKind, DiffLineKind as LineKind};

use super::chrome as draw_chrome;
use super::split::{self, PaneFocus};
use crate::ui::blocks::plural;
use crate::ui::chrome;
use crate::ui::panel::truncate;
use crate::ui::theme::Styles;
use crate::wrap::{width, wrap};

/// Width in cells of the searchable file list beside the hunks.
const FILE_COLUMN: u16 = 36;

impl DiffRow {
    fn line(&self, styles: Styles) -> Line<'static> {
        match self {
            Self::Blank => Line::from(""),
            Self::Note(note) => chrome::styled(note, styles.dim()),
            Self::Hunk(header) => chrome::styled(header, styles.hunk()),
            Self::Line { kind, gutter, text } => {
                let style = match kind {
                    LineKind::Added => styles.added(),
                    LineKind::Removed => styles.removed(),
                    LineKind::Context => styles.dim(),
                };
                Line::from(vec![
                    Span::styled(gutter.clone(), styles.faint()),
                    Span::styled(format!("{}{text}", sign(*kind)), style),
                ])
            }
        }
    }
}

pub fn render(frame: &mut Frame, area: Rect, styles: Styles, diff: &mut DiffState) {
    diff.hit.reset(1);
    diff.visible_rows = 0;

    let matched = diff.matching_indices();
    let title = {
        let mut parts = vec!["Git diff".to_owned()];
        if !diff.filter.trim().is_empty() {
            parts.push(format!("showing {}/{}", matched.len(), diff.files.len()));
        } else if !diff.files.is_empty() {
            parts.push(plural(diff.files.len() as u64, "file"));
        }
        let (added, removed) = diff.total_counts();
        if added > 0 || removed > 0 {
            parts.push(format!("+{added} −{removed}"));
        }
        parts.join(" · ")
    };

    let hints: Vec<(&str, &str)> = match diff.focus {
        PaneFocus::Detail => vec![
            ("↑↓/jk", "scroll"),
            ("pgup/pgdn", "page"),
            ("r", "refresh"),
            ("c", "copy"),
            ("←/esc", "files"),
        ],
        PaneFocus::List => {
            let mut h = vec![
                ("type", "filter"),
                ("↑↓", "file"),
                ("→/tab", "scroll"),
                ("ctrl+r", "refresh"),
                ("ctrl+y", "copy"),
            ];
            if !diff.filter.is_empty() {
                h.push(("ctrl+u", "clear"));
            }
            h.push(("esc", "back"));
            h
        }
    };
    draw_chrome(frame, area, styles, &title, &hints);

    let body = Rect {
        y: area.y + 1,
        height: area.height.saturating_sub(2),
        ..area
    };
    let panes = split::layout_panes(
        frame,
        body,
        FILE_COLUMN,
        diff.detail_open || diff.focus == PaneFocus::Detail,
        diff.focus == PaneFocus::Detail,
        styles,
    );

    if let Some(list_area) = panes.list {
        diff.hit.list_area = list_area;
        diff.hit.list_first = render_file_list(
            frame,
            list_area,
            styles,
            diff,
            &matched,
            diff.focus == PaneFocus::List,
        );
    }

    if let Some(detail_area) = panes.detail {
        diff.hit.detail_area = detail_area;
        render_hunks(frame, detail_area, styles, diff);
    }
}

/// The file's line counts: `+a −b` once measured, `?` on failure, `·` while loading.
fn count_spans(file: &DiffFile, styles: Styles, selected: bool) -> Vec<Span<'static>> {
    match file.lines {
        Some((added, removed)) => vec![
            Span::styled(format!("+{added}"), styles.added()),
            Span::raw(" "),
            Span::styled(format!("−{removed}"), styles.removed()),
        ],
        None if file.failed => vec![Span::styled(
            "?",
            if selected {
                styles.text()
            } else {
                styles.dim()
            },
        )],
        None => vec![Span::styled(
            "·",
            if selected {
                styles.text()
            } else {
                styles.dim()
            },
        )],
    }
}

fn marker(kind: ChangeKind) -> &'static str {
    match kind {
        ChangeKind::Added => "A",
        ChangeKind::Deleted => "D",
        ChangeKind::Renamed => "R",
        ChangeKind::Conflict => "!",
        ChangeKind::Modified => "M",
        ChangeKind::Unchanged => "=",
        ChangeKind::Unknown => "?",
    }
}

fn marker_style(kind: ChangeKind, styles: Styles, selected: bool) -> Style {
    match kind {
        ChangeKind::Added => styles.added(),
        ChangeKind::Deleted => styles.removed(),
        ChangeKind::Conflict => styles.warning(),
        ChangeKind::Renamed | ChangeKind::Modified => {
            if selected {
                styles.accent()
            } else {
                styles.text()
            }
        }
        _ => {
            if selected {
                styles.text()
            } else {
                styles.dim()
            }
        }
    }
}

fn render_file_list(
    frame: &mut Frame,
    area: Rect,
    styles: Styles,
    diff: &DiffState,
    matched: &[usize],
    focused: bool,
) -> usize {
    let placeholder = if focused {
        "type to filter files…"
    } else {
        "esc to search files…"
    };
    let list_body =
        chrome::draw_filter_header(frame, area, &diff.filter, placeholder, focused, styles);
    if list_body.is_empty() {
        return 0;
    }
    let buf = frame.buffer_mut();
    if matched.is_empty() {
        let message = if diff.files.is_empty() {
            if diff.status.is_empty() {
                "No changed files"
            } else {
                diff.status.as_str()
            }
        } else {
            "No matching files · ctrl+u clears filter"
        };
        buf.set_line(
            list_body.x,
            list_body.y,
            &chrome::styled(
                &truncate(message, usize::from(list_body.width)),
                styles.dim(),
            ),
            list_body.width,
        );
        return 0;
    }

    let (first, visible_slots) = split::list_window(
        diff.selected,
        matched.len(),
        usize::from(list_body.height),
        1,
    );
    let selected = diff.selected.min(matched.len().saturating_sub(1));
    let total_w = usize::from(list_body.width);
    let span_width =
        |spans: &[Span<'static>]| spans.iter().map(|s| width(&s.content)).sum::<usize>();

    for (slot, (filtered_idx, &real_idx)) in matched
        .iter()
        .enumerate()
        .skip(first)
        .take(visible_slots)
        .enumerate()
    {
        let y = list_body.y + slot as u16;
        if y >= list_body.bottom() {
            break;
        }
        let file = &diff.files[real_idx];
        let is_selected = filtered_idx == selected;
        if is_selected {
            chrome::paint_highlight_band(
                buf,
                Rect {
                    x: list_body.x,
                    y,
                    width: list_body.width,
                    height: 1,
                },
                styles,
            );
        }
        let prefix = if is_selected { "▎" } else { " " };
        let prefix_style = if is_selected {
            styles.accent()
        } else {
            Style::default()
        };
        let path_style = if is_selected {
            styles.bold()
        } else {
            styles.text()
        };
        let counts = count_spans(file, styles, is_selected);
        let counts_w = span_width(&counts);
        // Layout: `▎ M path          +a −r`
        let fixed = 1 + 1 + 1 + 1 + 1 + counts_w;
        let path_room = total_w.saturating_sub(fixed).max(4);
        let path = truncate(&file.path, path_room);
        let used = 4 + width(&path);
        let gap = total_w.saturating_sub(used + counts_w).max(1);

        let mut spans = vec![
            Span::styled(prefix, prefix_style),
            Span::raw(" "),
            Span::styled(
                marker(file.kind),
                marker_style(file.kind, styles, is_selected),
            ),
            Span::raw(" "),
            Span::styled(path, path_style),
            Span::raw(" ".repeat(gap)),
        ];
        spans.extend(counts);
        buf.set_line(list_body.x, y, &Line::from(spans), list_body.width);
    }
    first
}

fn render_hunks(frame: &mut Frame, area: Rect, styles: Styles, diff: &mut DiffState) {
    if area.is_empty() {
        return;
    }
    let header = match diff.current() {
        Some(file) => {
            let mut spans = vec![
                Span::styled(
                    format!("{} ", marker(file.kind)),
                    marker_style(file.kind, styles, true),
                ),
                Span::styled(file.path.clone(), styles.bold()),
            ];
            if file.lines.is_some() || file.failed {
                spans.push(Span::styled(" · ", styles.meta()));
                spans.extend(count_spans(file, styles, false));
            }
            if diff.focus == PaneFocus::Detail {
                spans.push(Span::styled(" · scrolling", styles.accent()));
            }
            Line::from(spans)
        }
        None => chrome::styled("No file selected", styles.dim()),
    };
    let content = split::draw_detail_header(
        frame,
        area,
        &header,
        diff.focus == PaneFocus::Detail,
        styles,
    );
    diff.visible_rows = usize::from(content.height);
    if content.is_empty() {
        return;
    }

    if diff.rows.is_empty() {
        let message = if diff.status.is_empty() {
            "No changes to show."
        } else {
            diff.status.as_str()
        };
        frame.buffer_mut().set_line(
            content.x,
            content.y,
            &chrome::styled(message, styles.dim()),
            content.width,
        );
        return;
    }

    // Wrap each diff row preserving its line-number gutter indent, reserving
    // room for the right-edge scrollbar when lines exceed the viewport.
    let wrap_w = usize::from(content.width.saturating_sub(2).max(1));
    let mut wrapped: Vec<Line<'static>> = Vec::with_capacity(diff.rows.len());
    for row in &diff.rows {
        let line = row.line(styles);
        let Some((body, gutter)) = line.spans.split_last() else {
            wrapped.push(Line::default());
            continue;
        };
        let indent: usize = gutter.iter().map(|span| width(&span.content)).sum();
        let room = wrap_w.saturating_sub(indent).max(1);
        for (index, chunk) in wrap(&body.content, room).into_iter().enumerate() {
            let mut spans = if index == 0 {
                gutter.to_vec()
            } else {
                vec![Span::raw(" ".repeat(indent))]
            };
            spans.push(Span::styled(chunk, body.style));
            wrapped.push(Line::from(spans));
        }
    }
    diff.scroll = chrome::render_scrollable_lines(frame, content, &wrapped, diff.scroll, styles);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::diff::{hunk_rows, rows, text};
    use minicore_host_workspace::DiffResult;
    use serde_json::json;

    use super::super::capture;
    use crate::ui::theme::Palette;

    fn diff_fixture(mut value: serde_json::Value) -> DiffResult {
        value["change_ref"] = json!({"kind":"tool","session_id":"00000000000000000001","index":0,
            "tool_call":{"turn_id":"00000000000000000001","tool_call_id":"write"}});
        value["comparison"] = json!("tool_before_after");
        value["origin"] = json!("tool");
        value["versions_refreshed"] = json!(false);
        value["next_cursor"] = serde_json::Value::Null;
        value.as_object_mut().unwrap().remove("path");
        for hunk in value["hunks"].as_array_mut().unwrap() {
            let mut offset = 0;
            for line in hunk["lines"].as_array_mut().unwrap() {
                line["line_byte_offset"] = json!(offset);
                offset = if line["line_complete"] == true {
                    0
                } else {
                    offset + line["text"].as_str().unwrap().len()
                };
            }
        }
        serde_json::from_value(value).unwrap()
    }

    fn page() -> DiffResult {
        diff_fixture(json!({
            "binary": false, "stale": false, "truncated": false,
            "path": "src/main.rs", "complete": true,
            "hunks": [{"old_start": 10, "old_count": 2, "new_start": 10, "new_count": 3,
                       "lines": [
                         {"kind": "context", "old_index": 10, "new_index": 10,
                          "text": " fn main() {", "line_complete": true},
                         {"kind": "removed", "old_index": 11, "text": "  old();",
                          "line_complete": true},
                         {"kind": "added", "new_index": 11, "text": "  new();",
                          "line_complete": true},
                       ]}]
        }))
    }

    fn hunk_lines(page: &DiffResult, styles: Styles) -> Vec<Line<'static>> {
        hunk_rows(page).iter().map(|row| row.line(styles)).collect()
    }

    fn records_lines(
        pages: &[DiffResult],
        tools: &[Option<String>],
        styles: Styles,
    ) -> Vec<Line<'static>> {
        rows(pages, tools)
            .iter()
            .map(|row| row.line(styles))
            .collect()
    }

    fn row_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn hunk_lines_keep_the_diff_shape_and_colors() {
        let palette = Palette::dark();
        let lines = hunk_lines(&page(), Styles::of(palette));
        let text: Vec<String> = lines.iter().map(row_text).collect();
        assert_eq!(
            text,
            [
                "@@ -10,2 +10,3 @@",
                "10 10   fn main() {",
                "11    -  old();",
                "   11 +  new();"
            ]
        );
        assert_eq!(lines[0].spans[0].style.fg, Some(palette.hunk));
        assert_eq!(lines[1].spans[0].style.fg, Some(palette.faint), "gutter");
        assert_eq!(lines[2].spans[1].style.fg, Some(palette.removed));
        assert_eq!(lines[3].spans[1].style.fg, Some(palette.added));
    }

    #[test]
    fn hunk_headers_use_unified_one_based_numbers() {
        let styles = Styles::of(Palette::dark());
        let page: DiffResult = diff_fixture(json!({
            "binary": false, "stale": false, "truncated": false,
            "path": "a.py", "complete": true,
            "hunks": [
                {"old_start": 1, "old_count": 2, "new_start": 1, "new_count": 2, "lines": []},
                {"old_start": 5, "old_count": 0, "new_start": 6, "new_count": 1, "lines": []}
            ]
        }));
        let text: Vec<String> = hunk_lines(&page, styles).iter().map(row_text).collect();
        assert_eq!(text, ["@@ -1,2 +1,2 @@", "@@ -5,0 +6,1 @@"]);
    }

    #[test]
    fn a_line_split_into_fragments_is_one_row() {
        let styles = Styles::of(Palette::dark());
        let page: DiffResult = diff_fixture(json!({
            "binary": false, "stale": false, "truncated": false,
            "path": "a.py", "complete": true,
            "hunks": [{"old_start": 0, "old_count": 0, "new_start": 1, "new_count": 1,
                       "lines": [
                         {"kind": "added", "new_index": 1, "text": "long ", "line_complete": false},
                         {"kind": "added", "new_index": 1, "text": "line\n", "line_complete": true}
                       ]}]
        }));
        let text: Vec<String> = hunk_lines(&page, styles).iter().map(row_text).collect();
        assert_eq!(text[1..], ["  1 +long line"]);
    }

    #[test]
    fn several_records_of_one_file_are_separated_and_named() {
        let styles = Styles::of(Palette::dark());
        let pages = [page(), page(), page()];
        let tools = [Some("write".to_owned()), Some("edit".to_owned()), None];
        let text: Vec<String> = records_lines(&pages, &tools, styles)
            .iter()
            .map(row_text)
            .collect();
        let separators: Vec<&String> = text.iter().filter(|row| row.starts_with("──")).collect();
        assert_eq!(
            separators,
            [
                "── write · change 1 of 3 ──",
                "── edit · change 2 of 3 ──",
                "── change 3 of 3 ──"
            ]
        );
        let single: Vec<String> = records_lines(&pages[..1], &tools[..1], styles)
            .iter()
            .map(row_text)
            .collect();
        assert!(
            !single.iter().any(|row| row.starts_with("──")),
            "{single:?}"
        );
    }

    #[test]
    fn diff_text_never_leaks_the_original_line_terminator() {
        let styles = Styles::of(Palette::dark());
        let page: DiffResult = diff_fixture(json!({
            "binary": false, "stale": false, "truncated": false,
            "path": "a.rs", "complete": true,
            "hunks": [{"old_start": 1, "old_count": 0, "new_start": 1, "new_count": 2,
                       "lines": [
                         {"kind": "added", "text": "added();\n", "line_complete": true},
                         {"kind": "removed", "text": "gone();\r\n", "line_complete": true},
                       ]}]
        }));
        let lines = hunk_lines(&page, styles);
        let text: Vec<String> = lines
            .iter()
            .map(|line| line.spans.last().unwrap().content.to_string())
            .collect();
        assert_eq!(text[1], "+added();");
        assert_eq!(text[2], "-gone();");
        assert!(!text[1].contains('\n'), "{text:?}");
    }

    #[test]
    fn an_empty_page_explains_itself_instead_of_drawing_nothing() {
        let styles = Styles::of(Palette::dark());
        let page = |binary: bool| -> DiffResult {
            diff_fixture(json!({
                "hunks": [], "complete": false, "binary": binary, "stale": false, "truncated": false
            }))
        };
        let (empty, binary) = (page(false), page(true));
        assert_eq!(hunk_lines(&empty, styles).len(), 1);
        let text: String = hunk_lines(&binary, styles)[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(text.contains("Binary"), "{text}");
    }

    fn view() -> DiffState {
        let mut view = DiffState::default();
        view.set_files(vec![
            DiffFile {
                path: "a.rs".into(),
                kind: ChangeKind::Modified,
                lines: Some((3, 1)),
                failed: false,
                change_refs: Vec::new(),
            },
            DiffFile {
                path: "b.rs".into(),
                kind: ChangeKind::Added,
                lines: Some((9, 0)),
                failed: false,
                change_refs: Vec::new(),
            },
        ]);
        view
    }

    #[test]
    fn the_diff_view_shows_a_searchable_file_column_and_the_hunks() {
        let styles = Styles::of(Palette::dark());
        let mut view = view();
        view.rows = hunk_rows(&page());
        let text = capture(
            |frame| render(frame, frame.area(), styles, &mut view),
            110,
            20,
        );
        assert!(
            text[0].contains("Git diff · 2 files · +12 −1"),
            "{}",
            text[0]
        );
        assert!(
            text.iter()
                .any(|row| row.contains("/ type to filter files")),
            "{text:?}"
        );
        assert!(
            text.iter()
                .any(|row| row.contains("▎ M a.rs") && row.contains("+3 −1")),
            "{text:?}"
        );
        assert!(
            text.iter()
                .any(|row| row.contains("A b.rs") && row.contains("+9 −0")),
            "{text:?}"
        );
        assert!(
            text.iter().any(|row| row.contains("@@ -10,2 +10,3 @@")),
            "{text:?}"
        );
        assert!(
            text.iter().any(|row| row.contains("11    -  old();")),
            "{text:?}"
        );
        assert!(text.last().unwrap().contains("esc back"), "{text:?}");
    }

    #[test]
    fn an_unmeasured_file_row_shows_a_dot_instead_of_a_number() {
        let styles = Styles::of(Palette::dark());
        let mut view = view();
        view.files[1].lines = None;
        let text = capture(
            |frame| render(frame, frame.area(), styles, &mut view),
            110,
            20,
        );
        assert!(
            text.iter()
                .any(|row| row.contains("A b.rs") && row.contains(" ·")),
            "{text:?}"
        );
    }

    #[test]
    fn a_narrow_terminal_toggles_between_file_list_and_hunks() {
        let styles = Styles::of(Palette::dark());
        let mut view = view();
        view.rows = hunk_rows(&page());
        let list_only = capture(
            |frame| render(frame, frame.area(), styles, &mut view),
            60,
            16,
        );
        assert!(
            list_only.iter().any(|row| row.contains("▎ M a.rs")),
            "{list_only:?}"
        );
        assert!(
            !list_only.iter().any(|row| row.contains("+  new();")),
            "{list_only:?}"
        );

        view.detail_open = true;
        let hunks_only = capture(
            |frame| render(frame, frame.area(), styles, &mut view),
            60,
            16,
        );
        assert!(
            hunks_only.iter().any(|row| row.contains("+  new();")),
            "{hunks_only:?}"
        );
    }

    #[test]
    fn copied_diff_text_is_the_path_plus_the_hunks() {
        let mut view = view();
        view.rows = hunk_rows(&page());
        let text = text(&view);
        assert!(text.starts_with("a.rs\n"), "{text:?}");
        assert!(text.contains("+  new();"), "{text:?}");
    }

    #[test]
    fn long_diff_lines_wrap_instead_of_overflowing() {
        let styles = Styles::of(Palette::dark());
        let mut view = view();
        view.detail_open = true;
        view.rows = vec![DiffRow::Line {
            kind: LineKind::Added,
            gutter: String::new(),
            text: "x".repeat(120),
        }];
        let text = capture(
            |frame| render(frame, frame.area(), styles, &mut view),
            60,
            12,
        );
        let x_rows = text.iter().filter(|row| row.contains('x')).count();
        assert!(x_rows >= 2, "{text:?}");
        for row in &text {
            assert!(width(row) <= 60);
        }
    }
}
