//! Block rendering: one function per block kind, following the rail style of
//! design §5.5 and §7.2. The layout cache calls these once per change and
//! stores the returned lines, so a block never re-renders per frame.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use super::markdown;
use super::rails;
use super::theme::Styles;
use super::wrap;
use crate::protocol::read::UserKind;
use crate::transcript::block::{NoticeLevel, TRUNCATED_MARKER, ToolState};
use crate::transcript::layout::Fold;
use crate::transcript::{Block, ToolCall};

/// Columns the assistant's inset costs, including the rail.
pub const INSET: usize = 2;

/// The end of an expanded card's hint row. Clicking that row opens the tool
/// view.
pub const FULL_OUTPUT_HINT: &str = "click for full output";

/// Renders one block at `width`. Always returns at least one line so a block
/// occupies a row even when it has no content of its own.
pub fn render(block: &Block, styles: Styles, width: u16, fold: Fold) -> Vec<Line<'static>> {
    if (width as usize) < MIN_WIDTH {
        return vec![Line::styled(
            wrap::truncate(&wrap::sanitize(&plain_text(block)), width as usize),
            styles.dim(),
        )];
    }
    let width = width as usize;
    match block {
        Block::User { text, kind } => user(text, *kind, styles, width),
        Block::Assistant {
            reasoning,
            text,
            streaming,
            ..
        } => assistant(reasoning, text, *streaming, styles, width, fold),
        Block::Tool(call) => tool(call, styles, width, call.is_expanded(fold.expanded_tools)),
        Block::TurnEnd {
            outcome,
            duration,
            requests,
            tool_rounds,
            changes,
            ..
        } => vec![turn_end(
            outcome,
            *duration,
            *requests,
            *tool_rounds,
            changes.as_ref(),
            styles,
            width,
        )],
        Block::Compaction {
            operation_id: _,
            before,
            after,
            coverage,
            summary,
        } => compaction(
            *before,
            *after,
            *coverage,
            summary.as_deref(),
            styles,
            width,
            fold,
        ),
        Block::Notice { level, text } => {
            let style = match level {
                NoticeLevel::Error => styles.error(),
                NoticeLevel::Info => styles.dim(),
            };
            wrap::wrap(text, width)
                .into_iter()
                .map(|line| Line::styled(line, style))
                .collect()
        }
        Block::EarlierHistory { hidden_items } => vec![Line::styled(
            format!("↑ {hidden_items} earlier items · home to load"),
            styles.subtle(),
        )],
    }
}

/// The text of a block without styles, for degenerate widths and `/copy`.
pub fn plain_text(block: &Block) -> String {
    match block {
        Block::User { text, .. } | Block::Assistant { text, .. } => text.clone(),
        Block::Tool(call) => format!("{} {}", call.name, call.header_detail()),
        Block::Notice { text, .. } => text.clone(),
        Block::Compaction { summary, .. } => summary.clone().unwrap_or_default(),
        _ => String::new(),
    }
}

/// Widths below this cannot hold a rail, a card border and one glyph. Such a
/// frame is a degenerate terminal; the block then draws as clipped plain text.
pub const MIN_WIDTH: usize = 8;

fn user(text: &str, kind: UserKind, styles: Styles, width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if kind == UserKind::Steering {
        lines.push(Line::styled("↳ steering", styles.dim()));
    }
    let body = markdown::render(text, width.saturating_sub(INSET), &styles, styles.text());
    let mut body: Vec<Line<'static>> = body
        .into_iter()
        .map(|line| rails::user_rail(line, styles))
        .collect();
    if body.is_empty() {
        body.push(rails::user_rail(Line::raw(""), styles));
    }
    lines.extend(body);
    lines
}

