//! The draft: the conversation screen before any session exists. A session is
//! created by the first message and takes the settings picked meanwhile.

use serde_json::json;

use super::{App, NoticeLevel};
use crate::protocol::{ProfileInfo, Reasoning};

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

    /// Switches the session's model, or the draft's.
    pub(super) fn set_model(&mut self, id: String) {
        if self.session.is_some() {
            self.update_session(json!({ "model": id }));
        } else {
            self.notice(NoticeLevel::Info, format!("New session: model {id}"));
            self.launch.model = Some(id);
        }
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
