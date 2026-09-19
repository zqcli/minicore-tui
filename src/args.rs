//! Flat command-line parsing (development spec 6.1). Values are plain
//! strings or paths; the workspace is only recorded, never read. Agent
//! discovery is fixed to `--agent-bin` (default `minicore-agent` on PATH)
//! and `--agent-config`; no search across multiple locations happens.

use std::fmt;
use std::path::PathBuf;

use crate::protocol::Reasoning;
use crate::theme::ThemeKind;

pub const USAGE: &str = "\
minicore-tui — a Pi-style coding agent TUI for minicore-agent

Usage: minicore-tui [OPTIONS]

Options:
  --config <PATH>            TUI config file [default: platform config path]
  --agent-bin <PATH>         minicore-agent binary [default: minicore-agent]
  --agent-config <PATH>      agent config file (required unless config supplies it)
  --workspace <PATH>         workspace for a new session [default: cwd]
  --continue                 open the most recent session in this workspace
  --session <ID>             open this exact session id (no prompts)
  --profile <ID>             default profile for a new session
  --model <ID>               default model for a new session
  --reasoning <LEVEL>        default reasoning (auto|disabled|low|medium|high|xhigh|max|ultra)
  --theme <dark|light>       color theme [default: dark]
  --debug                    log RPC method/id/bytes/timing to a temp file
  --version                  print version
  --help                     print help
";

/// Parsed CLI arguments. The workspace is recorded as a string only; the
/// TUI never reads or validates it (spec 6.1) and prefers PATH-safe
/// `PathBuf`s (spec 14).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    pub config_path: Option<PathBuf>,
    pub agent_bin: PathBuf,
    pub agent_config: PathBuf,
    pub workspace: PathBuf,
    pub agent_bin_explicit: bool,
    pub agent_config_explicit: bool,
    pub theme_explicit: bool,
    /// Whether `--workspace` was passed explicitly; a Ready app opens a
    /// pre-filled new-session form only then (spec 6.1).
    pub workspace_explicit: bool,
    pub profile: Option<String>,
    pub model: Option<String>,
    pub reasoning: Option<Reasoning>,
    pub theme: ThemeKind,
    /// `--continue`: resume the most recent session of the current workspace.
    pub continue_recent: bool,
    /// `--session <ID>`: open this exact id, no selector.
    pub session: Option<String>,
    pub debug: bool,
    pub help: bool,
    pub version: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgsError {
    UnknownFlag(String),
    MissingValue(String),
    MissingRequired(&'static str),
    InvalidTheme(String),
    InvalidReasoning(String),
    ConflictingFlags(&'static str, &'static str),
}

impl Args {
    /// Applies the loaded TUI config only where the CLI did not provide an
    /// explicit value. The backend Agent config remains required after this
    /// merge, so startup cannot silently invent a path.
    pub fn apply_tui_config(&mut self, config: &crate::config::TuiConfig) -> Result<(), ArgsError> {
        if !self.agent_bin_explicit {
            if let Some(path) = &config.agent_executable {
                self.agent_bin = path.clone();
            }
        }
        if !self.agent_config_explicit {
            if let Some(path) = &config.agent_config {
                self.agent_config = path.clone();
            }
        }
        if !self.theme_explicit {
            self.theme = config.theme;
        }
        if self.agent_config.as_os_str().is_empty() {
            return Err(ArgsError::MissingRequired(
                "--agent-config or [agent].config",
            ));
        }
        Ok(())
    }
}

impl fmt::Display for ArgsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownFlag(flag) => write!(f, "unknown flag `{flag}`"),
            Self::MissingValue(flag) => write!(f, "flag `{flag}` requires a value"),
            Self::MissingRequired(flag) => write!(f, "flag `{flag}` is required"),
            Self::InvalidTheme(value) => {
                write!(f, "invalid theme `{value}` (expected `dark` or `light`)")
            }
            Self::InvalidReasoning(value) => write!(
                f,
                "invalid reasoning `{value}` (expected `auto`, `disabled`, `low`, `medium`, `high`, `xhigh`, `max`, or `ultra`)"
            ),
            Self::ConflictingFlags(a, b) => write!(f, "`{a}` and `{b}` cannot be combined"),
        }
    }
}

