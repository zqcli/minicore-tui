//! Selector panels (development spec 24-28): a bordered dock panel with a
//! title, extra header rows, a search row, the item list, and a position
//! counter. Renderers are pure read-only views of `App`; the selection
//! state lives in the dock, and item filtering reuses the same helpers as
//! `App::update` so both phases always agree.

use std::time::SystemTime;

use ratatui::Frame;
use ratatui::layout::{Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};

use crate::app::App;
use crate::markdown::{column_width, line_width};
use crate::protocol::{ModelInfo, ProfileInfo, Reasoning, SessionInfo, SessionStatusWire};
use crate::state::selection::{
    filtered_models, filtered_profiles, filtered_sessions, parse_rfc3339, reasoning_description,
    reasoning_label, supported_reasoning,
};
use crate::theme::Theme;
use crate::ui::layout;

/// Renders whichever selector the dock is showing; the new-session form and
/// the composer are rendered by their own modules.
pub fn render(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    match &app.dock {
        crate::state::selection::Dock::SessionSelector(state) => {
            render_session(frame, area, app, theme, state)
        }
        crate::state::selection::Dock::ModelSelector(state) => {
            render_model(frame, area, app, theme, state)
        }
        crate::state::selection::Dock::ReasoningSelector(state) => {
            render_reasoning(frame, area, app, theme, state)
        }
        crate::state::selection::Dock::ProfileSelector(state) => {
            render_profile(frame, area, app, theme, state)
        }
        _ => {}
    }
}

pub fn render_model(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    theme: &Theme,
    state: &crate::state::selection::SelectorState,
) {
    let items = filtered_models(&app.catalogs.models, &state.query);
    // ✓ current marks the ACTIVE session's model; the selection feeds a new
    // session and never claims the current one moved (spec 26.3).
    let current = app.active_view().map(|view| view.info.model.clone());
    let header = vec![Line::from(Span::styled(
        "Changing model creates a new session.",
        Style::new().fg(theme.muted),
    ))];
    let width = inner_width(area);
    let lines: Vec<Vec<Line<'static>>> = items
        .iter()
        .map(|model| {
            vec![model_line(
                theme,
                model,
                current.as_deref() == Some(model.id.as_str()),
                width,
            )]
        })
        .collect();
    shell(
        frame,
        area,
        theme,
        "Select model",
        header,
        Some(&state.query),
        vec![1; items.len()],
        lines,
        state.cursor,
        items.len(),
        "No matching items",
        state.error.as_deref(),
    );
}

pub fn render_reasoning(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    theme: &Theme,
    state: &crate::state::selection::SelectorState,
) {
    let model = app
        .new_session()
        .map(|draft| draft.model.clone())
        .unwrap_or_default();
    let levels = supported_reasoning(&app.catalogs.models, &model);
    // Never let the user believe the current session changed (spec 27.3).
    let current = app.active_view().map(|view| view.info.reasoning);
    let mut header = vec![Line::from(vec![
        Span::styled("Current session: ".to_owned(), Style::new().fg(theme.dim)),
        Span::styled(
            current.map(reasoning_label).unwrap_or("—"),
            Style::new().fg(theme.muted),
        ),
    ])];
    if let Some(draft) = app.new_session() {
        header.push(Line::from(vec![
            Span::styled(
                "New session setting: ".to_owned(),
                Style::new().fg(theme.dim),
            ),
            Span::styled(
                reasoning_label(draft.reasoning),
                Style::new().fg(theme.reasoning_color(draft.reasoning)),
            ),
        ]));
    }
    let width = inner_width(area);
    let lines: Vec<Vec<Line<'static>>> = levels
        .iter()
        .map(|level| vec![reasoning_line(theme, *level, width)])
        .collect();
    shell(
        frame,
        area,
        theme,
        "Select reasoning",
        header,
        None,
        vec![1; levels.len()],
        lines,
        state.cursor,
        levels.len(),
        "No supported reasoning for this model",
        state.error.as_deref(),
    );
}

pub fn render_profile(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    theme: &Theme,
    state: &crate::state::selection::SelectorState,
) {
    let items = filtered_profiles(&app.catalogs.profiles, &state.query);
    let width = inner_width(area);
    let lines: Vec<Vec<Line<'static>>> = items
        .iter()
        .map(|profile| vec![profile_line(theme, profile, width)])
        .collect();
    shell(
        frame,
        area,
        theme,
        "Select profile",
        Vec::new(),
        Some(&state.query),
        vec![1; items.len()],
        lines,
        state.cursor,
        items.len(),
        "No matching items",
        state.error.as_deref(),
    );
}

