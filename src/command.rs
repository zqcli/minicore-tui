//! Outbound effects produced by `App::update` and executed by the main
//! loop (development spec 9.1), plus the slash-command parser (spec 23).
//! Executing a command never touches the app; failures flow back as
//! `AppEvent`s (e.g. `AppEvent::RpcSendFailed`).

use std::fmt;

use crate::protocol::OutgoingRequest;
use crate::theme::ThemeKind;

/// A side effect the main loop must perform on behalf of `App::update`.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum AppCommand {
    /// Write one already-numbered request line to the agent. The request id
    /// was allocated and registered in `pending_requests` inside `update`,
    /// before this command left it.
    Rpc(OutgoingRequest),
    /// Kill the agent child (the shutdown fallback path).
    KillChild,
    /// Copy already-sanitized selected presentation text through the single
    /// clipboard adapter. Its Debug output is length-only.
    CopySelection(ClipboardText),
    /// Start one owned loaded-content search scan.
    LocalScan(Box<crate::jobs::LocalScanRequest>),
    /// Start the one owned export writer with an already-validated target and
    /// the bounded channel it drains (spec §17.4).
    StartExport(Box<StartExportRequest>),
    /// Persist the local TUI config through one owned blocking job.
    PersistConfig(Box<PersistConfigRequest>),
    /// Start one owned external-editor job for the captured draft.
    StartEditor(Box<StartEditorRequest>),
    /// The agent process is fully gone (or never existed); leave the TUI.
    Exit,
}

/// Everything the owned export writer needs. The receiver is the bounded
/// channel the App feeds; the path was validated without touching the file
/// system.
pub struct StartExportRequest {
    pub capture: crate::jobs::ExportCapture,
    pub target: std::path::PathBuf,
    pub overwrite: bool,
    /// The App and the job share this token: setting it makes the writer abort
    /// immediately even while its bounded channel is full.
    pub cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub rx: tokio::sync::mpsc::Receiver<crate::jobs::ExportInbound>,
    pub spec: crate::state::export::ExportSpec,
}

impl std::fmt::Debug for StartExportRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StartExportRequest")
            .field("target", &self.target.display().to_string())
            .field("overwrite", &self.overwrite)
            .field("include_thinking", &self.spec.include_thinking)
            .field("include_tool", &self.spec.include_tool)
            .field("raw_oversized", &self.spec.raw_oversized)
            .finish()
    }
}

#[derive(Debug)]
pub struct PersistConfigRequest {
    pub path: std::path::PathBuf,
    pub config: crate::config::TuiConfig,
}

#[derive(Debug)]
pub struct StartEditorRequest {
    pub capture: crate::jobs::EditorCapture,
    pub editor: crate::config::EditorConfig,
    pub draft: String,
}

#[derive(Clone, Eq, PartialEq)]
pub struct ClipboardText {
    text: String,
    /// The capture identity: a completion for another session/revision is
    /// stale feedback and never decorates a newer selection (spec §5.5).
    session_id: String,
    revision: u64,
}

impl ClipboardText {
    pub fn new(text: String, session_id: String, revision: u64) -> Self {
        Self {
            text,
            session_id,
            revision,
        }
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }
}

impl fmt::Debug for ClipboardText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClipboardText")
            .field("chars", &self.text.chars().count())
            .field("bytes", &self.text.len())
            .field("revision", &self.revision)
            .finish()
    }
}