/// Parses CLI flags. Callers pass the arguments without `argv[0]`.
pub fn parse<I>(args: I) -> Result<Args, ArgsError>
where
    I: IntoIterator<Item = String>,
{
    let mut parsed = Args {
        config_path: None,
        agent_bin: PathBuf::from("minicore-agent"),
        agent_config: PathBuf::new(),
        workspace: PathBuf::new(),
        agent_bin_explicit: false,
        agent_config_explicit: false,
        theme_explicit: false,
        workspace_explicit: false,
        profile: None,
        model: None,
        reasoning: None,
        theme: ThemeKind::Dark,
        continue_recent: false,
        session: None,
        debug: false,
        help: false,
        version: false,
    };
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        // Split `--flag=value`; value-taking flags read the next argument
        // when the inline form is absent.
        let (name, inline) = match arg.split_once('=') {
            Some((name, value)) => (name.to_owned(), Some(value.to_owned())),
            None => (arg.clone(), None),
        };
        let mut value = || -> Result<String, ArgsError> {
            match inline {
                Some(ref inline) => Ok(inline.clone()),
                None => {
                    let next = args
                        .next()
                        .ok_or_else(|| ArgsError::MissingValue(name.clone()))?;
                    // A following flag is not a value: `--workspace --debug`
                    // is a missing value, not a path named `--debug`.
                    if next.starts_with('-') {
                        return Err(ArgsError::MissingValue(name.clone()));
                    }
                    Ok(next)
                }
            }
        };
        match name.as_str() {
            "--config" => parsed.config_path = Some(PathBuf::from(value()?)),
            "--agent-bin" => {
                parsed.agent_bin = PathBuf::from(value()?);
                parsed.agent_bin_explicit = true;
            }
            "--agent-config" => {
                parsed.agent_config = PathBuf::from(value()?);
                parsed.agent_config_explicit = true;
            }
            "--workspace" => {
                parsed.workspace = PathBuf::from(value()?);
                parsed.workspace_explicit = true;
            }
            "--profile" => parsed.profile = Some(value()?),
            "--model" => parsed.model = Some(value()?),
            "--reasoning" => parsed.reasoning = Some(parse_reasoning(&value()?)?),
            "--theme" => {
                parsed.theme = parse_theme(&value()?)?;
                parsed.theme_explicit = true;
            }
            "--continue" => parsed.continue_recent = true,
            "--session" => parsed.session = Some(value()?),
            "--debug" => parsed.debug = true,
            "--help" | "-h" => parsed.help = true,
            "--version" | "-V" => parsed.version = true,
            other => return Err(ArgsError::UnknownFlag(other.to_owned())),
        }
    }
    // `--help`/`--version` are pure meta actions and work without a config;
    // every other invocation is run mode and needs `--agent-config`. Every
    // argument is parsed first, so an unknown flag still errors even when
    // `--help` is present.
    if parsed.continue_recent && parsed.session.is_some() {
        return Err(ArgsError::ConflictingFlags("--continue", "--session"));
    }
    if !parsed.help
        && !parsed.version
        && parsed.config_path.is_none()
        && parsed.agent_config.as_os_str().is_empty()
    {
        return Err(ArgsError::MissingRequired("--agent-config or --config"));
    }
    Ok(parsed)
}

fn parse_theme(value: &str) -> Result<ThemeKind, ArgsError> {
    match value {
        "dark" => Ok(ThemeKind::Dark),
        "light" => Ok(ThemeKind::Light),
        other => Err(ArgsError::InvalidTheme(other.to_owned())),
    }
}

