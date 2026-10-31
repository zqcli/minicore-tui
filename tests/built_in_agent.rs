//! The built-in Agent and the TUI's real subprocess/provider path.
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

#[cfg(unix)]
#[tokio::test]
async fn shutdown_joins_the_child_after_its_ack_before_aborting_pipe_tasks() {
    use std::{future::Future, task::Poll, time::Duration};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    tokio::time::timeout(Duration::from_secs(12), async {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("control.sock");
        let marker = directory.path().join("finished");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let mut command = Command::new("python3");
        command
            .arg(format!(
                "{}/tests/support/shutdown_child.py",
                env!("CARGO_MANIFEST_DIR")
            ))
            .arg(&socket)
            .arg(&marker);
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        let client = Client::spawn(command, tx).unwrap();
        let mut shutdown = Box::pin(client.shutdown());
        std::future::poll_fn(|context| {
            assert!(shutdown.as_mut().poll(context).is_pending());
            Poll::Ready(())
        })
        .await;
        let (mut control, _) = listener.accept().await.unwrap();
        let mut ack = [0];
        control.read_exact(&mut ack).await.unwrap();
        assert_eq!(ack, *b"A");
        let fence: serde_json::Value = client
            .call("test.fence", &serde_json::Value::Null)
            .await
            .unwrap();
        assert_eq!(fence, serde_json::json!({"fenced":true}));
        // The later reply proves the reader delivered the shutdown ACK. Poll
        // only now, while the real child is still held by the explicit gate.
        std::future::poll_fn(|context| {
            assert!(
                shutdown.as_mut().poll(context).is_pending(),
                "shutdown returned at its ACK without joining the child exit owner"
            );
            Poll::Ready(())
        })
        .await;
        assert!(!marker.exists());
        control.write_all(b"R").await.unwrap();
        shutdown.await;
        assert_eq!(
            std::fs::read_to_string(marker).unwrap(),
            "child exit owner finished\n"
        );
    })
    .await
    .expect("bounded child-exit shutdown regression");
}

