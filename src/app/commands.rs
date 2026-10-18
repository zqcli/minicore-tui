//! The slash-command table and its executor.

use super::Action;
use crate::protocol::Reasoning;

/// One palette entry: the command name, a one-line description and how to run
/// it. `arg` documents the optional argument for the palette hint row.
pub struct Command {
    pub name: &'static str,
    pub description: &'static str,
    pub arg: Option<&'static str>,
}

pub const COMMANDS: &[Command] = &[
    Command {
        name: "new",
        description: "Start a new session",
        arg: None,
    },
    Command {
        name: "sessions",
        description: "Open or delete a session",
        arg: None,
    },
    Command {
        name: "resume",
        description: "Open or delete a session",
        arg: None,
    },
    Command {
        name: "rename",
        description: "Rename this session",
        arg: Some("title"),
    },
    Command {
        name: "model",
        description: "Switch the model",
        arg: None,
    },
    Command {
        name: "reasoning",
        description: "Switch the reasoning level",
        arg: None,
    },
    Command {
        name: "compact",
        description: "Compact the context now",
        arg: None,
    },
    Command {
        name: "diff",
        description: "Review the changes",
        arg: Some("turn|session|workspace"),
    },
    Command {
        name: "context",
        description: "Show context usage",
        arg: None,
    },
    Command {
        name: "copy",
        description: "Copy the last answer",
        arg: None,
    },
    Command {
        name: "export",
        description: "Export the conversation as Markdown",
        arg: Some("path"),
    },
    Command {
        name: "theme",
        description: "Switch the color theme",
        arg: Some("dark|light"),
    },
    Command {
        name: "reload",
        description: "Reload the agent configuration",
        arg: None,
    },
    Command {
        name: "help",
        description: "Show keys and commands",
        arg: None,
    },
    Command {
        name: "quit",
        description: "Exit minicore-tui",
        arg: None,
    },
];

impl Command {
    /// The palette row: `name — description`.
    pub fn row(&self) -> String {
        format!("/{}  {}", self.name, self.description)
    }
}

/// Fuzzy subsequence match, used by the palette filter.
pub fn matches(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let mut haystack = haystack.chars().flat_map(char::to_lowercase);
    needle
        .chars()
        .flat_map(char::to_lowercase)
        .all(|expected| haystack.any(|actual| actual == expected))
}

/// Today as `YYYY-MM-DD` in UTC, for default export names.
pub fn today() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    let (year, month, day) = civil_from_days((seconds / 86_400) as i64);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Days since the Unix epoch to a civil date (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Result of executing one palette entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Executed {
    Action(Action),
    /// The command opens a picker or a full-screen view.
    Panel(super::Panel),
    /// Accepted, but there is nothing to do yet.
    Noop,
}

/// Parses and resolves `/name arg`. Returns `None` when the input is not a
/// command line at all; unknown names resolve to `Executed::Noop` after a
/// notice is produced by the caller.
pub fn parse(input: &str) -> Option<(&str, &str)> {
    let input = input.trim();
    let rest = input.strip_prefix('/')?;
    let (name, arg) = match rest.split_once(char::is_whitespace) {
        Some((name, arg)) => (name, arg.trim()),
        None => (rest, ""),
    };
    Some((name, arg))
}

/// Resolves one command line to an action or a panel.
pub fn execute(name: &str, arg: &str) -> Option<Executed> {
    Some(match name {
        "new" => Executed::Action(Action::NewSession),
        "sessions" | "resume" => Executed::Panel(super::Panel::Sessions),
        "rename" if !arg.is_empty() => Executed::Action(Action::Rename(arg.to_owned())),
        "rename" => Executed::Noop,
        "model" => Executed::Panel(super::Panel::Models),
        "reasoning" => match Reasoning::parse(arg) {
            Some(level) => Executed::Action(Action::SetReasoning(level)),
            None => Executed::Panel(super::Panel::Reasoning),
        },
        "compact" => Executed::Action(Action::Compact),
        "diff" => Executed::Panel(super::Panel::Diff(match arg {
            "session" => super::DiffScope::Session,
            "workspace" => super::DiffScope::Workspace,
            _ => super::DiffScope::Turn,
        })),
        "context" => Executed::Panel(super::Panel::Context),
        "copy" => Executed::Action(Action::CopyLastAnswer),
        "export" => Executed::Action(Action::Export((!arg.is_empty()).then(|| arg.to_owned()))),
        // No argument toggles between the two palettes; an explicit argument
        // wins. Parsing catches typos as `Noop` so the caller can say so.
        "theme" => match arg {
            "" => Executed::Action(Action::ToggleTheme),
            "dark" => Executed::Action(Action::SetTheme(crate::theme::ThemeKind::Dark)),
            "light" => Executed::Action(Action::SetTheme(crate::theme::ThemeKind::Light)),
            _ => Executed::Noop,
        },
        "reload" => Executed::Action(Action::ReloadAgent),
        "help" => Executed::Panel(super::Panel::Help),
        "quit" => Executed::Action(Action::Quit),
        _ => return None,
    })
}

/// Commands matching a palette filter, in table order.
pub fn filter(input: &str) -> Vec<&'static Command> {
    COMMANDS
        .iter()
        .filter(|command| matches(command.name, input))
        .collect()
}

