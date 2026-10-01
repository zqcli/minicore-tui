//! Help must suspend one exact input context, without becoming a navigation stack.
use super::*;
use crate::state::panels::{ContextState, Focus, MainView};
use crate::state::selection::SessionScope;
use crate::ui::testapp;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn ready_app() -> App {
    let mut app = testapp::open_empty(ThemeKind::Dark, "ses_1", None, "high");
    app.composer.set_text("first 中🙂\nsecond retained line");
    app.composer.move_to(0, 3);
    app
}

fn key(app: &mut App, code: KeyCode) -> Vec<AppCommand> {
    app.update(AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(
        code,
        KeyModifiers::NONE,
    ))))
}

fn round_trip(app: &mut App, close: KeyCode) {
    assert!(key(app, KeyCode::F(1)).is_empty());
    assert!(matches!(app.dock, Dock::Help));
    assert!(key(app, close).is_empty());
}

fn edited_form(app: &mut App) -> NewSessionState {
    assert!(app.update(AppEvent::OpenNewSession).is_empty());
    let Dock::NewSession(form) = &mut app.dock else {
        panic!("expected new form")
    };
    form.workspace = "/synthetic/中 project".into();
    form.title = "UNSENT title 中🙂".into();
    form.field = NewSessionField::Title;
    form.field_cursor = 8;
    form.clone()
}

#[test]
fn help_return_new_form_restores_fields_and_cursor_exactly_once() {
    let mut app = ready_app();
    let expected = edited_form(&mut app);
    for close in [KeyCode::Esc, KeyCode::F(1), KeyCode::Esc] {
        round_trip(&mut app, close);
        assert_eq!(app.dock, Dock::NewSession(expected.clone()));
        assert_eq!(app.composer.cursor(), (0, 3));
    }
    assert!(key(&mut app, KeyCode::Esc).is_empty());
    assert_eq!(app.dock, Dock::Composer);
    round_trip(&mut app, KeyCode::Esc);
    assert_eq!(app.dock, Dock::Composer);
    assert!(app.new_session().is_none());
}

#[test]
fn help_return_nested_selectors_retain_filter_and_parent_form() {
    for kind in [
        SelectorKind::Model,
        SelectorKind::Reasoning,
        SelectorKind::Profile,
    ] {
        let mut app = ready_app();
        let form = edited_form(&mut app);
        app.open_selector(kind);
        let state = app.selector_state_mut().unwrap();
        state.query = "synthetic-filter 中".into();
        state.cursor = 2;
        state.model_context = Some("retained-model".into());
        let expected = app.dock.clone();
        for close in [KeyCode::Esc, KeyCode::F(1)] {
            round_trip(&mut app, close);
            assert_eq!(app.dock, expected);
            assert_eq!(app.new_session(), Some(&form));
        }
        assert!(key(&mut app, KeyCode::Esc).is_empty());
        assert_eq!(app.dock, Dock::NewSession(form));
    }
}

#[test]
fn help_return_session_filter_scope_and_identity_are_preserved() {
    let mut app = ready_app();
    let mut selector = SessionSelectorState::new(Some("ses_1".into()));
    selector.query = "ses_1".into();
    selector.scope = SessionScope::All;
    app.dock = Dock::SessionSelector(selector.clone());
    for close in [KeyCode::F(1), KeyCode::Esc] {
        round_trip(&mut app, close);
        assert_eq!(app.dock, Dock::SessionSelector(selector.clone()));
    }
}

#[test]
fn help_return_active_model_filter_does_not_become_a_new_form() {
    let mut app = ready_app();
    app.update(AppEvent::OpenModelSelector);
    app.update(AppEvent::SetSelectorQuery {
        query: "deep".into(),
    });
    let expected = app.dock.clone();
    round_trip(&mut app, KeyCode::Esc);
    assert_eq!(app.dock, expected);
    assert!(app.new_session().is_none());
}

#[test]
fn help_return_logs_scroll_is_independent_of_help_scroll() {
    let mut app = ready_app();
    app.open_dock(Dock::Logs);
    app.panel_scroll = 13;
    for close in [KeyCode::Esc, KeyCode::F(1)] {
        assert!(key(&mut app, KeyCode::F(1)).is_empty());
        assert_eq!(app.panel_scroll, 0);
        assert!(key(&mut app, KeyCode::PageDown).is_empty());
        assert!(app.panel_scroll > 0);
        assert!(key(&mut app, close).is_empty());
        assert_eq!(app.dock, Dock::Logs);
        assert_eq!(app.panel_scroll, 13);
    }
}

