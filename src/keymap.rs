//! The fixed key mapping (development spec 22): pure and deterministic, a
//! `KeyEvent` plus the current `&App` becomes one `Action` that only
//! `App::update` applies. No dynamic key configuration and no handler
//! registry (spec 22.4).

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::app::{App, ConnectionState};
use crate::state::selection::{Dock, SessionPanelMode};

/// One semantic action produced by the key map. `App::update` decides the
/// side effects; the map never touches the app mutably.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    DetailActivate,
    WorkspaceType(char),
    WorkspaceBackspace,
    WorkspaceDelete,
    WorkspaceCursor(i32),
    WorkspaceHome,
    WorkspaceEnd,
    WorkspaceClear,
    WorkspaceField,
    WorkspaceCase,
    WorkspaceMove(i32),
    WorkspaceSelect(bool),
    WorkspaceMore(bool),
    FileMore,
    PreviewReference,
    DetailFocus,
    DetailEscape,
    DetailTab(i32),
    DetailScroll(i32),
    DetailEnd,
    DetailRefresh,
    DetailCopy,
    ClearSelection,
    None,
    /// Leave the TUI (q on Help/Fatal, second Ctrl+C, idle Ctrl+D).
    Quit,
    /// First Ctrl+C (composer content was present or the first press).
    FirstCtrlC,
    /// Ctrl+D.
    CtrlD,
    TypeChar(char),
    CompletionMove(i32),
    CompletionAccept,
    CompletionEnter,
    CompletionOpen,
    CompletionFilter(char),
    CompletionFilterBackspace,
    CompletionFilterClear,
    CompletionCancel,
    Newline,
    Backspace,
    Delete,
    CursorMove(EditorCursor),
    LineStart,
    LineEnd,
    WordDelete,
    DeleteToLineStart,
    DeleteToLineEnd,
    Undo,
    Redo,
    Submit,
    HistoryPrev,
    HistoryNext,
    OpenHelp,
    /// Edit the exact Composer draft with the configured external editor.
    OpenExternalEditor,
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
    OpenNewSession,
    RefreshSessions,
    SessionRename,
    SessionClose,
    SessionDelete,
    /// Open the selected closed session read-only through `session.read`
    /// only (spec §10.1).
    SessionBrowse,
    /// Explicitly continue the read-only session (Ctrl+G): open it without
    /// sending anything.
    SessionContinue,
    /// Toggle the session selector between this workspace and all projects.
    SessionScopeToggle,
    /// Search panel: edit the one-line query.
    SearchTypeChar(char),
    SearchBackspace,
    SearchDelete,
    SearchCursor(i32),
    SearchHome,
    SearchEnd,
    SearchClear,
    /// Search panel: move the result cursor by `delta` matches.
    SearchMove(i32),
    /// Search panel Enter: start a generation, then jump to the match.
    SearchConfirm,
    /// Search panel `n`/`p`: move and jump.
    SearchStep(i32),
    /// Search panel Ctrl+A: switch loaded content and full session.
    SearchScopeToggle,
    /// Search panel `s`: stop the running scan.
    SearchStop,
    /// Search panel Esc: leave the result list, then close the search.
    SearchEscape,
    /// Export form: edit the local target path.
    ExportTypeChar(char),
    ExportBackspace,
    ExportDelete,
    ExportCursor(i32),
    ExportHome,
    ExportEnd,
    ExportClear,
    /// Export form Enter: validate and start the export.
    ExportSubmit,
    /// Export form Ctrl+T/Ctrl+P/Ctrl+U: optional content and the separate
    /// unsaved-turn choice.
    ExportToggleThinking,
    ExportToggleTool,
    ExportToggleUnsaved,
    /// Export form Ctrl+Y: the explicit overwrite confirmation.
    ExportToggleOverwrite,
    /// Export form Ctrl+R: stream oversized items as raw sanitized JSON.
    ExportToggleRaw,
    /// Export form Esc: close the form (cancelling a running export).
    ExportEscape,
    /// Settings form actions.
    SettingsTypeChar(char),
    SettingsBackspace,
    SettingsDelete,
    SettingsCursor(i32),
    SettingsHome,
    SettingsEnd,
    SettingsNewline,
    SettingsClear,
    SettingsFieldStep(i32),
    SettingsToggle,
    SettingsSubmit,
    SettingsEscape,
    SessionDeleteToggle,
    SessionRenameChar(char),
    SessionRenameBackspace,
    SessionRenameDelete,
    SessionRenameCursor(i32),
    SessionRenameClear,
    SessionRenameHome,
    SessionRenameEnd,
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
    let cancellable = app.active_view().is_some_and(|view| {
        view.is_preparing()
            || view.live.as_ref().is_some_and(|live| {
                (!live.waiting
                    || view.state.as_ref().is_some_and(|state| {
                        state.status == crate::protocol::SessionStatusWire::WaitingForInput
                    }))
                    && view.state.as_ref().is_none_or(|state| {
                        matches!(
                            state.status,
                            crate::protocol::SessionStatusWire::Idle
                                | crate::protocol::SessionStatusWire::Running
                                | crate::protocol::SessionStatusWire::WaitingForInput
                        )
                    })
            })
    });

    if let Dock::SessionSelector(state) = &app.dock {
        if matches!(&state.mode, SessionPanelMode::Rename { .. }) {
            return session_rename_keys(key, press, typing);
        }
    }

    // The search panel owns the keyboard while it is open. It adds no global
    // shortcut: every binding here is local to this panel, and Esc leaves the
    // search before anything can cancel a turn (spec §17).
    if app.workspace_browser().is_some() {
        return match key.code {
            KeyCode::Esc if press => Action::CloseDock,
            KeyCode::Up => Action::WorkspaceMove(-1),
            KeyCode::Down => Action::WorkspaceMove(1),
            KeyCode::PageUp => Action::WorkspaceMove(-5),
            KeyCode::PageDown => Action::WorkspaceMove(5),
            KeyCode::Tab | KeyCode::BackTab if press => Action::WorkspaceField,
            KeyCode::F(4) if press => Action::WorkspaceSelect(true),
            KeyCode::F(5) if press => Action::WorkspaceMore(true),
            KeyCode::Enter if press => Action::WorkspaceSelect(false),
            KeyCode::Char('n') if ctrl(&key) && press => Action::WorkspaceMore(false),
            KeyCode::Char('i') if ctrl(&key) && press => Action::WorkspaceCase,
            KeyCode::Backspace => Action::WorkspaceBackspace,
            KeyCode::Delete => Action::WorkspaceDelete,
            KeyCode::Left => Action::WorkspaceCursor(-1),
            KeyCode::Right => Action::WorkspaceCursor(1),
            KeyCode::Home => Action::WorkspaceHome,
            KeyCode::End => Action::WorkspaceEnd,
            KeyCode::Char('a') if ctrl(&key) => Action::WorkspaceHome,
            KeyCode::Char('e') if ctrl(&key) => Action::WorkspaceEnd,
            KeyCode::Char('u') if ctrl(&key) && press => Action::WorkspaceClear,
            KeyCode::Char(c) if typing && !ctrl(&key) && !alt(&key) => Action::WorkspaceType(c),
            _ => Action::None,
        };
    }
    if let Dock::Search(state) = &app.dock {
        return search_keys(key, press, typing, state.mode);
    }

    // The export form is modal too. Its keys are local: typing edits the
    // target, and the toggles use Ctrl chords so a path can never trigger one.
    if let Dock::Export(form) = &app.dock {
        return export_keys(key, press, typing, form.running());
    }
    if let Dock::Settings(state) = &app.dock {
        return settings_keys(key, press, typing, state.submitting);
    }

    // Completion controls must not shadow explicit editor chords, even when
    // an opaque slash-command argument still has a compact object row.
    if typing
        && matches!(app.dock, Dock::Composer)
        && app.focused_region() == crate::state::panels::Focus::Editor
    {
        match key.code {
            KeyCode::Enter if shift(&key) => return Action::Newline,
            KeyCode::Up if alt(&key) => return Action::HistoryPrev,
            KeyCode::Down if alt(&key) => return Action::HistoryNext,
            _ => {}
        }
    }

    if typing
        && !matches!(app.connection, ConnectionState::Failed(_))
        && matches!(app.dock, Dock::Composer)
        && app.focused_region() == crate::state::panels::Focus::Editor
        && app
            .slash_completion
            .as_ref()
            .is_some_and(|completion| completion.popup.is_some())
    {
        match key.code {
            KeyCode::Char(c) if !ctrl(&key) && !alt(&key) => return Action::CompletionFilter(c),
            KeyCode::Backspace => return Action::CompletionFilterBackspace,
            KeyCode::Char('u' | 'k') if ctrl(&key) => return Action::CompletionFilterClear,
            KeyCode::Enter if press => return Action::CompletionEnter,
            KeyCode::Tab if press => return Action::CompletionAccept,
            KeyCode::Esc => return Action::CompletionCancel,
            _ => {}
        }
    }

    if typing && matches!(app.dock, Dock::Composer) {
        if press && key.code == KeyCode::F(1) {
            return Action::OpenHelp;
        }
        if press
            && app.focused_region() == crate::state::panels::Focus::Editor
            && app.slash_completion.is_some()
        {
            match key.code {
                KeyCode::PageUp => return Action::CompletionMove(-5),
                KeyCode::PageDown => return Action::CompletionMove(5),
                _ => {}
            }
        }
        if press
            && key.code == KeyCode::Char('g')
            && ctrl(&key)
            && app.focused_region() == crate::state::panels::Focus::Editor
        {
            // A cold read-only session keeps its explicit Continue binding.
            return if app.active_view().is_some_and(|view| view.browsing) {
                Action::SessionContinue
            } else {
                Action::OpenExternalEditor
            };
        }
        if key.code == KeyCode::F(6) {
            return if press {
                Action::DetailFocus
            } else {
                Action::None
            };
        }
        if key.code == KeyCode::Esc && app.has_text_selection() {
            return Action::ClearSelection;
        }
        if key.code == KeyCode::Esc
            && app.focused_region() == crate::state::panels::Focus::Editor
            && app.slash_completion.is_some()
        {
            return Action::CompletionCancel;
        }
        if key.code == KeyCode::F(4)
            && press
            && app.focused_region() == crate::state::panels::Focus::Editor
        {
            return Action::PreviewReference;
        }
        if app.has_main_detail() {
            if key.code == KeyCode::Esc {
                return Action::DetailEscape;
            }
            if app.focused_region() == crate::state::panels::Focus::Main {
                return match key.code {
                    KeyCode::Enter
                        if press && (app.changes().is_some() || app.context_panel().is_some()) =>
                    {
                        Action::DetailActivate
                    }
                    KeyCode::Tab if press => Action::DetailTab(1),
                    KeyCode::BackTab if press => Action::DetailTab(-1),
                    KeyCode::PageUp => {
                        Action::DetailScroll(-(i32::from(app.main_body_area().height).max(1)))
                    }
                    KeyCode::PageDown => {
                        Action::DetailScroll(i32::from(app.main_body_area().height).max(1))
                    }
                    KeyCode::Up => Action::DetailScroll(-1),
                    KeyCode::Down => Action::DetailScroll(1),
                    KeyCode::End => Action::DetailEnd,
                    KeyCode::F(5) => Action::DetailRefresh,
                    KeyCode::Char('n')
                        if ctrl(&key)
                            && (app.file_preview().is_some() || app.changes().is_some()) =>
                    {
                        Action::FileMore
                    }
                    KeyCode::Char('c') if ctrl(&key) && shift(&key) => Action::DetailCopy,
                    _ => Action::None,
                };
            }
            match key.code {
                KeyCode::PageUp => return Action::CursorMove(EditorCursor::Up),
                KeyCode::PageDown => return Action::CursorMove(EditorCursor::Down),
                KeyCode::End => return Action::LineEnd,
                _ => {}
            }
        } else if app.focused_region() == crate::state::panels::Focus::Main {
            return match key.code {
                KeyCode::PageUp => Action::ScrollWindow(-1),
                KeyCode::PageDown => Action::ScrollWindow(1),
                KeyCode::Up => Action::ScrollRows(-1),
                KeyCode::Down => Action::ScrollRows(1),
                KeyCode::End => Action::ScrollBottom,
                KeyCode::Esc if press && cancellable => Action::CancelTurn,
                _ => Action::None,
            };
        }
    }
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
        if matches!(&app.dock, Dock::SessionSelector(state) if matches!(&state.mode, SessionPanelMode::Browse))
        {
            match key.code {
                KeyCode::F(2) => return Action::SessionRename,
                KeyCode::F(5) => return Action::RefreshSessions,
                KeyCode::Delete => return Action::SessionDelete,
                KeyCode::Char('d') if ctrl(&key) => return Action::SessionDelete,
                KeyCode::Char('w') if ctrl(&key) => return Action::SessionClose,
                KeyCode::Char('b') if ctrl(&key) => return Action::SessionBrowse,
                KeyCode::Char('g') if ctrl(&key) => return Action::SessionContinue,
                KeyCode::Char('a') if ctrl(&key) => return Action::SessionScopeToggle,
                _ => {}
            }
        }
        if matches!(
            &app.dock,
            Dock::SessionSelector(state)
                if matches!(&state.mode, SessionPanelMode::ConfirmDelete { .. })
        ) && matches!(
            key.code,
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Left | KeyCode::Right
        ) {
            return Action::SessionDeleteToggle;
        }
        match key.code {
            KeyCode::F(1) if matches!(app.dock, Dock::Help) => return Action::CloseDock,
            KeyCode::F(1) => return Action::OpenHelp,
            KeyCode::Char('c') if ctrl(&key) => return Action::FirstCtrlC,
            KeyCode::Char('d') if ctrl(&key) => return Action::CtrlD,
            KeyCode::Char('n') if ctrl(&key) => return Action::OpenNewSession,
            KeyCode::Char('r') if ctrl(&key) => return Action::OpenSessions,
            KeyCode::Char('l') if ctrl(&key) => return Action::OpenModel,
            KeyCode::Char('g') if ctrl(&key) => return Action::SessionContinue,
            KeyCode::Char('o') if ctrl(&key) => return Action::ToggleTools,
            KeyCode::Char('t') if ctrl(&key) => return Action::ToggleReasoning,
            KeyCode::Esc => {
                if matches!(app.dock, Dock::Composer) && app.slash_completion.is_some() {
                    return Action::CompletionCancel;
                }
                return match &app.dock {
                    Dock::Composer if cancellable => Action::CancelTurn,
                    Dock::Composer => Action::None,
                    _ => Action::CloseDock,
                };
            }
            KeyCode::PageUp => {
                if matches!(app.dock, Dock::Composer) && app.slash_completion.is_some() {
                    return Action::CompletionMove(-5);
                }
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
                if matches!(app.dock, Dock::Composer) && app.slash_completion.is_some() {
                    return Action::CompletionMove(5);
                }
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

    if matches!(app.dock, Dock::Composer) && app.slash_completion.is_some() {
        if typing && key.code == KeyCode::Up {
            return Action::CompletionMove(-1);
        }
        if typing && key.code == KeyCode::Down {
            return Action::CompletionMove(1);
        }
        if press && key.code == KeyCode::Char(' ') && ctrl(&key) {
            return Action::CompletionOpen;
        }
        if press && key.code == KeyCode::Tab {
            return Action::CompletionAccept;
        }
        if press && key.code == KeyCode::Enter {
            return Action::CompletionEnter;
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
        Dock::Composer => composer_keys(key, press, repeat, typing),
        Dock::NewSession(_) => new_session_keys(key, press, typing),
        Dock::SessionSelector(_)
        | Dock::ModelSelector(_)
        | Dock::ReasoningSelector(_)
        | Dock::ProfileSelector(_) => selector_keys(key, press, typing),
        Dock::Help | Dock::Logs => panel_keys(key, press, typing),
        // The search, export, and settings panels are handled before this
        // match (they own the keyboard while open).
        Dock::Search(_) | Dock::Export(_) | Dock::Settings(_) | Dock::Workspace(_) => Action::None,
    }
}

fn composer_keys(key: KeyEvent, press: bool, repeat: bool, typing: bool) -> Action {
    // The reducer rejects ordinary prompt/steer submissions in blocked or
    // finishing states, but the editor remains usable for local slash
    // commands such as `/reload` and `/close confirm`.
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
                    'u' => Action::DeleteToLineStart,
                    'k' => Action::DeleteToLineEnd,
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

fn session_rename_keys(key: KeyEvent, press: bool, typing: bool) -> Action {
    match key.code {
        KeyCode::Enter if press => Action::SelectorConfirm,
        KeyCode::Esc if press => Action::CloseDock,
        KeyCode::Char('a') if ctrl(&key) && press => Action::SessionRenameHome,
        KeyCode::Char('e') if ctrl(&key) && press => Action::SessionRenameEnd,
        KeyCode::Char('u') if ctrl(&key) && press => Action::SessionRenameClear,
        KeyCode::Char(c) if !ctrl(&key) && typing => Action::SessionRenameChar(c),
        KeyCode::Backspace if typing => Action::SessionRenameBackspace,
        KeyCode::Delete if typing => Action::SessionRenameDelete,
        KeyCode::Left if typing => Action::SessionRenameCursor(-1),
        KeyCode::Right if typing => Action::SessionRenameCursor(1),
        KeyCode::Home if press => Action::SessionRenameHome,
        KeyCode::End if press => Action::SessionRenameEnd,
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
/// Panel-local keys for the conversation search (spec §17.1/§17.2). Typing
/// always edits the query; `n`/`p` and the stop/scope keys are result-list
/// actions so a letter can never be swallowed while the user types.
fn search_keys(
    key: KeyEvent,
    press: bool,
    typing: bool,
    mode: crate::state::search::SearchPanelMode,
) -> Action {
    use crate::state::search::SearchPanelMode;
    if !(press || typing && matches!(mode, SearchPanelMode::Input)) {
        return Action::None;
    }
    let results = matches!(mode, SearchPanelMode::Results);
    match key.code {
        KeyCode::Esc => Action::SearchEscape,
        KeyCode::Enter => Action::SearchConfirm,
        KeyCode::Backspace => Action::SearchBackspace,
        KeyCode::Delete => Action::SearchDelete,
        KeyCode::Left => Action::SearchCursor(-1),
        KeyCode::Right => Action::SearchCursor(1),
        KeyCode::Up => Action::SearchMove(-1),
        KeyCode::Down => Action::SearchMove(1),
        KeyCode::PageUp => Action::SearchMove(-10),
        KeyCode::PageDown => Action::SearchMove(10),
        KeyCode::Home if results => Action::SearchMove(-1000),
        KeyCode::End if results => Action::SearchMove(1000),
        KeyCode::Home => Action::SearchHome,
        KeyCode::End => Action::SearchEnd,
        KeyCode::Char('e') if ctrl(&key) && !results => Action::SearchEnd,
        KeyCode::Char('u') if ctrl(&key) => Action::SearchClear,
        KeyCode::Char('a') if ctrl(&key) => Action::SearchScopeToggle,
        KeyCode::Char('n') if results => Action::SearchStep(1),
        KeyCode::Char('p') if results => Action::SearchStep(-1),
        KeyCode::Char('s') if results => Action::SearchStop,
        KeyCode::Char(c) if !ctrl(&key) && !alt(&key) => Action::SearchTypeChar(c),
        _ => Action::None,
    }
}

/// Panel-local keys for the export form (spec §17.4). Typing always edits the
/// target; every toggle requires Ctrl so a path character can never change the
/// export contract.
fn export_keys(key: KeyEvent, press: bool, typing: bool, running: bool) -> Action {
    if !(press || typing) {
        return Action::None;
    }
    match key.code {
        KeyCode::Esc => Action::ExportEscape,
        KeyCode::Enter => Action::ExportSubmit,
        KeyCode::Backspace => Action::ExportBackspace,
        KeyCode::Delete if !running => Action::ExportDelete,
        KeyCode::Left if !running => Action::ExportCursor(-1),
        KeyCode::Right if !running => Action::ExportCursor(1),
        KeyCode::Home if !running => Action::ExportHome,
        KeyCode::End if !running => Action::ExportEnd,
        KeyCode::Char('a') if ctrl(&key) && !running => Action::ExportHome,
        KeyCode::Char('e') if ctrl(&key) && !running => Action::ExportEnd,
        KeyCode::Char('u') if ctrl(&key) => Action::ExportClear,
        KeyCode::Char('t') if ctrl(&key) && !running => Action::ExportToggleThinking,
        KeyCode::Char('p') if ctrl(&key) && !running => Action::ExportToggleTool,
        KeyCode::Char('n') if ctrl(&key) && !running => Action::ExportToggleUnsaved,
        KeyCode::Char('y') if ctrl(&key) && !running => Action::ExportToggleOverwrite,
        KeyCode::Char('r') if ctrl(&key) && !running => Action::ExportToggleRaw,
        KeyCode::Char(c) if !ctrl(&key) && !alt(&key) && !running => Action::ExportTypeChar(c),
        _ => Action::None,
    }
}

fn settings_keys(key: KeyEvent, press: bool, typing: bool, submitting: bool) -> Action {
    if submitting {
        return if press && key.code == KeyCode::Esc {
            Action::SettingsEscape
        } else {
            Action::None
        };
    }
    if !(press || typing) {
        return Action::None;
    }
    match key.code {
        KeyCode::Esc => Action::SettingsEscape,
        KeyCode::Enter if press => Action::SettingsToggle,
        KeyCode::Tab if press => Action::SettingsFieldStep(1),
        KeyCode::BackTab if press => Action::SettingsFieldStep(-1),
        KeyCode::Up if press => Action::SettingsFieldStep(-1),
        KeyCode::Down if press => Action::SettingsFieldStep(1),
        KeyCode::Backspace => Action::SettingsBackspace,
        KeyCode::Delete => Action::SettingsDelete,
        KeyCode::Left => Action::SettingsCursor(-1),
        KeyCode::Right => Action::SettingsCursor(1),
        KeyCode::Home => Action::SettingsHome,
        KeyCode::End => Action::SettingsEnd,
        KeyCode::Char('a') if ctrl(&key) => Action::SettingsHome,
        KeyCode::Char('e') if ctrl(&key) => Action::SettingsEnd,
        KeyCode::Char('j') if ctrl(&key) => Action::SettingsNewline,
        KeyCode::Char('u') if ctrl(&key) => Action::SettingsClear,
        KeyCode::Char('s') if ctrl(&key) && press => Action::SettingsSubmit,
        KeyCode::Char(c) if !ctrl(&key) && !alt(&key) => Action::SettingsTypeChar(c),
        _ => Action::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use crate::state::selection::{Dock, SessionPanelMode, SessionSelectorState};
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
    fn external_editor_shortcut_is_composer_editor_only_and_one_shot() {
        use crate::state::panels::Focus;
        let mut a = app();
        assert_eq!(map(&a, ctrl('g')), Action::OpenExternalEditor);
        for kind in [KeyEventKind::Repeat, KeyEventKind::Release] {
            assert_eq!(
                map(&a, key(KeyCode::Char('g'), KeyModifiers::CONTROL, kind)),
                Action::None
            );
        }
        a.focus = Focus::Main;
        assert_eq!(map(&a, ctrl('g')), Action::None);
        a.focus = Focus::Editor;
        a.dock = Dock::SessionSelector(SessionSelectorState::new(None));
        assert_eq!(map(&a, ctrl('g')), Action::SessionContinue);
        for dock in [
            Dock::Help,
            Dock::Logs,
            Dock::ModelSelector(crate::state::selection::SelectorState::new(
                crate::state::selection::SelectorKind::Model,
            )),
            Dock::Settings(crate::state::settings::SettingsState::from_config(
                &Default::default(),
            )),
        ] {
            a.dock = dock;
            assert_ne!(map(&a, ctrl('g')), Action::OpenExternalEditor);
        }
    }

    #[test]
    fn settings_text_edit_keys_remain_local() {
        let mut a = app();
        a.dock = Dock::Settings(crate::state::settings::SettingsState::from_config(
            &crate::config::TuiConfig::default(),
        ));
        for (code, action) in [
            (KeyCode::Left, Action::SettingsCursor(-1)),
            (KeyCode::Right, Action::SettingsCursor(1)),
            (KeyCode::Home, Action::SettingsHome),
            (KeyCode::End, Action::SettingsEnd),
            (KeyCode::Delete, Action::SettingsDelete),
        ] {
            assert_eq!(map(&a, press(code, KeyModifiers::empty())), action);
        }
        assert_eq!(map(&a, ctrl('a')), Action::SettingsHome);
        assert_eq!(map(&a, ctrl('e')), Action::SettingsEnd);
        assert_eq!(map(&a, ctrl('j')), Action::SettingsNewline);
        assert_eq!(map(&a, ctrl('s')), Action::SettingsSubmit);
    }

    #[test]
    fn detail_editor_pages_completion_while_main_scrolls_and_f1_opens_help() {
        use crate::state::panels::{ContextState, Focus, MainView};
        let mut a = app();
        a.main_view = MainView::Context(Box::new(ContextState {
            session: "synthetic".into(),
            epoch: 1,
            generation: 1,
            conversation_scroll: None,
            offset: 0,
            action: 0,
            scrollbar_grab: None,
        }));
        a.slash_completion = Some(crate::app::SlashCompletionState {
            popup: None,
            source_revision: a.composer.editor_revision(),
            session_owner: a.sessions.active.clone(),
            group: None,
            argument_command: None,
            filter: String::new(),
            start: 0,
            end: 1,
            items: vec!["/help".into()],
            selected: 0,
        });
        a.focus = Focus::Editor;
        assert_eq!(map(&a, ctrl('g')), Action::OpenExternalEditor);
        assert_eq!(
            map(&a, press(KeyCode::PageUp, KeyModifiers::NONE)),
            Action::CompletionMove(-5)
        );
        assert_eq!(
            map(&a, press(KeyCode::PageDown, KeyModifiers::NONE)),
            Action::CompletionMove(5)
        );
        a.focus = Focus::Main;
        assert_eq!(map(&a, ctrl('g')), Action::None);
        assert!(
            matches!(map(&a, press(KeyCode::PageDown, KeyModifiers::NONE)), Action::DetailScroll(delta) if delta > 0)
        );
        assert_eq!(
            map(&a, press(KeyCode::F(1), KeyModifiers::NONE)),
            Action::OpenHelp
        );
    }

    #[test]
    fn slash_completion_page_keys_move_candidates_before_transcript() {
        let mut a = app();
        a.slash_completion = Some(crate::app::SlashCompletionState {
            popup: None,
            source_revision: a.composer.editor_revision(),
            session_owner: a.sessions.active.clone(),
            group: None,
            argument_command: None,
            filter: String::new(),
            start: 0,
            end: 1,
            items: vec!["/help".into()],
            selected: 0,
        });
        assert_eq!(
            map(&a, press(KeyCode::PageUp, KeyModifiers::empty())),
            Action::CompletionMove(-5)
        );
        assert_eq!(
            map(&a, press(KeyCode::PageDown, KeyModifiers::empty())),
            Action::CompletionMove(5)
        );
        assert_eq!(
            map(&a, press(KeyCode::Up, KeyModifiers::empty())),
            Action::CompletionMove(-1)
        );
        assert_eq!(
            map(&a, press(KeyCode::Tab, KeyModifiers::empty())),
            Action::CompletionAccept
        );
        assert_eq!(
            map(&a, press(KeyCode::Enter, KeyModifiers::empty())),
            Action::CompletionEnter
        );
        assert_eq!(
            map(&a, press(KeyCode::Esc, KeyModifiers::empty())),
            Action::CompletionCancel
        );
    }

    #[test]
    fn search_scope_toggle_is_available_for_no_hit_input_and_results() {
        for mode in [
            crate::state::search::SearchPanelMode::Input,
            crate::state::search::SearchPanelMode::Results,
        ] {
            let mut a = app();
            a.dock = Dock::Search(crate::state::search::SearchPanelState {
                mode,
                ..Default::default()
            });
            assert_eq!(map(&a, ctrl('a')), Action::SearchScopeToggle);
            assert_eq!(map(&a, char_press('a')), Action::SearchTypeChar('a'));
        }
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
    fn line_delete_keys_preserve_panel_bindings_and_ignore_releases() {
        let mut a = app();
        for character in ['u', 'k'] {
            assert_eq!(
                map(
                    &a,
                    key(
                        KeyCode::Char(character),
                        KeyModifiers::CONTROL,
                        KeyEventKind::Release
                    )
                ),
                Action::None
            );
        }
        assert_eq!(new_session_keys(ctrl('u'), true, true), Action::FieldClear);
        assert_eq!(
            session_rename_keys(ctrl('u'), true, true),
            Action::SessionRenameClear
        );
        assert_eq!(
            search_keys(
                ctrl('u'),
                true,
                true,
                crate::state::search::SearchPanelMode::Input
            ),
            Action::SearchClear
        );
        assert_eq!(
            export_keys(ctrl('u'), true, true, false),
            Action::ExportClear
        );
        assert_eq!(
            settings_keys(ctrl('u'), true, true, false),
            Action::SettingsClear
        );
        a.dock = Dock::Workspace(Box::new(crate::state::workspace::WorkspaceBrowser::new(
            crate::state::workspace::BrowserKind::Files,
            "ses_1".into(),
            0,
            0,
            std::time::Instant::now(),
        )));
        assert_eq!(map(&a, ctrl('u')), Action::WorkspaceClear);
        assert_eq!(map(&a, ctrl('k')), Action::None);
        a.dock = Dock::Help;
        assert_eq!(map(&a, ctrl('u')), Action::None);
        assert_eq!(map(&a, ctrl('k')), Action::None);
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
        assert_eq!(map(&a, ctrl('u')), Action::DeleteToLineStart);
        assert_eq!(map(&a, ctrl('k')), Action::DeleteToLineEnd);
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

    #[test]
    fn session_panel_shortcuts_do_not_consume_filter_letters() {
        let mut app = app();
        app.dock = Dock::SessionSelector(SessionSelectorState::new(Some("ses_1".into())));
        assert_eq!(
            map(&app, press(KeyCode::F(2), KeyModifiers::empty())),
            Action::SessionRename
        );
        assert_eq!(
            map(&app, press(KeyCode::F(5), KeyModifiers::empty())),
            Action::RefreshSessions
        );
        assert_eq!(
            map(&app, press(KeyCode::Char('w'), KeyModifiers::CONTROL)),
            Action::SessionClose
        );
        assert_eq!(
            map(&app, press(KeyCode::Delete, KeyModifiers::empty())),
            Action::SessionDelete
        );
        assert_eq!(map(&app, ctrl('d')), Action::SessionDelete);
        assert_eq!(map(&app, char_press('r')), Action::SelectorChar('r'));

        if let Dock::SessionSelector(state) = &mut app.dock {
            state.mode = SessionPanelMode::Rename {
                draft: "old".into(),
                cursor: 3,
                submitting: false,
            };
        }
        assert_eq!(map(&app, char_press('中')), Action::SessionRenameChar('中'));
        assert_eq!(
            map(&app, press(KeyCode::Backspace, KeyModifiers::empty())),
            Action::SessionRenameBackspace
        );
        assert_eq!(
            map(&app, press(KeyCode::Enter, KeyModifiers::empty())),
            Action::SelectorConfirm
        );
    }
}
