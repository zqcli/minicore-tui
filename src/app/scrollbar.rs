//! Scrollbar and jump-label geometry, shared by the renderer and the mouse.

use std::ops::Range;

/// Composited on the last row while the view is detached from the tail.
pub const JUMP_LABEL: &str = " ↓ Jump to latest message · End ";

/// The cells of the jump label on the last row, centred.
pub fn jump_label(width: u16) -> Range<u16> {
    let len = (JUMP_LABEL.chars().count() as u16).min(width);
    let start = (width - len) / 2;
    start..start + len
}

/// The scrollbar thumb's first row and length on a `track`-row bar over
/// `total` rows scrolled to `top`; `None` when everything fits.
pub fn thumb(track: usize, total: usize, top: usize) -> Option<(usize, usize)> {
    if track == 0 || total <= track {
        return None;
    }
    let len = ((track * track + total / 2) / total).clamp(2.min(track), track);
    let (room, max_top) = (track - len, total - track);
    let start = (top.min(max_top) * room + max_top / 2) / max_top;
    Some((start, len))
}

/// The scroll top that puts a `len`-row thumb's first row on track row `row`.
pub fn thumb_top(track: usize, total: usize, len: usize, row: isize) -> usize {
    let room = track.saturating_sub(len);
    if room == 0 {
        return 0;
    }
    let row = row.clamp(0, room as isize) as usize;
    let max_top = total.saturating_sub(track);
    (row * max_top + room / 2) / room
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_thumb_is_sized_by_the_visible_share_and_never_below_two_rows() {
        assert_eq!(thumb(10, 10, 0), None, "nothing to scroll");
        assert_eq!(thumb(10, 20, 0), Some((0, 5)));
        assert_eq!(thumb(10, 20, 10), Some((5, 5)), "at the bottom");
        assert_eq!(thumb(10, 1000, 0), Some((0, 2)));
        assert_eq!(thumb(10, 1000, 990), Some((8, 2)));
        assert_eq!(thumb(10, 1000, 495), Some((4, 2)), "halfway");
    }

    #[test]
    fn dragging_the_thumb_maps_its_row_back_to_a_scroll_top() {
        assert_eq!(thumb_top(10, 20, 5, 0), 0);
        assert_eq!(thumb_top(10, 20, 5, 5), 10);
        assert_eq!(thumb_top(10, 20, 5, 9), 10, "clamped at the end");
        assert_eq!(thumb_top(10, 20, 5, -3), 0, "clamped at the start");
        for top in [0, 3, 7, 10] {
            let (start, len) = thumb(10, 20, top).unwrap();
            let back = thumb_top(10, 20, len, start as isize);
            assert_eq!(
                thumb(10, 20, back),
                Some((start, len)),
                "round trip from {top}"
            );
        }
    }

    #[test]
    fn the_jump_label_is_centred_and_clipped_to_narrow_rows() {
        let len = JUMP_LABEL.chars().count() as u16;
        assert_eq!(len, 32);
        assert_eq!(jump_label(100), 34..66);
        assert_eq!(jump_label(20), 0..20);
    }
}
