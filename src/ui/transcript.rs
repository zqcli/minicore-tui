//! The transcript/history scroll view: durable blocks and the live loop tail (spec r2).

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::text::Span;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::app::App;
use crate::markdown::wrap_plain;
use crate::state::session::SessionView;
use crate::state::tool::{ToolKey, ToolPresentationState};
use crate::state::transcript::{ToolBlock, TranscriptBlock};
use crate::state::view::{
    ConversationLayout, ConversationSelection, CopyIndex, CopyRange, DurableCacheKey, FoldOverride,
    LayoutKey, PreparedConversation, PreparedDurable, SectionId, SectionIndex, SectionKind,
    SectionLayout, SectionRange, SourceMap, SourceRow,
};
use crate::theme::Theme;
use crate::ui::{assistant, header, layout, reasoning, tool, user};

#[derive(Clone)]
pub struct DurableLayoutSnapshot {
    pub session_id: String,
    pub session_epoch: u64,
    pub blocks: Arc<Vec<Arc<TranscriptBlock>>>,
    pub reasoning_folds: Arc<HashMap<crate::state::view::ReasoningKey, FoldOverride>>,
    pub tool_folds: Arc<HashMap<ToolKey, FoldOverride>>,
    pub summary_folds: Arc<HashMap<usize, FoldOverride>>,
    pub tool_presentations: Arc<HashMap<ToolKey, Arc<ToolPresentationState>>>,
    pub tools_expanded: bool,
    pub user_timestamps: Arc<HashMap<usize, String>>,
    pub live_user_timestamp: Option<String>,
    pub live_user_time_accepted: bool,
    pub live_user_loop_id: Option<String>,
    pub live_tool_keys: Arc<HashSet<ToolKey>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DurableLayoutIdentity {
    pub generation: u64,
    pub session_id: String,
    pub session_epoch: u64,
    pub transcript_revision: u64,
    pub width: u16,
    pub theme: crate::theme::ThemeKind,
    pub reasoning_visible: bool,
    pub live_tool_keys: Arc<HashSet<ToolKey>>,
}

pub struct DurableLayoutRequest {
    pub identity: DurableLayoutIdentity,
    pub snapshot: DurableLayoutSnapshot,
    pub previous: Option<Arc<PreparedDurable>>,
    pub viewport: Range<usize>,
    pub cancel: Arc<AtomicBool>,
}

#[derive(Debug)]
pub struct DurableLayoutResult {
    pub identity: DurableLayoutIdentity,
    pub durable: Arc<PreparedDurable>,
    pub changed_sections: usize,
    pub tool_index_lookups: usize,
    pub complete: bool,
}

impl DurableLayoutSnapshot {
    pub fn from_view(view: &SessionView) -> Self {
        Self {
            session_id: view.info.session_id.clone(),
            session_epoch: view.session_epoch,
            blocks: Arc::clone(&view.transcript.blocks),
            reasoning_folds: Arc::clone(&view.reasoning_folds),
            tool_folds: Arc::clone(&view.tool_folds),
            summary_folds: Arc::clone(&view.summary_folds),
            tool_presentations: Arc::clone(&view.tool_presentations),
            tools_expanded: view.tools_expanded,
            user_timestamps: Arc::clone(&view.user_timestamps),
            live_user_timestamp: view.live_user_timestamp.clone(),
            live_user_time_accepted: view.live_user_time_accepted,
            live_user_loop_id: view
                .live
                .as_ref()
                .and_then(|live| live.reference.as_ref())
                .map(|turn| turn.loop_id.clone()),
            live_tool_keys: crate::state::view::live_tool_keys(view),
        }
    }
}

pub(crate) trait DurableLayoutSource {
    fn session_id(&self) -> &str;
    fn blocks(&self) -> &[Arc<TranscriptBlock>];
    fn reasoning_folds(&self) -> &HashMap<crate::state::view::ReasoningKey, FoldOverride>;
    fn tool_folds(&self) -> &HashMap<ToolKey, FoldOverride>;
    fn summary_folds(&self) -> &HashMap<usize, FoldOverride>;
    fn tool_presentations(&self) -> &HashMap<ToolKey, Arc<ToolPresentationState>>;
    fn tools_expanded(&self) -> bool;
    fn user_timestamps(&self) -> &HashMap<usize, String>;
    fn live_user_timestamp(&self) -> Option<&str>;
    fn live_user_time_accepted(&self) -> bool;
    fn live_user_loop_id(&self) -> Option<&str>;
    fn live_tool_owner(&self, key: &ToolKey) -> bool;
}

impl DurableLayoutSource for SessionView {
    fn session_id(&self) -> &str {
        &self.info.session_id
    }

    fn blocks(&self) -> &[Arc<TranscriptBlock>] {
        &self.transcript.blocks
    }

    fn reasoning_folds(&self) -> &HashMap<crate::state::view::ReasoningKey, FoldOverride> {
        self.reasoning_folds.as_ref()
    }

    fn tool_folds(&self) -> &HashMap<ToolKey, FoldOverride> {
        self.tool_folds.as_ref()
    }

    fn summary_folds(&self) -> &HashMap<usize, FoldOverride> {
        &self.summary_folds
    }

    fn tool_presentations(&self) -> &HashMap<ToolKey, Arc<ToolPresentationState>> {
        self.tool_presentations.as_ref()
    }

    fn tools_expanded(&self) -> bool {
        self.tools_expanded
    }

    fn user_timestamps(&self) -> &HashMap<usize, String> {
        self.user_timestamps.as_ref()
    }

    fn live_user_timestamp(&self) -> Option<&str> {
        self.live_user_timestamp.as_deref()
    }

    fn live_user_time_accepted(&self) -> bool {
        self.live_user_time_accepted
    }

    fn live_user_loop_id(&self) -> Option<&str> {
        self.live
            .as_ref()
            .and_then(|live| live.reference.as_ref())
            .map(|turn| turn.loop_id.as_str())
    }

    fn live_tool_owner(&self, key: &ToolKey) -> bool {
        let Some(live) = self.live.as_ref() else {
            return false;
        };
        let loop_id = live
            .reference
            .as_ref()
            .map_or("", |reference| reference.loop_id.as_str());
        key.session_id == self.info.session_id
            && key.loop_id == loop_id
            && live.requests.iter().any(|request| {
                request.request_index == key.request_index
                    && request
                        .tools
                        .iter()
                        .any(|tool| tool.tool_call_id == key.tool_call_id)
            })
    }
}

impl DurableLayoutSource for DurableLayoutSnapshot {
    fn session_id(&self) -> &str {
        &self.session_id
    }

    fn blocks(&self) -> &[Arc<TranscriptBlock>] {
        &self.blocks
    }

    fn reasoning_folds(&self) -> &HashMap<crate::state::view::ReasoningKey, FoldOverride> {
        &self.reasoning_folds
    }

    fn tool_folds(&self) -> &HashMap<ToolKey, FoldOverride> {
        &self.tool_folds
    }

    fn summary_folds(&self) -> &HashMap<usize, FoldOverride> {
        &self.summary_folds
    }

    fn tool_presentations(&self) -> &HashMap<ToolKey, Arc<ToolPresentationState>> {
        self.tool_presentations.as_ref()
    }

    fn tools_expanded(&self) -> bool {
        self.tools_expanded
    }

    fn user_timestamps(&self) -> &HashMap<usize, String> {
        &self.user_timestamps
    }

    fn live_user_timestamp(&self) -> Option<&str> {
        self.live_user_timestamp.as_deref()
    }

    fn live_user_time_accepted(&self) -> bool {
        self.live_user_time_accepted
    }

    fn live_user_loop_id(&self) -> Option<&str> {
        self.live_user_loop_id.as_deref()
    }

