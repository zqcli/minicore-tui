//! Resident terminal views, with immutable operation addresses.

use super::Session;
use minicore_host_protocol::{HostIncarnation, SessionRef};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub session: SessionRef,
    pub incarnation: HostIncarnation,
    pub epoch: u64,
}
impl Session {
    pub fn target(&self) -> Target {
        Target {
            session: self.address(),
            incarnation: self.state.host_incarnation.clone(),
            epoch: self.epoch,
        }
    }
}
#[derive(Default)]
pub struct Sessions {
    items: Vec<Session>,
    selected: Option<u64>,
}
impl Sessions {
    pub fn current(&self) -> Option<&Session> {
        self.selected.and_then(|epoch| self.get(epoch))
    }
    pub fn current_mut(&mut self) -> Option<&mut Session> {
        self.selected.and_then(|epoch| self.get_mut(epoch))
    }
    pub fn is_current(&self, epoch: u64) -> bool {
        self.selected == Some(epoch)
    }
    pub fn get(&self, epoch: u64) -> Option<&Session> {
        self.items.iter().find(|session| session.epoch == epoch)
    }
    pub fn get_mut(&mut self, epoch: u64) -> Option<&mut Session> {
        self.items.iter_mut().find(|session| session.epoch == epoch)
    }
    pub fn find(&self, target: &SessionRef) -> Option<&Session> {
        self.items.iter().find(|session| {
            session.info.agent_id == target.agent_id && session.info.id == target.session_id
        })
    }
    pub fn iter(&self) -> impl Iterator<Item = &Session> {
        self.items.iter()
    }
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Session> {
        self.items.iter_mut()
    }
    pub fn others(&self) -> impl Iterator<Item = &Session> {
        self.items
            .iter()
            .filter(|session| !self.is_current(session.epoch))
    }
    pub fn insert(&mut self, session: Session) {
        self.items.push(session);
    }
    pub fn remove(&mut self, target: &SessionRef) {
        self.items.retain(|session| {
            session.info.agent_id != target.agent_id || session.info.id != target.session_id
        });
        if self.selected.is_some_and(|epoch| self.get(epoch).is_none()) {
            self.selected = None;
        }
    }
    pub fn select(&mut self, epoch: Option<u64>) {
        if let Some(epoch) = epoch {
            let at = self
                .items
                .iter()
                .position(|session| session.epoch == epoch)
                .expect("resident selection");
            self.items[..=at].rotate_right(1);
        }
        self.selected = epoch;
    }
}
