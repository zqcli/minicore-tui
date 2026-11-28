//! Keys for pickers, sheets and views.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::overlay::{
    TaskFocus, async_tasks, matching_agents, matching_async_tasks, matching_models, session_label,
    session_target,
};
use crate::app::picker;
use crate::app::{Action, App, Overlay, find};

impl App {
    pub(super) fn on_overlay_paste(&mut self, text: &str) -> bool {
        if matches!(self.overlay, Overlay::Files { .. }) {
            self.edit_file_filter(text);
            return true;
        }
        if let Overlay::Delegations { focus, .. } = self.overlay {
            match focus {
                TaskFocus::Composer => {
                    if self.inspected_can_send() {
                        self.task_inspector.composer.insert(text);
                    }
                    return true;
                }
                TaskFocus::Detail => return true,
                TaskFocus::List => {}
            }
        }
        if let Overlay::Diff(diff) = &self.overlay
            && diff.focus == crate::ui::views::split::PaneFocus::Detail
        {
            return true;
        }
        if !matches!(
            self.overlay,
            Overlay::Find { .. }
                | Overlay::Sessions { renaming: None, .. }
                | Overlay::Models { .. }
                | Overlay::Agents { .. }
                | Overlay::Delegations { .. }
                | Overlay::Diff(_)
                | Overlay::Help { .. }
        ) {
            return false;
        }
        let single_line = text.replace("\r\n", " ").replace(['\r', '\n'], " ");
        let text = crate::wrap::sanitize(&single_line);
        if !text.is_empty() {
            if let Overlay::Help { filter, scroll } = &mut self.overlay {
                filter.push_str(&text);
                *scroll = 0;
                return true;
            }
            if let Overlay::Diff(diff) = &mut self.overlay {
                diff.filter.push_str(&text);
                diff.select(0);
                self.load_selected_diff();
                return true;
            }
            self.edit_overlay_filter(|filter| {
                filter.push_str(&text);
                true
            });
        }
        true
    }

