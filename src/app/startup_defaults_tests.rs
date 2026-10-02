use super::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::{Value, json};

fn requests(commands: Vec<AppCommand>) -> Vec<OutgoingRequest> {
    commands
        .into_iter()
        .filter_map(|command| match command {
            AppCommand::Rpc(request) => Some(request),
            _ => None,
        })
        .collect()
}
fn reply(app: &mut App, request: &OutgoingRequest, result: Value) -> Vec<OutgoingRequest> {
    requests(
        app.update(AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Response(
            RpcResponse {
                id: request.id,
                result: Some(result),
                error: None,
            },
        )))),
    )
}
fn reject(app: &mut App, request: &OutgoingRequest) {
    app.update(AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Response(
        RpcResponse {
            id: request.id,
            result: None,
            error: Some(crate::protocol::RpcError {
                code: -32602,
                message: "unknown explicit model Z/Unknown 中".into(),
                data: None,
            }),
        },
    ))));
}
fn prefs() -> CliPrefs {
    CliPrefs {
        auto_create_on_ready: true,
        ..CliPrefs::default()
    }
}
fn bootstrap(app: &mut App) -> Vec<OutgoingRequest> {
    let initial = requests(app.update(AppEvent::Bootstrap));
    assert_eq!(initial.len(), 4);
    let mut outgoing = Vec::new();
    for request in initial {
        let result = match request.method {
            "agent.ping" => {
                json!({"version":"0.5.0", "protocol_version":1, "capabilities":crate::protocol::REQUIRED_CAPABILITIES})
            }
            "model.list" => json!({"models":[
                {"id":"a-first","model_ref":"provider/first","context_window":128000,"supports_tools":true,"supported_reasoning":["auto","high"]},
                {"id":"z-default","model_ref":"provider/default","context_window":128000,"supports_tools":true,"supported_reasoning":["auto","low"]}
            ]}),
            "profile.list" => json!({"profiles":[
                {"id":"a-first","model":"a-first","reasoning":"high","tools":["read"]},
                {"id":"z-default","model":"z-default","reasoning":"low","tools":["read"]}
            ]}),
            "session.list" => json!({"sessions":[]}),
            other => panic!("unexpected bootstrap method {other}"),
        };
        outgoing.extend(reply(app, &request, result));
    }
    outgoing
}
fn create(prefs: CliPrefs) -> (App, OutgoingRequest) {
    let mut app = App::with_cli_prefs("/workspace 中".into(), prefs);
    let mut out = bootstrap(&mut app);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].method, "session.create");
    (app, out.remove(0))
}
fn session() -> Value {
    json!({"session":{"session_id":"ses_default","title":null,"profile":"z-default","workspace":"/workspace 中","model":"z-default","reasoning":"low","loaded":true,"created_at":"2026-10-01T20:00:00Z","updated_at":"2026-10-01T20:00:00Z"}})
}
fn key(app: &mut App, code: KeyCode) -> Vec<AppCommand> {
    app.update(AppEvent::Terminal(CrosstermEvent::Key(KeyEvent::new(
        code,
        KeyModifiers::NONE,
    ))))
}

#[test]
fn startup_omits_synthesized_defaults_and_sends_one_local_create() {
    let (mut app, request) = create(prefs());
    assert_eq!(app.catalogs.next_model.as_deref(), Some("a-first"));
    assert_eq!(request.params, json!({"workspace":"/workspace 中"}));
    assert!(app.startup_create_pending());
    assert!(matches!(app.dock, Dock::Composer));
    for part in [
        BootstrapPart::Ping,
        BootstrapPart::Models,
        BootstrapPart::Profiles,
        BootstrapPart::Sessions,
    ] {
        assert!(app.bootstrap_progress(part).is_empty());
    }
    assert!(app.update(AppEvent::Bootstrap).is_empty());
    assert!(app.update(AppEvent::Tick).is_empty());
    assert_eq!(
        app.pending_requests
            .values()
            .filter(|kind| matches!(kind, RequestKind::CreateSession { .. }))
            .count(),
        1
    );
}

