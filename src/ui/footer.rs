//! The footer (development spec 15.8, 31): shortened workspace on the left,
//! session title or short id on the right, a status word, and
//! `model • reasoning`. Nothing fabricated is ever shown (no token or cost).

use std::path::{Path, PathBuf};

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::{App, ConnectionState};
use crate::markdown::column_width;
use crate::protocol::Reasoning;
use crate::state::session::SessionView;
use crate::state::tool::ToolStatus;
use crate::theme::Theme;
use crate::ui::layout;

pub fn render(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let view = app.active_view();
    let width = area.width as usize;
    let two_rows = area.height > 1 && width >= 80;

    let (status_word, status_color) = status_line(app, view, theme);
    let model = view.map(|view| {
        format!(
            "{} • {}",
            view.info.model,
            reasoning_label(view.info.reasoning)
        )
    });

    if two_rows {
        let workspace = shorten_workspace(&app.catalogs.default_workspace, home_dir().as_deref());
        let right = view
            .map(title_or_short_id)
            .unwrap_or_else(|| "no session".to_owned());
        let row0 = sides_line(&workspace, &right, width, theme.dim, theme.dim);
        let row1 = model
            .map(|model| sides_line(&status_word, &model, width, status_color, theme.muted))
            .unwrap_or_else(|| sides_line(&status_word, "", width, status_color, theme.dim));
        let lines = vec![row0, row1];
        frame.render_widget(Paragraph::new(lines), area);
    } else {
        let line = model
            .map(|model| sides_line(&status_word, &model, width, status_color, theme.muted))
            .unwrap_or_else(|| sides_line(&status_word, "", width, status_color, theme.dim));
        frame.render_widget(Paragraph::new(vec![line]), area);
    }
}

/// One footer row with the right side anchored to the edge.
fn sides_line(
    left: &str,
    right: &str,
    width: usize,
    left_color: ratatui::style::Color,
    right_color: ratatui::style::Color,
) -> Line<'static> {
    let right_w = column_width(right);
    let left = layout::truncate(left, width.saturating_sub(right_w).saturating_sub(1));
    let left_w = column_width(&left);
    let gap = width.saturating_sub(left_w + right_w);
    Line::from(vec![
        Span::styled(left.to_owned(), Style::new().fg(left_color)),
        Span::styled(" ".repeat(gap), Style::new()),
        Span::styled(right.to_owned(), Style::new().fg(right_color)),
    ])
}

fn status_line(
    app: &App,
    view: Option<&SessionView>,
    theme: &Theme,
) -> (String, ratatui::style::Color) {
    match app.connection {
        ConnectionState::Starting => ("Starting".to_owned(), theme.dim),
        ConnectionState::ShuttingDown => ("Shutting down".to_owned(), theme.dim),
        ConnectionState::Failed(_) => ("Disconnected".to_owned(), theme.error),
        ConnectionState::Ready => match view {
            None => ("Idle".to_owned(), theme.dim),
            Some(view) => {
                if view.event_gap {
                    return ("⚠ live output incomplete".to_owned(), theme.warning);
                }
                if let Some(live) = &view.live {
                    if live.cancel_requested {
                        ("Cancelling".to_owned(), theme.dim)
                    } else if let Some(tool) = live.tools.iter().find(|tool| {
                        matches!(tool.status, ToolStatus::Pending | ToolStatus::Running)
                    }) {
                        (format!("Running {}", tool.name), theme.dim)
                    } else {
                        ("Streaming".to_owned(), theme.dim)
                    }
                } else {
                    ("Idle".to_owned(), theme.dim)
                }
            }
        },
    }
}

fn reasoning_label(reasoning: Reasoning) -> &'static str {
    match reasoning {
        Reasoning::Disabled => "disabled",
        Reasoning::Auto => "auto",
        Reasoning::Low => "low",
        Reasoning::Medium => "medium",
        Reasoning::High => "high",
    }
}

fn title_or_short_id(view: &SessionView) -> String {
    match &view.info.title {
        Some(title) if !title.is_empty() => title.clone(),
        _ => view.info.session_id.chars().take(8).collect(),
    }
}

/// The user's home directory for workspace shortening; `None` keeps paths
/// unshortened (no canonicalization anywhere, spec 31.1).
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
}

/// `/home/user/project` under home `/home/user` becomes `~/project`. The
/// prefix must end at a path component; no canonicalization is performed.
pub fn shorten_workspace(path: &Path, home: Option<&Path>) -> String {
    let Some(home) = home else {
        return path.to_string_lossy().into_owned();
    };
    let path_text = path.to_string_lossy();
    let home_text = home.to_string_lossy();
    if path == home {
        return "~".to_owned();
    }
    if let Some(rest) = path_text.strip_prefix(home_text.as_ref()) {
        if rest.is_empty() {
            return "~".to_owned();
        }
        if rest.starts_with('/') || rest.starts_with('\\') {
            return format!("~{rest}");
        }
    }
    path_text.into_owned()
}
