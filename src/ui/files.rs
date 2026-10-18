//! The `@` file picker's candidate list. The agent-side query lives in
//! `app::tasks`; this module only holds what the picker shows, so the reducer
//! and the renderer agree on the filter without sharing a widget.

use std::sync::{Mutex, OnceLock};

/// Recently fetched workspace paths, in the order the Agent returned them.
fn store() -> &'static Mutex<Vec<String>> {
    static STORE: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(Vec::new()))
}

/// Replaces the candidate list after a `workspace.files`/`workspace.search`.
pub fn set_candidates(paths: Vec<String>) {
    *store().lock().unwrap() = paths;
}

/// Candidates matching `filter`, as workspace-relative paths.
pub fn filtered(filter: &str) -> Vec<String> {
    let filter = filter.trim();
    store()
        .lock()
        .unwrap()
        .iter()
        .filter(|path| filter.is_empty() || path.contains(filter))
        .map(|path| format!("@{path}"))
        .collect()
}

pub fn filter_count(filter: &str) -> usize {
    filtered(filter).len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_are_prefixed_with_at_signs() {
        set_candidates(vec!["src/main.rs".into(), "README.md".into()]);
        assert_eq!(filtered("").len(), 2);
        assert_eq!(filtered("main"), vec!["@src/main.rs".to_owned()]);
        assert_eq!(filter_count("zzz"), 0);
    }
}
