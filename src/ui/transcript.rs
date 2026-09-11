//! The transcript/history scroll view: durable blocks and the live loop tail (spec r2).

use std::collections::HashSet;
use std::sync::Arc;

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
use crate::state::transcript::{ToolBlock, TranscriptBlock};
use crate::state::view::{
    ConversationSelection, CopyRange, DurableCacheKey, FoldOverride, PreparedConversation,
    PreparedDurable, SectionId, SectionKind, SectionRange,
};
use crate::theme::Theme;
use crate::ui::{assistant, header, layout, reasoning, tool, user};

struct PreparedSection {
    id: SectionId,
    lines: Vec<Line<'static>>,
    link_cells: Vec<Vec<std::ops::Range<usize>>>,
    collapsible: bool,
    folded: bool,
}

/// Builds one complete immutable conversation snapshot. The same rows and
/// metadata are consumed by measurement, rendering, hit testing, selection,
/// and copying; callers install the result through `App::update`.
pub fn prepare_conversation(app: &App, width: u16) -> PreparedConversation {
    let theme = app.theme.theme();
    let durable = app.active_view().map(|view| {
        let key = DurableCacheKey::new(view, width, app.theme, app.reasoning_visible);
        if let Some(cached) = view
            .transcript
            .render_cache
            .as_ref()
            .filter(|cached| cached.key == key)
        {
            return Arc::clone(cached);
        }
        let (lines, sections, copy_ranges, link_cells) =
            build_durable_prepared(&theme, view, width as usize, app.reasoning_visible);
        Arc::new(PreparedDurable {
            key,
            lines,
            sections,
            copy_ranges,
            link_cells,
        })
    });
    let mut live_sections = Vec::new();
    let header_rows = header::lines(&theme, app).len();
    let (durable_lines, durable_links_aligned) = all_lines_with_durable(
        &theme,
        app,
        width as usize,
        durable.as_ref().map_or(&[], |d| d.lines.as_slice()),
        durable.as_ref().map_or(&[], |d| d.link_cells.as_slice()),
        durable
            .as_ref()
            .and_then(|d| d.sections.last().map(|section| section.id.kind)),
        Some(&mut live_sections),
    );
    let mut conversation = PreparedConversation {
        lines: durable_lines,
        link_cells: durable_links_aligned,
        width,
        session_id: app.active_view().map(|view| view.info.session_id.clone()),
        transcript_revision: app
            .active_view()
            .map_or(0, |view| view.transcript.render_revision),
        durable: durable.clone(),
        ..PreparedConversation::default()
    };
    conversation.sections = durable
        .as_ref()
        .into_iter()
        .flat_map(|d| d.sections.iter().cloned())
        .map(|mut section| {
            section.rows = section.rows.start + header_rows..section.rows.end + header_rows;
            section
        })
        .collect();
    conversation.copy_ranges = durable
        .as_ref()
        .into_iter()
        .flat_map(|d| d.copy_ranges.iter().cloned())
        .map(|mut range| {
            range.row += header_rows;
            range
        })
        .collect();
    // History copy/link metadata is already prepared; only build the live tail.
    let lines = &conversation.lines;
    conversation
        .copy_ranges
        .extend(live_sections.iter().flat_map(|section| {
            let copy_start = copy_start_for_kind(&section.id.kind);
            section.rows.clone().map(move |row| {
                let text = section_copy_text(section, row, lines, copy_start);
                CopyRange {
                    row,
                    columns: copy_start..width as usize,
                    decorative: section_copy_is_decorative(section, row, &text),
                    text,
                }
            })
        }));
    conversation.sections.extend(live_sections);
    conversation
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
    if let Some((pending, visible_rows, marker)) = app.scrollbar_drag_preview(&view.info.session_id)
    {
        let visible_rows = visible_rows.min(height).min(total);
        return ScrollPosition {
            offset: pending.min(total.saturating_sub(visible_rows.max(1))),
            visible_rows,
            marker: marker && total > height,
        };
    }
    let marker = !view.scroll.follow_tail && total > height;
    let visible_rows = if marker {
        height.saturating_sub(1)
    } else {
        height
    };
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
    app.prepared_conversation(width).map_or_else(
        || prepare_conversation(app, width).total_rows(),
        PreparedConversation::total_rows,
    )
}

