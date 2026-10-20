//! Key and mouse dispatch: the single place that turns terminal input into
//! [`Action`]s or direct edits of the composer and the views.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use super::overlay::{matching_models, matching_paths};
use super::{Action, Answer, App, Overlay, Panel, Scroll, find};
use crate::app::commands::{self, Executed};
use crate::protocol::Reasoning;
use crate::transcript::{Block, NoticeLevel};
use crate::ui::{blocks, picker, selection, sheets};

/// A second Ctrl+C within this long of the first quits.
const QUIT_WINDOW: std::time::Duration = std::time::Duration::from_millis(500);

impl App {
    /// Routes a key event to the focused surface; the action it asks for, if any.
    pub fn on_key(&mut self, key: KeyEvent) -> Option<Action> {
        self.notices.clear();
        if self.fatal.is_some() {
            return matches!(key.code, KeyCode::Char('q') | KeyCode::Esc).then_some(Action::Quit);
        }
        if !self.overlay.is_none() {
            return self.on_overlay_key(key);
        }
        if self.has_interaction() {
            return self.on_interaction_key(key);
        }
        self.on_composer_input(key)
    }

    /// A composer key, then the completion list brought in step with the draft.
    fn on_composer_input(&mut self, key: KeyEvent) -> Option<Action> {
        let before = self.composer.text().to_owned();
        let action = self.on_composer_key(key);
        let edited = self.composer.text() != before;
        let typed = edited
            && matches!(
                key.code,
                KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Delete
            );
        self.sync_completion(edited, typed);
        action
    }

    /// Keeps the inline completion list in step with the draft: an edit resets
    /// the selection to the best match and a draft it no longer fits closes it.
    /// Typing in the `/name` of a one-line draft opens the command list.
    fn sync_completion(&mut self, edited: bool, typed: bool) {
        let fits = match self.overlay {
            Overlay::Palette { .. } => self.palette_query().is_some(),
            Overlay::Files { .. } => self.mention_query().is_some(),
            Overlay::None => {
                if typed && self.palette_query().is_some() && self.word_start() == 0 {
                    self.overlay = Overlay::Palette { selected: 0 };
                }
                return;
            }
            _ => return,
        };
        match &mut self.overlay {
            _ if !fits => self.overlay = Overlay::None,
            Overlay::Palette { selected } | Overlay::Files { selected, .. } if edited => {
                *selected = 0;
            }
            _ => {}
        }
    }

    /// Rows the completion list offers for the current draft.
    fn completion_len(&self) -> usize {
        match &self.overlay {
            Overlay::Palette { .. } => self
                .palette_query()
                .map_or(0, |query| commands::filter(query).len()),
            Overlay::Files { paths, .. } => self
                .mention_query()
                .map_or(0, |query| matching_paths(paths, query).len()),
            _ => 0,
        }
    }

    /// Keys while the completion list is open. It takes the arrows, Tab, Enter
    /// and Esc while it has rows; every other key edits the draft that filters it.
    fn on_completion_key(&mut self, key: KeyEvent) -> Option<Action> {
        let rows = self.completion_len();
        let plain = key.modifiers.is_empty();
        match key.code {
            // The draft stays: it may be a path rather than a command.
            KeyCode::Esc => self.overlay = Overlay::None,
            KeyCode::Up if plain && rows > 0 => self.overlay_move(-1),
            KeyCode::Down if plain && rows > 0 => self.overlay_move(1),
            KeyCode::Tab if plain && rows > 0 => self.apply_completion(),
            KeyCode::Enter if plain && rows > 0 => return self.accept_completion(),
            // An `@` with no match never sends the draft by accident.
            KeyCode::Enter if plain && matches!(self.overlay, Overlay::Files { .. }) => {}
            // No command matches: the line is a prompt.
            KeyCode::Enter if plain => {
                self.overlay = Overlay::None;
                return self.on_composer_input(key);
            }
            _ => return self.on_composer_input(key),
        }
        None
    }

