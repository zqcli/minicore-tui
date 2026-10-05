//! Tool execution surfaces. Model tools always use the green/blue/error
//! `toolExecution` state colors; the user `!bash` surface is intentionally
//! not inferred from a tool name and is not part of this renderer.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::markdown::{CopyCells, column_width};
use crate::protocol::{ToolDisplayWire, ToolOutcomeWire};
use crate::state::tool::{LiveTool, ToolStatus};
use crate::state::transcript::ToolBlock;
use crate::theme::Theme;
use crate::ui::rail::{self, ToolSurfaceState};

/// Tool rows with explicit renderer-owned copy decorations.
pub struct RenderedTool {
    pub lines: Vec<Line<'static>>,
    pub copy_cells: Vec<Option<CopyCells>>,
}

/// Compatibility entry points for callers without structured execution facts.
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
    render_card(
        theme,
        &block.name,
        block.result.as_deref(),
        display,
        None,
        width,
        block.expanded || all_expanded,
        durable_state(block),
        durable_summary(block),
    )
    .lines
}

pub fn durable_with_facts(
    theme: &Theme,
    block: &ToolBlock,
    width: usize,
    all_expanded: bool,
    facts: Option<&crate::state::tool::ToolFacts>,
) -> Vec<Line<'static>> {
    durable_with_metadata(theme, block, width, all_expanded, facts).lines
}

pub fn durable_with_metadata(
    theme: &Theme,
    block: &ToolBlock,
    width: usize,
    all_expanded: bool,
    facts: Option<&crate::state::tool::ToolFacts>,
) -> RenderedTool {
    render_card(
        theme,
        &block.name,
        facts
            .and_then(|f| f.result.as_deref())
            .or(block.result.as_deref()),
        facts.map(|f| f.display.as_ref()),
        facts,
        width,
        block.expanded || all_expanded,
        durable_state(block),
        durable_summary(block),
    )
}

pub fn live(theme: &Theme, tool: &LiveTool, width: usize) -> Vec<Line<'static>> {
    live_with_display(theme, tool, width, tool.display.as_deref())
}

pub fn live_with_display(
    theme: &Theme,
    tool: &LiveTool,
    width: usize,
    display: Option<&ToolDisplayWire>,
) -> Vec<Line<'static>> {
    render_card(
        theme,
        &tool.name,
        tool.result.as_deref(),
        display,
        None,
        width,
        tool.expanded,
        status_surface(tool.status),
        live_summary(tool),
    )
    .lines
}

pub fn live_with_facts(
    theme: &Theme,
    tool: &LiveTool,
    width: usize,
    facts: Option<&crate::state::tool::ToolFacts>,
) -> Vec<Line<'static>> {
    live_with_metadata(theme, tool, width, facts).lines
}

pub fn live_with_metadata(
    theme: &Theme,
    tool: &LiveTool,
    width: usize,
    facts: Option<&crate::state::tool::ToolFacts>,
) -> RenderedTool {
    render_card(
        theme,
        &tool.name,
        facts
            .and_then(|f| f.result.as_deref())
            .or(tool.result.as_deref()),
        facts
            .map(|f| f.display.as_ref())
            .or(tool.display.as_deref()),
        facts,
        width,
        tool.expanded,
        status_surface(tool.status),
        live_summary(tool),
    )
}

/// Include only facts used by the card in the durable layout revision.
pub fn facts_revision(facts: &crate::state::tool::ToolFacts) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    std::mem::discriminant(&facts.status).hash(&mut hash);
    facts
        .outcome
        .as_ref()
        .map(std::mem::discriminant)
        .hash(&mut hash);
    facts.command.is_some().hash(&mut hash);
    if let Some(command) = &facts.command {
        std::mem::discriminant(&command.status).hash(&mut hash);
        command.exit_code.hash(&mut hash);
        command.signal.hash(&mut hash);
        (command.stderr_observed_end > 0).hash(&mut hash);
    }
    if let Some(invocation) = &facts.invocation {
        std::mem::discriminant(&invocation.subject).hash(&mut hash);
        match &invocation.subject {
            crate::protocol::ToolSubjectWire::File { path } => path.hash(&mut hash),
            crate::protocol::ToolSubjectWire::Command { script, .. } => script.hash(&mut hash),
            crate::protocol::ToolSubjectWire::Other => invocation.input.preview.hash(&mut hash),
        }
    }
    facts
        .process_output
        .as_ref()
        .map(|streams| [streams[0].revision, streams[1].revision])
        .hash(&mut hash);
    facts.process_count_partial().hash(&mut hash);
    facts.process_output_partial().hash(&mut hash);
    facts.input_line_count().hash(&mut hash);
    facts
        .input_text()
        .map(|(_, partial)| partial)
        .hash(&mut hash);
    facts.output_line_count.hash(&mut hash);
    facts.count_partial.hash(&mut hash);
    facts.display.body_truncated.hash(&mut hash);
    facts.display.input_line_count.hash(&mut hash);
    facts.stream_lines.iter().any(|s| s.gap).hash(&mut hash);
    facts
        .inline
        .as_ref()
        .and_then(|load| load.error.as_deref())
        .hash(&mut hash);
    facts
        .execution
        .as_ref()
        .map(|execution| std::mem::discriminant(&execution.input_availability))
        .hash(&mut hash);
    facts
        .result
        .as_ref()
        .map(|text| {
            (
                std::sync::Arc::as_ptr(text) as *const u8 as usize,
                text.len(),
            )
        })
        .hash(&mut hash);
    facts.result_truncated.hash(&mut hash);
    hash.finish()
}

