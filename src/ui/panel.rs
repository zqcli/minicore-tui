//! The one bottom-sheet style shared by every selector: pickers, the approval
//! prompt and the find bar. A `─` border above and below, a bold title with a
//! status on the right, a hint row of `key desc` pairs, an optional search
//! line and the list. It takes the editor's place in the dock.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::theme::Styles;
use crate::app::picker;
use crate::wrap::{self, sanitize, width, wrap};

/// A sheet never shows more than this many list rows; the rest scroll.
pub const MAX_ROWS: usize = 10;
/// Cells a row spends on the pointer and the current-item mark before its label.
const POINTER: usize = 2;

/// Emphasis of a row's label, so approving and denying read at a glance.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tone {
    #[default]
    Plain,
    Success,
    Failure,
    Warning,
}

/// How a sheet lays out its rows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Kind {
    /// `→` pointer, the detail right after the label.
    #[default]
    Arrow,
    /// `→` pointer, details aligned in one column.
    Column,
    /// `›` pointer on a highlighted band, the detail at the right edge.
    Session,
}

#[derive(Clone, Debug)]
pub struct Row {
    pub label: String,
    pub detail: String,
    pub tone: Tone,
    /// `Some(true)` marks the current item with `✓`; `Some(false)` keeps the
    /// mark column blank so labels stay aligned.
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
    /// Scope or count shown at the right of the title row.
    pub status: Option<String>,
    /// `key desc` pairs separated by ` · `.
    pub hint: String,
    /// The typed filter, drawn after a `> ` prompt; `None` for a sheet without one.
    pub search: Option<String>,
    /// Non-selectable lines between the header and the list.
    pub body: Vec<Line<'static>>,
    pub rows: Vec<Row>,
    pub selected: Option<usize>,
    pub kind: Kind,
    /// Lines below the list: the empty state, the selected item's details.
    pub notes: Vec<Line<'static>>,
}

impl Sheet {
    pub fn new(title: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            status: None,
            hint: hint.into(),
            search: None,
            body: Vec::new(),
            rows: Vec::new(),
            selected: None,
            kind: Kind::default(),
            notes: Vec::new(),
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

    pub fn status(mut self, status: impl Into<String>) -> Self {
        self.status = Some(status.into());
        self
    }

    pub fn search(mut self, search: impl Into<String>) -> Self {
        self.search = Some(search.into());
        self
    }

    pub fn kind(mut self, kind: Kind) -> Self {
        self.kind = kind;
        self
    }

    pub fn body(mut self, body: Vec<Line<'static>>) -> Self {
        self.body = body;
        self
    }

    pub fn notes(mut self, notes: Vec<Line<'static>>) -> Self {
        self.notes = notes;
        self
    }

    fn visible_rows(&self) -> usize {
        self.rows.len().min(MAX_ROWS)
    }

    /// Rows other than the list and its counter.
    fn chrome(&self) -> usize {
        let hint = usize::from(!self.hint.is_empty());
        let search = if self.search.is_some() { 3 } else { 0 };
        2 + 1 + hint + search + self.body.len() + self.notes.len()
    }

    /// Rows the sheet occupies at full size.
    pub fn height(&self) -> u16 {
        let counter = usize::from(self.rows.len() > self.visible_rows());
        (self.chrome() + self.visible_rows() + counter) as u16
    }

    /// The sheet as `width` x `height` rows, the list shrunk to fit.
    pub fn lines(&self, styles: Styles, width: u16, height: u16) -> Vec<Line<'static>> {
        let cells = usize::from(width);
        let height = usize::from(height);
        let list = self
            .visible_rows()
            .min(height.saturating_sub(self.chrome()));
        let border = || Line::styled("─".repeat(cells), styles.border());

        let mut lines = vec![border(), self.title_line(styles, cells)];
        if !self.hint.is_empty() {
            lines.push(hint_line(&self.hint, styles, cells));
        }
        if let Some(search) = &self.search {
            lines.push(Line::default());
            lines.push(Line::from(vec![
                Span::styled("> ", styles.accent()),
                Span::styled(truncate(search, cells.saturating_sub(2)), styles.text()),
            ]));
            lines.push(Line::default());
        }
        lines.extend(self.body.iter().cloned());

        let window = picker::window(self.selected.unwrap_or(0), self.rows.len(), list);
        let column = self.label_column();
        for index in window {
            lines.push(self.row_line(styles, cells, column, index));
        }
        if self.rows.len() > list && height > self.chrome() + list {
            let at = self.selected.map_or(1, |selected| selected + 1);
            let counter = format!("  ({at}/{})", self.rows.len());
            lines.push(Line::styled(counter, styles.dim()));
        }
        lines.extend(self.notes.iter().cloned());
        lines.truncate(height.saturating_sub(1));
        lines.push(border());
        lines
    }