fn assistant(
    reasoning: &str,
    text: &str,
    streaming: bool,
    styles: Styles,
    width: usize,
    fold: Fold,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let thinking = reasoning.trim_end();
    if !thinking.is_empty() {
        if fold.show_reasoning {
            for line in
                markdown::render(thinking, width.saturating_sub(INSET), &styles, styles.dim())
            {
                lines.push(rails::reasoning_rail(line, styles));
            }
        } else if streaming {
            // While a message streams, the live reasoning tail is the useful
            // part; the folded summary replaces it once the message ends.
            let tail: Vec<&str> = thinking.lines().rev().take(3).collect();
            let tail: Vec<&str> = tail.into_iter().rev().collect();
            for line in markdown::render(
                &tail.join("\n"),
                width.saturating_sub(INSET),
                &styles,
                styles.dim(),
            ) {
                lines.push(rails::reasoning_rail(line, styles));
            }
        } else {
            let rows = thinking.lines().count().max(1);
            let summary = Line::styled(
                format!("Thought · {rows} {}", plural(rows, "line")),
                styles.dim().add_modifier(ratatui::style::Modifier::ITALIC),
            );
            lines.push(rails::reasoning_rail(summary, styles));
        }
    }
    if !text.trim().is_empty() {
        for line in markdown::render(text, width.saturating_sub(INSET), &styles, styles.text()) {
            lines.push(rails::rail(line, styles));
        }
    }
    lines
}

fn tool(call: &ToolCall, styles: Styles, width: usize, expanded: bool) -> Vec<Line<'static>> {
    let (glyph, state_style, band) = match call.state {
        ToolState::Pending => ("·", styles.dim(), styles.theme.tool_bg),
        ToolState::Running => ("⠋", styles.accent(), styles.theme.tool_bg),
        ToolState::Succeeded => ("✓", styles.success(), styles.theme.tool_success_bg),
        ToolState::Failed => ("✗", styles.error(), styles.theme.tool_error_bg),
        ToolState::Denied | ToolState::Cancelled => {
            ("⊘", styles.warning(), styles.theme.tool_error_bg)
        }
    };
    let inner = width.saturating_sub(INSET);
    let (header, clipped) = tool_header(call, glyph, state_style, styles, inner);
    let row = |spans: Vec<Span<'static>>| card_row(spans, state_style, styles, band);
    let mut lines = vec![row(header)];
    let body_row = |line: &str, style: Style| {
        let text = wrap::truncate(&wrap::sanitize(line), inner.saturating_sub(2));
        row(vec![Span::styled(format!("  {text}"), style)])
    };
    if expanded
        && call.name == "bash"
        && let Some(command) = call.expanded_input.as_deref()
        && (clipped || command.trim_end().contains('\n'))
    {
        for line in command.lines().take(EXPANDED_ROWS) {
            lines.push(body_row(line, styles.dim()));
        }
    }
    let body = card_body(call, expanded);
    for line in &body.lines {
        let style = if !body.patch {
            styles.subtle()
        } else if is_patch_header(line) {
            styles.dim()
        } else {
            styles.patch(line)
        };
        lines.push(body_row(line, style));
    }
    if body.hidden > 0 || (expanded && call.truncated) {
        let mut hint = "… ".to_owned();
        if body.hidden > 0 {
            let which = if body.tail { "earlier" } else { "more" };
            let lines = plural(body.hidden, "line");
            hint.push_str(&format!("{} {which} {lines} · ", body.hidden));
        }
        hint.push_str(if expanded {
            FULL_OUTPUT_HINT
        } else {
            "ctrl+o to expand"
        });
        lines.push(body_row(&hint, styles.subtle()));
    }
    lines
}

/// Body rows of a folded card, by kind (design §5.5), and of an expanded one;
/// the tool view has the rest.
const FOLDED_PATCH_ROWS: usize = 8;
const FOLDED_WRITE_ROWS: usize = 6;
const FOLDED_OUTPUT_ROWS: usize = 4;
const FOLDED_SHORT_ROWS: usize = 3;
const EXPANDED_ROWS: usize = 200;

/// What a card shows under its header.
struct Body<'a> {
    lines: Vec<&'a str>,
    /// Lines left out: before the shown ones for a tail, after them for a head.
    hidden: usize,
    tail: bool,
    patch: bool,
}