#[cfg(unix)]
#[tokio::test]
async fn acceptance_consumers_reject_causal_identity_and_steering_order_false_greens() {
    let support = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/support");
    let mut command = Command::new("python3");
    command
        .current_dir(&support)
        .args([
            "-m",
            "unittest",
            "test_agent_observer",
            "test_canonical_observer",
            "-v",
        ])
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .kill_on_drop(true);
    let output = tokio::time::timeout(std::time::Duration::from_secs(30), command.output())
        .await
        .expect("bounded observer consumer regressions")
        .expect("python3 observer consumer regressions");
    assert!(
        output.status.success(),
        "strict observer consumers failed ({}):\n{}{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Run one Python PTY helper with a bounded lifetime and fail on any error.
#[cfg(unix)]
async fn run_helper(script: &str, args: &[&str], deadline: std::time::Duration) {
    use std::{
        os::unix::{fs::PermissionsExt, process::CommandExt},
        process::Stdio,
        time::Duration,
    };
    use tokio::io::AsyncReadExt;

    let evidence_parent = std::env::var_os("MINICORE_E2E_EVIDENCE_DIR");
    let evidence = match &evidence_parent {
        Some(parent) => tempfile::Builder::new()
            .prefix("real-chain-")
            .tempdir_in(parent),
        None => tempfile::Builder::new().prefix("real-chain-").tempdir(),
    }
    .expect("one private evidence directory per real-chain helper");
    std::fs::set_permissions(evidence.path(), std::fs::Permissions::from_mode(0o700))
        .expect("private helper evidence permissions");
    let evidence_path = evidence.path().to_path_buf();
    let _evidence_guard = match evidence_parent {
        Some(_) => {
            let _ = evidence.keep();
            None
        }
        None => Some(evidence),
    };
    let registry = evidence_path.join("process-groups");
    std::fs::File::create(&registry).expect("private child-group registry");
    let mut command = Command::new("python3");
    command.as_std_mut().process_group(0);
    command
        .arg(format!(
            "{}/tests/support/{script}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .arg(env!("CARGO_BIN_EXE_minicore-tui"))
        .args(args)
        .env("MINICORE_E2E_PROCESS_REGISTRY", &registry)
        .env("MINICORE_E2E_EVIDENCE_DIR", &evidence_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(feature = "e2e-observe")]
    command.env("MINICORE_E2E_CAPTURE_AGENT", "1");
    let mut child = command
        .spawn()
        .expect("python3 is required for the Unix real-chain tests");
    // Drain both pipes while the helper runs: waiting first can deadlock when
    // either pipe fills, even though the process itself is healthy.
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let output = tokio::spawn(async move {
        let mut bytes = Vec::new();
        stdout.take(1024 * 1024 + 1).read_to_end(&mut bytes).await?;
        Ok::<_, std::io::Error>(bytes)
    });
    let errors = tokio::spawn(async move {
        let mut bytes = Vec::new();
        stderr.take(1024 * 1024 + 1).read_to_end(&mut bytes).await?;
        Ok::<_, std::io::Error>(bytes)
    });
    let helper_id = child.id().expect("running helper");
    std::fs::write(evidence_path.join("helper-group"), format!("{helper_id}\n"))
        .expect("record actual helper group for outer timeout cleanup");
    let waited = tokio::time::timeout(deadline, child.wait()).await;
    let timed_out = waited.is_err();
    if timed_out {
        let _ = tokio::time::timeout(
            Duration::from_secs(2),
            Command::new("kill")
                .args(["-TERM", "--", &format!("-{helper_id}")])
                .status(),
        )
        .await;
        let _ = tokio::time::timeout(Duration::from_secs(8), child.wait()).await;
        // The PTY leader creates a separate session. Killing only the Python
        // helper/group cannot clean it; use its private registered group IDs.
        let groups = std::fs::read_to_string(&registry).unwrap_or_default();
        for id in groups
            .lines()
            .filter_map(|line| line.parse::<u32>().ok())
            .chain(std::iter::once(helper_id))
            .filter(|id| *id > 1)
        {
            let _ = tokio::time::timeout(
                Duration::from_secs(2),
                Command::new("kill")
                    .args(["-KILL", "--", &format!("-{id}")])
                    .status(),
            )
            .await;
        }
        let _ = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
    }
    let stdout = tokio::time::timeout(Duration::from_secs(3), output)
        .await
        .expect("helper stdout closes")
        .unwrap()
        .unwrap();
    let stderr = tokio::time::timeout(Duration::from_secs(3), errors)
        .await
        .expect("helper stderr closes")
        .unwrap()
        .unwrap();
    assert!(
        stdout.len() <= 1024 * 1024 && stderr.len() <= 1024 * 1024,
        "helper output exceeded its evidence budget"
    );
    assert!(
        !timed_out,
        "real-chain helper exceeded its outer deadline:\n{}{}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr)
    );
    let status = waited.unwrap().expect("wait for real-chain helper");
    assert!(
        status.success(),
        "helper {script} {args:?} failed ({status}):\n{}{}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr),
    );
    print!("{}", String::from_utf8_lossy(&stdout));
}

#[cfg(unix)]
async fn real_chain(case: &str) {
    run_helper("real_chain.py", &[case], std::time::Duration::from_secs(85)).await;
}

#[cfg(unix)]
#[tokio::test]
async fn tui_real_chain_completes_a_short_answer() {
    real_chain("answer").await;
}

#[cfg(unix)]
#[tokio::test]
async fn tui_real_chain_runs_a_read_tool() {
    real_chain("tool").await;
}

#[cfg(unix)]
#[tokio::test]
async fn tui_real_chain_denies_an_approval() {
    real_chain("approval").await;
}

#[cfg(unix)]
#[tokio::test]
async fn tui_real_chain_reports_a_provider_error() {
    real_chain("error").await;
}

#[cfg(unix)]
#[tokio::test]
async fn tui_real_chain_cancels_a_stream() {
    real_chain("cancel").await;
}

/// A gated two-turn run: the provider is held across a real turn boundary.
#[cfg(unix)]
#[tokio::test]
async fn tui_real_chain_holds_and_releases_across_two_turns() {
    run_helper(
        "real_chain_load.py",
        &["integrity", "--screen-only"],
        std::time::Duration::from_secs(180),
    )
    .await;
}

#[cfg(all(unix, feature = "e2e-observe"))]
#[tokio::test]
async fn tui_real_chain_verifies_actual_installed_canonical_content() {
    run_helper(
        "real_chain_load.py",
        &["integrity"],
        std::time::Duration::from_secs(180),
    )
    .await;
}

#[cfg(all(unix, feature = "e2e-observe"))]
async fn observed_scenario(case: &str) {
    run_helper(
        "real_chain_scenarios.py",
        &[case, "--count", "2"],
        std::time::Duration::from_secs(180),
    )
    .await;
}

#[cfg(all(unix, feature = "e2e-observe"))]
#[tokio::test]
async fn tui_real_chain_starts_preaccepted_successors_without_another_operation() {
    observed_scenario("automatic").await;
}

#[cfg(all(unix, feature = "e2e-observe"))]
#[tokio::test]
async fn tui_real_chain_discovers_ten_completions_after_main_application_pause() {
    observed_scenario("main-pause").await;
}

#[cfg(all(unix, feature = "e2e-observe"))]
#[tokio::test]
async fn tui_real_chain_steering_keeps_actual_input_bytes_and_request_boundary() {
    observed_scenario("steering").await;
}

#[cfg(all(unix, feature = "e2e-observe"))]
#[tokio::test]
async fn tui_real_chain_completed_idle_does_not_issue_more_result_queries() {
    observed_scenario("idle").await;
}

#[cfg(all(unix, feature = "e2e-observe"))]
#[tokio::test]
async fn tui_real_chain_repeated_provider_errors_install_terminal_results() {
    observed_scenario("error").await;
}

#[cfg(all(unix, feature = "e2e-observe"))]
#[tokio::test]
async fn tui_real_chain_repeated_cancels_preserve_exact_terminal_identity() {
    observed_scenario("cancel").await;
}

#[cfg(all(unix, feature = "e2e-observe"))]
#[tokio::test]
async fn tui_real_chain_installs_full_multipage_answer_bytes() {
    observed_scenario("pagination").await;
}

#[cfg(all(unix, feature = "e2e-observe"))]
#[tokio::test]
async fn tui_real_chain_routes_overlapping_resident_sessions() {
    observed_scenario("resident").await;
}

#[cfg(all(unix, feature = "e2e-observe"))]
#[tokio::test]
async fn tui_real_chain_recovers_after_terminal_read_pause() {
    observed_scenario("slow-terminal").await;
}

#[cfg(all(unix, feature = "e2e-observe"))]
#[tokio::test]
async fn tui_real_chain_nonreading_phone_reconnects_with_full_projection() {
    observed_scenario("slow-phone").await;
}

#[cfg(all(unix, feature = "e2e-observe"))]
async fn observed_fault(case: &str) {
    run_helper(
        "real_chain_faults.py",
        &[case],
        std::time::Duration::from_secs(180),
    )
    .await;
}

#[cfg(all(unix, feature = "e2e-observe"))]
#[tokio::test]
async fn tui_real_chain_rollback_preserves_unsaved_before_resumed_saved_turn() {
    observed_fault("save-rollback").await;
}

#[cfg(all(unix, feature = "e2e-observe"))]
#[tokio::test]
async fn tui_real_chain_unknown_append_stays_visibly_blocked_and_uncertain() {
    observed_fault("save-unknown").await;
}

#[cfg(all(unix, feature = "e2e-observe"))]
#[tokio::test]
async fn tui_real_chain_persistence_pause_preserves_finishing_then_successor() {
    observed_fault("before-persist").await;
}

#[cfg(all(unix, feature = "e2e-observe"))]
#[tokio::test]
async fn tui_real_chain_repairs_terminal_runtime_loss_without_a_later_gap() {
    observed_fault("tail-loss").await;
}

#[cfg(all(unix, feature = "e2e-observe"))]
#[tokio::test]
async fn tui_real_chain_evicted_page_keeps_charge_and_never_installs_a_prefix() {
    observed_fault("page-pressure").await;
}

#[cfg(all(unix, feature = "e2e-observe"))]
#[tokio::test]
async fn tui_real_chain_unsaved_unavailability_is_explicit_not_empty_canonical() {
    observed_fault("unavailable").await;
}

#[cfg(all(unix, feature = "e2e-observe"))]
#[tokio::test]
async fn tui_real_phone_reconnect_preserves_actual_unavailable_recovery_and_saving() {
    observed_fault("phone-recovery").await;
}

#[cfg(all(unix, feature = "e2e-observe"))]
#[tokio::test]
async fn tui_real_chain_large_unsaved_pages_reuse_one_measured_source() {
    observed_fault("large-unsaved").await;
}

#[cfg(all(unix, feature = "e2e-observe"))]
#[tokio::test]
async fn tui_real_chain_ungated_followups_are_actual_completion_successors() {
    run_helper(
        "real_chain_natural.py",
        &["successor", "--count", "2"],
        std::time::Duration::from_secs(180),
    )
    .await;
}

#[cfg(all(unix, feature = "e2e-observe"))]
#[tokio::test]
async fn tui_real_chain_text_input_answers_the_same_runtime_tool_without_trimming() {
    run_helper(
        "real_chain_input.py",
        &["input-text", "--count", "2"],
        std::time::Duration::from_secs(180),
    )
    .await;
}

#[cfg(all(unix, feature = "e2e-observe"))]
#[tokio::test]
async fn tui_real_chain_choice_input_answers_the_same_runtime_tool() {
    run_helper(
        "real_chain_input.py",
        &["input-choice", "--count", "2"],
        std::time::Duration::from_secs(180),
    )
    .await;
}

#[cfg(all(unix, feature = "e2e-observe"))]
#[tokio::test]
async fn tui_real_chain_actual_phone_writer_overflow_closes_then_reconnects_fully() {
    run_helper(
        "real_chain_phone_pressure.py",
        &["phone-overflow"],
        std::time::Duration::from_secs(180),
    )
    .await;
}

/// The natural-load acceptance run: at least 100 completions per class with no
/// fault injection. Opt-in because it drives a long real session, and separate
/// from the deterministic fault cases so the counts stay honest.
#[cfg(unix)]
#[tokio::test]
#[ignore = "natural-load: build --features e2e-observe and set MINICORE_TUI_NATURAL=short|long|tools|approvals"]
async fn tui_real_chain_natural_load() {
    let class = match std::env::var("MINICORE_TUI_NATURAL").as_deref() {
        Ok("short") => &["natural", "--count", "100"][..],
        Ok("long") => &["natural", "--count", "100", "--long-stream"][..],
        Ok("tools") => &["natural", "--count", "100", "--tool-every"][..],
        Ok("approvals") => &["approvals", "--count", "100"][..],
        other => panic!("select an implemented natural class, got {other:?}"),
    };
    let mut args = Vec::new();
    args.extend_from_slice(class);
    run_helper(
        "real_chain_load.py",
        &args,
        std::time::Duration::from_secs(3600),
    )
    .await;
}
