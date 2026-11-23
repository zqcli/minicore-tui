use super::*;

fn directory() -> (tempfile::TempDir, ConfigDirectory) {
    let temp = tempfile::tempdir().unwrap();
    let directory =
        ConfigDirectory::resolve(Some(temp.path()), Some(&temp.path().join("runtime"))).unwrap();
    (temp, directory)
}

#[test]
fn first_run_uses_the_shared_host_skeleton_without_creating_runtime_records() {
    let (temp, directory) = directory();
    let Prepared::FirstRun(message) = prepare(&directory, Some(temp.path())).unwrap() else {
        panic!("expected first-run instructions");
    };
    assert!(message.contains("chat_completions or responses"));
    let loaded = minicore_host_config::load_config_directory(directory.dir()).unwrap();
    assert_eq!(
        loaded.host.models[0].model,
        loaded.agents[0].config.session_defaults.model
    );
    assert_eq!(
        loaded.agents[0]
            .config
            .session_defaults
            .workspace
            .as_ref()
            .unwrap()
            .path,
        std::fs::canonicalize(temp.path())
            .unwrap()
            .to_string_lossy()
    );
    assert_eq!(load_ui(&directory).unwrap().theme, None);
    assert!(directory.commands_dir().is_dir());
    assert!(!directory.data_root().exists());
}

#[test]
fn scaffold_preserves_existing_host_and_ui_documents() {
    let (temp, directory) = directory();
    let host_file = directory.dir().join("host.json");
    std::fs::write(&host_file, "preserve this Host document").unwrap();
    std::fs::write(directory.ui_file(), r#"{"ui":{"bell":false}}"#).unwrap();
    scaffold::write(&directory, Some(temp.path())).unwrap();
    assert_eq!(
        std::fs::read_to_string(host_file).unwrap(),
        "preserve this Host document"
    );
    assert_eq!(
        std::fs::read_to_string(directory.ui_file()).unwrap(),
        r#"{"ui":{"bell":false}}"#
    );
}

#[test]
fn preparation_does_not_parse_business_files_or_touch_host_records() {
    let (_temp, directory) = directory();
    std::fs::create_dir_all(directory.data_root()).unwrap();
    let record = directory.data_root().join("host.json");
    std::fs::write(&record, "owned by Host").unwrap();
    std::fs::write(directory.dir().join("host.json"), "{ invalid Host JSON").unwrap();
    std::fs::create_dir(directory.dir().join("agents")).unwrap();
    std::fs::write(
        directory.dir().join("agents").join("agent.json"),
        "{ invalid Agent JSON",
    )
    .unwrap();
    let Prepared::Launch(launch) = prepare(&directory, None).unwrap() else {
        panic!("Host, not TUI preparation, loads business files");
    };
    assert_eq!(launch.config_dir, directory.dir());
    assert_eq!(launch.data_root, directory.data_root());
    assert_eq!(std::fs::read_to_string(record).unwrap(), "owned by Host");
}

#[test]
fn terminal_json_rejects_unknown_settings_and_reports_its_own_path() {
    let (_temp, directory) = directory();
    for invalid in [
        r#"{"models":[]}"#,
        r#"{"ui":{"model":"provider/model"}}"#,
        r#"{"ui":{"theme":"blue"}}"#,
        "{\n invalid JSON",
    ] {
        std::fs::write(directory.ui_file(), invalid).unwrap();
        let error = load_ui(&directory).unwrap_err().to_string();
        assert!(error.contains("tui.json:"));
    }
}

#[test]
fn missing_ui_uses_defaults_and_obsolete_toml_is_not_read() {
    let (_temp, directory) = directory();
    std::fs::write(
        directory.dir().join("config.toml"),
        "[ui]\ntheme = 'light'\n",
    )
    .unwrap();
    assert_eq!(load_ui(&directory).unwrap().theme, None);
}

#[test]
fn saving_theme_preserves_editor_and_bell_and_does_not_touch_host_config() {
    let (_temp, directory) = directory();
    let host_file = directory.dir().join("host.json");
    std::fs::write(&host_file, "Host-owned configuration").unwrap();
    std::fs::write(
        directory.ui_file(),
        r#"{"ui":{"bell":false,"editor":["code","--wait"]}}"#,
    )
    .unwrap();
    save_theme(&directory.ui_file(), ThemeKind::Light).unwrap();
    let ui = load_ui(&directory).unwrap();
    assert_eq!(ui.theme, Some(ThemeKind::Light));
    assert!(!ui.bell);
    assert_eq!(ui.editor, ["code", "--wait"]);
    assert_eq!(
        std::fs::read_to_string(host_file).unwrap(),
        "Host-owned configuration"
    );
}

#[test]
fn saving_theme_does_not_replace_invalid_preferences() {
    let (_temp, directory) = directory();
    let invalid = r#"{"ui":{"bell":"invalid"}}"#;
    std::fs::write(directory.ui_file(), invalid).unwrap();
    assert!(save_theme(&directory.ui_file(), ThemeKind::Light).is_err());
    assert_eq!(
        std::fs::read_to_string(directory.ui_file()).unwrap(),
        invalid
    );
}

#[test]
fn saving_theme_can_create_only_terminal_preferences() {
    let temp = tempfile::tempdir().unwrap();
    let directory = ConfigDirectory::resolve(
        Some(&temp.path().join("config")),
        Some(&temp.path().join("data")),
    )
    .unwrap();
    save_theme(&directory.ui_file(), ThemeKind::Light).unwrap();
    assert_eq!(load_ui(&directory).unwrap().theme, Some(ThemeKind::Light));
    assert!(!directory.dir().join("host.json").exists());
    assert!(!directory.data_root().exists());
}