    fn live_tool_owner(&self, key: &ToolKey) -> bool {
        self.live_tool_keys.contains(key)
    }
}

/// Builds one complete immutable conversation snapshot. The same rows and
/// metadata are consumed by measurement, rendering, hit testing, selection,
/// and copying; callers install the result through `App::update`.
pub fn prepare_conversation(app: &App, width: u16) -> PreparedConversation {
    prepare_conversation_inner(app, width, None, true)
}

/// The no-session startup screen has no durable transcript. Production uses
/// this small path before a session is selected; active sessions always wait
/// for the owned layout worker.
pub fn prepare_startup_conversation(app: &App, width: u16) -> PreparedConversation {
    debug_assert!(app.active_view().is_none());
    prepare_conversation(app, width)
}

pub fn prepare_conversation_from_cache(
    app: &App,
    width: u16,
    ready_durable: Arc<PreparedDurable>,
) -> PreparedConversation {
    prepare_conversation_inner(app, width, Some(ready_durable), false)
}

/// Compatibility name for test harnesses and older integration fixtures. The
/// production renderer uses [`prepare_conversation_from_cache`] explicitly.
pub fn prepare_conversation_with_durable(
    app: &App,
    width: u16,
    ready_durable: Arc<PreparedDurable>,
) -> PreparedConversation {
    prepare_conversation_from_cache(app, width, ready_durable)
}

fn prepare_conversation_inner(
    app: &App,
    width: u16,
    ready_durable: Option<Arc<PreparedDurable>>,
    allow_durable_build: bool,
) -> PreparedConversation {
    let theme = app.theme.theme();
    let durable = app.active_view().map(|view| {
        let key = DurableCacheKey::new(view, width, app.theme, app.reasoning_visible);
        if let Some(ready) = ready_durable.as_ref().filter(|ready| ready.key == key) {
            return Arc::clone(ready);
        }
        if let Some(cached) = view
            .transcript
            .render_cache
            .as_ref()
            .filter(|cached| cached.key == key)
        {
            return Arc::clone(cached);
        }
        assert!(
            allow_durable_build,
            "production layout must install a worker-produced durable layout"
        );
        // A cache miss is the only point that rebuilds the durable layout;
        // a cache hit below must not count (spec §25.1).
        let previous = view.transcript.render_cache.as_deref();
        let (layout, changed_sections, tool_index_lookups) = build_durable_layout(
            &theme,
            app.theme,
            view,
            width,
            app.reasoning_visible,
            previous,
            0..0,
            None,
            None,
        )
        .expect("synchronous test layout cannot be cancelled");
        crate::perf::add(crate::perf::Counter::LayoutCalls, changed_sections as u64);
        crate::perf::add(
            crate::perf::Counter::ToolIndexLookups,
            tool_index_lookups as u64,
        );
        Arc::new(PreparedDurable { key, layout })
    });
    let header = header::lines(&theme, app);
    let header_rows = header.len();
    // The shared durable frame is never copied: the frame only records how
    // many leading rows the header boundary drops, and the live tail starts
    // after it.
    let durable_skip = usize::from(
        header.last().is_some_and(layout::line_is_blank)
            && durable
                .as_ref()
                .and_then(|durable| durable.layout.row(0))
                .is_some_and(layout::line_is_blank),
    );
    let durable_rows = durable.as_ref().map_or(0, |durable| {
        durable.layout.total_rows.saturating_sub(durable_skip)
    });
    let durable_last_row_blank = durable
        .as_ref()
        .and_then(|durable| {
            durable
                .layout
                .row(durable.layout.total_rows.saturating_sub(1))
        })
        .map_or_else(
            || header.last().is_some_and(layout::line_is_blank),
            |line| layout::line_is_blank(line),
        );
    let last_kind = durable.as_ref().and_then(|durable| {
        durable
            .layout
            .sections
            .last()
            .map(|section| section.layout.key.section.kind)
    });
    let mut live_sections = Vec::new();
    let durable_tool_keys = durable
        .as_ref()
        .map(|durable| durable.layout.tool_keys.as_ref());
    let (mut live, mut live_links) = build_live_tail(
        &theme,
        app,
        width as usize,
        last_kind,
        durable_tool_keys,
        Some(&mut live_sections),
    );
    // One boundary blank can be shared between the durable block and the
    // first live row; the builders record rows with that row still present,
    // so the frame shifts live rows by one less when it is dropped.
    let live_skip =
        usize::from(durable_last_row_blank && live.first().is_some_and(layout::line_is_blank));
    if live_skip == 1 {
        live.remove(0);
        live_links.remove(0);
    }
    // While the busy status row is visible, exactly one clear transparent
    // blank must separate the last *frame* row (which may be the last durable
    // row when the live tail is empty) from it. Sections that already end with
    // a transparent blank must not get a second one; this row belongs to no
    // section, so ranges/copy/hits exclude it consistently.
    let last_frame_blank = match (live.last(), durable_rows, header.last()) {
        (Some(line), ..) => layout::line_is_blank(line),
        (None, rows, _) if rows > 0 => durable_last_row_blank,
        (None, _, Some(line)) => layout::line_is_blank(line),
        (None, _, None) => true,
    };
    if layout::busy(app) && !last_frame_blank {
        live.push(Line::default());
        live_links.push(Vec::new());
    }
    let live_base = header_rows + durable_rows - live_skip;
    let copy_start_for_live: Vec<(std::ops::Range<usize>, usize)> = live_sections
        .iter()
        .map(|section| (section.rows.clone(), copy_start_for_kind(&section.id.kind)))
        .collect();
    let mut live_source = String::new();
    let mut live_copy_meta = Vec::new();
    for (rows, copy_start) in &copy_start_for_live {
        let section_source_start = live_source.len();
        for row in rows.clone() {
            let section = live_sections
                .iter()
                .find(|section| section.rows.contains(&row))
                .expect("live copy row belongs to a live section");
            let text = section_copy_text(section, row, &live, *copy_start);
            let start = live_source.len();
            live_source.push_str(&text);
            let end = live_source.len();
            live_copy_meta.push((
                row,
                *copy_start,
                start..end,
                start.saturating_sub(section_source_start),
                section_copy_is_decorative(section, row, &text),
            ));
            live_source.push('\n');
        }
    }
    let live_source: Arc<str> = live_source.into();
    let live_copy: Vec<CopyRange> = live_copy_meta
        .into_iter()
        .map(
            |(row, copy_start, source_range, source_offset, decorative)| CopyRange {
                row,
                columns: copy_start..width as usize,
                source: Arc::clone(&live_source),
                source_offset,
                source_range,
                hard_break_after: true,
                decorative,
            },
        )
        .collect();
    let live_sections: Vec<SectionRange> = live_sections
        .into_iter()
        .map(|mut section| {
            section.rows = section.rows.start + live_base..section.rows.end + live_base;
            section
        })
        .collect();
    PreparedConversation {
        width,
        session_id: app.active_view().map(|view| view.info.session_id.clone()),
        transcript_revision: app
            .active_view()
            .map_or(0, |view| view.transcript.render_revision),
        durable: durable.clone(),
        durable_skip,
        header,
        header_links: vec![Vec::new(); header_rows],
        live,
        live_links,
        sections: SectionIndex {
            durable: durable.as_ref().map(|durable| Arc::clone(&durable.layout)),
            durable_base: header_rows,
            durable_skip,
            live: Arc::new(live_sections),
        },
        copy_ranges: CopyIndex {
            durable: durable.as_ref().map(|durable| Arc::clone(&durable.layout)),
            durable_base: header_rows,
            durable_skip,
            live_base,
            live: Arc::new(live_copy),
        },
    }
}

fn section_revision<V: DurableLayoutSource>(view: &V, id: &SectionId, block_revision: u64) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    id.hash(&mut hasher);
    block_revision.hash(&mut hasher);
    if let Some(tool_call_id) = id.tool_call_id.as_deref() {
        if let Some(presentation) = view.tool_presentations().get(&ToolKey::new(
            view.session_id(),
            id.loop_id.as_deref().unwrap_or_default(),
            id.request_index.unwrap_or_default(),
            tool_call_id,
        )) {
            presentation.display.detail.hash(&mut hasher);
            presentation.display.expanded_input.hash(&mut hasher);
            presentation.display.hidden_line_count.hash(&mut hasher);
            presentation.display.truncated.hash(&mut hasher);
        }
    }
    if id.kind == SectionKind::User {
        id.history_index
            .and_then(|index| view.user_timestamps().get(&index))
            .hash(&mut hasher);
    }
    hasher.finish()
}

fn block_content_revision(block: &TranscriptBlock) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::mem::discriminant(block).hash(&mut hasher);
    match block {
        TranscriptBlock::User(block) => {
            block.index.hash(&mut hasher);
            block.loop_id.hash(&mut hasher);
            std::mem::discriminant(&block.kind).hash(&mut hasher);
            block.text.hash(&mut hasher);
            block.pending.hash(&mut hasher);
        }
        TranscriptBlock::Assistant(block) => {
            block.index.hash(&mut hasher);
            block.loop_id.hash(&mut hasher);
            block.request_index.hash(&mut hasher);
            for part in &block.parts {
                std::mem::discriminant(part).hash(&mut hasher);
                match part {
                    crate::state::transcript::AssistantPart::Text(text)
                    | crate::state::transcript::AssistantPart::Reasoning(text) => {
                        text.hash(&mut hasher)
                    }
                    crate::state::transcript::AssistantPart::ToolCall(call) => {
                        call.tool_call_id.hash(&mut hasher);
                        call.name.hash(&mut hasher);
                        call.call_index.hash(&mut hasher);
                    }
                }
            }
            block.finish_reason.hash(&mut hasher);
            block.terminal_error.hash(&mut hasher);
        }
        TranscriptBlock::Tool(block) => {
            block.index.hash(&mut hasher);
            block.loop_id.hash(&mut hasher);
            block.request_index.hash(&mut hasher);
            block.tool_call_id.hash(&mut hasher);
            block.name.hash(&mut hasher);
            block.result.hash(&mut hasher);
            block
                .outcome
                .map(|outcome| std::mem::discriminant(&outcome))
                .hash(&mut hasher);
            block
                .live_status
                .map(|status| std::mem::discriminant(&status))
                .hash(&mut hasher);
            block.progress.hash(&mut hasher);
            block.expanded.hash(&mut hasher);
        }
        TranscriptBlock::Summary(block) => {
            block.index.hash(&mut hasher);
            block.content.hash(&mut hasher);
        }
        TranscriptBlock::HistoryPlaceholder(block) => {
            block.index.hash(&mut hasher);
            block.total_bytes.hash(&mut hasher);
        }
    }
    hasher.finish()
}

/// Returns the transcript content rows available in `height`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ScrollPosition {
    pub offset: usize,
    pub visible_rows: usize,
    pub marker: bool,
}

pub(crate) fn scroll_position(app: &App, total: usize, height: usize) -> ScrollPosition {
    if height == 0 {
        return ScrollPosition {
            offset: 0,
            visible_rows: 0,
            marker: false,
        };
    }
    let Some(view) = app.active_view() else {
        return ScrollPosition {
            offset: total.saturating_sub(height),
            visible_rows: total.min(height),
            marker: false,
        };
    };
    let marker = !view.scroll.follow_tail && total > height;
    let visible_rows = height;
    let max_offset = total.saturating_sub(visible_rows.max(1));
    let offset = if view.scroll.follow_tail {
        max_offset
    } else {
        view.scroll.offset.min(max_offset)
    };
    ScrollPosition {
        offset,
        visible_rows: visible_rows.min(total),
        marker,
    }
}

pub fn visible_rows(app: &App, total_lines: usize, height: u16) -> usize {
    scroll_position(app, total_lines, height as usize).visible_rows
}

/// Pure measure for the main loop: the wrapped transcript line count at `width`.
pub fn total_lines(app: &App, width: u16) -> usize {
    if app.async_layout_enabled() {
        return app
            .prepared_conversation(width)
            .map_or(0, PreparedConversation::total_rows);
    }
    app.prepared_conversation(width).map_or_else(
        || prepare_conversation(app, width).total_rows(),
        PreparedConversation::total_rows,
    )
}

/// Builds every transcript row (startup header, durable blocks, live tail).
pub fn all_lines(_theme: &Theme, app: &App, width: usize) -> Vec<Line<'static>> {
    if app.async_layout_enabled() {
        return app
            .prepared_conversation(width as u16)
            .map_or_else(Vec::new, PreparedConversation::lines);
    }
    app.prepared_conversation(width as u16).map_or_else(
        || prepare_conversation(app, width as u16).lines(),
        PreparedConversation::lines,
    )
}

/// Per rendered line, the content-cell ranges inside a markdown link.
type LinkRow = Vec<std::ops::Range<usize>>;

