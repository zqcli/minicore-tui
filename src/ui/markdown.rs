//! Markdown to styled lines.
//!
//! A small adapter over `pulldown-cmark`: it keeps the inline styling the
//! transcript needs (headings, emphasis, code, lists, quotes, tables, links)
//! and renders straight into pre-wrapped [`Line`]s, so the layout cache stores
//! exactly the rows that get drawn.
//!
//! [`StreamRenderer`] is the streaming path. An assistant message grows one
//! token at a time, and re-parsing the whole message on every delta would be
//! quadratic. The renderer cuts the text at its last *stable* boundary, caches
//! the lines of everything before it, and re-renders only the tail. A stable
//! boundary is a place where the following text cannot change how the previous
//! block renders, so the incremental result is identical to rendering the whole
//! text in one pass.

use std::sync::Arc;

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::theme::Styles;
use super::wrap;

fn options() -> Options {
    Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH
}

/// Renders `text` into lines no wider than `width`. Blocks are separated by one
/// blank line; nothing separates the items of a list.
pub fn render(text: &str, width: usize, styles: &Styles, base: Style) -> Vec<Line<'static>> {
    let source = wrap::sanitize(text);
    let mut out = Vec::new();
    if width < 4 {
        out.extend(wrap::wrapped(&source, width, base));
        return out;
    }
    let mut builder = Builder {
        styles,
        base,
        width,
        out: &mut out,
        spans: Vec::new(),
        open: Vec::new(),
        heading: None,
        code: None,
        lists: Vec::new(),
        quote: 0,
        table: None,
    };
    for event in Parser::new_ext(&source, options()) {
        builder.event(event);
    }
    builder.flush();
    while out.last().is_some_and(is_blank) {
        out.pop();
    }
    out
}

/// Renders markdown and returns the visible text of each produced line.
pub fn render_with_text(
    text: &str,
    width: usize,
    styles: &Styles,
    base: Style,
) -> (Vec<Line<'static>>, Vec<String>) {
    let lines = render(text, width, styles, base);
    let text = lines.iter().map(Line::to_string).collect();
    (lines, text)
}

#[derive(Clone, Debug)]
enum Open {
    Bold,
    Italic,
    Strike,
    Link(String),
}

impl Open {
    fn same_kind(&self, other: &Self) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }
}

struct CodeBuf {
    lang: String,
    text: String,
}

struct ListState {
    indent: usize,
    index: u64,
    ordered: bool,
}

struct Table {
    rows: Vec<Vec<String>>,
    /// The header is the first row; the grid prints a rule under it.
    header: bool,
}

struct Builder<'a> {
    styles: &'a Styles,
    base: Style,
    width: usize,
    out: &'a mut Vec<Line<'static>>,
    /// Pending inline run of the current paragraph or heading.
    spans: Vec<Span<'static>>,
    open: Vec<Open>,
    heading: Option<HeadingLevel>,
    code: Option<CodeBuf>,
    lists: Vec<ListState>,
    quote: usize,
    table: Option<Table>,
}

