//! Section-level conversation layout (spec §11).
//!
//! One `ConversationLayout` owns a snapshot of `Arc<SectionLayout>` values in
//! row order plus the integer prefix offsets of their first rows. Row lookup
//! is a binary search over the prefixes, so the viewport materializes only the
//! sections it overlaps; hit tests, copy and the scrollbar read the same
//! installed layout instead of rebuilding a full-history buffer per frame.
//!
//! Sections are immutable and keyed by `SectionLayoutKey` (stable `SectionId`
//! plus width, theme, fold state and a caller-provided content signature), so
//! a fold toggle replaces exactly one `Arc` and every other section is reused
//! unchanged. Absolute positions live in the layout's prefix vector and its
//! shared `SectionRange` metadata, never inside the reused section Arcs.
//!
//! This module is deliberately concrete: one layout type, one key type, one
//! offset vector. There is no generic framework and no Fenwick tree; the
//! prefix vector plus `partition_point` is enough for row order.

use std::ops::Range;
use std::sync::Arc;

use ratatui::text::Line;

use crate::state::view::{CopyRange, LinkRow, SectionId, SectionRange};

/// Identity of one laid-out section. A layout pass may reuse a previous
/// `SectionLayout` only when the whole key matches.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SectionLayoutKey {
    pub id: SectionId,
    pub width: u16,
    pub theme: crate::theme::ThemeKind,
    /// Resolved fold state: folding a section changes its key (and therefore
    /// its height) but not the keys of its neighbours.
    pub folded: bool,
    /// Content signature supplied by the producer (for example a history
    /// index plus a content length). The layout only compares it.
    pub signature: u64,
}

impl SectionLayoutKey {
    pub fn new(
        id: SectionId,
        width: u16,
        theme: crate::theme::ThemeKind,
        folded: bool,
        signature: u64,
    ) -> Self {
        Self {
            id,
            width,
            theme,
            folded,
            signature,
        }
    }
}

/// Where one laid-out row's copy text comes from in the source block. The
/// layout stores it once; frames only borrow it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CopySpan {
    /// Source row inside the section body (hard newline count for plain text,
    /// wrapped row for markdown).
    pub source_row: usize,
    /// First source byte covered by this rendered row.
    pub byte_start: usize,
    /// Half-open source byte range covered by this rendered row.
    pub bytes: Range<usize>,
}

/// One immutable, laid-out section. `rows` are local: the owning layout owns
/// the absolute offsets.
#[derive(Debug)]
pub struct SectionLayout {
    pub key: SectionLayoutKey,
    /// Rendered rows, owned by this section only.
    pub lines: Vec<Line<'static>>,
    /// Link cell ranges, parallel to `lines`.
    pub links: Vec<LinkRow>,
    /// Copy metadata, parallel to `lines`; `CopyRange.row` is local.
    pub copy: Vec<CopyRange>,
    /// Source mapping, parallel to `lines` (used by selection and copy).
    pub source: Vec<CopySpan>,
    /// Content-cell range for hit arbitration.
    pub content_columns: Range<usize>,
    pub collapsible: bool,
    pub folded: bool,
}

impl SectionLayout {
    pub fn height(&self) -> usize {
        self.lines.len()
    }
}

/// An immutable layout snapshot. Cloning it bumps one refcount; installing a
/// frame never copies section rows or metadata.
#[derive(Debug, Default)]
pub struct ConversationLayout {
    pub width: u16,
    pub theme: Option<crate::theme::ThemeKind>,
    sections: Vec<Arc<SectionLayout>>,
    /// `prefixes[i]` is the absolute first row of `sections[i]`; the last
    /// entry is the total row count.
    prefixes: Vec<usize>,
    total_rows: usize,
    /// Absolute section metadata, computed once per layout and shared with
    /// every frame that reads it.
    ranges: Arc<Vec<SectionRange>>,
}

impl ConversationLayout {
    /// Builds a layout from sections in order. Absolute row positions are
    /// derived here, so replacing one section (a fold) renumbers cheaply
    /// without rewriting the other section Arcs.
    pub fn from_sections(
        width: u16,
        theme: crate::theme::ThemeKind,
        sections: Vec<Arc<SectionLayout>>,
    ) -> Self {
        let mut prefixes = Vec::with_capacity(sections.len() + 1);
        let mut ranges = Vec::with_capacity(sections.len());
        let mut row = 0usize;
        for section in &sections {
            prefixes.push(row);
            let height = section.height();
            ranges.push(SectionRange {
                id: section.key.id.clone(),
                rows: row..row + height,
                content_columns: section.content_columns.clone(),
                collapsible: section.collapsible,
                folded: section.folded,
            });
            row += height;
        }
        prefixes.push(row);
        Self {
            width,
            theme: Some(theme),
            sections,
            prefixes,
            total_rows: row,
            ranges: Arc::new(ranges),
        }
    }

