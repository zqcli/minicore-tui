//! Gray `Steering: …` queue rows in the dock, ABOVE the Working status row
//! (0.2.4 queue contract, per the user's reference image). Display-only: the
//! queue never scrolls with the transcript, never consumes unbounded height,
//! and never squeezes a modal/selector dock.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::App;
use unicode_segmentation::UnicodeSegmentation;

use crate::state::turn::{PendingSteerState, SteerQueueState};
use crate::theme::Theme;

/// Visible queue entries for the active session in FIFO (local_id) order:
/// locally-unsent items first, then in-flight (Sending/Accepted) entries.
pub fn queue_entries(app: &App) -> Vec<(u64, String, String)> {
    let Some(view) = app.active_view() else {
        return Vec::new();
    };
    let mut entries: Vec<(u64, String, String)> = view
        .steer_queue
        .iter()
        .map(|item| {
            let state = match item.state {
                SteerQueueState::Unsent => String::new(),
                SteerQueueState::Unconfirmed => "save unconfirmed".to_owned(),
            };
            (item.local_id, item.text.clone(), state)
        })
        .collect();
    if let Some(live) = view.live.as_ref() {
        for steer in &live.pending_steers {
            let state = match steer.state {
                PendingSteerState::Sending => "sending…",
                PendingSteerState::Queued => "accepted",
                PendingSteerState::Unconfirmed => "save unconfirmed",
                PendingSteerState::Persisted | PendingSteerState::NotRecorded => continue,
            };
            entries.push((steer.local_id, steer.text.clone(), state.to_owned()));
        }
    }
    entries.sort_by_key(|(local_id, _, _)| *local_id);
    entries
}

/// True when Alt+Up can actually withdraw the next unsent item into the empty
/// editor (accurate hint, never a fake "edit all").
pub fn withdrawal_available(app: &App) -> bool {
    let Some(view) = app.active_view() else {
        return false;
    };
    if !app.composer.is_empty() {
        return false;
    }
    view.steer_queue
        .iter()
        .any(|item| item.state == SteerQueueState::Unsent && !item.handoff)
}

/// Flattens a possibly multi-line steer text into one safe display line:
/// literal newlines/control characters are replaced with a single space while
/// the STORED full text is preserved (nothing is truncated in state).
pub fn flatten_preview(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending_space = false;
    for ch in text.chars() {
        if ch.is_control() {
            pending_space = !out.is_empty();
        } else {
            if pending_space && !out.is_empty() && !out.ends_with(' ') {
                out.push(' ');
            }
            pending_space = false;
            out.push(ch);
        }
    }
    out
}

/// Cell-aware truncation: an exact fit is returned untouched (never silently
/// drops the last cell); over-width text keeps as many WHOLE grapheme clusters
/// as fit in `width - 1` display columns (the ellipsis occupies the last
/// column), never splitting a full-width (CJK) or multi-codepoint character.
pub(crate) fn truncate_to_width(label: String, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if crate::markdown::column_width(&label) <= width {
        return label;
    }
    let available = width - 1;
    let mut current = 0usize;
    let mut kept = String::new();
    for grapheme in UnicodeSegmentation::graphemes(label.as_str(), true) {
        let grapheme_width = crate::markdown::column_width(grapheme);
        if current + grapheme_width > available {
            break;
        }
        current += grapheme_width;
        kept.push_str(grapheme);
    }
    kept.push('…');
    kept
}

/// Functional Alt+Up hint shown whenever the next UNSENT item can actually be
/// withdrawn into the empty editor (the user reference shows it in a normal
/// queue too); "paused" is only a prefix, never a gate. This single predicate
/// is shared by the renderer and the layout height reservation.
pub fn hint_label(app: &App) -> Option<&'static str> {
    if !withdrawal_available(app) {
        return None;
    }
    let paused = app
        .active_view()
        .is_some_and(|view| view.steer_queue_paused);
    Some(if paused {
        "paused · ⌥↑ edit next queued message"
    } else {
        "⌥↑ edit next queued message"
    })
}

