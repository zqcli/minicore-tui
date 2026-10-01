use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::{App, AppCommand, EditorCursor, LastClick, MousePress, MouseTarget, ScrollbarDrag};
use crate::state::composer::MAX_COMPOSER_BYTES;
use crate::state::selection::{Dock, NewSessionField};
use crate::state::session::SessionView;
use crate::state::tool::ToolKey;
use crate::state::transcript::TranscriptBlock;
use crate::state::view::SelectionGranularity;
use crate::state::view::{
    ConversationSelection, FoldOverride, PreparedConversation, SelectionPoint,
};

#[derive(Debug)]
pub(super) struct SelectionDrag {
    pub(super) session_id: String,
    pub(super) column: u16,
    pub(super) row: u16,
    pub(super) initial: Option<(SelectionPoint, SelectionPoint)>,
    pub(super) next_deadline: Instant,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct EditorSelection {
    pub(super) anchor: usize,
    pub(super) focus: usize,
    pub(super) dragged: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashCompletionState {
    pub source_revision: u64,
    pub session_owner: Option<String>,
    pub start: usize,
    pub end: usize,
    pub items: Vec<crate::command::MenuEntry>,
    pub group: Option<crate::command::CommandGroup>,
    /// Canonical command whose finite argument suggestions are displayed.
    pub argument_command: Option<&'static str>,
    pub filter: String,
    pub selected: usize,
}

impl SlashCompletionState {
    pub fn submits_literal(&self) -> bool {
        matches!(self.argument_command, Some("model" | "reasoning"))
    }

    pub fn visible_limit(&self) -> usize {
        if self.group.is_none() && self.filter.is_empty() {
            6
        } else {
            5
        }
    }
}

impl App {
    pub(super) fn composer_move(&mut self, direction: EditorCursor) -> Vec<AppCommand> {
        match direction {
            EditorCursor::Left => {
                self.composer_preferred_visual_col = None;
                self.composer.move_left();
            }
            EditorCursor::Right => {
                self.composer_preferred_visual_col = None;
                self.composer.move_right();
            }
            // History recall at the buffer edges (spec 22.2): up on the
            // first row, down on the last row.
            EditorCursor::Up => {
                if self.composer_is_on_first_visual_line()
                    && (self.composer.is_empty()
                        || self.composer.is_history_browsing()
                        || self.composer.cursor().1 == 0)
                {
                    self.composer_preferred_visual_col = None;
                    self.composer.history_prev();
                } else {
                    self.move_composer_vertically(-1);
                }
            }
            EditorCursor::Down => {
                if self.composer_is_on_last_visual_line() && self.composer.is_history_browsing() {
                    self.composer_preferred_visual_col = None;
                    self.composer.history_next();
                } else {
                    self.move_composer_vertically(1);
                }
            }
        }
        Vec::new()
    }

    fn composer_layout(&self) -> crate::ui::editor_layout::EditorLayout {
        let screen = crate::ui::layout::screen_layout(
            self,
            ratatui::layout::Rect {
                x: 0,
                y: 0,
                width: self.terminal_size.0,
                height: self.terminal_size.1,
            },
        );
        let width = crate::ui::rail::content_width(
            screen.panel.width as usize,
            crate::ui::rail::RAIL_WIDTH,
        );
        let display = self.composer.display_content();
        let lines = display.split('\n').map(str::to_owned).collect::<Vec<_>>();
        let markers = self.composer.display_paste_markers();
        let rows = crate::ui::editor_layout::EditorLayout::row_count_with_atomic_ranges(
            &lines, width, &markers,
        );
        crate::ui::editor_layout::EditorLayout::new_with_atomic_ranges(
            &lines,
            width,
            rows.max(1),
            self.composer.display_cursor(),
            &markers,
        )
    }

    fn composer_is_on_first_visual_line(&self) -> bool {
        self.composer_layout().cursor_row == 0
    }

    fn composer_is_on_last_visual_line(&self) -> bool {
        let layout = self.composer_layout();
        layout.cursor_row + 1 >= layout.rows.len()
    }

    fn move_composer_vertically(&mut self, delta: i32) {
        let layout = self.composer_layout();
        let current = layout.cursor_row as i32;
        let target = current + delta;
        if target < 0 || target >= layout.rows.len() as i32 {
            return;
        }
        let current = current as usize;
        let target = target as usize;
        let current_row = &layout.rows[current];
        let target_row = &layout.rows[target];
        let current_is_last = current + 1 >= layout.rows.len()
            || layout.rows[current + 1].logical_line != current_row.logical_line;
        let target_is_last = target + 1 >= layout.rows.len()
            || layout.rows[target + 1].logical_line != target_row.logical_line;
        let current_max = if current_is_last {
            current_row.width
        } else {
            current_row.width.saturating_sub(1)
        };
        let target_max = if target_is_last {
            target_row.width
        } else {
            target_row.width.saturating_sub(1)
        };
        let current_col = layout.cursor_col;
        let preferred = self.composer_preferred_visual_col;
        let desired = preferred.unwrap_or(current_col);
        let target_col = desired.min(target_max);
        if target_max < current_col && preferred.is_none() {
            self.composer_preferred_visual_col = Some(current_col);
        } else if (preferred.is_some() && target_max >= desired) || current_col < current_max {
            self.composer_preferred_visual_col = None;
        }
        let Some((line, column)) = layout.cursor_at(target, target_col) else {
            return;
        };
        self.composer.move_to_display(line, column);
    }

    /// Pasted text: CRLF/CR normalize to LF, inserted in one edit (no
    /// per-character events). Composer keeps the newlines; selector queries
    /// and new-session text fields flatten them (spec 43.7).
    pub(super) fn handle_paste(&mut self, text: String) -> Vec<AppCommand> {
        self.composer_preferred_visual_col = None;
        self.editor_selection = None;
        // Match the native editor's paste normalization: CRLF/CR become LF,
        // tabs become four spaces, and other control bytes are discarded.
        let mut normalized = text
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .replace('\t', "    ")
            .chars()
            .filter(|character| *character == '\n' || *character >= ' ')
            .collect::<String>();
        if matches!(self.dock, Dock::Composer)
            && matches!(normalized.chars().next(), Some('/' | '~' | '.'))
        {
            let (line, cursor) = self.composer.cursor();
            let previous = self
                .composer
                .lines()
                .get(line)
                .and_then(|current| current.chars().take(cursor).last());
            if previous
                .is_some_and(|character| character.is_ascii_alphanumeric() || character == '_')
            {
                normalized.insert(0, ' ');
            }
        }
        match &self.dock {
            Dock::Composer => {
                self.slash_completion = None;
                if !self.admit_draft_input(normalized.len()) {
                    return Vec::new();
                }
                if !self.composer_mut().insert_paste(&normalized) {
                    self.notice(
                        super::NoticeLevel::Warning,
                        format!("composer limit is {MAX_COMPOSER_BYTES} UTF-8 bytes"),
                    );
                }
            }
            Dock::NewSession(_) => {
                if !self.new_session().is_some_and(|draft| draft.submitting) {
                    self.field_insert(&normalized.replace('\n', ""));
                }
            }
            Dock::SessionSelector(_)
            | Dock::ModelSelector(_)
            | Dock::ReasoningSelector(_)
            | Dock::ProfileSelector(_) => {
                if matches!(
                    &self.dock,
                    Dock::SessionSelector(state)
                        if matches!(&state.mode, crate::state::selection::SessionPanelMode::Rename { .. })
                ) {
                    self.field_insert(&normalized.replace('\n', ""));
                    return Vec::new();
                }
                if self.selector_state().is_some_and(|state| state.submitting)
                    || matches!(
                        &self.dock,
                        Dock::SessionSelector(state)
                            if !matches!(&state.mode, crate::state::selection::SessionPanelMode::Browse)
                    )
                {
                    return Vec::new();
                }
                if let Dock::SessionSelector(state) = &mut self.dock {
                    state.query.push_str(&normalized.replace('\n', ""));
                    self.reconcile_session_selection(false);
                } else if let Some(state) = self.selector_state_mut() {
                    state.query.push_str(&normalized.replace('\n', ""));
                    state.cursor = 0;
                }
            }
            _ => {}
        }
        Vec::new()
    }

    pub(super) fn refresh_slash_completion(&mut self) {
        if !matches!(self.dock, Dock::Composer) {
            self.slash_completion = None;
            return;
        }
        let (line, cursor) = self.composer.cursor();
        if line != 0 {
            self.slash_completion = None;
            return;
        }
        let Some(text) = self.composer.lines().get(line) else {
            self.slash_completion = None;
            return;
        };
        if cursor != text.chars().count() || self.composer.lines().len() != 1 {
            self.slash_completion = None;
            return;
        }
        // Command completion applies only at the end of one command line.
        // The native command provider only returns command suggestions when
        // the text before the cursor starts at column zero. The parser still
        // accepts leading whitespace; completion intentionally follows the
        // editor provider rather than widening its trigger context.
        let prefix = text.chars().take(cursor).collect::<String>();
        let Some(candidate) = prefix.strip_prefix('/') else {
            self.slash_completion = None;
            return;
        };
        let content = self.composer.content();
        if self.slash_dismissed_text.as_ref() == Some(&content) {
            self.slash_completion = None;
            return;
        }
        self.slash_dismissed_text = None;
        let models = self
            .catalogs
            .models
            .iter()
            .map(|m| m.id.clone())
            .collect::<Vec<_>>();
        let model = self
            .active_view()
            .map(|v| v.info.model.as_str())
            .unwrap_or("");
        let reasoning = crate::state::selection::supported_reasoning(&self.catalogs.models, model)
            .iter()
            .filter_map(|level| {
                serde_json::to_value(level)
                    .ok()?
                    .as_str()
                    .map(str::to_owned)
            })
            .collect::<Vec<_>>();
        self.slash_completion =
            crate::command::menu::page(candidate, &models, &reasoning).map(|page| {
                SlashCompletionState {
                    source_revision: self.composer.editor_revision(),
                    session_owner: self.sessions.active.clone(),
                    start: 0,
                    end: cursor,
                    items: page.entries,
                    selected: 0,
                    group: page.group,
                    argument_command: page.argument_command,
                    filter: page.filter,
                }
            });
    }

    fn slash_completion_current(&self, completion: &SlashCompletionState) -> bool {
        matches!(self.dock, Dock::Composer)
            && completion.session_owner == self.sessions.active
            && completion.source_revision == self.composer.editor_revision()
            && self.composer.cursor() == (0, completion.end)
    }

    pub(super) fn discard_stale_slash_completion(&mut self) {
        if self
            .slash_completion
            .as_ref()
            .is_some_and(|completion| !self.slash_completion_current(completion))
        {
            self.slash_completion = None;
        }
    }

    pub(super) fn cancel_slash_completion(&mut self) {
        let Some(completion) = self.slash_completion.take() else {
            return;
        };
        if !self.slash_completion_current(&completion) {
            return;
        }
        if let Some(group) = completion.group {
            let replacement = if completion.filter.is_empty() {
                "/".to_owned()
            } else {
                format!("/{} ", group.name())
            };
            self.composer
                .replace_range(0, completion.start, completion.end, &replacement);
            self.refresh_slash_completion();
        } else {
            self.slash_dismissed_text = Some(self.composer.content());
        }
    }

    pub(super) fn move_slash_completion(&mut self, delta: i32) {
        let Some(completion) = self.slash_completion.as_mut() else {
            return;
        };
        let len = completion.items.len();
        if len == 0 {
            return;
        }
        completion.selected = (completion.selected as i32 + delta).rem_euclid(len as i32) as usize;
    }

    /// Returns `true` when completion still needs input rather than immediate
    /// submission (a required argument, or the existing skill-command path).
    pub(super) fn accept_slash_completion(&mut self) -> bool {
        let Some(completion) = self.slash_completion.take() else {
            return false;
        };
        if !self.slash_completion_current(&completion) {
            return true;
        }
        let Some(item) = completion.items.get(completion.selected) else {
            self.slash_completion = Some(completion);
            // Tab remains a no-op; Enter validates the current literal text.
            return false;
        };
        let (line, _) = self.composer.cursor();
        self.composer.replace_range(
            line,
            completion.start,
            completion.end,
            &format!("{} ", item.text),
        );
        let needs_input = item.needs_input();
        if needs_input && !item.text.starts_with("/skill:") {
            if let crate::command::MenuKind::Command(name) = item.kind {
                if let Some(spec) = crate::command::command_spec(name) {
                    self.notice(super::NoticeLevel::Info, spec.usage);
                }
            }
            self.refresh_slash_completion();
        }
        needs_input
    }

    pub(super) fn handle_mouse(&mut self, mouse: MouseEvent) -> Vec<AppCommand> {
        if let Some(commands) = self.context_mouse(mouse) {
            return commands;
        }
        if let Some(commands) = self.review_mouse(mouse) {
            return commands;
        }
        if let Some(commands) = self.workspace_mouse(mouse) {
            return commands;
        }
        if let Some(commands) = self.handle_tool_mouse(mouse) {
            return commands;
        }
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            && matches!(self.dock, Dock::Composer)
        {
            if self.async_layout && !self.transcript_input_ready() {
                if let Some(key) = self.displayed_tool_hit(mouse.column, mouse.row) {
                    return self.open_tool_detail(key);
                }
            }
            let screen = crate::ui::layout::screen_layout(
                self,
                ratatui::layout::Rect::new(0, 0, self.terminal_size.0, self.terminal_size.1),
            );
            if let Some(prepared) = self.prepared_conversation(screen.transcript.width) {
                let position = crate::ui::transcript::scroll_position(
                    self,
                    prepared.total_rows(),
                    screen.transcript.height as usize,
                );
                let key = crate::ui::tool_detail::detail_hits(
                    prepared,
                    screen.transcript,
                    position.offset,
                    position.visible_rows,
                )
                .into_iter()
                .find(|(hit, _)| hit.contains((mouse.column, mouse.row).into()))
                .map(|(_, key)| key);
                if let Some(key) = key {
                    return self.open_tool_detail(key);
                }
            }
        }
        if !self.scrollbar_allowed() && self.scrollbar_drag.is_some() {
            self.cancel_scrollbar_drag();
        }
        if self.scrollbar_drag.is_some() {
            match mouse.kind {
                MouseEventKind::Up(_) => {
                    self.finish_scrollbar_drag(mouse.row);
                    self.mouse_down = None;
                    self.update_scrollbar_hover(mouse.column, mouse.row);
                    return Vec::new();
                }
                MouseEventKind::Down(_) | MouseEventKind::Drag(_) => {
                    self.update_scrollbar_drag(mouse.row);
                    return Vec::new();
                }
                _ => {}
            }
        }
        self.update_scrollbar_hover(mouse.column, mouse.row);
        let wheel_step = if mouse
            .modifiers
            .contains(crossterm::event::KeyModifiers::ALT)
        {
            5
        } else {
            1
        };
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                self.clear_selection();
                self.panel_click = None;
                if self.selector_state().is_some() || self.session_selector_state().is_some() {
                    self.apply_action(super::Action::SelectorMove(-1))
                } else {
                    self.apply_action(super::Action::ScrollRows(-wheel_step))
                }
            }
            MouseEventKind::ScrollDown => {
                self.clear_selection();
                self.panel_click = None;
                if self.selector_state().is_some() || self.session_selector_state().is_some() {
                    self.apply_action(super::Action::SelectorMove(1))
                } else {
                    self.apply_action(super::Action::ScrollRows(wheel_step))
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                self.mouse_pressed_on_link = false;
                self.scrollbar_drag = None;
                self.mouse_down = None;
                self.update_scrollbar_hover(mouse.column, mouse.row);
                if self.session_selector_state().is_some() {
                    if self.session_panel_busy() {
                        self.panel_click = None;
                        self.clear_selection();
                        return Vec::new();
                    }
                    let area = ratatui::layout::Rect {
                        x: 0,
                        y: 0,
                        width: self.terminal_size.0,
                        height: self.terminal_size.1,
                    };
                    let panel_area = crate::ui::layout::screen_layout(self, area).panel;
                    let action = self.session_selector_state().and_then(|state| {
                        crate::ui::selector::session_action_at(
                            self,
                            panel_area,
                            state,
                            mouse.column,
                            mouse.row,
                        )
                    });
                    if let Some(action) = action {
                        self.mouse_down = Some(MousePress {
                            target: MouseTarget::SessionAction(action),
                            column: mouse.column,
                            row: mouse.row,
                        });
                        self.clear_selection();
                        return Vec::new();
                    }
                    let selected = self.session_selector_state().and_then(|state| {
                        crate::ui::selector::session_item_at(
                            self,
                            panel_area,
                            state,
                            mouse.column,
                            mouse.row,
                        )
                    });
                    if let Some(session_id) = selected {
                        let click_count = self.panel_click_count(&session_id);
                        if let Some(state) = self.session_selector_state_mut() {
                            state.selected_session_id = Some(session_id.clone());
                        }
                        self.mouse_down = Some(MousePress {
                            target: MouseTarget::SessionSelector {
                                session_id,
                                click_count,
                            },
                            column: mouse.column,
                            row: mouse.row,
                        });
                    } else {
                        self.panel_click = None;
                        self.clear_selection();
                    }
                    return Vec::new();
                }
                if self.selector_state().is_some() {
                    if self.selector_state().is_some_and(|state| state.submitting) {
                        self.selector_click = None;
                        self.clear_selection();
                        return Vec::new();
                    }
                    let area = ratatui::layout::Rect {
                        x: 0,
                        y: 0,
                        width: self.terminal_size.0,
                        height: self.terminal_size.1,
                    };
                    let hit = self.selector_state().and_then(|state| {
                        crate::ui::selector::selector_item_at(
                            self,
                            crate::ui::layout::screen_layout(self, area).panel,
                            state,
                            mouse.column,
                            mouse.row,
                        )
                    });
                    if let Some(hit) = hit {
                        let click_count = self.selector_click_count(hit.kind, &hit.key);
                        if let Some(state) = self.selector_state_mut() {
                            state.cursor = hit.index;
                        }
                        self.mouse_down = Some(MousePress {
                            target: MouseTarget::Selector {
                                kind: hit.kind,
                                key: hit.key,
                                click_count,
                            },
                            column: mouse.column,
                            row: mouse.row,
                        });
                    } else {
                        self.selector_click = None;
                        self.clear_selection();
                    }
                    return Vec::new();
                }
                if matches!(self.dock, Dock::NewSession(_)) {
                    let area = ratatui::layout::Rect {
                        x: 0,
                        y: 0,
                        width: self.terminal_size.0,
                        height: self.terminal_size.1,
                    };
                    let panel_area = crate::ui::layout::screen_layout(self, area).panel;
                    let field = self.new_session().and_then(|draft| {
                        crate::ui::new_session::field_at(panel_area, draft, mouse.column, mouse.row)
                    });
                    if let Some((field, cursor)) = field {
                        if let Dock::NewSession(draft) = &mut self.dock {
                            if !draft.submitting {
                                draft.field = field;
                                if let Some(cursor) = cursor {
                                    draft.field_cursor = cursor;
                                }
                            }
                        }
                        self.mouse_down = Some(MousePress {
                            target: MouseTarget::NewSessionField(field),
                            column: mouse.column,
                            row: mouse.row,
                        });
                    }
                    self.clear_selection();
                    return Vec::new();
                }
                if let Some((total, height)) = self.scroll_marker_hit(mouse.column, mouse.row) {
                    self.clear_selection();
                    self.set_transcript_offset(total.saturating_sub(height), total, height);
                    return Vec::new();
                }
                if self.begin_scrollbar_drag(mouse.column, mouse.row) {
                    self.clear_selection();
                    self.mouse_down = Some(MousePress {
                        target: MouseTarget::Scrollbar,
                        column: mouse.column,
                        row: mouse.row,
                    });
                    return Vec::new();
                }
                if self.earlier_history_hit(mouse.column, mouse.row) {
                    self.clear_selection();
                    return self.load_earlier_history();
                }
                if self.move_composer_cursor(mouse.column, mouse.row) {
                    self.focus = crate::state::panels::Focus::Editor;
                    self.clear_selection();
                    let point = self.composer_point_at(mouse.column, mouse.row);
                    let offset = point.map(|point| self.composer_display_offset(point));
                    if let Some(offset) = offset {
                        self.editor_selection = Some(EditorSelection {
                            anchor: offset,
                            focus: offset,
                            dragged: false,
                        });
                    }
                    self.mouse_down = Some(MousePress {
                        target: MouseTarget::Editor,
                        column: mouse.column,
                        row: mouse.row,
                    });
                    return Vec::new();
                }
                self.clear_selection();
                let Some(point) = self.conversation_point(mouse.column, mouse.row) else {
                    return Vec::new();
                };
                self.mouse_pressed_on_link = self.pressed_cell_is_link(mouse.column, mouse.row);
                let word = self.word_selection(point.clone());
                let click_count = self.click_count(
                    mouse.row,
                    word.anchor.column,
                    word.focus.column.saturating_add(1),
                );
                let selection = match click_count {
                    2 => word,
                    count if count >= 3 => self.paragraph_selection(point),
                    _ => ConversationSelection {
                        session_id: self.sessions.active.clone().unwrap_or_default(),
                        anchor: point.clone(),
                        focus: point,
                        granularity: SelectionGranularity::Character,
                        dragged: false,
                    },
                };
                self.selection = Some(selection);
                self.mouse_down = Some(MousePress {
                    target: MouseTarget::Conversation(
                        self.selection
                            .as_ref()
                            .map(|selection| selection.anchor.clone())
                            .expect("selection was just installed"),
                    ),
                    column: mouse.column,
                    row: mouse.row,
                });
                self.selection_drag = None;
                Vec::new()
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                let selecting = self
                    .mouse_down
                    .as_ref()
                    .is_some_and(|press| matches!(&press.target, MouseTarget::Conversation(_)));
                let initial = self
                    .selection_drag
                    .as_ref()
                    .and_then(|drag| drag.initial.clone())
                    .or_else(|| {
                        self.selection.as_ref().and_then(|selection| {
                            (selection.granularity != SelectionGranularity::Character)
                                .then(|| (selection.anchor.clone(), selection.focus.clone()))
                        })
                    });
                let now = self.instant_now();
                let direction = self.selection_drag_direction(mouse.row);
                let next_deadline = match self.selection_drag.as_ref() {
                    None => {
                        if direction == 0 {
                            now
                        } else {
                            now + Duration::from_millis(50)
                        }
                    }
                    Some(drag) => {
                        let previous_direction = self.selection_drag_direction(drag.row);
                        if direction == 0 {
                            now
                        } else if direction != previous_direction {
                            now + Duration::from_millis(50)
                        } else {
                            drag.next_deadline
                        }
                    }
                };
                self.selection_drag = selecting
                    .then(|| self.sessions.active.clone())
                    .flatten()
                    .map(|session_id| SelectionDrag {
                        session_id,
                        column: mouse.column,
                        row: mouse.row,
                        initial,
                        next_deadline,
                    });
                self.drag_mouse(mouse.column, mouse.row)
            }
            MouseEventKind::Moved => Vec::new(),
            MouseEventKind::Up(MouseButton::Left) => {
                let pressed = self.mouse_down.take();
                let drag_initial = self
                    .selection_drag
                    .as_ref()
                    .and_then(|drag| drag.initial.clone());
                self.selection_drag = None;
                match pressed {
                    Some(MousePress {
                        target: MouseTarget::Scrollbar,
                        ..
                    }) => {
                        self.finish_scrollbar_drag(mouse.row);
                        self.update_scrollbar_hover(mouse.column, mouse.row);
                    }
                    Some(MousePress {
                        target: MouseTarget::Conversation(_point),
                        column,
                        row,
                    }) => {
                        let same_position = column == mouse.column && row == mouse.row;
                        if !same_position {
                            let initial = drag_initial.or_else(|| {
                                self.selection
                                    .as_ref()
                                    .filter(|selection| {
                                        selection.granularity != SelectionGranularity::Character
                                    })
                                    .map(|selection| {
                                        (selection.anchor.clone(), selection.focus.clone())
                                    })
                            });
                            self.selection_drag = Some(SelectionDrag {
                                session_id: self.sessions.active.clone().unwrap_or_default(),
                                column: mouse.column,
                                row: mouse.row,
                                initial,
                                next_deadline: self.instant_now() + Duration::from_millis(50),
                            });
                            if let Some(point) = self.conversation_point(mouse.column, mouse.row) {
                                self.update_conversation_selection(point);
                            }
                            self.selection_drag = None;
                        }
                        let should_copy = self.selection.as_ref().is_some_and(|selection| {
                            selection.session_id == self.sessions.active.clone().unwrap_or_default()
                                && (!selection.is_empty() || selection.dragged)
                        });
                        if should_copy {
                            return self.copy_selection_command();
                        }
                        if same_position
                            && !self.mouse_pressed_on_link
                            && self
                                .selection
                                .as_ref()
                                .is_some_and(|selection| !selection.dragged)
                        {
                            self.toggle_section_at(mouse.column, mouse.row);
                        }
                    }
                    Some(MousePress {
                        target:
                            MouseTarget::SessionSelector {
                                session_id,
                                click_count,
                            },
                        column,
                        row,
                    }) => {
                        if column == mouse.column && row == mouse.row && click_count >= 2 {
                            let area = ratatui::layout::Rect {
                                x: 0,
                                y: 0,
                                width: self.terminal_size.0,
                                height: self.terminal_size.1,
                            };
                            let current_target = self.session_selector_state().and_then(|state| {
                                crate::ui::selector::session_item_at(
                                    self,
                                    crate::ui::layout::screen_layout(self, area).panel,
                                    state,
                                    mouse.column,
                                    mouse.row,
                                )
                            });
                            if current_target.as_deref() != Some(session_id.as_str()) {
                                self.panel_click = None;
                                return Vec::new();
                            }
                            if let Some(state) = self.session_selector_state_mut() {
                                state.selected_session_id = Some(session_id);
                            }
                            return self.confirm_session_selector();
                        }
                    }
                    Some(MousePress {
                        target:
                            MouseTarget::Selector {
                                kind,
                                key,
                                click_count,
                            },
                        column,
                        row,
                    }) => {
                        if column == mouse.column && row == mouse.row && click_count >= 2 {
                            let area = ratatui::layout::Rect {
                                x: 0,
                                y: 0,
                                width: self.terminal_size.0,
                                height: self.terminal_size.1,
                            };
                            let current = self.selector_state().and_then(|state| {
                                crate::ui::selector::selector_item_at(
                                    self,
                                    crate::ui::layout::screen_layout(self, area).panel,
                                    state,
                                    mouse.column,
                                    mouse.row,
                                )
                            });
                            if current
                                .as_ref()
                                .is_some_and(|hit| hit.kind == kind && hit.key == key)
                            {
                                return self.confirm_dock();
                            }
                        }
                    }
                    Some(MousePress {
                        target: MouseTarget::SessionAction(action),
                        column,
                        row,
                    }) => {
                        if column == mouse.column && row == mouse.row {
                            let area = ratatui::layout::Rect {
                                x: 0,
                                y: 0,
                                width: self.terminal_size.0,
                                height: self.terminal_size.1,
                            };
                            let current = self.session_selector_state().and_then(|state| {
                                crate::ui::selector::session_action_at(
                                    self,
                                    crate::ui::layout::screen_layout(self, area).panel,
                                    state,
                                    mouse.column,
                                    mouse.row,
                                )
                            });
                            if current == Some(action) {
                                return self.session_panel_action(action);
                            }
                        }
                    }
                    Some(MousePress {
                        target: MouseTarget::NewSessionField(field),
                        column,
                        row,
                    }) => {
                        if column == mouse.column
                            && row == mouse.row
                            && field == NewSessionField::Create
                        {
                            return self.confirm_dock();
                        }
                    }
                    Some(MousePress {
                        target: MouseTarget::Editor,
                        ..
                    }) if self
                        .editor_selection
                        .as_ref()
                        .is_some_and(|selection| selection.dragged) =>
                    {
                        return self.copy_editor_selection_command();
                    }
                    Some(MousePress {
                        target: MouseTarget::Editor,
                        ..
                    }) => {}
                    None => {}
                }
                Vec::new()
            }
            _ => {
                self.mouse_down = None;
                self.selection_drag = None;
                Vec::new()
            }
        }
    }

    pub(super) fn clear_selection(&mut self) {
        self.selection = None;
        self.selection_drag = None;
        self.editor_selection = None;
    }

    /// Continue a conversation selection while the pointer is held at the
    /// viewport edge. The pointer is retained in terminal coordinates; each
    /// tick commits at most one row and then resolves the new focus point
    /// through the same prepared geometry used by ordinary drag events.
    pub(super) fn auto_scroll_selection(&mut self) {
        let Some((drag_session_id, drag_column, drag_row, drag_next_deadline)) =
            self.selection_drag.as_ref().map(|drag| {
                (
                    drag.session_id.clone(),
                    drag.column,
                    drag.row,
                    drag.next_deadline,
                )
            })
        else {
            return;
        };
        if self.sessions.active.as_deref() != Some(drag_session_id.as_str()) {
            self.selection_drag = None;
            return;
        }
        let screen = crate::ui::layout::screen_layout(
            self,
            ratatui::layout::Rect {
                x: 0,
                y: 0,
                width: self.terminal_size.0,
                height: self.terminal_size.1,
            },
        );
        let direction = self.selection_drag_direction(drag_row);
        if direction == 0 {
            return;
        }
        let now = self.instant_now();
        if now < drag_next_deadline {
            return;
        }
        let (total, visible) = self.transcript_scroll_extent();
        let max_offset = total.saturating_sub(visible.max(1));
        let current_offset = self.active_view().map_or(0, |view| {
            if view.scroll.follow_tail {
                max_offset
            } else {
                view.scroll.offset
            }
        });
        if (direction < 0 && current_offset == 0) || (direction > 0 && current_offset >= max_offset)
        {
            self.selection_drag = None;
            return;
        }
        let before_scroll = self.active_view().map(|view| {
            (
                view.scroll.follow_tail,
                view.scroll.offset,
                self.viewport.0,
                self.viewport.1,
            )
        });
        self.transcript_scroll(direction);
        let after_scroll = self.active_view().map(|view| {
            (
                view.scroll.follow_tail,
                view.scroll.offset,
                self.viewport.0,
                self.viewport.1,
            )
        });
        if before_scroll == after_scroll {
            self.selection_drag = None;
            return;
        }
        if let Some(drag) = self.selection_drag.as_mut() {
            drag.next_deadline = now + Duration::from_millis(50);
        }

        let row = if direction < 0 {
            screen.transcript.y
        } else {
            screen.transcript.bottom().saturating_sub(1)
        };
        let column = drag_column.min(
            screen
                .content
                .x
                .saturating_add(screen.content.width.saturating_sub(1)),
        );
        if let Some(point) = self.conversation_point(column, row) {
            self.update_conversation_selection(point);
        }
    }

    pub(super) fn selection_drag_direction(&self, row: u16) -> i32 {
        let screen = crate::ui::layout::screen_layout(
            self,
            ratatui::layout::Rect {
                x: 0,
                y: 0,
                width: self.terminal_size.0,
                height: self.terminal_size.1,
            },
        );
        if row <= screen.transcript.y {
            -1
        } else if row >= screen.transcript.bottom().saturating_sub(1) {
            1
        } else {
            0
        }
    }

    fn click_count(&mut self, row: u16, word_start: usize, word_end: usize) -> u8 {
        let now = self.instant_now();
        let count = self
            .last_click
            .as_ref()
            .filter(|last| {
                last.row == row
                    && last.word_start == word_start
                    && last.word_end == word_end
                    && now.saturating_duration_since(last.at) <= Duration::from_millis(500)
            })
            .map_or(1, |last| last.count.saturating_add(1).min(3));
        self.last_click = Some(LastClick {
            row,
            at: now,
            count,
            word_start,
            word_end,
        });
        count
    }

    fn update_conversation_selection(&mut self, point: SelectionPoint) {
        let tail_offset = if self
            .active_view()
            .is_some_and(|view| view.scroll.follow_tail)
        {
            let screen = crate::ui::layout::screen_layout(
                self,
                ratatui::layout::Rect {
                    x: 0,
                    y: 0,
                    width: self.terminal_size.0,
                    height: self.terminal_size.1,
                },
            );
            let prepared = self.conversation_for_input(screen.content.width);
            let visible = crate::ui::transcript::visible_rows(
                self,
                prepared.total_rows(),
                screen.transcript.height,
            );
            prepared.total_rows().saturating_sub(visible)
        } else {
            0
        };
        let granularity = self
            .selection
            .as_ref()
            .map(|selection| selection.granularity);
        let next_points = match granularity {
            Some(SelectionGranularity::Word) => {
                let range = self.word_selection(point);
                let (start, end) = range.ordered_points();
                Some((start.clone(), end.clone()))
            }
            Some(SelectionGranularity::Paragraph) => {
                let range = self.paragraph_selection(point);
                let (start, end) = range.ordered_points();
                Some((start.clone(), end.clone()))
            }
            Some(SelectionGranularity::Character) => self
                .selection
                .as_ref()
                .map(|selection| (selection.anchor.clone(), point)),
            None => Some((point.clone(), point)),
        };
        let next_points = self
            .selection_drag
            .as_ref()
            .and_then(|drag| drag.initial.as_ref())
            .and_then(|(initial_anchor, initial_focus)| {
                let (initial_start, initial_end) = if (initial_anchor.row, initial_anchor.column)
                    <= (initial_focus.row, initial_focus.column)
                {
                    (initial_anchor, initial_focus)
                } else {
                    (initial_focus, initial_anchor)
                };
                let (range_start, range_end) = next_points.as_ref()?;
                Some(
                    if (range_start.row, range_start.column)
                        < (initial_start.row, initial_start.column)
                    {
                        (initial_end.clone(), range_start.clone())
                    } else {
                        (initial_start.clone(), range_end.clone())
                    },
                )
            })
            .or(next_points);
        if let Some(selection) = self.selection.as_mut() {
            if selection.session_id == self.sessions.active.clone().unwrap_or_default() {
                let (anchor, focus) = next_points.expect("conversation point exists");
                selection.anchor = anchor;
                selection.focus = focus;
                selection.dragged = true;
                if let Some(view) = self.active_session_mut() {
                    if view.scroll.follow_tail {
                        view.scroll.offset = tail_offset;
                    }
                    view.scroll.follow_tail = false;
                }
            }
        }
    }

    fn drag_mouse(&mut self, column: u16, row: u16) -> Vec<AppCommand> {
        let Some(press) = self.mouse_down.as_ref() else {
            return Vec::new();
        };
        match &press.target {
            MouseTarget::Scrollbar => {
                self.update_scrollbar_drag(row);
                Vec::new()
            }
            MouseTarget::Conversation(_) => {
                let Some(point) = self.conversation_point(column, row) else {
                    return Vec::new();
                };
                self.update_conversation_selection(point);
                Vec::new()
            }
            MouseTarget::SessionSelector { .. }
            | MouseTarget::Selector { .. }
            | MouseTarget::SessionAction(_)
            | MouseTarget::NewSessionField(_) => Vec::new(),
            MouseTarget::Editor => {
                let Some(point) = self.composer_point_at(column, row) else {
                    return Vec::new();
                };
                self.composer.move_to_display(point.0, point.1);
                let offset = self.composer_display_offset(point);
                if let Some(selection) = self.editor_selection.as_mut() {
                    selection.focus = offset;
                    selection.dragged = true;
                }
                Vec::new()
            }
        }
    }

    pub(super) fn copy_selection_command(&mut self) -> Vec<AppCommand> {
        let Some(selection) = self.selection.as_ref() else {
            return Vec::new();
        };
        let width = self.terminal_content_width();
        let prepared = self.conversation_for_input(width);
        let text = crate::ui::transcript::selection_text(&prepared, selection);
        if text.is_empty() {
            return Vec::new();
        }
        vec![self.capture_copy(text)]
    }

    pub(crate) fn terminal_content_width(&self) -> u16 {
        self.terminal_size
            .0
            .saturating_sub(crate::ui::rail::APP_GUTTER_WIDTH)
    }

    fn earlier_history_hit(&self, column: u16, row: u16) -> bool {
        if crate::ui::header::earlier_history_start(self).is_none() {
            return false;
        }
        let screen = crate::ui::layout::screen_layout(
            self,
            ratatui::layout::Rect::new(0, 0, self.terminal_size.0, self.terminal_size.1),
        );
        if !screen.transcript.contains((column, row).into()) {
            return false;
        }
        let prepared = self.conversation_for_input(screen.content.width);
        let position = crate::ui::transcript::scroll_position(
            self,
            prepared.total_rows(),
            screen.transcript.height as usize,
        );
        position.offset == 0
            && row == screen.transcript.y
            && position.visible_rows > 0
            && !self.transcript_overlay_at(screen.transcript, prepared.total_rows(), column, row)
    }

    fn conversation_point(&self, column: u16, row: u16) -> Option<SelectionPoint> {
        let area = ratatui::layout::Rect {
            x: 0,
            y: 0,
            width: self.terminal_size.0,
            height: self.terminal_size.1,
        };
        let screen = crate::ui::layout::screen_layout(self, area);
        if !screen.transcript.contains((column, row).into()) {
            return None;
        }
        let prepared = self.conversation_for_input(screen.content.width);
        let total = prepared.total_rows();
        let height = screen.transcript.height as usize;
        let position = crate::ui::transcript::scroll_position(self, total, height);
        let local_row = row.saturating_sub(screen.transcript.y) as usize;
        if local_row >= position.visible_rows
            || self.transcript_overlay_at(screen.transcript, total, column, row)
        {
            return None;
        }
        let logical_row = position.offset.saturating_add(local_row);
        let relative_column = column.saturating_sub(screen.content.x) as usize;
        let section = prepared.section_at(logical_row, relative_column)?;
        Some(SelectionPoint {
            row: logical_row,
            column: relative_column
                .max(section.content_columns.start)
                .min(section.content_columns.end.saturating_sub(1)),
            section_row: logical_row.saturating_sub(section.rows.start),
            section_id: Some(section.id.clone()),
        })
    }

    fn word_selection(&self, point: SelectionPoint) -> ConversationSelection {
        let width = self.terminal_content_width();
        let prepared = self.conversation_for_input(width);
        let Some(copy) = prepared
            .copy_ranges
            .iter()
            .find(|copy| copy.row == point.row)
        else {
            return ConversationSelection {
                session_id: self.sessions.active.clone().unwrap_or_default(),
                anchor: point.clone(),
                focus: point,
                granularity: SelectionGranularity::Word,
                dragged: false,
            };
        };
        let relative = point.column.saturating_sub(copy.columns.start);
        let (start, end) = word_cell_bounds(copy.text, relative);
        let mut anchor = point.clone();
        let mut focus = point.clone();
        anchor.column = copy.columns.start + start;
        focus.column = copy.columns.start + end.saturating_sub(1);
        ConversationSelection {
            session_id: self.sessions.active.clone().unwrap_or_default(),
            anchor,
            focus,
            granularity: SelectionGranularity::Word,
            dragged: false,
        }
    }

    /// RAIL-14 pressedUrl guard: a press on a rendered markdown link cell is
    /// recorded so the release cannot fold the containing section. Geometry
    /// comes from the same markdown layout pass that produced the drawn lines
    /// (`PreparedConversation::links_at`), so links inside inline code or
    /// bold runs are detected regardless of their foreground color, and no
    /// same-colored non-link text can produce a false positive.
    pub(crate) fn pressed_cell_is_link(&self, column: u16, row: u16) -> bool {
        let width = self.terminal_content_width();
        let area = ratatui::layout::Rect {
            x: 0,
            y: 0,
            width: self.terminal_size.0,
            height: self.terminal_size.1,
        };
        let screen = crate::ui::layout::screen_layout(self, area);
        if !screen.transcript.contains((column, row).into()) {
            return false;
        }
        let prepared = self.conversation_for_input(width);
        let total = prepared.total_rows();
        let height = screen.transcript.height as usize;
        let position = crate::ui::transcript::scroll_position(self, total, height);
        let local_row = row.saturating_sub(screen.transcript.y) as usize;
        if local_row >= position.visible_rows
            || self.transcript_overlay_at(screen.transcript, total, column, row)
        {
            return false;
        }
        let logical_row = position.offset.saturating_add(local_row);
        if logical_row >= prepared.total_rows() {
            return false;
        }
        let cell = column.saturating_sub(screen.content.x) as usize;
        prepared
            .links_at(logical_row)
            .iter()
            .any(|range| range.contains(&cell))
    }

    pub(super) fn paragraph_selection(&self, point: SelectionPoint) -> ConversationSelection {
        let width = self.terminal_content_width();
        let prepared = self.conversation_for_input(width);
        let mut start = point.row;
        let mut end = point.row;
        while start > 0 && copy_row_text(&prepared, start - 1).is_some_and(|text| !text.is_empty())
        {
            start -= 1;
        }
        while end + 1 < prepared.total_rows()
            && copy_row_text(&prepared, end + 1).is_some_and(|text| !text.is_empty())
        {
            end += 1;
        }
        let mut anchor = point.clone();
        let mut focus = point.clone();
        anchor.row = start;
        anchor.column = first_copy_column(&prepared, start);
        anchor.section_id = prepared
            .sections
            .iter()
            .find(|section| section.rows.contains(&start))
            .map(|section| section.id.clone());
        anchor.section_row = anchor
            .section_id
            .as_ref()
            .and_then(|id| prepared.sections.iter().find(|section| &section.id == id))
            .map_or(0, |section| start.saturating_sub(section.rows.start));
        focus.row = end;
        focus.column = copy_row_width(&prepared, end).saturating_sub(1);
        focus.section_id = prepared
            .sections
            .iter()
            .find(|section| section.rows.contains(&end))
            .map(|section| section.id.clone());
        focus.section_row = focus
            .section_id
            .as_ref()
            .and_then(|id| prepared.sections.iter().find(|section| &section.id == id))
            .map_or(0, |section| end.saturating_sub(section.rows.start));
        ConversationSelection {
            session_id: self.sessions.active.clone().unwrap_or_default(),
            anchor,
            focus,
            granularity: SelectionGranularity::Paragraph,
            dragged: false,
        }
    }

    pub(super) fn update_scrollbar_hover(&mut self, column: u16, row: u16) -> bool {
        if !self.scrollbar_allowed() {
            let changed = self.scrollbar.active;
            self.scrollbar = crate::ui::scrollbar::ScrollbarState::default();
            return changed;
        }
        if self.scrollbar_drag.is_some() {
            return false;
        }
        let active = if self.sessions.active.is_some()
            && column.checked_add(1) == Some(self.terminal_size.0)
        {
            let screen = crate::ui::layout::screen_layout(
                self,
                ratatui::layout::Rect::new(0, 0, self.terminal_size.0, self.terminal_size.1),
            );
            let (total, height) = self.transcript_scroll_extent();
            total > height && screen.transcript.contains((column, row).into())
        } else {
            false
        };
        self.scrollbar.set_active(active, self.instant_now())
    }

    fn scroll_marker_hit(&self, column: u16, row: u16) -> Option<(usize, usize)> {
        let screen = crate::ui::layout::screen_layout(
            self,
            ratatui::layout::Rect::new(0, 0, self.terminal_size.0, self.terminal_size.1),
        );
        let total = self
            .conversation_for_input(screen.content.width)
            .total_rows();
        self.marker_hit_in(screen.transcript, total, column, row)
            .then_some((total, screen.transcript.height as usize))
    }

    fn marker_hit_in(
        &self,
        area: ratatui::layout::Rect,
        total: usize,
        column: u16,
        row: u16,
    ) -> bool {
        let Some(view) = self.active_view() else {
            return false;
        };
        if view.scroll.follow_tail || total <= area.height as usize {
            return false;
        }
        let label = if view.scroll.new_content {
            "↓ new output"
        } else {
            "↑ scroll position"
        };
        crate::ui::transcript::marker_area(
            area,
            label,
            self.scrollbar_visible(total, area.height as usize),
        )
        .contains((column, row).into())
    }

    fn transcript_overlay_at(
        &self,
        area: ratatui::layout::Rect,
        total: usize,
        column: u16,
        row: u16,
    ) -> bool {
        self.marker_hit_in(area, total, column, row)
            || (self.scrollbar_visible(total, area.height as usize)
                && column.checked_add(1) == Some(area.right())
                && area.contains((column, row).into()))
    }

    pub(super) fn begin_scrollbar_drag(&mut self, column: u16, row: u16) -> bool {
        let area = ratatui::layout::Rect {
            x: 0,
            y: 0,
            width: self.terminal_size.0,
            height: self.terminal_size.1,
        };
        let screen = crate::ui::layout::screen_layout(self, area);
        if !screen.transcript.contains((column, row).into()) {
            return false;
        }
        let prepared = self.conversation_for_input(screen.content.width);
        let total = prepared.total_rows();
        let current =
            crate::ui::transcript::scroll_position(self, total, screen.transcript.height as usize)
                .offset;
        let Some(geometry) = crate::ui::scrollbar::geometry(screen.transcript, total, current)
        else {
            return false;
        };
        if column as usize != geometry.column
            || (row as usize) < geometry.track_top
            || (row as usize) >= geometry.track_top + geometry.track_height
        {
            return false;
        }
        let Some(session_id) = self.sessions.active.clone() else {
            return false;
        };
        let on_thumb = row as usize >= geometry.thumb_top
            && (row as usize) < geometry.thumb_top + geometry.thumb_height;
        self.scrollbar_drag = Some(ScrollbarDrag {
            session_id,
            grab_offset: if on_thumb {
                row as usize - geometry.thumb_top
            } else {
                geometry.thumb_height / 2
            },
        });
        self.scrollbar.set_active(true, self.instant_now());
        if !on_thumb {
            self.update_scrollbar_drag(row);
        }
        true
    }

    pub(super) fn update_scrollbar_drag(&mut self, row: u16) {
        let Some(drag) = self.scrollbar_drag.as_ref() else {
            return;
        };
        if self.sessions.active.as_deref() != Some(drag.session_id.as_str()) {
            self.cancel_scrollbar_drag();
            return;
        }
        let grab_offset = drag.grab_offset;
        let screen = crate::ui::layout::screen_layout(
            self,
            ratatui::layout::Rect::new(0, 0, self.terminal_size.0, self.terminal_size.1),
        );
        let prepared = self.conversation_for_input(screen.content.width);
        let total = prepared.total_rows();
        let height = screen.transcript.height as usize;
        let position = crate::ui::transcript::scroll_position(self, total, height);
        let Some(geometry) =
            crate::ui::scrollbar::geometry(screen.transcript, total, position.offset)
        else {
            self.cancel_scrollbar_drag();
            return;
        };
        let offset = crate::ui::scrollbar::scroll_top_at(geometry, row as usize, grab_offset);
        self.set_transcript_offset(offset, total, height);
    }

    pub(super) fn finish_scrollbar_drag(&mut self, _row: u16) {
        // Positions are applied during drag; release never remaps stale coordinates.
        self.scrollbar_drag = None;
        self.scrollbar.set_active(false, self.instant_now());
    }

    pub(super) fn cancel_scrollbar_drag(&mut self) {
        match &mut self.main_view {
            crate::state::panels::MainView::Conversation => {}
            crate::state::panels::MainView::Changes(s) => s.scrollbar_grab = None,
            crate::state::panels::MainView::Context(c) => c.scrollbar_grab = None,
            crate::state::panels::MainView::ToolDetail(detail) => detail.scrollbar_grab = None,
            crate::state::panels::MainView::FilePreview(file) => file.scrollbar_grab = None,
        }
        self.scrollbar_drag = None;
        self.scrollbar = crate::ui::scrollbar::ScrollbarState::default();
        self.mouse_down = None;
    }

    fn move_composer_cursor(&mut self, column: u16, row: u16) -> bool {
        let Some(point) = self.composer_point_at(column, row) else {
            return false;
        };
        self.composer.move_to_display(point.0, point.1);
        true
    }

    fn composer_point_at(&self, column: u16, row: u16) -> Option<(usize, usize)> {
        if !matches!(self.dock, Dock::Composer) {
            return None;
        }
        let area = ratatui::layout::Rect {
            x: 0,
            y: 0,
            width: self.terminal_size.0,
            height: self.terminal_size.1,
        };
        let screen = crate::ui::layout::screen_layout(self, area);
        if !screen.panel.contains((column, row).into()) {
            return None;
        }
        let content_width = crate::ui::rail::content_width(
            screen.panel.width as usize,
            crate::ui::rail::RAIL_WIDTH,
        );
        let display = self.composer.display_content();
        let display_lines = display.split('\n').map(str::to_owned).collect::<Vec<_>>();
        let editor_height = screen.panel.height.saturating_sub(
            crate::ui::layout::composer_completion_rows(self)
                .min(screen.panel.height.saturating_sub(1)),
        );
        let paste_markers = self.composer.display_paste_markers();
        let editor_layout = crate::ui::editor_layout::EditorLayout::new_with_atomic_ranges(
            &display_lines,
            content_width,
            editor_height.max(1) as usize,
            self.composer.display_cursor(),
            &paste_markers,
        );
        let local_row = row.saturating_sub(screen.panel.y) as usize;
        if local_row >= editor_height as usize {
            return None;
        }
        let local_column = column
            .saturating_sub(screen.panel.x)
            .saturating_sub(crate::ui::rail::RAIL_WIDTH as u16) as usize;
        editor_layout.cursor_at(local_row, local_column)
    }

    fn composer_display_offset(&self, point: (usize, usize)) -> usize {
        let display = self.composer.display_content();
        let mut offset = 0;
        for (line, text) in display.split('\n').enumerate() {
            if line == point.0 {
                return offset + point.1.min(text.chars().count());
            }
            offset += text.chars().count() + 1;
        }
        display.chars().count()
    }

    pub(crate) fn composer_selection_range(&self) -> Option<Range<usize>> {
        let selection = self.editor_selection?;
        if !selection.dragged || selection.anchor == selection.focus {
            return None;
        }
        let display = self.composer.display_content();
        let total = display.chars().count();
        let start = selection.anchor.min(selection.focus).min(total);
        let maximum = selection.anchor.max(selection.focus).min(total);
        let end = if maximum < total {
            let suffix = display.chars().skip(maximum).collect::<String>();
            maximum
                + suffix
                    .graphemes(true)
                    .next()
                    .map_or(1, |grapheme| grapheme.chars().count())
        } else {
            maximum
        };
        Some(start..end)
    }

    fn copy_editor_selection_command(&mut self) -> Vec<AppCommand> {
        let Some(range) = self.composer_selection_range() else {
            return Vec::new();
        };
        let text = self
            .composer
            .display_content()
            .chars()
            .skip(range.start)
            .take(range.end.saturating_sub(range.start))
            .collect::<String>();
        if text.is_empty() {
            return Vec::new();
        }
        vec![self.capture_copy(text)]
    }

    fn toggle_section_at(&mut self, column: u16, row: u16) {
        let area = ratatui::layout::Rect {
            x: 0,
            y: 0,
            width: self.terminal_size.0,
            height: self.terminal_size.1,
        };
        let screen = crate::ui::layout::screen_layout(self, area);
        if !screen.transcript.contains((column, row).into()) {
            return;
        }
        let prepared = self.conversation_for_input(screen.content.width);
        let total = prepared.total_rows();
        let height = screen.transcript.height as usize;
        let position = crate::ui::transcript::scroll_position(self, total, height);
        let local_row = row.saturating_sub(screen.transcript.y) as usize;
        if local_row >= position.visible_rows
            || self.transcript_overlay_at(screen.transcript, total, column, row)
        {
            return;
        }
        let logical_row = position.offset.saturating_add(local_row);
        let relative_column = column.saturating_sub(screen.content.x) as usize;
        let Some(section) = prepared.section_at(logical_row, relative_column) else {
            return;
        };
        if !section.collapsible {
            return;
        }
        let live_only = section.id.history_index.is_none();
        let id = section.id.clone();
        match id.kind {
            crate::state::view::SectionKind::Tool => {
                if let (Some(loop_id), Some(request_index), Some(tool_call_id)) = (
                    id.loop_id.as_deref(),
                    id.request_index,
                    id.tool_call_id.as_deref(),
                ) {
                    if let Some(view) = self.active_session_mut() {
                        let session_id = view.info.session_id.clone();
                        let key = ToolKey::new(&session_id, loop_id, request_index, tool_call_id);
                        let Some(current) = current_tool_expanded(view, &key) else {
                            return;
                        };
                        let expanded = !current;
                        Arc::make_mut(&mut view.tool_folds).insert(
                            key,
                            if expanded {
                                FoldOverride::Expanded
                            } else {
                                FoldOverride::Collapsed
                            },
                        );
                        view.scroll.follow_tail = false;
                        view.scroll.offset = position.offset;
                        view.scroll.new_content = false;
                        if !live_only {
                            view.transcript.invalidate();
                        }
                    }
                }
            }
            crate::state::view::SectionKind::Thinking => {
                if let (Some(loop_id), Some(request_index)) =
                    (id.loop_id.as_deref(), id.request_index)
                {
                    let key =
                        crate::state::view::ReasoningKey::new(loop_id, request_index, id.ordinal);
                    if let Some(view) = self.active_session_mut() {
                        let expanded = !view
                            .reasoning_folds
                            .get(&key)
                            .is_some_and(FoldOverride::expanded);
                        Arc::make_mut(&mut view.reasoning_folds).insert(
                            key,
                            if expanded {
                                FoldOverride::Expanded
                            } else {
                                FoldOverride::Collapsed
                            },
                        );
                        view.scroll.follow_tail = false;
                        view.scroll.offset = position.offset;
                        view.scroll.new_content = false;
                        view.transcript.invalidate();
                    }
                }
            }
            crate::state::view::SectionKind::Summary => {
                if let (Some(index), Some(view)) = (id.history_index, self.active_session_mut()) {
                    let expanded = !view
                        .summary_folds
                        .get(&index)
                        .is_some_and(FoldOverride::expanded);
                    Arc::make_mut(&mut view.summary_folds).insert(
                        index,
                        if expanded {
                            FoldOverride::Expanded
                        } else {
                            FoldOverride::Collapsed
                        },
                    );
                    view.scroll.follow_tail = false;
                    view.scroll.offset = position.offset;
                    view.scroll.new_content = false;
                    view.transcript.invalidate();
                }
            }
            crate::state::view::SectionKind::AssistantText
            | crate::state::view::SectionKind::User
            | crate::state::view::SectionKind::Notice => {}
        }
        if live_only {
            self.prepared_conversation = None;
        }
    }
}

pub(super) fn set_all_tools_expanded(view: &mut SessionView, expanded: bool) {
    view.tools_expanded = expanded;
    let keys = view
        .transcript
        .blocks
        .iter()
        .filter_map(|block| match block.as_ref() {
            TranscriptBlock::Tool(tool) => Some(ToolKey::new(
                &view.info.session_id,
                &tool.loop_id,
                tool.request_index,
                &tool.tool_call_id,
            )),
            _ => None,
        })
        .collect::<Vec<_>>();
    for block in view.transcript.blocks_mut() {
        let block = std::sync::Arc::make_mut(block);
        if let TranscriptBlock::Tool(tool) = &mut *block {
            tool.expanded = expanded;
        }
    }
    for key in keys {
        Arc::make_mut(&mut view.tool_folds).insert(
            key,
            if expanded {
                FoldOverride::Expanded
            } else {
                FoldOverride::Collapsed
            },
        );
    }
    view.transcript.invalidate();
}

fn copy_row_text(conversation: &PreparedConversation, row: usize) -> Option<&str> {
    conversation.copy_row(row).map(|copy| copy.text)
}

fn first_copy_column(conversation: &PreparedConversation, row: usize) -> usize {
    conversation
        .copy_row(row)
        .map_or(0, |copy| copy.columns.start)
}

fn copy_row_width(conversation: &PreparedConversation, row: usize) -> usize {
    conversation.copy_row(row).map_or(1, |copy| {
        copy.columns.start + UnicodeWidthStr::width(copy.text)
    })
}

pub fn word_cell_bounds(text: &str, target: usize) -> (usize, usize) {
    #[derive(Clone)]
    struct Segment<'a> {
        text: &'a str,
        start: usize,
        end: usize,
        word_like: bool,
        numeric: bool,
        joiner: bool,
    }

