//! The transcript/history scroll view: durable blocks and the live loop tail (spec r2).

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;

use crate::app::App;
use crate::markdown::wrap_plain;
use crate::state::session::SessionView;
use crate::state::transcript::{PreparedTranscriptCache, TranscriptBlock};
use crate::theme::Theme;
use crate::ui::{assistant, header, layout, reasoning, tool, user};

/// Prepares only the durable history portion for the active session.
pub fn prepare_cache(app: &App, width: u16) -> Option<PreparedTranscriptCache> {
    let (session_id, key) = app.transcript_cache_key(width)?;
    let view = app.sessions.known.get(&session_id)?;
    if view.transcript.render_cache.matches(&key) {
        return None;
    }
    let theme = app.theme.theme();
    let lines = build_durable_lines(&theme, view, width as usize, app.reasoning_visible);
    Some(PreparedTranscriptCache {
        session_id,
        key: Some(key),
        lines,
    })
}

/// Returns the transcript content rows available in `height`.
pub fn visible_rows(app: &App, total_lines: usize, height: u16) -> usize {
    let budget = height as usize;
    if budget == 0 {
        return 0;
    }
    if is_scrolled_away(app, total_lines, budget) {
        budget.saturating_sub(1).min(total_lines)
    } else {
        budget.min(total_lines)
    }
}

/// Pure measure for the main loop: the wrapped transcript line count at `width`.
pub fn total_lines(app: &App, width: u16) -> usize {
    let theme = app.theme.theme();
    all_lines(&theme, app, width as usize).len()
}

/// Builds every transcript row (startup header, durable blocks, live tail).
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
        let section = match block {
            TranscriptBlock::User(u) => user::lines(theme, u, width),
            TranscriptBlock::Assistant(a) => assistant::lines(theme, a, width, reasoning_visible),
            TranscriptBlock::Tool(t) => tool::durable(theme, t, width, view.tools_expanded),
            TranscriptBlock::Summary(summary) => summary_lines(theme, width, &summary.content),
        };
        layout::append_section(&mut lines, section);
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
        layout::append_section_ref(&mut lines, durable);

        // Warning for unsaved loop if persistence failed (spec 30.5)
        if let Some(unsaved) = &view.unsaved_loop {
            let error_style = Style::new()
                .fg(ratatui::style::Color::White)
                .bg(theme.error);
            let mut banner_lines = vec![
                Line::default(),
                layout::filled(
                    " ⚠ UNSAVED TURN ",
                    width,
                    error_style.add_modifier(ratatui::style::Modifier::BOLD),
                ),
            ];
            let sentences = [
                "This turn finished, but the Agent did not confirm saving it.",
                "The session is blocked. Tool side effects may already exist.",
                "Closing releases this result; reopening reads whatever the Store can recover.",
            ];
            for sentence in sentences {
                let wrapped = crate::markdown::wrap_plain(
                    sentence,
                    width.saturating_sub(2).max(1),
                    error_style,
                );
                for wline in wrapped {
                    banner_lines.push(layout::filled(&format!(" {wline}"), width, error_style));
                }
            }
            if view.event_gap || unsaved.event_gap {
                let gap_sentence = "Some live output may be missing.";
                let wrapped = crate::markdown::wrap_plain(
                    gap_sentence,
                    width.saturating_sub(2).max(1),
                    error_style,
                );
                for wline in wrapped {
                    banner_lines.push(layout::filled(&format!(" {wline}"), width, error_style));
                }
            }
            banner_lines.push(Line::default());
            layout::append_section(&mut lines, banner_lines);
        }

        // Render completed steer notices retained across loop boundaries
        for steer in &view.completed_steers {
            if steer.state == crate::state::turn::PendingSteerState::Persisted {
                continue;
            }
            let state_label = match &steer.state {
                crate::state::turn::PendingSteerState::Sending => "sending…",
                crate::state::turn::PendingSteerState::Queued => "accepted awaiting history",
                crate::state::turn::PendingSteerState::Persisted => "persisted",
                crate::state::turn::PendingSteerState::NotRecorded => "not recorded",
                crate::state::turn::PendingSteerState::Unconfirmed => "save unconfirmed",
            };
            let label = format!(" ⠸ Steering ({}): {}", state_label, steer.text);
            let steer_lines = vec![
                Line::default(),
                layout::filled(
                    &label,
                    width,
                    Style::new().fg(theme.accent).bg(theme.card_bg),
                ),
                Line::default(),
            ];
            layout::append_section(&mut lines, steer_lines);
        }

        if let Some(live) = &view.live {
            live_section(theme, live, width, app.reasoning_visible, &mut lines);
        }

        if view.can_show_last_result() {
            if let Some(result) = &view.last_result {
                layout::append_section(&mut lines, last_result_lines(theme, result, width));
            }
        }
    }
    lines
}