#[test]
fn help_return_composer_multiline_and_detail_focus_are_unchanged() {
    let mut app = ready_app();
    for main in [false, true] {
        if main {
            app.main_view = MainView::Context(Box::new(ContextState {
                session: "ses_1".into(),
                epoch: app.active_view().unwrap().session_epoch,
                generation: 1,
                conversation_scroll: None,
                offset: 4,
                action: 0,
                scrollbar_grab: None,
            }));
            app.focus = Focus::Main;
        }
        for close in [KeyCode::F(1), KeyCode::Esc] {
            round_trip(&mut app, close);
            assert_eq!(app.dock, Dock::Composer);
            assert_eq!(app.composer.content(), "first 中🙂\nsecond retained line");
            assert_eq!(app.composer.cursor(), (0, 3));
            if main {
                assert_eq!(app.focus, Focus::Main);
                assert_eq!(app.context_panel().unwrap().offset, 4);
            }
        }
    }
}

#[test]
fn help_return_explicit_navigation_discards_detached_form() {
    let mut app = ready_app();
    edited_form(&mut app);
    app.open_selector(SelectorKind::Model);
    assert!(key(&mut app, KeyCode::F(1)).is_empty());
    assert!(
        app.new_session().is_none(),
        "hidden form must not own another selector"
    );
    assert!(app.update(AppEvent::OpenModelSelector).is_empty());
    assert!(
        app.new_session().is_none(),
        "new model selector must target the active session"
    );
    assert!(key(&mut app, KeyCode::Esc).is_empty());
    assert_eq!(app.dock, Dock::Composer);
    round_trip(&mut app, KeyCode::Esc);
    assert_eq!(app.dock, Dock::Composer);
}

#[test]
fn help_return_pending_create_stays_with_its_owner_until_response() {
    let mut app = ready_app();
    edited_form(&mut app);
    let create = testapp::take_requests(app.update(AppEvent::SubmitNewSession)).remove(0);
    let submitted = app.dock.clone();
    assert!(key(&mut app, KeyCode::F(1)).is_empty());
    assert_eq!(app.dock, submitted);
    assert!(
        app.notices
            .iter()
            .any(|notice| notice.text.contains("Help"))
    );
    testapp::respond_rpc_error(&mut app, &create, -32603, "synthetic create failure");
    assert!(!app.new_session().unwrap().submitting);
    let failed = app.dock.clone();
    round_trip(&mut app, KeyCode::Esc);
    assert_eq!(app.dock, failed);
}

#[test]
fn help_return_session_change_never_restores_a_stale_selector() {
    let mut app = ready_app();
    app.update(AppEvent::OpenModelSelector);
    assert!(key(&mut app, KeyCode::F(1)).is_empty());
    app.set_active_session(None);
    assert!(key(&mut app, KeyCode::Esc).is_empty());
    assert_eq!(app.dock, Dock::Composer);
    assert!(app.new_session().is_none());
}

#[test]
fn help_return_fatal_and_quit_keep_the_existing_shutdown_meaning() {
    let mut app = ready_app();
    edited_form(&mut app);
    assert!(key(&mut app, KeyCode::F(1)).is_empty());
    let requests = testapp::take_requests(key(&mut app, KeyCode::Char('q')));
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "agent.shutdown");
    let mut app = ready_app();
    edited_form(&mut app);
    assert!(key(&mut app, KeyCode::F(1)).is_empty());
    app.update(AppEvent::RpcChannelEnded);
    assert!(matches!(app.connection, ConnectionState::Failed(_)));
    assert!(matches!(
        key(&mut app, KeyCode::Char('q')).as_slice(),
        [AppCommand::Exit]
    ));
}

fn model_catalog(app: &mut App) {
    app.catalogs.models = ["deep", "fast"]
        .into_iter()
        .map(|id| ModelInfo {
            id: id.into(),
            model_ref: id.into(),
            context_window: 32_000,
            supports_tools: true,
            supported_reasoning: vec![Reasoning::Low, Reasoning::High],
        })
        .collect();
}

