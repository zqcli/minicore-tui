//! The busy status row (development spec 15.6): a 10-frame spinner plus a
//! Working / `Running <tool>` / Cancelling label, only rendered while busy.

use ratatui::Frame;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::state::tool::ToolStatus;
use crate::theme::Theme;

/// Spinner frames advance with `App.frame_count` via `AppEvent::Tick`.
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn render(frame: &mut Frame, area: ratatui::layout::Rect, app: &App, theme: &Theme) {
    let frame_index = (app.frame_count % SPINNER.len() as u64) as usize;
    let line = Line::from(vec![
        Span::styled(SPINNER[frame_index], Style::new().fg(theme.accent)),
        Span::styled(
            format!(" {}", busy_label(app)),
            Style::new().fg(theme.muted),
        ),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn busy_label(app: &App) -> String {
    let Some(view) = app.active_view() else {
        return "Working".to_owned();
    };
    let Some(live) = &view.live else {
        return "Working".to_owned();
    };
    if live.cancel_requested {
        return "Cancelling".to_owned();
    }
    if let Some(tool) = live
        .tools
        .iter()
        .find(|tool| matches!(tool.status, ToolStatus::Pending | ToolStatus::Running))
    {
        return format!("Running {}…", tool.name);
    }
    "Working".to_owned()
}
