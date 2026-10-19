//! The bottom band: status line, queue, notice, composer and footer, as plain
//! rows so the composer can grow with its content.

use ratatui::text::{Line, Span};

use super::composer;
use super::theme::Styles;
use super::wrap;
use crate::app::{App, QueuedKind};
use crate::transcript::block::NoticeLevel;

/// Every row of the bottom band, top to bottom, and the composer cursor as
/// (row, column) within them.
pub fn band(app: &App, styles: Styles, width: u16) -> (Vec<Line<'static>>, (u16, u16)) {
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
    if let Some(notice) = app.notices.back() {
        let style = match notice.level {
            NoticeLevel::Error => styles.error(),
            NoticeLevel::Info => styles.dim(),
        };
        rows.push(Line::styled(notice.text.clone(), style));
    }
    let top = rows.len() as u16;
    let (draft, cursor) = composer::render(
        app.composer.text(),
        Some(app.composer.display_cursor()),
        styles,
        width,
    );
    rows.extend(draft);
    rows.push(footer(app, styles, width));
    (rows, (top + cursor.0, cursor.1))
}

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// The spinner glyph for a point in time. The main loop redraws on every tick
/// while something is in flight, so a frame derived from the clock animates.
pub fn spinner(elapsed: std::time::Duration) -> &'static str {
    SPINNER[(elapsed.as_millis() / 100 % SPINNER.len() as u128) as usize]
}

fn status_line(app: &App, styles: Styles, label: &str) -> Line<'static> {
    let live = app
        .session
        .as_ref()
        .and_then(|session| session.live.as_ref());
    let mut text = format!("{} ", spinner(app.started.elapsed()));
    // A running tool names itself, unless a prompt, a compaction or an
    // interrupt is what the user is waiting on.
    match live
        .filter(|live| !live.interrupting && !app.has_interaction() && !app.compacting())
        .and_then(|live| live.running_tool.as_ref())
    {
        Some((tool, detail)) => {
            text.push_str(&format!("Running {tool}"));
            if !detail.is_empty() {
                text.push(' ');
                text.push_str(&wrap::truncate(&detail.replace('\n', " "), 60));
            }
        }
        None => text.push_str(label),
    }
    if let Some(elapsed) = app.busy_elapsed() {
        text.push_str(" · ");
        text.push_str(&crate::app::format_duration(elapsed));
    }
    if let Some(live) = live {
        if live.requests > 1 {
            text.push_str(&format!(" · request {}", live.requests));
        }
        if live.tool_rounds > 0 {
            text.push_str(" · ");
            text.push_str(&crate::app::plural(live.tool_rounds, "tool"));
        }
    }
    if app.has_interaction() {
        // Esc answers the prompt with a denial; it does not cancel the turn.
        text.push_str(" · esc to deny");
    } else if app.is_busy() {
        text.push_str(" · esc to interrupt");
    }
    Line::styled(text, styles.accent())
}

/// Footer: `dir@branch · title · model · reasoning · ● state`, with the token
/// totals and context percent right-aligned. Unknown facts are left out
/// rather than shown as placeholders. When space is short the title goes
/// first, then the token totals; the context percent stays.
fn footer(app: &App, styles: Styles, width: u16) -> Line<'static> {
    let workspace = app
        .workspace
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| app.workspace.display().to_string());
    let session = app.session.as_ref();
    let branch = session
        .and_then(|session| session.presentation.as_ref())
        .and_then(|presentation| presentation.git_branch.as_deref())
        .filter(|branch| !branch.trim().is_empty());
    let mut place = workspace;
    if let Some(branch) = branch {
        place.push('@');
        place.push_str(branch);
    }
    let state = if app.is_busy() {
        "● working"
    } else if app.compacting() {
        "● compacting"
    } else if app.ready {
        "● ready"
    } else {
        "● connecting"
    };
    let title = session
        .and_then(|session| session.info.title.as_deref())
        .filter(|title| !title.trim().is_empty())
        .map(|title| wrap::truncate(title, 32));
    let settings = session.map(|session| {
        format!(
            "{} · {} · ",
            session.info.model,
            session.info.reasoning.label()
        )
    });
    let context = app.context_usage().percent.map_or_else(
        || "ctx ?".to_owned(),
        |percent| format!("ctx {percent:.0}%"),
    );
    let totals = session.map(|session| {
        let usage = &session.usage;
        let mut text = String::new();
        if usage.input + usage.output > 0 {
            text.push_str(&format!(
                "↑{} ↓{} ",
                crate::ui::format_tokens(usage.input),
                crate::ui::format_tokens(usage.output)
            ));
            if usage.cached > 0 {
                text.push_str(&format!("R{} ", crate::ui::format_tokens(usage.cached)));
            }
            text.push_str("· ");
        }
        text.push_str(&context);
        text
    });

    let width = width as usize;
    let left =
        wrap::width(&place) + 3 + settings.as_deref().map_or(0, wrap::width) + wrap::width(state);
    let fits = |right: &str, extra: usize| left + extra + wrap::width(right) + 2 <= width;
    let right = totals.map(|totals| {
        if fits(&totals, 0) {
            totals
        } else {
            context.clone()
        }
    });
    let title =
        title.filter(|title| fits(right.as_deref().unwrap_or_default(), wrap::width(title) + 3));

    let mut spans = vec![Span::styled(format!("{place} · "), styles.subtle())];
    if let Some(title) = title {
        spans.push(Span::styled(title, styles.dim()));
        spans.push(Span::styled(" · ", styles.subtle()));
    }
    if let Some(settings) = settings {
        spans.push(Span::styled(settings, styles.subtle()));
    }
    spans.push(Span::styled(
        state,
        if app.is_busy() {
            styles.accent()
        } else {
            styles.subtle()
        },
    ));
    if let Some(right) = right {
        let used: usize = spans.iter().map(|span| wrap::width(&span.content)).sum();
        let gap = width.saturating_sub(used + wrap::width(&right)).max(2);
        spans.push(Span::raw(" ".repeat(gap)));
        spans.push(Span::styled(right, styles.subtle()));
    }
    Line::from(spans)
}
