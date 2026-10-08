//! Assistant text is transparent and aligned to the thinking Rail. Tool-call
//! identity parts stay in the state model; matching ToolBlocks render the
//! execution surface as siblings without exposing arguments here.

use std::collections::HashMap;
use std::sync::Arc;

use ratatui::style::Style;
use ratatui::text::Line;

use crate::markdown::MarkdownRenderer;
use crate::state::transcript::{AssistantBlock, AssistantPart};
use crate::state::view::{FoldOverride, ReasoningKey, SectionKind};
use crate::theme::Theme;
use crate::ui::{layout, reasoning};

pub fn lines(
    theme: &Theme,
    block: &AssistantBlock,
    width: usize,
    reasoning_visible: bool,
) -> Vec<Line<'static>> {
    lines_with_folds(theme, block, width, reasoning_visible, &HashMap::new())
}

pub fn lines_with_folds(
    theme: &Theme,
    block: &AssistantBlock,
    width: usize,
    reasoning_visible: bool,
    folds: &HashMap<ReasoningKey, FoldOverride>,
) -> Vec<Line<'static>> {
    // Mirror the transcript's boundary sharing: a section's leading blank is
    // consumed by the previous section's trailing blank, so the flattened
    // rows keep exactly one transparent spacer between sections.
    sections_with_folds(theme, block, width, reasoning_visible, folds)
        .into_iter()
        .fold(
            (Vec::new(), false),
            |(mut lines, mut trailing_blank), section| {
                for (index, line) in section.lines.into_iter().enumerate() {
                    if index == 0 && trailing_blank && line.spans.is_empty() {
                        continue;
                    }
                    trailing_blank = line.spans.is_empty();
                    lines.push(line);
                }
                (lines, trailing_blank)
            },
        )
        .0
}

pub struct AssistantSection {
    pub lines: Vec<Line<'static>>,
    /// Content-cell ranges that are inside a markdown link, per rendered
    /// line (parallel to `lines`). Empty for non-link sections.
    pub link_cells: Vec<Vec<std::ops::Range<usize>>>,
    pub kind: SectionKind,
    pub ordinal: u32,
    pub collapsible: bool,
    pub folded: bool,
    /// A ToolCall part is a position marker. The matching ToolBlock is
    /// rendered by the transcript at this exact point rather than being
    /// moved after all assistant text.
    pub tool_call: Option<crate::protocol::ToolCallViewWire>,
    /// For markdown-rendered text: per rendered row, whether the row ends a
    /// logical source line rather than a soft wrap. `None` when the rows came
    /// from a plain wrapper that already preserves source lines.
    pub hard_breaks: Option<Vec<bool>>,
    pub copy_cells: Option<Vec<Option<crate::markdown::CopyCells>>>,
}

/// Cheap section metadata collected before Markdown/layout work. The source
/// is shared with the eventual SectionLayout so unchanged sections can be
/// reused without rendering their siblings again.
#[derive(Clone)]
pub struct AssistantSectionInput {
    pub source: Arc<str>,
    pub kind: SectionKind,
    pub ordinal: u32,
    pub collapsible: bool,
    pub folded: bool,
    pub tool_call: Option<crate::protocol::ToolCallViewWire>,
    pub in_hidden_run: bool,
}

