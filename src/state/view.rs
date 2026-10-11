//! Stable display identities and pure hit/selection geometry.
//!
//! These values are presentation state only. They do not contain RPC
//! requests, workspace access, or execution authority, and fold overrides
//! are never persisted to Agent history.

use ratatui::text::Line;
use std::collections::HashSet;
use std::sync::Arc;

use crate::state::tool::ToolKey;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DurableCacheKey {
    pub revision: u64,
    pub width: u16,
    pub theme: crate::theme::ThemeKind,
    pub reasoning_visible: bool,
    pub tools_expanded: bool,
    pub live_tool_keys: Arc<HashSet<ToolKey>>,
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
            live_tool_keys: live_tool_keys(view),
        }
    }
}

/// The durable layout only needs to know which live tool cards are real
/// owners. This scans the bounded live tail, not transcript history, so a
/// fallback ToolCall section cannot become stale when a live tool first
/// appears without invalidating every frame's historical rows.
pub(crate) fn live_tool_keys(view: &crate::state::session::SessionView) -> Arc<HashSet<ToolKey>> {
    let mut keys = HashSet::new();
    if let Some(live) = view.live.as_ref() {
        let loop_id = live
            .reference
            .as_ref()
            .map_or("", |reference| reference.loop_id.as_str());
        for request in &live.requests {
            for tool in &request.tools {
                keys.insert(ToolKey::new(
                    &view.info.session_id,
                    loop_id,
                    request.request_index,
                    &tool.tool_call_id,
                ));
            }
        }
    }
    Arc::new(keys)
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
    pub reasoning_visible: bool,
}

/// One immutable, independently reusable section layout. Rows and all source
/// metadata are shared by frame snapshots; changing another section does not
/// copy this section's text or links.
#[derive(Debug)]
pub struct SectionLayout {
    pub key: LayoutKey,
    pub order: usize,
    pub rows: Arc<Vec<Line<'static>>>,
    pub source: Arc<str>,
    pub source_map: Arc<SourceMap>,
    pub copy_ranges: Arc<Vec<CopyRange>>,
    pub link_cells: Arc<Vec<LinkRow>>,
    pub content_columns: std::ops::Range<usize>,
    pub collapsible: bool,
    pub folded: bool,
}

#[derive(Clone, Debug)]
pub struct SourceMap {
    pub source: Arc<str>,
    pub rows: Arc<Vec<SourceRow>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceRow {
    pub source_range: std::ops::Range<usize>,
    pub hard_break_after: bool,
    pub decorative: bool,
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
    /// Durable ToolKeys are built with the immutable layout and reused by
    /// every live-tail composition; the frame path never rescans history.
    pub tool_keys: Arc<HashSet<ToolKey>>,
    blank: Line<'static>,
}

impl ConversationLayout {
    pub fn retained_bytes(&self) -> usize {
        self.sections
            .iter()
            .map(|placement| placement.layout.retained_bytes())
            .sum()
    }

