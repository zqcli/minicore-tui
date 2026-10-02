use super::*;
use crate::ui::testapp;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn app() -> App {
    let mut a = testapp::open_empty(ThemeKind::Dark, "ses_1", None, "high");
    a.catalogs.models = testapp::standard_catalog()
        .0
        .into_iter()
        .map(|value| serde_json::from_value(value).unwrap())
        .collect();
    a
}
fn key(app: &mut App, code: KeyCode) -> Vec<AppCommand> {
    app.update(AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(
        code,
        KeyModifiers::NONE,
    ))))
}
fn edit(app: &mut App, text: &str) {
    app.composer.set_text(text);
    app.refresh_slash_completion();
}

#[test]
fn group_enter_opens_choices_tab_never_executes_and_escape_dismisses() {
    let mut a = app();
    edit(&mut a, "/session");
    assert!(key(&mut a, KeyCode::Enter).is_empty());
    assert_eq!(a.composer.content(), "/session");
    assert!(a.slash_completion.as_ref().unwrap().popup.is_some());
    type_command(&mut a, "ren");
    assert_eq!(a.composer.content(), "/session");
    assert!(key(&mut a, KeyCode::Tab).is_empty());
    assert_eq!(a.composer.content(), "/session rename ");
    assert!(a.pending_requests.is_empty());
    edit(&mut a, "/session re");
    assert!(key(&mut a, KeyCode::Enter).is_empty());
    assert!(a.slash_completion.as_ref().unwrap().popup.is_some());
    type_command(&mut a, "rename");
    assert!(key(&mut a, KeyCode::Esc).is_empty());
    assert_eq!(a.slash_completion.as_ref().unwrap().filter, "");
    assert!(key(&mut a, KeyCode::Esc).is_empty());
    assert_eq!(a.composer.content(), "/session re");
    assert!(a.slash_completion.is_none());
    for code in [KeyCode::Left, KeyCode::Right] {
        assert!(key(&mut a, code).is_empty());
        assert!(a.slash_completion.is_none());
    }
    assert!(key(&mut a, KeyCode::Char('n')).is_empty());
    assert!(a.slash_completion.is_some());
}

#[test]
fn no_match_cannot_dispatch_previous_selection_or_cancel() {
    let mut a = app();
    edit(&mut a, "/session zzz");
    assert!(a.slash_completion.as_ref().unwrap().items.is_empty());
    for code in [KeyCode::Down, KeyCode::Tab, KeyCode::Enter] {
        assert!(key(&mut a, code).is_empty());
        assert_eq!(a.composer.content(), "/session zzz");
    }
    assert!(key(&mut a, KeyCode::Esc).is_empty());
    assert_eq!(a.composer.content(), "/session zzz");
}

#[test]
fn inline_model_uses_compatibility_picker_and_one_atomic_update() {
    let mut a = app();
    assert!(a.run_command("/model fast").is_empty());
    assert!(
        matches!(&a.dock,Dock::ReasoningSelector(s) if s.model_context.as_deref()==Some("fast"))
    );
    assert!(key(&mut a, KeyCode::Esc).is_empty());
    assert!(
        a.pending_requests
            .values()
            .all(|r| !matches!(r, RequestKind::UpdateSession { .. }))
    );
    assert!(a.run_command("/model fast").is_empty());
    let requests = testapp::take_requests(a.confirm_reasoning_item());
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "session.update");
    assert_eq!(requests[0].params["model"], "fast");
    assert_eq!(requests[0].params["reasoning"], "low");
    assert!(a.run_command("/reasoning medium").is_empty());
}

#[test]
fn inline_reasoning_validates_supported_values_and_model_id_case() {
    let mut a = app();
    for text in ["/model DEEP", "/reasoning max", "/reasoning nonsense"] {
        assert!(a.run_command(text).is_empty());
        assert_eq!(a.dock, Dock::Composer);
    }
    let requests = testapp::take_requests(a.run_command("/reasoning LOW"));
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].params["reasoning"], "low");
}