/// Enumerates ordered assistant sections without invoking Markdown rendering.
pub fn section_inputs(
    block: &AssistantBlock,
    reasoning_visible: bool,
    folds: &HashMap<ReasoningKey, FoldOverride>,
) -> Vec<AssistantSectionInput> {
    let mut out = Vec::new();
    let mut in_hidden_run = false;
    let mut index = 0;
    let mut reasoning_ordinal = 0;
    let mut text_ordinal = 0;
    while index < block.parts.len() {
        if matches!(block.parts[index], AssistantPart::Reasoning(_)) {
            let mut reasoning_parts = Vec::new();
            while let Some(AssistantPart::Reasoning(text)) = block.parts.get(index) {
                reasoning_parts.push(text.as_str());
                index += 1;
            }
            let source_bytes = reasoning_parts.iter().map(|text| text.len()).sum::<usize>();
            let joined: Arc<str> = if source_bytes > crate::limits::LAYOUT_SECTION_BYTES {
                format!(
                    "[large reasoning section: {source_bytes} bytes; read explicitly to render]"
                )
                .into()
            } else {
                reasoning_parts.concat().into()
            };
            let key = ReasoningKey::new(&block.loop_id, block.request_index, reasoning_ordinal);
            reasoning_ordinal += 1;
            let expanded = folds.get(&key).map(FoldOverride::expanded);
            // Actual fold eligibility is determined during width-aware rendering.
            let collapsible = reasoning_visible;
            out.push(AssistantSectionInput {
                source: joined,
                kind: SectionKind::Thinking,
                ordinal: reasoning_ordinal - 1,
                collapsible,
                folded: collapsible && !expanded.unwrap_or(false),
                tool_call: None,
                in_hidden_run,
            });
            in_hidden_run = !reasoning_visible;
            continue;
        }

        let part = &block.parts[index];
        index += 1;
        match part {
            AssistantPart::Text(text) => {
                out.push(AssistantSectionInput {
                    source: if text.len() > crate::limits::LAYOUT_SECTION_BYTES {
                        format!(
                            "[large assistant section: {} bytes; read explicitly to render]",
                            text.len()
                        )
                        .into()
                    } else {
                        Arc::from(text.as_str())
                    },
                    kind: SectionKind::AssistantText,
                    ordinal: text_ordinal,
                    collapsible: false,
                    folded: false,
                    tool_call: None,
                    in_hidden_run: false,
                });
                text_ordinal += 1;
                in_hidden_run = false;
            }
            AssistantPart::ToolCall(call) => {
                out.push(AssistantSectionInput {
                    source: Arc::from(""),
                    kind: SectionKind::Tool,
                    ordinal: call.call_index,
                    collapsible: true,
                    folded: false,
                    tool_call: Some(call.clone()),
                    in_hidden_run: false,
                });
                in_hidden_run = false;
            }
            AssistantPart::Reasoning(_) => unreachable!("reasoning runs are handled above"),
        }
    }
    out
}

/// Renders one previously enumerated section. This is the only function in
/// the incremental path that performs Markdown or reasoning layout work.
pub fn render_section(
    theme: &Theme,
    input: &AssistantSectionInput,
    width: usize,
    reasoning_visible: bool,
) -> AssistantSection {
    match input.kind {
        SectionKind::Thinking => {
            let (rendered, collapsible) = reasoning::render_with_state(
                theme,
                &input.source,
                width,
                reasoning_visible,
                input.in_hidden_run,
                Some(!input.folded),
                false,
            );
            AssistantSection {
                link_cells: rendered.link_cells,
                lines: rendered.lines,
                kind: input.kind,
                ordinal: input.ordinal,
                collapsible,
                folded: collapsible && input.folded,
                tool_call: None,
                hard_breaks: Some(rendered.hard_breaks),
                copy_cells: Some(rendered.copy_cells),
            }
        }
        SectionKind::AssistantText => {
            render_text_section(theme, &input.source, width, input.ordinal)
        }
        SectionKind::Tool | SectionKind::User | SectionKind::Summary | SectionKind::Notice => {
            AssistantSection {
                lines: Vec::new(),
                link_cells: Vec::new(),
                kind: input.kind,
                ordinal: input.ordinal,
                collapsible: input.collapsible,
                folded: input.folded,
                tool_call: input.tool_call.clone(),
                hard_breaks: None,
                copy_cells: None,
            }
        }
    }
}

