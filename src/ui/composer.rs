//! Draws the composer: the rail, the draft with its paste chips and the cursor.
//! The editing model lives in [`crate::app::composer`].

use std::ops::Range;

use ratatui::text::{Line, Span};

use super::theme::Styles;
use super::wrap;
use crate::app::composer::Composer;

/// The editor body is never shorter than this many rows.
pub const MIN_ROWS: usize = 4;

/// The editor body is never taller than this many rows; completions come on top.
const MAX_ROWS: usize = 12;

/// Rows the editor body may take on a terminal `height` rows tall.
pub fn max_rows(height: u16) -> usize {
    (usize::from(height) * 32 / 100).clamp(MIN_ROWS, MAX_ROWS)
}

const PLACEHOLDER: &str = "Ask anything · / commands · @ files";

/// The draft as drawn: sanitized text with chips in place of payloads, the
/// chips' byte ranges in it and the cursor's byte offset in it.
fn shown(composer: &Composer) -> (String, Vec<Range<usize>>, usize) {
    let (text, pastes, cursor) = (composer.text(), composer.pastes(), composer.cursor());
    let mut shown = String::with_capacity(text.len());
    let mut chips = Vec::new();
    let mut caret = None;
    let mut at = 0;
    let ends = pastes.iter().map(|paste| paste.range.start);
    for (index, end) in ends.chain([text.len()]).enumerate() {
        if caret.is_none() && (at..=end).contains(&cursor) {
            // Sanitizing the prefix keeps an escape sequence whole; a cursor
            // inside one lands on the cell before it.
            caret = Some(shown.len() + wrap::sanitize(&text[at..cursor]).len());
        }
        shown.push_str(&wrap::sanitize(&text[at..end]));
        let Some(paste) = pastes.get(index) else {
            break;
        };
        let start = shown.len();
        shown.push_str(&paste.label(index + 1));
        chips.push(start..shown.len());
        at = paste.range.end;
    }
    let caret = caret.unwrap_or(shown.len());
    (shown, chips, caret)
}

/// The editor body for a band `width` cells wide: the rail at column 0 and the
/// draft from column 1, padded or scrolled to between [`MIN_ROWS`] and
/// `max_rows` rows. Returns the rows and the cursor as (row, column).
pub fn render(
    composer: &Composer,
    styles: Styles,
    width: u16,
    max_rows: usize,
) -> (Vec<Line<'static>>, (u16, u16)) {
    let width = usize::from(width).max(3);
    // One cell stays free after the text, so a cursor at the end of a full row
    // does not wrap.
    let limit = width - 2;
    let (shown, chips, caret) = shown(composer);
    // A chip wraps as one word: its spaces are masked while finding breaks.
    let mut shape = shown.clone().into_bytes();
    for chip in &chips {
        for byte in &mut shape[chip.clone()] {
            if *byte == b' ' {
                *byte = b'_';
            }
        }
    }
    let shape = String::from_utf8(shape).expect("only ASCII spaces were replaced");

    let mut body: Vec<Vec<Span<'static>>> = Vec::new();
    let mut cursor = (0, 0);
    let mut offset = 0;
    for line in shape.split('\n') {
        let ranges = wrap::breaks(line, limit);
        let end = offset + line.len();
        if (offset..=end).contains(&caret) {
            let at = caret - offset;
            let index = ranges
                .iter()
                .rposition(|range| range.start <= at)
                .unwrap_or(0);
            let from = offset + ranges[index].start.min(at);
            cursor = (
                body.len() + index,
                wrap::width(&shown[from..caret]).min(limit),
            );
        }
        for range in ranges {
            let start = offset + range.start;
            let text = shown[start..offset + range.end].trim_end();
            body.push(styled(&shown, &chips, start..start + text.len(), styles));
        }
        offset = end + 1;
    }
    if composer.is_empty() {
        let hint = wrap::truncate(PLACEHOLDER, limit);
        body = vec![vec![Span::styled(hint, styles.faint())]];
    }

    let rows = body.len();
    let target = rows.clamp(MIN_ROWS, max_rows.max(MIN_ROWS));
    let top = if rows > target {
        let top = composer
            .view_top
            .get()
            .min(rows - target)
            .min(cursor.0)
            .max((cursor.0 + 1).saturating_sub(target));
        composer.view_top.set(top);
        top
    } else {
        0
    };
    // A short draft sits in the middle, a single line on the second row.
    let blank = target.saturating_sub(rows) / 2;
    let mut lines: Vec<Line<'static>> =
        (0..blank).map(|_| row(Vec::new(), width, styles)).collect();
    lines.extend(
        body.into_iter()
            .skip(top)
            .take(target)
            .map(|spans| row(spans, width, styles)),
    );
    lines.resize_with(target, || row(Vec::new(), width, styles));
    let cursor = (blank + cursor.0 - top, 1 + cursor.1);
    (lines, (cursor.0 as u16, cursor.1 as u16))
}

/// The spans of `shown[range]`, with the parts inside a chip styled as one.
fn styled(
    shown: &str,
    chips: &[Range<usize>],
    range: Range<usize>,
    styles: Styles,
) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut at = range.start;
    for chip in chips
        .iter()
        .filter(|chip| chip.start < range.end && chip.end > range.start)
    {
        let start = chip.start.max(range.start);
        let end = chip.end.min(range.end);
        if at < start {
            spans.push(Span::styled(shown[at..start].to_owned(), styles.text()));
        }
        spans.push(Span::styled(
            shown[start..end].to_owned(),
            styles.paste_chip(),
        ));
        at = end;
    }
    if at < range.end {
        spans.push(Span::styled(shown[at..range.end].to_owned(), styles.text()));
    }
    spans
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
        let (lines, caret) = render(c, styles(), width, 12);
        let rows = lines
            .iter()
            .map(|line| line.to_string().trim_end().to_owned())
            .collect();
        (rows, caret)
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
        let (lines, _) = render(&c, styles(), 60, 12);
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
        assert_eq!(rows[1], format!("▎{PLACEHOLDER}"));
        assert_eq!(cursor, (1, 1), "the cursor sits on the second row");
        let (lines, _) = render(&Composer::new(), styles(), 50, 12);
        assert!(lines.iter().all(|line| line.width() == 50), "full rows");
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