#[test]
fn unknown_qualified_command_keeps_exact_draft_without_rpc() {
    let mut a = app();
    let text = "/session unknown 中文 Mixed/Path";
    a.composer.set_text(text);
    assert!(a.submit_composer().is_empty());
    assert_eq!(a.composer.content(), text);
}

#[test]
fn slash_small_screen_keeps_editable_line_selection_and_controls() {
    for (width, height) in [(100, 30), (60, 16)] {
        let mut a = app();
        edit(&mut a, "/conversation ");
        a.notice(NoticeLevel::Warning, "A visible notice");
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| crate::ui::render(frame, &a)).unwrap();
        let rows = terminal
            .backend()
            .buffer()
            .content
            .chunks(width as usize)
            .map(|r| r.iter().map(|c| c.symbol()).collect::<String>())
            .collect::<Vec<_>>();
        assert!(rows.iter().any(|r| r.contains("/conversation ")));
        assert!(rows.iter().any(|r| r.contains("→ Conversation")));
        assert!(rows.iter().any(|r| r.contains("Tab") && r.contains("Esc")));
    }
}

#[test]
fn editing_in_middle_or_multiline_does_not_replace_command_suffix() {
    let mut a = app();
    for text in ["/session", "/session rename Title", "/help\nretained"] {
        a.composer.set_text(text);
        a.composer.move_to(0, 4);
        a.refresh_slash_completion();
        assert!(a.slash_completion.is_none());
        assert!(key(&mut a, KeyCode::Tab).is_empty());
        assert_eq!(a.composer.content(), text);
    }
}

#[test]
fn invalid_inline_value_retains_exact_draft() {
    let mut a = app();
    for text in ["/model Missing/Model", "/reasoning max"] {
        a.composer.set_text(text);
        assert!(a.submit_composer().is_empty());
        assert_eq!(a.composer.content(), text);
    }
}

#[test]
fn read_only_configuration_changes_never_send_update() {
    for text in ["/model deep", "/reasoning low"] {
        let mut a = app();
        a.sessions.known.get_mut("ses_1").unwrap().browsing = true;
        assert!(a.run_command(text).is_empty());
        assert!(a.notices().iter().any(|n| n.text.contains("Read-only")));
    }
}

#[test]
fn every_qualified_leaf_preserves_the_legacy_executor() {
    use crate::command::{COMMANDS, CommandArgs, menu};
    for spec in COMMANDS {
        let Some(group) = spec.group else {
            continue;
        };
        let arg = match spec.args {
            CommandArgs::Theme => " light",
            CommandArgs::ToolRef => " ses_1 loop_1 2 call_2",
            _ => "",
        };
        let child = if spec.name == "sessions" {
            "list"
        } else {
            spec.name
        };
        let qualified = format!("/{} {child}{arg}", group.name());
        let legacy = format!("/{}{arg}", spec.name);
        assert!(
            parse_command(&qualified).is_ok(),
            "{qualified} must be valid"
        );
        assert!(
            parse_command(&qualified) == parse_command(&legacy),
            "{qualified} must retain legacy executor"
        );
    }
    assert_eq!(
        menu::page("model deep", &["deep-v2".into(), "deep".into()], &[])
            .unwrap()
            .entries[0]
            .text,
        "/model deep"
    );
    assert!(
        menu::page("configure", &[], &[])
            .unwrap()
            .entries
            .iter()
            .any(|item| item.text == "/session configure")
    );
}

#[test]
fn stale_menu_cannot_execute_or_rewrite_a_new_draft() {
    let mut a = app();
    edit(&mut a, "/quit");
    a.composer.set_text("retained new draft");
    assert!(a.accept_slash_completion());
    assert_eq!(a.composer.content(), "retained new draft");
    assert!(a.slash_completion.is_none());
    edit(&mut a, "/session rename");
    a.composer.move_to(0, 4);
    a.cancel_slash_completion();
    assert_eq!(a.composer.content(), "/session rename");
    edit(&mut a, "/quit");
    a.set_active_session(None);
    assert!(a.slash_completion.is_none());
    assert!(key(&mut a, KeyCode::Enter).is_empty());
}

