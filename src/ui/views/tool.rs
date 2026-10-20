//! The tool-output view: a card's input, then the streams it printed.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;

use crate::app::Session;
use crate::app::tasks::StreamText;
use crate::app::tool::ToolState;
use crate::transcript::ToolCall;

use super::{chrome, styled};
use crate::ui::theme::Styles;
use crate::ui::wrap::sanitize;

fn state_label(state: crate::transcript::ToolState) -> &'static str {
    match state {
        crate::transcript::ToolState::Pending => "requested",
        crate::transcript::ToolState::Running => "running",
        crate::transcript::ToolState::Succeeded => "succeeded",
        crate::transcript::ToolState::Failed => "failed",
        crate::transcript::ToolState::Denied => "denied",
        crate::transcript::ToolState::Cancelled => "cancelled",
    }
}

/// One row of the tool view's output, before it is styled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolRow {
    Text(String),
    /// The heading of a stream: `── stderr ──`.
    Section(String),
    Note(&'static str),
}

impl ToolRow {
    fn text(&self) -> String {
        match self {
            Self::Text(text) => text.clone(),
            Self::Section(name) => format!("── {name} ──"),
            Self::Note(note) => (*note).to_owned(),
        }
    }

    fn line(&self, styles: Styles) -> Line<'static> {
        match self {
            Self::Text(text) => Line::raw(text.clone()),
            Self::Section(name) => {
                let style = if name == "stderr" {
                    styles.warning()
                } else {
                    styles.dim()
                };
                styled(&self.text(), style)
            }
            Self::Note(note) => styled(note, styles.dim()),
        }
    }
}

/// The lines of plain text as rows, made safe for the terminal.
pub fn text_rows(text: &str) -> Vec<ToolRow> {
    text.lines()
        .map(|line| ToolRow::Text(sanitize(line).into_owned()))
        .collect()
}

/// The rows of the answered streams, each under a heading when there are several.
pub fn stream_rows(streams: &[(String, Option<StreamText>)]) -> Vec<ToolRow> {
    let sections = streams.len() > 1;
    let mut rows = Vec::new();
    for (name, text) in streams {
        let Some(text) = text else { continue };
        let body = text_rows(&text.text);
        let noted = text.dropped_head || text.expired || text.cut;
        if body.is_empty() && !noted {
            continue;
        }
        if sections {
            if !rows.is_empty() {
                rows.push(ToolRow::Text(String::new()));
            }
            rows.push(ToolRow::Section(name.clone()));
        }
        if text.dropped_head {
            rows.push(ToolRow::Note("… earlier output is no longer retained"));
        }
        if text.expired && body.is_empty() {
            rows.push(ToolRow::Note("… this output has expired from the agent"));
        }
        rows.extend(body);
        if text.cut {
            rows.push(ToolRow::Note("… cut at 1 MiB"));
        }
    }
    rows
}

/// The call's input: a bash command always, since the detail is a summary
/// that can name another one; any other input only when it adds to the detail.
fn tool_input(call: &ToolCall) -> &str {
    let input = call.expanded_input.as_deref().unwrap_or_default();
    if call.name == "bash" || input.trim() != call.detail.trim() {
        input
    } else {
        ""
    }
}

/// The text of the tool view, input then output, for `c`.
pub fn text(tool: &ToolState, session: Option<&Session>) -> String {
    let input = session
        .and_then(|session| session.transcript.find_tool(&tool.key))
        .map_or("", tool_input);
    let mut out = String::new();
    for line in input
        .lines()
        .map(str::to_owned)
        .chain(tool.rows.iter().map(ToolRow::text))
    {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

pub fn render(
    frame: &mut Frame,
    area: Rect,
    styles: Styles,
    tool: &ToolState,
    session: Option<&Session>,
) {
    let Some((session, call)) =
        session.and_then(|session| Some((session, session.transcript.find_tool(&tool.key)?)))
    else {
        return;
    };
    let mut title = format!("{} · {}", call.name, state_label(call.state));
    let calls = || session.transcript.tool_calls();
    if let Some(index) = calls().position(|other| other.key == tool.key) {
        title.push_str(&format!(" · tool {} of {}", index + 1, calls().count()));
    }
    if !tool.status.is_empty() && !tool.rows.is_empty() {
        title.push_str(&format!(" · {}", tool.status));
    }
    chrome(
        frame,
        area,
        styles,
        &title,
        &[
            ("[ ]", "previous/next tool"),
            ("↑↓ pgup/pgdn", "scroll"),
            ("c", "copy"),
            ("esc", "back"),
        ],
    );
    let mut y = area.y + 1;
    let bottom = area.bottom().saturating_sub(1);
    let heading = match (call.name == "bash", call.detail.is_empty()) {
        (true, _) if !tool_input(call).is_empty() => "command",
        (_, false) => call.detail.as_str(),
        _ => "",
    };
    if !heading.is_empty() {
        let heading = styled(&sanitize(heading), styles.dim());
        frame.buffer_mut().set_line(area.x, y, &heading, area.width);
        y += 1;
    }
    for text in tool_input(call).lines() {
        if y >= bottom {
            return;
        }
        let line = styled(&sanitize(text), styles.dim());
        frame.buffer_mut().set_line(area.x, y, &line, area.width);
        y += 1;
    }
    if y < bottom {
        y += 1;
    }
    let rows = bottom.saturating_sub(y) as usize;
    if rows == 0 {
        return;
    }
    if tool.rows.is_empty() {
        let message = if tool.status.is_empty() {
            "No output."
        } else {
            tool.status.as_str()
        };
        frame
            .buffer_mut()
            .set_line(area.x, y, &styled(message, styles.dim()), area.width);
        return;
    }
    for (offset, row) in tool.rows.iter().skip(tool.scroll).take(rows).enumerate() {
        frame
            .buffer_mut()
            .set_line(area.x, y + offset as u16, &row.line(styles), area.width);
    }
}
