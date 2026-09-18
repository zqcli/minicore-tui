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
    pub reasoning_folds: HashMap<crate::state::view::ReasoningKey, FoldOverride>,
    pub tool_folds: HashMap<ToolKey, FoldOverride>,
    pub tool_presentations: Arc<HashMap<ToolKey, Arc<ToolPresentationState>>>,
    pub tools_expanded: bool,
    pub user_timestamps: HashMap<usize, String>,
    pub live_user_timestamp: Option<String>,
    pub live_user_time_accepted: bool,
    pub live_user_loop_id: Option<String>,
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
            reasoning_folds: view.reasoning_folds.clone(),
            tool_folds: view.tool_folds.clone(),
            tool_presentations: Arc::clone(&view.tool_presentations),
            tools_expanded: view.tools_expanded,
            user_timestamps: view.user_timestamps.clone(),
            live_user_timestamp: view.live_user_timestamp.clone(),
            live_user_time_accepted: view.live_user_time_accepted,
            live_user_loop_id: view
                .live
                .as_ref()
                .and_then(|live| live.reference.as_ref())
                .map(|turn| turn.loop_id.clone()),
        }
    }
}

pub(crate) trait DurableLayoutSource {
    fn session_id(&self) -> &str;
    fn blocks(&self) -> &[Arc<TranscriptBlock>];
    fn reasoning_folds(&self) -> &HashMap<crate::state::view::ReasoningKey, FoldOverride>;
    fn tool_folds(&self) -> &HashMap<ToolKey, FoldOverride>;
    fn tool_presentations(&self) -> &HashMap<ToolKey, Arc<ToolPresentationState>>;
    fn tools_expanded(&self) -> bool;
    fn user_timestamps(&self) -> &HashMap<usize, String>;
    fn live_user_timestamp(&self) -> Option<&str>;
    fn live_user_time_accepted(&self) -> bool;
    fn live_user_loop_id(&self) -> Option<&str>;
}

impl DurableLayoutSource for SessionView {
    fn session_id(&self) -> &str {
        &self.info.session_id
    }

    fn blocks(&self) -> &[Arc<TranscriptBlock>] {
        &self.transcript.blocks
    }

    fn reasoning_folds(&self) -> &HashMap<crate::state::view::ReasoningKey, FoldOverride> {
        &self.reasoning_folds
    }

