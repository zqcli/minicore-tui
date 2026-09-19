//! Local `/settings` form state.
//!
//! The form edits only the small TUI configuration schema. Agent paths are
//! launch preferences; applying them never reloads or kills the running Agent.

use crate::config::{EditorConfig, TuiConfig};
use crate::theme::ThemeKind;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettingsField {
    Theme,
    Thinking,
    Tools,
    EditorExecutable,
    EditorArgs,
    AgentExecutable,
    AgentConfig,
    Apply,
}

impl SettingsField {
    pub const ALL: [Self; 8] = [
        Self::Theme,
        Self::Thinking,
        Self::Tools,
        Self::EditorExecutable,
        Self::EditorArgs,
        Self::AgentExecutable,
        Self::AgentConfig,
        Self::Apply,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Theme => "Theme",
            Self::Thinking => "Show thinking",
            Self::Tools => "Expand tools",
            Self::EditorExecutable => "Editor executable",
            Self::EditorArgs => "Editor args (one per line)",
            Self::AgentExecutable => "Agent executable",
            Self::AgentConfig => "Agent config",
            Self::Apply => "Apply and persist",
        }
    }

    fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|field| *field == self)
            .unwrap_or(0)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SettingsState {
    pub draft: TuiConfig,
    pub field: SettingsField,
    pub editor_executable: String,
    pub editor_args: String,
    pub agent_executable: String,
    pub agent_config: String,
    pub cursor: usize,
    pub submitting: bool,
    pub error: Option<String>,
}

impl SettingsState {
    pub fn from_config(config: &TuiConfig) -> Self {
        let editor = config.editor.clone().unwrap_or(EditorConfig {
            executable: String::new(),
            args: Vec::new(),
        });
        Self {
            draft: config.clone(),
            field: SettingsField::Theme,
            editor_executable: editor.executable,
            editor_args: editor.args.join("\n"),
            agent_executable: config
                .agent_executable
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
            agent_config: config
                .agent_config
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
            cursor: 0,
            submitting: false,
            error: None,
        }
    }

    pub fn step(&mut self, delta: i32) {
        let index = (self.field.index() as i32 + delta).rem_euclid(SettingsField::ALL.len() as i32)
            as usize;
        self.field = SettingsField::ALL[index];
        self.cursor = 0;
    }

    pub fn toggle(&mut self) {
        match self.field {
            SettingsField::Theme => {
                self.draft.theme = match self.draft.theme {
                    ThemeKind::Dark => ThemeKind::Light,
                    ThemeKind::Light => ThemeKind::Dark,
                };
            }
            SettingsField::Thinking => self.draft.thinking_visible = !self.draft.thinking_visible,
            SettingsField::Tools => self.draft.tools_expanded = !self.draft.tools_expanded,
            SettingsField::Apply
            | SettingsField::EditorExecutable
            | SettingsField::EditorArgs
            | SettingsField::AgentExecutable
            | SettingsField::AgentConfig => {}
        }
    }

    pub fn active_text_mut(&mut self) -> Option<&mut String> {
        match self.field {
            SettingsField::EditorExecutable => Some(&mut self.editor_executable),
            SettingsField::EditorArgs => Some(&mut self.editor_args),
            SettingsField::AgentExecutable => Some(&mut self.agent_executable),
            SettingsField::AgentConfig => Some(&mut self.agent_config),
            _ => None,
        }
    }

    pub fn type_char(&mut self, character: char) {
        let cursor = self.cursor;
        if let Some(text) = self.active_text_mut() {
            let cursor = cursor.min(text.len());
            let cursor = floor_boundary(text, cursor);
            text.insert(cursor, character);
            self.cursor = cursor + character.len_utf8();
        }
    }

    pub fn backspace(&mut self) {
        let cursor = self.cursor;
        if let Some(text) = self.active_text_mut() {
            let cursor = floor_boundary(text, cursor.min(text.len()));
            let Some(character) = text[..cursor].chars().next_back() else {
                return;
            };
            let start = cursor - character.len_utf8();
            text.replace_range(start..cursor, "");
            self.cursor = start;
        }
    }

    pub fn clear(&mut self) {
        if let Some(text) = self.active_text_mut() {
            text.clear();
            self.cursor = 0;
        }
    }

    pub fn build_config(&self) -> Result<TuiConfig, String> {
        let editor_args = self
            .editor_args
            .split('\n')
            .map(str::to_owned)
            .filter(|arg| !arg.is_empty())
            .collect::<Vec<_>>();
        let editor = if self.editor_executable.trim().is_empty() {
            if editor_args.is_empty() {
                None
            } else {
                return Err("editor args require an editor executable".to_owned());
            }
        } else {
            let editor = EditorConfig {
                executable: self.editor_executable.clone(),
                args: editor_args,
            };
            editor.validate().map_err(|error| error.to_string())?;
            Some(editor)
        };
        Ok(TuiConfig {
            theme: self.draft.theme,
            thinking_visible: self.draft.thinking_visible,
            tools_expanded: self.draft.tools_expanded,
            editor,
            agent_executable: nonempty_path(&self.agent_executable),
            agent_config: nonempty_path(&self.agent_config),
        })
    }
}

fn nonempty_path(value: &str) -> Option<std::path::PathBuf> {
    (!value.trim().is_empty()).then(|| std::path::PathBuf::from(value))
}

fn floor_boundary(text: &str, mut index: usize) -> usize {
    index = index.min(text.len());
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_roundtrip_preserves_utf8_and_direct_argument_rows() {
        let mut state = SettingsState::from_config(&TuiConfig::default());
        state.field = SettingsField::EditorExecutable;
        for character in "/usr/bin/editor".chars() {
            state.type_char(character);
        }
        state.field = SettingsField::EditorArgs;
        state.editor_args = "--wait\n--title\n你好".to_owned();
        state.field = SettingsField::Thinking;
        state.toggle();
        let config = state.build_config().unwrap();
        assert_eq!(
            config.editor.unwrap().args,
            vec!["--wait", "--title", "你好"]
        );
        assert!(!config.thinking_visible);
    }

    #[test]
    fn settings_step_wraps_without_a_dynamic_keymap() {
        let mut state = SettingsState::from_config(&TuiConfig::default());
        state.step(-1);
        assert_eq!(state.field, SettingsField::Apply);
        state.step(1);
        assert_eq!(state.field, SettingsField::Theme);
    }
}
