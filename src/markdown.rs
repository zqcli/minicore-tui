//! Lightweight Markdown rendering on top of `pulldown-cmark` (development
//! spec 20). Durable messages are parsed during update-owned cache
//! preparation into pre-wrapped, styled lines. Live answer text uses
//! `wrap_plain`; live reasoning parses its request-local buffer as Markdown.
//! Neither path invalidates or reparses the durable cache on a delta.
//! No other UI module depends on pulldown-cmark.
//!
//! `tui-markdown` was evaluated first: its bundled `Theme` cannot express
//! the spec palette exactly (card backgrounds, per-reasoning colors, code
//! border) and it renders as a self-laying-out widget, which does not fit
//! the single-writer transcript line slicing used by the scroll view. The
//! small wrapper below keeps all styling in one place.

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::theme::Theme;

#[cfg(test)]
use std::cell::Cell;

#[cfg(test)]
thread_local! {
    static MARKDOWN_PARSE_COUNT: Cell<usize> = const { Cell::new(0) };
}

/// One styled inline run.
#[derive(Clone)]
struct Seg {
    text: String,
    style: Style,
    /// True when this segment belongs to a markdown link (visible text or the
    /// surfaced URL). Used for real link geometry, not colors.
    link: bool,
}

/// A block-level markdown element.
enum Block {
    /// Lossless, width-wrapped source when list layout exceeds safe bounds.
    Plain(String),
    Paragraph(Vec<Seg>),
    Heading {
        level: u8,
        segs: Vec<Seg>,
    },
    Quote(Vec<Seg>),
    Code {
        text: String,
    },
    List {
        ordered: bool,
        start: u64,
        items: Vec<Vec<Block>>,
    },
    Rule,
}

#[derive(Clone, Copy)]
enum InlineAttr {
    Italic,
    Bold,
    Link,
}

const MAX_LIST_DEPTH: usize = 64;

/// Each open list owns its items; an item can contain paragraphs, code, or
/// another list. Closing a nested list must not close or renumber its parent.
struct ListBuilder {
    ordered: bool,
    start: u64,
    items: Vec<Vec<Block>>,
}

struct Builder<'a> {
    theme: &'a Theme,
    blocks: Vec<Block>,
    lists: Vec<ListBuilder>,
    quote_paras: Vec<Vec<Seg>>,
    quote: bool,
    heading: Option<HeadingLevel>,
    inline: Vec<Seg>,
    attrs: Vec<InlineAttr>,
    link: Option<(String, usize)>,
    code: Option<String>,
}

impl Builder<'_> {
    fn text(&mut self, text: &str) {
        if let Some(buf) = self.code.as_mut() {
            buf.push_str(text);
            return;
        }
        let mut style = Style::new();
        if self
            .attrs
            .iter()
            .any(|attr| matches!(attr, InlineAttr::Italic))
        {
            style = style.add_modifier(Modifier::ITALIC);
        }
        if self
            .attrs
            .iter()
            .any(|attr| matches!(attr, InlineAttr::Bold))
        {
            style = style.add_modifier(Modifier::BOLD);
        }
        if let Some(InlineAttr::Link) = self.attrs.last() {
            style = style.fg(self.theme.md_link);
        }
        self.push_seg(Seg {
            text: text.to_owned(),
            style,
            link: matches!(self.attrs.last(), Some(InlineAttr::Link)),
        });
    }

    /// Inline code arrives as a single text event (no start/end pair).
    fn text_code(&mut self, text: &str) {
        if let Some(buf) = self.code.as_mut() {
            buf.push_str(text);
            return;
        }
        self.push_seg(Seg {
            text: text.to_owned(),
            style: Style::new().fg(self.theme.md_code),
            link: matches!(self.attrs.last(), Some(InlineAttr::Link)),
        });
    }

    fn push_seg(&mut self, seg: Seg) {
        if seg.text.is_empty() {
            return;
        }
        self.inline.push(seg);
    }

    /// Ends the current inline run: into the open list item, a quote, or a
    /// paragraph block.
    fn flush(&mut self) {
        if self.inline.is_empty() {
            return;
        }
        let segs = std::mem::take(&mut self.inline);
        if self.lists.is_empty() && self.quote {
            self.quote_paras.push(segs);
        } else {
            self.push_block(Block::Paragraph(segs));
        }
    }

    fn push_block(&mut self, block: Block) {
        if let Some(item) = self.lists.last_mut().and_then(|list| list.items.last_mut()) {
            item.push(block);
        } else {
            self.blocks.push(block);
        }
    }

    fn list_begin(&mut self, ordered: bool, start: u64) {
        // Tight lists omit paragraph events, so flush the parent item's text
        // before entering a nested list rather than merging it with the child.
        self.flush();
        self.lists.push(ListBuilder {
            ordered,
            start,
            items: Vec::new(),
        });
    }

    fn item_begin(&mut self) {
        if let Some(list) = self.lists.last_mut() {
            list.items.push(Vec::new());
        }
    }

    fn list_layout_fits(&self, width: usize, content_width: usize) -> bool {
        if self.lists.len() > MAX_LIST_DEPTH {
            return false;
        }
        let prefix_width: usize = self
            .lists
            .iter()
            .map(|list| {
                if list.ordered {
                    let number = list.start + list.items.len().saturating_sub(1) as u64;
                    number.to_string().len() + 2
                } else {
                    2
                }
            })
            .sum();
        // Keep room for a wide character (plus borders for framed code).
        // Marker widths include the current item number, so a sibling
        // transition from 9 to 10 is checked too.
        prefix_width <= width.saturating_sub(content_width)
    }

    fn list_end(&mut self) {
        self.flush();
        if let Some(ListBuilder {
            ordered,
            start,
            items,
        }) = self.lists.pop()
        {
            self.push_block(Block::List {
                ordered,
                start,
                items,
            });
        }
    }

    fn quote_end(&mut self) {
        self.flush();
        self.quote = false;
        if self.quote_paras.is_empty() {
            return;
        }
        let mut segs: Vec<Seg> = Vec::new();
        for para in self.quote_paras.drain(..) {
            if !segs.is_empty() {
                segs.push(Seg {
                    text: " ".to_owned(),
                    style: Style::new(),
                    link: false,
                });
            }
            segs.extend(para);
        }
        self.push_block(Block::Quote(segs));
    }

    fn heading_end(&mut self) {
        let level = self.heading.take();
        let segs = std::mem::take(&mut self.inline);
        let level = match level {
            Some(level) => level as u8,
            None => 6,
        };
        if !segs.is_empty() {
            self.push_block(Block::Heading { level, segs });
        }
    }

    fn link_end(&mut self) {
        // Close the link attr, then surface a URL that differs from the
        // visible text as a dim parenthetical (spec 16.2 mdLinkUrl).
        self.attrs.pop();
        let Some((url, start)) = self.link.take() else {
            return;
        };
        let visible: String = self.inline[start..]
            .iter()
            .map(|seg| seg.text.as_str())
            .collect();
        if !url.is_empty() && url != visible && !url.contains(char::is_whitespace) {
            self.push_seg(Seg {
                text: format!(" ({url})"),
                style: Style::new().fg(self.theme.md_link_url),
                link: true,
            });
        }
    }

    fn finish(&mut self) {
        self.flush();
        if self.quote {
            self.quote_end();
        }
        if self.heading.is_some() {
            self.heading_end();
        }
        while !self.lists.is_empty() {
            self.list_end();
        }
    }
}

