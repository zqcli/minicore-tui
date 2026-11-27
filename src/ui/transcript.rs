//! The transcript pane: block rendering through the layout cache, scroll
//! resolution, the welcome header and the overlays drawn over the rows: drag
//! highlight, jump label and scrollbar. A frame touches only the
//! visible rows.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_segmentation::UnicodeSegmentation;

use super::blocks;
use super::markdown::StreamRenderer;
use super::theme::Styles;
use crate::app::scrollbar::{JUMP_LABEL, jump_label, thumb};
use crate::app::selection;
use crate::app::{App, Overlay, Scroll, find};
use crate::transcript::layout::{Fold, Layout, Rows, Viewport};
use crate::transcript::{Block, Grouping, RequestKey, StepKind, Transcript};

/// Streaming markdown renderers of live assistant blocks, by request. Each
/// caches its stable prefix, so a delta re-parses only the tail.
#[derive(Default)]
pub struct RenderState {
    streams: HashMap<RequestKey, Stream>,
    /// What the streams were rendered for; any change restarts them.
    key: Option<(u16, Styles)>,
    /// Rows of the welcome header cached in front of block 0.
    pub header_rows: usize,
}

struct Stream {
    text: StreamRenderer,
    reasoning: StreamRenderer,
}

impl RenderState {
    fn prepare(&mut self, key: (u16, Styles), blocks: &[Block]) {
        if self.key != Some(key) {
            self.key = Some(key);
            self.streams.clear();
        }
        // Live blocks sit at the end, so a match is found after a few steps.
        self.streams.retain(|key, _| {
            blocks.iter().rev().any(|block| {
                matches!(block, Block::Assistant { key: live, streaming: true, .. } if live == key)
            })
        });
    }

    fn render(&mut self, block: &Block, (width, fold, styles): (u16, Fold, Styles)) -> Rows {
        let Block::Assistant {
            key,
            reasoning,
            text,
            streaming: true,
            expanded,
            ..
        } = block
        else {
            return blocks::render(block, styles, width, fold).into();
        };
        if (width as usize) < blocks::MIN_WIDTH
            && reasoning.trim().is_empty()
            && text.trim().is_empty()
        {
            return blocks::render(block, styles, width, fold).into();
        }
        let inner = blocks::text_width(width as usize);
        let stream = self.streams.entry(key.clone()).or_insert_with(|| Stream {
            text: StreamRenderer::new(inner, styles.text()).inset(blocks::INSET),
            reasoning: StreamRenderer::new(inner, blocks::thinking_style(styles)),
        });
        stream.text.reset(text);
        stream.reasoning.reset(&blocks::reasoning_source(reasoning));
        let thinking: Vec<&Line<'static>> = stream
            .reasoning
            .rows(&styles)
            .iter()
            .flat_map(|chunk| chunk.iter())
            .collect();
        let mut head = blocks::thinking_rows(
            &thinking,
            expanded.unwrap_or(fold.expanded),
            fold.show_reasoning,
            styles,
            width as usize,
        );
        let reply = stream.text.rows(&styles);
        if !head.is_empty() && !reply.is_empty() {
            head.push(Line::default());
        }
        let mut rows = Rows::from(head);
        for chunk in reply {
            rows.push(chunk.clone());
        }
        rows
    }
}

/// `path` with the home directory written `~`.
pub fn home_relative(path: &Path) -> PathBuf {
    let home = crate::config::home_dir();
    match home
        .as_deref()
        .and_then(|home| path.strip_prefix(home).ok())
    {
        Some(rest) => Path::new("~").join(rest),
        None => path.to_path_buf(),
    }
}

/// The transcript's part of `area`: everything right of the one-column gutter.
pub fn inner(area: Rect) -> Rect {
    let gutter = area.width.min(1);
    Rect {
        x: area.x + gutter,
        width: area.width - gutter,
        ..area
    }
}