fn layout_block_order<V: DurableLayoutSource>(
    view: &V,
    viewport: Range<usize>,
    previous: Option<&PreparedDurable>,
) -> Vec<usize> {
    let count = view.blocks().len();
    let mut target = viewport.start / 4;
    if let Some(previous) = previous {
        if let Some(index) = previous
            .layout
            .sections
            .iter()
            .find(|placement| {
                placement.rows.start < viewport.end && placement.rows.end > viewport.start
            })
            .and_then(|placement| placement.layout.key.section.history_index)
        {
            target = view
                .blocks()
                .iter()
                .position(|block| block.index() == Some(index))
                .unwrap_or(target);
        }
    }
    if target >= count || viewport.start == 0 {
        return (0..count).collect();
    }
    let mut order = Vec::with_capacity(count);
    order.push(target);
    for distance in 1..count {
        if let Some(index) = target.checked_sub(distance) {
            order.push(index);
        }
        if let Some(index) = target.checked_add(distance).filter(|index| *index < count) {
            order.push(index);
        }
        if order.len() == count {
            break;
        }
    }
    order
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_durable_layout<V: DurableLayoutSource>(
    theme: &Theme,
    theme_kind: crate::theme::ThemeKind,
    view: &V,
    width: u16,
    reasoning_visible: bool,
    previous: Option<&PreparedDurable>,
    viewport: Range<usize>,
    cancel: Option<&AtomicBool>,
    mut batch_sink: Option<&mut dyn FnMut(Vec<Arc<SectionLayout>>) -> bool>,
) -> Option<(Arc<ConversationLayout>, usize, usize)> {
    let mut tool_index: HashMap<(&str, u32, &str), &ToolBlock> = HashMap::new();
    let block_revisions: HashMap<usize, u64> = view
        .blocks()
        .iter()
        .filter_map(|block| {
            block
                .index()
                .map(|index| (index, block_content_revision(block)))
        })
        .collect();
    let mut assistant_tool_keys = HashSet::new();
    for block in view.blocks() {
        match block.as_ref() {
            TranscriptBlock::Tool(tool) => {
                tool_index.insert(
                    (
                        tool.loop_id.as_str(),
                        tool.request_index,
                        tool.tool_call_id.as_str(),
                    ),
                    tool,
                );
            }
            TranscriptBlock::Assistant(assistant) => {
                for part in &assistant.parts {
                    if let crate::state::transcript::AssistantPart::ToolCall(call) = part {
                        assistant_tool_keys.insert(ToolKey::new(
                            view.session_id(),
                            &assistant.loop_id,
                            assistant.request_index,
                            &call.tool_call_id,
                        ));
                    }
                }
            }
            _ => {}
        }
    }
    let cached: HashMap<LayoutKey, Arc<SectionLayout>> = previous
        .map(|previous| {
            previous
                .layout
                .sections
                .iter()
                .map(|placement| (placement.layout.key.clone(), Arc::clone(&placement.layout)))
                .collect()
        })
        .unwrap_or_default();
    let mut sections = Vec::new();
    let mut pending_batch = Vec::new();
    let mut changed = 0;
    let mut tool_index_lookups = 0;

    let block_order = layout_block_order(view, viewport, previous);
    for ordinal in block_order {
        let block = &view.blocks()[ordinal];
        if cancel.is_some_and(|cancel| cancel.load(std::sync::atomic::Ordering::Relaxed)) {
            return None;
        }
        let block_revision = block
            .index()
            .and_then(|index| block_revisions.get(&index).copied())
            .unwrap_or_else(|| block_content_revision(block));
        if let TranscriptBlock::Assistant(assistant_block) = block.as_ref() {
            for (section_offset, input) in assistant::section_inputs(
                assistant_block,
                reasoning_visible,
                view.reasoning_folds(),
            )
            .into_iter()
            .enumerate()
            {
                if let Some(call) = &input.tool_call {
                    let tool = match tool_index.get(&(
                        assistant_block.loop_id.as_str(),
                        assistant_block.request_index,
                        call.tool_call_id.as_str(),
                    )) {
                        Some(tool) => {
                            tool_index_lookups += 1;
                            (*tool).clone()
                        }
                        None => {
                            let key = ToolKey::new(
                                view.session_id(),
                                &assistant_block.loop_id,
                                assistant_block.request_index,
                                &call.tool_call_id,
                            );
                            if view.live_tool_owner(&key) {
                                // A real live Tool owns the current status/result;
                                // do not let a history-only marker mask it.
                                continue;
                            }
                            fallback_tool_block(view, assistant_block, call)
                        }
                    };
                    let id = SectionId {
                        session_id: view.session_id().into(),
                        loop_id: Some(tool.loop_id.clone().into()),
                        request_index: Some(tool.request_index),
                        kind: SectionKind::Tool,
                        ordinal: 0,
                        tool_call_id: Some(tool.tool_call_id.clone().into()),
                        history_index: Some(assistant_block.index),
                    };
                    let folded = !effective_tool_expanded_for(view, &tool);
                    let key = LayoutKey {
                        section: id.clone(),
                        revision: section_revision(view, &id, block_revision),
                        width,
                        theme: theme_kind,
                        folded,
                        reasoning_visible: true,
                    };
                    if let Some(layout) = cached.get(&key) {
                        if !push_layout_section(
                            Arc::clone(layout),
                            &mut sections,
                            &mut pending_batch,
                            &mut batch_sink,
                        ) {
                            return None;
                        }
                        continue;
                    }
                    let source_hint = tool
                        .result
                        .as_deref()
                        .unwrap_or(tool.name.as_str())
                        .to_owned();
                    let lines = durable_block_lines(
                        theme,
                        view,
                        &TranscriptBlock::Tool(tool),
                        width as usize,
                        reasoning_visible,
                    );
                    if let Some(layout) = make_section_layout(
                        key,
                        lines,
                        Vec::new(),
                        true,
                        folded,
                        ordinal.saturating_mul(1_000_000) + section_offset,
                        Some(source_hint.as_str()),
                        None,
                        None,
                    ) {
                        if !push_layout_section(
                            layout,
                            &mut sections,
                            &mut pending_batch,
                            &mut batch_sink,
                        ) {
                            return None;
                        }
                        changed += 1;
                    }
                    continue;
                }

                let id = SectionId {
                    session_id: view.session_id().into(),
                    loop_id: Some(assistant_block.loop_id.clone().into()),
                    request_index: Some(assistant_block.request_index),
                    kind: input.kind,
                    ordinal: input.ordinal,
                    tool_call_id: None,
                    history_index: Some(assistant_block.index),
                };
                let key = LayoutKey {
                    section: id.clone(),
                    revision: section_revision(view, &id, block_revision),
                    width,
                    theme: theme_kind,
                    folded: input.folded,
                    reasoning_visible: input.kind == SectionKind::Thinking && reasoning_visible,
                };
                if let Some(layout) = cached.get(&key) {
                    if !push_layout_section(
                        Arc::clone(layout),
                        &mut sections,
                        &mut pending_batch,
                        &mut batch_sink,
                    ) {
                        return None;
                    }
                    continue;
                }
                let rendered =
                    assistant::render_section(theme, &input, width as usize, reasoning_visible);
                if let Some(layout) = make_section_layout(
                    key,
                    rendered.lines,
                    rendered.link_cells,
                    rendered.collapsible,
                    rendered.folded,
                    ordinal.saturating_mul(1_000_000) + section_offset,
                    Some(input.source.as_ref()),
                    rendered.hard_breaks.as_deref(),
                    rendered.copy_cells.as_deref(),
                ) {
                    if !push_layout_section(
                        layout,
                        &mut sections,
                        &mut pending_batch,
                        &mut batch_sink,
                    ) {
                        return None;
                    }
                    changed += 1;
                }
            }
            continue;
        }

        if let TranscriptBlock::Tool(tool) = block.as_ref() {
            let tool_key = ToolKey::new(
                view.session_id(),
                &tool.loop_id,
                tool.request_index,
                &tool.tool_call_id,
            );
            if assistant_tool_keys.contains(&tool_key) {
                continue;
            }
        }
        let id = section_id(view.session_id(), block, ordinal as u32);
        let folded = matches!(block.as_ref(), TranscriptBlock::Tool(tool) if {
            let key = ToolKey::new(
                view.session_id(),
                &tool.loop_id,
                tool.request_index,
                &tool.tool_call_id,
            );
            matches!(view.tool_folds().get(&key), Some(FoldOverride::Collapsed))
                || !effective_tool_expanded_for(view, tool)
        }) || matches!(block.as_ref(), TranscriptBlock::Summary(summary)
            if !view.summary_folds().get(&summary.index).is_some_and(FoldOverride::expanded));
        let key = LayoutKey {
            section: id.clone(),
            revision: section_revision(view, &id, block_revision),
            width,
            theme: theme_kind,
            folded,
            reasoning_visible: true,
        };
        if let Some(layout) = cached.get(&key) {
            if !push_layout_section(
                Arc::clone(layout),
                &mut sections,
                &mut pending_batch,
                &mut batch_sink,
            ) {
                return None;
            }
            continue;
        }
        let (lines, links, breaks, copies) =
            if let TranscriptBlock::Summary(summary) = block.as_ref() {
                let (lines, links, breaks) =
                    compaction_summary_lines(theme, width as usize, &summary.content, folded);
                (lines, links, breaks, Vec::new())
            } else if let TranscriptBlock::User(user) = block.as_ref()
                && user.text.len() <= crate::limits::LAYOUT_SECTION_BYTES
            {
                let rendered = durable_user_rows(theme, view, user, width as usize);
                (
                    rendered.lines,
                    rendered.link_cells,
                    rendered.hard_breaks,
                    rendered.copy_cells,
                )
            } else {
                (
                    durable_block_lines(theme, view, block, width as usize, reasoning_visible),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                )
            };
        let collapsible = matches!(
            block.as_ref(),
            TranscriptBlock::Tool(_) | TranscriptBlock::Summary(_)
        );
        if let Some(layout) = make_section_layout(
            key,
            lines,
            links,
            collapsible,
            folded,
            ordinal.saturating_mul(1_000_000),
            block_source(block),
            (!breaks.is_empty()).then_some(breaks.as_slice()),
            (!copies.is_empty()).then_some(copies.as_slice()),
        ) {
            if !push_layout_section(layout, &mut sections, &mut pending_batch, &mut batch_sink) {
                return None;
            }
            changed += 1;
        }
    }
    if let Some(sink) = batch_sink.as_mut() {
        if !pending_batch.is_empty() && !sink(std::mem::take(&mut pending_batch)) {
            return None;
        }
        if !sink(Vec::new()) {
            return None;
        }
    }
    Some((
        Arc::new(ConversationLayout::from_sections(sections)),
        changed,
        tool_index_lookups,
    ))
}

const LAYOUT_BATCH_SECTIONS: usize = 64;

fn block_source(block: &TranscriptBlock) -> Option<&str> {
    match block {
        TranscriptBlock::User(block) => Some(block.text.as_str()),
        TranscriptBlock::Tool(block) => block.result.as_deref().or(Some(block.name.as_str())),
        TranscriptBlock::Summary(block) => Some(block.content.as_str()),
        TranscriptBlock::Assistant(_) | TranscriptBlock::HistoryPlaceholder(_) => None,
    }
}

fn fallback_tool_block<V: DurableLayoutSource>(
    view: &V,
    assistant: &crate::state::transcript::AssistantBlock,
    call: &crate::protocol::ToolCallViewWire,
) -> ToolBlock {
    let key = ToolKey::new(
        view.session_id(),
        &assistant.loop_id,
        assistant.request_index,
        &call.tool_call_id,
    );
    let facts = view.tool_presentations().get(&key);
    let name = facts
        .map(|facts| facts.display.detail.as_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(call.name.as_str())
        .to_owned();
    ToolBlock {
        index: None,
        loop_id: assistant.loop_id.clone(),
        request_index: assistant.request_index,
        tool_call_id: call.tool_call_id.clone(),
        name,
        result: facts.and_then(|facts| facts.result.clone()),
        outcome: facts.and_then(|facts| facts.outcome),
        live_status: facts.map(|facts| facts.status),
        progress: None,
        expanded: false,
    }
}

fn push_layout_section(
    section: Arc<SectionLayout>,
    sections: &mut Vec<Arc<SectionLayout>>,
    batch: &mut Vec<Arc<SectionLayout>>,
    sink: &mut Option<&mut dyn FnMut(Vec<Arc<SectionLayout>>) -> bool>,
) -> bool {
    if let Some(sink) = sink.as_mut() {
        batch.push(section);
        if batch.len() >= LAYOUT_BATCH_SECTIONS {
            return sink(std::mem::take(batch));
        }
    } else {
        sections.push(section);
    }
    true
}

#[allow(clippy::too_many_arguments)]
fn make_section_layout(
    key: LayoutKey,
    lines: Vec<Line<'static>>,
    mut link_cells: Vec<Vec<std::ops::Range<usize>>>,
    collapsible: bool,
    folded: bool,
    order: usize,
    source_hint: Option<&str>,
    rendered_breaks: Option<&[bool]>,
    rendered_copy_cells: Option<&[Option<crate::markdown::CopyCells>]>,
) -> Option<Arc<SectionLayout>> {
    if lines.is_empty() {
        return None;
    }
    link_cells.resize_with(lines.len(), Vec::new);
    // A Summary section without a logical source is the explicit large-item
    // placeholder. It is visible as a bounded notice, but it is not a
    // complete source range and must never enter copy/selection text.
    let copyable = !(key.section.kind == SectionKind::Summary && source_hint.is_none());
    let range = SectionRange {
        id: key.section.clone(),
        rows: 0..lines.len(),
        content_columns: content_columns_for_kind(&key.section.kind, key.width as usize),
        collapsible,
        folded,
    };
    let row_texts = (0..lines.len())
        .map(|row| {
            let copy = rendered_copy_cells
                .and_then(|rows| rows.get(row))
                .and_then(Option::as_ref);
            let text = match copy {
                Some(copy) if copy.decorative => String::new(),
                Some(copy) => slice_cell_range(
                    &line_copy_text(&lines[row], 0),
                    copy.columns.start,
                    copy.columns.end,
                ),
                None if rendered_copy_cells.is_some() => {
                    line_copy_text(&lines[row], range.content_columns.start)
                }
                None => section_copy_text(&range, row, &lines, range.content_columns.start),
            };
            let decorative = !copyable
                || copy.is_some_and(|copy| copy.decorative)
                || (text.is_empty() && (row == 0 || row + 1 == lines.len()))
                || (rendered_copy_cells.is_none()
                    && section_copy_is_decorative(&range, row, &text));
            (text, decorative)
        })
        .collect::<Vec<_>>();
    let source: Arc<str> = source_hint.map(Arc::<str>::from).unwrap_or_else(|| {
        row_texts
            .iter()
            .map(|(text, _)| text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
            .into()
    });
    let last_content_row = row_texts
        .iter()
        .enumerate()
        .rev()
        .find(|(_, (text, decorative))| !*decorative && !text.is_empty())
        .map_or(0, |(row, _)| row);
    // Markdown sections know exactly which rendered row ends a logical source
    // line, so a soft wrap inside a paragraph or code line never becomes a
    // fake newline. Rows rendered from plain text fall back to aligning the
    // source lines with their wrapped rows.
    let hard_break_rows = match rendered_breaks {
        Some(breaks) => {
            let mut rows: Vec<bool> = breaks.to_vec();
            rows.resize(row_texts.len(), false);
            for (row, (_text, decorative)) in row_texts.iter().enumerate() {
                if *decorative {
                    rows[row] = false;
                }
            }
            if last_content_row < rows.len() {
                rows[last_content_row] = true;
            }
            rows
        }
        None => hard_break_rows(
            source_hint,
            &row_texts,
            range
                .content_columns
                .end
                .saturating_sub(range.content_columns.start),
            last_content_row,
        ),
    };
    let logical_ranges = source_ranges(source_hint, &row_texts, &hard_break_rows);
    let copy_source: Arc<str> = row_texts
        .iter()
        .map(|(text, _)| text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
        .into();
    let copy_ranges_in_source = source_ranges(None, &row_texts, &hard_break_rows);
    let copy_ranges: Vec<CopyRange> = row_texts
        .into_iter()
        .enumerate()
        .map(|(row, (_text, decorative))| CopyRange {
            row,
            columns: rendered_copy_cells
                .and_then(|rows| rows.get(row))
                .and_then(Option::as_ref)
                .map_or_else(
                    || range.content_columns.clone(),
                    |copy| copy.columns.clone(),
                ),
            source: Arc::clone(&copy_source),
            source_offset: logical_ranges.get(row).map_or(0, |range| range.start),
            source_range: copy_ranges_in_source.get(row).cloned().unwrap_or(0..0),
            hard_break_after: hard_break_rows.get(row).copied().unwrap_or(false),
            decorative,
        })
        .collect();
    let source_map = Arc::new(SourceMap {
        source: Arc::clone(&source),
        rows: Arc::new(
            logical_ranges
                .into_iter()
                .enumerate()
                .map(|(row, source_range)| SourceRow {
                    source_range,
                    hard_break_after: hard_break_rows.get(row).copied().unwrap_or(false),
                    decorative: copy_ranges.get(row).is_none_or(|copy| copy.decorative),
                })
                .collect(),
        ),
    });
    Some(Arc::new(SectionLayout {
        key,
        order,
        rows: Arc::new(lines),
        source,
        source_map,
        copy_ranges: Arc::new(copy_ranges),
        link_cells: Arc::new(link_cells),
        content_columns: range.content_columns,
        collapsible,
        folded,
    }))
}

fn source_ranges(
    source_hint: Option<&str>,
    rows: &[(String, bool)],
    hard_breaks: &[bool],
) -> Vec<Range<usize>> {
    let Some(source) = source_hint else {
        let mut offset = 0usize;
        return rows
            .iter()
            .map(|(text, _)| {
                let range = offset..offset.saturating_add(text.len());
                offset = offset.saturating_add(text.len()).saturating_add(1);
                range
            })
            .collect();
    };
    let logical_lines: Vec<(usize, usize, usize)> = source
        .split_inclusive('\n')
        .scan(0usize, |offset, line| {
            let start = *offset;
            *offset = offset.saturating_add(line.len());
            let end = start + line.trim_end_matches('\n').len();
            Some((start, end, *offset))
        })
        .collect();
    let mut line = 0usize;
    let mut cursor = logical_lines.first().map_or(0, |entry| entry.0);
    rows.iter()
        .enumerate()
        .map(|(row, (text, decorative))| {
            if *decorative {
                return cursor..cursor;
            }
            let Some((line_start, line_end, next_line)) = logical_lines.get(line).copied() else {
                return source.len()..source.len();
            };
            if text.is_empty() {
                let range = cursor..cursor;
                if hard_breaks.get(row).copied().unwrap_or(false) {
                    line = line.saturating_add(1);
                    cursor = next_line;
                }
                return range;
            }
            cursor = cursor.max(line_start).min(line_end);
            let start = cursor;
            let wanted = text.len().min(line_end.saturating_sub(cursor));
            let mut end = cursor.saturating_add(wanted);
            while end > cursor && !source.is_char_boundary(end) {
                end -= 1;
            }
            if hard_breaks.get(row).copied().unwrap_or(false) {
                end = line_end;
                line = line.saturating_add(1);
                cursor = next_line;
            } else {
                cursor = end;
            }
            start..end
        })
        .collect()
}

fn hard_break_rows(
    source: Option<&str>,
    rows: &[(String, bool)],
    width: usize,
    last_content_row: usize,
) -> Vec<bool> {
    let mut breaks = vec![false; rows.len()];
    let content_rows: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter_map(|(row, (_text, decorative))| (!*decorative).then_some(row))
        .collect();
    let Some(source) = source else {
        if last_content_row < breaks.len() {
            breaks[last_content_row] = true;
        }
        return breaks;
    };
    if !source.contains('\n') {
        if last_content_row < breaks.len() {
            breaks[last_content_row] = true;
        }
        return breaks;
    }
    let mut cursor = 0;
    for logical_line in source.split('\n') {
        if cursor >= content_rows.len() {
            break;
        }
        let visual_rows = wrap_plain(logical_line, width.max(1), Style::default())
            .len()
            .max(1);
        let end = cursor
            .saturating_add(visual_rows)
            .saturating_sub(1)
            .min(content_rows.len().saturating_sub(1));
        breaks[content_rows[end]] = true;
        cursor = end.saturating_add(1);
    }
    if last_content_row < breaks.len() {
        breaks[last_content_row] = true;
    }
    breaks
}

fn needs_user_gap(previous: Option<SectionKind>, current: SectionKind) -> bool {
    previous == Some(SectionKind::User) && current == SectionKind::User
}

/// The single decorative row inside a section whose content never enters a
/// copied selection: the fold hint of a collapsed Tool/Thinking section sits
/// at the row before the closing blank (end-2), and the accepted-steer
/// awaiting-history marker is the last row of a provisional user card
/// (kind User with no durable history index). Blank boundary rows are
/// handled separately by the callers.
fn decorative_row(section: &SectionRange) -> usize {
    match section.id.kind {
        SectionKind::Tool | SectionKind::Thinking if section.folded => {
            section.rows.end.saturating_sub(2)
        }
        SectionKind::User if section.id.history_index.is_none() => {
            section.rows.end.saturating_sub(1)
        }
        _ => usize::MAX,
    }
}

fn section_copy_text(
    section: &SectionRange,
    row: usize,
    lines: &[Line<'static>],
    copy_start: usize,
) -> String {
    if row == decorative_row(section)
        || (section.id.kind == SectionKind::Summary
            && section.collapsible
            && (row == section.rows.start + 1 || section.folded))
    {
        return String::new();
    }
    lines
        .get(row)
        .map(|line| line_copy_text(line, copy_start))
        .unwrap_or_default()
}

fn section_copy_is_decorative(section: &SectionRange, row: usize, text: &str) -> bool {
    row == decorative_row(section)
        || (section.id.kind == SectionKind::Summary
            && section.collapsible
            && (row == section.rows.start + 1 || section.folded))
        || (text.is_empty() && (row == section.rows.start || row + 1 == section.rows.end))
}

fn line_copy_text(line: &Line<'_>, mut skip: usize) -> String {
    let mut text = String::new();
    for span in &line.spans {
        for character in span.content.chars() {
            let width = crate::markdown::char_width(character);
            if skip > 0 {
                if width <= skip {
                    skip -= width;
                    continue;
                }
                skip = 0;
            }
            text.push(character);
        }
    }
    text.trim_end().to_owned()
}

/// Returns only the selected content cells. Decorative rail cells and filled
/// right padding are absent from `CopyRange.text`; empty boundary rows are
/// trimmed so section spacers do not become copied newlines.
pub fn selection_text(
    conversation: &PreparedConversation,
    selection: &ConversationSelection,
) -> String {
    if conversation.session_id.as_deref() != Some(selection.session_id.as_str())
        || selection.is_empty()
    {
        return String::new();
    }
    let (start, focus) = selection.ordered_points();
    let end_row = focus.row;
    let mut rows = Vec::new();
    let mut hard_break = false;
    for row in start.row..=end_row {
        let Some(copy) = conversation.copy_ranges.iter().find(|copy| copy.row == row) else {
            continue;
        };
        if copy.decorative {
            continue;
        }
        let start_column = if row == start.row {
            start.column.saturating_sub(copy.columns.start)
        } else {
            0
        };
        let end_column = if row == end_row {
            focus
                .column
                .saturating_add(1)
                .saturating_sub(copy.columns.start)
        } else {
            copy.columns.end.saturating_sub(copy.columns.start)
        };
        if !rows.is_empty() && hard_break {
            rows.push("\n".to_owned());
        }
        rows.push(slice_cell_range(copy.text, start_column, end_column));
        hard_break = copy.hard_break_after;
    }
    while rows.first().is_some_and(|row| row.is_empty()) {
        rows.remove(0);
    }
    while rows.last().is_some_and(|row| row.is_empty()) {
        rows.pop();
    }
    rows.concat()
}

fn slice_cell_range(text: &str, start: usize, end: usize) -> String {
    if end <= start {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for grapheme in text.graphemes(true) {
        let width = UnicodeWidthStr::width(grapheme);
        let next = used + width;
        if next > start && used < end {
            out.push_str(grapheme);
        }
        used = next;
        if used >= end {
            break;
        }
    }
    out
}

fn apply_selection(
    lines: Vec<Line<'static>>,
    row_offset: usize,
    selection: Option<&ConversationSelection>,
    sections: &SectionIndex,
    theme: &Theme,
    clip_end: usize,
) -> Vec<Line<'static>> {
    let Some(selection) = selection.filter(|selection| !selection.is_empty()) else {
        return lines;
    };
    let (start, end) = selection.ordered_points();
    lines
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            let row = row_offset + index;
            if row < start.row || row > end.row {
                return line;
            }
            let section = sections.iter().find(|section| section.rows.contains(&row));
            let content_start = section
                .as_ref()
                .map_or(0, |section| section.content_columns.start);
            let content_end = section
                .as_ref()
                .map_or(usize::MAX, |section| section.content_columns.end);
            let from = (if row == start.row { start.column } else { 0 }).max(content_start);
            let to = (if row == end.row {
                end.column.saturating_add(1)
            } else {
                usize::MAX
            })
            .min(content_end)
            .min(clip_end);
            select_line_cells(line, from, to, theme, clip_end)
        })
        .collect()
}

fn select_line_cells(
    line: Line<'static>,
    from: usize,
    to: usize,
    theme: &Theme,
    clip_end: usize,
) -> Line<'static> {
    let mut used = 0;
    let mut spans = Vec::new();
    for span in line.spans {
        for grapheme in span.content.graphemes(true) {
            let width = UnicodeWidthStr::width(grapheme);
            let selected = used < to && used + width > from && used + width <= clip_end;
            let style = if selected {
                Style::new().fg(theme.selection_fg).bg(theme.selection_bg)
            } else {
                span.style
            };
            spans.push(Span::styled(grapheme.to_owned(), style));
            used += width;
        }
    }
    Line::from(spans)
}

fn copy_start_for_kind(kind: &SectionKind) -> usize {
    match kind {
        SectionKind::User | SectionKind::Tool => crate::ui::rail::SURFACE_CONTENT_START,
        SectionKind::AssistantText | SectionKind::Thinking | SectionKind::Summary => 1,
        SectionKind::Notice => 0,
    }
}

fn content_columns_for_kind(kind: &SectionKind, width: usize) -> std::ops::Range<usize> {
    let start = matches!(kind, SectionKind::User | SectionKind::Tool)
        .then_some(crate::ui::rail::SURFACE_CONTENT_START)
        .unwrap_or(1)
        .min(width);
    start..width
}

fn durable_user_rows<V: DurableLayoutSource>(
    theme: &Theme,
    view: &V,
    user_block: &crate::state::transcript::UserBlock,
    width: usize,
) -> crate::markdown::RenderedMarkdown {
    user::lines_with_timestamp_metadata(
        theme,
        user_block,
        width,
        user_block
            .index
            .and_then(|index| view.user_timestamps().get(&index).map(String::as_str))
            .or_else(|| {
                user_block.loop_id.as_ref().and_then(|loop_id| {
                    view.live_user_timestamp()
                        .filter(|_| view.live_user_loop_id() == Some(loop_id.as_str()))
                })
            }),
        user_block.pending
            && !view.live_user_time_accepted()
            && view.live_user_timestamp().is_none(),
    )
}

fn durable_block_lines<V: DurableLayoutSource>(
    theme: &Theme,
    view: &V,
    block: &TranscriptBlock,
    width: usize,
    reasoning_visible: bool,
) -> Vec<Line<'static>> {
    let body_bytes = match block {
        TranscriptBlock::User(user) => user.text.len(),
        TranscriptBlock::Assistant(assistant) => assistant
            .parts
            .iter()
            .map(|part| match part {
                crate::state::transcript::AssistantPart::Text(text)
                | crate::state::transcript::AssistantPart::Reasoning(text) => text.len(),
                crate::state::transcript::AssistantPart::ToolCall(call) => call.name.len(),
            })
            .sum(),
        TranscriptBlock::Tool(tool) => tool
            .result
            .as_ref()
            .map_or(tool.name.len(), |result| result.len()),
        TranscriptBlock::Summary(summary) => summary.content.len(),
        TranscriptBlock::HistoryPlaceholder(_) => 0,
    };
    if body_bytes > crate::limits::LAYOUT_SECTION_BYTES {
        return summary_lines(
            theme,
            width,
            &format!("[large history section: {body_bytes} bytes; read explicitly to render]"),
        );
    }
    match block {
        TranscriptBlock::User(user_block) => {
            durable_user_rows(theme, view, user_block, width).lines
        }
        TranscriptBlock::Assistant(assistant_block) => assistant::lines_with_folds(
            theme,
            assistant_block,
            width,
            reasoning_visible,
            view.reasoning_folds(),
        ),
        TranscriptBlock::Tool(tool_block) => {
            let render_tool = effective_tool_block_for(view, tool_block);
            let display = tool_display(view, tool_block);
            tool::durable_with_display(theme, &render_tool, width, false, display)
        }
        TranscriptBlock::Summary(summary) => {
            compaction_summary_lines(
                theme,
                width,
                &summary.content,
                !view
                    .summary_folds()
                    .get(&summary.index)
                    .is_some_and(FoldOverride::expanded),
            )
            .0
        }
        TranscriptBlock::HistoryPlaceholder(placeholder) => summary_lines(
            theme,
            width,
            &format!(
                "[large history item {}: {} bytes; read explicitly to decode]",
                placeholder.index, placeholder.total_bytes
            ),
        ),
    }
}

fn section_id(session_id: &str, block: &TranscriptBlock, _ordinal: u32) -> SectionId {
    match block {
        TranscriptBlock::User(user) => SectionId {
            session_id: session_id.into(),
            loop_id: user.loop_id.as_deref().map(std::sync::Arc::from),
            request_index: None,
            kind: SectionKind::User,
            // The history index/loop identity is the stable anchor. The
            // position in the rendered block vector changes when an older
            // page is prepended and must not invalidate a selection.
            ordinal: 0,
            tool_call_id: None,
            history_index: user.index,
        },
        TranscriptBlock::Assistant(assistant) => SectionId {
            session_id: session_id.into(),
            loop_id: Some(assistant.loop_id.clone().into()),
            request_index: Some(assistant.request_index),
            kind: SectionKind::AssistantText,
            ordinal: 0,
            tool_call_id: None,
            history_index: Some(assistant.index),
        },
        TranscriptBlock::Tool(tool) => SectionId {
            session_id: session_id.into(),
            loop_id: Some(tool.loop_id.clone().into()),
            request_index: Some(tool.request_index),
            kind: SectionKind::Tool,
            ordinal: 0,
            tool_call_id: Some(tool.tool_call_id.clone().into()),
            history_index: tool.index,
        },
        TranscriptBlock::Summary(_summary) => SectionId {
            session_id: session_id.into(),
            loop_id: None,
            request_index: None,
            kind: SectionKind::Summary,
            ordinal: 0,
            tool_call_id: None,
            history_index: Some(_summary.index),
        },
        TranscriptBlock::HistoryPlaceholder(placeholder) => SectionId {
            session_id: session_id.into(),
            loop_id: None,
            request_index: None,
            kind: SectionKind::Summary,
            ordinal: 0,
            tool_call_id: None,
            history_index: Some(placeholder.index),
        },
    }
}

fn tool_display<'a, V: DurableLayoutSource>(
    view: &'a V,
    tool: &ToolBlock,
) -> Option<&'a crate::protocol::ToolDisplayWire> {
    view.tool_presentations()
        .get(&ToolKey::new(
            view.session_id(),
            &tool.loop_id,
            tool.request_index,
            &tool.tool_call_id,
        ))
        .map(|presentation| presentation.display.as_ref())
}

