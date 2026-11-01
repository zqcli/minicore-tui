//! Resident sessions and the terminal's selection.

use super::Session;

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

    pub fn find(&self, id: &str) -> Option<&Session> {
        self.items
            .iter()
            .find(|session| session.info.session_id == id)
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

    pub fn remove(&mut self, id: &str) {
        self.items.retain(|session| session.info.session_id != id);
        if self.selected.is_some_and(|epoch| self.get(epoch).is_none()) {
            self.selected = None;
        }
    }

    /// Selection changes recency, never the target of an in-flight operation.
    pub fn select(&mut self, epoch: Option<u64>) {
        if let Some(epoch) = epoch {
            let at = self
                .items
                .iter()
                .position(|session| session.epoch == epoch)
                .expect("select a resident session");
            self.items[..=at].rotate_right(1);
        }
        self.selected = epoch;
    }
}
