//! Windows console input. Crossterm reports a bracketed paste there as its
//! keys, markers included, so the markers are decoded here and an unbracketed
//! multiline burst is taken as one paste. Unix terminals deliver `Event::Paste`.

use std::time::Duration;

use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyModifiers};
use futures_util::StreamExt;
use tokio::sync::mpsc::UnboundedSender;

/// How long held keys wait for the rest of a burst.
const QUIET: Duration = Duration::from_millis(30);
/// How long an open bracketed paste waits for its end marker.
const PASTE_QUIET: Duration = Duration::from_secs(1);
const BEGIN: &str = "\x1b[200~";
const END: &str = "\x1b[201~";

/// Forwards decoded terminal events until the stream or the receiver closes.
pub async fn forward(events: &mut EventStream, tx: &UnboundedSender<Event>) {
    let mut decoder = Decoder::default();
    loop {
        let wait = decoder.wait();
        let ready = tokio::select! {
            biased;
            event = events.next() => match event {
                Some(Ok(event)) => decoder.push(event),
                Some(Err(_)) => continue,
                None => return,
            },
            () = tokio::time::sleep(wait.unwrap_or_default()), if wait.is_some() => decoder.expire(),
        };
        for event in ready {
            if tx.send(event).is_err() {
                return;
            }
        }
    }
}

#[derive(Default)]
struct Decoder {
    keys: Vec<KeyEvent>,
    text: String,
    paste: Option<String>,
}

impl Decoder {
    /// The quiet time after which [`Self::expire`] releases what is held.
    fn wait(&self) -> Option<Duration> {
        if self.paste.is_some() {
            Some(PASTE_QUIET)
        } else {
            (!self.keys.is_empty()).then_some(QUIET)
        }
    }

    fn push(&mut self, event: Event) -> Vec<Event> {
        let Event::Key(key) = event else {
            let mut ready = self.flush();
            ready.push(event);
            return ready;
        };
        if key.is_release() {
            return Vec::new();
        }
        if let Some(paste) = &mut self.paste {
            // Nothing inside a paste is an action, not even Ctrl+C or Enter.
            if let Some(ch) = pasted(key) {
                paste.push(ch);
                if let Some(text) = paste.strip_suffix(END) {
                    let text = text.to_owned();
                    self.paste = None;
                    return vec![Event::Paste(text)];
                }
            }
            return Vec::new();
        }
        let Some(ch) = typed(key) else {
            let mut ready = self.flush();
            ready.push(event);
            return ready;
        };
        self.keys.push(key);
        self.text.push(ch);
        if self.text.ends_with(BEGIN) {
            self.text.truncate(self.text.len() - BEGIN.len());
            self.keys.truncate(self.keys.len() - BEGIN.chars().count());
            let ready = self.flush();
            self.paste = Some(String::new());
            return ready;
        }
        Vec::new()
    }

    /// Releases held keys, and a paste whose end marker never came as it is.
    fn expire(&mut self) -> Vec<Event> {
        let mut ready = self.flush();
        ready.extend(self.paste.take().map(Event::Paste));
        ready
    }

    fn flush(&mut self) -> Vec<Event> {
        let text = std::mem::take(&mut self.text);
        let keys = std::mem::take(&mut self.keys);
        if keys.len() > 1 && text.contains(['\n', '\r']) && !text.contains('\x1b') {
            vec![Event::Paste(text)]
        } else {
            keys.into_iter().map(Event::Key).collect()
        }
    }
}

