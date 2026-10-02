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
        block.result.as_deref(),
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
        tool.result.as_deref(),
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
    if warning {
        colors.rail = theme.warning;
    }
    let status = process.map_or(status.clone(), |process| format!("{status} · {process}"));
    let title = format!("{} · {status}", clip_summary(name, 18));
    let mut out = vec![Line::default()];
    let available = width.saturating_sub(rail::SURFACE_CONTENT_START);
    for (text, color) in [
        (
            &title,
            if warning {
                theme.warning
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
                    clip_target(text, available)
                } else {
                    // The conversation overlays its detail action at the right.
                    clip_summary(
                        text,
                        available.saturating_sub(if width >= 16 { 9 } else { 0 }),
                    )
                },
                Style::new().fg(color),
            )),
        ));
    }
    let mut footer_row = None;
    if expanded {
        append_body(theme, width, colors, display, result, &mut out);
        footer_row = Some(out.len());
        out.push(rail::surface_row(
            width,
            colors,
            rail::SURFACE_CONTENT_START,
            Line::from(Span::styled(
                clip_summary("ctrl+o collapse", available),
                Style::new().fg(theme.tool_muted),
            )),
        ));
    } else {
        if let Some(summary) = output_summary(name, result, facts) {
            out.push(rail::surface_row(
                width,
                colors,
                rail::SURFACE_CONTENT_START,
                Line::from(Span::styled(
                    clip_summary(&summary, available),
                    Style::new().fg(theme.tool_output),
                )),
            ));
        }
        let content_width = width.saturating_sub(rail::SURFACE_CONTENT_START + 2).max(1);
        let hidden = display
            .and_then(|d| d.expanded_input.as_deref())
            .map_or(0, |text| wrapped_result_row_count(text, content_width))
            + result.map_or(0, |text| wrapped_result_row_count(text, content_width));
        let partial =
            display.is_some_and(|d| d.truncated) || facts.is_some_and(|f| f.result_truncated);
        if hidden > 0 || partial {
            footer_row = Some(out.len());
            let hint = format!(
                "{hidden} hidden rows{} · ctrl+o expand",
                if partial { " · partial" } else { "" }
            );
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
    }
    out.push(Line::default());
    let mut copy_cells = vec![None; out.len()];
    copy_cells[0] = Some(CopyCells::decoration());
    copy_cells[out.len() - 1] = Some(CopyCells::decoration());
    if let Some(row) = footer_row {
        copy_cells[row] = Some(CopyCells::decoration());
    }
    RenderedTool {
        lines: out,
        copy_cells,
    }
}

