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
use unicode_segmentation::UnicodeSegmentation;

/// Per-process cap on remembered submitted messages (spec 22.2/43.7).
pub const MAX_HISTORY: usize = 100;
/// Maximum UTF-8 bytes accepted by the prompt/steering composer.
pub const MAX_COMPOSER_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PasteRange {
    pub id: usize,
    pub start: usize,
    pub end: usize,
    pub line_count: usize,
    pub char_count: usize,
}

/// Whether navigating history touches the editor's live draft.
pub struct Composer {
    textarea: TextArea<'static>,
    history: VecDeque<String>,
    /// Position within `history` while recalling an old message;
    /// `None` means the editor holds the live draft.
    history_index: Option<usize>,
    draft: String,
    /// Monotonic content-edit generation used to correlate delayed acks.
    editor_revision: u64,
    /// Display-only ranges for large paste payloads. The payload remains in
    /// the TextArea and is what `content()` returns.
    pastes: Vec<PasteRange>,
    paste_undo: Vec<Vec<PasteRange>>,
    paste_redo: Vec<Vec<PasteRange>>,
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
            editor_revision: 0,
            pastes: Vec::new(),
            paste_undo: Vec::new(),
            paste_redo: Vec::new(),
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

    /// The editor projection with large pasted payloads replaced by native
    /// markers. This is never used for submission or Agent requests.
    pub fn display_content(&self) -> String {
        project_pastes(&self.content(), &self.pastes)
    }

    /// Cursor position in the projected editor text. Entering a hidden paste
    /// range expands that range for display so cursor/edit coordinates remain
    /// truthful to the raw buffer.
    pub fn display_cursor(&self) -> (usize, usize) {
        let raw = self.content();
        let raw_cursor = global_cursor(&self.textarea, &raw);
        let display = project_pastes(&raw, &self.pastes);
        let display_cursor = projected_cursor(raw_cursor, &self.pastes);
        line_col_at(&display, display_cursor)
    }

    pub fn paste_ranges(&self) -> &[PasteRange] {
        &self.pastes
    }

    pub fn display_paste_markers(&self) -> Vec<std::ops::Range<usize>> {
        self.pastes
            .iter()
            .map(|paste| {
                let start = projected_cursor(paste.start, &self.pastes);
                start..start + paste_marker(paste).chars().count()
            })
            .collect()
    }

    /// Moves to a validated logical line/scalar-column position. The
    /// `TextArea` remains the cursor authority; callers only provide a
    /// terminal-cell position after passing through `EditorLayout`.
    pub fn move_to(&mut self, line: usize, column: usize) {
        self.textarea
            .move_cursor(CursorMove::Jump(line as u16, column as u16));
    }

    /// Moves from the projected editor coordinates back to the raw buffer.
    /// Clicking a paste marker lands at its raw start; the next edit then
    /// expands that range through the normal paste reconciliation path.
    pub fn move_to_display(&mut self, line: usize, column: usize) {
        let display = self.display_content();
        let display_cursor = global_line_column(&display, line, column);
        let raw_cursor = raw_cursor_for_display(display_cursor, &self.pastes);
        let (raw_line, raw_column) = line_col_at(&self.content(), raw_cursor);
        self.move_to(raw_line, raw_column);
    }

