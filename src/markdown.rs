//! Lightweight Markdown rendering on top of `pulldown-cmark` (development
//! spec 20). Durable messages are parsed during update-owned cache
//! preparation into pre-wrapped, styled lines. Live answer text and reasoning
//! parse their request-local buffers through the same Markdown renderer.
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
use std::sync::Arc;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::theme::Theme;

#[cfg(test)]
use std::cell::Cell;

#[cfg(test)]
thread_local! {
    static MARKDOWN_PARSE_COUNT: Cell<usize> = const { Cell::new(0) };
}

/// A visible chunk of one parsed table cell. Only cell content is retained;
/// borders, alignment padding, and repeated narrow-layout labels are absent.
/// Raw source offsets identify the logical row/cell across wraps and deltas.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TableCopyFragment {
    /// Actual glyph columns, or the empty cell's layout slot when `text` is
    /// empty. Selecting that slot contributes no glyphs, only its column place.
    pub columns: std::ops::Range<usize>,
    pub text: String,
    pub row_offset: usize,
    pub cell_offset: usize,
    pub cell_index: usize,
    /// Byte position in the cell's visible text, independent of display rows.
    pub chunk_offset: usize,
    /// The original pipe between this cell and its parsed predecessor, if any.
    pub separator_before: Option<usize>,
}

/// Renderer-owned copy geometry for a decorated row. Bounds are display cells,
/// not bytes; `decorative` rows have no source content (for example a code frame).
#[derive(Clone, Debug)]
pub struct CopyCells {
    pub columns: std::ops::Range<usize>,
    pub decorative: bool,
    /// Raw Markdown byte position, independent of visual wrapping and escaping.
    pub source_offset: Option<usize>,
    /// Ordinary Markdown rows retain their existing list-marker copy behavior;
    /// code/table frames keep explicit content bounds when a list prefixes them.
    list_prefix_copyable: bool,
    /// Table-only discontinuous content geometry; shared with prepared rows.
    pub table_fragments: Option<Arc<[TableCopyFragment]>>,
}

impl CopyCells {
    /// Explicit source-content bounds for framed Markdown or tool rows.
    pub(crate) fn content(columns: std::ops::Range<usize>, source_offset: Option<usize>) -> Self {
        Self {
            columns,
            decorative: false,
            source_offset,
            ..Self::decoration()
        }
    }

    pub fn shifted(mut self, offset: usize) -> Self {
        self.shift_columns(offset);
        self
    }

    fn shift_columns(&mut self, offset: usize) {
        self.columns = self.columns.start + offset..self.columns.end + offset;
        if let Some(fragments) = self.table_fragments.as_mut() {
            for fragment in Arc::make_mut(fragments) {
                fragment.columns = fragment.columns.start + offset..fragment.columns.end + offset;
            }
        }
    }

    fn table(
        columns: std::ops::Range<usize>,
        fragments: Vec<TableCopyFragment>,
        source_offset: Option<usize>,
    ) -> Self {
        if fragments.is_empty() {
            return Self::decoration();
        }
        Self {
            columns,
            decorative: false,
            source_offset,
            list_prefix_copyable: false,
            table_fragments: Some(fragments.into()),
        }
    }

    pub fn decoration() -> Self {
        Self {
            columns: 0..0,
            decorative: true,
            source_offset: None,
            list_prefix_copyable: false,
            table_fragments: None,
        }
    }
}

/// Text and interaction facts from one Markdown rendering pass.
#[derive(Default)]
pub struct RenderedMarkdown {
    pub lines: Vec<Line<'static>>,
    pub link_cells: Vec<Vec<std::ops::Range<usize>>>,
    pub hard_breaks: Vec<bool>,
    /// `None` means the whole ordinary content row; explicit bounds remove
    /// renderer-owned framing without guessing from text or colors.
    pub copy_cells: Vec<Option<CopyCells>>,
}

/// Sparse positions where displayed bytes diverge from raw Markdown bytes.
/// Ordinary text needs only a start offset; parser transformations and stripped
/// code indentation add boundaries, never a full per-character source map.
#[derive(Clone, Default)]
struct SourceSpan {
    start: usize,
    adjustments: Vec<(usize, usize)>,
}

impl SourceSpan {
    fn at(&self, display: usize) -> usize {
        let index = self
            .adjustments
            .partition_point(|(offset, _)| *offset <= display);
        match index.checked_sub(1).map(|index| self.adjustments[index]) {
            Some((offset, raw)) => raw + display - offset,
            None => self.start + display,
        }
    }

    fn literal(raw: &str, start: usize) -> Self {
        let mut span = Self {
            start,
            adjustments: Vec::new(),
        };
        let mut expansion = 0;
        for (offset, ch) in raw.char_indices() {
            if crate::safe_text::is_unsafe_display_control(ch) {
                let escaped = crate::safe_text::safe_display(&ch.to_string()).into_owned();
                let display = offset + expansion;
                // A wrap inside a multi-character escape still refers to the
                // original control character, never an interior UTF-8 byte.
                span.adjustments.extend(
                    escaped
                        .char_indices()
                        .map(|(index, _)| (display + index, start + offset)),
                );
                expansion += escaped.len() - ch.len_utf8();
                span.adjustments.push((
                    offset + ch.len_utf8() + expansion,
                    start + offset + ch.len_utf8(),
                ));
            }
        }
        span
    }

    fn parsed(text: &str, raw: &str, start: usize) -> Self {
        if text == raw {
            return Self {
                start,
                adjustments: Vec::new(),
            };
        }
        let safe = crate::safe_text::safe_display(raw);
        let mut offset = safe.find(text);
        if offset.is_none() {
            // Code spans normalize embedded newlines to spaces and may trim
            // one surrounding space. These substitutions preserve byte widths;
            // a long multiline span must not give every wrap the opening tick.
            let ticks = safe.bytes().take_while(|byte| *byte == b'`').count();
            if ticks > 0 && safe.len() >= ticks * 2 && safe.ends_with(&safe[..ticks]) {
                let body = safe[ticks..safe.len() - ticks].replace('\n', " ");
                let trim = usize::from(
                    body.starts_with(' ')
                        && body.ends_with(' ')
                        && body.chars().any(|ch| ch != ' '),
                );
                if body.get(trim..body.len().saturating_sub(trim)) == Some(text) {
                    offset = Some(ticks + trim);
                }
            }
        }
        if let Some(offset) = offset {
            let literal = Self::literal(raw, start);
            return Self {
                start: literal.at(offset),
                adjustments: literal
                    .adjustments
                    .into_iter()
                    .filter(|(display, _)| *display >= offset && *display <= offset + text.len())
                    .map(|(display, raw)| (display - offset, raw))
                    .collect(),
            };
        }
        // Entity/backslash decoding and a parser-generated partial tab can
        // produce a glyph with no one-to-one source byte. Keep its raw span.
        let mut adjustments: Vec<_> = text
            .char_indices()
            .map(|(offset, _)| (offset, start))
            .collect();
        adjustments.push((text.len(), start + raw.len()));
        Self { start, adjustments }
    }

