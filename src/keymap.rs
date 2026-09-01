//! The fixed key mapping (development spec 22): pure and deterministic, a
//! `KeyEvent` plus the current `&App` becomes one `Action` that only
//! `App::update` applies. No dynamic key configuration and no handler
//! registry (spec 22.4).

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::app::{App, ConnectionState};
use crate::state::selection::Dock;

/// One semantic action produced by the key map. `App::update` decides the
/// side effects; the map never touches the app mutably.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    None,
    /// Leave the TUI (q on Help/Fatal, second Ctrl+C, idle Ctrl+D).
    Quit,
    /// First Ctrl+C (composer content was present or the first press).
    FirstCtrlC,
    /// Ctrl+D.
    CtrlD,
    TypeChar(char),
    Newline,
    Backspace,
    Delete,
    CursorMove(EditorCursor),
    LineStart,
    LineEnd,
    WordDelete,
    Undo,
    Redo,
    Submit,
    HistoryPrev,
    HistoryNext,
    OpenHelp,
    OpenLogs,
    OpenSessions,
    OpenModel,
    OpenReasoning,
    ToggleTools,
    ToggleReasoning,
    /// Esc closed a panel, or aborted when a turn is running.
    CloseDock,
    CancelTurn,
    SelectorMove(i32),
    SelectorPage(i32),
    SelectorConfirm,
    SelectorChar(char),
    SelectorBackspace,
    SelectorClear,
    FieldStep(i32),
    FieldChar(char),
    FieldBackspace,
    FieldClear,
    FieldCursor(i32),
    FieldHome,
    FieldEnd,
    /// Scroll the focused panel (rows; negative = up; the active panel is
    /// decided by `App::update`).
    ScrollRows(i32),
    /// Scroll by one viewport.
    ScrollWindow(i32),
    ScrollTop,
    ScrollBottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorCursor {
    Left,
    Right,
    Up,
    Down,
}

fn ctrl(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL)
}

fn shift(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::SHIFT)
}

fn alt(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::ALT)
}

/// Maps a key to an action. `Release` is always ignored; one-shot global
/// shortcuts require `Press`, while repeated keystrokes are tolerated for
/// text entry and cursor movement (spec 22.3, 43.6).
pub fn map(app: &App, key: KeyEvent) -> Action {
    if key.kind == KeyEventKind::Release {
        return Action::None;
    }
    let press = key.kind == KeyEventKind::Press;
    let repeat = key.kind == KeyEventKind::Repeat;
    let typing = press || repeat;
    let running = app.active_view().is_some_and(|view| view.live.is_some());

    if press {
        // `q` quits only from the help panel and the fatal overlay; it is
        // an ordinary character everywhere else (spec 22.1).
        if let KeyCode::Char('q') = key.code {
            if !ctrl(&key)
                && !alt(&key)
                && (matches!(app.dock, Dock::Help)
                    || matches!(app.connection, ConnectionState::Failed(_)))
            {
                return Action::Quit;
            }
        }
        match key.code {
            KeyCode::F(1) if matches!(app.dock, Dock::Help) => return Action::CloseDock,
            KeyCode::F(1) => return Action::OpenHelp,
            KeyCode::Char('c') if ctrl(&key) => return Action::FirstCtrlC,
            KeyCode::Char('d') if ctrl(&key) => return Action::CtrlD,
            KeyCode::Char('r') if ctrl(&key) => return Action::OpenSessions,
            KeyCode::Char('l') if ctrl(&key) => return Action::OpenModel,
            KeyCode::Char('o') if ctrl(&key) => return Action::ToggleTools,
            KeyCode::Char('t') if ctrl(&key) => return Action::ToggleReasoning,
            KeyCode::Esc => {
                return match &app.dock {
                    Dock::Composer if running => Action::CancelTurn,
                    Dock::Composer => Action::None,
                    _ => Action::CloseDock,
                };
            }
            KeyCode::PageUp => {
                return if matches!(
                    app.dock,
                    Dock::SessionSelector(_)
                        | Dock::ModelSelector(_)
                        | Dock::ReasoningSelector(_)
                        | Dock::ProfileSelector(_)
                ) {
                    Action::SelectorPage(-1)
                } else {
                    Action::ScrollWindow(-1)
                };
            }
            KeyCode::PageDown => {
                return if matches!(
                    app.dock,
                    Dock::SessionSelector(_)
                        | Dock::ModelSelector(_)
                        | Dock::ReasoningSelector(_)
                        | Dock::ProfileSelector(_)
                ) {
                    Action::SelectorPage(1)
                } else {
                    Action::ScrollWindow(1)
                };
            }
            KeyCode::Home => {
                if ctrl(&key) {
                    return Action::ScrollTop;
                }
                return match &app.dock {
                    Dock::Composer => Action::LineStart,
                    Dock::NewSession(_) => Action::FieldHome,
                    _ => Action::ScrollTop,
                };
            }
            KeyCode::End => {
                if ctrl(&key) {
                    return Action::ScrollBottom;
                }
                return match &app.dock {
                    Dock::Composer => Action::LineEnd,
                    Dock::NewSession(_) => Action::FieldEnd,
                    _ => Action::ScrollBottom,
                };
            }
            _ => {}
        }
    }

    // Shift+Tab: reasoning selector from the composer, back to the form
    // from the reasoning selector, previous field in the form.
    if press && key.code == KeyCode::BackTab {
        return match &app.dock {
            Dock::ReasoningSelector(_) => Action::CloseDock,
            Dock::NewSession(_) => Action::FieldStep(-1),
            _ => Action::OpenReasoning,
        };
    }

    match &app.dock {
        Dock::Composer => composer_keys(key, press, repeat, typing, running),
        Dock::NewSession(_) => new_session_keys(key, press, typing),
        Dock::SessionSelector(_)
        | Dock::ModelSelector(_)
        | Dock::ReasoningSelector(_)
        | Dock::ProfileSelector(_) => selector_keys(key, press, typing),
        Dock::Help | Dock::Logs => panel_keys(key, press, typing),
    }
}

