//! The transcript scroll view (development spec 18, 29-32): durable blocks
//! are prepared into cached wrapped lines, while the header and live tail are
//! derived for each frame. Rendering and measurement read the same durable
//! cache and never mutate the app.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::markdown::wrap_plain;
use crate::protocol::TurnTerminalWire;
use crate::state::session::SessionView;
use crate::state::transcript::{PreparedTranscriptCache, TranscriptBlock};
use crate::theme::Theme;
use crate::ui::{assistant, header, layout, reasoning, tool, user};

/// Prepares only the durable transcript portion for the active session. The
/// returned value is inert until `App::update` receives it and verifies that
/// its session, revision, width, theme, and visibility inputs are unchanged.
pub fn prepare_cache(app: &App, width: u16) -> Option<PreparedTranscriptCache> {
    let (session_id, key) = app.transcript_cache_key(width)?;
    let view = app.active_view()?;
    if view.transcript.render_cache.matches(&key) {
        return None;
    }
    let theme = Theme::for_kind(app.theme);
    let lines = build_durable_lines(&theme, view, width as usize, app.reasoning_visible);
    Some(PreparedTranscriptCache {
        session_id,
        key,
        lines,
    })
}

pub fn render(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let width = area.width as usize;
    let durable = durable_lines_for_render(app, theme, width);
    let lines = all_lines_with_durable(theme, app, width, &durable);
    let total = lines.len();
    let view = app.active_view();
    let follow = view.is_none_or(|view| view.scroll.follow_tail);
    let offset = view.map_or(0, |view| view.scroll.offset);
    let height = area.height as usize;
    // Follow the tail by construction; otherwise clamp the stored offset and
    // flag whether content remains below the visible window (spec 32).
    let start = if follow {
        total.saturating_sub(height)
    } else {
        offset.min(total.saturating_sub(height))
    };
    let end = (start + height).min(total);
    let visible: Vec<Line<'static>> = lines[start..end].to_vec();
    frame.render_widget(Paragraph::new(visible), area);

    if !follow && total > end {
        let hint = Paragraph::new(Line::from(Span::styled(
            "↓ new output",
            Style::new().fg(theme.dim),
        )));
        frame.render_widget(
            hint,
            Rect::new(area.x, area.y + area.height.saturating_sub(1), 12, 1),
        );
    }
}

/// Pure measure for the main loop: the wrapped transcript line count at
/// `width`, identical to what `render` slices. Both paths read the same
/// prepared durable line set when its key is valid; a missing/stale cache is a
/// safe read-only fallback until the main loop prepares it.
pub fn total_lines(app: &App, width: u16) -> usize {
    let theme = Theme::for_kind(app.theme);
    let durable = durable_lines_for_render(app, &theme, width as usize);
    all_lines_with_durable(&theme, app, width as usize, &durable).len()
}

/// Builds every transcript row (startup header, durable blocks, live tail)
/// so the renderer and scroll measurement never disagree.
pub fn all_lines(theme: &Theme, app: &App, width: usize) -> Vec<Line<'static>> {
    let durable = durable_lines_for_render(app, theme, width);
    all_lines_with_durable(theme, app, width, &durable)
}

fn durable_lines_for_render(app: &App, theme: &Theme, width: usize) -> Vec<Line<'static>> {
    let Some(view) = app.active_view() else {
        return Vec::new();
    };
    let key = view.transcript.cache_key(
        width as u16,
        app.theme,
        app.reasoning_visible,
        view.tools_expanded,
    );
    view.transcript.render_cache.lines(&key).map_or_else(
        || build_durable_lines(theme, view, width, app.reasoning_visible),
        |lines| lines.to_vec(),
    )
}

fn build_durable_lines(
    theme: &Theme,
    view: &SessionView,
    width: usize,
    reasoning_visible: bool,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for block in &view.transcript.blocks {
        match block {
            TranscriptBlock::User(user) => lines.extend(user::lines(theme, user, width)),
            TranscriptBlock::Assistant(assistant) => {
                lines.extend(assistant::lines(theme, assistant, width, reasoning_visible))
            }
            TranscriptBlock::Tool(tool) => {
                lines.extend(tool::durable(theme, tool, width, view.tools_expanded))
            }
            TranscriptBlock::Summary(_) => lines.extend(summary_lines(theme, width)),
            TranscriptBlock::Terminal(terminal) => {
                lines.extend(terminal_lines(theme, terminal, width))
            }
        }
    }
    lines
}

