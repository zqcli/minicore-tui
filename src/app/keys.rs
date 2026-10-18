//! Key and mouse dispatch: the single place that turns terminal input into
//! [`Action`]s or composer edits.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use super::{Action, App, Overlay, features};
use crate::app::commands::{self, Executed};

/// What one input event resolved to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Input {
    Action(Action),
    Edited,
    /// The overlay consumed the event; redraw.
    Consumed,
    /// Nothing to do.
    Ignored,
}

impl App {
    /// Routes a key event to the focused surface.
    pub fn on_key(&mut self, key: KeyEvent) -> Input {
        if self.fatal.is_some() {
            return match key.code {
                KeyCode::Char('q') | KeyCode::Esc => Input::Action(Action::Quit),
                _ => Input::Ignored,
            };
        }
        if self.overlay.is_none() && self.has_interaction() {
            return self.on_interaction_key(key);
        }
        if !self.overlay.is_none() {
            return self.on_overlay_key(key);
        }
        self.on_composer_key(key)
    }

    /// Approval prompts own the keyboard until answered. Anything else is
    /// swallowed rather than reaching the draft behind the prompt.
    fn on_interaction_key(&mut self, key: KeyEvent) -> Input {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let options = self
            .session
            .as_ref()
            .and_then(|session| session.interaction.as_ref())
            .map_or(0, |interaction| interaction.options().len());
        match key.code {
            KeyCode::Char('y') if !ctrl => Input::Action(Action::Answer(super::Answer::Allow)),
            KeyCode::Char('n') if !ctrl => Input::Action(Action::Answer(super::Answer::Deny)),
            KeyCode::Esc => Input::Action(Action::Answer(super::Answer::Deny)),
            KeyCode::Enter => {
                let choice = self.interaction_choice;
                let is_approval = self
                    .session
                    .as_ref()
                    .and_then(|session| session.interaction.as_ref())
                    .is_some_and(|interaction| {
                        matches!(interaction.kind, crate::app::InteractionKind::Approval)
                    });
                Input::Action(Action::Answer(if is_approval {
                    if choice == 0 {
                        super::Answer::Allow
                    } else {
                        super::Answer::Deny
                    }
                } else {
                    super::Answer::Choice(choice)
                }))
            }
            KeyCode::Up | KeyCode::Down => {
                if options > 0 {
                    let delta = if key.code == KeyCode::Up { -1 } else { 1 };
                    let next =
                        (self.interaction_choice as isize + delta).clamp(0, options as isize - 1);
                    self.interaction_choice = next as usize;
                }
                Input::Consumed
            }
            _ => Input::Ignored,
        }
    }

