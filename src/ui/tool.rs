//! Tool cards (development spec 15.5, 29). Live tools show name/status plus
//! any progress; durable tools get a safe summary (the Agent transcript
//! carries no arguments, so the name stands alone) and a collapsed result
//! preview that is capped at 40 lines and 32 KiB (spec 29.2).

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::markdown::column_width;
use crate::state::tool::{LiveTool, ToolStatus};
use crate::state::transcript::ToolBlock;
use crate::theme::Theme;
use crate::ui::layout;

const MAX_PREVIEW_LINES: usize = 40;
const MAX_PREVIEW_CHARS: usize = 32 * 1024;

/// A durable tool card (background per terminal/outcome state).
pub fn durable(
    theme: &Theme,
    block: &ToolBlock,
    width: usize,
    all_expanded: bool,
) -> Vec<Line<'static>> {
    let mut out = vec![Line::default()];
    let (bg, status) = durable_status(theme, block);
    let base = Style::new().fg(theme.text).bg(bg);
    header(name_line(block), status, width, base, &mut out);
    if block.expanded || all_expanded {
        match &block.result {
            Some(result) => preview(theme, result, width, bg, &mut out),
            None => out.push(layout::filled("  (no result)", width, base)),
        }
    }
    out.push(Line::default());
    out
}

/// A live tool card (background per live status).
pub fn live(theme: &Theme, tool: &LiveTool, width: usize) -> Vec<Line<'static>> {
    let mut out = vec![Line::default()];
    let (bg, status) = match tool.status {
        ToolStatus::Pending | ToolStatus::Running => (theme.tool_pending_bg, "running"),
        ToolStatus::Succeeded => (theme.tool_success_bg, "success"),
        ToolStatus::Failed => (theme.tool_error_bg, "failed"),
        ToolStatus::Denied => (theme.tool_error_bg, "denied"),
        ToolStatus::Cancelled => (theme.tool_error_bg, "cancelled"),
    };
    let base = Style::new().fg(theme.text).bg(bg);
    header(tool.name.clone(), status, width, base, &mut out);
    if let Some(progress) = &tool.progress {
        out.push(layout::filled(&format!("  {progress}"), width, base));
    }
    out.push(Line::default());
    out
}

fn header(name: String, status: &str, width: usize, base: Style, out: &mut Vec<Line<'static>>) {
    let name_w = column_width(&name);
    let gap = width.saturating_sub(name_w + status.len() + 3);
    let spans = vec![
        Span::styled(" ", base),
        Span::styled(name, base.add_modifier(Modifier::BOLD)),
        Span::styled(" ".repeat(gap), base),
        Span::styled(format!(" {status}"), base),
    ];
    out.push(layout::fill_line(Line::from(spans), width, base));
}

/// The safe call summary: the transcript carries no arguments, so the name
/// stands alone (spec 29.2/29.3).
fn name_line(block: &ToolBlock) -> String {
    block.name.clone()
}

fn durable_status(theme: &Theme, block: &ToolBlock) -> (ratatui::style::Color, &'static str) {
    if let Some(status) = &block.live_status {
        match status {
            ToolStatus::Pending | ToolStatus::Running => (theme.tool_pending_bg, "running"),
            ToolStatus::Succeeded => (theme.tool_success_bg, "success"),
            ToolStatus::Failed => (theme.tool_error_bg, "failed"),
            ToolStatus::Denied => (theme.tool_error_bg, "denied"),
            ToolStatus::Cancelled => (theme.tool_error_bg, "cancelled"),
        }
    } else {
        match block.outcome.as_deref() {
            Some("success") => (theme.tool_success_bg, "success"),
            Some("failed") | Some("input_provided") => (theme.tool_error_bg, "failed"),
            Some("denied") => (theme.tool_error_bg, "denied"),
            Some("cancelled") => (theme.tool_error_bg, "cancelled"),
            _ => (theme.tool_pending_bg, "running"),
        }
    }
}

fn preview(
    theme: &Theme,
    result: &str,
    width: usize,
    bg: ratatui::style::Color,
    out: &mut Vec<Line<'static>>,
) {
    let base = Style::new().fg(theme.text).bg(bg);
    let original_lines = result.lines().count();
    let truncated: String = result.chars().take(MAX_PREVIEW_CHARS).collect();
    let shown: Vec<&str> = truncated.lines().take(MAX_PREVIEW_LINES).collect();
    for line in &shown {
        out.push(layout::filled(&format!("  {line}"), width, base));
    }
    let hidden = original_lines.saturating_sub(shown.len());
    if hidden > 0 {
        out.push(layout::filled(
            &format!("  … {hidden} more lines"),
            width,
            base.fg(theme.dim),
        ));
    }
}
