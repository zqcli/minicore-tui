//! The diff view: a file list beside the hunks of the selected file.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use crate::app::diff::{DiffFile, DiffState};
use crate::protocol::ChangeKind;
use crate::protocol::changes::{DiffLine, DiffPage, LineKind};

use super::{chrome, styled};
use crate::ui::panel::truncate;
use crate::ui::theme::Styles;
use crate::ui::wrap::{sanitize, width, wrap};

/// File-list column width, as a fraction of the view when the terminal is wide
/// enough for a side-by-side layout.
const FILE_COLUMN: u16 = 30;
const MIN_SPLIT_WIDTH: u16 = 80;
const MIN_SPLIT_HEIGHT: u16 = 12;

/// One row of a file's diff, before it is styled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiffRow {
    Blank,
    /// Dim text: the separator between records, or why there is no diff.
    Note(String),
    /// `@@ -a,b +c,d @@`
    Hunk(String),
    /// A diff line: the dim `old new` line-number gutter, then the text.
    Line {
        kind: LineKind,
        gutter: String,
        text: String,
    },
}

impl DiffRow {
    fn line(&self, styles: Styles) -> Line<'static> {
        match self {
            Self::Blank => Line::from(""),
            Self::Note(note) => styled(note, styles.dim()),
            Self::Hunk(header) => styled(header, styles.hunk()),
            Self::Line { kind, gutter, text } => {
                let style = match kind {
                    LineKind::Added => styles.added(),
                    LineKind::Removed => styles.removed(),
                    LineKind::Context | LineKind::Unknown => styles.dim(),
                };
                Line::from(vec![
                    Span::styled(gutter.clone(), styles.faint()),
                    Span::styled(format!("{}{text}", sign(*kind)), style),
                ])
            }
        }
    }

    /// The row as `c` copies it: without the line-number gutter.
    fn body(&self) -> String {
        match self {
            Self::Blank => String::new(),
            Self::Note(text) | Self::Hunk(text) => text.clone(),
            Self::Line { kind, text, .. } => format!("{}{text}", sign(*kind)),
        }
    }
}

fn sign(kind: LineKind) -> &'static str {
    match kind {
        LineKind::Added => "+",
        LineKind::Removed => "-",
        LineKind::Context | LineKind::Unknown => " ",
    }
}

/// The text of the diff view: the file's path, then its rows.
pub fn text(diff: &DiffState) -> String {
    let mut out = String::new();
    if let Some(file) = diff.current() {
        out.push_str(&file.path);
        out.push('\n');
    }
    for row in &diff.rows {
        out.push_str(&row.body());
        out.push('\n');
    }
    out
}

/// Rows for every change record of one file; each record is its own
/// comparison, so several are told apart by a separator naming the tool.
pub fn rows(pages: &[DiffPage], tools: &[Option<String>]) -> Vec<DiffRow> {
    if pages.len() <= 1 {
        return match pages.first() {
            Some(page) => hunk_rows(page),
            None => vec![DiffRow::Note("No differences.".into())],
        };
    }
    let mut rows = Vec::new();
    for (index, page) in pages.iter().enumerate() {
        let position = format!("change {} of {}", index + 1, pages.len());
        let label = match tools.get(index).and_then(Option::as_deref) {
            Some(tool) => format!("── {tool} · {position} ──"),
            None => format!("── {position} ──"),
        };
        if index > 0 {
            rows.push(DiffRow::Blank);
        }
        rows.push(DiffRow::Note(label));
        rows.extend(hunk_rows(page));
    }
    rows
}