/// Draws the transcript into `area` and records the drawn rows for copy.
pub fn draw(frame: &mut Frame, area: Rect, app: &mut App, styles: Styles) {
    let area = inner(area);
    let workspace = match app.resident.current() {
        Some(session) => session
            .info
            .config
            .workspace
            .as_ref()
            .map(|workspace| Path::new(&workspace.path)),
        None => Some(app.workspace.as_path()),
    };
    let welcome = || header(styles, area.width, workspace);
    let Some(session) = app.resident.current() else {
        // A draft shows the header; only waiting gets a line under it.
        let status = if !app.ready {
            Some("Starting MiniCore Host…")
        } else if app.creating_session() {
            Some("Creating the session…")
        } else if app.loading_session {
            Some("Opening the session…")
        } else {
            None
        };
        let mut lines = welcome();
        if let Some(status) = status {
            lines.push(Line::styled(format!(" {status}"), styles.dim()));
        }
        frame.render_widget(Paragraph::new(lines), area);
        return;
    };
    // Block renderers subtract their inset from the width.
    let width = area.width.max(blocks::INSET as u16 + 1);
    let fold = Fold {
        expanded: app.tools_expanded,
        show_reasoning: app.show_reasoning,
    };
    let key = (width, fold, styles);
    let layout = &mut app.layout;
    let blocks = session.transcript.blocks();
    layout.state.prepare((width, styles), blocks);
    // The header rides in front of block 0, so it scrolls and anchors with the
    // transcript without being a block.
    let transcript = &session.transcript;
    let mut render = |state: &mut RenderState, index: usize, block: &Block| {
        let mut rows = match transcript.grouping(index) {
            // Drawn inside the Agent card it belongs to.
            _ if transcript.absorbed(index) => Vec::new().into(),
            Grouping::None => state.render(block, key),
            // Steps are finished blocks; they need no streaming state.
            grouping => blocks::render_with(block, grouping, styles, width, fold).into(),
        };
        if index == 0 {
            let header = welcome();
            state.header_rows = header.len();
            rows.prepend(header);
        }
        rows
    };
    layout.prepare(
        blocks,
        session.transcript.revisions(),
        |index| !matches!(transcript.grouping(index), Grouping::None),
        width,
        fold,
        &mut render,
    );
    let height = area.height as usize;
    // The dock grows and shrinks under an anchored view; its bottom row stays
    // put, so the tail the user is reading is never covered.
    let drawn = layout.viewport.height;
    if let Scroll::Anchor { block, offset } = app.scroll
        && drawn > 0
        && drawn != height
    {
        let bottom = (layout.anchor_row(block, offset) + drawn).min(layout.total_rows());
        let (block, offset) = layout.anchor(bottom.saturating_sub(height));
        app.scroll = Scroll::Anchor { block, offset };
    }
    let scroll = app.scroll;
    let held = app.held;
    let resolve = |layout: &Layout<RenderState>| {
        let max_top = layout.total_rows().saturating_sub(height);
        match scroll {
            Scroll::Follow => max_top,
            // A fold keeps the row it was clicked at, even if the shorter
            // transcript then leaves blank rows below; scrolling ends that.
            Scroll::Anchor { block, offset } if held => layout
                .anchor_row(block, offset)
                .min(layout.total_rows().saturating_sub(1)),
            // An anchor is resolved against the current layout, so content
            // growing below it does not move the view; shrinking cannot blank it.
            Scroll::Anchor { block, offset } => layout.anchor_row(block, offset).min(max_top),
        }
    };
    let mut top = resolve(layout);
    while layout.settle(blocks, top, height, &mut render) {
        top = resolve(layout);
    }
    let total = layout.total_rows();
    // An anchor a shrink pushed to the end follows again, as in `scroll_to`.
    if matches!(app.scroll, Scroll::Anchor { .. }) && !held && top == total.saturating_sub(height) {
        app.scroll = Scroll::Follow;
    }
    // A selection marks screen cells, so it means nothing once the rows move.
    if top != layout.viewport.top {
        app.selection = None;
    }
    layout.viewport = Viewport {
        top,
        height,
        width: area.width,
        origin: (area.x, area.y),
    };
    let mut rows: Vec<Cow<Line>> = if blocks.is_empty() {
        let header = welcome();
        layout.state.header_rows = header.len();
        header.into_iter().map(Cow::Owned).collect()
    } else {
        layout
            .visible(top, height)
            .into_iter()
            .map(Cow::Borrowed)
            .collect()
    };
    if !blocks.is_empty() {
        animate_thinking(&mut rows, transcript, layout, fold, app.started.elapsed());
    }
    highlight_selection(&mut rows, area, app.selection);
    if let Overlay::Find { query, .. } = &app.overlay {
        let style = styles.amber().add_modifier(Modifier::REVERSED);
        for row in &mut rows {
            if let Some(marked) = find::mark(row, query, style) {
                *row = Cow::Owned(marked);
            }
        }
    }
    let buffer = frame.buffer_mut();
    for (y, row) in (area.y..area.bottom()).zip(&rows) {
        let area = Rect {
            y,
            height: 1,
            ..area
        };
        super::put_line(buffer, area, row);
    }
    if top + height < total && area.height > 0 {
        draw_jump_label(buffer, area, styles);
    }
    if app.scrollbar_shown()
        && let Some((start, len)) = thumb(height, total, top)
    {
        let glyph = match app.scrollbar_grab {
            Some(_) => "█",
            None => "┃",
        };
        for row in 0..height {
            let cell = &mut buffer[(area.right() - 1, area.y + row as u16)];
            match (start..start + len).contains(&row) {
                true => cell.set_symbol(glyph).set_style(styles.text()),
                false => cell.set_symbol("│").set_style(styles.faint()),
            };
        }
    }
}

