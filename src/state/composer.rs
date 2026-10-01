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
pub const MAX_COMPOSER_BYTES: usize = crate::limits::COMPOSER_DRAFT_BYTES;
/// Fixed undo capacity keeps old snapshots from bypassing the draft budget.
pub const MAX_COMPOSER_HISTORIES: usize = 128;
/// The pinned editor does not expose its undo snapshots, so the draft budget
/// charges at most this many buffer-sized undo records until the capacity is
/// trimmed (spec §12.1: a measured fixed count is acceptable).
pub const UNDO_SNAPSHOT_ESTIMATE: usize = 8;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PasteRange {
    pub id: usize,
    pub start: usize,
    pub end: usize,
    pub line_count: usize,
    pub char_count: usize,
}

/// Whether navigating history touches the editor's live draft.
///
/// Debug is manual: it reports sizes and the cursor only, never draft text.
pub struct Composer {
    textarea: TextArea<'static>,
    history: VecDeque<String>,
    /// Position within `history` while recalling an old message;
    /// `None` means the editor holds the live draft.
    history_index: Option<usize>,
    draft: String,
    /// Monotonic content-edit generation used to correlate delayed acks.
    editor_revision: u64,
    /// Cached UTF-8 byte length of the buffer. Ordinary edits adjust it by
    /// their delta instead of joining the whole buffer (spec §12.1, §25.1).
    byte_len: usize,
    /// Display-only ranges for large paste payloads. The payload remains in
    /// the TextArea and is what `content()` returns.
    pastes: Vec<PasteRange>,
    paste_undo: Vec<Vec<PasteRange>>,
    paste_redo: Vec<Vec<PasteRange>>,
    /// Current editor undo capacity; trimming it frees the oldest records.
    undo_capacity: usize,
    /// Ephemeral source mapping for the last explicit path insertion, not an attachment.
    /// Any content edit degrades it to ordinary text; cursor movement does not.
    file_reference: Option<(usize, std::ops::Range<usize>, String)>,
}

impl std::fmt::Debug for Composer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Composer")
            .field("bytes", &self.byte_len)
            .field("lines", &self.textarea.lines().len())
            .field("revision", &self.editor_revision)
            .field("pastes", &self.pastes.len())
            .finish()
    }
}

impl Default for Composer {
    fn default() -> Self {
        Self::new()
    }
}