pub fn render_session(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    theme: &Theme,
    state: &crate::state::selection::SelectorState,
) {
    let items = filtered_sessions(&app.sessions.list, &state.query);
    let wide = area.width >= 70;
    let width = inner_width(area);
    let mut lines = Vec::with_capacity(items.len());
    for info in &items {
        lines.push(session_lines(app, theme, info, wide, width));
    }
    shell(
        frame,
        area,
        theme,
        "Select session",
        Vec::new(),
        Some(&state.query),
        vec![2; items.len()],
        lines,
        state.cursor,
        items.len(),
        "No matching sessions",
        state.error.as_deref(),
    );
}

/// The usable row width inside the panel's 1-cell rounded border.
fn inner_width(area: Rect) -> usize {
    area.width.saturating_sub(2) as usize
}

/// One model row: id, compact context, tools support, supported reasoning,
/// and the ✓ current marker for the active session's model (spec 26.3).
fn model_line(theme: &Theme, model: &ModelInfo, current: bool, width: usize) -> Line<'static> {
    let mut spans = vec![Span::styled(model.id.clone(), Style::new().fg(theme.text))];
    spans.push(Span::styled(
        format!("  {}", compact_context(model.context_window)),
        Style::new().fg(theme.muted),
    ));
    let tools = if model.supports_tools {
        "✓ tools"
    } else {
        "— tools"
    };
    spans.push(Span::styled(
        format!("  {tools}"),
        Style::new().fg(if model.supports_tools {
            theme.success
        } else {
            theme.dim
        }),
    ));
    let reasoning = model
        .supported_reasoning
        .iter()
        .map(|level| reasoning_label(*level))
        .collect::<Vec<_>>()
        .join("/");
    if !reasoning.is_empty() {
        spans.push(Span::styled(
            format!("  • {reasoning}"),
            Style::new().fg(theme.muted),
        ));
    }
    if current {
        spans.push(Span::styled("  ✓ current", Style::new().fg(theme.success)));
    }
    fit_spans(spans, width.saturating_sub(2))
}

fn compact_context(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M context", n as f64 / 1_000_000.0).replace(".0M", "M")
    } else if n >= 1_000 {
        format!("{}k context", n / 1_000)
    } else {
        format!("{n} context")
    }
}

/// One reasoning row in its thinking color with the spec description
/// (spec 27.2): `→ high   Deep reasoning`.
fn reasoning_line(theme: &Theme, level: Reasoning, width: usize) -> Line<'static> {
    fit_spans(
        vec![
            Span::styled(
                reasoning_label(level),
                Style::new().fg(theme.reasoning_color(level)),
            ),
            Span::styled(
                format!("  {}", reasoning_description(level)),
                Style::new().fg(theme.muted),
            ),
        ],
        width.saturating_sub(2),
    )
}

fn profile_line(theme: &Theme, profile: &ProfileInfo, width: usize) -> Line<'static> {
    let tools = profile.tools.join(", ");
    fit_spans(
        vec![
            Span::styled(profile.id.clone(), Style::new().fg(theme.text)),
            Span::styled(format!("  tools: {tools}"), Style::new().fg(theme.muted)),
        ],
        width.saturating_sub(2),
    )
}

/// Two session rows (spec 28.4). Wide: title plus `model · reasoning` and
/// the relative age, with the workspace on the second row. Narrow: title
/// and age, then `model/reasoning`.
fn session_lines(
    app: &App,
    theme: &Theme,
    info: &SessionInfo,
    wide: bool,
    width: usize,
) -> Vec<Line<'static>> {
    let title = title_or_short_id(info);
    let age = relative_age(&info.updated_at, (app.now)());
    let marker = session_marker(app, info);
    let right = if wide {
        format!(
            "{} · {}   {age}",
            info.model,
            reasoning_label(info.reasoning)
        )
    } else {
        age
    };
    let line1 = sides(&title, &right, width, theme);
    let line2_text = if wide {
        format!("  {marker} {}", info.workspace)
    } else {
        format!(
            "  {marker} {}/{}",
            info.model,
            reasoning_label(info.reasoning)
        )
    };
    let line2 = Line::from(Span::styled(line2_text, Style::new().fg(theme.muted)));
    vec![line1, line2]
}

