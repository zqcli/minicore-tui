//! The status line, the queue, notices, the composer band and the footer.
//!
//! All of them are plain rows in one vertical list: the app renders the
//! transcript above and this band below, so the composer can grow with its
//! content without a second layout pass.

use ratatui::text::{Line, Span};

use super::composer;
use super::theme::Styles;
use super::wrap;
use crate::app::{App, QueuedKind};
use crate::transcript::block::NoticeLevel;

/// Every row of the bottom band, top to bottom.
pub fn lines(app: &App, styles: Styles, width: u16) -> Vec<Line<'static>> {
    let mut rows = Vec::new();
    if let Some(label) = app.status_label() {
        rows.push(status_line(app, styles, label));
    }
    for queued in app
        .session
        .as_ref()
        .map(|session| session.queue.iter().take(3))
        .into_iter()
        .flatten()
    {
        let glyph = match queued.kind {
            QueuedKind::Steer => "↳",
            QueuedKind::Follow => "⏵",
        };
        rows.push(Line::styled(
            format!(
                "{glyph} {} · {}",
                queued.label(),
                wrap::truncate(&queued.text.replace('\n', " "), 60)
            ),
            styles.dim(),
        ));
    }
    if let Some(session) = app.session.as_ref()
        && session.queue.len() > 3
    {
        rows.push(Line::styled(
            format!("  +{} more queued", session.queue.len() - 3),
            styles.subtle(),
        ));
    }
    // The approval prompt is a bottom sheet (screens::sheet), not a band row.
    if let Some(notice) = app.notices.back() {
        let style = match notice.level {
            NoticeLevel::Error => styles.error(),
            NoticeLevel::Info => styles.dim(),
        };
        rows.push(Line::styled(notice.text.clone(), style));
    }
    let (draft, _) = composer::render(
        app.composer.text(),
        Some(app.composer.display_cursor()),
        styles,
        width,
    );
    rows.extend(draft);
    rows.push(footer(app, styles));
    rows
}

/// Where the terminal cursor sits: absolute rows from the top of the band.
pub fn cursor(app: &App, styles: Styles, width: u16) -> (u16, u16) {
    let prefix = lines_before_composer(app);
    let (_, cursor) = composer::render(
        app.composer.text(),
        Some(app.composer.display_cursor()),
        styles,
        width,
    );
    (prefix + cursor.0, cursor.1)
}

fn lines_before_composer(app: &App) -> u16 {
    let status = u16::from(app.status_label().is_some());
    let queue = app.session.as_ref().map_or(0, |session| {
        session.queue.len().min(3) as u16 + u16::from(session.queue.len() > 3)
    });
    let interaction = app.interaction_lines().len() as u16;
    let notice = u16::from(!app.notices.is_empty());
    status + queue + interaction + notice
}

fn status_line(app: &App, styles: Styles, label: &str) -> Line<'static> {
    let mut text = String::new();
    if app.is_busy() && !app.compacting() {
        text.push_str("⠋ ");
    }
    text.push_str(label);
    if let Some(phase) = app
        .session
        .as_ref()
        .and_then(|session| session.live.as_ref())
        .and_then(|live| live.running_tool.as_ref())
        && label.starts_with("Running")
        && !phase.0.is_empty()
    {
        text.push(' ');
        text.push_str(&phase.0);
    }
    if let Some(elapsed) = app.busy_elapsed() {
        text.push_str(" · ");
        text.push_str(&crate::app::format_duration(elapsed));
    }
    if let Some(session) = app.session.as_ref()
        && let Some(live) = session.live.as_ref()
    {
        if live.requests > 1 {
            text.push_str(&format!(" · request {}", live.requests));
        }
        if live.tool_rounds > 0 {
            text.push_str(&format!(" · {} tools", live.tool_rounds));
        }
        if live.interrupting {
            text.push_str(" · interrupting");
        }
    }
    if app.is_busy() {
        text.push_str(" · esc to interrupt");
    }
    Line::styled(text, styles.accent())
}

/// Footer: workspace@branch · session · model · reasoning · state · tokens.
fn footer(app: &App, styles: Styles) -> Line<'static> {
    let workspace = app
        .workspace
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| app.workspace.display().to_string());
    let branch = app
        .session
        .as_ref()
        .and_then(|session| session.presentation.as_ref())
        .and_then(|presentation| presentation.git_branch.clone())
        .unwrap_or_else(|| "-".into());
    let model = app
        .session
        .as_ref()
        .map_or("-".to_owned(), |session| session.info.model.clone());
    let reasoning = app
        .session
        .as_ref()
        .map_or("-", |session| session.info.reasoning.label());
    let state = if app.is_busy() {
        "● working"
    } else if app.compacting() {
        "● compacting"
    } else if app.ready {
        "● ready"
    } else {
        "● connecting"
    };
    let mut spans = vec![Span::styled(
        format!("{workspace}@{branch} · {model} · {reasoning} · "),
        styles.subtle(),
    )];
    spans.push(Span::styled(
        state,
        if app.is_busy() {
            styles.accent()
        } else {
            styles.subtle()
        },
    ));
    if let Some(usage) = app.session.as_ref().map(|session| &session.usage)
        && usage.input + usage.output > 0
    {
        let context = app
            .context_usage()
            .percent
            .map(|percent| format!("{percent:.0}%"))
            .unwrap_or_else(|| "?".into());
        spans.push(Span::styled(
            format!(
                "   ↑{} ↓{} · ctx {context}",
                crate::ui::tokens(usage.input),
                crate::ui::tokens(usage.output),
            ),
            styles.subtle(),
        ));
    }
    Line::from(spans)
}
