//! Word wrapping, display width and terminal-safe text.
//!
//! Everything that reaches the terminal passes through [`sanitize`] first:
//! ANSI sequences are removed and control characters become visible pictures,
//! so agent or tool output can never move the cursor or recolor the frame.
//!
//! Wrapping breaks at whitespace, between wide (CJK) characters and inside
//! overlong tokens, and never splits a grapheme.

use std::borrow::Cow;
use std::ops::Range;

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Display width of `text` in terminal cells.
pub fn width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

/// Display width of the first `offset` bytes of `text`.
pub fn width_upto(text: &str, offset: usize) -> usize {
    UnicodeWidthStr::width(&text[..offset.min(text.len())])
}

/// True for characters that occupy two cells (CJK ideographs, most emoji).
pub fn is_wide(ch: char) -> bool {
    ch.width() == Some(2)
}

/// Splits a line into `width`-cell chunks, ignoring word boundaries. Used for
/// code and for words that do not fit on a line of their own.
pub fn char_breaks(line: &str, cell_limit: usize) -> Vec<Range<usize>> {
    let limit = cell_limit.max(1);
    let mut out = Vec::new();
    let mut start = 0;
    let mut col = 0;
    for (offset, grapheme) in line.grapheme_indices(true) {
        let cells = width(grapheme);
        if start < offset && col + cells > limit {
            out.push(start..offset);
            start = offset;
            col = 0;
        }
        col += cells;
    }
    out.push(start..line.len());
    out
}

/// Visual line ranges (byte offsets) of one logical line wrapped at `width`
/// cells. Whitespace that a break lands on is left at the end of the range;
/// [`wrap_line`] trims it.
pub fn breaks(line: &str, cell_limit: usize) -> Vec<Range<usize>> {
    let limit = cell_limit.max(1);
    if line.is_empty() {
        return std::iter::once(0..0).collect();
    }
    let units = units(line, limit);
    if units.is_empty() {
        return std::iter::once(0..line.len()).collect();
    }
    let mut out = Vec::new();
    let mut start = 0;
    let mut end = 0;
    let mut col = 0;
    for (gap, word) in units {
        let gap_width = width(&line[gap.clone()]);
        let word_width = width(&line[word.clone()]);
        if col > 0 && col + gap_width + word_width > limit {
            out.push(start..end);
            start = word.start;
            col = 0;
        }
        if col == 0 && start != word.start {
            // Keep leading indentation when the word still fits beside it.
            start = if gap_width + word_width <= limit {
                gap.start
            } else {
                word.start
            };
        }
        col += gap_width + word_width;
        end = word.end;
    }
    out.push(start..line.len());
    out
}

/// `(preceding whitespace, word)` pairs, with words wider than `limit` split
/// into cell-sized chunks so every unit fits on one line.
fn units(line: &str, limit: usize) -> Vec<(Range<usize>, Range<usize>)> {
    let mut out = Vec::new();
    let mut gap_start = 0;
    let mut pos = 0;
    while let Some(offset) = line[pos..].find(|ch: char| !ch.is_whitespace()) {
        let word_start = pos + offset;
        let word_end = line[word_start..]
            .find(char::is_whitespace)
            .map_or(line.len(), |offset| word_start + offset);
        if width(&line[word_start..word_end]) <= limit {
            out.push((gap_start..word_start, word_start..word_end));
        } else {
            for (index, chunk) in char_breaks(&line[word_start..word_end], limit)
                .into_iter()
                .enumerate()
            {
                let gap = if index == 0 {
                    gap_start..word_start
                } else {
                    word_start..word_start
                };
                out.push((gap, word_start + chunk.start..word_start + chunk.end));
            }
        }
        gap_start = word_end;
        pos = word_end;
    }
    out
}

/// Wraps one logical line, trimming the whitespace each break lands on.
pub fn wrap_line(line: &str, cell_limit: usize) -> Vec<String> {
    breaks(line, cell_limit)
        .into_iter()
        .map(|range| line[range].trim_end().to_owned())
        .collect()
}