/// A locally-interpreted `/` command (spec 23.2). These never turn into
/// RPC by themselves; `App::update` maps them to local state and only the
/// resulting requests (e.g. a transcript reload) hit the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalCommand {
    Tool(crate::state::tool::ToolKey),
    /// Create a session directly in the current workspace with the most
    /// recent explicit configuration (spec §10.4); no catalog-form detour.
    New,
    /// Open the pre-filled new-session form (`/new form`, Ctrl+N).
    NewForm,
    /// Open the conversation search with an optional literal (spec §17.1).
    Search {
        query: String,
        scope: crate::state::search::SearchScope,
    },
    /// Jump to the previous/next user prompt (`/prev`, `/next`).
    PromptJump(i32),
    /// Jump to the newest user prompt (`/latest`).
    Latest,
    /// Copy already-rendered text through the single clipboard owner
    /// (spec §17.3). `/copy` without an argument copies the last reply.
    Copy {
        target: CopyTarget,
    },
    /// Open the local export form, optionally pre-filled with a target path
    /// (spec §17.4). `raw_oversized` selects the explicit raw-JSON streaming
    /// entry for items above the automatic decode ceiling. Nothing is written
    /// until the form is submitted.
    Export {
        target: String,
        raw_oversized: bool,
    },
    /// Open the session selector.
    Resume,
    /// Open the session selector.
    Sessions,
    /// Open the model selector (target: a new session).
    Model,
    /// Open the reasoning selector (target: a new session).
    Reasoning,
    /// Open the local TUI settings form.
    Settings,
    /// Open the external editor for the current draft.
    Editor,
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
    /// Read the current context/preparation snapshot.
    Context,
    /// Start one manual compaction operation.
    Compact,
    /// Re-read only this session's view data (history/presentation).
    Refresh,
    /// Rename a session; `None` opens the rename dialog (spec §10.4).
    Rename {
        title: Option<String>,
    },
    /// Reload Agent configuration and refresh safe read-only TUI state.
    Reload,
    /// Normal shutdown intent (`agent.shutdown` arrives in Phase 6).
    Quit,
    /// Close the active session (spec 12, 52).
    Close {
        confirm: bool,
    },
    /// Delete a session (spec 12).
    Delete {
        confirm: bool,
    },
}

/// What `/copy` takes (spec §17.3). Reusing the existing hit/copy operations
/// means no copy path ever issues a remote read to "complete" a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyTarget {
    /// The last completed Assistant reply, excluding Thinking.
    LastReply,
    /// The message under the selection anchor or the top of the viewport.
    Message,
    /// The fenced code block of that message.
    Code,
    /// The existing mouse/keyboard selection.
    Selection,
}

impl CopyTarget {
    pub fn label(self) -> &'static str {
        match self {
            Self::LastReply => "last reply",
            Self::Message => "message",
            Self::Code => "code block",
            Self::Selection => "selection",
        }
    }
}

/// One implemented slash command. The table is the single source for the
/// parser, the help panel and the completion popup, so an entry can never
/// advertise a command the reducer does not execute (spec §10.5, §23).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandSpec {
    pub name: &'static str,
    /// Usage line shown by help and completion.
    pub usage: &'static str,
    /// One-line description shown by the help panel.
    pub summary: &'static str,
    pub args: CommandArgs,
}

/// Argument shapes the parser knows how to validate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandArgs {
    ToolRef,
    /// No arguments accepted.
    None,
    /// `/theme <dark|light>`.
    Theme,
    /// `/close [confirm]`, `/delete [confirm]`.
    OptionalConfirm,
    /// `/rename [title]`: the title is the rest of the line.
    OptionalTitle,
    /// `/new [form]`.
    NewForm,
    /// `/search [full] [literal]`: the literal is the rest of the line; the
    /// leading `full` keyword starts the explicit full-session scan.
    Search,
    /// `/copy [last|message|code|selection]`.
    Copy,
    /// `/export [path]`: the path is the rest of the line.
    OptionalPath,
}

