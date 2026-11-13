//! Terminal preferences; execution configuration belongs to the native Host types.

use std::path::Path;

use serde::Deserialize;

use super::write_atomic;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
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

    fn label(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Ui {
    pub theme: ThemeKind,
    /// Editor executable plus arguments; the draft path is appended.
    pub editor: Vec<String>,
    pub bell: bool,
}

impl Default for Ui {
    fn default() -> Self {
        Self {
            theme: ThemeKind::Dark,
            editor: Vec::new(),
            bell: true,
        }
    }
}

#[derive(Default, Deserialize)]
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

/// Saves the theme without replacing comments or other terminal preferences.
pub fn save_theme(file: &Path, kind: ThemeKind) -> Result<(), String> {
    let text = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(format!("{}: {error}", file.display())),
    };
    let mut document = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| format!("{}: {error}", file.display()))?;
    document.entry("ui").or_insert(toml_edit::table())["theme"] = toml_edit::value(kind.label());
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", file.display()))?;
    }
    write_atomic(file, &document.to_string(), 0o644)
        .map_err(|error| format!("{}: {error}", file.display()))
}