fn all_lines_with_durable(
    theme: &Theme,
    app: &App,
    width: usize,
    durable: &[Line<'static>],
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.extend(header::lines(theme, app));
    if let Some(view) = app.active_view() {
        lines.extend_from_slice(durable);
        if let Some(live) = &view.live {
            live_section(theme, live, width, app.reasoning_visible, &mut lines);
        }
    }
    lines
}

/// The live turn tail: the pending user card already lives in the durable
/// blocks, so only reasoning, streaming text, and live tools are appended
/// here (spec 30.3).
fn live_section(
    theme: &Theme,
    live: &crate::state::turn::LiveTurn,
    width: usize,
    reasoning_visible: bool,
    out: &mut Vec<Line<'static>>,
) {
    out.extend(reasoning::live_lines(
        theme,
        &live.reasoning,
        width,
        reasoning_visible,
    ));
    if !live.text.is_empty() {
        let base = Style::new().fg(theme.text);
        for line in wrap_plain(&live.text, width.saturating_sub(1).max(1), base) {
            out.push(layout::left_pad(line, 1));
        }
    }
    for live_tool in &live.tools {
        out.extend(tool::live(theme, live_tool, width));
    }
}

fn summary_lines(theme: &Theme, width: usize) -> Vec<Line<'static>> {
    vec![
        Line::default(),
        layout::filled(
            " Conversation compacted",
            width,
            Style::new().fg(theme.muted).bg(theme.card_bg),
        ),
        Line::default(),
    ]
}