fn parse_reasoning(value: &str) -> Result<Reasoning, ArgsError> {
    match value {
        "auto" => Ok(Reasoning::Auto),
        "disabled" => Ok(Reasoning::Disabled),
        "low" => Ok(Reasoning::Low),
        "medium" => Ok(Reasoning::Medium),
        "high" => Ok(Reasoning::High),
        "xhigh" => Ok(Reasoning::XHigh),
        "max" => Ok(Reasoning::Max),
        "ultra" => Ok(Reasoning::Ultra),
        other => Err(ArgsError::InvalidReasoning(other.to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_flags(args: &[&str]) -> Result<Args, ArgsError> {
        parse(args.iter().map(|flag| flag.to_string()))
    }

    #[test]
    fn defaults_use_path_agent_and_dark_theme() {
        let parsed = parse_flags(&["--agent-config", "agent.toml"]).unwrap();
        assert_eq!(parsed.agent_bin, PathBuf::from("minicore-agent"));
        assert_eq!(parsed.theme, ThemeKind::Dark);
        assert!(!parsed.workspace_explicit);
        assert_eq!(parsed.profile, None);
        assert_eq!(parsed.reasoning, None);
        assert!(!parsed.debug);
        assert!(!parsed.help && !parsed.version);
    }

    #[test]
    fn agent_config_is_required_without_a_tui_config() {
        assert_eq!(
            parse_flags(&[]),
            Err(ArgsError::MissingRequired("--agent-config or --config"))
        );
        // Run-mode flags still require a backend config source.
        assert_eq!(
            parse_flags(&["--theme", "light"]),
            Err(ArgsError::MissingRequired("--agent-config or --config"))
        );
        assert!(parse_flags(&["--config", "tui.toml"]).is_ok());
    }

    #[test]
    fn help_and_version_work_without_an_agent_config() {
        for flag in ["--help", "-h"] {
            let parsed = parse_flags(&[flag]).unwrap();
            assert!(parsed.help, "{flag} sets help");
            assert!(!parsed.version);
        }
        for flag in ["--version", "-V"] {
            let parsed = parse_flags(&[flag]).unwrap();
            assert!(parsed.version, "{flag} sets version");
            assert!(!parsed.help);
        }
    }

    #[test]
    fn unknown_flags_error_even_when_help_or_version_is_present() {
        // Argument order never short-circuits: `--bad` is still reported.
        assert_eq!(
            parse_flags(&["--help", "--bad"]),
            Err(ArgsError::UnknownFlag("--bad".to_owned()))
        );
        assert_eq!(
            parse_flags(&["--bad", "--help"]),
            Err(ArgsError::UnknownFlag("--bad".to_owned()))
        );
        assert_eq!(
            parse_flags(&["--version", "--bad"]),
            Err(ArgsError::UnknownFlag("--bad".to_owned()))
        );
    }

    #[test]
    fn help_with_a_config_still_prints_help() {
        let parsed =
            parse_flags(&["--help", "--agent-config", "a.toml", "--theme", "light"]).unwrap();
        assert!(parsed.help);
        assert_eq!(parsed.theme, ThemeKind::Light);
    }

    #[test]
    fn parses_all_flags_in_both_forms() {
        let parsed = parse_flags(&[
            "--agent-bin",
            "/opt/minicore-agent",
            "--agent-config=cfg/agent.toml",
            "--workspace",
            "/srv/work",
            "--profile",
            "coding",
            "--model",
            "gpt-4o",
            "--reasoning",
            "high",
            "--theme",
            "light",
            "--debug",
        ])
        .unwrap();
        assert_eq!(parsed.agent_bin, PathBuf::from("/opt/minicore-agent"));
        assert_eq!(parsed.agent_config, PathBuf::from("cfg/agent.toml"));
        assert_eq!(parsed.workspace, PathBuf::from("/srv/work"));
        assert!(parsed.workspace_explicit);
        assert_eq!(parsed.profile.as_deref(), Some("coding"));
        assert_eq!(parsed.model.as_deref(), Some("gpt-4o"));
        assert_eq!(parsed.reasoning, Some(Reasoning::High));
        assert_eq!(parsed.theme, ThemeKind::Light);
        assert!(parsed.debug);
    }

    #[test]
    fn workspace_defaults_to_cwd_without_flag() {
        let parsed = parse_flags(&["--agent-config", "agent.toml"]).unwrap();
        assert!(parsed.workspace.as_os_str().is_empty());
        assert!(!parsed.workspace_explicit);
    }

    #[test]
    fn parses_help_and_version() {
        assert!(
            parse_flags(&["--help", "--agent-config", "a.toml"])
                .unwrap()
                .help
        );
        assert!(
            parse_flags(&["-h", "--agent-config", "a.toml"])
                .unwrap()
                .help
        );
        assert!(
            parse_flags(&["--version", "--agent-config", "a.toml"])
                .unwrap()
                .version
        );
        assert!(
            parse_flags(&["-V", "--agent-config", "a.toml"])
                .unwrap()
                .version
        );
    }

    #[test]
    fn parses_continue_and_session_startup_selection() {
        let parsed = parse_flags(&["--agent-config", "a.toml", "--continue"]).unwrap();
        assert!(parsed.continue_recent);
        assert_eq!(parsed.session, None);
        let parsed = parse_flags(&["--agent-config=a.toml", "--session=ses_9"]).unwrap();
        assert_eq!(parsed.session.as_deref(), Some("ses_9"));
        assert!(!parsed.continue_recent);
        assert_eq!(
            parse_flags(&["--agent-config", "a.toml", "--continue", "--session", "s"]),
            Err(ArgsError::ConflictingFlags("--continue", "--session"))
        );
        assert_eq!(
            parse_flags(&["--agent-config", "a.toml", "--session"]),
            Err(ArgsError::MissingValue("--session".to_owned()))
        );
    }

    #[test]
    fn rejects_unknown_flags() {
        assert_eq!(
            parse_flags(&["--bogus", "--agent-config", "a.toml"]),
            Err(ArgsError::UnknownFlag("--bogus".to_owned()))
        );
    }

    #[test]
    fn rejects_missing_values() {
        assert_eq!(
            parse_flags(&["--agent-bin"]),
            Err(ArgsError::MissingValue("--agent-bin".to_owned()))
        );
        assert_eq!(
            parse_flags(&["--workspace", "--agent-config", "a.toml"]),
            Err(ArgsError::MissingValue("--workspace".to_owned()))
        );
    }

    #[test]
    fn rejects_invalid_theme_and_reasoning() {
        assert_eq!(
            parse_flags(&["--theme", "blue", "--agent-config", "a.toml"]),
            Err(ArgsError::InvalidTheme("blue".to_owned()))
        );
        assert_eq!(
            parse_flags(&["--reasoning", "turbo", "--agent-config", "a.toml"]),
            Err(ArgsError::InvalidReasoning("turbo".to_owned()))
        );
    }

    #[test]
    fn reasoning_cli_accepts_all_agent_levels_and_rejects_unknown() {
        for (value, level) in [
            ("auto", Reasoning::Auto),
            ("disabled", Reasoning::Disabled),
            ("low", Reasoning::Low),
            ("medium", Reasoning::Medium),
            ("high", Reasoning::High),
            ("xhigh", Reasoning::XHigh),
            ("max", Reasoning::Max),
            ("ultra", Reasoning::Ultra),
        ] {
            let parsed = parse_flags(&["--reasoning", value, "--agent-config", "a.toml"]).unwrap();
            assert_eq!(parsed.reasoning, Some(level), "--reasoning {value}");
        }
        assert!(matches!(
            parse_flags(&["--reasoning", "turbo", "--agent-config", "a.toml"]),
            Err(ArgsError::InvalidReasoning(_))
        ));
    }

    #[test]
    fn cli_values_override_tui_config_defaults() {
        let mut parsed = parse_flags(&[
            "--agent-bin",
            "/cli/agent",
            "--agent-config",
            "/cli/agent.toml",
            "--theme",
            "light",
        ])
        .unwrap();
        let config = crate::config::TuiConfig {
            theme: ThemeKind::Dark,
            agent_executable: Some(PathBuf::from("/config/agent")),
            agent_config: Some(PathBuf::from("/config/agent.toml")),
            ..crate::config::TuiConfig::default()
        };
        parsed.apply_tui_config(&config).unwrap();
        assert_eq!(parsed.agent_bin, PathBuf::from("/cli/agent"));
        assert_eq!(parsed.agent_config, PathBuf::from("/cli/agent.toml"));
        assert_eq!(parsed.theme, ThemeKind::Light);
    }
}
