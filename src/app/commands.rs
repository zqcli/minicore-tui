//! The slash-command table and its executor.

use super::Action;
use crate::protocol::Reasoning;

/// One palette entry: the command name, its argument hint and a one-line description.
pub struct Command {
    pub name: &'static str,
    /// `<required>` or `[optional]`, empty when the command takes no argument.
    pub args: &'static str,
    pub description: &'static str,
}

pub const COMMANDS: &[Command] = &[
    Command {
        name: "model",
        args: "",
        description: "Select the model (opens the selector)",
    },
    Command {
        name: "reasoning",
        args: "[level]",
        description: "Set the reasoning level, or pick one",
    },
    Command {
        name: "diff",
        args: "[session|workspace]",
        description: "Review file changes (the last turn by default)",
    },
    Command {
        name: "tool",
        args: "",
        description: "Show the full output of a tool call",
    },
    Command {
        name: "context",
        args: "",
        description: "Show context usage",
    },
    Command {
        name: "export",
        args: "[path]",
        description: "Export the conversation as Markdown",
    },
    Command {
        name: "copy",
        args: "",
        description: "Copy the last answer to the clipboard",
    },
    Command {
        name: "rename",
        args: "<title>",
        description: "Rename this session",
    },
    Command {
        name: "theme",
        args: "[dark|light]",
        description: "Switch the colour theme",
    },
    Command {
        name: "help",
        args: "",
        description: "Show keys and commands",
    },
    Command {
        name: "new",
        args: "",
        description: "Start a new session",
    },
    Command {
        name: "compact",
        args: "",
        description: "Compact the session context now",
    },
    Command {
        name: "resume",
        args: "",
        description: "Resume a different session",
    },
    Command {
        name: "sessions",
        args: "",
        description: "Open, filter or delete sessions",
    },
    Command {
        name: "reload",
        args: "",
        description: "Reload the agent configuration",
    },
    Command {
        name: "quit",
        args: "",
        description: "Quit minicore-tui",
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
            "dark" => Executed::Action(Action::SetTheme(crate::config::ThemeKind::Dark)),
            "light" => Executed::Action(Action::SetTheme(crate::config::ThemeKind::Light)),
            _ => Executed::Noop,
        },
        "reload" => Executed::Action(Action::ReloadAgent),
        "help" => Executed::Panel(super::Panel::Help),
        "quit" => Executed::Action(Action::Quit),
        _ => return None,
    })
}

/// Commands matching a palette filter: an exact name, then name prefixes,
/// then other fuzzy matches, each in table order.
pub fn filter(input: &str) -> Vec<&'static Command> {
    let mut matches: Vec<&Command> = COMMANDS
        .iter()
        .filter(|command| crate::ui::picker::fuzzy_match(command.name, input))
        .collect();
    matches.sort_by_key(|command| (command.name != input, !command.name.starts_with(input)));
    matches
}

/// Whether the composer holds a one-line `/command` draft.
pub fn is_command_line(composer: &str) -> bool {
    composer.starts_with('/') && !composer.contains('\n')
}

/// The notice for a command run without the argument it needs.
pub fn needs_argument(name: &str) -> String {
    let args = COMMANDS
        .iter()
        .find(|command| command.name == name)
        .map_or("an argument", |command| command.args);
    format!("/{name} needs {args}")
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
        assert!(filter("usr/local/bin").is_empty(), "a path matches nothing");
    }

    #[test]
    fn exact_and_prefix_matches_rank_first() {
        let names = |input| -> Vec<&str> { filter(input).iter().map(|c| c.name).collect() };
        assert_eq!(names("mo")[0], "model");
        assert_eq!(names("res"), ["resume", "reasoning"]);
        assert_eq!(names("re")[..3], ["reasoning", "rename", "resume"]);
        assert_eq!(names("new")[0], "new");
    }

    #[test]
    fn a_missing_argument_names_the_hint() {
        assert_eq!(needs_argument("rename"), "/rename needs <title>");
        assert_eq!(needs_argument("theme"), "/theme needs [dark|light]");
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
                crate::config::ThemeKind::Light
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
