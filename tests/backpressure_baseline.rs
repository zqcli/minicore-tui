//! Outbound admission and backpressure (Spec §5.2, REF-05/06/REF-50).
//!
//! These tests drive the real `RpcProcess` around a spawned child that never
//! reads its stdin. They prove the UI-path admission is synchronous and
//! bounded: ordinary requests stop at 28 queued slots, four more are reserved
//! for control, and a refused request is never written (so it may be retried
//! or restored without any unknown-write ambiguity).
//!
//! They never enter a terminal and never talk to a real Agent.

#![cfg(unix)]

use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use minicore_tui::protocol::{OutgoingRequest, RequestId};
use minicore_tui::rpc::{
    OUTBOUND_NORMAL_CAPACITY, OUTBOUND_QUEUE_CAPACITY, RpcProcess, SendClass, SendError,
};

fn request(id: u64, method: &'static str, body: &str) -> OutgoingRequest {
    OutgoingRequest::new(
        RequestId(id),
        method,
        serde_json::json!({"session_id": "ses", "text": body}),
    )
}

/// A child that never reads stdin: the writer task blocks in the OS pipe, so
/// only the bounded in-process queue can absorb requests.
fn stalled_process() -> (RpcProcess, TempScript, TempConfig) {
    let script = TempScript::new();
    let config = TempConfig::new(script.path.parent().expect("script dir"), "unused");
    let process = RpcProcess::spawn(&script.path, &config.path).expect("spawn stalling child");
    (process, script, config)
}

#[tokio::test]
async fn normal_admission_is_synchronous_and_bounded_at_28() {
    let (process, _script, _config) = stalled_process();
    let body = "x".repeat(512 * 1024);
    let started = Instant::now();
    for id in 0..OUTBOUND_NORMAL_CAPACITY as u64 {
        process
            .try_send(request(id + 1, "turn.send", &body), SendClass::Normal)
            .expect("a normal slot is free");
        // Admission must never wait for the writer or the child.
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "admission must not block on the stalled writer"
        );
    }
    match process.try_send(request(99, "turn.send", &body), SendClass::Normal) {
        Err(SendError::QueueFull(SendClass::Normal)) => {}
        other => panic!("expected a full normal class, got {other:?}"),
    }
    process.kill_child();
}

#[tokio::test]
async fn four_control_slots_stay_reserved_after_the_normal_class_is_full() {
    let (process, _script, _config) = stalled_process();
    let body = "x".repeat(512 * 1024);
    for id in 0..OUTBOUND_NORMAL_CAPACITY as u64 {
        process
            .try_send(request(id + 1, "turn.send", &body), SendClass::Normal)
            .expect("a normal slot is free");
    }
    let reserved = OUTBOUND_QUEUE_CAPACITY - OUTBOUND_NORMAL_CAPACITY;
    assert_eq!(reserved, 4, "the reserve is explicit");
    for id in 0..reserved as u64 {
        process
            .try_send(request(200 + id, "turn.cancel", &body), SendClass::Control)
            .expect("a reserved control slot is free");
    }
    match process.try_send(request(300, "turn.cancel", &body), SendClass::Control) {
        Err(SendError::QueueFull(SendClass::Control)) => {}
        other => panic!("expected a full control class, got {other:?}"),
    }
    process.kill_child();
}

#[tokio::test]
async fn an_oversized_line_is_rejected_before_any_write() {
    let (process, _script, _config) = stalled_process();
    let body = "x".repeat(1024 * 1024 + 1);
    match process.try_send(request(1, "turn.send", &body), SendClass::Normal) {
        Err(SendError::RequestTooLarge {
            actual_bytes,
            max_bytes,
        }) => {
            assert!(actual_bytes > max_bytes);
            assert_eq!(max_bytes, 1024 * 1024);
        }
        other => panic!("expected a local size rejection, got {other:?}"),
    }
    process.kill_child();
}

/// The UI command path must never await the writer or the clipboard.
#[test]
fn ui_command_dispatch_uses_synchronous_admission_and_owned_jobs() {
    let main = include_str!("../src/main.rs");
    // Test helpers may still await `send` to drive a fake child; only the
    // production (pre-`#[cfg(test)]`) half must stay synchronous.
    let production = main
        .split("#[cfg(test)]")
        .next()
        .expect("main.rs has a production half");
    assert!(
        production.contains("process.try_send("),
        "the UI path admits synchronously"
    );
    assert!(
        !production.contains("process.send("),
        "the UI path must not await the outbound queue"
    );
    assert!(
        main.contains("jobs.copy_to_clipboard("),
        "the clipboard runs as an owned job"
    );
    let rpc = include_str!("../src/rpc.rs");
    assert!(rpc.contains("pub const OUTBOUND_QUEUE_CAPACITY: usize = 32;"));
    assert!(rpc.contains("pub const OUTBOUND_NORMAL_CAPACITY: usize = 28;"));
    assert!(rpc.contains("pub const MAX_WIRE_BUDGET_BYTES: usize = 64 * 1024 * 1024;"));
}

/// A shell script that ignores its arguments and sleeps, so it never reads the
/// request channel. The parent directory also holds the temp config and is
/// removed on drop.
struct TempScript {
    path: PathBuf,
}

impl TempScript {
    fn new() -> Self {
        let dir = unique_temp_dir();
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
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::remove_dir(dir);
        }
    }
}

/// Tests in one binary run concurrently; each needs its own directory, or
/// writing a script while another test executes it fails with ETXTBSY.
fn unique_temp_dir() -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "mct-bp-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

struct TempConfig {
    path: PathBuf,
}

impl TempConfig {
    fn new(dir: &std::path::Path, content: &str) -> Self {
        let path = dir.join("config.toml");
        std::fs::write(&path, content).expect("write temp config");
        Self { path }
    }
}

impl Drop for TempConfig {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
