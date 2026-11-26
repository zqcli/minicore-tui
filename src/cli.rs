//! Command-line inputs for the in-process Host and terminal frontend.

use std::path::PathBuf;

use minicore_host_protocol::ReasoningPreference;

use crate::config::ThemeKind;

pub const USAGE: &str = "\
minicore-tui — a terminal frontend for the embedded MiniCore Host

Usage: minicore-tui [OPTIONS] [WORKSPACE]

Options:
  --config-dir <DIR>     shared JSON configuration [default: ~/.minicore/config]
  --data-root <DIR>      persistent data [default: ~/.minicore/data]
  --agent-id <ID>        select this Host Agent
  --workspace <DIR>      workspace for a new session [default: cwd; same as WORKSPACE]
  --session <ID>         open this session
  --continue            open the most recent session in this workspace
  --model <REF>          full model override for the selected/new session
  --reasoning <LEVEL>    auto|disabled|low|medium|high|xhigh|max|ultra
  --theme <THEME>        dark|light
  -h, --help            print this help
  -V, --version         print the version
";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Args {
    pub config_dir: Option<PathBuf>,
    pub data_root: Option<PathBuf>,
    pub agent_id: Option<String>,
    pub workspace: Option<PathBuf>,
    pub session: Option<String>,
    pub continue_recent: bool,
    pub model: Option<String>,
    pub reasoning: Option<ReasoningPreference>,
    pub theme: Option<ThemeKind>,
    pub help: bool,
    pub version: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ArgsError {
    #[error("unknown option {0}")]
    Unknown(String),
    #[error("{0} requires a value")]
    MissingValue(String),
    #[error("invalid --reasoning value {0}")]
    Reasoning(String),
    #[error("invalid --theme value {0}")]
    Theme(String),
    #[error("--session and --continue cannot be combined")]
    Conflict,
    #[error("--workspace and positional workspace cannot be combined")]
    WorkspaceConflict,
    #[error("unexpected argument {0}")]
    Unexpected(String),
}

impl Args {
    pub fn parse<I, S>(arguments: I) -> Result<Self, ArgsError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut args = Self::default();
        let mut arguments = arguments.into_iter().map(Into::into);
        let mut workspace_flag = false;
        let mut positional_workspace = false;
        let mut positional_only = false;
        while let Some(argument) = arguments.next() {
            if positional_only {
                if workspace_flag {
                    return Err(ArgsError::WorkspaceConflict);
                }
                if positional_workspace {
                    return Err(ArgsError::Unexpected(argument));
                }
                args.workspace = Some(PathBuf::from(argument));
                positional_workspace = true;
                continue;
            }
            let mut value = |name: &str| {
                arguments
                    .next()
                    .ok_or_else(|| ArgsError::MissingValue(name.to_owned()))
            };
            match argument.as_str() {
                "--" => positional_only = true,
                "--config-dir" => args.config_dir = Some(PathBuf::from(value("--config-dir")?)),
                "--data-root" => args.data_root = Some(PathBuf::from(value("--data-root")?)),
                "--agent-id" => args.agent_id = Some(value("--agent-id")?),
                "--workspace" => {
                    if positional_workspace {
                        return Err(ArgsError::WorkspaceConflict);
                    }
                    args.workspace = Some(PathBuf::from(value("--workspace")?));
                    workspace_flag = true;
                }
                "--session" => args.session = Some(value("--session")?),
                "--continue" => args.continue_recent = true,
                "--model" => args.model = Some(value("--model")?),
                "--reasoning" => {
                    let level = value("--reasoning")?;
                    args.reasoning = Some(reasoning(&level).ok_or(ArgsError::Reasoning(level))?);
                }
                "--theme" => {
                    let theme = value("--theme")?;
                    args.theme = Some(ThemeKind::parse(&theme).ok_or(ArgsError::Theme(theme))?);
                }
                "-h" | "--help" => args.help = true,
                "-V" | "--version" => args.version = true,
                other if other.starts_with('-') => {
                    return Err(ArgsError::Unknown(other.to_owned()));
                }
                "agent" => return Err(ArgsError::Unexpected("agent".to_owned())),
                other => {
                    if workspace_flag {
                        return Err(ArgsError::WorkspaceConflict);
                    }
                    if positional_workspace {
                        return Err(ArgsError::Unexpected(other.to_owned()));
                    }
                    args.workspace = Some(PathBuf::from(other));
                    positional_workspace = true;
                }
            }
        }
        if args.session.is_some() && args.continue_recent {
            return Err(ArgsError::Conflict);
        }
        Ok(args)
    }
}

