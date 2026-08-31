//! The composer editor (development spec 21, 22.2, 43.7): a thin wrapper
//! around the pinned `tui-textarea` so the rest of the app never touches an
//! editor directly. Editing methods are only called by `App::update`;
//! renderers read `lines`/`cursor` and never mutate.
//!
//! Rendering is done by the UI layer (which wraps each line with
//! `unicode-width`) rather than `TextArea::widget`, so CJK/emoji wrapping
//! matches the rest of the transcript; the TextArea is the single source of
//! truth for buffered text, the cursor, and undo/redo history.

use std::collections::VecDeque;

use tui_textarea::{CursorMove, TextArea};

/// Per-process cap on remembered submitted messages (spec 22.2/43.7).
pub const MAX_HISTORY: usize = 100;

/// Whether navigating history touches the editor's live draft.
pub struct Composer {
    textarea: TextArea<'static>,
    history: VecDeque<String>,
    /// Position within `history` while recalling an old message;
    /// `None` means the editor holds the live draft.
    history_index: Option<usize>,
    draft: String,
}

impl Default for Composer {
    fn default() -> Self {
        Self::new()
    }
}

impl Composer {
    pub fn new() -> Self {
        Self {
            textarea: TextArea::default(),
            history: VecDeque::new(),
            history_index: None,
            draft: String::new(),
        }
    }

    // ---- read-only (render + app) -------------------------------------

    /// The buffered lines; never mutated by renderers.
    pub fn lines(&self) -> &[String] {
        self.textarea.lines()
    }

    /// `(row, col)` in char offsets within `lines()[row]` (UTF-8 safe).
    pub fn cursor(&self) -> (usize, usize) {
        self.textarea.cursor()
    }

    pub fn is_empty(&self) -> bool {
        self.textarea.lines().iter().all(|line| line.is_empty())
    }

    /// The full buffer joined with `\n`, used for submit and paste math.
    pub fn content(&self) -> String {
        self.textarea.lines().join("\n")
    }