    let raw_segments = text.split_word_bound_indices().collect::<Vec<_>>();
    let mut segments = Vec::new();
    let mut cell_start = 0;
    let mut index = 0;
    while index < raw_segments.len() {
        let (byte, segment) = raw_segments[index];
        if segment.chars().all(is_cjk_dictionary_char) {
            let run_end = (index + 1..=raw_segments.len())
                .find(|&candidate| {
                    candidate == raw_segments.len()
                        || !raw_segments[candidate]
                            .1
                            .chars()
                            .all(is_cjk_dictionary_char)
                })
                .unwrap_or(raw_segments.len());
            let run_end_byte = raw_segments
                .get(run_end)
                .map_or(text.len(), |(byte, _)| *byte);
            let run = &text[byte..run_end_byte];
            // Real dictionary word segmentation for CJK runs (spec 7.3/14.3),
            // replacing the former cell-pair heuristic. ICU4X 2.1.2
            // `new_auto` reproduces the pinned Pi `Intl.Segmenter` output on
            // every oracle phrase (Chinese and Japanese); cells are derived
            // from the UTF-8 boundaries via the existing cell width mapping.
            for range in dictionary_word_ranges(run) {
                let start = cell_start + UnicodeWidthStr::width(&run[..range.start]);
                let end = cell_start + UnicodeWidthStr::width(&run[..range.end]);
                segments.push(Segment {
                    text: "",
                    start,
                    end,
                    word_like: true,
                    numeric: false,
                    joiner: false,
                });
            }
            cell_start += UnicodeWidthStr::width(run);
            index = run_end;
        } else {
            let end = cell_start + UnicodeWidthStr::width(segment);
            segments.push(Segment {
                text: segment,
                start: cell_start,
                end,
                word_like: segment.chars().any(char::is_alphanumeric),
                numeric: segment.chars().all(char::is_numeric),
                joiner: matches!(segment, "/" | "-"),
            });
            cell_start = end;
            index += 1;
        }
    }
    if segments.is_empty() {
        return (0, 0);
    }