/// `/` palette rows for the picker, deduplicated by name.
pub fn palette_rows(input: &str) -> Vec<(String, &'static Command)> {
    filter(input)
        .into_iter()
        .map(|command| (command.row(), command))
        .collect()
}

/// Hint row for the selected command.
pub fn hint(command: &Command) -> String {
    match command.arg {
        Some(arg) => format!("/{} <{arg}>", command.name),
        None => format!("/{}", command.name),
    }
}

/// Whether the composer holds a command line that should open the palette.
pub fn opens_palette(composer: &str, cursor_at_end: bool) -> bool {
    cursor_at_end && composer.starts_with('/') && !composer.contains('\n')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_filter_accepts_subsequences() {
        assert!(matches("reasoning", "rsn"));
        assert!(matches("compact", "compact"));
        assert!(!matches("compact", "zzz"));
        assert!(matches("sessions", ""));
    }

    #[test]
    fn parse_splits_name_and_argument() {
        assert_eq!(parse("/rename my title"), Some(("rename", "my title")));
        assert_eq!(parse("/compact"), Some(("compact", "")));
        assert_eq!(parse("not a command"), None);
    }

    #[test]
    fn slash_rows_pair_names_with_descriptions() {
        let rows = palette_rows("re");
        assert!(rows.iter().any(|(row, _)| row.starts_with("/reasoning")));
        assert!(rows.iter().any(|(row, _)| row.starts_with("/rename")));
    }

    #[test]
    fn today_is_an_iso_date() {
        let today = today();
        assert_eq!(today.len(), 10);
        assert!(today.starts_with("20"));
    }

    #[test]
    fn theme_without_an_argument_toggles_and_an_explicit_one_wins() {
        assert!(matches!(
            execute("theme", ""),
            Some(Executed::Action(crate::app::Action::ToggleTheme))
        ));
        assert!(matches!(
            execute("theme", "light"),
            Some(Executed::Action(crate::app::Action::SetTheme(
                crate::theme::ThemeKind::Light
            )))
        ));
        assert!(matches!(
            execute("theme", "dark"),
            Some(Executed::Action(_))
        ));
        assert!(matches!(execute("theme", "purple"), Some(Executed::Noop)));
    }

    #[test]
    fn diff_scope_comes_from_the_argument() {
        assert!(matches!(
            execute("diff", "workspace"),
            Some(Executed::Panel(crate::app::Panel::Diff(
                crate::app::DiffScope::Workspace
            )))
        ));
        assert!(matches!(
            execute("diff", "session"),
            Some(Executed::Panel(crate::app::Panel::Diff(
                crate::app::DiffScope::Session
            )))
        ));
        assert!(matches!(
            execute("diff", ""),
            Some(Executed::Panel(crate::app::Panel::Diff(
                crate::app::DiffScope::Turn
            )))
        ));
    }

    #[test]
    fn export_defaults_to_no_path_but_accepts_one() {
        assert!(matches!(
            execute("export", ""),
            Some(Executed::Action(crate::app::Action::Export(None)))
        ));
        assert!(matches!(
            execute("export", "out.md"),
            Some(Executed::Action(crate::app::Action::Export(Some(_))))
        ));
    }

    #[test]
    fn known_commands_resolve_and_unknown_ones_do_not() {
        assert!(matches!(execute("new", ""), Some(Executed::Action(_))));
        assert!(matches!(
            execute("sessions", ""),
            Some(Executed::Panel(crate::app::Panel::Sessions))
        ));
        assert!(execute("nonsense", "").is_none());
    }
}
