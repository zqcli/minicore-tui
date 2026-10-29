//! `config.toml`: profiles, UI preferences, the Agent binary and the Agent-wide
//! loop and compaction policy.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use toml::Spanned;

use super::compose::{AgentFile, AgentModel, AgentProfile, Approval, PromptFile};
use super::models::{LEVELS, join};
use super::{ConfigError, Home, Source, write_atomic};
use crate::protocol::Reasoning;

/// The Agent's complete tool set; a profile without `tools` gets all of it.
const TOOLS: [&str; 5] = ["read", "write", "edit", "apply_patch", "bash"];
const LOOP_KEYS: [&str; 7] = [
    "event_capacity",
    "prompt_timeout_seconds",
    "model_timeout_seconds",
    "policy_timeout_seconds",
    "tool_timeout_seconds",
    "model_retry_attempts",
    "model_retry_base_delay_millis",
];
/// The Agent refuses a larger system prompt file.
const MAX_PROMPT_BYTES: usize = 128 * 1024;

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
    /// Ring the terminal bell when a long turn ends or waits for approval.
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
#[serde(deny_unknown_fields)]
pub struct AgentSection {
    /// An external Agent, relative to the TUI directory.
    pub binary: Option<PathBuf>,
}

/// The Agent's automatic compaction policy; unset fields keep the Agent's.
#[derive(Clone, Copy, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Compaction {
    #[serde(skip_serializing_if = "Option::is_none")]
    enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    trigger_percent: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_percent: Option<u8>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    model: Spanned<String>,
    reasoning: Option<Spanned<String>>,
    approval: Option<Approval>,
    tools: Option<Spanned<Vec<String>>>,
    /// Relative to the TUI directory; default `prompt/SYSTEM_PROMPT.md`.
    prompt: Option<Spanned<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    default_profile: Spanned<String>,
    #[serde(default)]
    pub(super) remote: super::remote::RemoteSection,
    #[serde(default)]
    profiles: BTreeMap<String, Profile>,
    #[serde(default)]
    pub ui: Ui,
    #[serde(default)]
    pub agent: AgentSection,
    #[serde(default, rename = "loop")]
    loop_options: BTreeMap<String, Spanned<u64>>,
    compaction: Option<Spanned<Compaction>>,
}

impl Settings {
    /// Checks the settings against the built models and the prompt files.
    pub(super) fn build(
        &self,
        source: &Source,
        models: BTreeMap<String, AgentModel>,
        home: &Home,
    ) -> Result<AgentFile, ConfigError> {
        if self.profiles.is_empty() {
            return Err(source.error(None, "no [profiles.*] defined"));
        }
        let default = &self.default_profile;
        if !self.profiles.contains_key(default.get_ref()) {
            return Err(source.error(
                Some(default.span()),
                format!(
                    "default_profile: {:?} is not defined under [profiles.*] (defined: {})",
                    default.get_ref(),
                    join(self.profiles.keys())
                ),
            ));
        }
        let mut checked = BTreeSet::new();
        let mut profiles = BTreeMap::new();
        for (id, profile) in &self.profiles {
            let built = profile.build(id, source, &models, home, &mut checked)?;
            profiles.insert(id.clone(), built);
        }
        let mut loop_options = BTreeMap::new();
        for (key, value) in &self.loop_options {
            if !LOOP_KEYS.contains(&key.as_str()) {
                return Err(source.error(
                    Some(value.span()),
                    format!(
                        "[loop] {key}: unknown key (known: {})",
                        LOOP_KEYS.join(", ")
                    ),
                ));
            }
            loop_options.insert(key.clone(), *value.get_ref());
        }
        if let Some(compaction) = &self.compaction {
            let c = compaction.get_ref();
            let trigger = c.trigger_percent.unwrap_or(95);
            let target = c.target_percent.unwrap_or(50);
            if target == 0 || target >= trigger || trigger > 100 {
                return Err(source.error(
                    Some(compaction.span()),
                    format!(
                        "[compaction] needs 0 < target_percent < trigger_percent <= 100 (target {target}, trigger {trigger})"
                    ),
                ));
            }
        }
        Ok(AgentFile {
            data_dir: home.session_dir(),
            default_profile: default.get_ref().clone(),
            loop_options: (!loop_options.is_empty()).then_some(loop_options),
            compaction: self.compaction.as_ref().map(|c| *c.get_ref()),
            profiles,
            models,
        })
    }
}