/// Renders durable Markdown messages and request-local live reasoning.
pub struct MarkdownRenderer<'a> {
    theme: &'a Theme,
    /// Thinking sections preserve raw single newlines as visual line breaks
    /// instead of the common-mark soft-break space (0.2.2 scoped to
    /// reasoning only; default Assistant/User rendering is unchanged).
    preserve_breaks: bool,
}

impl<'a> MarkdownRenderer<'a> {
    pub fn new(theme: &'a Theme) -> Self {
        Self {
            theme,
            preserve_breaks: false,
        }
    }

    /// A reasoning-only renderer where a single newline renders as a hard
    /// line break; blank paragraph separation is still markdown-normal.
    pub(crate) fn preserving_breaks(theme: &'a Theme) -> Self {
        Self {
            theme,
            preserve_breaks: true,
        }
    }

    /// Parses `text` into blocks.
    fn parse(&self, text: &str, width: usize) -> Vec<Block> {
        let text = crate::safe_text::safe_display(text);
        #[cfg(test)]
        MARKDOWN_PARSE_COUNT.with(|count| count.set(count.get() + 1));
        let options = Options::empty();
        let parser = Parser::new_ext(&text, options);
        let mut b = Builder {
            theme: self.theme,
            blocks: Vec::new(),
            lists: Vec::new(),
            quote_paras: Vec::new(),
            quote: false,
            heading: None,
            inline: Vec::new(),
            attrs: Vec::new(),
            link: None,
            code: None,
        };
        for event in parser {
            let list_content_width = match &event {
                Event::Start(Tag::List(_) | Tag::Item) => Some(2),
                Event::Start(Tag::CodeBlock(_)) if !b.lists.is_empty() => Some(4),
                _ => None,
            };
            match event {
                Event::Start(tag) => match tag {
                    Tag::Heading { level, .. } => b.heading = Some(level),
                    Tag::BlockQuote(..) => b.quote = true,
                    Tag::List(Some(start)) => b.list_begin(true, start),
                    Tag::List(None) => b.list_begin(false, 1),
                    Tag::Item => b.item_begin(),
                    Tag::CodeBlock(CodeBlockKind::Indented | CodeBlockKind::Fenced(_)) => {
                        b.flush();
                        b.code = Some(String::new())
                    }
                    Tag::Emphasis => b.attrs.push(InlineAttr::Italic),
                    Tag::Strong => b.attrs.push(InlineAttr::Bold),
                    Tag::Link { dest_url, .. } => {
                        b.attrs.push(InlineAttr::Link);
                        b.link = Some((dest_url.to_string(), b.inline.len()));
                    }
                    _ => {}
                },
                Event::End(tag) => match tag {
                    TagEnd::Paragraph => b.flush(),
                    TagEnd::Heading(_) => b.heading_end(),
                    TagEnd::BlockQuote(..) => b.quote_end(),
                    TagEnd::List(_) => b.list_end(),
                    TagEnd::Item => b.flush(),
                    TagEnd::CodeBlock => {
                        let text = b.code.take().unwrap_or_default();
                        b.push_block(Block::Code { text });
                    }
                    TagEnd::Emphasis | TagEnd::Strong => {
                        b.attrs.pop();
                    }
                    TagEnd::Link => b.link_end(),
                    _ => {}
                },
                Event::Text(text) => b.text(&text),
                Event::Code(text) => b.text_code(&text),
                Event::SoftBreak => b.push_seg(Seg {
                    text: if self.preserve_breaks {
                        "\n".to_owned()
                    } else {
                        " ".to_owned()
                    },
                    style: Style::new(),
                    link: matches!(b.attrs.last(), Some(InlineAttr::Link)),
                }),
                Event::HardBreak => b.push_seg(Seg {
                    text: "\n".to_owned(),
                    style: Style::new(),
                    link: matches!(b.attrs.last(), Some(InlineAttr::Link)),
                }),
                Event::Rule => {
                    b.flush();
                    b.push_block(Block::Rule);
                }
                _ => {}
            }
            if list_content_width.is_some_and(|content| !b.list_layout_fits(width, content)) {
                // pulldown-cmark does not bound list nesting. Avoid recursive
                // render/drop overflow and indentation wider than the view by
                // retaining the entire sanitized source as plain wrapped text.
                return vec![Block::Plain(text.to_string())];
            }
        }
        b.finish();
        b.blocks
    }