#[test]
fn completed_finite_values_accept_trailing_spaces() {
    use crate::command::menu;
    for query in ["model deep ", "reasoning high ", "theme dark "] {
        let p = menu::page(query, &["deep-v2".into(), "deep".into()], &["high".into()]).unwrap();
        assert_eq!(p.entries[0].text, format!("/{}", query.trim()));
    }
}

fn mixed_case_model_app() -> App {
    let mut a = app();
    let mut model = a.catalogs.models[0].clone();
    model.id = "M-Exact-Model".into();
    model.model_ref = "Provider/Exact".into();
    a.catalogs.models.push(model);
    a
}

fn type_command(a: &mut App, text: &str) {
    for c in text.chars() {
        assert!(key(a, KeyCode::Char(c)).is_empty());
    }
}

#[test]
fn optional_argument_enter_validates_literal_case_and_prefix_without_rewriting() {
    for text in [
        "/model m-exact-model",
        "/model M-Exact",
        "/model Missing/Model",
        "/model Provider/Exact",
        "/reasoning lo",
        "/reasoning max",
    ] {
        let mut a = mixed_case_model_app();
        type_command(&mut a, text);
        let revision = a.composer.editor_revision();
        let cursor = a.composer.cursor();
        assert!(a.slash_completion.as_ref().unwrap().submits_literal());
        assert!(key(&mut a, KeyCode::Enter).is_empty());
        assert_eq!(a.composer.content(), text);
        assert_eq!(a.composer.cursor(), cursor);
        assert_eq!(a.composer.editor_revision(), revision);
        assert!(a.pending_requests.is_empty());
        assert!(a.notices().iter().any(|n| n.level == NoticeLevel::Error));
        assert_eq!(a.dock, Dock::Composer);
    }
}

#[test]
fn optional_argument_tab_explicitly_fills_canonical_case_without_submitting() {
    let mut a = mixed_case_model_app();
    type_command(&mut a, "/model m-exact");
    assert!(key(&mut a, KeyCode::Tab).is_empty());
    assert_eq!(a.composer.content(), "/model M-Exact-Model ");
    assert!(a.pending_requests.is_empty());
    assert!(key(&mut a, KeyCode::Tab).is_empty());
    assert!(a.pending_requests.is_empty());
    let outgoing = testapp::take_requests(key(&mut a, KeyCode::Enter));
    assert_eq!(outgoing.len(), 1);
    assert_eq!(outgoing[0].method, "session.update");
    assert_eq!(outgoing[0].params["model"], "M-Exact-Model");
}

#[test]
fn optional_argument_arrow_selection_only_changes_what_popup_fills() {
    let mut a = app();
    type_command(&mut a, "/reasoning ");
    assert!(key(&mut a, KeyCode::Enter).is_empty());
    assert!(a.slash_completion.as_ref().unwrap().popup.is_some());
    assert!(key(&mut a, KeyCode::Down).is_empty());
    let completion = a.slash_completion.as_ref().unwrap();
    let chosen = completion.items[completion.selected].text.clone();
    assert!(key(&mut a, KeyCode::Tab).is_empty());
    assert_eq!(a.composer.content(), format!("{chosen} "));
    assert!(a.pending_requests.is_empty());
    let outgoing = testapp::take_requests(key(&mut a, KeyCode::Enter));
    assert_eq!(outgoing.len(), 1);
    assert_eq!(
        outgoing[0].params["reasoning"],
        chosen.split_whitespace().nth(1).unwrap()
    );
}

#[test]
fn optional_argument_empty_tail_enter_opens_compact_picker() {
    for text in ["/model ", "/model   ", "/reasoning ", "/reasoning   "] {
        let mut a = app();
        type_command(&mut a, text);
        assert!(key(&mut a, KeyCode::Down).is_empty());
        assert!(key(&mut a, KeyCode::Enter).is_empty());
        assert!(a.pending_requests.is_empty());
        assert_eq!(a.dock, Dock::Composer);
        assert!(a.slash_completion.as_ref().unwrap().popup.is_some());
        assert!(key(&mut a, KeyCode::Esc).is_empty());
        assert!(a.pending_requests.is_empty());
    }
}