/// Builds every transcript row (startup header, durable blocks, live tail).
pub fn all_lines(_theme: &Theme, app: &App, width: usize) -> Vec<Line<'static>> {
    app.prepared_conversation(width as u16).map_or_else(
        || prepare_conversation(app, width as u16).lines,
        |prepared| prepared.lines.clone(),
    )
}

/// Per rendered line, the content-cell ranges inside a markdown link.
type LinkRow = Vec<std::ops::Range<usize>>;

fn build_durable_prepared(
    theme: &Theme,
    view: &SessionView,
    width: usize,
    reasoning_visible: bool,
) -> (
    Vec<Line<'static>>,
    Vec<SectionRange>,
    Vec<CopyRange>,
    Vec<LinkRow>,
) {
    let mut lines = Vec::new();
    let mut sections = Vec::new();
    let mut copy_ranges = Vec::new();
    let mut link_cells = Vec::new();
    let mut rendered_tools = HashSet::new();
    for (ordinal, block) in view.transcript.blocks.iter().enumerate() {
        if let TranscriptBlock::Assistant(assistant_block) = block {
            for assistant_section in assistant::sections_with_folds(
                theme,
                assistant_block,
                width,
                reasoning_visible,
                &view.reasoning_folds,
            ) {
                if let Some(call) = assistant_section.tool_call {
                    let key = crate::state::tool::ToolKey::new(
                        &view.info.session_id,
                        &assistant_block.loop_id,
                        assistant_block.request_index,
                        &call.tool_call_id,
                    );
                    let tool = view
                        .transcript
                        .blocks
                        .iter()
                        .find_map(|block| match block {
                            TranscriptBlock::Tool(tool)
                                if tool.loop_id == assistant_block.loop_id
                                    && tool.request_index == assistant_block.request_index
                                    && tool.tool_call_id == call.tool_call_id =>
                            {
                                Some(tool.clone())
                            }
                            _ => None,
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
                    rendered_tools.insert(key);
                    let folded = !effective_tool_expanded(view, &tool);
                    append_prepared_section(
                        &mut lines,
                        &mut sections,
                        &mut copy_ranges,
                        &mut link_cells,
                        PreparedSection {
                            id: SectionId {
                                session_id: view.info.session_id.clone(),
                                loop_id: Some(tool.loop_id.clone()),
                                request_index: Some(tool.request_index),
                                kind: SectionKind::Tool,
                                ordinal: 0,
                                tool_call_id: Some(tool.tool_call_id.clone()),
                                // Keep the assistant/tool relationship stable while a
                                // result arrives and supplies its own history index.
                                history_index: Some(assistant_block.index),
                            },
                            lines: durable_block_lines(
                                theme,
                                view,
                                &TranscriptBlock::Tool(tool.clone()),
                                width,
                                reasoning_visible,
                            ),
                            link_cells: Vec::new(),
                            collapsible: true,
                            folded,
                        },
                        width,
                    );
                    continue;
                }
                append_prepared_section(
                    &mut lines,
                    &mut sections,
                    &mut copy_ranges,
                    &mut link_cells,
                    PreparedSection {
                        id: SectionId {
                            session_id: view.info.session_id.clone(),
                            loop_id: Some(assistant_block.loop_id.clone()),
                            request_index: Some(assistant_block.request_index),
                            kind: assistant_section.kind,
                            ordinal: assistant_section.ordinal,
                            tool_call_id: None,
                            history_index: Some(assistant_block.index),
                        },
                        lines: assistant_section.lines,
                        link_cells: assistant_section.link_cells,
                        collapsible: assistant_section.collapsible,
                        folded: assistant_section.folded,
                    },
                    width,
                );
            }
            continue;
        }
        if let TranscriptBlock::Tool(tool) = block {
            let key = crate::state::tool::ToolKey::new(
                &view.info.session_id,
                &tool.loop_id,
                tool.request_index,
                &tool.tool_call_id,
            );
            if rendered_tools.contains(&key) {
                continue;
            }
        }
        let section = durable_block_lines(theme, view, block, width, reasoning_visible);
        if section.is_empty() {
            continue;
        }
        let id = section_id(&view.info.session_id, block, ordinal as u32);
        append_user_gap(
            &mut lines,
            &mut link_cells,
            sections.last().map(|section| section.id.kind),
            id.kind,
        );
        let before = lines.len();
        layout::append_section(&mut lines, section);
        let after = lines.len();
        // Non-markdown blocks never carry links, but link row alignment must
        // stay exact with the line array.
        while link_cells.len() < lines.len() {
            link_cells.push(Vec::new());
        }
        sections.push(SectionRange {
            id,
            rows: before..after,
            content_columns: content_columns_for(block, width),
            collapsible: matches!(block, TranscriptBlock::Tool(_)),
            folded: matches!(block, TranscriptBlock::Tool(tool) if {
                let key = crate::state::tool::ToolKey::new(
                    &view.info.session_id,
                    &tool.loop_id,
                    tool.request_index,
                    &tool.tool_call_id,
                );
                matches!(view.tool_folds.get(&key), Some(FoldOverride::Collapsed))
                    || !effective_tool_expanded(view, tool)
            }),
        });
        let copy_start = copy_start_for_kind(&sections.last().unwrap().id.kind);
        for row in before..after {
            let section = sections.last().expect("section was just appended");
            let text = section_copy_text(section, row, &lines, copy_start);
            let decorative = section_copy_is_decorative(section, row, &text);
            copy_ranges.push(CopyRange {
                row,
                columns: copy_start..width,
                text,
                decorative,
            });
        }
    }
    (lines, sections, copy_ranges, link_cells)
}

fn append_prepared_section(
    lines: &mut Vec<Line<'static>>,
    sections: &mut Vec<SectionRange>,
    copy_ranges: &mut Vec<CopyRange>,
    link_cells: &mut Vec<Vec<std::ops::Range<usize>>>,
    prepared: PreparedSection,
    width: usize,
) {
    let PreparedSection {
        id,
        lines: section,
        link_cells: section_links,
        collapsible,
        folded,
    } = prepared;
    if section.is_empty() {
        return;
    }
    append_user_gap(
        lines,
        link_cells,
        sections.last().map(|section| section.id.kind),
        id.kind,
    );
    let before = lines.len();
    layout::append_section(lines, section);
    let after = lines.len();
    // A shared blank boundary row may have been dropped from the front; it is
    // always blank so its link rows (empty) can be discarded with it.
    let added = after - before;
    let section_links = &section_links[section_links.len().saturating_sub(added)..];
    link_cells.extend(section_links.iter().cloned());
    let content_columns = content_columns_for_kind(&id.kind, width);
    let copy_start = copy_start_for_kind(&id.kind);
    sections.push(SectionRange {
        id,
        rows: before..after,
        content_columns,
        collapsible,
        folded,
    });
    for row in before..after {
        let section = sections.last().expect("section was just appended");
        let text = section_copy_text(section, row, lines, copy_start);
        let decorative = section_copy_is_decorative(section, row, &text);
        copy_ranges.push(CopyRange {
            row,
            columns: copy_start..width,
            text,
            decorative,
        });
    }
}

fn needs_user_gap(previous: Option<SectionKind>, current: SectionKind) -> bool {
    previous == Some(SectionKind::User) && current == SectionKind::User
}

fn append_user_gap(
    lines: &mut Vec<Line<'static>>,
    link_cells: &mut Vec<LinkRow>,
    previous: Option<SectionKind>,
    current: SectionKind,
) {
    if needs_user_gap(previous, current) {
        lines.push(Line::default());
        link_cells.push(Vec::new());
    }
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
        rows.push(slice_cell_range(&copy.text, start_column, end_column));
    }
    while rows.first().is_some_and(|row| row.is_empty()) {
        rows.remove(0);
    }
    while rows.last().is_some_and(|row| row.is_empty()) {
        rows.pop();
    }
    let mut result = String::new();
    for row in rows {
        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str(&row);
    }
    result
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
    sections: &[SectionRange],
    theme: &Theme,
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
            let content_start = section.map_or(0, |section| section.content_columns.start);
            let content_end = section.map_or(usize::MAX, |section| section.content_columns.end);
            let from = (if row == start.row { start.column } else { 0 }).max(content_start);
            let to = (if row == end.row {
                end.column.saturating_add(1)
            } else {
                usize::MAX
            })
            .min(content_end);
            select_line_cells(line, from, to, theme)
        })
        .collect()
}

