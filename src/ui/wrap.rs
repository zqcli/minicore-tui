//! Word wrapping and text sanitizing.
//!
//! Breaks happen at whitespace, and between CJK/fullwidth characters, so prose
//! never wraps inside a word. Overlong tokens (URLs, paths) hard-break. Width
//! is measured with `unicode-width`, so wide characters occupy two cells.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Replaces control characters that would otherwise be interpreted by the
/// terminal, and strips ANSI escape sequences.
pub fn sanitize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        match character {
            '\x1b' => {
                // CSI: ESC [ … final byte in @..~
                if chars.peek() == Some(&'[') {
                    chars.next();
                    for next in chars.by_ref() {
                        if ('@'..='~').contains(&next) {
                            break;
                        }
                    }
                } else {
                    // OSC and two-byte sequences: drop the introducer and the
                    // next byte; anything past that is treated as text.
                    chars.next();
                }
            }
            '\t' => out.push_str("    "),
            '\n' | '\r' => out.push(character),
            character if character.is_control() => out.push('�'),
            character => out.push(character),
        }
    }
    out
}

/// Columns `text` occupies, ignoring control characters.
pub fn width(text: &str) -> usize {
    UnicodeWidthStr::width(sanitize(text).as_str())
}

/// Wraps `text` to `width` columns. Existing newlines are preserved, and the
/// leading indentation of a wrapped line is kept on its continuation.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let text = sanitize(text);
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        wrap_line(paragraph, width, &mut lines);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn wrap_line(line: &str, width: usize, out: &mut Vec<String>) {
    if line.is_empty() {
        out.push(String::new());
        return;
    }
    // Continuations repeat the leading indentation of the source line.
    let indent: String = line
        .chars()
        .take_while(|character| character.is_whitespace())
        .collect();
    let body = &line[indent.len()..];
    if body.is_empty() {
        out.push(String::new());
        return;
    }
    let indent_width = indent_width(&indent);
    let mut current = String::new();
    let mut current_width = 0usize;
    for chunk in break_chunks(body) {
        let chunk_width = UnicodeWidthStr::width(chunk);
        let is_space = chunk.chars().all(char::is_whitespace);
        if is_space {
            if !current.is_empty() {
                current.push(' ');
                current_width += 1;
            }
            continue;
        }
        if current_width > 0 && current_width + chunk_width > width {
            out.push(std::mem::take(&mut current).trim_end().to_owned());
            current_width = 0;
        }
        if current_width == 0 && !current.is_empty() {
            current.push_str(&indent);
            current_width = indent_width.min(width.saturating_sub(chunk_width));
        }
        if current_width == 0 {
            current.push_str(&indent);
            current_width = indent_width;
        }
        if current_width + chunk_width > width {
            // Overlong token: hard-break by grapheme within the remaining cells.
            let mut used = current_width;
            for grapheme in chunk.graphemes(true) {
                let grapheme_width = UnicodeWidthStr::width(grapheme);
                if used + grapheme_width > width && used > indent_width {
                    out.push(std::mem::take(&mut current).trim_end().to_owned());
                    current.push_str(&indent);
                    used = indent_width;
                }
                current.push_str(grapheme);
                used += grapheme_width;
            }
            current_width = used;
            continue;
        }
        current.push_str(chunk);
        current_width += chunk_width;
    }
    if !current.trim_end().is_empty() {
        out.push(current.trim_end().to_owned());
    }
}

/// Splits text into words, whitespace and CJK runs. A CJK character is its own
/// chunk because a line break is allowed between any two of them.
fn break_chunks(text: &str) -> Vec<&str> {
    let mut chunks = Vec::new();
    let mut start = 0;
    let mut last_was_wide = false;
    for (offset, character) in text.char_indices() {
        let wide = matches!(UnicodeWidthChar::width(character), Some(2));
        let whitespace = character.is_whitespace();
        if wide || last_was_wide {
            if start < offset {
                chunks.push(&text[start..offset]);
            }
            chunks.push(&text[offset..offset + character.len_utf8()]);
            start = offset + character.len_utf8();
            last_was_wide = wide;
            continue;
        }
        if whitespace {
            if start < offset {
                chunks.push(&text[start..offset]);
            }
            let end = offset + character.len_utf8();
            chunks.push(&text[offset..end]);
            start = end;
        }
        last_was_wide = false;
    }
    if start < text.len() {
        chunks.push(&text[start..]);
    }
    chunks
}

fn indent_width(indent: &str) -> usize {
    UnicodeWidthStr::width(indent)
}

/// Clips `text` to `width`, appending `suffix` right-aligned when it fits.
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

/// Truncates to at most `width` columns, adding an ellipsis when cut.
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

    #[test]
    fn prose_breaks_only_at_whitespace() {
        let lines = wrap("the quick brown fox jumps over the lazy dog", 12);
        for line in &lines {
            assert!(line.split(' ').all(|word| !word.is_empty()));
            assert!(width(line) <= 12, "{line:?} is too wide");
        }
        assert_eq!(
            lines.join(" ").split_whitespace().collect::<Vec<_>>(),
            vec![
                "the", "quick", "brown", "fox", "jumps", "over", "the", "lazy", "dog"
            ]
        );
    }

    #[test]
    fn cjk_text_may_break_between_characters() {
        let lines = wrap("这是一个很长的中文句子需要换行", 8);
        assert!(lines.len() > 1);
        for line in &lines {
            assert!(width(line) <= 8);
        }
        assert_eq!(lines.concat(), "这是一个很长的中文句子需要换行");
    }

    #[test]
    fn overlong_tokens_hard_break() {
        let lines = wrap("https://example.com/an/extremely/long/path/segment", 10);
        assert!(lines.len() > 1);
        for line in &lines {
            assert!(width(line) <= 10, "{line:?}");
        }
    }

    #[test]
    fn indentation_survives_wrapped_list_items() {
        let lines = wrap(
            "  - a list item that is long enough to wrap onto another line",
            20,
        );
        assert!(lines[0].starts_with("  - "));
        assert!(lines[1].starts_with("  "), "{:?}", lines[1]);
    }

    #[test]
    fn control_characters_and_ansi_are_removed() {
        assert_eq!(sanitize("a\u{1b}[31mred\u{1b}[0m"), "ared");
        assert_eq!(sanitize("tab\there"), "tab    here");
        assert_eq!(sanitize("bell\u{7}"), "bell�");
    }

    #[test]
    fn clipping_right_aligns_the_suffix() {
        let line = clip("▎ ✓ bash  run tests", "0.4s · exit 0", 30);
        assert_eq!(width(&line), 30);
        assert!(line.ends_with("0.4s · exit 0"));
    }

    #[test]
    fn truncation_adds_an_ellipsis_within_the_budget() {
        assert_eq!(truncate("abcdefgh", 5), "abcd…");
        assert_eq!(truncate("abc", 5), "abc");
    }
}