    fn append(&mut self, display: usize, other: Self) {
        if display == 0 {
            self.start = other.start;
        } else if self.at(display) != other.start {
            self.adjustments.push((display, other.start));
        }
        self.adjustments.extend(
            other
                .adjustments
                .into_iter()
                .map(|(offset, raw)| (display + offset, raw)),
        );
    }
}

/// One styled inline run.
#[derive(Clone)]
struct Seg {
    text: String,
    style: Style,
    /// True when this segment belongs to a markdown link (visible text or the
    /// surfaced URL). Used for real link geometry, not colors.
    link: bool,
    source: Option<SourceSpan>,
}

/// A block-level markdown element.
enum Block {
    /// Lossless, width-wrapped source when list layout exceeds safe bounds.
    Plain {
        text: String,
        source: SourceSpan,
    },
    Paragraph(Vec<Seg>),
    Heading {
        level: u8,
        segs: Vec<Seg>,
    },
    Quote(Vec<Seg>),
    Code {
        text: String,
        source: SourceSpan,
    },
    Table(Vec<TableRow>),
    List {
        ordered: bool,
        start: u64,
        items: Vec<Vec<Block>>,
    },
    Rule,
}

struct TableRow {
    source_offset: usize,
    cells: Vec<TableCell>,
}

struct TableCell {
    source_range: std::ops::Range<usize>,
    separator_before: Option<usize>,
    segs: Vec<Seg>,
}

#[derive(Clone, Copy)]
enum InlineAttr {
    Italic,
    Bold,
    Strike,
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
    link: Option<(String, usize, SourceSpan)>,
    code: Option<String>,
    code_source: SourceSpan,
    table: Option<Vec<TableRow>>,
}

