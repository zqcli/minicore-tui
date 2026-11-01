//! The draft: the welcome screen before any session exists. A session is
//! created by the first message and takes the settings picked meanwhile.

use serde_json::{Value, json};

use super::{App, LaunchSettings, NoticeLevel};
use crate::protocol::{ProfileInfo, Reasoning};

/// The levels that rank against each other, lowest first.
const SCALE: [Reasoning; 6] = [
    Reasoning::Low,
    Reasoning::Medium,
    Reasoning::High,
    Reasoning::XHigh,
    Reasoning::Max,
    Reasoning::Ultra,
];

pub(super) const NO_SESSION: &str = "No session yet: send a message first";

impl LaunchSettings {
    fn profile<'a>(
        &self,
        profiles: &'a [ProfileInfo],
        default: Option<&str>,
    ) -> Option<&'a ProfileInfo> {
        let id = self.profile.as_deref().or(default)?;
        profiles.iter().find(|profile| profile.id == id)
    }

    pub fn model_id<'a>(
        &'a self,
        profiles: &'a [ProfileInfo],
        default: Option<&str>,
    ) -> Option<&'a str> {
        self.model
            .as_deref()
            .or_else(|| Some(self.profile(profiles, default)?.model.as_str()))
    }

    pub fn reasoning(&self, profiles: &[ProfileInfo], default: Option<&str>) -> Option<Reasoning> {
        self.reasoning
            .or_else(|| Some(self.profile(profiles, default)?.reasoning))
    }
}

impl App {
    pub(super) fn create_fields(&self) -> Value {
        let mut fields = self.launch.fields(true);
        fields["workspace"] = json!(self.workspace);
        fields
    }

    pub fn draft_launch(&self) -> &LaunchSettings {
        self.remote
            .draft
            .as_ref()
            .map_or(&self.launch, |draft| &draft.launch)
    }

    /// The model the next message runs on: the session's, else the one a new session would get.
    pub fn model_id(&self) -> Option<&str> {
        match self.resident.current() {
            Some(session) => Some(&session.info.model),
            None => self
                .draft_launch()
                .model_id(&self.profiles, self.default_profile.as_deref()),
        }
    }

    /// The reasoning level of the next message, resolved like [`App::model_id`].
    pub fn reasoning(&self) -> Option<Reasoning> {
        match self.resident.current() {
            Some(session) => Some(session.info.reasoning),
            None => self
                .draft_launch()
                .reasoning(&self.profiles, self.default_profile.as_deref()),
        }
    }

    /// Switches the session's model, or the draft's; a reasoning level the
    /// model lacks moves to the nearest one it has in the same request.
    pub(super) fn set_model(&mut self, id: String) {
        if self.resident.current().is_some() {
            let fields = self.model_fields(&id, self.reasoning());
            self.update_session(fields);
        } else {
            if let Some(level) = self.reasoning_for_level(&id, self.reasoning()) {
                self.launch.reasoning = Some(level);
            }
            self.launch.model = Some(id);
        }
    }

    pub(super) fn model_fields(&mut self, model: &str, current: Option<Reasoning>) -> Value {
        let mut fields = json!({"model": model});
        if let Some(level) = self.reasoning_for_level(model, current) {
            fields["reasoning"] = json!(level);
        }
        fields
    }

    fn reasoning_for_level(
        &mut self,
        model: &str,
        current: Option<Reasoning>,
    ) -> Option<Reasoning> {
        let current = current?;
        let supported = &self
            .models
            .iter()
            .find(|info| info.id == model)?
            .supported_reasoning;
        if supported.is_empty() || supported.contains(&current) {
            return None;
        }
        let level = nearest_reasoning(current, supported);
        let text = format!(
            "{model} does not support reasoning {}; using {}",
            current.label(),
            level.label()
        );
        self.notice(NoticeLevel::Info, text);
        Some(level)
    }

    /// Sets the session's reasoning level, or the draft's.
    pub(super) fn set_reasoning(&mut self, level: Reasoning) {
        if self.resident.current().is_some() {
            self.update_session(json!({ "reasoning": level }));
        } else {
            self.launch.reasoning = Some(level);
        }
    }

    /// `/new`: leaves the session; the first message creates the next one.
    pub(super) fn new_draft(&mut self, profile: Option<String>) {
        self.show_session(None);
        if profile.is_some() {
            self.launch.profile = profile;
        }
    }
}

/// The supported level closest to `current` on [`SCALE`], the higher one on a
/// tie; the first supported level when `current` or none of them is on it.
fn nearest_reasoning(current: Reasoning, supported: &[Reasoning]) -> Reasoning {
    let rank = |level| SCALE.iter().position(|scaled| *scaled == level);
    rank(current)
        .and_then(|from| {
            supported
                .iter()
                .filter_map(|&level| Some((level, rank(level)?)))
                .min_by_key(|&(_, at)| (at.abs_diff(from), std::cmp::Reverse(at)))
        })
        .map_or(supported[0], |(level, _)| level)
}

#[cfg(test)]
mod tests {
    use super::*;
    use Reasoning::*;

    #[test]
    fn the_nearest_supported_level_wins_and_a_tie_goes_up() {
        assert_eq!(nearest_reasoning(XHigh, &[High, Max]), Max);
        assert_eq!(nearest_reasoning(Ultra, &[Low, Medium, High]), High);
        assert_eq!(nearest_reasoning(Low, &[Medium, High]), Medium);
    }

    #[test]
    fn a_level_off_the_scale_takes_the_first_supported_one() {
        assert_eq!(nearest_reasoning(Auto, &[Medium, High]), Medium);
        assert_eq!(nearest_reasoning(Disabled, &[High, Max]), High);
        assert_eq!(nearest_reasoning(High, &[Auto, Disabled]), Auto);
    }
}
