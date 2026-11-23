//! Terminal preferences; execution configuration belongs to the native Host types.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::write_atomic;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeKind {
    #[default]
    Dark,
    Light,
}

impl ThemeKind {
    /// The terminal's own background as `COLORFGBG` reports it (`fg;bg`,
    /// ANSI indexes); dark when it says nothing. Text must be readable on the
    /// background the terminal actually draws.
    pub fn detect() -> Self {
        Self::from_colorfgbg(std::env::var("COLORFGBG").ok().as_deref())
    }

    fn from_colorfgbg(value: Option<&str>) -> Self {
        match value
            .and_then(|value| value.rsplit(';').next())
            .and_then(|bg| bg.parse::<u8>().ok())
        {
            Some(7 | 9..=15) => Self::Light,
            _ => Self::Dark,
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "dark" => Some(Self::Dark),
            "light" => Some(Self::Light),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Ui {
    /// Unset follows the terminal's background; see [`ThemeKind::detect`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<ThemeKind>,
    /// Editor executable plus arguments; the draft path is appended.
    pub editor: Vec<String>,
    pub bell: bool,
}

impl Default for Ui {
    fn default() -> Self {
        Self {
            theme: None,
            editor: Vec::new(),
            bell: true,
        }
    }
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub ui: Ui,
}

pub fn editor_command(configured: &[String]) -> Option<Vec<String>> {
    if !configured.is_empty() {
        return Some(configured.to_vec());
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

/// Saves valid JSON while preserving the other terminal preferences.
pub fn save_theme(file: &Path, kind: ThemeKind) -> Result<(), String> {
    let mut settings = super::load_settings(file).map_err(|error| error.to_string())?;
    settings.ui.theme = Some(kind);
    let text = serde_json::to_string_pretty(&settings)
        .map_err(|error| format!("{}: {error}", file.display()))?;
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", file.display()))?;
    }
    write_atomic(file, &format!("{text}\n"), 0o600)
        .map_err(|error| format!("{}: {error}", file.display()))
}

#[cfg(test)]
mod theme_detection {
    use super::ThemeKind;

    #[test]
    fn colorfgbg_background_picks_the_readable_palette() {
        assert_eq!(ThemeKind::from_colorfgbg(Some("15;0")), ThemeKind::Dark);
        assert_eq!(ThemeKind::from_colorfgbg(Some("0;15")), ThemeKind::Light);
        assert_eq!(
            ThemeKind::from_colorfgbg(Some("0;default;7")),
            ThemeKind::Light
        );
        assert_eq!(ThemeKind::from_colorfgbg(None), ThemeKind::Dark);
        assert_eq!(ThemeKind::from_colorfgbg(Some("junk")), ThemeKind::Dark);
    }
}
