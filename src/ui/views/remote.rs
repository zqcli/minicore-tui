//! The remote status screen: while remote control is on it takes the
//! transcript's place and says how to connect and what the Agent is doing.

use std::path::Path;
use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::app::App;
use crate::app::overlay::session_label;
use crate::remote::whole_seconds;
use crate::transcript::Block;
use crate::ui::theme::Styles;
use crate::ui::{blocks, put_line, status, transcript};
use crate::wrap;

/// Cells before a label.
const INDENT: usize = 3;

pub fn draw(frame: &mut Frame, area: Rect, app: &App, styles: Styles) {
    let area = transcript::inner(area);
    let width = usize::from(area.width);
    let remote = &app.remote;
    let session = app.session.as_ref();

    let (state, state_style) = match remote.phones.as_slice() {
        [] => ("○ waiting for a phone".to_owned(), styles.warning()),
        [phone] => {
            let device = wrap::truncate(&wrap::sanitize(&phone.connection.device.name), 24);
            (format!("● {device} connected"), styles.success())
        }
        phones => (
            format!("● {} phones connected", phones.len()),
            styles.success(),
        ),
    };
    let title = " Remote control";
    let gap = width.saturating_sub(wrap::width(title) + wrap::width(&state) + 1);
    let mut lines = vec![
        Line::from(vec![
            Span::styled(title, styles.bold()),
            Span::raw(" ".repeat(gap.max(1))),
            Span::styled(state, state_style),
        ]),
        Line::default(),
    ];

    let text = styles.text();
    let mut listening: Vec<_> = remote.listening.iter().map(ToString::to_string).collect();
    if remote.tls {
        listening.push("TLS".into());
    }
    let mut connect = vec![
        (
            "Address",
            remote.url().expect("remote is on").to_owned(),
            text,
        ),
        ("Listening", listening.join(" · "), text),
        ("Password", remote.password().unwrap_or_default(), text),
    ];
    if remote.phones.is_empty() {
        let none = "none yet · open the address on your phone and enter the password";
        connect.push(("Phones", none.to_owned(), text));
    }
    for (i, phone) in remote.phones.iter().enumerate() {
        let viewing = phone.view.session_id().map_or_else(
            || "new session".to_owned(),
            |id| {
                let session = app.live(id).expect("a phone views a live session");
                session_label(&session.info, Some(session))
            },
        );
        let device = format!(
            "{} · {} · connected at {}",
            phone.connection.device.name,
            viewing,
            blocks::clock_label(phone.connected_at)
        );
        connect.push((if i == 0 { "Phones" } else { "" }, device, text));
    }

    let workspace = session.map_or(app.workspace.as_path(), |session| {
        Path::new(&session.info.workspace)
    });
    let model = [
        app.model_id().map(str::to_owned),
        app.reasoning().map(|level| level.label().to_owned()),
    ];
    let mut work = vec![
        (
            "Session",
            session.map_or_else(
                || "New session · starts with the phone's first message".to_owned(),
                |session| session_label(&session.info, Some(session)),
            ),
            text,
        ),
        (
            "Workspace",
            transcript::home_relative(workspace).display().to_string(),
            text,
        ),
        (
            "Model",
            model.into_iter().flatten().collect::<Vec<_>>().join(" · "),
            text,
        ),
        ("Now", activity(app), text),
    ];
    if let Some(waiting) = session
        .map(|session| session.queue.items.len() as u64)
        .filter(|waiting| *waiting > 0)
    {
        let queue = format!("{} waiting", blocks::plural(waiting, "message"));
        work.push(("Queue", queue, text));
    }
    let others: String = status::others(app, styles)
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    if !others.is_empty() {
        work.push(("Others", others, text));
    }
    if remote.failed_logins > 0 {
        let mut sign_in = blocks::plural(u64::from(remote.failed_logins), "wrong password");
        let now = tokio::time::Instant::now();
        if let Some(until) = remote.paused_until.filter(|until| *until > now) {
            sign_in.push_str(&format!(" · paused for {} s", whole_seconds(until - now)));
        }
        work.push(("Sign-in", sign_in, styles.warning()));
    }

    let column = connect
        .iter()
        .chain(&work)
        .map(|(label, _, _)| label.len())
        .max()
        .unwrap_or(0)
        + 2;
    let room = width.saturating_sub(INDENT + column);
    for group in [connect, work] {
        for (label, value, style) in group {
            lines.push(row(label, &value, style, column, room, styles));
        }
        lines.push(Line::default());
    }

    let mut hints = vec![("/remote off", "take back control")];
    if app.is_busy() || app.compacting() {
        hints.push(("esc", "stop the turn"));
    }
    hints.push(("/remote password", "new password"));
    // Whole hints give way from the end rather than being cut mid-word.
    let mut spans = super::key_hints(&hints, styles);
    while hints.len() > 1 && status::spans_width(&spans) > width.saturating_sub(INDENT) {
        hints.pop();
        spans = super::key_hints(&hints, styles);
    }
    let mut hint_row = vec![Span::raw(" ".repeat(INDENT))];
    hint_row.extend(spans);
    lines.push(Line::from(hint_row));

    for (y, line) in (area.y..area.bottom()).zip(&lines) {
        put_line(
            frame.buffer_mut(),
            Rect {
                y,
                height: 1,
                ..area
            },
            line,
        );
    }
    transcript::draw_flash(frame, area, app, styles);
}