fn clip_summary(text: &str, width: usize) -> String {
    let text = rail::collapsed_simple_line(text);
    if column_width(&text) <= width {
        text
    } else if width == 0 {
        String::new()
    } else {
        format!("{}…", rail::clip_cells(&text, width - 1))
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

fn output_summary(
    name: &str,
    result: Option<&str>,
    facts: Option<&crate::state::tool::ToolFacts>,
) -> Option<String> {
    if name.eq_ignore_ascii_case("bash") {
        if let Some(result) = result {
            if let Some((_, stderr)) = result.split_once("\nstderr:\n") {
                if let Some(line) = stderr.lines().find(|l| !l.trim().is_empty()) {
                    return Some(format!("stderr: {line}"));
                }
            }
            if let Some((_, stdout)) = result.split_once("\nstdout:\n") {
                if let Some(line) = stdout
                    .split("\nstderr:")
                    .next()
                    .unwrap_or_default()
                    .lines()
                    .find(|l| !l.trim().is_empty())
                {
                    return Some(format!("stdout: {line}"));
                }
            }
        }
        if facts
            .and_then(|f| f.command.as_deref())
            .is_some_and(|c| c.stderr_observed_end > 0)
        {
            return Some("stderr output available in detail".to_owned());
        }
    }
    result_summary(result)
}

fn append_body(
    theme: &Theme,
    width: usize,
    colors: rail::SurfaceColors,
    display: Option<&ToolDisplayWire>,
    result: Option<&str>,
    out: &mut Vec<Line<'static>>,
) {
    for text in [display.and_then(|d| d.expanded_input.as_deref()), result]
        .into_iter()
        .flatten()
        .filter(|text| !text.is_empty())
    {
        for line in text.split('\n') {
            push_wrapped_row(theme, width, colors, line, "  ", out);
        }
    }
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
fn result_summary(result: Option<&str>) -> Option<String> {
    let line = result?.lines().find(|line| !line.trim().is_empty())?;
    Some(clip_summary(line, 160))
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
        let line = crate::safe_text::safe_display(line);
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
            detail: detail.into(),
            expanded_input: input.map(str::to_owned),
            input_line_count: Some(999),
            hidden_line_count: Some(999),
            truncated: false,
        }
    }

    #[test]
    fn collapsed_success_and_failure_keep_target_and_result_in_both_themes() {
        for theme in [Theme::dark(), Theme::light()] {
            for width in [59, 79, 119] {
                let display = display("release-notes.md", Some("one\ntwo"));
                let mut block = card("write", "Wrote 37 lines successfully");
                let rendered = text(&super::durable_with_display(
                    &theme,
                    &block,
                    width,
                    false,
                    Some(&display),
                ));
                assert!(rendered.contains("write · completed"));
                assert!(rendered.contains("release-notes.md"));
                assert!(rendered.contains("Wrote 37 lines successfully"));
                assert!(rendered.contains("3 hidden rows"));
                block.outcome = Some(ToolOutcomeWire::Failed);
                block.result = Some("The operation failed: ".repeat(50).into());
                let rendered = text(&super::durable_with_display(
                    &theme,
                    &block,
                    width,
                    false,
                    Some(&display),
                ));
                assert!(rendered.contains("write · failed"));
                assert!(rendered.contains("release-notes.md"));
                assert!(rendered.contains('…'));
            }
        }
    }

    #[test]
    fn collapsed_lifecycle_is_textual_and_patch_target_is_recovered() {
        let mut block = card(
            "apply_patch",
            "patched 67 bytes to 68 bytes at generated-live.txt",
        );
        let display = display(
            "tool apply_patch",
            Some("--- a/generated-live.txt\n+++ b/generated-live.txt\n@@ -1 +1 @@\n-old\n+new"),
        );
        for (status, label) in [
            (crate::state::tool::ToolStatus::Pending, "pending"),
            (crate::state::tool::ToolStatus::Running, "running"),
            (crate::state::tool::ToolStatus::Succeeded, "completed"),
        ] {
            block.live_status = Some(status);
            let rendered = text(&super::durable_with_display(
                &Theme::dark(),
                &block,
                59,
                false,
                Some(&display),
            ));
            assert!(rendered.contains(&format!("apply_patch · {label}")));
            assert!(rendered.contains("generated-live.txt"));
            assert!(rendered.contains("patched 67 bytes to 68 bytes"));
        }
    }

    #[test]
    fn compact_bash_nonzero_warns_without_changing_invocation_outcome() {
        for theme in [Theme::dark(), Theme::light()] {
            let block = card("bash", "exit_code: 7\nstdout:\n\nstderr:\nSIMULATED-ERROR");
            let rows = super::durable_with_display(
                &theme,
                &block,
                59,
                false,
                Some(&display("exit 7", None)),
            );
            let rendered = text(&rows);
            assert!(rendered.contains("completed · exit 7 (nonzero)"));
            assert!(rendered.contains("stderr: SIMULATED-ERROR"));
            assert_eq!(rows[1].spans[0].style.fg, Some(theme.warning));
            assert_eq!(block.outcome, Some(ToolOutcomeWire::Success));
        }
    }

    #[test]
    fn hidden_count_matches_expanded_body_including_blank_sanitized_wrapped_rows() {
        for width in [20, 59, 79] {
            let block = card("read", &format!("{}\n\n\tend\n", "中文内容".repeat(30)));
            let display = display("notes.txt", Some("a\n\n"));
            let compact = text(&super::durable_with_display(
                &Theme::dark(),
                &block,
                width,
                false,
                Some(&display),
            ));
            let expanded =
                super::durable_with_display(&Theme::dark(), &block, width, true, Some(&display));
            // Two common headers, expanded fold control, two exterior spacers.
            assert!(
                compact.contains(&format!("{} hidden rows", expanded.len() - 5)),
                "{compact}"
            );
        }
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
            assert!(
                super::output_summary("bash", None, Some(&facts))
                    .unwrap()
                    .contains("stderr")
            );
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
        assert!(rendered.contains("completed · exit 7 (nonzero)"));
        assert!(rendered.contains("stderr: error"));
    }

    #[test]
    fn empty_collapsed_tool_has_no_empty_expand_claim() {
        let rows = super::durable(&Theme::dark(), &card("read", ""), 59, false);
        assert!(!text(&rows).contains("hidden rows"));
        assert!(!text(&rows).contains("expand"));
        assert_eq!(rows.len(), 4);
        let rendered =
            super::durable_with_metadata(&Theme::dark(), &card("read", ""), 59, false, None);
        assert!(
            rendered.copy_cells[2].is_none(),
            "empty card target must remain copyable"
        );
    }
}
