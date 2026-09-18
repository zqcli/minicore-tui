//! Tool execution surfaces. Model tools always use the green/blue/error
//! `toolExecution` state colors; the user `!bash` surface is intentionally
//! not inferred from a tool name and is not part of this renderer.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::markdown::column_width;
use crate::protocol::{ToolDisplayWire, ToolOutcomeWire};
use crate::state::tool::{LiveTool, ToolStatus};
use crate::state::transcript::ToolBlock;
use crate::theme::Theme;
use crate::ui::rail::{self, ToolSurfaceState};

/// A durable tool card. The optional display is the Agent's bounded
/// whitelist presentation; without it, history remains safe and falls back
/// to the tool name.
pub fn durable(
    theme: &Theme,
    block: &ToolBlock,
    width: usize,
    all_expanded: bool,
) -> Vec<Line<'static>> {
    durable_with_display(theme, block, width, all_expanded, None)
}

pub fn durable_with_display(
    theme: &Theme,
    block: &ToolBlock,
    width: usize,
    all_expanded: bool,
    display: Option<&ToolDisplayWire>,
) -> Vec<Line<'static>> {
    let state = durable_state(block);
    let colors = rail::tool_colors(theme, state);
    let expanded = block.expanded || all_expanded;
    let detail = display
        .map(|display| display.detail.as_str())
        .filter(|detail| !detail.is_empty())
        .unwrap_or(&block.name);
    let hidden = display
        .and_then(|display| display.hidden_line_count)
        .unwrap_or_else(|| {
            wrapped_result_row_count(
                block.result.as_deref().unwrap_or_default(),
                width.saturating_sub(rail::SURFACE_CONTENT_START + 2).max(1),
            )
        });
    let summary = durable_summary(block);
    let mut out = vec![Line::default()];
    if expanded {
        expanded_rows(
            theme,
            width,
            colors,
            &block.name,
            detail,
            display,
            summary.as_deref(),
            block.result.as_deref(),
            &mut out,
        );
    } else {
        out.extend(simple_rows(
            theme,
            width,
            colors,
            &block.name,
            detail,
            hidden,
            summary.as_deref(),
        ));
    }
    out.push(Line::default());
    out
}

/// A live tool card. Its identity and display are supplied by the App event
/// reducer; the renderer never guesses `bashExecution` from `tool.name`.
pub fn live(theme: &Theme, tool: &LiveTool, width: usize) -> Vec<Line<'static>> {
    live_with_display(theme, tool, width, None)
}

pub fn live_with_display(
    theme: &Theme,
    tool: &LiveTool,
    width: usize,
    display: Option<&ToolDisplayWire>,
) -> Vec<Line<'static>> {
    let state = match tool.status {
        ToolStatus::Pending | ToolStatus::Running => ToolSurfaceState::Pending,
        ToolStatus::Succeeded => ToolSurfaceState::Success,
        ToolStatus::Failed | ToolStatus::Denied => ToolSurfaceState::Error,
        // Spec 4.2/6.4: a cancelled call uses its own surface even though the
        // fixed Rail renderer has no distinct cancelled scene.
        ToolStatus::Cancelled => ToolSurfaceState::Cancelled,
    };
    let colors = rail::tool_colors(theme, state);
    let detail = display
        .map(|display| display.detail.as_str())
        .filter(|detail| !detail.is_empty())
        .unwrap_or(tool.name.as_str());
    let result = tool.result.as_deref();
    let hidden = display
        .and_then(|display| display.hidden_line_count)
        .unwrap_or_else(|| {
            wrapped_result_row_count(
                result.unwrap_or_default(),
                width.saturating_sub(rail::SURFACE_CONTENT_START + 2).max(1),
            )
        });
    let summary = live_summary(tool);
    let mut out = vec![Line::default()];
    if tool.expanded {
        expanded_rows(
            theme,
            width,
            colors,
            &tool.name,
            detail,
            display,
            summary.as_deref(),
            result,
            &mut out,
        );
    } else {
        out.extend(simple_rows(
            theme,
            width,
            colors,
            &tool.name,
            detail,
            hidden,
            summary.as_deref(),
        ));
    }
    out.push(Line::default());
    out
}

