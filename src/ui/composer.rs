//! Draws the composer: the rail, the draft with its paste chips and the cursor.
//! The editing model lives in [`crate::app::composer`].

use std::ops::Range;

use ratatui::text::{Line, Span};

use super::theme::Styles;
use crate::app::composer::Composer;
use crate::wrap;

/// The editor body is never shorter than this many rows.
pub const MIN_ROWS: usize = 4;

/// The editor body is never taller than this many rows; completions come on top.
const MAX_ROWS: usize = 12;

/// Rows the editor body may take on a terminal `height` rows tall.
pub fn max_rows(height: u16) -> usize {
    (usize::from(height) * 32 / 100).clamp(MIN_ROWS, MAX_ROWS)
}

/// Shown instead while a full-screen view owns the keys.
const VIEW_PLACEHOLDER: &str = "view open · esc to return";

/// Sanitized projection and the legal editing stops on it. A paste chip has
/// only two stops, so a click or Shift motion cannot split its hidden payload.
struct Shown {
    text: String,
    chips: Vec<Range<usize>>,
    stops: Vec<(usize, usize)>, // (display byte, buffer byte)
}

impl Shown {
    fn new(composer: &Composer) -> Self {
        let mut out = Self {
            text: String::new(),
            chips: Vec::new(),
            stops: vec![(0, 0)],
        };
        let mut at = 0;
        for (index, end) in composer
            .pastes()
            .iter()
            .map(|p| p.range.start)
            .chain([composer.text().len()])
            .enumerate()
        {
            let raw = &composer.text()[at..end];
            let safe = wrap::sanitize(raw);
            let base = out.text.len();
            // The common printable path is linear. Only sanitized control text
            // needs prefix projection to keep escape sequences indivisible.
            let unchanged = safe.as_ref() == raw;
            for (offset, grapheme) in raw.grapheme_indices(true) {
                let end = offset + grapheme.len();
                let display = if unchanged {
                    end
                } else {
                    wrap::sanitize(&raw[..end]).len()
                };
                out.stops.push((base + display, at + end));
            }
            out.text.push_str(&safe);
            if let Some(paste) = composer.pastes().get(index) {
                let start = out.text.len();
                out.text.push_str(&paste.label(index + 1));
                out.chips.push(start..out.text.len());
                out.stops.push((out.text.len(), paste.range.end));
                at = paste.range.end;
            }
        }
        out
    }

    fn display(&self, raw: usize) -> usize {
        self.stops
            .iter()
            .rev()
            .find(|(_, at)| *at <= raw)
            .map_or(0, |(at, _)| *at)
    }

    fn raw(&self, display: usize) -> usize {
        let index = self.stops.partition_point(|(at, _)| *at <= display);
        let left = self.stops[index.saturating_sub(1)];
        match self.stops.get(index) {
            Some(right) if display - left.0 > right.0 - display => right.1,
            _ => left.1,
        }
    }
}

use unicode_segmentation::UnicodeSegmentation;

struct Geometry {
    shown: Shown,
    rows: Vec<Range<usize>>,
    cursor: (usize, usize),
    target: usize,
    top: usize,
    blank: usize,
    limit: usize,
}

fn visible_end(text: &str, range: &Range<usize>, limit: usize) -> usize {
    let mut used = 0;
    for (at, grapheme) in text[range.clone()].grapheme_indices(true) {
        used += wrap::width(grapheme);
        if used > limit {
            return range.start + at;
        }
    }
    range.end
}

fn geometry(composer: &Composer, width: u16, max_rows: usize) -> Geometry {
    let limit = usize::from(width).max(3) - 2;
    let shown = Shown::new(composer);
    let caret = shown.display(composer.cursor());
    let mut shape = shown.text.clone().into_bytes();
    for chip in &shown.chips {
        for byte in &mut shape[chip.clone()] {
            if *byte == b' ' {
                *byte = b'_';
            }
        }
    }
    let shape = String::from_utf8(shape).expect("only ASCII spaces were replaced");
    let mut rows = Vec::new();
    let mut cursor = (0, 0);
    let mut offset = 0;
    for line in shape.split('\n') {
        let ranges = wrap::breaks(line, limit);
        let end = offset + line.len();
        if (offset..=end).contains(&caret) {
            let at = caret - offset;
            let index = ranges.iter().rposition(|r| r.start <= at).unwrap_or(0);
            let from = offset + ranges[index].start.min(at);
            cursor = (
                rows.len() + index,
                wrap::width(&shown.text[from..caret]).min(limit),
            );
        }
        rows.extend(ranges.into_iter().map(|r| offset + r.start..offset + r.end));
        offset = end + 1;
    }
    let target = rows.len().clamp(MIN_ROWS, max_rows.max(MIN_ROWS));
    let top = if rows.len() > target {
        composer
            .view_top
            .get()
            .min(rows.len() - target)
            .min(cursor.0)
            .max((cursor.0 + 1).saturating_sub(target))
    } else {
        0
    };
    let blank = target.saturating_sub(rows.len()) / 2;
    Geometry {
        shown,
        rows,
        cursor,
        target,
        top,
        blank,
        limit,
    }
}

