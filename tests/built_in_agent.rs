//! The TUI-composed config, served by the Agent built into this executable.
use std::path::Path;

use minicore_tui::config::{self, Home, Prepared, Request};
use minicore_tui::protocol::{METHOD_PING, PingResult, validate_backend};
use minicore_tui::rpc::{AGENT_SUBCOMMAND, Client};
use tokio::process::Command;

const CONFIG: &str = "default_profile = \"coding\"\n\n[profiles.coding]\nmodel = \"m\"\n";
const MODELS: &str = r#"[providers.local]
api = "open_ai_responses"
base_url = "http://127.0.0.1:9/v1"
api_key_env = "MINICORE_TUI_TEST_KEY"

[models.m]
provider = "local"
context_window = 64000
max_output_tokens = 8000
"#;

fn compose(dir: &Path) -> std::path::PathBuf {
    std::fs::create_dir_all(dir.join("prompt")).unwrap();
    std::fs::write(dir.join("config.toml"), CONFIG).unwrap();
    std::fs::write(dir.join("models.toml"), MODELS).unwrap();
    std::fs::write(dir.join("prompt/SYSTEM_PROMPT.md"), "Test.\n").unwrap();
    let home = Home::resolve(Some(dir)).unwrap();
    let Ok(Prepared::Launch(launch)) = config::prepare(&home, &Request::default(), &|_| true)
    else {
        panic!("the test home composes");
    };
    assert_eq!(launch.agent_binary, None);
    launch.agent_config
}

#[tokio::test]
async fn the_built_in_agent_serves_the_composed_config() {
    let dir = tempfile::tempdir().unwrap();
    let agent_config = compose(dir.path());
    let mut command = Command::new(env!("CARGO_BIN_EXE_minicore-tui"));
    command
        .arg(AGENT_SUBCOMMAND)
        .arg("--config")
        .arg(&agent_config)
        .arg("--stdio")
        .env("MINICORE_TUI_TEST_KEY", "unused");
    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    let client = Client::spawn(command, tx).unwrap();
    let ping: PingResult = client.call(METHOD_PING, &()).await.unwrap();
    client.shutdown().await;
    assert_eq!(ping.version, minicore_agent::agent_version());
    assert_eq!(validate_backend(&ping), Ok(()));
}
