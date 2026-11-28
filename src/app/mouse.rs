//! Mouse: wheel scrolling, the scrollbar, click-to-open, and drag, word or
//! line selection that copies on release.

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use super::{App, DOUBLE_CLICK, Overlay, Panel, Press, Scroll};
use super::{scrollbar, selection};
use crate::transcript::{Block, Grouping, Transcript};

impl App {
    /// Mouse: wheel scrolling, the scrollbar, click-to-open, and drag, word or
    /// line selection that copies on release. Returns whether the screen may
    /// have changed; plain pointer motion elsewhere does not.
    pub fn on_mouse(&mut self, mouse: MouseEvent) -> bool {
        let cell = (mouse.column, mouse.row);
        // Single-click selects and double-click confirms on any bottom sheet
        // (Sessions, Models, Agents, Reasoning, Approval, Unconfirmed) and on
        // the floating `/` command and `@` file semi-popups.
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            && self.sheet_hit.contains(cell.0, cell.1)
        {
            self.composer_dragging = false;
            return self.click_sheet(cell);
        }
        let uncertain_edit = matches!(
            self.overlay,
            Overlay::Unconfirmed {
                mode: super::unconfirmed::ReviewMode::Edit(_),
                ..
            }
        );
        let inspector_editor =
            matches!(self.overlay, Overlay::Delegations { .. }) && self.inspected_can_send();
        let editor = (inspector_editor
            || ((!self.has_interaction()
                || uncertain_edit
                || self.resident.current().is_some_and(|session| {
                    session
                        .interaction
                        .as_ref()
                        .is_some_and(|pending| pending.is_text())
                }))
                && matches!(
                    self.overlay,
                    Overlay::None
                        | Overlay::Palette { .. }
                        | Overlay::Unconfirmed {
                            mode: super::unconfirmed::ReviewMode::Edit(_),
                            ..
                        }
                )))
            && (self.fatal.is_none() || uncertain_edit)
            && !self.composer_area.is_empty();
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left)
                if editor
                    && self
                        .composer_area
                        .contains(ratatui::layout::Position::new(cell.0, cell.1)) =>
            {
                if let Overlay::Delegations {
                    focus, detail_open, ..
                } = &mut self.overlay
                {
                    *focus = super::overlay::TaskFocus::Composer;
                    *detail_open = true;
                }
                self.press = None;
                self.selection = None;
                self.scrollbar_grab = None;
                self.place_composer_cursor(cell, mouse.modifiers.contains(KeyModifiers::SHIFT));
                self.composer_dragging = true;
                return true;
            }
            MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left)
                if self.composer_dragging =>
            {
                if editor {
                    self.place_composer_cursor(cell, true);
                }
                // Like a transcript selection, a draft one is copied on release.
                if matches!(mouse.kind, MouseEventKind::Up(_)) {
                    self.composer_dragging = false;
                    if let Some(text) = self.active_editor().selected_text() {
                        self.copy(text.to_owned());
                    }
                }
                return true;
            }
            MouseEventKind::Down(_) => {
                self.composer_dragging = false;
                self.active_editor_mut().clear_selection();
            }
            _ => {}
        }
        // A full-screen view hides the transcript the press would land on.
        let view = self.overlay.is_view();
        match mouse.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let step = if mouse.modifiers.contains(KeyModifiers::ALT) {
                    5
                } else {
                    1
                };
                let delta = if mouse.kind == MouseEventKind::ScrollUp {
                    -step
                } else {
                    step
                };
                // The transcript scrolls only while it is the visible surface.
                match self.overlay {
                    Overlay::None | Overlay::Find { .. } => self.scroll_by(delta),
                    Overlay::Delegations { .. } => {
                        if self.task_inspector.hit.in_list(cell.0, cell.1) {
                            self.overlay_move(delta.signum());
                            self.ensure_inspector_session();
                        } else {
                            self.inspector_scroll_by(delta);
                        }
                    }
                    Overlay::Diff(ref diff) => {
                        if diff.hit.in_list(cell.0, cell.1) {
                            self.overlay_move(delta.signum());
                        } else {
                            self.scroll_view(delta);
                        }
                    }
                    Overlay::Error { .. }
                    | Overlay::Context { .. }
                    | Overlay::SessionInfo { .. }
                    | Overlay::Remote { .. } => self.scroll_view(delta),
                    _ => self.overlay_move(delta.signum()),
                }
                true
            }
            MouseEventKind::Down(MouseButton::Left)
                if matches!(self.overlay, Overlay::Delegations { .. }) =>
            {
                if self.task_inspector.hit.in_detail(cell.0, cell.1)
                    && self
                        .task_inspector
                        .layout
                        .viewport
                        .area()
                        .contains(cell.into())
                {
                    if let Overlay::Delegations {
                        focus, detail_open, ..
                    } = &mut self.overlay
                    {
                        *focus = super::overlay::TaskFocus::Detail;
                        *detail_open = true;
                    }
                    self.press_at(cell);
                    true
                } else {
                    self.press = None;
                    self.selection = None;
                    self.click_task_inspector(cell);
                    true
                }
            }
            MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left)
                if matches!(self.overlay, Overlay::Delegations { .. })
                    && self.press.as_ref().is_some_and(|press| {
                        self.task_inspector
                            .hit
                            .in_detail(press.cell.0, press.cell.1)
                            && self
                                .task_inspector
                                .layout
                                .viewport
                                .area()
                                .contains(press.cell.into())
                    }) =>
            {
                self.extend_selection(cell);
                if matches!(mouse.kind, MouseEventKind::Up(_)) {
                    if let Some(drag) = self.selection {
                        self.copy_selection(drag);
                    } else if self.press.as_ref().is_some_and(|press| press.clicks == 1)
                        && self.click_task_inspector(cell)
                    {
                        self.press = None;
                    }
                }
                true
            }
            MouseEventKind::Down(MouseButton::Left) if matches!(self.overlay, Overlay::Diff(_)) => {
                self.click_diff_view(cell)
            }
            _ if view => false,
            MouseEventKind::Moved => {
                let on_thumb = self.scrollbar_thumb(cell).is_some();
                if on_thumb {
                    self.scrolled_at = Some(tokio::time::Instant::now());
                }
                on_thumb
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some((start, len)) = self.scrollbar_thumb(cell) {
                    let at = (cell.1 - self.layout.viewport.origin.1) as usize;
                    // A press on the track jumps the thumb's centre there.
                    let grab = match (start..start + len).contains(&at) {
                        true => at - start,
                        false => len / 2,
                    };
                    self.scrollbar_grab = Some(grab);
                    self.drag_scrollbar(cell.1, grab);
                } else {
                    self.press_at(cell);
                }
                true
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(grab) = self.scrollbar_grab {
                    self.drag_scrollbar(cell.1, grab);
                } else {
                    self.extend_selection(cell);
                }
                true
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if self.scrollbar_grab.take().is_some() {
                    self.scrolled_at = Some(tokio::time::Instant::now());
                    return true;
                }
                self.extend_selection(cell);
                if let Some(drag) = self.selection {
                    self.copy_selection(drag);
                } else if self
                    .press
                    .as_ref()
                    .is_some_and(|press| press.clicks == 1 && press.cell == cell)
                    && self.click_transcript(cell)
                {
                    // A click that toggled or followed a transcript control does
                    // not start a double-click word selection.
                    self.press = None;
                }
                true
            }
            _ => false,
        }
    }

    fn place_composer_cursor(&mut self, cell: (u16, u16), extend: bool) {
        let area = self.composer_area;
        let row = (cell.1 as isize - area.y as isize).clamp(-1, area.height as isize);
        let column = cell
            .0
            .saturating_sub(area.x)
            .min(area.width.saturating_sub(1));
        let at = crate::ui::composer::hit_test_row(
            self.active_editor(),
            area.width,
            self.composer_max_rows,
            row,
            column,
        );
        self.active_editor_mut().place_cursor(at, extend);
    }

    /// A left press: a click, or the second or third of one on the same word,
    /// which selects the word or the whole row.
    fn press_at(&mut self, cell: (u16, u16)) {
        let layout = self.selection_layout();
        let area = layout.viewport.area();
        let text = layout.row_text(cell.1).unwrap_or_default();
        let word = cell
            .0
            .checked_sub(area.x)
            .and_then(|column| selection::word_at(&text, column));
        let now = tokio::time::Instant::now();
        let clicks = match &self.press {
            Some(last)
                if word.is_some()
                    && last.word == word
                    && last.cell.1 == cell.1
                    && now - last.at <= DOUBLE_CLICK =>
            {
                last.clicks % 3 + 1
            }
            _ => 1,
        };
        let cells = match clicks {
            2 => word.clone(),
            3 => Some(0..crate::wrap::width(text.trim_end()) as u16),
            _ => None,
        };
        self.selection = cells
            .filter(|cells| !cells.is_empty())
            .map(|cells| selection::Drag {
                from: (area.x + cells.start, cell.1),
                to: (area.x + cells.end - 1, cell.1),
            });
        self.press = Some(Press {
            cell,
            at: now,
            clicks,
            word,
        });
    }

    /// Moves a drag's moving end to `cell`; a word or line selection keeps the
    /// range its click picked.
    fn extend_selection(&mut self, cell: (u16, u16)) {
        let Some(press) = &self.press else {
            return;
        };
        self.selection = match press.clicks {
            1 if press.cell == cell => None,
            1 => Some(selection::Drag {
                from: press.cell,
                to: cell,
            }),
            _ => self.selection,
        };
    }

    /// The thumb's rows when `cell` is on the transcript's scrollbar column
    /// and there is something to scroll.
    fn scrollbar_thumb(&self, (column, row): (u16, u16)) -> Option<(usize, usize)> {
        let viewport = self.layout.viewport;
        let area = viewport.area();
        let on_bar = column + 1 == area.right() && row >= area.y && row < area.bottom();
        on_bar
            .then(|| scrollbar::thumb(viewport.height, self.layout.total_rows(), viewport.top))
            .flatten()
    }

    /// Scrolls so the held thumb, `grab` rows below its top, follows the pointer.
    fn drag_scrollbar(&mut self, row: u16, grab: usize) {
        let viewport = self.layout.viewport;
        let total = self.layout.total_rows();
        let Some((_, len)) = scrollbar::thumb(viewport.height, total, viewport.top) else {
            return;
        };
        let first = row as isize - viewport.origin.1 as isize - grab as isize;
        self.scroll_to(scrollbar::thumb_top(viewport.height, total, len, first));
    }

    /// Whether `cell` is on the jump label, which shows while the view is
    /// detached from the tail.
    fn on_jump_label(&self, (column, row): (u16, u16)) -> bool {
        let viewport = self.layout.viewport;
        let area = viewport.area();
        viewport.top + viewport.height < self.layout.total_rows()
            && row + 1 == area.bottom()
            && column
                .checked_sub(area.x)
                .is_some_and(|column| scrollbar::jump_label(area.width).contains(&column))
    }

    /// Toggles cards and thinking, follows the tail, or opens the Git diff.
    /// Returns whether the click was handled by an interactive target.
    fn click_transcript(&mut self, (column, row): (u16, u16)) -> bool {
        if !self.overlay.is_none() {
            return false;
        }
        if self.on_jump_label((column, row)) {
            self.scroll = Scroll::Follow;
            return true;
        }
        let Some((index, line)) = self.layout.hit(column, row) else {
            return false;
        };
        // Block 0's rows start with the welcome header.
        let header = if index == 0 {
            self.layout.state.header_rows
        } else {
            0
        };
        if line < header {
            return false;
        }
        let all_expanded = self.tools_expanded;
        let text = self
            .layout
            .row_text(row)
            .unwrap_or_default()
            .trim()
            .to_owned();
        let height = self.layout.content_height(index);
        let async_target = self.resident.current().and_then(|session| {
            match session.transcript.blocks().get(index)? {
                Block::Tool(call) if call.agent_card.is_some() => {
                    let card = call.agent_card.as_deref()?;
                    Some(card.key.child.session_id.to_string())
                }
                Block::Tool(call) if call.is_delegation_receipt() => {
                    let assoc = call.delegations.first()?;
                    Some(assoc.child.session_id.to_string())
                }
                Block::User {
                    kind: crate::transcript::UserKind::Delegated,
                    text,
                    ..
                } => crate::ui::delegated::summary(text).map(|summary| summary.session_id),
                _ => None,
            }
        });
        // Clicking a delegated dispatch row (folded or on its footer) or a
        // completion banner opens the Ctrl+B Async Tasks Inspector focused on
        // that child Session.
        let open_inspector = async_target.filter(|_| {
            let is_group_head = self.resident.current().is_some_and(|session| {
                matches!(
                    session.transcript.grouping(index),
                    crate::transcript::Grouping::Head { .. }
                ) && line == header
            });
            !is_group_head
                && self.resident.current().is_some_and(|session| {
                    match session.transcript.blocks().get(index) {
                        Some(Block::User {
                            kind: crate::transcript::UserKind::Delegated,
                            ..
                        }) => true,
                        Some(Block::Tool(call)) if call.agent_card.is_some() => {
                            !call.is_expanded(all_expanded) || line + 2 == height
                        }
                        Some(Block::Tool(call)) if call.is_delegation_receipt() => {
                            line + 1 == height
                        }
                        _ => false,
                    }
                })
        });
        if let Some(session_id) = open_inspector {
            self.open_delegation_for(&session_id);
            return true;
        }
        let following = matches!(self.scroll, Scroll::Follow);
        let Some(session) = self.resident.current_mut() else {
            return false;
        };
        if let Some(keep_follow) = toggle_fold(
            &mut session.transcript,
            index,
            line == header,
            &text,
            all_expanded,
            following,
        ) {
            if !keep_follow {
                self.pin_view();
            }
            return true;
        }
        match session.transcript.blocks().get(index) {
            Some(Block::User {
                kind: crate::transcript::UserKind::Delegated,
                ..
            }) => {
                if let Some(Block::User { expanded, .. }) = session.transcript.block_mut(index) {
                    let open = !expanded.unwrap_or(all_expanded);
                    *expanded = (open != all_expanded).then_some(open);
                    self.pin_view();
                    return true;
                }
            }
            Some(Block::TurnEnd {
                turn_id,
                reason: Some(reason),
                ..
            }) if !reason.is_empty() => {
                self.overlay = Overlay::Error {
                    text: format!(
                        "Agent: {}\nSession: {}\nTurn: {turn_id}\n\n{reason}",
                        session.info.agent_id, session.info.id
                    ),
                    scroll: 0,
                    visible_rows: 0,
                };
                return true;
            }
            Some(Block::TurnEnd {
                changes: Some(changes),
                ..
            }) if !changes.is_empty() => {
                self.open_panel(Panel::Diff);
                return true;
            }
            _ => {}
        }
        false
    }

    /// Copies the selection that just ended; it stays highlighted until the
    /// next press, key or scroll.
    fn copy_selection(&mut self, drag: selection::Drag) {
        if let Some(text) = self
            .selected_text(drag)
            .filter(|text| !text.trim().is_empty())
        {
            self.copy(text);
        }
    }

    /// Handles left clicks on any bottom sheet or floating `/` / `@` completion
    /// popup: single-click highlights a row; double-clicking the same row
    /// confirms/runs it (preventing accidental switches during long tasks).
    fn click_sheet(&mut self, (column, row): (u16, u16)) -> bool {
        let Some(index) = self.sheet_hit.row_at(column, row) else {
            self.sheet_click = None;
            return true;
        };
        let now = tokio::time::Instant::now();
        let confirmed = self
            .sheet_click
            .is_some_and(|(prev, at)| prev == index && now - at <= DOUBLE_CLICK);
        self.sheet_click = if confirmed { None } else { Some((index, now)) };

        match &mut self.overlay {
            Overlay::Palette { selected }
            | Overlay::Files { selected, .. }
            | Overlay::Sessions { selected, .. }
            | Overlay::Models { selected, .. }
            | Overlay::Agents { selected, .. }
            | Overlay::AgentDelete { selected, .. }
            | Overlay::Reasoning { selected } => *selected = index,
            Overlay::Unconfirmed { selected, .. } => *selected = index,
            Overlay::None => {
                if let Some(session) = self.resident.current_mut()
                    && let Some(interaction) = session.interaction.as_mut()
                    && !interaction.submitted
                {
                    let last = interaction.options().len().saturating_sub(1);
                    interaction.selected = index.min(last);
                }
            }
            _ => return true,
        }

        if !confirmed {
            return true;
        }
        match self.overlay {
            Overlay::Palette { .. } | Overlay::Files { .. } => {
                if let Some(action) = self.accept_completion() {
                    self.on_action(action);
                }
            }
            Overlay::Sessions { .. }
            | Overlay::Models { .. }
            | Overlay::Agents { .. }
            | Overlay::Reasoning { .. } => {
                if let Some(action) = self.on_overlay_accept() {
                    self.on_action(action);
                }
            }
            Overlay::None => {
                let answer = self.resident.current().and_then(|session| {
                    let interaction = session.interaction.as_ref()?;
                    if interaction.submitted || now < session.armed_at {
                        return None;
                    }
                    let approval = matches!(
                        interaction.request(),
                        minicore_host_core::InteractionRequestData::Approval { .. }
                    );
                    Some(if approval {
                        if index == 0 {
                            super::Answer::Allow
                        } else {
                            super::Answer::Deny
                        }
                    } else {
                        super::Answer::Choice(index)
                    })
                });
                if let Some(answer) = answer {
                    self.on_action(super::Action::Answer(answer));
                }
            }
            _ => {}
        }
        true
    }

    /// Handles left clicks inside the `/diff` split workspace using the shared
    /// [`crate::ui::views::split::SplitHitAreas`] geometry.
    fn click_diff_view(&mut self, (column, row): (u16, u16)) -> bool {
        use crate::ui::views::split::PaneFocus;
        let Overlay::Diff(diff) = &mut self.overlay else {
            return false;
        };
        if diff.hit.in_list(column, row) {
            diff.focus = PaneFocus::List;
            let matched_len = diff.matching_indices().len();
            if let Some(idx) = diff.hit.list_item_at(column, row, matched_len) {
                let changed = idx != diff.selected;
                diff.detail_open = true;
                if changed {
                    diff.select(idx);
                    self.load_selected_diff();
                }
            }
            return true;
        }
        if diff.hit.in_detail(column, row) {
            diff.focus = PaneFocus::Detail;
            diff.detail_open = true;
            return true;
        }
        false
    }

    /// Handles left clicks inside the `Ctrl+B` Async Tasks Inspector:
    /// selecting rows in the left list, focusing the right child conversation,
    /// jumping to tail, or toggling `Working`/`Worked`/tool/thinking folds.
    /// Plain text returns false so successive clicks can select a word or row.
    fn click_task_inspector(&mut self, (column, row): (u16, u16)) -> bool {
        if self.task_inspector.hit.in_list(column, row) {
            let matched_len = if let Overlay::Delegations { filter, .. } = &self.overlay {
                let all = super::overlay::async_tasks(self);
                super::overlay::matching_async_tasks(&all, filter).len()
            } else {
                0
            };
            let picked = self
                .task_inspector
                .hit
                .list_item_at(column, row, matched_len);
            if let Overlay::Delegations {
                selected, focus, ..
            } = &mut self.overlay
            {
                *focus = super::overlay::TaskFocus::List;
                if let Some(idx) = picked {
                    *selected = idx;
                }
            }
            self.ensure_inspector_session();
            return true;
        }
        if !self.task_inspector.hit.in_detail(column, row) {
            return false;
        }
        if let Overlay::Delegations {
            focus, detail_open, ..
        } = &mut self.overlay
        {
            *focus = super::overlay::TaskFocus::Detail;
            *detail_open = true;
        }
        if self.on_inspector_jump_label((column, row)) {
            self.task_inspector.scroll = Scroll::Follow;
            return true;
        }
        let Some((index, line)) = self.task_inspector.layout.hit(column, row) else {
            return false;
        };
        let all_expanded = self.tools_expanded;
        let text = self
            .task_inspector
            .layout
            .row_text(row)
            .unwrap_or_default()
            .trim()
            .to_owned();
        let following = matches!(self.task_inspector.scroll, Scroll::Follow);
        let Some(session) = self.inspected_session_mut() else {
            return false;
        };
        if let Some(keep_follow) = toggle_fold(
            &mut session.transcript,
            index,
            line == 0,
            &text,
            all_expanded,
            following,
        ) {
            if !keep_follow {
                self.inspector_pin_view();
            }
            return true;
        }
        false
    }

    fn on_inspector_jump_label(&self, (column, row): (u16, u16)) -> bool {
        let viewport = self.task_inspector.layout.viewport;
        let area = viewport.area();
        viewport.top + viewport.height < self.task_inspector.layout.total_rows()
            && row + 1 == area.bottom()
            && column
                .checked_sub(area.x)
                .is_some_and(|col| scrollbar::jump_label(area.width).contains(&col))
    }
}