/// ● loaded, ◉ running, ○ known-but-unloaded, space unknown (spec 28.5).
fn session_marker(app: &App, info: &SessionInfo) -> &'static str {
    let Some(view) = app.sessions.known.get(&info.session_id) else {
        return " ";
    };
    let running = view.live.is_some()
        || view
            .state
            .as_ref()
            .is_some_and(|state| state.status == SessionStatusWire::Running);
    if running {
        "◉"
    } else if view.info.loaded || view.transcript.complete {
        "●"
    } else {
        "○"
    }
}

fn title_or_short_id(info: &SessionInfo) -> String {
    match &info.title {
        Some(title) if !title.is_empty() => title.clone(),
        _ => info.session_id.chars().take(8).collect(),
    }
}

/// The shared panel shell: accent rounded border, title, header rows,
/// optional search row, item rows (first row gets the `→`/`  ` prefix,
/// selected rows get the selected background), empty text, error line, and
/// the `(n/N)` position counter. All rows are left-aligned content; the
/// counter is dim.
#[allow(clippy::too_many_arguments)]
fn shell(
    frame: &mut Frame,
    area: Rect,
    theme: &Theme,
    title: &str,
    header: Vec<Line<'static>>,
    search: Option<&str>,
    heights: Vec<usize>,
    item_lines: Vec<Vec<Line<'static>>>,
    cursor: usize,
    count: usize,
    empty: &str,
    error: Option<&str>,
) {
    frame.render_widget(
        Block::bordered().border_style(Style::new().fg(theme.border_accent)),
        area,
    );
    let inner = area.inner(Margin::new(1, 1));
    let width = inner.width as usize;

    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(Span::styled(
        title.to_owned(),
        Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
    )));
    lines.extend(header);
    if let Some(error) = error {
        lines.push(Line::from(Span::styled(
            format!("⚠ {error}"),
            Style::new().fg(theme.error),
        )));
    }
    if let Some(query) = search {
        lines.push(Line::from(vec![
            Span::styled("> ", Style::new().fg(theme.accent)),
            Span::styled(query.to_owned(), Style::new().fg(theme.text)),
        ]));
    }

    let avail = inner.height as usize;
    let cap = avail.saturating_sub(lines.len() + 1); // + the counter row
    if count == 0 {
        lines.push(Line::from(Span::styled(
            empty.to_owned(),
            Style::new().fg(theme.muted),
        )));
    } else {
        let (start, end) = visible_window(&heights, cursor, cap);
        for (index, item_rows) in item_lines.iter().enumerate().take(end).skip(start) {
            let selected = index == cursor;
            for (row, line) in item_rows.iter().enumerate() {
                let mut line = line.clone();
                if row == 0 {
                    line = prefixed(line, selected, theme);
                }
                lines.push(highlight(line, selected, theme, width));
            }
        }
    }
    let shown = if count == 0 {
        0
    } else {
        cursor.min(count - 1) + 1
    };
    lines.push(Line::from(Span::styled(
        format!("({shown}/{count})"),
        Style::new().fg(theme.dim),
    )));
    while lines.len() < avail {
        lines.push(Line::default());
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

/// Trims trailing content so a line fits `width` display cells without
/// splitting characters (overflows only occur at extreme narrow widths).
fn fit_spans(mut spans: Vec<Span<'static>>, width: usize) -> Line<'static> {
    while spans
        .iter()
        .map(|s| column_width(s.content.as_ref()))
        .sum::<usize>()
        > width
    {
        let Some(span) = spans.last_mut() else {
            break;
        };
        let content: String = span.content.chars().collect();
        if content.is_empty() {
            spans.pop();
            continue;
        }
        let mut trimmed = content;
        trimmed.pop();
        span.content = trimmed.into();
        if span.content.is_empty() {
            spans.pop();
        }
    }
    Line::from(spans)
}

fn prefixed(mut line: Line<'static>, selected: bool, theme: &Theme) -> Line<'static> {
    let prefix = if selected {
        Span::styled(
            "→ ",
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        )
    } else {
        Span::raw("  ")
    };
    line.spans.insert(0, prefix);
    line
}

/// Paints the row background (selected_bg for the selection, page_bg
/// otherwise) and pads it to the full panel width.
pub(crate) fn highlight(
    mut line: Line<'static>,
    selected: bool,
    theme: &Theme,
    width: usize,
) -> Line<'static> {
    let bg = if selected {
        theme.selected_bg
    } else {
        theme.page_bg
    };
    line = line.patch_style(Style::new().bg(bg));
    let fill = width.saturating_sub(line_width(&line));
    if fill > 0 {
        line.spans
            .push(Span::styled(" ".repeat(fill), Style::new()));
    }
    line
}

/// A row with the left content and the right content anchored to the edge
/// (the `→`/`  ` prefix is added by `shell`).
fn sides(left: &str, right: &str, width: usize, theme: &Theme) -> Line<'static> {
    let usable = width.saturating_sub(2); // arrow prefix reservation
    let right_w = column_width(right);
    let left_cap = usable.saturating_sub(right_w).saturating_sub(1);
    let left = layout::truncate(left, left_cap);
    let left_w = column_width(&left);
    let gap = usable.saturating_sub(left_w + right_w);
    Line::from(vec![
        Span::styled(left.to_owned(), Style::new().fg(theme.text)),
        Span::styled(" ".repeat(gap), Style::new()),
        Span::styled(right.to_owned(), Style::new().fg(theme.muted)),
    ])
}

/// The item window that keeps `cursor` visible inside `cap` rows, expanding
/// upward as far as the budget allows.
fn visible_window(heights: &[usize], cursor: usize, cap: usize) -> (usize, usize) {
    let n = heights.len();
    if n == 0 || cap == 0 {
        return (0, 0);
    }
    let cursor = cursor.min(n - 1);
    let mut start = cursor;
    loop {
        let end = fit_rows(heights, start, cap);
        if start == 0 {
            return (start, end);
        }
        let prev = start - 1;
        if rows_in(heights, prev, end) <= cap {
            start = prev;
        } else {
            return (start, end);
        }
    }
}

fn fit_rows(heights: &[usize], start: usize, cap: usize) -> usize {
    let mut rows = 0;
    let mut index = start;
    while index < heights.len() && rows + heights[index] <= cap {
        rows += heights[index];
        index += 1;
    }
    index
}

fn rows_in(heights: &[usize], start: usize, end: usize) -> usize {
    heights[start..end.min(heights.len())].iter().sum()
}

// ---- relative age ------------------------------------------------------

/// Human age of an RFC3339 `updated_at` against `now`: `now`, `5m`, `3h`,
/// `2d`. Unparsable timestamps render empty (spec 28.4).
pub fn relative_age(updated_at: &str, now: SystemTime) -> String {
    let Some(then) = parse_rfc3339(updated_at) else {
        return String::new();
    };
    let Ok(diff) = now.duration_since(then) else {
        return "now".to_owned();
    };
    let secs = diff.as_secs();
    if secs < 60 {
        "now".to_owned()
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: u64) -> SystemTime {
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(s)
    }

    #[test]
    fn relative_age_formats_seconds_minutes_hours_days() {
        let now = secs(1_800_000_000);
        assert_eq!(relative_age("2027-01-15T07:59:50.000Z", now), "now");
        assert_eq!(relative_age("2027-01-15T07:55:00.000Z", now), "5m");
        assert_eq!(relative_age("2027-01-15T05:00:00.000Z", now), "3h");
        assert_eq!(relative_age("2027-01-14T08:00:00.000Z", now), "1d");
    }

    #[test]
    fn model_line_marks_the_current_session_model() {
        let theme = Theme::dark();
        let model = ModelInfo {
            id: "deep".into(),
            model_ref: "x".into(),
            context_window: 128_000,
            supports_tools: true,
            supported_reasoning: vec![Reasoning::Auto, Reasoning::High],
        };
        let line = model_line(&theme, &model, true, 80);
        let joined: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(joined.contains("deep"));
        assert!(joined.contains("128k context"));
        assert!(joined.contains("✓ tools"));
        assert!(joined.contains("auto/high"));
        assert!(joined.contains("✓ current"));
        assert_line_fits(&line, 80);
    }

    fn assert_line_fits(line: &Line, width: usize) {
        assert!(line_width(line) <= width, "line overflowed: {line:?}");
    }
}