fn tool_hidden_line_count(
    tool: &ToolBlock,
    display: Option<&crate::protocol::ToolDisplayWire>,
) -> Option<usize> {
    display
        .and_then(|display| display.hidden_line_count)
        .or_else(|| {
            tool.result
                .as_deref()
                .map(|result| tool::result_line_count(Some(result)))
        })
}

pub(crate) fn effective_tool_expanded(view: &SessionView, tool: &ToolBlock) -> bool {
    effective_tool_expanded_for(view, tool)
}

fn effective_tool_expanded_for<V: DurableLayoutSource>(view: &V, tool: &ToolBlock) -> bool {
    let key = ToolKey::new(
        view.session_id(),
        &tool.loop_id,
        tool.request_index,
        &tool.tool_call_id,
    );
    resolve_tool_expanded_for(
        view,
        &key,
        tool.expanded,
        &tool.name,
        tool_hidden_line_count(tool, tool_display(view, tool)),
    )
}

pub(crate) fn effective_live_tool_expanded(
    view: &SessionView,
    key: &crate::state::tool::ToolKey,
    tool: &crate::state::tool::LiveTool,
) -> bool {
    let hidden_line_count = view
        .tool_presentations
        .get(key)
        .and_then(|presentation| presentation.display.hidden_line_count)
        .or_else(|| {
            tool.result
                .as_deref()
                .map(|result| tool::result_line_count(Some(result)))
        });
    resolve_tool_expanded(view, key, tool.expanded, &tool.name, hidden_line_count)
}

