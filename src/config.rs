//! Persistent TUI configuration.
//!
//! This file deliberately contains only local UI/launch preferences. It never
//! copies provider credentials, endpoints, model catalogs, or Agent data. The
//! editor is one explicit executable plus an argument vector; no shell parsing
//! or editor fallback chain is performed.

use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::theme::ThemeKind;

pub const CONFIG_FILE_NAME: &str = "config.toml";
pub const MAX_CONFIG_BYTES: usize = 256 * 1024;

/// One explicit external editor command. `args` are passed directly to
/// `std::process::Command`; the temporary draft path is appended by the job.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EditorConfig {
    pub executable: String,
    pub args: Vec<String>,
}

impl EditorConfig {
    pub fn validate(&self) -> Result<(), ConfigValueError> {
        if self.executable.trim().is_empty() {
            return Err(ConfigValueError::EmptyEditorExecutable);
        }
        if self.executable.contains('\0') || self.args.iter().any(|arg| arg.contains('\0')) {
            return Err(ConfigValueError::NulInEditorCommand);
        }
        Ok(())
    }
}

/// Preferences stored by the TUI. Agent paths are optional here because the
/// CLI can still supply them explicitly; the final launch resolver requires a
/// config path before spawning Agent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TuiConfig {
    pub theme: ThemeKind,
    pub thinking_visible: bool,
    pub tools_expanded: bool,
    pub editor: Option<EditorConfig>,
    pub agent_executable: Option<PathBuf>,
    pub agent_config: Option<PathBuf>,
}

impl Default for TuiConfig {
    fn default() -> Self {
        Self {
            theme: ThemeKind::Dark,
            thinking_visible: true,
            tools_expanded: false,
            editor: None,
            agent_executable: None,
            agent_config: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigValueError {
    EmptyEditorExecutable,
    NulInEditorCommand,
    EditorEnvironmentHasArguments,
    EditorEnvironmentNotUtf8,
}

impl fmt::Display for ConfigValueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyEditorExecutable => {
                write!(formatter, "editor executable must not be empty")
            }
            Self::NulInEditorCommand => {
                write!(formatter, "editor executable and args must not contain NUL")
            }
            Self::EditorEnvironmentHasArguments => write!(
                formatter,
                "MINICORE_TUI_EDITOR must contain one executable only; configure arguments in the TUI config"
            ),
            Self::EditorEnvironmentNotUtf8 => {
                write!(formatter, "MINICORE_TUI_EDITOR must be valid UTF-8")
            }
        }
    }
}

#[derive(Debug)]
pub enum ConfigError {
    Missing(PathBuf),
    Read(PathBuf),
    TooLarge(PathBuf),
    Invalid(PathBuf),
    Value(PathBuf, ConfigValueError),
    Write(PathBuf),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(path) => write!(
                formatter,
                "TUI config file does not exist: {}",
                path.display()
            ),
            Self::Read(path) => write!(
                formatter,
                "TUI config file cannot be read: {}",
                path.display()
            ),
            Self::TooLarge(path) => write!(
                formatter,
                "TUI config file is too large: {}",
                path.display()
            ),
            Self::Invalid(path) => write!(
                formatter,
                "invalid TUI config TOML/schema: {}",
                path.display()
            ),
            Self::Value(path, error) => write!(
                formatter,
                "invalid TUI config value at {}: {error}",
                path.display()
            ),
            Self::Write(path) => write!(formatter, "cannot persist TUI config: {}", path.display()),
        }
    }
}

impl std::error::Error for ConfigError {}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    #[serde(default)]
    ui: UiFileConfig,
    editor: Option<EditorFileConfig>,
    agent: Option<AgentFileConfig>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct UiFileConfig {
    theme: Option<String>,
    thinking_visible: Option<bool>,
    tools_expanded: Option<bool>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EditorFileConfig {
    executable: String,
    #[serde(default)]
    args: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentFileConfig {
    executable: Option<PathBuf>,
    config: Option<PathBuf>,
}

#[derive(Serialize)]
struct PersistedConfig<'a> {
    ui: PersistedUi<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    editor: Option<PersistedEditor<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<PersistedAgent<'a>>,
}

#[derive(Serialize)]
struct PersistedUi<'a> {
    theme: &'a str,
    thinking_visible: bool,
    tools_expanded: bool,
}

#[derive(Serialize)]
struct PersistedEditor<'a> {
    executable: &'a str,
    args: &'a [String],
}

#[derive(Serialize)]
struct PersistedAgent<'a> {
    executable: Option<&'a Path>,
    config: Option<&'a Path>,
}