    /// The selected command or path, if the list has one.
    fn selected_completion(&self) -> Option<String> {
        match &self.overlay {
            Overlay::Palette { selected } => commands::filter(self.palette_query()?)
                .get(*selected)
                .map(|command| command.name.to_owned()),
            Overlay::Files { selected, paths } => matching_paths(paths, self.mention_query()?)
                .get(*selected)
                .map(|path| (*path).clone()),
            _ => None,
        }
    }

    /// Tab: puts the selected completion into the draft and closes the list.
    /// A command keeps the argument typed after it; a path ends the `@` token.
    fn apply_completion(&mut self) {
        let Some(choice) = self.selected_completion() else {
            return;
        };
        match std::mem::replace(&mut self.overlay, Overlay::None) {
            Overlay::Palette { .. } => {
                let text = self.composer.text();
                let rest = &text[text.find(char::is_whitespace).unwrap_or(text.len())..];
                let rest = if rest.trim().is_empty() { " " } else { rest };
                self.composer.restore_text(&format!("/{choice}{rest}"));
            }
            _ => {
                let start = self.word_start();
                while self.composer.cursor() > start {
                    self.composer.backspace();
                }
                self.composer.insert(&format!("@{choice} "));
            }
        }
    }

    /// Enter: a command runs with the argument typed after its name, which
    /// may belong to another fuzzy match than the typed name; a path is inserted.
    fn accept_completion(&mut self) -> Option<Action> {
        if !matches!(self.overlay, Overlay::Palette { .. }) {
            self.apply_completion();
            return None;
        }
        let name = self.selected_completion()?;
        let arg = commands::parse(self.composer.text()).map_or("", |(_, arg)| arg);
        match commands::execute(&name, arg) {
            Some(Executed::Action(action)) => {
                self.overlay = Overlay::None;
                self.composer.clear();
                Some(action)
            }
            Some(Executed::Panel(panel)) => {
                self.composer.clear();
                self.open_panel(panel);
                None
            }
            // The completed name stays in the draft for the argument it needs.
            _ => {
                self.apply_completion();
                self.notice(NoticeLevel::Error, commands::needs_argument(&name));
                None
            }
        }
    }

    /// Approval prompts own the keyboard until answered; other keys are
    /// swallowed rather than reaching the draft behind the prompt.
    fn on_interaction_key(&mut self, key: KeyEvent) -> Option<Action> {
        let interaction = self.session.as_mut()?.interaction.as_mut()?;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let answer = match key.code {
            KeyCode::Char('y') if !ctrl => Answer::Allow,
            KeyCode::Char('n') if !ctrl => Answer::Deny,
            KeyCode::Esc => Answer::Deny,
            KeyCode::Enter => Answer::Choice(interaction.selected),
            KeyCode::Up => {
                interaction.selected = interaction.selected.saturating_sub(1);
                return None;
            }
            KeyCode::Down => {
                let last = interaction.options().len() - 1;
                interaction.selected = (interaction.selected + 1).min(last);
                return None;
            }
            _ => return None,
        };
        Some(Action::Answer(answer))
    }