    /// Renders `text` into lines no wider than `width`. `style` is the base
    /// every span starts from (e.g. the user card adds its background here).
    pub fn render(&self, text: &str, width: usize, style: Style) -> Vec<Line<'static>> {
        let blocks = self.parse(text, width);
        let mut lines = Vec::new();
        let mut first = true;
        for block in &blocks {
            if !first {
                lines.push(Line::default());
            }
            first = false;
            self.block_lines(block, width, style, &mut lines);
        }
        lines
    }

    /// Renders markdown and returns, alongside each produced line, the cell
    /// ranges (content coordinates) that belong to a link. Derived from the
    /// same layout pass as the returned lines, so it can never disagree with
    /// what is drawn (RAIL-14 pressedUrl geometry, not a color heuristic).
    pub fn render_with_links(
        &self,
        text: &str,
        width: usize,
        style: Style,
    ) -> (Vec<Line<'static>>, Vec<Vec<std::ops::Range<usize>>>) {
        let (lines, link_cells, _) = self.render_with_breaks(text, width, style);
        (lines, link_cells)
    }

    /// Renders markdown and
    /// returns, alongside each produced line, the link cells and whether the
    /// line ends a *logical* source line (a paragraph/list item/code line
    /// boundary) rather than a soft wrap. Copy and export use the third output
    /// so a soft-wrapped row never gains a newline while real paragraph and
    /// code-line breaks survive.
    pub fn render_with_breaks(
        &self,
        text: &str,
        width: usize,
        style: Style,
    ) -> (
        Vec<Line<'static>>,
        Vec<Vec<std::ops::Range<usize>>>,
        Vec<bool>,
    ) {
        let blocks = self.parse(text, width);
        let mut lines = Vec::new();
        let mut link_cells = Vec::new();
        let mut hard_breaks = Vec::new();
        let mut first = true;
        for block in &blocks {
            if !first {
                // The blank row between two blocks is itself a visible line, so
                // the copy text keeps the paragraph gap (`a\n\nb`).
                lines.push(Line::default());
                link_cells.push(Vec::new());
                hard_breaks.push(true);
            }
            first = false;
            let start = lines.len();
            self.block_lines_with_links(
                block,
                width,
                style,
                &mut lines,
                &mut link_cells,
                &mut hard_breaks,
            );
            hard_breaks.resize(lines.len(), false);
            if lines.len() > start {
                // A markdown block always ends the visual line it closes, so
                // the next block starts on a new line.
                hard_breaks[lines.len() - 1] = true;
            }
        }
        (lines, link_cells, hard_breaks)
    }

    fn block_lines(&self, block: &Block, width: usize, base: Style, out: &mut Vec<Line<'static>>) {
        let mut breaks = Vec::new();
        self.block_lines_breaks(block, width, base, out, &mut breaks);
    }

    fn block_lines_breaks(
        &self,
        block: &Block,
        width: usize,
        base: Style,
        out: &mut Vec<Line<'static>>,
        hard_breaks: &mut Vec<bool>,
    ) {
        match block {
            Block::Plain(text) => {
                // Unlike Markdown soft breaks, every source newline (including
                // consecutive and trailing ones) survives the safe fallback.
                for raw in text.split('\n') {
                    let chunks = chunk_line(raw, width);
                    let last = chunks.len() - 1;
                    for (index, chunk) in chunks.into_iter().enumerate() {
                        out.push(Line::from(Span::styled(chunk, base)));
                        hard_breaks.push(index == last);
                    }
                }
            }
            Block::Paragraph(segs) => {
                let lines = wrap_segments_breaks(segs, width, base);
                for (line, hard) in lines {
                    out.push(line);
                    hard_breaks.push(hard);
                }
            }
            Block::Heading { level, segs } => {
                let mut style = base.fg(self.theme.md_heading);
                if *level <= 2 {
                    style = style.add_modifier(Modifier::BOLD);
                }
                for (line, hard) in wrap_segments_breaks(segs, width, style) {
                    out.push(line);
                    hard_breaks.push(hard);
                }
            }
            Block::Quote(segs) => {
                let inner = width.saturating_sub(2).max(1);
                let quote = base.fg(self.theme.md_quote);
                let wrapped = wrap_segments_breaks(segs, inner, quote);
                let marker = Span::styled("▍ ", Style::new().fg(self.theme.md_quote));
                let indent = Span::styled("  ", Style::new().fg(self.theme.md_quote));
                for (index, (line, hard)) in wrapped.into_iter().enumerate() {
                    let mut spans = vec![if index == 0 {
                        marker.clone()
                    } else {
                        indent.clone()
                    }];
                    spans.extend(line.spans);
                    out.push(Line::from(spans));
                    hard_breaks.push(hard);
                }
            }
            Block::Code { text } => self.code_lines(text, width, out, hard_breaks),
            Block::List {
                ordered,
                start,
                items,
            } => {
                let bullet_color = Style::new().fg(self.theme.md_list_bullet);
                for (index, item) in items.iter().enumerate() {
                    let marker = if *ordered {
                        format!("{}. ", start + index as u64)
                    } else {
                        "• ".to_owned()
                    };
                    let marker_w = UnicodeWidthStr::width(marker.as_str());
                    let inner = width.saturating_sub(marker_w).max(1);
                    let item_start = out.len();
                    for (block_index, block) in item.iter().enumerate() {
                        // Child lists follow their parent paragraph directly;
                        // distinct paragraphs/code blocks keep a visible gap.
                        if block_index > 0 && !matches!(block, Block::List { .. }) {
                            out.push(Line::default());
                            hard_breaks.push(true);
                        }
                        self.block_lines_breaks(block, inner, base, out, hard_breaks);
                        if let Some(hard) = hard_breaks.last_mut() {
                            *hard = true;
                        }
                    }
                    if out.len() == item_start {
                        out.push(Line::default());
                        hard_breaks.push(true);
                    }
                    let bullet = Span::styled(marker, bullet_color);
                    let indent = Span::styled(" ".repeat(marker_w), Style::new());
                    for (line_index, line) in out[item_start..].iter_mut().enumerate() {
                        if line_index == 0 {
                            line.spans.insert(0, bullet.clone());
                        } else if !line.spans.is_empty() {
                            line.spans.insert(0, indent.clone());
                        }
                    }
                }
            }
            Block::Rule => {
                out.push(Line::from(Span::styled(
                    "─".repeat(width),
                    Style::new().fg(self.theme.border),
                )));
                hard_breaks.push(false);
            }
        }
    }

    fn block_lines_with_links(
        &self,
        block: &Block,
        width: usize,
        base: Style,
        out: &mut Vec<Line<'static>>,
        link_cells: &mut Vec<Vec<std::ops::Range<usize>>>,
        hard_breaks: &mut Vec<bool>,
    ) {
        match block {
            Block::Paragraph(segs) => {
                wrap_segments_links(segs, width, base, out, link_cells, hard_breaks);
            }
            _ => {
                self.block_lines_breaks(block, width, base, out, hard_breaks);
                while link_cells.len() < out.len() {
                    link_cells.push(Vec::new());
                }
            }
        }
    }

    /// A single-color framed code block: border in `md_code_border`, content
    /// in `md_code_block`, indentation preserved, long lines soft-wrapped
    /// (spec 20.3). No syntax highlighting.
    fn code_lines(
        &self,
        text: &str,
        width: usize,
        out: &mut Vec<Line<'static>>,
        hard_breaks: &mut Vec<bool>,
    ) {
        let border = Style::new().fg(self.theme.md_code_border);
        let content = Style::new().fg(self.theme.md_code_block);
        let inner = width.saturating_sub(2).max(1);
        if width < 3 {
            for line in text.lines() {
                let chunks = chunk_line(line, inner);
                let chunks = if chunks.is_empty() {
                    vec![String::new()]
                } else {
                    chunks
                };
                let last = chunks.len() - 1;
                for (index, chunk) in chunks.into_iter().enumerate() {
                    out.push(Line::from(Span::styled(chunk, content)));
                    hard_breaks.push(index == last);
                }
            }
            return;
        }
        out.push(Line::from(vec![
            Span::styled("╭", border),
            Span::styled("─".repeat(inner), border),
            Span::styled("╮", border),
        ]));
        hard_breaks.push(false);
        for raw in text.lines() {
            let chunks = chunk_line(raw, inner);
            let chunks = if chunks.is_empty() {
                vec![String::new()]
            } else {
                chunks
            };
            let last = chunks.len() - 1;
            for (index, chunk) in chunks.into_iter().enumerate() {
                let pad = " ".repeat(inner.saturating_sub(UnicodeWidthStr::width(chunk.as_str())));
                out.push(Line::from(vec![
                    Span::styled("│", border),
                    Span::styled(format!("{chunk}{pad}"), content),
                    Span::styled("│", border),
                ]));
                hard_breaks.push(index == last);
            }
        }
        out.push(Line::from(vec![
            Span::styled("╰", border),
            Span::styled("─".repeat(inner), border),
            Span::styled("╯", border),
        ]));
        hard_breaks.push(false);
    }
}

