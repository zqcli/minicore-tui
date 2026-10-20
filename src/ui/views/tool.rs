//! The tool-output view: a card's input, then the streams it printed.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;

use crate::app::Session;
use crate::app::tool::{ToolRow, ToolView, tool_input};

use super::{chrome, styled};
use crate::ui::theme::Styles;
use crate::wrap::sanitize;

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

impl ToolRow {
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

pub fn render(
    frame: &mut Frame,
    area: Rect,
    styles: Styles,
    tool: &ToolView,
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
