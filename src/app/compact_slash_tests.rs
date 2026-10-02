use super::*;
use crate::ui::testapp;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

fn app() -> App {
    let mut app = testapp::open_empty(ThemeKind::Dark, "ses_1", None, "high");
    app.catalogs.models = testapp::standard_catalog()
        .0
        .into_iter()
        .map(|value| serde_json::from_value(value).unwrap())
        .collect();
    app
}
fn key(app: &mut App, code: KeyCode) -> Vec<AppCommand> {
    chord(app, code, KeyModifiers::NONE)
}
fn chord(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> Vec<AppCommand> {
    app.update(AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(
        code, modifiers,
    ))))
}
fn edit(app: &mut App, text: &str) {
    app.composer.set_text(text);
    app.refresh_slash_completion();
}
fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        assert!(key(app, KeyCode::Char(c)).is_empty());
    }
}
fn popup(app: &App) -> &SlashCompletionState {
    let completion = app.slash_completion.as_ref().unwrap();
    assert!(completion.popup.is_some());
    completion
}

#[test]
fn object_completion_and_explicit_choice_have_separate_commit_boundaries() {
    let mut app = app();
    edit(&mut app, "/ren");
    assert_eq!(app.slash_completion.as_ref().unwrap().items.len(), 1);
    assert!(key(&mut app, KeyCode::Enter).is_empty());
    assert_eq!(app.composer.content(), "/ren");
    assert!(popup(&app).items.len() > 1);
    assert!(app.pending_requests.is_empty());
    assert!(key(&mut app, KeyCode::Esc).is_empty());
    edit(&mut app, "/ren");
    app.slash_dismissed_text = None;
    app.refresh_slash_completion();
    assert!(key(&mut app, KeyCode::Tab).is_empty());
    assert_eq!(app.composer.content(), "/rename ");
    assert!(app.pending_requests.is_empty());
    assert!(chord(&mut app, KeyCode::Char(' '), KeyModifiers::CONTROL).is_empty());
    assert_eq!(popup(&app).items.len(), 7);
    type_text(&mut app, "list");
    assert_eq!(app.composer.content(), "/rename ");
    assert!(key(&mut app, KeyCode::Enter).is_empty());
    assert_eq!(app.composer.content(), "/session list ");
    assert!(app.slash_completion.as_ref().unwrap().popup.is_none());
    assert!(app.pending_requests.is_empty());
    assert_eq!(app.dock, Dock::Composer);
    assert!(!key(&mut app, KeyCode::Enter).is_empty());
    assert!(matches!(app.dock, Dock::SessionSelector(_)));
}

#[test]
fn popup_filter_escape_and_paste_preserve_entry_draft_revision_and_undo() {
    let mut app = app();
    type_text(&mut app, "/session");
    let revision = app.composer.editor_revision();
    let cursor = app.composer.cursor();
    assert!(key(&mut app, KeyCode::Enter).is_empty());
    assert_eq!(popup(&app).popup.as_ref().unwrap().entry_draft, "/session");
    assert!(
        app.update(AppEvent::Terminal(CrosstermEvent::Paste("ren\n".into())))
            .is_empty()
    );
    assert_eq!(popup(&app).filter, "ren");
    assert_eq!(app.composer.content(), "/session");
    assert_eq!(app.composer.editor_revision(), revision);
    assert!(key(&mut app, KeyCode::Esc).is_empty());
    assert_eq!(popup(&app).filter, "");
    assert!(key(&mut app, KeyCode::Esc).is_empty());
    assert!(app.slash_completion.is_none());
    assert_eq!(app.composer.cursor(), cursor);
    assert_eq!(app.composer.editor_revision(), revision);
    assert!(app.pending_requests.is_empty());
    app.composer.undo();
    assert_ne!(app.composer.content(), "/session");
}

