//! Turning a mouse drag into transcript text. The renderer stores the rows it
//! actually drew, so copying works on what the user sees rather than on a
//! second layout pass — and it stays exact while a turn streams.

use std::ops::Range;

use ratatui::layout::Rect;

use super::wrap::width;

/// Two terminal cells, in drawing order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Drag {
    pub from: (u16, u16),
    pub to: (u16, u16),
}

impl Drag {
    /// The same drag with its ends in reading order.
    pub fn ordered(self) -> ((u16, u16), (u16, u16)) {
        let (from, to) = (self.from, self.to);
        if (from.1, from.0) <= (to.1, to.0) {
            (from, to)
        } else {
            (to, from)
        }
    }

    /// True when the pointer never left the cell it started on: a click, not a
    /// selection.
    pub fn is_click(self) -> bool {
        self.from == self.to
    }
}

/// The plain text of every row the transcript drew, indexed by screen row.
#[derive(Clone, Debug, Default)]
pub struct Screen {
    /// First screen row of `rows`.
    pub top: u16,
    pub rows: Vec<String>,
}

impl Screen {
    pub fn set(&mut self, top: u16, rows: Vec<String>) {
        self.top = top;
        self.rows = rows;
    }

    pub fn row(&self, row: u16) -> Option<&str> {
        let index = row.checked_sub(self.top)? as usize;
        self.rows.get(index).map(String::as_str)
    }
}

/// The visible text of `drag` inside `area`, joined with newlines.
pub fn text(screen: &Screen, area: Rect, drag: Drag) -> String {
    if drag.is_click() || area.width == 0 || area.height == 0 {
        return String::new();
    }
    let (from, to) = drag.ordered();
    let mut lines: Vec<String> = Vec::new();
    for row in from.1..=to.1 {
        if row < area.y || row >= area.bottom() {
            continue;
        }
        let Some(text) = screen.row(row) else {
            continue;
        };
        let column_from = if row == from.1 {
            from.0.max(area.x)
        } else {
            area.x
        };
        let column_to = if row == to.1 { to.0 } else { area.right() };
        lines.push(slice_cells(text, column_from, column_to));
    }
    lines.join("\n").trim_end().to_owned()
}

/// One row sliced to `[from, to)` terminal cells. Columns are cells, so wide
/// characters are never split.
fn slice_cells(text: &str, from: u16, to: u16) -> String {
    let mut out = String::new();
    let mut column = 0u16;
    for grapheme in unicode_segmentation::UnicodeSegmentation::graphemes(text, true) {
        let cells = width(grapheme) as u16;
        let end = column + cells.max(1);
        if end > from && column < to {
            out.push_str(grapheme);
        }
        column = end;
        if column >= to {
            break;
        }
    }
    out.trim_end().to_owned()
}

/// The cell range a drag covers on one row, for the selection highlight.
pub fn row_span(row: u16, drag: Drag, area: Rect) -> Option<Range<u16>> {
    let (from, to) = drag.ordered();
    if row < from.1 || row > to.1 || row < area.y || row >= area.bottom() {
        return None;
    }
    let start = if row == from.1 {
        from.0.max(area.x)
    } else {
        area.x
    };
    let end = if row == to.1 { to.0 + 1 } else { area.right() };
    (end > start).then_some(start..end)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(rows: &[&str]) -> Screen {
        Screen {
            top: 0,
            rows: rows.iter().map(|row| (*row).to_owned()).collect(),
        }
    }

    fn area() -> Rect {
        Rect::new(0, 0, 40, 10)
    }

    #[test]
    fn a_drag_across_one_line_copies_that_slice() {
        let screen = screen(&["hello world"]);
        let drag = Drag {
            from: (2, 0),
            to: (7, 0),
        };
        assert_eq!(text(&screen, area(), drag), "llo w");
    }

    #[test]
    fn a_drag_across_rows_joins_them_with_newlines() {
        let screen = screen(&["first", "second"]);
        let drag = Drag {
            from: (1, 0),
            to: (3, 1),
        };
        assert_eq!(text(&screen, area(), drag), "irst\nsec");
    }

    #[test]
    fn a_backwards_drag_copies_in_reading_order() {
        let screen = screen(&["first", "second"]);
        let forwards = Drag {
            from: (1, 0),
            to: (3, 1),
        };
        let backwards = Drag {
            from: (3, 1),
            to: (1, 0),
        };
        assert_eq!(text(&screen, area(), forwards), text(&screen, area(), backwards));
    }

    #[test]
    fn a_click_selects_nothing() {
        let screen = screen(&["hello"]);
        let drag = Drag {
            from: (2, 0),
            to: (2, 0),
        };
        assert!(drag.is_click(), "a click must not copy a character");
    }

    #[test]
    fn rows_without_content_contribute_nothing() {
        let screen = screen(&["one"]);
        let drag = Drag {
            from: (0, 5),
            to: (2, 7),
        };
        assert_eq!(text(&screen, area(), drag), "");
    }

    #[test]
    fn a_drag_past_the_last_block_stops_there() {
        let screen = screen(&["only"]);
        let drag = Drag {
            from: (0, 0),
            to: (39, 9),
        };
        assert_eq!(text(&screen, area(), drag), "only");
    }

    #[test]
    fn wide_characters_are_sliced_by_cells_not_bytes() {
        let screen = screen(&["中文ab"]);
        let drag = Drag {
            from: (2, 0),
            to: (4, 0),
        };
        assert_eq!(text(&screen, area(), drag), "文");
    }

    #[test]
    fn the_highlight_span_covers_the_dragged_cells() {
        let drag = Drag {
            from: (4, 1),
            to: (9, 3),
        };
        assert_eq!(row_span(0, drag, area()), None, "above the drag");
        assert_eq!(row_span(1, drag, area()), Some(4..40));
        assert_eq!(row_span(2, drag, area()), Some(0..40));
        assert_eq!(row_span(3, drag, area()), Some(0..10));
        assert_eq!(row_span(4, drag, area()), None, "below the drag");
    }

    #[test]
    fn a_click_has_a_one_cell_span() {
        let drag = Drag {
            from: (3, 2),
            to: (3, 2),
        };
        assert_eq!(row_span(2, drag, area()), Some(3..4));
    }
}