/// Splits a (possibly indented) line into `width`-wide chunks, preserving
/// leading whitespace and replacing tabs.
fn chunk_line(line: &str, width: usize) -> Vec<String> {
    let line = line.replace('\t', "    ");
    let width = width.max(1);
    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_w = 0usize;
    for ch in line.chars() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
        if current_w + cw > width && !current.is_empty() {
            chunks.push(std::mem::take(&mut current));
            current_w = 0;
        }
        current.push(ch);
        current_w += cw;
    }
    if !current.is_empty() {
        chunks.push(current);
    } else if line.is_empty() {
        chunks.push(String::new());
    }
    chunks
}

/// Wraps styled segments to `width` cells. Each emitted row reports whether it
/// terminated a logical source line (a `\n` inside the run or the end of the
/// run) rather than a soft wrap, so copy/export can rebuild the original text.
fn wrap_segments_breaks(segs: &[Seg], width: usize, base: Style) -> Vec<(Line<'static>, bool)> {
    let width = width.max(1);
    let mut lines: Vec<(Line<'static>, bool)> = Vec::new();
    let mut current: Vec<Span<'static>> = Vec::new();
    let mut current_w = 0usize;
    for seg in segs {
        let style = base.patch(seg.style);
        for ch in seg.text.chars() {
            if ch == '\n' {
                if !current.is_empty() {
                    lines.push((Line::from(std::mem::take(&mut current)), true));
                    current_w = 0;
                }
                continue;
            }
            let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
            if current_w + cw > width && !current.is_empty() {
                lines.push((Line::from(std::mem::take(&mut current)), false));
                current_w = 0;
            }
            push_span_char(&mut current, ch, style);
            current_w += cw;
        }
    }
    if !current.is_empty() {
        lines.push((Line::from(current), true));
    }
    if lines.is_empty() {
        lines.push((Line::default(), true));
    }
    lines
}

/// Wraps styled segments like [`wrap_segments_breaks`] and records, per
/// emitted line, the content-cell
/// ranges covered by link segments and whether the line ended a logical
/// source line. All outputs come from one walk so they can never describe
/// different layouts.
fn wrap_segments_links(
    segs: &[Seg],
    width: usize,
    base: Style,
    lines: &mut Vec<Line<'static>>,
    link_cells: &mut Vec<Vec<std::ops::Range<usize>>>,
    hard_breaks: &mut Vec<bool>,
) {
    let width = width.max(1);
    #[allow(clippy::too_many_arguments)]
    fn flush(
        lines: &mut Vec<Line<'static>>,
        link_cells: &mut Vec<Vec<std::ops::Range<usize>>>,
        hard_breaks: &mut Vec<bool>,
        current: &mut Vec<Span<'static>>,
        current_links: &mut Vec<std::ops::Range<usize>>,
        current_w: &mut usize,
        hard: bool,
    ) {
        if !current.is_empty() {
            lines.push(Line::from(std::mem::take(current)));
            link_cells.push(std::mem::take(current_links));
            hard_breaks.push(hard);
            *current_w = 0;
        }
    }
    let mut current: Vec<Span<'static>> = Vec::new();
    let mut current_links: Vec<std::ops::Range<usize>> = Vec::new();
    let mut current_w = 0usize;
    for seg in segs {
        let style = base.patch(seg.style);
        for ch in seg.text.chars() {
            if ch == '\n' {
                flush(
                    lines,
                    link_cells,
                    hard_breaks,
                    &mut current,
                    &mut current_links,
                    &mut current_w,
                    true,
                );
                continue;
            }
            let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
            if current_w + cw > width && !current.is_empty() {
                flush(
                    lines,
                    link_cells,
                    hard_breaks,
                    &mut current,
                    &mut current_links,
                    &mut current_w,
                    false,
                );
            }
            if seg.link && cw > 0 {
                current_links.push(current_w..current_w + cw);
            }
            push_span_char(&mut current, ch, style);
            current_w += cw;
        }
    }
    flush(
        lines,
        link_cells,
        hard_breaks,
        &mut current,
        &mut current_links,
        &mut current_w,
        true,
    );
    if lines.is_empty() {
        lines.push(Line::default());
        link_cells.push(Vec::new());
        hard_breaks.push(true);
    }
}

/// Appends one character to the preceding span when its effective style is
/// unchanged. This keeps Unicode width decisions character-based while
/// emitting one allocation per contiguous styled run instead of one per char.
fn push_span_char(spans: &mut Vec<Span<'static>>, ch: char, style: Style) {
    if let Some(last) = spans.last_mut() {
        if last.style == style {
            last.content.to_mut().push(ch);
            return;
        }
    }
    spans.push(Span::styled(ch.to_string(), style));
}

/// Wraps plain text for streaming answers and the composer (spec 20.4)
/// with no markdown parsing. `\n` is a real line boundary: a newline ends
/// the current line, so consecutive newlines yield the same number of blank
/// lines and a leading/trailing newline yields a blank row — the rendered
/// line count is exactly `text.split('\n').count()`. Long lines are greedy-
/// chunked to `width` display cells. Appending a streamed delta after a
/// newline only fills the tail line, so the paragraph structure of a live
/// message matches the original text after every delta.
pub fn wrap_plain(text: &str, width: usize, style: Style) -> Vec<Line<'static>> {
    let text = crate::safe_text::safe_display(text);
    let width = width.max(1);
    let mut lines = Vec::new();
    for raw in text.split('\n') {
        if raw.is_empty() {
            lines.push(Line::default());
            continue;
        }
        for chunk in chunk_line(raw, width) {
            lines.push(Line::from(Span::styled(chunk, style)));
        }
    }
    if lines.is_empty() {
        lines.push(Line::default());
    }
    lines
}

