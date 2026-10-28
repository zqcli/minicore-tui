//! Live sessions: the foreground on screen and the background ones, which keep
//! running, say when they need attention and come back as they were left.

use std::path::PathBuf;

use super::overlay::session_label;
use super::{App, LiveStatus, NoticeLevel, Overlay, Scroll, Session};
use crate::transcript::Block;

impl App {
    /// Where the background session `id` is.
    pub(super) fn live_index(&self, id: &str) -> Option<usize> {
        self.background
            .iter()
            .position(|session| session.info.session_id == id)
    }

    /// The live session `id`, on screen or in the background.
    pub fn live(&self, id: &str) -> Option<&Session> {
        self.session
            .iter()
            .chain(&self.background)
            .find(|session| session.info.session_id == id)
    }

    /// Background sessions working (running or compacting), and those waiting
    /// for an approval.
    pub fn background_counts(&self) -> (usize, usize) {
        self.other_counts(
            self.session
                .as_ref()
                .map(|session| session.info.session_id.as_str()),
        )
    }

    pub fn other_counts(&self, current: Option<&str>) -> (usize, usize) {
        self.session
            .iter()
            .chain(&self.background)
            .filter(|session| Some(session.info.session_id.as_str()) != current)
            .fold((0, 0), |(working, approval), session| {
                match session.live_status() {
                    LiveStatus::Idle => (working, approval),
                    LiveStatus::Approval => (working, approval + 1),
                    LiveStatus::Running | LiveStatus::Compacting => (working + 1, approval),
                }
            })
    }

    /// The foreground session's label, for notices about it.
    pub(super) fn session_label(&self) -> String {
        let session = self.session.as_ref().expect("a live session");
        session_label(&session.info, Some(session))
    }

    /// Runs reducer code for the background session at `index` by swapping it
    /// into the foreground with its own view state, so the code needs no
    /// second path and the foreground's view stays as it is.
    pub(super) fn with_background(&mut self, index: usize, op: impl FnOnce(&mut Self)) {
        let mut session = self.background.remove(index);
        std::mem::swap(&mut self.scroll, &mut session.scroll);
        let foreground = self.session.replace(session);
        self.background_op = true;
        let id = self.session.as_ref().unwrap().info.session_id.clone();
        self.with_notice_for(&id, op);
        self.background_op = false;
        let mut session = std::mem::replace(&mut self.session, foreground)
            .expect("reducer code keeps its session");
        std::mem::swap(&mut self.scroll, &mut session.scroll);
        self.background.insert(index, session);
    }

    /// Brings the background session at `index` to the screen at once: no
    /// history is read, only the footer's figures are refreshed.
    pub(super) fn switch_to(&mut self, index: usize) {
        let session = self.background.remove(index);
        self.show_session(Some(session));
        self.refresh_branch();
        self.refresh_context();
    }

    /// Puts `session` on screen, or the draft for `None`, with the composer
    /// text and view it was left with; the foreground it replaces stays live,
    /// first in the background.
    pub(super) fn show_session(&mut self, mut session: Option<Session>) {
        let typed = self.composer.text().to_owned();
        match self.session.take() {
            Some(mut left) => {
                left.draft = (!typed.is_empty()).then_some(typed);
                left.scroll = self.scroll;
                self.background.insert(0, left);
            }
            None => self.draft_text = typed,
        }
        let text = match &mut session {
            Some(session) => {
                session.unseen = false;
                self.scroll = session.scroll;
                self.workspace = PathBuf::from(&session.info.workspace);
                let returned = if self.remote.active() {
                    Vec::new()
                } else {
                    std::mem::take(&mut session.returned)
                };
                let parts: Vec<String> = session.draft.take().into_iter().chain(returned).collect();
                parts.join("\n\n")
            }
            None => {
                self.scroll = Scroll::Follow;
                std::mem::take(&mut self.draft_text)
            }
        };
        self.composer.set_text(&text);
        self.session = session;
        self.layout.clear();
        self.overlay = Overlay::None;
        self.selection = None;
    }

    /// Announces a background turn end and marks it unseen unless a phone views it.
    pub(super) fn background_turn_ended(&mut self) {
        let label = self.session_label();
        let id = self.session.as_ref().unwrap().info.session_id.clone();
        let viewed = self.remote.viewing(&id);
        let session = self.session_mut();
        session.unseen = !viewed;
        let Some(Block::TurnEnd {
            outcome, duration, ..
        }) = session.transcript.blocks().last()
        else {
            unreachable!("ending a turn appends its turn-end line");
        };
        let text = match (outcome.is_completed(), duration) {
            (true, Some(duration)) => {
                format!(
                    "finished in {}",
                    crate::ui::blocks::format_duration(*duration)
                )
            }
            (true, None) => "finished".to_owned(),
            (false, _) => match outcome.provider().and_then(|provider| provider.status) {
                Some(status) => format!("{} ({status})", outcome.label()),
                None => outcome.label(),
            },
        };
        self.attention_notice(&id, format!("\"{label}\" {text}"), "");
    }

    /// A background approval rings and names the session to switch to.
    pub(super) fn background_approval(&mut self) {
        self.ring = true;
        let id = self.session.as_ref().unwrap().info.session_id.clone();
        let text = format!("\"{}\" needs approval", self.session_label());
        self.attention_notice(&id, text, " · ctrl+r");
    }

    /// Tells the TUI (with its key `hint`) and the phones not viewing `id`.
    fn attention_notice(&mut self, id: &str, text: String, hint: &str) {
        self.remote.send_viewers(
            id,
            false,
            serde_json::json!({"t":"notice", "level":"info", "text":text}),
        );
        self.push_notice(NoticeLevel::Info, format!("{text}{hint}"));
    }

    /// Why session `id` cannot be deleted now: it is on screen, or still
    /// working in the background.
    pub(super) fn delete_refusal(&self, id: &str) -> Option<String> {
        if self
            .session
            .as_ref()
            .is_some_and(|session| session.info.session_id == id)
        {
            return Some("Open another session before deleting this one".to_owned());
        }
        let session = &self.background[self.live_index(id)?];
        (session.live_status() != LiveStatus::Idle).then(|| {
            format!(
                "Stop \"{}\" first: open it and press Esc",
                session_label(&session.info, Some(session))
            )
        })
    }
}