    fn title_line(&self, styles: Styles, cells: usize) -> Line<'static> {
        let title = truncate(&self.title, cells);
        let mut spans = vec![Span::styled(title.clone(), styles.bold())];
        if let Some(status) = &self.status {
            let room = cells.saturating_sub(width(&title) + 2);
            let status = truncate(status, room);
            let gap = cells.saturating_sub(width(&title) + width(&status));
            spans.push(Span::raw(" ".repeat(gap)));
            spans.push(Span::styled(status, styles.dim()));
        }
        Line::from(spans)
    }

    /// Where the details start in a [`Kind::Column`] sheet.
    fn label_column(&self) -> usize {
        let widest = self.rows.iter().map(|row| width(&row.label)).max();
        widest.map_or(0, |widest| (widest + 2).min(32))
    }

    fn row_line(&self, styles: Styles, cells: usize, column: usize, index: usize) -> Line<'static> {
        let row = &self.rows[index];
        let selected = self.selected == Some(index);
        let session = self.kind == Kind::Session;
        let current = row.mark == Some(true);

        let pointer = match (selected, session) {
            (true, true) => "› ",
            (true, false) => "→ ",
            (false, _) => "  ",
        };
        let mark = match row.mark {
            Some(true) if !session => "✓ ",
            Some(_) if !session => "  ",
            _ => "",
        };
        let label_style = match row.tone {
            Tone::Plain if session => {
                let base = if current {
                    styles.accent()
                } else {
                    styles.text()
                };
                if selected {
                    base.add_modifier(Modifier::BOLD)
                } else {
                    base
                }
            }
            Tone::Plain if selected => styles.accent(),
            Tone::Plain => styles.text(),
            tone => {
                let base = match tone {
                    Tone::Success => styles.success(),
                    Tone::Failure => styles.failure(),
                    _ => styles.warning(),
                };
                if selected {
                    base.add_modifier(Modifier::BOLD)
                } else {
                    base
                }
            }
        };
        let detail_style = if selected && self.kind == Kind::Column {
            label_style
        } else {
            styles.dim()
        };

        let detail = sanitize(&row.detail).into_owned();
        let room = cells.saturating_sub(POINTER + width(mark));
        let (label, gap) = match self.kind {
            Kind::Arrow => (truncate(&row.label, room), 1),
            Kind::Column => {
                let label = truncate(&row.label, room.min(column.max(POINTER) - POINTER));
                let gap = column.saturating_sub(width(&label)).max(1);
                (label, gap)
            }
            Kind::Session => {
                let label = truncate(&row.label, room.saturating_sub(width(&detail) + 2));
                let gap = room.saturating_sub(width(&label) + width(&detail)).max(1);
                (label, gap)
            }
        };
        let mut spans = vec![
            Span::styled(pointer, styles.accent()),
            Span::styled(mark, styles.accent()),
            Span::styled(label, label_style),
        ];
        if !detail.is_empty() {
            spans.push(Span::raw(" ".repeat(gap)));
            spans.push(Span::styled(detail, detail_style));
        }
        let line = Line::from(spans);
        if selected && session {
            line.style(styles.highlight())
        } else {
            line
        }
    }
}

/// `key desc` pairs: the key is the first word of each ` · ` part.
fn hint_line(hint: &str, styles: Styles, cells: usize) -> Line<'static> {
    let hint = truncate(hint, cells);
    let mut spans = Vec::new();
    for (index, part) in hint.split(" · ").enumerate() {
        if index > 0 {
            spans.push(Span::styled(" · ", styles.dim()));
        }
        let (key, text) = part.split_once(' ').unwrap_or((part, ""));
        spans.push(Span::styled(key.to_owned(), styles.faint()));
        if !text.is_empty() {
            spans.push(Span::styled(format!(" {text}"), styles.dim()));
        }
    }
    Line::from(spans)
}

