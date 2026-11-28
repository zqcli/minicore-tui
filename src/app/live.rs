//! Terminal selection changes no Host default or execution owner.

use super::{App, LiveStatus, NoticeLevel, Overlay, Scroll, Session};
use minicore_host_protocol::*;
use std::collections::HashSet;

impl App {
    pub fn open_session(&mut self, target: SessionRef) {
        if self
            .return_to
            .as_ref()
            .is_some_and(|(child, _)| child != &target)
        {
            self.return_to = None;
        }
        if self
            .resident
            .current()
            .is_some_and(|s| s.info.agent_id == target.agent_id && s.info.id == target.session_id)
        {
            self.cancel_navigation();
            return;
        }
        if let Some(epoch) = self.resident.find(&target).map(|s| s.epoch) {
            self.switch_to(epoch);
        } else {
            self.request_session_open(target);
        }
    }
    pub fn live(&self, target: &SessionRef) -> Option<&Session> {
        self.resident.find(target)
    }
    pub fn background_counts(&self) -> (usize, usize) {
        let mut counts = (0, 0);
        let mut residents = HashSet::new();
        for session in self.resident.iter() {
            residents.insert((&session.info.agent_id, &session.info.id));
            if !self.resident.is_current(session.epoch) {
                match session.live_status() {
                    LiveStatus::Idle => {}
                    LiveStatus::Approval => counts.1 += 1,
                    _ => counts.0 += 1,
                }
            }
        }
        for row in self.sessions() {
            if residents.contains(&(&row.agent_id, &row.id)) {
                continue;
            }
            let status = if row.pending_interactions > 0 {
                LiveStatus::Approval
            } else if row.active_turn_id.is_some()
                || row.owned_work.preparing
                || row
                    .owned_work
                    .compaction
                    .as_ref()
                    .is_some_and(|work| !work.state.finished())
            {
                LiveStatus::Running
            } else {
                LiveStatus::Idle
            };
            match status {
                LiveStatus::Idle => {}
                LiveStatus::Approval => counts.1 += 1,
                _ => counts.0 += 1,
            }
        }
        counts
    }
    pub(super) fn session_label(&self, epoch: u64) -> String {
        let s = self.resident.get(epoch).expect("resident label");
        s.info
            .title
            .clone()
            .or_else(|| s.auto_title())
            .unwrap_or_else(|| s.info.id.to_string())
    }
    pub(super) fn switch_to(&mut self, epoch: u64) {
        if self.resident.is_current(epoch) {
            return;
        }
        self.select_session(Some(epoch));
        self.refresh_session_details_for(epoch);
        self.refresh_workspace_for(epoch);
    }
    fn cancel_navigation(&mut self) {
        self.open_generation += 1;
        self.opening_target = None;
        self.loading_session = false;
    }
    pub(super) fn select_session(&mut self, epoch: Option<u64>) {
        self.cancel_navigation();
        let text = self.composer.text().to_owned();
        match self.resident.current_mut() {
            Some(left) => {
                left.draft = (!text.is_empty()).then_some(text);
                left.scroll = self.scroll;
            }
            None => self.draft_text = text,
        }
        self.resident.select(epoch);
        let text = match self.resident.current_mut() {
            Some(s) => {
                s.unseen = false;
                self.scroll = s.scroll;
                self.selected_agent = Some(s.info.agent_id.clone());
                if let Some(workspace) = &s.info.config.workspace {
                    self.workspace = workspace.path.clone().into();
                }
                s.draft
                    .take()
                    .into_iter()
                    .chain(std::mem::take(&mut s.returned))
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
            self.resident.get(epoch).expect("resident scroll").scroll
        }
    }
    pub(super) fn set_scroll_for(&mut self, epoch: u64, scroll: Scroll) {
        if self.resident.is_current(epoch) {
            self.scroll = scroll;
        } else {
            self.session_mut(epoch).scroll = scroll;
        }
    }
    pub(super) fn background_turn_ended(&mut self, epoch: u64, outcome: TurnOutcome) {
        self.session_mut(epoch).unseen = true;
        let label = self.session_label(epoch);
        let status = match outcome {
            TurnOutcome::Completed => "finished",
            TurnOutcome::Cancelled => "cancelled",
            TurnOutcome::Failed => "failed",
        };
        self.push_notice(NoticeLevel::Info, format!("\"{label}\" {status} · ctrl+r"));
        self.ring = true;
    }
    pub(super) fn background_approval(&mut self, epoch: u64) {
        self.ring = true;
        self.push_notice(
            NoticeLevel::Info,
            format!("\"{}\" needs input · ctrl+r", self.session_label(epoch)),
        );
    }
}

impl App {
    fn current_ref(&self) -> Option<SessionRef> {
        self.resident.current().map(|session| SessionRef {
            agent_id: session.info.agent_id.clone(),
            session_id: session.info.id.clone(),
        })
    }

    /// Opens a delegated child so that Esc comes back to this Session.
    pub(super) fn open_child(&mut self, child: SessionRef) {
        let parent = self.current_ref();
        self.open_session(child.clone());
        self.return_to = parent.map(|parent| (child, parent));
    }

    /// The parent Esc returns to, while the child opened from it is current.
    pub fn return_target(&self) -> Option<&SessionRef> {
        let (child, parent) = self.return_to.as_ref()?;
        (self.current_ref().as_ref() == Some(child)).then_some(parent)
    }
}
