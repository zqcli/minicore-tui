//! The TUI's own preferences. Never Agent credentials: only local paths, the
//! theme and the editor command.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeKind {
    #[default]
    Dark,
    Light,
}

impl ThemeKind {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "dark" => Some(Self::Dark),
            "light" => Some(Self::Light),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub agent: Option<PathBuf>,
    pub agent_config: Option<PathBuf>,
    pub theme: ThemeKind,
    /// Editor executable plus arguments; the draft path is appended.
    pub editor: Vec<String>,
    /// Ring the terminal bell when a long turn ends or waits for approval.
    pub bell: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            agent: None,
            agent_config: None,
            theme: ThemeKind::Dark,
            editor: Vec::new(),
            bell: true,
        }
    }
}

/// The directory holding the config file, which also holds `prompts/`.
pub fn config_dir() -> Option<PathBuf> {
    config_path()?.parent().map(Path::to_path_buf)
}

fn config_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("MINICORE_TUI_CONFIG") {
        return Some(PathBuf::from(path));
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("minicore-tui").join("config.toml"))
}

/// The prompt history file, next to the config file.
pub fn history_path() -> Option<PathBuf> {
    Some(config_path()?.with_file_name("history.jsonl"))
}

impl Config {
    /// Loads the config file; a missing or malformed one gives the defaults.
    pub fn load() -> Self {
        Self::load_from(config_path().as_deref()).unwrap_or_default()
    }

    pub fn load_from(path: Option<&Path>) -> Option<Self> {
        let path = path?;
        let text = std::fs::read_to_string(path).ok()?;
        toml::from_str(&text).ok()
    }

    /// Writes the config back, creating the directory when needed.
    pub fn save(&self) -> Result<(), String> {
        let Some(path) = config_path() else {
            return Err("no config path is available".into());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("{}: {error}", parent.display()))?;
        }
        let text = toml::to_string_pretty(self).map_err(|error| error.to_string())?;
        std::fs::write(&path, text).map_err(|error| format!("{}: {error}", path.display()))
    }

    /// The editor command: config first, then `$VISUAL`/`$EDITOR`.
    pub fn editor(&self) -> Option<Vec<String>> {
        if !self.editor.is_empty() {
            return Some(self.editor.clone());
        }
        for name in ["VISUAL", "EDITOR"] {
            if let Some(value) = std::env::var_os(name) {
                let value = value.to_string_lossy().into_owned();
                let mut parts = value.split_whitespace().map(str::to_owned);
                if let Some(program) = parts.next() {
                    return Some(std::iter::once(program).chain(parts).collect());
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        let config: Config = toml::from_str("theme = \"light\"").unwrap();
        assert_eq!(config.theme, ThemeKind::Light);
        assert!(config.editor.is_empty());
        assert!(config.agent.is_none());
    }

    #[test]
    fn theme_names_parse_and_unknown_ones_do_not() {
        assert_eq!(ThemeKind::parse("dark"), Some(ThemeKind::Dark));
        assert_eq!(ThemeKind::parse("light"), Some(ThemeKind::Light));
        assert_eq!(ThemeKind::parse("sepia"), None);
    }

    #[test]
    fn unknown_keys_are_rejected_so_typos_are_visible() {
        assert!(toml::from_str::<Config>("them = \"light\"").is_err());
    }

    #[test]
    fn an_explicit_editor_wins_over_the_environment() {
        let config = Config {
            editor: vec!["ed".into(), "-s".into()],
            ..Config::default()
        };
        assert_eq!(config.editor(), Some(vec!["ed".into(), "-s".into()]));
    }
}