#[allow(clippy::too_many_arguments)]
fn render_card(
    theme: &Theme,
    name: &str,
    result: Option<&str>,
    display: Option<&ToolDisplayWire>,
    facts: Option<&crate::state::tool::ToolFacts>,
    width: usize,
    expanded: bool,
    state: ToolSurfaceState,
    status: String,
) -> RenderedTool {
    let mut colors = rail::tool_colors(theme, state);
    let mut detail = target(name, display, facts);
    if matches!(name, "apply_patch" | "patch")
        && (detail == name || detail == format!("tool {name}"))
    {
        if let Some(path) = result
            .and_then(|r| r.strip_prefix("patched "))
            .and_then(|r| r.split_once(" bytes at "))
            .map(|(_, path)| path.trim())
        {
            detail = path.to_owned();
        }
    }
    let (process, warning) = command_summary(name, result, facts);
    let failed = warning || state == ToolSurfaceState::Error;
    if failed {
        colors.rail = theme.error;
        colors.background = theme.page_bg;
    }
    let status = process.map_or(status.clone(), |process| format!("{status} · {process}"));
    let title = if expanded {
        format!("{} · {status}", clip_summary(name, 18))
    } else {
        name.to_owned()
    };
    let mut out = vec![Line::default()];
    let available = width.saturating_sub(rail::SURFACE_CONTENT_START);
    for (text, color) in [
        (
            &title,
            if failed {
                theme.error
            } else {
                theme.tool_title
            },
        ),
        (&detail, theme.tool_output),
    ] {
        out.push(rail::surface_row(
            width,
            colors,
            rail::SURFACE_CONTENT_START,
            Line::from(Span::styled(
                if text == &detail {
                    if expanded {
                        clip_target(text, available)
                    } else {
                        clip_line(
                            &visible_tool_line(text.lines().next().unwrap_or_default()),
                            available,
                        )
                    }
                } else {
                    clip_summary(text, available)
                },
                Style::new().fg(color),
            )),
        ));
    }
    let input = facts.and_then(|f| f.input_text()).or_else(|| {
        display.and_then(|d| {
            d.expanded_input
                .as_deref()
                .map(|text| (text, d.body_truncated))
        })
    });
    let partial = display.is_some_and(|d| d.body_truncated || d.truncated)
        || input.is_some_and(|(_, partial)| partial)
        || facts
            .is_some_and(|f| f.result_truncated || f.count_partial || f.process_output_partial());
    let footer_row;
    if expanded {
        // A complete one-line Bash command is already present in its target
        // row. Keep the body when clipping or normalization would lose bytes.
        let body_input = input.map(|(text, _)| text).filter(|text| {
            name != "bash"
                || text.contains('\n')
                || visible_tool_line(text) != clip_target(&detail, available)
        });
        append_body(
            theme, width, colors, name, body_input, result, failed, &mut out,
        );
        if result.is_none() {
            if let Some(streams) = facts.and_then(|f| f.process_output.as_ref()) {
                for (stream, label) in streams.iter().zip(["stdout:", "stderr:"]) {
                    let text = stream.display_text();
                    if text.is_empty() {
                        continue;
                    }
                    push_wrapped_row(theme.tool_muted, width, colors, label, "  ", &mut out);
                    for line in text.split('\n') {
                        push_wrapped_row(
                            if failed {
                                theme.error
                            } else {
                                theme.tool_output
                            },
                            width,
                            colors,
                            line,
                            "  ",
                            &mut out,
                        );
                    }
                }
            }
        }
        footer_row = out.len();
        let load_error = facts
            .and_then(|f| f.inline.as_ref())
            .and_then(|load| load.error.as_deref());
        let input_state = facts
            .and_then(|f| f.execution.as_ref())
            .and_then(|execution| {
                if !matches!(name, "bash" | "write" | "edit" | "apply_patch" | "patch") {
                    return None;
                }
                match execution.input_availability {
                    crate::protocol::ToolDataAvailabilityWire::Expired => Some("Input expired"),
                    crate::protocol::ToolDataAvailabilityWire::Unavailable => {
                        Some("Input unavailable")
                    }
                    _ => None,
                }
            });
        let error_hint = load_error
            .map(|error| format!("{error} · collapse and expand to retry"))
            .or_else(|| input_state.map(str::to_owned));
        out.push(rail::surface_row(
            width,
            colors,
            rail::SURFACE_CONTENT_START,
            Line::from(Span::styled(
                clip_summary(
                    error_hint.as_deref().unwrap_or(if partial {
                        "partial · ctrl+o collapse"
                    } else {
                        "ctrl+o collapse"
                    }),
                    available,
                ),
                Style::new().fg(theme.tool_muted),
            )),
        ));
    } else {
        footer_row = out.len();
        let input_count = facts.and_then(|f| f.input_line_count());
        let input_lines = input_count
            .map(|(count, _)| count)
            .or_else(|| display.and_then(|d| d.input_line_count))
            .or_else(|| {
                display
                    .and_then(|d| d.expanded_input.as_deref())
                    .map(result_line_count_text)
            })
            .or_else(|| {
                (!matches!(name, "bash" | "write" | "edit" | "apply_patch" | "patch")).then_some(0)
            });
        let output_lines = facts
            .and_then(|f| f.output_line_count)
            .or_else(|| result.map(result_line_count_text));
        let known_lines = input_lines.zip(output_lines).or_else(|| {
            (facts.is_some_and(|f| f.count_partial || f.process_count_partial())
                && (input_lines.is_some() || output_lines.is_some()))
            .then_some((input_lines.unwrap_or(0), output_lines.unwrap_or(0)))
        });
        let hint = match known_lines {
            Some((input, output)) => format!(
                "{}{count} lines hidden",
                if facts.is_some_and(|f| f.count_partial
                    || f.result_truncated
                    || f.display.body_truncated
                    || f.process_count_partial())
                    || input_count.is_some_and(|(_, partial)| partial)
                {
                    "≥"
                } else {
                    ""
                },
                count = input.saturating_add(output)
            ),
            None => "Lines unknown".to_owned(),
        };
        out.push(rail::surface_row(
            width,
            colors,
            rail::SURFACE_CONTENT_START,
            Line::from(Span::styled(
                clip_summary(&hint, available),
                Style::new().fg(theme.tool_muted),
            )),
        ));
    }
    out.push(Line::default());
    let mut copy_cells = vec![None; out.len()];
    copy_cells[0] = Some(CopyCells::decoration());
    copy_cells[out.len() - 1] = Some(CopyCells::decoration());
    copy_cells[footer_row] = Some(CopyCells::decoration());
    RenderedTool {
        lines: out,
        copy_cells,
    }
}

