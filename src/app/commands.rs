//! The slash-command table and its executor.

use super::Action;
use super::templates::Template;
use crate::protocol::{ModelInfo, Reasoning};

/// One palette entry: the command name, its argument hint and a one-line description.
#[derive(Clone, Copy)]
pub struct Command<'a> {
    pub name: &'a str,
    /// `<required>` or `[optional]`, empty when the command takes no argument.
    pub args: &'a str,
    pub description: &'a str,
}

pub const COMMANDS: &[Command<'static>] = &[
    Command {
        name: "model",
        args: "[id]",
        description: "Switch the model, or pick one",
    },
    Command {
        name: "reasoning",
        args: "[level]",
        description: "Set the reasoning level, or pick one",
    },
    Command {
        name: "diff",
        args: "",
        description: "Show Git working-tree changes, including untracked files",
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
        args: "[profile]",
        description: "Start a new session",
    },
    Command {
        name: "session",
        args: "",
        description: "Show this session's id, model and totals",
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
        name: "remote",
        args: "[on|off|status|password [text]]",
        description: "Let a phone drive this TUI, or set its password",
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

impl super::App {
    /// The text of template `name` run with `arg`, if there is such a template.
    pub fn expand_template(&self, name: &str, arg: &str) -> Option<String> {
        let template = self
            .templates
            .iter()
            .find(|template| template.name == name)?;
        Some(template.expand(arg))
    }
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
        "new" if arg.is_empty() => Executed::Action(Action::NewSession),
        "new" => Executed::Action(Action::NewSessionWith(arg.to_owned())),
        "session" => Executed::Panel(super::Panel::SessionInfo),
        "sessions" | "resume" => Executed::Panel(super::Panel::Sessions),
        "rename" if !arg.is_empty() => Executed::Action(Action::Rename(arg.to_owned())),
        "rename" => Executed::Noop,
        "model" if arg.is_empty() => Executed::Panel(super::Panel::Models),
        "model" => Executed::Action(Action::SwitchModel(arg.to_owned())),
        "reasoning" => match Reasoning::parse(arg) {
            Some(level) => Executed::Action(Action::SetReasoning(level)),
            None => Executed::Panel(super::Panel::Reasoning),
        },
        "compact" => Executed::Action(Action::Compact),
        "diff" => Executed::Panel(super::Panel::Diff),
        "context" => Executed::Panel(super::Panel::Context),
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
        "remote" => Executed::Action(Action::Remote(arg.to_owned())),
        _ => return None,
    })
}

/// Commands and templates matching a palette filter: an exact name, then name
/// prefixes, then other fuzzy matches, each in table order.
pub fn filter<'a>(input: &str, templates: &'a [Template]) -> Vec<Command<'a>> {
    let mut matches: Vec<Command<'a>> = COMMANDS
        .iter()
        .copied()
        .chain(templates.iter().map(Template::command))
        .filter(|command| crate::app::picker::fuzzy_match(command.name, input))
        .collect();
    matches.sort_by_key(|command| (command.name != input, !command.name.starts_with(input)));
    matches
}

/// The `name` of a draft that is just `/name`: a typo for a command, unlike a
/// path (`/usr/bin`) or a sentence (`/etc is where…`), which are prompts.
pub fn bare_word(text: &str) -> Option<&str> {
    let name = text.trim().strip_prefix('/')?;
    let mut chars = name.chars();
    let word = chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic())
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'));
    word.then_some(name)
}

/// The known name nearest to `name`, when it is only a typo away: one edit
/// for a short word, two for a longer one.
pub fn closest<'a>(name: &str, known: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    let allowed = if name.len() <= 4 { 1 } else { 2 };
    known
        .map(|candidate| (edit_distance(name, candidate), candidate))
        .filter(|(distance, _)| *distance <= allowed)
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, candidate)| candidate)
}

fn edit_distance(left: &str, right: &str) -> usize {
    let right: Vec<char> = right.chars().collect();
    let mut row: Vec<usize> = (0..=right.len()).collect();
    for (i, a) in left.chars().enumerate() {
        let mut diagonal = std::mem::replace(&mut row[0], i + 1);
        for (j, b) in right.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (diagonal + usize::from(a != *b))
                .min(above + 1)
                .min(row[j] + 1);
            diagonal = above;
        }
    }
    row[right.len()]
}

/// The notice for a bare `/word` that names nothing.
pub fn unknown_notice(name: &str, closest: Option<&str>) -> String {
    match closest {
        Some(near) => format!(
            "Unknown command /{name} · did you mean /{near}? · enter again sends it as text"
        ),
        None => format!("Unknown command /{name} · enter again sends it as text"),
    }
}

