//! Selector panels (development spec 24-28): a bordered dock panel with a
//! title, extra header rows, a search row, the item list, and a position
//! counter. Renderers are pure read-only views of `App`; the selection
//! state lives in the dock, and item filtering reuses the same helpers as
//! `App::update` so both phases always agree.

use std::ops::Range;
use std::time::SystemTime;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::App;
use crate::markdown::{column_width, line_width};
use crate::protocol::{ModelInfo, ProfileInfo, Reasoning, SessionInfo};
use crate::state::selection::{
    SelectorKind, SelectorState, SessionConfirmChoice, SessionPanelAction, SessionPanelMode,
    SessionSelectorState, filtered_models, filtered_profiles, filtered_sessions, parse_rfc3339,
    reasoning_description, reasoning_label, supported_reasoning,
};
use crate::theme::Theme;
use crate::ui::layout;
use crate::ui::panel::{self, PanelSpec};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SelectorHit {
    pub kind: SelectorKind,
    pub index: usize,
    pub key: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SessionActionHit {
    pub action: SessionPanelAction,
    pub rect: Rect,
}

pub(crate) fn catalog_panel_layout(
    area: Rect,
    app: &App,
    state: &SelectorState,
) -> panel::PanelLayout {
    let header_rows = match state.kind {
        SelectorKind::Model => 1,
        SelectorKind::Reasoning => 1 + u16::from(app.new_session().is_some()),
        SelectorKind::Profile | SelectorKind::Session => 0,
    };
    let query = !matches!(state.kind, SelectorKind::Reasoning | SelectorKind::Session);
    panel::layout(
        area,
        PanelSpec::new(header_rows + u16::from(state.error.is_some()), query, 1),
    )
}

pub(crate) fn catalog_visible_window(app: &App, area: Rect, state: &SelectorState) -> Range<usize> {
    let heights = match state.kind {
        SelectorKind::Model => vec![1; filtered_models(&app.catalogs.models, &state.query).len()],
        SelectorKind::Profile => {
            vec![1; filtered_profiles(&app.catalogs.profiles, &state.query).len()]
        }
        SelectorKind::Reasoning => {
            let model = app
                .new_session()
                .map(|draft| draft.model.clone())
                .or_else(|| state.model_context.clone())
                .or_else(|| app.active_view().map(|view| view.info.model.clone()))
                .unwrap_or_default();
            vec![1; supported_reasoning(&app.catalogs.models, &model).len()]
        }
        SelectorKind::Session => Vec::new(),
    };
    panel::visible_window(
        &heights,
        state.cursor,
        catalog_panel_layout(area, app, state).content.height as usize,
    )
}

pub(crate) fn selector_item_at(
    app: &App,
    area: Rect,
    state: &SelectorState,
    column: u16,
    row: u16,
) -> Option<SelectorHit> {
    let geometry = catalog_panel_layout(area, app, state);
    if column < geometry.content.x || column >= geometry.content.right() {
        return None;
    }
    let local_row = geometry.content_row(row)?;
    let (keys, count) = match state.kind {
        SelectorKind::Model => {
            let items = filtered_models(&app.catalogs.models, &state.query);
            (
                items.iter().map(|item| item.id.clone()).collect::<Vec<_>>(),
                items.len(),
            )
        }
        SelectorKind::Profile => {
            let items = filtered_profiles(&app.catalogs.profiles, &state.query);
            (
                items.iter().map(|item| item.id.clone()).collect::<Vec<_>>(),
                items.len(),
            )
        }
        SelectorKind::Reasoning => {
            let model = app
                .new_session()
                .map(|draft| draft.model.clone())
                .or_else(|| state.model_context.clone())
                .or_else(|| app.active_view().map(|view| view.info.model.clone()))
                .unwrap_or_default();
            let items = supported_reasoning(&app.catalogs.models, &model);
            (
                items
                    .iter()
                    .map(|level| reasoning_label(*level).to_owned())
                    .collect::<Vec<_>>(),
                items.len(),
            )
        }
        SelectorKind::Session => return None,
    };
    if count == 0 {
        return None;
    }
    let visible = catalog_visible_window(app, area, state);
    let index = visible.start + local_row;
    (index < visible.end).then(|| SelectorHit {
        kind: state.kind,
        index,
        key: keys[index].clone(),
    })
}

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
    // ✓ current marks the ACTIVE session's model. An active-session
    // selection is applied through session.update at a request boundary.
    let current = app.active_view().map(|view| view.info.model.clone());
    let header = if app.sessions.active.is_some() && app.new_session().is_none() {
        vec![Line::from(Span::styled(
            "Model applies at the next model request.",
            Style::new().fg(theme.muted),
        ))]
    } else {
        vec![Line::from(Span::styled(
            "Changing model creates a new session.",
            Style::new().fg(theme.muted),
        ))]
    };
    let width = inner_width(area);
    let lines: Vec<Vec<Line<'static>>> = items
        .iter()
        .map(|model| {
            vec![model_line(
                theme,
                model,
                current.as_deref().is_some_and(|current| {
                    current == model.id.as_str() || current == model.model_ref.as_str()
                }),
                width,
            )]
        })
        .collect();
    let geometry = catalog_panel_layout(area, app, state);
    shell(
        frame,
        geometry,
        theme,
        "Select model",
        header,
        Some(&state.query),
        vec![1; items.len()],
        lines,
        Some(state.cursor),
        items.len(),
        "No matching items",
        state.error.as_deref(),
        None,
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
        .or_else(|| state.model_context.clone())
        .or_else(|| app.active_view().map(|view| view.info.model.clone()))
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
    let geometry = catalog_panel_layout(area, app, state);
    shell(
        frame,
        geometry,
        theme,
        "Select reasoning",
        header,
        None,
        vec![1; levels.len()],
        lines,
        Some(state.cursor),
        levels.len(),
        "No supported reasoning for this model",
        state.error.as_deref(),
        None,
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
    let geometry = catalog_panel_layout(area, app, state);
    shell(
        frame,
        geometry,
        theme,
        "Select profile",
        Vec::new(),
        Some(&state.query),
        vec![1; items.len()],
        lines,
        Some(state.cursor),
        items.len(),
        "No matching items",
        state.error.as_deref(),
        None,
    );
}

pub fn render_session(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    theme: &Theme,
    state: &SessionSelectorState,
) {
    let geometry = session_panel_layout(area, state);
    panel::render_frame(frame, geometry, theme);
    frame.render_widget(
        Paragraph::new(vec![Line::from(Span::styled(
            "Select session",
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        ))]),
        geometry.title,
    );
    if let Some(error) = state.error.as_deref() {
        frame.render_widget(
            Paragraph::new(vec![Line::from(Span::styled(
                format!("⚠ {error}"),
                Style::new().fg(theme.error),
            ))]),
            geometry.header,
        );
    }

    match &state.mode {
        SessionPanelMode::Browse => {
            let items = session_items(app, state);
            let wide = area.width >= 70;
            let width = geometry.content.width as usize;
            let lines = items
                .iter()
                .map(|info| session_lines(app, theme, info, wide, width))
                .collect::<Vec<_>>();
            let selected = selected_session_index(state, &items);
            let footer = session_footer(theme, app, state, &items, selected, geometry);
            let _ = shell(
                frame,
                geometry,
                theme,
                "Select session",
                Vec::new(),
                Some(&state.query),
                session_item_heights(wide, items.len()),
                lines,
                selected,
                items.len(),
                "No matching sessions",
                None,
                Some(footer.lines),
            );
        }
        SessionPanelMode::Rename {
            draft,
            cursor,
            submitting,
        } => {
            let target = session_target(app, state);
            let mut lines = vec![Line::from(Span::styled(
                format!(
                    "Rename {} [{}]",
                    target
                        .as_ref()
                        .map(title_or_short_id)
                        .unwrap_or_else(|| "session".to_owned()),
                    target
                        .as_ref()
                        .map(short_id)
                        .unwrap_or_else(|| "unknown".to_owned())
                ),
                Style::new().fg(theme.text),
            ))];
            let value = if *submitting {
                format!("Title: {draft}  (saving…)")
            } else {
                format!("Title: {draft}")
            };
            lines.push(Line::from(Span::styled(value, Style::new().fg(theme.text))));
            lines.push(Line::from(Span::styled(
                "Enter saves · Esc cancels",
                Style::new().fg(theme.dim),
            )));
            render_form_lines(frame, geometry.content, lines);
            frame.render_widget(
                Paragraph::new(vec![Line::from(Span::styled(
                    "Enter Save · Esc Cancel",
                    Style::new().fg(theme.dim),
                ))]),
                geometry.footer,
            );
            if !*submitting && geometry.content.height > 1 {
                let prefix = "Title: ";
                let x = geometry.content.x
                    + column_width(prefix) as u16
                    + column_width(&draft.chars().take(*cursor).collect::<String>()) as u16;
                if x < geometry.content.right() {
                    if let Some(cell) = frame
                        .buffer_mut()
                        .cell_mut((x, geometry.content.y.saturating_add(1)))
                    {
                        cell.set_fg(theme.page_bg);
                        cell.set_bg(theme.text);
                    }
                }
            }
        }
        SessionPanelMode::ConfirmClose | SessionPanelMode::ConfirmCloseForDelete => {
            let target = session_target(app, state);
            let title = if matches!(&state.mode, SessionPanelMode::ConfirmCloseForDelete) {
                "Close before deleting?"
            } else {
                "Close this session?"
            };
            let mut lines = vec![Line::from(Span::styled(
                title,
                Style::new().fg(theme.warning).add_modifier(Modifier::BOLD),
            ))];
            lines.push(Line::from(Span::styled(
                target
                    .as_ref()
                    .map(|info| format!("{} [{}]", title_or_short_id(info), short_id(info)))
                    .unwrap_or_else(|| "unknown session".to_owned()),
                Style::new().fg(theme.text),
            )));
            lines.push(Line::from(Span::styled(
                if matches!(&state.mode, SessionPanelMode::ConfirmCloseForDelete) {
                    "A second confirmation is required before permanent deletion."
                } else {
                    "The session must be idle before it can be closed."
                },
                Style::new().fg(theme.muted),
            )));
            render_form_lines(frame, geometry.content, lines);
            frame.render_widget(
                Paragraph::new(vec![Line::from(Span::styled(
                    "Enter Close · Esc Cancel",
                    Style::new().fg(theme.dim),
                ))]),
                geometry.footer,
            );
        }
        SessionPanelMode::ConfirmDelete { choice, submitting } => {
            let target = session_target(app, state);
            let mut lines = vec![Line::from(Span::styled(
                "Permanently delete this session?",
                Style::new().fg(theme.error).add_modifier(Modifier::BOLD),
            ))];
            lines.push(Line::from(Span::styled(
                target
                    .as_ref()
                    .map(|info| format!("{} [{}]", title_or_short_id(info), short_id(info)))
                    .unwrap_or_else(|| "unknown session".to_owned()),
                Style::new().fg(theme.text),
            )));
            lines.push(Line::from(Span::styled(
                "This cannot be undone.",
                Style::new().fg(theme.warning),
            )));
            lines.push(render_choice_buttons(theme, geometry, *choice, *submitting));
            render_form_lines(frame, geometry.content, lines);
            frame.render_widget(
                Paragraph::new(vec![Line::from(Span::styled(
                    "Tab / ← → choose · Enter activates · Esc Cancel",
                    Style::new().fg(theme.dim),
                ))]),
                geometry.footer,
            );
        }
    }
}

fn render_form_lines(frame: &mut Frame, area: Rect, mut lines: Vec<Line<'static>>) {
    let height = area.height as usize;
    lines.truncate(height);
    while lines.len() < height {
        lines.push(Line::default());
    }
    frame.render_widget(Paragraph::new(lines), area);
}

fn session_items<'a>(app: &'a App, state: &SessionSelectorState) -> Vec<&'a SessionInfo> {
    filtered_sessions(&app.sessions.list, &state.query)
        .into_iter()
        .filter(|info| {
            !app.sessions.pending_deletes.contains(&info.session_id)
                && !app.session_absent(&info.session_id)
        })
        .collect()
}

