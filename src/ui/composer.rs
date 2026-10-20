//! The composer: a grapheme-safe draft with undo, prompt history and paste
//! tokens. The buffer holds the real text; the user sees each large paste as one
//! atomic `[paste #N +L lines]` chip, and [`Projection`] maps between the two.

use std::cell::Cell;
use std::collections::VecDeque;
use std::ops::Range;

use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;

use super::theme::Styles;
use super::wrap;

/// The editor body is never shorter than this many rows.
pub const MIN_ROWS: usize = 4;

/// Pastes of more lines or characters than these become a chip.
const TOKEN_LINES: usize = 10;
const TOKEN_CHARS: usize = 1000;
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
    pub chars: usize,
}

impl Paste {
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
    pastes: Vec<Paste>,
}

/// Text from a paste, an editor or a file: line endings become `\n`, tabs four
/// spaces, and other control bytes are dropped before they reach the Agent.
fn normalize(text: &str) -> String {
    let mut text = text.replace("\r\n", "\n").replace('\r', "\n");
    if text.contains('\t') {
        text = text.replace('\t', "    ");
    }
    if text.chars().any(|ch| ch != '\n' && ch < ' ') {
        text.retain(|ch| ch == '\n' || ch >= ' ');
    }
    text
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
    /// First body row the last frame showed of a draft taller than the editor;
    /// kept so the window only scrolls when the cursor leaves it.
    view_top: Cell<usize>,
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
        // Text also arrives from outside the keyboard — the external editor and
        // a draft file. A CRLF there would reach the Agent as a literal CR.
        let text = normalize(text);
        self.text.clear();
        self.text.push_str(&text);
        self.cursor = self.text.len();
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
        self.cursor = self.text.len();
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
        self.cursor = 0;
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
        // Dropped here, not at commit: typing between undo and redo would
        // otherwise let Ctrl+Y mix the abandoned branch back in.
        if self.gesture.is_none() {
            self.redo.clear();
        }
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
        let text = normalize(text);
        let text = text.as_str();
        self.snapshot();
        let start = self.cursor;
        self.text.insert_str(start, text);
        self.cursor = start + text.len();
        self.shift(start, text.len());
        let lines = text.lines().count().max(1);
        let chars = text.chars().count();
        if lines > TOKEN_LINES || chars > TOKEN_CHARS {
            self.pastes.push(Paste {
                range: start..self.cursor,
                lines,
                chars,
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
        self.commit();
        let end = self.cursor;
        self.move_word_left();
        let start = self.cursor;
        self.cursor = end;
        self.delete_range(start..end, true);
    }

    pub fn delete_to_line_start(&mut self) {
        self.commit();
        let end = self.cursor;
        let start = self.line_start(self.cursor_position().0);
        self.cursor = end;
        self.delete_range(start..end, true);
    }

    pub fn delete_to_line_end(&mut self) {
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
    fn load(&mut self, text: String) {
        self.commit();
        self.gesture = None;
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

/// The editor body is never taller than this many rows; completions come on top.
const MAX_ROWS: usize = 12;

/// Rows the editor body may take on a terminal `height` rows tall.
pub fn max_rows(height: u16) -> usize {
    (usize::from(height) * 32 / 100).clamp(MIN_ROWS, MAX_ROWS)
}

const PLACEHOLDER: &str = "Ask anything · / commands · @ files";

impl Composer {
    /// The draft as drawn: sanitized text with chips in place of payloads, the
    /// chips' byte ranges in it and the cursor's byte offset in it.
    fn shown(&self) -> (String, Vec<Range<usize>>, usize) {
        let mut shown = String::with_capacity(self.text.len());
        let mut chips = Vec::new();
        let mut caret = None;
        let mut at = 0;
        let ends = self.pastes.iter().map(|paste| paste.range.start);
        for (index, end) in ends.chain([self.text.len()]).enumerate() {
            if caret.is_none() && (at..=end).contains(&self.cursor) {
                // Sanitizing the prefix keeps an escape sequence whole; a cursor
                // inside one lands on the cell before it.
                caret = Some(shown.len() + wrap::sanitize(&self.text[at..self.cursor]).len());
            }
            shown.push_str(&wrap::sanitize(&self.text[at..end]));
            let Some(paste) = self.pastes.get(index) else {
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
    let (shown, chips, caret) = composer.shown();
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

    #[test]
    fn pasted_text_is_normalized_before_it_reaches_the_agent() {
        let mut c = Composer::new();
        c.insert("one\r\ntwo\rthree\tfour\u{7}five");
        assert_eq!(c.text(), "one\ntwo\nthree    fourfive");
        assert!(!c.text().contains('\r'));
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
