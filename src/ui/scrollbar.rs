//! The conversation-only Rail scrollbar.
//!
//! The transcript remains the owner of scroll state. This module only maps a
//! committed or drag-preview offset to a thumb and paints that thumb over the
//! existing Ratatui buffer; the transparent track never overwrites content.

use ratatui::Frame;
use ratatui::layout::Rect;

use crate::theme::Theme;

pub const DRAG_ANIMATION: std::time::Duration = std::time::Duration::from_millis(90);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScrollbarGeometry {
    pub column: usize,
    pub track_top: usize,
    pub track_height: usize,
    pub thumb_top: usize,
    pub thumb_height: usize,
    pub max_thumb_start: usize,
    pub max_scroll_top: usize,
}

pub fn geometry(
    area: Rect,
    total_rows: usize,
    visible_rows: usize,
    scroll_top: usize,
) -> Option<ScrollbarGeometry> {
    let track_height = area.height as usize;
    let visible_rows = visible_rows.max(1);
    if area.width == 0 || track_height == 0 || total_rows <= visible_rows {
        return None;
    }
    let thumb_height = ((track_height * track_height) as f64 / total_rows as f64)
        .round()
        .max(2.0)
        .min(track_height as f64) as usize;
    let max_thumb_start = track_height.saturating_sub(thumb_height);
    let max_scroll_top = total_rows.saturating_sub(visible_rows);
    let thumb_offset = thumb_offset(max_scroll_top, max_thumb_start, scroll_top);
    Some(ScrollbarGeometry {
        column: area.x as usize + area.width as usize - 1,
        track_top: area.y as usize,
        track_height,
        thumb_top: area.y as usize + thumb_offset.min(max_thumb_start),
        thumb_height,
        max_thumb_start,
        max_scroll_top,
    })
}

pub fn thumb_top_for_scroll(geometry: ScrollbarGeometry, scroll_top: usize) -> usize {
    geometry.track_top
        + thumb_offset(
            geometry.max_scroll_top,
            geometry.max_thumb_start,
            scroll_top,
        )
}

fn thumb_offset(max_scroll_top: usize, max_thumb_start: usize, scroll_top: usize) -> usize {
    if max_scroll_top == 0 {
        0
    } else {
        ((scroll_top.min(max_scroll_top) as f64 / max_scroll_top as f64) * max_thumb_start as f64)
            .round() as usize
    }
}

pub fn scroll_top_at(geometry: ScrollbarGeometry, pointer_y: usize, grab_offset: usize) -> usize {
    let thumb_offset = pointer_y
        .saturating_sub(geometry.track_top)
        .saturating_sub(grab_offset)
        .min(geometry.max_thumb_start);
    if geometry.max_thumb_start == 0 {
        0
    } else {
        ((thumb_offset as f64 / geometry.max_thumb_start as f64) * geometry.max_scroll_top as f64)
            .round() as usize
    }
}

pub fn render(
    frame: &mut Frame<'_>,
    area: Rect,
    total_rows: usize,
    visible_rows: usize,
    scroll_top: usize,
    theme: &Theme,
) {
    render_with_thumb(
        frame,
        area,
        total_rows,
        visible_rows,
        scroll_top,
        theme,
        None,
    );
}

pub fn render_with_thumb(
    frame: &mut Frame<'_>,
    area: Rect,
    total_rows: usize,
    visible_rows: usize,
    scroll_top: usize,
    theme: &Theme,
    thumb_top: Option<usize>,
) {
    let Some(geometry) = geometry(area, total_rows, visible_rows, scroll_top) else {
        return;
    };
    let thumb_top = thumb_top.unwrap_or(geometry.thumb_top);
    for row in thumb_top..thumb_top + geometry.thumb_height {
        if row >= area.y as usize + area.height as usize {
            break;
        }
        if let Some(cell) = frame
            .buffer_mut()
            .cell_mut((geometry.column as u16, row as u16))
        {
            cell.set_symbol("█");
            cell.set_fg(theme.rail_editor);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[test]
    fn geometry_matches_the_pinned_reference_dimensions() {
        let area = Rect::new(0, 0, 80, 20);
        assert_eq!(
            geometry(area, 100, 20, 0),
            Some(ScrollbarGeometry {
                column: 79,
                track_top: 0,
                track_height: 20,
                thumb_top: 0,
                thumb_height: 4,
                max_thumb_start: 16,
                max_scroll_top: 80,
            })
        );
        assert_eq!(geometry(area, 100, 20, 40).unwrap().thumb_top, 8);
        assert_eq!(geometry(area, 100, 20, 80).unwrap().thumb_top, 16);
        assert_eq!(geometry(area, 20, 20, 0), None);
    }

    #[test]
    fn drag_mapping_uses_thumb_grab_offset_and_reaches_both_edges() {
        let geometry = geometry(Rect::new(0, 0, 80, 20), 100, 20, 40).unwrap();
        assert_eq!(scroll_top_at(geometry, geometry.track_top, 0), 0);
        assert_eq!(
            scroll_top_at(
                geometry,
                geometry.track_top + geometry.max_thumb_start + 1,
                0
            ),
            80
        );
        assert_eq!(scroll_top_at(geometry, 10, 2), 40);
    }

    #[test]
    fn render_paints_only_the_blue_thumb_and_leaves_the_track_transparent() {
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        terminal
            .draw(|frame| {
                let area = Rect::new(0, 0, 80, 20);
                frame
                    .buffer_mut()
                    .cell_mut((79, 0))
                    .expect("top cell")
                    .set_symbol("x");
                render(frame, area, 100, 20, 40, &Theme::dark());
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer.cell((79, 0)).unwrap().symbol(), "x");
        for row in 8..12 {
            assert_eq!(buffer.cell((79, row)).unwrap().symbol(), "█");
        }
        assert_eq!(buffer.cell((79, 12)).unwrap().symbol(), " ");
    }
}