/// Every command the reducer can execute. Methods added in a later stage must
/// be listed here only together with their reducer arm.
pub const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        name: "tool",
        usage: "/tool <session_id> <loop_id> <request_index> <tool_call_id>",
        summary: "inspect one exact tool invocation; closing never cancels it",
        args: CommandArgs::ToolRef,
    },
    CommandSpec {
        name: "new",
        usage: "/new [form]",
        summary: "create a session here with the recent explicit model/profile/reasoning",
        args: CommandArgs::NewForm,
    },
    CommandSpec {
        name: "search",
        usage: "/search [full] [literal]",
        summary: "find literal text in loaded content, or scan the full session",
        args: CommandArgs::Search,
    },
    CommandSpec {
        name: "copy",
        usage: "/copy [last|message|code|selection]",
        summary: "copy the last reply, the current message, its code, or the selection",
        args: CommandArgs::Copy,
    },
    CommandSpec {
        name: "export",
        usage: "/export [raw] [path]",
        summary: "write this conversation's saved history to a local Markdown file",
        args: CommandArgs::OptionalPath,
    },
    CommandSpec {
        name: "prev",
        usage: "/prev",
        summary: "jump to the previous user prompt",
        args: CommandArgs::None,
    },
    CommandSpec {
        name: "next",
        usage: "/next",
        summary: "jump to the next user prompt",
        args: CommandArgs::None,
    },
    CommandSpec {
        name: "latest",
        usage: "/latest",
        summary: "jump to the newest user prompt and follow the tail",
        args: CommandArgs::None,
    },
    CommandSpec {
        name: "resume",
        usage: "/resume",
        summary: "continue the read-only session, or open the session selector",
        args: CommandArgs::None,
    },
    CommandSpec {
        name: "sessions",
        usage: "/sessions",
        summary: "open the session selector",
        args: CommandArgs::None,
    },
    CommandSpec {
        name: "model",
        usage: "/model",
        summary: "choose the model for a new session",
        args: CommandArgs::None,
    },
    CommandSpec {
        name: "reasoning",
        usage: "/reasoning",
        summary: "choose the reasoning level for a new session",
        args: CommandArgs::None,
    },
    CommandSpec {
        name: "settings",
        usage: "/settings",
        summary: "edit local TUI preferences and launch paths",
        args: CommandArgs::None,
    },
    CommandSpec {
        name: "editor",
        usage: "/editor",
        summary: "edit the current draft in the configured external editor",
        args: CommandArgs::None,
    },
    CommandSpec {
        name: "theme",
        usage: "/theme <dark|light>",
        summary: "switch the color palette",
        args: CommandArgs::Theme,
    },
    CommandSpec {
        name: "clear",
        usage: "/clear",
        summary: "re-read the local transcript view (never writes to the Store)",
        args: CommandArgs::None,
    },
    CommandSpec {
        name: "refresh",
        usage: "/refresh",
        summary: "re-read this session's history and presentation data",
        args: CommandArgs::None,
    },
    CommandSpec {
        name: "rename",
        usage: "/rename [title]",
        summary: "rename a session; without a title the rename dialog opens",
        args: CommandArgs::OptionalTitle,
    },
    CommandSpec {
        name: "help",
        usage: "/help",
        summary: "open the help panel",
        args: CommandArgs::None,
    },
    CommandSpec {
        name: "logs",
        usage: "/logs",
        summary: "open the agent log panel",
        args: CommandArgs::None,
    },
    CommandSpec {
        name: "cancel",
        usage: "/cancel",
        summary: "cancel the active loop",
        args: CommandArgs::None,
    },
    CommandSpec {
        name: "context",
        usage: "/context",
        summary: "read the current context/preparation snapshot",
        args: CommandArgs::None,
    },
    CommandSpec {
        name: "compact",
        usage: "/compact",
        summary: "start one manual compaction",
        args: CommandArgs::None,
    },
    CommandSpec {
        name: "reload",
        usage: "/reload",
        summary: "reload Agent configuration and the catalogs",
        args: CommandArgs::None,
    },
    CommandSpec {
        name: "quit",
        usage: "/quit",
        summary: "shut the agent down and leave",
        args: CommandArgs::None,
    },
    CommandSpec {
        name: "close",
        usage: "/close [confirm]",
        summary: "close the active session (results are still received)",
        args: CommandArgs::OptionalConfirm,
    },
    CommandSpec {
        name: "delete",
        usage: "/delete [confirm]",
        summary: "delete a closed session after confirmation",
        args: CommandArgs::OptionalConfirm,
    },
];

/// The spec for an implemented command name, if any.
pub fn command_spec(name: &str) -> Option<&'static CommandSpec> {
    COMMANDS.iter().find(|spec| spec.name == name)
}

pub fn slash_command_candidates(query: &str) -> Vec<String> {
    let query = query.to_ascii_lowercase();
    let mut matches = COMMANDS
        .iter()
        .filter_map(|spec| fuzzy_score(&query, spec.name).map(|score| (score, spec.name)))
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| left.0.total_cmp(&right.0));
    matches
        .into_iter()
        .map(|(_, name)| format!("/{name}"))
        .collect()
}