    fn on_overlay_key(&mut self, key: KeyEvent) -> Option<Action> {
        if matches!(
            self.overlay,
            Overlay::Palette { .. } | Overlay::Files { .. }
        ) {
            return self.on_completion_key(key);
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let tool = matches!(self.overlay, Overlay::Tool(_));
        let scrolling = tool || matches!(self.overlay, Overlay::Diff(_));
        let page = self.viewport_rows().saturating_sub(2) as isize;
        match key.code {
            KeyCode::Esc => self.overlay = Overlay::None,
            KeyCode::Char('[') | KeyCode::Left if tool => self.step_tool(-1),
            KeyCode::Char(']') | KeyCode::Right if tool => self.step_tool(1),
            KeyCode::PageUp if scrolling => self.scroll_view(-page),
            KeyCode::PageDown if scrolling => self.scroll_view(page),
            KeyCode::Home if scrolling => self.scroll_view(isize::MIN),
            KeyCode::End if scrolling => self.scroll_view(isize::MAX),
            KeyCode::Char('c') if scrolling || matches!(self.overlay, Overlay::Context) => {
                self.copy_view();
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => self.find_step(false),
            KeyCode::Enter => return self.on_overlay_accept(),
            KeyCode::Up => self.overlay_move(-1),
            KeyCode::Tab if matches!(self.overlay, Overlay::Diff(_)) => self.diff_cycle_scope(),
            KeyCode::Tab | KeyCode::Down => self.overlay_move(1),
            KeyCode::PageUp => self.overlay_move(-10),
            KeyCode::PageDown => self.overlay_move(10),
            KeyCode::Backspace => self.overlay_filter_push(None),
            KeyCode::Char('d') if ctrl => self.delete_confirm(),
            KeyCode::Char('r') if matches!(self.overlay, Overlay::Context) => {
                self.refresh_context();
            }
            KeyCode::Char(character) if !ctrl => self.overlay_filter_push(Some(character)),
            _ => {}
        }
        None
    }

    /// Ctrl+D in the sessions picker: the first press arms the delete, the
    /// second deletes the selected session.
    fn delete_confirm(&mut self) {
        let Overlay::Sessions {
            rows,
            filter,
            selected,
            armed,
        } = &mut self.overlay
        else {
            return;
        };
        let resident = self.session.as_ref();
        let Some(info) = sheets::filtered_sessions(rows, filter, resident)
            .get(*selected)
            .copied()
        else {
            return;
        };
        let id = info.session_id.clone();
        if resident.is_some_and(|session| session.info.session_id == id) {
            self.notice(
                NoticeLevel::Error,
                "Open another session before deleting this one",
            );
        } else if armed.as_deref() == Some(id.as_str()) {
            *armed = None;
            self.on_action(Action::DeleteSession(id));
        } else {
            let label = sheets::session_label(info, resident);
            *armed = Some(id);
            self.notice(
                NoticeLevel::Info,
                format!("Press Ctrl+D again to delete \"{label}\" permanently"),
            );
        }
    }

    /// Enter / Shift+Enter in the find bar: jump to the next or previous match.
    fn find_step(&mut self, forward: bool) {
        let Overlay::Find {
            matches, current, ..
        } = &mut self.overlay
        else {
            return;
        };
        *current = find::step_match(*current, matches.len(), forward);
        self.show_match();
    }

    /// Puts the find bar's current match at the top of the view.
    fn show_match(&mut self) {
        if let Overlay::Find {
            matches, current, ..
        } = &self.overlay
            && let Some(found) = matches.get(*current)
        {
            self.scroll = Scroll::Anchor {
                block: found.block,
                offset: 0,
            };
        }
    }

    /// Moves the selected row of the active overlay by `delta`.
    pub fn overlay_move(&mut self, delta: isize) {
        let len = match &self.overlay {
            Overlay::Palette { .. } | Overlay::Files { .. } => self.completion_len(),
            Overlay::Sessions { rows, filter, .. } => {
                sheets::filtered_sessions(rows, filter, self.session.as_ref()).len()
            }
            Overlay::Models { filter, .. } => matching_models(&self.models, filter).len(),
            Overlay::Reasoning { .. } => self.reasoning_levels().len(),
            Overlay::Diff(diff) => diff.files.len(),
            _ => 0,
        };
        match &mut self.overlay {
            Overlay::Palette { selected }
            | Overlay::Sessions { selected, .. }
            | Overlay::Models { selected, .. }
            | Overlay::Reasoning { selected }
            | Overlay::Files { selected, .. } => *selected = picker::step(*selected, delta, len),
            Overlay::Diff(diff) => {
                let next = (diff.selected as isize + delta).clamp(0, len.saturating_sub(1) as isize)
                    as usize;
                if next != diff.selected {
                    diff.select(next);
                    self.load_selected_diff();
                }
            }
            Overlay::Help { scroll } => *scroll = scroll.saturating_add_signed(delta * 3),
            Overlay::Tool(_) => self.scroll_view(delta),
            Overlay::Find { .. } => self.find_step(delta > 0),
            Overlay::Context | Overlay::None => {}
        }
    }

    /// Scrolls the diff or tool view by `delta` rows; they clamp at the ends.
    fn scroll_view(&mut self, delta: isize) {
        // The view's title and hint rows are not content.
        let rows = self.viewport_rows().saturating_sub(2);
        match &mut self.overlay {
            Overlay::Diff(diff) => diff.scroll_by(delta, rows),
            Overlay::Tool(tool) => tool.scroll_by(delta, rows),
            _ => {}
        }
    }

    /// Types `character` into the overlay's filter, or deletes the last one.
    fn overlay_filter_push(&mut self, character: Option<char>) {
        let edit = |text: &mut String| match character {
            Some(character) => text.push(character),
            None => {
                text.pop();
            }
        };
        let blocks = self
            .session
            .as_ref()
            .map_or(&[][..], |session| session.transcript.blocks());
        match &mut self.overlay {
            Overlay::Sessions {
                filter, selected, ..
            }
            | Overlay::Models { filter, selected } => {
                edit(filter);
                *selected = 0;
            }
            Overlay::Find {
                query,
                matches,
                current,
            } => {
                edit(query);
                *matches = find::find_matches(blocks, query);
                *current = 0;
            }
            _ => {}
        }
        self.show_match();
    }

    fn on_overlay_accept(&mut self) -> Option<Action> {
        match std::mem::replace(&mut self.overlay, Overlay::None) {
            Overlay::Sessions {
                selected,
                filter,
                rows,
                ..
            } => sheets::filtered_sessions(&rows, &filter, self.session.as_ref())
                .get(selected)
                .map(|info| Action::OpenSession(info.session_id.clone())),
            find @ Overlay::Find { .. } => {
                self.overlay = find;
                self.find_step(true);
                None
            }
            Overlay::Models { filter, selected } => matching_models(&self.models, &filter)
                .get(selected)
                .map(|model| Action::SetModel(model.id.clone())),
            Overlay::Reasoning { selected } => self
                .reasoning_levels()
                .get(selected)
                .map(|level| Action::SetReasoning(*level)),
            Overlay::Palette { .. }
            | Overlay::Files { .. }
            | Overlay::Context
            | Overlay::Help { .. }
            | Overlay::Diff(_)
            | Overlay::Tool(_)
            | Overlay::None => None,
        }
    }

    fn on_composer_key(&mut self, key: KeyEvent) -> Option<Action> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        if !matches!(key.code, KeyCode::Char('c') if ctrl) {
            self.quit_armed = None;
        }
        match key.code {
            KeyCode::Enter if alt => return Some(Action::QueueFollowUp),
            KeyCode::Enter if shift => self.composer.newline(),
            KeyCode::Enter => return self.submit_or_command(),
            KeyCode::Char('j') if ctrl => self.composer.newline(),
            KeyCode::Esc => self.escape(),
            KeyCode::Char('c') if ctrl => {
                let now = tokio::time::Instant::now();
                if self.quit_armed.is_some_and(|at| now - at <= QUIT_WINDOW) {
                    return Some(Action::Quit);
                }
                self.quit_armed = Some(now);
                if self.composer.is_empty() {
                    self.notice(NoticeLevel::Info, "Press Ctrl+C again to quit");
                } else {
                    self.composer.clear();
                }
            }
            KeyCode::Char('d') if ctrl => {
                if self.composer.is_empty() {
                    return Some(Action::Quit);
                }
                self.composer.delete();
            }
            KeyCode::Char('g') if ctrl => return Some(Action::EditDraft),
            KeyCode::Char('o') if ctrl => self.tools_expanded = !self.tools_expanded,
            KeyCode::Char('t') if ctrl => self.show_reasoning = !self.show_reasoning,
            KeyCode::Char('r') if ctrl => self.open_panel(Panel::Sessions),
            KeyCode::Char('l') if ctrl => self.open_panel(Panel::Models),
            KeyCode::Char('f') if ctrl => self.open_panel(Panel::Find),
            KeyCode::Char('a') if ctrl => self.composer.move_home(),
            KeyCode::Char('e') if ctrl => self.composer.move_end(),
            KeyCode::Char('u') if ctrl => self.composer.delete_to_line_start(),
            KeyCode::Char('k') if ctrl => self.composer.delete_to_line_end(),
            // Many terminals send Ctrl+Backspace as ^H.
            KeyCode::Char('w' | 'h') if ctrl => self.composer.delete_word_left(),
            KeyCode::Char('z') if ctrl => {
                self.composer.undo();
            }
            KeyCode::Char('y') if ctrl => {
                self.composer.redo();
            }
            KeyCode::BackTab => return self.next_reasoning().map(Action::SetReasoning),
            // Mid-word `@` is text, as in an address.
            KeyCode::Char('@') if self.word_start() == self.composer.cursor() => {
                self.composer.type_char('@');
                self.open_panel(Panel::Files);
            }
            KeyCode::Char(_) if ctrl => {}
            KeyCode::Char(character) => self.composer.type_char(character),
            KeyCode::Backspace if ctrl || alt => self.composer.delete_word_left(),
            KeyCode::Backspace => self.composer.backspace(),
            KeyCode::Delete => self.composer.delete(),
            KeyCode::Left if ctrl || alt => self.composer.move_word_left(),
            KeyCode::Right if ctrl || alt => self.composer.move_word_right(),
            KeyCode::Left => self.composer.move_left(),
            KeyCode::Right => self.composer.move_right(),
            // Arrows move inside a multi-line draft and only browse history at
            // its first and last line.
            KeyCode::Up if alt => return Some(Action::RecallQueued),
            KeyCode::Up if ctrl => self.jump_to_user_message(false),
            KeyCode::Down if ctrl => self.jump_to_user_message(true),
            KeyCode::Up => {
                if !self.composer.move_up() {
                    self.composer.history_prev();
                }
            }
            KeyCode::Down => {
                if !self.composer.move_down() {
                    self.composer.history_next();
                }
            }
            KeyCode::Home if ctrl => {
                self.scroll = Scroll::Anchor {
                    block: 0,
                    offset: 0,
                };
                // The top of the loaded window is also where the next page begins.
                self.load_older();
            }
            KeyCode::End if ctrl => self.scroll = Scroll::Follow,
            KeyCode::Home => self.composer.move_home(),
            KeyCode::End => self.composer.move_end(),
            // Two rows of the previous page stay in view.
            KeyCode::PageUp => self.scroll_by(-self.page_rows()),
            KeyCode::PageDown => self.scroll_by(self.page_rows()),
            KeyCode::F(1) => self.open_panel(Panel::Help),
            _ => {}
        }
        None
    }

    /// A terminal paste. The composer decides whether the payload becomes a
    /// single token; a pasted `/name` opens the command list like typing does.
    pub fn on_paste(&mut self, text: &str) {
        if self.has_interaction() {
            return;
        }
        self.composer.insert(text);
        self.sync_completion(true, true);
    }

    /// A draft is a command only when its first word names one; anything else
    /// starting with `/` (a path, say) is a prompt.
    fn submit_or_command(&mut self) -> Option<Action> {
        let executed = commands::parse(self.composer.text())
            .and_then(|(name, argument)| commands::execute(name, argument));
        match executed {
            Some(Executed::Action(action)) => {
                self.composer.clear();
                Some(action)
            }
            Some(Executed::Panel(panel)) => {
                self.composer.clear();
                self.open_panel(panel);
                None
            }
            Some(Executed::Noop) => {
                let name = commands::parse(self.composer.text()).map_or("", |(name, _)| name);
                self.notice(NoticeLevel::Error, commands::needs_argument(name));
                None
            }
            None => Some(Action::Submit),
        }
    }

    fn page_rows(&self) -> isize {
        self.viewport_rows().saturating_sub(2).max(1) as isize
    }

    /// Ctrl+↑/↓: puts the previous or next user message at the top. Past the
    /// last one the view follows the tail again.
    fn jump_to_user_message(&mut self, forward: bool) {
        let blocks = self.transcript_blocks();
        let layout = &self.layout;
        let (top, offset) = match self.scroll {
            Scroll::Anchor { block, offset } => (block, offset),
            Scroll::Follow if forward => return,
            Scroll::Follow => {
                let max_top = layout.total_rows().saturating_sub(layout.viewport.height);
                layout.anchor(max_top)
            }
        };
        let is_user = |block: &Block| matches!(block, Block::User { .. });
        let target = if forward {
            blocks
                .iter()
                .enumerate()
                .skip(top + 1)
                .find(|(_, block)| is_user(block))
                .map(|(index, _)| index)
        } else {
            let end = if offset > 0 { top + 1 } else { top };
            blocks[..end.min(blocks.len())].iter().rposition(is_user)
        };
        self.scroll = match target {
            Some(block) => Scroll::Anchor { block, offset: 0 },
            None if forward => Scroll::Follow,
            None => self.scroll,
        };
    }

    /// Mouse: wheel scrolling, click-to-open, and drag selection that copies on
    /// release.
    pub fn on_mouse(&mut self, mouse: MouseEvent) {
        match mouse.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let delta = if mouse.kind == MouseEventKind::ScrollUp {
                    -3
                } else {
                    3
                };
                // The transcript scrolls only while it is the visible surface.
                match self.overlay {
                    Overlay::None | Overlay::Find { .. } => self.scroll_by(delta),
                    Overlay::Diff(_) | Overlay::Tool(_) => self.scroll_view(delta),
                    _ => self.overlay_move(delta.signum()),
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                self.selection_start = Some((mouse.column, mouse.row));
                self.selection_end = None;
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if self.selection_start.is_some() {
                    self.selection_end = Some((mouse.column, mouse.row));
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                let cell = (mouse.column, mouse.row);
                if self.selection_start == Some(cell) {
                    self.selection_start = None;
                    self.selection_end = None;
                    self.click_transcript(cell);
                } else {
                    self.selection_end = Some(cell);
                    self.copy_selection();
                }
            }
            _ => {}
        }
    }

    /// A click: a card header folds the card, its hint opens the tool view, and
    /// a turn-end line opens the turn's diff.
    fn click_transcript(&mut self, (column, row): (u16, u16)) {
        if !self.overlay.is_none() {
            return;
        }
        let Some((index, line)) = self.layout.hit(column, row) else {
            return;
        };
        let all_expanded = self.tools_expanded;
        let Some(session) = self.session.as_mut() else {
            return;
        };
        match session.transcript.blocks().get(index) {
            Some(Block::Tool(call)) => {
                let key = call.key.clone();
                // The hint is a card's last row; the drawn row is what was clicked.
                let on_hint = self
                    .rendered
                    .row(row)
                    .is_some_and(|text| text.trim_end().ends_with(blocks::FULL_OUTPUT_HINT));
                if line == 0 {
                    if let Some(call) = session.transcript.find_tool_mut(&key) {
                        call.toggle_expanded(all_expanded);
                    }
                } else if on_hint {
                    self.open_panel(Panel::Tool(key));
                }
            }
            Some(Block::TurnEnd { .. }) => self.open_panel(Panel::Diff(super::DiffScope::Turn)),
            _ => {}
        }
    }

    /// Copies the drag that just ended, if it covered more than one cell.
    fn copy_selection(&mut self) {
        let (from, to) = (self.selection_start.take(), self.selection_end.take());
        let (Some(from), Some(to)) = (from, to) else {
            return;
        };
        let drag = selection::Drag { from, to };
        if drag.is_click() {
            return;
        }
        if let Some(text) = self
            .selected_text(drag)
            .filter(|text| !text.trim().is_empty())
        {
            self.copy(text);
        }
    }

    /// The reasoning levels this session's model supports, in Agent order.
    pub fn reasoning_levels(&self) -> Vec<Reasoning> {
        self.session
            .as_ref()
            .and_then(|session| session.model(&self.models))
            .map(|model| model.supported_reasoning.clone())
            .unwrap_or_default()
    }

    /// Shift+Tab: the next supported reasoning level.
    fn next_reasoning(&self) -> Option<Reasoning> {
        let current = self.session.as_ref()?.info.reasoning;
        let levels = self.reasoning_levels();
        let next = levels
            .iter()
            .position(|level| *level == current)
            .map_or(0, |at| (at + 1) % levels.len());
        levels.get(next).copied()
    }
}