fn selected_session_index(state: &SessionSelectorState, items: &[&SessionInfo]) -> Option<usize> {
    state
        .selected_session_id
        .as_deref()
        .and_then(|selected| items.iter().position(|info| info.session_id == selected))
}

fn session_target(app: &App, state: &SessionSelectorState) -> Option<SessionInfo> {
    let id = state.selected_session_id.as_deref()?;
    app.sessions
        .known
        .get(id)
        .map(|view| view.info.clone())
        .or_else(|| {
            app.sessions
                .list
                .iter()
                .find(|info| info.session_id == id)
                .cloned()
        })
}

fn session_item_heights(wide: bool, count: usize) -> Vec<usize> {
    vec![usize::from(wide) + 1; count]
}

/// Geometry used by both the session renderer and its mouse hit-test path.
pub(crate) fn session_panel_layout(area: Rect, state: &SessionSelectorState) -> panel::PanelLayout {
    let browse = matches!(&state.mode, SessionPanelMode::Browse);
    panel::layout(
        area,
        PanelSpec::new(
            u16::from(state.error.is_some()),
            browse,
            if browse { 2 } else { 1 },
        ),
    )
}

/// Resolves a pointer in the session content region to a stable session ID.
pub(crate) fn session_item_at(
    app: &App,
    area: Rect,
    state: &SessionSelectorState,
    column: u16,
    row: u16,
) -> Option<String> {
    if !matches!(&state.mode, SessionPanelMode::Browse) {
        return None;
    }
    let geometry = session_panel_layout(area, state);
    let local_row = geometry.content_row(row)?;
    if column < geometry.content.x || column >= geometry.content.right() {
        return None;
    }
    let items = session_items(app, state);
    let selected = selected_session_index(state, &items);
    let wide = area.width >= 70;
    let visible = panel::visible_window(
        &session_item_heights(wide, items.len()),
        selected.unwrap_or(0),
        geometry.content.height as usize,
    );
    let height = usize::from(wide) + 1;
    let index = visible.start + local_row / height;
    (index < visible.end).then(|| items[index].session_id.clone())
}

