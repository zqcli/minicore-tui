//! Small hand-rolled argument parser: no clap, no subcommands, and every
//! option has exactly one meaning.

use std::path::PathBuf;

use crate::config::ThemeKind;
use crate::protocol::Reasoning;

pub const USAGE: &str = "\
minicore-tui — a pi-style coding agent TUI for minicore-agent

Usage: minicore-tui [OPTIONS]
       minicore-tui agent --config <FILE> --stdio   (the built-in Agent the TUI starts)

Options:
  --home <DIR>           TUI directory: config.toml, models.toml, prompt/, session/
                         [default: $MINICORE_HOME, else the executable's directory]
  --agent <BIN>          run this external minicore-agent instead of the built-in
                         one [default: [agent] binary in config.toml, if set]
  --agent-config <FILE>  use this Agent config as is; skips config.toml, models.toml
                         and the first-run files (for tests and advanced use)
  --workspace <DIR>      workspace for a new session [default: cwd]
  --session <ID>         open this session
  --continue             open the most recent session in this workspace
  --profile <ID>         profile for a new session
  --model <ID>           model for a new session
  --reasoning <LEVEL>    auto|disabled|low|medium|high|xhigh|max|ultra
  --remote              start remote control before opening the terminal
  --theme <THEME>        dark|light
  -h, --help             print this help
  -V, --version          print the version
";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Args {
    pub home: Option<PathBuf>,
    pub agent: Option<PathBuf>,
    pub agent_config: Option<PathBuf>,
    pub workspace: Option<PathBuf>,
    pub session: Option<String>,
    pub continue_recent: bool,
    pub remote: bool,
    pub profile: Option<String>,
    pub model: Option<String>,
    pub reasoning: Option<Reasoning>,
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
    #[error(
        "--config was replaced: --home names the TUI directory, --agent-config a raw Agent file"
    )]
    ConfigRenamed,
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
        while let Some(argument) = arguments.next() {
            let mut value = |name: &str| {
                arguments
                    .next()
                    .ok_or_else(|| ArgsError::MissingValue(name.to_owned()))
            };
            match argument.as_str() {
                "--home" => args.home = Some(PathBuf::from(value("--home")?)),
                "--agent" => args.agent = Some(PathBuf::from(value("--agent")?)),
                "--agent-config" => {
                    args.agent_config = Some(PathBuf::from(value("--agent-config")?));
                }
                "--config" => return Err(ArgsError::ConfigRenamed),
                "--workspace" => args.workspace = Some(PathBuf::from(value("--workspace")?)),
                "--session" => args.session = Some(value("--session")?),
                "--continue" => args.continue_recent = true,
                "--remote" => args.remote = true,
                "--profile" => args.profile = Some(value("--profile")?),
                "--model" => args.model = Some(value("--model")?),
                "--reasoning" => {
                    let level = value("--reasoning")?;
                    args.reasoning =
                        Some(Reasoning::parse(&level).ok_or(ArgsError::Reasoning(level))?);
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
                other => return Err(ArgsError::Unexpected(other.to_owned())),
            }
        }
        if args.session.is_some() && args.continue_recent {
            return Err(ArgsError::Conflict);
        }
        Ok(args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_documented_option() {
        let args = Args::parse([
            "--home",
            "/h",
            "--agent",
            "/bin/agent",
            "--agent-config",
            "/etc/agent.toml",
            "--workspace",
            "/w",
            "--session",
            "ses_1",
            "--profile",
            "coding",
            "--model",
            "deep",
            "--reasoning",
            "xhigh",
            "--theme",
            "light",
            "--remote",
        ])
        .unwrap();
        assert_eq!(
            args.agent.as_deref(),
            Some(std::path::Path::new("/bin/agent"))
        );
        assert_eq!(args.home.as_deref(), Some(std::path::Path::new("/h")));
        assert_eq!(
            args.agent_config.as_deref(),
            Some(std::path::Path::new("/etc/agent.toml"))
        );
        assert_eq!(args.session.as_deref(), Some("ses_1"));
        assert_eq!(args.reasoning, Some(Reasoning::XHigh));
        assert_eq!(args.theme, Some(ThemeKind::Light));
        assert!(args.remote);
    }

    #[test]
    fn rejects_conflicts_missing_values_and_unknown_flags() {
        assert_eq!(
            Args::parse(["--session", "x", "--continue"]).unwrap_err(),
            ArgsError::Conflict
        );
        assert!(matches!(
            Args::parse(["--model"]).unwrap_err(),
            ArgsError::MissingValue(_)
        ));
        assert!(matches!(
            Args::parse(["--nope"]).unwrap_err(),
            ArgsError::Unknown(_)
        ));
        assert!(matches!(
            Args::parse(["--reasoning", "turbo"]).unwrap_err(),
            ArgsError::Reasoning(_)
        ));
    }

    #[test]
    fn the_old_config_flag_says_what_replaced_it() {
        assert_eq!(
            Args::parse(["--config", "/a.toml"]).unwrap_err(),
            ArgsError::ConfigRenamed
        );
    }

    #[test]
    fn the_new_path_options_need_a_value() {
        for flag in ["--home", "--agent-config"] {
            assert_eq!(
                Args::parse([flag]).unwrap_err(),
                ArgsError::MissingValue(flag.to_owned())
            );
        }
    }
}