#[test]
fn popup_no_match_tab_enter_never_dispatches_or_changes_draft() {
    let mut app = app();
    edit(&mut app, "/session");
    key(&mut app, KeyCode::Enter);
    type_text(&mut app, "zzz-中文");
    assert!(popup(&app).items.is_empty());
    for code in [KeyCode::Up, KeyCode::Down, KeyCode::Tab, KeyCode::Enter] {
        assert!(key(&mut app, code).is_empty());
        assert_eq!(app.composer.content(), "/session");
        assert!(popup(&app).items.is_empty());
        assert!(app.pending_requests.is_empty());
    }
}

#[test]
fn model_popup_highlight_fill_and_literal_commit_are_distinct() {
    let mut app = app();
    edit(&mut app, "/model ");
    let revision = app.composer.editor_revision();
    key(&mut app, KeyCode::Enter);
    assert_eq!(
        popup(&app).items[popup(&app).selected].value(),
        Some("deep")
    );
    type_text(&mut app, "fast");
    assert_eq!(app.active_view().unwrap().info.model, "deep");
    assert_eq!(app.active_view().unwrap().info.reasoning, Reasoning::High);
    assert_eq!(app.composer.editor_revision(), revision);
    assert!(key(&mut app, KeyCode::Enter).is_empty());
    assert_eq!(app.composer.content(), "/model fast ");
    assert!(app.pending_requests.is_empty());
    assert!(key(&mut app, KeyCode::Enter).is_empty());
    assert!(
        matches!(&app.dock, Dock::ReasoningSelector(state) if state.model_context.as_deref() == Some("fast"))
    );
    assert!(app.pending_requests.is_empty());
    let requests = testapp::take_requests(app.confirm_reasoning_item());
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "session.update");
    assert_eq!(requests[0].params["model"], "fast");
    assert_eq!(requests[0].params["reasoning"], "low");
}

#[test]
fn stale_popup_cannot_rewrite_new_draft_cursor_or_owner() {
    for mutation in 0..3 {
        let mut app = app();
        edit(&mut app, "/session");
        key(&mut app, KeyCode::Enter);
        type_text(&mut app, "new");
        match mutation {
            0 => app.composer.set_text("newer text"),
            1 => app.composer.move_to(0, 2),
            _ => app.sessions.active = Some("different-owner".into()),
        }
        let content = app.composer.content();
        let cursor = app.composer.cursor();
        assert!(app.apply_action(Action::CompletionAccept).is_empty());
        assert_eq!(app.composer.content(), content);
        assert_eq!(app.composer.cursor(), cursor);
        assert!(app.slash_completion.is_none());
        assert!(app.pending_requests.is_empty());
    }
}

#[test]
fn ordinary_left_right_and_line_edits_never_change_a_value() {
    let mut app = app();
    edit(&mut app, "/reasoning low");
    let before = app.active_view().unwrap().info.reasoning;
    for code in [KeyCode::Left, KeyCode::Left, KeyCode::Right] {
        assert!(key(&mut app, code).is_empty());
        assert_eq!(app.composer.content(), "/reasoning low");
    }
    assert_eq!(app.active_view().unwrap().info.reasoning, before);
    assert!(chord(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL).is_empty());
    assert_eq!(app.composer.content(), "/reasoning lo");
    assert!(chord(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL).is_empty());
    assert_eq!(app.composer.content(), "");
    assert!(app.pending_requests.is_empty());
}

#[test]
fn popup_ctrl_u_k_clear_only_filter_and_left_resumes_editor() {
    let mut app = app();
    edit(&mut app, "/session");
    key(&mut app, KeyCode::Enter);
    for c in ['u', 'k'] {
        type_text(&mut app, "rename");
        assert!(chord(&mut app, KeyCode::Char(c), KeyModifiers::CONTROL).is_empty());
        assert_eq!(popup(&app).filter, "");
        assert_eq!(app.composer.content(), "/session");
    }
    assert!(key(&mut app, KeyCode::Left).is_empty());
    assert_eq!(app.composer.cursor(), (0, 7));
    assert!(app.slash_completion.is_none());
    assert!(app.pending_requests.is_empty());
}