/// Draws any `Transcript` (such as a child Session inside `Ctrl+B`) using the
/// exact same incremental layout cache, `Working`/`Worked` folding, live
/// thinking spinner, scrollbar and jump pill as the main conversation.
#[allow(clippy::too_many_arguments)]
pub fn draw_embedded(
    frame: &mut Frame,
    area: Rect,
    styles: Styles,
    transcript: &Transcript,
    layout: &mut Layout<RenderState>,
    scroll: &mut Scroll,
    selection: &mut Option<selection::Drag>,
    held: bool,
    fold: Fold,
    elapsed: std::time::Duration,
    show_scrollbar: bool,
) {
    if area.is_empty() {
        return;
    }
    let width = area.width.max(blocks::INSET as u16 + 1);
    let key = (width, fold, styles);
    let blocks = transcript.blocks();
    layout.state.prepare((width, styles), blocks);
    layout.state.header_rows = 0;
    let mut render =
        |state: &mut RenderState, index: usize, block: &Block| match transcript.grouping(index) {
            _ if transcript.absorbed(index) => Vec::new().into(),
            Grouping::None => state.render(block, key),
            grouping => blocks::render_with(block, grouping, styles, width, fold).into(),
        };
    layout.prepare(
        blocks,
        transcript.revisions(),
        |index| !matches!(transcript.grouping(index), Grouping::None),
        width,
        fold,
        &mut render,
    );
    let height = area.height as usize;
    let drawn = layout.viewport.height;
    if let Scroll::Anchor { block, offset } = *scroll
        && drawn > 0
        && drawn != height
    {
        let bottom = (layout.anchor_row(block, offset) + drawn).min(layout.total_rows());
        let (block, offset) = layout.anchor(bottom.saturating_sub(height));
        *scroll = Scroll::Anchor { block, offset };
    }
    let current_scroll = *scroll;
    let resolve = |layout: &Layout<RenderState>| {
        let max_top = layout.total_rows().saturating_sub(height);
        match current_scroll {
            Scroll::Follow => max_top,
            Scroll::Anchor { block, offset } if held => layout
                .anchor_row(block, offset)
                .min(layout.total_rows().saturating_sub(1)),
            Scroll::Anchor { block, offset } => layout.anchor_row(block, offset).min(max_top),
        }
    };
    let mut top = resolve(layout);
    while layout.settle(blocks, top, height, &mut render) {
        top = resolve(layout);
    }
    let total = layout.total_rows();
    if matches!(*scroll, Scroll::Anchor { .. }) && !held && top == total.saturating_sub(height) {
        *scroll = Scroll::Follow;
    }
    if top != layout.viewport.top || area != layout.viewport.area() {
        *selection = None;
    }
    layout.viewport = Viewport {
        top,
        height,
        width: area.width,
        origin: (area.x, area.y),
    };
    let mut rows: Vec<Cow<Line>> = layout
        .visible(top, height)
        .into_iter()
        .map(Cow::Borrowed)
        .collect();
    if !blocks.is_empty() {
        animate_thinking(&mut rows, transcript, layout, fold, elapsed);
    }
    highlight_selection(&mut rows, area, *selection);
    let buffer = frame.buffer_mut();
    for (y, row) in (area.y..area.bottom()).zip(&rows) {
        super::put_line(
            buffer,
            Rect {
                y,
                height: 1,
                ..area
            },
            row,
        );
    }
    if top + height < total && area.height > 0 {
        draw_jump_label(buffer, area, styles);
    }
    if (show_scrollbar || top + height < total || top > 0)
        && let Some((start, len)) = thumb(height, total, top)
    {
        for row in 0..height {
            let cell = &mut buffer[(area.right() - 1, area.y + row as u16)];
            match (start..start + len).contains(&row) {
                true => cell.set_symbol("┃").set_style(styles.text()),
                false => cell.set_symbol("│").set_style(styles.faint()),
            };
        }
    }
}

