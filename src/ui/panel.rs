//! The unified lightweight bottom-sheet & semi-popup engine shared by every
//! selector, confirmation, approval prompt, find bar, and the `/` command and
//! `@` file completion popups.

use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::chrome;
use super::theme::Styles;
use crate::app::picker;
use crate::wrap::{self, sanitize, width, wrap};

/// Default maximum list rows shown at once before windowing scrolls.
pub const MAX_ROWS: usize = 10;
/// Cells a row spends on the selection rail/pointer column (`▎ ` / `✓ ` / `  `).
const POINTER: usize = 2;

/// Emphasis of a row's label, so approving, denying and live states read at a glance.
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
    /// Detail immediately follows the label.
    #[default]
    Arrow,
    /// Details align in a shared column; optional right-aligned badge.
    Column,
    /// Detail sits at the right edge (used by Sessions).
    Session,
}

/// Recorded geometry of the last rendered sheet or inline completion popup for mouse clicks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SheetHitArea {
    pub area: Rect,
    pub list_y: u16,
    pub list_rows: usize,
    pub first: usize,
    pub total: usize,
}

impl SheetHitArea {
    pub fn contains(&self, col: u16, row: u16) -> bool {
        self.area.contains(Position::new(col, row))
    }

    pub fn row_at(&self, col: u16, row: u16) -> Option<usize> {
        if !self.contains(col, row) || self.list_rows == 0 || self.total == 0 {
            return None;
        }
        if row < self.list_y {
            return None;
        }
        let slot = usize::from(row - self.list_y);
        if slot >= self.list_rows {
            return None;
        }
        let index = self.first + slot;
        (index < self.total).then_some(index)
    }
}

#[derive(Clone, Debug)]
pub struct Row {
    pub label: String,
    /// Optional argument hint shown dim right after `label` (e.g. `[title]`).
    pub args: String,
    pub detail: String,
    /// Optional right-aligned metadata/hotkey badge (e.g. `ctrl+b`, `● dark`).
    pub badge: String,
    pub tone: Tone,
    /// `Some(true)` marks the current item with `✓`; `Some(false)` keeps the
    /// mark column blank so labels stay aligned.
    pub mark: Option<bool>,
}