#[test]
fn startup_preserves_every_explicit_override_combination_and_auto() {
    for mask in 0..8 {
        for reasoning in [Reasoning::Auto, Reasoning::High] {
            let preference = CliPrefs {
                profile: (mask & 1 != 0).then(|| "Z/Unknown profile 中".into()),
                model: (mask & 2 != 0).then(|| "Z/Unknown 中".into()),
                reasoning: (mask & 4 != 0).then_some(reasoning),
                ..prefs()
            };
            let (mut app, request) = create(preference.clone());
            let fields = request.params.as_object().unwrap();
            assert_eq!(
                fields.get("profile"),
                preference.profile.as_ref().map(|v| json!(v)).as_ref()
            );
            assert_eq!(
                fields.get("model"),
                preference.model.as_ref().map(|v| json!(v)).as_ref()
            );
            assert_eq!(
                fields.get("reasoning"),
                preference.reasoning.map(|v| json!(v)).as_ref()
            );
            reject(&mut app, &request);
            assert!(
                app.notices
                    .back()
                    .unwrap()
                    .text
                    .contains("unknown explicit model")
            );
            assert!(app.notices.back().unwrap().text.contains("/new form"));
            assert!(!app.startup_create_pending());
            app.catalogs.next_model = Some("a-first".into());
            app.catalogs.next_profile = Some("a-first".into());
            app.catalogs.next_reasoning = Some(Reasoning::High);
            let retry = requests(app.create_session_quick());
            assert_eq!(retry.len(), 1);
            assert_eq!(
                retry[0].params, request.params,
                "form seats must never overwrite explicit creation intent"
            );
        }
    }
}

#[test]
fn startup_pending_keeps_typed_draft_and_all_editor_state_across_help_and_ack() {
    let (mut app, request) = create(prefs());
    app.composer.type_text("first 中🙂\n");
    app.composer.insert_paste(&"paste中".repeat(300));
    app.composer.type_text(" tail");
    app.composer.undo();
    app.composer.move_left();
    let raw = app.composer.content();
    let visible = app.composer.display_content();
    let cursor = app.composer.cursor();
    let revision = app.composer.editor_revision();
    let markers = app.composer.paste_ranges().to_vec();
    for _ in 0..3 {
        assert!(key(&mut app, KeyCode::Enter).is_empty());
        assert_eq!(app.composer.content(), raw);
        assert!(app.open_new_session().is_empty());
        assert!(app.open_selector(SelectorKind::Model).is_empty());
        assert!(app.open_selector(SelectorKind::Session).is_empty());
        assert!(app.reload().is_empty());
    }
    assert!(key(&mut app, KeyCode::F(1)).is_empty());
    assert!(matches!(app.dock, Dock::Help));
    let followups = reply(&mut app, &request, session());
    assert!(followups.iter().all(|r| !r.method.starts_with("turn.")));
    assert_eq!(app.sessions.active.as_deref(), Some("ses_default"));
    assert!(matches!(app.dock, Dock::Help));
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.composer.content(), raw);
    assert_eq!(app.composer.display_content(), visible);
    assert_eq!(app.composer.cursor(), cursor);
    assert_eq!(app.composer.editor_revision(), revision);
    assert_eq!(app.composer.paste_ranges(), markers);
    app.composer.redo();
    assert!(app.composer.content().ends_with(" tail"));
    app.composer.undo();
    assert_eq!(app.composer.content(), raw);
}

#[test]
fn startup_failure_never_retries_or_discards_the_scratch_draft() {
    let (mut app, request) = create(prefs());
    app.composer.type_text("Keep this unfinished 中 prompt");
    app.composer.move_left();
    let cursor = app.composer.cursor();
    reject(&mut app, &request);
    for _ in 0..4 {
        assert!(app.update(AppEvent::Bootstrap).is_empty());
        assert!(app.bootstrap_progress(BootstrapPart::Profiles).is_empty());
        assert!(app.update(AppEvent::Tick).is_empty());
    }
    assert!(app.sessions.active.is_none());
    assert_eq!(app.composer.content(), "Keep this unfinished 中 prompt");
    assert_eq!(app.composer.cursor(), cursor);
    assert!(app.notices.back().unwrap().sticky);
    app.open_new_session();
    assert!(app.new_session().is_some());
    app.cancel_dock();
    assert_eq!(app.composer.cursor(), cursor);
}