/// The character an unmodified key types, which a burst may hold.
fn typed(key: KeyEvent) -> Option<char> {
    if !key.modifiers.difference(KeyModifiers::SHIFT).is_empty() {
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

/// The character a key inside a paste stands for; the console reports a pasted
/// control character such as LF as Ctrl plus its letter.
fn pasted(key: KeyEvent) -> Option<char> {
    match key.code {
        KeyCode::Char(ch)
            if key.modifiers.contains(KeyModifiers::CONTROL) && ch.is_ascii_alphabetic() =>
        {
            Some(char::from(ch.to_ascii_uppercase() as u8 & 0x1f))
        }
        KeyCode::Char(ch) => Some(ch),
        KeyCode::Enter => Some('\n'),
        KeyCode::Tab => Some('\t'),
        KeyCode::Esc => Some('\x1b'),
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

    fn feed(decoder: &mut Decoder, text: &str) -> Vec<Event> {
        text.chars()
            .flat_map(|ch| {
                decoder.push(key(match ch {
                    '\x1b' => KeyCode::Esc,
                    '\n' => KeyCode::Enter,
                    _ => KeyCode::Char(ch),
                }))
            })
            .collect()
    }

    #[test]
    fn a_multiline_burst_is_one_paste_including_the_trailing_enter() {
        let mut decoder = Decoder::default();
        assert!(feed(&mut decoder, "中文\nsecond\n").is_empty());
        assert_eq!(decoder.wait(), Some(QUIET));
        assert_eq!(decoder.expire(), [Event::Paste("中文\nsecond\n".into())]);
    }

    #[test]
    fn a_native_paste_passes_through() {
        let mut decoder = Decoder::default();
        let text = "/new\n/delete\n".to_owned();
        assert_eq!(
            decoder.push(Event::Paste(text.clone())),
            [Event::Paste(text)]
        );
    }

    #[test]
    fn a_bracketed_paste_survives_gaps_and_an_embedded_escape() {
        let mut decoder = Decoder::default();
        assert!(feed(&mut decoder, BEGIN).is_empty());
        for part in ["/new\n", "hello\x1b[31m", "中文\n"] {
            assert!(feed(&mut decoder, part).is_empty());
            assert_eq!(decoder.wait(), Some(PASTE_QUIET));
        }
        assert_eq!(
            feed(&mut decoder, END),
            [Event::Paste("/new\nhello\x1b[31m中文\n".into())]
        );
        assert_eq!(decoder.wait(), None);
    }

    #[test]
    fn typing_enter_and_shortcuts_keep_their_meaning() {
        let mut decoder = Decoder::default();
        assert!(feed(&mut decoder, "ab").is_empty());
        assert_eq!(
            decoder.expire(),
            [key(KeyCode::Char('a')), key(KeyCode::Char('b'))]
        );
        assert!(decoder.push(key(KeyCode::Enter)).is_empty());
        assert_eq!(decoder.expire(), [key(KeyCode::Enter)]);
        let shift = Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
        assert_eq!(decoder.push(shift.clone()), [shift]);
        let release = Event::Key(KeyEvent::new_with_kind(
            KeyCode::Char('x'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        ));
        assert!(decoder.push(release).is_empty());
        assert_eq!(decoder.wait(), None);
    }

    #[test]
    fn a_held_escape_goes_out_before_the_key_that_ends_the_burst() {
        let mut decoder = Decoder::default();
        feed(&mut decoder, "a\x1b");
        assert_eq!(
            decoder.push(key(KeyCode::Left)),
            [
                key(KeyCode::Char('a')),
                key(KeyCode::Esc),
                key(KeyCode::Left)
            ]
        );
    }

    #[test]
    fn control_keys_in_a_paste_are_its_characters_not_actions() {
        let mut decoder = Decoder::default();
        feed(&mut decoder, BEGIN);
        for (code, modifiers) in [
            (KeyCode::Char('a'), KeyModifiers::NONE),
            (KeyCode::Char('j'), KeyModifiers::CONTROL),
            (KeyCode::Char('c'), KeyModifiers::CONTROL),
            (KeyCode::Enter, KeyModifiers::SHIFT),
        ] {
            assert!(
                decoder
                    .push(Event::Key(KeyEvent::new(code, modifiers)))
                    .is_empty()
            );
        }
        assert_eq!(feed(&mut decoder, END), [Event::Paste("a\n\u{3}\n".into())]);
    }

    #[test]
    fn a_paste_whose_end_never_comes_is_released_after_a_pause() {
        let mut decoder = Decoder::default();
        feed(&mut decoder, BEGIN);
        feed(&mut decoder, "partial");
        assert_eq!(decoder.wait(), Some(PASTE_QUIET));
        assert_eq!(decoder.expire(), [Event::Paste("partial".into())]);
        assert_eq!(decoder.wait(), None);
        assert!(feed(&mut decoder, "x").is_empty());
        assert_eq!(decoder.expire(), [key(KeyCode::Char('x'))]);
    }
}
