//! Public-boundary integration tests for `RpcProcess`. These exercise only
//! the published API surface (spawn validation before the terminal, ordinary
//! error types) and deliberately do not reimplement the transport: the full
//! OS pipe/lifecycle pipeline (out-of-order frames, events-before-response,
//! crash EOF, hang-kill, clean shutdown, frame bounds, top-level errors) is
//! pinned by the in-crate suite in `src/rpc.rs`, which drives the production
//! `RpcProcess::from_child` against a scripted fake agent — no extra binary.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use minicore_tui::rpc::RpcProcess;

/// Uniquely-named temp file that removes itself on drop (including panic
/// unwinds), so tests never leak config scratch files and never touch user
/// config or data directories.
struct TempConfig {
    path: std::path::PathBuf,
}

impl TempConfig {
    fn new(prefix: &str, content: &str) -> Self {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!("mct-rpc-io-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join(format!(
            "{prefix}-{}.toml",
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&path, content).expect("write temp config");
        Self { path }
    }
}

impl Drop for TempConfig {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_dir(
            std::env::temp_dir().join(format!("mct-rpc-io-{}", std::process::id())),
        );
    }
}

#[test]
fn spawn_rejects_a_missing_config_with_a_plain_error() {
    let error = RpcProcess::spawn(
        Path::new("minicore-agent"),
        Path::new("/nonexistent/minicore-e2e/agent.toml"),
    )
    .expect_err("a missing config is an ordinary error before the terminal");
    assert!(matches!(
        error,
        minicore_tui::rpc::RpcError::ConfigMissing(_)
    ));
    // The message is user-facing and names the config, without panicking.
    assert!(error.to_string().contains("config file does not exist"));
}

#[test]
fn spawn_reports_an_unusable_agent_binary() {
    let config = TempConfig::new("agent", "mode: serve");
    let error = RpcProcess::spawn(Path::new("/nonexistent/minicore-agent-bin"), &config.path)
        .expect_err("an unusable binary is an ordinary spawn error");
    assert!(matches!(error, minicore_tui::rpc::RpcError::Spawn(_)));
    assert!(error.to_string().contains("failed to spawn"));
}
