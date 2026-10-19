//! The slash-command table and its executor.

use super::Action;
use crate::protocol::Reasoning;

/// One palette entry: the command name and a one-line description.
pub struct Command {
    pub name: &'static str,
    pub description: &'static str,
}

pub const COMMANDS: &[Command] = &[
    Command {
        name: "new",
        description: "Start a new session",
    },
    Command {
        name: "sessions",
        description: "Open or delete a session",
    },
    Command {
        name: "resume",
        description: "Open or delete a session",
    },
    Command {
        name: "rename",
        description: "Rename this session",
    },
    Command {
        name: "model",
        description: "Switch the model",
    },
    Command {
        name: "reasoning",
        description: "Switch the reasoning level",
    },
    Command {
        name: "compact",
        description: "Compact the context now",
    },
    Command {
        name: "diff",
        description: "Review the changes",
    },
    Command {
        name: "tool",
        description: "Show the full output of a tool call",
    },
    Command {
        name: "context",
        description: "Show context usage",
    },
    Command {
        name: "copy",
        description: "Copy the last answer",
    },
    Command {
        name: "export",
        description: "Export the conversation as Markdown",
    },
    Command {
        name: "theme",
        description: "Switch the color theme",
    },
    Command {
        name: "reload",
        description: "Reload the agent configuration",
    },
    Command {
        name: "help",
        description: "Show keys and commands",
    },
    Command {
        name: "quit",
        description: "Exit minicore-tui",
    },
];

/// Result of executing one palette entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Executed {
    Action(Action),
    /// The command opens a picker or a full-screen view.
    Panel(super::Panel),
    /// Accepted, but there is nothing to do yet.
    Noop,
}

/// Splits `/name arg`; `None` when the input is not a command line.
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
        "tool" => Executed::Action(Action::ShowTool),
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
        .filter(|command| crate::ui::picker::fuzzy_match(command.name, input))
        .collect()
}

/// Whether the composer holds a command line that should open the palette.
pub fn opens_palette(composer: &str) -> bool {
    composer.starts_with('/') && !composer.contains('\n')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_splits_name_and_argument() {
        assert_eq!(parse("/rename my title"), Some(("rename", "my title")));
        assert_eq!(parse("/compact"), Some(("compact", "")));
        assert_eq!(parse("not a command"), None);
    }

    #[test]
    fn the_filter_is_a_fuzzy_subsequence_match() {
        let names: Vec<&str> = filter("rsn").iter().map(|command| command.name).collect();
        assert_eq!(names, ["reasoning"]);
        assert_eq!(filter("").len(), COMMANDS.len());
        assert!(filter("zzz").is_empty());
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