    // Intl.Segmenter treats internal MidLetter/MidNum punctuation as part of
    // a word in cases such as `foo.bar`, `foo:bar`, `foo·bar`, and `1,234.5`.
    // unicode-segmentation exposes the UAX boundary pieces, so merge only
    // those well-defined internal cases before applying the Rail / and -
    // terminal-word joiners.
    let mut index = 1;
    while index + 1 < segments.len() {
        let left = &segments[index - 1];
        let middle = &segments[index];
        let right = &segments[index + 1];
        let internal = left.word_like
            && right.word_like
            && (matches!(middle.text, "." | ":" | "·")
                || (middle.text == "," && left.numeric && right.numeric));
        if internal {
            let end = right.end;
            let numeric = left.numeric && right.numeric && middle.text != ":" && middle.text != "·";
            segments[index - 1].end = end;
            segments[index - 1].numeric = numeric;
            segments.remove(index + 1);
            segments.remove(index);
        } else {
            index += 1;
        }
    }

    let selected = segments
        .iter()
        .enumerate()
        .find(|(_, segment)| {
            target < segment.end || (target == segment.start && segment.start == segment.end)
        })
        .map_or(segments.len() - 1, |(index, _)| index);
    let can_join = |left: &Segment<'_>, right: &Segment<'_>| {
        (left.word_like || left.joiner)
            && (right.word_like || right.joiner)
            && (left.joiner || right.joiner)
    };
    let mut first = selected;
    while first > 0 && can_join(&segments[first - 1], &segments[first]) {
        first -= 1;
    }
    let mut last = selected + 1;
    while last < segments.len() && can_join(&segments[last - 1], &segments[last]) {
        last += 1;
    }
    (
        segments[first].start,
        segments.get(last - 1).map_or(0, |segment| segment.end),
    )
}