impl Composer {
    pub fn new() -> Self {
        let mut textarea = TextArea::default();
        textarea.set_max_histories(MAX_COMPOSER_HISTORIES);
        Self {
            textarea,
            history: VecDeque::new(),
            history_index: None,
            draft: String::new(),
            editor_revision: 0,
            byte_len: 0,
            pastes: Vec::new(),
            paste_undo: Vec::new(),
            paste_redo: Vec::new(),
            undo_capacity: MAX_COMPOSER_HISTORIES,
            file_reference: None,
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
        let removed = raw
            .chars()
            .skip(start)
            .take(end.saturating_sub(start))
            .map(char::len_utf8)
            .sum::<usize>();
        if self
            .byte_len
            .saturating_sub(removed)
            .saturating_add(replacement.len())
            > MAX_COMPOSER_BYTES
        {
            return;
        }
        self.move_to(line, start);
        let offset = self.cursor_char_offset();
        // Deletion and insertion are separate native undo records. Keep the
        // projection snapshot aligned with each, including empty operations.
        self.delete_raw_range(offset, offset + end - start);
        self.type_text(replacement);
    }

    pub fn is_empty(&self) -> bool {
        self.textarea.lines().iter().all(|line| line.is_empty())
    }

    /// The full buffer joined with `\n`, used for submit, export and paste
    /// math. Ordinary editing never calls this (spec §12.1).
    pub fn content(&self) -> String {
        crate::perf::count(crate::perf::Counter::ComposerFullJoins);
        self.textarea.lines().join("\n")
    }

    /// Cached UTF-8 byte length without materializing the buffer.
    pub fn byte_len(&self) -> usize {
        self.byte_len
    }

    /// Bytes this draft retains, including the bounded undo/redo estimate,
    /// paste projections, paste-history snapshots and recalled messages. The
    /// budget must cover these, not only the visible text (spec §12.1, §21).
    /// The estimate is a fixed record count, so this never walks the buffer.
    pub fn retained_bytes(&self) -> usize {
        // Undo + redo + paste-history records all hold buffer-sized or
        // range-sized snapshots; charge a measured fixed count of each.
        let snapshots = self
            .undo_capacity
            .min(UNDO_SNAPSHOT_ESTIMATE)
            .saturating_add(self.paste_undo.len().min(UNDO_SNAPSHOT_ESTIMATE))
            .saturating_add(self.paste_redo.len().min(UNDO_SNAPSHOT_ESTIMATE));
        let undo = self
            .byte_len
            .saturating_mul(snapshots)
            .min(MAX_COMPOSER_BYTES.saturating_mul(3 * UNDO_SNAPSHOT_ESTIMATE));
        let pastes = self
            .pastes
            .iter()
            .map(|range| range.char_count.saturating_mul(4).saturating_add(64))
            .sum::<usize>();
        let recalled = self.history.iter().map(String::len).sum::<usize>() + self.draft.len();
        self.byte_len
            + undo
            + pastes
            + recalled
            + self
                .file_reference
                .as_ref()
                .map_or(0, |(_, _, path)| path.capacity())
    }

    /// Changes the retained undo capacity. The pinned editor resets its
    /// history here, so projection history must be reset too. Un-sent text is
    /// never touched.
    pub fn set_undo_capacity(&mut self, capacity: usize) {
        self.undo_capacity = capacity.clamp(1, MAX_COMPOSER_HISTORIES);
        self.textarea.set_max_histories(self.undo_capacity);
        self.paste_undo.clear();
        self.paste_redo.clear();
    }

    /// The current undo capacity (for budget reports and tests).
    pub fn undo_capacity(&self) -> usize {
        self.undo_capacity
    }

    /// The TextArea for the renderer (read-only widget access).
    pub fn textarea(&self) -> &TextArea<'static> {
        &self.textarea
    }

    // ---- editing (App::update only) -----------------------------------

    pub fn type_char(&mut self, c: char) -> bool {
        let bytes = c.len_utf8();
        if !self.can_insert_bytes(bytes) {
            return false;
        }
        let start = self.edit_offset();
        self.textarea.insert_char(c);
        self.record_edit();
        self.byte_len += bytes;
        self.reconcile_pastes(start, start, 1);
        self.bump_revision();
        true
    }

    pub fn type_text(&mut self, text: &str) -> bool {
        self.insert_text(text, false)
    }

    /// Inserts a paste as one edit. Large payloads receive a display-only
    /// marker while `content()` continues to return the complete original.
    pub fn insert_paste(&mut self, text: &str) -> bool {
        self.insert_text(text, true)
    }