/// Maps a cell in the rendered editor (including its rail/padding) back to a
/// grapheme or chip boundary. Rendering and hit-testing share all geometry.
pub fn hit_test(composer: &Composer, width: u16, max_rows: usize, row: u16, column: u16) -> usize {
    hit_test_row(composer, width, max_rows, row as isize, column)
}

/// A drag just outside the viewport reaches the adjacent wrapped row, allowing
/// the next frame to scroll the editor without moving the transcript.
pub fn hit_test_row(
    composer: &Composer,
    width: u16,
    max_rows: usize,
    row: isize,
    column: u16,
) -> usize {
    let g = geometry(composer, width, max_rows);
    let index =
        (g.top as isize + row - g.blank as isize).clamp(0, g.rows.len() as isize - 1) as usize;
    let range = &g.rows[index];
    let text = &g.shown.text[range.start..visible_end(&g.shown.text, range, g.limit)];
    let column = usize::from(column.saturating_sub(1)).min(g.limit);
    let mut used = 0;
    let mut display = range.start + text.len();
    for (at, grapheme) in text.grapheme_indices(true) {
        let cells = wrap::width(grapheme);
        if used + cells > column {
            display = range.start + at;
            break;
        }
        used += cells;
    }
    g.shown.raw(display)
}

/// The editor rows and caret (row, column), including padding and scrolling.
pub fn render(
    composer: &Composer,
    styles: Styles,
    width: u16,
    max_rows: usize,
    active: bool,
) -> (Vec<Line<'static>>, (u16, u16)) {
    let g = geometry(composer, width, max_rows);
    composer.view_top.set(g.top);
    let selection = composer
        .selection()
        .map(|r| g.shown.display(r.start)..g.shown.display(r.end));
    let mut body: Vec<Vec<Span<'static>>> = g
        .rows
        .iter()
        .map(|range| {
            let text = &g.shown.text[range.start..visible_end(&g.shown.text, range, g.limit)];
            let end = range.start + text.len();
            let mut boundaries = vec![range.start, end];
            for chip in &g.shown.chips {
                boundaries.extend([
                    chip.start.clamp(range.start, end),
                    chip.end.clamp(range.start, end),
                ]);
            }
            if let Some(selected) = &selection {
                boundaries.extend([
                    selected.start.clamp(range.start, end),
                    selected.end.clamp(range.start, end),
                ]);
            }
            boundaries.sort_unstable();
            boundaries.dedup();
            let mut spans: Vec<_> = boundaries
                .windows(2)
                .map(|pair| {
                    let from = pair[0];
                    let to = pair[1];
                    let mut style = if g.shown.chips.iter().any(|r| r.contains(&from)) {
                        styles.paste_chip()
                    } else {
                        styles.text()
                    };
                    if selection
                        .as_ref()
                        .is_some_and(|r| r.start < to && r.end > from)
                    {
                        style = style.patch(styles.selection());
                    }
                    Span::styled(g.shown.text[from..to].to_owned(), style)
                })
                .collect();
            // A selected newline has one visible cell even on an otherwise empty row.
            if g.shown.text.as_bytes().get(range.end) == Some(&b'\n')
                && selection.as_ref().is_some_and(|r| r.contains(&range.end))
            {
                spans.push(Span::styled(" ", styles.selection()));
            }
            spans
        })
        .collect();
    if composer.is_empty() && !active {
        body = vec![vec![Span::styled(
            wrap::truncate(VIEW_PLACEHOLDER, g.limit),
            styles.faint(),
        )]];
    }
    let width = usize::from(width).max(3);
    let mut lines: Vec<_> = (0..g.blank)
        .map(|_| row(Vec::new(), width, styles))
        .collect();
    lines.extend(
        body.into_iter()
            .skip(g.top)
            .take(g.target)
            .map(|spans| row(spans, width, styles)),
    );
    lines.resize_with(g.target, || row(Vec::new(), width, styles));
    if !active {
        for span in lines.iter_mut().flat_map(|line| &mut line.spans) {
            span.style = span.style.patch(styles.faint());
        }
    }
    let cursor = (
        (g.blank + g.cursor.0 - g.top) as u16,
        (1 + g.cursor.1) as u16,
    );
    (lines, cursor)
}

