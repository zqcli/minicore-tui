//! Keys for pickers, sheets and views.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::overlay::{matching_agents, matching_models, session_target};
use crate::app::picker;
use crate::app::{Action, App, NoticeLevel, Overlay, find};

impl App {
    pub(super) fn on_overlay_paste(&mut self, text: &str) -> bool {
        if !matches!(
            self.overlay,
            Overlay::Find { .. }
                | Overlay::Sessions { renaming: None, .. }
                | Overlay::Models { .. }
                | Overlay::Agents { .. }
        ) {
            return false;
        }
        let single_line = text.replace("\r\n", " ").replace(['\r', '\n'], " ");
        let text = crate::wrap::sanitize(&single_line);
        if !text.is_empty() {
            self.edit_overlay_filter(|filter| {
                filter.push_str(&text);
                true
            });
        }
        true
    }

    pub(super) fn on_overlay_key(&mut self, key: KeyEvent) -> Option<Action> {
        if matches!(self.overlay, Overlay::AgentDelete { .. }) {
            match key.code {
                KeyCode::Char('y') if key.modifiers.is_empty() => {
                    if let Overlay::AgentDelete {
                        agent_id, sessions, ..
                    } = std::mem::replace(&mut self.overlay, Overlay::None)
                    {
                        return Some(Action::DeleteAgent {
                            agent_id,
                            session_ids: sessions.into_iter().map(|(id, _)| id).collect(),
                        });
                    }
                }
                KeyCode::Esc | KeyCode::Char('n') => self.overlay = Overlay::None,
                KeyCode::Up => self.overlay_move(-1),
                KeyCode::Down => self.overlay_move(1),
                KeyCode::PageUp => self.overlay_move(-10),
                KeyCode::PageDown => self.overlay_move(10),
                _ => {}
            }
            return None;
        }
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
            return self.on_rename_key(key);
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let scrolling = matches!(self.overlay, Overlay::Diff(_) | Overlay::Error { .. });
        let page = match &self.overlay {
            Overlay::Error { visible_rows, .. } => *visible_rows,
            _ => self.viewport_rows().saturating_sub(2),
        }
        .max(1) as isize;
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
            KeyCode::Char('c') if matches!(self.overlay, Overlay::SessionInfo) => {
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
        if let Overlay::Agents { filter, selected } = &self.overlay {
            let id = matching_agents(&self.agents, filter)
                .get(*selected)
                .map(|agent| agent.id.clone());
            if let Some(id) = id {
                self.confirm_agent_delete(id);
            }
            return;
        }
        let Some(info) = self.selected_session() else {
            return;
        };
        let target = session_target(info);
        let label = self.session_title(info);
        let again = matches!(&self.overlay, Overlay::Sessions { armed: Some(armed), .. } if *armed == target);
        if let Overlay::Sessions { armed, .. } = &mut self.overlay {
            *armed = (!again).then(|| target.clone());
        }
        if again {
            self.on_action(Action::DeleteSession(target));
        } else {
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
            Overlay::Sessions { rows, filter, .. } => self.filtered_sessions(rows, filter).len(),
            Overlay::Models { filter, .. } => matching_models(&self.models, filter).len(),
            Overlay::Agents { filter, .. } => matching_agents(&self.agents, filter).len(),
            Overlay::Reasoning { .. } => self.reasoning_levels().len(),
            Overlay::AgentDelete { sessions, .. } => sessions.len(),
            Overlay::Diff(diff) => diff.files.len(),
            _ => 0,
        };
        match &mut self.overlay {
            Overlay::Palette { selected }
            | Overlay::Sessions { selected, .. }
            | Overlay::Models { selected, .. }
            | Overlay::Agents { selected, .. }
            | Overlay::AgentDelete { selected, .. }
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
            Overlay::Help { scroll } | Overlay::Error { scroll, .. } => {
                *scroll = scroll.saturating_add_signed(delta * 3)
            }
            Overlay::Find { .. } => self.find_step(delta > 0),
            Overlay::Unconfirmed { .. } => self.move_local_input(delta),
            Overlay::Context | Overlay::SessionInfo | Overlay::None => {}
        }
    }

    /// Scrolls the diff view by `delta` rows, clamping at the ends.
    pub(crate) fn scroll_view(&mut self, delta: isize) {
        // The title and hint rows are not content.
        let rows = self.viewport_rows().saturating_sub(2);
        match &mut self.overlay {
            Overlay::Diff(diff) => diff.scroll_by(delta, rows),
            Overlay::Error { scroll, .. } => *scroll = scroll.saturating_add_signed(delta),
            _ => {}
        }
    }

    /// Types `character` into the overlay's filter, or deletes the last one.
    fn overlay_filter_push(&mut self, character: Option<char>) {
        self.edit_overlay_filter(|text| match character {
            Some(character) => {
                text.push(character);
                true
            }
            None => text.pop().is_some(),
        });
    }

    fn edit_overlay_filter(&mut self, edit: impl FnOnce(&mut String) -> bool) {
        let show_reasoning = self.show_reasoning;
        let blocks = self
            .resident
            .current()
            .map_or(&[][..], |session| session.transcript.blocks());
        match &mut self.overlay {
            Overlay::Sessions {
                filter,
                selected,
                armed,
                ..
            } => {
                if !edit(filter) {
                    return;
                }
                *selected = 0;
                *armed = None;
            }
            Overlay::Models { filter, selected } | Overlay::Agents { filter, selected } => {
                if !edit(filter) {
                    return;
                }
                *selected = 0;
            }
            Overlay::Find {
                query,
                matches,
                current,
            } => {
                if !edit(query) {
                    return;
                }
                *matches = find::find_matches(blocks, query, show_reasoning);
                *current = 0;
            }
            _ => return,
        }
        self.show_match();
    }

    fn on_overlay_accept(&mut self) -> Option<Action> {
        match std::mem::replace(&mut self.overlay, Overlay::None) {
            review @ (Overlay::Unconfirmed { .. } | Overlay::AgentDelete { .. }) => {
                self.overlay = review;
                None
            }
            Overlay::Sessions {
                selected,
                filter,
                rows,
                ..
            } => self
                .filtered_sessions(&rows, &filter)
                .get(selected)
                .map(|info| Action::OpenSession(session_target(info))),
            find @ Overlay::Find { .. } => {
                self.overlay = find;
                self.find_step(true);
                None
            }
            Overlay::Models { filter, selected } => matching_models(&self.models, &filter)
                .get(selected)
                .map(|model| Action::SwitchModel(model.model.to_string())),
            Overlay::Agents { filter, selected } => matching_agents(&self.agents, &filter)
                .get(selected)
                .map(|agent| Action::SelectAgent(agent.id.clone())),
            Overlay::Reasoning { selected } => self
                .reasoning_levels()
                .get(selected)
                .map(|level| Action::SetReasoning(*level)),
            Overlay::SessionInfo => {
                self.copy_session_id();
                None
            }
            Overlay::Palette { .. }
            | Overlay::Files { .. }
            | Overlay::Context
            | Overlay::Help { .. }
            | Overlay::Error { .. }
            | Overlay::Diff(_)
            | Overlay::None => None,
        }
    }
}
