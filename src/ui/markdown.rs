//! Markdown to pre-wrapped styled lines, over `pulldown-cmark`. A growing message
//! renders through [`StreamRenderer`], which caches everything before the last
//! boundary later text cannot change, so a delta re-parses only the tail.

use std::sync::Arc;

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::theme::Styles;
use super::wrap;

fn options() -> Options {
    Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS
}

/// Renders `text` into lines no wider than `width`. Blocks are separated by one
/// blank line; nothing separates the items of a tight list.
pub fn render(text: &str, width: usize, styles: &Styles, base: Style) -> Vec<Line<'static>> {
    let source = wrap::sanitize(text);
    if width < 4 {
        return wrap::wrapped(&source, width, base);
    }
    let mut builder = Builder {
        styles,
        base,
        width,
        out: Vec::new(),
        separated: true,
        spans: Vec::new(),
        open: Vec::new(),
        heading: None,
        code: None,
        lists: Vec::new(),
        quote: 0,
        table: None,
        cell: false,
    };
    for event in Parser::new_ext(&source, options()) {
        builder.event(event);
    }
    builder.flush();
    if builder.separated {
        builder.out.pop();
    }
    builder.out
}

enum Open {
    Bold,
    Italic,
    Strike,
    /// `start` indexes the first label span, so the url can be compared to it.
    Link {
        url: String,
        start: usize,
    },
}

struct CodeBuf {
    lang: String,
    text: String,
}

struct ListState {
    index: u64,
    ordered: bool,
    task: Option<bool>,
    /// The current item has not drawn its marker yet.
    marker_pending: bool,
    loose: bool,
    items: usize,
}

impl ListState {
    fn marker(&self) -> String {
        let bullet = if self.ordered {
            format!("{}. ", self.index)
        } else {
            "- ".to_owned()
        };
        match self.task {
            Some(true) => bullet + "[x] ",
            Some(false) => bullet + "[ ] ",
            None => bullet,
        }
    }
}

struct Table {
    rows: Vec<Vec<Vec<Span<'static>>>>,
    /// The first row is a header and draws bold.
    header: bool,
}

struct Builder<'a> {
    styles: &'a Styles,
    base: Style,
    width: usize,
    out: Vec<Line<'static>>,
    /// Nothing or a separator was pushed last, so no further blank is due.
    separated: bool,
    /// Pending inline run of the current paragraph, heading or table cell.
    spans: Vec<Span<'static>>,
    open: Vec<Open>,
    heading: Option<HeadingLevel>,
    code: Option<CodeBuf>,
    lists: Vec<ListState>,
    quote: usize,
    table: Option<Table>,
    cell: bool,
}