fn begin_update(app: &mut App) -> OutgoingRequest {
    model_catalog(app);
    app.update(AppEvent::OpenModelSelector);
    app.update(AppEvent::SetSelectorQuery {
        query: "fast".into(),
    });
    testapp::take_requests(app.update(AppEvent::ConfirmDock)).remove(0)
}

#[test]
fn help_return_owned_update_ack_and_error_cannot_leave_a_stuck_selector() {
    for success in [false, true] {
        let mut app = ready_app();
        let request = begin_update(&mut app);
        assert_eq!(request.method, "session.update");
        let submitting = app.dock.clone();
        assert!(key(&mut app, KeyCode::F(1)).is_empty());
        assert_eq!(app.dock, submitting);
        if success {
            let mut info = app.active_view().unwrap().info.clone();
            info.model = "fast".into();
            testapp::respond(
                &mut app,
                &request,
                serde_json::json!({"session": info, "active_revision": null}),
            );
            assert_eq!(app.dock, Dock::Composer);
            assert_eq!(app.active_view().unwrap().info.model, "fast");
        } else {
            testapp::respond_rpc_error(&mut app, &request, -32603, "synthetic update failure");
            assert!(!app.selector_state().unwrap().submitting);
            assert_eq!(app.selector_state().unwrap().query, "fast");
        }
        let settled = app.dock.clone();
        round_trip(&mut app, KeyCode::Esc);
        assert_eq!(app.dock, settled);
    }
}

#[test]
fn help_return_late_background_update_does_not_consume_an_independent_form() {
    for success in [false, true] {
        let mut app = ready_app();
        let request = begin_update(&mut app);
        let form = edited_form(&mut app);
        assert!(key(&mut app, KeyCode::F(1)).is_empty());
        assert_eq!(app.dock, Dock::Help);
        if success {
            let mut info = app.active_view().unwrap().info.clone();
            info.model = "fast".into();
            testapp::respond(
                &mut app,
                &request,
                serde_json::json!({"session": info, "active_revision": null}),
            );
        } else {
            testapp::respond_rpc_error(&mut app, &request, -32603, "synthetic background failure");
        }
        assert_eq!(app.dock, Dock::Help);
        assert!(key(&mut app, KeyCode::Esc).is_empty());
        assert_eq!(app.dock, Dock::NewSession(form));
    }
}

#[test]
fn help_return_late_create_response_keeps_the_newer_nested_draft() {
    for success in [false, true] {
        let mut app = ready_app();
        edited_form(&mut app);
        let old = testapp::take_requests(app.update(AppEvent::SubmitNewSession)).remove(0);
        let newer = edited_form(&mut app);
        app.open_selector(SelectorKind::Reasoning);
        app.update(AppEvent::SetSelectorQuery { query: "hi".into() });
        let selector = app.dock.clone();
        assert!(key(&mut app, KeyCode::F(1)).is_empty());
        assert_eq!(app.dock, Dock::Help);
        if success {
            let mut info = app.active_view().unwrap().info.clone();
            info.session_id = "created_old_draft".into();
            testapp::respond(&mut app, &old, serde_json::json!({"session": info}));
            assert_eq!(app.sessions.active.as_deref(), Some("created_old_draft"));
        } else {
            testapp::respond_rpc_error(&mut app, &old, -32603, "synthetic old create failure");
        }
        assert_eq!(app.dock, Dock::Help);
        assert!(key(&mut app, KeyCode::Esc).is_empty());
        assert_eq!(app.dock, selector);
        assert_eq!(app.new_session(), Some(&newer));
        assert!(key(&mut app, KeyCode::Esc).is_empty());
        assert_eq!(app.dock, Dock::NewSession(newer));
    }
}

#[test]
fn help_return_session_owned_lifecycle_blocks_but_background_lifecycle_does_not() {
    for owned in [false, true] {
        let mut app = ready_app();
        let request = testapp::take_requests(app.update(AppEvent::OpenSession {
            session_id: "ses_other".into(),
        }))
        .remove(0);
        let target = if owned { "ses_other" } else { "ses_1" };
        app.dock = Dock::SessionSelector(SessionSelectorState::new(Some(target.into())));
        let original = app.dock.clone();
        assert!(key(&mut app, KeyCode::F(1)).is_empty());
        if owned {
            assert_eq!(app.dock, original);
            testapp::respond_rpc_error(&mut app, &request, -32603, "synthetic open failure");
            round_trip(&mut app, KeyCode::Esc);
            assert!(matches!(app.dock, Dock::SessionSelector(_)));
        } else {
            assert_eq!(app.dock, Dock::Help);
            testapp::respond_rpc_error(&mut app, &request, -32603, "synthetic background failure");
            assert!(key(&mut app, KeyCode::Esc).is_empty());
            assert_eq!(app.dock, original);
        }
    }
}