pub(crate) fn resolve_tool_expanded(
    view: &SessionView,
    key: &crate::state::tool::ToolKey,
    base_expanded: bool,
    name: &str,
    hidden_line_count: Option<usize>,
) -> bool {
    resolve_tool_expanded_for(view, key, base_expanded, name, hidden_line_count)
}

fn resolve_tool_expanded_for<V: DurableLayoutSource>(
    view: &V,
    key: &crate::state::tool::ToolKey,
    base_expanded: bool,
    name: &str,
    hidden_line_count: Option<usize>,
) -> bool {
    match view.tool_folds().get(key) {
        Some(crate::state::view::FoldOverride::Expanded) => true,
        Some(crate::state::view::FoldOverride::Collapsed) => false,
        None => {
            base_expanded
                || view.tools_expanded()
                || tool::default_expanded(name, hidden_line_count)
        }
    }
}

fn effective_tool_block_for<V: DurableLayoutSource>(view: &V, tool: &ToolBlock) -> ToolBlock {
    let mut render_tool = tool.clone();
    render_tool.expanded = effective_tool_expanded_for(view, tool);
    render_tool
}

/// Builds the rows after the header and the shared durable block. The durable
/// rows are never copied here: the frame composes them by reference, so a
/// live delta costs only the live tail (`spec §11.2`).
fn build_live_tail(
    theme: &Theme,
    app: &App,
    width: usize,
    durable_last_kind: Option<SectionKind>,
    durable_tool_keys: Option<&HashSet<ToolKey>>,
    live_sections: Option<&mut Vec<SectionRange>>,
) -> (Vec<Line<'static>>, Vec<LinkRow>) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut link_rows: Vec<LinkRow> = Vec::new();
    if let Some(view) = app.active_view() {
        let mut live_previous_kind = durable_last_kind;

        // Warning for unsaved loop if persistence failed (spec 30.5)
        if let Some(unsaved) = &view.unsaved_loop {
            let error_style = Style::new()
                .fg(ratatui::style::Color::White)
                .bg(theme.error);
            let mut banner_lines = vec![
                Line::default(),
                layout::filled(
                    " ⚠ UNSAVED TURN ",
                    width,
                    error_style.add_modifier(ratatui::style::Modifier::BOLD),
                ),
            ];
            let sentences = [
                "This turn finished, but the Agent did not confirm saving it.",
                "The session is blocked. Tool side effects may already exist.",
                "Closing releases this result; reopening reads whatever the Store can recover.",
            ];
            for sentence in sentences {
                let wrapped = crate::markdown::wrap_plain(
                    sentence,
                    width.saturating_sub(2).max(1),
                    error_style,
                );
                for wline in wrapped {
                    banner_lines.push(layout::filled(&format!(" {wline}"), width, error_style));
                }
            }
            if view.event_gap || unsaved.event_gap {
                let gap_sentence = "Some live output may be missing.";
                let wrapped = crate::markdown::wrap_plain(
                    gap_sentence,
                    width.saturating_sub(2).max(1),
                    error_style,
                );
                for wline in wrapped {
                    banner_lines.push(layout::filled(&format!(" {wline}"), width, error_style));
                }
            }
            banner_lines.push(Line::default());
            layout::append_section(&mut lines, banner_lines);
            live_previous_kind = Some(SectionKind::Notice);
            while link_rows.len() < lines.len() {
                link_rows.push(Vec::new());
            }
        }

        // Render completed steer notices retained across loop boundaries
        for steer in &view.completed_steers {
            if steer.state == crate::state::turn::PendingSteerState::Persisted {
                continue;
            }
            let state_label = match &steer.state {
                crate::state::turn::PendingSteerState::Sending => "sending…",
                crate::state::turn::PendingSteerState::Queued => "accepted awaiting history",
                crate::state::turn::PendingSteerState::Persisted => "persisted",
                crate::state::turn::PendingSteerState::NotRecorded => "not recorded",
                crate::state::turn::PendingSteerState::Unconfirmed => "save unconfirmed",
            };
            let label = format!(" ⠸ Steering ({}): {}", state_label, steer.text);
            let steer_lines = vec![
                Line::default(),
                layout::filled(
                    &label,
                    width,
                    Style::new().fg(theme.accent).bg(theme.card_bg),
                ),
                Line::default(),
            ];
            layout::append_section(&mut lines, steer_lines);
            live_previous_kind = Some(SectionKind::Notice);
            while link_rows.len() < lines.len() {
                link_rows.push(Vec::new());
            }
        }

        if let Some(live) = &view.live {
            live_section(
                theme,
                view,
                live,
                width,
                app.reasoning_visible,
                live_previous_kind,
                durable_tool_keys,
                &mut lines,
                live_sections,
                &mut link_rows,
            );
            while link_rows.len() < lines.len() {
                link_rows.push(Vec::new());
            }
        }

        if view.can_show_last_result() {
            if let Some(result) = &view.last_result {
                layout::append_section(&mut lines, last_result_lines(theme, result, width));
                while link_rows.len() < lines.len() {
                    link_rows.push(Vec::new());
                }
            }
        }
    }
    while link_rows.len() < lines.len() {
        link_rows.push(Vec::new());
    }
    (lines, link_rows)
}