/// A character that participates in a CJK dictionary word run: Han ideographs
/// plus Kana. These scripts have no spaces, so word boundaries come from the
/// ICU4X dictionary rather than UAX#29 letter runs.
fn is_cjk_dictionary_char(character: char) -> bool {
    matches!(
        character as u32,
        0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xf900..=0xfaff // Han
            | 0x3040..=0x309f // Hiragana
            | 0x30a0..=0x30ff // Katakana
    )
}

/// Lazily-built ICU4X word segmenter (spec 7.3/14.3). Pinned to
/// `icu_segmenter` 2.1.2, the 2.1.x line that shares the ICU provider
/// ecosystem already in this repository's lockfile. `new_auto` applies the
/// dictionary (and LSTM where the model is needed, e.g. Japanese kana
/// suffixes) per script; on Han runs it is real word segmentation, matching
/// the pinned Pi `Intl.Segmenter` output on every oracle phrase
/// (verified cell-by-cell by `tests/word_oracle.rs`).
fn han_dictionary_segmenter() -> &'static icu_segmenter::WordSegmenterBorrowed<'static> {
    static SEGMENTER: std::sync::OnceLock<icu_segmenter::WordSegmenterBorrowed<'static>> =
        std::sync::OnceLock::new();
    SEGMENTER.get_or_init(|| {
        icu_segmenter::WordSegmenter::new_auto(
            icu_segmenter::options::WordBreakInvariantOptions::default(),
        )
    })
}