#[test]
fn optional_argument_literal_enter_retains_configuration_fences() {
    for text in ["/model deep", "/reasoning low"] {
        for mode in 0..3 {
            let mut a = app();
            let view = a.sessions.known.get_mut("ses_1").unwrap();
            match mode {
                0 => view.browsing = true,
                1 => view.closing = true,
                _ => view.state.as_mut().unwrap().status = SessionStatusWire::Finishing,
            }
            type_command(&mut a, text);
            assert!(key(&mut a, KeyCode::Enter).is_empty());
            assert!(a.pending_requests.is_empty());
            assert_eq!(a.active_view().unwrap().info.model, "deep");
            assert_eq!(a.active_view().unwrap().info.reasoning, Reasoning::High);
        }
    }
}

#[test]
fn no_match_enter_validates_current_literal_and_never_uses_stale_choice() {
    let mut a = app();
    type_command(&mut a, "/session zzz中文");
    assert!(a.slash_completion.as_ref().unwrap().items.is_empty());
    let cursor = a.composer.cursor();
    assert!(key(&mut a, KeyCode::Tab).is_empty());
    assert!(a.notices().is_empty());
    assert!(key(&mut a, KeyCode::Enter).is_empty());
    assert_eq!(a.composer.content(), "/session zzz中文");
    assert_eq!(a.composer.cursor(), cursor);
    assert!(
        a.notices()
            .iter()
            .any(|n| n.text.contains("unknown /session action"))
    );
    assert!(a.pending_requests.is_empty());
}

#[test]
fn optional_argument_hint_is_truthful_at_minimum_size() {
    for text in ["/model m-exact", "/model missing", "/reasoning lo"] {
        let mut a = mixed_case_model_app();
        type_command(&mut a, text);
        a.notice(
            NoticeLevel::Warning,
            "Visible notice while editing settings",
        );
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, 16)).unwrap();
        terminal.draw(|frame| crate::ui::render(frame, &a)).unwrap();
        let rows = terminal
            .backend()
            .buffer()
            .content
            .chunks(60)
            .map(|r| r.iter().map(|c| c.symbol()).collect::<String>())
            .collect::<Vec<_>>();
        assert!(
            rows.iter()
                .any(|r| r.contains("Tab") && r.contains("Enter run typed") && r.contains("Esc")),
            "{rows:?}"
        );
    }
}

#[test]
fn optional_argument_exact_values_still_use_existing_updates() {
    for text in ["/model M-Exact-Model", "/reasoning LOW"] {
        let mut a = mixed_case_model_app();
        type_command(&mut a, text);
        let outgoing = testapp::take_requests(key(&mut a, KeyCode::Enter));
        assert_eq!(outgoing.len(), 1, "{text}");
        assert_eq!(outgoing[0].method, "session.update");
        assert_eq!(outgoing[0].params["session_id"], "ses_1");
        if text.starts_with("/model") {
            assert_eq!(outgoing[0].params["model"], "M-Exact-Model");
        } else {
            assert_eq!(outgoing[0].params["reasoning"], "low");
        }
    }
}

#[test]
fn qualified_theme_choice_fills_before_literal_enter_applies() {
    let mut a = app();
    type_command(&mut a, "/app theme ");
    assert!(a.slash_completion.as_ref().unwrap().submits_literal());
    assert!(a.apply_action(Action::CompletionOpen).is_empty());
    assert!(a.slash_completion.as_ref().unwrap().popup.is_some());
    assert!(key(&mut a, KeyCode::Down).is_empty());
    assert!(key(&mut a, KeyCode::Enter).is_empty());
    assert_eq!(a.theme, ThemeKind::Dark);
    assert_eq!(a.composer.content(), "/app theme light ");
    assert!(testapp::take_requests(key(&mut a, KeyCode::Enter)).is_empty());
    assert_eq!(a.theme, ThemeKind::Light);
}