/// The terminal column of `text` in display cells (spec 8.4): two for CJK,
/// zero for combining marks. Never `String::len`.
pub fn column_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

/// The display width contributed by one character.
pub fn char_width(ch: char) -> usize {
    UnicodeWidthChar::width(ch).unwrap_or(0)
}

/// The display width of one already-built line.
pub fn line_width(line: &Line) -> usize {
    line.spans
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum()
}

#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn reset_parse_count() {
    MARKDOWN_PARSE_COUNT.with(|count| count.set(0));
}

#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn parse_count() -> usize {
    MARKDOWN_PARSE_COUNT.with(Cell::get)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dark_theme() -> Theme {
        Theme::dark()
    }

    fn text_of(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn paragraphs_bold_italic_and_inline_code_are_styled() {
        let theme = dark_theme();
        let renderer = MarkdownRenderer::new(&theme);
        let lines = renderer.render("plain **bold** *italic* `code`", 80, Style::new());
        let joined: String = lines.iter().map(text_of).collect();
        assert!(joined.contains("bold"));
        assert!(joined.contains("italic"));
        assert!(joined.contains("code"));
        let bold: Vec<&Span> = lines[0]
            .spans
            .iter()
            .filter(|s| s.style.add_modifier.contains(Modifier::BOLD))
            .collect();
        assert!(!bold.is_empty());
        let italic: Vec<&Span> = lines[0]
            .spans
            .iter()
            .filter(|s| s.style.add_modifier.contains(Modifier::ITALIC))
            .collect();
        assert!(!italic.is_empty());
        let code = lines[0]
            .spans
            .iter()
            .find(|s| s.style.fg == Some(dark_theme().md_code));
        assert!(code.is_some());
    }

    #[test]
    fn headings_lists_quotes_and_rules_render() {
        let theme = dark_theme();
        let renderer = MarkdownRenderer::new(&theme);
        let text = "# Title\n\n- one\n- two\n\n> quoted\n\n---\n";
        let lines = renderer.render(text, 60, Style::new());
        let joined: String = lines.iter().map(|l| text_of(l)).collect();
        assert!(joined.contains("Title"));
        assert!(joined.contains("one"));
        assert!(joined.contains("two"));
        assert!(joined.contains("quoted"));
        assert!(joined.contains("─"));
        let heading = lines[0]
            .spans
            .iter()
            .all(|s| s.style.fg == Some(dark_theme().md_heading));
        assert!(heading);
        let bullet = lines.iter().find(|l| text_of(l).starts_with('•'));
        assert!(bullet.is_some());
    }

    #[test]
    fn nested_lists_keep_parent_items_and_markers_in_source_order() {
        let theme = dark_theme();
        let renderer = MarkdownRenderer::new(&theme);
        let text =
            "1. Parent one\n   - Child alpha\n   - Child beta\n2. Parent two\n\nNESTED-END\n";
        let lines = renderer.render(text, 60, Style::new());
        assert_eq!(
            lines.iter().map(text_of).collect::<Vec<_>>(),
            [
                "1. Parent one",
                "   • Child alpha",
                "   • Child beta",
                "2. Parent two",
                "",
                "NESTED-END",
            ]
        );
    }

    #[test]
    fn mixed_nested_lists_keep_each_lists_start_and_following_siblings() {
        let theme = dark_theme();
        let renderer = MarkdownRenderer::new(&theme);
        // A non-1 ordered start needs a blank line after paragraph text.
        let text = "- Parent\n\n  9. Nine\n     - Child\n  10. Ten\n- Next\n";
        let lines = renderer.render(text, 60, Style::new());
        assert_eq!(
            lines.iter().map(text_of).collect::<Vec<_>>(),
            [
                "• Parent",
                "  9. Nine",
                "     • Child",
                "  10. Ten",
                "• Next"
            ]
        );
    }

    #[test]
    fn nested_list_cjk_wraps_under_content_and_keeps_logical_breaks() {
        let theme = dark_theme();
        let renderer = MarkdownRenderer::new(&theme);
        let text = "1. 你好世界\n   - 甲乙丙丁戊己\n2. 后续\n";
        let (lines, links, breaks) = renderer.render_with_breaks(text, 9, Style::new());
        let rendered = lines.iter().map(text_of).collect::<Vec<_>>();
        assert_eq!(
            rendered,
            [
                "1. 你好世",
                "   界",
                "   • 甲乙",
                "     丙丁",
                "     戊己",
                "2. 后续"
            ]
        );
        assert!(lines.iter().all(|line| line_width(line) <= 9));
        assert_eq!(breaks, [false, true, false, false, true, true]);
        assert_eq!(links.len(), lines.len());
        assert_eq!(
            renderer
                .render(text, 9, Style::new())
                .iter()
                .map(text_of)
                .collect::<Vec<_>>(),
            rendered
        );
    }

    #[test]
    fn loose_list_paragraphs_and_code_stay_in_the_same_numbered_item() {
        let theme = dark_theme();
        let renderer = MarkdownRenderer::new(&theme);
        let text = "3. First\n\n   Second\n\n   ```text\n   code\n   ```\n\n   After code\n4. Next\n\nOutside\n";
        let (lines, _, breaks) = renderer.render_with_breaks(text, 12, Style::new());
        assert_eq!(
            lines.iter().map(text_of).collect::<Vec<_>>(),
            [
                "3. First",
                "",
                "   Second",
                "",
                "   ╭───────╮",
                "   │code   │",
                "   ╰───────╯",
                "",
                "   After cod",
                "   e",
                "4. Next",
                "",
                "Outside",
            ]
        );
        assert!(lines.iter().all(|line| line_width(line) <= 12));
        assert_eq!(breaks.len(), lines.len());
        assert!(breaks[6], "code block closes before the next paragraph");
        assert!(!breaks[8], "soft wrapping does not add a source newline");
        assert!(
            breaks[9],
            "the final item paragraph closes its logical line"
        );
    }

    #[test]
    fn empty_list_items_keep_their_marker_and_number() {
        let theme = dark_theme();
        let renderer = MarkdownRenderer::new(&theme);
        let lines = renderer.render("1.\n2. Next\n", 20, Style::new());
        assert_eq!(
            lines.iter().map(text_of).collect::<Vec<_>>(),
            ["1. ", "2. Next"]
        );
    }

    fn copy_text(lines: &[Line<'_>], breaks: &[bool]) -> String {
        let mut text = String::new();
        for (index, (line, hard)) in lines.iter().zip(breaks).enumerate() {
            text.push_str(&text_of(line));
            if *hard && index + 1 < lines.len() {
                text.push('\n');
            }
        }
        text
    }

    #[test]
    fn narrow_deep_lists_fall_back_without_losing_source_or_newlines() {
        let theme = dark_theme();
        let renderer = MarkdownRenderer::new(&theme);
        let mut text = String::from("\n");
        for depth in 0..10 {
            text.push_str(&format!("{}- 层{depth}\n", "  ".repeat(depth)));
        }
        text.push_str("\nNESTED-END\n");
        let (lines, links, breaks) = renderer.render_with_breaks(&text, 12, Style::new());
        assert!(matches!(
            renderer.parse(&text, 12).as_slice(),
            [Block::Plain(_)]
        ));
        assert!(lines.iter().all(|line| line_width(line) <= 12));
        assert_eq!(copy_text(&lines, &breaks), text);
        assert_eq!(links.len(), lines.len());
        assert_eq!(breaks.len(), lines.len());
        assert_eq!(
            renderer
                .render(&text, 12, Style::new())
                .iter()
                .map(text_of)
                .collect::<Vec<_>>(),
            lines.iter().map(text_of).collect::<Vec<_>>()
        );
    }

    #[test]
    fn excessive_list_depth_falls_back_even_with_ample_width() {
        let theme = dark_theme();
        let renderer = MarkdownRenderer::new(&theme);
        let text = format!("\n\n{}leaf\u{1b}\n\nTAIL\n", "- ".repeat(4096));
        let safe = crate::safe_text::safe_display(&text);
        let blocks = renderer.parse(&text, 10000);
        assert!(matches!(blocks.as_slice(), [Block::Plain(source)] if source == safe.as_ref()));
        let (lines, _, breaks) = renderer.render_with_breaks(&text, 10000, Style::new());
        assert_eq!(copy_text(&lines, &breaks), safe);
        assert!(lines.iter().all(|line| line_width(line) <= 10000));
        // The boundary itself is accepted, but no recursively owned list tree
        // beyond it is constructed, rendered, or dropped.
        let at_limit = format!("{}leaf", "- ".repeat(MAX_LIST_DEPTH));
        assert!(matches!(
            renderer.parse(&at_limit, 10000).as_slice(),
            [Block::List { .. }]
        ));
        assert_eq!(
            renderer
                .render(&at_limit, 10000, Style::new())
                .iter()
                .map(text_of)
                .collect::<String>(),
            format!("{}leaf", "• ".repeat(MAX_LIST_DEPTH))
        );
    }

    #[test]
    fn ordered_sibling_digit_growth_rechecks_the_list_width_budget() {
        let theme = dark_theme();
        let renderer = MarkdownRenderer::new(&theme);
        let text = "9. 甲\n10. 乙\n";
        // Three marker cells + two content cells fit, but four + two do not.
        assert!(matches!(
            renderer.parse("9. 甲", 5).as_slice(),
            [Block::List { .. }]
        ));
        let (lines, _, breaks) = renderer.render_with_breaks(text, 5, Style::new());
        assert!(matches!(
            renderer.parse(text, 5).as_slice(),
            [Block::Plain(_)]
        ));
        assert!(lines.iter().all(|line| line_width(line) <= 5));
        assert_eq!(copy_text(&lines, &breaks), text);
    }

    #[test]
    fn list_code_frame_reserves_room_for_a_wide_character() {
        let theme = dark_theme();
        let renderer = MarkdownRenderer::new(&theme);
        let text = "- ```\n  界\n  ```\n";
        let (lines, _, breaks) = renderer.render_with_breaks(text, 5, Style::new());
        assert!(
            lines.iter().all(|line| line_width(line) <= 5),
            "rows must stay within five cells: {:?}",
            lines.iter().map(text_of).collect::<Vec<_>>()
        );
        assert!(matches!(
            renderer.parse(text, 5).as_slice(),
            [Block::Plain(_)]
        ));
        assert_eq!(copy_text(&lines, &breaks), text);
        // At six cells the bullet, code border, and CJK character all fit.
        assert!(matches!(
            renderer.parse(text, 6).as_slice(),
            [Block::List { .. }]
        ));
        assert!(
            renderer
                .render(text, 6, Style::new())
                .iter()
                .all(|line| line_width(line) <= 6)
        );
    }

    #[test]
    fn code_block_is_framed_and_preserves_indent() {
        let theme = dark_theme();
        let renderer = MarkdownRenderer::new(&theme);
        let text = "```rust\n    fn main() {}\n```\n";
        let lines = renderer.render(text, 40, Style::new());
        let joined: String = lines.iter().map(text_of).collect();
        assert!(joined.contains("╭"));
        assert!(joined.contains("╰"));
        assert!(joined.contains("    fn main() {}"));
        let frame = lines
            .iter()
            .find(|l| l.spans.iter().any(|s| s.content.as_ref() == "╭"));
        assert!(frame.is_some());
        let code = lines
            .iter()
            .find(|l| text_of(l).contains("fn main"))
            .unwrap();
        assert!(
            code.spans
                .iter()
                .any(|s| s.style.fg == Some(dark_theme().md_code_block))
        );
    }

    #[test]
    fn link_surfaces_a_different_url_in_dim() {
        let theme = dark_theme();
        let renderer = MarkdownRenderer::new(&theme);
        let lines = renderer.render("[pi](https://example.com)", 60, Style::new());
        let joined = lines.iter().map(text_of).collect::<String>();
        assert!(joined.contains("https://example.com"));
        let url = lines[0]
            .spans
            .iter()
            .find(|s| s.style.fg == Some(dark_theme().md_link_url));
        assert!(url.is_some());
    }

    #[test]
    fn render_with_links_reports_real_link_cells_not_colors() {
        let theme = dark_theme();
        let renderer = MarkdownRenderer::new(&theme);
        // Visible text and the surfaced URL are both link cells.
        let (lines, links) = renderer.render_with_links(
            "see [docs](https://example.com/doc) please",
            60,
            Style::new(),
        );
        assert!(
            links[0].iter().any(|range| {
                lines[0]
                    .spans
                    .iter()
                    .any(|span| span.content.as_ref().contains("docs"))
                    && range.start < 60
            }),
            "visible link text must be a link cell"
        );
        let all_cells: Vec<usize> = links[0].iter().flat_map(|r| r.clone()).collect();
        assert!(all_cells.iter().any(|&cell| {
            // cells at the mdLinkUrl parenthetical are also links
            lines[0]
                .spans
                .iter()
                .scan(0usize, |cursor, span| {
                    let start = *cursor;
                    *cursor += unicode_width::UnicodeWidthStr::width(span.content.as_ref());
                    Some((start, span))
                })
                .any(|(start, span)| {
                    span.style.fg == Some(theme.md_link_url)
                        && start <= cell
                        && cell
                            < start + unicode_width::UnicodeWidthStr::width(span.content.as_ref())
                })
        }));
        // Inline code inside a link is a link even though its fg is md_code,
        // and plain inline code outside a link is not.
        let (_, in_link) = renderer.render_with_links("[`x`](https://e.com)", 60, Style::new());
        assert!(
            in_link.iter().any(|row| !row.is_empty()),
            "code in a link is a link"
        );
        let (_, plain) = renderer.render_with_links("run `cargo test` now", 60, Style::new());
        assert!(
            plain.iter().all(|row| row.is_empty()),
            "bare inline code is not a link"
        );
    }

    #[test]
    fn soft_breaks_become_spaces_and_paragraphs_gap() {
        let theme = dark_theme();
        let renderer = MarkdownRenderer::new(&theme);
        let lines = renderer.render("one\ntwo\n\nthree", 60, Style::new());
        assert_eq!(lines.len(), 3, "blank line separates the two paragraphs");
        assert_eq!(text_of(&lines[0]), "one two");
        assert_eq!(text_of(&lines[2]), "three");
    }

    #[test]
    fn adjacent_same_style_text_is_one_span_and_style_boundaries_remain() {
        let theme = dark_theme();
        let renderer = MarkdownRenderer::new(&theme);
        let lines = renderer.render("plain **bold** plain", 80, Style::new());
        assert_eq!(lines.len(), 1);
        let contents: Vec<&str> = lines[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(contents, vec!["plain ", "bold", " plain"]);
        assert!(
            lines[0].spans[1]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
        assert!(
            !lines[0].spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );

        let cjk = renderer.render("你你a\u{0301}a", 80, Style::new());
        assert_eq!(cjk[0].spans.len(), 1, "combining marks stay in one run");
        assert_eq!(cjk[0].spans[0].content.as_ref(), "你你a\u{0301}a");
        assert_eq!(line_width(&cjk[0]), 6);
    }

    #[test]
    fn column_width_counts_cjk_as_two() {
        assert_eq!(column_width("abc"), 3);
        assert_eq!(column_width("你abc"), 5);
        assert_eq!(column_width("😀"), 2);
        assert_eq!(
            column_width("a\u{0301}b"),
            2,
            "combining mark adds no column"
        );
    }

    #[test]
    fn wrap_plain_never_exceeds_the_width_and_splits_at_overflow() {
        let lines = wrap_plain("hello world", 6, Style::new());
        let joined: Vec<String> = lines.iter().map(text_of).collect();
        let all: String = joined.join("");
        assert_eq!(all.replace(' ', ""), "helloworld");
        for line in &lines {
            assert!(
                line_width(line) <= 6,
                "line overflowed: {:?}",
                text_of(line)
            );
        }
    }

    #[test]
    fn wrap_plain_keeps_empty_lines_and_single_newlines() {
        let style = Style::new();
        let text =
            |t: &str| -> Vec<String> { wrap_plain(t, 60, style).iter().map(text_of).collect() };
        assert_eq!(text("a\nb"), vec!["a", "b"], "one newline = line break");
        assert_eq!(
            text("a\n\nb"),
            vec!["a", "", "b"],
            "a blank line survives between paragraphs"
        );
        assert_eq!(
            text("\n\na"),
            vec!["", "", "a"],
            "leading newlines keep their blank rows"
        );
        assert_eq!(
            text("a\n\n\n"),
            vec!["a", "", "", ""],
            "trailing newlines keep their blank rows"
        );
        assert_eq!(
            text("a\n"),
            vec!["a", ""],
            "a single trailing newline ends with one blank row"
        );
        assert_eq!(text(""), vec![""], "empty text is one empty line");
    }

    #[test]
    fn wrap_plain_cjk_emoji_blank_lines_lose_no_chars_or_width() {
        let style = Style::new();
        // Narrow width forces wrapping on every paragraph; a `\n\n` sits in
        // the middle so the blank line must survive the chunking.
        let probe = "你的名字abc\n\n😀emoji测试";
        let width = 6;
        let lines = wrap_plain(probe, width, style)
            .iter()
            .map(text_of)
            .collect::<Vec<_>>();
        let joined: String = lines.clone().join("");
        assert_eq!(joined, probe.replace('\n', ""), "no character is lost");
        assert_eq!(
            lines[2], "",
            "the paragraph gap stays a blank row after wrapping"
        );
        let total_width: usize = lines.iter().map(|l| column_width(l)).sum();
        assert_eq!(
            total_width,
            column_width(&joined),
            "no display width is lost or invented"
        );
        for line in &lines {
            assert!(column_width(line) <= width, "line overflowed: {:?}", line);
        }
        // A leading newline plus a long CJK row: structure preserved and
        // still width-bounded.
        let lines = wrap_plain("\n你😀你我", 4, style)
            .iter()
            .map(text_of)
            .collect::<Vec<_>>();
        assert_eq!(lines.join(""), "你😀你我");
        assert_eq!(lines[0], "");
        assert!(lines.iter().all(|l| column_width(l) <= 4));
    }
}