/// UTF-8 byte ranges of the dictionary words covering `run` (a Han-only
/// substring). A lone character is a single one-word range.
fn dictionary_word_ranges(run: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut previous = 0usize;
    for boundary in han_dictionary_segmenter().segment_str(run) {
        if boundary > previous {
            ranges.push(previous..boundary);
        }
        previous = boundary;
    }
    if ranges.is_empty() {
        ranges.push(0..run.len());
    }
    ranges
}

pub(super) fn composer_move(app: &mut App, direction: super::EditorCursor) -> Vec<AppCommand> {
    app.composer_move(direction)
}

pub(super) fn handle_paste(app: &mut App, text: String) -> Vec<AppCommand> {
    app.handle_paste(text)
}

pub(super) fn refresh_slash_completion(app: &mut App) {
    app.refresh_slash_completion();
}

pub(super) fn move_slash_completion(app: &mut App, delta: i32) {
    app.move_slash_completion(delta);
}

pub(super) fn accept_slash_completion(app: &mut App) -> bool {
    app.accept_slash_completion()
}

pub(super) fn handle_mouse(app: &mut App, mouse: MouseEvent) -> Vec<AppCommand> {
    app.handle_mouse(mouse)
}

pub(super) fn clear_selection(app: &mut App) {
    app.clear_selection();
}

