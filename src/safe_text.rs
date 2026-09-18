//! The one safe-display boundary for text that reaches the terminal
//! (spec §19, REF-48).
//!
//! Every string that comes from a model, tool, path, log, file, diff, or the
//! Agent's own metadata passes through [`safe_display`] immediately before it
//! becomes a terminal cell. Control characters that could move the cursor,
//! clear a line, or open an OSC/ANSI sequence are turned into visible escape
//! pictures; ordinary newlines and tabs are preserved for the layout code.
//!
//! This is a display filter, **not** a sandbox: it makes hostile bytes
//! visible, it does not make tool execution safe. It is also display-only by
//! contract — the sanitized string must never be used to compute or continue
//! an Agent-side cursor. Backend offsets and sizes always refer to the
//! original bytes (spec §5.4/§14.4).

use std::borrow::Cow;

/// Replaces unsafe control/format characters with visible escape pictures.
/// Returns a borrowed slice when nothing needs escaping, so the common path
/// performs no allocation and preserves byte identity for ordinary text.
pub fn safe_display(text: &str) -> Cow<'_, str> {
    if !text.chars().any(is_unsafe_display_control) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len() + 8);
    for character in text.chars() {
        if is_unsafe_display_control(character) {
            push_escape(&mut out, character);
        } else {
            out.push(character);
        }
    }
    Cow::Owned(out)
}

/// True for characters that must never reach the terminal as control cells:
/// C0 (except tab/newline), DEL, C1, and the bidirectional override/isolate
/// controls that can reorder visible text. The zero-width joiner is
/// deliberately preserved: it is part of legitimate emoji sequences.
fn is_unsafe_display_control(character: char) -> bool {
    matches!(
        character,
        '\0'..='\u{8}'
            | '\u{b}'
            | '\u{c}'
            | '\r'..='\u{1f}'
            | '\u{7f}'
            | '\u{80}'..='\u{9f}'
            | '\u{200e}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}'
    )
}

fn push_escape(out: &mut String, character: char) {
    let code = character as u32;
    match code {
        // Unicode control pictures for the C0 range and DEL.
        0x00..=0x1f => out.push(char::from_u32(0x2400 + code).expect("control picture")),
        0x7f => out.push('\u{2421}'),
        // C1 and format controls have no control picture; show the code point.
        _ => out.push_str(&format!("\\u{{{code:x}}}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_text_is_borrowed_unchanged() {
        let text = "plain text 你好 😀\nwith tab\tand newline";
        assert!(matches!(safe_display(text), Cow::Borrowed(_)));
        assert_eq!(safe_display(text), text);
    }

    #[test]
    fn escape_sequence_bytes_become_visible_pictures() {
        let hostile = "a\u{1b}]52;c;evil\u{7}b";
        let safe = safe_display(hostile);
        assert!(!safe.contains('\u{1b}'));
        assert!(!safe.contains('\u{7}'));
        assert_eq!(safe, "a␛]52;c;evil␇b");
    }

    #[test]
    fn carriage_return_and_c1_controls_are_escaped_not_executed() {
        assert_eq!(safe_display("one\rtwo"), "one␍two");
        assert_eq!(safe_display("x\u{9b}31m"), "x\\u{9b}31m");
        assert_eq!(safe_display("del\u{7f}"), "del␡");
    }

    #[test]
    fn bidi_overrides_are_escaped_but_zwj_is_preserved() {
        let text = "a\u{202e}b\u{200d}c";
        let safe = safe_display(text);
        assert!(!safe.contains('\u{202e}'));
        assert!(safe.contains("\\u{202e}"));
        assert!(
            safe.contains('\u{200d}'),
            "emoji joiners must survive the display boundary"
        );
    }

    #[test]
    fn escaping_changes_display_bytes_only() {
        // The documented contract: display text may grow, the Agent-facing
        // source string must not be replaced by it. Callers keep the original
        // bytes for cursors and offsets (spec §5.4).
        let raw = "raw\u{1b}bytes";
        let safe = safe_display(raw);
        assert_ne!(safe.len(), raw.len());
        // `String::len` is the byte length; ESC stays one raw byte even though
        // its display picture is three bytes.
        assert_eq!(raw.len(), 9);
        assert_eq!(&raw[3..4], "\u{1b}");
    }
}
