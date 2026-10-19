//! The bottom-sheet surfaces of the screen: the command palette, the four
//! pickers, the approval prompt and the find bar. Each function turns app state
//! into one [`Sheet`]; the screen composition decides where it goes.
//!
//! Keeping the mapping here means a picker's rows, hint and selection can be
//! rendered and hit-tested from the same description, and the reducer's
//! navigation (which walks the same filtered lists) cannot drift from what is
//! drawn.

use crate::app::commands;
use crate::app::{App, Overlay, Panel};
use crate::theme::Palette;

use super::files;
use super::panel::{Row, Sheet};
use super::sheets;
use super::views;

/// The sheet for the current overlay, or `None` when no sheet is open.
pub fn sheet(app: &App, palette: &Palette) -> Option<Sheet> {
    Some(match &app.overlay {
        Overlay::None => return None,
        Overlay::Palette { filter, selected } => palette_sheet(filter, *selected),
        Overlay::Sessions { filter, selected } => sessions_sheet(app, filter, *selected),
        Overlay::Models { selected } => models_sheet(app, *selected),
        Overlay::Reasoning { selected } => reasoning_sheet(app, *selected),
        Overlay::Files { filter, selected } => files_sheet(filter, *selected),
        Overlay::Find {
            query,
            matches,
            current,
        } => Sheet::new(
            sheets::find_title(query),
            sheets::find_hint(matches.len(), *current),
        ),
        Overlay::Interaction => interaction_sheet(app, palette),
        // Views own the transcript area instead of a sheet.
        Overlay::Context | Overlay::Help | Overlay::Diff { .. } | Overlay::Tool(_) => return None,
    })
}

fn palette_sheet(filter: &str, selected: usize) -> Sheet {
    let rows = commands::filter(filter)
        .into_iter()
        .map(|command| Row::new(command.name, command.description))
        .collect();
    Sheet::new("Commands", "enter run · esc close")
        .rows(rows)
        .selected(Some(selected))
}

fn sessions_sheet(app: &App, filter: &str, selected: usize) -> Sheet {
    let resident = app
        .session
        .as_ref()
        .map(|session| session.info.session_id.as_str());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    let rows = app
        .filtered_sessions(filter)
        .iter()
        .map(|info| {
            let current = resident == Some(info.session_id.as_str());
            sheets::session_row(info, app.session_label(info), current, now)
        })
        .collect();
    let hint = if app.delete_pending.is_some() {
        "ctrl+d again deletes · esc cancels"
    } else {
        "enter open · type filter · ctrl+d delete · esc close"
    };
    Sheet::new("Sessions", hint)
        .subtitle(filter_subtitle(filter))
        .rows(rows)
        .selected(Some(selected))
}

/// The typed filter, shown after a picker's title so it is visible somewhere.
fn filter_subtitle(filter: &str) -> Option<String> {
    (!filter.is_empty()).then(|| filter.to_owned())
}

fn models_sheet(app: &App, selected: usize) -> Sheet {
    let current = app
        .session
        .as_ref()
        .map(|session| session.info.model.clone());
    let rows = app
        .models
        .iter()
        .map(|model| {
            Row::new(
                model.id.clone(),
                match model.supported_reasoning.len() {
                    0 => String::new(),
                    count => format!("{count} reasoning levels"),
                },
            )
            .mark(Some(&model.id) == current.as_ref())
        })
        .collect();
    Sheet::new("Model", "enter switch · esc close")
        .rows(rows)
        .selected(Some(selected))
}

fn reasoning_sheet(app: &App, selected: usize) -> Sheet {
    let levels = app.reasoning_levels();
    let current = app.session.as_ref().map(|session| session.info.reasoning);
    let rows = levels
        .iter()
        .map(|level| Row::new(level.label(), String::new()).mark(Some(*level) == current))
        .collect();
    Sheet::new("Reasoning", "enter apply · shift+tab cycles · esc close")
        .rows(rows)
        .selected(Some(selected))
}

