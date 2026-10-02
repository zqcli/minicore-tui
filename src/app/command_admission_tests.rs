use super::*;
use crate::ui::testapp;

fn ready() -> App {
    let mut app = testapp::open_empty(ThemeKind::Dark, "ses_1", None, "high");
    app.catalogs.models = testapp::standard_catalog()
        .0
        .into_iter()
        .map(|value| serde_json::from_value(value).unwrap())
        .collect();
    app
}

fn assert_kept(app: &mut App, command: &str) {
    app.composer.set_text(command);
    let pending = app.pending_requests.len();
    assert!(app.submit_composer().is_empty(), "{command}");
    assert_eq!(app.composer.content(), command);
    assert_eq!(app.pending_requests.len(), pending);
    assert!(matches!(app.dock, Dock::Composer));
}

#[test]
fn missing_session_keeps_command_arguments() {
    let mut app = ready();
    app.sessions.active = None;
    for command in [
        "/files src/中文.rs",
        "/grep MixedCase",
        "/search Important",
        "/export report.md",
        "/rename New Title",
    ] {
        assert_kept(&mut app, command);
    }
}

#[test]
fn unloaded_workspace_keeps_filters_and_never_opens_session() {
    let mut app = ready();
    let view = app.sessions.known.get_mut("ses_1").unwrap();
    view.info.loaded = false;
    view.browsing = true;
    for command in ["/files src/main.rs", "/workspace grep ExactCase"] {
        assert_kept(&mut app, command);
    }
}

#[test]
fn pending_configuration_update_keeps_second_choice() {
    let mut app = ready();
    app.pending_requests.insert(
        crate::protocol::RequestId(987654),
        RequestKind::UpdateSession {
            session_id: "ses_1".into(),
            loop_id: None,
            model: Some("fast".into()),
            reasoning: None,
        },
    );
    for command in ["/model fast", "/reasoning high", "/model", "/reasoning"] {
        assert_kept(&mut app, command);
    }
}

#[test]
fn read_only_configuration_keeps_literal_choice() {
    let mut app = ready();
    app.sessions.known.get_mut("ses_1").unwrap().browsing = true;
    for command in ["/model fast", "/reasoning high"] {
        assert_kept(&mut app, command);
    }
}

#[test]
fn uncalibrated_or_closing_configuration_keeps_choice() {
    for closing in [false, true] {
        let mut app = ready();
        let view = app.sessions.known.get_mut("ses_1").unwrap();
        view.closing = closing;
        view.event_gap = !closing;
        for command in ["/model fast", "/reasoning high"] {
            assert_kept(&mut app, command);
        }
    }
}