/// Wraps plain text, keeping explicit newlines.
pub fn wrap(text: &str, cell_limit: usize) -> Vec<String> {
    text.split('\n')
        .flat_map(|line| wrap_line(line, cell_limit))
        .collect()
}

/// Wraps styled spans into lines no wider than `width`, trimming the
/// whitespace at each break and merging spans that share a style.
pub fn wrap_spans(spans: &[Span<'static>], cell_limit: usize) -> Vec<Line<'static>> {
    let text: String = spans.iter().map(|span| span.content.as_ref()).collect();
    breaks(&text, cell_limit)
        .into_iter()
        .map(|range| line_from(&text, range, spans))
        .collect()
}

/// Wraps plain text into styled lines.
pub fn wrapped(text: &str, cell_limit: usize, style: Style) -> Vec<Line<'static>> {
    text.split('\n')
        .flat_map(|line| {
            breaks(line, cell_limit).into_iter().map(move |range| {
                Line::from(Span::styled(line[range].trim_end().to_owned(), style))
            })
        })
        .collect()
}

fn line_from(text: &str, range: Range<usize>, spans: &[Span<'static>]) -> Line<'static> {
    let mut pieces: Vec<(Style, String)> = Vec::new();
    let mut offset = 0;
    for span in spans {
        let end = offset + span.content.len();
        let from = range.start.max(offset);
        let to = range.end.min(end);
        if from < to {
            let piece = &text[from..to];
            match pieces.last_mut() {
                Some((style, text)) if *style == span.style => text.push_str(piece),
                _ => pieces.push((span.style, piece.to_owned())),
            }
        }
        offset = end;
    }
    while let Some((_, text)) = pieces.last_mut() {
        let trimmed = text.trim_end();
        if trimmed.len() == text.len() {
            break;
        }
        text.truncate(trimmed.len());
        if text.is_empty() {
            pieces.pop();
        }
    }
    Line::from(
        pieces
            .into_iter()
            .map(|(style, text)| Span::styled(text, style))
            .collect::<Vec<_>>(),
    )
}

/// Makes text safe to draw: strips ANSI sequences, expands tabs, keeps
/// newlines, and shows every other control character as a picture.
pub fn sanitize(text: &str) -> Cow<'_, str> {
    if !text.chars().any(needs_escaping) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len() + 8);
    let mut chars = text.char_indices().peekable();
    while let Some((_, ch)) = chars.next() {
        match ch {
            '\n' => out.push('\n'),
            '\t' => out.push_str("    "),
            '\r' => {
                if !matches!(chars.peek(), Some((_, '\n'))) {
                    push_picture(&mut out, ch);
                }
            }
            '\u{1b}' => skip_escape(&mut chars),
            '\u{9b}' => skip_csi(&mut chars),
            ch if needs_escaping(ch) => push_picture(&mut out, ch),
            ch => out.push(ch),
        }
    }
    Cow::Owned(out)
}

fn needs_escaping(ch: char) -> bool {
    ch != '\n'
        && (ch == '\t'
            || ch == '\r'
            || ch.is_control()
            || matches!(
                ch,
                '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
            ))
}

fn skip_escape(chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>) {
    match chars.peek().map(|(_, ch)| *ch) {
        Some('[') => {
            chars.next();
            skip_csi(chars);
        }
        Some(']' | 'P' | 'X' | '^' | '_') => {
            chars.next();
            for (_, ch) in chars.by_ref() {
                if ch == '\u{7}' {
                    return;
                }
                if ch == '\u{1b}' {
                    if matches!(chars.peek(), Some((_, '\\'))) {
                        chars.next();
                    }
                    return;
                }
            }
        }
        Some(_) => {
            chars.next();
        }
        None => {}
    }
}

fn skip_csi(chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>) {
    for (_, ch) in chars.by_ref() {
        if matches!(ch, '\u{40}'..='\u{7e}') {
            return;
        }
    }
}

fn push_picture(out: &mut String, ch: char) {
    let code = ch as u32;
    match code {
        0x00..=0x1f => out.push(char::from_u32(0x2400 + code).expect("control picture")),
        0x7f => out.push('\u{2421}'),
        _ => {
            out.push_str("\\u{");
            out.push_str(&format!("{code:x}"));
            out.push('}');
        }
    }
}

/// Clips `text` to `width` cells, right-aligning `suffix` when both fit.
pub fn clip(text: &str, suffix: &str, width: usize) -> String {
    let text = sanitize(text);
    let suffix_width = self::width(suffix);
    if suffix.is_empty() {
        return truncate(&text, width);
    }
    if suffix_width + 2 >= width {
        return truncate(&text, width);
    }
    let available = width - suffix_width - 1;
    let head = truncate(&text, available);
    let padding = width - self::width(&head) - suffix_width;
    format!("{head}{}{suffix}", " ".repeat(padding))
}

/// Truncates to at most `width` cells, adding an ellipsis when cut.
pub fn truncate(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if self::width(text) <= width {
        return text.to_owned();
    }
    let mut out = String::new();
    let mut used = 0;
    for grapheme in text.graphemes(true) {
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if used + grapheme_width > width.saturating_sub(1) {
            break;
        }
        out.push_str(grapheme);
        used += grapheme_width;
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    #[test]
    fn wraps_at_words_and_drops_the_break_space() {
        assert_eq!(wrap("the quick brown fox", 10), ["the quick", "brown fox"]);
        assert_eq!(wrap("aaa bbb", 3), ["aaa", "bbb"]);
        assert_eq!(wrap("exactly ten", 11), ["exactly ten"]);
        assert_eq!(
            wrap("a very long sentence wraps here", 12),
            ["a very long", "sentence", "wraps here"]
        );
    }

    #[test]
    fn hard_breaks_overlong_tokens() {
        assert_eq!(wrap("abcdefghijkl", 5), ["abcde", "fghij", "kl"]);
        assert_eq!(
            wrap("https://example.com/some/long/path", 8),
            ["https://", "example.", "com/some", "/long/pa", "th"]
        );
        assert_eq!(
            wrap("a /very/long/path/file.rs b", 10),
            ["a", "/very/long", "/path/file", ".rs b"]
        );
    }

    #[test]
    fn breaks_between_wide_characters() {
        assert_eq!(wrap("中文测试文字", 4), ["中文", "测试", "文字"]);
        assert_eq!(wrap("ab中文", 4), ["ab中", "文"]);
        assert_eq!(wrap("中文 abc", 3), ["中", "文", "abc"]);
        assert_eq!(wrap("テスト", 4), ["テス", "ト"]);
    }

    #[test]
    fn never_splits_a_grapheme() {
        let family = "👨‍👩‍👧‍👦";
        let text = format!("{family}{family}");
        let lines = wrap(&text, 3);
        assert_eq!(lines.concat(), text);
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn keeps_explicit_newlines_empty_lines_and_indentation() {
        assert_eq!(wrap("a\n\nb", 5), ["a", "", "b"]);
        assert_eq!(wrap("", 5), [""]);
        assert_eq!(wrap("    indented", 20), ["    indented"]);
        assert_eq!(wrap("  - item with text", 12), ["  - item", "with text"]);
    }

    #[test]
    fn whitespace_runs_never_create_blank_lines() {
        assert_eq!(wrap("abc     def", 6), ["abc", "def"]);
        assert_eq!(wrap("abc        def", 5), ["abc", "def"]);
        assert_eq!(wrap("ab     cdef", 3), ["ab", "cde", "f"]);
    }

    #[test]
    fn styled_spans_survive_wrapping() {
        let spans = vec![
            Span::styled("hello ".to_owned(), Style::new().fg(Color::Red)),
            Span::styled("world again".to_owned(), Style::new().fg(Color::Blue)),
        ];
        let lines = wrap_spans(&spans, 8);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].to_string(), "hello");
        assert_eq!(lines[0].spans[0].style.fg, Some(Color::Red));
        assert_eq!(lines[1].to_string(), "world");
        assert_eq!(lines[2].to_string(), "again");
        assert_eq!(lines[2].spans.last().unwrap().style.fg, Some(Color::Blue));
    }

    #[test]
    fn adjacent_spans_with_one_style_are_merged() {
        let style = Style::new().fg(Color::Green);
        let spans = vec![Span::styled("ab", style), Span::styled("cd", style)];
        let lines = wrap_spans(&spans, 10);
        assert_eq!(lines[0].spans.len(), 1);
        assert_eq!(lines[0].spans[0].content, "abcd");
    }

    #[test]
    fn char_breaks_chunk_code_lines() {
        assert_eq!(char_breaks("0123456789", 4), vec![0..4, 4..8, 8..10]);
        assert_eq!(
            char_breaks("", 4),
            std::iter::once(0..0).collect::<Vec<_>>()
        );
        assert_eq!(char_breaks("中文", 2), vec![0..3, 3..6]);
    }

    #[test]
    fn sanitize_strips_escape_sequences() {
        assert_eq!(sanitize("\u{1b}[31mred\u{1b}[0m"), "red");
        assert_eq!(sanitize("a\u{1b}[1;32mb\u{1b}[0m"), "ab");
        assert!(!sanitize("a\u{1b}[31mred\u{1b}[0m").contains('\u{1b}'));
        assert_eq!(sanitize("bell\u{7}"), "bell␇");
        assert_eq!(sanitize("cr\rtwo"), "cr␍two");
        assert_eq!(sanitize("del\u{7f}"), "del␡");
    }

    #[test]
    fn sanitize_strips_osc_and_keeps_newlines_and_tabs() {
        assert_eq!(sanitize("\u{1b}]52;c;cGF5bG9hZA==\u{7}ok"), "ok");
        assert_eq!(sanitize("a\r\nb"), "a\nb");
        assert_eq!(sanitize("a\tb"), "a    b");
    }

    #[test]
    fn sanitize_escapes_bidi_overrides_but_keeps_joiners() {
        let text = sanitize("a\u{202e}b\u{200d}c");
        assert!(!text.contains('\u{202e}'));
        assert!(text.contains("\\u{202e}"));
        assert!(text.contains('\u{200d}'));
    }

    #[test]
    fn sanitize_borrows_clean_text() {
        assert!(matches!(
            sanitize("plain 中文 text\nsecond line"),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn width_counts_cells_not_chars() {
        assert_eq!(width("中文"), 4);
        assert_eq!(width("abc"), 3);
        assert_eq!(width_upto("a中b", 4), 3);
    }

    #[test]
    fn wrapping_never_exceeds_the_limit() {
        let text = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do \
                    eiusmod tempor incididunt ut labore et dolore magna aliqua. 中文混排 测试";
        for limit in 2..40 {
            for line in wrap(text, limit) {
                assert!(width(&line) <= limit, "{limit}: {line:?}");
            }
        }
    }

    #[test]
    fn wrapping_is_stable_under_reflow() {
        // Re-wrapping each produced line at a wider width must not change its
        // text: this is what keeps a scroll anchor meaningful.
        for line in wrap("alpha beta gamma delta epsilon", 7) {
            for wider in [7, 8, 12, 40] {
                assert_eq!(wrap(&line, wider)[0].trim_end(), line.trim_end());
            }
        }
    }

    #[test]
    fn wrapping_preserves_all_words() {
        let text = "one two three four five six seven eight nine ten";
        let joined = wrap(text, 9).join(" ");
        assert_eq!(joined, text);
    }
}
