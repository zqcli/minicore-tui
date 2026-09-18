//! Stable display identities and pure hit/selection geometry.
//!
//! These values are presentation state only. They do not contain RPC
//! requests, workspace access, or execution authority, and fold overrides
//! are never persisted to Agent history.

use ratatui::text::Line;
use std::sync::Arc;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DurableCacheKey {
    pub revision: u64,
    pub width: u16,
    pub theme: crate::theme::ThemeKind,
    pub reasoning_visible: bool,
    pub tools_expanded: bool,
}

impl DurableCacheKey {
    pub fn new(
        view: &crate::state::session::SessionView,
        width: u16,
        theme: crate::theme::ThemeKind,
        reasoning_visible: bool,
    ) -> Self {
        Self {
            revision: view.transcript.render_revision,
            width,
            theme,
            reasoning_visible,
            tools_expanded: view.tools_expanded,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SectionKind {
    User,
    AssistantText,
    Thinking,
    Tool,
    Summary,
    Notice,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ReasoningKey {
    pub loop_id: String,
    pub request_index: u32,
    pub ordinal: u32,
}

impl ReasoningKey {
    pub fn new(loop_id: &str, request_index: u32, ordinal: u32) -> Self {
        Self {
            loop_id: loop_id.to_owned(),
            request_index,
            ordinal,
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SectionId {
    /// Shared strings: cloning section metadata per frame must not allocate.
    pub session_id: Arc<str>,
    pub loop_id: Option<Arc<str>>,
    pub request_index: Option<u32>,
    pub kind: SectionKind,
    pub ordinal: u32,
    pub tool_call_id: Option<Arc<str>>,
    pub history_index: Option<usize>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct LayoutKey {
    pub section: SectionId,
    pub revision: u64,
    pub width: u16,
    pub theme: crate::theme::ThemeKind,
    pub folded: bool,
}

/// One immutable, independently reusable section layout. Rows and all source
/// metadata are shared by frame snapshots; changing another section does not
/// copy this section's text or links.
#[derive(Debug)]
pub struct SectionLayout {
    pub key: LayoutKey,
    pub rows: Arc<Vec<Line<'static>>>,
    pub source: Arc<str>,
    pub copy_ranges: Arc<Vec<CopyRange>>,
    pub link_cells: Arc<Vec<LinkRow>>,
    pub content_columns: std::ops::Range<usize>,
    pub collapsible: bool,
    pub folded: bool,
}

#[derive(Clone, Debug)]
pub struct SectionPlacement {
    pub layout: Arc<SectionLayout>,
    /// Global rows within the conversation layout. The local section may
    /// start at one because adjacent vertical padding is shared.
    pub rows: std::ops::Range<usize>,
    pub local_start: usize,
}

/// Stable section placements plus integer row prefixes. Row lookup never
/// scans the historical sections and never materializes a full line array.
#[derive(Debug)]
pub struct ConversationLayout {
    pub sections: Arc<Vec<SectionPlacement>>,
    pub offsets: Arc<Vec<usize>>,
    pub total_rows: usize,
    blank: Line<'static>,
}

impl ConversationLayout {
    pub fn from_sections(sections: Vec<Arc<SectionLayout>>) -> Self {
        let mut placements = Vec::with_capacity(sections.len());
        let mut offsets = Vec::with_capacity(sections.len());
        let mut total_rows = 0;
        let mut previous_kind = None;
        let mut previous_ends_blank = false;
        for layout in sections {
            if layout.rows.is_empty() {
                continue;
            }
            if previous_kind == Some(SectionKind::User)
                && layout.key.section.kind == SectionKind::User
            {
                total_rows += 1;
            }
            let local_start = usize::from(
                previous_ends_blank
                    && layout.rows.first().is_some_and(|line| line.spans.is_empty()),
            );
            let start = total_rows;
            let end = start + layout.rows.len().saturating_sub(local_start);
            placements.push(SectionPlacement {
                layout: Arc::clone(&layout),
                rows: start..end,
                local_start,
            });
            offsets.push(end);
            total_rows = end;
            previous_kind = Some(layout.key.section.kind);
            previous_ends_blank = layout
                .rows
                .last()
                .is_some_and(|line| line.spans.is_empty());
        }
        Self {
            sections: Arc::new(placements),
            offsets: Arc::new(offsets),
            total_rows,
            blank: Line::default(),
        }
    }

    pub fn row(&self, row: usize) -> Option<&Line<'static>> {
        if row >= self.total_rows {
            return None;
        }
        let index = self.offsets.partition_point(|end| *end <= row);
        let placement = self.sections.get(index)?;
        if row < placement.rows.start {
            return Some(&self.blank);
        }
        placement
            .layout
            .rows
            .get(placement.local_start + row - placement.rows.start)
    }

    pub fn links_at(&self, row: usize) -> &[std::ops::Range<usize>] {
        const EMPTY: &[std::ops::Range<usize>] = &[];
        if row >= self.total_rows {
            return EMPTY;
        }
        let index = self.offsets.partition_point(|end| *end <= row);
        let Some(placement) = self.sections.get(index) else {
            return EMPTY;
        };
        if row < placement.rows.start {
            return EMPTY;
        }
        placement
            .layout
            .link_cells
            .get(placement.local_start + row - placement.rows.start)
            .map_or(EMPTY, Vec::as_slice)
    }

    pub fn window(&self, offset: usize, rows: usize) -> Vec<Line<'static>> {
        let start = offset.min(self.total_rows);
        let end = start.saturating_add(rows).min(self.total_rows);
        let mut window = Vec::with_capacity(end.saturating_sub(start));
        let mut bytes = 0usize;
        for row in start..end {
            if let Some(line) = self.row(row) {
                bytes += line
                    .spans
                    .iter()
                    .map(|span| span.content.len())
                    .sum::<usize>();
                window.push(line.clone());
            }
        }
        crate::perf::add(
            crate::perf::Counter::ViewportRowsMaterialized,
            window.len() as u64,
        );
        crate::perf::add(crate::perf::Counter::ViewportTextBytesCloned, bytes as u64);
        window
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FoldOverride {
    Expanded,
    Collapsed,
}

impl FoldOverride {
    pub fn expanded(&self) -> bool {
        matches!(self, Self::Expanded)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SectionRange {
    pub id: SectionId,
    /// Half-open logical row range in the prepared conversation.
    pub rows: std::ops::Range<usize>,
    /// Half-open content-cell range relative to the conversation area.
    pub content_columns: std::ops::Range<usize>,
    pub collapsible: bool,
    pub folded: bool,
}

impl SectionRange {
    pub fn contains_row(&self, row: usize) -> bool {
        self.rows.contains(&row)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CopyRange {
    pub row: usize,
    pub columns: std::ops::Range<usize>,
    /// Shared row text: a frame copy bumps a refcount instead of the bytes.
    pub text: Arc<str>,
    /// True for layout-only boundary rows and folded hints. These rows may
    /// be rendered for spacing or affordances, but they are not transcript
    /// content when a selection is copied.
    pub decorative: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SelectionGranularity {
    Character,
    Word,
    Paragraph,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectionPoint {
    /// Logical row in the prepared conversation at the time of the point.
    pub row: usize,
    /// Cell column relative to the conversation area.
    pub column: usize,
    /// Stable section identity used to rebase this point after a live row
    /// grows or a preceding section changes height.
    pub section_id: Option<SectionId>,
    pub section_row: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConversationSelection {
    pub session_id: String,
    pub anchor: SelectionPoint,
    pub focus: SelectionPoint,
    pub granularity: SelectionGranularity,
    pub dragged: bool,
}

impl ConversationSelection {
    pub fn is_empty(&self) -> bool {
        self.anchor.row == self.focus.row
            && self.anchor.column == self.focus.column
            && self.granularity == SelectionGranularity::Character
            && !self.dragged
    }

    pub fn ordered_points(&self) -> (&SelectionPoint, &SelectionPoint) {
        if (self.anchor.row, self.anchor.column) <= (self.focus.row, self.focus.column) {
            (&self.anchor, &self.focus)
        } else {
            (&self.focus, &self.anchor)
        }
    }
}

/// Per-row link cell ranges, parallel to the rows of one frame part.
pub type LinkRow = Vec<std::ops::Range<usize>>;

/// Immutable history layout shared by the session cache and frame snapshots.
#[derive(Debug)]
pub struct PreparedDurable {
    pub key: DurableCacheKey,
    pub layout: Arc<ConversationLayout>,
}

#[derive(Clone, Debug)]
pub struct SectionView {
    pub id: SectionId,
    pub rows: std::ops::Range<usize>,
    pub content_columns: std::ops::Range<usize>,
    pub collapsible: bool,
    pub folded: bool,
}

impl SectionView {
    pub fn contains_row(&self, row: usize) -> bool {
        self.rows.contains(&row)
    }
}

#[derive(Clone, Debug, Default)]
pub struct SectionIndex {
    pub durable: Option<Arc<ConversationLayout>>,
    pub durable_base: usize,
    pub durable_skip: usize,
    pub live: Arc<Vec<SectionRange>>,
}

impl SectionIndex {
    pub fn iter(&self) -> impl Iterator<Item = SectionView> + '_ {
        let durable = self
            .durable
            .as_ref()
            .into_iter()
            .flat_map(|layout| layout.sections.iter())
            .filter_map(move |placement| {
                let start = placement.rows.start.saturating_sub(self.durable_skip)
                    + self.durable_base;
                let end = placement.rows.end.saturating_sub(self.durable_skip)
                    + self.durable_base;
                (end > start).then_some(SectionView {
                    id: placement.layout.key.section.clone(),
                    rows: start..end,
                    content_columns: placement.layout.content_columns.clone(),
                    collapsible: placement.layout.collapsible,
                    folded: placement.layout.folded,
                })
            });
        durable.chain(self.live.iter().map(|section| SectionView {
            id: section.id.clone(),
            rows: section.rows.clone(),
            content_columns: section.content_columns.clone(),
            collapsible: section.collapsible,
            folded: section.folded,
        }))
    }

    pub fn first(&self) -> Option<SectionView> {
        self.iter().next()
    }
}

impl IntoIterator for SectionIndex {
    type Item = SectionView;
    type IntoIter = std::vec::IntoIter<SectionView>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter().collect::<Vec<_>>().into_iter()
    }
}

impl<'a> IntoIterator for &'a SectionIndex {
    type Item = SectionView;
    type IntoIter = Box<dyn Iterator<Item = SectionView> + 'a>;

    fn into_iter(self) -> Self::IntoIter {
        Box::new(self.iter())
    }
}

#[derive(Clone, Debug, Default)]
pub struct CopyIndex {
    pub durable: Option<Arc<ConversationLayout>>,
    pub durable_base: usize,
    pub durable_skip: usize,
    pub live_base: usize,
    pub live: Arc<Vec<CopyRange>>,
}

#[derive(Clone, Copy, Debug)]
pub struct CopyView<'a> {
    pub row: usize,
    pub columns: &'a std::ops::Range<usize>,
    pub text: &'a str,
    pub decorative: bool,
}

impl CopyIndex {
    pub fn iter(&self) -> impl Iterator<Item = CopyView<'_>> {
        let durable = self
            .durable
            .as_ref()
            .into_iter()
            .flat_map(|layout| layout.sections.iter())
            .flat_map(move |placement| {
                placement.layout.copy_ranges.iter().filter_map(move |copy| {
                    let local_row = copy.row;
                    (local_row >= placement.local_start).then_some(CopyView {
                        row: placement.rows.start + local_row - placement.local_start
                            + self.durable_base.saturating_sub(self.durable_skip),
                        columns: &copy.columns,
                        text: copy.text.as_ref(),
                        decorative: copy.decorative,
                    })
                })
            });
        durable.chain(self.live.iter().map(move |copy| CopyView {
            row: copy.row + self.live_base,
            columns: &copy.columns,
            text: copy.text.as_ref(),
            decorative: copy.decorative,
        }))
    }

    pub fn row(&self, row: usize) -> Option<CopyView<'_>> {
        self.iter().find(|copy| copy.row == row)
    }
}

#[derive(Clone, Debug, Default)]
pub struct PreparedConversation {
    /// Content width used to build the rows. It is the width after the App
    /// gutter, not the raw terminal width.
    pub width: u16,
    /// Active session identity at preparation time; `None` is the startup
    /// screen. This prevents a prepared background session from being drawn
    /// after activation changes.
    pub session_id: Option<String>,
    /// Durable transcript revision observed while preparing the rows.
    pub transcript_revision: u64,
    /// History preparation carried back to the App-owned cache installer.
    /// The rows are shared with the session cache, so installing a frame
    /// copies no history (`spec §11.2`).
    pub durable: Option<Arc<PreparedDurable>>,
    /// Leading durable rows the frame drops because the header already ends
    /// with a blank row (the boundary rule of `layout::append_section_ref`).
    pub durable_skip: usize,
    /// Header rows, before the durable block. Rebuilt per frame and small.
    pub header: Vec<Line<'static>>,
    pub header_links: Vec<LinkRow>,
    /// Rows after the durable block: notices, live sections, busy status.
    pub live: Vec<Line<'static>>,
    pub live_links: Vec<LinkRow>,
    pub sections: SectionIndex,
    pub copy_ranges: CopyIndex,
}

impl PreparedConversation {
    pub fn header_rows(&self) -> usize {
        self.header.len()
    }

    /// Durable rows actually visible in this frame (the skip is excluded).
    pub fn durable_rows(&self) -> usize {
        self.durable.as_ref().map_or(0, |durable| {
            durable.layout.total_rows.saturating_sub(self.durable_skip)
        })
    }

    pub fn live_rows(&self) -> usize {
        self.live.len()
    }

    pub fn total_rows(&self) -> usize {
        self.header_rows() + self.durable_rows() + self.live_rows()
    }

    /// One absolute frame row, borrowed from the installed frame parts. Hit
    /// tests, copy and the render window share this single fact.
    pub fn row(&self, row: usize) -> Option<&Line<'static>> {
        let header = self.header_rows();
        if row < header {
            return self.header.get(row);
        }
        let row = row - header;
        let durable = self.durable_rows();
        if row < durable {
            return self
                .durable
                .as_ref()?
                .layout
                .row(self.durable_skip.checked_add(row)?);
        }
        self.live.get(row - durable)
    }

    /// Link cell ranges for one absolute frame row; empty when the row has
    /// no markdown link.
    pub fn links_at(&self, row: usize) -> &[std::ops::Range<usize>] {
        const EMPTY: &[std::ops::Range<usize>] = &[];
        let header = self.header_rows();
        if row < header {
            return self.header_links.get(row).map_or(EMPTY, Vec::as_slice);
        }
        let row = row - header;
        let durable = self.durable_rows();
        if row < durable {
            return self
                .durable
                .as_ref()
                .map_or(EMPTY, |durable| {
                    durable.layout.links_at(self.durable_skip + row)
                });
        }
        self.live_links
            .get(row - durable)
            .map_or(EMPTY, Vec::as_slice)
    }

    /// Clones only the requested rows. The draw path uses this; nothing in
    /// the frame materializes the whole durable history.
    pub fn window(&self, offset: usize, rows: usize) -> Vec<Line<'static>> {
        let total = self.total_rows();
        let start = offset.min(total);
        let end = start.saturating_add(rows).min(total);
        let mut window = Vec::with_capacity(end.saturating_sub(start));
        let mut bytes = 0usize;
        for row in start..end {
            if let Some(line) = self.row(row) {
                bytes += line
                    .spans
                    .iter()
                    .map(|span| span.content.len())
                    .sum::<usize>();
                window.push(line.clone());
            }
        }
        // Real call point: the rows and bytes that became owned are measured
        // here, so a zero cannot come from a constant.
        crate::perf::add(
            crate::perf::Counter::ViewportRowsMaterialized,
            window.len() as u64,
        );
        crate::perf::add(crate::perf::Counter::ViewportTextBytesCloned, bytes as u64);
        window
    }

    /// Full materialization, for diagnostics and tests only. The draw path
    /// never calls this, so a nonzero historical clone count means a full
    /// frame was built somewhere it should not have been.
    pub fn lines(&self) -> Vec<Line<'static>> {
        let rows = self.total_rows();
        let lines = self.window(0, rows);
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

    /// Test-only: replaces the frame with `total` blank rows so scroll math
    /// can be exercised without building a huge transcript.
    #[cfg(test)]
    pub fn set_test_rows(&mut self, total: usize) {
        self.header = vec![Line::default(); total];
        self.header_links = vec![Vec::new(); total];
        self.durable = None;
        self.durable_skip = 0;
        self.live.clear();
        self.live_links.clear();
        self.sections = SectionIndex::default();
        self.copy_ranges = CopyIndex::default();
    }

    /// Stable identity of the shared historical frame; retention tests compare
    /// it instead of a per-frame buffer address.
    pub fn history_ptr(&self) -> *const PreparedDurable {
        self.durable.as_ref().map_or(std::ptr::null(), Arc::as_ptr)
    }

    /// Copy metadata for one absolute row, if any. `copy_ranges` is ordered by
    /// row, so this is a lookup, not a scan.
    pub fn copy_row(&self, row: usize) -> Option<CopyView<'_>> {
        self.copy_ranges.row(row)
    }

    pub fn section_at(&self, row: usize, column: usize) -> Option<SectionView> {
        self.sections
            .iter()
            .find(|section| section.contains_row(row) && section.content_columns.contains(&column))
    }
}