/// Turns one `changes.diff` page into rows. Header numbers are unified-diff
/// 1-based; the Agent's starts are 0-based line indexes.
fn hunk_rows(page: &DiffPage) -> Vec<DiffRow> {
    if page.binary {
        return vec![DiffRow::Note("Binary file: the diff is not shown.".into())];
    }
    if page.hunks.is_empty() {
        let note = match page.availability.as_deref() {
            Some("unavailable") => "The original content is unavailable for this change.",
            Some("partial") => "Only part of this diff is available.",
            _ => "No differences.",
        };
        return vec![DiffRow::Note(note.into())];
    }
    let digits = page
        .hunks
        .iter()
        .flat_map(|hunk| {
            [
                hunk.old_start + hunk.old_count,
                hunk.new_start + hunk.new_count,
            ]
        })
        .max()
        .unwrap_or(0)
        .to_string()
        .len();
    let mut rows = Vec::new();
    for hunk in &page.hunks {
        rows.push(DiffRow::Hunk(format!(
            "@@ -{},{} +{},{} @@",
            unified_start(hunk.old_start, hunk.old_count),
            hunk.old_count,
            unified_start(hunk.new_start, hunk.new_count),
            hunk.new_count
        )));
        // A long line arrives as several fragments; only the last one is
        // `line_complete`. They are joined back into one row.
        let mut pending: Option<(&DiffLine, String)> = None;
        for line in &hunk.lines {
            let (first, text) = match pending.take() {
                Some((first, mut text)) => {
                    text.push_str(&line.text);
                    (first, text)
                }
                None => (line, line.text.clone()),
            };
            if line.line_complete {
                rows.push(diff_row(first, &text, digits));
            } else {
                pending = Some((first, text));
            }
        }
        if let Some((first, text)) = pending {
            rows.push(diff_row(first, &text, digits));
        }
    }
    if !page.complete {
        rows.push(DiffRow::Note("… the diff continues".into()));
    }
    rows
}

/// A hunk start as unified diff prints it: the first line's 1-based number, or
/// the line before an empty range.
fn unified_start(start: u64, count: u64) -> u64 {
    if count > 0 { start + 1 } else { start }
}

fn diff_row(line: &DiffLine, text: &str, digits: usize) -> DiffRow {
    let number = |index: Option<u64>| index.map_or(String::new(), |index| (index + 1).to_string());
    // Diff text carries the original line terminator; the row already ends.
    DiffRow::Line {
        kind: line.kind,
        gutter: format!(
            "{:>digits$} {:>digits$} ",
            number(line.old_index),
            number(line.new_index)
        ),
        text: sanitize(text.trim_end_matches(['\n', '\r'])).into_owned(),
    }
}

/// Where the file list and the hunk pane go.
fn split(area: Rect) -> (Option<Rect>, Rect) {
    let body = Rect {
        y: area.y + 1,
        height: area.height.saturating_sub(2),
        ..area
    };
    if area.width < MIN_SPLIT_WIDTH || body.height < MIN_SPLIT_HEIGHT {
        return (None, body);
    }
    let list = Rect {
        width: FILE_COLUMN,
        ..body
    };
    let hunks = Rect {
        x: body.x + FILE_COLUMN + 1,
        width: body.width.saturating_sub(FILE_COLUMN + 1),
        ..body
    };
    (Some(list), hunks)
}

pub fn render(frame: &mut Frame, area: Rect, styles: Styles, diff: &DiffState) {
    let mut title = format!("← Diff · {}", diff.scope.label());
    if let Some(file) = diff.current() {
        title.push_str(" · ");
        title.push_str(&file.path);
    }
    chrome(
        frame,
        area,
        styles,
        &title,
        "tab scope · ↑↓ file · PgUp/PgDn scroll · c copy · esc back",
    );
    let (list, hunks) = split(area);
    if let Some(list) = list {
        render_file_list(frame, list, styles, diff);
    }
    render_hunks(frame, hunks, styles, diff);
}

/// The file's line column: `+a −b` once measured, `?` when the read failed and
/// `·` while unknown.
fn count_spans(file: &DiffFile, styles: Styles) -> Vec<Span<'static>> {
    match file.lines {
        Some((added, removed)) => vec![
            Span::styled(format!("+{added}"), styles.added()),
            Span::raw(" "),
            Span::styled(format!("−{removed}"), styles.removed()),
        ],
        None if file.failed => vec![Span::styled("?", styles.dim())],
        None => vec![Span::styled("·", styles.dim())],
    }
}

fn marker(kind: ChangeKind) -> &'static str {
    match kind {
        ChangeKind::Added => "A",
        ChangeKind::Deleted => "D",
        ChangeKind::Renamed => "R",
        ChangeKind::Conflict => "!",
        ChangeKind::Modified | ChangeKind::Unknown => "M",
    }
}