impl Builder<'_> {
    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => self.text(&text),
            Event::Code(text) => {
                let style = self.inline_style().patch(self.styles.code());
                self.spans.push(Span::styled(text.into_string(), style));
            }
            Event::SoftBreak => self.spans.push(Span::styled(" ", self.inline_style())),
            Event::HardBreak => {
                if self.cell {
                    self.spans.push(Span::styled(" ", self.inline_style()));
                } else {
                    self.flush();
                }
            }
            Event::Rule => {
                self.flush();
                self.blank();
                let rule = "─".repeat(self.inner_width().min(80));
                self.emit(vec![Line::from(Span::styled(rule, self.styles.faint()))]);
            }
            Event::Html(html) | Event::InlineHtml(html) => {
                let text = strip_tags(&html);
                if !text.trim().is_empty() {
                    self.text(&text);
                }
            }
            Event::FootnoteReference(name) => self.text(&format!("[{name}]")),
            Event::TaskListMarker(done) => {
                if let Some(list) = self.lists.last_mut() {
                    list.task = Some(done);
                }
            }
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => match self.lists.last_mut() {
                Some(list) => list.loose = true,
                None => self.blank(),
            },
            Tag::Heading { level, .. } => {
                self.flush();
                self.blank();
                self.heading = Some(level);
                if level >= HeadingLevel::H3 {
                    let hashes = format!("{} ", "#".repeat(level as usize));
                    self.spans.push(Span::styled(hashes, self.inline_style()));
                }
            }
            Tag::CodeBlock(kind) => {
                self.flush();
                self.blank();
                let lang = match kind {
                    CodeBlockKind::Fenced(info) => info
                        .split_whitespace()
                        .next()
                        .unwrap_or_default()
                        .to_owned(),
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
                self.blank();
                self.lists.push(ListState {
                    index: start.unwrap_or(1),
                    ordered: start.is_some(),
                    task: None,
                    marker_pending: false,
                    loose: false,
                    items: 0,
                });
            }
            Tag::Item => {
                self.flush();
                if self
                    .lists
                    .last()
                    .is_some_and(|list| list.loose && list.items > 0)
                {
                    self.separate();
                }
                if let Some(list) = self.lists.last_mut() {
                    list.items += 1;
                    list.task = None;
                    list.marker_pending = true;
                }
            }
            Tag::Table(_) => {
                self.flush();
                self.blank();
                self.table = Some(Table {
                    rows: Vec::new(),
                    header: false,
                });
            }
            Tag::TableHead | Tag::TableRow => {
                if let Some(table) = self.table.as_mut() {
                    table.rows.push(Vec::new());
                    table.header |= matches!(tag, Tag::TableHead);
                }
            }
            Tag::TableCell => self.cell = true,
            Tag::Emphasis => self.open.push(Open::Italic),
            Tag::Strong => self.open.push(Open::Bold),
            Tag::Strikethrough => self.open.push(Open::Strike),
            Tag::Link { dest_url, .. } => {
                let url = dest_url.strip_prefix("mailto:").unwrap_or(&dest_url);
                self.open.push(Open::Link {
                    url: url.to_owned(),
                    start: self.spans.len(),
                });
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
                if self.lists.last().is_some_and(|list| list.marker_pending) {
                    self.emit(vec![Line::default()]);
                }
                if let Some(list) = self.lists.last_mut() {
                    list.index += 1;
                }
            }
            TagEnd::TableCell => {
                self.cell = false;
                let spans = std::mem::take(&mut self.spans);
                if let Some(row) = self.table.as_mut().and_then(|table| table.rows.last_mut()) {
                    row.push(spans);
                }
            }
            TagEnd::Table => {
                if let Some(table) = self.table.take() {
                    self.table_block(&table);
                }
            }
            TagEnd::Emphasis => {
                self.close(|open| matches!(open, Open::Italic));
            }
            TagEnd::Strong => {
                self.close(|open| matches!(open, Open::Bold));
            }
            TagEnd::Strikethrough => {
                self.close(|open| matches!(open, Open::Strike));
            }
            TagEnd::Link => {
                if let Some(Open::Link { url, start }) =
                    self.close(|open| matches!(open, Open::Link { .. }))
                {
                    let start = start.min(self.spans.len());
                    let label: String = self.spans[start..]
                        .iter()
                        .map(|span| span.content.as_ref())
                        .collect();
                    let label = label.trim();
                    if !label.is_empty() && label != url {
                        let suffix = format!(" ({url})");
                        self.spans.push(Span::styled(suffix, self.styles.dim()));
                    }
                }
            }
            _ => {}
        }
    }

    fn close(&mut self, kind: impl Fn(&Open) -> bool) -> Option<Open> {
        let index = self.open.iter().rposition(kind)?;
        Some(self.open.remove(index))
    }

    /// The style of inline text in the current block with every open marker applied.
    fn inline_style(&self) -> Style {
        let mut style = match self.heading {
            Some(HeadingLevel::H1) => self.styles.heading().add_modifier(Modifier::UNDERLINED),
            Some(_) => self.styles.heading(),
            None if self.quote > 0 => self.styles.dim().add_modifier(Modifier::ITALIC),
            None => self.base,
        };
        for open in &self.open {
            style = match open {
                Open::Bold => style.add_modifier(Modifier::BOLD),
                Open::Italic => style.add_modifier(Modifier::ITALIC),
                Open::Strike => style.add_modifier(Modifier::CROSSED_OUT),
                Open::Link { .. } => style.patch(self.styles.link()),
            };
        }
        style
    }

    fn text(&mut self, text: &str) {
        if let Some(code) = self.code.as_mut() {
            code.text.push_str(text);
            return;
        }
        let style = self.inline_style();
        for (index, part) in text.split('\n').enumerate() {
            if index > 0 {
                if self.cell {
                    self.spans.push(Span::styled(" ", style));
                } else {
                    self.flush();
                }
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
        let lines = wrap::wrap_spans(&spans, self.inner_width());
        self.emit(lines);
    }

    /// The quote borders and list indentation in front of a block's first line
    /// (with the item marker) and of every later line.
    fn margin(&self) -> (Vec<Span<'static>>, Vec<Span<'static>>) {
        let mut rest = Vec::new();
        for _ in 0..self.quote {
            rest.push(Span::styled("│ ", self.styles.faint()));
        }
        let mut first = rest.clone();
        if let Some(list) = self.lists.last() {
            let indent = "    ".repeat(self.lists.len() - 1);
            let marker = list.marker();
            rest.push(Span::raw(format!(
                "{indent}{}",
                " ".repeat(wrap::width(&marker))
            )));
            if !indent.is_empty() {
                first.push(Span::raw(indent));
            }
            first.push(Span::styled(marker, self.styles.success()));
        }
        (first, rest)
    }

    fn inner_width(&self) -> usize {
        let (_, rest) = self.margin();
        self.width.saturating_sub(spans_width(&rest)).max(1)
    }

    fn emit(&mut self, lines: Vec<Line<'static>>) {
        let (first, rest) = self.margin();
        for line in lines {
            let marker = self
                .lists
                .last_mut()
                .is_some_and(|list| std::mem::take(&mut list.marker_pending));
            let mut spans = if marker { first.clone() } else { rest.clone() };
            spans.extend(line.spans);
            self.out.push(Line::from(spans));
            self.separated = false;
        }
    }

    /// Separates this block from the previous one; blocks inside a list item stay packed.
    fn blank(&mut self) {
        if self.lists.is_empty() {
            self.separate();
        }
    }

    fn separate(&mut self) {
        if self.separated {
            return;
        }
        let line = if self.quote == 0 {
            Line::default()
        } else {
            let border = "│ ".repeat(self.quote).trim_end().to_owned();
            Line::from(Span::styled(border, self.styles.faint()))
        };
        self.out.push(line);
        self.separated = true;
    }

    fn code_block(&mut self, code: &CodeBuf) {
        let width = self.inner_width();
        let fence = self.styles.faint();
        let body = self.styles.code_block();
        let mut lines = vec![Line::from(Span::styled(
            wrap::truncate(&format!("```{}", code.lang), width),
            fence,
        ))];
        let text = code.text.strip_suffix('\n').unwrap_or(&code.text);
        for line in text.split('\n') {
            if line.is_empty() {
                lines.push(Line::default());
                continue;
            }
            for chunk in wrap::char_breaks(line, width.saturating_sub(2)) {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(line[chunk].to_owned(), body),
                ]));
            }
        }
        lines.push(Line::from(Span::styled("```", fence)));
        self.emit(lines);
    }

    fn table_block(&mut self, table: &Table) {
        let columns = table.rows.iter().map(Vec::len).max().unwrap_or(0);
        if columns == 0 {
            return;
        }
        let room = self.inner_width().saturating_sub(3 * columns + 1);
        if room < 3 * columns {
            self.table_stacked(table, columns);
            return;
        }
        let mut widths = vec![1; columns];
        for row in &table.rows {
            for (column, cell) in row.iter().enumerate() {
                widths[column] = widths[column].max(spans_width(cell));
            }
        }
        // Shrinking the widest column first keeps short columns unwrapped.
        while widths.iter().sum::<usize>() > room {
            if let Some(widest) = widths.iter_mut().max() {
                *widest -= 1;
            }
        }
        let border = self.base;
        let rule = |left: &str, middle: &str, right: &str| {
            let runs: Vec<String> = widths.iter().map(|width| "─".repeat(*width)).collect();
            let joint = format!("─{middle}─");
            Line::from(Span::styled(
                format!("{left}─{}─{right}", runs.join(&joint)),
                border,
            ))
        };
        let mut lines = vec![rule("┌", "┬", "┐")];
        for (index, row) in table.rows.iter().enumerate() {
            let bold = index == 0 && table.header;
            let cells: Vec<Vec<Line<'static>>> = (0..columns)
                .map(|column| {
                    let spans = row.get(column).map(Vec::as_slice).unwrap_or_default();
                    wrap::wrap_spans(spans, widths[column])
                })
                .collect();
            let height = cells.iter().map(Vec::len).max().unwrap_or(1);
            for line_index in 0..height {
                let mut spans = vec![Span::styled("│ ", border)];
                for (column, cell) in cells.iter().enumerate() {
                    if column > 0 {
                        spans.push(Span::styled(" │ ", border));
                    }
                    let content = cell
                        .get(line_index)
                        .map(|line| line.spans.clone())
                        .unwrap_or_default();
                    let used = spans_width(&content);
                    for mut span in content {
                        if bold {
                            span.style = span.style.add_modifier(Modifier::BOLD);
                        }
                        spans.push(span);
                    }
                    spans.push(Span::raw(" ".repeat(widths[column].saturating_sub(used))));
                }
                spans.push(Span::styled(" │", border));
                lines.push(Line::from(spans));
            }
            if index + 1 < table.rows.len() {
                lines.push(rule("├", "┼", "┤"));
            }
        }
        lines.push(rule("└", "┴", "┘"));
        self.emit(lines);
    }

    /// Columns too narrow for a grid: one labelled value per line.
    fn table_stacked(&mut self, table: &Table, columns: usize) {
        let width = self.inner_width();
        let text_of = |row: &[Vec<Span<'static>>], column: usize| -> String {
            row.get(column)
                .map(|spans| spans.iter().map(|span| span.content.as_ref()).collect())
                .unwrap_or_default()
        };
        let header: Vec<String> = (0..columns)
            .map(|column| text_of(&table.rows[0], column))
            .collect();
        for (index, row) in table.rows.iter().enumerate() {
            if index > 0 {
                self.blank();
            }
            let mut lines = Vec::new();
            for (column, label) in header.iter().enumerate() {
                let value = text_of(row, column);
                let text = if index == 0 || label.is_empty() || *label == value {
                    value
                } else {
                    format!("{label}: {value}")
                };
                for (line_index, line) in wrap::wrap(&text, width).into_iter().enumerate() {
                    let style = if line_index == 0 {
                        self.base.add_modifier(Modifier::BOLD)
                    } else {
                        self.base
                    };
                    lines.push(Line::from(Span::styled(line, style)));
                }
            }
            self.emit(lines);
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

/// Streaming renderer that caches everything before the last stable boundary.
pub struct StreamRenderer {
    text: String,
    width: usize,
    base: Style,
    scan: Scan,
    /// Committed lines, shared so a tail-only update does not copy them.
    prefix: Arc<[Line<'static>]>,
    /// The last frame's result, reused while nothing changed at all.
    current: Option<Arc<[Line<'static>]>>,
}

impl StreamRenderer {
    pub fn new(width: usize, base: Style) -> Self {
        Self {
            text: String::new(),
            width,
            base,
            scan: Scan::default(),
            prefix: Arc::from(Vec::new()),
            current: None,
        }
    }

    /// Appends a delta. The scan and the prefix cache survive, so only the tail
    /// is parsed again on the next render.
    pub fn push(&mut self, delta: &str) {
        if !delta.is_empty() {
            self.text.push_str(delta);
            self.current = None;
        }
    }

    /// Points the renderer at the block's authoritative text. While the text
    /// only grows the cached prefix stays valid; any other text starts over.
    pub fn reset(&mut self, text: &str) {
        match text.strip_prefix(self.text.as_str()) {
            Some(delta) => self.push(delta),
            None => {
                *self = Self::new(self.width, self.base);
                self.text.push_str(text);
            }
        }
    }

    /// Bytes already rendered into the cached prefix; used by tests to prove a
    /// tail-only update does not re-parse the stable part.
    #[cfg(test)]
    pub fn stable_bytes(&self) -> usize {
        self.scan.cached
    }

    /// Renders the current text, re-rendering only what changed.
    pub fn lines(&mut self, styles: &Styles) -> Arc<[Line<'static>]> {
        if let Some(current) = &self.current {
            return current.clone();
        }
        let stable = self.scan.advance(&self.text);
        if stable > self.scan.cached {
            let fresh = render(
                &self.text[self.scan.cached..stable],
                self.width,
                styles,
                self.base,
            );
            if !fresh.is_empty() {
                let mut prefix = Vec::with_capacity(self.prefix.len() + fresh.len() + 1);
                prefix.extend_from_slice(&self.prefix);
                if !prefix.is_empty() {
                    prefix.push(Line::default());
                }
                prefix.extend(fresh);
                self.prefix = Arc::from(prefix);
            }
            self.scan.cached = stable;
        }
        let tail = render(
            &self.text[self.scan.cached..],
            self.width,
            styles,
            self.base,
        );
        let lines: Arc<[Line<'static>]> = if tail.is_empty() {
            self.prefix.clone()
        } else {
            let mut lines = Vec::with_capacity(self.prefix.len() + tail.len() + 1);
            lines.extend_from_slice(&self.prefix);
            if !lines.is_empty() {
                lines.push(Line::default());
            }
            lines.extend(tail);
            Arc::from(lines)
        };
        self.current = Some(lines.clone());
        lines
    }
}

/// Finds, one line at a time, the last offset later text cannot re-render; a cut
/// at a blank line waits for the next line to prove a list, quote or table ended.
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
        // Fences and closers share CommonMark's three-space indent rule, so a
        // fence nested in a list closes and the cut advances instead of stalling.
        let indent = content.len() - trimmed.len();
        if let Some((marker, len)) = self.fence {
            let closed = indent <= 3
                && trimmed.bytes().take_while(|byte| *byte == marker).count() >= len
                && trimmed.bytes().all(|byte| byte == marker);
            if closed {
                self.fence = None;
                self.blank = None;
                self.commit(end);
            }
            return;
        }
        if indent <= 3
            && let Some(open) = fence_open(trimmed)
        {
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
    use crate::ui::theme::Palette;

    fn styles() -> Styles {
        Styles::of(Palette::dark())
    }

    fn plain(text: &str, width: usize) -> Vec<String> {
        render(text, width, &styles(), Style::default())
            .iter()
            .map(|line| line.to_string())
            .collect()
    }

    fn styled(text: &str, width: usize) -> Vec<Line<'static>> {
        render(text, width, &styles(), Style::default())
    }

    /// The style of the first span containing `needle`.
    fn style_of(lines: &[Line<'static>], needle: &str) -> Style {
        lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .find(|span| span.content.contains(needle))
            .unwrap_or_else(|| panic!("no span with {needle:?} in {lines:?}"))
            .style
    }

    #[test]
    fn headings_use_the_heading_style_by_level() {
        let source = "# One\n\n## Two\n\n### Three\n\n#### Four";
        assert_eq!(
            plain(source, 40),
            ["One", "", "Two", "", "### Three", "", "#### Four"]
        );
        let lines = styled(source, 40);
        let heading = styles().heading();
        assert_eq!(
            style_of(&lines, "One"),
            heading.add_modifier(Modifier::UNDERLINED)
        );
        assert_eq!(style_of(&lines, "Two"), heading);
        assert_eq!(style_of(&lines, "### "), heading);
        assert_eq!(style_of(&lines, "Three"), heading);
        assert!(heading.add_modifier.contains(Modifier::BOLD));
        assert_eq!(heading.fg, Some(Palette::dark().heading));
    }

    #[test]
    fn renders_setext_headings() {
        assert_eq!(plain("Title\n=====\n\nbody", 40), ["Title", "", "body"]);
    }

    #[test]
    fn inline_code_is_colored_without_backticks() {
        assert_eq!(plain("Use `cargo test` now.", 40), ["Use cargo test now."]);
        let lines = styled("Use `cargo test` now.", 40);
        assert_eq!(style_of(&lines, "cargo test"), styles().code());
        assert_eq!(style_of(&lines, "cargo test").bg, None);
        let heading = styled("## Run `x`", 40);
        assert_eq!(
            style_of(&heading, "x"),
            styles().heading().patch(styles().code()),
            "code inside a heading keeps the heading weight"
        );
    }

    #[test]
    fn bold_italic_and_strikethrough_add_modifiers() {
        let lines = styled("**b** *i* ~~s~~ ***bi***", 40);
        assert_eq!(plain("**b** *i* ~~s~~ ***bi***", 40), ["b i s bi"]);
        assert_eq!(style_of(&lines, "b").add_modifier, Modifier::BOLD);
        assert_eq!(style_of(&lines, "i").add_modifier, Modifier::ITALIC);
        assert_eq!(style_of(&lines, "s").add_modifier, Modifier::CROSSED_OUT);
        assert_eq!(
            style_of(&lines, "bi").add_modifier,
            Modifier::BOLD | Modifier::ITALIC
        );
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
        assert_eq!(
            plain("[**bold** label](https://x.y)", 60),
            ["bold label (https://x.y)"],
            "a label of several spans is compared as a whole"
        );
        let lines = styled("see [the docs](https://example.com/docs)", 80);
        assert_eq!(style_of(&lines, "the docs"), styles().link());
        assert_eq!(style_of(&lines, "(https"), styles().dim());
    }

    #[test]
    fn list_bullets_take_the_bullet_color() {
        assert_eq!(plain("- one\n- two", 40), ["- one", "- two"]);
        assert_eq!(plain("1. first\n2. second", 40), ["1. first", "2. second"]);
        assert_eq!(plain("3. three\n4. four", 40), ["3. three", "4. four"]);
        let lines = styled("- one\n\n1. first", 40);
        assert_eq!(style_of(&lines, "- "), styles().success());
        assert_eq!(style_of(&lines, "1. "), styles().success());
    }

    #[test]
    fn nested_lists_indent_four_spaces_per_level() {
        let lines = plain("- one\n- two\n  - nested\n    - deeper\n- three", 40);
        assert_eq!(
            lines,
            [
                "- one",
                "- two",
                "    - nested",
                "        - deeper",
                "- three"
            ]
        );
        let ordered = plain("1. a\n   - b\n2. c", 40);
        assert_eq!(ordered, ["1. a", "    - b", "2. c"]);
    }

    #[test]
    fn wrapped_items_align_under_their_text() {
        assert_eq!(
            plain("- alpha beta gamma delta", 12),
            ["- alpha beta", "  gamma", "  delta"]
        );
        assert_eq!(
            plain("- top\n  - alpha beta gamma", 16),
            ["- top", "    - alpha beta", "      gamma"]
        );
        assert_eq!(
            plain("- [ ] alpha beta gamma", 16),
            ["- [ ] alpha beta", "      gamma"]
        );
    }

    #[test]
    fn task_markers_stay_inside_the_bullet() {
        let lines = styled("- [x] done\n- [ ] todo", 40);
        assert_eq!(
            plain("- [x] done\n- [ ] todo", 40),
            ["- [x] done", "- [ ] todo"]
        );
        assert_eq!(style_of(&lines, "- [x] "), styles().success());
        assert_eq!(style_of(&lines, "done"), Style::default());
    }

    #[test]
    fn loose_list_items_are_separated_by_one_blank_line() {
        assert_eq!(
            plain("- a\n\n- b\n\n- c\n\nafter", 40),
            ["- a", "", "- b", "", "- c", "", "after"]
        );
    }

    #[test]
    fn hard_breaks_in_an_item_continue_under_the_text() {
        assert_eq!(plain("- a  \n  b", 40), ["- a", "  b"]);
    }

    #[test]
    fn block_quotes_draw_a_border_and_italic_dim_text() {
        assert_eq!(plain("> quoted text", 40), ["│ quoted text"]);
        assert_eq!(plain("> a\n>\n> b", 40), ["│ a", "│", "│ b"]);
        assert_eq!(plain("> a\n> > b", 40), ["│ a", "│", "│ │ b"]);
        let lines = styled("> **q**", 40);
        assert_eq!(style_of(&lines, "│ "), styles().faint());
        assert_eq!(
            style_of(&lines, "q"),
            styles()
                .dim()
                .add_modifier(Modifier::ITALIC | Modifier::BOLD)
        );
        let wrapped = plain("> alpha beta gamma", 12);
        assert_eq!(wrapped, ["│ alpha beta", "│ gamma"]);
    }

    #[test]
    fn rules_span_the_width_up_to_eighty_cells() {
        let rule = plain("a\n\n---\n\nb", 10);
        assert_eq!(rule, ["a", "", "─".repeat(10).as_str(), "", "b"]);
        assert_eq!(plain("---", 120), ["─".repeat(80)]);
        assert_eq!(style_of(&styled("---", 20), "─"), styles().faint());
    }

    #[test]
    fn code_blocks_keep_fences_and_indent_the_body() {
        let lines = plain("```rust\nfn main() {\n    body();\n}\n```", 40);
        assert_eq!(
            lines,
            ["```rust", "  fn main() {", "      body();", "  }", "```"]
        );
        let rendered = styled("```py\nx = 1\n```", 40);
        assert_eq!(style_of(&rendered, "```py"), styles().faint());
        assert_eq!(style_of(&rendered, "x = 1"), styles().code_block());
        assert!(
            rendered
                .iter()
                .flat_map(|line| line.spans.iter())
                .all(|span| span.style.bg.is_none())
        );
        assert_eq!(
            plain("```\na\n\nb\n```", 40),
            ["```", "  a", "", "  b", "```"]
        );
        assert_eq!(
            plain("```\nabcdefghij\n```", 8),
            ["```", "  abcdef", "  ghij", "```"],
            "long code lines break by cells and keep the indent"
        );
    }

    #[test]
    fn code_blocks_in_list_items_follow_the_item_text() {
        assert_eq!(
            plain("- run:\n\n  ```sh\n  ls\n  ```", 40),
            ["- run:", "  ```sh", "    ls", "  ```"]
        );
    }

    #[test]
    fn tables_draw_a_full_box_with_bold_headers() {
        let lines = plain("| name | value |\n|---|---|\n| a | 1 |\n| bb | 22 |\n", 60);
        assert_eq!(
            lines,
            [
                "┌──────┬───────┐",
                "│ name │ value │",
                "├──────┼───────┤",
                "│ a    │ 1     │",
                "├──────┼───────┤",
                "│ bb   │ 22    │",
                "└──────┴───────┘",
            ]
        );
        let rendered = styled("| h |\n|---|\n| `c` |", 40);
        assert!(
            style_of(&rendered, "h")
                .add_modifier
                .contains(Modifier::BOLD)
        );
        assert_eq!(style_of(&rendered, "c"), styles().code());
    }

    #[test]
    fn shrunk_tables_wrap_the_widest_column() {
        let source = "| k | description |\n|---|---|\n| a | one two three four |\n";
        let lines = plain(source, 20);
        assert!(
            lines.iter().all(|line| wrap::width(line) <= 20),
            "{lines:?}"
        );
        assert_eq!(
            lines,
            [
                "┌───┬──────────────┐",
                "│ k │ description  │",
                "├───┼──────────────┤",
                "│ a │ one two      │",
                "│   │ three four   │",
                "└───┴──────────────┘",
            ]
        );
    }

    #[test]
    fn table_cells_take_inline_html_and_breaks() {
        let text = plain("| A | B |\n|---|---|\n| x | y<br>z |\n\nAfter.", 40);
        assert!(text.iter().any(|row| row.contains("yz")), "{text:?}");
        assert_eq!(text.last().unwrap(), "After.");
        assert_eq!(text[text.len() - 2], "");
    }

    #[test]
    fn renders_narrow_tables_as_labelled_lines() {
        let lines = plain("| name | value |\n|---|---|\n| aa | 1 |\n", 12);
        let joined = lines.join("\n");
        assert!(joined.contains("name: aa"), "{joined}");
        assert!(joined.contains("value: 1"), "{joined}");
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
    fn exactly_one_blank_row_follows_every_block_but_the_last() {
        let source = "# head\n\npara\n- item\n\n> quote\n\n---\n\n```\ncode\n```\n\n| a |\n|---|\n| b |\n\nend";
        assert_eq!(
            plain(source, 20),
            [
                "head",
                "",
                "para",
                "",
                "- item",
                "",
                "│ quote",
                "",
                "─".repeat(20).as_str(),
                "",
                "```",
                "  code",
                "```",
                "",
                "┌───┐",
                "│ a │",
                "├───┤",
                "│ b │",
                "└───┘",
                "",
                "end",
            ]
        );
        assert_eq!(
            plain("first\n\nsecond\n\nthird", 40),
            ["first", "", "second", "", "third"]
        );
        assert_eq!(plain("- item\n\npara", 40), ["- item", "", "para"]);
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

    #[test]
    fn a_list_indented_fence_commits_its_cut() {
        // Opening and closing fences are both indented like the list item, so
        // the scan must treat them the same way or the cut never advances.
        let text = "- item\n\n  ```\n  code\n  ```\n\nafter\n";
        let cut = stable_of(text);
        assert!(cut > 0, "the closed fence lets the cut advance past - item");
        assert!(
            cut <= text.len(),
            "the cut never runs past the scanned text ({cut})"
        );
        let styles = styles();
        let mut renderer = StreamRenderer::new(40, Style::default());
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
            let want: Vec<String> = render(&text[..=index], 40, &styles, Style::default())
                .iter()
                .map(Line::to_string)
                .collect();
            assert_eq!(got, want, "after byte {index}");
        }
    }

    #[test]
    fn an_indented_code_block_is_not_a_fence() {
        // Four spaces is an indented code block, not a fence opener, so the
        // scan must not swallow the rest of the text waiting for a closer.
        let text = "para\n\n    ```\n    not a fence\n\nafter\n";
        let cut = stable_of(text);
        assert!(cut > 0, "the paragraph behind the block is committed");
        let lines = plain(text, 40);
        assert!(
            lines.iter().any(|line| line.contains("after")),
            "text after an indented block still renders: {lines:?}"
        );
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
    fn a_tail_update_reuses_the_prefix_and_skips_reparsing() {
        let styles = styles();
        let mut renderer = StreamRenderer::new(40, Style::default());
        // A closed paragraph is stable, so the prefix is committed.
        renderer.push("stable paragraph\n\n");
        let first = renderer.lines(&styles);
        let committed = renderer.stable_bytes();
        assert!(committed > 0, "the closed paragraph is cached");

        // Growing the tail must not re-render the stable part.
        renderer.push("grow");
        let second = renderer.lines(&styles);
        assert_eq!(
            renderer.stable_bytes(),
            committed,
            "the stable cut did not move, so nothing before it is re-parsed"
        );
        assert_eq!(
            &second[..first.len()],
            &first[..],
            "the cached prefix lines are reused verbatim"
        );

        // An unchanged frame returns the very same allocation.
        let third = renderer.lines(&styles);
        assert!(
            Arc::ptr_eq(&second, &third),
            "an idle frame allocates nothing"
        );
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