fn session_footer(
    theme: &Theme,
    _app: &App,
    _state: &SessionSelectorState,
    _items: &[&SessionInfo],
    _selected: Option<usize>,
    panel: panel::PanelLayout,
) -> SessionFooter {
    let rows = [
        [
            (SessionPanelAction::Open, "Enter Open"),
            (SessionPanelAction::New, "Ctrl+N New"),
            (SessionPanelAction::Refresh, "F5 Refresh"),
        ],
        [
            (SessionPanelAction::Rename, "F2 Rename"),
            (SessionPanelAction::Close, "Ctrl+W Close"),
            (SessionPanelAction::Delete, "Del Delete"),
        ],
    ];
    let mut lines = Vec::new();
    let mut hits = Vec::new();
    for (row, actions) in rows.into_iter().enumerate() {
        let (line, row_hits) = action_row(theme, panel, row, actions);
        lines.push(line);
        hits.extend(row_hits);
    }
    SessionFooter { lines, hits }
}

#[derive(Debug, Clone)]
struct SessionFooter {
    lines: Vec<Line<'static>>,
    hits: Vec<SessionActionHit>,
}

fn action_row(
    theme: &Theme,
    panel: panel::PanelLayout,
    row: usize,
    actions: [(SessionPanelAction, &'static str); 3],
) -> (Line<'static>, Vec<SessionActionHit>) {
    let mut spans = Vec::new();
    let mut hits = Vec::new();
    let mut x = panel.footer.x;
    for (index, (action, label)) in actions.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(" · ", Style::new().fg(theme.dim)));
            x = x.saturating_add(3);
        }
        let label_width = column_width(label) as u16;
        spans.push(Span::styled(label, Style::new().fg(theme.accent)));
        let width = label_width.min(panel.footer.right().saturating_sub(x));
        if width > 0 && panel.footer.y.saturating_add(row as u16) < panel.footer.bottom() {
            hits.push(SessionActionHit {
                action,
                rect: Rect::new(x, panel.footer.y.saturating_add(row as u16), width, 1),
            });
        }
        x = x.saturating_add(label_width);
    }
    (Line::from(spans), hits)
}

