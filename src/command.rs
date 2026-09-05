//! Outbound effects produced by `App::update` and executed by the main
//! loop (development spec 9.1), plus the slash-command parser (spec 23).
//! Executing a command never touches the app; failures flow back as
//! `AppEvent`s (e.g. `AppEvent::RpcSendFailed`).

use std::fmt;

use crate::protocol::OutgoingRequest;
use crate::theme::ThemeKind;

/// A side effect the main loop must perform on behalf of `App::update`.
#[derive(Debug)]
pub enum AppCommand {
    /// Write one already-numbered request line to the agent. The request id
    /// was allocated and registered in `pending_requests` inside `update`,
    /// before this command left it.
    Rpc(OutgoingRequest),
    /// Kill the agent child (the shutdown fallback path).
    KillChild,
    /// The agent process is fully gone (or never existed); leave the TUI.
    Exit,
}

/// A locally-interpreted `/` command (spec 23.2). These never turn into
/// RPC by themselves; `App::update` maps them to local state and only the
/// resulting requests (e.g. a transcript reload) hit the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalCommand {
    /// Open the new-session form from the catalog defaults.
    New,
    /// Open the session selector.
    Resume,
    /// Open the session selector.
    Sessions,
    /// Open the model selector (target: a new session).
    Model,
    /// Open the reasoning selector (target: a new session).
    Reasoning,
    /// Switch the color palette.
    Theme(ThemeKind),
    /// Clear the local transcript view and reload the active session.
    Clear,
    /// Open the help panel.
    Help,
    /// Open the agent-log panel.
    Logs,
    /// Cancel the active loop through `turn.cancel`.
    Cancel,
    /// Re-read the currently retained turn result through `turn.wait`.
    Refresh,
    /// Normal shutdown intent (`agent.shutdown` arrives in Phase 6).
    Quit,
    /// Close the active session (spec 12, 52).
    Close { confirm: bool },
    /// Delete a session (spec 12).
    Delete { confirm: bool },
}

/// Why a slash line was rejected locally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandIssue {
    /// The input does not start with `/` after leading whitespace.
    NotACommand,
    Unknown(String),
    InvalidArgs(String),
}

impl fmt::Display for CommandIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotACommand => write!(f, "not a command"),
            Self::Unknown(name) => write!(f, "unknown command `{name}`"),
            Self::InvalidArgs(message) => write!(f, "{message}"),
        }
    }
}

/// Parses `input` only when its first non-whitespace character is `/`
/// (spec 23.1). Unknown commands and unexpected arguments are local issues;
/// the caller shows a notice and never sends an RPC command.
pub fn parse_command(input: &str) -> Result<LocalCommand, CommandIssue> {
    let input = input.trim_start();
    let Some(rest) = input.strip_prefix('/') else {
        return Err(CommandIssue::NotACommand);
    };
    let (name, args) = match rest.split_once(char::is_whitespace) {
        Some((name, args)) => (name, args.trim()),
        None => (rest, ""),
    };
    let name = name.to_ascii_lowercase();

    let no_args = |cmd: LocalCommand| -> Result<LocalCommand, CommandIssue> {
        if args.is_empty() {
            Ok(cmd)
        } else {
            Err(CommandIssue::InvalidArgs(format!(
                "usage: /{name} (no arguments)"
            )))
        }
    };

    match name.as_str() {
        "" => Err(CommandIssue::Unknown("/".to_owned())),
        "new" => no_args(LocalCommand::New),
        "resume" => no_args(LocalCommand::Resume),
        "sessions" => no_args(LocalCommand::Sessions),
        "model" => no_args(LocalCommand::Model),
        "reasoning" => no_args(LocalCommand::Reasoning),
        "theme" => match args {
            "dark" => Ok(LocalCommand::Theme(ThemeKind::Dark)),
            "light" => Ok(LocalCommand::Theme(ThemeKind::Light)),
            _ => Err(CommandIssue::InvalidArgs(
                "usage: /theme <dark|light>".to_owned(),
            )),
        },
        "clear" => no_args(LocalCommand::Clear),
        "help" => no_args(LocalCommand::Help),
        "logs" => no_args(LocalCommand::Logs),
        "cancel" => no_args(LocalCommand::Cancel),
        "refresh" => no_args(LocalCommand::Refresh),
        "quit" => no_args(LocalCommand::Quit),
        "close" => match args {
            "" => Ok(LocalCommand::Close { confirm: false }),
            "confirm" | "--force" | "force" => Ok(LocalCommand::Close { confirm: true }),
            _ => Err(CommandIssue::InvalidArgs(
                "usage: /close [confirm]".to_owned(),
            )),
        },
        "delete" => match args {
            "" => Ok(LocalCommand::Delete { confirm: false }),
            "confirm" | "--force" | "force" => Ok(LocalCommand::Delete { confirm: true }),
            _ => Err(CommandIssue::InvalidArgs(
                "usage: /delete [confirm]".to_owned(),
            )),
        },
        other => Err(CommandIssue::Unknown(other.to_owned())),
    }
}

