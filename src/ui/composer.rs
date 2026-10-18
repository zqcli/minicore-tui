//! The composer: a grapheme-safe text buffer with a cursor, undo history,
//! prompt history and paste tokens.
//!
//! The buffer stores the real text, including paste payloads. What the user
//! sees is a projection in which each large paste is one `[pasted 120 lines]`
//! token, and a paste is atomic there: the cursor can sit before or after it,
//! never inside it. [`Projection`] is the only place that converts between the
//! two coordinate systems.
//!
//! A cursor is a byte offset at a grapheme boundary, so edits never split a
//! grapheme or a paste.

use std::collections::VecDeque;
use std::ops::Range;

use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;

use super::theme::Styles;
use super::wrap;

/// Rows a draft needs at `width`, capped at [`COMPOSER_ROWS`].
pub const COMPOSER_ROWS: usize = 10;

/// Multi-line pastes longer than this many lines become a token, as do single
/// lines above [`TOKEN_BYTES`].
const TOKEN_LINES: usize = 3;
const TOKEN_BYTES: usize = 400;
/// Undo steps kept before the oldest is dropped.
const UNDO_STEPS: usize = 200;
/// Submitted prompts kept for recall.
const HISTORY_STEPS: usize = 100;

/// A paste displayed as one token.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Paste {
    /// Byte range of the payload in the buffer.
    pub range: Range<usize>,
    pub lines: usize,
}

impl Paste {
    pub fn label(&self) -> String {
        if self.lines <= 1 {
            format!("[pasted {} chars]", self.range.len())
        } else {
            format!("[pasted {} lines]", self.lines)
        }
    }
}

/// Maps between buffer offsets and projected display offsets.
pub struct Projection<'a> {
    text: &'a str,
    pastes: &'a [Paste],
}

impl<'a> Projection<'a> {
    fn new(text: &'a str, pastes: &'a [Paste]) -> Self {
        Self { text, pastes }
    }

    /// The displayed text: payloads replaced by their tokens.
    pub fn text(&self) -> String {
        let mut out = String::with_capacity(self.text.len());
        let mut at = 0;
        for paste in self.pastes {
            out.push_str(&self.text[at..paste.range.start]);
            out.push_str(&paste.label());
            at = paste.range.end;
        }
        out.push_str(&self.text[at..]);
        out
    }

    /// Display offset of a buffer offset. Offsets inside a payload snap to the
    /// start of its token.
    pub fn display(&self, raw: usize) -> usize {
        let mut shown = 0;
        let mut at = 0;
        for paste in self.pastes {
            if raw <= paste.range.start {
                return shown + raw - at;
            }
            shown += paste.range.start - at + paste.label().len();
            at = paste.range.end;
        }
        shown + raw.saturating_sub(at)
    }

