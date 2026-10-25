//! Keys for pickers, sheets and views.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::overlay::{filtered_sessions, matching_models, session_label};
use crate::app::picker;
use crate::app::{Action, App, NoticeLevel, Overlay, find};

impl App {
    pub(super) fn on_overlay_key(&mut self, key: KeyEvent) -> Option<Action> {
        if matches!(
            self.overlay,
            Overlay::Palette { .. } | Overlay::Files { .. }
        ) {
            return self.on_completion_key(key);
        }
        if matches!(
            self.overlay,
            Overlay::Sessions {
                renaming: Some(_),
                ..
            }
        ) {
            self.on_rename_key(key);
            return None;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let scrolling = matches!(self.overlay, Overlay::Diff(_));
        let page = self.viewport_rows().saturating_sub(2) as isize;
        match key.code {
            KeyCode::Esc => self.overlay = Overlay::None,
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
            KeyCode::Tab | KeyCode::Down => self.overlay_move(1),
            KeyCode::PageUp => self.overlay_move(-10),
            KeyCode::PageDown => self.overlay_move(10),
            KeyCode::Backspace => self.overlay_filter_push(None),
            KeyCode::Char('d') if ctrl => self.delete_confirm(),
            KeyCode::Char('n') if ctrl => self.start_rename(),
            KeyCode::Char('c') if matches!(self.overlay, Overlay::SessionInfo { .. }) => {
                self.copy_session_id();
            }
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
        if self.remote.active() {
            self.remote_flash();
            return;
        }
        let Overlay::Sessions {
            rows,
            filter,
            selected,
            armed,
            ..
        } = &mut self.overlay
        else {
            return;
        };
        let resident = self.session.as_ref();
        let Some(info) = filtered_sessions(rows, filter, resident)
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
            let label = session_label(info, resident);
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

    /// Moves the selected row of the active overlay by `delta`.
    pub fn overlay_move(&mut self, delta: isize) {
        let len = match &self.overlay {
            Overlay::Palette { .. } | Overlay::Files { .. } => self.completion_len(),
            Overlay::Sessions { rows, filter, .. } => {
                filtered_sessions(rows, filter, self.session.as_ref()).len()
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
            Overlay::Find { .. } => self.find_step(delta > 0),
            Overlay::Context | Overlay::SessionInfo { .. } | Overlay::None => {}
        }
    }

    /// Scrolls the diff view by `delta` rows, clamping at the ends.
    pub(crate) fn scroll_view(&mut self, delta: isize) {
        // The title and hint rows are not content.
        let rows = self.viewport_rows().saturating_sub(2);
        if let Overlay::Diff(diff) = &mut self.overlay {
            diff.scroll_by(delta, rows);
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
        let show_reasoning = self.show_reasoning;
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
                *matches = find::find_matches(blocks, query, show_reasoning);
                *current = 0;
            }
            _ => {}
        }
        self.show_match();
    }

    fn on_overlay_accept(&mut self) -> Option<Action> {
        if self.remote.active()
            && matches!(
                self.overlay,
                Overlay::Sessions { .. } | Overlay::Models { .. } | Overlay::Reasoning { .. }
            )
        {
            self.remote_flash();
            return None;
        }
        match std::mem::replace(&mut self.overlay, Overlay::None) {
            Overlay::Sessions {
                selected,
                filter,
                rows,
                ..
            } => filtered_sessions(&rows, &filter, self.session.as_ref())
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
            Overlay::SessionInfo { .. } => {
                self.copy_session_id();
                None
            }
            Overlay::Palette { .. }
            | Overlay::Files { .. }
            | Overlay::Context
            | Overlay::Help { .. }
            | Overlay::Diff(_)
            | Overlay::None => None,
        }
    }
}