fn render_choice_buttons(
    theme: &Theme,
    panel: panel::PanelLayout,
    choice: SessionConfirmChoice,
    submitting: bool,
) -> Line<'static> {
    let cancel = "[ Cancel ]";
    let delete = if submitting {
        "[ Deleting… ]"
    } else {
        "[ Delete ]"
    };
    let cancel_style = if choice == SessionConfirmChoice::Cancel {
        Style::new().fg(theme.page_bg).bg(theme.text)
    } else {
        Style::new().fg(theme.text).bg(theme.selected_bg)
    };
    let delete_style = if choice == SessionConfirmChoice::Confirm {
        Style::new().fg(theme.page_bg).bg(theme.error)
    } else {
        Style::new().fg(theme.text).bg(theme.selected_bg)
    };
    let _ = panel;
    Line::from(vec![
        Span::styled(cancel, cancel_style),
        Span::raw("  "),
        Span::styled(delete, delete_style),
    ])
}

pub(crate) fn session_action_at(
    app: &App,
    area: Rect,
    state: &SessionSelectorState,
    column: u16,
    row: u16,
) -> Option<SessionPanelAction> {
    let panel = session_panel_layout(area, state);
    match &state.mode {
        SessionPanelMode::Browse => {
            let items = session_items(app, state);
            let selected = selected_session_index(state, &items);
            session_footer(&Theme::dark(), app, state, &items, selected, panel)
                .hits
                .into_iter()
                .find(|hit| hit.rect.contains((column, row).into()))
                .map(|hit| hit.action)
        }
        SessionPanelMode::ConfirmDelete { .. } => {
            let button_row = panel.content.y.saturating_add(3);
            if row != button_row {
                return None;
            }
            let cancel_width = column_width("[ Cancel ]") as u16;
            let cancel = Rect::new(panel.content.x, button_row, cancel_width, 1);
            let delete_x = panel
                .content
                .x
                .saturating_add(cancel_width)
                .saturating_add(2);
            let delete = Rect::new(delete_x, button_row, column_width("[ Delete ]") as u16, 1);
            if cancel.contains((column, row).into()) {
                Some(SessionPanelAction::Cancel)
            } else if delete.contains((column, row).into()) {
                Some(SessionPanelAction::ConfirmDelete)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// The usable row width inside the panel's 1-cell rounded border.
fn inner_width(area: Rect) -> usize {
    area.width.saturating_sub(2) as usize
}

/// One model row: id, compact context, tools support, supported reasoning,
/// and the ✓ current marker for the active session's model (spec 26.3).
fn model_line(theme: &Theme, model: &ModelInfo, current: bool, width: usize) -> Line<'static> {
    let id = crate::safe_text::safe_display(&model.id).into_owned();
    let mut spans = vec![Span::styled(id, Style::new().fg(theme.text))];
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
    let id = crate::safe_text::safe_display(&profile.id).into_owned();
    let tools = crate::safe_text::safe_display(&profile.tools.join(", ")).into_owned();
    fit_spans(
        vec![
            Span::styled(id, Style::new().fg(theme.text)),
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
    let title = crate::safe_text::safe_display(&title_or_short_id(info)).into_owned();
    let age = relative_age(&info.updated_at, (app.now)());
    let marker = session_marker(app, info);
    let model = crate::safe_text::safe_display(&info.model).into_owned();
    let workspace = crate::safe_text::safe_display(&info.workspace).into_owned();
    let right = if wide {
        format!("{} · {}   {age}", model, reasoning_label(info.reasoning))
    } else {
        age
    };
    let line1 = sides(&title, &right, width, theme);
    let line2_text = if wide {
        format!("  {marker} {workspace}")
    } else {
        format!("  {marker} {model}/{}", reasoning_label(info.reasoning))
    };
    let line2 = Line::from(Span::styled(line2_text, Style::new().fg(theme.muted)));
    if wide {
        vec![line1, line2]
    } else {
        vec![line1]
    }
}

/// ● loaded, ◉ running, ◌ finishing, ! blocked, ○ known-but-unloaded,
/// space unknown (spec 28.5).
fn session_marker(app: &App, info: &SessionInfo) -> &'static str {
    let Some(view) = app.sessions.known.get(&info.session_id) else {
        return " ";
    };
    match view.state.as_ref().map(|state| state.status) {
        Some(crate::protocol::SessionStatusWire::Running)
        | Some(crate::protocol::SessionStatusWire::WaitingForInput) => "◉",
        Some(crate::protocol::SessionStatusWire::Finishing) => "◌",
        Some(crate::protocol::SessionStatusWire::Blocked) => "!",
        Some(crate::protocol::SessionStatusWire::Idle) | None => {
            if view.info.loaded || view.transcript.complete {
                "●"
            } else {
                "○"
            }
        }
    }
}

fn title_or_short_id(info: &SessionInfo) -> String {
    match &info.title {
        Some(title) if !title.is_empty() => title.clone(),
        _ => info.session_id.chars().take(8).collect(),
    }
}

fn short_id(info: &SessionInfo) -> String {
    info.session_id.chars().take(8).collect()
}

/// The shared panel shell: accent rounded border, title, header rows,
/// optional search row, item rows (first row gets the `→`/`  ` prefix,
/// selected rows get the selected background), empty text, error line, and
/// the `(n/N)` position counter. All rows are left-aligned content; the
/// counter is dim.
#[allow(clippy::too_many_arguments)]
fn shell(
    frame: &mut Frame,
    geometry: panel::PanelLayout,
    theme: &Theme,
    title: &str,
    header: Vec<Line<'static>>,
    search: Option<&str>,
    heights: Vec<usize>,
    item_lines: Vec<Vec<Line<'static>>>,
    cursor: Option<usize>,
    count: usize,
    empty: &str,
    error: Option<&str>,
    footer: Option<Vec<Line<'static>>>,
) -> panel::PanelLayout {
    panel::render_frame(frame, geometry, theme);
    let width = geometry.content.width as usize;

    frame.render_widget(
        Paragraph::new(vec![Line::from(Span::styled(
            title.to_owned(),
            Style::new().fg(theme.accent).add_modifier(Modifier::BOLD),
        ))]),
        geometry.title,
    );
    let mut header_lines = header;
    if let Some(error) = error {
        header_lines.push(Line::from(Span::styled(
            format!("⚠ {error}"),
            Style::new().fg(theme.error),
        )));
    }
    frame.render_widget(Paragraph::new(header_lines), geometry.header);
    if let Some(query) = search {
        frame.render_widget(
            Paragraph::new(vec![Line::from(vec![
                Span::styled("> ", Style::new().fg(theme.accent)),
                Span::styled(query.to_owned(), Style::new().fg(theme.text)),
            ])]),
            geometry.query.unwrap_or_default(),
        );
    }

    let cap = geometry.content.height as usize;
    let mut content_lines: Vec<Line<'static>> = Vec::new();
    if count == 0 {
        content_lines.push(Line::from(Span::styled(
            empty.to_owned(),
            Style::new().fg(theme.muted),
        )));
    } else {
        let visible = panel::visible_window(&heights, cursor.unwrap_or(0), cap);
        for (index, item_rows) in item_lines
            .iter()
            .enumerate()
            .take(visible.end)
            .skip(visible.start)
        {
            let selected = cursor == Some(index);
            for (row, line) in item_rows.iter().enumerate() {
                let mut line = line.clone();
                if row == 0 {
                    line = prefixed(line, selected, theme);
                }
                content_lines.push(highlight(line, selected, theme, width));
            }
        }
    }
    while content_lines.len() < cap {
        content_lines.push(Line::default());
    }
    frame.render_widget(Paragraph::new(content_lines), geometry.content);
    let shown = cursor.map_or(0, |cursor| {
        if count == 0 {
            0
        } else {
            cursor.min(count - 1) + 1
        }
    });
    let footer = footer.unwrap_or_else(|| {
        vec![Line::from(Span::styled(
            format!("({shown}/{count})"),
            Style::new().fg(theme.dim),
        ))]
    });
    frame.render_widget(Paragraph::new(footer), geometry.footer);
    geometry
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