fn composer_keys(key: KeyEvent, press: bool, repeat: bool, typing: bool, running: bool) -> Action {
    // While a turn is running the editor is frozen: every editing key is
    // ignored (Esc already handled globally as cancel).
    if running {
        return Action::None;
    }
    if !typing {
        return Action::None;
    }
    let _ = repeat;
    let _ = press;
    match key.code {
        KeyCode::Char(c) => {
            if ctrl(&key) {
                return match c {
                    'a' => Action::LineStart,
                    'e' => Action::LineEnd,
                    'w' => Action::WordDelete,
                    'j' => Action::Newline,
                    'z' => Action::Undo,
                    'y' => Action::Redo,
                    _ => Action::None,
                };
            }
            Action::TypeChar(c)
        }
        KeyCode::Enter => {
            if shift(&key) {
                Action::Newline
            } else {
                Action::Submit
            }
        }
        KeyCode::Backspace => {
            if ctrl(&key) {
                Action::WordDelete
            } else {
                Action::Backspace
            }
        }
        KeyCode::Delete => Action::Delete,
        KeyCode::Left => Action::CursorMove(EditorCursor::Left),
        KeyCode::Right => Action::CursorMove(EditorCursor::Right),
        KeyCode::Up => {
            if alt(&key) {
                Action::HistoryPrev
            } else {
                Action::CursorMove(EditorCursor::Up)
            }
        }
        KeyCode::Down => {
            if alt(&key) {
                Action::HistoryNext
            } else {
                Action::CursorMove(EditorCursor::Down)
            }
        }
        KeyCode::Tab => Action::None,
        _ => Action::None,
    }
}

fn selector_keys(key: KeyEvent, press: bool, typing: bool) -> Action {
    match key.code {
        KeyCode::Enter if press => Action::SelectorConfirm,
        KeyCode::Up if typing => Action::SelectorMove(-1),
        KeyCode::Down if typing => Action::SelectorMove(1),
        KeyCode::PageUp if press => Action::SelectorPage(-1),
        KeyCode::PageDown if press => Action::SelectorPage(1),
        KeyCode::Char(c) if !ctrl(&key) && typing => Action::SelectorChar(c),
        KeyCode::Backspace if typing => Action::SelectorBackspace,
        KeyCode::Char('u') if ctrl(&key) && press => Action::SelectorClear,
        _ => Action::None,
    }
}

fn new_session_keys(key: KeyEvent, press: bool, typing: bool) -> Action {
    match key.code {
        KeyCode::Enter if press => Action::SelectorConfirm,
        KeyCode::Tab if press => Action::FieldStep(1),
        KeyCode::Char(c) if !ctrl(&key) && typing => Action::FieldChar(c),
        KeyCode::Backspace if typing => Action::FieldBackspace,
        KeyCode::Left if typing => Action::FieldCursor(-1),
        KeyCode::Right if typing => Action::FieldCursor(1),
        KeyCode::Char('u') if ctrl(&key) && press => Action::FieldClear,
        _ => Action::None,
    }
}