    /// The TextArea for the renderer (read-only widget access).
    pub fn textarea(&self) -> &TextArea<'static> {
        &self.textarea
    }

    // ---- editing (App::update only) -----------------------------------

    pub fn type_char(&mut self, c: char) {
        self.textarea.insert_char(c);
    }

    pub fn type_text(&mut self, text: &str) {
        self.textarea.insert_str(text);
    }

    pub fn newline(&mut self) {
        self.textarea.insert_newline();
    }

    /// Backspace; joins lines at word edges exactly as tui-textarea does.
    pub fn backspace(&mut self) {
        self.textarea.delete_char();
    }

    /// Delete (forward).
    pub fn delete(&mut self) {
        self.textarea.delete_next_char();
    }

    pub fn move_left(&mut self) {
        self.textarea.move_cursor(CursorMove::Back);
    }

    pub fn move_right(&mut self) {
        self.textarea.move_cursor(CursorMove::Forward);
    }

    pub fn move_up(&mut self) {
        self.textarea.move_cursor(CursorMove::Up);
    }

    pub fn move_down(&mut self) {
        self.textarea.move_cursor(CursorMove::Down);
    }

    /// True when the cursor sits on the first row (history recall trigger).
    pub fn at_first_line(&self) -> bool {
        self.textarea.cursor().0 == 0
    }

    /// True when the cursor sits on the last row (history recall trigger).
    pub fn at_last_line(&self) -> bool {
        self.textarea.cursor().0 + 1 >= self.textarea.lines().len()
    }

    pub fn line_start(&mut self) {
        self.textarea.move_cursor(CursorMove::Head);
    }

    pub fn line_end(&mut self) {
        self.textarea.move_cursor(CursorMove::End);
    }

    pub fn word_delete(&mut self) {
        self.textarea.delete_word();
    }

    pub fn undo(&mut self) {
        self.textarea.undo();
    }

    pub fn redo(&mut self) {
        self.textarea.redo();
    }

    /// Empties the buffer and drops any history navigation state.
    pub fn clear(&mut self) {
        self.set_text("");
        self.history_index = None;
    }

    /// Replaces the whole buffer (send-failure recovery, /clear, history)
    /// with the cursor at the end.
    pub fn set_text(&mut self, text: &str) {
        self.textarea = TextArea::new(vec![text.to_owned()]);
        self.textarea.move_cursor(CursorMove::End);
    }

    // ---- history (spec 22.2, 43.7) ------------------------------------

    /// Records a freshly submitted non-empty message (deduplicated against
    /// the newest entry) and resets navigation to the live draft.
    pub fn submit_pushed(&mut self, submitted: &str) {
        if self.history.back().is_none_or(|last| last != submitted) {
            self.history.push_back(submitted.to_owned());
            while self.history.len() > MAX_HISTORY {
                self.history.pop_front();
            }
        }
        self.history_index = None;
        self.draft = String::new();
    }

    /// Recalls the previous message. The live draft is saved once; each
    /// recall overwrites the editor with the history entry.
    pub fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        if self.history_index.is_none() {
            self.draft = self.content();
            self.history_index = Some(self.history.len() - 1);
        } else if let Some(index) = self.history_index {
            if index > 0 {
                self.history_index = Some(index - 1);
            }
        }
        if let Some(index) = self.history_index {
            let message = self.history.get(index).cloned();
            if let Some(message) = message {
                self.set_text(&message);
            }
        }
    }

    /// Moves toward newer messages and finally back to the live draft.
    pub fn history_next(&mut self) {
        let Some(index) = self.history_index else {
            return;
        };
        if index + 1 < self.history.len() {
            self.history_index = Some(index + 1);
            let message = self.history.get(index + 1).cloned();
            if let Some(message) = message {
                self.set_text(&message);
            }
        } else {
            self.history_index = None;
            let draft = std::mem::take(&mut self.draft);
            self.set_text(&draft);
        }
    }

    pub fn history_len(&self) -> usize {
        self.history.len()
    }

    /// The surviving draft when editing a recalled history entry.
    pub fn history_draft(&self) -> &str {
        &self.draft
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_inserts_into_the_buffer_with_char_cursor() {
        let mut composer = Composer::new();
        composer.type_char('你');
        composer.type_char('好');
        assert_eq!(composer.content(), "你好");
        assert_eq!(composer.cursor(), (0, 2));
        composer.move_left();
        composer.type_char(' ');
        assert_eq!(composer.content(), "你 好");
        assert_eq!(composer.content().chars().count(), 3);
    }

    #[test]
    fn multiline_editing_and_blank_lines_roundtrip() {
        let mut composer = Composer::new();
        composer.type_text("hello");
        composer.newline();
        composer.type_text("world");
        assert_eq!(composer.lines(), &["hello".to_owned(), "world".to_owned()]);
        composer.move_up();
        composer.line_end();
        composer.backspace();
        composer.newline();
        // Removing the 'o' left "hell" on the first row; a newline after it
        // makes the second row empty between "hell" and "world".
        assert_eq!(composer.content(), "hell\n\nworld");
        assert_eq!(composer.lines().len(), 3);
    }

    #[test]
    fn history_recall_preserves_the_draft_and_enforces_the_cap() {
        let mut composer = Composer::new();
        for i in 0..(MAX_HISTORY + 5) {
            composer.set_text(&format!("msg {i}"));
            composer.submit_pushed(&format!("msg {i}"));
        }
        assert_eq!(composer.history_len(), MAX_HISTORY);
        assert_eq!(composer.history.back().map(String::as_str), Some("msg 104"));

        composer.set_text("draft text");
        composer.history_prev();
        assert_eq!(composer.content(), "msg 104");
        assert_eq!(composer.history_draft(), "draft text");
        composer.history_prev();
        assert_eq!(composer.content(), "msg 103");
        composer.history_next();
        assert_eq!(composer.content(), "msg 104");
        composer.history_next();
        assert_eq!(composer.content(), "draft text");
    }

    #[test]
    fn submitting_deduplicates_the_newest_entry() {
        let mut composer = Composer::new();
        composer.set_text("same");
        composer.submit_pushed("same");
        composer.submit_pushed("same");
        assert_eq!(composer.history_len(), 1);
    }
}