#[test]
fn help_return_streaming_and_pending_reads_do_not_block_help_or_cancel_turns() {
    let mut app = testapp::live_turn(ThemeKind::Dark);
    app.composer.set_text("unsent during stream");
    let turn = app
        .active_view()
        .unwrap()
        .live
        .as_ref()
        .unwrap()
        .reference
        .clone();
    assert!(!app.pending_requests.is_empty());
    round_trip(&mut app, KeyCode::Esc);
    assert_eq!(
        app.active_view().unwrap().live.as_ref().unwrap().reference,
        turn
    );
    assert_eq!(app.composer.content(), "unsent during stream");
    assert!(
        !app.pending_requests
            .values()
            .any(|kind| matches!(kind, RequestKind::CancelTurn(_)))
    );
    let refresh = testapp::take_requests(app.update(AppEvent::OpenSessionSelector));
    assert!(
        refresh
            .iter()
            .any(|request| request.method == "session.list")
    );
    let selector = app.dock.clone();
    round_trip(&mut app, KeyCode::F(1));
    assert_eq!(app.dock, selector);
}

#[test]
fn help_return_refresh_reconciles_removed_selection_without_losing_filter() {
    let mut app = ready_app();
    let initial = testapp::take_requests(app.update(AppEvent::OpenSessionSelector)).remove(0);
    let info = app.active_view().unwrap().info.clone();
    testapp::respond(&mut app, &initial, serde_json::json!({"sessions": [info]}));
    app.update(AppEvent::SetSelectorQuery {
        query: "ses_".into(),
    });
    let request = testapp::take_requests(app.refresh_sessions()).remove(0);
    assert!(key(&mut app, KeyCode::F(1)).is_empty());
    assert_eq!(app.dock, Dock::Help);
    testapp::respond(&mut app, &request, serde_json::json!({"sessions": []}));
    assert!(key(&mut app, KeyCode::Esc).is_empty());
    let selector = app.session_selector_state().unwrap();
    assert_eq!(selector.query, "ses_");
    assert_eq!(selector.selected_session_id, None);
}

#[test]
fn help_return_same_session_new_epoch_and_away_back_are_fenced() {
    for reopen in [false, true] {
        let mut app = ready_app();
        app.update(AppEvent::OpenReasoningSelector);
        assert!(key(&mut app, KeyCode::F(1)).is_empty());
        if reopen {
            app.active_session_mut().unwrap().session_epoch += 1;
        } else {
            app.set_active_session(None);
            app.set_active_session(Some("ses_1".into()));
        }
        assert!(key(&mut app, KeyCode::Esc).is_empty());
        assert_eq!(app.dock, Dock::Composer);
        assert!(app.new_session().is_none());
    }
}

#[test]
fn help_return_repeated_f1_events_and_scroll_keys_never_pop_saved_context() {
    let mut app = ready_app();
    let form = edited_form(&mut app);
    assert!(key(&mut app, KeyCode::F(1)).is_empty());
    let mut repeat = KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE);
    repeat.kind = crossterm::event::KeyEventKind::Repeat;
    assert!(
        app.update(AppEvent::Terminal(CrosstermEvent::Key(repeat)))
            .is_empty()
    );
    assert_eq!(app.dock, Dock::Help);
    for code in [
        KeyCode::Down,
        KeyCode::PageDown,
        KeyCode::End,
        KeyCode::Home,
        KeyCode::Up,
        KeyCode::PageUp,
    ] {
        assert!(key(&mut app, code).is_empty());
        assert_eq!(app.dock, Dock::Help);
    }
    assert!(key(&mut app, KeyCode::F(1)).is_empty());
    assert_eq!(app.dock, Dock::NewSession(form));
    assert!(key(&mut app, KeyCode::Esc).is_empty());
    assert_eq!(app.dock, Dock::Composer);
}
