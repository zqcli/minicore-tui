//! `models.toml`: providers hold what endpoints share, models what a single
//! model needs. Each key maps onto one field of the Agent's `[models.X]`.

use std::collections::BTreeMap;

use serde::Deserialize;
use toml::Spanned;

use super::compose::AgentModel;
use super::{ConfigError, Source};
use crate::protocol::Reasoning;

/// The only provider API the Agent implements.
const API: &str = "open_ai_responses";
const DEFAULT_SAFETY_MARGIN: u32 = 4000;
pub(super) const LEVELS: &str = "auto, disabled, low, medium, high, xhigh, max, ultra";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelsFile {
    #[serde(default)]
    providers: BTreeMap<String, Provider>,
    #[serde(default)]
    models: BTreeMap<String, Model>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Provider {
    api: Spanned<String>,
    base_url: Spanned<String>,
    /// The name of the environment variable holding the key, never the key.
    api_key_env: Spanned<String>,
    /// Default for the provider's models.
    request_timeout_seconds: Option<Spanned<u64>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Model {
    provider: Spanned<String>,
    /// The name sent to the API; defaults to the id.
    model: Option<String>,
    context_window: Spanned<u32>,
    max_output_tokens: Spanned<u32>,
    safety_margin_tokens: Option<u32>,
    /// Defaults to `["auto"]`.
    reasoning: Option<Spanned<Vec<String>>>,
    /// Defaults to true.
    tools: Option<bool>,
    request_timeout_seconds: Option<Spanned<u64>>,
}

pub(super) fn join<'a>(keys: impl Iterator<Item = &'a String>) -> String {
    keys.map(String::as_str).collect::<Vec<_>>().join(", ")
}

impl ModelsFile {
    pub(super) fn build(
        &self,
        source: &Source,
    ) -> Result<BTreeMap<String, AgentModel>, ConfigError> {
        if self.models.is_empty() {
            return Err(source.error(
                None,
                "no [models.*] defined; add a provider and a model (see docs/configuration.md)",
            ));
        }
        for (name, provider) in &self.providers {
            provider.check(name, source)?;
        }
        self.models
            .iter()
            .map(|(id, model)| Ok((id.clone(), model.build(id, self, source)?)))
            .collect()
    }
}

impl Provider {
    fn check(&self, name: &str, source: &Source) -> Result<(), ConfigError> {
        let at = |span: std::ops::Range<usize>, message: String| {
            source.error(Some(span), format!("[providers.{name}] {message}"))
        };
        if self.api.get_ref() != API {
            return Err(at(
                self.api.span(),
                format!(
                    "api: {:?} is not supported (supported: {API})",
                    self.api.get_ref()
                ),
            ));
        }
        let url = self.base_url.get_ref();
        let host = url
            .strip_prefix("https://")
            .or_else(|| url.strip_prefix("http://"))
            .unwrap_or_default();
        if host.is_empty() || url.contains(['?', '#', '@', ' ']) {
            return Err(at(
                self.base_url.span(),
                format!(
                    "base_url: {url:?} must be an http(s) URL without credentials, query or fragment"
                ),
            ));
        }
        let key = self.api_key_env.get_ref();
        let is_name = !key.is_empty()
            && !key.starts_with(|ch: char| ch.is_ascii_digit())
            && key
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_');
        if !is_name {
            return Err(at(
                self.api_key_env.span(),
                "api_key_env must be the NAME of an environment variable such as OPENAI_API_KEY, not a key; secrets never go in this file".into(),
            ));
        }
        if let Some(timeout) = &self.request_timeout_seconds
            && *timeout.get_ref() == 0
        {
            return Err(at(
                timeout.span(),
                "request_timeout_seconds must be above 0".into(),
            ));
        }
        Ok(())
    }
}

impl Model {
    fn build(
        &self,
        id: &str,
        file: &ModelsFile,
        source: &Source,
    ) -> Result<AgentModel, ConfigError> {
        let at = |span: std::ops::Range<usize>, message: String| {
            source.error(Some(span), format!("[models.{id}] {message}"))
        };
        let provider_name = self.provider.get_ref();
        let Some(provider) = file.providers.get(provider_name) else {
            return Err(at(
                self.provider.span(),
                format!(
                    "provider: {provider_name:?} is not defined under [providers.*] (defined: {})",
                    join(file.providers.keys())
                ),
            ));
        };
        let grammar = id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':' | b'/'));
        if id.is_empty() || id.len() > 256 || !grammar {
            return Err(at(
                self.provider.span(),
                "the id may only contain letters, digits and _ - . : / (quote it in the table name)".into(),
            ));
        }
        let name = self.model.as_deref().unwrap_or(id);
        if name.trim().is_empty() {
            return Err(at(self.provider.span(), "model must not be blank".into()));
        }
        let output = *self.max_output_tokens.get_ref();
        let context = *self.context_window.get_ref();
        let margin = self.safety_margin_tokens.unwrap_or(DEFAULT_SAFETY_MARGIN);
        if output == 0 {
            return Err(at(
                self.max_output_tokens.span(),
                "max_output_tokens must be above 0".into(),
            ));
        }
        if u64::from(context) <= u64::from(output) + u64::from(margin) {
            return Err(at(
                self.context_window.span(),
                format!(
                    "context_window {context} must exceed max_output_tokens {output} + safety_margin_tokens {margin}"
                ),
            ));
        }
        let supported_reasoning = match &self.reasoning {
            None => vec![Reasoning::Auto],
            Some(levels) => {
                if levels.get_ref().is_empty() {
                    return Err(at(
                        levels.span(),
                        "reasoning must list at least one level".into(),
                    ));
                }
                levels
                    .get_ref()
                    .iter()
                    .map(|level| {
                        Reasoning::parse(level).ok_or_else(|| {
                            at(
                                levels.span(),
                                format!("reasoning: unknown level {level:?} (one of {LEVELS})"),
                            )
                        })
                    })
                    .collect::<Result<_, _>>()?
            }
        };
        let timeout = self
            .request_timeout_seconds
            .as_ref()
            .or(provider.request_timeout_seconds.as_ref());
        if let Some(timeout) = &self.request_timeout_seconds
            && *timeout.get_ref() == 0
        {
            return Err(at(
                timeout.span(),
                "request_timeout_seconds must be above 0".into(),
            ));
        }
        Ok(AgentModel {
            provider: provider.api.get_ref().clone(),
            model: name.to_owned(),
            base_url: provider.base_url.get_ref().clone(),
            api_key_env: provider.api_key_env.get_ref().clone(),
            physical_context_window: context,
            output_budget_tokens: output,
            safety_margin_tokens: margin,
            supported_reasoning,
            supports_tools: self.tools.unwrap_or(true),
            request_timeout_seconds: timeout.map(|t| *t.get_ref()),
        })
    }
}
