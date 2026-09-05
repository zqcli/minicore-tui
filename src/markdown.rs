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
}

/// A block-level markdown element.
enum Block {
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
        items: Vec<Vec<Seg>>,
    },
    Rule,
}

#[derive(Clone, Copy)]
enum InlineAttr {
    Italic,
    Bold,
    Link,
}

struct Builder<'a> {
    theme: &'a Theme,
    blocks: Vec<Block>,
    list: Option<(bool, u64, Vec<Vec<Seg>>)>,
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
        if let Some((_, _, items)) = self.list.as_mut() {
            items.push(segs);
        } else if self.quote {
            self.quote_paras.push(segs);
        } else {
            self.blocks.push(Block::Paragraph(segs));
        }
    }

    fn list_begin(&mut self, ordered: bool, start: u64) {
        if self.list.is_none() {
            self.list = Some((ordered, start, Vec::new()));
        }
    }

    fn list_end(&mut self) {
        self.flush();
        if let Some((ordered, start, items)) = self.list.take() {
            self.blocks.push(Block::List {
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
                });
            }
            segs.extend(para);
        }
        self.blocks.push(Block::Quote(segs));
    }

    fn heading_end(&mut self) {
        let level = self.heading.take();
        let segs = std::mem::take(&mut self.inline);
        let level = match level {
            Some(level) => level as u8,
            None => 6,
        };
        if !segs.is_empty() {
            self.blocks.push(Block::Heading { level, segs });
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
        self.list_end();
    }
}

/// Renders durable Markdown messages and request-local live reasoning.
pub struct MarkdownRenderer<'a> {
    theme: &'a Theme,
}

impl<'a> MarkdownRenderer<'a> {
    pub fn new(theme: &'a Theme) -> Self {
        Self { theme }
    }

    /// Parses `text` into blocks.
    fn parse(&self, text: &str) -> Vec<Block> {
        #[cfg(test)]
        MARKDOWN_PARSE_COUNT.with(|count| count.set(count.get() + 1));
        let options = Options::empty();
        let parser = Parser::new_ext(text, options);
        let mut b = Builder {
            theme: self.theme,
            blocks: Vec::new(),
            list: None,
            quote_paras: Vec::new(),
            quote: false,
            heading: None,
            inline: Vec::new(),
            attrs: Vec::new(),
            link: None,
            code: None,
        };
        for event in parser {
            match event {
                Event::Start(tag) => match tag {
                    Tag::Heading { level, .. } => b.heading = Some(level),
                    Tag::BlockQuote(..) => b.quote = true,
                    Tag::List(Some(start)) => b.list_begin(true, start),
                    Tag::List(None) => b.list_begin(false, 1),
                    Tag::Item => {}
                    Tag::CodeBlock(CodeBlockKind::Indented | CodeBlockKind::Fenced(_)) => {
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
                        b.blocks.push(Block::Code { text });
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
                    text: " ".to_owned(),
                    style: Style::new(),
                }),
                Event::HardBreak => b.push_seg(Seg {
                    text: "\n".to_owned(),
                    style: Style::new(),
                }),
                Event::Rule => b.blocks.push(Block::Rule),
                _ => {}
            }
        }
        b.finish();
        b.blocks
    }

    /// Renders `text` into lines no wider than `width`. `style` is the base
    /// every span starts from (e.g. the user card adds its background here).
    pub fn render(&self, text: &str, width: usize, style: Style) -> Vec<Line<'static>> {
        let blocks = self.parse(text);
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