fn clip_summary(text: &str, width: usize) -> String {
    clip_line(&rail::collapsed_simple_line(text), width)
}

fn clip_line(text: &str, width: usize) -> String {
    if column_width(text) <= width {
        text.to_owned()
    } else if width == 0 {
        String::new()
    } else {
        format!("{}…", rail::clip_cells(text, width - 1))
    }
}

/// Keep both the path/command prefix and its distinguishing suffix.
fn clip_target(text: &str, width: usize) -> String {
    let text = rail::collapsed_simple_line(text);
    if column_width(&text) <= width || width < 5 {
        return clip_summary(&text, width);
    }
    let prefix_width = (width - 1) / 3;
    let suffix_width = width - 1 - prefix_width;
    let mut suffix = String::new();
    for grapheme in text.graphemes(true).rev() {
        if column_width(grapheme) + column_width(&suffix) > suffix_width {
            break;
        }
        suffix.insert_str(0, grapheme);
    }
    format!("{}…{suffix}", rail::clip_cells(&text, prefix_width))
}

fn target(
    name: &str,
    display: Option<&ToolDisplayWire>,
    facts: Option<&crate::state::tool::ToolFacts>,
) -> String {
    if let Some(invocation) = facts.and_then(|f| f.invocation.as_deref()) {
        match &invocation.subject {
            crate::protocol::ToolSubjectWire::File { path } => return path.clone(),
            crate::protocol::ToolSubjectWire::Command { script, .. } => return script.clone(),
            crate::protocol::ToolSubjectWire::Other => {}
        }
        // apply_patch currently has an Other subject. Read only its explicit
        // path field, never display the raw argument object.
        if matches!(name, "apply_patch" | "patch") {
            if let Ok(input) = serde_json::from_str::<serde_json::Value>(&invocation.input.preview)
            {
                if let Some(path) = input.get("path").and_then(|p| p.as_str()) {
                    return path.to_owned();
                }
            }
        }
    }
    if name == "bash" {
        if let Some(command) = display.and_then(|d| d.expanded_input.as_deref()) {
            return command.to_owned();
        }
    }
    if matches!(name, "apply_patch" | "patch") {
        if let Some(input) = display.and_then(|d| d.expanded_input.as_deref()) {
            let paths: Vec<_> = input
                .lines()
                .filter_map(|line| {
                    [
                        "*** Update File: ",
                        "*** Add File: ",
                        "*** Delete File: ",
                        "+++ b/",
                        "--- a/",
                    ]
                    .iter()
                    .find_map(|prefix| line.strip_prefix(prefix))
                })
                .collect();
            let mut unique = Vec::new();
            for path in paths {
                if !unique.contains(&path) {
                    unique.push(path);
                }
            }
            if !unique.is_empty() {
                return unique.join(", ");
            }
        }
    }
    display
        .map(|d| d.detail.as_str())
        .filter(|d| !d.is_empty())
        .unwrap_or(name)
        .to_owned()
}

