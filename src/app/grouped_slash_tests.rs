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
fn group_enter_drills_tab_never_executes_and_escape_walks_back() {
    let mut a = app();
    edit(&mut a, "/session");
    assert!(key(&mut a, KeyCode::Enter).is_empty());
    assert_eq!(a.composer.content(), "/session ");
    assert_eq!(
        a.slash_completion.as_ref().unwrap().group,
        Some(crate::command::CommandGroup::Session)
    );
    edit(&mut a, "/session ren");
    assert!(key(&mut a, KeyCode::Tab).is_empty());
    assert_eq!(a.composer.content(), "/session rename ");
    assert!(key(&mut a, KeyCode::Tab).is_empty());
    assert!(
        a.pending_requests
            .values()
            .all(|r| !matches!(r, RequestKind::UpdateSession { .. }))
    );
    edit(&mut a, "/session re");
    assert!(key(&mut a, KeyCode::Esc).is_empty());
    assert_eq!(a.composer.content(), "/session ");
    assert!(key(&mut a, KeyCode::Esc).is_empty());
    assert_eq!(a.composer.content(), "/");
    assert!(key(&mut a, KeyCode::Esc).is_empty());
    assert!(a.slash_completion.is_none());
    for code in [KeyCode::Left, KeyCode::Right] {
        assert!(key(&mut a, code).is_empty());
        assert!(a.slash_completion.is_none());
    }
    assert!(key(&mut a, KeyCode::Char('m')).is_empty());
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
    assert_eq!(a.composer.content(), "/session ");
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
        assert!(rows.iter().any(|r| r.contains("→ /")));
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
