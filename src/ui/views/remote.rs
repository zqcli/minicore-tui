//! Full-screen `/remote` connection and live status overlay.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use crate::app::{App, overlay};
use crate::ui::chrome;
use crate::ui::theme::Styles;

use super::styled;

pub fn render(
    frame: &mut Frame,
    area: Rect,
    styles: Styles,
    app: &App,
    show_key: bool,
    scroll: usize,
) -> usize {
    let Some(remote) = &app.remote else {
        let lines = vec![styled("Remote access is not running", styles.dim())];
        return super::page::render(
            frame,
            area,
            styles,
            "Remote",
            &[("esc", "back")],
            None,
            &lines,
            scroll,
        )
        .0;
    };

    let proto = if remote.tls {
        "Remote WSS"
    } else {
        "Remote WS"
    };
    let badge = if remote.status.operator_is_network {
        if remote.status.connections > 1 {
            format!("web operating · {} clients", remote.status.connections)
        } else {
            "web operating".to_owned()
        }
    } else if remote.status.connections > 0 {
        format!("{} connected (read-only)", remote.status.connections)
    } else {
        "waiting for web".to_owned()
    };
    let title = format!("{proto} · {badge}");

    let content_w = usize::from(area.width.saturating_sub(2)).max(1);
    let lines = build_lines(app, remote, show_key, styles, content_w);
    let toggle_label = if show_key { "hide key" } else { "show key" };
    let hints = [
        ("c", "copy key"),
        ("u", "copy url"),
        ("s", toggle_label),
        ("r", "rotate key"),
        ("esc/q", "stop & return"),
    ];
    super::page::render(frame, area, styles, &title, &hints, None, &lines, scroll).0
}

fn build_lines(
    app: &App,
    remote: &crate::host::RemoteServer,
    show_key: bool,
    styles: Styles,
    width: usize,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    // Live connection state banner
    if remote.status.operator_is_network {
        lines.push(Line::from(vec![
            Span::styled("● ", styles.success()),
            Span::styled("Web client is operating this Host", styles.bold()),
            Span::styled(" · terminal input paused", styles.meta()),
        ]));
    } else if remote.status.connections > 0 {
        lines.push(Line::from(vec![
            Span::styled("● ", styles.accent()),
            Span::styled(
                format!(
                    "{} client(s) connected (read-only)",
                    remote.status.connections
                ),
                styles.text(),
            ),
        ]));
    } else {
        lines.push(Line::from(vec![
            Span::styled("○ ", styles.dim()),
            Span::styled(
                "Waiting for minicore-web to connect · terminal control handed off",
                styles.meta(),
            ),
        ]));
    }

    lines.push(Line::default());
    lines.push(styled("Connection", styles.dim()));

    let listening = if remote.tls {
        format!("{} · TLS", remote.listen_addr)
    } else {
        format!("{} · WS (reverse proxy / CF -> WSS)", remote.listen_addr)
    };
    let key_display = if show_key {
        remote.access_key.clone()
    } else {
        "•".repeat(remote.access_key.len())
    };
    let clients = match (remote.status.connections, remote.status.operator_is_network) {
        (0, _) => "none".to_owned(),
        (1, true) => "1 connected · operating".to_owned(),
        (n, true) => format!("{n} connected · 1 operating"),
        (n, false) => format!("{n} connected · read-only"),
    };
    let control = if remote.status.operator_is_network {
        "web (operator) · terminal read-only"
    } else {
        "available for web · terminal read-only"
    };

    let connection_facts = vec![
        ("address", remote.display_url.clone(), Some(styles.accent())),
        ("listening", listening, None),
        (
            "access key",
            key_display,
            Some(if show_key {
                styles.bold()
            } else {
                styles.dim()
            }),
        ),
        ("clients", clients, None),
        ("control", control.to_owned(), None),
    ];
    lines.extend(chrome::kv_lines(&connection_facts, styles, width));

    lines.push(Line::default());
    lines.push(styled("Live Host", styles.dim()));

    let (session_label, workspace_label, model_label) = match app.resident.current() {
        Some(session) => {
            let label = match &session.info.title {
                Some(title) if !title.trim().is_empty() => format!(
                    "{title} · {}",
                    overlay::short_session_id(session.info.id.as_str())
                ),
                _ => session.info.id.to_string(),
            };
            let ws = session
                .info
                .config
                .workspace
                .as_ref()
                .map_or_else(|| "none".to_owned(), |w| w.path.clone());
            let model = format!(
                "{} · {}",
                session.info.config.model,
                crate::ui::sheets::reasoning_label(session.info.config.reasoning)
            );
            (label, ws, model)
        }
        None => (
            "none".to_owned(),
            app.workspace.display().to_string(),
            app.model_id().unwrap_or("default").to_owned(),
        ),
    };
    let activity = app.status_label().unwrap_or("idle").to_owned();
    let tasks = overlay::async_tasks(app);
    let tasks_label = if tasks.is_empty() {
        "none".to_owned()
    } else {
        let running = tasks.iter().filter(|t| t.is_running()).count();
        format!("{running} running · {} total", tasks.len())
    };
    let host_facts = vec![
        ("session", session_label, None),
        ("workspace", workspace_label, None),
        ("model", model_label, None),
        ("activity", activity, None),
        ("async tasks", tasks_label, None),
    ];
    lines.extend(chrome::kv_lines(&host_facts, styles, width));

    if remote.workspace_roots_empty {
        lines.push(Line::default());
        lines.push(styled(
            "Note: host.json workspace_roots is empty; configure workspace_roots to browse or create directories from minicore-web.",
            styles.dim(),
        ));
    }

    lines
}