pub(super) fn auto_scroll_selection(app: &mut App) {
    app.auto_scroll_selection();
}

pub(super) fn cancel_scrollbar_drag(app: &mut App) {
    app.cancel_scrollbar_drag();
}

/// Handles the global Tool expansion action at the UI seam. RPC reducers do
/// not know about local fold policy or prepared geometry.
pub(super) fn toggle_tools(app: &mut App, session_id: &str) {
    if let Some(view) = app.sessions.known.get_mut(session_id) {
        let expanded = !view.tools_expanded;
        set_all_tools_expanded(view, expanded);
    }
}

/// Handles one Tool fold without letting the global expansion state override a
/// manual per-tool choice.
pub(super) fn toggle_tool(
    app: &mut App,
    session_id: &str,
    loop_id: &str,
    request_index: u32,
    tool_call_id: &str,
) {
    let Some(view) = app.sessions.known.get_mut(session_id) else {
        return;
    };
    let key = ToolKey::new(session_id, loop_id, request_index, tool_call_id);
    let Some(current) = current_tool_expanded(view, &key) else {
        return;
    };
    let has_durable_tool = view.transcript.blocks.iter().any(|block| {
        matches!(
            block.as_ref(),
            TranscriptBlock::Tool(tool)
                if tool.loop_id == loop_id
                    && tool.request_index == request_index
                    && tool.tool_call_id == tool_call_id
        )
    });
    let expanded = !current;
    for block in view.transcript.blocks_mut() {
        let block = std::sync::Arc::make_mut(block);
        if let TranscriptBlock::Tool(tool) = &mut *block {
            if tool.loop_id == loop_id
                && tool.request_index == request_index
                && tool.tool_call_id == tool_call_id
            {
                tool.expanded = expanded;
            }
        }
    }
    if let Some(live) = view.live.as_mut() {
        let matches_key = live.reference.as_ref().is_some_and(|reference| {
            reference.session_id == key.session_id && reference.loop_id == key.loop_id
        });
        if matches_key {
            if let Some(tool) = live
                .requests
                .iter_mut()
                .find(|request| request.request_index == request_index)
                .and_then(|request| {
                    request
                        .tools
                        .iter_mut()
                        .find(|tool| tool.tool_call_id == tool_call_id)
                })
            {
                tool.expanded = expanded;
            }
        }
    }
    Arc::make_mut(&mut view.tool_folds).insert(
        key,
        if expanded {
            FoldOverride::Expanded
        } else {
            FoldOverride::Collapsed
        },
    );
    if has_durable_tool {
        view.transcript.invalidate();
    } else {
        app.prepared_conversation = None;
    }
}