pub fn default_path() -> PathBuf {
    default_path_from(
        std::env::var_os("XDG_CONFIG_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

fn default_path_from(xdg: Option<&std::ffi::OsStr>, home: Option<&std::ffi::OsStr>) -> PathBuf {
    if let Some(xdg) = xdg.filter(|value| !value.is_empty()) {
        return PathBuf::from(xdg)
            .join("minicore-tui")
            .join(CONFIG_FILE_NAME);
    }
    if let Some(home) = home.filter(|value| !value.is_empty()) {
        return PathBuf::from(home)
            .join(".config")
            .join("minicore-tui")
            .join(CONFIG_FILE_NAME);
    }
    PathBuf::from(CONFIG_FILE_NAME)
}

/// Loads an explicit config path, or treats a missing default config as the
/// built-in defaults. No fallback path is searched after an explicit error.
pub fn load(path: &Path, explicit: bool) -> Result<TuiConfig, ConfigError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound && !explicit => {
            return Ok(TuiConfig::default());
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(ConfigError::Missing(path.to_owned()));
        }
        Err(_) => return Err(ConfigError::Read(path.to_owned())),
    };
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err(ConfigError::TooLarge(path.to_owned()));
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| ConfigError::Invalid(path.to_owned()))?;
    let file: FileConfig =
        toml::from_str(text).map_err(|_| ConfigError::Invalid(path.to_owned()))?;
    from_file(path, file)
}

fn from_file(path: &Path, file: FileConfig) -> Result<TuiConfig, ConfigError> {
    let mut config = TuiConfig::default();
    if let Some(theme) = file.ui.theme {
        config.theme = match theme.as_str() {
            "dark" => ThemeKind::Dark,
            "light" => ThemeKind::Light,
            _ => return Err(ConfigError::Invalid(path.to_owned())),
        };
    }
    if let Some(value) = file.ui.thinking_visible {
        config.thinking_visible = value;
    }
    if let Some(value) = file.ui.tools_expanded {
        config.tools_expanded = value;
    }
    if let Some(editor) = file.editor {
        let editor = EditorConfig {
            executable: editor.executable,
            args: editor.args,
        };
        editor
            .validate()
            .map_err(|error| ConfigError::Value(path.to_owned(), error))?;
        config.editor = Some(editor);
    }
    if let Some(agent) = file.agent {
        if agent
            .executable
            .as_ref()
            .is_some_and(|path| invalid_path_value(path))
            || agent
                .config
                .as_ref()
                .is_some_and(|path| invalid_path_value(path))
        {
            return Err(ConfigError::Invalid(path.to_owned()));
        }
        config.agent_executable = agent.executable;
        config.agent_config = agent.config;
    }
    Ok(config)
}

/// Resolve the one permitted environment source. Values containing arguments
/// are rejected instead of being split or passed through a shell.
pub fn editor_from_environment() -> Result<Option<EditorConfig>, ConfigValueError> {
    let value = match std::env::var("MINICORE_TUI_EDITOR") {
        Ok(value) => value,
        Err(std::env::VarError::NotPresent) => return Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err(ConfigValueError::EditorEnvironmentNotUtf8);
        }
    };
    editor_from_environment_value(&value)
}

fn editor_from_environment_value(value: &str) -> Result<Option<EditorConfig>, ConfigValueError> {
    if value.trim().is_empty() {
        return Ok(None);
    }
    if value.chars().any(char::is_whitespace) {
        return Err(ConfigValueError::EditorEnvironmentHasArguments);
    }
    let editor = EditorConfig {
        executable: value.to_owned(),
        args: Vec::new(),
    };
    editor.validate()?;
    Ok(Some(editor))
}

fn invalid_path_value(path: &Path) -> bool {
    path.as_os_str().is_empty() || path.to_string_lossy().contains('\0')
}