    fn tool_folds(&self) -> &HashMap<ToolKey, FoldOverride> {
        &self.tool_folds
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
        self.live
            .as_ref()
            .and_then(|live| live.reference.as_ref())
            .map(|turn| turn.loop_id.as_str())
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

pub fn prepare_conversation_with_durable(
    app: &App,
    width: u16,
    ready_durable: Arc<PreparedDurable>,
) -> PreparedConversation {
    prepare_conversation_inner(app, width, Some(ready_durable), false)
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
    let (mut live, mut live_links) = build_live_tail(
        &theme,
        app,
        width as usize,
        last_kind,
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
                section_copy_is_decorative(section, row, &text),
            ));
            live_source.push('\n');
        }
    }
    let live_source: Arc<str> = live_source.into();
    let live_copy: Vec<CopyRange> = live_copy_meta
        .into_iter()
        .map(|(row, copy_start, source_range, decorative)| CopyRange {
            row,
            columns: copy_start..width as usize,
            source: Arc::clone(&live_source),
            source_range,
            hard_break_after: true,
            decorative,
        })
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
    for block in view.blocks() {
        if let TranscriptBlock::Tool(tool) = block.as_ref() {
            tool_index.insert(
                (
                    tool.loop_id.as_str(),
                    tool.request_index,
                    tool.tool_call_id.as_str(),
                ),
                tool,
            );
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
    let mut rendered_tools = HashSet::new();
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
                    let tool = tool_index
                        .get(&(
                            assistant_block.loop_id.as_str(),
                            assistant_block.request_index,
                            call.tool_call_id.as_str(),
                        ))
                        .map(|tool| {
                            tool_index_lookups += 1;
                            (*tool).clone()
                        })
                        .unwrap_or_else(|| ToolBlock {
                            index: None,
                            loop_id: assistant_block.loop_id.clone(),
                            request_index: assistant_block.request_index,
                            tool_call_id: call.tool_call_id.clone(),
                            name: call.name.clone(),
                            result: None,
                            outcome: None,
                            live_status: None,
                            progress: None,
                            expanded: false,
                        });
                    let tool_key = ToolKey::new(
                        view.session_id(),
                        &tool.loop_id,
                        tool.request_index,
                        &tool.tool_call_id,
                    );
                    rendered_tools.insert(tool_key);
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
                ) {
                    sections.push(layout);
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
            if rendered_tools.contains(&tool_key) {
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
        });
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
        let lines = durable_block_lines(theme, view, block, width as usize, reasoning_visible);
        let collapsible = matches!(block.as_ref(), TranscriptBlock::Tool(_));
        if let Some(layout) = make_section_layout(
            key,
            lines,
            Vec::new(),
            collapsible,
            folded,
            ordinal.saturating_mul(1_000_000),
            block_source(block),
        ) {
            if !push_layout_section(layout, &mut sections, &mut Vec::new(), &mut batch_sink) {
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

fn make_section_layout(
    key: LayoutKey,
    lines: Vec<Line<'static>>,
    mut link_cells: Vec<Vec<std::ops::Range<usize>>>,
    collapsible: bool,
    folded: bool,
    order: usize,
    source_hint: Option<&str>,
) -> Option<Arc<SectionLayout>> {
    if lines.is_empty() {
        return None;
    }
    link_cells.resize_with(lines.len(), Vec::new);
    let range = SectionRange {
        id: key.section.clone(),
        rows: 0..lines.len(),
        content_columns: content_columns_for_kind(&key.section.kind, key.width as usize),
        collapsible,
        folded,
    };
    let row_texts = (0..lines.len())
        .map(|row| {
            let text = section_copy_text(&range, row, &lines, range.content_columns.start);
            (text.clone(), section_copy_is_decorative(&range, row, &text))
        })
        .collect::<Vec<_>>();
    let source: Arc<str> = row_texts
        .iter()
        .map(|(text, _)| text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
        .into();
    let mut offset = 0;
    let hard_break_rows = source_hint.is_some_and(|source| source.contains('\n'));
    let last_content_row = row_texts
        .iter()
        .enumerate()
        .rev()
        .find(|(_, (text, decorative))| !*decorative && !text.is_empty())
        .map_or(0, |(row, _)| row);
    let copy_ranges: Vec<CopyRange> = row_texts
        .into_iter()
        .enumerate()
        .map(|(row, (text, decorative))| {
            let start = offset;
            offset += text.len();
            let end = offset;
            offset += 1;
            CopyRange {
                row,
                columns: range.content_columns.clone(),
                source: Arc::clone(&source),
                source_range: start..end,
                hard_break_after: hard_break_rows || row == last_content_row,
                decorative,
            }
        })
        .collect();
    let source_map = Arc::new(SourceMap {
        source: Arc::clone(&source),
        rows: Arc::new(
            copy_ranges
                .iter()
                .map(|copy| SourceRow {
                    source_range: copy.source_range.clone(),
                    hard_break_after: copy.hard_break_after,
                    decorative: copy.decorative,
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
    if row == decorative_row(section) {
        return String::new();
    }
    lines
        .get(row)
        .map(|line| line_copy_text(line, copy_start))
        .unwrap_or_default()
}

fn section_copy_is_decorative(section: &SectionRange, row: usize, text: &str) -> bool {
    row == decorative_row(section)
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
        SectionKind::AssistantText | SectionKind::Thinking => 1,
        SectionKind::Summary | SectionKind::Notice => 0,
    }
}

fn content_columns_for_kind(kind: &SectionKind, width: usize) -> std::ops::Range<usize> {
    let start = matches!(kind, SectionKind::User | SectionKind::Tool)
        .then_some(crate::ui::rail::SURFACE_CONTENT_START)
        .unwrap_or(1)
        .min(width);
    start..width
}

fn durable_block_lines<V: DurableLayoutSource>(
    theme: &Theme,
    view: &V,
    block: &TranscriptBlock,
    width: usize,
    reasoning_visible: bool,
) -> Vec<Line<'static>> {
    match block {
        TranscriptBlock::User(user_block) => user::lines_with_timestamp(
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
        ),
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
        TranscriptBlock::Summary(summary) => summary_lines(theme, width, &summary.content),
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
        .map(|presentation| &presentation.display)
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
                &mut lines,
                live_sections,
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
}

impl LiveRenderContext<'_> {
    fn append_reasoning(
        &self,
        out: &mut Vec<Line<'static>>,
        ranges: &mut Option<&mut Vec<SectionRange>>,
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
            reasoning::reasoning_lines_with_fold(
                self.theme,
                text,
                self.width,
                self.reasoning_visible,
                in_hidden_run,
                expanded,
            ),
            self.width,
            self.reasoning_visible && raw_lines > 3,
            self.reasoning_visible && raw_lines > 3 && !expanded.unwrap_or(false),
        );
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
        .map(|presentation| &presentation.display);
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
    out: &mut Vec<Line<'static>>,
    ranges: Option<&mut Vec<SectionRange>>,
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