    fn block_lines(&self, block: &Block, width: usize, base: Style, out: &mut Vec<Line<'static>>) {
        match block {
            Block::Paragraph(segs) => out.extend(self.wrap_segments(segs, width, base)),
            Block::Heading { level, segs } => {
                let mut style = base.fg(self.theme.md_heading);
                if *level <= 2 {
                    style = style.add_modifier(Modifier::BOLD);
                }
                out.extend(self.wrap_segments(segs, width, style));
            }
            Block::Quote(segs) => {
                let inner = width.saturating_sub(2).max(1);
                let quote = base.fg(self.theme.md_quote);
                let wrapped = self.wrap_segments(segs, inner, quote);
                let marker = Span::styled("▍ ", Style::new().fg(self.theme.md_quote));
                let indent = Span::styled("  ", Style::new().fg(self.theme.md_quote));
                for (index, line) in wrapped.into_iter().enumerate() {
                    let mut spans = vec![if index == 0 {
                        marker.clone()
                    } else {
                        indent.clone()
                    }];
                    spans.extend(line.spans);
                    out.push(Line::from(spans));
                }
            }
            Block::Code { text } => self.code_lines(text, width, out),
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
                    let wrapped = self.wrap_segments(item, inner, base);
                    let bullet = Span::styled(marker.clone(), bullet_color);
                    let indent = Span::styled(" ".repeat(marker_w), Style::new());
                    for (line_index, line) in wrapped.into_iter().enumerate() {
                        let mut spans = vec![if line_index == 0 {
                            bullet.clone()
                        } else {
                            indent.clone()
                        }];
                        spans.extend(line.spans);
                        out.push(Line::from(spans));
                    }
                }
            }
            Block::Rule => {
                out.push(Line::from(Span::styled(
                    "─".repeat(width),
                    Style::new().fg(self.theme.border),
                )));
            }
        }
    }

    /// A single-color framed code block: border in `md_code_border`, content
    /// in `md_code_block`, indentation preserved, long lines soft-wrapped
    /// (spec 20.3). No syntax highlighting.
    fn code_lines(&self, text: &str, width: usize, out: &mut Vec<Line<'static>>) {
        let border = Style::new().fg(self.theme.md_code_border);
        let content = Style::new().fg(self.theme.md_code_block);
        let inner = width.saturating_sub(2).max(1);
        if width < 3 {
            for line in text.lines() {
                for chunk in chunk_line(line, inner) {
                    out.push(Line::from(Span::styled(chunk, content)));
                }
            }
            return;
        }
        out.push(Line::from(vec![
            Span::styled("╭", border),
            Span::styled("─".repeat(inner), border),
            Span::styled("╮", border),
        ]));
        for raw in text.lines() {
            for chunk in chunk_line(raw, inner) {
                let pad = " ".repeat(inner.saturating_sub(UnicodeWidthStr::width(chunk.as_str())));
                out.push(Line::from(vec![
                    Span::styled("│", border),
                    Span::styled(format!("{chunk}{pad}"), content),
                    Span::styled("│", border),
                ]));
            }
        }
        out.push(Line::from(vec![
            Span::styled("╰", border),
            Span::styled("─".repeat(inner), border),
            Span::styled("╯", border),
        ]));
    }

    /// Greedy word-wrap styled segments to `width` display cells.
    fn wrap_segments(&self, segs: &[Seg], width: usize, base: Style) -> Vec<Line<'static>> {
        wrap_segments(segs, width, base)
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

fn wrap_segments(segs: &[Seg], width: usize, base: Style) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current: Vec<Span<'static>> = Vec::new();
    let mut current_w = 0usize;
    for seg in segs {
        let style = seg.style.patch(base);
        for ch in seg.text.chars() {
            if ch == '\n' {
                if !current.is_empty() {
                    lines.push(Line::from(std::mem::take(&mut current)));
                    current_w = 0;
                }
                continue;
            }
            let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
            if current_w + cw > width && !current.is_empty() {
                lines.push(Line::from(std::mem::take(&mut current)));
                current_w = 0;
            }
            push_span_char(&mut current, ch, style);
            current_w += cw;
        }
    }
    if !current.is_empty() {
        lines.push(Line::from(current));
    }
    if lines.is_empty() {
        lines.push(Line::default());
    }
    lines
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