impl Profile {
    fn build(
        &self,
        id: &str,
        source: &Source,
        models: &BTreeMap<String, AgentModel>,
        home: &Home,
        checked: &mut BTreeSet<PathBuf>,
    ) -> Result<AgentProfile, ConfigError> {
        let at = |span: &std::ops::Range<usize>, message: String| {
            source.error(Some(span.clone()), format!("[profiles.{id}] {message}"))
        };
        let model_id = self.model.get_ref();
        let Some(model) = models.get(model_id) else {
            return Err(at(
                &self.model.span(),
                format!(
                    "model: {model_id:?} is not defined in models.toml (defined: {})",
                    join(models.keys())
                ),
            ));
        };
        let reasoning_span = self
            .reasoning
            .as_ref()
            .map_or(self.model.span(), Spanned::span);
        let reasoning = match &self.reasoning {
            Some(level) => Reasoning::parse(level.get_ref()).ok_or_else(|| {
                at(
                    &reasoning_span,
                    format!(
                        "reasoning: unknown level {:?} (one of {LEVELS})",
                        level.get_ref()
                    ),
                )
            })?,
            None => Reasoning::Auto,
        };
        if !model.supported_reasoning.contains(&reasoning) {
            let supported: Vec<_> = model
                .supported_reasoning
                .iter()
                .map(|r| r.label())
                .collect();
            return Err(at(
                &reasoning_span,
                format!(
                    "reasoning {:?} is not in the `reasoning` list of model {model_id:?} in models.toml ({})",
                    reasoning.label(),
                    supported.join(", ")
                ),
            ));
        }
        let tools_span = self.tools.as_ref().map_or(self.model.span(), Spanned::span);
        let tools = match &self.tools {
            Some(tools) => tools.get_ref().clone(),
            None => TOOLS.map(str::to_owned).to_vec(),
        };
        let mut seen = BTreeSet::new();
        for tool in &tools {
            if !TOOLS.contains(&tool.as_str()) || !seen.insert(tool) {
                return Err(at(
                    &tools_span,
                    format!(
                        "tools: {tool:?} is unknown or repeated (known: {})",
                        TOOLS.join(", ")
                    ),
                ));
            }
        }
        if !tools.is_empty() && !model.supports_tools {
            return Err(at(
                &tools_span,
                format!(
                    "model {model_id:?} has `tools = false` in models.toml; set `tools = []` here"
                ),
            ));
        }
        let prompt = match &self.prompt {
            Some(path) => home.join(Path::new(path.get_ref())),
            None => home.system_prompt_file(),
        };
        if checked.insert(prompt.clone())
            && let Err(reason) = check_prompt(&prompt)
        {
            return Err(match &self.prompt {
                Some(path) => at(
                    &path.span(),
                    format!("prompt: {}: {reason}", prompt.display()),
                ),
                None => ConfigError {
                    file: prompt,
                    line: None,
                    message: reason,
                },
            });
        }
        Ok(AgentProfile {
            model: model_id.clone(),
            reasoning,
            system_prompt: PromptFile { file: prompt },
            tools,
            approval: self.approval.unwrap_or_default(),
        })
    }
}

/// Why the Agent would refuse this system prompt file, if it would.
fn check_prompt(path: &Path) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            "system prompt not found; create it (or run again in an empty directory to scaffold one)".to_owned()
        } else {
            format!("system prompt unreadable: {error}")
        }
    })?;
    if text.trim().is_empty() {
        return Err("system prompt is empty".into());
    }
    if text.len() > MAX_PROMPT_BYTES {
        return Err(format!(
            "system prompt is over {} KiB",
            MAX_PROMPT_BYTES >> 10
        ));
    }
    if text
        .chars()
        .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\t' | '\r'))
    {
        return Err("system prompt contains control characters".into());
    }
    Ok(())
}

/// The editor command: the setting first, then `$VISUAL`/`$EDITOR`.
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

/// Sets `[ui] theme` in `config.toml` and leaves its comments and other keys alone.
pub fn save_theme(file: &Path, kind: ThemeKind) -> Result<(), String> {
    let text =
        std::fs::read_to_string(file).map_err(|error| format!("{}: {error}", file.display()))?;
    let mut document = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| format!("{}: {error}", file.display()))?;
    document.entry("ui").or_insert(toml_edit::table())["theme"] = toml_edit::value(kind.label());
    write_atomic(file, &document.to_string(), 0o644)
        .map_err(|error| format!("{}: {error}", file.display()))
}