/// The model `arg` names: an exact id, else the one id it fuzzy-matches.
pub fn resolve_model<'a>(models: &'a [ModelInfo], arg: &str) -> Result<&'a ModelInfo, String> {
    if let Some(model) = models.iter().find(|model| model.id == arg) {
        return Ok(model);
    }
    let found = super::overlay::matching_models(models, arg);
    match found.as_slice() {
        [] => Err(format!("Unknown model {arg}")),
        [model] => Ok(model),
        many => {
            let ids: Vec<&str> = many.iter().map(|model| model.id.as_str()).collect();
            Err(format!("{arg} matches {}: {}", many.len(), ids.join(", ")))
        }
    }
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
        let names: Vec<&str> = filter("rsn", &[])
            .iter()
            .map(|command| command.name)
            .collect();
        assert_eq!(names, ["reasoning"]);
        assert_eq!(filter("", &[]).len(), COMMANDS.len());
        assert!(filter("zzz", &[]).is_empty());
        assert!(
            filter("usr/local/bin", &[]).is_empty(),
            "a path matches nothing"
        );
    }

    #[test]
    fn exact_and_prefix_matches_rank_first() {
        let names = |input| -> Vec<&str> { filter(input, &[]).iter().map(|c| c.name).collect() };
        assert_eq!(names("mo")[0], "model");
        assert_eq!(names("res"), ["resume", "reasoning"]);
        assert_eq!(names("re")[..3], ["reasoning", "rename", "resume"]);
        assert_eq!(names("sess"), ["session", "sessions"]);
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
    fn diff_takes_no_argument_and_ignores_extra_text() {
        for arg in ["", "session", "workspace", "other text"] {
            assert_eq!(
                execute("diff", arg),
                Some(Executed::Panel(crate::app::Panel::Diff))
            );
        }
        let command = COMMANDS
            .iter()
            .find(|command| command.name == "diff")
            .unwrap();
        assert!(command.args.is_empty());
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

    #[test]
    fn only_a_bare_word_can_be_a_mistyped_command() {
        assert_eq!(bare_word("/modle"), Some("modle"));
        assert_eq!(bare_word("  /foo-bar_2 "), Some("foo-bar_2"));
        for prompt in [
            "/usr/bin",
            "/usr/local/bin/foo is a path",
            "/etc is where config lives",
            "/",
            "//foo",
            "/9lives",
            "/foo\nbar",
            "plain text",
        ] {
            assert_eq!(bare_word(prompt), None, "{prompt:?} is a prompt");
        }
    }

    #[test]
    fn the_closest_command_is_offered_only_for_a_near_miss() {
        let known = || COMMANDS.iter().map(|command| command.name);
        assert_eq!(closest("modle", known()), Some("model"));
        assert_eq!(closest("sesions", known()), Some("sessions"));
        assert_eq!(closest("foo", known()), None);
        assert_eq!(
            unknown_notice("modle", Some("model")),
            "Unknown command /modle · did you mean /model? · enter again sends it as text"
        );
        assert_eq!(
            unknown_notice("foo", None),
            "Unknown command /foo · enter again sends it as text"
        );
    }

    #[test]
    fn model_and_new_take_an_argument() {
        assert!(matches!(
            execute("model", ""),
            Some(Executed::Panel(crate::app::Panel::Models))
        ));
        assert_eq!(
            execute("model", "luna"),
            Some(Executed::Action(Action::SwitchModel("luna".into())))
        );
        assert_eq!(
            execute("new", "ask"),
            Some(Executed::Action(Action::NewSessionWith("ask".into())))
        );
        assert_eq!(
            execute("new", ""),
            Some(Executed::Action(Action::NewSession))
        );
        assert!(matches!(
            execute("session", ""),
            Some(Executed::Panel(crate::app::Panel::SessionInfo))
        ));
    }

    fn model(id: &str) -> ModelInfo {
        serde_json::from_value(serde_json::json!({"id": id, "supported_reasoning": []})).unwrap()
    }

    #[test]
    fn a_model_argument_resolves_exactly_then_by_a_unique_fuzzy_match() {
        let models = [model("luna"), model("gpt-6-luna"), model("deep")];
        assert_eq!(resolve_model(&models, "luna").unwrap().id, "luna");
        assert_eq!(resolve_model(&models, "dp").unwrap().id, "deep");
        assert_eq!(
            resolve_model(&models, "lun").unwrap_err(),
            "lun matches 2: luna, gpt-6-luna"
        );
        assert_eq!(
            resolve_model(&models, "zzz").unwrap_err(),
            "Unknown model zzz"
        );
    }

    #[test]
    fn templates_join_the_command_list_after_the_builtins_rank() {
        let templates = [Template::parse("review".into(), "Review staged changes")];
        let names = |input| -> Vec<&str> {
            let found = filter(input, &templates);
            found.iter().map(|command| command.name).collect()
        };
        assert_eq!(names("rev"), ["review"]);
        assert_eq!(names("re").last(), Some(&"review"));
        let found = filter("review", &templates);
        assert_eq!(found[0].description, "Review staged changes");
    }
}