/// `bash` shows its output tail; `edit`, `apply_patch` and `write` the head of
/// their input; `read`, failed and other tools the head of their result.
fn card_body(call: &ToolCall, expanded: bool) -> Body<'_> {
    let rows = |folded: usize| if expanded { EXPANDED_ROWS } else { folded };
    let failed = call.state == ToolState::Failed;
    if call.name == "bash" {
        let output = call.output_content();
        let shown = rows(FOLDED_OUTPUT_ROWS).min(output.len());
        return Body {
            hidden: call.output_dropped + output.len() - shown,
            lines: output[output.len() - shown..].to_vec(),
            tail: true,
            patch: false,
        };
    }
    // A failed change explains itself in its result, not in its input.
    if let Some(input) = call.expanded_input.as_deref()
        && !(failed && !call.output.is_empty())
    {
        let write = call.name == "write";
        let all: Vec<&str> = input.lines().collect();
        // Folded patches skip their file and hunk headers: the header already
        // names the files, so the rows go to the change itself.
        let source: Vec<&str> = all
            .iter()
            .copied()
            .filter(|line| write || !is_noise(line, expanded))
            .collect();
        let folded = if write {
            FOLDED_WRITE_ROWS
        } else {
            FOLDED_PATCH_ROWS
        };
        let shown = rows(folded).min(source.len());
        return Body {
            hidden: source.len() - shown + call.hidden_input_lines(),
            lines: source[..shown].to_vec(),
            tail: false,
            patch: !write,
        };
    }
    let output = call.output_content();
    let folded = if failed || call.name == "read" {
        FOLDED_SHORT_ROWS
    } else {
        FOLDED_OUTPUT_ROWS
    };
    let shown = rows(folded).min(output.len());
    // The Agent's count includes the cut marker the card leaves out.
    let markers = call
        .output
        .iter()
        .filter(|line| *line == TRUNCATED_MARKER)
        .count();
    let total = call
        .output_lines
        .map_or(0, |total| total.saturating_sub(markers))
        .max(output.len());
    Body {
        hidden: total - shown,
        lines: output[..shown].to_vec(),
        tail: false,
        patch: false,
    }
}

/// A diff's file and hunk header line.
fn is_patch_header(line: &str) -> bool {
    ["--- ", "+++ ", "diff --git", "index ", "@@", "*** "]
        .iter()
        .any(|prefix| line.starts_with(prefix))
}

/// Patch lines a card leaves out. The envelope and a bare `@@` say nothing;
/// headers with ranges and paths only earn a row when the card is expanded.
fn is_noise(line: &str, expanded: bool) -> bool {
    let line = line.trim_end();
    if line == "*** Begin Patch" || line == "*** End Patch" || line == "@@" {
        return true;
    }
    !expanded
        && ["--- ", "+++ ", "diff --git", "index ", "@@"]
            .iter()
            .any(|prefix| line.starts_with(prefix))
}

/// The header row: status glyph, tool name and detail on the left, the meta
/// (duration, exit code, line counts, truncation) right-aligned. Returns the
/// spans and whether the left side had to be cut.
fn tool_header(
    call: &ToolCall,
    glyph: &str,
    state_style: Style,
    styles: Styles,
    width: usize,
) -> (Vec<Span<'static>>, bool) {
    let mut meta: Vec<Vec<Span<'static>>> = Vec::new();
    if let Some(duration) = call.duration {
        meta.push(vec![Span::styled(
            crate::app::format_duration(duration),
            styles.dim(),
        )]);
    }
    if let Some(code) = call.exit_code {
        let style = if code == 0 {
            styles.dim()
        } else {
            styles.error()
        };
        meta.push(vec![Span::styled(format!("exit {code}"), style)]);
    }
    if let Some((added, removed)) = call.line_changes()
        && added + removed > 0
    {
        meta.push(vec![
            Span::styled(format!("+{added}"), styles.diff_add()),
            Span::raw(" "),
            Span::styled(format!("−{removed}"), styles.diff_del()),
        ]);
    }
    if call.truncated {
        meta.push(vec![Span::styled("truncated", styles.dim())]);
    }
    let mut right: Vec<Span<'static>> = Vec::new();
    for (index, item) in meta.into_iter().enumerate() {
        if index > 0 {
            right.push(Span::styled(" · ", styles.dim()));
        }
        right.extend(item);
    }
    let right_width: usize = right.iter().map(|span| wrap::width(&span.content)).sum();
    if right_width + 2 >= width {
        right.clear();
    }
    let room = if right.is_empty() {
        width
    } else {
        width - right_width - 1
    };
    let title = format!("{glyph} {}", call.name);
    let detail = call.header_detail();
    let detail = wrap::sanitize(&detail);
    let full = if detail.is_empty() {
        title.clone()
    } else {
        format!("{title}  {detail}")
    };
    let left = wrap::truncate(&full, room);
    let clipped = left != full;
    let split = if left.starts_with(&title) {
        title.len()
    } else {
        left.len()
    };
    let mut spans = vec![
        Span::styled(left[..split].to_owned(), state_style),
        Span::styled(left[split..].to_owned(), styles.text()),
    ];
    let padding = width.saturating_sub(wrap::width(&left) + right_width);
    if !right.is_empty() {
        spans.push(Span::raw(" ".repeat(padding)));
        spans.extend(right);
    }
    (spans, clipped)
}