/// The same width-aware Markdown and interaction geometry is used while text
/// streams and after it is persisted. No second parse is needed for links/copy.
pub(super) fn render_text_section(
    theme: &Theme,
    text: &str,
    width: usize,
    ordinal: u32,
) -> AssistantSection {
    let renderer = MarkdownRenderer::new(theme);
    let base = Style::new().fg(theme.text);
    let inner = width.saturating_sub(1).max(1);
    let rendered = renderer.render_with_metadata(text, inner, base);
    let links = rendered.link_cells;
    let breaks = rendered.hard_breaks;
    let copies = rendered.copy_cells;
    let lines: Vec<_> = rendered
        .lines
        .into_iter()
        .map(|line| crate::ui::rail::inset_row(width, 1, line))
        .collect();
    let link_cells: Vec<Vec<std::ops::Range<usize>>> = links
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|range| range.start + 1..range.end + 1)
                .collect()
        })
        .collect();
    let vertical = layout::vertical_section(lines);
    let mut vertical_links = Vec::with_capacity(vertical.len());
    let mut vertical_breaks = Vec::with_capacity(vertical.len());
    let mut vertical_copies = Vec::with_capacity(vertical.len());
    if !vertical.is_empty() {
        vertical_links.push(Vec::new());
        vertical_links.extend(link_cells);
        vertical_links.push(Vec::new());
        vertical_breaks.push(false);
        vertical_breaks.extend(breaks);
        vertical_breaks.push(false);
        vertical_copies.push(Some(crate::markdown::CopyCells::decoration()));
        vertical_copies.extend(
            copies
                .into_iter()
                .map(|copy| copy.map(|copy| copy.shifted(1))),
        );
        vertical_copies.push(Some(crate::markdown::CopyCells::decoration()));
    }
    AssistantSection {
        lines: vertical,
        link_cells: vertical_links,
        kind: SectionKind::AssistantText,
        ordinal,
        collapsible: false,
        folded: false,
        tool_call: None,
        hard_breaks: Some(vertical_breaks),
        copy_cells: Some(vertical_copies),
    }
}

pub fn sections_with_folds(
    theme: &Theme,
    block: &AssistantBlock,
    width: usize,
    reasoning_visible: bool,
    folds: &HashMap<ReasoningKey, FoldOverride>,
) -> Vec<AssistantSection> {
    let mut out = Vec::new();
    let mut in_hidden_run = false;
    let mut index = 0;
    let mut reasoning_ordinal = 0;
    let mut text_ordinal = 0;
    while index < block.parts.len() {
        if matches!(block.parts[index], AssistantPart::Reasoning(_)) {
            let mut reasoning_parts = Vec::new();
            while let Some(AssistantPart::Reasoning(text)) = block.parts.get(index) {
                reasoning_parts.push(text.as_str());
                index += 1;
            }
            let joined = reasoning_parts.concat();
            let key = ReasoningKey::new(&block.loop_id, block.request_index, reasoning_ordinal);
            reasoning_ordinal += 1;
            let expanded = folds.get(&key).map(FoldOverride::expanded);
            let (rendered, collapsible) = reasoning::render_with_state(
                theme,
                &joined,
                width,
                reasoning_visible,
                in_hidden_run,
                expanded,
                false,
            );
            let has_section = !rendered.lines.is_empty();
            if has_section {
                let folded = collapsible && !expanded.unwrap_or(false);
                out.push(AssistantSection {
                    lines: rendered.lines,
                    link_cells: rendered.link_cells,
                    kind: SectionKind::Thinking,
                    ordinal: reasoning_ordinal - 1,
                    collapsible,
                    folded,
                    tool_call: None,
                    hard_breaks: Some(rendered.hard_breaks),
                    copy_cells: Some(rendered.copy_cells),
                });
                in_hidden_run = !reasoning_visible;
            }
            continue;
        }

        let part = &block.parts[index];
        index += 1;
        match part {
            AssistantPart::Text(text) => {
                out.push(render_text_section(theme, text, width, text_ordinal));
                text_ordinal += 1;
                in_hidden_run = false;
            }
            AssistantPart::Reasoning(_) => unreachable!("reasoning runs are handled above"),
            AssistantPart::ToolCall(call) => {
                // Preserve this position in the ordered parts. The matching
                // ToolBlock is rendered by the transcript at full identity;
                // never stringify call arguments here.
                out.push(AssistantSection {
                    lines: Vec::new(),
                    link_cells: Vec::new(),
                    kind: SectionKind::Tool,
                    ordinal: call.call_index,
                    collapsible: true,
                    folded: false,
                    tool_call: Some(call.clone()),
                    hard_breaks: None,
                    copy_cells: None,
                });
                in_hidden_run = false;
            }
        }
    }
    out
}
