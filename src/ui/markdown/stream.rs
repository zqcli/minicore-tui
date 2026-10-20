//! Streaming markdown: a renderer that caches every row before the last
//! boundary later text cannot change, so a delta re-parses only the tail.

use std::sync::Arc;

use ratatui::style::Style;
use ratatui::text::Line;

use super::render;
use crate::ui::rails::indent;
use crate::ui::theme::Styles;

/// Streaming renderer that caches everything before the last stable boundary.
pub struct StreamRenderer {
    text: String,
    width: usize,
    base: Style,
    /// Columns every row is moved right by, so the cached rows are final.
    inset: usize,
    scan: Scan,
    /// Final rows: one chunk per commit and the tail last, so a delta copies
    /// none of the committed rows.
    chunks: Vec<Arc<[Line<'static>]>>,
    /// Whether the last chunk is the tail, which every delta replaces.
    has_tail: bool,
    /// Whether `chunks` shows the current text.
    rendered: bool,
}

impl StreamRenderer {
    pub fn new(width: usize, base: Style) -> Self {
        Self {
            text: String::new(),
            width,
            base,
            inset: 0,
            scan: Scan::default(),
            chunks: Vec::new(),
            has_tail: false,
            rendered: false,
        }
    }

    pub fn inset(mut self, columns: usize) -> Self {
        self.inset = columns;
        self
    }

    /// Appends a delta. The scan and the committed rows survive, so only the
    /// tail is parsed again on the next render.
    pub fn push(&mut self, delta: &str) {
        if !delta.is_empty() {
            self.text.push_str(delta);
            self.rendered = false;
        }
    }

    /// Points the renderer at the block's authoritative text. While the text
    /// only grows the cached rows stay valid; any other text starts over.
    pub fn reset(&mut self, text: &str) {
        match text.strip_prefix(self.text.as_str()) {
            Some(delta) => self.push(delta),
            None => {
                *self = Self::new(self.width, self.base).inset(self.inset);
                self.text.push_str(text);
            }
        }
    }

    /// Bytes already rendered into the committed chunks; used by tests to prove
    /// a tail-only update does not re-parse the stable part.
    #[cfg(test)]
    pub fn stable_bytes(&self) -> usize {
        self.scan.cached
    }

    /// The rows of the current text, re-rendering only what changed.
    pub fn rows(&mut self, styles: &Styles) -> &[Arc<[Line<'static>]>] {
        if self.rendered {
            return &self.chunks;
        }
        if self.has_tail {
            self.chunks.pop();
            self.has_tail = false;
        }
        let stable = self.scan.advance(&self.text);
        if stable > self.scan.cached {
            let fresh = self.render_range(self.scan.cached..stable, styles);
            self.chunks.extend(fresh);
            self.scan.cached = stable;
        }
        if let Some(tail) = self.render_range(self.scan.cached.., styles) {
            self.chunks.push(tail);
            self.has_tail = true;
        }
        self.rendered = true;
        &self.chunks
    }

    /// One chunk for `range` of the text, behind a blank row unless it is the first.
    fn render_range(
        &self,
        range: impl std::slice::SliceIndex<str, Output = str>,
        styles: &Styles,
    ) -> Option<Arc<[Line<'static>]>> {
        let rows = render(&self.text[range], self.width, styles, self.base);
        if rows.is_empty() {
            return None;
        }
        let blank = (!self.chunks.is_empty()).then(Line::default);
        let rows = blank.into_iter().chain(rows);
        Some(match self.inset {
            0 => rows.collect(),
            inset => rows.map(|row| indent(row, inset)).collect(),
        })
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

    fn texts(renderer: &mut StreamRenderer, styles: &Styles) -> Vec<String> {
        renderer
            .rows(styles)
            .iter()
            .flat_map(|chunk| chunk.iter())
            .map(Line::to_string)
            .collect()
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
                let got = texts(&mut renderer, &styles);
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
            let got = texts(&mut renderer, &styles);
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
            let got = texts(&mut renderer, &styles);
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
        let lines = render(text, 40, &styles(), Style::default());
        assert!(
            lines.iter().any(|line| line.to_string().contains("after")),
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
        let first = renderer.rows(&styles).to_vec();
        let committed = renderer.stable_bytes();
        assert!(committed > 0, "the closed paragraph is cached");

        // Growing the tail must not re-render the stable part.
        renderer.push("grow");
        let second = renderer.rows(&styles).to_vec();
        assert_eq!(
            renderer.stable_bytes(),
            committed,
            "the stable cut did not move, so nothing before it is re-parsed"
        );
        assert!(
            Arc::ptr_eq(&second[0], &first[0]),
            "the committed rows are shared, not copied"
        );

        // An unchanged frame returns the very same allocations.
        let third = renderer.rows(&styles).to_vec();
        assert!(
            Arc::ptr_eq(&second[1], &third[1]),
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
            streamed.rows(&styles);
        }
        let mut whole = StreamRenderer::new(24, Style::default());
        whole.push(text);
        assert_eq!(texts(&mut streamed, &styles), texts(&mut whole, &styles));
    }

    #[test]
    fn trailing_newline_does_not_duplicate_blank_lines() {
        let styles = styles();
        let mut renderer = StreamRenderer::new(40, Style::default());
        renderer.push("first\n\n");
        assert_eq!(texts(&mut renderer, &styles).len(), 1);
        renderer.push("\n");
        assert_eq!(
            texts(&mut renderer, &styles).len(),
            1,
            "trailing blanks add no rows"
        );
        renderer.push("second\n\n");
        assert_eq!(texts(&mut renderer, &styles), ["first", "", "second"]);
    }

    #[test]
    fn text_that_is_not_an_extension_starts_the_scan_over() {
        let styles = styles();
        let mut renderer = StreamRenderer::new(40, Style::default());
        renderer.reset("one\n\ntwo is long enough to commit\n\nthree");
        renderer.rows(&styles);
        renderer.reset("beta");
        assert_eq!(texts(&mut renderer, &styles), ["beta"]);
    }

    #[test]
    fn the_inset_moves_every_row_including_the_blank_separators() {
        let styles = styles();
        let mut renderer = StreamRenderer::new(40, Style::default()).inset(2);
        renderer.reset("one\n\ntwo");
        assert_eq!(texts(&mut renderer, &styles), ["  one", "  ", "  two"]);
    }
}
