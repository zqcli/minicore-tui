//! Normalize terminal input before any key can submit a message. Native Paste
//! events are authoritative; raw bracketed-paste markers are also decoded (the
//! Windows console-record backend otherwise exposes them as individual keys).
//! Legacy unbracketed text/Enter bursts are held for a short quiet window and
//! inserted as one paste, never executed as commands. Timing is only a fallback:
//! an arbitrarily slow legacy paste is indistinguishable from physical typing.

use std::time::Duration;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

pub const QUIET: Duration = Duration::from_millis(30);
const BEGIN: &str = "\x1b[200~";
const END: &str = "\x1b[201~";

#[derive(Default)]
pub struct InputDecoder {
    legacy_bursts: bool,
    keys: Vec<KeyEvent>,
    text: String,
    paste: Option<String>,
}

impl InputDecoder {
    pub fn new(legacy_bursts: bool) -> Self {
        Self {
            legacy_bursts,
            ..Self::default()
        }
    }
    /// Only undecided legacy input needs a deadline. A bracketed paste remains
    /// protected even if the terminal delivers its payload in slow chunks.
    pub fn pending(&self) -> bool {
        !self.keys.is_empty()
    }

    pub fn push(&mut self, event: Event) -> Vec<Event> {
        if let Event::Key(key) = event {
            if key.is_release() {
                return Vec::new();
            }
            if let Some(paste) = &mut self.paste {
                // No key received inside a framed paste is ever dispatched as
                // an action, including Ctrl+C, Shift+Enter or embedded escapes.
                let ch = match key.code {
                    KeyCode::Char(ch) => Some(ch),
                    KeyCode::Enter => Some('\n'),
                    KeyCode::Tab => Some('\t'),
                    KeyCode::Esc => Some('\x1b'),
                    _ => None,
                };
                if let Some(ch) = ch {
                    paste.push(ch);
                    if paste.ends_with(END) {
                        paste.truncate(paste.len() - END.len());
                        return vec![Event::Paste(self.paste.take().unwrap())];
                    }
                }
                return Vec::new();
            }
            if let Some(ch) = literal(key) {
                self.keys.push(key);
                self.text.push(ch);
                if self.text.ends_with(BEGIN) {
                    self.text.truncate(self.text.len() - BEGIN.len());
                    self.keys.truncate(self.keys.len() - BEGIN.chars().count());
                    let ready = self.flush();
                    self.paste = Some(String::new());
                    return ready;
                }
                if !self.legacy_bursts && !BEGIN.starts_with(&self.text) {
                    return self.flush();
                }
                return Vec::new();
            }
            // Preserve modifiers/shortcuts as keys, never reinterpret them as
            // pasted commands or approval decisions.
            let mut ready = self.flush();
            ready.push(Event::Key(key));
            return ready;
        }
        let mut ready = self.flush();
        ready.push(event);
        ready
    }

    /// EOF must not lose a payload if its terminal disappeared before END.
    pub fn finish(&mut self) -> Vec<Event> {
        let mut ready = self.flush();
        if let Some(text) = self.paste.take() {
            ready.push(Event::Paste(text));
        }
        ready
    }

    pub fn flush(&mut self) -> Vec<Event> {
        let text = std::mem::take(&mut self.text);
        let keys = std::mem::take(&mut self.keys);
        if keys.len() > 1 && text.contains(['\n', '\r']) && !text.contains('\x1b') {
            vec![Event::Paste(text)]
        } else {
            keys.into_iter().map(Event::Key).collect()
        }
    }
}