#[test]
fn startup_explicit_session_and_continue_miss_never_auto_create() {
    for selection in [
        StartupSession::Exact("missing".into()),
        StartupSession::ContinueCurrentWorkspace,
    ] {
        let mut app = App::with_cli_prefs(
            "/workspace 中".into(),
            CliPrefs {
                startup_session: Some(selection.clone()),
                ..prefs()
            },
        );
        let outgoing = bootstrap(&mut app);
        assert!(outgoing.iter().all(|r| r.method != "session.create"));
        if let StartupSession::Exact(_) = selection {
            let request = outgoing
                .iter()
                .find(|r| r.method == "session.open")
                .unwrap();
            reject(&mut app, request);
        } else {
            assert!(matches!(app.dock, Dock::SessionSelector(_)));
        }
        assert!(!app.auto_create_on_ready);
        assert!(app.bootstrap_progress(BootstrapPart::Sessions).is_empty());
        assert!(app.sessions.active.is_none());
    }
}

#[test]
fn startup_shutdown_rejects_late_create_ack_without_adopting_draft() {
    let (mut app, request) = create(prefs());
    app.composer.type_text("shutdown draft 中");
    let commands = requests(app.request_shutdown());
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].method, "agent.shutdown");
    assert!(reply(&mut app, &request, session()).is_empty());
    assert!(app.sessions.active.is_none());
    assert_eq!(app.composer.content(), "shutdown draft 中");
}

#[test]
fn startup_pending_header_is_prompt_first_and_truthful_at_minimum_size() {
    let (mut app, _) = create(prefs());
    app.update(AppEvent::Terminal(CrosstermEvent::Resize(60, 16)));
    app.composer.type_text("ready to type 中");
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, 16)).unwrap();
    terminal
        .draw(|frame| crate::ui::render(frame, &app))
        .unwrap();
    let text = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect::<String>();
    assert!(text.contains("Creating default session"), "{text}");
    assert!(text.contains("your draft is kept"), "{text}");
    assert!(text.contains("ready to type 中"), "{text}");
    assert!(!text.contains("Choose settings below"));
    assert_eq!(
        crate::ui::footer::footer_view(&app).status,
        crate::ui::footer::FooterStatus::Starting
    );
}

#[test]
fn startup_quick_creation_retains_session_panel_busy_fences() {
    for kind in [
        RequestKind::RefreshSessions {
            selected_session_id: None,
        },
        RequestKind::RenameSession {
            session_id: "other".into(),
        },
        RequestKind::DeleteSession {
            session_id: "other".into(),
        },
    ] {
        let mut app = App::new("/workspace".into());
        app.connection = ConnectionState::Ready;
        app.pending_requests.insert(RequestId(77), kind);
        assert!(app.create_session_quick().is_empty());
        assert_eq!(app.pending_requests.len(), 1);
        app.pending_requests.clear();
        let created = requests(app.create_session_quick());
        assert_eq!(created.len(), 1);
        assert_eq!(created[0].method, "session.create");
    }
}

#[test]
fn startup_send_failure_keeps_draft_and_does_not_retry() {
    let (mut app, request) = create(prefs());
    app.composer.type_text("never lose this draft");
    assert!(
        app.update(AppEvent::RpcSendFailed {
            id: request.id,
            error: RpcError::RequestTooLarge {
                actual_bytes: 2,
                max_bytes: 1
            },
        })
        .is_empty()
    );
    assert!(!app.startup_create_pending());
    assert_eq!(app.composer.content(), "never lose this draft");
    assert!(app.notices.back().unwrap().text.contains("/new form"));
    assert!(app.bootstrap_progress(BootstrapPart::Ping).is_empty());
    assert!(app.update(AppEvent::Tick).is_empty());
}

#[test]
fn startup_adopts_an_open_scratch_palette_with_its_unchanged_editor() {
    let (mut app, request) = create(prefs());
    app.composer.type_text("/mo");
    ui_actions::refresh_slash_completion(&mut app);
    let before = app.slash_completion.as_ref().expect("scratch palette");
    let range = (before.start, before.end, before.selected);
    let revision = app.composer.editor_revision();
    reply(&mut app, &request, session());
    assert_eq!(app.composer.content(), "/mo");
    assert_eq!(app.composer.editor_revision(), revision);
    let after = app
        .slash_completion
        .as_ref()
        .expect("palette follows adopted scratch editor");
    assert_eq!((after.start, after.end, after.selected), range);
    assert_eq!(after.source_revision, revision);
    assert_eq!(after.session_owner.as_deref(), Some("ses_default"));
}