fn current_tool_expanded(view: &SessionView, key: &ToolKey) -> Option<bool> {
    if key.session_id != view.info.session_id {
        return None;
    }
    if let Some(expanded) = view
        .transcript
        .blocks
        .iter()
        .find_map(|block| match block.as_ref() {
            TranscriptBlock::Tool(tool)
                if tool.loop_id == key.loop_id
                    && tool.request_index == key.request_index
                    && tool.tool_call_id == key.tool_call_id =>
            {
                Some(crate::ui::transcript::effective_tool_expanded(view, tool))
            }
            _ => None,
        })
    {
        return Some(expanded);
    }

    let live = view.live.as_ref()?;
    let reference = live.reference.as_ref()?;
    if reference.session_id != key.session_id || reference.loop_id != key.loop_id {
        return None;
    }
    live.requests
        .iter()
        .find(|request| request.request_index == key.request_index)
        .and_then(|request| {
            request
                .tools
                .iter()
                .find(|tool| tool.tool_call_id == key.tool_call_id)
        })
        .map(|tool| crate::ui::transcript::effective_live_tool_expanded(view, key, tool))
}

pub(super) fn toggle_reasoning_section(
    app: &mut App,
    session_id: &str,
    loop_id: &str,
    request_index: u32,
    ordinal: u32,
) {
    let Some(view) = app.sessions.known.get_mut(session_id) else {
        return;
    };
    let key = crate::state::view::ReasoningKey::new(loop_id, request_index, ordinal);
    let expanded = view
        .reasoning_folds
        .get(&key)
        .is_none_or(|override_| !override_.expanded());
    Arc::make_mut(&mut view.reasoning_folds).insert(
        key,
        if expanded {
            FoldOverride::Expanded
        } else {
            FoldOverride::Collapsed
        },
    );
    view.transcript.invalidate();
}

