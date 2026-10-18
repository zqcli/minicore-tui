//! The one bottom-sheet style shared by every panel: pickers, the approval
//! prompt and the find bar. An accent title row, full-width selectable rows
//! with a highlight band, and a dim hint row. No borders, no boxes.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::theme::Palette;

use super::wrap::{sanitize, width, wrap_line};

/// A sheet never covers more than this many list rows; the rest scroll.
pub const MAX_ROWS: usize = 12;
/// One cell of padding on each side of every row.
const PAD: u16 = 1;

/// Emphasis of the right-hand detail text, so a failed row reads as a failure
/// without a second color scheme.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tone {
    #[default]
    Plain,
    Success,
    Failure,
    Warning,
}

#[derive(Clone, Debug)]
pub struct Row {
    pub label: String,
    pub detail: String,
    pub tone: Tone,
}

impl Row {
    pub fn new(label: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            detail: detail.into(),
            tone: Tone::Plain,
        }
    }

    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = tone;
        self
    }
}

/// Everything a sheet draws. Callers own the content; this module owns the
/// geometry and the visual system.
pub struct Sheet<'a> {
    pub title: &'a str,
    /// Filter text shown after the title, or a status such as `3 of 12`.
    pub subtitle: Option<String>,
    /// Non-selectable lines between the title and the list (prompt text,
    /// an interaction question, a confirmation).
    pub body: Vec<Line<'a>>,
    pub rows: Vec<Row>,
    pub selected: Option<usize>,
    pub offset: usize,
    pub hint: &'a str,
}

impl<'a> Sheet<'a> {
    pub fn new(title: &'a str, hint: &'a str) -> Self {
        Self {
            title,
            subtitle: None,
            body: Vec::new(),
            rows: Vec::new(),
            selected: None,
            offset: 0,
            hint,
        }
    }

    pub fn rows(mut self, rows: Vec<Row>) -> Self {
        self.rows = rows;
        self
    }

    pub fn selected(mut self, selected: Option<usize>) -> Self {
        self.selected = selected;
        self
    }

    pub fn offset(mut self, offset: usize) -> Self {
        self.offset = offset;
        self
    }

    pub fn subtitle(mut self, subtitle: Option<String>) -> Self {
        self.subtitle = subtitle;
        self
    }

    pub fn body(mut self, body: Vec<Line<'a>>) -> Self {
        self.body = body;
        self
    }

    fn visible_rows(&self) -> usize {
        self.rows.len().min(MAX_ROWS)
    }

    /// Rows the sheet occupies, excluding any separator the caller adds.
    pub fn height(&self) -> u16 {
        1 + self.body.len() as u16 + self.visible_rows() as u16 + 1
    }

    /// The window of rows actually drawn, from the scroll offset.
    fn window(&self) -> std::ops::Range<usize> {
        let visible = self.visible_rows();
        let start = self
            .offset
            .min(self.rows.len().saturating_sub(visible.min(self.rows.len())));
        start..(start + visible).min(self.rows.len())
    }
}

/// Draws `sheet` into `area`, which must be at least [`Sheet::height`] rows.
pub fn render(frame: &mut Frame, area: Rect, palette: &Palette, sheet: &Sheet) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let mut y = area.y;
    let bottom = area.bottom();

    render_title(frame, Rect { height: 1, ..area }, palette, sheet);
    y += 1;

    for line in &sheet.body {
        if y >= bottom {
            return;
        }
        draw_line(frame, area, y, palette, line.clone());
        y += 1;
    }

    let window = sheet.window();
    let selected = sheet.selected;
    for index in window {
        if y >= bottom {
            return;
        }
        let row = &sheet.rows[index];
        let highlighted = selected == Some(index);
        let row_area = Rect {
            x: area.x,
            y,
            width: area.width,
            height: 1,
        };
        render_row(frame, row_area, palette, row, highlighted);
        y += 1;
    }

    if y < bottom {
        draw_line(
            frame,
            area,
            y,
            palette,
            Line::from(Span::styled(
                sanitize(sheet.hint).into_owned(),
                Style::default().fg(palette.faint),
            )),
        );
    }
}

fn render_title(frame: &mut Frame, area: Rect, palette: &Palette, sheet: &Sheet) {
    let mut spans = vec![Span::styled(
        sanitize(sheet.title).into_owned(),
        Style::default()
            .fg(palette.accent)
            .add_modifier(Modifier::BOLD),
    )];
    if let Some(subtitle) = &sheet.subtitle {
        spans.push(Span::styled("  ", Style::default()));
        spans.push(Span::styled(
            sanitize(subtitle).into_owned(),
            Style::default().fg(palette.dim),
        ));
    }
    draw_line(frame, area, area.y, palette, Line::from(spans));
}