#[test]
fn opaque_argument_tab_retains_case_unicode_spacing_and_aliases() {
    for text in [
        "/rename My  Title 中文  ",
        "/session rename My  Title 中文  ",
        "/search /session New Mixed/Path",
        "/workspace files My  Folder/A.rs",
    ] {
        let mut app = app();
        edit(&mut app, text);
        let revision = app.composer.editor_revision();
        assert!(key(&mut app, KeyCode::Tab).is_empty());
        assert_eq!(app.composer.content(), text);
        assert_eq!(app.composer.editor_revision(), revision);
        assert!(app.pending_requests.is_empty());
    }
}

#[test]
fn mouse_uses_the_visible_compact_and_popup_rows_at_minimum_size() {
    let mut app = app();
    app.terminal_size = (60, 16);
    edit(&mut app, "/session");
    app.notice(NoticeLevel::Warning, "Visible warning");
    let screen = crate::ui::layout::screen_layout(&app, ratatui::layout::Rect::new(0, 0, 60, 16));
    let height = crate::ui::layout::composer_completion_rows(&app)
        .min(screen.panel.height.saturating_sub(1));
    let geometry = crate::ui::layout::slash_completion_geometry(
        app.slash_completion.as_ref().unwrap(),
        height,
        16,
    );
    let row = screen.panel.bottom() - height + u16::from(geometry.show_header);
    let mouse = |row| {
        AppEvent::Terminal(CrosstermEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 20,
            row,
            modifiers: KeyModifiers::NONE,
        }))
    };
    app.focus = crate::state::panels::Focus::Main;
    assert!(app.update(mouse(row)).is_empty());
    assert_eq!(app.focus, crate::state::panels::Focus::Editor);
    assert!(popup(&app).items.len() > 3);
    let screen = crate::ui::layout::screen_layout(&app, ratatui::layout::Rect::new(0, 0, 60, 16));
    let height = crate::ui::layout::composer_completion_rows(&app)
        .min(screen.panel.height.saturating_sub(1));
    let geometry = crate::ui::layout::slash_completion_geometry(popup(&app), height, 16);
    assert!(geometry.end - geometry.start <= 3);
    let expected = popup(&app).items[geometry.start].text.clone();
    let row = screen.panel.bottom() - height + u16::from(geometry.show_header);
    assert!(app.update(mouse(row)).is_empty());
    assert_eq!(app.composer.content(), format!("{expected} "));
    assert!(app.slash_completion.as_ref().unwrap().popup.is_none());
    assert!(app.pending_requests.is_empty());
}

#[test]
fn scratch_reasoning_uses_existing_default_form_model_without_early_form_creation() {
    let mut app = app();
    app.set_active_session(None);
    app.catalogs.next_model = Some("fast".into());
    edit(&mut app, "/reasoning");
    assert!(key(&mut app, KeyCode::Enter).is_empty());
    assert_eq!(popup(&app).items.len(), 2);
    assert_eq!(popup(&app).items[0].value(), Some("low"));
    assert_eq!(popup(&app).items[1].value(), Some("medium"));
    assert!(app.new_session().is_none());
    assert!(key(&mut app, KeyCode::Enter).is_empty());
    assert_eq!(app.composer.content(), "/reasoning low ");
    assert!(app.new_session().is_none());
    assert!(app.pending_requests.is_empty());
    assert!(key(&mut app, KeyCode::Enter).is_empty());
    assert_eq!(app.new_session().unwrap().reasoning, Reasoning::Low);
    assert_eq!(app.new_session().unwrap().model, "fast");
    assert!(app.pending_requests.is_empty());
}

#[test]
fn popup_catalog_refresh_keeps_filter_and_revalidates_changed_model_support() {
    let mut app = app();
    edit(&mut app, "/reasoning");
    key(&mut app, KeyCode::Enter);
    type_text(&mut app, "h");
    assert_eq!(popup(&app).items.len(), 1);
    app.sessions.known.get_mut("ses_1").unwrap().info.model = "fast".into();
    assert!(app.update(AppEvent::Tick).is_empty());
    assert_eq!(popup(&app).filter, "h");
    assert!(popup(&app).items.is_empty());
    assert!(key(&mut app, KeyCode::Enter).is_empty());
    assert_eq!(app.composer.content(), "/reasoning");
    assert!(app.pending_requests.is_empty());
}