fn last_result_lines(
    theme: &Theme,
    result: &crate::protocol::TurnResultViewWire,
    width: usize,
) -> Vec<Line<'static>> {
    use crate::protocol::LoopOutcomeWire;
    use crate::ui::status::{result_color, result_summary};

    let (badge, outcome_style) = match &result.outcome {
        LoopOutcomeWire::Completed => (
            "✓",
            Style::new()
                .fg(result_color(result, theme))
                .bg(theme.card_bg),
        ),
        LoopOutcomeWire::Cancelled { .. } => (
            "⊘",
            Style::new()
                .fg(result_color(result, theme))
                .bg(theme.card_bg),
        ),
        LoopOutcomeWire::Failed { .. } => (
            "✗",
            Style::new()
                .fg(result_color(result, theme))
                .bg(theme.card_bg),
        ),
    };

    let content = format!(
        " {} Turn {} · requests: {} · tool rounds: {}",
        badge,
        result_summary(result),
        result.requests,
        result.tool_rounds
    );

    vec![
        Line::default(),
        layout::filled(&content, width, outcome_style),
        Line::default(),
    ]
}

/// The live loop tail: supports multi-request loops, live tools, and pending steers.
fn live_section(
    theme: &Theme,
    live: &crate::state::turn::LiveLoop,
    width: usize,
    reasoning_visible: bool,
    out: &mut Vec<Line<'static>>,
) {
    let mut sections = Vec::new();

    for req in &live.requests {
        if live.requests.len() > 1 || req.request_index > 0 || req.model.is_empty() {
            let info = if req.model.is_empty() {
                format!("Request #{} · config unknown", req.request_index)
            } else {
                format!(
                    "Request #{} · {} · {:?}",
                    req.request_index, req.model, req.reasoning
                )
            };
            sections.push(layout::left_pad(
                Line::from(vec![ratatui::text::Span::styled(
                    info,
                    Style::new().fg(theme.muted),
                )]),
                1,
            ));
        }
        if !req.reasoning_text.is_empty() {
            layout::append_section(
                &mut sections,
                reasoning::live_lines(theme, &req.reasoning_text, width, reasoning_visible),
            );
        }
        if !req.text.is_empty() {
            let base = Style::new().fg(theme.text);
            let lines = wrap_plain(&req.text, width.saturating_sub(1).max(1), base)
                .into_iter()
                .map(|line| layout::left_pad(line, 1))
                .collect();
            layout::append_section(&mut sections, layout::vertical_section(lines));
        }
        for live_tool in &req.tools {
            layout::append_section(&mut sections, tool::live(theme, live_tool, width));
        }
    }

    // Pending steers
    for steer in &live.pending_steers {
        let state_label = match &steer.state {
            crate::state::turn::PendingSteerState::Sending => "sending…",
            crate::state::turn::PendingSteerState::Queued => "accepted awaiting history",
            crate::state::turn::PendingSteerState::Persisted => "persisted",
            crate::state::turn::PendingSteerState::NotRecorded => "not recorded",
            crate::state::turn::PendingSteerState::Unconfirmed => "save unconfirmed",
        };
        let label = format!(" ⠸ Steering ({}): {}", state_label, steer.text);
        sections.push(Line::default());
        sections.push(layout::filled(
            &label,
            width,
            Style::new().fg(theme.accent).bg(theme.card_bg),
        ));
        sections.push(Line::default());
    }

    layout::append_section(out, sections);
}

fn summary_lines(theme: &Theme, width: usize, content: &str) -> Vec<Line<'static>> {
    let label = if content.is_empty() {
        " Conversation compacted".to_owned()
    } else {
        format!(" Summary: {content}")
    };
    vec![
        Line::default(),
        layout::filled(
            &label,
            width,
            Style::new().fg(theme.muted).bg(theme.card_bg),
        ),
        Line::default(),
    ]
}

pub fn render(frame: &mut Frame<'_>, area: Rect, app: &App, theme: &Theme) {
    let width = area.width as usize;
    let height = area.height as usize;
    if width == 0 || height == 0 {
        return;
    }
    let lines = all_lines(theme, app, width);
    let total = lines.len();
    let (offset, marker) = if is_scrolled_away(app, total, height) {
        let visible = height.saturating_sub(1);
        let offset = app
            .active_view()
            .map(|view| view.scroll.offset)
            .unwrap_or(0);
        let max_offset = total.saturating_sub(visible);
        (offset.min(max_offset), true)
    } else {
        (total.saturating_sub(height), false)
    };
    let budget = if marker {
        height.saturating_sub(1)
    } else {
        height
    };
    let slice: Vec<Line<'static>> = lines.into_iter().skip(offset).take(budget).collect();
    let body_area = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: slice.len() as u16,
    };
    frame.render_widget(ratatui::widgets::Paragraph::new(slice), body_area);
    if marker {
        let marker_y = area.y.saturating_add(height as u16).saturating_sub(1);
        let marker_area = Rect {
            x: area.x,
            y: marker_y,
            width: area.width,
            height: 1,
        };
        render_marker(frame, marker_area, app, theme);
    }
}

fn render_marker(frame: &mut Frame<'_>, area: Rect, app: &App, theme: &Theme) {
    let label = if app
        .active_view()
        .is_some_and(|view| view.scroll.new_content)
    {
        "↓ new output"
    } else {
        "↑ scroll position"
    };
    let line = layout::filled(
        label,
        area.width as usize,
        Style::new().fg(theme.dim).bg(theme.page_bg),
    );
    frame.render_widget(ratatui::widgets::Paragraph::new(line), area);
}

fn is_scrolled_away(app: &App, total: usize, height: usize) -> bool {
    let Some(view) = app.active_view() else {
        return false;
    };
    if view.scroll.follow_tail || total <= height {
        return false;
    }
    let visible = height.saturating_sub(1);
    let max_offset = total.saturating_sub(visible);
    view.scroll.offset < max_offset
}