    fn insert_text(&mut self, text: &str, pasted: bool) -> bool {
        if !self.can_insert_bytes(text.len()) {
            return false;
        }
        let start = if pasted {
            self.cursor_char_offset()
        } else {
            self.edit_offset()
        };
        if !self.textarea.insert_str(text) {
            return true;
        }
        self.record_edit();
        // TextArea strips one trailing CR from each inserted logical line.
        // Measure the inserted text, not the full draft, to retain the fast path.
        let stripped = text.split('\n').filter(|line| line.ends_with('\r')).count();
        let char_count = text.chars().count() - stripped;
        self.byte_len += text.len() - stripped;
        self.reconcile_pastes(start, start, char_count);
        let line_count = text.split('\n').count();
        if pasted && (line_count > 10 || char_count > 1_000) {
            self.pastes.push(PasteRange {
                id: 0,
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

    /// Whether `additional` more UTF-8 bytes fit in the draft budget. Uses the
    /// cached length, so it never joins the buffer.
    pub fn can_insert_bytes(&self, additional: usize) -> bool {
        self.byte_len.saturating_add(additional) <= MAX_COMPOSER_BYTES
    }

    /// An edit offset is only needed when a projection needs reconciliation.
    /// Avoid scanning preceding lines in drafts without any hidden pastes.
    fn edit_offset(&self) -> usize {
        if self.pastes.is_empty() {
            0
        } else {
            self.cursor_char_offset()
        }
    }

    /// Global char offset of the cursor, without joining the buffer. Sums the
    /// lines before the cursor (O(chars before the cursor), no allocation).
    fn cursor_char_offset(&self) -> usize {
        let (row, column) = self.textarea.cursor();
        let mut offset = 0usize;
        for line in self.textarea.lines().iter().take(row) {
            offset += line.chars().count() + 1;
        }
        offset + column
    }

    pub fn newline(&mut self) -> bool {
        self.type_char('\n')
    }

    /// Backspace; joins lines at word edges exactly as tui-textarea does.
    pub fn backspace(&mut self) {
        let cursor = self.cursor_char_offset();
        if let Some(paste) = self.pastes.iter().find(|paste| paste.end == cursor) {
            self.delete_raw_range(paste.start, paste.end);
        } else {
            let deleted = self.previous_char_bytes();
            if self.textarea.delete_char() {
                self.record_edit();
                self.byte_len -= deleted;
                self.reconcile_pastes(cursor - 1, cursor, 0);
                self.bump_revision();
            }
        }
    }

    /// Delete (forward).
    pub fn delete(&mut self) {
        let cursor = self.cursor_char_offset();
        if let Some(paste) = self.pastes.iter().find(|paste| paste.start == cursor) {
            self.delete_raw_range(paste.start, paste.end);
        } else {
            let deleted = self.next_char_bytes();
            if self.textarea.delete_next_char() {
                self.record_edit();
                self.byte_len -= deleted;
                self.reconcile_pastes(cursor, cursor + 1, 0);
                self.bump_revision();
            }
        }
    }

    /// Delete a known raw range in one native undo record. All callers start
    /// at or after its beginning. Backward cursor movement does not edit text
    /// and avoids truncating large paste coordinates to TextArea's u16 Jump.
    fn delete_raw_range(&mut self, start: usize, end: usize) {
        if start == end {
            return;
        }
        let cursor = self.cursor_char_offset();
        debug_assert!(start <= cursor && start < end);
        for _ in start..cursor {
            self.textarea.move_cursor(CursorMove::Back);
        }
        if self.textarea.delete_str(end - start) {
            self.record_edit();
            self.byte_len = self.textarea.lines().iter().map(String::len).sum::<usize>()
                + self.textarea.lines().len()
                - 1;
            self.reconcile_pastes(start, end, 0);
            self.bump_revision();
        }
    }

    fn previous_char_bytes(&self) -> usize {
        let (row, column) = self.textarea.cursor();
        if column > 0 {
            return self.textarea.lines()[row]
                .chars()
                .nth(column - 1)
                .map_or(0, char::len_utf8);
        }
        usize::from(row > 0)
    }

    fn next_char_bytes(&self) -> usize {
        let (row, column) = self.textarea.cursor();
        let line = &self.textarea.lines()[row];
        if let Some(character) = line.chars().nth(column) {
            return character.len_utf8();
        }
        usize::from(row + 1 < self.textarea.lines().len())
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

    /// Delete to a logical editor line boundary, not a soft-wrapped screen
    /// row. A collapsed paste is one atomic segment, as with word deletion.
    /// At the boundary this is a no-op: never consume the adjacent newline.
    pub fn delete_to_line_boundary(&mut self, forward: bool) {
        let raw_cursor = self.cursor_char_offset();
        let display = self.display_content();
        let cursor = projected_cursor(raw_cursor, &self.pastes);
        let (row, column) = line_col_at(&display, cursor);
        let target = if forward {
            cursor + display.split('\n').nth(row).unwrap_or("").chars().count() - column
        } else {
            cursor - column
        };
        if target != cursor {
            let start = raw_cursor_for_display(cursor.min(target), &self.pastes);
            let end = raw_cursor_for_display(cursor.max(target), &self.pastes);
            self.delete_raw_range(start, end);
        }
    }

    pub fn word_delete(&mut self) {
        let raw_cursor = self.cursor_char_offset();
        let display = self.display_content();
        let display_cursor = projected_cursor(raw_cursor, &self.pastes);
        let markers = self.display_paste_markers();
        let target = word_backward_cursor(&display, display_cursor, &markers);
        if target != display_cursor {
            let raw_start = raw_cursor_for_display(target, &self.pastes);
            let raw_end = raw_cursor_for_display(display_cursor, &self.pastes);
            self.delete_raw_range(raw_start, raw_end);
        }
    }

    pub fn undo(&mut self) {
        if self.textarea.undo() {
            // Only advance projection history when the native edit succeeded.
            if let Some(previous) = self.paste_undo.pop() {
                self.paste_redo
                    .push(std::mem::replace(&mut self.pastes, previous));
            }
            self.byte_len = self.content().len();
            self.bump_revision();
        }
    }

    pub fn redo(&mut self) {
        if self.textarea.redo() {
            if let Some(next) = self.paste_redo.pop() {
                self.paste_undo
                    .push(std::mem::replace(&mut self.pastes, next));
            }
            self.byte_len = self.content().len();
            self.bump_revision();
        }
    }

    /// Empties the buffer and drops any history navigation state.
    pub fn clear(&mut self) {
        self.set_text("");
        self.history_index = None;
    }

    /// Replaces the whole buffer (send-failure recovery, /clear, history)
    /// with the cursor at the end.
    pub fn set_text(&mut self, text: &str) {
        let mut normalized = text
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .replace('\t', "    ");
        if normalized.len() > MAX_COMPOSER_BYTES {
            let mut end = MAX_COMPOSER_BYTES;
            while end > 0 && !normalized.is_char_boundary(end) {
                end -= 1;
            }
            normalized.truncate(end);
        }
        let lines = normalized
            .split('\n')
            .map(str::to_owned)
            .collect::<Vec<_>>();
        self.textarea = TextArea::new(lines);
        self.textarea.set_max_histories(self.undo_capacity);
        self.textarea.move_cursor(CursorMove::Bottom);
        self.textarea.move_cursor(CursorMove::End);
        self.pastes.clear();
        self.paste_undo.clear();
        self.paste_redo.clear();
        self.byte_len = normalized.len();
        self.bump_revision();
    }

    pub fn editor_revision(&self) -> u64 {
        self.editor_revision
    }

    pub fn remember_file_reference(
        &mut self,
        line: usize,
        start: usize,
        chars: usize,
        path: String,
    ) {
        self.file_reference = Some((line, start..start + chars, path));
    }

    pub fn file_reference_at_cursor(&self) -> Option<&str> {
        let (line, range, path) = self.file_reference.as_ref()?;
        let (row, col) = self.cursor();
        (*line == row && col >= range.start && col <= range.end).then_some(path.as_str())
    }

    fn bump_revision(&mut self) {
        self.editor_revision = self.editor_revision.wrapping_add(1);
        self.file_reference = None;
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
            while self.history.iter().map(String::len).sum::<usize>()
                > crate::limits::COMPOSER_ALL_DRAFTS_BYTES
            {
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

    /// Record the projection before reconciling a successful native edit.
    /// Mirror the pinned TextArea history's bounded queue, including eviction
    /// before a branch edit when undo + redo already fill its capacity.
    fn record_edit(&mut self) {
        if self.paste_undo.len() + self.paste_redo.len() == self.undo_capacity
            && !self.paste_undo.is_empty()
        {
            self.paste_undo.remove(0);
        }
        self.paste_undo.push(self.pastes.clone());
        self.paste_redo.clear();
    }

    /// Apply a known scalar edit range. Diffing equal text is ambiguous (for
    /// example deleting an x just before an all-x paste), so use the actual
    /// edit coordinates even when the surrounding payload repeats.
    fn reconcile_pastes(&mut self, start: usize, end: usize, inserted: usize) {
        let delta = inserted as isize - (end - start) as isize;
        self.pastes.retain_mut(|paste| {
            if paste.end <= start {
                true
            } else if paste.start >= end {
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
            cursor = cursor
                .saturating_sub(paste.end - paste.start)
                .saturating_add(marker_len);
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

    /// Spec §25.1: after a 256 KiB paste, ordinary typing must not join the
    /// whole buffer. Only the paste itself materializes text.
    #[test]
    fn ordinary_typing_after_a_large_paste_does_not_join_the_buffer() {
        let mut composer = Composer::new();
        // Just under the draft cap so the ordinary typing below fits.
        let payload = "x".repeat(256 * 1024 - 1024);
        assert!(composer.insert_paste(&payload));
        assert_eq!(composer.paste_ranges().len(), 1);
        let before = crate::perf::snapshot().composer_full_joins;
        for _ in 0..40 {
            assert!(composer.type_char('a'));
        }
        let after = crate::perf::snapshot().composer_full_joins;
        assert_eq!(
            after, before,
            "ordinary typing must not materialize the whole draft"
        );
        assert_eq!(
            composer.byte_len(),
            payload.len() + 40,
            "the cached byte length tracks the delta"
        );
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
    fn line_deletion_preserves_newlines_unicode_and_boundary_noops() {
        for (forward, expected) in [
            (false, "first\n尾巴\nlast"),
            (true, "first\n中e\u{301}🙂\nlast"),
        ] {
            let mut composer = Composer::new();
            composer.set_text("first\n中e\u{301}🙂尾巴\nlast");
            composer.move_to(1, 4);
            composer.delete_to_line_boundary(forward);
            assert_eq!(composer.content(), expected);
            assert_eq!(composer.byte_len(), expected.len());
            let revision = composer.editor_revision();
            composer.delete_to_line_boundary(forward);
            assert_eq!(composer.content(), expected);
            assert_eq!(
                composer.editor_revision(),
                revision,
                "boundary must not add undo history"
            );
            composer.undo();
            assert_eq!(composer.content(), "first\n中e\u{301}🙂尾巴\nlast");
            composer.redo();
            assert_eq!(composer.content(), expected);
        }
        for text in ["", "\n", "one\n\nthree\n"] {
            let mut composer = Composer::new();
            composer.set_text(text);
            let revision = composer.editor_revision();
            composer.delete_to_line_boundary(false);
            composer.delete_to_line_boundary(true);
            assert_eq!(composer.content(), text);
            assert_eq!(composer.editor_revision(), revision);
        }
    }

    #[test]
    fn line_deletion_treats_collapsed_paste_atomically_and_restores_history() {
        for payload in ["x".repeat(1_001), "\n".repeat(10), "你🙂\n".repeat(11)] {
            for forward in [false, true] {
                let mut composer = Composer::new();
                composer.type_text("before\nPRE");
                composer.insert_paste(&payload);
                composer.type_text("TAIL\nafter");
                let original = composer.content();
                let ranges = composer.paste_ranges().to_vec();
                if forward {
                    composer.move_to_display(1, 0);
                } else {
                    let end = composer
                        .display_content()
                        .split('\n')
                        .nth(1)
                        .unwrap()
                        .chars()
                        .count();
                    composer.move_to_display(1, end);
                }
                composer.delete_to_line_boundary(forward);
                assert_eq!(composer.content(), "before\n\nafter");
                assert!(composer.paste_ranges().is_empty());
                for _ in 0..2 {
                    composer.undo();
                    assert_eq!(composer.content(), original);
                    assert_eq!(composer.paste_ranges(), ranges);
                    assert_eq!(composer.byte_len(), original.len());
                    composer.redo();
                    assert_eq!(composer.content(), "before\n\nafter");
                    assert!(composer.paste_ranges().is_empty());
                }
            }
        }
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
    fn short_multiline_pastes_can_project_to_longer_markers() {
        let mut composer = Composer::new();
        composer.type_text("PRE");
        composer.insert_paste(&"\n".repeat(10));
        composer.type_text("TAIL");
        assert_eq!(composer.display_content(), "PRE[paste #1 +11 lines]TAIL");
        assert_eq!(composer.display_cursor(), (0, 27));
        assert_eq!(composer.display_paste_markers(), vec![3..23]);
        composer.move_to_display(0, 3);
        composer.move_right();
        assert_eq!(composer.cursor(), (10, 0));
        assert_eq!(composer.display_cursor(), (0, 23));
        composer.move_left();
        assert_eq!(composer.cursor(), (0, 3));

        composer.move_right();
        composer.insert_paste(&"x".repeat(1_001));
        assert_eq!(composer.display_cursor(), (0, 44));
        assert_eq!(composer.display_paste_markers(), vec![3..23, 23..44]);
        composer.move_to_display(0, 44);
        assert_eq!(composer.cursor(), (10, 1_001));
    }

    #[test]
    fn paste_deletion_is_one_undo_record_for_all_delete_keys() {
        for payload in ["x".repeat(1_001), "\n".repeat(10), "你🙂\n".repeat(11)] {
            for key in 0..3 {
                let mut composer = Composer::new();
                composer.type_text("PRE");
                composer.insert_paste(&payload);
                composer.type_text("TAIL");
                composer.move_to_display(0, 3);
                if key != 1 {
                    composer.move_right();
                }
                let original = composer.content();
                let ranges = composer.paste_ranges().to_vec();
                match key {
                    0 => composer.backspace(),
                    1 => composer.delete(),
                    _ => composer.word_delete(),
                }
                assert_eq!(composer.content(), "PRETAIL", "delete key {key}");
                assert!(composer.paste_ranges().is_empty());
                assert_eq!(composer.byte_len(), 7);
                for _ in 0..2 {
                    composer.undo();
                    assert_eq!(composer.content(), original, "undo key {key}");
                    assert_eq!(composer.paste_ranges(), ranges);
                    assert_eq!(composer.byte_len(), original.len());
                    composer.redo();
                    assert_eq!(composer.content(), "PRETAIL");
                    assert!(composer.paste_ranges().is_empty());
                }
            }
        }
    }

    #[test]
    fn deleting_a_paste_after_a_large_prefix_does_not_truncate_coordinates() {
        let mut composer = Composer::new();
        let prefix = "p".repeat(70_000);
        let payload = "x".repeat(1_001);
        composer.type_text(&prefix);
        composer.insert_paste(&payload);
        composer.backspace();
        assert_eq!(composer.content(), prefix);
        composer.undo();
        assert_eq!(composer.content(), format!("{prefix}{payload}"));
        assert_eq!(composer.paste_ranges()[0].start, 70_000);
    }

    #[test]
    fn deleting_before_a_paste_preserves_the_suffix_and_undo_alignment() {
        for backward in [false, true] {
            let mut composer = Composer::new();
            let payload = "x".repeat(1_001);
            composer.type_text("PRE");
            composer.insert_paste(&payload);
            composer.type_text("TAIL");
            composer.move_to(0, usize::from(backward));
            if backward {
                composer.backspace();
            } else {
                composer.delete();
            }
            assert_eq!(composer.display_content(), "RE[paste #1 1001 chars]TAIL");
            assert_eq!(composer.paste_ranges()[0].start, 2);
            composer.move_right();
            composer.move_right();
            composer.delete();
            assert_eq!(composer.content(), "RETAIL");
            composer.undo();
            assert_eq!(composer.content(), format!("RE{payload}TAIL"));
            assert_eq!(composer.display_content(), "RE[paste #1 1001 chars]TAIL");
            composer.undo();
            assert_eq!(composer.content(), format!("PRE{payload}TAIL"));
            assert_eq!(composer.display_content(), "PRE[paste #1 1001 chars]TAIL");
            composer.redo();
            composer.redo();
            assert_eq!(composer.content(), "RETAIL");
        }
    }

    #[test]
    fn repeated_text_edits_use_actual_coordinates_not_a_text_diff() {
        for backward in [false, true] {
            let mut composer = Composer::new();
            composer.type_text("x");
            composer.insert_paste(&"x".repeat(1_001));
            composer.type_text("x");
            composer.move_to(0, usize::from(backward));
            if backward {
                composer.backspace();
            } else {
                composer.delete();
            }
            assert_eq!(composer.display_content(), "[paste #1 1001 chars]x");
            composer.type_char('x');
            assert_eq!(composer.display_content(), "x[paste #1 1001 chars]x");
            composer.move_to(0, 501);
            if backward {
                composer.backspace();
            } else {
                composer.delete();
            }
            assert!(composer.paste_ranges().is_empty());
            assert_eq!(composer.content(), "x".repeat(1_002));
            composer.undo();
            assert_eq!(composer.display_content(), "x[paste #1 1001 chars]x");
            composer.redo();
            assert!(composer.paste_ranges().is_empty());
        }
    }

    #[test]
    fn newline_and_unicode_edits_shift_multiple_pastes_exactly() {
        let mut composer = Composer::new();
        let payload = "界".repeat(1_001);
        composer.type_text("你\n");
        composer.insert_paste(&payload);
        composer.insert_paste(&"\n".repeat(10));
        composer.type_text("尾");
        composer.move_to(1, 0);
        composer.backspace();
        assert_eq!(composer.paste_ranges()[0].start, 1);
        assert_eq!(composer.paste_ranges()[1].start, 1_002);
        assert_eq!(
            composer.display_content(),
            "你[paste #1 1001 chars][paste #2 +11 lines]尾"
        );
        assert_eq!(composer.byte_len(), composer.content().len());
        composer.move_to(0, 0);
        composer.delete();
        assert_eq!(composer.paste_ranges()[0].start, 0);
        assert_eq!(composer.byte_len(), composer.content().len());
        composer.delete();
        assert_eq!(composer.display_content(), "[paste #1 +11 lines]尾");
        composer.undo();
        assert_eq!(
            composer.display_content(),
            "[paste #1 1001 chars][paste #2 +11 lines]尾"
        );
    }

    #[test]
    fn no_op_edits_and_exhausted_history_do_not_advance_paste_history() {
        let mut composer = Composer::new();
        let payload = "x".repeat(1_001);
        composer.insert_paste(&payload);
        let revision = composer.editor_revision();
        composer.delete();
        composer.type_text("");
        composer.insert_paste("");
        composer.redo();
        assert_eq!(composer.editor_revision(), revision);
        composer.undo();
        assert_eq!(composer.content(), "");
        assert!(composer.paste_ranges().is_empty());
        let revision = composer.editor_revision();
        composer.backspace();
        composer.delete();
        composer.word_delete();
        composer.replace_range(0, 0, 0, "");
        composer.type_text("\r");
        composer.undo();
        assert_eq!(composer.editor_revision(), revision);
        composer.redo();
        assert_eq!(composer.content(), payload);
        assert_eq!(composer.display_content(), "[paste #1 1001 chars]");
        composer.redo();
        composer.undo();
        assert!(composer.paste_ranges().is_empty());
    }

    #[test]
    fn paste_history_matches_native_capacity_reset_and_branch_eviction() {
        let mut composer = Composer::new();
        composer.set_undo_capacity(2);
        let payload = "x".repeat(1_001);
        composer.insert_paste(&payload);
        composer.type_char('a');
        composer.undo();
        composer.type_char('b');
        composer.undo();
        composer.undo(); // The original paste insertion was evicted.
        assert_eq!(composer.content(), payload);
        assert_eq!(composer.display_content(), "[paste #1 1001 chars]");
        composer.redo();
        assert_eq!(composer.display_content(), "[paste #1 1001 chars]b");
        composer.set_undo_capacity(1);
        composer.undo();
        assert_eq!(composer.display_content(), "[paste #1 1001 chars]b");
        assert!(composer.paste_undo.is_empty());
        assert!(composer.paste_redo.is_empty());
        composer.set_text("");
        assert_eq!(composer.textarea.max_histories(), 1);
        composer.insert_paste(&payload);
        for _ in 0..(MAX_COMPOSER_HISTORIES + 5) {
            composer.type_char('a');
        }
        assert_eq!(composer.paste_undo.len(), 1);
    }

    #[test]
    fn range_replacement_keeps_a_snapshot_for_each_native_edit() {
        let mut composer = Composer::new();
        composer.type_text("PRE");
        composer.insert_paste(&"x".repeat(1_001));
        composer.replace_range(0, 0, 3, "NEW");
        assert_eq!(composer.display_content(), "NEW[paste #1 1001 chars]");
        composer.undo();
        assert_eq!(composer.display_content(), "[paste #1 1001 chars]");
        composer.undo();
        assert_eq!(composer.display_content(), "PRE[paste #1 1001 chars]");
        composer.redo();
        composer.redo();
        assert_eq!(composer.display_content(), "NEW[paste #1 1001 chars]");
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
