//! Turning a mouse drag into transcript text, read from the rows the last frame
//! showed, so copying works on what the user sees and stays exact while a turn
//! streams.

use std::ops::Range;

use ratatui::layout::Rect;
use unicode_segmentation::UnicodeSegmentation;

use crate::wrap::width;

/// Two terminal cells, in drawing order; both ends are selected.
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
}

/// The visible text of `drag` inside `area`, joined with newlines; `row`
/// gives a screen row's text and the cells of it that are content.
pub fn text(row: impl Fn(u16) -> Option<(String, Range<u16>)>, area: Rect, drag: Drag) -> String {
    let (from, to) = drag.ordered();
    let lines: Vec<String> = (from.1..=to.1)
        .filter_map(|screen_row| {
            let (text, content) = row(screen_row)?;
            let span = row_span(screen_row, drag, area, content)?;
            Some(slice_cells(&text, span.start, span.end))
        })
        .collect();
    lines.join("\n").trim_end().to_owned()
}

/// One row sliced to `[from, to)` terminal cells. Columns are cells, so wide
/// characters are never split.
fn slice_cells(text: &str, from: u16, to: u16) -> String {
    let mut out = String::new();
    let mut column = 0u16;
    for grapheme in text.graphemes(true) {
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

/// The content cells a drag covers on one screen row, counted from `area`'s
/// left edge.
pub fn row_span(row: u16, drag: Drag, area: Rect, content: Range<u16>) -> Option<Range<u16>> {
    let (from, to) = drag.ordered();
    if row < from.1 || row > to.1 || row < area.y || row >= area.bottom() {
        return None;
    }
    let start = if row == from.1 {
        from.0.max(area.x)
    } else {
        area.x
    };
    let end = if row == to.1 {
        (to.0 + 1).min(area.right())
    } else {
        area.right()
    };
    let start = (start - area.x).max(content.start);
    let end = (end - area.x).min(content.end);
    (end > start).then_some(start..end)
}

/// The cells of the word under `column`, as a double-click selects it: paths
/// and kebab-case names stay whole. `None` past the end of the row.
pub fn word_at(text: &str, column: u16) -> Option<Range<u16>> {
    struct Segment {
        cells: Range<u16>,
        word: bool,
        joiner: bool,
    }
    let mut start = 0u16;
    let segments: Vec<Segment> = text
        .split_word_bounds()
        .map(|segment| {
            let end = start + width(segment) as u16;
            let joiner = matches!(segment, "/" | "-");
            let word = joiner || segment.chars().any(char::is_alphanumeric);
            let cells = start..end;
            start = end;
            Segment {
                cells,
                word,
                joiner,
            }
        })
        .collect();
    let clicked = segments
        .iter()
        .position(|segment| segment.cells.contains(&column))?;
    let joins =
        |left: &Segment, right: &Segment| left.word && right.word && (left.joiner || right.joiner);
    let mut first = clicked;
    while first > 0 && joins(&segments[first - 1], &segments[first]) {
        first -= 1;
    }
    let mut last = clicked;
    while last + 1 < segments.len() && joins(&segments[last], &segments[last + 1]) {
        last += 1;
    }
    Some(segments[first].cells.start..segments[last].cells.end)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The text lookup of a screen whose first row is `top`.
    fn screen(rows: &[&str], top: u16) -> impl Fn(u16) -> Option<(String, Range<u16>)> {
        let rows: Vec<String> = rows.iter().map(|row| (*row).to_owned()).collect();
        move |row| Some((rows.get(row.checked_sub(top)? as usize)?.clone(), ALL))
    }

    /// Every cell of a row is content.
    const ALL: Range<u16> = 0..u16::MAX;

    fn area() -> Rect {
        Rect::new(0, 0, 40, 10)
    }

    #[test]
    fn a_drag_across_one_line_copies_that_slice() {
        let screen = screen(&["hello world"], 0);
        let drag = Drag {
            from: (2, 0),
            to: (7, 0),
        };
        assert_eq!(text(&screen, area(), drag), "llo wo", "both ends included");
    }

    #[test]
    fn a_drag_across_rows_joins_them_with_newlines() {
        let screen = screen(&["first", "second"], 0);
        let drag = Drag {
            from: (1, 0),
            to: (3, 1),
        };
        assert_eq!(text(&screen, area(), drag), "irst\nseco");
    }

    #[test]
    fn a_backwards_drag_copies_in_reading_order() {
        let screen = screen(&["first", "second"], 0);
        let forwards = Drag {
            from: (1, 0),
            to: (3, 1),
        };
        let backwards = Drag {
            from: (3, 1),
            to: (1, 0),
        };
        assert_eq!(
            text(&screen, area(), forwards),
            text(&screen, area(), backwards)
        );
    }

    #[test]
    fn rows_without_content_contribute_nothing() {
        let screen = screen(&["one"], 0);
        let drag = Drag {
            from: (0, 5),
            to: (2, 7),
        };
        assert_eq!(text(&screen, area(), drag), "");
    }

    #[test]
    fn a_drag_past_the_last_block_stops_there() {
        let screen = screen(&["only"], 0);
        let drag = Drag {
            from: (0, 0),
            to: (39, 9),
        };
        assert_eq!(text(&screen, area(), drag), "only");
    }

    #[test]
    fn a_recorded_frame_copies_the_rows_the_terminal_shows() {
        let screen = screen(&["first row", "second"], 3);
        let area = Rect::new(0, 3, 20, 2);
        let drag = Drag {
            from: (0, 3),
            to: (20, 4),
        };
        assert_eq!(text(&screen, area, drag), "first row\nsecond");
    }

    #[test]
    fn wide_characters_are_sliced_by_cells_not_bytes() {
        let screen = screen(&["中文ab"], 0);
        let drag = Drag {
            from: (2, 0),
            to: (3, 0),
        };
        assert_eq!(text(&screen, area(), drag), "文");
    }

    #[test]
    fn the_gutter_is_neither_copied_nor_counted() {
        let screen = screen(&["hello world"], 0);
        let area = Rect::new(1, 0, 40, 10);
        let drag = Drag {
            from: (0, 0),
            to: (5, 0),
        };
        assert_eq!(text(&screen, area, drag), "hello");
        assert_eq!(row_span(0, drag, area, ALL), Some(0..5));
    }

    #[test]
    fn the_highlight_span_covers_the_dragged_cells() {
        let drag = Drag {
            from: (4, 1),
            to: (9, 3),
        };
        assert_eq!(row_span(0, drag, area(), ALL), None, "above the drag");
        assert_eq!(row_span(1, drag, area(), ALL), Some(4..40));
        assert_eq!(row_span(2, drag, area(), ALL), Some(0..40));
        assert_eq!(row_span(3, drag, area(), ALL), Some(0..10));
        assert_eq!(row_span(4, drag, area(), ALL), None, "below the drag");
    }

    #[test]
    fn a_double_click_word_keeps_paths_and_kebab_names_whole() {
        let row = "run src/ui/mod.rs --low-stock now.";
        let pick = |column: u16| {
            word_at(row, column).map(|cells| &row[cells.start as usize..cells.end as usize])
        };
        assert_eq!(pick(0), Some("run"));
        assert_eq!(pick(8), Some("src/ui/mod.rs"));
        assert_eq!(pick(20), Some("--low-stock"));
        assert_eq!(pick(30), Some("now"));
        assert_eq!(pick(3), Some(" "), "a space is its own run");
        assert_eq!(pick(80), None);
    }

    #[test]
    fn a_click_has_a_one_cell_span() {
        let drag = Drag {
            from: (3, 2),
            to: (3, 2),
        };
        assert_eq!(row_span(2, drag, area(), ALL), Some(3..4));
    }

    #[test]
    fn only_the_content_cells_of_a_row_are_taken() {
        let drag = Drag {
            from: (0, 0),
            to: (39, 1),
        };
        let rows = |row: u16| {
            let text = ["▎ $ cargo test", "▎ ... (3 more lines)"][row as usize];
            Some((text.to_owned(), [2..14, 0..0][row as usize].clone()))
        };
        assert_eq!(row_span(0, drag, area(), 2..14), Some(2..14));
        assert_eq!(row_span(1, drag, area(), 0..0), None);
        assert_eq!(text(rows, area(), drag), "$ cargo test");
    }
}