    pub(super) fn on_overlay_key(&mut self, key: KeyEvent) -> Option<Action> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        // Typing in help narrows it, like every other list that can be searched.
        if let Overlay::Help { scroll, filter } = &mut self.overlay {
            match key.code {
                KeyCode::Char('u') if ctrl => {
                    filter.clear();
                    *scroll = 0;
                    return None;
                }
                KeyCode::Char(ch) if !ctrl => {
                    filter.push(ch);
                    *scroll = 0;
                    return None;
                }
                KeyCode::Backspace if !filter.is_empty() => {
                    filter.pop();
                    *scroll = 0;
                    return None;
                }
                _ => {}
            }
        }
        if matches!(self.overlay, Overlay::Confirm { .. }) {
            match key.code {
                KeyCode::Char('y') if key.modifiers.is_empty() => {
                    if let Overlay::Confirm { action, .. } =
                        std::mem::replace(&mut self.overlay, Overlay::None)
                    {
                        return Some(action);
                    }
                }
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('n') => self.overlay = Overlay::None,
                _ => {}
            }
            return None;
        }
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
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let scrolling = matches!(
            self.overlay,
            Overlay::Error { .. } | Overlay::Context { .. } | Overlay::SessionInfo { .. }
        );
        let page = match &self.overlay {
            Overlay::Error { visible_rows, .. } => *visible_rows,
            Overlay::Diff(diff) => diff.visible_rows.saturating_sub(2).max(1),
            Overlay::Delegations { .. } => self
                .task_inspector
                .layout
                .viewport
                .height
                .saturating_sub(2)
                .max(1),
            _ => self.viewport_rows().saturating_sub(2),
        }
        .max(1) as isize;
        if matches!(self.overlay, Overlay::Delegations { .. }) {
            self.on_delegations_key(key, ctrl, alt, shift, page);
            return None;
        }
        if matches!(self.overlay, Overlay::Diff(_)) {
            self.on_diff_key(key, ctrl, page);
            return None;
        }
        if matches!(self.overlay, Overlay::Remote { .. }) {
            return self.on_remote_key(key, ctrl, page);
        }
        match key.code {
            KeyCode::Esc => self.overlay = Overlay::None,
            KeyCode::PageUp if scrolling => self.scroll_view(-page),
            KeyCode::PageDown if scrolling => self.scroll_view(page),
            KeyCode::Home if scrolling => self.scroll_view(isize::MIN),
            KeyCode::End if scrolling => self.scroll_view(isize::MAX),
            KeyCode::Char('c')
                if matches!(
                    self.overlay,
                    Overlay::Error { .. } | Overlay::Context { .. }
                ) =>
            {
                self.copy_view();
            }
            KeyCode::Char('u') if ctrl => {
                self.edit_overlay_filter(|filter| {
                    let had = !filter.is_empty();
                    filter.clear();
                    had
                });
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
            KeyCode::Char('r')
                if !ctrl
                    && matches!(
                        self.overlay,
                        Overlay::Error { .. } | Overlay::SessionInfo { .. }
                    ) =>
            {
                return Some(Action::RetryRecovery);
            }
            KeyCode::Char('l') if !ctrl && matches!(self.overlay, Overlay::SessionInfo { .. }) => {
                return Some(Action::Load);
            }
            KeyCode::Char('u') if !ctrl && matches!(self.overlay, Overlay::SessionInfo { .. }) => {
                return Some(Action::Unload);
            }
            KeyCode::Char('x') if !ctrl && matches!(self.overlay, Overlay::SessionInfo { .. }) => {
                return Some(Action::ConfirmClose);
            }
            KeyCode::Char('r') if matches!(self.overlay, Overlay::Context { .. }) => {
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
        let label = session_label(info).to_owned();
        self.overlay = Overlay::Confirm {
            title: "Delete Session".into(),
            lines: vec![
                format!("{label} · {}", target.session_id),
                "Its stored history is deleted permanently.".into(),
            ],
            action: Action::DeleteSession(target),
        };
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
        self.sync_inspector_selection();
        let len = match &self.overlay {
            Overlay::Palette { .. } | Overlay::Files { .. } => self.completion_len(),
            Overlay::Sessions { rows, filter, .. } => {
                self.picker_sessions(rows, filter).len()
            }
            Overlay::Models { filter, .. } => matching_models(&self.models, filter).len(),
            Overlay::Agents { filter, .. } => matching_agents(&self.agents, filter).len(),
            Overlay::Reasoning { .. } => self.reasoning_levels().len(),
            Overlay::AgentDelete { sessions, .. } => sessions.len(),
            Overlay::Delegations { filter, .. } => {
                let all = async_tasks(self);
                matching_async_tasks(&all, filter).len()
            }
            Overlay::Diff(diff) => diff.matching_indices().len(),
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
            Overlay::Delegations { selected, .. } => {
                let next = picker::step(*selected, delta, len);
                if next != *selected {
                    *selected = next;
                }
            }
            Overlay::Diff(diff) => {
                let next = (diff.selected as isize + delta).clamp(0, len.saturating_sub(1) as isize)
                    as usize;
                if next != diff.selected {
                    diff.select(next);
                    self.load_selected_diff();
                }
            }
            Overlay::Help { scroll, .. }
            | Overlay::Error { scroll, .. }
            | Overlay::Context { scroll }
            | Overlay::SessionInfo { scroll }
            | Overlay::Remote { scroll, .. } => *scroll = scroll.saturating_add_signed(delta * 3),
            Overlay::Find { .. } => self.find_step(delta > 0),
            Overlay::Unconfirmed { .. } => self.move_local_input(delta),
            Overlay::Confirm { .. } | Overlay::None => {}
        }
    }

    /// Scrolls the active view by `delta` rows, clamping at the ends.
    pub(crate) fn scroll_view(&mut self, delta: isize) {
        match &mut self.overlay {
            Overlay::Diff(diff) => diff.scroll = diff.scroll.saturating_add_signed(delta),
            Overlay::Error { scroll, .. }
            | Overlay::Help { scroll, .. }
            | Overlay::Context { scroll }
            | Overlay::SessionInfo { scroll }
            | Overlay::Remote { scroll, .. } => *scroll = scroll.saturating_add_signed(delta),
            Overlay::Delegations { .. } => self.inspector_scroll_by(delta),
            _ => {}
        }
    }

    fn on_diff_key(&mut self, key: KeyEvent, ctrl: bool, page: isize) {
        use crate::ui::views::split::{MIN_SPLIT_WIDTH, PaneFocus};
        let Overlay::Diff(diff) = &self.overlay else {
            return;
        };
        let focus = diff.focus;
        match focus {
            PaneFocus::Detail => match key.code {
                KeyCode::Esc | KeyCode::Left if !ctrl => {
                    if let Overlay::Diff(diff) = &mut self.overlay {
                        diff.focus = PaneFocus::List;
                        diff.detail_open = false;
                    }
                }
                KeyCode::Up | KeyCode::Char('k') if !ctrl => self.scroll_view(-3),
                KeyCode::Down | KeyCode::Char('j') if !ctrl => self.scroll_view(3),
                KeyCode::PageUp => self.scroll_view(-page),
                KeyCode::PageDown => self.scroll_view(page),
                KeyCode::Home => self.scroll_view(isize::MIN),
                KeyCode::End => self.scroll_view(isize::MAX),
                KeyCode::Char('r') => self.refresh_diff(),
                KeyCode::Char('c') if !ctrl => self.copy_view(),
                KeyCode::Char('y') if ctrl => self.copy_view(),
                _ => {}
            },
            PaneFocus::List => match key.code {
                KeyCode::Esc => {
                    let narrow = self.layout.viewport.width < MIN_SPLIT_WIDTH;
                    if let Overlay::Diff(diff) = &mut self.overlay
                        && narrow
                        && diff.detail_open
                    {
                        diff.detail_open = false;
                    } else {
                        self.overlay = Overlay::None;
                    }
                }
                KeyCode::Up => self.overlay_move(-1),
                KeyCode::Down => self.overlay_move(1),
                KeyCode::Right | KeyCode::Tab | KeyCode::Enter if !ctrl => {
                    let has_file =
                        matches!(&self.overlay, Overlay::Diff(d) if d.current().is_some());
                    if has_file && let Overlay::Diff(diff) = &mut self.overlay {
                        diff.focus = PaneFocus::Detail;
                        diff.detail_open = true;
                    }
                }
                KeyCode::PageUp => self.scroll_view(-page),
                KeyCode::PageDown => self.scroll_view(page),
                KeyCode::Home => self.scroll_view(isize::MIN),
                KeyCode::End => self.scroll_view(isize::MAX),
                KeyCode::Char('u') if ctrl => {
                    if let Overlay::Diff(diff) = &mut self.overlay
                        && !diff.filter.is_empty()
                    {
                        diff.filter.clear();
                        diff.select(0);
                        self.load_selected_diff();
                    }
                }
                KeyCode::Char('r') if ctrl => self.refresh_diff(),
                KeyCode::Char('y') if ctrl => self.copy_view(),
                KeyCode::Backspace => {
                    if let Overlay::Diff(diff) = &mut self.overlay
                        && diff.filter.pop().is_some()
                    {
                        diff.select(0);
                        self.load_selected_diff();
                    }
                }
                KeyCode::Char(ch) if !ctrl => {
                    if let Overlay::Diff(diff) = &mut self.overlay {
                        diff.filter.push(ch);
                        diff.select(0);
                        self.load_selected_diff();
                    }
                }
                _ => {}
            },
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
        let transcript = self.resident.current().map(|session| &session.transcript);
        let mut refresh_inspector = false;
        match &mut self.overlay {
            Overlay::Sessions {
                filter, selected, ..
            } => {
                if !edit(filter) {
                    return;
                }
                *selected = 0;
            }
            Overlay::Models { filter, selected } | Overlay::Agents { filter, selected } => {
                if !edit(filter) {
                    return;
                }
                *selected = 0;
            }
            Overlay::Delegations {
                filter,
                selected,
                focus,
                detail_open,
            } => {
                if !edit(filter) {
                    return;
                }
                *selected = 0;
                *focus = TaskFocus::List;
                *detail_open = false;
                refresh_inspector = true;
            }
            Overlay::Find {
                query,
                matches,
                current,
            } => {
                if !edit(query) {
                    return;
                }
                *matches = transcript
                    .map(|transcript| find::find_matches(transcript, query, show_reasoning))
                    .unwrap_or_default();
                *current = 0;
            }
            _ => return,
        }
        if refresh_inspector {
            self.ensure_inspector_session();
        }
        self.show_match();
    }

    fn on_delegations_key(
        &mut self,
        key: KeyEvent,
        ctrl: bool,
        alt: bool,
        shift: bool,
        page: isize,
    ) {
        let Overlay::Delegations { focus, .. } = self.overlay else {
            return;
        };
        if focus != TaskFocus::Composer {
            self.sync_inspector_selection();
        }
        match focus {
            TaskFocus::Composer => match key.code {
                KeyCode::Enter if alt || ctrl => self.submit_inspector_composer(true),
                KeyCode::Char('j') if alt && ctrl => self.submit_inspector_composer(true),
                KeyCode::Enter if shift => self.task_inspector.composer.newline(),
                KeyCode::Char('j') if ctrl => self.task_inspector.composer.newline(),
                KeyCode::Enter => self.submit_inspector_composer(false),
                KeyCode::Esc => {
                    if let Overlay::Delegations { focus, .. } = &mut self.overlay {
                        *focus = TaskFocus::List;
                    }
                }
                KeyCode::Tab if self.task_inspector.composer.is_empty() => {
                    if let Overlay::Delegations { focus, .. } = &mut self.overlay {
                        *focus = TaskFocus::List;
                    }
                }
                KeyCode::PageUp => self.inspector_scroll_by(-page),
                KeyCode::PageDown => self.inspector_scroll_by(page),
                KeyCode::Char('o') if ctrl => {
                    self.tools_expanded = !self.tools_expanded;
                    self.inspector_pin_view();
                }
                _ => {
                    if self.inspected_can_send() {
                        super::edit_text(&mut self.task_inspector.composer, key);
                    }
                }
            },
            TaskFocus::Detail => match key.code {
                KeyCode::Esc | KeyCode::Left if !ctrl => {
                    if let Overlay::Delegations {
                        focus, detail_open, ..
                    } = &mut self.overlay
                    {
                        *focus = TaskFocus::List;
                        *detail_open = false;
                    }
                }
                KeyCode::Up | KeyCode::Char('k') if !ctrl => self.inspector_scroll_by(-3),
                KeyCode::Down | KeyCode::Char('j') if !ctrl => self.inspector_scroll_by(3),
                KeyCode::PageUp => self.inspector_scroll_by(-page),
                KeyCode::PageDown => self.inspector_scroll_by(page),
                KeyCode::Home => self.inspector_scroll_by(isize::MIN),
                KeyCode::End => self.inspector_scroll_by(isize::MAX),
                KeyCode::Char('o') if ctrl => {
                    self.tools_expanded = !self.tools_expanded;
                    self.inspector_pin_view();
                }
                KeyCode::Enter | KeyCode::Tab if !ctrl => {
                    if self.inspected_can_send()
                        && let Overlay::Delegations { focus, .. } = &mut self.overlay
                    {
                        *focus = TaskFocus::Composer;
                    }
                }
                KeyCode::Char('x') if ctrl => {
                    if let Some(epoch) = self.inspected_session().map(|session| session.epoch) {
                        self.interrupt_session(epoch);
                    }
                }
                KeyCode::Char('e') if ctrl => self.open_selected_delegation(),
                KeyCode::Char('y') if ctrl => self.copy_selected_delegation(),
                KeyCode::Char('g') if ctrl => self.goto_selected_delegation(),
                _ => {}
            },
            TaskFocus::List => match key.code {
                KeyCode::Esc => {
                    let narrow =
                        self.layout.viewport.width < crate::ui::views::tasks::MIN_SPLIT_WIDTH;
                    if let Overlay::Delegations { detail_open, .. } = &mut self.overlay
                        && narrow
                        && *detail_open
                    {
                        *detail_open = false;
                    } else {
                        self.overlay = Overlay::None;
                    }
                }
                KeyCode::Up => {
                    self.overlay_move(-1);
                    self.ensure_inspector_session();
                }
                KeyCode::Down => {
                    self.overlay_move(1);
                    self.ensure_inspector_session();
                }
                KeyCode::Right | KeyCode::Tab if !ctrl => {
                    let has_match = self.has_selected_delegation();
                    if has_match {
                        if let Overlay::Delegations {
                            focus, detail_open, ..
                        } = &mut self.overlay
                        {
                            *focus = TaskFocus::Detail;
                            *detail_open = true;
                        }
                        self.ensure_inspector_session();
                    }
                }
                KeyCode::Enter => {
                    let has_match = self.has_selected_delegation();
                    if has_match {
                        let next_focus = if self.inspected_can_send() {
                            TaskFocus::Composer
                        } else {
                            TaskFocus::Detail
                        };
                        if let Overlay::Delegations {
                            focus, detail_open, ..
                        } = &mut self.overlay
                        {
                            *focus = next_focus;
                            *detail_open = true;
                        }
                        self.ensure_inspector_session();
                    }
                }
                KeyCode::PageUp => self.inspector_scroll_by(-page),
                KeyCode::PageDown => self.inspector_scroll_by(page),
                KeyCode::Home => self.inspector_scroll_by(isize::MIN),
                KeyCode::End => self.inspector_scroll_by(isize::MAX),
                KeyCode::Char('u') if ctrl => {
                    if let Overlay::Delegations {
                        filter, selected, ..
                    } = &mut self.overlay
                    {
                        filter.clear();
                        *selected = 0;
                    }
                    self.ensure_inspector_session();
                }
                KeyCode::Char('o') if ctrl => {
                    self.tools_expanded = !self.tools_expanded;
                    self.inspector_pin_view();
                }
                KeyCode::Char('e') if ctrl => self.open_selected_delegation(),
                KeyCode::Char('y') if ctrl => self.copy_selected_delegation(),
                KeyCode::Char('g') if ctrl => self.goto_selected_delegation(),
                KeyCode::Backspace => self.overlay_filter_push(None),
                KeyCode::Char(ch) if !ctrl => self.overlay_filter_push(Some(ch)),
                _ => {}
            },
        }
    }

    fn has_selected_delegation(&self) -> bool {
        let Overlay::Delegations {
            filter, selected, ..
        } = &self.overlay
        else {
            return false;
        };
        let all = async_tasks(self);
        self.inspector_index(&matching_async_tasks(&all, filter), *selected)
            .is_some()
    }

    fn open_selected_delegation(&mut self) {
        let Overlay::Delegations {
            filter, selected, ..
        } = &self.overlay
        else {
            return;
        };
        let filter = filter.clone();
        let selected = *selected;
        let picked = {
            let all = async_tasks(self);
            let matched = matching_async_tasks(&all, &filter);
            self.inspector_index(&matched, selected)
                .and_then(|idx| matched[idx].open_target())
        };
        self.overlay = Overlay::None;
        match picked {
            Some((_, true)) => self.notice(
                crate::app::NoticeLevel::Info,
                "Child history is unavailable; its observed preview remains in Ctrl+B",
            ),
            Some((child, false)) => self.open_child(child),
            None => self.notice(
                crate::app::NoticeLevel::Info,
                "Origin Session is in earlier unloaded history; press Home to load it",
            ),
        }
    }

    fn copy_selected_delegation(&mut self) {
        let Overlay::Delegations {
            filter, selected, ..
        } = &self.overlay
        else {
            return;
        };
        let filter = filter.clone();
        let selected = *selected;
        let text = {
            let all = async_tasks(self);
            let matched = matching_async_tasks(&all, &filter);
            self.inspector_index(&matched, selected)
                .map(|idx| matched[idx].copy_text())
        };
        if let Some(text) = text.filter(|t| !t.trim().is_empty()) {
            self.copy(text);
        }
    }

    fn goto_selected_delegation(&mut self) {
        let Overlay::Delegations {
            filter, selected, ..
        } = &self.overlay
        else {
            return;
        };
        let filter = filter.clone();
        let selected = *selected;
        let origin = {
            let all = async_tasks(self);
            let matched = matching_async_tasks(&all, &filter);
            self.inspector_index(&matched, selected)
                .and_then(|idx| matched[idx].origin_target())
        };
        if let Some((parent, origin_ref)) = origin {
            self.overlay = Overlay::None;
            self.locate_agent_card(parent, origin_ref);
        } else {
            self.notice(
                crate::app::NoticeLevel::Info,
                "Origin dispatch is in earlier unloaded history",
            );
        }
    }

    pub(crate) fn on_overlay_accept(&mut self) -> Option<Action> {
        match std::mem::replace(&mut self.overlay, Overlay::None) {
            review @ (Overlay::Unconfirmed { .. }
            | Overlay::AgentDelete { .. }
            | Overlay::Confirm { .. }) => {
                self.overlay = review;
                None
            }
            Overlay::Sessions {
                selected,
                filter,
                rows,
                ..
            } => self.picker_sessions(&rows, &filter)
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
            Overlay::SessionInfo { .. } => {
                self.copy_session_id();
                None
            }
            Overlay::Palette { .. }
            | Overlay::Delegations { .. }
            | Overlay::Files { .. }
            | Overlay::Context { .. }
            | Overlay::Help { .. }
            | Overlay::Error { .. }
            | Overlay::Remote { .. }
            | Overlay::Diff(_)
            | Overlay::None => None,
        }
    }

    fn on_remote_key(&mut self, key: KeyEvent, ctrl: bool, page: isize) -> Option<Action> {
        match key.code {
            KeyCode::Esc => return Some(Action::RemoteStop),
            KeyCode::Char('q') if !ctrl => return Some(Action::RemoteStop),
            KeyCode::Char('c') if !ctrl => {
                if let Some(key_text) = self.remote.as_ref().map(|r| r.access_key.clone()) {
                    self.copy_with_label(key_text, "Access key copied");
                }
            }
            KeyCode::Char('u') if !ctrl => {
                if let Some(url) = self.remote.as_ref().map(|r| r.display_url.clone()) {
                    self.copy_with_label(url, "Address copied");
                }
            }
            KeyCode::Char('s') if !ctrl => {
                if let Overlay::Remote { show_key, .. } = &mut self.overlay {
                    *show_key = !*show_key;
                }
            }
            KeyCode::Char('r') if !ctrl => return Some(Action::RemoteRotateKey),
            KeyCode::Up | KeyCode::Char('k') if !ctrl => self.scroll_view(-1),
            KeyCode::Down | KeyCode::Char('j') if !ctrl => self.scroll_view(1),
            KeyCode::PageUp => self.scroll_view(-page),
            KeyCode::PageDown => self.scroll_view(page),
            KeyCode::Home => self.scroll_view(isize::MIN),
            KeyCode::End => self.scroll_view(isize::MAX),
            _ => {}
        }
        None
    }
}