// The cached rows keep the static glyph; only visible, live markers animate.
fn animate_thinking(
    rows: &mut [Cow<'_, Line<'static>>],
    transcript: &Transcript,
    layout: &Layout<RenderState>,
    fold: Fold,
    elapsed: std::time::Duration,
) {
    for (index, row) in rows.iter_mut().enumerate() {
        let Some(at) = row.spans.iter().position(|span| span.content == "∴ ") else {
            continue;
        };
        let (block, _) = layout.anchor(layout.viewport.top + index);
        let thinking = match (transcript.grouping(block), transcript.blocks().get(block)) {
            (Grouping::Head { summary, open, .. }, _) if !open && !fold.expanded => {
                summary.live
                    && summary.thinking
                    && matches!(summary.latest, Some((StepKind::Thinking, _)))
            }
            (
                _,
                Some(Block::Assistant {
                    text,
                    streaming: true,
                    ..
                }),
            ) => text.trim().is_empty(),
            _ => false,
        };
        if thinking {
            row.to_mut().spans[at].content = format!("{} ", super::status::spinner(elapsed)).into();
        }
    }
}

fn draw_jump_label(buffer: &mut Buffer, area: Rect, styles: Styles) {
    // Clear the bottom row so the centered pill never sits over half a content row.
    let bar = Rect {
        y: area.bottom() - 1,
        height: 1,
        ..area
    };
    buffer.set_style(bar, Style::reset());
    for x in bar.left()..bar.right() {
        buffer[(x, bar.y)].set_symbol(" ");
    }
    let cells = jump_label(area.width);
    let pill = if styles.composer().bg.is_none() {
        Style::new().add_modifier(Modifier::REVERSED)
    } else {
        styles.composer()
    };
    let label_rect = Rect {
        x: area.x + cells.start,
        y: area.bottom() - 1,
        width: cells.len() as u16,
        height: 1,
    };
    buffer.set_style(label_rect, pill);
    let line = if let Some((lead, rest)) = JUMP_LABEL.split_once("Jump")
        && let Some((body, tail)) = rest.split_once(" · ")
    {
        Line::from(vec![
            Span::styled(lead, pill.patch(styles.accent())),
            Span::styled(format!("Jump{body}"), pill.patch(styles.text())),
            Span::styled(format!(" · {tail}"), pill.patch(styles.meta())),
        ])
    } else {
        Line::styled(JUMP_LABEL, pill.patch(styles.text()))
    };
    super::put_line(buffer, label_rect, &line);
}

