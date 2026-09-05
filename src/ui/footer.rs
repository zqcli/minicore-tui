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
use crate::state::selection::reasoning_label;
use crate::state::session::SessionView;
use crate::state::tool::ToolStatus;
use crate::theme::Theme;
use crate::ui::layout;
use crate::ui::status::{result_color, result_summary};

pub fn render(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let view = app.active_view();
    let width = area.width as usize;
    let two_rows = area.height > 1 && width >= 80;

    let (status_word, status_color) = status_line(app, view, theme);
    let model = view.map(|view| {
        if view.unsaved_loop.is_some() {
            "unsaved turn".to_owned()
        } else {
            let current_desc =
                if let Some(request) = view.live.as_ref().and_then(|live| live.requests.last()) {
                    if request.model.is_empty() {
                        format!("request {} · config unknown", request.request_index)
                    } else {
                        format!(
                            "request {} · {} · {} · rev {}",
                            request.request_index,
                            request.model,
                            reasoning_label(request.reasoning),
                            request.config_revision,
                        )
                    }
                } else if let Some(request) = view.last_request.as_ref() {
                    format!(
                        "request {} · {} · {} · rev {}",
                        request.request_index,
                        request.model,
                        reasoning_label(request.reasoning),
                        request.revision,
                    )
                } else if let Some(loop_state) = view
                    .state
                    .as_ref()
                    .and_then(|state| state.active_loop.as_ref())
                {
                    format!("request {} · config unknown", loop_state.request_index)
                } else {
                    format!(
                        "{} • {}",
                        view.info.model,
                        reasoning_label(view.info.reasoning)
                    )
                };

            if let Some(update) = &view.config_update {
                if update.state == crate::state::session::ConfigUpdateState::WaitingBoundary {
                    let live_loop_matches = match (&update.loop_id, &view.live) {
                        (Some(target), Some(live)) => live
                            .reference
                            .as_ref()
                            .is_some_and(|r| &r.loop_id == target),
                        (None, _) => true,
                        _ => false,
                    };
                    if live_loop_matches {
                        let next_config = match (&update.model, update.reasoning) {
                            (Some(m), Some(r)) => format!("{} • {}", m, reasoning_label(r)),
                            (Some(m), None) => m.clone(),
                            (None, Some(r)) => reasoning_label(r).to_string(),
                            (None, None) => String::new(),
                        };
                        if !next_config.is_empty() {
                            let next_str = if let Some(rev) = update.revision {
                                format!("next: {next_config} · rev {rev}")
                            } else {
                                format!("next: {next_config}")
                            };
                            return format!("{current_desc}      {next_str}");
                        }
                    }
                }
            }
            current_desc
        }
    });

    if two_rows {
        let home = home_dir();
        let workspace = view
            .map(|view| shorten_workspace(Path::new(&view.info.workspace), home.as_deref()))
            .unwrap_or_else(|| shorten_workspace(&app.catalogs.default_workspace, home.as_deref()));
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
                if let Some(state) = view.state.as_ref() {
                    match state.status {
                        crate::protocol::SessionStatusWire::WaitingForInput => {
                            return ("Waiting for input".to_owned(), theme.warning);
                        }
                        crate::protocol::SessionStatusWire::Finishing => {
                            if view.can_show_last_result() {
                                if let Some(result) = view.last_result.as_ref() {
                                    return (result_summary(result), result_color(result, theme));
                                }
                            }
                            return ("Saving".to_owned(), theme.dim);
                        }
                        crate::protocol::SessionStatusWire::Blocked => {
                            let reason = match state.block_reason {
                                Some(crate::protocol::SessionBlockReasonWire::Persistence) => {
                                    "persistence"
                                }
                                Some(crate::protocol::SessionBlockReasonWire::Internal) => {
                                    "internal"
                                }
                                None => "unknown",
                            };
                            let label = format!("Blocked · {reason}");
                            return if view.can_show_last_result() {
                                if let Some(result) = view.last_result.as_ref() {
                                    (format!("{label} · {}", result_summary(result)), theme.error)
                                } else {
                                    (label, theme.error)
                                }
                            } else {
                                (label, theme.error)
                            };
                        }
                        crate::protocol::SessionStatusWire::Idle
                        | crate::protocol::SessionStatusWire::Running => {}
                    }
                }
                if view.can_show_last_result() {
                    if let Some(result) = view.last_result.as_ref() {
                        let summary = result_summary(result);
                        return if view.event_gap {
                            (format!("⚠ {summary}"), theme.warning)
                        } else {
                            (summary, result_color(result, theme))
                        };
                    }
                }
                if view.event_gap {
                    return ("⚠ live output incomplete".to_owned(), theme.warning);
                }
                if view.live.as_ref().is_some_and(|live| live.waiting) {
                    return ("Result unconfirmed".to_owned(), theme.warning);
                }
                if let Some(live) = &view.live {
                    if live.cancel_requested {
                        ("Cancelling".to_owned(), theme.dim)
                    } else if let Some(tool) = live
                        .requests
                        .iter()
                        .flat_map(|request| request.tools.iter())
                        .find(|tool| {
                            matches!(tool.status, ToolStatus::Pending | ToolStatus::Running)
                        })
                    {
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
