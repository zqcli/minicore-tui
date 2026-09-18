//! Stage-A backpressure baseline (Spec §5.2, §25).
//!
//! The current v0.2.8 UI admission path is `RpcProcess::send`, which is
//! `async` and awaits a bounded 64-slot channel. When that channel is full and
//! the child never drains stdin, the send future does not complete. This test
//! reproduces that with a real spawned child that ignores its stdin, so the
//! stage B `try_send` conversion has a concrete before/after.
//!
//! It never enters a terminal and never talks to a real Agent.

#![cfg(unix)]

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use minicore_tui::protocol::{OutgoingRequest, RequestId};
use minicore_tui::rpc::RpcProcess;

/// Real reproduction: a child that never reads stdin leaves the 64-slot
/// outbound channel full; the 65th `send().await` must be still pending after
/// a short window, proving `App::update`'s caller would block on the UI path.
#[tokio::test]
async fn baseline_send_blocks_when_the_outbound_queue_is_full() {
    let script = TempScript::new();
    let config = TempConfig::new("baseline-bp", "unused");
    let process = RpcProcess::spawn(&script.path, &config.path).expect("spawn stalling child");

    // Fill the 64-slot queue with ~512 KiB requests. The child never reads, so
    // the OS pipe buffer also fills; after roughly 65 sends the queue-plus-pipe
    // is saturated.
    let body = "x".repeat(512 * 1024);
    let mut accepted = 0u64;
    for id in 0..80u64 {
        let request = OutgoingRequest::new(
            RequestId(id + 1),
            "turn.send",
            serde_json::json!({"session_id": "ses", "text": body}),
        );
        match tokio::time::timeout(Duration::from_millis(200), process.send(request)).await {
            Ok(Ok(())) => accepted += 1,
            Ok(Err(error)) => panic!("unexpected send error: {error}"),
            Err(_) => {
                // The queue and pipe are full: the current awaiting send has
                // no non-blocking admission, which is the defect.
                assert!(
                    accepted >= 64,
                    "only {accepted} sends were accepted before blocking"
                );
                return;
            }
        }
    }
    panic!("BASELINE: send never blocked even after 80 x 512 KiB requests");
}

/// The current queue capacity is a fixed 64 slots with no reserved control
/// class. Stage B changes this to 32 with 28 ordinary + 4 control.
#[test]
fn baseline_queue_capacity_constant() {
    let source = include_str!("../src/rpc.rs");
    assert!(
        source.contains("const REQUESTS_CHANNEL_CAPACITY: usize = 64;"),
        "BASELINE: outbound queue is 64 with no control reserve"
    );
    assert!(
        !source.contains("SEND_QUEUE_CAPACITY") && !source.contains("CONTROL_RESERVE"),
        "BASELINE: no named 28+4 split exists yet"
    );
}

/// The only current fast-fail path is the 1 MiB outbound line bound. Stage B's
/// `try_send` must also enforce it before the write.
#[test]
fn baseline_oversized_request_is_rejected_synchronously() {
    let source = include_str!("../src/rpc.rs");
    assert!(
        source.contains("pub const MAX_REQUEST_LINE_BYTES: usize = 1024 * 1024;"),
        "BASELINE: 1 MiB outbound line bound exists"
    );
    assert!(
        source.contains("RpcError::RequestTooLarge") || source.contains("RequestTooLarge {"),
        "BASELINE: oversized requests fail with a typed error"
    );
}

/// A shell script that ignores its arguments and sleeps, so it never reads the
/// request channel. Removes itself on drop.
struct TempScript {
    path: PathBuf,
}

impl TempScript {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("mct-baseline-bp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("stall-agent.sh");
        let mut file = std::fs::File::create(&path).expect("create script");
        writeln!(file, "#!/bin/sh\nsleep 60").expect("write script");
        drop(file);
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        Self { path }
    }
}

impl Drop for TempScript {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_dir(
            std::env::temp_dir().join(format!("mct-baseline-bp-{}", std::process::id())),
        );
    }
}

struct TempConfig {
    path: PathBuf,
}

impl TempConfig {
    fn new(prefix: &str, content: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("mct-baseline-bp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join(format!("{prefix}.toml"));
        std::fs::write(&path, content).expect("write temp config");
        Self { path }
    }
}

impl Drop for TempConfig {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_dir(
            std::env::temp_dir().join(format!("mct-baseline-bp-{}", std::process::id())),
        );
    }
}