/// The welcome card: name, version and current workspace.
fn header(styles: Styles, width: u16, workspace: Option<&Path>) -> Vec<Line<'static>> {
    let card = (width as usize).saturating_sub(2).min(76);
    if card < 20 {
        return vec![Line::styled(" minicore-tui", styles.title())]
            .into_iter()
            .map(super::rails::chrome)
            .collect();
    }
    let border = styles.faint();
    let inside = card - 4;
    let title = format!(" minicore-tui {} ", env!("CARGO_PKG_VERSION"));
    let top = format!(
        "╭─{title}{}╮",
        "─".repeat(card.saturating_sub(3 + crate::wrap::width(&title)))
    );
    let body = |text: String, style: Style| {
        let text = crate::wrap::truncate(&text, inside);
        let pad = inside - crate::wrap::width(&text);
        Line::from(vec![
            Span::styled(" │ ", border),
            Span::styled(text, style),
            Span::styled(format!("{} │", " ".repeat(pad)), border),
        ])
    };
    let place = workspace.map_or_else(
        || "No workspace".to_owned(),
        |path| home_relative(path).display().to_string(),
    );
    vec![
        Line::default(),
        Line::styled(format!(" {top}"), border),
        body(place, styles.text()),
        body(
            "f1 keys and commands · / commands · @ files".to_owned(),
            styles.meta(),
        ),
        Line::styled(format!(" ╰{}╯", "─".repeat(card.saturating_sub(2))), border),
        Line::default(),
    ]
    .into_iter()
    .map(super::rails::chrome)
    .collect()
}

/// Reverses cells `[from, to)` of one row, splitting spans at the boundaries;
/// a wide character is selected whole when any of its cells is.
fn highlight_selection(
    rows: &mut [Cow<'_, Line<'static>>],
    area: Rect,
    drag: Option<selection::Drag>,
) {
    if let Some(drag) = drag {
        for (index, row) in rows.iter_mut().enumerate() {
            let content = super::rails::content_cells(row);
            if let Some(span) = selection::row_span(area.y + index as u16, drag, area, content) {
                *row = Cow::Owned(highlight(row, span.start, span.end));
            }
        }
    }
}