#[test]
fn hidden_fatal_popup_does_not_steal_quit_or_mouse_input() {
    let mut app = app();
    edit(&mut app, "/session");
    key(&mut app, KeyCode::Enter);
    app.connection = ConnectionState::Failed("synthetic failure".into());
    assert_eq!(
        crate::keymap::map(&app, KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)),
        Action::Quit
    );
    let draft = app.composer.content();
    app.update(AppEvent::Terminal(CrosstermEvent::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 20,
        row: 20,
        modifiers: KeyModifiers::NONE,
    })));
    assert!(app.slash_completion.is_none());
    assert_eq!(app.composer.content(), draft);
    assert!(app.pending_requests.is_empty());
}

#[test]
fn chooser_selection_does_not_bypass_destructive_confirmation() {
    let mut app = app();
    edit(&mut app, "/session");
    key(&mut app, KeyCode::Enter);
    type_text(&mut app, "delete");
    assert!(key(&mut app, KeyCode::Enter).is_empty());
    assert_eq!(app.composer.content(), "/session delete ");
    assert!(app.notices().is_empty());
    assert!(app.pending_requests.is_empty());
    assert!(key(&mut app, KeyCode::Enter).is_empty());
    assert!(
        app.notices()
            .iter()
            .any(|notice| notice.text.contains("/delete confirm"))
    );
    assert!(app.pending_requests.is_empty());
}

#[test]
fn root_keyboard_navigation_and_scoped_prefix_enter_open_only_choices() {
    let mut app = app();
    edit(&mut app, "/");
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Down);
    assert!(key(&mut app, KeyCode::Enter).is_empty());
    assert_eq!(
        popup(&app).group,
        Some(crate::command::CommandGroup::Session)
    );
    assert_eq!(
        popup(&app).items[popup(&app).selected].action_name(),
        "list"
    );
    assert_eq!(app.composer.content(), "/");
    for text in ["/ren", "/mod", "/session ren"] {
        edit(&mut app, text);
        assert!(key(&mut app, KeyCode::Enter).is_empty());
        popup(&app);
        assert_eq!(app.composer.content(), text);
        assert!(app.pending_requests.is_empty());
    }
}

#[test]
fn no_active_session_popup_highlights_saved_defaults_without_creating_a_session() {
    let mut app = app();
    app.sessions.active = None;
    app.catalogs.next_model = Some("fast".into());
    app.catalogs.next_reasoning = Some(Reasoning::Low);
    for (command, value) in [("/model", "fast"), ("/reasoning", "low")] {
        edit(&mut app, command);
        assert!(key(&mut app, KeyCode::Enter).is_empty());
        assert_eq!(popup(&app).items[popup(&app).selected].value(), Some(value));
        assert!(app.pending_requests.is_empty());
        assert_eq!(app.dock, Dock::Composer);
        assert!(key(&mut app, KeyCode::Esc).is_empty());
        assert_eq!(app.composer.content(), command);
    }
}

#[test]
fn popup_catalog_refresh_preserves_identity_and_removes_obsolete_values() {
    let mut app = app();
    edit(&mut app, "/model");
    key(&mut app, KeyCode::Enter);
    let revision = app.composer.editor_revision();
    assert_eq!(
        popup(&app).items[popup(&app).selected].value(),
        Some("deep")
    );
    app.catalogs.models.reverse();
    assert!(app.update(AppEvent::Tick).is_empty());
    assert_eq!(
        popup(&app).items[popup(&app).selected].value(),
        Some("deep")
    );
    app.catalogs.models.retain(|model| model.id != "deep");
    assert!(app.update(AppEvent::Tick).is_empty());
    assert!(
        popup(&app)
            .items
            .iter()
            .all(|item| item.value() != Some("deep"))
    );
    assert_eq!(app.composer.editor_revision(), revision);
    app.catalogs.models.clear();
    assert!(app.update(AppEvent::Tick).is_empty());
    assert!(popup(&app).items.is_empty());
    assert!(key(&mut app, KeyCode::Enter).is_empty());
    assert_eq!(app.composer.content(), "/model");
    assert!(app.pending_requests.is_empty());
}

