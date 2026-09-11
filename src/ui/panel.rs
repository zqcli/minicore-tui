//! Shared geometry for dock panels.
//!
//! A panel owns one bordered rectangle and divides its inner area into the
//! same title, header, query, content, and footer rows for rendering and
//! pointer hit-testing. The individual panels still own their content and
//! state; this module only removes duplicated geometry and window math.

use std::ops::Range;

use ratatui::Frame;
use ratatui::layout::{Margin, Rect};
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Block, Paragraph};

use crate::theme::Theme;

/// Fixed rows owned by the non-content parts of a dock panel.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PanelSpec {
    pub title_rows: u16,
    pub header_rows: u16,
    pub query: bool,
    pub footer_rows: u16,
}

impl PanelSpec {
    pub const fn new(header_rows: u16, query: bool, footer_rows: u16) -> Self {
        Self {
            title_rows: 1,
            header_rows,
            query,
            footer_rows,
        }
    }

    pub const fn without_title(header_rows: u16, query: bool, footer_rows: u16) -> Self {
        Self {
            title_rows: 0,
            header_rows,
            query,
            footer_rows,
        }
    }
}

/// The complete panel geometry. Every consumer should use these rectangles
/// rather than deriving an inner row from the outer area independently.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PanelLayout {
    pub outer: Rect,
    pub inner: Rect,
    pub title: Rect,
    pub header: Rect,
    pub query: Option<Rect>,
    pub content: Rect,
    pub footer: Rect,
}

impl PanelLayout {
    /// Converts an absolute terminal row into a content-row index.
    pub fn content_row(&self, row: u16) -> Option<usize> {
        self.content
            .contains((self.content.x, row).into())
            .then(|| usize::from(row.saturating_sub(self.content.y)))
    }
}

/// Splits `area` into the shared panel regions.
pub fn layout(area: Rect, spec: PanelSpec) -> PanelLayout {
    let inner = area.inner(Margin::new(1, 1));
    let mut y = inner.y;
    let title = take_rows(inner, &mut y, spec.title_rows);
    let header = take_rows(inner, &mut y, spec.header_rows);
    let query = spec.query.then(|| take_rows(inner, &mut y, 1));

    let remaining = inner.height.saturating_sub(y.saturating_sub(inner.y));
    let footer_height = spec.footer_rows.min(remaining);
    let content_height = remaining.saturating_sub(footer_height);
    let content = Rect::new(inner.x, y, inner.width, content_height);
    y = y.saturating_add(content_height);
    let footer = Rect::new(inner.x, y, inner.width, footer_height);

    PanelLayout {
        outer: area,
        inner,
        title,
        header,
        query,
        content,
        footer,
    }
}

/// Draws the common rounded border.
pub fn render_frame(frame: &mut Frame, panel: PanelLayout, theme: &Theme) {
    frame.render_widget(
        Block::bordered().border_style(Style::new().fg(theme.border_accent)),
        panel.outer,
    );
}

/// Draws a line window in one of the shared panel regions.
pub fn render_window(frame: &mut Frame, area: Rect, lines: &[Line<'static>], scroll: usize) {
    let height = area.height as usize;
    let window: Vec<Line<'static>> = lines.iter().skip(scroll).take(height).cloned().collect();
    frame.render_widget(Paragraph::new(window), area);
}

/// Keeps the selected item visible while fitting complete variable-height
/// items into `cap` rows. The returned range is an item range, not a row
/// range, so renderers and hit-test callers can share it directly.
pub fn visible_window(heights: &[usize], cursor: usize, cap: usize) -> Range<usize> {
    let n = heights.len();
    if n == 0 || cap == 0 {
        return 0..0;
    }
    let cursor = cursor.min(n - 1);
    let mut start = cursor;
    loop {
        let end = fit_rows(heights, start, cap);
        if start == 0 {
            return start..end;
        }
        let previous = start - 1;
        if rows_in(heights, previous, end) <= cap {
            start = previous;
        } else {
            return start..end;
        }
    }
}

fn take_rows(inner: Rect, y: &mut u16, requested: u16) -> Rect {
    let consumed = y.saturating_sub(inner.y);
    let height = requested.min(inner.height.saturating_sub(consumed));
    let rect = Rect::new(inner.x, *y, inner.width, height);
    *y = (*y).saturating_add(height);
    rect
}

fn fit_rows(heights: &[usize], start: usize, cap: usize) -> usize {
    let mut rows = 0;
    let mut index = start;
    while index < heights.len() && rows + heights[index] <= cap {
        rows += heights[index];
        index += 1;
    }
    index
}

fn rows_in(heights: &[usize], start: usize, end: usize) -> usize {
    heights[start..end.min(heights.len())].iter().sum()
}