fn last_result_lines(
    theme: &Theme,
    result: &crate::protocol::TurnResultViewWire,
    width: usize,
) -> Vec<Line<'static>> {
    use crate::protocol::LoopOutcomeWire;
    use crate::ui::status::{result_color, result_summary};

    if matches!(result.outcome, LoopOutcomeWire::Completed) {
        return Vec::new();
    }

    let (badge, outcome_style) = match &result.outcome {
        LoopOutcomeWire::Completed => (
            "✓",
            Style::new()
                .fg(result_color(result, theme))
                .bg(theme.card_bg),
        ),
        LoopOutcomeWire::Cancelled { .. } => (
            "⊘",
            Style::new()
                .fg(result_color(result, theme))
                .bg(theme.card_bg),
        ),
        LoopOutcomeWire::Failed { .. } => (
            "✗",
            Style::new()
                .fg(result_color(result, theme))
                .bg(theme.card_bg),
        ),
    };

    let content = format!(
        " {} Turn {} · requests: {} · tool rounds: {}",
        badge,
        result_summary(result),
        result
            .requests
            .map_or_else(|| "unknown".to_owned(), |value| value.to_string()),
        result
            .tool_rounds
            .map_or_else(|| "unknown".to_owned(), |value| value.to_string())
    );

    vec![
        Line::default(),
        layout::filled(&content, width, outcome_style),
        Line::default(),
    ]
}

struct LiveRenderContext<'a> {
    theme: &'a Theme,
    view: &'a SessionView,
    session_id: &'a str,
    loop_id: &'a str,
    request_index: u32,
    width: usize,
    reasoning_visible: bool,
    durable_tool_keys: Option<&'a HashSet<ToolKey>>,
}

impl LiveRenderContext<'_> {
    fn append_reasoning(
        &self,
        out: &mut Vec<Line<'static>>,
        ranges: &mut Option<&mut Vec<SectionRange>>,
        link_rows: &mut Vec<LinkRow>,
        text: &str,
        ordinal: u32,
        in_hidden_run: bool,
    ) {
        let key = crate::state::view::ReasoningKey::new(self.loop_id, self.request_index, ordinal);
        let expanded = self
            .view
            .reasoning_folds
            .get(&key)
            .map(FoldOverride::expanded);
        let raw_lines = text.trim().split('\n').count();
        let rendered = reasoning::reasoning_with_metadata(
            self.theme,
            text,
            self.width,
            self.reasoning_visible,
            in_hidden_run,
            expanded,
        );
        let before = out.len();
        let rendered_len = rendered.lines.len();
        link_rows.resize(before, Vec::new());
        append_live_section(
            out,
            ranges,
            None,
            SectionId {
                session_id: self.session_id.into(),
                loop_id: Some(self.loop_id.into()),
                request_index: Some(self.request_index),
                kind: SectionKind::Thinking,
                ordinal,
                tool_call_id: None,
                history_index: None,
            },
            rendered.lines,
            self.width,
            self.reasoning_visible && raw_lines > 3,
            self.reasoning_visible && raw_lines > 3 && !expanded.unwrap_or(false),
        );
        let shared_blank = rendered_len.saturating_sub(out.len().saturating_sub(before));
        link_rows.extend(rendered.link_cells.into_iter().skip(shared_blank));
    }

    fn append_text(
        &self,
        out: &mut Vec<Line<'static>>,
        ranges: &mut Option<&mut Vec<SectionRange>>,
        text: &str,
        ordinal: u32,
    ) {
        let base = Style::new().fg(self.theme.text);
        let lines = wrap_plain(text, self.width.saturating_sub(1).max(1), base)
            .into_iter()
            .map(|line| crate::ui::rail::inset_row(self.width, 1, line))
            .collect();
        append_live_section(
            out,
            ranges,
            None,
            SectionId {
                session_id: self.session_id.into(),
                loop_id: Some(self.loop_id.into()),
                request_index: Some(self.request_index),
                kind: SectionKind::AssistantText,
                ordinal,
                tool_call_id: None,
                history_index: None,
            },
            layout::vertical_section(lines),
            self.width,
            false,
            false,
        );
    }

    fn append_tool(
        &self,
        out: &mut Vec<Line<'static>>,
        ranges: &mut Option<&mut Vec<SectionRange>>,
        tool: &crate::state::tool::LiveTool,
    ) {
        let key = ToolKey::new(
            self.session_id,
            self.loop_id,
            self.request_index,
            &tool.tool_call_id,
        );
        if self
            .durable_tool_keys
            .is_some_and(|keys| keys.contains(&key))
        {
            return;
        }
        let (id, lines, folded) = live_tool_render(
            self.theme,
            self.view,
            self.loop_id,
            self.request_index,
            tool,
            self.width,
        );
        append_live_section(out, ranges, None, id, lines, self.width, true, folded);
    }
}

fn live_tool_render(
    theme: &Theme,
    view: &SessionView,
    loop_id: &str,
    request_index: u32,
    tool: &crate::state::tool::LiveTool,
    width: usize,
) -> (SectionId, Vec<Line<'static>>, bool) {
    let tool_key = crate::state::tool::ToolKey::new(
        &view.info.session_id,
        loop_id,
        request_index,
        &tool.tool_call_id,
    );
    let display = view
        .tool_presentations
        .get(&tool_key)
        .map(|presentation| presentation.display.as_ref());
    let mut render_tool = tool.clone();
    render_tool.expanded = effective_live_tool_expanded(view, &tool_key, &render_tool);
    (
        SectionId {
            session_id: view.info.session_id.clone().into(),
            loop_id: Some(loop_id.into()),
            request_index: Some(request_index),
            kind: SectionKind::Tool,
            ordinal: 0,
            tool_call_id: Some(tool.tool_call_id.clone().into()),
            history_index: None,
        },
        tool::live_with_display(theme, &render_tool, width, display),
        matches!(
            view.tool_folds.get(&tool_key),
            Some(FoldOverride::Collapsed)
        ) || (!view.tools_expanded && !render_tool.expanded),
    )
}