fn render_row(frame: &mut Frame, area: Rect, palette: &Palette, row: &Row, highlighted: bool) {
    let (background, label_style) = if highlighted {
        (
            Some(palette.selection_bg),
            Style::default()
                .fg(palette.text)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        (None, Style::default().fg(palette.text))
    };
    if let Some(background) = background {
        frame
            .buffer_mut()
            .set_style(area, Style::default().bg(background));
    }

    let detail = sanitize(&row.detail);
    let content = area.width.saturating_sub(2 * PAD) as usize;
    let detail_width = width(&detail);
    let label = truncate(&row.label, content.saturating_sub(detail_width + 2));
    let gap = content
        .saturating_sub(width(&label))
        .saturating_sub(detail_width)
        .max(1);

    let detail_style = Style::default().fg(match row.tone {
        Tone::Plain => palette.faint,
        Tone::Success => palette.success,
        Tone::Failure => palette.failure,
        Tone::Warning => palette.warning,
    });
    let line = Line::from(vec![
        Span::styled(" ".repeat(PAD as usize), Style::default()),
        Span::styled(label, label_style),
        Span::styled(" ".repeat(gap), Style::default()),
        Span::styled(detail.into_owned(), detail_style),
    ]);
    draw_line(frame, area, area.y, palette, line);
}

/// Writes one line at `y`, clipped to `area`, over any band already painted.
fn draw_line(frame: &mut Frame, area: Rect, y: u16, _palette: &Palette, line: Line<'_>) {
    frame.buffer_mut().set_line(area.x, y, &line, area.width);
}

/// Shortens `text` to `cells`, ending with `…` when it was cut.
pub fn truncate(text: &str, cells: usize) -> String {
    let text = sanitize(text);
    if width(&text) <= cells {
        return text.into_owned();
    }
    if cells == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for grapheme in unicode_segmentation::UnicodeSegmentation::graphemes(&*text, true) {
        let cells_of = width(grapheme);
        if used + cells_of > cells.saturating_sub(1) {
            break;
        }
        out.push_str(grapheme);
        used += cells_of;
    }
    out.push('…');
    out
}

/// The row index under a pointer at `(column, row)` inside the sheet's `area`.
pub fn row_at(area: Rect, sheet: &Sheet, column: u16, row: u16) -> Option<usize> {
    if column < area.x || column >= area.right() || row < area.y || row >= area.bottom() {
        return None;
    }
    let body_rows = sheet.body.len() as u16;
    let list_top = area.y + 1 + body_rows;
    if row < list_top {
        return None;
    }
    let index = sheet.window().start + (row - list_top) as usize;
    (index < sheet.window().end).then_some(index)
}

/// Wraps one message into sheet body lines, so a long approval prompt still
/// fits the sheet instead of being cut.
pub fn body_lines(text: &str, cells: usize, style: Style) -> Vec<Line<'static>> {
    wrap_line(&sanitize(text), cells)
        .into_iter()
        .map(|line| Line::from(Span::styled(line, style)))
        .collect()
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;

    fn capture(sheet: &Sheet, width: u16, height: u16) -> Vec<String> {
        let palette = Palette::dark();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                render(frame, area, &palette, sheet);
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let cells = buffer.content();
        cells
            .chunks(width as usize)
            .map(|row| {
                row.iter()
                    .map(|cell| cell.symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    fn rows(count: usize) -> Vec<Row> {
        (0..count)
            .map(|index| Row::new(format!("row {index:02}"), format!("{index:02}")))
            .collect()
    }

    #[test]
    fn height_counts_title_rows_and_hint() {
        let sheet = Sheet::new("Title", "hint").rows(rows(3));
        assert_eq!(sheet.height(), 5);
        let sheet = sheet.body(vec![Line::from("question")]);
        assert_eq!(sheet.height(), 6);
    }

    #[test]
    fn the_title_is_accent_and_the_hint_is_dim() {
        let palette = Palette::dark();
        let sheet = Sheet::new("Sessions", "enter open").rows(rows(1));
        let text = capture(&sheet, 40, 3);
        assert_eq!(text[0], "Sessions");
        // The label is left aligned, the detail is right aligned.
        assert!(text[1].starts_with(" row 00"), "{:?}", text[1]);
        assert!(text[1].ends_with("00"), "{:?}", text[1]);
        assert_eq!(text[2], "enter open");

        let mut terminal = Terminal::new(TestBackend::new(40, 3)).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), &palette, &sheet))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let title = &buffer[(0u16, 0u16)];
        assert_eq!(title.fg, palette.accent, "title color");
        let hint = &buffer[(0u16, 2u16)];
        assert_eq!(hint.fg, palette.faint, "hint color");
    }

    #[test]
    fn a_narrow_sheet_shows_the_label_and_the_detail_without_overlap() {
        let sheet = Sheet::new("Models", "hint").rows(vec![Row::new(
            "a very long model identifier",
            "max reasoning",
        )]);
        let text = capture(&sheet, 24, sheet.height());
        assert_eq!(text[1], " a very…  max reasoning");
    }

    #[test]
    fn the_selected_row_is_one_full_width_band() {
        let palette = Palette::dark();
        let sheet = Sheet::new("Commands", "enter run")
            .rows(rows(3))
            .selected(Some(1));
        let mut terminal = Terminal::new(TestBackend::new(20, 5)).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), &palette, &sheet))
            .unwrap();
        let buffer = terminal.backend().buffer();
        for column in 0..20 {
            let cell = &buffer[(column, 2u16)];
            assert_eq!(
                cell.bg, palette.selection_bg,
                "background band must span the row at column {column}"
            );
        }
        assert_ne!(buffer[(0u16, 1u16)].bg, palette.selection_bg);
    }

    #[test]
    fn the_list_scrolls_to_the_offset_and_stops_at_the_row_cap() {
        let sheet = Sheet::new("Sessions", "hint").rows(rows(20)).offset(6);
        assert_eq!(sheet.height(), 1 + MAX_ROWS as u16 + 1);
        let text = capture(&sheet, 30, sheet.height());
        assert!(text[1].starts_with(" row 06"), "{:?}", text[1]);
        assert!(text[1].ends_with("06"), "{:?}", text[1]);
        assert!(
            text[MAX_ROWS].starts_with(" row 17"),
            "{:?}",
            text[MAX_ROWS]
        );
    }

    #[test]
    fn an_offset_past_the_end_is_clamped_so_the_last_row_stays_visible() {
        let sheet = Sheet::new("Sessions", "hint").rows(rows(20)).offset(99);
        let text = capture(&sheet, 30, sheet.height());
        assert!(
            text[MAX_ROWS].starts_with(" row 19"),
            "{:?}",
            text[MAX_ROWS]
        );
    }

    #[test]
    fn row_hit_testing_skips_the_title_body_and_hint() {
        let sheet = Sheet::new("Approval", "y allow")
            .body(vec![Line::from("Write a.rs?")])
            .rows(rows(2));
        let area = Rect::new(0, 10, 40, sheet.height());
        assert_eq!(row_at(area, &sheet, 3, 10), None, "title row");
        assert_eq!(row_at(area, &sheet, 3, 11), None, "body row");
        assert_eq!(row_at(area, &sheet, 3, 12), Some(0));
        assert_eq!(row_at(area, &sheet, 3, 13), Some(1));
        assert_eq!(row_at(area, &sheet, 3, 14), None, "hint row");
        assert_eq!(row_at(area, &sheet, 3, 99), None, "outside the area");
        assert_eq!(row_at(area, &sheet, 99, 12), None, "outside the columns");
    }

    #[test]
    fn truncation_keeps_the_leading_cells_and_marks_the_cut() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("abcdef", 0), "");
        // Wide characters are never split in half.
        assert_eq!(truncate("中文中文", 5), "中文…");
    }

    #[test]
    fn control_characters_never_reach_the_terminal() {
        let sheet = Sheet::new("Ti\u{1b}]52;x\u{7}tle", "h\u{1b}[2Jint")
            .rows(vec![Row::new("a\u{1b}[31mred", "b\u{7}")]);
        let text = capture(&sheet, 60, sheet.height()).join("\n");
        assert!(!text.contains('\u{1b}'), "{text:?}");
        assert!(!text.contains('\u{7}'), "{text:?}");
    }

    #[test]
    fn body_text_wraps_instead_of_being_cut() {
        let lines = body_lines(
            "Write to src/main.rs and then run the tests?",
            20,
            Style::default(),
        );
        assert!(lines.len() > 1);
        for line in &lines {
            let text: String = line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect();
            assert!(width(&text) <= 20, "{text:?}");
        }
    }
}