#[test]
fn startup_adoption_preserves_palette_dismissal_until_the_draft_changes() {
    let (mut app, request) = create(prefs());
    app.composer.type_text("/");
    app.refresh_slash_completion();
    assert!(key(&mut app, KeyCode::Esc).is_empty());
    assert!(app.slash_completion.is_none());
    assert_eq!(app.slash_dismissed_text.as_deref(), Some("/"));
    reply(&mut app, &request, session());
    for code in [KeyCode::Left, KeyCode::Right] {
        assert!(key(&mut app, code).is_empty());
        assert!(app.slash_completion.is_none());
    }
    app.refresh_slash_completion();
    assert!(app.slash_completion.is_none());
    assert_eq!(app.composer.content(), "/");
    assert!(key(&mut app, KeyCode::Char('m')).is_empty());
    assert!(app.slash_completion.is_some());
}

#[test]
fn startup_pending_allows_group_navigation_but_keeps_mutations_blocked() {
    let (mut app, request) = create(prefs());
    app.composer.set_text("/session");
    assert!(app.submit_composer().is_empty());
    assert_eq!(app.composer.content(), "/session ");
    let completion = app.slash_completion.as_ref().unwrap();
    assert_eq!(
        completion.group,
        Some(crate::command::CommandGroup::Session)
    );
    assert!(completion.session_owner.is_none());
    app.composer.set_text("/session new");
    app.refresh_slash_completion();
    assert!(key(&mut app, KeyCode::Enter).is_empty());
    assert!(app.startup_create_pending());
    assert_eq!(app.composer.content(), "/session new");
    assert_eq!(app.pending_requests.len(), 1);
    app.composer.set_text("/session ");
    app.refresh_slash_completion();
    reply(&mut app, &request, session());
    assert_eq!(app.composer.content(), "/session ");
    assert_eq!(
        app.slash_completion.as_ref().unwrap().group,
        Some(crate::command::CommandGroup::Session)
    );
    assert!(key(&mut app, KeyCode::Esc).is_empty());
    assert_eq!(app.composer.content(), "/session ");
}

#[test]
fn startup_success_and_explicit_recovery_clear_only_owned_startup_notices() {
    let (mut app, request) = create(prefs());
    app.composer.type_text("kept through failure");
    key(&mut app, KeyCode::Enter);
    assert!(app.notices.iter().any(|n| n.text == STARTUP_PENDING_NOTICE));
    reject(&mut app, &request);
    assert!(
        app.notices
            .iter()
            .any(|n| n.text.starts_with(STARTUP_FAILURE_PREFIX))
    );
    app.sticky_notice(NoticeLevel::Warning, "unrelated warning must remain");
    app.open_new_session();
    let create = requests(app.submit_new_session()).remove(0);
    reply(&mut app, &create, session());
    assert!(
        app.notices.iter().all(
            |n| n.text != STARTUP_PENDING_NOTICE && !n.text.starts_with(STARTUP_FAILURE_PREFIX)
        )
    );
    assert!(
        app.notices
            .iter()
            .any(|n| n.text == "unrelated warning must remain")
    );
}

#[test]
fn startup_ack_adopts_explicit_popup_filter_without_executing_its_candidate() {
    let (mut app, request) = create(prefs());
    app.composer.set_text("/session");
    app.refresh_slash_completion();
    assert!(key(&mut app, KeyCode::Enter).is_empty());
    for c in "new".chars() {
        assert!(key(&mut app, KeyCode::Char(c)).is_empty());
    }
    let before = app.slash_completion.clone().unwrap();
    assert!(before.popup.is_some());
    let revision = app.composer.editor_revision();
    reply(&mut app, &request, session());
    let after = app.slash_completion.as_ref().unwrap();
    assert_eq!(after.popup, before.popup);
    assert_eq!(after.filter, "new");
    assert_eq!(after.source_revision, revision);
    assert_eq!(after.session_owner.as_deref(), Some("ses_default"));
    assert_eq!(app.composer.content(), "/session");
    assert!(key(&mut app, KeyCode::Tab).is_empty());
    assert_eq!(app.composer.content(), "/session new ");
    assert!(
        !app.pending_requests
            .values()
            .any(|kind| matches!(kind, RequestKind::CreateSession { .. }))
    );
}