/// Pi's command list uses fuzzy subsequence matching rather than a strict
/// prefix filter. Keep the same scoring shape so an exact command wins while
/// preserving declaration order for equal scores.
fn fuzzy_score(query: &str, text: &str) -> Option<f64> {
    if query.is_empty() {
        return Some(0.0);
    }
    if query.len() > text.len() {
        return None;
    }
    let query = query.as_bytes();
    let text = text.as_bytes();
    let mut query_index = 0;
    let mut score = 0.0;
    let mut last_match = None;
    let mut consecutive = 0;
    for (index, character) in text.iter().enumerate() {
        if query_index == query.len() {
            break;
        }
        if *character != query[query_index] {
            continue;
        }
        let boundary =
            index == 0 || matches!(text[index - 1], b' ' | b'-' | b'_' | b'.' | b'/' | b':');
        if last_match == index.checked_sub(1) {
            consecutive += 1;
            score -= f64::from(consecutive * 5);
        } else {
            consecutive = 0;
            if let Some(last) = last_match {
                score += (index.saturating_sub(last + 1) * 2) as f64;
            }
        }
        if boundary {
            score -= 10.0;
        }
        score += index as f64 * 0.1;
        last_match = Some(index);
        query_index += 1;
    }
    if query_index != query.len() {
        return None;
    }
    if query == text {
        score -= 100.0;
    }
    Some(score)
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
/// (spec 23.1). The static table owns which names exist; this function only
/// validates their arguments. Unknown commands and unexpected arguments are
/// local issues; the caller shows a notice and never sends an RPC command.
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

    if name.is_empty() {
        return Err(CommandIssue::Unknown("/".to_owned()));
    }
    let spec = command_spec(&name).ok_or_else(|| CommandIssue::Unknown(name.clone()))?;
    let no_args = |cmd: LocalCommand| -> Result<LocalCommand, CommandIssue> {
        if args.is_empty() {
            Ok(cmd)
        } else {
            Err(CommandIssue::InvalidArgs(format!("usage: {}", spec.usage)))
        }
    };
    let confirm = |build: fn(bool) -> LocalCommand| -> Result<LocalCommand, CommandIssue> {
        match args {
            "" => Ok(build(false)),
            "confirm" | "--force" | "force" => Ok(build(true)),
            _ => Err(CommandIssue::InvalidArgs(format!("usage: {}", spec.usage))),
        }
    };

    match (spec.name, spec.args) {
        ("tool", _) => {
            let fields: Vec<_> = args.split_whitespace().collect();
            if fields.len() != 4 {
                return Err(CommandIssue::InvalidArgs(format!("usage: {}", spec.usage)));
            }
            let request_index = fields[2]
                .parse()
                .map_err(|_| CommandIssue::InvalidArgs(format!("usage: {}", spec.usage)))?;
            Ok(LocalCommand::Tool(crate::state::tool::ToolKey::new(
                fields[0],
                fields[1],
                request_index,
                fields[3],
            )))
        }
        ("search", _) => {
            let (scope, query) = match args.strip_prefix("full") {
                Some(rest) if rest.is_empty() || rest.starts_with(char::is_whitespace) => {
                    (crate::state::search::SearchScope::FullSession, rest.trim())
                }
                _ => (crate::state::search::SearchScope::Loaded, args),
            };
            Ok(LocalCommand::Search {
                query: query.to_owned(),
                scope,
            })
        }
        ("copy", _) => match args {
            "" | "last" | "reply" => Ok(LocalCommand::Copy {
                target: CopyTarget::LastReply,
            }),
            "message" | "msg" => Ok(LocalCommand::Copy {
                target: CopyTarget::Message,
            }),
            "code" => Ok(LocalCommand::Copy {
                target: CopyTarget::Code,
            }),
            "selection" => Ok(LocalCommand::Copy {
                target: CopyTarget::Selection,
            }),
            _ => Err(CommandIssue::InvalidArgs(format!("usage: {}", spec.usage))),
        },
        ("export", _) => {
            // `/export raw <path>` selects the explicit raw-JSON streaming
            // entry for oversized items; `/export <path>` keeps placeholders.
            let (raw_oversized, path) = match args.strip_prefix("raw") {
                Some(rest) if rest.is_empty() || rest.starts_with(char::is_whitespace) => {
                    (true, rest.trim())
                }
                _ => (false, args),
            };
            Ok(LocalCommand::Export {
                target: path.to_owned(),
                raw_oversized,
            })
        }
        ("prev", _) => no_args(LocalCommand::PromptJump(-1)),
        ("next", _) => no_args(LocalCommand::PromptJump(1)),
        ("latest", _) => no_args(LocalCommand::Latest),
        ("resume", _) => no_args(LocalCommand::Resume),
        ("sessions", _) => no_args(LocalCommand::Sessions),
        ("model", _) => no_args(LocalCommand::Model),
        ("reasoning", _) => no_args(LocalCommand::Reasoning),
        ("settings", _) => no_args(LocalCommand::Settings),
        ("editor", _) => no_args(LocalCommand::Editor),
        ("clear", _) => no_args(LocalCommand::Clear),
        ("refresh", _) => no_args(LocalCommand::Refresh),
        ("help", _) => no_args(LocalCommand::Help),
        ("logs", _) => no_args(LocalCommand::Logs),
        ("cancel", _) => no_args(LocalCommand::Cancel),
        ("context", _) => no_args(LocalCommand::Context),
        ("compact", _) => no_args(LocalCommand::Compact),
        ("reload", _) => no_args(LocalCommand::Reload),
        ("quit", _) => no_args(LocalCommand::Quit),
        ("theme", _) => match args {
            "dark" => Ok(LocalCommand::Theme(ThemeKind::Dark)),
            "light" => Ok(LocalCommand::Theme(ThemeKind::Light)),
            _ => Err(CommandIssue::InvalidArgs(format!("usage: {}", spec.usage))),
        },
        ("new", _) => match args {
            "" => Ok(LocalCommand::New),
            "form" | "--form" => Ok(LocalCommand::NewForm),
            _ => Err(CommandIssue::InvalidArgs(format!("usage: {}", spec.usage))),
        },
        ("rename", _) => Ok(LocalCommand::Rename {
            title: (!args.is_empty()).then(|| args.to_owned()),
        }),
        ("close", _) => confirm(|confirm| LocalCommand::Close { confirm }),
        ("delete", _) => confirm(|confirm| LocalCommand::Delete { confirm }),
        (other, _) => Err(CommandIssue::Unknown(other.to_owned())),
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

    /// D1d (spec §10.5): the help/completion table is the parse authority.
    /// Every listed command parses, every unlisted name is unknown, and the
    /// completion popup offers exactly the table's names.
    #[test]
    fn the_static_table_drives_parsing_and_completion() {
        for spec in COMMANDS {
            let parsed = parse_command(&format!("/{}", spec.name));
            if matches!(spec.args, CommandArgs::Theme | CommandArgs::ToolRef) {
                // A required argument: the table still owns the name, and the
                // bare form reports its usage instead of "unknown command".
                assert!(
                    matches!(parsed, Err(CommandIssue::InvalidArgs(_))),
                    "/{} must reach its usage: {parsed:?}",
                    spec.name
                );
                assert!(parse_command("/theme dark").is_ok());
                assert_eq!(
                    parse_command("/tool ses_1 lup_2 3 call_4"),
                    Ok(LocalCommand::Tool(crate::state::tool::ToolKey::new(
                        "ses_1", "lup_2", 3, "call_4"
                    )))
                );
            } else {
                assert!(
                    parsed.is_ok(),
                    "/{} must parse from its own table entry: {parsed:?}",
                    spec.name
                );
            }
        }
        let candidates = slash_command_candidates("");
        let mut expected = COMMANDS
            .iter()
            .map(|spec| format!("/{}", spec.name))
            .collect::<Vec<_>>();
        expected.sort();
        let mut candidates = candidates;
        candidates.sort();
        assert_eq!(candidates, expected);
        assert!(parse_command("/not-a-command").is_err());
        assert!(command_spec("not-a-command").is_none());
    }

    #[test]
    fn parses_every_implemented_command() {
        assert_eq!(parse_command("/new"), Ok(LocalCommand::New));
        assert_eq!(parse_command("/resume"), Ok(LocalCommand::Resume));
        assert_eq!(parse_command("/sessions"), Ok(LocalCommand::Sessions));
        assert_eq!(parse_command("/model"), Ok(LocalCommand::Model));
        assert_eq!(parse_command("/reasoning"), Ok(LocalCommand::Reasoning));
        assert_eq!(parse_command("/settings"), Ok(LocalCommand::Settings));
        assert_eq!(parse_command("/editor"), Ok(LocalCommand::Editor));
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
        assert_eq!(parse_command("/context"), Ok(LocalCommand::Context));
        assert_eq!(parse_command("/compact"), Ok(LocalCommand::Compact));
        assert_eq!(parse_command("/reload"), Ok(LocalCommand::Reload));
        assert_eq!(parse_command("/refresh"), Ok(LocalCommand::Refresh));
        assert_eq!(
            parse_command("/rename"),
            Ok(LocalCommand::Rename { title: None })
        );
        assert_eq!(
            parse_command("/rename fresh title"),
            Ok(LocalCommand::Rename {
                title: Some("fresh title".to_owned())
            })
        );
        assert_eq!(parse_command("/new form"), Ok(LocalCommand::NewForm));
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
    fn implemented_command_names_are_offered_and_reload_is_kept() {
        assert!(command_spec("reload").is_some());
        assert!(command_spec("context").is_some());
        assert!(command_spec("compact").is_some());
        // `/refresh` is implemented in D1, so completion offers it.
        assert_eq!(slash_command_candidates("ref"), vec!["/refresh"]);
        assert_eq!(slash_command_candidates("rel"), vec!["/reload"]);
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