impl Row {
    pub fn new(label: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            args: String::new(),
            detail: detail.into(),
            badge: String::new(),
            tone: Tone::Plain,
            mark: None,
        }
    }

    pub fn args(mut self, args: impl Into<String>) -> Self {
        self.args = args.into();
        self
    }

    pub fn badge(mut self, badge: impl Into<String>) -> Self {
        self.badge = badge.into();
        self
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
    /// The typed filter, drawn via the unified `/ query▌` bar; `None` without one.
    pub search: Option<String>,
    /// Non-selectable lines between the header and the list.
    pub body: Vec<Line<'static>>,
    pub rows: Vec<Row>,
    pub selected: Option<usize>,
    pub kind: Kind,
    /// Maximum visible rows before windowing scrolls.
    pub max_rows: usize,
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
            max_rows: MAX_ROWS,
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

    pub fn max_rows(mut self, max_rows: usize) -> Self {
        self.max_rows = max_rows.max(1);
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
        self.rows.len().min(self.max_rows)
    }

    /// Rows other than the list and its counter.
    fn chrome(&self) -> usize {
        let search = usize::from(self.search.is_some());
        2 + search + self.body.len() + self.notes.len()
    }

    /// Rows the sheet occupies at full size.
    pub fn height(&self) -> u16 {
        let counter = usize::from(self.rows.len() > self.visible_rows());
        (self.chrome() + self.visible_rows() + counter) as u16
    }

    /// The sheet as `width` x `height` rows, the list shrunk to fit.
    pub fn lines(&self, styles: Styles, width: u16, height: u16) -> Vec<Line<'static>> {
        self.build_lines(styles, width, height).0
    }

    fn build_lines(
        &self,
        styles: Styles,
        width: u16,
        height: u16,
    ) -> (Vec<Line<'static>>, usize, usize, usize) {
        let cells = usize::from(width);
        let height = usize::from(height);
        let list = self
            .visible_rows()
            .min(height.saturating_sub(self.chrome()));
        let mut lines = vec![self.title_line(styles, cells)];
        if let Some(search) = &self.search {
            let mut spans = vec![Span::raw(" ")];
            spans.extend(
                chrome::filter_line(
                    search,
                    "type to filter…",
                    true,
                    cells.saturating_sub(1),
                    styles,
                )
                .spans,
            );
            lines.push(Line::from(spans));
        }
        lines.extend(self.body.iter().cloned());

        let list_offset = lines.len();
        let window = picker::window(self.selected.unwrap_or(0), self.rows.len(), list);
        let first = window.start;
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
        let rendered_rows = lines.len().saturating_sub(list_offset).min(list);
        lines.push(self.hint_border(styles, cells));
        (lines, list_offset, first, rendered_rows)
    }

    /// `─ Title ──────────── 5 of 12 ─`: the top border carries the name and count.
    fn title_line(&self, styles: Styles, cells: usize) -> Line<'static> {
        let border = styles.border();
        let title = truncate(&self.title, cells.saturating_sub(4));
        let status = self
            .status
            .as_deref()
            .map(|status| truncate(status, cells.saturating_sub(width(&title) + 8)))
            .filter(|status| !status.is_empty());
        let used = 3 + width(&title) + status.as_ref().map_or(0, |s| width(s) + 3);
        let mut spans = vec![
            Span::styled("─ ", border),
            Span::styled(title, styles.bold()),
            Span::styled(" ", border),
            Span::styled("─".repeat(cells.saturating_sub(used)), border),
        ];
        if let Some(status) = status {
            spans.push(Span::styled(" ", border));
            spans.push(Span::styled(status, styles.dim()));
            spans.push(Span::styled(" ─", border));
        }
        Line::from(spans)
    }

    /// `─ enter open · esc close ─────`: structured key/verb hints in the bottom rule.
    fn hint_border(&self, styles: Styles, cells: usize) -> Line<'static> {
        let border = styles.border();
        if self.hint.is_empty() {
            return Line::styled("─".repeat(cells), border);
        }
        let clipped = truncate(&self.hint, cells.saturating_sub(4));
        let pairs: Vec<(&str, &str)> = clipped
            .split(" · ")
            .map(|part| part.split_once(' ').unwrap_or((part, "")))
            .collect();
        let hint_spans = chrome::key_hints(&pairs, styles);
        let hint_w: usize = hint_spans.iter().map(|s| width(&s.content)).sum();
        let rest = cells.saturating_sub(hint_w + 3);
        let mut spans = Vec::with_capacity(hint_spans.len() + 2);
        spans.push(Span::styled("─ ", border));
        spans.extend(hint_spans);
        spans.push(Span::styled(format!(" {}", "─".repeat(rest)), border));
        Line::from(spans)
    }

    /// Where the details start in a [`Kind::Column`] sheet.
    fn label_column(&self) -> usize {
        let widest = self
            .rows
            .iter()
            .map(|row| {
                let lw = width(&row.label);
                if row.args.is_empty() {
                    lw
                } else {
                    lw + 1 + width(&row.args)
                }
            })
            .max();
        widest.map_or(0, |widest| (widest + 2).clamp(12, 34))
    }

    fn row_line(&self, styles: Styles, cells: usize, column: usize, index: usize) -> Line<'static> {
        let row = &self.rows[index];
        let selected = self.selected == Some(index);
        let session = self.kind == Kind::Session;
        let current = row.mark == Some(true);

        // Unified selection indicator: `▎ ` on the active row (matching Ctrl+B & /diff),
        // `✓ ` for non-selected current Session rows, blank otherwise.
        let pointer = match (selected, session && current) {
            (true, _) => "▎ ",
            (false, true) => "✓ ",
            (false, false) => "  ",
        };
        let mark = match row.mark {
            _ if session => "",
            Some(true) => "✓ ",
            Some(false) => "  ",
            None => "",
        };
        let label_style = match row.tone {
            Tone::Plain => {
                let base = if current && !selected {
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
        let detail_style = if selected {
            styles.text()
        } else {
            styles.dim()
        };
        let args_style = if selected {
            styles.text()
        } else {
            styles.meta()
        };
        let badge_style = if selected {
            styles.text()
        } else {
            styles.meta()
        };

        let room = cells.saturating_sub(POINTER + width(mark));
        let badge = sanitize(&row.badge);
        let badge_w = if !badge.is_empty() && room > width(&badge) + 12 {
            width(&badge)
        } else {
            0
        };
        let badge_reserve = if badge_w > 0 { badge_w + 2 } else { 0 };
        let body_room = room.saturating_sub(badge_reserve);

        let detail = sanitize(&row.detail);
        let detail = if session {
            let id = detail
                .split_once(" · ")
                .map_or(detail.as_ref(), |(id, _)| id);
            if body_room <= width(id) + 1 {
                wrap::truncate(id, body_room)
            } else {
                wrap::truncate(&detail, body_room.saturating_sub(1))
            }
        } else {
            detail.into_owned()
        };

        let args_clean = sanitize(&row.args);
        let mut spans = vec![
            Span::styled(pointer, styles.accent()),
            Span::styled(mark, styles.accent()),
        ];

        let used_in_body = match self.kind {
            Kind::Arrow => {
                let label = truncate(&row.label, body_room);
                let lw = width(&label);
                spans.push(Span::styled(label, label_style));
                let mut used = lw;
                if !args_clean.is_empty() && body_room > used + 3 {
                    let arg = wrap::truncate(&args_clean, body_room.saturating_sub(used + 1));
                    used += 1 + width(&arg);
                    spans.push(Span::raw(" "));
                    spans.push(Span::styled(arg, args_style));
                }
                if !detail.is_empty() && body_room > used + 2 {
                    let d = wrap::truncate(&detail, body_room.saturating_sub(used + 1));
                    used += 1 + width(&d);
                    spans.push(Span::raw(" "));
                    spans.push(Span::styled(d, detail_style));
                }
                used
            }
            Kind::Column => {
                let col_cap = column.min(body_room.saturating_sub(4).max(8));
                let label = truncate(&row.label, col_cap);
                let mut left_w = width(&label);
                spans.push(Span::styled(label, label_style));
                if !args_clean.is_empty() && col_cap > left_w + 2 {
                    let arg = wrap::truncate(&args_clean, col_cap.saturating_sub(left_w + 1));
                    left_w += 1 + width(&arg);
                    spans.push(Span::raw(" "));
                    spans.push(Span::styled(arg, args_style));
                }
                let mut used = left_w;
                let detail_room = body_room.saturating_sub(column);
                if !detail.is_empty() && detail_room > 6 {
                    let gap = column.saturating_sub(left_w).max(1);
                    let d = wrap::truncate(&detail, body_room.saturating_sub(left_w + gap));
                    used = left_w + gap + width(&d);
                    spans.push(Span::raw(" ".repeat(gap)));
                    spans.push(Span::styled(d, detail_style));
                }
                used
            }
            Kind::Session => {
                let label = truncate(&row.label, body_room.saturating_sub(width(&detail) + 2));
                let gap = body_room.saturating_sub(width(&label) + width(&detail));
                let used = width(&label) + gap + width(&detail);
                spans.push(Span::styled(label, label_style));
                if !detail.is_empty() {
                    spans.push(Span::raw(" ".repeat(gap)));
                    spans.push(Span::styled(detail, detail_style));
                }
                used
            }
        };

        if badge_w > 0 {
            let pad = room.saturating_sub(used_in_body + badge_w).max(2);
            spans.push(Span::raw(" ".repeat(pad)));
            spans.push(Span::styled(badge.into_owned(), badge_style));
        }

        let line = Line::from(spans);
        if selected {
            line.style(styles.highlight())
        } else {
            line
        }
    }
}

/// Draws `sheet` into `area`.
pub fn render(frame: &mut Frame, area: Rect, styles: Styles, sheet: &Sheet) {
    let _ = render_hit(frame, area, styles, sheet);
}

/// Draws `sheet` into `area` and returns its mouse hit geometry.
pub fn render_hit(frame: &mut Frame, area: Rect, styles: Styles, sheet: &Sheet) -> SheetHitArea {
    let (lines, list_offset, first, list_rows) = sheet.build_lines(styles, area.width, area.height);
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
    SheetHitArea {
        area,
        list_y: area.y.saturating_add(list_offset as u16),
        list_rows,
        first,
        total: sheet.rows.len(),
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
    fn the_frame_carries_the_title_above_and_the_hint_below() {
        let sheet = Sheet::new("Title", "enter open · esc close")
            .status("3 sessions")
            .rows(rows(2))
            .selected(Some(0));
        assert_eq!(sheet.height(), 4);
        let lines = capture(&sheet, 40);
        assert_eq!(lines[0], format!("─ Title {} 3 sessions ─", "─".repeat(19)));
        assert_eq!(lines[1], "▎ row 00 00");
        assert_eq!(lines[2], "  row 01 01");
        assert_eq!(
            lines[3],
            format!("─ enter open · esc close {}", "─".repeat(15))
        );
    }

    #[test]
    fn a_search_sheet_puts_the_unified_slash_prompt_under_the_title() {
        let sheet = Sheet::new("Model", "esc cancel")
            .search("gpt")
            .rows(rows(1));
        let lines = capture(&sheet, 30);
        assert_eq!(lines[1], " / gpt▌");
        assert!(lines[2].contains("row 00"), "{lines:?}");
    }

    #[test]
    fn the_window_scrolls_and_a_counter_appears() {
        let sheet = Sheet::new("Models", "hint")
            .rows(rows(25))
            .selected(Some(12));
        assert_eq!(sheet.height(), 1 + MAX_ROWS as u16 + 1 + 1);
        let lines = capture(&sheet, 30);
        assert!(lines[1].starts_with("  row 07"), "{:?}", lines[1]);
        assert!(lines[6].starts_with("▎ row 12"), "{:?}", lines[6]);
        assert_eq!(lines[11], "  (13/25)");
        assert!(lines[12].starts_with("─ hint ─"), "{:?}", lines[12]);
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
        assert!(lines.iter().any(|line| line.starts_with("▎ row 20")));
        assert!(lines[7].starts_with("─ hint ─"), "{:?}", lines[7]);
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
            text(&lines[1]),
            format!("▎ Fix the parser{}3 2h", " ".repeat(10))
        );
        assert_eq!(lines[1].style.bg, Some(palette.selection_bg));
        assert_eq!(lines[2].style.bg, None);
        assert_eq!(text(&lines[2]), format!("  Other{}1 now", " ".repeat(18)));
    }

    #[test]
    fn the_current_item_gets_a_check_and_the_others_stay_aligned() {
        let sheet = Sheet::new("Model", "").rows(vec![
            Row::new("gpt-6", "[openai]").mark(true),
            Row::new("glm-5", "[zai]").mark(false),
        ]);
        let lines = capture(&sheet, 40);
        assert_eq!(lines[1], "  ✓ gpt-6 [openai]");
        assert_eq!(lines[2], "    glm-5 [zai]");
    }

    #[test]
    fn column_details_line_up_and_support_args_and_badges() {
        let sheet = Sheet::new("Commands", "")
            .kind(Kind::Column)
            .rows(vec![
                Row::new("/model", "Switch model")
                    .args("[id]")
                    .badge("ctrl+l"),
                Row::new("/tasks", "Async tasks").badge("ctrl+b"),
            ])
            .selected(Some(1));
        let lines = capture(&sheet, 56);
        assert!(lines[1].contains("/model [id]"), "{:?}", lines[1]);
        assert!(lines[1].ends_with("ctrl+l"), "{:?}", lines[1]);
        assert!(lines[2].starts_with("▎ /tasks"), "{:?}", lines[2]);
        assert!(lines[2].ends_with("ctrl+b"), "{:?}", lines[2]);
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
        assert_eq!(lines[1].spans[2].style.fg, Some(palette.success));
        assert_eq!(lines[2].spans[2].style.fg, Some(palette.failure));
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
        let last = sheet.height() - 1;
        for (x, y) in [(0u16, 0u16), (19, 0), (0, last), (19, last)] {
            assert_eq!(buffer[(x, y)].symbol(), "─");
            assert_eq!(buffer[(x, y)].fg, palette.border);
        }
    }

    #[test]
    fn long_labels_are_cut_before_the_detail() {
        let sheet = Sheet::new("Resume", "")
            .kind(Kind::Session)
            .rows(vec![Row::new("a very long session title indeed", "3 2h")]);
        let lines = capture(&sheet, 24);
        assert_eq!(lines[1], "  a very long ses…  3 2h");
        assert_eq!(width(&lines[1]), 24);
    }

    #[test]
    fn oversized_session_details_keep_the_leading_id_within_the_row() {
        for metadata in ["long agent name ".repeat(20), "工作代理名称".repeat(20)] {
            let sheet = Sheet::new("Resume", "")
                .kind(Kind::Session)
                .rows(vec![Row::new(
                    "a very long session title indeed",
                    format!("1acdef · {metadata} · running 0m · current · now"),
                )]);
            for cells in [8, 9, 10, 24, 40, 80] {
                let lines = capture(&sheet, cells);
                let row = &lines[1];
                assert!(row.trim_start().starts_with("1acdef"), "{row:?}");
                assert_eq!(row.ends_with('…'), cells >= 10, "{row:?}");
                assert!(width(row) <= usize::from(cells), "{row:?}");
            }
        }
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