pub fn render(frame: &mut Frame<'_>, area: Rect, app: &App, theme: &Theme) {
    let width = area.width as usize;
    if width == 0 || area.height == 0 {
        return;
    }
    let entries = queue_entries(app);
    if entries.is_empty() {
        return;
    }
    let visible = crate::ui::layout::MAX_DOCK_QUEUE_ROWS as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();
    for (_, text, state) in entries.iter().take(visible) {
        let flattened = flatten_preview(text);
        let label = if state.is_empty() {
            format!("Steering: {flattened}")
        } else {
            format!("Steering ({state}): {flattened}")
        };
        lines.push(Line::from(Span::styled(
            truncate_to_width(label, width),
            Style::new().fg(theme.muted).add_modifier(Modifier::DIM),
        )));
    }
    if entries.len() > visible {
        let overflow = entries.len() - visible;
        lines.push(Line::from(Span::styled(
            format!(" +{overflow} more"),
            Style::new().fg(theme.muted).add_modifier(Modifier::DIM),
        )));
    }
    if let Some(hint) = hint_label(app) {
        lines.push(Line::from(Span::styled(
            hint,
            Style::new()
                .fg(theme.footer_amber)
                .add_modifier(Modifier::DIM),
        )));
    }
    frame.render_widget(Paragraph::new(lines), area);
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use crate::event::AppEvent;
    use crate::theme::ThemeKind;
    use crate::ui::testapp;

    fn submit_steer(app: &mut App, text: &str) -> Option<crate::protocol::OutgoingRequest> {
        app.update(AppEvent::SteerTurn {
            session_id: "ses_1".to_owned(),
            text: text.to_owned(),
        })
        .into_iter()
        .filter_map(|command| match command {
            crate::command::AppCommand::Rpc(request) => Some(request),
            _ => None,
        })
        .find(|request| request.method == "turn.steer")
    }

    #[test]
    fn preview_is_cell_aware_and_flattens_multiline() {
        // Multiline steer text is flattened for the one-line dock preview
        // while the stored full text is preserved (nothing truncated in state).
        assert_eq!(flatten_preview("第一行\nsecond line"), "第一行 second line");

        // CJK-aware truncation never splits a full-width character and the
        // rendered queue row never exceeds the dock width in display columns.
        let mut app = testapp::live_turn(ThemeKind::Dark);
        let _ = submit_steer(
            &mut app,
            "第一行很长的宽字符队列文本塞满了屏幕还要继续塞直到触发省略号截断为止",
        );
        let width = 60u16;
        app.update(AppEvent::TerminalSize { width, height: 24 });
        let terminal = crate::ui::component_tests::draw(&app, width, 24);
        let rows = crate::ui::component_tests::buffer_lines(&terminal);
        let queue_row = rows
            .iter()
            .find(|row| row.contains("Steering"))
            .expect("queue row rendered above Working");
        // The long CJK text is truncated to the dock width with an ellipsis;
        // the boundary only ever falls on whole characters.
        assert!(
            queue_row.trim_end().ends_with('…'),
            "long CJK steer must be ellipsized: {queue_row:?}"
        );

        // Precise cell-aware truncation: a naive char-count would keep the
        // full-width char and overflow; we must stop BEFORE it.
        assert_eq!(truncate_to_width("abcd宽efg".to_owned(), 6), "abcd…");
        assert_eq!(
            crate::markdown::column_width(&truncate_to_width("abcd宽efg".to_owned(), 6)),
            5
        );
        assert_eq!(truncate_to_width("宽宽宽".to_owned(), 5), "宽宽…");
        assert_eq!(
            truncate_to_width("short".to_owned(), 6),
            "short",
            "no ellipsis when it fits"
        );
    }

    #[test]
    fn truncation_exact_fit_is_never_silently_truncated() {
        // EXACT fits must stay byte-for-byte unchanged (the old code always
        // dropped the last cell and appended an ellipsis).
        assert_eq!(truncate_to_width("abcd".to_owned(), 4), "abcd");
        assert_eq!(truncate_to_width("宽宽".to_owned(), 4), "宽宽");
        assert_eq!(truncate_to_width("ab".to_owned(), 2), "ab");
        // Width 0 can never show anything.
        assert_eq!(truncate_to_width("anything".to_owned(), 0), "");
        assert_eq!(truncate_to_width("".to_owned(), 0), "");
        // Over-width still ellipsizes exactly once at width-1 cells.
        assert_eq!(truncate_to_width("abcdefg".to_owned(), 6), "abcde…");
        assert_eq!(truncate_to_width("abcde".to_owned(), 4), "abc…");
        assert_eq!(truncate_to_width("宽宽宽".to_owned(), 5), "宽宽…");
    }

    #[test]
    fn truncation_is_grapheme_safe_and_never_splits_a_cluster() {
        // A ZWJ emoji family is ONE grapheme occupying 2 display cells.
        // Truncation keeps whole clusters: when only the first family fits,
        // the second must never be split into a dangling half-family (old
        // char-iteration DID split the ZWJ sequence).
        let family = "👨\u{200d}👩\u{200d}👧";
        assert_eq!(crate::markdown::column_width(family), 2);
        assert_eq!(truncate_to_width(format!("{family}X"), 2), "…");
        assert_eq!(
            truncate_to_width(format!("{family}{family}"), 3),
            format!("{family}…"),
            "the first whole cluster is kept, never a partial ZWJ family"
        );
    }

    #[test]
    fn normal_queued_steer_shows_withdrawal_hint_without_pause_prefix() {
        // The user reference shows the Alt+Up hint in a NORMAL (unpaused)
        // queue, not only when paused.
        let mut app = testapp::live_turn(ThemeKind::Dark);
        let steer = submit_steer(&mut app, "queued first").expect("steer issued");
        // Keep the item UNCONFIRMED-free and unsent but block its dispatch so
        // it stays withdrawable: pause instead lets us prove the unpaused
        // hint separately via a fresh admission that cannot auto-send.
        app.update(AppEvent::RpcSendFailed {
            id: steer.id,
            error: crate::rpc::RpcError::Closed,
        });
        // The send-failure path pauses; prove the unpaused variant too.
        let view = app.sessions.known.get_mut("ses_1").unwrap();
        view.steer_queue_paused = false;
        assert!(
            withdrawal_available(&app),
            "Alt+Up can withdraw the unsent item"
        );
        let hint = crate::ui::steer_queue::hint_label(&app).expect("hint shown");
        assert!(
            !hint.contains("paused"),
            "normal queue hint has no pause prefix"
        );
        assert!(hint.contains("⌥↑"), "normal queue still advertises Alt+Up");
        let terminal = crate::ui::component_tests::draw(&app, 80, 24);
        let rows = crate::ui::component_tests::buffer_lines(&terminal);
        assert!(
            rows.iter().any(|row| row.contains("⌥↑")),
            "normal queue renders the Alt+Up hint row"
        );
    }

    #[test]
    fn paused_queue_shows_withdrawal_hint_above_status() {
        let mut app = testapp::live_turn(ThemeKind::Dark);
        // A definite send failure restores the message into the unsent queue
        // and pauses it (0.2.4 D), which is the honest paused state.
        let steer = submit_steer(&mut app, "queued first").expect("steer issued");
        app.update(AppEvent::RpcSendFailed {
            id: steer.id,
            error: crate::rpc::RpcError::Closed,
        });
        let view = app.sessions.known.get("ses_1").unwrap();
        assert_eq!(
            view.steer_queue.len(),
            1,
            "message restored into unsent queue"
        );
        assert!(view.steer_queue_paused, "failure leaves the queue paused");
        assert!(
            withdrawal_available(&app),
            "Alt+Up can withdraw the unsent item"
        );
        let terminal = crate::ui::component_tests::draw(&app, 80, 24);
        let rows = crate::ui::component_tests::buffer_lines(&terminal);
        let working = rows
            .iter()
            .rposition(|row| row.contains("Working") || row.contains("Running"))
            .expect("busy status row");
        let above = &rows[working.saturating_sub(6)..working];
        assert!(
            above.iter().any(|row| row.contains("queued first")),
            "paused item stays visible above Working: {above:?}"
        );
        assert!(
            above.iter().any(|row| row.contains("⌥↑")),
            "paused state advertises the functional Alt+Up withdrawal hint: {above:?}"
        );
    }
}
