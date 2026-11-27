//! Full-screen views: diff, help, error details, context and session facts. Each
//! replaces the transcript area right of its gutter and keeps the footer; the
//! app owns scrolling and selection.

pub mod context;
pub mod diff;
mod error;
pub mod help;
pub mod page;
pub mod remote;
pub mod split;
pub mod tasks;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use crate::app::commands::COMMANDS;
use crate::app::{App, Overlay};

use super::chrome as ui_chrome;
use super::theme::Styles;

pub use ui_chrome::{key_hints, styled};

/// The title row with its context in meta colour, and the bottom key hints.
pub(super) fn chrome(
    frame: &mut Frame,
    area: Rect,
    styles: Styles,
    title: &str,
    hints: &[(&str, &str)],
) {
    let (name, context) = title.split_once(" · ").unwrap_or((title, ""));
    let mut spans = vec![
        Span::styled("← ", styles.meta()),
        Span::styled(name.to_owned(), styles.bold()),
    ];
    if !context.is_empty() {
        spans.push(Span::styled(format!(" · {context}"), styles.meta()));
    }
    let buffer = frame.buffer_mut();
    buffer.set_line(area.x, area.y, &Line::from(spans), area.width);
    if area.height > 1 {
        buffer.set_line(
            area.x,
            area.bottom() - 1,
            &Line::from(key_hints(hints, styles)),
            area.width,
        );
    }
}

/// Draws whichever view owns the transcript area; false when none is open.
pub fn draw(frame: &mut Frame, area: Rect, app: &mut App, styles: Styles) -> bool {
    let area = super::transcript::inner(area);
    if let Overlay::SessionInfo { scroll } = app.overlay {
        let lines = session_info_lines(
            app,
            styles,
            usize::from(area.width.saturating_sub(2)).max(1),
        );
        let (clamped, _) = page::render(
            frame,
            area,
            styles,
            "Session",
            &[
                ("c", "copy id"),
                ("r", "refresh view"),
                ("l/u", "load/unload"),
                ("x", "close"),
                ("esc", "back"),
            ],
            None,
            &lines,
            scroll,
        );
        if let Overlay::SessionInfo { scroll } = &mut app.overlay {
            *scroll = clamped;
        }
        return true;
    }
    if let Overlay::Remote { show_key, scroll } = app.overlay {
        let clamped = remote::render(frame, area, styles, app, show_key, scroll);
        if let Overlay::Remote { scroll, .. } = &mut app.overlay {
            *scroll = clamped;
        }
        return true;
    }
    match &mut app.overlay {
        Overlay::Diff(diff) => diff::render(frame, area, styles, diff),
        Overlay::Help { scroll, filter } => {
            help::render(frame, area, styles, COMMANDS, scroll, filter)
        }
        Overlay::Delegations { .. } => {
            tasks::render(frame, area, styles, app);
        }
        Overlay::Error {
            text,
            scroll,
            visible_rows,
        } => error::render(frame, area, styles, text, scroll, visible_rows),
        Overlay::Context { scroll } => {
            context::render(frame, area, styles, app.resident.current(), scroll);
        }
        _ => return false,
    }
    true
}

fn session_info_lines(app: &App, styles: Styles, width: usize) -> Vec<Line<'static>> {
    let Some(session) = app.resident.current() else {
        return vec![styled("No session is open", styles.dim())];
    };
    let info = &session.info;
    let tokens = session
        .context
        .as_ref()
        .and_then(|context| context.data.history.as_ref())
        .and_then(|history| history.latest_response.as_ref())
        .map_or_else(
            || "not reported".to_owned(),
            |response| context::usage_text(&response.usage),
        );
    let facts = vec![
        ("id", info.id.to_string(), None),
        (
            "agent",
            format!(
                "{} · {}",
                super::sheets::agent_label(app, &info.agent_id),
                info.agent_id
            ),
            None,
        ),
        (
            "title",
            info.title
                .clone()
                .unwrap_or_else(|| "Untitled session".into()),
            None,
        ),
        (
            "workspace",
            info.config
                .workspace
                .as_ref()
                .map_or_else(|| "none".into(), |workspace| workspace.path.clone()),
            None,
        ),
        (
            "storage",
            super::sheets::storage_label(info.mode).to_owned(),
            None,
        ),
        (
            "lifecycle",
            format!("{:?}", session.state.data.lifecycle).to_lowercase(),
            None,
        ),
        (
            "residency",
            format!("{:?}", session.state.data.residency).to_lowercase(),
            None,
        ),
        (
            "scheduling",
            format!("{:?}", session.state.data.scheduling).to_lowercase(),
            None,
        ),
        (
            "approval",
            match info.config.execution.approval {
                minicore_host_protocol::ApprovalMode::Ask => "ask",
                minicore_host_protocol::ApprovalMode::Auto => "auto",
            }
            .to_owned(),
            None,
        ),
        (
            "model",
            format!(
                "{} · {}",
                info.config.model,
                super::sheets::reasoning_label(info.config.reasoning)
            ),
            None,
        ),
        (
            "items",
            session
                .context
                .as_ref()
                .and_then(|context| context.data.history.as_ref())
                .map(|history| history.raw_items.to_string())
                .unwrap_or_else(|| "unknown".to_owned()),
            None,
        ),
        ("last usage", tokens, None),
    ];
    ui_chrome::kv_lines(&facts, styles, width)
}

#[cfg(test)]
fn capture(draw: impl FnOnce(&mut Frame), width: u16, height: u16) -> Vec<String> {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| draw(frame)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    buffer
        .content()
        .chunks(width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect()
}