fn panel_keys(key: KeyEvent, press: bool, typing: bool) -> Action {
    let _ = typing;
    match key.code {
        KeyCode::Up if press => Action::ScrollRows(-1),
        KeyCode::Down if press => Action::ScrollRows(1),
        KeyCode::PageUp if press => Action::ScrollWindow(-1),
        KeyCode::PageDown if press => Action::ScrollWindow(1),
        _ => Action::None,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use crate::state::selection::Dock;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

    fn app() -> App {
        App::new(std::path::PathBuf::from("/ws"))
    }

    fn key(code: KeyCode, mods: KeyModifiers, kind: KeyEventKind) -> KeyEvent {
        KeyEvent::new_with_kind(code, mods, kind)
    }

    fn press(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        key(code, mods, KeyEventKind::Press)
    }

    fn char_press(c: char) -> KeyEvent {
        press(KeyCode::Char(c), KeyModifiers::empty())
    }

    fn ctrl(c: char) -> KeyEvent {
        press(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn global_shortcuts_map_regardless_of_the_dock() {
        let a = app();
        assert_eq!(map(&a, ctrl('r')), Action::OpenSessions);
        assert_eq!(map(&a, ctrl('l')), Action::OpenModel);
        assert_eq!(map(&a, ctrl('o')), Action::ToggleTools);
        assert_eq!(map(&a, ctrl('t')), Action::ToggleReasoning);
        assert_eq!(
            map(&a, press(KeyCode::PageUp, KeyModifiers::empty())),
            Action::ScrollWindow(-1)
        );
        assert_eq!(
            map(&a, press(KeyCode::F(1), KeyModifiers::empty())),
            Action::OpenHelp
        );
        let mut a = a;
        a.dock = Dock::Help;
        assert_eq!(
            map(&a, press(KeyCode::F(1), KeyModifiers::empty())),
            Action::CloseDock
        );
    }

    #[test]
    fn release_events_are_ignored_but_repeats_type() {
        let a = app();
        assert_eq!(
            map(
                &a,
                key(
                    KeyCode::Char('x'),
                    KeyModifiers::empty(),
                    KeyEventKind::Release
                )
            ),
            Action::None
        );
        assert_eq!(
            map(
                &a,
                key(
                    KeyCode::Char('x'),
                    KeyModifiers::empty(),
                    KeyEventKind::Repeat
                )
            ),
            Action::TypeChar('x')
        );
        // One-shot shortcuts need a real press, not a repeat.
        assert_ne!(
            map(
                &a,
                key(
                    KeyCode::Char('r'),
                    KeyModifiers::CONTROL,
                    KeyEventKind::Repeat
                )
            ),
            Action::OpenSessions
        );
    }

    #[test]
    fn q_is_a_character_in_the_composer_but_quits_from_help() {
        let mut a = app();
        assert_eq!(map(&a, char_press('q')), Action::TypeChar('q'));
        a.dock = Dock::Help;
        assert_eq!(map(&a, char_press('q')), Action::Quit);
    }

    #[test]
    fn ctrl_c_and_ctrl_d_take_the_dedicated_actions() {
        let a = app();
        assert_eq!(map(&a, ctrl('c')), Action::FirstCtrlC);
        assert_eq!(map(&a, ctrl('d')), Action::CtrlD);
    }

    #[test]
    fn selector_pages_are_selection_pages_not_transcript_scroll() {
        let mut a = app();
        a.dock = Dock::ModelSelector(crate::state::selection::SelectorState::new(
            crate::state::selection::SelectorKind::Model,
        ));
        assert_eq!(
            map(&a, press(KeyCode::PageUp, KeyModifiers::empty())),
            Action::SelectorPage(-1)
        );
        assert_eq!(
            map(&a, press(KeyCode::PageDown, KeyModifiers::empty())),
            Action::SelectorPage(1)
        );
    }

    #[test]
    fn esc_is_contextual() {
        let mut a = app();
        assert_eq!(
            map(&a, press(KeyCode::Esc, KeyModifiers::empty())),
            Action::None
        );
        a.dock = Dock::ModelSelector(crate::state::selection::SelectorState::new(
            crate::state::selection::SelectorKind::Model,
        ));
        assert_eq!(
            map(&a, press(KeyCode::Esc, KeyModifiers::empty())),
            Action::CloseDock
        );
    }

    #[test]
    fn composer_keys_submit_newline_history_undo_and_word_delete() {
        let a = app();
        assert_eq!(
            map(&a, press(KeyCode::Enter, KeyModifiers::empty())),
            Action::Submit
        );
        assert_eq!(
            map(&a, press(KeyCode::Enter, KeyModifiers::SHIFT)),
            Action::Newline
        );
        assert_eq!(map(&a, ctrl('j')), Action::Newline);
        assert_eq!(map(&a, ctrl('a')), Action::LineStart);
        assert_eq!(map(&a, ctrl('e')), Action::LineEnd);
        assert_eq!(map(&a, ctrl('w')), Action::WordDelete);
        assert_eq!(map(&a, ctrl('z')), Action::Undo);
        assert_eq!(map(&a, ctrl('y')), Action::Redo);
        assert_eq!(map(&a, char_press('中')), Action::TypeChar('中'));
        assert_eq!(
            map(&a, press(KeyCode::Backspace, KeyModifiers::empty())),
            Action::Backspace
        );
        assert_eq!(
            map(&a, press(KeyCode::Up, KeyModifiers::ALT)),
            Action::HistoryPrev
        );
        assert_eq!(
            map(&a, press(KeyCode::Down, KeyModifiers::ALT)),
            Action::HistoryNext
        );
    }
}