/// One editor row exactly `width` cells wide: the rail, then `spans` on the
/// composer tint up to the last column.
pub fn row(spans: Vec<Span<'static>>, width: usize, styles: Styles) -> Line<'static> {
    let used: usize = spans.iter().map(|span| wrap::width(&span.content)).sum();
    let mut out = vec![Span::styled("▎", styles.user_rail())];
    out.extend(spans.into_iter().map(|span| {
        let style = styles.composer().patch(span.style);
        span.style(style)
    }));
    out.push(Span::styled(
        " ".repeat(width.saturating_sub(1 + used)),
        styles.composer(),
    ));
    Line::from(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn composer(text: &str) -> Composer {
        let mut composer = Composer::new();
        composer.set_text(text);
        composer
    }

    fn big_paste(lines: usize) -> String {
        (0..lines).map(|i| format!("line {i}\n")).collect()
    }

    fn styles() -> Styles {
        Styles::of(crate::ui::theme::Palette::dark())
    }

    /// Typed key by key, so control characters skip the paste normalization.
    fn typed(text: &str) -> Composer {
        let mut c = Composer::new();
        for ch in text.chars() {
            c.type_char(ch);
        }
        c
    }

    fn drawn(c: &Composer, width: u16) -> (Vec<String>, (u16, u16)) {
        let (lines, caret) = render(c, styles(), width, 12, true);
        let rows = lines
            .iter()
            .map(|line| line.to_string().trim_end().to_owned())
            .collect();
        (rows, caret)
    }

    #[test]
    fn click_geometry_round_trips_rendered_carets_including_unicode_wraps_and_scroll() {
        for text in [
            "",
            "abc  ",
            "a中e\u{301}👨‍👩‍👧‍👦z",
            "aaa bbb ccc ddd",
            "one\n\nthree",
            "row\n".repeat(20).as_str(),
        ] {
            for width in [8, 16, 50] {
                let mut c = composer(text);
                loop {
                    let (_, caret) = render(&c, styles(), width, 4, true);
                    let at = hit_test(&c, width, 4, caret.0, caret.1);
                    assert_eq!(at, c.cursor(), "{text:?} width={width} caret={caret:?}");
                    if c.cursor() == 0 {
                        break;
                    }
                    c.move_left();
                }
            }
        }
    }

    #[test]
    fn a_paste_chip_click_snaps_to_its_nearest_edge() {
        let mut c = composer("see ");
        let payload = big_paste(12);
        c.insert(&payload);
        let (_, caret) = render(&c, styles(), 60, 12, true);
        assert_eq!(hit_test(&c, 60, 12, caret.0, 6), 4);
        assert_eq!(hit_test(&c, 60, 12, caret.0, caret.1 - 1), c.text().len());
    }

    #[test]
    fn selected_unicode_and_newlines_are_highlighted_in_every_palette() {
        let mut c = composer("a中\n\nlast");
        c.place_cursor(1, false);
        c.place_cursor("a中\n\n".len(), true);
        for palette in [
            crate::ui::theme::Palette::dark(),
            crate::ui::theme::Palette::light(),
            crate::ui::theme::Palette::plain(),
        ] {
            let styles = Styles::of(palette);
            let (rows, _) = render(&c, styles, 40, 12, true);
            let selected: String = rows
                .iter()
                .flat_map(|r| &r.spans)
                .filter(|s| {
                    s.style
                        == styles
                            .composer()
                            .patch(styles.text())
                            .patch(styles.selection())
                        || s.style == styles.composer().patch(styles.selection())
                })
                .map(|s| s.content.as_ref())
                .collect();
            assert_eq!(selected, "中  ");
        }
    }

    #[test]
    fn an_escape_sequence_is_shown_as_text_and_never_reaches_the_terminal() {
        let (rows, _) = drawn(&typed("safe\u{1b}[31mred"), 40);
        let joined = rows.join("\n");
        assert!(
            !joined.contains('\u{1b}'),
            "no escape survives into the frame: {joined:?}"
        );
        assert!(joined.contains("safe"), "{joined:?}");
        assert!(joined.contains("red"), "{joined:?}");
    }

    #[test]
    fn the_cursor_keeps_its_cell_after_sanitizing() {
        let (_, plain) = drawn(&typed("ab"), 40);
        assert_eq!(plain, (1, 3), "second row; the rail plus two cells");

        // A tab shows as four cells, so the caret after it moves by four.
        let mut tabbed = typed("ab\tc");
        tabbed.move_left();
        let (_, caret) = drawn(&tabbed, 40);
        assert_eq!(caret, (1, 7), "the rail, ab and the tab's four cells");

        // An escape sequence collapses to one picture glyph; the caret after it
        // counts the visible cell, not the eight source bytes.
        let (rows, escaped) = drawn(&typed("a\u{1b}[31mb"), 40);
        assert_eq!(escaped, (1, 3), "the escape takes no cell");
        assert_eq!(rows[1], "▎ab", "the escape is dropped, not painted");
    }

    #[test]
    fn a_control_character_becomes_a_picture() {
        let (rows, _) = drawn(&typed("a\u{7}b"), 40);
        let joined = rows.join("\n");
        assert!(joined.contains('\u{2407}'), "{joined:?}");
    }

    #[test]
    fn the_cursor_lands_after_a_large_paste() {
        let mut c = composer("see ");
        c.insert(&big_paste(12));
        let (rows, cursor) = drawn(&c, 60);
        assert_eq!(rows[1], "▎see [paste #1 +12 lines]", "{rows:?}");
        assert_eq!(cursor, (1, 1 + "see [paste #1 +12 lines]".len() as u16));
        let (lines, _) = render(&c, styles(), 60, 12, true);
        let chip = lines[1]
            .spans
            .iter()
            .find(|span| span.content.starts_with("[paste"))
            .expect("the chip is its own span");
        assert_eq!(chip.style, styles().composer().patch(styles().paste_chip()));
    }

    #[test]
    fn the_cursor_lands_at_the_end_of_a_multi_line_paste() {
        let mut c = composer("head ");
        c.insert("one\ntwo\nthree");
        let (rows, cursor) = drawn(&c, 40);
        assert_eq!(rows[..3], ["▎head one", "▎two", "▎three"]);
        assert_eq!(cursor, (2, 6));
    }

    #[test]
    fn the_editor_pads_short_drafts_and_starts_text_after_the_rail() {
        let (rows, cursor) = drawn(&Composer::new(), 50);
        assert_eq!(rows.len(), MIN_ROWS);
        assert!(rows.iter().all(|row| row == "▎"), "no empty-draft hint");
        assert_eq!(cursor, (1, 1), "the cursor sits on the second row");
        let (lines, _) = render(&Composer::new(), styles(), 50, 12, true);
        assert!(lines.iter().all(|line| line.width() == 50), "full rows");
    }

    #[test]
    fn an_empty_editor_has_no_hint_at_any_width() {
        for width in [3, 8, 30, 80] {
            let (rows, cursor) = drawn(&Composer::new(), width);
            assert!(rows.iter().all(|row| row == "▎"), "{width}: {rows:?}");
            assert_eq!(cursor, (1, 1), "{width}: empty-draft caret");
        }
    }

    #[test]
    fn clearing_a_draft_leaves_the_editor_blank() {
        let mut c = composer("hello 中文");
        let (rows, _) = drawn(&c, 50);
        assert_eq!(rows[1], "▎hello 中文");

        c.set_text("");
        let (rows, cursor) = drawn(&c, 50);
        assert!(rows.iter().all(|row| row == "▎"), "{rows:?}");
        assert_eq!(cursor, (1, 1));
    }

    #[test]
    fn a_draft_wraps_by_word_and_the_cursor_follows_the_wrapped_word() {
        // The cursor never sits on the row a word left when it wrapped.
        let (rows, cursor) = drawn(&composer("hello wonderful"), 14);
        assert_eq!(rows[1..3], ["▎hello", "▎wonderful"]);
        assert_eq!(cursor, (2, 10));
    }

    #[test]
    fn a_tall_draft_scrolls_to_keep_the_cursor_visible() {
        let lines: Vec<String> = (0..20).map(|index| format!("line {index}")).collect();
        let mut c = composer(&lines.join("\n"));
        let (rows, cursor) = drawn(&c, 40);
        assert_eq!(rows.len(), 12, "capped");
        assert_eq!(rows[11], "▎line 19");
        assert_eq!(cursor.0, 11, "typing at the end keeps the last row");
        for _ in 0..5 {
            c.move_up();
        }
        let (rows, cursor) = drawn(&c, 40);
        assert_eq!(
            rows[11], "▎line 19",
            "moving inside the window does not scroll"
        );
        assert_eq!(cursor.0, 6);
        for _ in 0..10 {
            c.move_up();
        }
        let (rows, cursor) = drawn(&c, 40);
        assert_eq!((rows[0].as_str(), cursor.0), ("▎line 4", 0));
    }

    #[test]
    fn the_body_height_follows_the_terminal() {
        assert_eq!(max_rows(10), MIN_ROWS);
        assert_eq!(max_rows(30), 9);
        assert_eq!(max_rows(100), 12);
    }
}