#[cfg(test)]
mod tests {
    use super::word_cell_bounds;

    #[test]
    fn word_bounds_keep_internal_word_punctuation_together() {
        assert_eq!(word_cell_bounds("foo.bar", 1), (0, 7));
        assert_eq!(word_cell_bounds("foo:bar", 4), (0, 7));
        assert_eq!(word_cell_bounds("1,234.5", 3), (0, 7));
        assert_eq!(word_cell_bounds("foo=bar", 1), (0, 3));
        assert_eq!(word_cell_bounds("foo=bar", 5), (4, 7));
        assert_eq!(word_cell_bounds("foo/bar", 4), (0, 7));
        assert_eq!(word_cell_bounds("foo-bar", 4), (0, 7));
    }

    #[test]
    fn word_bounds_match_source_segmenter_for_emoji_combining_and_paths() {
        // Ground truth is the table produced by tools/reference_fixtures/cases/word_oracle.mts,
        // which runs the pinned pi-coding-agent 0.84.4 fullscreen
        // `getWordSelection` (real Intl.Segmenter + the pinned / and -
        // terminal-word joiners) on Node. tests/word_oracle.rs asserts every
        // cell of that table against this function. Emoji and ZWJ family
        // clusters are single non-word segments, combining marks stay inside
        // the word, and each path component is one selectable word.
        assert_eq!(word_cell_bounds("hello😀world", 1), (0, 5));
        assert_eq!(word_cell_bounds("hello😀world", 6), (5, 7));
        assert_eq!(word_cell_bounds("hello😀world", 8), (7, 12));
        assert_eq!(word_cell_bounds("👨‍👩‍👧family", 1), (0, 2));
        assert_eq!(word_cell_bounds("👨‍👩‍👧family", 6), (2, 8));
        assert_eq!(word_cell_bounds("cafe\u{301}test", 5), (0, 8));
        assert_eq!(word_cell_bounds("/usr/local/bin", 5), (0, 14));
        assert_eq!(word_cell_bounds("/usr/local/bin", 12), (0, 14));
        assert_eq!(word_cell_bounds("snake_case_var", 3), (0, 14));
        // "https" stays its own word (the ":" is not a joiner and is flanked
        // by a non-word slash, so no internal merge fires). The Rail path
        // joiner then merges the "/" run with the domain and path, exactly
        // like the pinned "foo/bar" -> whole word rule: clicking anywhere in
        // "//example.com/x" selects that whole joined run.
        assert_eq!(word_cell_bounds("https://example.com/x", 4), (0, 5));
        assert_eq!(word_cell_bounds("https://example.com/x", 5), (5, 6));
        assert_eq!(word_cell_bounds("https://example.com/x", 9), (6, 21));
    }

    #[test]
    fn word_bounds_group_cjk_runs_without_crossing_latin_boundaries() {
        // Intl.Segmenter groups CJK by dictionary word (RAIL-18), and the
        // model must never merge a Han run with neighboring Latin words.
        assert_eq!(word_cell_bounds("你好世界abc", 1), (0, 4));
        assert_eq!(word_cell_bounds("你好世界abc", 5), (4, 8));
        assert_eq!(word_cell_bounds("你好世界abc", 9), (8, 11));
        assert_eq!(word_cell_bounds("abc你好def", 1), (0, 3));
        assert_eq!(word_cell_bounds("abc你好def", 4), (3, 7));
        assert_eq!(word_cell_bounds("abc你好def", 8), (7, 10));
        assert_eq!(word_cell_bounds("你好,世界", 1), (0, 4));
        assert_eq!(word_cell_bounds("你好,世界", 7), (5, 9));
        assert_eq!(word_cell_bounds("abc-你好", 1), (0, 8));
        assert_eq!(word_cell_bounds("你", 1), (0, 2));
    }

    #[test]
    fn word_bounds_use_the_dictionary_not_a_cell_pair_heuristic() {
        // Real ICU4X dictionary segmentation matches the pinned Pi native
        // `Intl.Segmenter` for these phrases (each encoded here explicitly,
        // and every cell cross-checked against the native oracle fixture in
        // tests/word_oracle.rs). Chinese is 2-cell-per-char, so the old
        // cell-pair heuristic would have split 中华人民共和国 into 2-2-2-2-1;
        // the dictionary yields 中华/人民/共和国.
        let pair = |text: &str| {
            word_cell_bounds(text, 1) // a cell inside the first column
        };
        assert_eq!(pair("中华人民共和国"), (0, 4), "中华");
        assert_eq!(word_cell_bounds("中华人民共和国", 3), (0, 4), "中华");
        assert_eq!(word_cell_bounds("中华人民共和国", 5), (4, 8), "人民");
        assert_eq!(word_cell_bounds("中华人民共和国", 9), (8, 14), "共和国");
        assert_eq!(pair("计算机科学"), (0, 4), "计算");
        assert_eq!(word_cell_bounds("计算机科学", 5), (4, 6), "机");
        assert_eq!(word_cell_bounds("计算机科学", 7), (6, 10), "科学");
        assert_eq!(pair("南京市长江大桥"), (0, 6), "南京市");
        assert_eq!(word_cell_bounds("我们正在开发终端应用", 1), (0, 4), "我们");
        assert_eq!(word_cell_bounds("我们正在开发终端应用", 9), (8, 12), "终端");
        // Numbers split the CJK run and stay selectable on their own.
        assert_eq!(word_cell_bounds("我2004年在北京大学学习", 1), (0, 2), "我");
        assert_eq!(
            word_cell_bounds("我2004年在北京大学学习", 3),
            (2, 6),
            "2004"
        );
        assert_eq!(word_cell_bounds("我2004年在北京大学学习", 7), (6, 8), "年");
        assert_eq!(
            word_cell_bounds("我2004年在北京大学学习", 11),
            (10, 14),
            "北京"
        );
        // Japanese kana participates in the dictionary run (対象/です etc.).
        assert_eq!(
            word_cell_bounds("日本語のテキストです", 17),
            (16, 20),
            "です"
        );
        assert_eq!(
            word_cell_bounds("日本語漢字かな混じり", 11),
            (10, 14),
            "かな"
        );
        assert_eq!(
            word_cell_bounds("日本語漢字かな混じり", 15),
            (14, 20),
            "混じり"
        );
    }
}
