//! The draft: the welcome screen before any session exists. A session is
//! created by the first message and takes the settings picked meanwhile.

use serde_json::json;

use super::{App, NoticeLevel};
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

impl App {
    /// The model the next message runs on: the session's, else the one a new session would get.
    pub fn model_id(&self) -> Option<&str> {
        match &self.session {
            Some(session) => Some(&session.info.model),
            None => self
                .launch
                .model
                .as_deref()
                .or_else(|| Some(self.draft_profile()?.model.as_str())),
        }
    }

    /// The reasoning level of the next message, resolved like [`App::model_id`].
    pub fn reasoning(&self) -> Option<Reasoning> {
        match &self.session {
            Some(session) => Some(session.info.reasoning),
            None => self
                .launch
                .reasoning
                .or_else(|| Some(self.draft_profile()?.reasoning)),
        }
    }

    /// The profile a new session would use: the one asked for, else the Agent's default.
    fn draft_profile(&self) -> Option<&ProfileInfo> {
        let id = self
            .launch
            .profile
            .as_ref()
            .or(self.default_profile.as_ref())?;
        self.profiles.iter().find(|profile| profile.id == *id)
    }

    /// Switches the session's model, or the draft's; a reasoning level the
    /// model lacks moves to the nearest one it has in the same request.
    pub(super) fn set_model(&mut self, id: String) {
        if self.session.is_some() {
            let mut fields = json!({ "model": id });
            if let Some(level) = self.reasoning_for(&id) {
                fields["reasoning"] = json!(level);
            }
            self.update_session(fields);
        } else {
            self.notice(NoticeLevel::Info, format!("New session: model {id}"));
            if let Some(level) = self.reasoning_for(&id) {
                self.launch.reasoning = Some(level);
            }
            self.launch.model = Some(id);
        }
    }

    /// The level to switch to when `model` does not support the current one,
    /// with a notice saying so.
    fn reasoning_for(&mut self, model: &str) -> Option<Reasoning> {
        let current = self.reasoning()?;
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
        if self.session.is_some() {
            self.update_session(json!({ "reasoning": level }));
        } else {
            let text = format!("New session: reasoning {}", level.label());
            self.notice(NoticeLevel::Info, text);
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

    /// The first Enter of a draft: creates the session, which then sends the
    /// text. Text entered while a session is being opened stays in the editor.
    pub(super) fn start_session(&mut self) {
        if !self.ready || self.loading_session {
            return;
        }
        let text = self.composer.submit().trim_end().to_owned();
        if text.is_empty() {
            return;
        }
        self.first_message = Some(text);
        self.create_session();
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