    fn on_overlay_key(&mut self, key: KeyEvent) -> Input {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => {
                self.delete_pending = None;
                self.overlay = Overlay::None;
                Input::Consumed
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.find_step(false);
                Input::Consumed
            }
            KeyCode::Enter => self.on_overlay_accept(),
            KeyCode::Up => {
                self.overlay_move(-1);
                Input::Consumed
            }
            KeyCode::Tab if matches!(self.overlay, Overlay::Diff { .. }) => {
                self.diff_cycle_scope();
                Input::Consumed
            }
            KeyCode::Tab => {
                self.overlay_move(1);
                Input::Consumed
            }
            KeyCode::Down => {
                self.overlay_move(1);
                Input::Consumed
            }
            KeyCode::Char('c') if ctrl => self.copy_view(),
            KeyCode::PageUp => {
                self.overlay_move(-10);
                Input::Consumed
            }
            KeyCode::PageDown => {
                self.overlay_move(10);
                Input::Consumed
            }
            KeyCode::Backspace if matches!(self.overlay, Overlay::Palette { .. }) => {
                self.composer.backspace();
                self.refresh_palette();
                Input::Edited
            }
            KeyCode::Backspace => {
                self.overlay_filter_push(None);
                Input::Consumed
            }
            KeyCode::Char('d') if ctrl => {
                self.delete_confirm();
                Input::Consumed
            }
            KeyCode::Char('r') if matches!(self.overlay, Overlay::Context) => {
                self.refresh_context();
                Input::Consumed
            }
            KeyCode::Char('y') if matches!(self.overlay, Overlay::Interaction) => {
                self.interaction_choice = 0;
                self.on_overlay_accept()
            }
            KeyCode::Char('n') if matches!(self.overlay, Overlay::Interaction) => {
                self.interaction_choice = 1;
                self.on_overlay_accept()
            }
            // The palette is driven by the composer line, so its arguments
            // (`/rename <title>`, `/export <path>`) stay typable.
            KeyCode::Char(character)
                if !ctrl && matches!(self.overlay, Overlay::Palette { .. }) =>
            {
                self.composer.type_char(character);
                self.refresh_palette();
                Input::Edited
            }
            KeyCode::Char(character) if !ctrl => {
                self.overlay_filter_push(Some(character));
                Input::Consumed
            }
            _ => Input::Ignored,
        }
    }

    /// Ctrl+D in the sessions picker: the first press arms the delete, the
    /// second deletes the selected session.
    fn delete_confirm(&mut self) {
        let Overlay::Sessions { selected, .. } = &self.overlay else {
            return;
        };
        let Some(info) = self.session_rows.get(*selected) else {
            return;
        };
        let id = info.session_id.clone();
        let resident = self
            .session
            .as_ref()
            .is_some_and(|session| session.info.session_id == id);
        match features::delete_step(self.delete_pending.as_deref(), &id, resident) {
            features::DeleteStep::Ignore => {}
            features::DeleteStep::Resident => self.notice(
                crate::transcript::block::NoticeLevel::Error,
                "Open another session before deleting this one",
            ),
            features::DeleteStep::Arm => {
                let label = info
                    .title
                    .clone()
                    .unwrap_or_else(|| "this session".to_owned());
                self.delete_pending = Some(id);
                self.notice(
                    crate::transcript::block::NoticeLevel::Info,
                    format!("Press Ctrl+D again to delete \"{label}\" permanently"),
                );
            }
            features::DeleteStep::Delete => {
                self.delete_pending = None;
                self.on_action(Action::DeleteSession(id));
            }
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
        if matches.is_empty() {
            return;
        }
        let next = super::features::step_match(*current, matches.len(), forward);
        *current = next;
        let block = matches[next].block;
        self.scroll = super::Scroll::Anchor { block, offset: 0 };
    }

    /// Moves the selected row of the active overlay by `delta`.
    pub fn overlay_move(&mut self, delta: isize) {
        let step = |selected: usize, len: usize| -> usize {
            if len == 0 {
                return 0;
            }
            let next = selected as isize + delta;
            next.clamp(0, len as isize - 1) as usize
        };
        let models = self.models.len();
        let reasoning = self.reasoning_levels().len();
        let sessions = match &self.overlay {
            Overlay::Sessions { filter, .. } => self.filtered_sessions(filter).len(),
            _ => self.session_rows.len(),
        };
        match &mut self.overlay {
            Overlay::Palette { selected, filter } => {
                let len = commands::filter(filter).len();
                *selected = step(*selected, len);
            }
            Overlay::Sessions { selected, .. } => *selected = step(*selected, sessions),
            Overlay::Models { selected } => *selected = step(*selected, models),
            Overlay::Reasoning { selected } => *selected = step(*selected, reasoning),
            Overlay::Files { selected, filter } => {
                let len = crate::ui::files::filter_count(filter);
                *selected = step(*selected, len);
            }
            Overlay::Diff { file, scroll, .. } => {
                let next = step(*file, self.diff.files.len());
                if next != *file {
                    *file = next;
                    *scroll = 0;
                    self.diff.select(next);
                    // The hunks of the newly selected file are paged in.
                    self.load_selected_diff();
                }
            }
            Overlay::Help => {
                self.help_scroll = self.help_scroll.saturating_add_signed(delta * 3);
            }
            Overlay::Find {
                current, matches, ..
            } => {
                *current = super::features::step_match(*current, matches.len(), delta > 0);
            }
            Overlay::Interaction => {
                let options = self
                    .session
                    .as_ref()
                    .and_then(|session| session.interaction.as_ref())
                    .map_or(0, |interaction| interaction.options().len());
                self.interaction_choice = step(self.interaction_choice, options);
            }
            _ => {}
        }
    }

    fn overlay_filter_push(&mut self, character: Option<char>) {
        if let Overlay::Find { query, .. } = &mut self.overlay {
            match character {
                Some(character) => query.push(character),
                None => {
                    query.pop();
                }
            }
            let trimmed = query.trim().to_owned();
            // Recompute outside the overlay borrow: the blocks live in the
            // session, which `self` still owns.
            let found = features::find_matches(self.transcript_blocks(), &trimmed);
            if let Overlay::Find {
                matches, current, ..
            } = &mut self.overlay
            {
                *matches = found;
                *current = 0;
            }
            return;
        }
        match &mut self.overlay {
            Overlay::Palette { filter, selected } => {
                if let Some(character) = character {
                    filter.push(character);
                } else {
                    filter.pop();
                }
                *selected = 0;
            }
            Overlay::Sessions {
                filter, selected, ..
            } => {
                if let Some(character) = character {
                    filter.push(character);
                } else {
                    filter.pop();
                }
                *selected = 0;
            }
            Overlay::Find { .. } => {}
            Overlay::Files { filter, selected } => {
                if let Some(character) = character {
                    filter.push(character);
                } else {
                    filter.pop();
                }
                *selected = 0;
            }
            _ => {}
        }
    }

    /// Re-filters the command palette from the composer line and keeps the
    /// selection on the top row.
    fn refresh_palette(&mut self) {
        let Some((name, _)) = commands::parse(self.composer.text()) else {
            self.overlay = Overlay::None;
            return;
        };
        let Overlay::Palette { filter, selected } = &mut self.overlay else {
            return;
        };
        *filter = name.to_owned();
        *selected = 0;
    }

    fn on_overlay_accept(&mut self) -> Input {
        let overlay = std::mem::replace(&mut self.overlay, Overlay::None);
        match overlay {
            Overlay::Palette { selected, filter } => {
                let matches = commands::filter(&filter);
                let Some(command) = matches.get(selected).or_else(|| matches.first()) else {
                    self.composer.clear();
                    return Input::Consumed;
                };
                // The composer holds the whole command line, so an argument
                // typed after the name reaches the command.
                let arg = commands::parse(self.composer.text())
                    .filter(|(name, _)| *name == command.name)
                    .map_or("", |(_, arg)| arg)
                    .to_owned();
                self.composer.clear();
                match commands::execute(command.name, &arg) {
                    Some(Executed::Action(action)) => Input::Action(action),
                    Some(Executed::Panel(panel)) => {
                        self.open_panel(panel);
                        Input::Consumed
                    }
                    _ => Input::Consumed,
                }
            }
            Overlay::Sessions {
                selected, filter, ..
            } => {
                let rows = self.filtered_sessions(&filter);
                let Some(info) = rows.get(selected) else {
                    return Input::Consumed;
                };
                let id = info.session_id.clone();
                Input::Action(Action::OpenSession(id))
            }
            Overlay::Find { .. } => {
                // Enter is "next match"; the bar stays open.
                self.find_step(true);
                Input::Consumed
            }
            Overlay::Models { selected } => {
                let Some(model) = self.models.get(selected) else {
                    return Input::Consumed;
                };
                Input::Action(Action::SetModel(model.id.clone()))
            }
            Overlay::Reasoning { selected } => {
                let levels = self.reasoning_levels();
                let Some(level) = levels.get(selected) else {
                    return Input::Consumed;
                };
                Input::Action(Action::SetReasoning(*level))
            }
            Overlay::Files { filter, selected } => {
                let path = crate::ui::files::filtered(&filter)
                    [selected.min(crate::ui::files::filter_count(&filter).saturating_sub(1))]
                .clone();
                self.composer.insert(&path);
                Input::Edited
            }
            Overlay::Interaction => {
                let choice = self.interaction_choice;
                let is_approval = self
                    .session
                    .as_ref()
                    .and_then(|session| session.interaction.as_ref())
                    .is_some_and(|interaction| {
                        matches!(interaction.kind, crate::app::InteractionKind::Approval)
                    });
                Input::Action(Action::Answer(if is_approval {
                    if choice == 0 {
                        super::Answer::Allow
                    } else {
                        super::Answer::Deny
                    }
                } else {
                    super::Answer::Choice(choice)
                }))
            }
            _ => Input::Consumed,
        }
    }

    fn on_composer_key(&mut self, key: KeyEvent) -> Input {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        if !matches!(key.code, KeyCode::Char('c') if ctrl) {
            self.quit_armed = false;
        }
        match key.code {
            KeyCode::Enter if alt => Input::Action(Action::QueueFollowUp),
            KeyCode::Enter if shift => {
                self.composer.newline();
                Input::Edited
            }
            KeyCode::Enter => self.submit_or_command(),
            KeyCode::Char('j') if ctrl => {
                self.composer.newline();
                Input::Edited
            }
            KeyCode::Esc => {
                if self.escape() {
                    Input::Consumed
                } else {
                    Input::Ignored
                }
            }
            KeyCode::Char('c') if ctrl => {
                if !self.composer.is_empty() {
                    self.composer.clear();
                    self.quit_armed = false;
                    return Input::Edited;
                }
                if self.quit_armed {
                    Input::Action(Action::Quit)
                } else {
                    self.quit_armed = true;
                    self.notice(
                        crate::transcript::block::NoticeLevel::Info,
                        "Press Ctrl+C again to quit",
                    );
                    Input::Edited
                }
            }
            KeyCode::Char('d') if ctrl => {
                if self.composer.is_empty() {
                    Input::Action(Action::Quit)
                } else {
                    self.composer.delete();
                    Input::Edited
                }
            }
            KeyCode::Char('g') if ctrl => Input::Action(Action::EditDraft),
            KeyCode::Char('o') if ctrl => {
                self.tools_expanded = !self.tools_expanded;
                Input::Consumed
            }
            KeyCode::Char('t') if ctrl => {
                self.show_reasoning = !self.show_reasoning;
                Input::Consumed
            }
            KeyCode::Char('r') if ctrl => {
                self.open_panel(super::Panel::Sessions);
                Input::Consumed
            }
            KeyCode::Char('l') if ctrl => {
                self.open_panel(super::Panel::Models);
                Input::Consumed
            }
            KeyCode::Char('f') if ctrl => {
                self.open_panel(super::Panel::Find);
                Input::Consumed
            }
            KeyCode::Char('a') if ctrl => {
                self.composer.move_home();
                Input::Edited
            }
            KeyCode::Char('e') if ctrl => {
                self.composer.move_end();
                Input::Edited
            }
            KeyCode::Char('u') if ctrl => {
                self.composer.delete_to_line_start();
                Input::Edited
            }
            KeyCode::Char('k') if ctrl => {
                self.composer.delete_to_line_end();
                Input::Edited
            }
            KeyCode::Char('w') if ctrl => {
                self.composer.delete_word_left();
                Input::Edited
            }
            KeyCode::Char('z') if ctrl => {
                self.composer.undo();
                Input::Edited
            }
            KeyCode::Char('y') if ctrl => {
                self.composer.redo();
                Input::Edited
            }
            KeyCode::BackTab => Input::Action(Action::SetReasoning(self.next_reasoning())),
            KeyCode::Char('@') => {
                self.composer.type_char('@');
                self.open_panel(super::Panel::Files);
                Input::Edited
            }
            KeyCode::Char(character) if ctrl => {
                let _ = character;
                Input::Ignored
            }
            KeyCode::Char(character) => {
                self.composer.type_char(character);
                if commands::opens_palette(self.composer.text(), true) {
                    self.open_panel(super::Panel::Palette);
                }
                Input::Edited
            }
            KeyCode::Backspace => {
                self.composer.backspace();
                Input::Edited
            }
            KeyCode::Delete => {
                self.composer.delete();
                Input::Edited
            }
            KeyCode::Left if alt => {
                self.composer.move_word_left();
                Input::Edited
            }
            KeyCode::Right if alt => {
                self.composer.move_word_right();
                Input::Edited
            }
            KeyCode::Left => {
                self.composer.move_left();
                Input::Edited
            }
            KeyCode::Right => {
                self.composer.move_right();
                Input::Edited
            }
            // Arrows move inside a multi-line draft and only browse history at
            // its first and last line.
            KeyCode::Up if self.composer.move_up() => Input::Edited,
            KeyCode::Up => self.browse_history(true),
            KeyCode::Down if self.composer.move_down() => Input::Edited,
            KeyCode::Down => self.browse_history(false),
            KeyCode::Home if ctrl => self.scroll_to_start(),
            KeyCode::End if ctrl => self.scroll_to_end(),
            KeyCode::Home => {
                self.composer.move_home();
                Input::Edited
            }
            KeyCode::End => {
                self.composer.move_end();
                Input::Edited
            }
            KeyCode::PageUp => self.scroll_pages(-1),
            KeyCode::PageDown => self.scroll_pages(1),
            KeyCode::F(1) => {
                self.open_panel(super::Panel::Help);
                Input::Consumed
            }
            _ => Input::Ignored,
        }
    }

    /// A terminal paste. The composer decides whether the payload becomes a
    /// single token, and the palette follows immediately (`/` or `@` pasted
    /// first opens the matching picker).
    pub fn on_paste(&mut self, text: &str) {
        if self.has_interaction() {
            return;
        }
        self.composer.insert(text);
        if commands::opens_palette(self.composer.text(), true)
            && matches!(self.overlay, Overlay::None)
        {
            self.open_panel(super::Panel::Palette);
        }
    }

    fn submit_or_command(&mut self) -> Input {
        if let Some((name, argument)) = commands::parse(self.composer.text()) {
            let argument = argument.to_owned();
            match commands::execute(name, &argument) {
                Some(Executed::Action(action)) => {
                    self.composer.clear();
                    return Input::Action(action);
                }
                Some(Executed::Panel(panel)) => {
                    self.composer.clear();
                    self.open_panel(panel);
                    return Input::Consumed;
                }
                Some(Executed::Noop) => {
                    self.notice(
                        crate::transcript::block::NoticeLevel::Error,
                        "Missing argument",
                    );
                    return Input::Consumed;
                }
                None => {
                    if name.is_empty() {
                        return Input::Consumed;
                    }
                    self.notice(
                        crate::transcript::block::NoticeLevel::Error,
                        format!("Unknown command /{name}"),
                    );
                    return Input::Consumed;
                }
            }
        }
        Input::Action(Action::Submit)
    }

    /// Prompt history browsing at the first or last line of the draft.
    fn browse_history(&mut self, up: bool) -> Input {
        let moved = if up {
            self.composer.history_prev()
        } else {
            self.composer.history_next()
        };
        if moved { Input::Edited } else { Input::Ignored }
    }

    fn scroll_pages(&mut self, direction: isize) -> Input {
        self.scroll_by(
            direction * self.viewport_rows() as isize,
            self.viewport_rows(),
        );
        Input::Consumed
    }

    fn scroll_to_start(&mut self) -> Input {
        self.scroll = super::Scroll::Anchor {
            block: 0,
            offset: 0,
        };
        // The top of the loaded window is also where the next page begins, so
        // the same request that a scroll to the edge makes belongs here.
        self.on_action(super::Action::LoadOlder);
        Input::Consumed
    }

    fn scroll_to_end(&mut self) -> Input {
        self.scroll = super::Scroll::Follow;
        Input::Consumed
    }

    /// Mouse: wheel scrolling, click-to-open, and drag selection that copies on
    /// release.
    pub fn on_mouse(&mut self, mouse: MouseEvent) -> Input {
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                if matches!(self.overlay, Overlay::Diff { .. }) {
                    self.scroll_diff(-3);
                } else if matches!(self.overlay, Overlay::Tool(_)) {
                    self.scroll_tool(-3);
                } else {
                    self.scroll_by(-3, self.viewport_rows());
                }
                Input::Consumed
            }
            MouseEventKind::ScrollDown => {
                if matches!(self.overlay, Overlay::Diff { .. }) {
                    self.scroll_diff(3);
                } else if matches!(self.overlay, Overlay::Tool(_)) {
                    self.scroll_tool(3);
                } else {
                    self.scroll_by(3, self.viewport_rows());
                }
                Input::Consumed
            }
            MouseEventKind::Down(MouseButton::Left) => {
                self.selection_start = Some((mouse.column, mouse.row));
                self.selection_end = None;
                Input::Consumed
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if self.selection_start.is_some() {
                    self.selection_end = Some((mouse.column, mouse.row));
                }
                Input::Consumed
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.selection_end = Some((mouse.column, mouse.row));
                self.copy_selection();
                Input::Consumed
            }
            _ => Input::Ignored,
        }
    }

    /// Copies the drag that just ended, if it covered more than one cell.
    fn copy_selection(&mut self) {
        let (Some(from), Some(to)) = (self.selection_start, self.selection_end) else {
            self.selection_start = None;
            self.selection_end = None;
            return;
        };
        self.selection_start = None;
        self.selection_end = None;
        let drag = crate::ui::selection::Drag { from, to };
        if drag.is_click() {
            return;
        }
        let Some(text) = self.selected_text(drag) else {
            return;
        };
        if !text.trim().is_empty() {
            self.copy(text);
        }
    }

    fn scroll_diff(&mut self, delta: isize) {
        let rows = self.viewport_rows();
        self.diff.scroll_by(delta * 3, rows);
        if let Overlay::Diff { scroll, .. } = &mut self.overlay {
            *scroll = self.diff.scroll.min(u16::MAX as usize) as u16;
        }
    }

    fn scroll_tool(&mut self, delta: isize) {
        let rows = self.viewport_rows();
        self.tool_view.scroll_by(delta * 3, rows);
    }

    /// The reasoning levels this session's model supports, in Agent order.
    pub fn reasoning_levels(&self) -> Vec<crate::protocol::Reasoning> {
        let levels = self
            .session
            .as_ref()
            .and_then(|session| session.model(&self.models))
            .map(|model| model.supported_reasoning.clone())
            .unwrap_or_default();
        if levels.is_empty() {
            vec![
                crate::protocol::Reasoning::Auto,
                crate::protocol::Reasoning::Low,
                crate::protocol::Reasoning::Medium,
                crate::protocol::Reasoning::High,
            ]
        } else {
            levels
        }
    }

    /// Shift+Tab: the next supported reasoning level.
    fn next_reasoning(&self) -> crate::protocol::Reasoning {
        let levels = self.reasoning_levels();
        let current = self
            .session
            .as_ref()
            .map_or(crate::protocol::Reasoning::default(), |session| {
                session.info.reasoning
            });
        let index = levels
            .iter()
            .position(|level| *level == current)
            .unwrap_or(0);
        levels[(index + 1) % levels.len()]
    }
}