impl Builder<'_> {
    fn text(&mut self, text: &str, source: SourceSpan) {
        if let Some(buf) = self.code.as_mut() {
            self.code_source.append(buf.len(), source);
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
        if self
            .attrs
            .iter()
            .any(|attr| matches!(attr, InlineAttr::Strike))
        {
            style = style.add_modifier(Modifier::CROSSED_OUT);
        }
        if let Some(InlineAttr::Link) = self.attrs.last() {
            style = style.fg(self.theme.md_link);
        }
        self.push_seg(Seg {
            text: text.to_owned(),
            style,
            link: matches!(self.attrs.last(), Some(InlineAttr::Link)),
            source: Some(source),
        });
    }

    /// Inline code arrives as a single text event (no start/end pair).
    fn text_code(&mut self, text: &str, source: SourceSpan) {
        if let Some(buf) = self.code.as_mut() {
            self.code_source.append(buf.len(), source);
            buf.push_str(text);
            return;
        }
        self.push_seg(Seg {
            text: text.to_owned(),
            style: Style::new().fg(self.theme.md_code),
            link: matches!(self.attrs.last(), Some(InlineAttr::Link)),
            source: Some(source),
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
                    source: None,
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
        let Some((url, start, url_source)) = self.link.take() else {
            return;
        };
        let visible: String = self.inline[start..]
            .iter()
            .map(|seg| seg.text.as_str())
            .collect();
        if !url.is_empty() && url != visible && !url.contains(char::is_whitespace) {
            let mut source = SourceSpan::parsed(" (", "", url_source.start);
            let end = url_source.at(url.len());
            source.append(2, url_source);
            source.append(2 + url.len(), SourceSpan::parsed(")", "", end));
            self.push_seg(Seg {
                text: format!(" ({url})"),
                style: Style::new().fg(self.theme.md_link_url),
                link: true,
                source: Some(source),
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

/// Parser positions are in sanitized display bytes. Table identities must use
/// raw source positions so escaping earlier text cannot shift a stable cell.
fn raw_table_offset(display_offset: usize, offsets: Option<&[(usize, usize)]>) -> usize {
    offsets.map_or(display_offset, |offsets| {
        let index = offsets.partition_point(|(display, _)| *display <= display_offset);
        let (display, raw) = offsets[index.saturating_sub(1)];
        raw + display_offset - display
    })
}

/// Sanitizing never removes newlines, so literal rows can retain their raw
/// source-line offsets without a second parser or per-character source map.
fn literal_source(display: &str, source: &str) -> Block {
    Block::Plain {
        text: display.to_owned(),
        source: SourceSpan::literal(source, 0),
    }
}

/// Renders durable Markdown messages and request-local live text/reasoning.
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
        let source = text;
        let text = crate::safe_text::safe_display(source);
        // Parsing sanitized text preserves the existing safe-display semantics.
        // Map parser offsets back to raw bytes only when escaping changed them.
        let source_offsets = if matches!(text, std::borrow::Cow::Owned(_)) {
            let mut offsets = vec![(0, 0)];
            let mut expansion = 0;
            for (raw, ch) in source.char_indices() {
                if crate::safe_text::is_unsafe_display_control(ch) {
                    expansion +=
                        crate::safe_text::safe_display(&ch.to_string()).len() - ch.len_utf8();
                    let raw_end = raw + ch.len_utf8();
                    offsets.push((raw_end + expansion, raw_end));
                }
            }
            Some(offsets)
        } else {
            None
        };
        #[cfg(test)]
        MARKDOWN_PARSE_COUNT.with(|count| count.set(count.get() + 1));
        let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
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
            code_source: SourceSpan::default(),
            table: None,
        };
        for (event, range) in parser.into_offset_iter() {
            let raw_offset = |position| {
                source_offsets.as_ref().map_or(position, |offsets| {
                    let index = offsets.partition_point(|(display, _)| *display <= position);
                    let (display, raw) = offsets[index.saturating_sub(1)];
                    raw + position - display
                })
            };
            let event_source = |display: &str| {
                let start = raw_offset(range.start);
                let end = raw_offset(range.end);
                SourceSpan::parsed(display, &source[start..end], start)
            };
            let list_content_width = match &event {
                Event::Start(Tag::List(_) | Tag::Item) => Some(2),
                Event::Start(Tag::CodeBlock(_)) if !b.lists.is_empty() => Some(4),
                _ => None,
            };
            match event {
                Event::Start(tag) => match tag {
                    Tag::Table(_) => {
                        // Quotes currently collect paragraph runs separately.
                        // Keep compound quoted tables literal rather than
                        // emitting the table before its surrounding prose.
                        if b.quote {
                            return vec![literal_source(&text, source)];
                        }
                        b.flush();
                        b.table = Some(Vec::new());
                    }
                    Tag::TableHead | Tag::TableRow => {
                        if let Some(rows) = b.table.as_mut() {
                            rows.push(TableRow {
                                source_offset: raw_table_offset(
                                    range.start,
                                    source_offsets.as_deref(),
                                ),
                                cells: Vec::new(),
                            });
                        }
                    }
                    Tag::TableCell => {
                        if let Some(row) = b.table.as_mut().and_then(|rows| rows.last_mut()) {
                            let source_range =
                                raw_table_offset(range.start, source_offsets.as_deref())
                                    ..raw_table_offset(range.end, source_offsets.as_deref());
                            // The parser gives cell boundaries excluding separators.
                            // Inspect only the gap between neighboring parsed cells,
                            // never a displayed border or a literal pipe inside a cell.
                            let separator_before = row.cells.last().and_then(|previous| {
                                source
                                    .get(previous.source_range.end..source_range.start)
                                    .and_then(|gap| gap.find('|'))
                                    .map(|offset| previous.source_range.end + offset)
                            });
                            row.cells.push(TableCell {
                                source_range,
                                separator_before,
                                segs: Vec::new(),
                            });
                        }
                    }
                    Tag::Heading { level, .. } => b.heading = Some(level),
                    Tag::BlockQuote(..) => b.quote = true,
                    Tag::List(Some(start)) => b.list_begin(true, start),
                    Tag::List(None) => b.list_begin(false, 1),
                    Tag::Item => b.item_begin(),
                    Tag::CodeBlock(CodeBlockKind::Indented | CodeBlockKind::Fenced(_)) => {
                        b.flush();
                        b.code_source = SourceSpan::default();
                        b.code = Some(String::new())
                    }
                    Tag::Emphasis => b.attrs.push(InlineAttr::Italic),
                    Tag::Strong => b.attrs.push(InlineAttr::Bold),
                    Tag::Strikethrough => b.attrs.push(InlineAttr::Strike),
                    Tag::Link { dest_url, .. } => {
                        b.attrs.push(InlineAttr::Link);
                        b.link = Some((
                            dest_url.to_string(),
                            b.inline.len(),
                            event_source(&dest_url),
                        ));
                    }
                    _ => {}
                },
                Event::End(tag) => match tag {
                    TagEnd::TableCell => {
                        let segs = std::mem::take(&mut b.inline);
                        if let Some(cell) = b
                            .table
                            .as_mut()
                            .and_then(|rows| rows.last_mut())
                            .and_then(|row| row.cells.last_mut())
                        {
                            cell.segs = segs;
                        }
                    }
                    TagEnd::Table => {
                        if let Some(rows) = b.table.take() {
                            b.push_block(Block::Table(rows));
                        }
                    }
                    TagEnd::Paragraph => b.flush(),
                    TagEnd::Heading(_) => b.heading_end(),
                    TagEnd::BlockQuote(..) => b.quote_end(),
                    TagEnd::List(_) => b.list_end(),
                    TagEnd::Item => b.flush(),
                    TagEnd::CodeBlock => {
                        let text = b.code.take().unwrap_or_default();
                        let source = std::mem::take(&mut b.code_source);
                        b.push_block(Block::Code { text, source });
                    }
                    TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => {
                        b.attrs.pop();
                    }
                    TagEnd::Link => b.link_end(),
                    _ => {}
                },
                // Mixed HTML/XML can cross Markdown block boundaries. Keep
                // the complete source literal so parser-inserted paragraph
                // gaps or ignored wrappers cannot alter the submitted text.
                Event::Html(_) | Event::InlineHtml(_) => {
                    return vec![literal_source(&text, source)];
                }
                Event::Text(text) => b.text(&text, event_source(&text)),
                Event::Code(text) => b.text_code(&text, event_source(&text)),
                Event::SoftBreak => b.push_seg(Seg {
                    text: if self.preserve_breaks {
                        "\n".to_owned()
                    } else {
                        " ".to_owned()
                    },
                    style: Style::new(),
                    link: matches!(b.attrs.last(), Some(InlineAttr::Link)),
                    source: Some(event_source(if self.preserve_breaks { "\n" } else { " " })),
                }),
                Event::HardBreak => b.push_seg(Seg {
                    text: "\n".to_owned(),
                    style: Style::new(),
                    link: matches!(b.attrs.last(), Some(InlineAttr::Link)),
                    source: Some(event_source("\n")),
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
                return vec![literal_source(&text, source)];
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
        let rendered = self.render_with_metadata(text, width, style);
        (rendered.lines, rendered.link_cells, rendered.hard_breaks)
    }

    pub fn render_with_metadata(&self, text: &str, width: usize, style: Style) -> RenderedMarkdown {
        let blocks = self.parse(text, width);
        let mut lines = Vec::new();
        let mut link_cells = Vec::new();
        let mut hard_breaks = Vec::new();
        let mut copy_cells = Vec::new();
        let mut first = true;
        for block in &blocks {
            if !first {
                // The blank row between two blocks is itself a visible line, so
                // the copy text keeps the paragraph gap (`a\n\nb`).
                lines.push(Line::default());
                link_cells.push(Vec::new());
                hard_breaks.push(true);
                copy_cells.push(None);
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
                &mut copy_cells,
            );
            hard_breaks.resize(lines.len(), false);
            if lines.len() > start {
                // A markdown block always ends the visual line it closes, so
                // the next block starts on a new line.
                hard_breaks[lines.len() - 1] = true;
            }
        }
        // Record content bounds before cards add background padding. Soft-wrap
        // trailing spaces belong to the source and must not be trimmed by copy.
        for (line, copy) in lines.iter().zip(copy_cells.iter_mut()) {
            if copy.is_none() {
                *copy = Some(CopyCells {
                    columns: 0..line_width(line),
                    decorative: false,
                    source_offset: None,
                    list_prefix_copyable: true,
                    table_fragments: None,
                });
            }
        }
        RenderedMarkdown {
            lines,
            link_cells,
            hard_breaks,
            copy_cells,
        }
    }

    fn block_lines(&self, block: &Block, width: usize, base: Style, out: &mut Vec<Line<'static>>) {
        let mut breaks = Vec::new();
        self.block_lines_breaks(block, width, base, out, &mut breaks, &mut Vec::new());
    }

    fn block_lines_breaks(
        &self,
        block: &Block,
        width: usize,
        base: Style,
        out: &mut Vec<Line<'static>>,
        hard_breaks: &mut Vec<bool>,
        copy_cells: &mut Vec<Option<CopyCells>>,
    ) {
        copy_cells.resize(out.len(), None);
        match block {
            Block::Plain { text, source } => {
                let mut line_offset = 0;
                for raw in text.split('\n') {
                    let chunks = chunk_line_offsets(raw, width);
                    let last = chunks.len().saturating_sub(1);
                    for (index, (chunk, offset)) in chunks.into_iter().enumerate() {
                        copy_cells.push(Some(CopyCells::content(
                            0..UnicodeWidthStr::width(chunk.as_str()),
                            Some(source.at(line_offset + offset)),
                        )));
                        out.push(Line::from(Span::styled(chunk, base)));
                        hard_breaks.push(index == last);
                    }
                    line_offset += raw.len() + 1;
                }
            }
            Block::Paragraph(segs) | Block::Heading { segs, .. } => {
                let style = match block {
                    Block::Heading { level, .. } => {
                        let heading = base.fg(self.theme.md_heading);
                        if *level <= 2 {
                            heading.add_modifier(Modifier::BOLD)
                        } else {
                            heading
                        }
                    }
                    _ => base,
                };
                for (line, hard, source_offset) in wrap_segments_metadata(segs, width, style) {
                    copy_cells.push(Some(CopyCells {
                        columns: 0..line_width(&line),
                        decorative: false,
                        source_offset,
                        list_prefix_copyable: true,
                        table_fragments: None,
                    }));
                    out.push(line);
                    hard_breaks.push(hard);
                }
            }
            Block::Quote(segs) => {
                let inner = width.saturating_sub(2).max(1);
                let quote = base.fg(self.theme.md_quote);
                let wrapped = wrap_segments_metadata(segs, inner, quote);
                let marker = Span::styled("▍ ", Style::new().fg(self.theme.md_quote));
                let indent = Span::styled("  ", Style::new().fg(self.theme.md_quote));
                for (index, (line, hard, source_offset)) in wrapped.into_iter().enumerate() {
                    copy_cells.push(Some(CopyCells {
                        columns: 0..2 + line_width(&line),
                        decorative: false,
                        source_offset,
                        list_prefix_copyable: true,
                        table_fragments: None,
                    }));
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
            Block::Code { text, source } => {
                self.code_lines(text, source, width, out, hard_breaks, copy_cells);
            }
            Block::Table(rows) => self.table_lines(rows, width, base, out, hard_breaks, copy_cells),
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
                        self.block_lines_breaks(block, inner, base, out, hard_breaks, copy_cells);
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
                        if let Some(Some(copy)) = copy_cells.get_mut(item_start + line_index) {
                            if copy.list_prefix_copyable {
                                copy.columns.end += marker_w;
                            } else {
                                copy.shift_columns(marker_w);
                            }
                        }
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
        copy_cells.resize(out.len(), None);
    }

    #[allow(clippy::too_many_arguments)]
    fn block_lines_with_links(
        &self,
        block: &Block,
        width: usize,
        base: Style,
        out: &mut Vec<Line<'static>>,
        link_cells: &mut Vec<Vec<std::ops::Range<usize>>>,
        hard_breaks: &mut Vec<bool>,
        copy_cells: &mut Vec<Option<CopyCells>>,
    ) {
        copy_cells.resize(out.len(), None);
        match block {
            Block::Paragraph(segs) => {
                wrap_segments_links(segs, width, base, out, link_cells, hard_breaks, copy_cells);
            }
            _ => {
                self.block_lines_breaks(block, width, base, out, hard_breaks, copy_cells);
                while link_cells.len() < out.len() {
                    link_cells.push(Vec::new());
                }
            }
        }
        copy_cells.resize(out.len(), None);
    }

    /// Tables use bounded columns when they fit, and one labeled cell at a
    /// time when the viewport is too narrow. No cell is clipped or discarded.
    #[allow(clippy::too_many_arguments)]
    fn table_lines(
        &self,
        rows: &[TableRow],
        width: usize,
        base: Style,
        out: &mut Vec<Line<'static>>,
        hard_breaks: &mut Vec<bool>,
        copy_cells: &mut Vec<Option<CopyCells>>,
    ) {
        let columns = rows.iter().map(|row| row.cells.len()).max().unwrap_or(0);
        if columns == 0 {
            return;
        }
        let separators = columns.saturating_sub(1).saturating_mul(3);
        let available = width.saturating_sub(separators);
        if available < columns.saturating_mul(4) {
            for (row_index, row) in rows.iter().enumerate() {
                if row_index > 0 {
                    out.push(Line::default());
                    hard_breaks.push(true);
                    copy_cells.push(Some(CopyCells::decoration()));
                }
                for (column, cell) in row.cells.iter().enumerate() {
                    let mut segs = Vec::new();
                    if row_index > 0 {
                        if let Some(header) = rows[0].cells.get(column) {
                            segs.extend(header.segs.iter().cloned().map(|mut seg| {
                                seg.source = None;
                                seg
                            }));
                            segs.push(Seg {
                                text: ": ".into(),
                                style: Style::new(),
                                link: false,
                                source: None,
                            });
                        }
                    }
                    // Wrapping omits inline newlines. Count only emitted label
                    // bytes, then split each actual row at the label/value edge.
                    let label_bytes: usize = segs
                        .iter()
                        .map(|seg| {
                            seg.text
                                .chars()
                                .filter(|ch| *ch != '\n')
                                .map(char::len_utf8)
                                .sum::<usize>()
                        })
                        .sum();
                    segs.extend(cell.segs.iter().cloned());
                    let empty_value = cell
                        .segs
                        .iter()
                        .all(|seg| seg.text.chars().all(|ch| ch == '\n'));
                    let mut displayed_bytes = 0;
                    let wrapped = wrap_segments_metadata(&segs, width, base);
                    let last = wrapped.len().saturating_sub(1);
                    for (line_index, (line, hard, source_offset)) in wrapped.into_iter().enumerate()
                    {
                        let text = line.to_string();
                        let skip = label_bytes.saturating_sub(displayed_bytes).min(text.len());
                        let start = UnicodeWidthStr::width(&text[..skip]);
                        let mut fragments = Vec::new();
                        if skip < text.len() {
                            fragments.push(TableCopyFragment {
                                columns: start..start + UnicodeWidthStr::width(&text[skip..]),
                                text: text[skip..].to_owned(),
                                row_offset: row.source_offset,
                                cell_offset: cell.source_range.start,
                                cell_index: column,
                                chunk_offset: displayed_bytes.saturating_sub(label_bytes),
                                separator_before: cell.separator_before,
                            });
                        } else if empty_value && line_index == last {
                            // An empty source cell still owns a column place.
                            // One marker on its final label row preserves that
                            // place without copying any generated label text.
                            fragments.push(TableCopyFragment {
                                columns: 0..line_width(&line).max(1),
                                text: String::new(),
                                row_offset: row.source_offset,
                                cell_offset: cell.source_range.start,
                                cell_index: column,
                                chunk_offset: 0,
                                separator_before: cell.separator_before,
                            });
                        }
                        displayed_bytes += text.len();
                        // Only value glyphs carry provenance; repeated labels
                        // cannot steal the anchor from this source cell.
                        copy_cells.push(Some(CopyCells::table(
                            0..line_width(&line),
                            fragments,
                            source_offset
                                .or_else(|| empty_value.then_some(cell.source_range.start)),
                        )));
                        out.push(line);
                        hard_breaks.push(hard);
                    }
                }
            }
        } else {
            let mut widths = vec![4; columns];
            for row in rows {
                for (column, cell) in row.cells.iter().enumerate() {
                    let text: String = cell.segs.iter().map(|seg| seg.text.as_str()).collect();
                    widths[column] = widths[column].max(UnicodeWidthStr::width(text.as_str()));
                }
            }
            // Allocate without iterating over unbounded source widths.
            let mut remaining = available;
            for (column, cell_width) in widths.iter_mut().enumerate() {
                *cell_width = (*cell_width).min(remaining / (columns - column));
                remaining -= *cell_width;
            }
            for (row_index, row) in rows.iter().enumerate() {
                let cells: Vec<_> = widths
                    .iter()
                    .enumerate()
                    .map(|(column, width)| {
                        let segs = row
                            .cells
                            .get(column)
                            .map(|cell| cell.segs.as_slice())
                            .unwrap_or(&[]);
                        wrap_segments_metadata(
                            segs,
                            *width,
                            if row_index == 0 {
                                base.add_modifier(Modifier::BOLD)
                            } else {
                                base
                            },
                        )
                    })
                    .collect();
                let height = cells.iter().map(Vec::len).max().unwrap_or(1);
                let mut chunk_offsets = vec![0; columns];
                for line_index in 0..height {
                    let mut spans = Vec::new();
                    let mut fragments = Vec::new();
                    let mut display_column = 0;
                    let mut row_source_offset = None;
                    let mut empty_source_offset = None;
                    for (column, wrapped) in cells.iter().enumerate() {
                        if column > 0 {
                            spans.push(Span::styled(" │ ", base.fg(self.theme.border)));
                            display_column += 3;
                        }
                        let line = wrapped
                            .get(line_index)
                            .map(|(line, _, _)| line.clone())
                            .unwrap_or_default();
                        let cell_width = line_width(&line);
                        let padding = widths[column].saturating_sub(cell_width);
                        if let Some(cell) = row.cells.get(column) {
                            let text = line.to_string();
                            if !text.is_empty() {
                                if row_source_offset.is_none() {
                                    row_source_offset =
                                        wrapped.get(line_index).and_then(|(_, _, offset)| *offset);
                                }
                                let text_bytes = text.len();
                                fragments.push(TableCopyFragment {
                                    columns: display_column..display_column + cell_width,
                                    text,
                                    row_offset: row.source_offset,
                                    cell_offset: cell.source_range.start,
                                    cell_index: column,
                                    chunk_offset: chunk_offsets[column],
                                    separator_before: cell.separator_before,
                                });
                                chunk_offsets[column] += text_bytes;
                            } else if line_index == 0 && wrapped.len() == 1 {
                                empty_source_offset.get_or_insert(cell.source_range.start);
                                fragments.push(TableCopyFragment {
                                    columns: display_column..display_column + widths[column],
                                    text: String::new(),
                                    row_offset: row.source_offset,
                                    cell_offset: cell.source_range.start,
                                    cell_index: column,
                                    chunk_offset: 0,
                                    separator_before: cell.separator_before,
                                });
                            }
                        }
                        spans.extend(line.spans);
                        display_column += cell_width;
                        if column + 1 < columns {
                            spans.push(Span::raw(" ".repeat(padding)));
                            display_column += padding;
                        }
                    }
                    copy_cells.push(Some(CopyCells::table(
                        0..display_column,
                        fragments,
                        row_source_offset.or(empty_source_offset),
                    )));
                    out.push(Line::from(spans));
                    // Source-copy joins chunks by their parsed logical cell,
                    // regardless of which other columns wrap on this row.
                    hard_breaks.push(line_index + 1 == height);
                }
                if row_index == 0 {
                    let border = widths
                        .iter()
                        .map(|width| "─".repeat(*width))
                        .collect::<Vec<_>>()
                        .join("─┼─");
                    out.push(Line::from(Span::styled(border, base.fg(self.theme.border))));
                    hard_breaks.push(false);
                    copy_cells.push(Some(CopyCells::decoration()));
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
        source: &SourceSpan,
        width: usize,
        out: &mut Vec<Line<'static>>,
        hard_breaks: &mut Vec<bool>,
        copy_cells: &mut Vec<Option<CopyCells>>,
    ) {
        let border = Style::new().fg(self.theme.md_code_border);
        let content = Style::new().fg(self.theme.md_code_block);
        let inner = width.saturating_sub(2).max(1);
        if width < 3 {
            let mut line_offset = 0;
            for line in text.lines() {
                let chunks = chunk_line_offsets(line, inner);
                let last = chunks.len() - 1;
                for (index, (chunk, offset)) in chunks.into_iter().enumerate() {
                    copy_cells.push(Some(CopyCells::content(
                        0..UnicodeWidthStr::width(chunk.as_str()),
                        Some(source.at(line_offset + offset)),
                    )));
                    out.push(Line::from(Span::styled(chunk, content)));
                    hard_breaks.push(index == last);
                }
                line_offset += line.len() + 1;
            }
            return;
        }
        out.push(Line::from(vec![
            Span::styled("╭", border),
            Span::styled("─".repeat(inner), border),
            Span::styled("╮", border),
        ]));
        hard_breaks.push(false);
        copy_cells.push(Some(CopyCells::decoration()));
        let mut line_offset = 0;
        for raw in text.lines() {
            let chunks = chunk_line_offsets(raw, inner);
            let last = chunks.len() - 1;
            for (index, (chunk, offset)) in chunks.into_iter().enumerate() {
                let source_width = UnicodeWidthStr::width(chunk.as_str());
                copy_cells.push(Some(CopyCells::content(
                    1..1 + source_width,
                    Some(source.at(line_offset + offset)),
                )));
                let pad = " ".repeat(inner.saturating_sub(source_width));
                out.push(Line::from(vec![
                    Span::styled("│", border),
                    Span::styled(format!("{chunk}{pad}"), content),
                    Span::styled("│", border),
                ]));
                hard_breaks.push(index == last);
            }
            line_offset += raw.len() + 1;
        }
        out.push(Line::from(vec![
            Span::styled("╰", border),
            Span::styled("─".repeat(inner), border),
            Span::styled("╯", border),
        ]));
        hard_breaks.push(false);
        copy_cells.push(Some(CopyCells::decoration()));
    }
}

/// Splits a (possibly indented) line into `width`-wide chunks, preserving
/// leading whitespace and replacing tabs.
fn chunk_line(line: &str, width: usize) -> Vec<String> {
    chunk_line_offsets(line, width)
        .into_iter()
        .map(|(text, _)| text)
        .collect()
}

/// Offset before display-only tab expansion, including a wrap inside a tab.
fn chunk_line_offsets(line: &str, width: usize) -> Vec<(String, usize)> {
    let width = width.max(1);
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_w = 0;
    let mut current_offset = 0;
    for (offset, ch) in line.char_indices() {
        let (ch, count) = if ch == '\t' { (' ', 4) } else { (ch, 1) };
        for _ in 0..count {
            let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
            if current_w + cw > width && !current.is_empty() {
                chunks.push((std::mem::take(&mut current), current_offset));
                current_w = 0;
            }
            if current.is_empty() {
                current_offset = offset;
            }
            current.push(ch);
            current_w += cw;
        }
    }
    if !current.is_empty() || chunks.is_empty() {
        chunks.push((current, current_offset));
    }
    chunks
}

/// Wraps styled segments to `width` cells. Each emitted row reports whether it
/// terminated a logical source line (a `\n` inside the run or the end of the
/// run) rather than a soft wrap, so copy/export can rebuild the original text.
fn wrap_segments_metadata(
    segs: &[Seg],
    width: usize,
    base: Style,
) -> Vec<(Line<'static>, bool, Option<usize>)> {
    let width = width.max(1);
    let mut lines: Vec<(Line<'static>, bool, Option<usize>)> = Vec::new();
    let mut current: Vec<Span<'static>> = Vec::new();
    let mut current_w = 0usize;
    let mut current_source = None;
    for seg in segs {
        let style = base.patch(seg.style);
        for (offset, ch) in seg.text.char_indices() {
            if ch == '\n' {
                if !current.is_empty() {
                    lines.push((
                        Line::from(std::mem::take(&mut current)),
                        true,
                        current_source.take(),
                    ));
                    current_w = 0;
                }
                continue;
            }
            let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
            if current_w + cw > width && !current.is_empty() {
                lines.push((
                    Line::from(std::mem::take(&mut current)),
                    false,
                    current_source.take(),
                ));
                current_w = 0;
            }
            if current_source.is_none() {
                current_source = seg.source.as_ref().map(|source| source.at(offset));
            }
            push_span_char(&mut current, ch, style);
            current_w += cw;
        }
    }
    if !current.is_empty() {
        lines.push((Line::from(current), true, current_source));
    }
    if lines.is_empty() {
        lines.push((
            Line::default(),
            true,
            segs.first()
                .and_then(|seg| seg.source.as_ref().map(|source| source.start)),
        ));
    }
    lines
}

/// Wraps styled segments like [`wrap_segments_metadata`] and records, per
/// emitted line, the content-cell
/// ranges covered by link segments and whether the line ended a logical
/// source line. All outputs come from one walk so they can never describe
/// different layouts.
#[allow(clippy::too_many_arguments)]
fn wrap_segments_links(
    segs: &[Seg],
    width: usize,
    base: Style,
    lines: &mut Vec<Line<'static>>,
    link_cells: &mut Vec<Vec<std::ops::Range<usize>>>,
    hard_breaks: &mut Vec<bool>,
    copy_cells: &mut Vec<Option<CopyCells>>,
) {
    let width = width.max(1);
    #[allow(clippy::too_many_arguments)]
    fn flush(
        lines: &mut Vec<Line<'static>>,
        link_cells: &mut Vec<Vec<std::ops::Range<usize>>>,
        hard_breaks: &mut Vec<bool>,
        copy_cells: &mut Vec<Option<CopyCells>>,
        current: &mut Vec<Span<'static>>,
        current_links: &mut Vec<std::ops::Range<usize>>,
        current_w: &mut usize,
        current_source: &mut Option<usize>,
        hard: bool,
    ) {
        if !current.is_empty() {
            copy_cells.push(Some(CopyCells {
                columns: 0..*current_w,
                decorative: false,
                source_offset: current_source.take(),
                list_prefix_copyable: true,
                table_fragments: None,
            }));
            lines.push(Line::from(std::mem::take(current)));
            link_cells.push(std::mem::take(current_links));
            hard_breaks.push(hard);
            *current_w = 0;
        }
    }
    let mut current = Vec::new();
    let mut current_links = Vec::new();
    let mut current_w = 0usize;
    let mut current_source = None;
    for seg in segs {
        let style = base.patch(seg.style);
        for (offset, ch) in seg.text.char_indices() {
            if ch == '\n' {
                flush(
                    lines,
                    link_cells,
                    hard_breaks,
                    copy_cells,
                    &mut current,
                    &mut current_links,
                    &mut current_w,
                    &mut current_source,
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
                    copy_cells,
                    &mut current,
                    &mut current_links,
                    &mut current_w,
                    &mut current_source,
                    false,
                );
            }
            if current_source.is_none() {
                current_source = seg.source.as_ref().map(|source| source.at(offset));
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
        copy_cells,
        &mut current,
        &mut current_links,
        &mut current_w,
        &mut current_source,
        true,
    );
    if lines.is_empty() {
        lines.push(Line::default());
        link_cells.push(Vec::new());
        hard_breaks.push(true);
        copy_cells.push(Some(CopyCells {
            columns: 0..0,
            decorative: false,
            source_offset: segs
                .first()
                .and_then(|seg| seg.source.as_ref().map(|source| source.start)),
            list_prefix_copyable: true,
            table_fragments: None,
        }));
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

/// Wraps plain text for unformatted UI rows and the composer (spec 20.4)
/// with no markdown parsing. `\n` is a real line boundary: a newline ends
/// the current line, so consecutive newlines yield the same number of blank
/// lines and a leading/trailing newline yields a blank row — the rendered
/// line count is exactly `text.split('\n').count()`. Long lines are greedy-
/// chunked to `width` display cells. Appending a streamed delta after a
/// newline only fills the tail line, so the paragraph structure of a live
/// plain-text buffer matches the original text after every delta.
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
    fn html_and_xml_remain_visible_literal_source() {
        let theme = dark_theme();
        let renderer = MarkdownRenderer::new(&theme);
        for source in [
            "<instructions>\nReview the release notes.\n</instructions>",
            "<div>\nhello\n\nworld\n</div>",
            "before <em>literal</em> after",
            "<script>alert('safe literal');</script>",
        ] {
            let rendered = renderer.render_with_metadata(source, 80, Style::new());
            assert_eq!(
                rendered
                    .lines
                    .iter()
                    .map(text_of)
                    .collect::<Vec<_>>()
                    .join("\n"),
                source
            );
            assert_eq!(rendered.copy_cells.len(), rendered.lines.len());
            assert_eq!(rendered.hard_breaks.len(), rendered.lines.len());
        }
        let safe = renderer.render("<x>\u{1b}]52;evil\u{7}</x>", 80, Style::new());
        assert_eq!(text_of(&safe[0]), "<x>␛]52;evil␇</x>");
    }

    #[test]
    fn fenced_rows_keep_raw_offsets_after_escaping_and_wrapping() {
        let theme = dark_theme();
        let renderer = MarkdownRenderer::new(&theme);
        let source = "控制\u{1b}prefix\n\n```rust\nFIRST_LONG_LINE\n```\n\n~~~text\nSECOND\n~~~";
        for width in [2, 10, 80] {
            let rendered = renderer.render_with_metadata(source, width, Style::new());
            let offsets: Vec<_> = rendered
                .copy_cells
                .iter()
                .flatten()
                .filter_map(|copy| copy.source_offset)
                .collect();
            assert!(offsets.contains(&source.find("FIRST_LONG_LINE").unwrap()));
            assert!(offsets.contains(&source.find("SECOND").unwrap()));
            assert!(
                offsets
                    .iter()
                    .all(|offset| source.is_char_boundary(*offset))
            );
            for (line, copy) in rendered.lines.iter().zip(&rendered.copy_cells) {
                let Some(copy) = copy.as_ref().filter(|copy| !copy.decorative) else {
                    continue;
                };
                let Some(offset) = copy.source_offset else {
                    continue;
                };
                let body = source.find("FIRST_LONG_LINE").unwrap();
                if (body..body + "FIRST_LONG_LINE".len()).contains(&offset) {
                    assert!(
                        text_of(line).contains(
                            &source[offset..body + "FIRST_LONG_LINE".len()]
                                .chars()
                                .next()
                                .unwrap()
                                .to_string()
                        )
                    );
                }
            }
            assert_eq!(rendered.copy_cells.len(), rendered.lines.len());
        }
    }

    #[test]
    fn wrapped_source_offsets_are_utf8_safe_across_parser_transformations() {
        let theme = dark_theme();
        for source in [
            "one 中文\nsecond café **bold** &amp; \\*literal\nlast 🙂",
            "   ```\n\tfoo中文café\n  \u{1b}a\u{9b}b\u{202e}c\n   ```",
            "- item\n\n  ```\n  first中文\n  second\n  ```",
            "    first中文\n    second café\n",
            "before <br> \u{1b}escaped 中文\nlast",
        ] {
            for width in [1, 2, 3, 8, 19, 79] {
                let rendered =
                    MarkdownRenderer::new(&theme).render_with_metadata(source, width, Style::new());
                for offset in rendered
                    .copy_cells
                    .iter()
                    .flatten()
                    .filter_map(|copy| copy.source_offset)
                {
                    assert!(source.is_char_boundary(offset), "{source:?}: {offset}");
                }
                assert!(
                    rendered
                        .lines
                        .iter()
                        .all(|line| !text_of(line).contains('\u{1b}'))
                );
                assert_eq!(rendered.lines.len(), rendered.copy_cells.len());
            }
        }
    }

    #[test]
    fn multiline_inline_code_keeps_each_wrapped_source_position() {
        let theme = dark_theme();
        let source = "` first line\nsecond 中文 line\nthird line `";
        for width in [4, 9, 20] {
            let rendered =
                MarkdownRenderer::new(&theme).render_with_metadata(source, width, Style::new());
            let offsets: Vec<_> = rendered
                .copy_cells
                .iter()
                .flatten()
                .filter_map(|copy| copy.source_offset)
                .collect();
            assert!(offsets.len() > 1);
            assert!(offsets.windows(2).all(|pair| pair[0] < pair[1]));
            assert!(
                offsets
                    .iter()
                    .all(|offset| source.is_char_boundary(*offset))
            );
            assert!(offsets.last().unwrap() > &source.find("second").unwrap());
        }
    }

    #[test]
    fn literal_html_fallback_retains_fence_source_positions() {
        let theme = dark_theme();
        let source = "before <br> after\n\n```text\nFIRST_LONG_LINE\n```\n\n~~~text\nSECOND\n~~~";
        let rendered = MarkdownRenderer::new(&theme).render_with_metadata(source, 8, Style::new());
        for (line, copy) in rendered.lines.iter().zip(&rendered.copy_cells) {
            let text = text_of(line);
            let offset = copy.as_ref().unwrap().source_offset.unwrap();
            if text.starts_with("FIRST") {
                assert_eq!(offset, source.find("FIRST").unwrap());
            }
            if text.starts_with("NG_LINE") {
                assert_eq!(offset, source.find("NG_LINE").unwrap());
            }
            if text.starts_with("SECOND") {
                assert_eq!(offset, source.find("SECOND").unwrap());
            }
        }
        assert!(
            rendered
                .lines
                .iter()
                .map(text_of)
                .collect::<String>()
                .contains("before <br> after")
        );
    }

    #[test]
    fn tables_are_bounded_and_retain_cells_at_narrow_and_wide_widths() {
        let theme = dark_theme();
        let renderer = MarkdownRenderer::new(&theme);
        let source = "| Name | Status |\n| --- | --- |\n| alpha | ready |\n| beta | done |";
        for width in [8, 12, 24, 80] {
            let rendered = renderer.render_with_metadata(source, width, Style::new());
            let text = rendered
                .lines
                .iter()
                .map(text_of)
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                rendered.lines.iter().all(|line| line_width(line) <= width),
                "{text}"
            );
            if width < 11 {
                for value in ["alpha", "beta", "ready", "done"] {
                    assert!(text.replace('\n', "").contains(value), "{width}: {text}");
                }
            } else {
                // Grid wraps interleave columns; every source cell character
                // must remain, even when a word spans multiple display rows.
                let mut visible: Vec<_> = text.chars().filter(|ch| ch.is_alphanumeric()).collect();
                let mut expected: Vec<_> = "NameStatusalphareadybetadone".chars().collect();
                visible.sort_unstable();
                expected.sort_unstable();
                assert_eq!(visible, expected, "{width}: {text}");
            }
            assert_eq!(rendered.copy_cells.len(), rendered.lines.len());
            assert_eq!(rendered.hard_breaks.len(), rendered.lines.len());
            assert_eq!(rendered.link_cells.len(), rendered.lines.len());
        }
    }

    #[test]
    fn quoted_table_preserves_source_order_and_quote_markers() {
        let theme = dark_theme();
        let source = "> Before\n>\n> | A | B |\n> | --- | --- |\n> | x | y |\n>\n> After";
        let rendered = MarkdownRenderer::new(&theme).render_with_metadata(source, 40, Style::new());
        assert_eq!(
            rendered
                .lines
                .iter()
                .map(text_of)
                .collect::<Vec<_>>()
                .join("\n"),
            source
        );
        assert!(
            rendered
                .copy_cells
                .iter()
                .flatten()
                .all(|copy| copy.source_offset.is_some())
        );
    }

    #[test]
    fn nested_list_table_stays_between_its_item_paragraphs() {
        let theme = dark_theme();
        let source = "- Outer\n  - Before\n\n    | A | B |\n    | --- | --- |\n    | x | y |\n\n    After\n- Last";
        let renderer = MarkdownRenderer::new(&theme);
        for width in [12, 40] {
            let rendered = renderer.render_with_metadata(source, width, Style::new());
            let text = rendered
                .lines
                .iter()
                .map(text_of)
                .collect::<Vec<_>>()
                .join("\n");
            assert!(rendered.lines.iter().all(|line| line_width(line) <= width));
            let before = text.find("Before").unwrap();
            let table = text.find('x').unwrap();
            let after = text.find("After").unwrap();
            let last = text.find("Last").unwrap();
            assert!(before < table && table < after && after < last, "{text}");
            assert_eq!(rendered.copy_cells.len(), rendered.lines.len());
        }
    }

    #[test]
    fn table_cjk_cells_wrap_without_losing_characters() {
        let theme = dark_theme();
        let renderer = MarkdownRenderer::new(&theme);
        let source = "| 字段 | 内容 |\n| --- | --- |\n| 甲乙丙丁戊己 | 天地玄黄宇宙洪荒 |";
        for width in [8, 12, 20, 60] {
            let rendered = renderer.render_with_metadata(source, width, Style::new());
            assert!(rendered.lines.iter().all(|line| line_width(line) <= width));
            let text: String = rendered.lines.iter().map(text_of).collect();
            for ch in "甲乙丙丁戊己天地玄黄宇宙洪荒".chars() {
                assert_eq!(text.matches(ch).count(), 1);
            }
        }
    }

    #[test]
    fn prose_copy_bounds_preserve_spaces_at_soft_wraps() {
        let theme = dark_theme();
        let rendered = MarkdownRenderer::new(&theme).render_with_metadata(
            "first second third",
            6,
            Style::new(),
        );
        assert_eq!(text_of(&rendered.lines[0]), "first ");
        assert!(!rendered.hard_breaks[0]);
        assert_eq!(rendered.copy_cells[0].as_ref().unwrap().columns, 0..6);
        assert_eq!(
            rendered.lines.iter().map(text_of).collect::<String>(),
            "first second third"
        );
    }

    #[test]
    fn strikethrough_is_styled_without_literal_delimiters() {
        let theme = dark_theme();
        let lines = MarkdownRenderer::new(&theme).render("keep ~~removed~~", 40, Style::new());
        assert_eq!(text_of(&lines[0]), "keep removed");
        assert!(
            lines[0]
                .spans
                .iter()
                .any(|span| span.content.contains("removed")
                    && span.style.add_modifier.contains(Modifier::CROSSED_OUT))
        );
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
            [Block::Plain { .. }]
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
        assert!(matches!(blocks.as_slice(), [Block::Plain { text, .. }] if text == safe.as_ref()));
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
            [Block::Plain { .. }]
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
            [Block::Plain { .. }]
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
