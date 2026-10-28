//! The app's side of the terminal title, the bell and the exit hint.

use std::io::Write;

use super::App;
use crate::title::{POP_TITLE, Snapshot};

impl App {
    /// Writes the title and bell bytes the app state calls for, if any.
    pub fn sync_terminal(&mut self) {
        let out = self.terminal_bytes();
        if !out.is_empty() {
            let mut stdout = std::io::stdout();
            let _ = stdout.write_all(out.as_bytes());
            let _ = stdout.flush();
        }
    }

    pub(super) fn terminal_bytes(&mut self) -> String {
        let workspace = self
            .workspace
            .file_name()
            .map_or_else(
                || self.workspace.to_string_lossy(),
                |name| name.to_string_lossy(),
            )
            .into_owned();
        let auto_title = self
            .session
            .as_ref()
            .and_then(|session| session.auto_title());
        let title = self
            .session
            .as_ref()
            .and_then(|session| session.info.title.as_deref())
            .filter(|title| !title.trim().is_empty())
            .or(auto_title.as_deref());
        let snapshot = Snapshot {
            workspace: &workspace,
            title,
            approval: self.has_interaction()
                || self
                    .background
                    .iter()
                    .any(|session| session.interaction.is_some()),
            busy: self.busy_elapsed().is_some(),
            ring: std::mem::take(&mut self.ring),
        };
        self.notifier.update(&snapshot)
    }

    /// What to print once the terminal is restored: the title pop and, for a
    /// session with history, the command that resumes it.
    pub fn exit_output(&self) -> String {
        let mut out = String::new();
        if self.notifier.titled() {
            out.push_str(POP_TITLE);
        }
        if let Some(session) = self
            .session
            .as_ref()
            .filter(|session| !session.transcript.blocks().is_empty())
        {
            out.push_str(&format!(
                "Resume: minicore-tui --session {}\n",
                session.info.session_id
            ));
        }
        out
    }
}