/// The pane decides whether to pin its view; the transcript owns the fold.
fn toggle_fold(
    transcript: &mut Transcript,
    index: usize,
    header: bool,
    text: &str,
    all_expanded: bool,
    following: bool,
) -> Option<bool> {
    match transcript.grouping(index) {
        Grouping::Head { summary, open, .. }
            if header || (!open && !all_expanded && summary.live) =>
        {
            let keep_follow = following && summary.live && !*open;
            transcript.toggle_group(index);
            return Some(keep_follow);
        }
        Grouping::Tail {
            summary: Some(_), ..
        } if header => {
            transcript.toggle_group(index);
            return Some(false);
        }
        _ => {}
    }
    match transcript.blocks().get(index) {
        Some(Block::Tool(call)) if call.is_expanded(all_expanded) || !text.is_empty() => {
            if let Some(Block::Tool(call)) = transcript.block_mut(index) {
                call.toggle_expanded(all_expanded);
                return Some(false);
            }
        }
        Some(Block::Assistant { .. })
            if text.starts_with(crate::ui::rails::RAIL) || text.starts_with('∴') =>
        {
            if let Some(Block::Assistant { expanded, .. }) = transcript.block_mut(index) {
                let open = !expanded.unwrap_or(all_expanded);
                *expanded = (open != all_expanded).then_some(open);
                return Some(false);
            }
        }
        _ => {}
    }
    None
}