/// The live loop tail: supports multi-request loops, live tools, and pending steers.
#[allow(clippy::too_many_arguments)]
fn live_section(
    theme: &Theme,
    view: &SessionView,
    live: &crate::state::turn::LiveLoop,
    width: usize,
    reasoning_visible: bool,
    previous_kind: Option<SectionKind>,
    durable_tool_keys: Option<&HashSet<ToolKey>>,
    out: &mut Vec<Line<'static>>,
    ranges: Option<&mut Vec<SectionRange>>,
    link_rows: &mut Vec<LinkRow>,
) {
    let mut ranges = ranges;
    let session_id = view.info.session_id.clone();
    let loop_id = live
        .reference
        .as_ref()
        .map(|turn| turn.loop_id.clone())
        .unwrap_or_default();

    for req in &live.requests {
        let context = LiveRenderContext {
            theme,
            view,
            session_id: &session_id,
            loop_id: &loop_id,
            request_index: req.request_index,
            width,
            reasoning_visible,
            durable_tool_keys,
        };
        let mut rendered_tool_ids = HashSet::new();

        let mut reasoning_ordinal = 0;
        let mut text_ordinal = 0;
        let mut in_hidden_run = false;
        for part in &req.parts {
            match part {
                crate::state::turn::LivePart::Reasoning(text) => {
                    context.append_reasoning(
                        out,
                        &mut ranges,
                        link_rows,
                        text,
                        reasoning_ordinal,
                        in_hidden_run,
                    );
                    reasoning_ordinal += 1;
                    in_hidden_run = !reasoning_visible;
                }
                crate::state::turn::LivePart::Text(text) => {
                    context.append_text(out, &mut ranges, text, text_ordinal);
                    text_ordinal += 1;
                    in_hidden_run = false;
                }
                crate::state::turn::LivePart::Tool { tool_call_id } => {
                    if let Some(live_tool) = req
                        .tools
                        .iter()
                        .find(|tool| tool.tool_call_id == *tool_call_id)
                    {
                        rendered_tool_ids.insert(live_tool.tool_call_id.clone());
                        context.append_tool(out, &mut ranges, live_tool);
                    }
                    in_hidden_run = false;
                }
            }
        }
        // A dropped or out-of-order marker must not make a real Tool
        // disappear from the live tail.
        for live_tool in &req.tools {
            if !rendered_tool_ids.contains(&live_tool.tool_call_id) {
                context.append_tool(out, &mut ranges, live_tool);
            }
        }
    }

    // 0.2.4 queue contract: pending steers live as gray `Steering: …` rows in
    // the dock (steer_queue module), NOT as transcript user slates. Only
    // receipt-proven applied steers render here as provisional Steering user
    // cards (removed in exactly one place when the durable History card
    // appears), plus terminal failure warnings that could not persist.
    for applied in &view.applied_steers {
        let mut card = crate::ui::user::steering_lines(
            theme,
            &applied.text,
            width,
            applied.accepted_at.as_deref(),
            applied.accepted_at.is_none(),
        );
        card.push(Line::from(vec![ratatui::text::Span::styled(
            " ⠸ applied",
            Style::new().fg(theme.muted).add_modifier(Modifier::ITALIC),
        )]));
        append_live_section(
            out,
            &mut ranges,
            previous_kind,
            SectionId {
                session_id: session_id.clone().into(),
                loop_id: Some(loop_id.clone().into()),
                request_index: None,
                kind: SectionKind::User,
                ordinal: applied.local_id as u32,
                tool_call_id: None,
                history_index: None,
            },
            card,
            width,
            false,
            false,
        );
    }
    for steer in &live.pending_steers {
        let state_label = match steer.state {
            crate::state::turn::PendingSteerState::NotRecorded => "not recorded",
            crate::state::turn::PendingSteerState::Unconfirmed => "save unconfirmed",
            crate::state::turn::PendingSteerState::Sending
            | crate::state::turn::PendingSteerState::Queued
            | crate::state::turn::PendingSteerState::Persisted => continue,
        };
        let label = format!(" ⠸ Steering ({}): {}", state_label, steer.text);
        append_live_section(
            out,
            &mut ranges,
            None,
            SectionId {
                session_id: session_id.clone().into(),
                loop_id: Some(loop_id.clone().into()),
                request_index: None,
                kind: SectionKind::Notice,
                ordinal: steer.local_id as u32,
                tool_call_id: None,
                history_index: None,
            },
            vec![
                Line::default(),
                layout::filled(
                    &label,
                    width,
                    Style::new().fg(theme.accent).bg(theme.card_bg),
                ),
                Line::default(),
            ],
            width,
            false,
            false,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn append_live_section(
    out: &mut Vec<Line<'static>>,
    ranges: &mut Option<&mut Vec<SectionRange>>,
    previous_kind: Option<SectionKind>,
    id: SectionId,
    lines: Vec<Line<'static>>,
    width: usize,
    collapsible: bool,
    folded: bool,
) {
    if lines.is_empty() {
        return;
    }
    let previous_kind = ranges
        .as_deref()
        .and_then(|ranges| ranges.last().map(|section| section.id.kind))
        .or(previous_kind);
    if needs_user_gap(previous_kind, id.kind) {
        out.push(Line::default());
    }
    let before = out.len();
    layout::append_section(out, lines);
    if let Some(ranges) = ranges.as_deref_mut() {
        let content_columns = content_columns_for_kind(&id.kind, width);
        ranges.push(SectionRange {
            id,
            rows: before..out.len(),
            content_columns,
            collapsible,
            folded,
        });
    }
}

#[cfg(test)]
#[path = "compaction_summary_tests.rs"]
mod compaction_summary_tests;

/// Runs on the durable layout worker, with the same per-section byte ceiling
/// as other history bodies. The logical source remains the raw summary.
fn compaction_summary_lines(
    theme: &Theme,
    width: usize,
    content: &str,
    folded: bool,
) -> (Vec<Line<'static>>, Vec<Vec<Range<usize>>>, Vec<bool>) {
    let label = if folded {
        "[compaction] Compaction summary · click to expand"
    } else if content.len() > crate::limits::LAYOUT_SECTION_BYTES {
        "[compaction] Compaction summary · exceeds layout budget; /export to read"
    } else {
        "[compaction] Compaction summary · click to collapse"
    };
    let surface = |line| {
        crate::ui::rail::surface_row(width, crate::ui::rail::thinking_colors(theme), 1, line)
    };
    let mut lines = vec![
        Line::default(),
        surface(Line::styled(label, Style::new().fg(theme.muted))),
    ];
    let mut links = vec![Vec::new(), Vec::new()];
    let mut breaks = vec![false, false];
    if !folded && content.len() <= crate::limits::LAYOUT_SECTION_BYTES {
        let renderer = crate::markdown::MarkdownRenderer::new(theme);
        let (body, body_links, body_breaks) = renderer.render_with_breaks(
            content,
            width.saturating_sub(1).max(1),
            Style::new().fg(theme.text),
        );
        lines.extend(body.into_iter().map(surface));
        links.extend(body_links.into_iter().map(|row| {
            row.into_iter()
                .map(|range| range.start + 1..range.end + 1)
                .collect()
        }));
        breaks.extend(body_breaks);
    }
    lines.push(Line::default());
    links.push(Vec::new());
    breaks.push(false);
    (lines, links, breaks)
}

fn summary_lines(theme: &Theme, width: usize, content: &str) -> Vec<Line<'static>> {
    let label = if content.is_empty() {
        " Conversation compacted".to_owned()
    } else {
        format!(" Summary: {content}")
    };
    vec![
        Line::default(),
        layout::filled(
            &label,
            width,
            Style::new().fg(theme.muted).bg(theme.card_bg),
        ),
        Line::default(),
    ]
}

pub fn render(frame: &mut Frame<'_>, area: Rect, app: &App, theme: &Theme) {
    let width = area.width as usize;
    let height = area.height as usize;
    if width == 0 || height == 0 {
        return;
    }
    if app.async_layout_enabled() && app.prepared_conversation(area.width).is_none() {
        if let Some(previous) = app.transition_transcript_frame(area) {
            // Only transcript cells are replayed; composer/footer keep drawing
            // from current App state. A growing composer simply crops the view.
            let overlap = area.intersection(previous.area);
            for y in overlap.y..overlap.bottom() {
                for x in overlap.x..overlap.right() {
                    frame.buffer_mut()[(x, y)] = previous[(x, y)].clone();
                }
            }
            return;
        }
        let line = layout::filled(
            "Preparing conversation...",
            width,
            Style::new().fg(theme.muted).bg(theme.page_bg),
        );
        frame.render_widget(ratatui::widgets::Paragraph::new(line), area);
        return;
    }
    let fallback;
    let prepared = match app.prepared_conversation(area.width) {
        Some(prepared) => prepared,
        None => {
            fallback = prepare_conversation(app, area.width);
            &fallback
        }
    };
    let total = prepared.total_rows();
    let position = scroll_position(app, total, height);
    let offset = position.offset;
    let marker = position.marker;
    let budget = position.visible_rows;
    // Only the visible rows become owned; the shared durable block is read
    // through the frame, never re-cloned (`spec §11.7`).
    let slice: Vec<Line<'static>> = apply_selection(
        prepared.window(offset, budget),
        offset,
        app.selection.as_ref(),
        &prepared.sections,
        theme,
        width.saturating_sub(usize::from(app.scrollbar_visible(total, height))),
    );
    let body_area = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: slice.len() as u16,
    };
    frame.render_widget(ratatui::widgets::Paragraph::new(slice), body_area);
    for (hit, _) in crate::ui::tool_detail::detail_hits(prepared, area, offset, budget) {
        layout::clear_wide_overlay_edges(frame.buffer_mut(), hit);
        frame.render_widget(
            ratatui::widgets::Paragraph::new("[详情]")
                .style(Style::new().fg(theme.scrollbar_thumb).bg(theme.page_bg)),
            hit,
        );
    }
    if marker {
        let marker_y = area.y.saturating_add(height as u16).saturating_sub(1);
        let marker_area = Rect {
            x: area.x,
            y: marker_y,
            width: area.width,
            height: 1,
        };
        render_marker(
            frame,
            marker_area,
            app,
            theme,
            app.scrollbar_visible(total, height),
        );
    }
    if app.scrollbar_visible(total, height) {
        crate::ui::scrollbar::render(frame, area, total, offset, theme, app.scrollbar_active());
    }
}

fn render_marker(frame: &mut Frame<'_>, area: Rect, app: &App, theme: &Theme, scrollbar: bool) {
    let label = if app
        .active_view()
        .is_some_and(|view| view.scroll.new_content)
    {
        "↓ new output"
    } else {
        "↑ scroll position"
    };
    let area = marker_area(area, label, scrollbar);
    layout::clear_wide_overlay_edges(frame.buffer_mut(), area);
    let line = layout::filled(
        label,
        area.width as usize,
        Style::new().fg(theme.dim).bg(theme.page_bg),
    );
    frame.render_widget(ratatui::widgets::Paragraph::new(line), area);
}

pub(crate) fn marker_area(area: Rect, label: &str, scrollbar: bool) -> Rect {
    let width = area.width.saturating_sub(u16::from(scrollbar));
    let label_width = crate::markdown::column_width(label).min(width as usize) as u16;
    Rect::new(
        area.x + (width - label_width) / 2,
        area.bottom().saturating_sub(1),
        label_width,
        1,
    )
}

#[cfg(test)]
mod source_map_tests {
    use super::*;

    fn copied_text(layout: &SectionLayout) -> String {
        let mut out = String::new();
        let mut hard_break = false;
        for copy in layout.copy_ranges.iter().filter(|copy| !copy.decorative) {
            if hard_break {
                out.push('\n');
            }
            out.push_str(copy.text());
            hard_break = copy.hard_break_after;
        }
        out
    }

    fn key(kind: SectionKind) -> LayoutKey {
        LayoutKey {
            section: SectionId {
                session_id: Arc::from("source-test"),
                loop_id: Some(Arc::from("loop")),
                request_index: Some(0),
                kind,
                ordinal: 0,
                tool_call_id: None,
                history_index: Some(0),
            },
            revision: 1,
            width: 12,
            theme: crate::theme::ThemeKind::Dark,
            folded: false,
            reasoning_visible: true,
        }
    }

    #[test]
    fn copied_fenced_code_excludes_frame_and_padding() {
        let theme = crate::theme::Theme::dark();
        let input = assistant::AssistantSectionInput {
            source: Arc::from("Before\n\n```python\n  print(\"中文\")\n```\nAfter"),
            kind: SectionKind::AssistantText,
            ordinal: 0,
            collapsible: false,
            folded: false,
            tool_call: None,
            in_hidden_run: false,
        };
        let rendered = assistant::render_section(&theme, &input, 24, true);
        let layout = make_section_layout(
            key(SectionKind::AssistantText),
            rendered.lines,
            rendered.link_cells,
            false,
            false,
            0,
            Some(input.source.as_ref()),
            rendered.hard_breaks.as_deref(),
            rendered.copy_cells.as_deref(),
        )
        .unwrap();
        assert_eq!(copied_text(&layout), "Before\n\n  print(\"中文\")\n\nAfter");
    }