/// One card row: the rail in the state's color, then the spans, all on the
/// card's background band.
fn card_row(spans: Vec<Span<'static>>, rail: Style, styles: Styles, band: Color) -> Line<'static> {
    let paint = |style: Style| if styles.color { style.bg(band) } else { style };
    let mut row = vec![Span::styled("▎ ", paint(rail))];
    row.extend(spans.into_iter().map(|span| {
        let style = paint(span.style);
        span.style(style)
    }));
    Line {
        style: paint(Style::default()),
        spans: row,
        alignment: None,
    }
}

fn plural(count: usize, word: &str) -> String {
    if count == 1 {
        word.to_owned()
    } else {
        format!("{word}s")
    }
}

fn turn_end(
    outcome: &crate::protocol::LoopOutcome,
    duration: Option<std::time::Duration>,
    requests: u32,
    tool_rounds: u64,
    changes: Option<&crate::transcript::ChangeSummary>,
    styles: Styles,
    width: usize,
) -> Line<'static> {
    let text = crate::app::turn_end_line(outcome, duration, requests, tool_rounds, changes);
    let labeled = format!("── {text} ──");
    let style = if outcome.is_completed() {
        styles.dim()
    } else {
        styles.warning()
    };
    Line::styled(wrap::truncate(&labeled, width), style)
}