impl Builder<'_> {
    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => self.text(&text),
            Event::Code(text) => {
                let style = self.styles.code().bg(self.styles.theme.code_bg);
                let style = self.decorate(style);
                self.spans.push(Span::styled(format!("`{text}`"), style));
            }
            Event::SoftBreak => self.spans.push(Span::raw(" ".to_owned())),
            Event::HardBreak => self.flush(),
            Event::Rule => {
                self.flush();
                self.blank();
                let rule = "─".repeat(self.width);
                self.out
                    .push(Line::from(Span::styled(rule, self.styles.subtle())));
            }
            Event::Html(html) | Event::InlineHtml(html) => {
                let text = strip_tags(&html);
                if !text.trim().is_empty() {
                    self.text(&text);
                }
            }
            Event::FootnoteReference(name) => self.text(&format!("[{name}]")),
            Event::TaskListMarker(done) => self.text(if done { "[x] " } else { "[ ] " }),
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => self.blank(),
            Tag::Heading { level, .. } => {
                self.flush();
                self.blank();
                self.heading = Some(level);
            }
            Tag::CodeBlock(kind) => {
                self.flush();
                self.blank();
                let lang = match kind {
                    CodeBlockKind::Fenced(info) => info.trim().to_owned(),
                    CodeBlockKind::Indented => String::new(),
                };
                self.code = Some(CodeBuf {
                    lang,
                    text: String::new(),
                });
            }
            Tag::BlockQuote(_) => {
                self.flush();
                self.blank();
                self.quote += 1;
            }
            Tag::List(start) => {
                self.flush();
                if self.lists.is_empty() {
                    self.blank();
                }
                self.lists.push(ListState {
                    indent: 2 * self.lists.len(),
                    index: start.unwrap_or(1),
                    ordered: start.is_some(),
                });
            }
            Tag::Item => self.flush(),
            Tag::Table(_) => {
                self.flush();
                self.blank();
                self.table = Some(Table {
                    rows: Vec::new(),
                    header: false,
                });
            }
            Tag::TableHead => {
                if let Some(table) = self.table.as_mut() {
                    table.rows.push(Vec::new());
                    table.header = true;
                }
            }
            Tag::TableRow => {
                if let Some(table) = self.table.as_mut() {
                    table.rows.push(Vec::new());
                }
            }
            Tag::TableCell => {
                if let Some(row) = self.table.as_mut().and_then(|table| table.rows.last_mut()) {
                    row.push(String::new());
                }
            }
            Tag::Emphasis => self.open.push(Open::Italic),
            Tag::Strong => self.open.push(Open::Bold),
            Tag::Strikethrough => self.open.push(Open::Strike),
            Tag::Link { dest_url, .. } => {
                let url = dest_url.strip_prefix("mailto:").unwrap_or(&dest_url);
                self.open.push(Open::Link(url.to_owned()));
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => self.flush(),
            TagEnd::Heading(_) => {
                self.flush();
                self.heading = None;
            }
            TagEnd::CodeBlock => {
                if let Some(code) = self.code.take() {
                    self.code_block(&code);
                }
            }
            TagEnd::BlockQuote(_) => {
                self.flush();
                self.quote = self.quote.saturating_sub(1);
            }
            TagEnd::List(_) => {
                self.flush();
                self.lists.pop();
            }
            TagEnd::Item => {
                self.flush();
                if let Some(list) = self.lists.last_mut() {
                    list.index += 1;
                }
            }
            TagEnd::Table => {
                if let Some(table) = self.table.take() {
                    self.table_block(&table);
                }
            }
            TagEnd::Emphasis => self.close(Open::Italic),
            TagEnd::Strong => self.close(Open::Bold),
            TagEnd::Strikethrough => self.close(Open::Strike),
            TagEnd::Link => {
                if let Some(Open::Link(url)) = self.open.last().cloned() {
                    self.close(Open::Link(url.clone()));
                    let label = self
                        .spans
                        .last()
                        .map(|span| span.content.trim().to_owned())
                        .unwrap_or_default();
                    if !label.is_empty() && label != url {
                        let suffix = format!(" ({url})");
                        self.spans.push(Span::styled(suffix, self.styles.dim()));
                    }
                }
            }
            _ => {}
        }
    }

    fn close(&mut self, kind: Open) {
        if let Some(index) = self.open.iter().rposition(|open| open.same_kind(&kind)) {
            self.open.remove(index);
        }
    }

    /// The inline style implied by every marker currently open.
    fn decorate(&self, style: Style) -> Style {
        let mut style = style;
        for open in &self.open {
            style = match open {
                Open::Bold => style.add_modifier(Modifier::BOLD),
                Open::Italic => style.add_modifier(Modifier::ITALIC),
                Open::Strike => style.add_modifier(Modifier::CROSSED_OUT),
                Open::Link(_) => self.styles.link(),
            };
        }
        style
    }

    fn text(&mut self, text: &str) {
        if let Some(code) = self.code.as_mut() {
            code.text.push_str(text);
            return;
        }
        if let Some(row) = self.table.as_mut().and_then(|table| table.rows.last_mut()) {
            if let Some(cell) = row.last_mut() {
                cell.push_str(text);
            }
            return;
        }
        let style = match self.heading {
            Some(level) if level <= HeadingLevel::H2 => self.styles.heading(),
            Some(_) => self.styles.heading().remove_modifier(Modifier::BOLD),
            None => self.base,
        };
        let style = self.decorate(style);
        for (index, part) in text.split('\n').enumerate() {
            if index > 0 {
                self.flush();
            }
            if !part.is_empty() {
                self.spans.push(Span::styled(part.to_owned(), style));
            }
        }
    }

    /// Wraps and emits the pending inline run.
    fn flush(&mut self) {
        if self.spans.is_empty() {
            return;
        }
        let spans = std::mem::take(&mut self.spans);
        let (lead, cont) = self.prefix();
        let avail = self.width.saturating_sub(spans_width(&lead)).max(1);
        let mut first = true;
        for line in wrap::wrap_spans(&spans, avail) {
            let mut spans = if first { lead.clone() } else { cont.clone() };
            spans.extend(line.spans);
            self.out.push(Line::from(spans));
            first = false;
        }
    }

    /// The quote rail, list indentation and list marker in front of a line.
    fn prefix(&self) -> (Vec<Span<'static>>, Vec<Span<'static>>) {
        let mut lead = Vec::new();
        let mut cont = Vec::new();
        for _ in 0..self.quote {
            lead.push(Span::styled("▍ ", self.styles.subtle()));
            cont.push(Span::styled("▍ ", self.styles.subtle()));
        }
        let last = self.lists.len().saturating_sub(1);
        for (index, list) in self.lists.iter().enumerate() {
            let pad = " ".repeat(list.indent);
            lead.push(Span::raw(pad.clone()));
            cont.push(Span::raw(pad));
            if index < last {
                continue;
            }
            let label = if list.ordered {
                format!("{}. ", list.index)
            } else {
                "• ".to_owned()
            };
            let label_width = wrap::width(&label);
            lead.push(Span::styled(label, self.styles.dim()));
            cont.push(Span::raw(" ".repeat(label_width)));
        }
        (lead, cont)
    }

    /// Separates this block from the previous one with a single blank line.
    fn blank(&mut self) {
        if self.out.last().is_some_and(|line| !is_blank(line)) {
            self.out.push(Line::default());
        }
    }

    fn code_block(&mut self, code: &CodeBuf) {
        let inner = self.width.saturating_sub(2).max(1);
        let border = self.styles.code_border();
        let content = self.styles.code();
        let label = format!(" {} ", code.lang);
        if code.lang.is_empty() || wrap::width(&label) + 3 > inner {
            self.out.push(Line::from(Span::styled(
                format!("╭{}╮", "─".repeat(inner)),
                border,
            )));
        } else {
            let rest = inner.saturating_sub(wrap::width(&label) + 1);
            self.out.push(Line::from(vec![
                Span::styled("╭─", border),
                Span::styled(label, self.styles.dim()),
                Span::styled(format!("{}╮", "─".repeat(rest)), border),
            ]));
        }
        let body = code.text.strip_suffix('\n').unwrap_or(&code.text);
        for line in body.split('\n') {
            let clean = wrap::sanitize(line);
            for chunk in wrap::char_breaks(&clean, inner) {
                let text = &clean[chunk];
                let pad = " ".repeat(inner.saturating_sub(wrap::width(text)));
                self.out.push(Line::from(vec![
                    Span::styled("│", border),
                    Span::styled(format!("{text}{pad}"), content),
                    Span::styled("│", border),
                ]));
            }
        }
        self.out.push(Line::from(Span::styled(
            format!("╰{}╯", "─".repeat(inner)),
            border,
        )));
    }

    fn table_block(&mut self, table: &Table) {
        let columns = table.rows.iter().map(Vec::len).max().unwrap_or(0);
        if columns == 0 {
            return;
        }
        let available = self.width.saturating_sub(3 * columns.saturating_sub(1));
        if available < columns * 6 {
            self.table_stacked(table, columns);
        } else {
            self.table_grid(table, columns, available);
        }
    }

    /// Columns too narrow for a grid: one labelled value per line.
    fn table_stacked(&mut self, table: &Table, columns: usize) {
        let header = table.rows.first().cloned().unwrap_or_default();
        for (index, row) in table.rows.iter().enumerate() {
            if index > 0 {
                self.blank();
            }
            for column in 0..columns {
                let value = row.get(column).map(String::as_str).unwrap_or_default();
                let label = header.get(column).map(String::as_str).unwrap_or_default();
                let text = if index == 0 || label.is_empty() || label == value {
                    value.to_owned()
                } else {
                    format!("{label}: {value}")
                };
                for (line_index, line) in
                    wrap::wrap(&text, self.width.max(1)).into_iter().enumerate()
                {
                    let style = if line_index == 0 {
                        self.styles.bold()
                    } else {
                        self.styles.text()
                    };
                    self.out.push(Line::from(Span::styled(line, style)));
                }
            }
        }
    }

    fn table_grid(&mut self, table: &Table, columns: usize, available: usize) {
        let mut widths = vec![3usize; columns];
        for row in &table.rows {
            for (column, cell) in row.iter().enumerate() {
                widths[column] = widths[column].max(wrap::width(cell));
            }
        }
        let share = (available / columns).max(3);
        for width in &mut widths {
            *width = (*width).min(share);
        }
        let rows: Vec<Vec<Vec<String>>> = table
            .rows
            .iter()
            .map(|row| {
                (0..columns)
                    .map(|column| {
                        let cell = row.get(column).map(String::as_str).unwrap_or_default();
                        wrap::wrap(cell, widths[column])
                    })
                    .collect()
            })
            .collect();
        for (index, row) in rows.iter().enumerate() {
            let height = row.iter().map(Vec::len).max().unwrap_or(1);
            for line_index in 0..height {
                let mut spans = Vec::new();
                for (column, cell) in row.iter().enumerate() {
                    if column > 0 {
                        spans.push(Span::styled(" │ ", self.styles.subtle()));
                    }
                    let text = cell.get(line_index).map(String::as_str).unwrap_or_default();
                    let pad = " ".repeat(widths[column].saturating_sub(wrap::width(text)));
                    let style = if index == 0 {
                        self.styles.bold()
                    } else {
                        self.styles.text()
                    };
                    spans.push(Span::styled(format!("{text}{pad}"), style));
                }
                self.out.push(Line::from(spans));
            }
            if index == 0 && table.header {
                let rule = widths
                    .iter()
                    .map(|width| "─".repeat(*width))
                    .collect::<Vec<_>>()
                    .join("─┼─");
                self.out
                    .push(Line::from(Span::styled(rule, self.styles.subtle())));
            }
        }
    }
}