#[test]
fn failed_or_shutdown_connection_invalidates_popup_before_acceptance() {
    for connection in [
        ConnectionState::Failed("closed".into()),
        ConnectionState::ShuttingDown,
    ] {
        let mut app = app();
        edit(&mut app, "/session");
        key(&mut app, KeyCode::Enter);
        app.connection = connection;
        assert!(app.apply_action(Action::CompletionAccept).is_empty());
        assert!(app.slash_completion.is_none());
        assert_eq!(app.composer.content(), "/session");
        assert!(app.pending_requests.is_empty());
    }
}

#[test]
fn activating_direct_alias_keeps_its_selected_action_identity() {
    for (draft, expected, activation) in [
        ("/ren", "/session rename ", KeyCode::Enter),
        ("/resume", "/session resume ", KeyCode::Char(' ')),
        ("/delete", "/session delete ", KeyCode::Char(' ')),
        (
            "/session configure",
            "/session configure ",
            KeyCode::Char(' '),
        ),
    ] {
        let mut app = app();
        edit(&mut app, draft);
        let modifiers = if activation == KeyCode::Enter {
            KeyModifiers::NONE
        } else {
            KeyModifiers::CONTROL
        };
        assert!(chord(&mut app, activation, modifiers).is_empty());
        let completion = popup(&app);
        assert_eq!(
            format!("{} ", completion.items[completion.selected].text),
            expected
        );
        assert!(key(&mut app, KeyCode::Enter).is_empty());
        assert_eq!(app.composer.content(), expected);
        assert!(app.pending_requests.is_empty());
    }
}

#[test]
fn explicit_editor_chords_take_priority_over_compact_rows_and_popups() {
    for draft in [
        "/search arbitrary text",
        "/rename My title",
        "/delete confirm",
        "/session",
    ] {
        for with_popup in [false, true] {
            let mut app = app();
            edit(&mut app, draft);
            if with_popup {
                assert!(app.apply_action(Action::CompletionOpen).is_empty());
                assert!(popup(&app).popup.is_some());
            }
            for (code, expected) in [
                (KeyCode::Up, Action::HistoryPrev),
                (KeyCode::Down, Action::HistoryNext),
            ] {
                assert_eq!(
                    crate::keymap::map(&app, KeyEvent::new(code, KeyModifiers::ALT)),
                    expected
                );
            }
            assert!(chord(&mut app, KeyCode::Enter, KeyModifiers::SHIFT).is_empty());
            assert_eq!(app.composer.content(), format!("{draft}\n"));
            assert!(app.slash_completion.is_none());
            assert!(app.pending_requests.is_empty());
            assert_eq!(app.dock, Dock::Composer);
        }
    }
}

#[test]
fn popup_wheel_selection_requests_redraw_before_accepting() {
    let mut app = app();
    app.terminal_size = (60, 16);
    edit(&mut app, "/session");
    key(&mut app, KeyCode::Enter);
    let selected = popup(&app).selected;
    let screen = crate::ui::layout::screen_layout(&app, ratatui::layout::Rect::new(0, 0, 60, 16));
    assert!(app.update(AppEvent::Rendered).is_empty());
    assert!(!app.dirty);
    assert!(
        app.update(AppEvent::Terminal(CrosstermEvent::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 20,
            row: screen.panel.bottom() - 2,
            modifiers: KeyModifiers::NONE,
        })))
        .is_empty()
    );
    assert_ne!(popup(&app).selected, selected);
    assert!(app.dirty);
    let expected = popup(&app).items[popup(&app).selected].text.clone();
    assert!(key(&mut app, KeyCode::Enter).is_empty());
    assert_eq!(app.composer.content(), format!("{expected} "));
    assert!(app.pending_requests.is_empty());
}