/// Returns the Rail default for a tool when no manual fold override exists.
/// `write` is always compact; every other tool uses the source estimator's
/// 20-row boundary. The caller supplies the Agent's bounded hidden-row count.
pub fn default_expanded(name: &str, hidden_line_count: Option<usize>) -> bool {
    if name == "write" {
        return false;
    }
    hidden_line_count.is_none_or(|hidden| hidden < 20)
}

#[allow(clippy::too_many_arguments)]
fn expanded_rows(
    theme: &Theme,
    width: usize,
    colors: rail::SurfaceColors,
    name: &str,
    detail: &str,
    display: Option<&ToolDisplayWire>,
    summary: Option<&str>,
    result: Option<&str>,
    out: &mut Vec<Line<'static>>,
) {
    out.push(rail::surface_row(
        width,
        colors,
        rail::SURFACE_CONTENT_START,
        Line::from(Span::styled(
            rail::collapsed_simple_line(name),
            Style::new()
                .fg(theme.tool_title)
                .add_modifier(Modifier::BOLD),
        )),
    ));
    let detail = rail::collapsed_simple_line(detail);
    out.push(rail::surface_row(
        width,
        colors,
        rail::SURFACE_CONTENT_START,
        Line::from(Span::styled(detail, Style::new().fg(theme.tool_output))),
    ));
    if let Some(input) = display.and_then(|display| display.expanded_input.as_deref()) {
        for line in input.split('\n') {
            push_wrapped_row(theme, width, colors, line, "  ", out);
        }
    }
    if let Some(result) = result.filter(|result| !result.is_empty()) {
        for line in result.split('\n') {
            push_wrapped_row(theme, width, colors, line, "  ", out);
        }
    } else if let Some(summary) = summary {
        push_wrapped_row(theme, width, colors, summary, "  ", out);
    }
}

fn simple_rows(
    theme: &Theme,
    width: usize,
    colors: rail::SurfaceColors,
    title: &str,
    detail: &str,
    hidden: usize,
    summary: Option<&str>,
) -> Vec<Line<'static>> {
    let title = rail::collapsed_simple_line(title);
    let detail = match summary {
        Some(summary) => format!(
            "{} · {}",
            rail::collapsed_simple_line(summary),
            rail::collapsed_simple_line(detail),
        ),
        None => rail::collapsed_simple_line(detail),
    };
    let mut hint = vec![Span::styled(
        format!("... ({hidden} more lines, "),
        Style::new().fg(theme.tool_muted),
    )];
    hint.push(Span::styled("ctrl+o", Style::new().fg(theme.dim)));
    hint.push(Span::styled(
        " to expand)",
        Style::new().fg(theme.tool_muted),
    ));
    [
        Line::from(Span::styled(
            title,
            Style::new()
                .fg(theme.tool_title)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(detail, Style::new().fg(theme.tool_output))),
        Line::from(hint),
    ]
    .into_iter()
    .map(|line| rail::surface_row(width, colors, rail::SURFACE_CONTENT_START, line))
    .collect()
}

fn durable_summary(block: &ToolBlock) -> Option<String> {
    if let Some(status) = block.live_status {
        return status_summary(status, block.result.as_deref());
    }
    outcome_summary(block.outcome, block.result.as_deref())
}

fn live_summary(tool: &LiveTool) -> Option<String> {
    status_summary(tool.status, tool.result.as_deref())
}

fn outcome_summary(outcome: Option<ToolOutcomeWire>, result: Option<&str>) -> Option<String> {
    match outcome {
        Some(ToolOutcomeWire::Failed) => Some(failure_summary("failed", result)),
        Some(ToolOutcomeWire::Denied) => Some(failure_summary("denied", result)),
        Some(ToolOutcomeWire::Cancelled) => Some(cancelled_summary()),
        Some(ToolOutcomeWire::Unknown) => Some("outcome unknown: unconfirmed".to_owned()),
        Some(ToolOutcomeWire::Success | ToolOutcomeWire::InputProvided) | None => None,
    }
}