    /// Buffer offset of a display offset. Offsets inside a token snap to the
    /// start of its payload.
    pub fn raw(&self, display: usize) -> usize {
        let mut shown = 0;
        let mut at = 0;
        for paste in self.pastes {
            let head = paste.range.start - at;
            if display <= shown + head {
                return at + display.saturating_sub(shown);
            }
            shown += head;
            let token = paste.label().len();
            if display <= shown + token {
                return paste.range.start;
            }
            shown += token;
            at = paste.range.end;
        }
        at + display.saturating_sub(shown)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Snapshot {
    text: String,
    cursor: usize,
    pastes: Vec<Paste>,
}

#[derive(Default)]
pub struct Composer {
    text: String,
    /// Cursor byte offset, at a grapheme boundary and outside every payload.
    cursor: usize,
    /// Display column to keep while moving between lines.
    column: Option<usize>,
    pastes: Vec<Paste>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// The buffer before the current typing gesture, committed when it ends.
    gesture: Option<Snapshot>,
    history: VecDeque<String>,
    /// Position in `history` while recalling; `None` means the live draft.
    recall: Option<usize>,
    draft: String,
}

impl Composer {
    pub fn new() -> Self {
        Self::default()
    }

    /// The full draft, including paste payloads.
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn pastes(&self) -> &[Paste] {
        &self.pastes
    }

    pub fn projection(&self) -> Projection<'_> {
        Projection::new(&self.text, &self.pastes)
    }

    /// The draft as displayed, with paste tokens.
    pub fn display(&self) -> String {
        self.projection().text()
    }

    /// Cursor offset in the displayed text.
    pub fn display_cursor(&self) -> usize {
        self.projection().display(self.cursor)
    }

    /// Line and cell column of the cursor in the displayed text.
    pub fn cursor_position(&self) -> (usize, usize) {
        let display = self.display();
        let cursor = self.display_cursor();
        let line = display[..cursor].matches('\n').count();
        let start = display[..cursor].rfind('\n').map_or(0, |at| at + 1);
        (line, wrap::width(&display[start..cursor]))
    }

    // ---- draft lifecycle ----------------------------------------------

    /// Replaces the draft, dropping tokens and history.
    pub fn set_text(&mut self, text: &str) {
        self.text.clear();
        self.text.push_str(text);
        self.cursor = self.text.len();
        self.pastes.clear();
        self.undo.clear();
        self.redo.clear();
        self.gesture = None;
        self.column = None;
        self.recall = None;
    }

    pub fn clear(&mut self) {
        if self.text.is_empty() {
            return;
        }
        self.snapshot();
        self.set_text("");
        self.commit();
    }

    /// Ends the current typing gesture so the next edit starts a new undo step.
    pub fn end_gesture(&mut self) {
        self.commit();
        self.column = None;
    }

    /// Records a submitted prompt and starts a fresh draft.
    pub fn submit(&mut self) -> String {
        self.commit();
        let text = std::mem::take(&mut self.text);
        self.cursor = 0;
        self.pastes.clear();
        self.column = None;
        self.recall = None;
        self.draft.clear();
        if !text.trim().is_empty() {
            self.history.push_back(text.clone());
            if self.history.len() > HISTORY_STEPS {
                self.history.pop_front();
            }
        }
        text
    }

    // ---- undo ---------------------------------------------------------

    fn snapshot(&mut self) {
        self.gesture.get_or_insert_with(|| Snapshot {
            text: self.text.clone(),
            cursor: self.cursor,
            pastes: self.pastes.clone(),
        });
    }

    fn commit(&mut self) {
        self.recall = None;
        let Some(snapshot) = self.gesture.take() else {
            return;
        };
        if snapshot.text == self.text {
            return;
        }
        self.undo.push(snapshot);
        if self.undo.len() > UNDO_STEPS {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    fn current(&self) -> Snapshot {
        Snapshot {
            text: self.text.clone(),
            cursor: self.cursor,
            pastes: self.pastes.clone(),
        }
    }

    fn restore(&mut self, snapshot: Snapshot) {
        self.text = snapshot.text;
        self.cursor = snapshot.cursor;
        self.pastes = snapshot.pastes;
        self.column = None;
    }

    pub fn undo(&mut self) -> bool {
        self.commit();
        let Some(snapshot) = self.undo.pop() else {
            return false;
        };
        self.redo.push(self.current());
        self.restore(snapshot);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(snapshot) = self.redo.pop() else {
            return false;
        };
        self.undo.push(self.current());
        self.restore(snapshot);
        true
    }

    // ---- editing ------------------------------------------------------

    pub fn insert(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.snapshot();
        let start = self.cursor;
        self.text.insert_str(start, text);
        self.cursor = start + text.len();
        self.shift(start, text.len());
        let lines = text.lines().count().max(1);
        if lines > TOKEN_LINES || text.len() > TOKEN_BYTES {
            self.pastes.push(Paste {
                range: start..self.cursor,
                lines,
            });
            self.pastes.sort_by_key(|paste| paste.range.start);
        }
        self.column = None;
        self.commit();
    }

    /// Types one character; consecutive calls form a single undo step.
    pub fn type_char(&mut self, ch: char) {
        if ch == '\n' || ch == '\r' {
            self.newline();
            return;
        }
        self.snapshot();
        let mut buf = [0u8; 4];
        let text = ch.encode_utf8(&mut buf);
        let start = self.cursor;
        self.text.insert_str(start, text);
        self.cursor = start + text.len();
        self.shift(start, text.len());
        self.column = None;
    }

    pub fn newline(&mut self) {
        self.snapshot();
        let start = self.cursor;
        self.text.insert(start, '\n');
        self.cursor = start + 1;
        self.shift(start, 1);
        self.column = None;
        self.commit();
    }

    fn shift(&mut self, at: usize, len: usize) {
        for paste in &mut self.pastes {
            if at <= paste.range.start {
                paste.range.start += len;
                paste.range.end += len;
            } else if at < paste.range.end {
                paste.range.end += len;
            }
        }
    }

    fn paste_starting_at(&self, cursor: usize) -> Option<&Paste> {
        self.pastes.iter().find(|paste| paste.range.start == cursor)
    }

    fn paste_ending_at(&self, cursor: usize) -> Option<&Paste> {
        self.pastes.iter().find(|paste| paste.range.end == cursor)
    }

    // ---- movement -----------------------------------------------------

    pub fn move_left(&mut self) {
        self.cursor = match self.paste_ending_at(self.cursor) {
            Some(paste) => paste.range.start,
            None => prev_grapheme(&self.text, self.cursor),
        };
        self.column = None;
    }

    pub fn move_right(&mut self) {
        self.cursor = match self.paste_starting_at(self.cursor) {
            Some(paste) => paste.range.end,
            None => next_grapheme(&self.text, self.cursor),
        };
        self.column = None;
    }

    pub fn move_document_start(&mut self) {
        self.cursor = 0;
        self.column = None;
    }

    pub fn move_document_end(&mut self) {
        self.cursor = self.text.len();
        self.column = None;
    }

    pub fn move_home(&mut self) {
        self.cursor = self.line_start(self.cursor_position().0);
        self.column = None;
    }

    pub fn move_end(&mut self) {
        self.cursor = self.line_end(self.cursor_position().0);
        self.column = None;
    }

    /// Moves one logical line up. False on the first line, where the caller
    /// may switch to prompt recall.
    pub fn move_up(&mut self) -> bool {
        let line = self.cursor_position().0;
        if line == 0 {
            return false;
        }
        let column = self.column.unwrap_or_else(|| self.cursor_position().1);
        self.cursor = self.offset_at_column(line - 1, column);
        self.column = Some(column);
        true
    }

    /// Moves one logical line down. False on the last line.
    pub fn move_down(&mut self) -> bool {
        let line = self.cursor_position().0;
        if line + 1 >= self.display().split('\n').count() {
            return false;
        }
        let column = self.column.unwrap_or_else(|| self.cursor_position().1);
        self.cursor = self.offset_at_column(line + 1, column);
        self.column = Some(column);
        true
    }

    pub fn move_word_left(&mut self) {
        if let Some(paste) = self.paste_ending_at(self.cursor) {
            self.cursor = paste.range.start;
            self.column = None;
            return;
        }
        let mut at = self.cursor;
        while at > 0 {
            let prev = prev_grapheme(&self.text, at);
            if !self.text[prev..at].chars().all(char::is_whitespace) {
                break;
            }
            at = prev;
        }
        while at > 0 {
            if let Some(paste) = self.paste_ending_at(at) {
                at = paste.range.start;
                break;
            }
            let prev = prev_grapheme(&self.text, at);
            if self.text[prev..at].chars().all(char::is_whitespace) {
                break;
            }
            at = prev;
        }
        self.cursor = at;
        self.column = None;
    }

    pub fn move_word_right(&mut self) {
        let mut at = self.cursor;
        if let Some(paste) = self.paste_starting_at(at) {
            at = paste.range.end;
        } else {
            while at < self.text.len() {
                let next = next_grapheme(&self.text, at);
                if self.paste_starting_at(next).is_some()
                    || self.text[at..next].chars().all(char::is_whitespace)
                {
                    break;
                }
                at = next;
            }
        }
        while at < self.text.len() {
            let next = next_grapheme(&self.text, at);
            if !self.text[at..next].chars().all(char::is_whitespace) {
                break;
            }
            at = next;
        }
        self.cursor = at;
        self.column = None;
    }

    // ---- deletion -----------------------------------------------------

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.snapshot();
        let start = match self.paste_ending_at(self.cursor) {
            Some(paste) => paste.range.start,
            None => prev_grapheme(&self.text, self.cursor),
        };
        self.delete_range(start..self.cursor, false);
        self.cursor = start;
    }

    pub fn delete(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        let start = self.cursor;
        let end = match self.paste_starting_at(self.cursor) {
            Some(paste) => paste.range.end,
            None => next_grapheme(&self.text, self.cursor),
        };
        self.delete_range(start..end, false);
        self.cursor = start;
    }

    pub fn delete_word_left(&mut self) {
        let end = self.cursor;
        self.move_word_left();
        let start = self.cursor;
        self.cursor = end;
        self.delete_range(start..end, true);
    }

    pub fn delete_to_line_start(&mut self) {
        let end = self.cursor;
        let start = self.line_start(self.cursor_position().0);
        self.cursor = end;
        self.delete_range(start..end, true);
    }

    pub fn delete_to_line_end(&mut self) {
        let start = self.cursor;
        let end = self.line_end(self.cursor_position().0);
        self.cursor = start;
        self.delete_range(start..end, true);
    }

    /// Removes `range`, extending it over any paste it only partly covers, so
    /// a token is never left half-deleted.
    fn delete_range(&mut self, range: Range<usize>, snapshot: bool) {
        if range.is_empty() {
            return;
        }
        if snapshot {
            self.snapshot();
        }
        let mut start = range.start;
        let mut end = range.end;
        for paste in &self.pastes {
            if paste.range.start < end && paste.range.end > start {
                start = start.min(paste.range.start);
                end = end.max(paste.range.end);
            }
        }
        let len = end - start;
        self.text.replace_range(start..end, "");
        self.pastes
            .retain(|paste| paste.range.end <= start || paste.range.start >= end);
        for paste in &mut self.pastes {
            if paste.range.start >= end {
                paste.range.start -= len;
                paste.range.end -= len;
            }
        }
        self.cursor = self.cursor.min(start);
        self.column = None;
        self.commit();
    }

    // ---- prompt history -----------------------------------------------

    /// Recalls an older prompt, keeping the live draft for the way back.
    pub fn history_prev(&mut self) -> bool {
        if self.history.is_empty() {
            return false;
        }
        let index = match self.recall {
            None => {
                self.draft = std::mem::take(&mut self.text);
                self.history.len() - 1
            }
            Some(0) => return false,
            Some(index) => index - 1,
        };
        self.recall = Some(index);
        self.load(self.history[index].clone());
        true
    }

    pub fn history_next(&mut self) -> bool {
        let Some(index) = self.recall else {
            return false;
        };
        if index + 1 < self.history.len() {
            self.recall = Some(index + 1);
            self.load(self.history[index + 1].clone());
            return true;
        }
        self.recall = None;
        let draft = std::mem::take(&mut self.draft);
        self.load(draft);
        true
    }

    fn load(&mut self, text: String) {
        self.text = text;
        self.cursor = self.text.len();
        self.pastes.clear();
        self.column = None;
    }

    // ---- line geometry ------------------------------------------------

    /// Buffer offset where the display line `line` starts.
    fn line_start(&self, line: usize) -> usize {
        let display = self.display();
        let offset: usize = display
            .split('\n')
            .take(line)
            .map(|part| part.len() + 1)
            .sum();
        self.projection().raw(offset.min(display.len()))
    }

    /// Buffer offset where the display line `line` ends.
    fn line_end(&self, line: usize) -> usize {
        let display = self.display();
        let offset: usize = display
            .split('\n')
            .take(line)
            .map(|part| part.len() + 1)
            .sum();
        let end = display[offset..]
            .find('\n')
            .map_or(display.len(), |at| offset + at);
        self.projection().raw(end)
    }

    /// The buffer offset on display line `line` closest to `column` cells.
    fn offset_at_column(&self, line: usize, column: usize) -> usize {
        let display = self.display();
        let start: usize = display
            .split('\n')
            .take(line)
            .map(|part| part.len() + 1)
            .sum();
        let row = display[start..].split('\n').next().unwrap_or_default();
        let mut used = 0;
        for (at, grapheme) in row.grapheme_indices(true) {
            let cells = wrap::width(grapheme);
            if used + cells > column {
                return self.projection().raw(start + at);
            }
            used += cells;
        }
        self.projection().raw(start + row.len())
    }
}

/// Renders the composer band: a rail plus the wrapped draft. Returns the rows
/// and the cursor position relative to the first row.
///
/// The rail is two cells wide, so the draft wraps to `width - 2`.
pub fn render(
    text: &str,
    cursor: Option<usize>,
    styles: Styles,
    width: u16,
) -> (Vec<Line<'static>>, (u16, u16)) {
    let total = width.max(3) as usize;
    let content = total - 2;
    let empty = text.is_empty();
    let shown = if empty {
        "Ask anything · / commands · @ files"
    } else {
        text
    };
    let caret = cursor.unwrap_or(shown.len()).min(shown.len());
    let body = if empty {
        styles.subtle()
    } else {
        styles.text()
    };

    let mut rows: Vec<Line<'static>> = Vec::new();
    let mut caret_row = 0usize;
    let mut caret_column = 2usize;
    let mut offset = 0usize;
    for line in shown.split('\n') {
        let pieces = wrap::wrap(line, content);
        if (offset..=offset + line.len()).contains(&caret) {
            let prefix = &line[..caret.saturating_sub(offset).min(line.len())];
            let prefix_rows = wrap::wrap(prefix, content);
            caret_row = rows.len() + prefix_rows.len() - 1;
            caret_column = 2 + wrap::width(prefix_rows.last().map_or("", String::as_str));
        }
        for piece in pieces {
            rows.push(rail_row(&piece, body, styles));
        }
        offset += line.len() + 1;
    }
    if rows.is_empty() {
        rows.push(rail_row("", body, styles));
    }

    let start = if rows.len() > COMPOSER_ROWS {
        caret_row
            .saturating_sub(COMPOSER_ROWS / 2)
            .min(rows.len() - COMPOSER_ROWS)
    } else {
        0
    };
    let rows: Vec<Line<'static>> = rows.into_iter().skip(start).take(COMPOSER_ROWS).collect();
    (
        rows,
        (
            (caret_row - start) as u16,
            caret_column.min(total - 1) as u16,
        ),
    )
}

/// One composer row: accent rail, then the draft cell.
fn rail_row(text: &str, style: ratatui::style::Style, styles: Styles) -> Line<'static> {
    let mut spans = vec![Span::styled("▎ ", styles.accent())];
    spans.push(Span::styled(text.to_owned(), style));
    Line {
        style: styles.composer(),
        spans,
        alignment: None,
    }
}

fn prev_grapheme(text: &str, cursor: usize) -> usize {
    text[..cursor]
        .grapheme_indices(true)
        .next_back()
        .map_or(0, |(at, _)| at)
}

fn next_grapheme(text: &str, cursor: usize) -> usize {
    text[cursor..]
        .grapheme_indices(true)
        .next()
        .map_or(text.len(), |(_, grapheme)| cursor + grapheme.len())
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

    #[test]
    fn typing_and_deleting_respect_graphemes() {
        let mut c = Composer::new();
        c.type_char('a');
        c.type_char('中');
        c.type_char('😀');
        c.end_gesture();
        assert_eq!(c.text(), "a中😀");
        c.backspace();
        assert_eq!(c.text(), "a中");
        c.backspace();
        assert_eq!(c.text(), "a");
        c.end_gesture();
        c.move_home();
        c.delete();
        assert_eq!(c.text(), "");
    }

    #[test]
    fn multi_codepoint_graphemes_are_one_step() {
        let family = "👨‍👩‍👧‍👦";
        let mut c = composer(family);
        c.move_home();
        c.move_right();
        assert_eq!(c.cursor(), family.len());
        c.move_left();
        assert_eq!(c.cursor(), 0);
        c.delete();
        assert_eq!(c.text(), "");
    }

    #[test]
    fn home_and_end_stay_on_the_current_line() {
        let mut c = composer("one\ntwo");
        assert_eq!(c.cursor(), 7);
        c.move_home();
        assert_eq!(c.cursor(), 4);
        c.move_end();
        assert_eq!(c.cursor(), 7);
        assert!(c.move_up());
        assert_eq!(c.cursor(), 3);
        c.move_home();
        assert_eq!(c.cursor(), 0);
        assert!(c.move_down());
        assert_eq!(c.cursor(), 4);
    }

    #[test]
    fn vertical_movement_keeps_the_display_column() {
        let mut c = composer("abcdef\nxyz\nabcdef");
        c.move_document_start();
        c.move_right();
        c.move_right();
        assert_eq!(c.cursor(), 2);
        assert!(c.move_down());
        assert_eq!(c.cursor(), 9);
        assert!(c.move_down());
        assert_eq!(c.cursor(), 13);
        assert!(c.move_up());
        assert_eq!(c.cursor(), 9);
    }

    #[test]
    fn vertical_movement_clamps_to_short_lines_and_remembers_the_column() {
        let mut c = composer("abcdef\nxy\nabcdef");
        c.move_document_start();
        c.move_right();
        c.move_right();
        c.move_right();
        assert_eq!(c.cursor_position().1, 3);
        assert!(c.move_down());
        assert_eq!(c.cursor(), 9, "the short line ends at column 2");
        assert!(c.move_down());
        assert_eq!(c.cursor(), 13, "the preferred column is still 3");
    }

    #[test]
    fn vertical_movement_reports_the_edges() {
        let mut c = composer("one\ntwo");
        assert!(!c.move_down());
        c.move_up();
        assert!(!c.move_up());
    }

    #[test]
    fn word_motions_stop_at_word_boundaries() {
        let mut c = composer("alpha beta gamma");
        c.move_document_start();
        c.move_word_right();
        assert_eq!(c.cursor(), 6);
        c.move_word_right();
        assert_eq!(c.cursor(), 11);
        c.move_word_right();
        assert_eq!(c.cursor(), 16);
        c.move_word_right();
        assert_eq!(c.cursor(), 16, "the end of the draft is a stop");
        c.move_word_left();
        assert_eq!(c.cursor(), 11);
        c.move_word_left();
        assert_eq!(c.cursor(), 6);
        c.move_word_left();
        assert_eq!(c.cursor(), 0);
    }

    #[test]
    fn word_deletion_stops_at_the_previous_word_start() {
        let mut c = composer("alpha beta gamma");
        c.delete_word_left();
        assert_eq!(c.text(), "alpha beta ");
        c.delete_word_left();
        assert_eq!(c.text(), "alpha ");
        c.delete_word_left();
        assert_eq!(c.text(), "");
    }

    #[test]
    fn line_deletion_is_bounded_by_the_line() {
        let mut c = composer("one\ntwo three");
        c.delete_to_line_start();
        assert_eq!(c.text(), "one\n");
        let mut c = composer("one\ntwo three");
        c.move_home();
        c.move_right();
        c.delete_to_line_end();
        assert_eq!(c.text(), "one\nt");
    }

    #[test]
    fn undo_and_redo_restore_text_and_cursor() {
        let mut c = Composer::new();
        c.insert("hello");
        c.insert(" world");
        assert_eq!(c.text(), "hello world");
        assert!(c.undo());
        assert_eq!(c.text(), "hello");
        assert!(c.undo());
        assert_eq!(c.text(), "");
        assert!(!c.undo());
        assert!(c.redo());
        assert_eq!(c.text(), "hello");
        assert!(c.redo());
        assert_eq!(c.text(), "hello world");
        assert!(!c.redo());
    }

    #[test]
    fn one_typing_gesture_is_one_undo_step() {
        let mut c = Composer::new();
        for ch in "typed".chars() {
            c.type_char(ch);
        }
        c.end_gesture();
        assert!(c.undo());
        assert_eq!(c.text(), "");
    }

    #[test]
    fn large_paste_becomes_a_token_and_keeps_the_payload() {
        let payload = big_paste(40);
        let mut c = Composer::new();
        c.insert(&payload);
        assert_eq!(c.pastes().len(), 1);
        assert_eq!(c.display(), "[pasted 40 lines]");
        assert_eq!(c.text(), payload);
        assert_eq!(c.display_cursor(), c.display().len());
    }

    #[test]
    fn backspace_removes_a_whole_paste_token() {
        let payload = big_paste(40);
        let mut c = Composer::new();
        c.insert("before ");
        c.insert(&payload);
        assert_eq!(c.text(), format!("before {payload}"));
        c.end_gesture();
        c.backspace();
        assert_eq!(c.text(), "before ");
        assert!(c.pastes().is_empty());
    }

    #[test]
    fn deletion_never_leaves_half_a_paste() {
        let payload = big_paste(40);
        let mut c = Composer::new();
        c.insert(&payload);
        c.insert("tail");
        c.end_gesture();
        c.delete_word_left();
        assert!(c.pastes().is_empty());
        assert_eq!(c.text(), "");
    }

    #[test]
    fn the_cursor_steps_over_a_paste_token() {
        let payload = big_paste(40);
        let mut c = Composer::new();
        c.insert(&payload);
        let end = c.text().len();
        assert_eq!(c.cursor(), end, "the cursor sits after the inserted token");
        c.end_gesture();
        c.move_left();
        assert_eq!(c.cursor(), 0);
        c.move_right();
        assert_eq!(c.cursor(), end);
    }

    #[test]
    fn word_motion_steps_over_a_paste_token() {
        let payload = big_paste(40);
        let mut c = Composer::new();
        c.insert(&payload);
        let end = c.text().len();
        c.move_word_right();
        assert_eq!(c.cursor(), end);
        c.end_gesture();
        c.move_word_left();
        assert_eq!(c.cursor(), 0);
    }

    #[test]
    fn small_pastes_stay_plain_text() {
        let mut c = Composer::new();
        c.insert("a short paste");
        assert!(c.pastes().is_empty());
        assert_eq!(c.display(), "a short paste");
    }

    #[test]
    fn display_cursor_accounts_for_tokens() {
        let mut c = Composer::new();
        c.insert(&big_paste(40));
        c.insert("tail");
        assert_eq!(c.display(), "[pasted 40 lines]tail");
        assert_eq!(c.display_cursor(), c.display().len());
        c.end_gesture();
        c.move_home();
        assert_eq!(c.display_cursor(), 0);
        c.move_right();
        assert_eq!(c.display_cursor(), "[pasted 40 lines]".len());
    }

    #[test]
    fn text_edits_shift_paste_ranges() {
        let payload = big_paste(40);
        let mut c = Composer::new();
        c.insert(&payload);
        let before = c.pastes()[0].range.clone();
        c.end_gesture();
        c.move_document_start();
        c.insert("> ");
        assert_eq!(c.pastes()[0].range.start, before.start + 2);
        assert_eq!(c.text(), format!("> {payload}"));
    }

    #[test]
    fn tokens_render_as_single_lines_for_movement() {
        let payload = big_paste(40);
        let mut c = composer("head\n");
        c.insert(&payload);
        c.insert("tail");
        assert_eq!(c.display(), "head\n[pasted 40 lines]tail");
        c.move_home();
        assert_eq!(c.cursor(), 5);
        assert!(!c.move_down(), "the token line is the last line");
        assert!(c.move_up());
        assert_eq!(c.cursor(), 0);
    }

    #[test]
    fn clear_discards_the_draft_and_history() {
        let mut c = composer("draft");
        c.clear();
        assert!(c.is_empty());
        assert!(!c.undo());
    }

    #[test]
    fn submitted_prompts_are_recalled_newest_first() {
        let mut c = Composer::new();
        for prompt in ["first", "second"] {
            c.insert(prompt);
            assert_eq!(c.submit(), prompt);
        }
        assert!(c.is_empty());
        assert!(c.history_prev());
        assert_eq!(c.text(), "second");
        assert!(c.history_prev());
        assert_eq!(c.text(), "first");
        assert!(!c.history_prev());
        assert!(c.history_next());
        assert_eq!(c.text(), "second");
        assert!(c.history_next());
        assert_eq!(c.text(), "");
        assert!(!c.history_next());
    }

    #[test]
    fn recall_restores_the_live_draft() {
        let mut c = Composer::new();
        c.insert("submitted");
        c.submit();
        c.insert("work in progress");
        assert!(c.history_prev());
        assert_eq!(c.text(), "submitted");
        assert!(c.history_next());
        assert_eq!(c.text(), "work in progress");
    }

    #[test]
    fn cursor_position_reports_line_and_cell_column() {
        let mut c = composer("ab\nc中");
        assert_eq!(c.cursor_position(), (1, 3));
        c.move_up();
        assert_eq!(c.cursor_position(), (0, 2));
        c.move_home();
        assert_eq!(c.cursor_position(), (0, 0));
    }

    #[test]
    fn empty_submissions_are_not_recorded_in_history() {
        let mut c = Composer::new();
        c.insert("   ");
        c.submit();
        assert!(!c.history_prev());
    }
}
