//! The tool-output view: a card's input, then the streams it printed.

use std::ops::Range;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;

use crate::app::Session;
use crate::app::tool::{ToolRow, ToolView, tool_input};
use crate::transcript::ToolCall;

use super::{chrome, right_edge, styled};
use crate::ui::theme::Styles;
use crate::wrap::{self, sanitize};

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

/// The title row's right part: the place in the output and the jump buttons,
/// with the columns the buttons occupy for the click hit test.
pub struct Controls {
    pub text: String,
    pub top: Range<u16>,
    pub bottom: Range<u16>,
}

/// The controls for an output of `rows` rows; none while it fits.
pub fn controls(area: Rect, tool: &ToolView, rows: usize) -> Controls {
    let total = tool.rows.len();
    let mut controls = Controls {
        text: String::new(),
        top: 0..0,
        bottom: 0..0,
    };
    if total <= rows {
        return controls;
    }
    let last = (tool.scroll + rows).min(total);
    let mut text = format!("{}–{last} of {total}", tool.scroll + 1);
    // The button's columns within `text`, which `right_edge` then places.
    fn button(text: &mut String, label: &str) -> Range<usize> {
        text.push_str(" · ");
        let start = wrap::width(text);
        text.push_str(label);
        start..wrap::width(text)
    }
    let top = (tool.scroll > 0).then(|| button(&mut text, "↑ top"));
    let bottom = (last < total).then(|| button(&mut text, "↓ bottom"));
    let x = right_edge(area, &text) as usize;
    let place = |range: Option<Range<usize>>| {
        range.map_or(0..0, |range| {
            (x + range.start) as u16..(x + range.end) as u16
        })
    };
    controls.top = place(top);
    controls.bottom = place(bottom);
    controls.text = text;
    controls
}

/// The rows of `area` that show the output: under the heading and the input,
/// above the hint row.
pub fn output_span(area: Rect, call: &ToolCall) -> Range<u16> {
    let bottom = area.bottom().saturating_sub(1);
    let above = 1 + usize::from(!heading(call).is_empty()) + tool_input(call).lines().count();
    let end = (area.y as usize + above).min(bottom as usize) as u16;
    // A blank row separates the input from the output when there is room.
    end + u16::from(end < bottom)..bottom
}

fn heading(call: &ToolCall) -> &str {
    match (call.name == "bash", call.detail.is_empty()) {
        (true, _) if !tool_input(call).is_empty() => "command",
        (_, false) => call.detail.as_str(),
        _ => "",
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
    let output = output_span(area, call);
    chrome(
        frame,
        area,
        styles,
        &title,
        &controls(area, tool, output.len()).text,
        &[
            ("[ ]", "prev/next"),
            ("↑↓ pgup/pgdn", "scroll"),
            ("home/end", "top/bottom"),
            ("c", "copy"),
            ("esc", "back"),
        ],
    );
    let heading = Some(heading(call)).filter(|heading| !heading.is_empty());
    let above = heading.into_iter().chain(tool_input(call).lines());
    for (y, text) in (area.y + 1..output.start).zip(above) {
        let line = styled(&sanitize(text), styles.dim());
        frame.buffer_mut().set_line(area.x, y, &line, area.width);
    }
    if output.is_empty() {
        return;
    }
    if tool.rows.is_empty() {
        let message = if tool.status.is_empty() {
            "No output."
        } else {
            tool.status.as_str()
        };
        frame.buffer_mut().set_line(
            area.x,
            output.start,
            &styled(message, styles.dim()),
            area.width,
        );
        return;
    }
    for (y, row) in output.zip(tool.rows.iter().skip(tool.scroll)) {
        frame
            .buffer_mut()
            .set_line(area.x, y, &row.line(styles), area.width);
    }
}
