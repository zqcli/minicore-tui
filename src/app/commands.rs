//! The slash-command table and its executor.

use super::Action;
use super::templates::Template;
use minicore_host_core::ModelDescription;
use minicore_host_protocol::{ApprovalMode, ReasoningPreference, WebSearchMode};

/// One palette entry: the command name, its argument hint and a one-line description.
#[derive(Clone, Copy)]
pub struct Command<'a> {
    pub name: &'a str,
    /// `<required>` or `[optional]`, empty when the command takes no argument.
    pub args: &'a str,
    pub description: &'a str,
}

// Grouped by use: session, context and workspace, model, session control,
// Agents, then the terminal itself.
pub const COMMANDS: &[Command<'static>] = &[
    Command {
        name: "new",
        args: "",
        description: "Start a new session in the selected Agent",
    },
    Command {
        name: "sessions",
        args: "",
        description: "Open, filter, rename or delete sessions",
    },
    Command {
        name: "session",
        args: "",
        description: "Show this session and its owning Agent",
    },
    Command {
        name: "rename",
        args: "[title]",
        description: "Rename this session; omit the title to edit the current one",
    },
    Command {
        name: "tasks",
        args: "",
        description: "Inspect and search async tasks and delegated children",
    },
    Command {
        name: "compact",
        args: "",
        description: "Compact the session context now",
    },
    Command {
        name: "context",
        args: "",
        description: "Show native context facts and past response usage",
    },
    Command {
        name: "diff",
        args: "",
        description: "Show Git working-tree changes, including untracked files",
    },
    Command {
        name: "copy",
        args: "",
        description: "Copy the last answer to the clipboard",
    },
    Command {
        name: "export",
        args: "[path]",
        description: "Export the conversation as Markdown",
    },
    Command {
        name: "model",
        args: "[id]",
        description: "Switch the model, or pick one",
    },
    Command {
        name: "reasoning",
        args: "[level]",
        description: "Set a valid reasoning level; omit it to pick one",
    },
    Command {
        name: "search",
        args: "[live|cached|off]",
        description: "Toggle provider web search or set its mode",
    },
    Command {
        name: "approval",
        args: "[ask|auto]",
        description: "Set approval for the next turn; omit it to pick one",
    },
    Command {
        name: "pause",
        args: "",
        description: "Pause input consumption in this session",
    },
    Command {
        name: "continue",
        args: "",
        description: "Resume this session",
    },
    Command {
        name: "default-session",
        args: "",
        description: "Set this session as its Agent's default",
    },
    Command {
        name: "agents",
        args: "",
        description: "Select an Agent",
    },
    Command {
        name: "agent-new",
        args: "<name>",
        description: "Copy the selected Agent's configuration into a new dynamic Agent",
    },
    Command {
        name: "agent-start",
        args: "",
        description: "Start the selected Agent",
    },
    Command {
        name: "agent-stop",
        args: "",
        description: "Stop the selected Agent",
    },
    Command {
        name: "agent-delete",
        args: "",
        description: "Review and delete the selected Agent and its captured sessions",
    },
    Command {
        name: "theme",
        args: "[dark|light]",
        description: "Set the colour theme; omit it to pick one",
    },
    Command {
        name: "reload",
        args: "",
        description: "Reload Host and Agent JSON from the bound configuration directory",
    },
    Command {
        name: "remote",
        args: "[on|off|key] [addr]",
        description: "Expose this Host over WSS/WS for minicore-web and show connection info",
    },
    Command {
        name: "unconfirmed",
        args: "",
        description: "Review local inputs whose acceptance is unconfirmed",
    },
    Command {
        name: "error",
        args: "",
        description: "Read or copy the last operation error in full",
    },
    Command {
        name: "help",
        args: "",
        description: "Show keys and commands",
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
    /// Put this draft back so its choices or current value can be edited.
    Prefill(String),
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

    /// The rewrite of a line no built-in command claims: `//` escapes a leading
    /// slash and a template expands; `Err` names a bare `/word` (or a single-line
    /// `/word arg` whose name is a close typo of a known command) that is neither.
    pub fn prompt_text<'a>(&self, text: &'a str) -> Result<Option<String>, &'a str> {
        if let Some(escaped) = text.strip_prefix('/').filter(|rest| rest.starts_with('/')) {
            return Ok(Some(escaped.to_owned()));
        }
        if let Some(expanded) =
            parse(text).and_then(|(name, argument)| self.expand_template(name, argument))
        {
            return Ok(Some(expanded));
        }
        if let Some(word) = bare_word(text) {
            return Err(word);
        }
        if let Some(word) = command_word(text) {
            let known = COMMANDS
                .iter()
                .map(|command| command.name)
                .chain(self.templates.iter().map(|template| template.name.as_str()));
            if closest(word, known).is_some() {
                return Err(word);
            }
        }
        Ok(None)
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
        "new" => Executed::Noop,
        "agents" => Executed::Panel(super::Panel::Agents),
        "agent-start" => Executed::Action(Action::StartAgent),
        "agent-stop" => Executed::Action(Action::StopAgent),
        "agent-delete" => Executed::Panel(super::Panel::AgentDelete),
        "default-session" => Executed::Action(Action::SetDefaultSession),
        "approval" => match arg {
            "" => Executed::Prefill("/approval ".into()),
            "ask" => Executed::Action(Action::SetApproval(ApprovalMode::Ask)),
            "auto" => Executed::Action(Action::SetApproval(ApprovalMode::Auto)),
            _ => Executed::Noop,
        },
        "agent-new" if !arg.is_empty() => Executed::Action(Action::CreateAgent(arg.to_owned())),
        "agent-new" => Executed::Noop,
        "session" => Executed::Panel(super::Panel::SessionInfo),
        "sessions" => Executed::Panel(super::Panel::Sessions),
        "pause" => Executed::Action(Action::Pause),
        "continue" => Executed::Action(Action::Resume),
        "rename" if !arg.is_empty() => Executed::Action(Action::Rename(arg.to_owned())),
        "rename" => Executed::Prefill("/rename ".into()),
        "tasks" => Executed::Panel(super::Panel::Delegations),
        "model" if arg.is_empty() => Executed::Panel(super::Panel::Models),
        "model" => Executed::Action(Action::SwitchModel(arg.to_owned())),
        "reasoning" if arg.is_empty() => Executed::Panel(super::Panel::Reasoning),
        "reasoning" => match parse_reasoning(arg) {
            Some(level) => Executed::Action(Action::SetReasoning(level)),
            None => Executed::Noop,
        },
        "search" => match arg {
            "" => Executed::Action(Action::SetWebSearch(None)),
            "live" => Executed::Action(Action::SetWebSearch(Some(WebSearchMode::Live))),
            "cached" => Executed::Action(Action::SetWebSearch(Some(WebSearchMode::Cached))),
            "off" => Executed::Action(Action::SetWebSearch(Some(WebSearchMode::Off))),
            _ => Executed::Noop,
        },
        "compact" => Executed::Action(Action::Compact),
        "diff" => Executed::Panel(super::Panel::Diff),
        "context" => Executed::Panel(super::Panel::Context),
        "copy" => Executed::Action(Action::CopyLastAnswer),
        "export" => Executed::Action(Action::Export((!arg.is_empty()).then(|| arg.to_owned()))),
        // Like every enumerated command, no argument offers the choices; F4
        // toggles. Parsing catches typos as `Noop` so the caller can say so.
        "theme" => match arg {
            "" => Executed::Prefill("/theme ".into()),
            "dark" => Executed::Action(Action::SetTheme(crate::config::ThemeKind::Dark)),
            "light" => Executed::Action(Action::SetTheme(crate::config::ThemeKind::Light)),
            _ => Executed::Noop,
        },
        "reload" => Executed::Action(Action::ReloadHost),
        "remote" => match arg {
            "" | "on" => Executed::Action(Action::RemoteStart(None)),
            "off" => Executed::Action(Action::RemoteStop),
            "key" => Executed::Action(Action::RemoteRotateKey),
            other => {
                if let Some(rest) = other.strip_prefix("on")
                    && rest.starts_with(char::is_whitespace)
                {
                    let addr = rest.trim();
                    Executed::Action(Action::RemoteStart(
                        (!addr.is_empty()).then(|| addr.to_owned()),
                    ))
                } else if other.parse::<std::net::SocketAddr>().is_ok() {
                    Executed::Action(Action::RemoteStart(Some(other.to_owned())))
                } else {
                    Executed::Noop
                }
            }
        },
        "unconfirmed" => Executed::Panel(super::Panel::Unconfirmed),
        "help" => Executed::Panel(super::Panel::Help),
        "error" => Executed::Panel(super::Panel::Error),
        "quit" => Executed::Action(Action::Quit),
        _ => return None,
    })
}

