//! Full-screen views. They replace the transcript area and keep the composer
//! and footer, so a view never hides the draft.
//!
//! The data-heavy views (diff, tool output, context) take everything they show
//! as plain values, so the same renderer serves a live read, a cached read and
//! a test.

use ratatui::text::Line;

use crate::app::commands;
use crate::app::{App, DiffScope, Overlay};
use crate::protocol::SessionInfo;

use super::theme::Styles;
use super::wrap;

/// The frame of a view: title row and hint row.
pub struct Frame<'a> {
    pub title: String,
    pub hint: &'a str,
}

impl Frame<'_> {
    pub fn rows(&self, styles: Styles, width: u16) -> Vec<Line<'static>> {
        let width = width.max(9) as usize;
        vec![
            Line::styled(wrap::truncate(&self.title, width), styles.accent()),
            Line::styled(wrap::truncate(self.hint, width), styles.subtle()),
        ]
    }
}

/// The view the current overlay shows, if it replaces the transcript.
pub fn frame(app: &App) -> Option<Frame<'static>> {
    Some(match &app.overlay {
        Overlay::Help => Frame {
            title: "Help · keys and commands".into(),
            hint: "esc back · ↑↓ scroll · press q to quit",
        },
        Overlay::Context => Frame {
            title: "Context".into(),
            hint: "esc back · the numbers come from the agent's last report",
        },
        Overlay::Diff { scope, .. } => Frame {
            title: format!("Diff · {}", scope_label(*scope)),
            hint: "esc back · tab scope · ↑↓ file · pgup/pgdn scroll",
        },
        Overlay::Tool(_) => Frame {
            title: "Tool output".into(),
            hint: "esc back · pgup/pgdn scroll",
        },
        _ => return None,
    })
}

fn scope_label(scope: DiffScope) -> &'static str {
    match scope {
        DiffScope::Turn => "this turn",
        DiffScope::Session => "this session",
        DiffScope::Workspace => "the workspace",
    }
}

/// The help body: keys, then the command table.
pub fn help(styles: Styles, width: u16) -> Vec<Line<'static>> {
    let width = width.max(9) as usize;
    let mut lines = Vec::new();
    for (keys, action) in KEYS {
        lines.push(key_row(keys, action, styles, width));
    }
    lines.push(Line::raw(""));
    for command in commands::COMMANDS {
        let name = format!("/{}", command.name);
        lines.push(key_row(&name, command.description, styles, width));
    }
    lines
}

const KEYS: &[(&str, &str)] = &[
    ("enter", "send, or steer while a turn runs"),
    ("alt+enter", "queue a follow-up"),
    ("shift+enter", "newline"),
    ("esc", "close a panel, then interrupt the turn"),
    ("ctrl+c", "clear the draft, twice to quit"),
    ("ctrl+g", "edit the draft in $EDITOR"),
    ("ctrl+r", "sessions"),
    ("ctrl+l", "model"),
    ("shift+tab", "next reasoning level"),
    ("ctrl+o", "expand or fold tool cards"),
    ("ctrl+t", "show or hide reasoning"),
    ("ctrl+f", "find in the transcript"),
    ("pgup/pgdn", "scroll the transcript"),
    ("ctrl+home/end", "top or newest"),
    ("ctrl+↑/↓", "previous or next prompt"),
    ("f1", "this help"),
];

fn key_row(keys: &str, action: &str, styles: Styles, width: usize) -> Line<'static> {
    const COLUMN: usize = 14;
    if width <= COLUMN {
        return Line::styled(
            wrap::truncate(&format!("{keys} {action}"), width),
            styles.text(),
        );
    }
    let padding = COLUMN.saturating_sub(wrap::width(keys));
    Line::styled(
        format!(
            "{keys}{}{}",
            " ".repeat(padding),
            wrap::truncate(action, width - COLUMN)
        ),
        styles.text(),
    )
}

/// The context view: one plain-English summary of what the agent reports.
pub fn context(app: &App, styles: Styles, width: u16) -> Vec<Line<'static>> {
    let width = width.max(9) as usize;
    let usage = app.context_usage();
    let Some(session) = app.session.as_ref() else {
        return vec![Line::styled("No session is open", styles.dim())];
    };
    let mut lines = Vec::new();
    let used = usage
        .tokens
        .map(crate::app::format_tokens)
        .unwrap_or_else(|| "unknown".into());
    let budget = usage
        .budget
        .map(crate::app::format_tokens)
        .unwrap_or_else(|| "unknown".into());
    let percent = usage
        .percent
        .map(|percent| format!("{percent:.0}%"))
        .unwrap_or_else(|| "unknown".into());
    lines.push(Line::styled(
        format!("Context: {used} of {budget} input budget ({percent})"),
        styles.text(),
    ));
    if let Some(estimate) = session
        .context
        .as_ref()
        .and_then(|budget| budget.estimated_history_tokens)
    {
        lines.push(Line::styled(
            format!(
                "Estimated history: {} (an estimate, not a count)",
                crate::app::format_tokens(estimate)
            ),
            styles.dim(),
        ));
    }
    if let Some(compaction) = &session.compaction {
        lines.push(Line::styled(
            format!("Compacting now: {}", compaction.phase.label()),
            styles.warning(),
        ));
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        format!(
            "Model {} · reasoning {} · session {}",
            session.info.model,
            session.info.reasoning.label(),
            short_id(&session.info)
        ),
        styles.dim(),
    ));
    lines.push(Line::styled(
        format!(
            "Workspace {} · branch {}",
            app.workspace.display(),
            session
                .presentation
                .as_ref()
                .and_then(|presentation| presentation.git_branch.as_deref())
                .unwrap_or("-")
        ),
        styles.dim(),
    ));
    lines
        .into_iter()
        .map(|line| clip_line(line, width))
        .collect()
}

fn short_id(info: &SessionInfo) -> &str {
    let id = info.session_id.as_str();
    let short = id.strip_prefix("ses_").unwrap_or(id);
    &short[..short.len().min(8)]
}

fn clip_line(line: Line<'static>, width: usize) -> Line<'static> {
    let text = line.to_string();
    if wrap::width(&text) <= width {
        return line;
    }
    Line::styled(wrap::truncate(&text, width), line.style)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{Palette, ThemeKind};

    fn styles() -> Styles {
        Styles::new(Palette::for_kind(ThemeKind::Dark), false)
    }

    #[test]
    fn help_lists_every_command_the_palette_offers() {
        let text: String = help(styles(), 80)
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        for command in commands::COMMANDS {
            assert!(
                text.contains(&format!("/{}", command.name)),
                "help is missing /{}",
                command.name
            );
        }
    }

    #[test]
    fn help_rows_stay_within_the_width() {
        for width in [12, 20, 60] {
            for line in help(styles(), width) {
                assert!(wrap::width(&line.to_string()) <= width as usize);
            }
        }
    }

    #[test]
    fn a_view_frame_names_the_scope() {
        for (scope, expected) in [
            (DiffScope::Turn, "this turn"),
            (DiffScope::Session, "this session"),
            (DiffScope::Workspace, "the workspace"),
        ] {
            assert_eq!(scope_label(scope), expected);
        }
    }
}
