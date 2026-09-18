//! Discovery catalogs and the defaults for a future new session (spec 12.3).

use std::path::PathBuf;

use crate::protocol::{ModelInfo, ProfileInfo, Reasoning};

/// Everything the bootstrap phase learns about models, profiles, and the
/// defaults a new session would use.
#[derive(Debug)]
pub struct CatalogState {
    pub models: Vec<ModelInfo>,
    pub profiles: Vec<ProfileInfo>,
    /// True once ping + all three catalog/session list requests succeeded.
    pub loaded: bool,
    pub next_profile: Option<String>,
    pub next_model: Option<String>,
    pub next_reasoning: Option<Reasoning>,
    pub default_workspace: PathBuf,
    /// Monotone catalog generation. A local create/open/rename/close/delete
    /// bumps it, so a `session.list` response issued before that mutation is
    /// discarded instead of resurrecting an old title or a deleted row
    /// (spec §3.5).
    pub session_list_generation: u64,
}

impl CatalogState {
    pub fn seed_seats(
        &mut self,
        known: &std::collections::BTreeMap<String, crate::state::SessionView>,
    ) {
        if self.next_profile.is_none() {
            self.next_profile = self.profiles.first().map(|profile| profile.id.clone());
        }
        if self.next_model.is_none() {
            self.next_model = self
                .profiles
                .first()
                .map(|profile| profile.model.clone())
                .or_else(|| self.models.first().map(|model| model.id.clone()));
        }
        if self.next_reasoning.is_none() {
            self.next_reasoning = self
                .profiles
                .first()
                .map(|profile| profile.reasoning)
                .or_else(|| known.values().next().map(|view| view.info.reasoning))
                .or(Some(Reasoning::Auto));
        }
    }
}
