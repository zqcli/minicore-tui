//! What the terminal shows while the TUI is out of sight: the window title
//! and the bell.

use std::time::Duration;

use crate::wrap;

/// A turn must have run this long before its end or an approval rings the bell.
const LONG_TURN: Duration = Duration::from_secs(10);

/// Pushes the terminal's title, so the exit can pop it back.
const PUSH_TITLE: &str = "\x1b[22;0t";
pub const POP_TITLE: &str = "\x1b[23;0t";

/// What the title and the bell are derived from.
pub struct Snapshot<'a> {
    pub workspace: &'a str,
    pub title: Option<&'a str>,
    pub approval: bool,
    /// How long the running turn has run; `None` when idle.
    pub busy_for: Option<Duration>,
}

pub struct Notifier {
    pub bell: bool,
    title: Option<String>,
    last_busy: Option<Duration>,
    approval_rung: bool,
}

impl Default for Notifier {
    fn default() -> Self {
        Self {
            bell: true,
            title: None,
            last_busy: None,
            approval_rung: false,
        }
    }
}

impl Notifier {
    /// Whether the title was ever set, so the exit has one to pop.
    pub fn titled(&self) -> bool {
        self.title.is_some()
    }

    /// The bytes for the terminal now: the title when it changed, the bell
    /// when a long turn just ended or just asked for approval.
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
        let ended =
            self.last_busy.is_some_and(|busy| busy >= LONG_TURN) && snapshot.busy_for.is_none();
        let asked = snapshot.approval
            && !self.approval_rung
            && snapshot.busy_for.is_some_and(|busy| busy >= LONG_TURN);
        if self.bell && (ended || asked) {
            out.push('\x07');
        }
        self.last_busy = snapshot.busy_for;
        self.approval_rung = snapshot.approval;
        out
    }
}

/// `workspace · title · state`.
fn title(snapshot: &Snapshot) -> String {
    let state = match (snapshot.approval, snapshot.busy_for) {
        (true, _) => "approval",
        (false, Some(_)) => "working",
        (false, None) => "idle",
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

    /// The bells in `out`, not counting the terminator of a title sequence.
    fn bells(out: &str) -> usize {
        let mut rest = out;
        let mut count = 0;
        while let Some(at) = rest.find("\x1b]0;") {
            count += rest[..at].matches('\x07').count();
            let title = &rest[at..];
            rest = &title[title.find('\x07').map_or(title.len(), |end| end + 1)..];
        }
        count + rest.matches('\x07').count()
    }

    fn snapshot(busy: Option<u64>, approval: bool) -> Snapshot<'static> {
        Snapshot {
            workspace: "proj",
            title: Some("Fix parser"),
            approval,
            busy_for: busy.map(Duration::from_secs),
        }
    }

    #[test]
    fn the_title_is_written_on_change_only() {
        let mut notifier = Notifier::default();
        let first = notifier.update(&snapshot(None, false));
        assert_eq!(first, "\x1b[22;0t\x1b]0;proj · Fix parser · idle\x07");
        assert_eq!(notifier.update(&snapshot(None, false)), "");
        assert_eq!(
            notifier.update(&snapshot(Some(1), false)),
            "\x1b]0;proj · Fix parser · working\x07"
        );
        assert_eq!(notifier.update(&snapshot(Some(2), false)), "");
        assert_eq!(
            notifier.update(&snapshot(Some(3), true)),
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
            busy_for: None,
        });
        assert!(out.ends_with("\x1b]0;pro␇j x · idle\x07"), "{out:?}");
        assert_eq!(out.matches('\x07').count(), 1);
    }

    #[test]
    fn a_long_turn_rings_when_it_ends_and_a_short_one_does_not() {
        let mut notifier = Notifier::default();
        notifier.update(&snapshot(Some(30), false));
        assert_eq!(bells(&notifier.update(&snapshot(None, false))), 1);
        assert_eq!(bells(&notifier.update(&snapshot(None, false))), 0);

        notifier.update(&snapshot(Some(4), false));
        assert_eq!(bells(&notifier.update(&snapshot(None, false))), 0);
    }

    #[test]
    fn a_long_turn_rings_once_when_an_approval_appears() {
        let mut notifier = Notifier::default();
        notifier.update(&snapshot(Some(12), false));
        assert_eq!(bells(&notifier.update(&snapshot(Some(13), true))), 1);
        assert_eq!(bells(&notifier.update(&snapshot(Some(14), true))), 0);
        // The turn ends after the approval: that is a second, separate ring.
        assert_eq!(bells(&notifier.update(&snapshot(None, false))), 1);
    }

    #[test]
    fn a_young_turn_does_not_ring_for_an_approval() {
        let mut notifier = Notifier::default();
        notifier.update(&snapshot(Some(2), false));
        assert_eq!(bells(&notifier.update(&snapshot(Some(3), true))), 0);
    }

    #[test]
    fn the_bell_can_be_turned_off_and_the_title_still_updates() {
        let mut notifier = Notifier {
            bell: false,
            ..Notifier::default()
        };
        notifier.update(&snapshot(Some(30), false));
        let out = notifier.update(&snapshot(None, false));
        assert!(out.contains("idle"));
        assert_eq!(bells(&out), 0);
    }
}