fn highlight(line: &Line<'static>, from: u16, to: u16) -> Line<'static> {
    let reversed = |style: Style| style.add_modifier(Modifier::REVERSED);
    let mut spans = Vec::with_capacity(line.spans.len() + 2);
    let mut column = 0u16;
    for span in &line.spans {
        let mut run = String::new();
        let mut run_selected: Option<bool> = None;
        for grapheme in span.content.graphemes(true) {
            let cells = (crate::wrap::width(grapheme) as u16).max(1);
            let selected = column + cells > from && column < to;
            if let Some(picked) = run_selected.filter(|current| *current != selected) {
                spans.push(Span::styled(
                    std::mem::take(&mut run),
                    if picked {
                        reversed(span.style)
                    } else {
                        span.style
                    },
                ));
            }
            run_selected = Some(selected);
            run.push_str(grapheme);
            column += cells;
        }
        if let Some(selected) = run_selected {
            let style = if selected {
                reversed(span.style)
            } else {
                span.style
            };
            spans.push(Span::styled(run, style));
        }
    }
    Line {
        spans,
        style: line.style,
        alignment: line.alignment,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minicore_host_core::HistoryRecordData;
    use ratatui::style::Color;
    use serde_json::json;

    use crate::transcript::Transcript;
    use crate::ui::theme::Palette;

    fn reversed(style: Style) -> bool {
        style.add_modifier.contains(Modifier::REVERSED)
    }

    fn text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn assistant(index: u64, text: &str, reasoning: &str, streaming: bool) -> Block {
        Block::Assistant {
            key: RequestKey::response("00000000000000000001".parse().unwrap(), index),
            reasoning: reasoning.into(),
            text: text.into(),
            streaming,
            partial: false,
            expanded: None,
        }
    }

    fn tool_history() -> Vec<HistoryRecordData> {
        let mut records = Vec::new();
        for (index, name) in ["alpha", "beta", "gamma"].into_iter().enumerate() {
            let mut content = vec![json!({"type":"tool_call","data":{
                "tool_call_id":name,"name":name,"arguments":{},"call_index":0
            }})];
            if index == 2 {
                content.insert(
                    0,
                    json!({"type":"reasoning","data":{
                        "text":"First step\nSecond step\nThird step\nLast step"
                    }}),
                );
            }
            let common = json!({
                "recorded_at":"2026-11-15T10:00:00Z",
                "turn_id":"00000000000000000001","response_index":index
            });
            for (position, item) in [
                (
                    index * 2,
                    json!({"kind":"assistant","response":{
                        "content":content,"finish_reason":"tool_calls","usage":{}
                    }}),
                ),
                (
                    index * 2 + 1,
                    json!({"kind":"tool_result","tool_call_id":name,
                        "name":name,"result":{"outcome":"success","output":{
                            "content":format!("{name}-before\n\n\n{name}-after")
                        }}
                    }),
                ),
            ] {
                let mut record = common.clone();
                record["position"] = json!(position);
                record
                    .as_object_mut()
                    .unwrap()
                    .extend(item.as_object().unwrap().clone());
                records.push(serde_json::from_value(record).unwrap());
            }
        }
        records
    }

    fn prepare_layout(
        layout: &mut Layout<RenderState>,
        transcript: &Transcript,
        width: u16,
        fold: Fold,
    ) -> Vec<Line<'static>> {
        let styles = Styles::of(Palette::dark());
        let blocks = transcript.blocks();
        layout.state.prepare((width, styles), blocks);
        let mut render = |state: &mut RenderState, index: usize, block: &Block| match transcript
            .grouping(index)
        {
            Grouping::None => state.render(block, (width, fold, styles)),
            grouping => blocks::render_with(block, grouping, styles, width, fold).into(),
        };
        layout.prepare(
            blocks,
            transcript.revisions(),
            |index| !matches!(transcript.grouping(index), Grouping::None),
            width,
            fold,
            &mut render,
        );
        while layout.settle(blocks, 0, 1000, &mut render) {}
        layout.viewport = Viewport {
            top: 0,
            height: layout.total_rows(),
            width,
            origin: (0, 0),
        };
        layout
            .visible(0, layout.total_rows())
            .into_iter()
            .cloned()
            .collect()
    }

    #[test]
    fn empty_assistants_stay_invisible_at_narrow_and_normal_widths() {
        let styles = Styles::of(Palette::dark());
        for width in (0..=10).chain([60]) {
            for blank in ["", " \n\t"] {
                for streaming in [true, false] {
                    let block = assistant(0, blank, blank, streaming);
                    let mut state = RenderState::default();
                    let rows = state.render(&block, (width, Fold::default(), styles));
                    assert_eq!(
                        rows.iter_from(0).count(),
                        0,
                        "width {width}, streaming {streaming}"
                    );
                }
            }
            let mut partial = assistant(0, "", "", false);
            if let Block::Assistant { partial, .. } = &mut partial {
                *partial = true;
            }
            assert!(!blocks::render(&partial, styles, width, Fold::default()).is_empty());
        }
    }

    #[test]
    fn native_tool_only_responses_keep_one_gap_during_streaming_and_history_reload() {
        let records = tool_history();
        for width in [24, 72] {
            for (expanded, show_reasoning) in
                [(false, true), (true, true), (false, false), (true, false)]
            {
                let fold = Fold {
                    expanded,
                    show_reasoning,
                };
                let canonical = Transcript::from_history(&records);
                let mut live = Transcript::default();
                for block in canonical.blocks() {
                    let mut block = block.clone();
                    if let Block::Assistant { streaming, .. } = &mut block {
                        *streaming = true;
                    }
                    live.push(block);
                }
                // The app folds a live run as it goes.
                live.regroup(&"00000000000000000001".parse().unwrap());
                let mut layout = Layout::<RenderState>::default();
                let streamed = prepare_layout(&mut layout, &live, width, fold);
                live.install_history_records(&records);
                let finalized = prepare_layout(&mut layout, &live, width, fold);
                assert_eq!(streamed, finalized, "width {width}, fold {fold:?}");
                let mut reloaded = Transcript::from_history(&records);
                let mut reload_layout = Layout::<RenderState>::default();
                assert_eq!(
                    finalized,
                    prepare_layout(&mut reload_layout, &reloaded, width, fold)
                );
                for index in 0..3 {
                    let key = RequestKey::response("00000000000000000001".parse().unwrap(), index);
                    assert!(live.has_canonical_response(&key));
                    assert!(reloaded.has_canonical_response(&key));
                }
                assert_eq!(live.len(), 6, "empty canonical identities remain installed");

                let mut next = 0;
                for (index, block) in live.blocks().iter().enumerate() {
                    let body = blocks::render_with(
                        block,
                        live.grouping(index),
                        Styles::of(Palette::dark()),
                        width,
                        fold,
                    );
                    if body.is_empty() {
                        continue;
                    }
                    if next > 0 {
                        assert_eq!(
                            finalized[next].to_string(),
                            "",
                            "one separator before block {index}"
                        );
                        assert_eq!(layout.hit(2, next as u16), None);
                        next += 1;
                    }
                    assert_eq!(&finalized[next..next + body.len()], body.as_slice());
                    for offset in 0..body.len() {
                        assert_eq!(layout.hit(2, (next + offset) as u16), Some((index, offset)));
                        let (anchor, row) = layout.anchor(next + offset);
                        assert_eq!(layout.anchor_row(anchor, row), next + offset);
                    }
                    next += body.len();
                }
                assert_eq!(next, finalized.len(), "no trailing separator");
                let copied = selection::text(
                    |row| {
                        layout.row_line(row).map(|line| {
                            (line.to_string(), super::super::rails::content_cells(line))
                        })
                    },
                    layout.viewport.area(),
                    selection::Drag {
                        from: (0, 0),
                        to: (width - 1, next as u16 - 1),
                    },
                );
                // A closed run shows only its newest step.
                assert_eq!(
                    copied.contains("alpha") && copied.contains("beta"),
                    expanded,
                    "{copied:?}"
                );
                assert!(copied.contains("gamma"), "{copied:?}");
                assert!(
                    !copied.contains('▎')
                        && !copied.contains("Thinking...")
                        && !copied.contains("to expand")
                );
                assert_eq!(
                    copied.contains("Last step"),
                    show_reasoning && expanded,
                    "{fold:?} {width}: {copied:?}"
                );

                let rendered = reload_layout.stats.blocks_rendered;
                reloaded.install_history_records(&records);
                assert_eq!(
                    prepare_layout(&mut reload_layout, &reloaded, width, fold),
                    finalized
                );
                assert_eq!(
                    reload_layout.stats.blocks_rendered, rendered,
                    "unchanged history reuses rows"
                );
            }
        }
    }

    #[test]
    fn layout_preserves_blank_lines_inside_tool_output_and_fenced_code() {
        let mut transcript = Transcript::from_history(&tool_history());
        transcript.push(assistant(3, "", "", false));
        transcript.push(assistant(
            4,
            "```text\ncode-before\n\n\ncode-after\n```",
            "",
            false,
        ));
        let rows = prepare_layout(
            &mut Layout::default(),
            &transcript,
            72,
            Fold {
                expanded: true,
                show_reasoning: true,
            },
        );
        for name in ["alpha", "code"] {
            let before = rows
                .iter()
                .position(|line| line.to_string().contains(&format!("{name}-before")))
                .unwrap();
            let after = rows
                .iter()
                .position(|line| line.to_string().contains(&format!("{name}-after")))
                .unwrap();
            assert_eq!(after - before, 3, "two content blank lines in {name}");
        }
    }

    #[test]
    fn a_stream_follows_its_request_when_the_blocks_around_it_move() {
        use crate::ui::theme::Palette;
        let live = |request_index, text: &str| Block::Assistant {
            key: RequestKey::response("00000000000000000001".parse().unwrap(), request_index),
            reasoning: String::new(),
            text: text.into(),
            streaming: true,
            partial: false,
            expanded: None,
        };
        let fold = Fold {
            expanded: false,
            show_reasoning: true,
        };
        let key = (60, fold, Styles::of(Palette::dark()));
        let first = live(0, "one\n\ntwo is long enough to leave a prefix\n\nthree");
        let second = live(1, "beta");
        let mut state = RenderState::default();
        state.prepare((key.0, key.2), &[first.clone(), second.clone()]);
        state.render(&first, key);
        state.render(&second, key);
        state.prepare((key.0, key.2), std::slice::from_ref(&second));
        let rendered = state.render(&second, key);
        let rows: Vec<String> = rendered.iter_from(0).map(Line::to_string).collect();
        let want: Vec<String> = blocks::render(&second, key.2, 60, fold)
            .iter()
            .map(Line::to_string)
            .collect();
        assert_eq!(rows, want);
    }

    #[test]
    fn folding_keeps_streamed_markdown_and_matches_a_full_render() {
        use std::sync::Arc;

        use crate::ui::theme::Palette;

        let request = RequestKey::response("00000000000000000001".parse().unwrap(), 0);
        let block = Block::Assistant {
            key: request.clone(),
            reasoning: "# Plan\n\nFirst step\nSecond step\nThird step\nFourth step".into(),
            text: "Stable **paragraph**.\n\nA changing tail.".into(),
            streaming: true,
            partial: false,
            expanded: None,
        };
        let styles = Styles::of(Palette::dark());
        let width = 60;
        let mut state = RenderState::default();
        state.prepare((width, styles), std::slice::from_ref(&block));
        state.render(&block, (width, Fold::default(), styles));
        let stream = state.streams.get_mut(&request).unwrap();
        let reply = stream.text.rows(&styles)[0].clone();
        let reasoning = stream.reasoning.rows(&styles)[0].clone();
        for (expanded, show_reasoning) in
            [(true, true), (false, true), (false, false), (true, false)]
        {
            let fold = Fold {
                expanded,
                show_reasoning,
            };
            state.prepare((width, styles), std::slice::from_ref(&block));
            let rendered = state.render(&block, (width, fold, styles));
            let expected = blocks::render(&block, styles, width, fold);
            assert_eq!(
                rendered.iter_from(0).collect::<Vec<_>>(),
                expected.iter().collect::<Vec<_>>(),
                "fold {fold:?}"
            );
            let stream = state.streams.get_mut(&request).unwrap();
            assert!(Arc::ptr_eq(&reply, &stream.text.rows(&styles)[0]));
            assert!(Arc::ptr_eq(&reasoning, &stream.reasoning.rows(&styles)[0]));
        }
    }

    #[test]
    fn a_selection_keeps_the_row_text_and_styles_only_its_cells() {
        let line = Line::from(vec![
            Span::raw("abcd"),
            Span::styled("efgh", Style::default().fg(Color::Red)),
        ]);
        let marked = highlight(&line, 2, 6);
        let parts: Vec<(&str, bool)> = marked
            .spans
            .iter()
            .map(|span| (span.content.as_ref(), reversed(span.style)))
            .collect();
        assert_eq!(text(&marked), "abcdefgh");
        assert_eq!(
            parts,
            vec![("ab", false), ("cd", true), ("ef", true), ("gh", false),],
            "only the dragged cells are reversed"
        );
        assert_eq!(
            marked.spans[2].style.fg,
            Some(Color::Red),
            "a selected span keeps its colours"
        );
        assert_eq!(marked.spans[3].style.fg, Some(Color::Red));
    }

    #[test]
    fn a_selection_over_a_whole_row_styles_it_without_splitting() {
        let line = Line::raw("row");
        let marked = highlight(&line, 0, 3);
        assert_eq!(marked.spans.len(), 1);
        assert_eq!(marked.spans[0].content, "row");
        assert!(reversed(marked.spans[0].style));
    }

    #[test]
    fn a_selection_never_splits_a_wide_character() {
        let line = Line::raw("日本");
        let marked = highlight(&line, 0, 2);
        assert_eq!(text(&marked), "日本");
        assert_eq!(marked.spans[0].content, "日");
        assert!(reversed(marked.spans[0].style));
        assert!(!reversed(marked.spans[1].style));
    }

    #[test]
    fn the_gutter_is_one_column_when_there_is_room() {
        assert_eq!(inner(Rect::new(0, 2, 80, 10)), Rect::new(1, 2, 79, 10));
        assert_eq!(inner(Rect::new(0, 0, 0, 10)).width, 0);
    }
}