    pub fn from_sections(mut sections: Vec<Arc<SectionLayout>>) -> Self {
        sections.sort_by_key(|section| section.order);
        let tool_keys = sections
            .iter()
            .filter_map(|section| {
                let id = &section.key.section;
                if id.kind != SectionKind::Tool {
                    return None;
                }
                Some(ToolKey::new(
                    id.session_id.as_ref(),
                    id.loop_id.as_ref()?.as_ref(),
                    id.request_index?,
                    id.tool_call_id.as_ref()?.as_ref(),
                ))
            })
            .collect();
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
                    && layout
                        .rows
                        .first()
                        .is_some_and(|line| line.spans.is_empty()),
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
            previous_ends_blank = layout.rows.last().is_some_and(|line| line.spans.is_empty());
        }
        Self {
            sections: Arc::new(placements),
            offsets: Arc::new(offsets),
            total_rows,
            tool_keys: Arc::new(tool_keys),
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

impl SectionLayout {
    pub fn retained_bytes(&self) -> usize {
        let rows = self
            .rows
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.len())
                    .sum::<usize>()
            })
            .sum::<usize>();
        let mut table_allocations = HashSet::new();
        let copy = self
            .copy_ranges
            .iter()
            .map(|range| {
                range.text().len()
                    + range.table_fragments.as_ref().map_or(0, |fragments| {
                        if !table_allocations.insert(Arc::as_ptr(fragments)) {
                            return 0;
                        }
                        // One shared slice allocation, including its Arc
                        // counters; owned chunk buffers can retain spare capacity.
                        std::mem::size_of_val(fragments.as_ref())
                            + 2 * std::mem::size_of::<usize>()
                            + fragments
                                .iter()
                                .map(|fragment| fragment.text.capacity())
                                .sum::<usize>()
                    })
            })
            .sum::<usize>();
        let links = self
            .link_cells
            .iter()
            .map(|row| row.len() * std::mem::size_of::<std::ops::Range<usize>>())
            .sum::<usize>();
        rows + copy + self.source.len() + links
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
    /// One shared source string per section/live run; each visual row only
    /// stores a byte range into it instead of owning another row body.
    pub source: Arc<str>,
    pub source_range: std::ops::Range<usize>,
    /// Byte offset in the section's logical source. For rendered copy text
    /// this remains separate from `source_range`, whose owner may be a
    /// bounded display-row source.
    pub source_offset: usize,
    /// A soft-wrapped row joins directly to the next source range. Hard
    /// newlines, real empty lines, and section boundaries insert `\n`.
    pub hard_break_after: bool,
    /// True for layout-only boundary rows and folded hints. These rows may
    /// be rendered for spacing or affordances, but they are not transcript
    /// content when a selection is copied.
    pub decorative: bool,
    /// Renderer-owned discontinuous cell chunks, present only for tables.
    pub table_fragments: Option<Arc<[crate::markdown::TableCopyFragment]>>,
}

impl CopyRange {
    pub fn text(&self) -> &str {
        &self.source[self.source_range.clone()]
    }
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

/// A content-based scroll position. `screen_row` keeps the anchored content
/// at the same terminal row while the section's rendered height changes.
/// `source_offset` survives wrapping, folding, prepending, and live-to-durable
/// reconciliation as long as the logical section remains available.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScrollAnchor {
    pub section_id: SectionId,
    pub source_offset: usize,
    pub screen_row: usize,
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

impl PreparedDurable {
    pub fn retained_bytes(&self) -> usize {
        self.layout.retained_bytes()
    }
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
    pub fn at_row(&self, row: usize) -> Option<SectionView> {
        if let Some(layout) = &self.durable {
            if row >= self.durable_base {
                let source_row = row - self.durable_base + self.durable_skip;
                let index = layout
                    .sections
                    .partition_point(|section| section.rows.end <= source_row);
                if let Some(section) = layout
                    .sections
                    .get(index)
                    .filter(|section| section.rows.contains(&source_row))
                {
                    return Some(SectionView {
                        id: section.layout.key.section.clone(),
                        rows: section.rows.start.saturating_sub(self.durable_skip)
                            + self.durable_base
                            ..section.rows.end.saturating_sub(self.durable_skip)
                                + self.durable_base,
                        content_columns: section.layout.content_columns.clone(),
                        collapsible: section.layout.collapsible,
                        folded: section.layout.folded,
                    });
                }
            }
        }
        let index = self.live.partition_point(|section| section.rows.end <= row);
        self.live
            .get(index)
            .filter(|section| section.rows.contains(&row))
            .map(|section| SectionView {
                id: section.id.clone(),
                rows: section.rows.clone(),
                content_columns: section.content_columns.clone(),
                collapsible: section.collapsible,
                folded: section.folded,
            })
    }
    pub fn iter(&self) -> impl Iterator<Item = SectionView> + '_ {
        let durable = self
            .durable
            .as_ref()
            .into_iter()
            .flat_map(|layout| layout.sections.iter())
            .filter_map(move |placement| {
                let start =
                    placement.rows.start.saturating_sub(self.durable_skip) + self.durable_base;
                let end = placement.rows.end.saturating_sub(self.durable_skip) + self.durable_base;
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
    pub source_offset: usize,
    pub hard_break_after: bool,
    pub decorative: bool,
    pub table_fragments: Option<&'a [crate::markdown::TableCopyFragment]>,
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
                        text: copy.text(),
                        source_offset: copy.source_offset,
                        hard_break_after: copy.hard_break_after,
                        decorative: copy.decorative,
                        table_fragments: copy.table_fragments.as_deref(),
                    })
                })
            });
        durable.chain(self.live.iter().map(move |copy| CopyView {
            row: copy.row + self.live_base,
            columns: &copy.columns,
            text: copy.text(),
            source_offset: copy.source_offset,
            hard_break_after: copy.hard_break_after,
            decorative: copy.decorative,
            table_fragments: copy.table_fragments.as_deref(),
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
    pub fn placeholder(active: Option<&crate::state::session::SessionView>, width: u16) -> Self {
        Self {
            width,
            session_id: active.map(|view| view.info.session_id.clone()),
            transcript_revision: active.map_or(0, |view| view.transcript.render_revision),
            ..Self::default()
        }
    }

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
            return self.durable.as_ref().map_or(EMPTY, |durable| {
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

    /// Locates an anchor by stable section identity and logical source byte.
    /// If the original section is unloaded, the nearest retained history
    /// section is used rather than pretending an absent range is copyable.
    pub fn has_scroll_anchor_section(&self, anchor: &ScrollAnchor) -> bool {
        self.sections.iter().any(|section| {
            section.id.session_id == anchor.section_id.session_id
                && section.id.loop_id == anchor.section_id.loop_id
                && section.id.request_index == anchor.section_id.request_index
                && section.id.kind == anchor.section_id.kind
                && section.id.ordinal == anchor.section_id.ordinal
                && section.id.tool_call_id == anchor.section_id.tool_call_id
                && (section.id.history_index == anchor.section_id.history_index
                    || section.id.history_index.is_none()
                    || anchor.section_id.history_index.is_none())
        })
    }

    pub fn row_for_scroll_anchor(&self, anchor: &ScrollAnchor) -> Option<usize> {
        let sections: Vec<SectionView> = self.sections.iter().collect();
        let stable = |section: &SectionView| {
            section.id.session_id == anchor.section_id.session_id
                && section.id.loop_id == anchor.section_id.loop_id
                && section.id.request_index == anchor.section_id.request_index
                && section.id.kind == anchor.section_id.kind
                && section.id.ordinal == anchor.section_id.ordinal
                && section.id.tool_call_id == anchor.section_id.tool_call_id
                && (section.id.history_index == anchor.section_id.history_index
                    || section.id.history_index.is_none()
                    || anchor.section_id.history_index.is_none())
        };
        let selected = sections
            .iter()
            .find(|section| stable(section))
            .or_else(|| {
                let target = anchor.section_id.history_index?;
                sections
                    .iter()
                    .filter(|section| section.id.history_index.is_some())
                    .min_by_key(|section| {
                        let distance = section.id.history_index.unwrap_or(target).abs_diff(target);
                        (distance, usize::from(section.id.kind != SectionKind::User))
                    })
            })
            .or_else(|| {
                sections
                    .iter()
                    .find(|section| section.id.kind == SectionKind::User)
            })
            .or_else(|| sections.first())?;
        let retained = stable(selected);
        self.copy_ranges
            .iter()
            .filter(|copy| selected.rows.contains(&copy.row) && !copy.decorative)
            .min_by_key(|copy| {
                // Source offsets are raw logical boundaries; rendered Markdown
                // byte lengths cannot define their ends. Retained anchors use
                // the latest preceding boundary (exact empty rows still win).
                // Before the first boundary or in an unloaded section, use nearest.
                (
                    retained && copy.source_offset > anchor.source_offset,
                    copy.source_offset.abs_diff(anchor.source_offset),
                )
            })
            .map(|copy| copy.row)
            .or(Some(selected.rows.start))
    }

    pub fn section_at(&self, row: usize, column: usize) -> Option<SectionView> {
        self.sections
            .iter()
            .find(|section| section.contains_row(row) && section.content_columns.contains(&column))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::UserMessageKindWire;
    use crate::state::transcript::{TranscriptBlock, UserBlock};
    use crate::theme::ThemeKind;
    use crate::ui::{layout, testapp, transcript};
    use ratatui::layout::Rect;

    #[test]
    fn scroll_anchor_keeps_interior_utf8_text_visible_after_widening() {
        const ANCHORED_TEXT: &str = "案，也不要在第一次失败后停止。";
        // At 89 terminal columns the anchor starts the final soft-wrapped
        // row. At 120 it lies late inside a row, nearer the paragraph gap's
        // source offset than the containing row's start.
        let source = format!(
            "{}请给出完整方{ANCHORED_TEXT}\n\n重点保证：JSONL 流式读取、中文与组合字符。",
            "先实现并验证".repeat(13),
        );
        let mut app = testapp::open_empty(ThemeKind::Dark, "ses_1", None, "high");
        let view = app.sessions.known.get_mut("ses_1").unwrap();
        for (index, text) in [
            "前一个用户 section。",
            source.as_str(),
            "后一个用户 section。",
        ]
        .into_iter()
        .enumerate()
        {
            view.transcript.push_block(TranscriptBlock::User(UserBlock {
                index: Some(index),
                loop_id: Some(format!("resize-{index}")),
                kind: UserMessageKindWire::Prompt,
                text: text.into(),
                pending: false,
            }));
        }
        let prepare = |columns| {
            let area = layout::screen_layout(&app, Rect::new(0, 0, columns, 24)).transcript;
            transcript::prepare_conversation(&app, area.width)
        };
        let before = prepare(89);
        let after = prepare(120);
        let section = before
            .sections
            .iter()
            .find(|section| section.id.history_index == Some(1))
            .unwrap();
        let first_visible = before
            .copy_ranges
            .iter()
            .find(|copy| {
                section.rows.contains(&copy.row)
                    && !copy.decorative
                    && copy.text.starts_with(ANCHORED_TEXT)
            })
            .expect("the narrow viewport must start on the paragraph's soft-wrapped tail");
        assert!(
            !before
                .copy_row(first_visible.row - 1)
                .unwrap()
                .hard_break_after
        );
        let anchor = ScrollAnchor {
            section_id: section.id.clone(),
            source_offset: first_visible.source_offset,
            screen_row: 0,
        };
        assert_eq!(anchor.source_offset, source.find(ANCHORED_TEXT).unwrap());
        assert!(source.is_char_boundary(anchor.source_offset));
        assert_eq!(
            before.row_for_scroll_anchor(&anchor),
            Some(first_visible.row)
        );

        let widened = after
            .durable
            .as_ref()
            .unwrap()
            .layout
            .sections
            .iter()
            .find(|placement| placement.layout.key.section == anchor.section_id)
            .unwrap();
        let containing = widened
            .layout
            .source_map
            .rows
            .iter()
            .find(|row| !row.decorative && row.source_range.contains(&anchor.source_offset))
            .expect("real reflow source metadata must still contain the UTF-8 anchor");
        assert!(containing.source_range.start < anchor.source_offset);
        assert!(source[containing.source_range.clone()].contains(ANCHORED_TEXT));
        assert!(after.has_scroll_anchor_section(&anchor));

        let restored = after.row_for_scroll_anchor(&anchor).unwrap();
        assert_eq!(
            after.sections.at_row(restored).unwrap().id,
            anchor.section_id,
            "surrounding section boundaries must not steal the anchor",
        );
        let visible = after
            .window(restored.saturating_sub(anchor.screen_row), 8)
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            visible.contains(ANCHORED_TEXT),
            "widening must keep the previously anchored logical text visible, not scroll past it to the paragraph gap:\n{visible}",
        );
    }

    #[test]
    fn scroll_anchor_keeps_parsed_markdown_text_visible_after_widening() {
        let source = format!("{}TAIL\n\nnext", "**a** ".repeat(45));
        let mut app = testapp::open_empty(ThemeKind::Dark, "ses_1", None, "high");
        let view = app.sessions.known.get_mut("ses_1").unwrap();
        for (index, text) in ["previous section", source.as_str(), "next section"]
            .into_iter()
            .enumerate()
        {
            view.transcript.push_block(TranscriptBlock::User(UserBlock {
                index: Some(index),
                loop_id: Some(format!("markdown-resize-{index}")),
                kind: UserMessageKindWire::Prompt,
                text: text.into(),
                pending: false,
            }));
        }
        let prepare = |columns| {
            let area = layout::screen_layout(&app, Rect::new(0, 0, columns, 24)).transcript;
            transcript::prepare_conversation(&app, area.width)
        };
        let before = prepare(89);
        let after = prepare(120);
        let section = before
            .sections
            .iter()
            .find(|section| section.id.history_index == Some(1))
            .unwrap();
        let first_visible = before
            .copy_ranges
            .iter()
            .find(|copy| section.rows.contains(&copy.row) && copy.text.contains("TAIL"))
            .unwrap();
        let anchor = ScrollAnchor {
            section_id: section.id.clone(),
            source_offset: first_visible.source_offset,
            screen_row: 0,
        };
        // The narrow row containing TAIL begins at a raw Markdown offset,
        // not at an offset into the rendered "a a ... TAIL" copy text.
        assert_eq!(anchor.source_offset, 254);
        assert_eq!(source.find("TAIL"), Some(270));
        assert_eq!(
            before.row_for_scroll_anchor(&anchor),
            Some(first_visible.row)
        );
        let widened_section = after
            .sections
            .iter()
            .find(|section| section.id == anchor.section_id)
            .unwrap();
        let containing = after
            .copy_ranges
            .iter()
            .find(|copy| widened_section.rows.contains(&copy.row) && copy.text.contains("TAIL"))
            .unwrap();
        assert_eq!(containing.source_offset, 2);
        assert!(
            !containing.text.contains("**"),
            "must exercise parsed Markdown"
        );
        assert!(
            containing.source_offset + containing.text.len() < anchor.source_offset,
            "rendered byte length must not be mistaken for a raw source boundary",
        );
        let restored = after.row_for_scroll_anchor(&anchor).unwrap();
        assert_eq!(
            after.sections.at_row(restored).unwrap().id,
            anchor.section_id
        );
        let visible = after
            .window(restored.saturating_sub(anchor.screen_row), 8)
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            visible.contains("TAIL"),
            "widening parsed Markdown must keep the previously anchored logical text visible:\n{visible}",
        );
    }

    #[test]
    fn scroll_anchor_preserves_copy_offset_boundaries_and_fallbacks() {
        let id = SectionId {
            session_id: "ses_1".into(),
            loop_id: Some("fold-target".into()),
            request_index: Some(0),
            kind: SectionKind::Tool,
            ordinal: 0,
            tool_call_id: Some("call-target".into()),
            history_index: Some(7),
        };
        let rows = [
            ("fold hint", 86, true),
            ("中文句", 80, false),
            ("", 89, false),
            ("尾", 100, false),
            ("", 103, true),
            ("neighbor", 86, false),
        ];
        let mut prepared = PreparedConversation {
            header: vec![Line::default(); 10],
            live: rows.iter().map(|(text, _, _)| Line::from(*text)).collect(),
            sections: SectionIndex {
                live: Arc::new(vec![
                    SectionRange {
                        id: id.clone(),
                        rows: 10..15,
                        content_columns: 0..80,
                        collapsible: true,
                        folded: true,
                    },
                    SectionRange {
                        id: SectionId {
                            loop_id: Some("neighbor".into()),
                            request_index: None,
                            kind: SectionKind::User,
                            tool_call_id: None,
                            history_index: Some(8),
                            ..id.clone()
                        },
                        rows: 15..16,
                        content_columns: 0..80,
                        collapsible: false,
                        folded: false,
                    },
                ]),
                ..SectionIndex::default()
            },
            copy_ranges: CopyIndex {
                live_base: 10,
                live: Arc::new(
                    rows.iter()
                        .enumerate()
                        .map(|(row, (text, source_offset, decorative))| CopyRange {
                            row,
                            columns: 0..80,
                            source: Arc::from(*text),
                            // Copy buffers need not use the logical source offsets.
                            source_range: 0..text.len(),
                            source_offset: *source_offset,
                            hard_break_after: true,
                            decorative: *decorative,
                            table_fragments: None,
                        })
                        .collect(),
                ),
                ..CopyIndex::default()
            },
            ..PreparedConversation::default()
        };
        let mut anchor = ScrollAnchor {
            section_id: id,
            source_offset: 86,
            screen_row: 3,
        };
        // For a retained source, 98 is between boundaries 89 and 100. Stay
        // at its predecessor, even if a rendered copy buffer is empty/short.
        for (offset, expected_row) in [
            (0, 11),
            (80, 11),
            (86, 11),
            (89, 12),
            (98, 12),
            (100, 13),
            (103, 13),
        ] {
            anchor.source_offset = offset;
            assert_eq!(prepared.row_for_scroll_anchor(&anchor), Some(expected_row));
        }
        // Unloaded anchors keep the old nearest-row fallback; offsets into
        // the replacement section do not identify the original source text.
        anchor.section_id.loop_id = Some("unloaded".into());
        anchor.source_offset = 86;
        assert!(!prepared.has_scroll_anchor_section(&anchor));
        assert_eq!(prepared.row_for_scroll_anchor(&anchor), Some(12));
        anchor.source_offset = 98;
        assert_eq!(prepared.row_for_scroll_anchor(&anchor), Some(13));
        anchor.section_id.loop_id = Some("fold-target".into());
        Arc::make_mut(&mut prepared.copy_ranges.live).clear();
        assert_eq!(prepared.row_for_scroll_anchor(&anchor), Some(10));
    }
}
