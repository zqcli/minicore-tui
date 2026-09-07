//! Cell-accurate editor layout shared by drawing, height measurement, and
//! mouse cursor placement.
//!
//! `tui-textarea` stores cursor columns as Unicode-scalar offsets. The
//! terminal, however, addresses display cells. This module is the one place
//! that converts between those coordinate systems; a wide grapheme is never
//! split into two editable positions.

use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VisualLine {
    pub logical_line: usize,
    pub start_char: usize,
    pub end_char: usize,
    pub text: String,
    pub width: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EditorLayout {
    pub width: usize,
    pub rows: Vec<VisualLine>,
    pub top: usize,
    pub target_rows: usize,
    pub top_padding: usize,
    pub cursor_row: usize,
    pub cursor_col: usize,
}

impl EditorLayout {
    pub fn new(lines: &[String], width: usize, target_rows: usize, cursor: (usize, usize)) -> Self {
        Self::new_with_atomic_ranges(lines, width, target_rows, cursor, &[])
    }

    /// Builds the layout while keeping the supplied display-only ranges
    /// atomic. The ranges are scalar offsets in the complete display buffer;
    /// they come from the Composer's validated paste metadata, so a literal
    /// string that merely resembles a paste marker remains ordinary text.
    pub fn new_with_atomic_ranges(
        lines: &[String],
        width: usize,
        target_rows: usize,
        cursor: (usize, usize),
        atomic_ranges: &[Range<usize>],
    ) -> Self {
        let width = width.max(1);
        let target_rows = target_rows.max(1);
        let rows = visual_lines(lines, width, atomic_ranges);
        let (cursor_row, cursor_col) = cursor_position(&rows, lines, cursor);
        let top = if rows.len() <= target_rows {
            0
        } else {
            cursor_row
                .saturating_sub(target_rows.saturating_sub(1))
                .min(rows.len().saturating_sub(target_rows))
        };
        let top_padding = target_rows
            .saturating_sub(rows.len())
            .checked_div(2)
            .unwrap_or(0);
        Self {
            width,
            rows,
            top,
            target_rows,
            top_padding,
            cursor_row,
            cursor_col,
        }
    }

    pub fn row_count(lines: &[String], width: usize) -> usize {
        Self::row_count_with_atomic_ranges(lines, width, &[])
    }

    pub fn row_count_with_atomic_ranges(
        lines: &[String],
        width: usize,
        atomic_ranges: &[Range<usize>],
    ) -> usize {
        visual_lines(lines, width.max(1), atomic_ranges).len()
    }

    /// Source row indices in screen order. `None` is centered padding.
    pub fn visible_rows(&self) -> Vec<Option<usize>> {
        (0..self.target_rows)
            .map(|screen_row| {
                if screen_row < self.top_padding {
                    None
                } else {
                    let source = self.top + screen_row - self.top_padding;
                    (source < self.rows.len()).then_some(source)
                }
            })
            .collect()
    }

    /// Cursor position in the fitted editor surface.
    pub fn screen_cursor(&self) -> (usize, usize) {
        (
            self.top_padding + self.cursor_row.saturating_sub(self.top),
            self.cursor_col,
        )
    }

    /// Converts a cell coordinate relative to the editor content into a
    /// scalar cursor offset. Padding is deliberately not a cursor target.
    pub fn cursor_at(&self, screen_row: usize, content_col: usize) -> Option<(usize, usize)> {
        let source_row = self.visible_rows().get(screen_row).copied().flatten()?;
        let row = self.rows.get(source_row)?;
        let char_offset = char_offset_at_cell(&row.text, content_col);
        Some((row.logical_line, row.start_char + char_offset))
    }
}

fn visual_lines(lines: &[String], width: usize, atomic_ranges: &[Range<usize>]) -> Vec<VisualLine> {
    let mut rows = Vec::new();
    let mut line_offset = 0;
    for (logical_line, line) in lines.iter().enumerate() {
        let segments = visual_segments(line, line_offset, atomic_ranges);
        if segments.is_empty() {
            rows.push(VisualLine {
                logical_line,
                start_char: 0,
                end_char: 0,
                text: String::new(),
                width: 0,
            });
            line_offset += 1;
            continue;
        }

        if UnicodeWidthStr::width(line.as_str()) <= width {
            rows.push(VisualLine {
                logical_line,
                start_char: 0,
                end_char: line.chars().count(),
                text: line.clone(),
                width: UnicodeWidthStr::width(line.as_str()),
            });
            line_offset += line.chars().count() + 1;
            continue;
        }

        let chunks = wrapped_chunks(&segments, width);
        for (start, end) in chunks {
            let first = &segments[start];
            let last = &segments[end.saturating_sub(1)];
            let text = line[first.byte_start..last.byte_end].to_owned();
            rows.push(VisualLine {
                logical_line,
                start_char: first.char_start,
                end_char: last.char_end,
                width: UnicodeWidthStr::width(text.as_str()),
                text,
            });
        }
        line_offset += line.chars().count() + 1;
    }
    if rows.is_empty() {
        rows.push(VisualLine {
            logical_line: 0,
            start_char: 0,
            end_char: 0,
            text: String::new(),
            width: 0,
        });
    }
    rows
}

#[derive(Clone, Debug)]
struct VisualSegment {
    byte_start: usize,
    byte_end: usize,
    char_start: usize,
    char_end: usize,
    text: String,
    width: usize,
    atomic: bool,
}

fn visual_segments(
    line: &str,
    line_offset: usize,
    atomic_ranges: &[Range<usize>],
) -> Vec<VisualSegment> {
    let mut segments = Vec::new();
    let mut byte_start = 0;
    let mut char_start = 0;
    while byte_start < line.len() {
        let (byte_end, char_end, atomic) = if let Some(range) = atomic_ranges.iter().find(|range| {
            range.start == line_offset + char_start
                && range.end > range.start
                && range.end <= line_offset + line.chars().count()
        }) {
            let local_end = range.end - line_offset;
            let byte_end = line
                .char_indices()
                .nth(local_end)
                .map_or(line.len(), |(byte, _)| byte);
            (byte_end, local_end, true)
        } else {
            let grapheme = line[byte_start..]
                .graphemes(true)
                .next()
                .expect("non-empty suffix has a grapheme");
            (
                byte_start + grapheme.len(),
                char_start + grapheme.chars().count(),
                false,
            )
        };
        let text = &line[byte_start..byte_end];
        segments.push(VisualSegment {
            byte_start,
            byte_end,
            char_start,
            char_end,
            text: text.to_owned(),
            width: UnicodeWidthStr::width(text),
            atomic,
        });
        byte_start = byte_end;
        char_start = char_end;
    }
    segments
}

fn wrapped_chunks(segments: &[VisualSegment], width: usize) -> Vec<(usize, usize)> {
    let mut chunks = Vec::new();
    let mut chunk_start = 0;
    let mut current_width = 0;
    let mut wrap_opportunity: Option<(usize, usize)> = None;

    for index in 0..segments.len() {
        let segment = &segments[index];
        if current_width + segment.width > width {
            if let Some((wrap_index, wrap_width)) = wrap_opportunity.take() {
                if chunk_start < wrap_index
                    && current_width.saturating_sub(wrap_width) + segment.width <= width
                {
                    chunks.push((chunk_start, wrap_index));
                    chunk_start = wrap_index;
                    current_width = current_width.saturating_sub(wrap_width);
                } else if chunk_start < index {
                    chunks.push((chunk_start, index));
                    chunk_start = index;
                    current_width = 0;
                    wrap_opportunity = None;
                } else {
                    // Defensive handling for a segment wider than the available
                    // width. Keeping it whole is safer than splitting a grapheme.
                    chunks.push((index, index + 1));
                    chunk_start = index + 1;
                    current_width = 0;
                    wrap_opportunity = None;
                    continue;
                }
            } else if chunk_start < index {
                chunks.push((chunk_start, index));
                chunk_start = index;
                current_width = 0;
                wrap_opportunity = None;
            } else {
                // Defensive handling for a segment wider than the available
                // width. Keeping it whole is safer than splitting a grapheme.
                chunks.push((index, index + 1));
                chunk_start = index + 1;
                current_width = 0;
                wrap_opportunity = None;
                continue;
            }
        }

        current_width += segment.width;
        if let Some(next) = segments.get(index + 1) {
            let current_is_whitespace = segment_is_whitespace(segment);
            let next_is_whitespace = segment_is_whitespace(next);
            if !next_is_whitespace
                && (current_is_whitespace
                    || (!current_is_whitespace
                        && (segment_is_cjk(segment) || segment_is_cjk(next))))
            {
                wrap_opportunity = Some((index + 1, current_width));
            }
        }
    }
    if chunk_start < segments.len() {
        chunks.push((chunk_start, segments.len()));
    }
    chunks
}

fn is_paste_marker_segment(segment: &VisualSegment) -> bool {
    segment.atomic
}

fn segment_is_whitespace(segment: &VisualSegment) -> bool {
    !is_paste_marker_segment(segment) && segment.text.chars().all(char::is_whitespace)
}

fn segment_is_cjk(segment: &VisualSegment) -> bool {
    !is_paste_marker_segment(segment)
        && segment.text.chars().any(|character| {
            matches!(
                character as u32,
                0x3400..=0x4dbf
                    | 0x4e00..=0x9fff
                    | 0xf900..=0xfaff
                    | 0x3040..=0x30ff
                    | 0xac00..=0xd7af
                    | 0x3100..=0x312f
                    | 0x31a0..=0x31bf
            )
        })
}

fn cursor_position(
    rows: &[VisualLine],
    lines: &[String],
    cursor: (usize, usize),
) -> (usize, usize) {
    let logical_line = cursor.0.min(lines.len().saturating_sub(1));
    let line = lines.get(logical_line).map(String::as_str).unwrap_or("");
    let cursor_char = cursor.1.min(line.chars().count());
    let row_index = rows
        .iter()
        .position(|row| {
            row.logical_line == logical_line
                && cursor_char >= row.start_char
                && cursor_char < row.end_char
        })
        .or_else(|| {
            rows.iter().enumerate().rev().find_map(|(index, row)| {
                (row.logical_line == logical_line && cursor_char == row.end_char).then_some(index)
            })
        })
        .unwrap_or(0);
    let row = &rows[row_index];
    let prefix = line
        .chars()
        .skip(row.start_char)
        .take(cursor_char.saturating_sub(row.start_char))
        .collect::<String>();
    (row_index, UnicodeWidthStr::width(prefix.as_str()))
}

fn char_offset_at_cell(text: &str, target: usize) -> usize {
    if target == 0 {
        return 0;
    }
    let mut used = 0;
    for (byte_index, grapheme) in text.grapheme_indices(true) {
        let width = UnicodeWidthStr::width(grapheme);
        if target < used + width {
            return text[..byte_index].chars().count();
        }
        used += width;
        if target == used {
            return text[..byte_index].chars().count() + grapheme.chars().count();
        }
    }
    text.chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &[&str]) -> Vec<String> {
        text.iter().map(|line| (*line).to_owned()).collect()
    }

    #[test]
    fn wraps_without_splitting_wide_graphemes() {
        let layout = EditorLayout::new(&lines(&["a😀中文b"]), 4, 4, (0, 0));
        assert_eq!(
            layout
                .rows
                .iter()
                .map(|row| row.text.as_str())
                .collect::<Vec<_>>(),
            vec!["a😀", "中文", "b"]
        );
        assert!(layout.rows.iter().all(|row| row.width <= 4));
    }

    #[test]
    fn cursor_at_wide_grapheme_middle_snaps_to_its_start() {
        let layout = EditorLayout::new(&lines(&["a中文"]), 8, 1, (0, 3));
        assert_eq!(layout.cursor_at(0, 2), Some((0, 1)));
        assert_eq!(layout.cursor_at(0, 3), Some((0, 2)));
    }

    #[test]
    fn centered_padding_is_not_a_cursor_target() {
        let layout = EditorLayout::new(&lines(&["text"]), 10, 4, (0, 4));
        assert_eq!(layout.visible_rows(), vec![None, Some(0), None, None]);
        assert_eq!(layout.cursor_at(0, 2), None);
    }

    #[test]
    fn cursor_boundary_moves_to_the_next_soft_wrapped_row() {
        let layout = EditorLayout::new(&lines(&["abcdef"]), 3, 2, (0, 3));
        assert_eq!(layout.screen_cursor(), (1, 0));
        assert_eq!(layout.cursor_at(0, 3), Some((0, 3)));
    }

    #[test]
    fn only_valid_paste_ranges_are_kept_atomic() {
        let marker = "[paste #1 +11 lines]";
        let literal = lines(&[&format!("x{marker}y")]);
        let literal_layout = EditorLayout::new(&literal, 20, 4, (0, 0));
        assert!(!literal_layout.rows.iter().any(|row| row.text == marker));

        let display = lines(&[&format!("x{marker}y")]);
        let atomic = std::iter::once(1..1 + marker.chars().count()).collect::<Vec<_>>();
        let atomic_layout = EditorLayout::new_with_atomic_ranges(&display, 20, 4, (0, 0), &atomic);
        assert!(atomic_layout.rows.iter().any(|row| row.text == marker));
    }
}