fn command_summary(
    name: &str,
    result: Option<&str>,
    facts: Option<&crate::state::tool::ToolFacts>,
) -> (Option<String>, bool) {
    use crate::protocol::CommandStatusWire as S;
    if let Some(command) = facts.and_then(|f| f.command.as_deref()) {
        let (text, warning) = match command.status {
            S::Exited => match (command.exit_code, command.signal) {
                (Some(code), _) => (
                    format!("exit {code}{}", if code != 0 { " (nonzero)" } else { "" }),
                    code != 0,
                ),
                (_, Some(signal)) => (format!("signal {signal}"), true),
                _ => ("exit unknown".to_owned(), true),
            },
            S::Running => ("process running".to_owned(), false),
            S::Cancelling => ("process cancelling".to_owned(), true),
            S::Cancelled => ("process cancelled".to_owned(), true),
            S::TimedOut => ("timed out".to_owned(), true),
            S::SpawnFailed => ("spawn failed".to_owned(), true),
            S::Failed => ("process failed".to_owned(), true),
        };
        return (Some(text), warning);
    }
    // Older histories have only the Agent's formatted Bash output.
    if name.eq_ignore_ascii_case("bash") {
        if let Some(value) = result
            .and_then(|r| r.lines().next())
            .and_then(|l| l.strip_prefix("exit_code: "))
        {
            return match value.parse::<i32>() {
                Ok(code) => (
                    Some(format!(
                        "exit {code}{}",
                        if code != 0 { " (nonzero)" } else { "" }
                    )),
                    code != 0,
                ),
                Err(_) => (Some("exit unknown".to_owned()), true),
            };
        }
    }
    (None, false)
}

#[allow(clippy::too_many_arguments)]
fn append_body(
    theme: &Theme,
    width: usize,
    colors: rail::SurfaceColors,
    name: &str,
    input: Option<&str>,
    result: Option<&str>,
    failed: bool,
    out: &mut Vec<Line<'static>>,
) {
    if let Some(input) = input.filter(|s| !s.is_empty()) {
        // Only explicit patch formats carry diff semantics. Legacy edit displays
        // concatenate old/new text and must not be guessed from a leading +/-.
        let diff = matches!(name, "apply_patch" | "patch" | "apply_batch")
            || (name == "edit" && input.starts_with("--- before\n+++ after\n@@"));
        let mut in_hunk = false;
        for line in input.split('\n') {
            let fg = if diff {
                diff_line_color(theme, line, &mut in_hunk)
            } else if name == "write" {
                theme.success
            } else {
                theme.tool_output
            };
            push_wrapped_row(fg, width, colors, line, "  ", out);
        }
    }
    if let Some(result) = result.filter(|s| !s.is_empty()) {
        for line in result.split('\n') {
            push_wrapped_row(
                if failed {
                    theme.error
                } else {
                    theme.tool_output
                },
                width,
                colors,
                line,
                "  ",
                out,
            );
        }
    }
}

