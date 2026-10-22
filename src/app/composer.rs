//! The composer: a grapheme-safe draft with undo, prompt history and paste
//! tokens. The buffer holds the real text; the user sees each large paste as one
//! atomic `[paste #N +L lines]` chip, and [`Projection`] maps between the two.

use std::cell::Cell;
use std::collections::VecDeque;
use std::ops::Range;
use std::path::PathBuf;

use unicode_segmentation::UnicodeSegmentation;

use crate::prompt_history::{self, Record};
use crate::wrap;

/// Pastes of more lines or characters than these become a chip.
const TOKEN_LINES: usize = 10;
const TOKEN_CHARS: usize = 1000;
/// Undo steps kept before the oldest is dropped.
const UNDO_STEPS: usize = 200;
/// Bytes of draft text the undo and redo steps may hold together.
const UNDO_BYTES: usize = 8 << 20;
/// Submitted prompts kept for recall.
const HISTORY_STEPS: usize = 100;

/// A paste displayed as one token.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Paste {
    /// Byte range of the payload in the buffer.
    pub range: Range<usize>,
    pub lines: usize,
    pub chars: usize,
}

impl Paste {
    fn new(text: &str, range: Range<usize>) -> Self {
        let payload = &text[range.clone()];
        Self {
            range,
            lines: payload.lines().count().max(1),
            chars: payload.chars().count(),
        }
    }

