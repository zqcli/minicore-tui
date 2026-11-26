//! Shared two-pane master-detail workspace layout reused by `Ctrl+B` Async
//! Tasks and `/diff` Git Diff: searchable left list, focus-aware vertical
//! divider, right detail header + body, narrow-screen list ↔ detail toggle,
//! and unified mouse hit geometry.

use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::text::Line;

use crate::ui::chrome;
use crate::ui::theme::Styles;

/// Terminals narrower than this switch between list and detail instead of splitting.
pub const MIN_SPLIT_WIDTH: u16 = 96;
/// Terminals shorter than this also switch between list and detail.
pub const MIN_SPLIT_HEIGHT: u16 = 12;

/// Active pane inside a two-pane workspace (used by `/diff` and mirrored by `TaskFocus`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PaneFocus {
    #[default]
    List,
    Detail,
}

/// Recorded geometry of the last rendered split workspace for mouse hit-testing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SplitHitAreas {
    pub list_area: Rect,
    pub detail_area: Rect,
    pub list_first: usize,
    pub item_height: usize,
}

impl SplitHitAreas {
    pub fn reset(&mut self, item_height: usize) {
        self.list_area = Rect::default();
        self.detail_area = Rect::default();
        self.list_first = 0;
        self.item_height = item_height.max(1);
    }

    pub fn in_list(&self, col: u16, row: u16) -> bool {
        self.list_area.contains(Position::new(col, row))
    }

    pub fn in_detail(&self, col: u16, row: u16) -> bool {
        self.detail_area.contains(Position::new(col, row))
    }

    /// Maps a click inside the left pane's list rows (below the 2-row filter
    /// header) to its matched item index, if any.
    pub fn list_item_at(&self, col: u16, row: u16, matched_len: usize) -> Option<usize> {
        if !self.in_list(col, row) || matched_len == 0 {
            return None;
        }
        let list_y = self.list_area.y.saturating_add(2);
        if row < list_y {
            return None;
        }
        let slot = usize::from(row - list_y) / self.item_height.max(1);
        let index = self.list_first + slot;
        (index < matched_len).then_some(index)
    }
}

/// Computed pane rects for the current frame.
pub struct SplitRegions {
    pub list: Option<Rect>,
    pub detail: Option<Rect>,
}

pub fn is_split(width: u16, height: u16) -> bool {
    width >= MIN_SPLIT_WIDTH && height >= MIN_SPLIT_HEIGHT
}

/// Splits `body` into left list and right detail panes when the terminal is
/// wide/tall enough (drawing the focus-aware `│` divider), or returns the
/// single active pane on narrow terminals.
pub fn layout_panes(
    frame: &mut Frame,
    body: Rect,
    list_width: u16,
    detail_open: bool,
    detail_focused: bool,
    styles: Styles,
) -> SplitRegions {
    if body.is_empty() {
        return SplitRegions {
            list: None,
            detail: None,
        };
    }
    if is_split(body.width, body.height) {
        let list_w = list_width.min(body.width.saturating_sub(24));
        let list_area = Rect {
            width: list_w,
            ..body
        };
        let sep_x = body.x + list_w + 1;
        let detail_area = Rect {
            x: sep_x + 2,
            width: body.width.saturating_sub(list_w + 3),
            ..body
        };
        let sep_style = if detail_focused {
            styles.accent()
        } else {
            styles.card_rail()
        };
        let buf = frame.buffer_mut();
        for y in body.y..body.bottom() {
            buf.set_string(sep_x, y, "│", sep_style);
        }
        SplitRegions {
            list: Some(list_area),
            detail: Some(detail_area),
        }
    } else if detail_open {
        SplitRegions {
            list: None,
            detail: Some(body),
        }
    } else {
        SplitRegions {
            list: Some(body),
            detail: None,
        }
    }
}

/// Draws the 2-row right-pane header (title + focus-colored `───`) and returns
/// the content area below it.
pub fn draw_detail_header(
    frame: &mut Frame,
    detail_area: Rect,
    header: &Line<'static>,
    focused: bool,
    styles: Styles,
) -> Rect {
    if detail_area.is_empty() {
        return Rect::default();
    }
    let buf = frame.buffer_mut();
    buf.set_line(detail_area.x, detail_area.y, header, detail_area.width);
    if detail_area.height == 1 {
        return Rect::default();
    }
    chrome::draw_rule(
        buf,
        detail_area.x,
        detail_area.y + 1,
        detail_area.width,
        focused,
        styles,
    );
    Rect {
        x: detail_area.x,
        y: detail_area.y + 2,
        width: detail_area.width,
        height: detail_area.height.saturating_sub(2),
    }
}

/// Computes `(first_index, visible_slots)` for a scrolled list window.
pub fn list_window(
    selected: usize,
    total: usize,
    list_height: usize,
    item_height: usize,
) -> (usize, usize) {
    let slots = (list_height / item_height.max(1)).max(1);
    let clamped = selected.min(total.saturating_sub(1));
    let first = clamped.saturating_sub(slots.saturating_sub(1));
    (first, slots)
}
