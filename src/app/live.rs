//! Live sessions: the foreground on screen and the background ones, which keep
//! running, say when they need attention and come back as they were left.

use std::path::PathBuf;

use super::overlay::session_label;
use super::{App, LiveStatus, NoticeLevel, Overlay, Scroll, Session};
use crate::transcript::Block;

impl App {
    /// Shows session `id`: at once when it is live, else opened in the Agent.
    pub fn open_session(&mut self, id: String) {
        if self
            .resident
            .current()
            .is_some_and(|session| session.info.session_id == id)
        {
            return;
        }
        if let Some(epoch) = self.live(&id).map(|session| session.epoch) {
            // The session being created takes the screen when it arrives.
            if self.first_message.is_none() {
                self.switch_to(epoch);
            }
            return;
        }
        self.request_session_open(id);
    }

    /// The loaded session, independently of which view is selected.
    pub fn live(&self, id: &str) -> Option<&Session> {
        self.resident.find(id)
    }

    /// Background sessions working (running or compacting), and those waiting
    /// for an approval.
    pub fn background_counts(&self) -> (usize, usize) {
        self.other_counts(
            self.resident
                .current()
                .map(|session| session.info.session_id.as_str()),
        )
    }

    pub fn other_counts(&self, current: Option<&str>) -> (usize, usize) {
        self.resident
            .iter()
            .filter(|session| Some(session.info.session_id.as_str()) != current)
            .fold((0, 0), |(working, approval), session| {
                match session.live_status() {
                    LiveStatus::Idle => (working, approval),
                    LiveStatus::Approval => (working, approval + 1),
                    LiveStatus::Running | LiveStatus::Compacting => (working + 1, approval),
                }
            })
    }

    pub(super) fn session_label(&self, epoch: u64) -> String {
        let session = self.resident.get(epoch).expect("a resident session");
        session_label(&session.info, Some(session))
    }

    pub(super) fn switch_to(&mut self, epoch: u64) {
        if self.resident.is_current(epoch) {
            return;
        }
        self.select_session(Some(epoch));
        self.refresh_session_details_for(epoch);
        self.refresh_context_for(epoch);
    }

    pub(super) fn show_session(&mut self, session: Option<Session>) {
        let epoch = session.as_ref().map(|session| session.epoch);
        if let Some(session) = session {
            self.resident.insert(session);
        }
        self.select_session(epoch);
    }

    /// Navigation restores a view without changing any operation's target.
    pub(super) fn select_session(&mut self, epoch: Option<u64>) {
        if self.opening_target.take().is_some() {
            self.loading_session = false;
        }
        let typed = self.composer.text().to_owned();
        match self.resident.current_mut() {
            Some(left) => {
                left.draft = (!typed.is_empty()).then_some(typed);
                left.scroll = self.scroll;
            }
            None => self.draft_text = typed,
        }
        self.resident.select(epoch);
        let text = match self.resident.current_mut() {
            Some(session) => {
                session.unseen = false;
                self.scroll = session.scroll;
                self.workspace = PathBuf::from(&session.info.workspace);
                let returned = if self.remote.active() {
                    Vec::new()
                } else {
                    std::mem::take(&mut session.returned)
                };
                session
                    .draft
                    .take()
                    .into_iter()
                    .chain(returned)
                    .collect::<Vec<_>>()
                    .join("\n\n")
            }
            None => {
                self.scroll = Scroll::Follow;
                std::mem::take(&mut self.draft_text)
            }
        };
        self.composer.set_text(&text);
        self.layout.clear();
        self.overlay = Overlay::None;
        self.selection = None;
    }

    pub(super) fn scroll_for(&self, epoch: u64) -> Scroll {
        if self.resident.is_current(epoch) {
            self.scroll
        } else {
            self.resident.get(epoch).expect("a resident session").scroll
        }
    }

    pub(super) fn set_scroll_for(&mut self, epoch: u64, scroll: Scroll) {
        if self.resident.is_current(epoch) {
            self.scroll = scroll;
        } else {
            self.session_mut(epoch).scroll = scroll;
        }
    }

    /// Announces a background turn end and marks it unseen unless a phone views it.
    pub(super) fn background_turn_ended(&mut self, epoch: u64, ended_loop: &str) {
        let label = self.session_label(epoch);
        let id = self.resident.get(epoch).unwrap().info.session_id.clone();
        let viewed = self.remote.viewing(&id);
        let session = self.session_mut(epoch);
        session.unseen = !viewed;
        let Some(Block::TurnEnd {
            outcome, duration, ..
        }) =
            session.transcript.blocks().iter().find(
                |block| matches!(block, Block::TurnEnd { loop_id, .. } if loop_id == ended_loop),
            )
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
    pub(super) fn background_approval(&mut self, epoch: u64) {
        self.ring = true;
        let id = self.resident.get(epoch).unwrap().info.session_id.clone();
        let text = format!("\"{}\" needs approval", self.session_label(epoch));
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

    /// Why session `id` cannot be deleted now: it is on screen, still working
    /// in the background, or holds queued messages the Agent would refuse to drop.
    pub(super) fn delete_refusal(&self, id: &str) -> Option<String> {
        if self
            .resident
            .current()
            .is_some_and(|session| session.info.session_id == id)
        {
            return Some("Open another session before deleting this one".to_owned());
        }
        let session = self.resident.find(id)?;
        let label = || session_label(&session.info, Some(session));
        if session.live_status() != LiveStatus::Idle {
            Some(format!("Stop \"{}\" first: open it and press Esc", label()))
        } else if !session.queue.items.is_empty() {
            Some(format!(
                "Clear the queued messages of \"{}\" first",
                label()
            ))
        } else {
            None
        }
    }
}