/// Draws `sheet` into `area`.
pub fn render(frame: &mut Frame, area: Rect, styles: Styles, sheet: &Sheet) {
    let lines = sheet.lines(styles, area.width, area.height);
    for (y, line) in (area.y..area.bottom()).zip(&lines) {
        frame.render_widget(
            line,
            Rect {
                y,
                height: 1,
                ..area
            },
        );
    }
}

/// Sanitizes `text` and shortens it to `cells`, ending with `…` when cut.
pub fn truncate(text: &str, cells: usize) -> String {
    wrap::truncate(&sanitize(text), cells)
}

/// Wraps one message into sheet body lines.
pub fn body_lines(text: &str, cells: usize, style: Style) -> Vec<Line<'static>> {
    wrap(&sanitize(text), cells)
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

    fn text(line: &Line) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn capture(sheet: &Sheet, width: u16) -> Vec<String> {
        let styles = Styles::of(Palette::dark());
        sheet
            .lines(styles, width, sheet.height())
            .iter()
            .map(text)
            .collect()
    }

    fn rows(count: usize) -> Vec<Row> {
        (0..count)
            .map(|index| Row::new(format!("row {index:02}"), format!("{index:02}")))
            .collect()
    }

    #[test]
    fn the_frame_is_a_border_title_hint_and_a_closing_border() {
        let sheet = Sheet::new("Title", "enter open · esc close")
            .status("3 sessions")
            .rows(rows(2))
            .selected(Some(0));
        assert_eq!(sheet.height(), 6);
        let lines = capture(&sheet, 40);
        assert_eq!(lines[0], "─".repeat(40));
        assert_eq!(lines[1], format!("Title{}3 sessions", " ".repeat(25)));
        assert_eq!(lines[2], "enter open · esc close");
        assert_eq!(lines[3], "→ row 00 00");
        assert_eq!(lines[4], "  row 01 01");
        assert_eq!(lines[5], "─".repeat(40));
    }

    #[test]
    fn a_search_sheet_has_blank_rows_around_the_prompt() {
        let sheet = Sheet::new("Model", "esc cancel")
            .search("gpt")
            .rows(rows(1));
        let lines = capture(&sheet, 30);
        assert_eq!(lines[3], "");
        assert_eq!(lines[4], "> gpt");
        assert_eq!(lines[5], "");
    }

    #[test]
    fn the_window_scrolls_and_a_counter_appears() {
        let sheet = Sheet::new("Models", "hint")
            .rows(rows(25))
            .selected(Some(12));
        assert_eq!(sheet.height(), 3 + 1 + MAX_ROWS as u16 + 1);
        let lines = capture(&sheet, 30);
        assert!(lines[3].starts_with("  row 07"), "{:?}", lines[3]);
        assert!(lines[8].starts_with("→ row 12"), "{:?}", lines[8]);
        assert_eq!(lines[13], "  (13/25)");
        assert_eq!(lines[14], "─".repeat(30));
    }

    #[test]
    fn a_short_list_has_no_counter() {
        let sheet = Sheet::new("Models", "hint")
            .rows(rows(10))
            .selected(Some(3));
        assert!(!capture(&sheet, 30).iter().any(|line| line.contains('/')));
    }

    #[test]
    fn a_cramped_area_shrinks_the_list_and_keeps_the_selection() {
        let styles = Styles::of(Palette::dark());
        let sheet = Sheet::new("Models", "hint")
            .rows(rows(25))
            .selected(Some(20));
        let lines: Vec<String> = sheet.lines(styles, 30, 8).iter().map(text).collect();
        assert_eq!(lines.len(), 8);
        assert!(lines.iter().any(|line| line.starts_with("→ row 20")));
        assert_eq!(lines[7], "─".repeat(30));
    }

    #[test]
    fn a_session_row_is_a_band_with_the_detail_at_the_right_edge() {
        let palette = Palette::dark();
        let styles = Styles::of(palette);
        let sheet = Sheet::new("Resume", "")
            .kind(Kind::Session)
            .rows(vec![
                Row::new("Fix the parser", "3 2h"),
                Row::new("Other", "1 now"),
            ])
            .selected(Some(0));
        let lines = sheet.lines(styles, 30, sheet.height());
        assert_eq!(
            text(&lines[2]),
            format!("› Fix the parser{}3 2h", " ".repeat(10))
        );
        assert_eq!(lines[2].style.bg, Some(palette.selection_bg));
        assert_eq!(lines[3].style.bg, None);
        assert_eq!(text(&lines[3]), format!("  Other{}1 now", " ".repeat(18)));
    }

    #[test]
    fn the_current_item_gets_a_check_and_the_others_stay_aligned() {
        let sheet = Sheet::new("Model", "").rows(vec![
            Row::new("gpt-6", "[openai]").mark(true),
            Row::new("glm-5", "[zai]").mark(false),
        ]);
        let lines = capture(&sheet, 40);
        assert_eq!(lines[2], "  ✓ gpt-6 [openai]");
        assert_eq!(lines[3], "    glm-5 [zai]");
    }

    #[test]
    fn column_details_line_up() {
        let sheet = Sheet::new("Reasoning", "")
            .kind(Kind::Column)
            .rows(vec![
                Row::new("low", "Light"),
                Row::new("medium", "Moderate"),
            ])
            .selected(Some(1));
        let lines = capture(&sheet, 40);
        assert_eq!(lines[2], "  low     Light");
        assert_eq!(lines[3], "→ medium  Moderate");
    }

    #[test]
    fn tones_colour_the_label() {
        let palette = Palette::dark();
        let styles = Styles::of(palette);
        let sheet = Sheet::new("Approval", "").rows(vec![
            Row::new("Allow once", "y").tone(Tone::Success),
            Row::new("Deny", "n").tone(Tone::Failure),
        ]);
        let lines = sheet.lines(styles, 30, sheet.height());
        assert_eq!(lines[2].spans[2].style.fg, Some(palette.success));
        assert_eq!(lines[3].spans[2].style.fg, Some(palette.failure));
    }

    #[test]
    fn the_hint_dims_keys_and_text_differently() {
        let palette = Palette::dark();
        let styles = Styles::of(palette);
        let line = hint_line("enter open · esc close", styles, 40);
        assert_eq!(text(&line), "enter open · esc close");
        assert_eq!(line.spans[0].style.fg, Some(palette.faint));
        assert_eq!(line.spans[1].style.fg, Some(palette.dim));
    }

    #[test]
    fn the_border_uses_the_border_colour_across_the_width() {
        let palette = Palette::dark();
        let styles = Styles::of(palette);
        let sheet = Sheet::new("T", "h");
        let mut terminal = Terminal::new(TestBackend::new(20, sheet.height())).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), styles, &sheet))
            .unwrap();
        let buffer = terminal.backend().buffer();
        for x in 0..20u16 {
            assert_eq!(buffer[(x, 0u16)].fg, palette.border);
            assert_eq!(buffer[(x, 3u16)].symbol(), "─");
        }
    }

    #[test]
    fn long_labels_are_cut_before_the_detail() {
        let sheet = Sheet::new("Resume", "")
            .kind(Kind::Session)
            .rows(vec![Row::new("a very long session title indeed", "3 2h")]);
        let lines = capture(&sheet, 24);
        assert_eq!(lines[2], "  a very long ses…  3 2h");
        assert_eq!(width(&lines[2]), 24);
    }

    #[test]
    fn control_characters_never_reach_the_terminal() {
        let sheet = Sheet::new("Ti\u{1b}]52;x\u{7}tle", "h\u{1b}[2Jint")
            .rows(vec![Row::new("a\u{1b}[31mred", "b\u{7}")]);
        let joined = capture(&sheet, 60).join("\n");
        assert!(!joined.contains('\u{1b}'), "{joined:?}");
        assert!(!joined.contains('\u{7}'), "{joined:?}");
    }

    #[test]
    fn truncation_keeps_the_leading_cells_and_marks_the_cut() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("abcdef", 0), "");
        assert_eq!(truncate("中文中文", 5), "中文…");
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
            assert!(width(&text(line)) <= 20);
        }
    }
}