    pub fn sections(&self) -> &[Arc<SectionLayout>] {
        &self.sections
    }

    /// Absolute row ranges of every section, shared with frames.
    pub fn ranges(&self) -> &Arc<Vec<SectionRange>> {
        &self.ranges
    }

    pub fn is_empty(&self) -> bool {
        self.sections.is_empty()
    }

    pub fn total_rows(&self) -> usize {
        self.total_rows
    }

    /// Index of the section that owns `row`, by binary search over the row
    /// prefixes. `None` for a row outside the layout.
    pub fn section_index_at(&self, row: usize) -> Option<usize> {
        if row >= self.total_rows {
            return None;
        }
        let index = self.prefixes.partition_point(|&start| start <= row);
        index
            .checked_sub(1)
            .filter(|&index| index < self.sections.len())
    }

    pub fn section_at_row(&self, row: usize) -> Option<&Arc<SectionLayout>> {
        self.section_index_at(row)
            .map(|index| &self.sections[index])
    }

    /// One absolute row, borrowed from its section.
    pub fn row(&self, row: usize) -> Option<&Line<'static>> {
        let index = self.section_index_at(row)?;
        let start = self.prefixes[index];
        self.sections[index].lines.get(row - start)
    }

    pub fn links_at(&self, row: usize) -> &[Range<usize>] {
        const EMPTY: &[Range<usize>] = &[];
        let Some(index) = self.section_index_at(row) else {
            return EMPTY;
        };
        let local = row - self.prefixes[index];
        self.sections[index]
            .links
            .get(local)
            .map_or(EMPTY, Vec::as_slice)
    }

    /// Copy metadata for one absolute row, borrowed from its section.
    pub fn copy_row(&self, row: usize) -> Option<&CopyRange> {
        let index = self.section_index_at(row)?;
        let local = row - self.prefixes[index];
        self.sections[index]
            .copy
            .iter()
            .find(|copy| copy.row == local)
    }

    pub fn source_span(&self, row: usize) -> Option<CopySpan> {
        let index = self.section_index_at(row)?;
        let local = row - self.prefixes[index];
        self.sections[index].source.get(local).cloned()
    }

    /// Hit arbitration: the section whose row and content columns contain the
    /// point, read from the installed layout.
    pub fn section_at(&self, row: usize, column: usize) -> Option<SectionRange> {
        let index = self.section_index_at(row)?;
        let range = self.ranges.get(index)?;
        range
            .content_columns
            .contains(&column)
            .then(|| range.clone())
    }

    /// Materializes only the rows the caller asks for. `overscan` rows on each
    /// side are included so scrolling does not re-clone the whole history; the
    /// result is bounded by `rows + 2 * overscan`, never by `total_rows`.
    pub fn window(&self, offset: usize, rows: usize, overscan: usize) -> Vec<Line<'static>> {
        let start = offset.saturating_sub(overscan);
        let end = offset
            .saturating_add(rows)
            .saturating_add(overscan)
            .min(self.total_rows);
        let mut window = Vec::with_capacity(end.saturating_sub(start));
        let mut bytes = 0usize;
        if start < end {
            for (index, section) in self.sections.iter().enumerate() {
                let section_start = self.prefixes[index];
                let section_end = section_start + section.height();
                if section_end <= start {
                    continue;
                }
                if section_start >= end {
                    break;
                }
                let from = start.max(section_start) - section_start;
                let to = end.min(section_end) - section_start;
                for line in &section.lines[from..to] {
                    bytes += line
                        .spans
                        .iter()
                        .map(|span| span.content.len())
                        .sum::<usize>();
                    window.push(line.clone());
                }
            }
        }
        crate::perf::add(
            crate::perf::Counter::ViewportRowsMaterialized,
            window.len() as u64,
        );
        crate::perf::add(crate::perf::Counter::ViewportTextBytesCloned, bytes as u64);
        window
    }

    /// Full materialization for diagnostics and tests; the draw path never
    /// calls it. Counted separately so a production regression is visible.
    pub fn lines(&self) -> Vec<Line<'static>> {
        let mut lines = Vec::with_capacity(self.total_rows);
        for section in &self.sections {
            lines.extend(section.lines.iter().cloned());
        }
        let bytes: usize = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.len())
            .sum();
        crate::perf::add(
            crate::perf::Counter::HistoricalTextBytesCloned,
            bytes as u64,
        );
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::view::SectionKind;

    fn id(ordinal: u32) -> SectionId {
        SectionId {
            session_id: "ses_1".into(),
            loop_id: Some("lup_1".into()),
            request_index: Some(0),
            kind: SectionKind::AssistantText,
            ordinal,
            tool_call_id: None,
            history_index: Some(ordinal as usize),
        }
    }

    fn section(ordinal: u32, height: usize, folded: bool) -> Arc<SectionLayout> {
        let lines: Vec<Line<'static>> = (0..height)
            .map(|index| Line::from(format!("row {ordinal}-{index}")))
            .collect();
        let copy: Vec<CopyRange> = lines
            .iter()
            .enumerate()
            .map(|(index, line)| CopyRange {
                row: index,
                columns: 0..2,
                text: line.to_string().into(),
                decorative: false,
            })
            .collect();
        let source = (0..height)
            .map(|index| CopySpan {
                source_row: index,
                byte_start: index * 4,
                bytes: index * 4..index * 4 + 4,
            })
            .collect();
        Arc::new(SectionLayout {
            key: SectionLayoutKey::new(
                id(ordinal),
                80,
                crate::theme::ThemeKind::Dark,
                folded,
                ordinal as u64,
            ),
            lines,
            links: vec![Vec::new(); height],
            copy,
            source,
            content_columns: 2..78,
            collapsible: true,
            folded,
        })
    }

    fn layout(sections: Vec<Arc<SectionLayout>>) -> ConversationLayout {
        ConversationLayout::from_sections(80, crate::theme::ThemeKind::Dark, sections)
    }

    /// Row lookup resolves through the prefix binary search, so a 50k-row
    /// layout answers a viewport query without walking sections.
    #[test]
    fn row_index_uses_prefix_binary_search() {
        let layout = layout(vec![section(0, 10, false), section(1, 5, false)]);
        assert_eq!(layout.total_rows(), 15);
        assert_eq!(layout.section_index_at(0), Some(0));
        assert_eq!(layout.section_index_at(9), Some(0));
        assert_eq!(layout.section_index_at(10), Some(1));
        assert_eq!(layout.section_index_at(14), Some(1));
        assert_eq!(layout.section_index_at(15), None);
        assert_eq!(
            layout.row(12).map(ToString::to_string).as_deref(),
            Some("row 1-2")
        );
        assert_eq!(layout.copy_row(12).map(|copy| &*copy.text), Some("row 1-2"));
        assert_eq!(layout.source_span(12).map(|span| span.source_row), Some(2));
        assert_eq!(
            layout.section_at(12, 3).map(|range| range.rows),
            Some(10..15)
        );
    }

    /// The viewport window clones only the overlapping rows plus overscan and
    /// is bounded by the request, never by the total row count.
    #[test]
    fn window_is_bounded_by_viewport_and_overscan() {
        crate::perf::reset();
        let layout = layout((0..100).map(|index| section(index, 10, false)).collect());
        let window = layout.window(4, 40, 0);
        assert_eq!(window.len(), 40);
        let after = crate::perf::snapshot();
        assert_eq!(after.viewport_rows_materialized, 40);
        assert!(after.viewport_text_bytes_cloned > 0);
        assert_eq!(after.historical_text_bytes_cloned, 0);
        assert_eq!(layout.window(500, 20, 5).len(), 30);
    }

    /// Folding one section changes only that section's key; the other section
    /// Arcs are reused and the absolute rows are renumbered by the layout.
    #[test]
    fn fold_key_change_is_local_to_one_section() {
        let first = vec![
            section(0, 10, false),
            section(1, 4, false),
            section(2, 6, false),
        ];
        let folded = vec![
            Arc::clone(&first[0]),
            section(1, 1, true),
            Arc::clone(&first[2]),
        ];
        let before = layout(first);
        let after = layout(folded);
        assert_ne!(before.sections()[1].key, after.sections()[1].key);
        assert!(Arc::ptr_eq(&before.sections()[0], &after.sections()[0]));
        assert!(Arc::ptr_eq(&before.sections()[2], &after.sections()[2]));
        assert_eq!(after.total_rows(), 17);
        assert_eq!(
            after.ranges()[1].rows,
            10..11,
            "folded height is renumbered"
        );
        assert_eq!(after.ranges()[2].rows, 11..17);
    }
}