    /// Replaces a scalar range on one logical line and leaves the cursor at
    /// the end of the replacement. Used for local completion only.
    pub fn replace_range(&mut self, line: usize, start: usize, end: usize, replacement: &str) {
        let Some(raw) = self.textarea.lines().get(line).cloned() else {
            return;
        };
        let start = start.min(raw.chars().count());
        let end = end.min(raw.chars().count()).max(start);
        self.begin_edit();
        let before = self.content();
        self.textarea
            .move_cursor(CursorMove::Jump(line as u16, start as u16));
        for _ in start..end {
            self.textarea.delete_next_char();
        }
        self.textarea.insert_str(replacement);
        let after = self.content();
        self.reconcile_pastes(&before, &after);
        self.bump_revision();
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

    pub fn type_char(&mut self, c: char) -> bool {
        if !self.can_insert(&c.to_string()) {
            return false;
        }
        self.begin_edit();
        let before = self.content();
        self.textarea.insert_char(c);
        let after = self.content();
        self.reconcile_pastes(&before, &after);
        self.bump_revision();
        true
    }

    pub fn type_text(&mut self, text: &str) -> bool {
        if !self.can_insert(text) {
            return false;
        }
        self.begin_edit();
        let before = self.content();
        self.textarea.insert_str(text);
        let after = self.content();
        self.reconcile_pastes(&before, &after);
        self.bump_revision();
        true
    }

    /// Inserts a paste as one edit. Large payloads receive a display-only
    /// marker while `content()` continues to return the complete original.
    pub fn insert_paste(&mut self, text: &str) -> bool {
        if !self.can_insert(text) {
            return false;
        }
        self.begin_edit();
        let before = self.content();
        let start = global_cursor(&self.textarea, &before);
        self.textarea.insert_str(text);
        let after = self.content();
        self.reconcile_pastes(&before, &after);
        let line_count = text.split('\n').count();
        let char_count = text.chars().count();
        if line_count > 10 || char_count > 1_000 {
            let id = self.pastes.iter().map(|paste| paste.id).max().unwrap_or(0) + 1;
            self.pastes.push(PasteRange {
                id,
                start,
                end: start + char_count,
                line_count,
                char_count,
            });
            self.renumber_pastes();
        }
        self.bump_revision();
        true
    }

    fn can_insert(&self, text: &str) -> bool {
        self.content().len().saturating_add(text.len()) <= MAX_COMPOSER_BYTES
    }

    pub fn newline(&mut self) -> bool {
        if !self.can_insert("\n") {
            return false;
        }
        self.begin_edit();
        let before = self.content();
        self.textarea.insert_newline();
        let after = self.content();
        self.reconcile_pastes(&before, &after);
        self.bump_revision();
        true
    }

    /// Backspace; joins lines at word edges exactly as tui-textarea does.
    pub fn backspace(&mut self) {
        self.begin_edit();
        let before = self.content();
        let cursor = global_cursor(&self.textarea, &before);
        if let Some(paste) = self.pastes.iter().find(|paste| paste.end == cursor) {
            let start = paste.start;
            let count = paste.end.saturating_sub(paste.start);
            for _ in 0..count {
                self.textarea.delete_char();
            }
            debug_assert_eq!(start, global_cursor(&self.textarea, &before));
        } else {
            self.textarea.delete_char();
        }
        let after = self.content();
        self.reconcile_pastes(&before, &after);
        self.bump_revision();
    }

    /// Delete (forward).
    pub fn delete(&mut self) {
        self.begin_edit();
        let before = self.content();
        let cursor = global_cursor(&self.textarea, &before);
        if let Some(paste) = self.pastes.iter().find(|paste| paste.start == cursor) {
            let count = paste.end.saturating_sub(paste.start);
            for _ in 0..count {
                self.textarea.delete_next_char();
            }
        } else {
            self.textarea.delete_next_char();
        }
        let after = self.content();
        self.reconcile_pastes(&before, &after);
        self.bump_revision();
    }

    pub fn move_left(&mut self) {
        self.move_display_cursor(false);
    }

    pub fn move_right(&mut self) {
        self.move_display_cursor(true);
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
        self.begin_edit();
        let before = self.content();
        let raw_cursor = global_cursor(&self.textarea, &before);
        let display = self.display_content();
        let display_cursor = projected_cursor(raw_cursor, &self.pastes);
        let markers = self.display_paste_markers();
        let target = word_backward_cursor(&display, display_cursor, &markers);
        if target != display_cursor {
            let raw_start = raw_cursor_for_display(target, &self.pastes);
            let raw_end = raw_cursor_for_display(display_cursor, &self.pastes);
            let (line, column) = line_col_at(&before, raw_end);
            self.move_to(line, column);
            for _ in raw_start..raw_end {
                self.textarea.delete_char();
            }
        }
        let after = self.content();
        self.reconcile_pastes(&before, &after);
        self.bump_revision();
    }

    pub fn undo(&mut self) {
        let current = self.pastes.clone();
        self.textarea.undo();
        if let Some(previous) = self.paste_undo.pop() {
            self.paste_redo.push(current);
            self.pastes = previous;
        }
        self.bump_revision();
    }

    pub fn redo(&mut self) {
        let current = self.pastes.clone();
        self.textarea.redo();
        if let Some(next) = self.paste_redo.pop() {
            self.paste_undo.push(current);
            self.pastes = next;
        }
        self.bump_revision();
    }

    /// Empties the buffer and drops any history navigation state.
    pub fn clear(&mut self) {
        self.set_text("");
        self.history_index = None;
    }

    /// Replaces the whole buffer (send-failure recovery, /clear, history)
    /// with the cursor at the end.
    pub fn set_text(&mut self, text: &str) {
        let normalized = text
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .replace('\t', "    ");
        let lines = normalized
            .split('\n')
            .map(str::to_owned)
            .collect::<Vec<_>>();
        self.textarea = TextArea::new(lines);
        self.textarea.move_cursor(CursorMove::Bottom);
        self.textarea.move_cursor(CursorMove::End);
        self.pastes.clear();
        self.paste_undo.clear();
        self.paste_redo.clear();
        self.bump_revision();
    }

    pub fn editor_revision(&self) -> u64 {
        self.editor_revision
    }

    fn bump_revision(&mut self) {
        self.editor_revision = self.editor_revision.wrapping_add(1);
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

    pub fn is_history_browsing(&self) -> bool {
        self.history_index.is_some()
    }

    fn begin_edit(&mut self) {
        self.paste_undo.push(self.pastes.clone());
        self.paste_redo.clear();
    }

    fn reconcile_pastes(&mut self, before: &str, after: &str) {
        let before_chars = before.chars().collect::<Vec<_>>();
        let after_chars = after.chars().collect::<Vec<_>>();
        let prefix = before_chars
            .iter()
            .zip(&after_chars)
            .take_while(|(left, right)| left == right)
            .count();
        let suffix = before_chars[prefix..]
            .iter()
            .rev()
            .zip(after_chars[prefix..].iter().rev())
            .take_while(|(left, right)| left == right)
            .count();
        let old_end = before_chars.len().saturating_sub(suffix);
        let new_len = after_chars.len().saturating_sub(prefix + suffix);
        let delta = new_len as isize - old_end.saturating_sub(prefix) as isize;
        self.pastes.retain_mut(|paste| {
            if paste.end <= prefix {
                true
            } else if paste.start >= old_end {
                paste.start = shift(paste.start, delta);
                paste.end = shift(paste.end, delta);
                true
            } else {
                false
            }
        });
        self.renumber_pastes();
    }

    fn renumber_pastes(&mut self) {
        self.pastes.sort_by_key(|paste| paste.start);
        for (index, paste) in self.pastes.iter_mut().enumerate() {
            paste.id = index + 1;
        }
    }
}

fn shift(value: usize, delta: isize) -> usize {
    if delta >= 0 {
        value.saturating_add(delta as usize)
    } else {
        value.saturating_sub(delta.unsigned_abs())
    }
}

fn global_cursor(textarea: &TextArea<'static>, content: &str) -> usize {
    let (line, column) = textarea.cursor();
    let mut offset = 0;
    for (index, raw) in textarea.lines().iter().enumerate() {
        if index == line {
            return offset + column.min(raw.chars().count());
        }
        offset += raw.chars().count() + 1;
    }
    content.chars().count()
}

fn project_pastes(content: &str, pastes: &[PasteRange]) -> String {
    let chars = content.chars().collect::<Vec<_>>();
    let mut out = String::new();
    let mut cursor = 0;
    for paste in pastes {
        if paste.start < cursor || paste.end > chars.len() || paste.start >= paste.end {
            continue;
        }
        out.extend(chars[cursor..paste.start].iter());
        out.push_str(&paste_marker(paste));
        cursor = paste.end;
    }
    out.extend(chars[cursor..].iter());
    out
}

fn paste_marker(paste: &PasteRange) -> String {
    if paste.line_count > 10 {
        format!("[paste #{} +{} lines]", paste.id, paste.line_count)
    } else {
        format!("[paste #{} {} chars]", paste.id, paste.char_count)
    }
}

fn projected_cursor(raw_cursor: usize, pastes: &[PasteRange]) -> usize {
    let mut cursor = raw_cursor;
    for paste in pastes {
        if raw_cursor > paste.start && raw_cursor < paste.end {
            return cursor.saturating_sub(raw_cursor - paste.start);
        }
        if paste.end <= raw_cursor {
            let marker_len = paste_marker(paste).chars().count();
            cursor = cursor.saturating_sub(paste.end - paste.start - marker_len);
        }
    }
    cursor
}

impl Composer {
    /// Moves across the projected editor text so a hidden paste payload is one
    /// native editor segment. The underlying TextArea still stores the raw
    /// payload; only the cursor navigation uses the display projection.
    fn move_display_cursor(&mut self, forward: bool) {
        let display = self.display_content();
        let raw = self.content();
        let raw_cursor = global_cursor(&self.textarea, &raw);
        let cursor = projected_cursor(raw_cursor, &self.pastes);
        let target = if forward {
            next_display_cursor(&display, cursor, &self.display_paste_markers())
        } else {
            previous_display_cursor(&display, cursor, &self.display_paste_markers())
        };
        let (line, column) = line_col_at(&display, target);
        self.move_to_display(line, column);
    }
}

fn previous_display_cursor(
    text: &str,
    cursor: usize,
    paste_markers: &[std::ops::Range<usize>],
) -> usize {
    if let Some(marker) = paste_markers.iter().find(|marker| marker.end == cursor) {
        return marker.start;
    }
    let byte = text
        .char_indices()
        .nth(cursor)
        .map_or(text.len(), |(byte, _)| byte);
    text[..byte]
        .graphemes(true)
        .next_back()
        .map_or(cursor, |grapheme| {
            cursor.saturating_sub(grapheme.chars().count())
        })
}

fn next_display_cursor(
    text: &str,
    cursor: usize,
    paste_markers: &[std::ops::Range<usize>],
) -> usize {
    if let Some(marker) = paste_markers.iter().find(|marker| marker.start == cursor) {
        return marker.end;
    }
    let byte = text
        .char_indices()
        .nth(cursor)
        .map_or(text.len(), |(byte, _)| byte);
    text[byte..]
        .graphemes(true)
        .next()
        .map_or(cursor, |grapheme| cursor + grapheme.chars().count())
}

fn word_backward_cursor(
    text: &str,
    cursor: usize,
    paste_markers: &[std::ops::Range<usize>],
) -> usize {
    #[derive(Clone, Copy)]
    struct Segment {
        start: usize,
        end: usize,
        word_like: bool,
        atomic: bool,
    }

    let mut segments = Vec::new();
    let mut position = 0;
    while position < cursor {
        if let Some(marker) = paste_markers
            .iter()
            .find(|marker| marker.start == position && marker.end <= cursor)
        {
            segments.push(Segment {
                start: marker.start,
                end: marker.end,
                word_like: false,
                atomic: true,
            });
            position = marker.end;
            continue;
        }
        let start_byte = text
            .char_indices()
            .nth(position)
            .map_or(text.len(), |(byte, _)| byte);
        let natural_end = text[start_byte..]
            .split_word_bound_indices()
            .next()
            .map_or(cursor, |(_, segment)| position + segment.chars().count());
        let marker_start = paste_markers
            .iter()
            .filter(|marker| marker.start > position && marker.start < natural_end)
            .map(|marker| marker.start)
            .min()
            .unwrap_or(natural_end);
        let end = marker_start.min(cursor);
        if end == position {
            position += 1;
            continue;
        }
        let byte_end = text
            .char_indices()
            .nth(end)
            .map_or(text.len(), |(byte, _)| byte);
        let part = &text[start_byte..byte_end];
        segments.push(Segment {
            start: position,
            end,
            word_like: part.chars().any(char::is_alphanumeric),
            atomic: false,
        });
        position = end;
    }

    let mut index = segments.len();
    let mut new_cursor = cursor;
    while index > 0 && !segments[index - 1].atomic {
        let segment = segments[index - 1];
        let byte_start = text
            .char_indices()
            .nth(segment.start)
            .map_or(text.len(), |(byte, _)| byte);
        let byte_end = text
            .char_indices()
            .nth(segment.end)
            .map_or(text.len(), |(byte, _)| byte);
        if !text[byte_start..byte_end].chars().all(char::is_whitespace) {
            break;
        }
        new_cursor = segment.start;
        index -= 1;
    }
    if index == 0 {
        return new_cursor;
    }
    if segments[index - 1].atomic || segments[index - 1].word_like {
        return segments[index - 1].start;
    }
    while index > 0 {
        let segment = segments[index - 1];
        if segment.atomic || segment.word_like {
            break;
        }
        new_cursor = segment.start;
        index -= 1;
    }
    new_cursor
}

fn global_line_column(content: &str, line: usize, column: usize) -> usize {
    let mut offset = 0;
    for (index, raw) in content.split('\n').enumerate() {
        if index == line {
            return offset + column.min(raw.chars().count());
        }
        offset += raw.chars().count() + 1;
    }
    content.chars().count()
}

fn raw_cursor_for_display(display_cursor: usize, pastes: &[PasteRange]) -> usize {
    let mut display_offset = 0;
    let mut raw_offset = 0;
    for paste in pastes {
        if paste.start < raw_offset {
            continue;
        }
        let before = paste.start - raw_offset;
        if display_cursor < display_offset + before {
            return raw_offset + display_cursor.saturating_sub(display_offset);
        }
        display_offset += before;
        let marker_len = paste_marker(paste).chars().count();
        if display_cursor < display_offset + marker_len {
            return paste.start;
        }
        display_offset += marker_len;
        raw_offset = paste.end;
    }
    raw_offset + display_cursor.saturating_sub(display_offset)
}

fn line_col_at(content: &str, cursor: usize) -> (usize, usize) {
    let mut line = 0;
    let mut column = 0;
    for character in content.chars().take(cursor) {
        if character == '\n' {
            line += 1;
            column = 0;
        } else {
            column += 1;
        }
    }
    (line, column)
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
    fn set_text_splits_logical_lines_and_normalizes_native_text() {
        let mut composer = Composer::new();
        composer.set_text("first\r\nsecond\tline");
        assert_eq!(
            composer.lines(),
            &["first".to_owned(), "second    line".to_owned()]
        );
        assert_eq!(composer.cursor(), (1, 14));
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

    #[test]
    fn large_paste_is_projected_as_a_marker_but_submits_raw_text() {
        let mut composer = Composer::new();
        let pasted = (1..=11)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(composer.insert_paste(&pasted));
        assert_eq!(composer.content(), pasted);
        assert_eq!(composer.paste_ranges().len(), 1);
        assert_eq!(composer.display_content(), "[paste #1 +11 lines]");
        assert_eq!(composer.display_cursor(), (0, 20));
        composer.undo();
        assert_eq!(composer.content(), "");
        assert!(composer.paste_ranges().is_empty());
        composer.redo();
        assert_eq!(composer.content(), pasted);
        assert_eq!(composer.display_content(), "[paste #1 +11 lines]");
    }

    #[test]
    fn cursor_and_delete_treat_a_large_paste_as_one_editor_segment() {
        let mut composer = Composer::new();
        let pasted = "x".repeat(1_001);
        composer.insert_paste(&pasted);

        composer.move_left();
        assert_eq!(composer.cursor(), (0, 0));
        composer.move_right();
        assert_eq!(composer.cursor(), (0, pasted.chars().count()));
        composer.backspace();
        assert!(composer.content().is_empty());

        composer.insert_paste(&pasted);
        composer.move_to(0, 0);
        composer.delete();
        assert!(composer.content().is_empty());
    }

    #[test]
    fn word_delete_uses_display_segments_without_entering_paste_payload() {
        let mut composer = Composer::new();
        composer.type_text("prefix ");
        let pasted = "x".repeat(1_001);
        composer.insert_paste(&pasted);
        composer.word_delete();
        assert_eq!(composer.content(), "prefix ");

        composer.set_text("hello!   ");
        composer.word_delete();
        assert_eq!(composer.content(), "hello");
        composer.word_delete();
        assert_eq!(composer.content(), "");
    }

    #[test]
    fn editing_inside_a_paste_expands_it_and_literal_marker_text_is_not_hidden() {
        let mut composer = Composer::new();
        let pasted = "a".repeat(1_001);
        composer.insert_paste(&pasted);
        composer.move_to(0, 500);
        composer.type_char('x');
        assert!(composer.paste_ranges().is_empty());
        assert!(composer.content().contains('x'));

        composer.set_text("[paste #1 +11 lines]");
        assert_eq!(composer.display_content(), "[paste #1 +11 lines]");
    }
}
