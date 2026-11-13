use super::*;

fn home() -> (tempfile::TempDir, Home) {
    let temp = tempfile::tempdir().unwrap();
    let home = Home::resolve(Some(temp.path())).unwrap();
    (temp, home)
}

#[test]
fn first_run_writes_native_templates_without_creating_host_records() {
    let (_temp, home) = home();
    let Prepared::FirstRun(message) = prepare(&home, &Request::default()).unwrap() else {
        panic!("expected first-run instructions");
    };
    assert!(message.contains("chat_completions or responses"));
    assert!(message.contains("not imported or modified"));
    let config = load_host_config(&home.host_config_file()).unwrap();
    let agent = load_new_agent(&home).unwrap();
    assert_eq!(config.models[0].model, agent.session_defaults.model);
    assert!(!home.data_root().exists());
    assert!(!home.dir().join("models.toml").exists());
    assert!(!home.dir().join("session").exists());
}

#[test]
fn native_provider_api_is_required_and_not_inferred_from_endpoint() {
    let (_temp, home) = home();
    scaffold::write(&home).unwrap();
    let path = home.host_config_file();
    let mut value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    value["models"][0].as_object_mut().unwrap().remove("api");
    std::fs::write(&path, serde_json::to_string(&value).unwrap()).unwrap();
    assert!(
        load_host_config(&path)
            .unwrap_err()
            .to_string()
            .contains("api")
    );
    value["models"][0]["api"] = "responses".into();
    std::fs::write(&path, serde_json::to_string(&value).unwrap()).unwrap();
    assert_eq!(
        load_host_config(&path).unwrap().models[0].api,
        minicore_host_core::OpenAiModelApi::Responses
    );
}

#[test]
fn scaffold_preserves_existing_files_including_role_instructions() {
    let (_temp, home) = home();
    std::fs::write(home.agent_defaults_file(), "keep this user document").unwrap();
    scaffold::write(&home).unwrap();
    assert_eq!(
        std::fs::read_to_string(home.agent_defaults_file()).unwrap(),
        "keep this user document"
    );
}

#[test]
fn launch_never_rewrites_the_owned_host_record_or_old_agent_store() {
    let (_temp, home) = home();
    std::fs::create_dir_all(home.data_root()).unwrap();
    std::fs::write(home.data_root().join("host.json"), "owned by Host").unwrap();
    std::fs::create_dir_all(home.dir().join("session")).unwrap();
    std::fs::write(home.dir().join("session/legacy.data"), "old Agent data").unwrap();
    let Prepared::Launch(launch) = prepare(&home, &Request::default()).unwrap() else {
        panic!("saved Host must launch without scaffolding");
    };
    assert_eq!(launch.data_root, home.data_root());
    assert_eq!(
        std::fs::read_to_string(home.data_root().join("host.json")).unwrap(),
        "owned by Host"
    );
    assert_eq!(
        std::fs::read_to_string(home.dir().join("session/legacy.data")).unwrap(),
        "old Agent data"
    );
    assert!(!home.host_config_file().exists());
}

#[test]
fn explicit_initial_config_is_parsed_even_when_saved_host_exists() {
    let (_temp, home) = home();
    std::fs::create_dir_all(home.data_root()).unwrap();
    std::fs::write(home.data_root().join("host.json"), "owned by Host").unwrap();
    let file = home.dir().join("invalid.json");
    std::fs::write(&file, "{").unwrap();
    let error = prepare(
        &home,
        &Request {
            host_config: Some(&file),
            ..Request::default()
        },
    )
    .err()
    .unwrap();
    assert!(error.contains("invalid.json:1:"));
    assert_eq!(
        std::fs::read_to_string(home.data_root().join("host.json")).unwrap(),
        "owned by Host"
    );
}

#[test]
fn old_profile_and_remote_configuration_fail_with_migration_guidance() {
    let (_temp, home) = home();
    for old in ["default_profile = 'coding'", "[remote]\nenabled = true"] {
        std::fs::write(home.config_file(), old).unwrap();
        let error = load_ui(&home).unwrap_err().to_string();
        assert!(error.contains("config.toml"));
        assert!(error.contains("only [ui]"));
        assert!(error.contains("not migrated"));
    }
}

#[test]
fn role_agent_defaults_and_session_overrides_use_native_inheritance() {
    use minicore_host_protocol::{SessionConfig, SessionConfigPatch, SessionOverrides};
    let (_temp, home) = home();
    scaffold::write(&home).unwrap();
    let agent = load_new_agent(&home).unwrap();
    let session = SessionConfig::from_agent(
        &agent,
        &SessionOverrides(SessionConfigPatch {
            model: Some("other/model".parse().unwrap()),
            ..SessionConfigPatch::default()
        }),
    )
    .unwrap();
    assert_eq!(session.model.as_str(), "other/model");
    assert_eq!(session.role, agent.role);
    assert_eq!(session.tools, agent.session_defaults.tools);
    assert_eq!(agent.session_defaults.model.as_str(), "provider/chat");
}

#[test]
fn saving_theme_keeps_comments_editor_and_bell_preferences() {
    let (_temp, home) = home();
    std::fs::write(
        home.config_file(),
        "# retain me\n[ui]\nbell = false\neditor = ['code', '--wait']\n",
    )
    .unwrap();
    save_theme(&home.config_file(), ThemeKind::Light).unwrap();
    let text = std::fs::read_to_string(home.config_file()).unwrap();
    assert!(text.contains("# retain me"));
    let ui = load_ui(&home).unwrap();
    assert_eq!(ui.theme, ThemeKind::Light);
    assert!(!ui.bell);
    assert_eq!(ui.editor, ["code", "--wait"]);
}

#[test]
fn saving_theme_can_create_preferences_for_an_existing_host_in_a_new_tui_home() {
    let temp = tempfile::tempdir().unwrap();
    let home = Home::resolve(Some(&temp.path().join("new-tui-home"))).unwrap();
    save_theme(&home.config_file(), ThemeKind::Light).unwrap();
    assert_eq!(load_ui(&home).unwrap().theme, ThemeKind::Light);
    assert!(!home.host_config_file().exists());
    assert!(!home.data_root().exists());
}