/// Removes HTML tags, keeping the text between them.
fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out
}

fn spans_width(spans: &[Span<'static>]) -> usize {
    spans.iter().map(|span| wrap::width(&span.content)).sum()
}

fn is_blank(line: &Line<'_>) -> bool {
    line.spans.iter().all(|span| span.content.trim().is_empty())
}

/// Streaming renderer that caches everything before the last stable boundary.
pub struct StreamRenderer {
    text: String,
    width: usize,
    base: Style,
    scan: Scan,
    prefix: Vec<Line<'static>>,
}

impl StreamRenderer {
    pub fn new(width: usize, base: Style) -> Self {
        Self {
            text: String::new(),
            width,
            base,
            scan: Scan::default(),
            prefix: Vec::new(),
        }
    }

    /// Appends a delta. The scan and the prefix cache survive, so only the tail
    /// is parsed again on the next render.
    pub fn push(&mut self, delta: &str) {
        self.text.push_str(delta);
    }

    /// Points the renderer at the block's authoritative text. The text only
    /// ever grows, so the cached prefix stays valid.
    pub fn reset(&mut self, text: &str) {
        if self.text != text {
            self.text.clear();
            self.text.push_str(text);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn set_width(&mut self, width: usize) {
        if width != self.width {
            self.width = width;
            self.prefix.clear();
            self.scan.cached = 0;
        }
    }

    /// Renders the current text, re-rendering only what changed.
    pub fn lines(&mut self, styles: &Styles) -> Arc<[Line<'static>]> {
        let stable = self.scan.advance(&self.text);
        if stable > self.scan.cached {
            let fresh = render(
                &self.text[self.scan.cached..stable],
                self.width,
                styles,
                self.base,
            );
            if !fresh.is_empty() {
                if !self.prefix.is_empty() {
                    self.prefix.push(Line::default());
                }
                self.prefix.extend(fresh);
            }
            self.scan.cached = stable;
        }
        let tail = render(
            &self.text[self.scan.cached..],
            self.width,
            styles,
            self.base,
        );
        if tail.is_empty() {
            return Arc::from(self.prefix.clone());
        }
        let mut lines = self.prefix.clone();
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        lines.extend(tail);
        Arc::from(lines)
    }
}

/// Finds the last byte offset that ends a complete block that later text
/// cannot change.
///
/// The scan is incremental: each line is consumed once, and a candidate cut at
/// a blank line is only committed when the next non-blank line proves it does
/// not continue the block before it (lists, quotes and tables can span blank
/// lines). Anything else stays in the re-rendered tail.
#[derive(Default)]
struct Scan {
    /// Bytes consumed so far; always a line boundary.
    scanned: usize,
    /// Bytes already rendered into the cached prefix.
    cached: usize,
    /// Last committed cut.
    stable: usize,
    fence: Option<(u8, usize)>,
    /// The block the most recent line belongs to.
    open: BlockKind,
    /// End of the most recent blank line, if a cut there may still be valid.
    blank: Option<usize>,
}

#[derive(Clone, Copy, Default, PartialEq)]
enum BlockKind {
    #[default]
    None,
    Paragraph,
    List,
    Quote,
    Table,
}

impl Scan {
    fn advance(&mut self, text: &str) -> usize {
        let mut offset = self.scanned;
        while let Some(newline) = text[offset..].find('\n') {
            let start = offset;
            let line = &text[start..start + newline];
            offset = start + newline + 1;
            self.line(line, start, offset);
        }
        self.scanned = offset;
        self.stable
    }

    fn line(&mut self, line: &str, start: usize, end: usize) {
        let content = line.trim_end();
        let trimmed = content.trim_start();
        if let Some((marker, len)) = self.fence {
            let closed = content.bytes().take_while(|byte| *byte == marker).count() >= len
                && content.bytes().all(|byte| byte == marker);
            if closed {
                self.fence = None;
                self.blank = None;
                self.commit(end);
            }
            return;
        }
        if let Some(open) = fence_open(trimmed) {
            self.fence = Some(open);
            self.blank = None;
            return;
        }
        if content.trim().is_empty() {
            // The cut goes before the blank line, so the blank line always
            // belongs to the re-rendered tail and separators are never doubled.
            if matches!(
                self.open,
                BlockKind::List | BlockKind::Quote | BlockKind::Table
            ) {
                self.blank = Some(start);
            } else {
                self.commit(start);
                self.open = BlockKind::None;
            }
            return;
        }
        if let Some(blank) = self.blank.take()
            && !self.continues(content, trimmed)
        {
            self.commit(blank);
            self.open = BlockKind::None;
        }
        let indent = content.len() - trimmed.len();
        let standalone =
            indent < 4 && (is_heading(trimmed) || is_setext_underline(content) || is_rule(content));
        if standalone {
            self.commit(end);
            self.open = BlockKind::None;
        } else if is_list_marker(trimmed) {
            self.open = BlockKind::List;
        } else if trimmed.starts_with('>') {
            self.open = BlockKind::Quote;
        } else if trimmed.starts_with('|') {
            self.open = BlockKind::Table;
        } else if indent < 4 {
            self.open = BlockKind::Paragraph;
        }
    }

    /// Whether a line after a blank line still belongs to the previous block.
    fn continues(&self, content: &str, trimmed: &str) -> bool {
        let indent = content.len() - trimmed.len();
        match self.open {
            BlockKind::List => indent >= 2 || is_list_marker(trimmed),
            BlockKind::Quote => trimmed.starts_with('>'),
            BlockKind::Table => trimmed.starts_with('|'),
            BlockKind::Paragraph | BlockKind::None => false,
        }
    }

    fn commit(&mut self, at: usize) {
        self.stable = self.stable.max(at);
    }
}

fn fence_open(content: &str) -> Option<(u8, usize)> {
    let marker = *content.as_bytes().first()?;
    if marker != b'`' && marker != b'~' {
        return None;
    }
    let len = content.bytes().take_while(|byte| *byte == marker).count();
    (len >= 3).then_some((marker, len))
}

fn is_heading(trimmed: &str) -> bool {
    let hashes = trimmed.bytes().take_while(|byte| *byte == b'#').count();
    hashes > 0 && hashes <= 6 && (trimmed.len() == hashes || trimmed.as_bytes()[hashes] == b' ')
}

fn is_setext_underline(content: &str) -> bool {
    let bytes = content.as_bytes();
    bytes.len() >= 2
        && (bytes[0] == b'=' || bytes[0] == b'-')
        && bytes.iter().all(|b| *b == bytes[0])
}

fn is_rule(content: &str) -> bool {
    let compact: Vec<u8> = content
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    compact.len() >= 3
        && matches!(compact[0], b'-' | b'*' | b'_')
        && compact.iter().all(|byte| *byte == compact[0])
}

fn is_list_marker(trimmed: &str) -> bool {
    if trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .or_else(|| trimmed.strip_prefix("+ "))
        .is_some()
    {
        return true;
    }
    let digits = trimmed.bytes().take_while(u8::is_ascii_digit).count();
    digits > 0 && trimmed[digits..].starts_with(". ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{Palette, ThemeKind};

    fn styles() -> Styles {
        Styles::new(Palette::for_kind(ThemeKind::Dark), false)
    }

    fn plain(text: &str, width: usize) -> Vec<String> {
        render(text, width, &styles(), Style::default())
            .iter()
            .map(|line| line.to_string())
            .collect()
    }

    #[test]
    fn renders_paragraphs_headings_and_emphasis() {
        let lines = plain("# Title\n\nSome **bold** and *italic* text.", 40);
        assert_eq!(lines[0], "Title");
        assert_eq!(lines[2], "Some bold and italic text.");
        let rendered = render("# Title", 40, &styles(), Style::default());
        assert!(
            rendered[0].spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
        assert!(
            rendered[0].spans[0]
                .style
                .fg
                .is_some_and(|color| color == styles().theme.heading)
        );
    }

    #[test]
    fn renders_code_spans_and_fences() {
        assert_eq!(
            plain("Use `cargo test` now.", 40),
            ["Use `cargo test` now."]
        );
        let fenced = plain("```rust\nlet x = 1;\n```", 40);
        assert!(fenced[0].starts_with("╭─ rust"), "{fenced:?}");
        assert!(fenced.iter().any(|line| line.contains("let x = 1;")));
        assert!(fenced.last().unwrap().starts_with('╰'));
        for line in &fenced {
            assert_eq!(wrap::width(line), 40, "{line:?}");
        }
        let unlabeled = plain("```\nplain\n```", 30);
        assert!(unlabeled[0].starts_with('╭'));
        assert!(unlabeled[0].ends_with('╮'));
    }

    #[test]
    fn keeps_indentation_inside_code() {
        let lines = plain("```\nfn main() {\n    body();\n}\n```", 40);
        assert!(lines.iter().any(|line| line.contains("    body();")));
    }

    #[test]
    fn renders_nested_lists_with_markers() {
        let lines = plain("- one\n- two\n  - nested\n", 40);
        assert_eq!(lines, ["• one", "• two", "  • nested"]);
        let ordered = plain("1. first\n2. second\n", 40);
        assert_eq!(ordered, ["1. first", "2. second"]);
    }

    #[test]
    fn wraps_list_items_with_hanging_indentation() {
        let lines = plain("- alpha beta gamma delta", 12);
        assert_eq!(lines, ["• alpha beta", "  gamma", "  delta"]);
    }

    #[test]
    fn renders_block_quotes_with_a_rail() {
        assert_eq!(plain("> quoted text", 40), ["▍ quoted text"]);
    }

    #[test]
    fn renders_tables_as_a_grid_when_wide_enough() {
        let lines = plain("| name | value |\n|---|---|\n| a | 1 |\n| bb | 22 |\n", 60);
        assert!(lines[0].starts_with("name"), "{lines:?}");
        assert!(lines[0].contains("value"));
        assert!(lines[1].contains('┼'));
        assert!(
            lines
                .iter()
                .any(|line| line.contains("bb") && line.contains("22")),
            "{lines:?}"
        );
    }

    #[test]
    fn renders_narrow_tables_as_labelled_lines() {
        let lines = plain("| name | value |\n|---|---|\n| aa | 1 |\n", 14);
        let joined = lines.join("\n");
        assert!(joined.contains("name: aa"), "{joined}");
        assert!(joined.contains("value: 1"), "{joined}");
    }

    #[test]
    fn renders_links_with_their_target_once() {
        assert_eq!(
            plain("see [the docs](https://example.com/docs) here", 80),
            ["see the docs (https://example.com/docs) here"]
        );
        assert_eq!(
            plain("[https://example.com](https://example.com)", 60),
            ["https://example.com"]
        );
    }

    #[test]
    fn drops_html_tags_instead_of_leaking_markup() {
        assert_eq!(plain("a <b>bold</b> word", 40), ["a bold word"]);
    }

    #[test]
    fn wraps_prose_between_words() {
        let lines = plain("alpha beta gamma delta epsilon zeta", 12);
        assert!(lines.iter().all(|line| wrap::width(line) <= 12));
        assert_eq!(lines.join(" "), "alpha beta gamma delta epsilon zeta");
    }

    #[test]
    fn separates_top_level_blocks_with_one_blank_line() {
        assert_eq!(
            plain("first\n\nsecond\n\nthird", 40),
            ["first", "", "second", "", "third"]
        );
        assert_eq!(plain("para\n\n- item", 40), ["para", "", "• item"]);
        assert_eq!(plain("- item\n\npara", 40), ["• item", "", "para"]);
        let fenced = plain("# head\n\ntext\n\n```\ncode\n```", 20);
        assert_eq!(fenced[..4], ["head", "", "text", ""]);
        assert_eq!(fenced[4], format!("╭{}╮", "─".repeat(18)));
        assert_eq!(fenced[5], format!("│code{}│", " ".repeat(14)));
        assert_eq!(fenced[6], format!("╰{}╯", "─".repeat(18)));
    }

    #[test]
    fn renders_setext_headings() {
        assert_eq!(plain("Title\n=====\n\nbody", 40), ["Title", "", "body"]);
    }

    #[test]
    fn renders_thematic_breaks_and_task_lists() {
        let lines = plain("- [x] done\n- [ ] todo", 40);
        assert_eq!(lines, ["• [x] done", "• [ ] todo"]);
        let rule = plain("a\n\n---\n\nb", 10);
        assert_eq!(
            rule,
            [
                "a".to_owned(),
                String::new(),
                "─".repeat(10),
                String::new(),
                "b".to_owned()
            ]
        );
    }

    #[test]
    fn strips_ansi_from_agent_text() {
        assert_eq!(plain("\u{1b}[31mred\u{1b}[0m text", 40), ["red text"]);
    }

    #[test]
    fn narrow_width_falls_back_to_plain_wrapping() {
        let lines = plain("some text here", 3);
        assert!(lines.iter().all(|line| wrap::width(line) <= 3));
    }

    #[test]
    fn incremental_render_equals_full_render() {
        let text = "# Heading\n\nA paragraph with **bold** text that is long enough to wrap \
                    over several lines in a narrow viewport.\n\n- item one\n- item two\n\n\
                    ```rust\nfn main() {}\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n\
                    > quoted\n\nFinal paragraph.\n";
        let styles = styles();
        for width in [12, 20, 40, 80] {
            for end in 0..=text.len() {
                if !text.is_char_boundary(end) {
                    continue;
                }
                let mut renderer = StreamRenderer::new(width, Style::default());
                renderer.push(&text[..end]);
                let got: Vec<String> = renderer
                    .lines(&styles)
                    .iter()
                    .map(Line::to_string)
                    .collect();
                let want: Vec<String> = render(&text[..end], width, &styles, Style::default())
                    .iter()
                    .map(Line::to_string)
                    .collect();
                assert_eq!(got, want, "prefix {end} at width {width}");
            }
        }
    }

    #[test]
    fn incremental_render_equals_full_render_byte_by_byte() {
        let text = "- a\n- b\n\n> quote\n> more\n\n```sh\necho hi\n```\n\nafter\n";
        let styles = styles();
        let mut renderer = StreamRenderer::new(30, Style::default());
        for index in 0..text.len() {
            if !text.is_char_boundary(index + 1) {
                continue;
            }
            renderer.push(&text[index..index + 1]);
            let got: Vec<String> = renderer
                .lines(&styles)
                .iter()
                .map(Line::to_string)
                .collect();
            let want: Vec<String> = render(&text[..=index], 30, &styles, Style::default())
                .iter()
                .map(Line::to_string)
                .collect();
            assert_eq!(got, want, "after byte {index}");
        }
    }

    #[test]
    fn stable_cuts_land_on_block_boundaries() {
        assert_eq!(stable_of(""), 0);
        assert_eq!(stable_of("one line\n"), 0);
        assert_eq!(stable_of("one line\n\n"), 9);
        assert_eq!(stable_of("```\ncode\n"), 0);
        assert_eq!(stable_of("```\ncode\n```\n"), 13);
        assert_eq!(stable_of("# head\n"), 7);
        assert_eq!(stable_of("1. a\n"), 0);
        assert_eq!(
            stable_of("1. a\n\n1. b\n"),
            0,
            "an ordered list can continue"
        );
        assert_eq!(stable_of("1. a\n\nparagraph\n"), 5);
        assert_eq!(stable_of("1. a\n    cont\n"), 0);
    }

    fn stable_of(text: &str) -> usize {
        Scan::default().advance(text)
    }

    #[test]
    fn stable_cuts_are_monotonic() {
        let text = "a\n\nb\n\nc\n";
        let mut scan = Scan::default();
        let mut last = 0;
        for end in 1..=text.len() {
            if !text.is_char_boundary(end) {
                continue;
            }
            let cut = scan.advance(&text[..end]);
            assert!(cut >= last, "{cut} < {last} at {end}");
            last = cut;
        }
        assert_eq!(
            last, 5,
            "the closed paragraph prefix; the rest stays in the tail"
        );
    }

    #[test]
    fn width_changes_reset_the_cached_prefix() {
        let styles = styles();
        let mut renderer = StreamRenderer::new(40, Style::default());
        renderer.push("first paragraph\n\nsecond paragraph\n");
        assert_eq!(renderer.lines(&styles).len(), 3);
        renderer.set_width(10);
        let narrow = renderer.lines(&styles);
        assert_eq!(
            narrow.len(),
            5,
            "{:?}",
            narrow.iter().map(Line::to_string).collect::<Vec<_>>()
        );
        assert_eq!(narrow[0].to_string(), "first");
        assert_eq!(narrow[1].to_string(), "paragraph");
        assert_eq!(narrow[2].to_string(), "");
        assert_eq!(narrow[3].to_string(), "second");
    }

    #[test]
    fn streamed_chunks_match_a_single_push() {
        let styles = styles();
        let text = "Some **streamed** answer.\n\n- one\n- two\n\nEnd.\n";
        let mut streamed = StreamRenderer::new(24, Style::default());
        for chunk in text.as_bytes().chunks(5) {
            streamed.push(&String::from_utf8_lossy(chunk));
            streamed.lines(&styles);
        }
        let mut whole = StreamRenderer::new(24, Style::default());
        whole.push(text);
        assert_eq!(streamed.lines(&styles), whole.lines(&styles));
    }

    #[test]
    fn trailing_newline_does_not_duplicate_blank_lines() {
        let styles = styles();
        let mut renderer = StreamRenderer::new(40, Style::default());
        renderer.push("first\n\n");
        assert_eq!(renderer.lines(&styles).len(), 1);
        renderer.push("\n");
        assert_eq!(
            renderer.lines(&styles).len(),
            1,
            "trailing blanks add no rows"
        );
        renderer.push("second\n\n");
        let lines = renderer.lines(&styles);
        assert_eq!(
            lines.iter().map(Line::to_string).collect::<Vec<_>>(),
            ["first", "", "second"]
        );
    }
}
