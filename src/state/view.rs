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
    pub session_id: String,
    pub loop_id: Option<String>,
    pub request_index: Option<u32>,
    pub kind: SectionKind,
    pub ordinal: u32,
    pub tool_call_id: Option<String>,
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
    pub text: String,
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
    pub durable: Option<Arc<PreparedDurable>>,
    pub lines: Vec<Line<'static>>,
    pub sections: Vec<SectionRange>,
    pub copy_ranges: Vec<CopyRange>,
    /// Content-cell ranges per rendered line that are inside a markdown
    /// link. Parallel to `lines`; the whole row is empty for non-link lines.
    /// Read-only click arbitration data (RAIL-14 pressedUrl), never changed
    /// by selection or copied as text.
    pub link_cells: Vec<Vec<std::ops::Range<usize>>>,
}

impl PreparedConversation {
    pub fn total_rows(&self) -> usize {
        self.lines.len()
    }

    pub fn section_at(&self, row: usize, column: usize) -> Option<&SectionRange> {
        self.sections
            .iter()
            .find(|section| section.contains_row(row) && section.content_columns.contains(&column))
    }
}

/// Add one rendered section and record the exact rows it occupies. The same
/// helper is used by source comparisons and the eventual click/selection
/// path, so a renderer cannot silently acquire a different height formula.
pub fn append_section(
    conversation: &mut PreparedConversation,
    id: SectionId,
    lines: Vec<Line<'static>>,
    content_columns: std::ops::Range<usize>,
    collapsible: bool,
    folded: bool,
) {
    if lines.is_empty() {
        return;
    }
    let start = conversation.lines.len();
    conversation.lines.extend(lines);
    conversation.sections.push(SectionRange {
        id,
        rows: start..conversation.lines.len(),
        content_columns,
        collapsible,
        folded,
    });
}
