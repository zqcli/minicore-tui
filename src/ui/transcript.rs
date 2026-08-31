//! The transcript scroll view (development spec 18, 29-32): durable blocks
//! rendered into pre-wrapped lines, the live turn section appended at the
//! tail, and `ScrollState`-driven slicing. Rendering never mutates the app.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::markdown::wrap_plain;
use crate::protocol::TurnTerminalWire;
use crate::state::transcript::TranscriptBlock;
use crate::state::turn::LiveTurn;
use crate::theme::Theme;
use crate::ui::{assistant, header, layout, reasoning, tool, user};

pub fn render(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let width = area.width as usize;
    let lines = all_lines(theme, app, width);
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
/// `width`, identical to what `render` slices. Phase 7 will cache this;
/// for now render and measure simply share one builder.
pub fn total_lines(app: &App, width: u16) -> usize {
    let theme = Theme::for_kind(app.theme);
    all_lines(&theme, app, width as usize).len()
}

/// Builds every transcript row (startup header, durable blocks, live tail)
/// so the renderer and the scroll measurement never disagree.
pub fn all_lines(theme: &Theme, app: &App, width: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.extend(header::lines(theme, app));
    if let Some(view) = app.active_view() {
        for block in &view.transcript.blocks {
            match block {
                TranscriptBlock::User(user) => lines.extend(user::lines(theme, user, width)),
                TranscriptBlock::Assistant(assistant) => lines.extend(assistant::lines(
                    theme,
                    assistant,
                    width,
                    app.reasoning_visible,
                )),
                TranscriptBlock::Tool(tool) => {
                    lines.extend(tool::durable(theme, tool, width, view.tools_expanded))
                }
                TranscriptBlock::Summary(_) => lines.extend(summary_lines(theme, width)),
                TranscriptBlock::Terminal(terminal) => {
                    lines.extend(terminal_lines(theme, terminal, width))
                }
            }
        }
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
    live: &LiveTurn,
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
