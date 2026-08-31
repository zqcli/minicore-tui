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
}
