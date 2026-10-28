//! What the terminal shows while the TUI is out of sight: the window title
//! and the bell.

use std::time::Duration;

use crate::wrap;

/// A turn must have run this long before its end or an approval rings the bell.
pub const LONG_TURN: Duration = Duration::from_secs(10);

/// Pushes the terminal's title, so the exit can pop it back.
const PUSH_TITLE: &str = "\x1b[22;0t";
pub const POP_TITLE: &str = "\x1b[23;0t";

/// What the title and the bell are derived from.
pub struct Snapshot<'a> {
    pub workspace: &'a str,
    pub title: Option<&'a str>,
    pub approval: bool,
    pub busy: bool,
    pub ring: bool,
}

pub struct Notifier {
    pub bell: bool,
    title: Option<String>,
}

impl Default for Notifier {
    fn default() -> Self {
        Self {
            bell: true,
            title: None,
        }
    }
}

impl Notifier {
    /// Whether the title was ever set, so the exit has one to pop.
    pub fn titled(&self) -> bool {
        self.title.is_some()
    }

    /// The bytes for the terminal now: the title when it changed, the bell
    /// when the snapshot rings.
    pub fn update(&mut self, snapshot: &Snapshot) -> String {
        let mut out = String::new();
        let title = title(snapshot);
        if self.title.as_deref() != Some(&title) {
            if self.title.is_none() {
                out.push_str(PUSH_TITLE);
            }
            out.push_str(&format!("\x1b]0;{title}\x07"));
            self.title = Some(title);
        }
        if self.bell && snapshot.ring {
            out.push('\x07');
        }
        out
    }
}

/// `workspace · title · state`.
fn title(snapshot: &Snapshot) -> String {
    let state = match (snapshot.approval, snapshot.busy) {
        (true, _) => "approval",
        (false, true) => "working",
        (false, false) => "idle",
    };
    let mut parts = vec![snapshot.workspace];
    parts.extend(snapshot.title);
    parts.push(state);
    let line = parts.join(" · ").replace('\n', " ");
    wrap::truncate(&wrap::sanitize(&line), 120)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(busy: bool, approval: bool, ring: bool) -> Snapshot<'static> {
        Snapshot {
            workspace: "proj",
            title: Some("Fix parser"),
            approval,
            busy,
            ring,
        }
    }

    #[test]
    fn the_title_is_written_on_change_only() {
        let mut notifier = Notifier::default();
        let first = notifier.update(&snapshot(false, false, false));
        assert_eq!(first, "\x1b[22;0t\x1b]0;proj · Fix parser · idle\x07");
        assert_eq!(notifier.update(&snapshot(false, false, false)), "");
        assert_eq!(
            notifier.update(&snapshot(true, false, false)),
            "\x1b]0;proj · Fix parser · working\x07"
        );
        assert_eq!(notifier.update(&snapshot(true, false, false)), "");
        assert_eq!(
            notifier.update(&snapshot(true, true, false)),
            "\x1b]0;proj · Fix parser · approval\x07"
        );
    }

    #[test]
    fn a_title_without_a_session_title_has_two_parts_and_no_control_bytes() {
        let mut notifier = Notifier::default();
        let out = notifier.update(&Snapshot {
            workspace: "pro\x07j\nx",
            title: None,
            approval: false,
            busy: false,
            ring: false,
        });
        assert!(out.ends_with("\x1b]0;pro␇j x · idle\x07"), "{out:?}");
        assert_eq!(out.matches('\x07').count(), 1);
    }

    #[test]
    fn the_bell_rings_once_per_request() {
        let mut notifier = Notifier::default();
        notifier.update(&snapshot(false, false, false));
        assert_eq!(notifier.update(&snapshot(false, false, true)), "\x07");
        assert_eq!(notifier.update(&snapshot(false, false, false)), "");
    }

    #[test]
    fn the_bell_can_be_turned_off_and_the_title_still_updates() {
        let mut notifier = Notifier {
            bell: false,
            ..Notifier::default()
        };
        let out = notifier.update(&snapshot(false, false, true));
        assert!(out.contains("idle"));
        assert_eq!(
            out.matches('\x07').count(),
            1,
            "only the title's terminator"
        );
    }
}
