//! Mouse: wheel scrolling, the scrollbar, click-to-open, and drag, word or
//! line selection that copies on release.

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use super::{App, DOUBLE_CLICK, Overlay, Panel, Press, Scroll};
use super::{scrollbar, selection};
use crate::transcript::Block;
use crate::ui::blocks;

impl App {
    /// Mouse: wheel scrolling, the scrollbar, click-to-open, and drag, word or
    /// line selection that copies on release. Returns whether the screen may
    /// have changed; plain pointer motion elsewhere does not.
    pub fn on_mouse(&mut self, mouse: MouseEvent) -> bool {
        let cell = (mouse.column, mouse.row);
        let editor = !self.has_interaction()
            && self.fatal.is_none()
            && matches!(
                self.overlay,
                Overlay::None | Overlay::Palette { .. } | Overlay::Files { .. }
            )
            && !self.composer_area.is_empty();
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left)
                if editor
                    && self
                        .composer_area
                        .contains(ratatui::layout::Position::new(cell.0, cell.1)) =>
            {
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
                    if let Some(text) = self.composer.selected_text() {
                        self.copy(text.to_owned());
                    }
                }
                return true;
            }
            MouseEventKind::Down(_) => {
                self.composer_dragging = false;
                self.composer.clear_selection();
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
                    Overlay::Diff(_) | Overlay::Tool(_) => self.scroll_view(delta),
                    _ => self.overlay_move(delta.signum()),
                }
                true
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
                {
                    self.click_transcript(cell);
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
            &self.composer,
            area.width,
            self.composer_max_rows,
            row,
            column,
        );
        self.composer.place_cursor(at, extend);
    }

    /// A left press: a click, or the second or third of one on the same word,
    /// which selects the word or the whole row.
    fn press_at(&mut self, cell: (u16, u16)) {
        let area = self.layout.viewport.area();
        let text = self.layout.row_text(cell.1).unwrap_or_default();
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

    /// A click: the jump label follows the tail again, a card or a thinking row
    /// folds that block, a card's full-output hint opens the tool view, and a
    /// turn-end line opens the turn's diff.
    fn click_transcript(&mut self, (column, row): (u16, u16)) {
        if !self.overlay.is_none() {
            return;
        }
        if self.on_jump_label((column, row)) {
            self.scroll = Scroll::Follow;
            return;
        }
        let Some(index) = self.layout.hit(column, row) else {
            return;
        };
        let all_expanded = self.tools_expanded;
        let text = self
            .layout
            .row_text(row)
            .unwrap_or_default()
            .trim()
            .to_owned();
        let Some(session) = self.session.as_mut() else {
            return;
        };
        match session.transcript.blocks().get(index) {
            Some(Block::Tool(call)) if text.ends_with(blocks::FULL_OUTPUT_HINT) => {
                let key = call.key.clone();
                self.open_panel(Panel::Tool(key));
            }
            Some(Block::Tool(call)) if !text.is_empty() => {
                let key = call.key.clone();
                if let Some(call) = session.transcript.find_tool_mut(&key) {
                    call.toggle_expanded(all_expanded);
                    self.pin_view();
                }
            }
            Some(Block::Assistant { .. }) if text.starts_with(crate::ui::rails::RAIL) => {
                if let Some(Block::Assistant { expanded, .. }) = session.transcript.block_mut(index)
                {
                    let open = !expanded.unwrap_or(all_expanded);
                    *expanded = (open != all_expanded).then_some(open);
                    self.pin_view();
                }
            }
            Some(Block::TurnEnd { .. }) => self.open_panel(Panel::Diff(super::DiffScope::Turn)),
            _ => {}
        }
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
}