    #[test]
    fn copied_code_keeps_literal_frames_spaces_and_wide_soft_wrapped_source() {
        let theme = crate::theme::Theme::dark();
        let code = format!("  │literal╭─╮  \n{}\n\n  end  ", "中文🙂abc".repeat(12));
        let input = assistant::AssistantSectionInput {
            source: Arc::from(format!("```\n{code}\n```")),
            kind: SectionKind::AssistantText,
            ordinal: 0,
            collapsible: false,
            folded: false,
            tool_call: None,
            in_hidden_run: false,
        };
        let rendered = assistant::render_section(&theme, &input, 24, true);
        let layout = make_section_layout(
            key(SectionKind::AssistantText),
            rendered.lines,
            rendered.link_cells,
            false,
            false,
            0,
            Some(input.source.as_ref()),
            rendered.hard_breaks.as_deref(),
            rendered.copy_cells.as_deref(),
        )
        .unwrap();
        assert_eq!(copied_text(&layout), code);
        let first = layout
            .copy_ranges
            .iter()
            .find(|copy| !copy.decorative)
            .unwrap();
        assert_eq!(
            first.columns.start, 2,
            "selection begins inside rail and code frame"
        );
    }

    #[test]
    fn copied_nested_code_keeps_only_renderer_owned_content_cells() {
        let theme = crate::theme::Theme::dark();
        let input = assistant::AssistantSectionInput {
            source: Arc::from("- item\n\n  ```\n  code│  \n  ```"),
            kind: SectionKind::AssistantText,
            ordinal: 0,
            collapsible: false,
            folded: false,
            tool_call: None,
            in_hidden_run: false,
        };
        let rendered = assistant::render_section(&theme, &input, 24, true);
        let layout = make_section_layout(
            key(SectionKind::AssistantText),
            rendered.lines,
            rendered.link_cells,
            false,
            false,
            0,
            Some(input.source.as_ref()),
            rendered.hard_breaks.as_deref(),
            rendered.copy_cells.as_deref(),
        )
        .unwrap();
        assert_eq!(copied_text(&layout), "• item\n\ncode│  ");
    }

    #[test]
    fn user_copy_source_retains_selectable_timestamp() {
        let theme = crate::theme::Theme::dark();
        let block = crate::state::transcript::UserBlock {
            index: Some(0),
            loop_id: Some("loop".into()),
            kind: crate::protocol::UserMessageKindWire::Prompt,
            text: "Synthetic user prompt 中文.".into(),
            pending: false,
        };
        let lines =
            user::lines_with_timestamp(&theme, &block, 80, Some("2026-10-01T14:00:00Z"), false);
        let layout = make_section_layout(
            key(SectionKind::User),
            lines,
            Vec::new(),
            false,
            false,
            0,
            Some(&block.text),
            None,
            None,
        )
        .unwrap();
        assert!(copied_text(&layout).starts_with(&block.text));
        assert!(copied_text(&layout).contains("10/1/2026"));
    }

    #[test]
    fn pending_user_copy_uses_card_owned_timestamp_and_code_geometry() {
        let theme = crate::theme::Theme::dark();
        let block = crate::state::transcript::UserBlock {
            index: None,
            loop_id: Some("loop".into()),
            kind: crate::protocol::UserMessageKindWire::Prompt,
            text: "Pending prompt\n\n```\n  │literal  \n```".into(),
            pending: true,
        };
        let rendered = user::lines_with_timestamp_metadata(&theme, &block, 80, None, true);
        let mut pending_key = key(SectionKind::User);
        pending_key.section.history_index = None;
        let layout = make_section_layout(
            pending_key,
            rendered.lines,
            rendered.link_cells,
            false,
            false,
            0,
            Some(&block.text),
            Some(&rendered.hard_breaks),
            Some(&rendered.copy_cells),
        )
        .unwrap();
        assert_eq!(
            copied_text(&layout),
            "Pending prompt\n\n  │literal  \ntime pending"
        );
    }

    #[test]
    fn collapsed_reasoning_keeps_visible_link_hit_cells() {
        let theme = crate::theme::Theme::dark();
        let input = assistant::AssistantSectionInput {
            source: Arc::from("[Local fixture](https://example.test)\none\ntwo\nthree\nfour"),
            kind: SectionKind::Thinking,
            ordinal: 0,
            collapsible: true,
            folded: true,
            tool_call: None,
            in_hidden_run: false,
        };
        let rendered = assistant::render_section(&theme, &input, 80, true);
        assert!(
            !rendered.link_cells[1].is_empty(),
            "visible reasoning link must retain hit geometry"
        );
        assert!(
            rendered.link_cells[1]
                .iter()
                .any(|range| range.contains(&2))
        );
        assert!(
            rendered.link_cells[4].is_empty(),
            "fold hint has no link target"
        );
    }

    #[test]
    fn live_reasoning_link_cells_follow_shared_blank_and_one_parse() {
        let app =
            crate::ui::testapp::open_empty(crate::theme::ThemeKind::Dark, "ses_1", None, "high");
        let theme = crate::theme::Theme::dark();
        let context = LiveRenderContext {
            theme: &theme,
            view: app.active_view().unwrap(),
            session_id: "ses_1",
            loop_id: "loop",
            request_index: 0,
            width: 80,
            reasoning_visible: true,
            durable_tool_keys: None,
        };
        let mut lines = vec![Line::from("previous"), Line::default()];
        let mut links = vec![Vec::new(), Vec::new()];
        let mut ranges = Vec::new();
        crate::markdown::reset_parse_count();
        context.append_reasoning(
            &mut lines,
            &mut Some(&mut ranges),
            &mut links,
            "[Local fixture](https://example.test)\none\ntwo\nthree\nfour",
            0,
            false,
        );
        assert_eq!(crate::markdown::parse_count(), 1);
        assert_eq!(lines.len(), links.len());
        assert!(lines[2].to_string().contains("Local fixture"));
        assert!(links[2].iter().any(|range| range.contains(&2)));
        assert!(links[5].is_empty(), "fold hint is not linked");
    }

    #[test]
    fn soft_wraps_share_source_without_inventing_a_hard_break() {
        let lines = crate::markdown::wrap_plain("alpha beta gamma", 6, Style::default());
        let layout = make_section_layout(
            key(SectionKind::AssistantText),
            lines,
            Vec::new(),
            false,
            false,
            0,
            Some("alpha beta gamma"),
            None,
            None,
        )
        .expect("wrapped section");
        assert_eq!(layout.source_map.source.as_ref(), "alpha beta gamma");
        assert!(layout.source_map.rows.len() > 1);
        assert!(
            layout
                .source_map
                .rows
                .iter()
                .take(layout.source_map.rows.len() - 1)
                .all(|row| !row.hard_break_after)
        );
        assert!(layout.source_map.rows.last().unwrap().hard_break_after);
        assert!(
            layout
                .source_map
                .rows
                .iter()
                .all(|row| row.source_range.end <= layout.source_map.source.len())
        );
    }

    #[test]
    fn blank_logical_lines_and_links_keep_source_bounds() {
        let theme = crate::theme::Theme::dark();
        let renderer = crate::markdown::MarkdownRenderer::new(&theme);
        let source = "one\n\n[three](https://example.test)";
        let (lines, links) = renderer.render_with_links(source, 11, Style::default());
        let lines = lines
            .into_iter()
            .map(|line| crate::ui::rail::inset_row(12, 1, line))
            .collect();
        let layout = make_section_layout(
            key(SectionKind::Summary),
            lines,
            links,
            false,
            false,
            0,
            Some(source),
            None,
            None,
        )
        .expect("markdown section");
        assert_eq!(layout.source_map.source.as_ref(), source);
        assert!(layout.link_cells.iter().flatten().next().is_some());
        assert!(
            layout
                .source_map
                .rows
                .iter()
                .all(|row| row.source_range.end <= source.len())
        );
        assert!(
            layout
                .source_map
                .rows
                .iter()
                .any(|row| row.hard_break_after)
        );
        assert!(copied_text(&layout).contains("one\n\nthree"));
    }

    #[test]
    fn markdown_soft_wraps_never_become_fake_newlines_in_the_copy_text() {
        let theme = crate::theme::Theme::dark();
        let renderer = crate::markdown::MarkdownRenderer::new(&theme);
        let long_line = "x".repeat(120);
        let source = format!("first line\n\n{long_line}\n\nlast line");
        let (lines, _links, breaks) = renderer.render_with_breaks(&source, 20, Style::default());
        let lines = lines
            .into_iter()
            .map(|line| crate::ui::rail::inset_row(21, 1, line))
            .collect();
        let layout = make_section_layout(
            key(SectionKind::AssistantText),
            lines,
            Vec::new(),
            false,
            false,
            0,
            Some(source.as_str()),
            Some(&breaks),
            None,
        )
        .expect("markdown section");
        assert_eq!(copied_text(&layout), source);
    }

    #[test]
    fn grapheme_ranges_are_utf8_boundary_safe() {
        let source = "🙂 café";
        let lines = crate::markdown::wrap_plain(source, 5, Style::default());
        let layout = make_section_layout(
            key(SectionKind::AssistantText),
            lines,
            Vec::new(),
            false,
            false,
            0,
            Some(source),
            None,
            None,
        )
        .expect("grapheme section");
        assert!(
            layout
                .source_map
                .rows
                .iter()
                .all(|row| source.is_char_boundary(row.source_range.start)
                    && source.is_char_boundary(row.source_range.end))
        );
    }

    #[test]
    fn copy_source_preserves_soft_wraps_wide_text_code_indent_and_blank_lines() {
        let source = "中文🙂abcdef\n\n    code 中文🙂";
        let lines = crate::markdown::wrap_plain(source, 11, Style::default())
            .into_iter()
            .map(|line| crate::ui::rail::inset_row(12, 1, line))
            .collect();
        let links = Vec::new();
        let layout = make_section_layout(
            key(SectionKind::Summary),
            lines,
            links,
            false,
            false,
            0,
            Some(source),
            None,
            None,
        )
        .expect("wide/code section");
        let copied = copied_text(&layout);
        assert!(copied.contains("中文🙂"));
        assert!(copied.contains("code 中文🙂"));
        assert!(copied.contains("\n\n"));
        assert!(copied.contains(' '));
    }

    #[test]
    fn link_wrapping_keeps_link_cells_and_does_not_invent_newlines() {
        let theme = crate::theme::Theme::dark();
        let renderer = crate::markdown::MarkdownRenderer::new(&theme);
        let source = "[中文链接](https://example.test/path)";
        let (lines, links) = renderer.render_with_links(source, 11, Style::default());
        let lines = lines
            .into_iter()
            .map(|line| crate::ui::rail::inset_row(12, 1, line))
            .collect();
        let layout = make_section_layout(
            key(SectionKind::Summary),
            lines,
            links,
            false,
            false,
            0,
            Some(source),
            None,
            None,
        )
        .expect("link section");
        let copied = copied_text(&layout);
        assert!(copied.contains("中文链接"));
        assert!(!copied.contains('\n'));
        assert!(layout.link_cells.iter().flatten().count() >= 2);
        assert!(
            layout
                .source_map
                .rows
                .iter()
                .all(|row| row.source_range.end <= source.len())
        );
    }

    #[test]
    fn unloaded_placeholder_is_visible_but_not_copyable() {
        let layout = make_section_layout(
            key(SectionKind::Summary),
            vec![Line::from("[large history item: read explicitly]")],
            Vec::new(),
            false,
            false,
            0,
            None,
            None,
            None,
        )
        .expect("placeholder section");

        assert!(layout.copy_ranges.iter().all(|copy| copy.decorative));
        assert!(layout.source_map.rows.iter().all(|row| row.decorative));
        assert!(copied_text(&layout).is_empty());
    }
}