fn literal(key: KeyEvent) -> Option<char> {
    if key.modifiers.intersects(
        KeyModifiers::CONTROL
            | KeyModifiers::ALT
            | KeyModifiers::SUPER
            | KeyModifiers::HYPER
            | KeyModifiers::META,
    ) {
        return None;
    }
    match key.code {
        KeyCode::Char(ch) => Some(ch),
        KeyCode::Enter if key.modifiers.is_empty() => Some('\n'),
        KeyCode::Tab if key.modifiers.is_empty() => Some('\t'),
        KeyCode::Esc if key.modifiers.is_empty() => Some('\x1b'),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEventKind;

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }
    fn feed(d: &mut InputDecoder, text: &str) -> Vec<Event> {
        text.chars()
            .flat_map(|ch| {
                d.push(key(match ch {
                    '\x1b' => KeyCode::Esc,
                    '\n' => KeyCode::Enter,
                    _ => KeyCode::Char(ch),
                }))
            })
            .collect()
    }

    #[test]
    fn legacy_multiline_burst_is_one_paste_including_trailing_enter() {
        let mut d = InputDecoder::new(true);
        assert!(feed(&mut d, "中文\r\nsecond\n").is_empty());
        assert_eq!(d.flush(), [Event::Paste("中文\r\nsecond\n".into())]);
    }

    #[test]
    fn native_paste_passes_through_without_parsing_its_newlines_or_commands() {
        let mut d = InputDecoder::new(true);
        let text = "/new\n/delete\n".to_owned();
        assert_eq!(d.push(Event::Paste(text.clone())), [Event::Paste(text)]);
    }

    #[test]
    fn raw_bracketed_paste_survives_quiet_gaps_and_embedded_escape() {
        let mut d = InputDecoder::new(true);
        assert!(feed(&mut d, BEGIN).is_empty());
        for part in ["/new\n", "hello\x1b[31m", "中文\n"] {
            assert!(feed(&mut d, part).is_empty());
            assert!(d.flush().is_empty());
            assert!(!d.pending());
        }
        assert_eq!(
            feed(&mut d, END),
            [Event::Paste("/new\nhello\x1b[31m中文\n".into())]
        );
    }

    #[test]
    fn ordinary_typing_enter_shortcuts_and_repeats_keep_their_meaning() {
        let mut d = InputDecoder::new(true);
        assert!(feed(&mut d, "ab").is_empty());
        assert_eq!(
            d.flush(),
            [key(KeyCode::Char('a')), key(KeyCode::Char('b'))]
        );
        assert!(d.push(key(KeyCode::Enter)).is_empty());
        assert_eq!(d.flush(), [key(KeyCode::Enter)]);
        let shift = Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
        assert_eq!(d.push(shift.clone()), [shift]);
        let repeat = Event::Key(KeyEvent::new_with_kind(
            KeyCode::Char('x'),
            KeyModifiers::NONE,
            KeyEventKind::Repeat,
        ));
        d.push(repeat.clone());
        assert_eq!(d.flush(), [repeat]);
        let release = Event::Key(KeyEvent::new_with_kind(
            KeyCode::Enter,
            KeyModifiers::NONE,
            KeyEventKind::Release,
        ));
        assert!(d.push(release).is_empty());
    }

    #[test]
    fn buffered_escape_is_not_lost_and_non_text_keys_flush_in_order() {
        let mut d = InputDecoder::new(true);
        feed(&mut d, "a\x1b");
        assert_eq!(
            d.push(key(KeyCode::Left)),
            [
                key(KeyCode::Char('a')),
                key(KeyCode::Esc),
                key(KeyCode::Left)
            ]
        );
    }

    #[test]
    fn native_backend_typing_has_no_burst_delay() {
        let mut d = InputDecoder::new(false);
        assert_eq!(d.push(key(KeyCode::Char('a'))), [key(KeyCode::Char('a'))]);
        assert_eq!(d.push(key(KeyCode::Enter)), [key(KeyCode::Enter)]);
        assert!(!d.pending());
    }

    #[test]
    fn framed_paste_never_leaks_shortcuts_and_unterminated_eof_keeps_text() {
        let mut d = InputDecoder::new(true);
        feed(&mut d, BEGIN);
        assert!(
            d.push(Event::Key(KeyEvent::new(
                KeyCode::Char('c'),
                KeyModifiers::CONTROL
            )))
            .is_empty()
        );
        assert!(
            d.push(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::SHIFT
            )))
            .is_empty()
        );
        assert_eq!(d.finish(), [Event::Paste("c\n".into())]);
    }
}