pub fn parse_reasoning(value: &str) -> Option<ReasoningPreference> {
    Some(match value {
        "auto" => ReasoningPreference::Auto,
        "disabled" => ReasoningPreference::Disabled,
        "low" => ReasoningPreference::Low,
        "medium" => ReasoningPreference::Medium,
        "high" => ReasoningPreference::High,
        "xhigh" => ReasoningPreference::XHigh,
        "max" => ReasoningPreference::Max,
        "ultra" => ReasoningPreference::Ultra,
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
    is_command_identifier(name).then_some(name)
}

/// The leading `/name` of a single-line `/name arg…` draft when `name` looks
/// like a command identifier (not a path like `/usr/bin/env foo`).
fn command_word(text: &str) -> Option<&str> {
    if text.contains('\n') {
        return None;
    }
    let (name, rest) = text
        .trim()
        .strip_prefix('/')?
        .split_once(char::is_whitespace)?;
    (!rest.trim().is_empty() && is_command_identifier(name)).then_some(name)
}

fn is_command_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic())
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
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
pub fn resolve_model<'a>(
    models: &'a [ModelDescription],
    arg: &str,
) -> Result<&'a ModelDescription, String> {
    if let Some(model) = models.iter().find(|model| model.model.as_str() == arg) {
        return Ok(model);
    }
    let found = super::overlay::matching_models(models, arg);
    match found.as_slice() {
        [] => Err(format!("Unknown model {arg}")),
        [model] => Ok(model),
        many => {
            let ids: Vec<&str> = many.iter().map(|model| model.model.as_str()).collect();
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
    if name == "reasoning" {
        return "Invalid reasoning level; expected auto|disabled|low|medium|high|xhigh|max|ultra"
            .into();
    }
    let args = COMMANDS
        .iter()
        .find(|command| command.name == name)
        .map_or("an argument", |command| command.args);
    if args.is_empty() {
        format!("/{name} takes no arguments")
    } else {
        format!("/{name} needs {args}")
    }
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
        assert_eq!(names("conti")[0], "continue");
        assert_eq!(names("re")[..2], ["rename", "reasoning"]);
        assert_eq!(names("sess"), ["sessions", "session", "default-session"]);
        assert_eq!(names("new")[0], "new");
    }

    #[test]
    fn a_missing_argument_names_the_hint() {
        assert_eq!(needs_argument("agent-new"), "/agent-new needs <name>");
        assert_eq!(needs_argument("theme"), "/theme needs [dark|light]");
    }

    #[test]
    fn theme_without_an_argument_offers_its_choices_and_an_explicit_one_wins() {
        assert_eq!(
            execute("theme", ""),
            Some(Executed::Prefill("/theme ".into()))
        );
        assert_eq!(
            execute("approval", ""),
            Some(Executed::Prefill("/approval ".into()))
        );
        assert_eq!(
            execute("rename", ""),
            Some(Executed::Prefill("/rename ".into()))
        );
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
    fn model_accepts_a_reference_and_new_uses_the_selected_agent() {
        assert!(matches!(
            execute("model", ""),
            Some(Executed::Panel(crate::app::Panel::Models))
        ));
        assert_eq!(
            execute("model", "luna"),
            Some(Executed::Action(Action::SwitchModel("luna".into())))
        );
        assert_eq!(execute("new", "ask"), Some(Executed::Noop));
        assert_eq!(
            execute("new", ""),
            Some(Executed::Action(Action::NewSession))
        );
        assert!(matches!(
            execute("session", ""),
            Some(Executed::Panel(crate::app::Panel::SessionInfo))
        ));
    }

    #[test]
    fn agent_and_session_control_commands_preserve_native_intents() {
        for (name, action) in [
            ("agent-start", Action::StartAgent),
            ("agent-stop", Action::StopAgent),
            ("default-session", Action::SetDefaultSession),
            ("pause", Action::Pause),
            ("continue", Action::Resume),
        ] {
            assert_eq!(execute(name, ""), Some(Executed::Action(action)));
        }
        assert_eq!(
            execute("agent-delete", ""),
            Some(Executed::Panel(crate::app::Panel::AgentDelete))
        );
        for (name, mode) in [("ask", ApprovalMode::Ask), ("auto", ApprovalMode::Auto)] {
            assert_eq!(
                execute("approval", name),
                Some(Executed::Action(Action::SetApproval(mode)))
            );
        }
        assert_eq!(execute("approval", "read_only"), Some(Executed::Noop));
        assert_eq!(execute("approval", "always"), Some(Executed::Noop));
        assert_eq!(
            execute("remote", ""),
            Some(Executed::Action(Action::RemoteStart(None)))
        );
        assert_eq!(
            execute("remote", "on [::1]:8443"),
            Some(Executed::Action(Action::RemoteStart(Some(
                "[::1]:8443".into()
            ))))
        );
        assert_eq!(
            execute("remote", "[::]:8443"),
            Some(Executed::Action(Action::RemoteStart(Some(
                "[::]:8443".into()
            ))))
        );
        assert_eq!(
            execute("remote", "off"),
            Some(Executed::Action(Action::RemoteStop))
        );
        assert_eq!(
            execute("remote", "key"),
            Some(Executed::Action(Action::RemoteRotateKey))
        );
        assert_eq!(execute("remote", "bogus"), Some(Executed::Noop));
    }

    #[test]
    fn redundant_control_shortcuts_are_not_builtins() {
        for removed in [
            "children", "child", "resume", "cancel", "wss", "recover", "load", "unload", "close",
        ] {
            assert!(execute(removed, "").is_none(), "removed /{removed}");
            assert!(COMMANDS.iter().all(|command| command.name != removed));
        }
    }

    fn model(id: &str) -> ModelDescription {
        serde_json::from_value(serde_json::json!({"model": id, "api":"responses", "context_window":200000, "supported_reasoning": [], "supports_tools":true})).unwrap()
    }

    #[test]
    fn a_model_argument_resolves_exactly_then_by_a_unique_fuzzy_match() {
        let models = [model("luna"), model("gpt-6-luna"), model("deep")];
        assert_eq!(
            resolve_model(&models, "luna").unwrap().model.as_str(),
            "luna"
        );
        assert_eq!(resolve_model(&models, "dp").unwrap().model.as_str(), "deep");
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
        assert_eq!(names("rev")[0], "review");
        let found = names("re");
        assert!(
            found.iter().position(|name| *name == "reload").unwrap()
                < found.iter().position(|name| *name == "review").unwrap()
        );
        assert!(
            found.contains(&"unconfirmed"),
            "the new review command participates in fuzzy matching"
        );
        let found = filter("review", &templates);
        assert_eq!(found[0].description, "Review staged changes");
    }

    #[test]
    fn misspelled_command_with_arguments_is_caught_while_paths_and_sentences_pass() {
        assert_eq!(command_word("/reanme minicore-web修改"), Some("reanme"));
        assert_eq!(
            closest("reanme", COMMANDS.iter().map(|command| command.name)),
            Some("rename")
        );
        assert_eq!(command_word("/usr/bin/env foo"), None);
        assert_eq!(
            closest("etc", COMMANDS.iter().map(|command| command.name)),
            None
        );
        assert_eq!(command_word("/reanme line1\nline2"), None);
    }
}