fn reasoning(value: &str) -> Option<ReasoningPreference> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_native_host_paths_and_session_selection() {
        let args = Args::parse([
            "--config-dir",
            "tui",
            "--data-root",
            "store",
            "--agent-id",
            "agent-id",
            "--workspace",
            "work",
            "--session",
            "session-id",
            "--model",
            "provider/main",
            "--reasoning",
            "xhigh",
            "--theme",
            "light",
        ])
        .unwrap();
        assert_eq!(args.data_root, Some(PathBuf::from("store")));
        assert_eq!(args.config_dir, Some(PathBuf::from("tui")));
        assert_eq!(args.agent_id.as_deref(), Some("agent-id"));
        assert_eq!(args.model.as_deref(), Some("provider/main"));
        assert_eq!(args.reasoning, Some(ReasoningPreference::XHigh));
        assert_eq!(args.theme, Some(ThemeKind::Light));
    }

    #[test]
    fn removed_agent_process_profile_and_web_options_are_rejected() {
        for flag in [
            "--agent",
            "--agent-config",
            "--profile",
            "--remote",
            "--home",
            "--host-config",
        ] {
            assert_eq!(
                Args::parse([flag]).unwrap_err(),
                ArgsError::Unknown(flag.into())
            );
        }
        assert_eq!(
            Args::parse(["agent"]).unwrap_err(),
            ArgsError::Unexpected("agent".into())
        );
    }

    #[test]
    fn rejects_conflicts_missing_values_and_invalid_preferences() {
        assert_eq!(
            Args::parse(["--session", "x", "--continue"]).unwrap_err(),
            ArgsError::Conflict
        );
        for flag in ["--config-dir", "--data-root", "--agent-id", "--model"] {
            assert_eq!(
                Args::parse([flag]).unwrap_err(),
                ArgsError::MissingValue(flag.into())
            );
        }
        assert!(matches!(
            Args::parse(["--reasoning", "turbo"]),
            Err(ArgsError::Reasoning(_))
        ));
        assert!(matches!(
            Args::parse(["--theme", "blue"]),
            Err(ArgsError::Theme(_))
        ));
    }

    #[test]
    fn accepts_positional_workspace_and_rejects_conflicts_or_extras() {
        let dot = Args::parse(["."]).unwrap();
        assert_eq!(dot.workspace, Some(PathBuf::from(".")));

        let with_flags = Args::parse(["--continue", "./project", "--theme", "dark"]).unwrap();
        assert_eq!(with_flags.workspace, Some(PathBuf::from("./project")));
        assert!(with_flags.continue_recent);
        assert_eq!(with_flags.theme, Some(ThemeKind::Dark));

        let escaped = Args::parse(["--", "-weird"]).unwrap();
        assert_eq!(escaped.workspace, Some(PathBuf::from("-weird")));

        let escaped_agent = Args::parse(["--", "agent"]).unwrap();
        assert_eq!(escaped_agent.workspace, Some(PathBuf::from("agent")));

        assert_eq!(
            Args::parse(["--workspace", "a", "."]).unwrap_err(),
            ArgsError::WorkspaceConflict
        );
        assert_eq!(
            Args::parse([".", "--workspace", "a"]).unwrap_err(),
            ArgsError::WorkspaceConflict
        );
        assert_eq!(
            Args::parse([".", "extra"]).unwrap_err(),
            ArgsError::Unexpected("extra".into())
        );
    }
}