/// True when `input` (after leading whitespace) starts with `/`, i.e. a
/// line the composer should route to the slash parser instead of sending.
pub fn is_slash_command(input: &str) -> bool {
    input.trim_start().starts_with('/')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_implemented_command() {
        assert_eq!(parse_command("/new"), Ok(LocalCommand::New));
        assert_eq!(parse_command("/resume"), Ok(LocalCommand::Resume));
        assert_eq!(parse_command("/sessions"), Ok(LocalCommand::Sessions));
        assert_eq!(parse_command("/model"), Ok(LocalCommand::Model));
        assert_eq!(parse_command("/reasoning"), Ok(LocalCommand::Reasoning));
        assert_eq!(
            parse_command("/theme dark"),
            Ok(LocalCommand::Theme(ThemeKind::Dark))
        );
        assert_eq!(
            parse_command("/theme light"),
            Ok(LocalCommand::Theme(ThemeKind::Light))
        );
        assert_eq!(parse_command("/clear"), Ok(LocalCommand::Clear));
        assert_eq!(parse_command("/help"), Ok(LocalCommand::Help));
        assert_eq!(parse_command("/logs"), Ok(LocalCommand::Logs));
        assert_eq!(parse_command("/cancel"), Ok(LocalCommand::Cancel));
        assert_eq!(parse_command("/refresh"), Ok(LocalCommand::Refresh));
        assert_eq!(parse_command("/quit"), Ok(LocalCommand::Quit));
        assert_eq!(
            parse_command("/close"),
            Ok(LocalCommand::Close { confirm: false })
        );
        assert_eq!(
            parse_command("/close confirm"),
            Ok(LocalCommand::Close { confirm: true })
        );
        assert_eq!(
            parse_command("/delete"),
            Ok(LocalCommand::Delete { confirm: false })
        );
        assert_eq!(
            parse_command("/delete confirm"),
            Ok(LocalCommand::Delete { confirm: true })
        );
    }

    #[test]
    fn leading_whitespace_and_case_are_flexible_but_trailing_args_are_not() {
        assert_eq!(parse_command("   /new  "), Ok(LocalCommand::New));
        assert_eq!(parse_command("/NEW"), Ok(LocalCommand::New));
        assert_eq!(
            parse_command("/Theme   light  "),
            Ok(LocalCommand::Theme(ThemeKind::Light))
        );
        assert!(matches!(
            parse_command("/clear extra"),
            Err(CommandIssue::InvalidArgs(_))
        ));
        assert!(matches!(
            parse_command("/new something"),
            Err(CommandIssue::InvalidArgs(_))
        ));
        assert!(matches!(
            parse_command("/theme blue"),
            Err(CommandIssue::InvalidArgs(_))
        ));
    }

    #[test]
    fn unknown_and_empty_commands_are_local_issues() {
        assert_eq!(
            parse_command("/fork"),
            Err(CommandIssue::Unknown("fork".to_owned()))
        );
        assert_eq!(
            parse_command("/"),
            Err(CommandIssue::Unknown("/".to_owned()))
        );
        assert_eq!(parse_command("hello"), Err(CommandIssue::NotACommand));
        assert_eq!(
            parse_command("  not a slash"),
            Err(CommandIssue::NotACommand)
        );
        assert!(!is_slash_command("plain text"));
        assert!(is_slash_command("  /new"));
    }
}