    /// The chip text; `number` counts the pastes in the draft from 1.
    pub fn label(&self, number: usize) -> String {
        if self.lines > TOKEN_LINES {
            format!("[paste #{number} +{} lines]", self.lines)
        } else {
            format!("[paste #{number} {} chars]", self.chars)
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
        for (index, paste) in self.pastes.iter().enumerate() {
            out.push_str(&self.text[at..paste.range.start]);
            out.push_str(&paste.label(index + 1));
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
        for (index, paste) in self.pastes.iter().enumerate() {
            if raw <= paste.range.start {
                return shown + raw - at;
            }
            shown += paste.range.start - at + paste.label(index + 1).len();
            at = paste.range.end;
        }
        shown + raw.saturating_sub(at)
    }

    /// Buffer offset of a display offset. Offsets inside a token snap to the
    /// start of its payload.
    pub fn raw(&self, display: usize) -> usize {
        let mut shown = 0;
        let mut at = 0;
        for (index, paste) in self.pastes.iter().enumerate() {
            let head = paste.range.start - at;
            if display <= shown + head {
                return at + display.saturating_sub(shown);
            }
            shown += head;
            let token = paste.label(index + 1).len();
            // Inside the token snaps to its payload; the cell right after it is
            // after the token, so typing there never lands inside the payload.
            if display < shown + token {
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
    anchor: Option<usize>,
    pastes: Vec<Paste>,
}

/// A submitted prompt or the draft set aside while recalling one, with its chips.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct Entry {
    text: String,
    pastes: Vec<Paste>,
}

impl Entry {
    fn from_record(record: Record) -> Self {
        let Record { text, pastes } = record;
        let mut end = 0;
        // A hand-edited file must not panic the slicing below.
        let valid = pastes.iter().all(|range| {
            let ok = range.start >= end
                && range.start < range.end
                && text.is_char_boundary(range.start)
                && text.is_char_boundary(range.end);
            end = range.end;
            ok
        });
        // Prompts saved before byte order marks were dropped still carry them.
        let bom = '\u{feff}';
        let at = |at: usize| at - text[..at].matches(bom).count() * bom.len_utf8();
        let pastes: Vec<_> = if valid {
            pastes
                .into_iter()
                .map(|range| at(range.start)..at(range.end))
                .filter(|range| !range.is_empty())
                .collect()
        } else {
            Vec::new()
        };
        let text = text.replace(bom, "");
        let pastes = pastes
            .into_iter()
            .map(|range| Paste::new(&text, range))
            .collect();
        Self { text, pastes }
    }

    fn record(&self) -> Record {
        Record {
            text: self.text.clone(),
            pastes: self
                .pastes
                .iter()
                .map(|paste| paste.range.clone())
                .collect(),
        }
    }
}

/// Text from a paste, an editor or a file: line endings become `\n`, tabs four
/// spaces, and other control bytes and byte order marks are dropped before they
/// reach the Agent.
fn normalize(text: &str) -> String {
    let mut text = text.replace("\r\n", "\n").replace('\r', "\n");
    if text.contains('\t') {
        text = text.replace('\t', "    ");
    }
    // Windows Terminal draws U+FEFF as a cell its width says it lacks, which
    // shifts the text past the caret.
    if text
        .chars()
        .any(|ch| (ch != '\n' && ch < ' ') || ch == '\u{feff}')
    {
        text.retain(|ch| ch == '\n' || (ch >= ' ' && ch != '\u{feff}'));
    }
    text
}

#[derive(Default)]
pub struct Composer {
    text: String,
    /// Cursor byte offset, at a grapheme boundary and outside every payload.
    cursor: usize,
    /// Fixed end of the editor selection, separate from transcript selection.
    anchor: Option<usize>,
    /// Display column to keep while moving between lines.
    column: Option<usize>,
    pastes: Vec<Paste>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// The buffer before the current typing gesture, committed when it ends.
    gesture: Option<Snapshot>,
    history: VecDeque<Entry>,
    /// Where `history` is saved after each submit.
    history_file: Option<PathBuf>,
    /// Position in `history` while recalling; `None` means the live draft.
    recall: Option<usize>,
    draft: Entry,
    /// Counts the changes to the draft text, so a caller can tell an edit from
    /// a cursor move without comparing the text.
    version: u64,
    /// First body row the last frame showed of a draft taller than the editor;
    /// kept so the window only scrolls when the cursor leaves it.
    pub view_top: Cell<usize>,
}

impl Composer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Loads the saved prompt history from `file` and saves it there on every submit.
    pub fn with_history_file(file: PathBuf) -> Self {
        let history = prompt_history::load(&file)
            .into_iter()
            .map(Entry::from_record)
            .collect();
        Self {
            history,
            history_file: Some(file),
            ..Self::default()
        }
    }

    pub fn version(&self) -> u64 {
        self.version
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

    pub fn selection(&self) -> Option<Range<usize>> {
        let anchor = self.anchor.filter(|&at| at != self.cursor)?;
        Some(anchor.min(self.cursor)..anchor.max(self.cursor))
    }

    pub fn selected_text(&self) -> Option<&str> {
        self.selection().map(|range| &self.text[range])
    }

    pub fn clear_selection(&mut self) {
        self.anchor = None;
    }

    /// Runs a caret motion that extends the selection from its anchor.
    pub fn extend_selection(&mut self, motion: impl FnOnce(&mut Self)) {
        self.close_gesture();
        let anchor = self.anchor.take().unwrap_or(self.cursor);
        motion(self);
        self.anchor = Some(anchor);
    }

    /// Moves the caret to `at`, an editing stop from the editor's hit test.
    pub fn place_cursor(&mut self, at: usize, extend: bool) {
        self.close_gesture();
        self.anchor = extend.then_some(self.anchor.unwrap_or(self.cursor));
        self.cursor = at;
        self.column = None;
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
        // Text also arrives from outside the keyboard — the external editor and
        // a draft file. A CRLF there would reach the Agent as a literal CR.
        let text = normalize(text);
        self.text = text;
        self.version += 1;
        self.cursor = self.text.len();
        self.anchor = None;
        self.pastes.clear();
        self.undo.clear();
        self.redo.clear();
        self.gesture = None;
        self.column = None;
        self.recall = None;
    }

    /// Puts text back without an undo step of its own (a refused send, an editor
    /// round trip), so the next undo walks back through the user's typing.
    pub fn restore_text(&mut self, text: &str) {
        let text = normalize(text);
        if self.text == text {
            return;
        }
        self.commit();
        self.text = text;
        self.version += 1;
        self.cursor = self.text.len();
        self.anchor = None;
        self.pastes.clear();
        self.column = None;
    }

    /// Clears the draft the way an edit would, so Ctrl+Z brings it back.
    pub fn clear(&mut self) {
        if self.text.is_empty() {
            return;
        }
        self.snapshot();
        self.text.clear();
        self.version += 1;
        self.cursor = 0;
        self.anchor = None;
        self.pastes.clear();
        self.column = None;
        self.commit();
    }

    /// Ends the current typing gesture so the next edit starts a new undo step.
    #[cfg(test)]
    pub fn end_gesture(&mut self) {
        self.commit();
        self.column = None;
    }

    /// Records a submitted prompt and starts a fresh draft.
    pub fn submit(&mut self) -> String {
        self.commit();
        let entry = Entry {
            text: std::mem::take(&mut self.text),
            pastes: std::mem::take(&mut self.pastes),
        };
        self.version += 1;
        self.cursor = 0;
        self.anchor = None;
        self.column = None;
        self.recall = None;
        self.draft = Entry::default();
        let text = entry.text.clone();
        let repeat = self.history.back().is_some_and(|last| last.text == text);
        if !text.trim().is_empty() && !repeat {
            self.history.push_back(entry);
            if self.history.len() > HISTORY_STEPS {
                self.history.pop_front();
            }
            self.save_history();
        }
        text
    }

    fn save_history(&self) {
        let Some(file) = &self.history_file else {
            return;
        };
        let records: Vec<Record> = self.history.iter().map(Entry::record).collect();
        // Losing the saved history must not get in the way of sending.
        let _ = prompt_history::save(file, &records);
    }

    // ---- undo ---------------------------------------------------------

    fn snapshot(&mut self) {
        // Dropped here, not at commit: typing between undo and redo would
        // otherwise let Ctrl+Y mix the abandoned branch back in.
        if self.gesture.is_none() {
            self.redo.clear();
        }
        self.gesture.get_or_insert_with(|| Snapshot {
            text: self.text.clone(),
            cursor: self.cursor,
            anchor: self.anchor,
            pastes: self.pastes.clone(),
        });
    }

    fn commit(&mut self) {
        self.recall = None;
        self.close_gesture();
    }

    /// Closes the open edit gesture as one undo step; a caret move does this
    /// without leaving a recalled prompt.
    fn close_gesture(&mut self) {
        let Some(snapshot) = self.gesture.take() else {
            return;
        };
        if snapshot.text == self.text {
            return;
        }
        self.undo.push(snapshot);
        let mut bytes: usize = self.undo.iter().map(|step| step.text.len()).sum();
        while self.undo.len() > UNDO_STEPS || (bytes > UNDO_BYTES && self.undo.len() > 1) {
            bytes -= self.undo.remove(0).text.len();
        }
        self.redo.clear();
    }

    fn current(&self) -> Snapshot {
        Snapshot {
            text: self.text.clone(),
            cursor: self.cursor,
            anchor: self.anchor,
            pastes: self.pastes.clone(),
        }
    }

    fn restore(&mut self, snapshot: Snapshot) {
        self.text = snapshot.text;
        self.version += 1;
        self.cursor = snapshot.cursor;
        self.anchor = snapshot.anchor;
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
        let text = normalize(text);
        let text = text.as_str();
        self.snapshot();
        self.erase_selection();
        let start = self.cursor;
        self.text.insert_str(start, text);
        self.version += 1;
        self.cursor = start + text.len();
        self.shift(start, text.len());
        let paste = Paste::new(&self.text, start..self.cursor);
        if paste.lines > TOKEN_LINES || paste.chars > TOKEN_CHARS {
            self.pastes.push(paste);
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
        self.erase_selection();
        let mut buf = [0u8; 4];
        let text = ch.encode_utf8(&mut buf);
        let start = self.cursor;
        self.text.insert_str(start, text);
        self.version += 1;
        self.cursor = start + text.len();
        self.shift(start, text.len());
        self.column = None;
    }

    pub fn newline(&mut self) {
        self.snapshot();
        self.erase_selection();
        let start = self.cursor;
        self.text.insert(start, '\n');
        self.version += 1;
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
        if let Some(range) = self.selection() {
            self.cursor = range.start;
            self.anchor = None;
            self.column = None;
            return;
        }
        self.anchor = None;
        self.cursor = match self.paste_ending_at(self.cursor) {
            Some(paste) => paste.range.start,
            None => prev_grapheme(&self.text, self.cursor),
        };
        self.column = None;
    }

    pub fn move_right(&mut self) {
        if let Some(range) = self.selection() {
            self.cursor = range.end;
            self.anchor = None;
            self.column = None;
            return;
        }
        self.anchor = None;
        self.cursor = match self.paste_starting_at(self.cursor) {
            Some(paste) => paste.range.end,
            None => next_grapheme(&self.text, self.cursor),
        };
        self.column = None;
    }

    pub fn move_home(&mut self) {
        self.anchor = None;
        self.cursor = self.line_start(self.cursor_position().0);
        self.column = None;
    }

    pub fn move_end(&mut self) {
        self.anchor = None;
        self.cursor = self.line_end(self.cursor_position().0);
        self.column = None;
    }

    /// Moves one logical line up. False on the first line, where the caller
    /// may switch to prompt recall.
    pub fn move_up(&mut self) -> bool {
        self.anchor = None;
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
        self.anchor = None;
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
        self.anchor = None;
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
        self.anchor = None;
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

    fn erase_selection(&mut self) {
        if let Some(range) = self.selection() {
            self.erase_range(range);
        }
        self.anchor = None;
    }

    pub fn delete_selection(&mut self) -> bool {
        let Some(range) = self.selection() else {
            return false;
        };
        self.commit();
        self.delete_range(range, true);
        true
    }

    pub fn backspace(&mut self) {
        if self.delete_selection() {
            return;
        }
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
        if self.delete_selection() {
            return;
        }
        self.commit();
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
        if self.delete_selection() {
            return;
        }
        self.commit();
        let end = self.cursor;
        self.move_word_left();
        let start = self.cursor;
        self.cursor = end;
        self.delete_range(start..end, true);
    }

    /// Deletes to the end of the next word, or a whole chip.
    pub fn delete_word_right(&mut self) {
        if self.delete_selection() {
            return;
        }
        self.commit();
        let start = self.cursor;
        let blank = |from: usize, to: usize| self.text[from..to].chars().all(char::is_whitespace);
        let mut end = start;
        while end < self.text.len() && blank(end, next_grapheme(&self.text, end)) {
            end = next_grapheme(&self.text, end);
        }
        if let Some(paste) = self.paste_starting_at(end) {
            end = paste.range.end;
        } else {
            while end < self.text.len()
                && self.paste_starting_at(end).is_none()
                && !blank(end, next_grapheme(&self.text, end))
            {
                end = next_grapheme(&self.text, end);
            }
        }
        self.delete_range(start..end, true);
    }

    pub fn delete_to_line_start(&mut self) {
        if self.delete_selection() {
            return;
        }
        self.commit();
        let end = self.cursor;
        let start = self.line_start(self.cursor_position().0);
        self.cursor = end;
        self.delete_range(start..end, true);
    }

    pub fn delete_to_line_end(&mut self) {
        if self.delete_selection() {
            return;
        }
        self.commit();
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
        self.erase_range(range);
        self.commit();
    }

    fn erase_range(&mut self, range: Range<usize>) {
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
        self.version += 1;
        self.pastes
            .retain(|paste| paste.range.end <= start || paste.range.start >= end);
        for paste in &mut self.pastes {
            if paste.range.start >= end {
                paste.range.start -= len;
                paste.range.end -= len;
            }
        }
        self.cursor = self.cursor.min(start);
        self.anchor = None;
        self.column = None;
    }

    // ---- prompt history -----------------------------------------------

    /// Recalls an older prompt, keeping the live draft for the way back.
    pub fn history_prev(&mut self) -> bool {
        if self.history.is_empty() {
            return false;
        }
        let index = match self.recall {
            None => {
                self.draft = Entry {
                    text: std::mem::take(&mut self.text),
                    pastes: std::mem::take(&mut self.pastes),
                };
                self.history.len() - 1
            }
            Some(0) => return false,
            Some(index) => index - 1,
        };
        self.load(self.history[index].clone());
        self.recall = Some(index);
        true
    }

    pub fn history_next(&mut self) -> bool {
        let Some(index) = self.recall else {
            return false;
        };
        if index + 1 < self.history.len() {
            self.load(self.history[index + 1].clone());
            self.recall = Some(index + 1);
            return true;
        }
        let draft = std::mem::take(&mut self.draft);
        self.load(draft);
        self.recall = None;
        true
    }

    /// Swaps in a recalled draft, first committing the live gesture so earlier
    /// typing does not merge with what follows.
    fn load(&mut self, entry: Entry) {
        self.commit();
        self.gesture = None;
        self.text = entry.text;
        self.version += 1;
        self.cursor = self.text.len();
        self.anchor = None;
        self.pastes = entry.pastes;
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

    fn move_to_start(composer: &mut Composer) {
        while composer.move_up() {}
        composer.move_home();
    }

    fn big_paste(lines: usize) -> String {
        (0..lines).map(|i| format!("line {i}\n")).collect()
    }

    fn submit_all(c: &mut Composer, prompts: &[&str]) {
        for prompt in prompts {
            c.insert(prompt);
            c.submit();
        }
    }

    #[test]
    fn a_paste_chip_survives_a_history_round_trip() {
        let payload = big_paste(15);
        let mut c = Composer::new();
        c.insert(&payload);
        c.submit();
        c.insert("half typed ");
        c.insert(&payload);
        let draft = c.display();

        assert!(c.history_prev());
        assert_eq!(c.display(), "[paste #1 +15 lines]", "the recalled prompt");
        assert_eq!(c.text(), payload);
        assert!(c.history_next());
        assert_eq!(c.display(), draft, "the live draft keeps its chip");
        assert_eq!(c.pastes().len(), 1);
    }

    #[test]
    fn a_repeated_prompt_is_recorded_once() {
        let mut c = Composer::new();
        submit_all(&mut c, &["same", "same", "other", "same"]);
        let mut recalled = Vec::new();
        while c.history_prev() {
            recalled.push(c.text().to_owned());
        }
        assert_eq!(recalled, ["same", "other", "same"]);
    }

    #[test]
    fn the_history_file_restores_prompts_and_chips_in_a_new_composer() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("state").join("history.jsonl");
        let payload = big_paste(15);
        let mut c = Composer::with_history_file(file.clone());
        submit_all(&mut c, &["first", "second\nline"]);
        c.insert("before ");
        c.insert(&payload);
        c.insert(" after");
        c.submit();

        let mut restarted = Composer::with_history_file(file);
        assert!(restarted.history_prev());
        assert_eq!(restarted.display(), "before [paste #1 +15 lines] after");
        assert_eq!(restarted.text(), format!("before {payload} after"));
        assert!(restarted.history_prev());
        assert_eq!(restarted.text(), "second\nline");
        assert!(restarted.history_prev());
        assert_eq!(restarted.text(), "first");
        assert!(!restarted.history_prev());
    }

    #[test]
    fn the_saved_history_keeps_only_the_newest_prompts() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("history.jsonl");
        let mut c = Composer::with_history_file(file.clone());
        for index in 0..HISTORY_STEPS + 5 {
            c.insert(&format!("prompt {index}"));
            c.submit();
        }
        let saved = prompt_history::load(&file);
        assert_eq!(saved.len(), HISTORY_STEPS);
        assert_eq!(saved[0].text, "prompt 5");
        assert_eq!(
            saved.last().unwrap().text,
            format!("prompt {}", HISTORY_STEPS + 4)
        );
    }

    #[test]
    fn a_damaged_chip_range_in_the_file_leaves_plain_text() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("history.jsonl");
        std::fs::write(
            &file,
            "{\"text\":\"héllo\",\"pastes\":[{\"start\":0,\"end\":2}]}\n",
        )
        .unwrap();
        let mut c = Composer::with_history_file(file);
        assert!(c.history_prev());
        assert_eq!(c.text(), "héllo");
        assert!(c.pastes().is_empty());
    }

    #[test]
    fn the_version_moves_with_the_text_and_not_with_the_cursor() {
        let mut c = composer("one\ntwo");
        let before = c.version();
        c.move_up();
        c.move_home();
        c.move_word_right();
        assert_eq!(c.version(), before, "moving is not an edit");
        c.type_char('x');
        assert_ne!(c.version(), before);
        let typed = c.version();
        assert!(c.undo());
        assert_ne!(c.version(), typed, "undo changes the text too");
    }

    #[test]
    fn undo_memory_is_bounded_for_a_large_draft() {
        let mut c = composer(&"x".repeat(UNDO_BYTES / 4));
        for _ in 0..40 {
            c.insert("y");
        }
        assert!(c.undo.len() <= 4, "kept {} steps", c.undo.len());
        assert!(c.undo(), "the newest step is always kept");
    }

    #[test]
    fn delete_word_right_removes_the_next_word_and_is_one_undo_step() {
        let mut c = composer("alpha beta  gamma");
        c.end_gesture();
        move_to_start(&mut c);
        c.delete_word_right();
        assert_eq!(c.text(), " beta  gamma");
        c.delete_word_right();
        assert_eq!(c.text(), "  gamma");
        assert_eq!(c.cursor(), 0);
        assert!(c.undo());
        assert_eq!(c.text(), " beta  gamma");
        c.move_end();
        c.delete_word_right();
        assert_eq!(c.text(), " beta  gamma", "nothing after the cursor");
    }

    #[test]
    fn delete_word_right_stops_before_a_chip_and_then_takes_it_whole() {
        let mut c = composer("ab");
        c.insert(&big_paste(12));
        c.insert(" tail");
        move_to_start(&mut c);
        c.delete_word_right();
        assert_eq!(c.display(), "[paste #1 +12 lines] tail");
        c.delete_word_right();
        assert_eq!(
            c.display(),
            " tail",
            "the whole chip, not a line of its payload"
        );
        assert!(c.pastes().is_empty());
    }

    #[test]
    fn pasted_text_is_normalized_before_it_reaches_the_agent() {
        let mut c = Composer::new();
        c.insert("one\r\ntwo\rthree\tfour\u{7}five");
        assert_eq!(c.text(), "one\ntwo\nthree    fourfive");
        assert!(!c.text().contains('\r'));
    }

    #[test]
    fn a_pasted_byte_order_mark_is_dropped() {
        let mut c = Composer::new();
        c.insert("\u{feff}作用abc");
        assert_eq!(c.text(), "作用abc");
        assert_eq!(c.cursor, c.text().len());
    }

    #[test]
    fn a_saved_prompt_with_a_byte_order_mark_is_recalled_without_it() {
        let entry = Entry::from_record(Record {
            text: "\u{feff}ab\u{feff}cd".into(),
            pastes: vec![3..5, 8..10],
        });
        assert_eq!(entry.text, "abcd");
        let ranges: Vec<_> = entry.pastes.iter().map(|p| p.range.clone()).collect();
        assert_eq!(ranges, [0..2, 2..4]);
    }

    #[test]
    fn text_from_the_editor_is_normalized_too() {
        // An editor that saves CRLF must not hand the Agent a literal CR.
        let mut c = Composer::new();
        c.set_text("alpha\r\nbeta\rgamma");
        assert_eq!(c.text(), "alpha\nbeta\ngamma");
        assert!(!c.text().contains('\r'));
        assert_eq!(c.cursor, c.text().len(), "the cursor sits at the end");
    }

    #[test]
    fn paste_token_counts_follow_the_normalized_text() {
        let mut c = Composer::new();
        c.insert(&"line\r\n".repeat(TOKEN_LINES + 1));
        assert_eq!(c.pastes.len(), 1, "a long paste becomes one token");
        assert_eq!(c.text().matches('\n').count(), TOKEN_LINES + 1);
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
        move_to_start(&mut c);
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
        move_to_start(&mut c);
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
        move_to_start(&mut c);
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
        assert_eq!(c.display(), "[paste #1 +40 lines]");
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
        assert_eq!(c.display(), "[paste #1 +40 lines]tail");
        assert_eq!(c.display_cursor(), c.display().len());
        c.end_gesture();
        c.move_home();
        assert_eq!(c.display_cursor(), 0);
        c.move_right();
        assert_eq!(c.display_cursor(), "[paste #1 +40 lines]".len());
    }

    #[test]
    fn text_edits_shift_paste_ranges() {
        let payload = big_paste(40);
        let mut c = Composer::new();
        c.insert(&payload);
        let before = c.pastes()[0].range.clone();
        c.end_gesture();
        move_to_start(&mut c);
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
        assert_eq!(c.display(), "head\n[paste #1 +40 lines]tail");
        c.move_home();
        assert_eq!(c.cursor(), 5);
        assert!(!c.move_down(), "the token line is the last line");
        assert!(c.move_up());
        assert_eq!(c.cursor(), 0);
    }

    /// One long payload line, so a display offset equals a buffer offset in
    /// the head and only the token shifts the tail.
    fn pasted() -> Composer {
        let mut c = Composer::new();
        c.insert("before ");
        c.insert(&"x".repeat(TOKEN_CHARS + 1));
        c.insert("after");
        c
    }

    fn token_end(c: &Composer) -> usize {
        let token = c.pastes()[0].label(1);
        let shown = c.projection().text();
        shown.find(&token).expect("the paste shows as its token") + token.len()
    }

    #[test]
    fn the_cell_after_a_token_maps_past_the_payload() {
        let c = pasted();
        assert_eq!(
            &c.text()[c.projection().raw(token_end(&c))..],
            "after",
            "the trailing edge is the position after the paste, not inside it"
        );
    }

    #[test]
    fn move_end_and_typing_stay_after_the_trailing_text() {
        let mut c = pasted();
        c.move_end();
        let end = c.cursor();
        assert_eq!(end, c.text().len());
        c.type_char('!');
        assert!(c.text().ends_with("after!"), "{:?}", c.text());
        assert_eq!(c.pastes().len(), 1, "the payload stays one token");
    }

    #[test]
    fn ctrl_e_and_end_keep_the_cursor_out_of_a_payload() {
        let mut c = pasted();
        c.move_home();
        assert_eq!(c.cursor(), 0);
        c.move_down();
        assert!(
            c.pastes()
                .iter()
                .all(|paste| !paste.range.contains(&c.cursor())),
            "the cursor never lands inside a payload"
        );
        c.move_end();
        assert_eq!(c.cursor(), c.text().len());
    }

    #[test]
    fn a_recall_ends_the_typing_gesture() {
        let mut c = Composer::new();
        c.insert("first");
        assert_eq!(c.submit(), "first");
        c.type_char('d');
        c.type_char('r');
        c.type_char('a');
        c.type_char('f');
        c.type_char('t');
        assert!(c.history_prev(), "recall the submitted prompt");
        assert_eq!(c.text(), "first");
        c.type_char('!');
        assert_eq!(c.text(), "first!");
        assert!(c.undo(), "the edit after the recall is undoable");
        assert_eq!(
            c.text(),
            "first",
            "undo stops at the recalled draft, not the pre-recall typing"
        );
        assert!(c.undo());
        assert_eq!(
            c.text(),
            "",
            "the pre-recall typing stays its own step, ending where it began"
        );
    }

    #[test]
    fn a_kill_does_not_swallow_the_typing_before_it() {
        let mut c = Composer::new();
        c.end_gesture();
        for ch in "alpha beta".chars() {
            c.type_char(ch);
        }
        c.delete_word_left();
        assert_eq!(c.text(), "alpha ");
        // First undo restores only the killed word; the typing stays put.
        assert!(c.undo());
        assert_eq!(c.text(), "alpha beta");
    }

    #[test]
    fn a_word_delete_is_one_undoable_step() {
        let mut c = composer("alpha beta gamma");
        c.end_gesture();
        c.delete_word_left();
        assert_eq!(c.text(), "alpha beta ");
        assert!(c.undo());
        assert_eq!(c.text(), "alpha beta gamma");
    }

    #[test]
    fn clear_is_undoable_and_keeps_earlier_undo_steps() {
        let mut c = composer("draft");
        c.insert(" more");
        c.clear();
        assert!(c.is_empty());
        assert!(c.undo(), "clearing the draft is itself an undo step");
        assert_eq!(c.text(), "draft more");
        assert!(c.undo(), "the typing before the clear is still undoable");
        assert_eq!(c.text(), "draft");
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

    #[test]
    fn shift_arrows_extend_shrink_reverse_and_never_split_graphemes() {
        let mut c = composer("a中e\u{301}👨‍👩‍👧‍👦z");
        c.extend_selection(Composer::move_left);
        assert_eq!(c.selected_text(), Some("z"));
        c.extend_selection(Composer::move_left);
        assert_eq!(c.selected_text(), Some("👨‍👩‍👧‍👦z"));
        c.extend_selection(Composer::move_right);
        assert_eq!(c.selected_text(), Some("z"));
        c.extend_selection(Composer::move_right);
        assert!(c.selection().is_none());
        c.place_cursor(1, false);
        c.extend_selection(Composer::move_left);
        assert_eq!(c.selected_text(), Some("a"));
        c.extend_selection(Composer::move_right);
        c.extend_selection(Composer::move_right);
        assert_eq!(c.selected_text(), Some("中"));
        c.move_right();
        assert_eq!(c.cursor(), "a中".len());
        assert!(c.selection().is_none());
    }

    #[test]
    fn typing_pasting_newline_and_delete_replace_selection_in_one_undo_step() {
        for operation in 0..4 {
            let mut c = composer("one中two");
            c.place_cursor(3, false);
            c.place_cursor(6, true);
            match operation {
                0 => c.type_char('X'),
                1 => c.insert("X\nY"),
                2 => c.newline(),
                _ => c.backspace(),
            }
            assert_eq!(
                c.text(),
                ["oneXtwo", "oneX\nYtwo", "one\ntwo", "onetwo"][operation]
            );
            assert!(c.selection().is_none());
            assert!(c.undo());
            assert_eq!(c.text(), "one中two");
            assert_eq!(c.selected_text(), Some("中"));
            assert!(c.redo());
            assert!(c.selection().is_none());
        }
    }

    #[test]
    fn paste_payload_is_selected_replaced_and_restored_as_an_atomic_chip() {
        let mut c = composer("prefix ");
        let payload = "line\n".repeat(12);
        c.insert(&payload);
        c.extend_selection(Composer::move_left);
        assert_eq!(c.selected_text(), Some(payload.as_str()));
        c.type_char('X');
        assert_eq!(c.text(), "prefix X");
        assert!(c.pastes().is_empty());
        c.undo();
        assert_eq!(c.pastes().len(), 1);
        assert_eq!(c.selected_text(), Some(payload.as_str()));
    }

    #[test]
    fn selecting_in_a_recalled_prompt_keeps_the_draft_to_return_to() {
        let mut c = composer("first");
        c.submit();
        c.type_char('d');
        assert!(c.history_prev());
        c.extend_selection(Composer::move_left);
        c.place_cursor(0, false);
        assert!(c.history_next());
        assert_eq!(c.text(), "d");
    }
}

#[cfg(test)]
mod undo_gesture_tests {
    use super::*;

    /// Typing, then undo, then redo: the ordinary editing loop must work
    /// without the caller having to close the gesture first.
    #[test]
    fn type_undo_redo_is_one_round_trip() {
        let mut c = Composer::new();
        for ch in "word".chars() {
            c.type_char(ch);
        }
        assert!(c.undo(), "undo after typing must do something");
        assert_eq!(c.text(), "");
        assert!(c.redo());
        assert_eq!(c.text(), "word");
    }

    /// An undo that closes a live gesture must not also swallow the earlier
    /// step: one Ctrl+Z after two separate words removes the second word.
    #[test]
    fn undo_closes_the_gesture_and_takes_one_step() {
        let mut c = Composer::new();
        for ch in "one ".chars() {
            c.type_char(ch);
        }
        c.end_gesture();
        for ch in "two".chars() {
            c.type_char(ch);
        }
        assert!(c.undo());
        assert_eq!(c.text(), "one ");
        assert!(c.undo());
        assert_eq!(c.text(), "");
        assert!(c.redo());
        assert_eq!(c.text(), "one ");
    }

    #[test]
    fn undo_after_a_paste_removes_the_whole_paste() {
        let mut c = Composer::new();
        c.type_char('a');
        c.end_gesture();
        c.insert("pasted");
        assert!(c.undo());
        assert_eq!(c.text(), "a");
        assert!(c.redo());
        assert_eq!(c.text(), "apasted");
    }

    #[test]
    fn undo_after_a_kill_restores_the_killed_text() {
        let mut c = Composer::new();
        for ch in "keep this".chars() {
            c.type_char(ch);
        }
        c.end_gesture();
        c.delete_word_left();
        assert_eq!(c.text(), "keep ");
        assert!(c.undo());
        assert_eq!(c.text(), "keep this");
    }

    #[test]
    fn undo_after_a_clear_restores_the_draft() {
        let mut c = Composer::new();
        for ch in "draft".chars() {
            c.type_char(ch);
        }
        c.end_gesture();
        c.clear();
        assert_eq!(c.text(), "");
        assert!(c.undo());
        assert_eq!(c.text(), "draft");
    }

    /// Redo must not resurrect a typing gesture either: after undo + new
    /// typing, the old redo branch is gone.
    #[test]
    fn typing_after_undo_drops_the_redo_branch() {
        let mut c = Composer::new();
        for ch in "ab".chars() {
            c.type_char(ch);
        }
        assert!(c.undo());
        c.type_char('c');
        assert!(!c.redo(), "the abandoned branch is not reachable");
        assert_eq!(c.text(), "c");
    }
}