fn render_file_list(frame: &mut Frame, area: Rect, styles: Styles, diff: &DiffState) {
    let rows = area.height as usize;
    let first = diff.selected.saturating_sub(rows.saturating_sub(1));
    let counts: Vec<Vec<Span<'static>>> = diff
        .files
        .iter()
        .map(|file| count_spans(file, styles))
        .collect();
    let span_width = |spans: &[Span<'static>]| spans.iter().map(|span| width(&span.content)).sum();
    let column = counts
        .iter()
        .map(|spans| span_width(spans))
        .max()
        .unwrap_or(1);
    for (row, (index, file)) in diff
        .files
        .iter()
        .enumerate()
        .skip(first)
        .take(rows)
        .enumerate()
    {
        let y = area.y + row as u16;
        let style = if index == diff.selected {
            let band = Rect {
                y,
                height: 1,
                ..area
            };
            frame.buffer_mut().set_style(band, styles.highlight());
            styles.bold()
        } else {
            styles.dim()
        };
        let count = &counts[index];
        let mut spans = vec![Span::styled(format!("{} ", marker(file.kind)), style)];
        spans.extend(count.iter().cloned());
        let path_width = (area.width as usize).saturating_sub(column + 3);
        spans.push(Span::raw(" ".repeat(column - span_width(count) + 1)));
        spans.push(Span::styled(truncate(&file.path, path_width), style));
        frame
            .buffer_mut()
            .set_line(area.x, y, &Line::from(spans), area.width);
    }
    if let Some(file) = diff.current()
        && area.height > 1
    {
        let counts = match file.lines {
            Some((added, removed)) => Line::from(vec![
                Span::styled(format!("+{added} "), styles.added()),
                Span::styled(format!("−{removed}"), styles.removed()),
            ]),
            None if file.failed => styled("line counts unavailable", styles.dim()),
            None => styled("reading…", styles.dim()),
        };
        frame
            .buffer_mut()
            .set_line(area.x, area.bottom() - 1, &counts, area.width);
    }
}

fn render_hunks(frame: &mut Frame, area: Rect, styles: Styles, diff: &DiffState) {
    if diff.rows.is_empty() {
        let message = if diff.status.is_empty() {
            "No changes to show."
        } else {
            diff.status.as_str()
        };
        frame
            .buffer_mut()
            .set_line(area.x, area.y, &styled(message, styles.dim()), area.width);
        return;
    }
    // The last span is the content and wraps; earlier spans are the gutter,
    // drawn on the first row and left blank on continuation rows.
    let mut y = area.y;
    for row in diff
        .rows
        .iter()
        .skip(diff.scroll)
        .take(area.height as usize)
    {
        let line = row.line(styles);
        let Some((body, gutter)) = line.spans.split_last() else {
            y += 1;
            continue;
        };
        let indent: usize = gutter.iter().map(|span| width(&span.content)).sum();
        let room = (area.width as usize).saturating_sub(indent).max(1);
        for (index, chunk) in wrap(&body.content, room).into_iter().enumerate() {
            if y >= area.bottom() {
                return;
            }
            let mut spans = if index == 0 {
                gutter.to_vec()
            } else {
                vec![Span::raw(" ".repeat(indent))]
            };
            spans.push(Span::styled(chunk, body.style));
            frame
                .buffer_mut()
                .set_line(area.x, y, &Line::from(spans), area.width);
            y += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    use super::super::capture;
    use crate::app::diff::DiffScope;
    use crate::ui::theme::Palette;

    fn page() -> DiffPage {
        serde_json::from_value(json!({
            "binary": false, "stale": false, "truncated": false,
            "change_ref": "tool:abc", "path": "src/main.rs", "complete": true,
            "hunks": [{"old_start": 9, "old_count": 2, "new_start": 9, "new_count": 3,
                       "lines": [
                         {"kind": "context", "old_index": 9, "new_index": 9,
                          "text": " fn main() {", "line_complete": true},
                         {"kind": "removed", "old_index": 10, "text": "  old();",
                          "line_complete": true},
                         {"kind": "added", "new_index": 10, "text": "  new();",
                          "line_complete": true},
                       ]}]
        }))
        .unwrap()
    }

    fn hunk_lines(page: &DiffPage, styles: Styles) -> Vec<Line<'static>> {
        hunk_rows(page).iter().map(|row| row.line(styles)).collect()
    }

    fn records_lines(
        pages: &[DiffPage],
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

    /// The Agent's starts are 0-based indexes; a header prints the 1-based
    /// first line, or the line before an empty range, as unified diff does.
    #[test]
    fn hunk_headers_use_unified_one_based_numbers() {
        let styles = Styles::of(Palette::dark());
        let page: DiffPage = serde_json::from_value(json!({
            "binary": false, "stale": false, "truncated": false,
            "change_ref": "tool:a", "path": "a.py", "complete": true,
            "hunks": [
                {"old_start": 0, "old_count": 2, "new_start": 0, "new_count": 2, "lines": []},
                {"old_start": 5, "old_count": 0, "new_start": 5, "new_count": 1, "lines": []}
            ]
        }))
        .unwrap();
        let text: Vec<String> = hunk_lines(&page, styles).iter().map(row_text).collect();
        assert_eq!(text, ["@@ -1,2 +1,2 @@", "@@ -5,0 +6,1 @@"]);
    }

    #[test]
    fn a_line_split_into_fragments_is_one_row() {
        let styles = Styles::of(Palette::dark());
        let page: DiffPage = serde_json::from_value(json!({
            "binary": false, "stale": false, "truncated": false,
            "change_ref": "tool:a", "path": "a.py", "complete": true,
            "hunks": [{"old_start": 0, "old_count": 0, "new_start": 0, "new_count": 1,
                       "lines": [
                         {"kind": "added", "new_index": 0, "text": "long ", "line_complete": false},
                         {"kind": "added", "new_index": 0, "text": "line\n", "line_complete": true}
                       ]}]
        }))
        .unwrap();
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
        // A single record needs no separator.
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
        let page: DiffPage = serde_json::from_value(json!({
            "binary": false, "stale": false, "truncated": false,
            "change_ref": "tool:abc", "path": "a.rs", "complete": true,
            "hunks": [{"old_start": 1, "old_count": 0, "new_start": 1, "new_count": 2,
                       "lines": [
                         {"kind": "added", "text": "added();\n", "line_complete": true},
                         {"kind": "removed", "text": "gone();\r\n", "line_complete": true},
                       ]}]
        }))
        .unwrap();
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
        let page = |binary: bool| -> DiffPage {
            serde_json::from_value(json!({
                "hunks": [], "complete": false, "binary": binary, "stale": false, "truncated": false
            }))
            .unwrap()
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
        let mut view = DiffState::new(DiffScope::Session);
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
    fn the_diff_view_shows_a_file_column_and_the_hunks() {
        let styles = Styles::of(Palette::dark());
        let mut view = view();
        view.rows = hunk_rows(&page());
        let text = capture(|frame| render(frame, frame.area(), styles, &view), 100, 20);
        assert!(text[0].contains("a.rs"), "{}", text[0]);
        assert!(
            text.iter().any(|row| row.starts_with("M +3 −1 a.rs")),
            "{text:?}"
        );
        assert!(
            text.iter().any(|row| row.starts_with("A +9 −0 b.rs")),
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
        let text = capture(|frame| render(frame, frame.area(), styles, &view), 100, 20);
        assert!(
            text.iter().any(|row| row.starts_with("A ·     b.rs")),
            "{text:?}"
        );
    }

    #[test]
    fn a_narrow_terminal_drops_the_file_column_instead_of_wrapping_it() {
        let styles = Styles::of(Palette::dark());
        let mut view = view();
        view.rows = hunk_rows(&page());
        let text = capture(|frame| render(frame, frame.area(), styles, &view), 60, 16);
        assert!(text[0].contains("Diff · session · a.rs"), "{}", text[0]);
        assert!(
            !text.iter().any(|row| row.contains("+3 −1 a.rs")),
            "{text:?}"
        );
        assert!(text.iter().any(|row| row.contains("+  new();")), "{text:?}");
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
        view.rows = vec![DiffRow::Line {
            kind: LineKind::Added,
            gutter: String::new(),
            text: "x".repeat(120),
        }];
        let text = capture(|frame| render(frame, frame.area(), styles, &view), 60, 12);
        let x_rows = text.iter().filter(|row| row.contains('x')).count();
        assert!(x_rows >= 2, "{text:?}");
        for row in &text {
            assert!(width(row) <= 60);
        }
    }
}