/// Terminal notices: completed is invisible; cancellation, deadline, and
/// failure surface as red/yellow notices (spec 18.6).
fn terminal_lines(
    theme: &Theme,
    terminal: &crate::state::transcript::TerminalBlock,
    width: usize,
) -> Vec<Line<'static>> {
    let (color, label) = match &terminal.terminal {
        TurnTerminalWire::Completed => return Vec::new(),
        TurnTerminalWire::CancelledByUser => (theme.warning, "Turn cancelled".to_owned()),
        TurnTerminalWire::CancelledByShutdown => {
            (theme.warning, "Turn cancelled by shutdown".to_owned())
        }
        TurnTerminalWire::CancelledByRestart => {
            (theme.warning, "Turn cancelled by restart".to_owned())
        }
        TurnTerminalWire::BudgetExceeded => (theme.warning, "Budget exceeded".to_owned()),
        TurnTerminalWire::Failed { diagnostic } => {
            (theme.error, format!("Turn failed: {}", diagnostic.code))
        }
    };
    vec![
        Line::default(),
        layout::filled(&format!(" ⚠ {label}"), width, Style::new().fg(color)),
        Line::default(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{AppEvent, RpcEvent};
    use crate::markdown;
    use crate::theme::ThemeKind;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use serde_json::json;

    fn prepared_app() -> App {
        crate::ui::testapp::chat(ThemeKind::Dark)
    }

    fn install_cache(app: &mut App, width: u16) {
        let prepared = prepare_cache(app, width).expect("cache is initially missing");
        app.update(AppEvent::TranscriptCachePrepared(prepared));
    }

    #[test]
    fn prepare_measure_and_render_parse_durable_markdown_once() {
        let mut app = crate::ui::testapp::open_empty(ThemeKind::Dark, "ses_1", None, "high");
        app.update(AppEvent::SubmitTurn {
            session_id: "ses_1".to_owned(),
            text: "pending user".to_owned(),
        });
        markdown::reset_parse_count();
        let prepared = prepare_cache(&app, 80).expect("initial preparation");
        assert_eq!(markdown::parse_count(), 1);
        app.update(AppEvent::TranscriptCachePrepared(prepared));
        let parsed = markdown::parse_count();
        let measured = total_lines(&app, 80);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), &app, &Theme::dark()))
            .unwrap();
        terminal
            .draw(|frame| render(frame, frame.area(), &app, &Theme::dark()))
            .unwrap();
        assert!(measured > 0);
        assert_eq!(
            markdown::parse_count(),
            parsed,
            "cache hit avoids reparsing"
        );
    }

    #[test]
    fn cache_key_changes_for_width_theme_reasoning_and_tools() {
        let mut app = prepared_app();
        install_cache(&mut app, 80);

        assert!(prepare_cache(&app, 81).is_some(), "width changes the key");
        app.update(AppEvent::SetTheme(ThemeKind::Light));
        assert!(prepare_cache(&app, 80).is_some(), "theme changes the key");
        app.update(AppEvent::ToggleReasoning);
        assert!(
            prepare_cache(&app, 80).is_some(),
            "reasoning changes the key"
        );
        app.update(AppEvent::ToggleTools {
            session_id: "ses_1".to_owned(),
        });
        assert!(prepare_cache(&app, 80).is_some(), "tools changes the key");
    }

    #[test]
    fn block_mutation_invalidates_but_live_delta_does_not() {
        let mut app = prepared_app();
        install_cache(&mut app, 80);
        let revision = app.active_view().unwrap().transcript.render_revision;
        app.update(AppEvent::SubmitTurn {
            session_id: "ses_1".to_owned(),
            text: "new pending".to_owned(),
        });
        assert!(
            app.active_view().unwrap().transcript.render_revision > revision,
            "pending user mutation advances the render revision"
        );

        let mut live = crate::ui::testapp::live_turn(ThemeKind::Dark);
        install_cache(&mut live, 80);
        let revision = live.active_view().unwrap().transcript.render_revision;
        live.update(AppEvent::Rpc(RpcEvent::Frame(
            crate::protocol::IncomingFrame::Notification(
                crate::protocol::RpcNotification::AgentEvent(
                    serde_json::from_value(json!({
                        "type": "output_delta",
                        "data": {
                            "turn": {"session_id":"ses_1", "instance_id":"ins_1", "turn_id":"trn_live"},
                            "channel": "text", "delta": "live only",
                            "meta": {"session_id":"ses_1", "instance_id":"ins_1", "dropped_before":0}
                        }
                    }))
                    .unwrap(),
                ),
            ),
        )));
        assert_eq!(
            live.active_view().unwrap().transcript.render_revision,
            revision,
            "live delta does not invalidate durable cache"
        );

        let mut tools = crate::ui::testapp::tools(ThemeKind::Dark);
        install_cache(&mut tools, 80);
        let revision = tools.active_view().unwrap().transcript.render_revision;
        tools.update(AppEvent::ToggleTool {
            session_id: "ses_1".to_owned(),
            turn_id: "trn_1".to_owned(),
            tool_call_id: "call-1".to_owned(),
        });
        assert!(
            tools.active_view().unwrap().transcript.render_revision > revision,
            "individual tool expansion advances the render revision"
        );
        assert!(prepare_cache(&tools, 80).is_some());
    }

    #[test]
    fn stale_preparation_is_rejected_and_session_caches_are_independent() {
        let mut app = prepared_app();
        let stale = prepare_cache(&app, 80).expect("initial cache");
        app.update(AppEvent::ToggleReasoning);
        app.update(AppEvent::TranscriptCachePrepared(stale));
        assert!(
            app.active_view()
                .unwrap()
                .transcript
                .render_cache
                .is_empty()
        );

        let (models, profiles, sessions) = crate::ui::testapp::standard_catalog();
        let mut app =
            crate::ui::testapp::ready_catalog(ThemeKind::Dark, models, profiles, sessions);
        crate::ui::testapp::open_session(&mut app, "ses_main");
        install_cache(&mut app, 80);
        let key_a = app.transcript_cache_key(80).unwrap().1;
        crate::ui::testapp::open_session(&mut app, "ses_recent");
        install_cache(&mut app, 80);
        let open = crate::ui::testapp::take_requests(app.update(AppEvent::OpenSession {
            session_id: "ses_main".to_owned(),
        }));
        let requests = crate::ui::testapp::take_requests(crate::ui::testapp::respond(
            &mut app,
            &open[0],
            json!({"session": {
                "session_id": "ses_main", "title": "Task", "profile": "coding",
                "workspace": "/project", "model": "deep", "reasoning": "high",
                "loaded": true, "instance_id": "i2",
                "created_at": "2026-01-02T03:04:05.006Z",
                "updated_at": "2026-01-02T03:04:05.006Z"
            }}),
        ));
        let state = requests
            .iter()
            .find(|request| request.method == "session.state")
            .expect("reopen requests state");
        crate::ui::testapp::take_requests(crate::ui::testapp::respond(
            &mut app,
            state,
            json!({
                "session_id": "ses_main", "instance_id": "i2", "status": "idle",
                "health": "healthy", "active_turn": null, "pending_interaction": null,
                "conversation_seq": 0, "last_terminal": null
            }),
        ));
        assert_eq!(app.transcript_cache_key(80).unwrap().1, key_a);
        assert!(prepare_cache(&app, 80).is_none());
    }
}