fn select_line_cells(line: Line<'static>, from: usize, to: usize, theme: &Theme) -> Line<'static> {
    let mut used = 0;
    let mut spans = Vec::new();
    for span in line.spans {
        for grapheme in span.content.graphemes(true) {
            let width = UnicodeWidthStr::width(grapheme);
            let selected = used < to && used + width > from;
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

fn content_columns_for(block: &TranscriptBlock, width: usize) -> std::ops::Range<usize> {
    let kind = match block {
        TranscriptBlock::User(_) => SectionKind::User,
        TranscriptBlock::Assistant(_) => SectionKind::AssistantText,
        TranscriptBlock::Tool(_) => SectionKind::Tool,
        TranscriptBlock::Summary(_) => SectionKind::Summary,
    };
    content_columns_for_kind(&kind, width)
}

fn content_columns_for_kind(kind: &SectionKind, width: usize) -> std::ops::Range<usize> {
    let start = matches!(kind, SectionKind::User | SectionKind::Tool)
        .then_some(crate::ui::rail::SURFACE_CONTENT_START)
        .unwrap_or(1)
        .min(width);
    start..width
}

fn durable_block_lines(
    theme: &Theme,
    view: &SessionView,
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
                .and_then(|index| view.user_timestamps.get(&index).map(String::as_str))
                .or_else(|| {
                    user_block.loop_id.as_ref().and_then(|loop_id| {
                        view.live_user_timestamp.as_deref().filter(|_| {
                            view.live.as_ref().is_some_and(|live| {
                                live.reference
                                    .as_ref()
                                    .is_some_and(|turn| &turn.loop_id == loop_id)
                            })
                        })
                    })
                }),
            user_block.pending
                && !view.live_user_time_accepted
                && view.live_user_timestamp.is_none(),
        ),
        TranscriptBlock::Assistant(assistant_block) => assistant::lines_with_folds(
            theme,
            assistant_block,
            width,
            reasoning_visible,
            &view.reasoning_folds,
        ),
        TranscriptBlock::Tool(tool_block) => {
            let render_tool = effective_tool_block(view, tool_block);
            let display = tool_display(view, tool_block);
            tool::durable_with_display(theme, &render_tool, width, false, display)
        }
        TranscriptBlock::Summary(summary) => summary_lines(theme, width, &summary.content),
    }
}

fn section_id(session_id: &str, block: &TranscriptBlock, _ordinal: u32) -> SectionId {
    match block {
        TranscriptBlock::User(user) => SectionId {
            session_id: session_id.to_owned(),
            loop_id: user.loop_id.clone(),
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
            session_id: session_id.to_owned(),
            loop_id: Some(assistant.loop_id.clone()),
            request_index: Some(assistant.request_index),
            kind: SectionKind::AssistantText,
            ordinal: 0,
            tool_call_id: None,
            history_index: Some(assistant.index),
        },
        TranscriptBlock::Tool(tool) => SectionId {
            session_id: session_id.to_owned(),
            loop_id: Some(tool.loop_id.clone()),
            request_index: Some(tool.request_index),
            kind: SectionKind::Tool,
            ordinal: 0,
            tool_call_id: Some(tool.tool_call_id.clone()),
            history_index: tool.index,
        },
        TranscriptBlock::Summary(_summary) => SectionId {
            session_id: session_id.to_owned(),
            loop_id: None,
            request_index: None,
            kind: SectionKind::Summary,
            ordinal: 0,
            tool_call_id: None,
            history_index: Some(_summary.index),
        },
    }
}

fn tool_display<'a>(
    view: &'a SessionView,
    tool: &ToolBlock,
) -> Option<&'a crate::protocol::ToolDisplayWire> {
    view.tool_presentations
        .get(&crate::state::tool::ToolKey::new(
            &view.info.session_id,
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
    let key = crate::state::tool::ToolKey::new(
        &view.info.session_id,
        &tool.loop_id,
        tool.request_index,
        &tool.tool_call_id,
    );
    resolve_tool_expanded(
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
    match view.tool_folds.get(key) {
        Some(crate::state::view::FoldOverride::Expanded) => true,
        Some(crate::state::view::FoldOverride::Collapsed) => false,
        None => {
            base_expanded || view.tools_expanded || tool::default_expanded(name, hidden_line_count)
        }
    }
}

fn effective_tool_block(view: &SessionView, tool: &ToolBlock) -> ToolBlock {
    let mut render_tool = tool.clone();
    render_tool.expanded = effective_tool_expanded(view, tool);
    render_tool
}

fn all_lines_with_durable(
    theme: &Theme,
    app: &App,
    width: usize,
    durable: &[Line<'static>],
    durable_links: &[LinkRow],
    durable_last_kind: Option<SectionKind>,
    live_sections: Option<&mut Vec<SectionRange>>,
) -> (Vec<Line<'static>>, Vec<LinkRow>) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut link_rows: Vec<LinkRow> = Vec::new();
    let header = header::lines(theme, app);
    link_rows.extend(header.iter().map(|_| Vec::new()));
    lines.extend(header);
    if let Some(view) = app.active_view() {
        let before = lines.len();
        layout::append_section_ref(&mut lines, durable);
        // Keep link rows aligned with whatever the boundary check actually
        // appended (it may drop one leading blank, which is always link-free).
        let added = lines.len() - before;
        link_rows.extend(
            durable_links[durable_links.len().saturating_sub(added)..]
                .iter()
                .cloned(),
        );
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
    // While the busy status row is visible, exactly one clear transparent
    // blank must separate the last transcript row from it, even when content
    // fills the viewport. Sections that already end with a transparent blank
    // (assistant text) must not get a second one; this row belongs to no
    // section, so ranges/copy/hits exclude it consistently.
    if layout::busy(app)
        && lines
            .last()
            .is_some_and(|line| !layout::is_transparent_blank(line))
    {
        lines.push(Line::default());
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
        result.requests,
        result.tool_rounds
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
                session_id: self.session_id.to_owned(),
                loop_id: Some(self.loop_id.to_owned()),
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
                session_id: self.session_id.to_owned(),
                loop_id: Some(self.loop_id.to_owned()),
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
            session_id: view.info.session_id.clone(),
            loop_id: Some(loop_id.to_owned()),
            request_index: Some(request_index),
            kind: SectionKind::Tool,
            ordinal: 0,
            tool_call_id: Some(tool.tool_call_id.clone()),
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

        if req.parts.is_empty() {
            // Old live state has only flattened channels. Keep the legacy
            // fallback explicit: its original order is unknown, so it is not
            // represented as if it were an ordered parts stream.
            if !req.reasoning_text.is_empty() {
                context.append_reasoning(out, &mut ranges, &req.reasoning_text, 0, false);
            }
            if !req.text.is_empty() {
                context.append_text(out, &mut ranges, &req.text, 0);
            }
            for live_tool in &req.tools {
                rendered_tool_ids.insert(live_tool.tool_call_id.clone());
                context.append_tool(out, &mut ranges, live_tool);
            }
        } else {
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
                session_id: session_id.clone(),
                loop_id: Some(loop_id.clone()),
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
                session_id: session_id.clone(),
                loop_id: Some(loop_id.clone()),
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
    let fallback;
    let prepared = match app.prepared_conversation(area.width) {
        Some(prepared) => prepared,
        None => {
            fallback = prepare_conversation(app, area.width);
            &fallback
        }
    };
    let total = prepared.lines.len();
    let position = scroll_position(app, total, height);
    let offset = position.offset;
    let marker = position.marker;
    let budget = position.visible_rows;
    let slice: Vec<Line<'static>> = apply_selection(
        prepared
            .lines
            .iter()
            .skip(offset)
            .take(budget)
            .cloned()
            .collect(),
        offset,
        app.selection.as_ref(),
        prepared.sections.as_slice(),
        theme,
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
        render_marker(frame, marker_area, app, theme);
    }
    if app.active_view().is_some() {
        let visible = position.visible_rows;
        crate::ui::scrollbar::render(frame, area, total, visible, offset, theme);
    }
}

fn render_marker(frame: &mut Frame<'_>, area: Rect, app: &App, theme: &Theme) {
    let label = if app
        .active_view()
        .is_some_and(|view| view.scroll.new_content)
    {
        "↓ new output"
    } else {
        "↑ scroll position"
    };
    let line = layout::filled(
        label,
        area.width as usize,
        Style::new().fg(theme.dim).bg(theme.page_bg),
    );
    frame.render_widget(ratatui::widgets::Paragraph::new(line), area);
}