fn diff_line_color(theme: &Theme, line: &str, in_hunk: &mut bool) -> ratatui::style::Color {
    if line.starts_with("@@") {
        *in_hunk = true;
        return theme.tool_muted;
    }
    if line.starts_with("*** ") || line.starts_with("diff --git ") {
        *in_hunk = line.starts_with("*** Add File: ");
        return theme.tool_muted;
    }
    if !*in_hunk && (line.starts_with("--- ") || line.starts_with("+++ ")) {
        return theme.tool_muted;
    }
    if *in_hunk {
        if line.starts_with('+') {
            return theme.success;
        }
        if line.starts_with('-') {
            return theme.error;
        }
    }
    theme.tool_output
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

fn durable_summary(block: &ToolBlock) -> String {
    if let Some(status) = block.live_status {
        return status_summary(status);
    }
    match block.outcome {
        Some(ToolOutcomeWire::Failed) => "failed",
        Some(ToolOutcomeWire::Denied) => "denied",
        Some(ToolOutcomeWire::Cancelled) => "cancelled",
        Some(ToolOutcomeWire::Unknown) => "outcome unknown: unconfirmed",
        Some(ToolOutcomeWire::Success) => "completed",
        Some(ToolOutcomeWire::InputProvided) => "input provided",
        None => "pending",
    }
    .to_owned()
}

fn live_summary(tool: &LiveTool) -> String {
    status_summary(tool.status)
}
fn status_summary(status: ToolStatus) -> String {
    match status {
        ToolStatus::Pending => "pending",
        ToolStatus::Running => "running",
        ToolStatus::Succeeded => "completed",
        ToolStatus::Failed => "failed",
        ToolStatus::Denied => "denied",
        ToolStatus::Cancelled => "cancelled",
    }
    .to_owned()
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

fn result_line_count_text(text: &str) -> usize {
    result_line_count(Some(text))
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
        let line = visible_tool_line(line);
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

// Terminals discard raw tabs in styled cells. Use a visible escape before both
// measurement and wrapping so argument separators never silently disappear.
fn visible_tool_line(line: &str) -> String {
    crate::safe_text::safe_display(line).replace('\t', "\\t")
}

fn push_wrapped_row(
    foreground: ratatui::style::Color,
    width: usize,
    colors: rail::SurfaceColors,
    line: &str,
    indent: &str,
    out: &mut Vec<Line<'static>>,
) {
    let line = visible_tool_line(line);
    let content_width = width
        .saturating_sub(rail::SURFACE_CONTENT_START)
        .saturating_sub(UnicodeWidthStr::width(indent));
    let content_width = content_width.max(1);
    if line.is_empty() {
        out.push(rail::surface_row(
            width,
            colors,
            rail::SURFACE_CONTENT_START,
            Line::default(),
        ));
        return;
    }
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
                Style::new().fg(foreground),
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
            result: Some(payload.clone().into()),
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
            result: Some(last.to_owned().into()),
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
    fn card(name: &str, result: &str) -> ToolBlock {
        ToolBlock {
            index: None,
            loop_id: "l".into(),
            request_index: 0,
            tool_call_id: "c".into(),
            name: name.into(),
            result: Some(result.to_owned().into()),
            outcome: Some(ToolOutcomeWire::Success),
            live_status: None,
            progress: None,
            expanded: false,
        }
    }
    fn text(lines: &[ratatui::text::Line<'_>]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
    fn display(detail: &str, input: Option<&str>) -> crate::protocol::ToolDisplayWire {
        crate::protocol::ToolDisplayWire {
            body_truncated: false,
            detail: detail.into(),
            expanded_input: input.map(str::to_owned),
            input_line_count: input.map(super::result_line_count_text),
            hidden_line_count: Some(999),
            truncated: false,
        }
    }

    #[test]
    fn collapsed_cards_have_three_content_rows_without_result_summaries() {
        for theme in [Theme::dark(), Theme::light()] {
            for name in ["bash", "read", "write", "edit", "apply_patch", "custom"] {
                for width in [24, 59, 79, 119] {
                    let rows = super::durable_with_display(
                        &theme,
                        &card(name, "PRIVATE RESULT"),
                        width,
                        false,
                        Some(&display(
                            "target-first-line\nHIDDEN COMMAND CONTINUATION",
                            Some("a\nb"),
                        )),
                    );
                    assert_eq!(rows.len(), 5); // Three content rows and the existing outer spacing.
                    let visible = text(&rows);
                    assert!(visible.contains(name));
                    assert!(visible.contains("3 lines hidden"));
                    assert!(!visible.contains("PRIVATE RESULT"));
                    assert!(!visible.contains("CONTINUATION"));
                    assert!(!visible.contains("completed"));
                    assert!(!visible.contains("ctrl+o"));
                    assert!(
                        rows.iter()
                            .all(|line| crate::markdown::line_width(line) <= width)
                    );
                }
            }
        }
    }

    #[test]
    fn collapsed_command_preserves_spaces_and_only_its_first_physical_line() {
        let display = display("ignored", Some("printf 'a  b'\nsecond command"));
        let rows = super::durable_with_display(
            &Theme::dark(),
            &card("bash", ""),
            59,
            false,
            Some(&display),
        );
        assert!(text(&rows).contains("printf 'a  b'"));
        assert!(!text(&rows).contains("second command"));
    }

    #[test]
    fn hidden_logical_lines_are_width_independent_and_do_not_count_target() {
        let block = card("write", &format!("{}\n\nend\n", "中文内容".repeat(30)));
        let display = display("target", Some("a\n\n"));
        for width in [20, 59, 79] {
            let rows =
                super::durable_with_display(&Theme::dark(), &block, width, false, Some(&display));
            assert!(text(&rows).contains("7 lines hidden"));
        }
    }

    #[test]
    fn missing_counts_are_unknown_and_target_clipping_is_not_a_body_lower_bound() {
        use std::sync::Arc;
        let mut facts = crate::state::tool::ToolFacts::new("write");
        facts.display = Arc::new(display("very long target", None));
        let block = card("write", "");
        assert!(
            text(
                &super::durable_with_metadata(&Theme::dark(), &block, 59, false, Some(&facts))
                    .lines
            )
            .contains("Lines unknown")
        );
        Arc::make_mut(&mut facts.display).input_line_count = Some(42);
        Arc::make_mut(&mut facts.display).truncated = true;
        facts.output_line_count = Some(0);
        let rendered =
            super::durable_with_metadata(&Theme::dark(), &block, 59, false, Some(&facts));
        assert!(text(&rendered.lines).contains("42 lines hidden"));
        assert!(!text(&rendered.lines).contains('≥'));
        assert!(rendered.copy_cells[3].as_ref().unwrap().decorative);
        facts.count_partial = true;
        assert!(
            text(
                &super::durable_with_metadata(&Theme::dark(), &block, 59, false, Some(&facts))
                    .lines
            )
            .contains("≥42 lines hidden")
        );
    }

    #[test]
    fn narrow_target_clips_explicitly_preserving_filename_and_graphemes() {
        let target = format!("/a/very/long/{}/release-notes.md", "目录/".repeat(20));
        let clipped = super::clip_target(&target, 45);
        assert!(clipped.contains('…'));
        assert!(clipped.ends_with("release-notes.md"));
        assert!(crate::markdown::column_width(&clipped) <= 45);
    }

    #[test]
    fn structured_command_status_preserves_unknown_signal_timeout_and_cancellation() {
        use crate::protocol::{CommandResultWire, CommandStatusWire as S};
        let mut facts = crate::state::tool::ToolFacts::new("bash");
        for (status, exit_code, signal, label) in [
            (S::Exited, None, None, "exit unknown"),
            (S::Exited, None, Some(9), "signal 9"),
            (S::TimedOut, None, None, "timed out"),
            (S::Cancelled, None, None, "process cancelled"),
            (S::Exited, Some(0), None, "exit 0"),
        ] {
            facts.command = Some(std::sync::Arc::new(CommandResultWire {
                status,
                exit_code,
                signal,
                termination_confirmed: true,
                stdout_base_offset: 0,
                stdout_observed_end: 0,
                stderr_base_offset: 0,
                stderr_observed_end: 10,
                output_complete: true,
                output_truncated: false,
            }));
            let result = super::command_summary("bash", Some("exit_code: 0"), Some(&facts));
            assert_eq!(result.0.as_deref(), Some(label));
            assert_eq!(result.1, exit_code != Some(0));
        }
    }
    #[test]
    fn facts_patch_path_and_live_nonzero_share_durable_summary() {
        use std::sync::Arc;
        let mut facts = crate::state::tool::ToolFacts::new("apply_patch");
        facts.invocation = Some(Arc::new(crate::protocol::ToolInvocationWire {
            tool_ref: crate::protocol::ToolRefWire {
                session_id: "s".into(),
                loop_id: "l".into(),
                request_index: 0,
                tool_call_id: "c".into(),
            },
            name: "apply_patch".into(),
            subject: crate::protocol::ToolSubjectWire::Other,
            subject_truncated: false,
            input: crate::protocol::ToolInputSummaryWire {
                total_bytes: 30,
                preview: r#"{"path":"patch-target.txt"}"#.into(),
                truncated: false,
                encoding: "utf8_json".into(),
            },
        }));
        let rows = super::durable_with_facts(
            &Theme::dark(),
            &card("apply_patch", "tool failed"),
            59,
            false,
            Some(&facts),
        );
        assert!(text(&rows).contains("patch-target.txt"));
        let live = crate::state::tool::LiveTool {
            tool_call_id: "c".into(),
            name: "bash".into(),
            status: crate::state::tool::ToolStatus::Succeeded,
            progress: None,
            display: Some(Arc::new(display("printf error >&2; exit 7", None))),
            result: Some("exit_code: 7\nstdout:\n\nstderr:\nerror".into()),
            result_truncated: false,
            expanded: false,
        };
        let rendered = text(&super::live_with_facts(&Theme::dark(), &live, 59, None));
        assert!(rendered.contains("bash"));
        assert!(!rendered.contains("stderr: error"));
    }

    #[test]
    fn empty_collapsed_tool_has_no_empty_expand_claim() {
        let rows = super::durable(&Theme::dark(), &card("read", ""), 59, false);
        assert!(!text(&rows).contains("hidden rows"));
        assert!(!text(&rows).contains("expand"));
        assert_eq!(rows.len(), 5);
        let rendered =
            super::durable_with_metadata(&Theme::dark(), &card("read", ""), 59, false, None);
        assert!(
            rendered.copy_cells[2].is_none(),
            "empty card target must remain copyable"
        );
    }

    #[test]
    fn expanded_bash_command_wraps_losslessly_in_both_themes() {
        let command = format!(
            "find . -type f -print0 | xargs -0 grep '{}'\nprintf done",
            "中文👨‍👩‍👧pattern".repeat(30)
        );
        let display = display("$ find …", Some(&command));
        for theme in [Theme::dark(), Theme::light()] {
            for width in [12, 24, 59, 119] {
                let rows = super::durable_with_display(
                    &theme,
                    &card("bash", ""),
                    width,
                    true,
                    Some(&display),
                );
                let body = &rows[3..rows.len() - 2];
                let rendered = body
                    .iter()
                    .flat_map(|line| &line.spans)
                    .filter(|span| span.style.fg == Some(theme.tool_output))
                    .map(|span| span.content.strip_prefix("  ").unwrap_or(&span.content))
                    .collect::<String>();
                assert_eq!(rendered, command.replace('\n', ""));
                assert!(
                    rows.iter()
                        .all(|line| crate::markdown::line_width(line) <= width)
                );
                let collapsed = super::durable_with_display(
                    &theme,
                    &card("bash", ""),
                    59,
                    false,
                    Some(&display),
                );
                assert!(text(&collapsed).contains("lines hidden"));
            }
        }
    }

    #[test]
    fn bash_tabs_remain_visible_in_real_terminal_cells() {
        use ratatui::{
            buffer::Buffer,
            layout::Rect,
            widgets::{Paragraph, Widget},
        };

        let command = "printf\tfoo\n\t中文👨‍👩‍👧end";
        for theme in [Theme::dark(), Theme::light()] {
            for width in [12usize, 24, 59] {
                let rows = super::durable_with_display(
                    &theme,
                    &card("bash", ""),
                    width,
                    true,
                    Some(&display("$ printf …", Some(command))),
                );
                let area = Rect::new(0, 0, width as u16, rows.len() as u16);
                let mut buffer = Buffer::empty(area);
                Paragraph::new(rows.clone()).render(area, &mut buffer);
                let start = (super::rail::SURFACE_CONTENT_START + 2) as u16;
                let mut visible = String::new();
                for y in 3..rows.len() - 2 {
                    let line = (start..width as u16)
                        .map(|x| buffer[(x, y as u16)].symbol())
                        .collect::<String>();
                    // Wide graphemes occupy continuation cells containing spaces.
                    visible.push_str(&line.replace(' ', ""));
                }
                assert_eq!(visible, command.replace('\t', "\\t").replace('\n', ""));
                assert_eq!(
                    super::wrapped_result_row_count(command, width - start as usize),
                    rows.len() - 5,
                );
            }
        }
    }

    #[test]
    fn patch_semantics_color_changes_but_not_context_metadata_or_results() {
        for theme in [Theme::dark(), Theme::light()] {
            for (name, input) in [
                (
                    "apply_patch",
                    "--- a/file\n+++ b/file\n@@ -1,2 +1,2 @@\n unchanged\n-old\n+new",
                ),
                (
                    "patch",
                    "*** Begin Patch\n*** Update File: file\n@@\n unchanged\n-old\n+new\n*** End Patch",
                ),
                (
                    "apply_batch",
                    "*** Begin Patch\n*** Add File: file\n+new\n*** End Patch",
                ),
                (
                    "edit",
                    "--- before\n+++ after\n@@ -1,2 +1,2 @@\n unchanged\n-old\n+new",
                ),
            ] {
                let rows = super::durable_with_display(
                    &theme,
                    &card(name, "+result is not a diff"),
                    59,
                    true,
                    Some(&display("file", Some(input))),
                );
                for span in rows.iter().flat_map(|line| &line.spans) {
                    let content = span.content.trim_start();
                    let expected = match content {
                        "+new" => Some(theme.success),
                        "-old" => Some(theme.error),
                        "unchanged" | "+result is not a diff" => Some(theme.tool_output),
                        _ if content.starts_with("@@")
                            || content.starts_with("*** ")
                            || content.starts_with("--- ")
                            || content.starts_with("+++ ") =>
                        {
                            Some(theme.tool_muted)
                        }
                        _ => None,
                    };
                    if let Some(expected) = expected {
                        assert_eq!(span.style.fg, Some(expected), "{name}: {content}");
                    }
                }
                assert!(
                    rows.iter()
                        .flat_map(|line| &line.spans)
                        .any(|span| span.content.trim() == "+new"
                            && span.style.fg == Some(theme.success))
                );
            }
        }
    }

    #[test]
    fn legacy_edit_and_read_are_not_misidentified_as_diffs() {
        let theme = Theme::dark();
        for name in ["read", "edit", "custom"] {
            let rows = super::durable_with_display(
                &theme,
                &card(name, "-result"),
                59,
                true,
                Some(&display("file", Some("+literal\n-literal"))),
            );
            for span in rows
                .iter()
                .flat_map(|line| &line.spans)
                .filter(|span| span.content.contains("literal") || span.content.contains("-result"))
            {
                assert_eq!(span.style.fg, Some(theme.tool_output));
            }
        }
        let rows = super::durable_with_display(
            &theme,
            &card("write", "written"),
            59,
            true,
            Some(&display("file", Some("new content"))),
        );
        assert!(rows.iter().flat_map(|line| &line.spans).any(|span| {
            span.content.contains("new content") && span.style.fg == Some(theme.success)
        }));
    }

    #[test]
    fn truncated_inputs_remain_explicitly_partial_when_expanded() {
        let mut display = display("file", Some("--- before\n+++ after\n@@\n same"));
        display.truncated = true;
        let rows = super::durable_with_display(
            &Theme::dark(),
            &card("edit", ""),
            59,
            true,
            Some(&display),
        );
        assert!(text(&rows).contains("partial · ctrl+o collapse"));
    }

    #[test]
    fn errors_use_error_foreground_on_chat_background_not_warning_surfaces() {
        for theme in [Theme::dark(), Theme::light()] {
            for expanded in [false, true] {
                let mut failed = card("read", "cannot read file");
                failed.outcome = Some(ToolOutcomeWire::Failed);
                for block in [
                    failed,
                    card("bash", "exit_code: 7\nstdout:\n\nstderr:\nerror"),
                ] {
                    let rows = super::durable(&theme, &block, 59, expanded);
                    assert_eq!(rows[1].spans[0].style.fg, Some(theme.error));
                    assert!(
                        rows.iter().flat_map(|line| &line.spans).all(|span| span
                            .style
                            .bg
                            .unwrap_or(ratatui::style::Color::Reset)
                            == theme.page_bg)
                    );
                    if expanded {
                        assert!(rows.iter().flat_map(|line| &line.spans).any(|span| {
                            span.content.contains(if block.name == "bash" {
                                "error"
                            } else {
                                "cannot read"
                            }) && span.style.fg == Some(theme.error)
                        }));
                    }
                }
            }
        }
    }
}
