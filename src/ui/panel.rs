//! The one bottom-sheet style shared by every panel: pickers, the approval
//! prompt and the find bar. An accent title row, full-width selectable rows
//! with a highlight band, and a dim hint row. No borders, no boxes.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::theme::Styles;
use super::wrap::{self, sanitize, width, wrap};

/// A sheet never shows more than this many list rows; the rest scroll.
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
    /// `Some(true)` marks the current item with an accent `●`; `Some(false)`
    /// keeps the marker column blank so labels stay aligned.
    pub mark: Option<bool>,
}

impl Row {
    pub fn new(label: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            detail: detail.into(),
            tone: Tone::Plain,
            mark: None,
        }
    }

    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = tone;
        self
    }

    pub fn mark(mut self, current: bool) -> Self {
        self.mark = Some(current);
        self
    }
}

/// Everything a sheet draws; it owns its text, so a picker can rebuild it every frame.
pub struct Sheet {
    pub title: String,
    /// Filter text shown after the title, or a status such as `3 of 12`.
    pub subtitle: Option<String>,
    /// Non-selectable lines between the title and the list (prompt text,
    /// an interaction question, a confirmation).
    pub body: Vec<Line<'static>>,
    pub rows: Vec<Row>,
    pub selected: Option<usize>,
    pub hint: String,
}

impl Sheet {
    pub fn new(title: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            subtitle: None,
            body: Vec::new(),
            rows: Vec::new(),
            selected: None,
            hint: hint.into(),
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

    pub fn subtitle(mut self, subtitle: Option<String>) -> Self {
        self.subtitle = subtitle;
        self
    }

    pub fn body(mut self, body: Vec<Line<'static>>) -> Self {
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

    /// The rows drawn: the first `MAX_ROWS`, scrolled just enough to keep the
    /// selection on screen.
    fn window(&self) -> std::ops::Range<usize> {
        let start = self
            .selected
            .map_or(0, |selected| (selected + 1).saturating_sub(MAX_ROWS));
        start..(start + MAX_ROWS).min(self.rows.len())
    }
}

/// Draws `sheet` into `area`, which must be at least [`Sheet::height`] rows.
pub fn render(frame: &mut Frame, area: Rect, styles: Styles, sheet: &Sheet) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let mut y = area.y;
    let bottom = area.bottom();

    render_title(frame, Rect { height: 1, ..area }, styles, sheet);
    y += 1;

    for line in &sheet.body {
        if y >= bottom {
            return;
        }
        draw_line(frame, area, y, line.clone());
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
        render_row(frame, row_area, styles, row, highlighted);
        y += 1;
    }

    if y < bottom {
        draw_line(
            frame,
            area,
            y,
            Line::from(Span::styled(clean(&sheet.hint), styles.faint())),
        );
    }
}

fn render_title(frame: &mut Frame, area: Rect, styles: Styles, sheet: &Sheet) {
    let mut spans = vec![Span::styled(clean(&sheet.title), styles.title())];
    if let Some(subtitle) = &sheet.subtitle {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(clean(subtitle), styles.dim()));
    }
    draw_line(frame, area, area.y, Line::from(spans));
}

fn render_row(frame: &mut Frame, area: Rect, styles: Styles, row: &Row, highlighted: bool) {
    let label_style = if highlighted {
        frame.buffer_mut().set_style(area, styles.highlight());
        styles.bold()
    } else {
        styles.text()
    };

    let detail = clean(&row.detail);
    let marker = match row.mark {
        Some(true) => Span::styled("● ", styles.accent()),
        Some(false) => Span::raw("  "),
        None => Span::raw(""),
    };
    let content =
        (area.width.saturating_sub(2 * PAD) as usize).saturating_sub(width(&marker.content));
    let detail_width = width(&detail);
    let label = truncate(&row.label, content.saturating_sub(detail_width + 2));
    let gap = content
        .saturating_sub(width(&label))
        .saturating_sub(detail_width)
        .max(1);

    let detail_style = match row.tone {
        Tone::Plain => styles.faint(),
        Tone::Success => styles.success(),
        Tone::Failure => styles.failure(),
        Tone::Warning => styles.warning(),
    };
    let line = Line::from(vec![
        Span::raw(" ".repeat(PAD as usize)),
        marker,
        Span::styled(label, label_style),
        Span::raw(" ".repeat(gap)),
        Span::styled(detail, detail_style),
    ]);
    draw_line(frame, area, area.y, line);
}

/// Writes one line at `y`, clipped to `area`, over any band already painted.
fn draw_line(frame: &mut Frame, area: Rect, y: u16, line: Line<'_>) {
    frame.buffer_mut().set_line(area.x, y, &line, area.width);
}

fn clean(text: &str) -> String {
    sanitize(text).into_owned()
}

/// Sanitizes `text` and shortens it to `cells`, ending with `…` when cut.
pub fn truncate(text: &str, cells: usize) -> String {
    wrap::truncate(&sanitize(text), cells)
}

/// Wraps one message into sheet body lines.
pub fn body_lines(text: &str, cells: usize, style: Style) -> Vec<Line<'static>> {
    wrap(&clean(text), cells)
        .into_iter()
        .map(|line| Line::from(Span::styled(line, style)))
        .collect()
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::ui::theme::Palette;

    fn capture(sheet: &Sheet, width: u16, height: u16) -> Vec<String> {
        let styles = Styles::of(Palette::dark());
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                render(frame, area, styles, sheet);
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
        let styles = Styles::of(palette);
        let sheet = Sheet::new("Sessions", "enter open").rows(rows(1));
        let text = capture(&sheet, 40, 3);
        assert_eq!(text[0], "Sessions");
        // The label is left aligned, the detail is right aligned.
        assert!(text[1].starts_with(" row 00"), "{:?}", text[1]);
        assert!(text[1].ends_with("00"), "{:?}", text[1]);
        assert_eq!(text[2], "enter open");

        let mut terminal = Terminal::new(TestBackend::new(40, 3)).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), styles, &sheet))
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
        let styles = Styles::of(palette);
        let sheet = Sheet::new("Commands", "enter run")
            .rows(rows(3))
            .selected(Some(1));
        let mut terminal = Terminal::new(TestBackend::new(20, 5)).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), styles, &sheet))
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
    fn the_list_stops_at_the_row_cap_and_scrolls_with_the_selection() {
        let sheet = Sheet::new("Sessions", "hint").rows(rows(20));
        assert_eq!(sheet.height(), 1 + MAX_ROWS as u16 + 1);
        let top = capture(&sheet.selected(Some(MAX_ROWS - 1)), 30, 14);
        assert!(top[1].starts_with(" row 00"), "{:?}", top[1]);
        assert!(top[MAX_ROWS].starts_with(" row 11"), "{:?}", top[MAX_ROWS]);

        let sheet = Sheet::new("Sessions", "hint")
            .rows(rows(20))
            .selected(Some(15));
        let text = capture(&sheet, 30, sheet.height());
        assert!(text[1].starts_with(" row 04"), "{:?}", text[1]);
        assert!(
            text[MAX_ROWS].starts_with(" row 15"),
            "the selection is the last visible row: {:?}",
            text[MAX_ROWS]
        );
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