/// A dim label in its column, then the value cut to `room` cells.
fn row(
    label: &str,
    value: &str,
    style: Style,
    column: usize,
    room: usize,
    styles: Styles,
) -> Line<'static> {
    let value = wrap::sanitize(&value.replace('\n', " ")).into_owned();
    Line::from(vec![
        Span::raw(" ".repeat(INDENT)),
        Span::styled(format!("{label:<column$}"), styles.dim()),
        Span::styled(wrap::truncate(&value, room), style),
    ])
}

/// What the Agent is doing, with the Working line's spinner while it works.
fn activity(app: &App) -> String {
    let session = app.session.as_ref();
    let spinner = status::spinner(app.started.elapsed());
    let elapsed = app
        .busy_elapsed()
        .map(|elapsed| format!(" · {}", seconds(elapsed)))
        .unwrap_or_default();
    if app.compacting() {
        return format!("{spinner} Compacting context");
    }
    if let Some(session) = session
        && let Some(interaction) = &session.interaction
    {
        // The card has the detail; the prompt can arrive before it does.
        let call = session
            .transcript
            .find_tool_by_call_id(&interaction.tool_call_id);
        let tool = call.map_or(interaction.tool_name.as_str(), |call| call.name.as_str());
        let mut text = format!("{spinner} Waiting for approval on the phone · {tool}");
        if let Some(detail) = call
            .map(|call| call.detail.as_str())
            .filter(|d| !d.is_empty())
        {
            text.push_str(&format!(" · {detail}"));
        }
        return text;
    }
    let live = session.and_then(|session| session.live.as_ref());
    if let Some((tool, detail)) = live
        .filter(|live| !live.interrupting)
        .and_then(|live| live.running_tool.as_ref())
    {
        let detail = match detail.is_empty() {
            true => String::new(),
            false => format!(" · {detail}"),
        };
        return format!("{spinner} Running {tool}{detail}{elapsed}");
    }
    if let Some(label) = app.status_label() {
        return format!("{spinner} {label}{elapsed}");
    }
    let last = app
        .transcript_blocks()
        .iter()
        .rev()
        .find_map(|block| match block {
            Block::TurnEnd {
                outcome, duration, ..
            } => Some((outcome.label(), *duration)),
            _ => None,
        });
    match last {
        Some((outcome, Some(duration))) => format!(
            "Idle · last turn {outcome} in {}",
            blocks::format_duration(duration)
        ),
        Some((outcome, None)) => format!("Idle · last turn {outcome}"),
        None => "Idle".to_owned(),
    }
}

/// Whole seconds of a running turn: `4s`, `3m07s`.
fn seconds(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    match seconds {
        0..60 => format!("{seconds}s"),
        _ => format!("{}m{:02}s", seconds / 60, seconds % 60),
    }
}
