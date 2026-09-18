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

/// Immutable history rows, shared by the installed snapshot and its session.
#[derive(Debug)]
pub struct PreparedDurable {
    pub key: DurableCacheKey,
    pub lines: Vec<Line<'static>>,
    pub sections: Vec<SectionRange>,
    pub copy_ranges: Vec<CopyRange>,
    pub link_cells: Vec<Vec<std::ops::Range<usize>>>,
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
    pub sections: Vec<SectionRange>,
    pub copy_ranges: Vec<CopyRange>,
}

impl PreparedConversation {
    pub fn header_rows(&self) -> usize {
        self.header.len()
    }

    /// Durable rows actually visible in this frame (the skip is excluded).
    pub fn durable_rows(&self) -> usize {
        self.durable.as_ref().map_or(0, |durable| {
            durable.lines.len().saturating_sub(self.durable_skip)
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
                .lines
                .get(self.durable_skip.checked_add(row)?);
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
                .and_then(|durable| durable.link_cells.get(self.durable_skip + row))
                .map_or(EMPTY, Vec::as_slice);
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
        self.sections.clear();
        self.copy_ranges.clear();
    }

    /// Stable identity of the shared historical frame; retention tests compare
    /// it instead of a per-frame buffer address.
    pub fn history_ptr(&self) -> *const PreparedDurable {
        self.durable.as_ref().map_or(std::ptr::null(), Arc::as_ptr)
    }

    /// Copy metadata for one absolute row, if any. `copy_ranges` is ordered by
    /// row, so this is a lookup, not a scan.
    pub fn copy_row(&self, row: usize) -> Option<&CopyRange> {
        self.copy_ranges
            .binary_search_by_key(&row, |copy| copy.row)
            .ok()
            .map(|index| &self.copy_ranges[index])
    }

    pub fn section_at(&self, row: usize, column: usize) -> Option<&SectionRange> {
        self.sections
            .iter()
            .find(|section| section.contains_row(row) && section.content_columns.contains(&column))
    }
}