fn files_sheet(filter: &str, selected: usize) -> Sheet {
    let rows = files::filtered(filter)
        .into_iter()
        .map(|path| Row::new(path, String::new()))
        .collect();
    Sheet::new(
        "Files",
        "enter insert the path · type to filter · esc close",
    )
    .subtitle(filter_subtitle(filter))
    .rows(rows)
    .selected(Some(selected))
}

pub fn approval(app: &App, palette: &Palette) -> Sheet {
    interaction_sheet(app, palette)
}

fn interaction_sheet(app: &App, palette: &Palette) -> Sheet {
    let Some(session) = app.session.as_ref() else {
        return Sheet::new("Approval", "esc closes");
    };
    let Some(interaction) = session.interaction.as_ref() else {
        return Sheet::new("Approval", "esc closes");
    };
    let options = interaction.options();
    sheets::interaction_sheet(
        &interaction.prompt,
        interaction.detail.as_deref(),
        &options,
        app.interaction_choice,
        palette,
    )
}

/// Records the transcript rows one frame drew, so a drag selection copies
/// exactly what is on screen. Call after painting the transcript.
pub fn record_transcript(app: &mut App, area: ratatui::layout::Rect, rows: &[ratatui::text::Line]) {
    let mut screen = std::mem::take(&mut app.rendered);
    screen.record(app, area, rows);
    app.rendered = screen;
}

/// Draws whichever full-screen view owns the transcript area. Returns false
/// when no view is open, so the caller draws the transcript instead.
pub fn draw_view(frame: &mut ratatui::Frame, area: ratatui::layout::Rect, app: &App) -> bool {
    let palette = app.palette;
    match &app.overlay {
        Overlay::Diff { .. } => views::render_diff(frame, area, &palette, &app.diff),
        Overlay::Tool(_) => views::render_tool(frame, area, &palette, &app.tool_view),
        Overlay::Context => views::render_context(frame, area, &palette, &app.context_view),
        Overlay::Help => {
            let commands = crate::app::features::command_rows();
            let help = views::help_view(&commands, &palette);
            views::render_help(frame, area, &palette, &help, app.help_scroll);
        }
        _ => return false,
    }
    true
}

/// The sheet a panel would open, for key handlers that need to know whether a
/// panel is a sheet or a view.
pub fn is_sheet(panel: &Panel) -> bool {
    !matches!(
        panel,
        Panel::Context | Panel::Help | Panel::Diff(_) | Panel::Tool(_)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sheets_are_only_the_overlays_that_do_not_replace_the_transcript() {
        assert!(is_sheet(&Panel::Palette));
        assert!(is_sheet(&Panel::Sessions));
        assert!(is_sheet(&Panel::Models));
        assert!(is_sheet(&Panel::Reasoning));
        assert!(is_sheet(&Panel::Files));
        assert!(is_sheet(&Panel::Find));
        assert!(is_sheet(&Panel::Interaction));
        assert!(!is_sheet(&Panel::Context));
        assert!(!is_sheet(&Panel::Help));
        assert!(!is_sheet(&Panel::Diff(crate::app::DiffScope::Turn)));
        assert!(!is_sheet(&Panel::Tool(crate::protocol::ToolRef {
            session_id: String::new(),
            loop_id: String::new(),
            request_index: 0,
            tool_call_id: String::new(),
        })));
    }

    #[test]
    fn the_palette_lists_command_names_and_descriptions() {
        let sheet = palette_sheet("re", 1);
        assert_eq!(sheet.title, "Commands");
        assert!(sheet.rows.iter().any(|row| row.label == "reasoning"));
        assert!(
            sheet
                .rows
                .iter()
                .any(|row| row.detail.contains("reasoning level"))
        );
        assert_eq!(sheet.selected, Some(1));
    }

    #[test]
    fn the_palette_filters_down_to_matching_commands() {
        let sheet = palette_sheet("zzz", 0);
        assert!(sheet.rows.is_empty());
    }
}