fn status_summary(status: ToolStatus, result: Option<&str>) -> Option<String> {
    match status {
        ToolStatus::Failed => Some(failure_summary("failed", result)),
        ToolStatus::Denied => Some(failure_summary("denied", result)),
        ToolStatus::Cancelled => Some(cancelled_summary()),
        ToolStatus::Pending | ToolStatus::Running | ToolStatus::Succeeded => None,
    }
}

fn failure_summary(label: &str, result: Option<&str>) -> String {
    format!(
        "{label}: {}",
        result_summary(result).unwrap_or_else(|| "unknown".to_owned())
    )
}

fn cancelled_summary() -> String {
    "cancelled".to_owned()
}

fn result_summary(result: Option<&str>) -> Option<String> {
    let line = result?.lines().find(|line| !line.trim().is_empty())?;
    let sample: String = line.chars().take(160).collect();
    let collapsed = rail::collapsed_simple_line(&sample);
    let clipped = rail::clip_cells(&collapsed, 120);
    (!clipped.is_empty()).then_some(clipped)
}

fn durable_state(block: &ToolBlock) -> ToolSurfaceState {
    if let Some(status) = block.live_status {
        return status_surface(status);
    }
    match block.outcome {
        Some(ToolOutcomeWire::Success | ToolOutcomeWire::InputProvided) => {
            ToolSurfaceState::Success
        }
        Some(ToolOutcomeWire::Cancelled) => ToolSurfaceState::Cancelled,
        Some(ToolOutcomeWire::Failed | ToolOutcomeWire::Denied | ToolOutcomeWire::Unknown) => {
            ToolSurfaceState::Error
        }
        None => ToolSurfaceState::Pending,
    }
}

fn status_surface(status: ToolStatus) -> ToolSurfaceState {
    match status {
        ToolStatus::Pending | ToolStatus::Running => ToolSurfaceState::Pending,
        ToolStatus::Succeeded => ToolSurfaceState::Success,
        ToolStatus::Failed | ToolStatus::Denied => ToolSurfaceState::Error,
        ToolStatus::Cancelled => ToolSurfaceState::Cancelled,
    }
}

/// Raw source-line count; used where no content width is available.
pub fn result_line_count(result: Option<&str>) -> usize {
    result.map_or(0, |result| {
        if result.is_empty() {
            0
        } else {
            result.split('\n').count()
        }
    })
}

/// Grapheme-safe row count of `text` wrapped at `content_width` cells. Used
/// as the fallback collapsed hidden count so it matches the visual rows an
/// expanded card exposes (a 300-char single line counts more than 1).
pub fn wrapped_result_row_count(text: &str, content_width: usize) -> usize {
    if text.is_empty() {
        return 0;
    }
    let content_width = content_width.max(1);
    let mut rows = 0usize;
    for line in text.split('\n') {
        if line.is_empty() {
            rows += 1;
            continue;
        }
        let mut width = 0usize;
        for grapheme in line.graphemes(true) {
            let gw = UnicodeWidthStr::width(grapheme);
            if width + gw > content_width && width > 0 {
                rows += 1;
                width = 0;
            }
            width += gw;
        }
        rows += 1;
    }
    rows
}

fn push_wrapped_row(
    theme: &Theme,
    width: usize,
    colors: rail::SurfaceColors,
    line: &str,
    indent: &str,
    out: &mut Vec<Line<'static>>,
) {
    let line = crate::safe_text::safe_display(line);
    let content_width = width
        .saturating_sub(rail::SURFACE_CONTENT_START)
        .saturating_sub(UnicodeWidthStr::width(indent));
    let content_width = content_width.max(1);
    let mut current = String::new();
    let mut current_w = 0usize;
    let flush = |out: &mut Vec<Line<'static>>, current: &mut String| {
        if current.is_empty() {
            return;
        }
        out.push(rail::surface_row(
            width,
            colors,
            rail::SURFACE_CONTENT_START,
            Line::from(Span::styled(
                format!("{indent}{current}"),
                Style::new().fg(theme.tool_output),
            )),
        ));
        current.clear();
    };
    for grapheme in line.graphemes(true) {
        let gw = UnicodeWidthStr::width(grapheme);
        if current_w + gw > content_width && current_w > 0 {
            flush(out, &mut current);
            current_w = 0;
        }
        current.push_str(grapheme);
        current_w += gw;
    }
    flush(out, &mut current);
}