fn compaction(
    before: Option<u64>,
    after: Option<u64>,
    coverage: Option<(Option<usize>, Option<usize>)>,
    summary: Option<&str>,
    styles: Styles,
    width: usize,
    fold: Fold,
) -> Vec<Line<'static>> {
    let text = match (before, after) {
        (Some(before), Some(after)) => format!(
            "─ Context compacted · {} → {} tokens ─",
            crate::ui::tokens(before),
            crate::ui::tokens(after)
        ),
        // An automatic compaction usually reports no token figures; the item
        // counts say what it did instead of leaving a bare marker.
        _ => match coverage {
            Some((covered, kept)) => {
                let items = covered.map(|n| format!("{n} items"));
                let kept = kept.map(|n| format!("{n} kept"));
                let parts: Vec<String> = items.into_iter().chain(kept).collect();
                format!("─ Context compacted · {} ─", parts.join(" · "))
            }
            None => "─ Context compacted ─".to_owned(),
        },
    };
    let mut lines = vec![Line::styled(wrap::truncate(&text, width), styles.dim())];
    if fold.show_reasoning
        && let Some(summary) = summary
    {
        for line in markdown::render(summary, width.saturating_sub(INSET), &styles, styles.dim()) {
            lines.push(rails::rail(line, styles));
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::read::UserKind;
    use crate::protocol::{LoopOutcome, ToolRef};
    use crate::theme::{Palette, ThemeKind};
    use crate::transcript::{ChangeSummary, RequestKey};
    use std::time::Duration;

    fn styles() -> Styles {
        Styles::new(Palette::for_kind(ThemeKind::Dark), false)
    }

    fn fold() -> Fold {
        Fold {
            expanded_tools: false,
            show_reasoning: false,
        }
    }

    fn text(lines: &[Line<'static>]) -> Vec<String> {
        lines.iter().map(Line::to_string).collect()
    }

    fn assistant(text: &str, reasoning: &str, streaming: bool) -> Block {
        Block::Assistant {
            key: RequestKey {
                loop_id: "l".into(),
                request_index: 0,
            },
            reasoning: reasoning.into(),
            text: text.into(),
            streaming,
        }
    }

    #[test]
    fn a_user_block_is_marked_by_its_rail() {
        let block = Block::User {
            text: "hello world".into(),
            kind: UserKind::Prompt,
        };
        let body = text(&render(&block, styles(), 40, fold()));
        assert!(body[0].starts_with("▎ hello world"), "{body:?}");
    }

    #[test]
    fn steering_blocks_are_labelled() {
        let block = Block::User {
            text: "focus on tests".into(),
            kind: UserKind::Steering,
        };
        let body = text(&render(&block, styles(), 40, fold()));
        assert!(body[0].contains("steering"));
    }

    #[test]
    fn assistant_prose_is_markdown_not_raw_text() {
        let block = assistant("## Title\n\nsome **bold** prose", "", false);
        let body = text(&render(&block, styles(), 60, fold()));
        assert!(body.iter().any(|line| line.contains("Title")));
        assert!(body.iter().any(|line| line.contains("bold")));
        assert!(
            !body.iter().any(|line| line.contains("**")),
            "markdown markers are not shown: {body:?}"
        );
    }

    #[test]
    fn finished_reasoning_collapses_and_ctrl_t_shows_it() {
        let block = assistant("the answer", "one\ntwo\nthree", false);
        let folded = text(&render(&block, styles(), 60, fold()));
        assert_eq!(folded[0], "▏ Thought · 3 lines", "{folded:?}");
        assert!(folded.iter().any(|line| line.contains("the answer")));

        let shown = text(&render(
            &block,
            styles(),
            60,
            Fold {
                expanded_tools: false,
                show_reasoning: true,
            },
        ));
        assert!(shown.iter().any(|line| line.contains("three")));
        assert!(
            shown[0].starts_with('▏'),
            "reasoning rows carry the reasoning rail: {shown:?}"
        );
    }

    #[test]
    fn streaming_reasoning_shows_only_the_tail() {
        let block = assistant("", "one\ntwo\nthree\nfour\nfive", true);
        let lines = text(&render(&block, styles(), 60, fold()));
        assert!(!lines.iter().any(|line| line.contains("one")));
        assert!(lines.iter().any(|line| line.contains("five")));
    }

    fn call() -> ToolCall {
        let mut call = ToolCall::new(
            ToolRef {
                session_id: "s".into(),
                loop_id: "l".into(),
                request_index: 0,
                tool_call_id: "c".into(),
            },
            "bash",
        );
        call.detail = "$ python3 -m unittest".into();
        call.state = ToolState::Succeeded;
        call.duration = Some(Duration::from_millis(400));
        call.exit_code = Some(0);
        call.output = vec!["Ran 30 tests".into(), "OK".into()];
        call.output_dropped = 12;
        call
    }

    fn expanded() -> Fold {
        Fold {
            expanded_tools: true,
            show_reasoning: false,
        }
    }

    #[test]
    fn a_tool_card_reports_status_duration_and_exit_code() {
        let block = Block::Tool(Box::new(call()));
        let body = text(&render(&block, styles(), 60, fold()));
        assert!(body[0].contains("✓ bash"));
        assert!(body[0].contains("0.4s"));
        assert!(body[0].contains("exit 0"));
        assert_eq!(
            body.last().unwrap(),
            "▎   … 12 earlier lines · ctrl+o to expand"
        );
    }

    #[test]
    fn a_bash_card_shows_the_output_tail_not_the_command() {
        let mut card = call();
        card.detail = "$ make test".into();
        card.expanded_input = Some("make test".into());
        card.output = (1..=6).map(|index| format!("out {index}")).collect();
        card.output_dropped = 0;
        let body = text(&render(&Block::Tool(Box::new(card)), styles(), 60, fold()));
        assert_eq!(
            body[1..],
            [
                "▎   out 3",
                "▎   out 4",
                "▎   out 5",
                "▎   out 6",
                "▎   … 2 earlier lines · ctrl+o to expand",
            ]
        );
    }

    #[test]
    fn an_expanded_bash_card_shows_a_long_command_above_its_output() {
        let mut card = call();
        card.expanded_input = Some("cd app\npython3 -m unittest".into());
        let body = text(&render(
            &Block::Tool(Box::new(card)),
            styles(),
            60,
            expanded(),
        ));
        assert_eq!(body[1], "▎   cd app");
        assert_eq!(body[2], "▎   python3 -m unittest");
        assert_eq!(body[3], "▎   Ran 30 tests");
        assert!(body.last().unwrap().ends_with(FULL_OUTPUT_HINT), "{body:?}");

        // A one-line command that fits the header is not repeated.
        let mut short = call();
        short.expanded_input = Some("python3 -m unittest".into());
        let body = text(&render(
            &Block::Tool(Box::new(short)),
            styles(),
            60,
            expanded(),
        ));
        assert_eq!(body[1], "▎   Ran 30 tests");
    }

    #[test]
    fn a_non_zero_exit_code_is_red() {
        let mut card = call();
        card.exit_code = Some(1);
        let lines = render(&Block::Tool(Box::new(card)), styles(), 60, fold());
        let exit = lines[0]
            .spans
            .iter()
            .find(|span| span.content == "exit 1")
            .expect("the exit code is in the header");
        assert_eq!(exit.style.fg, Some(styles().theme.error));
    }

    fn patch_card() -> ToolCall {
        let mut card = call();
        card.name = "apply_patch".into();
        card.detail = "tool apply_patch".into();
        card.exit_code = None;
        card.output = vec!["Applied".into()];
        card.expanded_input = Some(
            "diff --git a/app.py b/app.py\nindex 1..2 100644\n--- a/app.py\n+++ b/app.py\n\
             @@ -1,2 +1,3 @@\n def greet(name):\n+    \"\"\"Greet.\"\"\"\n     return name"
                .into(),
        );
        card
    }

    #[test]
    fn a_folded_patch_skips_its_headers_and_counts_what_is_left() {
        let body = text(&render(
            &Block::Tool(Box::new(patch_card())),
            styles(),
            60,
            fold(),
        ));
        assert!(body[0].starts_with("▎ ✓ apply_patch  app.py"), "{body:?}");
        assert!(body[0].ends_with("0.4s · +1 −0"), "{body:?}");
        assert_eq!(
            body[1..],
            [
                "▎    def greet(name):",
                "▎   +    \"\"\"Greet.\"\"\"",
                "▎        return name",
            ],
            "nothing is hidden, so there is no hint"
        );

        let expanded = text(&render(
            &Block::Tool(Box::new(patch_card())),
            styles(),
            60,
            expanded(),
        ));
        assert!(
            expanded.contains(&"▎   @@ -1,2 +1,3 @@".to_owned()),
            "{expanded:?}"
        );
    }

    #[test]
    fn a_long_patch_says_how_many_lines_follow() {
        let mut card = patch_card();
        let lines: Vec<String> = (0..12).map(|index| format!("+line {index}")).collect();
        card.expanded_input = Some(format!("@@\n{}", lines.join("\n")));
        let body = text(&render(&Block::Tool(Box::new(card)), styles(), 60, fold()));
        assert_eq!(body.len(), 1 + 8 + 1, "{body:?}");
        assert_eq!(body[1], "▎   +line 0", "a bare @@ takes no row");
        assert_eq!(body[9], "▎   … 4 more lines · ctrl+o to expand");
    }

    #[test]
    fn a_truncated_read_shows_its_first_lines_without_the_cut_marker() {
        let mut card = call();
        card.name = "read".into();
        card.detail = "large.txt:1-2000".into();
        card.duration = None;
        card.exit_code = None;
        card.output_dropped = 0;
        card.truncated = true;
        card.output_lines = Some(2001);
        card.output = Vec::new();
        // The 2,000 line result as previewed: cut inside line 5, then marked.
        card.apply_result("1: alpha\n2: beta\n3: gamma\n4: delta\n1\n[truncated]");
        let body = text(&render(&Block::Tool(Box::new(card)), styles(), 70, fold()));
        assert!(body[0].ends_with("truncated"), "{body:?}");
        assert_eq!(
            body[1..],
            [
                "▎   1: alpha",
                "▎   2: beta",
                "▎   3: gamma",
                "▎   … 1997 more lines · ctrl+o to expand",
            ]
        );
    }

    #[test]
    fn one_card_can_be_expanded_while_the_others_stay_folded() {
        let mut card = call();
        card.expanded = Some(true);
        let body = text(&render(&Block::Tool(Box::new(card)), styles(), 60, fold()));
        assert!(body.last().unwrap().ends_with(FULL_OUTPUT_HINT), "{body:?}");
    }

    #[test]
    fn a_folded_card_is_small_and_an_expanded_one_is_bounded() {
        let mut card = call();
        card.output = (0..400).map(|index| format!("line {index}")).collect();
        let block = Block::Tool(Box::new(card));
        let folded = render(&block, styles(), 60, fold());
        let expanded = render(
            &block,
            styles(),
            60,
            Fold {
                expanded_tools: true,
                show_reasoning: false,
            },
        );
        assert!(folded.len() <= 8, "folded cards stay small");
        assert!(expanded.len() > folded.len());
        assert!(expanded.len() <= 202, "expanded cards cap at 200 rows");
    }

    #[test]
    fn a_failed_card_uses_the_error_band() {
        let mut card = call();
        card.state = ToolState::Failed;
        card.output = vec!["Traceback (most recent call last)".into()];
        let lines = render(&Block::Tool(Box::new(card)), styles(), 60, fold());
        assert_eq!(lines[0].style.bg, Some(styles().theme.tool_error_bg));
    }

    #[test]
    fn tool_output_is_sanitized_before_it_reaches_the_terminal() {
        let mut card = call();
        card.output = vec!["\u{1b}[31mred\u{1b}[0m".into()];
        let body = text(&render(&Block::Tool(Box::new(card)), styles(), 60, fold()));
        assert!(body.iter().any(|line| line.contains("red")));
        assert!(!body.iter().any(|line| line.contains('\u{1b}')));
    }

    #[test]
    fn a_turn_end_line_names_the_outcome_and_the_change_size() {
        let block = Block::TurnEnd {
            loop_id: "l".into(),
            outcome: LoopOutcome::Completed,
            duration: Some(Duration::from_secs(187)),
            requests: 12,
            tool_rounds: 11,
            usage: Default::default(),
            changes: Some(ChangeSummary {
                files: 3,
                lines: Some((40, 7)),
            }),
        };
        let body = text(&render(&block, styles(), 100, fold()));
        let line = &body[0];
        assert!(line.contains("completed"), "{line}");
        assert!(line.contains("3m07s"), "{line}");
        assert!(line.contains("12 requests"), "{line}");
        assert!(line.contains("3 files changed (+40 −7)"), "{line}");
    }

    #[test]
    fn a_compaction_marker_shows_what_it_saved() {
        let block = Block::Compaction {
            operation_id: "tui-1".into(),
            before: Some(32_200),
            after: Some(2_700),
            coverage: None,
            summary: None,
        };
        let body = text(&render(&block, styles(), 80, fold()));
        assert!(body[0].contains("32.2k → 2.7k"), "{body:?}");
    }

    /// An automatic compaction reports no token figures, so the marker says what
    /// it covered instead of degenerating to a bare line.
    #[test]
    fn a_compaction_without_token_figures_reports_its_coverage() {
        let block = Block::Compaction {
            operation_id: "auto-lup_1".into(),
            before: None,
            after: None,
            coverage: Some((Some(177), Some(0))),
            summary: None,
        };
        let body = text(&render(&block, styles(), 80, fold()));
        assert!(body[0].contains("177 items"), "{body:?}");
        assert!(body[0].contains("0 kept"), "{body:?}");
    }

    /// An automatic result commonly reports what it covered but not what it
    /// kept; the count that exists must still be shown.
    #[test]
    fn a_compaction_with_only_a_covered_count_still_reports_it() {
        let block = Block::Compaction {
            operation_id: "auto-lup_2".into(),
            before: None,
            after: None,
            coverage: Some((Some(332), None)),
            summary: None,
        };
        let body = text(&render(&block, styles(), 80, fold()));
        assert!(body[0].contains("332 items"), "{body:?}");
        assert!(!body[0].contains("kept"), "{body:?}");
    }

    #[test]
    fn errors_are_red_and_archived_history_is_dim() {
        let error = Block::Notice {
            level: NoticeLevel::Error,
            text: "turn failed".into(),
        };
        let lines = render(&error, styles(), 40, fold());
        assert_eq!(lines[0].style.fg, Some(styles().theme.error));

        let history = Block::EarlierHistory { hidden_items: 40 };
        let body = text(&render(&history, styles(), 40, fold()));
        assert!(body[0].contains("40 earlier items"));
    }

    #[test]
    fn narrow_widths_never_overflow() {
        for block in [
            Block::User {
                text: "a very long word without any break opportunity at all".into(),
                kind: UserKind::Prompt,
            },
            assistant(
                "```rust\nfn main() { println!(\"wide code\"); }\n```",
                "",
                false,
            ),
            Block::Tool(Box::new(call())),
        ] {
            for width in 3..12 {
                for line in render(&block, styles(), width, fold()) {
                    assert!(
                        crate::ui::wrap::width(&line.to_string()) <= width as usize,
                        "{:?} at width {width}: {line:?}",
                        block.loop_id()
                    );
                }
            }
        }
    }
}