pub fn persist(path: &Path, config: &TuiConfig) -> Result<(), ConfigError> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|_| ConfigError::Write(path.to_owned()))?;
    }
    let theme = match config.theme {
        ThemeKind::Dark => "dark",
        ThemeKind::Light => "light",
    };
    let persisted = PersistedConfig {
        ui: PersistedUi {
            theme,
            thinking_visible: config.thinking_visible,
            tools_expanded: config.tools_expanded,
        },
        editor: config.editor.as_ref().map(|editor| PersistedEditor {
            executable: &editor.executable,
            args: &editor.args,
        }),
        agent: (config.agent_executable.is_some() || config.agent_config.is_some()).then_some(
            PersistedAgent {
                executable: config.agent_executable.as_deref(),
                config: config.agent_config.as_deref(),
            },
        ),
    };
    let text =
        toml::to_string_pretty(&persisted).map_err(|_| ConfigError::Write(path.to_owned()))?;
    // Never replace a readable configuration with one that the next startup
    // must reject. Measure the serialized bytes, including TOML escaping.
    if text.len() > MAX_CONFIG_BYTES {
        return Err(ConfigError::TooLarge(path.to_owned()));
    }
    let mut temp = tempfile::Builder::new()
        .prefix(".minicore-tui-config-")
        .tempfile_in(path.parent().unwrap_or_else(|| Path::new(".")))
        .map_err(|_| ConfigError::Write(path.to_owned()))?;
    temp.write_all(text.as_bytes())
        .and_then(|()| temp.as_file_mut().sync_all())
        .map_err(|_| ConfigError::Write(path.to_owned()))?;
    temp.persist(path)
        .map_err(|_| ConfigError::Write(path.to_owned()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_persist_keeps_existing_file_and_reduced_retry_is_readable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_FILE_NAME);
        let mut config = TuiConfig::default();
        persist(&path, &config).unwrap();
        let previous = fs::read(&path).unwrap();
        config.editor = Some(EditorConfig {
            executable: "/synthetic/editor".to_owned(),
            args: vec!["X".repeat(MAX_CONFIG_BYTES)],
        });
        assert!(matches!(
            persist(&path, &config),
            Err(ConfigError::TooLarge(_))
        ));
        assert_eq!(fs::read(&path).unwrap(), previous);
        assert_eq!(load(&path, true).unwrap(), TuiConfig::default());
        config.editor.as_mut().unwrap().args = vec!["--reduced".to_owned()];
        persist(&path, &config).unwrap();
        assert_eq!(load(&path, true).unwrap(), config);
    }

    #[test]
    fn default_path_prefers_xdg_then_home_without_searching_fallbacks() {
        assert_eq!(
            default_path_from(
                Some(std::ffi::OsStr::new("/tmp/xdg")),
                Some(std::ffi::OsStr::new("/home/u"))
            ),
            PathBuf::from("/tmp/xdg/minicore-tui/config.toml")
        );
        assert_eq!(
            default_path_from(None, Some(std::ffi::OsStr::new("/home/u"))),
            PathBuf::from("/home/u/.config/minicore-tui/config.toml")
        );
    }

    #[test]
    fn editor_environment_rejects_shell_like_arguments() {
        assert!(editor_from_environment_value("/bin/true --wait").is_err());
        assert!(
            editor_from_environment_value("/bin/true")
                .unwrap()
                .is_some()
        );
        assert_eq!(
            ConfigValueError::EditorEnvironmentHasArguments.to_string(),
            "MINICORE_TUI_EDITOR must contain one executable only; configure arguments in the TUI config"
        );
    }

    #[test]
    fn roundtrip_does_not_contain_provider_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_FILE_NAME);
        let config = TuiConfig {
            theme: ThemeKind::Light,
            thinking_visible: false,
            tools_expanded: true,
            editor: Some(EditorConfig {
                executable: "/bin/true".to_owned(),
                args: vec!["--wait".to_owned()],
            }),
            agent_executable: Some(PathBuf::from("/opt/minicore-agent")),
            agent_config: Some(PathBuf::from("/etc/minicore-agent.toml")),
        };
        persist(&path, &config).unwrap();
        let loaded = load(&path, true).unwrap();
        assert_eq!(loaded, config);
        let text = fs::read_to_string(path).unwrap();
        assert!(!text.contains("provider"));
        assert!(!text.contains("endpoint"));
        assert!(!text.contains("api_key"));
    }

    #[test]
    fn invalid_config_does_not_overwrite_the_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_FILE_NAME);
        fs::write(&path, "[ui]\ntheme = 'not-a-theme'\n").unwrap();
        assert!(matches!(load(&path, true), Err(ConfigError::Invalid(_))));
        assert_eq!(
            fs::read_to_string(path).unwrap(),
            "[ui]\ntheme = 'not-a-theme'\n"
        );
    }

    #[test]
    fn duplicate_keys_are_invalid_and_never_reset_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CONFIG_FILE_NAME);
        let text = "[ui]\ntheme = 'dark'\ntheme = 'light'\n";
        fs::write(&path, text).unwrap();
        assert!(matches!(load(&path, true), Err(ConfigError::Invalid(_))));
        assert_eq!(fs::read_to_string(path).unwrap(), text);
    }
}