#[allow(dead_code)]
fn _display_width(text: &str) -> usize {
    column_width(text)
}

#[cfg(test)]
mod tests {
    use super::{default_expanded, durable, wrapped_result_row_count};
    use crate::protocol::ToolOutcomeWire;
    use crate::state::transcript::ToolBlock;
    use crate::theme::Theme;
    use unicode_segmentation::UnicodeSegmentation;

    #[test]
    fn default_expansion_only_forces_write_and_uses_generic_boundary() {
        assert!(default_expanded("read", Some(1)));
        assert!(!default_expanded("write", Some(1)));
        assert!(default_expanded("apply_patch", Some(1)));
        assert!(default_expanded("bash", None));
        assert!(default_expanded("edit", Some(19)));
        assert!(default_expanded("custom_tool", Some(19)));
        assert!(!default_expanded("custom_tool", Some(20)));
        assert!(!default_expanded("custom_tool", Some(21)));
    }

    #[test]
    fn wrapped_hidden_count_reflects_visual_rows_and_survives_graphemes() {
        assert_eq!(wrapped_result_row_count("x".repeat(300).as_str(), 60), 5);
        assert_eq!(wrapped_result_row_count("a\nb\n", 60), 3);
        // ZWJ family is a single grapheme/cluster and must never be counted
        // as split by inner emoji widths.
        let family = "👨‍👩‍👧";
        assert_eq!(family.graphemes(true).count(), 1);
        assert_eq!(wrapped_result_row_count(family, 2), 1);
        assert_eq!(wrapped_result_row_count("中文🚀emoji", 6), 2);
    }

    #[test]
    fn expanded_durable_rows_expose_the_full_payload_without_omission() {
        let payload = format!(
            "{}{}\n{}\n",
            "alpha ".repeat(50),
            "中文内容😀emoji",
            "x".repeat(300),
        );
        let block = ToolBlock {
            index: None,
            loop_id: "t".to_owned(),
            request_index: 0,
            tool_call_id: "c".to_owned(),
            name: "read".to_owned(),
            result: Some(payload.clone()),
            outcome: Some(ToolOutcomeWire::Success),
            live_status: None,
            progress: None,
            expanded: true,
        };
        let theme = Theme::dark();
        let lines = durable(&theme, &block, 80, false);
        // Reconstruct only the content cells (rail glyph, indent, and the
        // width padding are layout, not payload).
        let mut rendered = String::new();
        for line in lines {
            for span in &line.spans {
                if span.style.fg == Some(theme.tool_output) {
                    let content = span.content.as_ref();
                    rendered.push_str(content.strip_prefix("  ").unwrap_or(content));
                }
            }
        }
        // The durable card prepends its title/detail row, and rendered rows
        // drop newline control characters; every payload character must still
        // be present in order as a suffix of the rendered content.
        let payload_flat = payload.replace('\n', "");
        assert!(
            rendered.ends_with(&payload_flat),
            "expanded card must expose every character"
        );
        // A one-line payload wrapped across cards must still reproduce exactly.
        let last = payload.trim_end_matches('\n').rsplit('\n').next().unwrap();
        let block_long = ToolBlock {
            result: Some(last.to_owned()),
            ..block.clone()
        };
        let lines_long = durable(&theme, &block_long, 80, false);
        let mut rendered_long = String::new();
        for line in lines_long {
            for span in &line.spans {
                if span.style.fg == Some(theme.tool_output) {
                    let content = span.content.as_ref();
                    rendered_long.push_str(content.strip_prefix("  ").unwrap_or(content));
                }
            }
        }
        assert!(
            rendered_long.ends_with(last),
            "a single long wrapped line must reproduce exactly"
        );
    }
}
