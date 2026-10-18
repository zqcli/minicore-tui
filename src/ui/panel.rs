//! The bottom-sheet panel: pickers, the command palette and the approval
//! prompt. One style for every sheet, per design §7.5: an accent title row, a
//! list with the selected row on a background band, and a dim hint row.
//!
//! Sheets sit *above* the composer, so the draft and the footer stay visible
//! while a picker is open.

use ratatui::text::Line;

use super::files;
use super::theme::Styles;
use super::wrap;
use crate::app::commands;
use crate::app::{App, Overlay};

/// One row of a sheet.
pub struct Row {
    pub label: String,
    pub detail: String,
}

impl Row {
    fn new(label: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            detail: detail.into(),
        }
    }
}

/// A rendered sheet: title, rows, hint.
pub struct Sheet {
    pub title: String,
    pub rows: Vec<Row>,
    pub selected: usize,
    pub hint: String,
}

/// The sheet the current overlay shows, or `None` when no sheet is open.
pub fn sheet(app: &App) -> Option<Sheet> {
    let sheet = match &app.overlay {
        Overlay::None
        | Overlay::Context
        | Overlay::Help
        | Overlay::Diff { .. }
        | Overlay::Tool(_) => return None,
        Overlay::Palette { filter, selected } => {
            let rows = commands::filter(filter)
                .into_iter()
                .map(|command| Row::new(command.name, command.description))
                .collect();
            Sheet {
                title: "Commands".into(),
                rows,
                selected: *selected,
                hint: "enter run · type to filter · esc close".into(),
            }
        }
        Overlay::Sessions {
            filter, selected, ..
        } => Sheet {
            title: "Sessions".into(),
            rows: app
                .sessions
                .iter()
                .filter(|info| {
                    filter.is_empty()
                        || info.session_id.contains(filter.as_str())
                        || info
                            .title
                            .as_deref()
                            .is_some_and(|title| title.contains(filter.as_str()))
                })
                .map(|info| {
                    let current = app
                        .session
                        .as_ref()
                        .is_some_and(|session| session.info.session_id == info.session_id);
                    Row::new(
                        format!(
                            "{}{}",
                            if current { "● " } else { "  " },
                            info.title
                                .as_deref()
                                .filter(|title| !title.trim().is_empty())
                                .unwrap_or(&info.session_id)
                        ),
                        format!("{} · {}", info.model, info.reasoning.label()),
                    )
                })
                .collect(),
            selected: *selected,
            hint: "enter open · ctrl+d delete · type to filter · esc close".into(),
        },
        Overlay::Models { selected } => {
            let current = app
                .session
                .as_ref()
                .map(|session| session.info.model.clone());
            Sheet {
                title: "Model".into(),
                rows: app
                    .models
                    .iter()
                    .map(|model| {
                        let mark = if Some(&model.id) == current.as_ref() {
                            "● "
                        } else {
                            "  "
                        };
                        Row::new(format!("{mark}{}", model.id), String::new())
                    })
                    .collect(),
                selected: *selected,
                hint: "enter switch · esc close".into(),
            }
        }
        Overlay::Reasoning { selected } => {
            let current = app.session.as_ref().map(|session| session.info.reasoning);
            Sheet {
                title: "Reasoning".into(),
                rows: app
                    .session_levels()
                    .iter()
                    .map(|level| {
                        let mark = if Some(*level) == current {
                            "● "
                        } else {
                            "  "
                        };
                        Row::new(format!("{mark}{}", level.label()), String::new())
                    })
                    .collect(),
                selected: *selected,
                hint: "enter apply · shift+tab cycles · esc close".into(),
            }
        }
        Overlay::Files { filter, selected } => Sheet {
            title: "Files".into(),
            rows: files::filtered(filter)
                .into_iter()
                .map(|path| Row::new(path, String::new()))
                .collect(),
            selected: *selected,
            hint: "enter insert the path · type to filter · esc close".into(),
        },
        Overlay::Find {
            query,
            matches,
            current,
        } => Sheet {
            title: format!("Find: {query}"),
            rows: Vec::new(),
            selected: 0,
            hint: match matches.len() {
                0 => "no matches · enter next · esc close".into(),
                total => format!(
                    "match {} of {total} · enter next · shift+enter previous · esc close",
                    current + 1
                ),
            },
        },
        Overlay::Interaction => {
            let Some(interaction) = app
                .session
                .as_ref()
                .and_then(|session| session.interaction.as_ref())
            else {
                return None;
            };
            let mut rows: Vec<Row> = Vec::new();
            if let Some(detail) = &interaction.detail {
                rows.push(Row::new(detail.clone(), String::new()));
            }
            rows.extend(
                interaction
                    .options()
                    .into_iter()
                    .map(|option| Row::new(option, String::new())),
            );
            Sheet {
                title: interaction.prompt.clone(),
                rows,
                selected: app.interaction_choice,
                hint: "y allow · n deny · ↑↓ choose · esc deny".into(),
            }
        }
    };
    Some(sheet)
}

/// The rows of the sheet at `width`.
pub fn rows(sheet: &Sheet, styles: Styles, width: u16) -> Vec<Line<'static>> {
    let width = width.max(9) as usize;
    let mut lines = vec![Line::styled(
        wrap::truncate(&sheet.title, width),
        styles.accent(),
    )];
    // A sheet shows a window around the selection, so a long session list never
    // pushes the composer off screen.
    const VISIBLE: usize = 8;
    let start = sheet.selected.saturating_sub(VISIBLE / 2);
    let end = (start + VISIBLE).min(sheet.rows.len());
    for (index, row) in sheet.rows.iter().enumerate().take(end).skip(start) {
        let selected = index == sheet.selected;
        let marker = if selected { "▶ " } else { "  " };
        let detail = if row.detail.is_empty() {
            String::new()
        } else {
            format!("  ·  {}", row.detail)
        };
        let text = format!("{marker}{}{detail}", row.label);
        let text = wrap::truncate(&text, width);
        lines.push(row_line(&text, selected, styles));
    }
    if end < sheet.rows.len() {
        lines.push(row_line(
            &format!("  +{} more", sheet.rows.len() - end),
            false,
            styles,
        ));
    }
    lines.push(Line::styled(
        wrap::truncate(&sheet.hint, width),
        styles.subtle(),
    ));
    lines
}

fn row_line(text: &str, selected: bool, styles: Styles) -> Line<'static> {
    let style = if selected {
        styles.selected(styles.text())
    } else {
        styles.text()
    };
    Line::styled(text.to_owned(), style)
}
