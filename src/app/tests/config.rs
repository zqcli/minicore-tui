use serde_json::json;

use super::harness::*;
use crate::config::Home;

const CONFIG: &str =
    "# keep me\ndefault_profile = \"coding\"\n\n[profiles.coding]\nmodel = \"main\"\n";
const MODELS: &str = "[providers.p]\napi = \"open_ai_responses\"\nbase_url = \"https://example.test/v1\"\napi_key_env = \"KEY\"\n\n[models.main]\nprovider = \"p\"\ncontext_window = 100000\nmax_output_tokens = 10000\n";

fn home_with_files() -> (tempfile::TempDir, Home) {
    let dir = tempfile::tempdir().unwrap();
    let home = Home::resolve(Some(dir.path())).unwrap();
    std::fs::create_dir(dir.path().join("prompt")).unwrap();
    std::fs::write(home.config_file(), CONFIG).unwrap();
    std::fs::write(home.models_file(), MODELS).unwrap();
    std::fs::write(home.system_prompt_file(), "Be brief.").unwrap();
    (dir, home)
}

#[tokio::test(start_paused = true)]
async fn the_theme_command_keeps_the_config_file_and_its_comments() {
    let (_dir, home) = home_with_files();
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.app.home = Some(home.clone());
    h.type_text("/theme light");
    h.press(crossterm::event::KeyCode::Enter);
    let text = std::fs::read_to_string(home.config_file()).unwrap();
    assert!(text.starts_with("# keep me\n"), "{text}");
    assert!(text.contains("[ui]\ntheme = \"light\""), "{text}");
}

#[tokio::test(start_paused = true)]
async fn reload_composes_the_agent_config_and_a_rejected_edit_never_reaches_the_agent() {
    let (_dir, home) = home_with_files();
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script("agent.reload", json!({}));
    h.app.home = Some(home.clone());
    h.app.composed = true;

    h.type_text("/reload");
    h.press(crossterm::event::KeyCode::Enter);
    h.settle().await;
    let generated = std::fs::read_to_string(home.agent_config_file()).unwrap();
    assert!(generated.contains("[models.main]"), "{generated}");
    assert_eq!(h.calls("agent.reload").len(), 1);

    std::fs::write(home.system_prompt_file(), "").unwrap();
    h.type_text("/reload");
    h.press(crossterm::event::KeyCode::Enter);
    h.settle().await;
    assert_eq!(h.calls("agent.reload").len(), 1);
    assert!(
        h.notices()
            .iter()
            .any(|text| text.ends_with("system prompt is empty")),
        "{:?}",
        h.notices()
    );
}

#[tokio::test(start_paused = true)]
async fn a_raw_agent_config_is_not_composed_on_reload() {
    let (_dir, home) = home_with_files();
    let mut h = Harness::opened(display_read(0, 0)).await;
    h.fake.script("agent.reload", json!({}));
    h.app.home = Some(home.clone());
    h.type_text("/reload");
    h.press(crossterm::event::KeyCode::Enter);
    h.settle().await;
    assert!(!home.agent_config_file().exists());
    assert_eq!(h.calls("agent.reload").len(), 1);
}
