//! C2b structural performance probes (Spec §11 and §25).
//!
//! The baseline helpers intentionally use the synchronous test harness. The
//! production path is exercised separately through `LocalJobs`' single owned
//! layout worker. Run the ignored acceptance probes with:
//!
//! ```text
//! cargo test --release --locked --test performance -- --ignored --nocapture
//! ```
//!
//! The structural numbers here feed `docs/performance.md`. Counts are measured
//! through public behavior (prepared row counts, live-delta preparation row
//! counts), never through a hidden telemetry hook.
//!
//! Two paths are distinguished deliberately:
//!
//! * **production frame path** — `main::prepare_frame_with_jobs` requests a
//!   bounded worker result and installs it through `AppEvent::DurableLayoutPrepared`;
//!   a live-only delta reuses the immutable durable cache and composes only the
//!   live tail.
//! * **diagnostic helper** — `ui::transcript::all_lines` clones the already
//!   prepared rows. It is not the per-frame cost; it is a measurement and test
//!   helper. Do not cite it as the production frame cost.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use minicore_tui::app::{App, ConnectionState};
use minicore_tui::clipboard::ClipboardPort;
#[cfg(all(unix, not(target_os = "macos")))]
use minicore_tui::clipboard::NativeClipboard;
#[cfg(all(unix, not(target_os = "macos")))]
use minicore_tui::command::AppCommand;
use minicore_tui::event::{AppEvent, JobOutcome, RpcEvent};
use minicore_tui::jobs::{CopyAdmission, LocalJobs};
#[cfg(all(unix, not(target_os = "macos")))]
use minicore_tui::protocol::RpcResponse;
use minicore_tui::protocol::{IncomingFrame, RpcNotification, TurnRef, UserMessageKindWire};
use minicore_tui::state::session::SessionView;
use minicore_tui::state::tool::{LiveTool, ToolKey, ToolStatus};
use minicore_tui::state::transcript::{
    AssistantBlock, AssistantPart, ToolBlock, TranscriptBlock, UserBlock,
};
use minicore_tui::state::turn::{LiveLoop, LivePart, LiveRequest, LocalSubmissionId};
use minicore_tui::ui::transcript::{all_lines, prepare_conversation, total_lines};
use serde_json::json;

const WIDTH: u16 = 79;

/// Deterministic migration gate for the editor budget. This uses the real
/// App terminal-input path and the real Composer counters, but deliberately
/// makes no wall-clock claim.
#[test]
fn refactor_migration_256k_draft_edit_is_join_free_and_bounded() {
    minicore_tui::perf::reset();
    let mut app = App::new(PathBuf::from("/project"));
    let payload = "x".repeat(minicore_tui::state::composer::MAX_COMPOSER_BYTES - 4096);
    app.update(AppEvent::Terminal(crossterm::event::Event::Paste(payload)));
    let before = minicore_tui::perf::snapshot();
    for _ in 0..2048 {
        app.update(AppEvent::Terminal(crossterm::event::Event::Key(
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('a'),
                crossterm::event::KeyModifiers::empty(),
            ),
        )));
    }
    let after = minicore_tui::perf::snapshot();
    assert_eq!(
        after.composer_full_joins - before.composer_full_joins,
        0,
        "ordinary input after a large paste must not join the whole draft"
    );
    assert_eq!(
        app.composer.byte_len(),
        minicore_tui::state::composer::MAX_COMPOSER_BYTES - 2048,
        "cached draft length must track input deltas"
    );
    assert!(
        app.composer.retained_bytes() <= minicore_tui::limits::COMPOSER_ALL_DRAFTS_BYTES,
        "retained draft capacity estimate must stay inside the all-drafts budget"
    );
}

/// Release-only timing probe for the same production App input path as the
/// deterministic gate above. Its P95 is evidence for local edit processing on
/// the fixed builder, not terminal input-to-frame latency.
#[test]
#[ignore = "Spec 25 Release timing probe; run with --release --ignored --nocapture"]
fn measure_release_256k_draft_edit_p95() {
    minicore_tui::perf::reset();
    let mut app = App::new(PathBuf::from("/project"));
    let payload = "x".repeat(minicore_tui::state::composer::MAX_COMPOSER_BYTES - 8192);
    app.update(AppEvent::Terminal(crossterm::event::Event::Paste(payload)));
    let joins_before_edits = minicore_tui::perf::snapshot().composer_full_joins;
    let mut samples = Vec::with_capacity(4096);
    for _ in 0..4096 {
        let started = Instant::now();
        app.update(AppEvent::Terminal(crossterm::event::Event::Key(
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('a'),
                crossterm::event::KeyModifiers::empty(),
            ),
        )));
        samples.push(started.elapsed());
    }
    samples.sort_unstable();
    let p95 = samples[(samples.len() * 95 / 100).saturating_sub(1)];
    let p99 = samples[(samples.len() * 99 / 100).saturating_sub(1)];
    let counters = minicore_tui::perf::snapshot();
    println!(
        "composer_256k_release: edits={} p95_us={} p99_us={} draft_bytes={} retained_capacity_estimate={} composer_full_joins_delta={}",
        samples.len(),
        p95.as_micros(),
        p99.as_micros(),
        app.composer.byte_len(),
        app.composer.retained_bytes(),
        counters.composer_full_joins - joins_before_edits,
    );
    assert!(
        p95 < Duration::from_millis(30),
        "draft edit P95 exceeded 30 ms"
    );
    assert_eq!(
        counters.composer_full_joins - joins_before_edits,
        0,
        "ordinary edits after the paste must not join the full draft"
    );
}

/// Same direct Composer workload as the independent 9d11ee6 baseline probe.
/// The App reducer probe above remains the production-path measurement; this
/// one exists only to make the before/after editor-container comparison
/// apples-to-apples.
#[test]
#[ignore = "Spec 25 baseline-comparison probe; run with --release --ignored --nocapture"]
fn measure_release_256k_composer_direct_p95() {
    minicore_tui::perf::reset();
    let mut composer = minicore_tui::state::composer::Composer::new();
    let payload = "x".repeat(minicore_tui::state::composer::MAX_COMPOSER_BYTES - 8192);
    assert!(composer.insert_paste(&payload));
    let joins_before_edits = minicore_tui::perf::snapshot().composer_full_joins;
    let mut samples = Vec::with_capacity(4096);
    for _ in 0..4096 {
        let started = Instant::now();
        assert!(composer.type_char('a'));
        samples.push(started.elapsed());
    }
    samples.sort_unstable();
    let p95 = samples[(samples.len() * 95 / 100).saturating_sub(1)];
    let p99 = samples[(samples.len() * 99 / 100).saturating_sub(1)];
    let counters = minicore_tui::perf::snapshot();
    println!(
        "composer_direct_256k_release: edits={} p95_us={} p99_us={} draft_bytes={} retained_capacity_estimate={} composer_full_joins_delta={}",
        samples.len(),
        p95.as_micros(),
        p99.as_micros(),
        composer.byte_len(),
        composer.retained_bytes(),
        counters.composer_full_joins - joins_before_edits,
    );
    assert!(
        p95 < Duration::from_millis(30),
        "direct draft edit P95 exceeded 30 ms"
    );
    assert_eq!(
        counters.composer_full_joins - joins_before_edits,
        0,
        "direct ordinary edits after the paste must not join the full draft"
    );
}

#[derive(Clone)]
struct BlockingClipboard {
    release: Arc<tokio::sync::Notify>,
}

impl ClipboardPort for BlockingClipboard {
    async fn set_text(&mut self, _text: &str) -> std::io::Result<()> {
        self.release.notified().await;
        Ok(())
    }
}

/// The owned clipboard task remains pending while App input, scroll, resize,
/// and an RPC-side stderr observation are reduced. This is a deterministic
/// non-blocking ownership check; the real hung-helper timeout remains in the
/// clipboard unit suite and the PTY harness.
#[tokio::test]
async fn clipboard_job_does_not_block_input_scroll_resize_or_rpc() {
    let release = Arc::new(tokio::sync::Notify::new());
    let mut jobs = LocalJobs::new();
    assert!(matches!(
        jobs.copy_with(
            "ses_perf",
            1,
            "selected text".to_owned(),
            BlockingClipboard {
                release: Arc::clone(&release),
            },
        ),
        CopyAdmission::Started(_)
    ));

    let mut app = app_with_history(200, 240);
    let started = Instant::now();
    app.update(AppEvent::Terminal(crossterm::event::Event::Key(
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::PageDown,
            crossterm::event::KeyModifiers::empty(),
        ),
    )));
    app.update(AppEvent::TerminalSize {
        width: 120,
        height: 40,
    });
    app.update(AppEvent::Rpc(RpcEvent::AgentStderr {
        bytes: 12,
        dropped: 0,
    }));
    assert!(
        started.elapsed() < Duration::from_millis(100),
        "input, scroll, resize, and RPC reduction must not wait on clipboard I/O"
    );

    release.notify_one();
    let event = jobs.events().recv().await.expect("clipboard completion");
    let outcome = match event {
        AppEvent::JobFinished(outcome @ JobOutcome::Clipboard { .. }) => outcome,
        other => panic!("unexpected local completion: {other:?}"),
    };
    jobs.reap_completion(&outcome).await;
    jobs.shutdown().await;
}

/// Runs the production `NativeClipboard`/`LocalJobs` path against a real Linux
/// `xclip` child selected by an isolated PATH. The outer test process owns the
/// fixture directory; the inner test process receives the PATH before Tokio
/// starts, so no test mutates a live process environment. The helper never
/// reads stdin and sleeps for two seconds, which makes the owned child wait
/// observable rather than replacing it with a gated async double.
#[cfg(all(unix, not(target_os = "macos")))]
#[tokio::test]
#[ignore = "Spec 25 real OS clipboard helper and event-loop acceptance"]
async fn real_native_clipboard_helper_keeps_event_loop_live_and_reaps() {
    const CHILD_ENV: &str = "MINICORE_TUI_CLIPBOARD_CHILD";
    const PID_ENV: &str = "MINICORE_TUI_CLIPBOARD_PID_FILE";
    const TEST_NAME: &str = "real_native_clipboard_helper_keeps_event_loop_live_and_reaps";

    if std::env::var_os(CHILD_ENV).is_some() {
        let pid_path = PathBuf::from(
            std::env::var_os(PID_ENV).expect("the clipboard helper PID path is injected"),
        );
        assert_eq!(NativeClipboard::new().program(), "xclip");
        let mut jobs = LocalJobs::new();
        let mut app = app_with_history(200, 240);
        let send = app
            .update(AppEvent::SubmitTurn {
                session_id: "ses_perf".into(),
                text: "running turn".into(),
            })
            .into_iter()
            .find_map(|command| match command {
                AppCommand::Rpc(request) => Some(request),
                _ => None,
            })
            .expect("a running turn owns a send request");
        let turn = TurnRef {
            session_id: "ses_perf".into(),
            loop_id: "loop_clipboard".into(),
        };
        app.update(AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Response(
            RpcResponse {
                id: send.id,
                result: Some(json!({"turn": turn})),
                error: None,
            },
        ))));
        app.update(AppEvent::Terminal(crossterm::event::Event::Paste(
            "draft survives a slow native clipboard".to_owned(),
        )));
        let draft = app.composer.content().to_owned();

        let started = Instant::now();
        assert!(matches!(
            jobs.copy_to_clipboard(
                "ses_perf",
                1,
                "x".repeat(minicore_tui::clipboard::MAX_CLIPBOARD_BYTES),
            ),
            CopyAdmission::Started(_)
        ));
        let pid_deadline = Instant::now() + Duration::from_secs(1);
        while !pid_path.exists() {
            assert!(
                Instant::now() < pid_deadline,
                "real xclip helper did not spawn"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let pid = std::fs::read_to_string(&pid_path)
            .expect("read helper PID")
            .trim()
            .parse::<u32>()
            .expect("helper PID is numeric");
        let status = std::fs::read_to_string(format!("/proc/{pid}/status"))
            .expect("real helper has a proc status");
        assert!(
            status
                .lines()
                .any(|line| line == format!("PPid:\t{}", std::process::id())),
            "PID file must identify the direct child, not an async gate"
        );
        assert!(!status.lines().any(|line| line.starts_with("State:\tZ")));

        let mut saw_cancel = false;
        for _ in 0..40 {
            app.update(AppEvent::Terminal(crossterm::event::Event::Key(
                crossterm::event::KeyEvent::new(
                    crossterm::event::KeyCode::PageDown,
                    crossterm::event::KeyModifiers::empty(),
                ),
            )));
            app.update(AppEvent::TerminalSize {
                width: 120,
                height: 40,
            });
            app.update(AppEvent::Rpc(RpcEvent::AgentStderr {
                bytes: 12,
                dropped: 0,
            }));
            if !saw_cancel {
                let commands = app.update(AppEvent::CancelTurn {
                    session_id: "ses_perf".into(),
                });
                assert!(commands.iter().any(|command| {
                    matches!(command, AppCommand::Rpc(request) if request.method == "turn.cancel")
                }));
                saw_cancel = true;
            }
            assert_eq!(app.composer.content(), draft);
            tokio::task::yield_now().await;
        }
        assert!(saw_cancel, "the exact running turn cancel stayed routable");

        let event = jobs.events().recv().await.expect("clipboard completion");
        let outcome = match event {
            AppEvent::JobFinished(outcome @ JobOutcome::Clipboard { .. }) => outcome,
            other => panic!("unexpected local completion: {other:?}"),
        };
        assert!(started.elapsed() >= Duration::from_millis(1_900));
        match &outcome {
            JobOutcome::Clipboard { result, .. } => {
                assert!(
                    result.is_err(),
                    "the non-reading helper must not claim success"
                );
            }
            _ => unreachable!(),
        }
        jobs.reap_completion(&outcome).await;
        assert!(!PathBuf::from(format!("/proc/{pid}")).exists());
        assert_eq!(
            app.composer.content(),
            draft,
            "clipboard failure keeps the draft"
        );
        println!(
            "native_clipboard_real_helper: pid={} elapsed_ms={} cancel=true draft_bytes={}",
            pid,
            started.elapsed().as_millis(),
            draft.len()
        );
        jobs.shutdown().await;
        return;
    }

    use std::os::unix::fs::PermissionsExt;
    use std::process::{Command, Stdio};
    let directory = tempfile::tempdir().expect("temporary xclip PATH directory");
    let helper = directory.path().join("xclip");
    let pid_path = directory.path().join("xclip.pid");
    std::fs::write(
        &helper,
        format!(
            "#!/bin/sh\nprintf '%s' \"$$\" > '{}'\nexec /bin/sleep 2\n",
            pid_path.display()
        ),
    )
    .expect("write real xclip helper");
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700))
        .expect("make real xclip helper executable");
    let old_path = std::env::var_os("PATH").unwrap_or_default();
    let path = format!(
        "{}:{}",
        directory.path().display(),
        old_path.to_string_lossy()
    );
    let mut child = Command::new(std::env::current_exe().expect("performance test binary"))
        .args(["--exact", TEST_NAME, "--ignored", "--nocapture"])
        .env(CHILD_ENV, "1")
        .env(PID_ENV, &pid_path)
        .env("PATH", path)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn isolated real clipboard test child");
    let status = child.wait().expect("wait for real clipboard test child");
    assert!(
        status.success(),
        "real clipboard event-loop child failed: {status}"
    );
    let pid = std::fs::read_to_string(&pid_path)
        .expect("real clipboard child must leave its PID record")
        .trim()
        .parse::<u32>()
        .expect("real clipboard PID is numeric");
    assert!(
        !PathBuf::from(format!("/proc/{pid}")).exists(),
        "the direct helper must be reaped before the test child exits"
    );
}

/// Builds an active, loaded session whose durable transcript has `messages`
/// assistant blocks, each a Markdown paragraph of roughly `bytes_per_message`.
fn app_with_history(messages: usize, bytes_per_message: usize) -> App {
    let mut app = App::new(PathBuf::from("/project"));
    app.connection = ConnectionState::Ready;
    let info = serde_json::from_value(json!({
        "session_id": "ses_perf",
        "title": "Performance",
        "profile": "coding",
        "workspace": "/project",
        "model": "deep",
        "reasoning": "high",
        "loaded": true,
        "created_at": "2026-01-01T00:00:00Z",
        "updated_at": "2026-01-01T00:00:00Z"
    }))
    .unwrap();
    let mut view = SessionView::new(info);
    view.state = Some(
        serde_json::from_value(json!({
            "session_id": "ses_perf",
            "status": "idle",
            "active_loop": null,
            "block_reason": null
        }))
        .unwrap(),
    );
    let body = "lorem ipsum dolor sit amet ".repeat(bytes_per_message / 27 + 1);
    for index in 0..messages {
        view.transcript
            .push_block(TranscriptBlock::Assistant(AssistantBlock {
                index,
                loop_id: format!("lup_perf_{index}"),
                request_index: 0,
                model: "deep".into(),
                reasoning_level: minicore_tui::protocol::Reasoning::High,
                parts: vec![AssistantPart::Text(format!("## msg {index}\n\n{body}"))],
                tool_calls: vec![],
                usage: Default::default(),
                finish_reason: "stop".into(),
                terminal_error: None,
            }));
    }
    view.transcript.complete = true;
    view.transcript.invalidate();
    app.sessions.known.insert("ses_perf".into(), view);
    app.sessions.active = Some("ses_perf".into());
    app
}

/// Appends `count` real `output_delta` events to the active live loop.
fn push_live_deltas(app: &mut App, turn: &TurnRef, count: usize) {
    for index in 0..count {
        let part = format!("delta-{index} ");
        let event = json!({
            "type": "output_delta",
            "data": {
                "turn": {"session_id": turn.session_id, "loop_id": turn.loop_id},
                "request_index": 0,
                "channel": "text",
                "delta": part,
                "meta": {"session_id": turn.session_id, "dropped_before": 0}
            }
        });
        app.update(AppEvent::Rpc(RpcEvent::Frame(IncomingFrame::Notification(
            RpcNotification::AgentEvent(serde_json::from_value(event).unwrap()),
        ))));
    }
}

async fn install_worker_layout(app: &mut App, jobs: &mut LocalJobs, width: u16) {
    let request = app
        .layout_request(width)
        .expect("the durable cache must miss before the worker request");
    let identity = request.identity.clone();
    assert!(jobs.try_schedule_layout(request));
    app.mark_layout_pending(identity);
    loop {
        let event = jobs.events().recv().await.expect("layout worker event");
        let complete = matches!(
            &event,
            AppEvent::DurableLayoutPrepared(result) if result.complete
        );
        app.update(event);
        if complete {
            break;
        }
    }
    assert!(app.prepared_conversation(width).is_some());
}

/// The partial durable-layout hand-off must preserve every section kind;
/// ToolCall markers must not be the only sections visible after the worker
/// completes.
#[tokio::test]
async fn async_layout_worker_preserves_user_assistant_and_tool_sections() {
    let mut app = app_with_history(0, 240);
    {
        let view = app
            .sessions
            .known
            .get_mut("ses_perf")
            .expect("performance session");
        view.transcript.push_block(TranscriptBlock::User(UserBlock {
            index: Some(0),
            loop_id: Some("loop_display".to_owned()),
            kind: UserMessageKindWire::Prompt,
            text: "user async visible".to_owned(),
            pending: false,
        }));
        view.transcript
            .push_block(TranscriptBlock::Assistant(AssistantBlock {
                index: 1,
                loop_id: "loop_display".to_owned(),
                request_index: 0,
                model: "deep".to_owned(),
                reasoning_level: minicore_tui::protocol::Reasoning::High,
                parts: vec![
                    AssistantPart::Text("assistant async visible".to_owned()),
                    AssistantPart::ToolCall(minicore_tui::protocol::ToolCallViewWire {
                        tool_call_id: "call_display".to_owned(),
                        name: "read".to_owned(),
                        call_index: 0,
                        display: None,
                    }),
                ],
                tool_calls: vec![],
                usage: Default::default(),
                finish_reason: "stop".to_owned(),
                terminal_error: None,
            }));
        view.transcript.push_block(TranscriptBlock::Tool(ToolBlock {
            index: Some(2),
            loop_id: "loop_display".to_owned(),
            request_index: 0,
            tool_call_id: "call_display".to_owned(),
            name: "read".to_owned(),
            result: Some(Arc::from("tool async visible")),
            outcome: None,
            live_status: None,
            progress: None,
            expanded: true,
        }));
        view.transcript.complete = true;
        view.transcript.invalidate();
    }
    app.enable_async_layout();
    let mut jobs = LocalJobs::new();
    install_worker_layout(&mut app, &mut jobs, WIDTH).await;
    let prepared = app
        .prepared_conversation(WIDTH)
        .expect("worker layout installed");
    let text = prepared
        .lines()
        .into_iter()
        .flat_map(|line| line.spans.into_iter())
        .map(|span| span.content.into_owned())
        .collect::<String>();
    assert!(
        text.contains("user async visible"),
        "User section missing: {text}"
    );
    assert!(
        text.contains("assistant async visible"),
        "Assistant section missing: {text}"
    );
    assert!(text.contains("read"), "Tool section missing: {text}");
    assert!(
        text.contains("tool async visible"),
        "Tool result missing: {text}"
    );
    jobs.shutdown().await;
}

#[test]
fn partial_durable_tool_prefers_live_state_over_an_incomplete_history_marker() {
    let mut app = app_with_history(0, 240);
    let turn = TurnRef {
        session_id: "ses_perf".to_owned(),
        loop_id: "loop_partial_tool".to_owned(),
    };
    let view = app
        .sessions
        .known
        .get_mut("ses_perf")
        .expect("performance session");
    view.transcript
        .push_block(TranscriptBlock::Assistant(AssistantBlock {
            index: 10,
            loop_id: turn.loop_id.clone(),
            request_index: 0,
            model: "deep".to_owned(),
            reasoning_level: minicore_tui::protocol::Reasoning::High,
            parts: vec![AssistantPart::ToolCall(
                minicore_tui::protocol::ToolCallViewWire {
                    tool_call_id: "call_partial_tool".to_owned(),
                    name: "read".to_owned(),
                    call_index: 0,
                    display: None,
                },
            )],
            tool_calls: vec![],
            usage: Default::default(),
            finish_reason: "tool_calls".to_owned(),
            terminal_error: None,
        }));
    let mut live = LiveLoop::new(LocalSubmissionId(1), "partial tool round".to_owned());
    live.reference = Some(turn);
    let mut request = LiveRequest::new(
        0,
        0,
        "deep".to_owned(),
        minicore_tui::protocol::Reasoning::High,
    );
    request.parts.push(LivePart::Tool {
        tool_call_id: "call_partial_tool".to_owned(),
    });
    request.tools.push(LiveTool {
        tool_call_id: "call_partial_tool".to_owned(),
        name: "read".to_owned(),
        status: ToolStatus::Failed,
        progress: Some("live-progress-unique".to_owned()),
        display: None,
        result: Some(Arc::from("live-result-unique")),
        result_truncated: false,
        expanded: true,
    });
    live.requests.push(request);
    view.live = Some(live);
    view.transcript.complete = false;
    view.transcript.invalidate();

    let prepared = prepare_conversation(&app, WIDTH);
    let matching_sections = prepared
        .sections
        .iter()
        .filter(|section| section.id.tool_call_id.as_deref() == Some("call_partial_tool"))
        .count();
    assert_eq!(
        matching_sections, 1,
        "partial history and live state must still render one tool card"
    );
    let text = prepared
        .lines()
        .into_iter()
        .flat_map(|line| line.spans.into_iter())
        .map(|span| span.content.into_owned())
        .collect::<String>();
    assert!(
        text.contains("live-result-unique"),
        "the live result must not be hidden by the incomplete durable marker: {text}"
    );
    let error_background = app.theme.theme().tool_error_bg;
    assert!(
        prepared.lines().iter().any(|line| {
            line.spans.iter().any(|span| {
                span.content.contains("live-result-unique")
                    && span.style.bg == Some(error_background)
            })
        }),
        "the live terminal error state must survive the durable/live transition: {text}"
    );

    let view = app
        .sessions
        .known
        .get_mut("ses_perf")
        .expect("performance session");
    view.live = None;
    view.transcript.push_block(TranscriptBlock::Tool(ToolBlock {
        index: Some(11),
        loop_id: "loop_partial_tool".to_owned(),
        request_index: 0,
        tool_call_id: "call_partial_tool".to_owned(),
        name: "read".to_owned(),
        result: Some(Arc::from("durable-result-unique")),
        outcome: Some(minicore_tui::protocol::ToolOutcomeWire::Success),
        live_status: None,
        progress: None,
        expanded: true,
    }));
    view.transcript.complete = true;
    view.transcript.invalidate();
    let persisted = prepare_conversation(&app, WIDTH);
    let persisted_matching_sections = persisted
        .sections
        .iter()
        .filter(|section| section.id.tool_call_id.as_deref() == Some("call_partial_tool"))
        .count();
    assert_eq!(
        persisted_matching_sections, 1,
        "history completion must replace the live card rather than duplicate it"
    );
    let persisted_text = persisted
        .lines()
        .into_iter()
        .flat_map(|line| line.spans.into_iter())
        .map(|span| span.content.into_owned())
        .collect::<String>();
    assert!(persisted_text.contains("durable-result-unique"));
}

#[test]
fn durable_tool_key_suppresses_a_live_tool_duplicate_at_a_nonzero_viewport() {
    let mut app = app_with_history(0, 240);
    let turn = TurnRef {
        session_id: "ses_perf".to_owned(),
        loop_id: "loop_display_tool".to_owned(),
    };
    let view = app
        .sessions
        .known
        .get_mut("ses_perf")
        .expect("performance session");
    view.transcript
        .push_block(TranscriptBlock::Assistant(AssistantBlock {
            index: 10,
            loop_id: turn.loop_id.clone(),
            request_index: 0,
            model: "deep".to_owned(),
            reasoning_level: minicore_tui::protocol::Reasoning::High,
            parts: vec![AssistantPart::ToolCall(
                minicore_tui::protocol::ToolCallViewWire {
                    tool_call_id: "call_display_tool".to_owned(),
                    name: "read".to_owned(),
                    call_index: 0,
                    display: None,
                },
            )],
            tool_calls: vec![],
            usage: Default::default(),
            finish_reason: "tool_calls".to_owned(),
            terminal_error: None,
        }));
    view.transcript.push_block(TranscriptBlock::Tool(ToolBlock {
        index: Some(11),
        loop_id: turn.loop_id.clone(),
        request_index: 0,
        tool_call_id: "call_display_tool".to_owned(),
        name: "read".to_owned(),
        result: Some(Arc::from("tool-result-unique")),
        outcome: Some(minicore_tui::protocol::ToolOutcomeWire::Success),
        live_status: None,
        progress: None,
        expanded: true,
    }));
    let mut live = LiveLoop::new(LocalSubmissionId(1), "tool round".to_owned());
    live.reference = Some(turn);
    let mut request = LiveRequest::new(
        0,
        0,
        "deep".to_owned(),
        minicore_tui::protocol::Reasoning::High,
    );
    request.parts.push(LivePart::Tool {
        tool_call_id: "call_display_tool".to_owned(),
    });
    request.tools.push(LiveTool {
        tool_call_id: "call_display_tool".to_owned(),
        name: "read".to_owned(),
        status: ToolStatus::Succeeded,
        progress: None,
        display: None,
        result: Some(Arc::from("tool-result-unique")),
        result_truncated: false,
        expanded: true,
    });
    live.requests.push(request);
    view.live = Some(live);
    view.transcript.complete = true;
    view.transcript.invalidate();
    app.viewport = (1, 80);

    let prepared = prepare_conversation(&app, WIDTH);
    let matching_sections = prepared
        .sections
        .iter()
        .filter(|section| section.id.tool_call_id.as_deref() == Some("call_display_tool"))
        .count();
    assert_eq!(
        matching_sections, 1,
        "tool card must have one durable owner"
    );
}

#[test]
fn cached_layout_reuses_durable_tool_key_index() {
    let mut app = app_with_history(0, 240);
    let view = app
        .sessions
        .known
        .get_mut("ses_perf")
        .expect("performance session");
    view.transcript
        .push_block(TranscriptBlock::Assistant(AssistantBlock {
            index: 10,
            loop_id: "loop_cached_tool".to_owned(),
            request_index: 0,
            model: "deep".to_owned(),
            reasoning_level: minicore_tui::protocol::Reasoning::High,
            parts: vec![AssistantPart::ToolCall(
                minicore_tui::protocol::ToolCallViewWire {
                    tool_call_id: "call_cached_tool".to_owned(),
                    name: "read".to_owned(),
                    call_index: 0,
                    display: None,
                },
            )],
            tool_calls: vec![],
            usage: Default::default(),
            finish_reason: "tool_calls".to_owned(),
            terminal_error: None,
        }));
    view.transcript.push_block(TranscriptBlock::Tool(ToolBlock {
        index: Some(11),
        loop_id: "loop_cached_tool".to_owned(),
        request_index: 0,
        tool_call_id: "call_cached_tool".to_owned(),
        name: "read".to_owned(),
        result: Some(Arc::from("cached-tool-result")),
        outcome: Some(minicore_tui::protocol::ToolOutcomeWire::Success),
        live_status: None,
        progress: None,
        expanded: true,
    }));
    view.transcript.complete = true;
    view.transcript.invalidate();

    let first = prepare_conversation(&app, WIDTH);
    let first_tool_keys = Arc::clone(
        &first
            .durable
            .as_ref()
            .expect("first durable layout")
            .layout
            .tool_keys,
    );
    let durable = first.durable.clone().expect("first durable layout");
    app.sessions
        .known
        .get_mut("ses_perf")
        .expect("performance session")
        .transcript
        .render_cache = Some(durable);
    let second = prepare_conversation(&app, WIDTH);
    let second_tool_keys = &second
        .durable
        .as_ref()
        .expect("cached durable layout")
        .layout
        .tool_keys;
    assert!(
        Arc::ptr_eq(&first_tool_keys, second_tool_keys),
        "live composition must reuse the cached durable ToolKey index"
    );
    assert!(second_tool_keys.contains(&ToolKey::new(
        "ses_perf",
        "loop_cached_tool",
        0,
        "call_cached_tool",
    )));
}

/// Production-path smoke test: the first durable layout is prepared by the
/// owned worker, while the App only installs the result and composes the
/// small live tail. No synchronous durable fallback is permitted in this
/// mode.
#[tokio::test]
async fn production_layout_worker_installs_current_width_only() {
    let mut app = app_with_history(64, 240);
    app.enable_async_layout();
    assert!(app.prepared_conversation(WIDTH).is_none());
    assert_eq!(total_lines(&app, WIDTH), 0);

    let mut jobs = LocalJobs::new();
    install_worker_layout(&mut app, &mut jobs, WIDTH).await;
    assert_eq!(app.prepared_conversation(WIDTH).unwrap().width, WIDTH);
    assert!(app.prepared_conversation(WIDTH + 1).is_none());
    assert!(app.layout_request(WIDTH + 1).is_some());

    app.update(AppEvent::Terminal(crossterm::event::Event::Key(
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('x'),
            crossterm::event::KeyModifiers::empty(),
        ),
    )));
    assert_eq!(app.composer.content(), "x");
    assert!(app.prepared_conversation(WIDTH).is_some());

    app.update(AppEvent::TerminalSize {
        width: 100,
        height: 24,
    });
    assert!(app.prepared_conversation(WIDTH).is_none());
    let resized = WIDTH - 1;
    install_worker_layout(&mut app, &mut jobs, resized).await;
    assert_eq!(app.prepared_conversation(resized).unwrap().width, resized);
    assert!(app.prepared_conversation(WIDTH).is_none());
    jobs.shutdown().await;
}

#[test]
fn durable_layout_snapshot_shares_tool_presentation_storage() {
    let app = app_with_history(1, 240);
    let view = app.active_view().expect("active performance view");
    let snapshot = minicore_tui::ui::transcript::DurableLayoutSnapshot::from_view(view);
    assert!(std::sync::Arc::ptr_eq(
        &snapshot.tool_presentations,
        &view.tool_presentations
    ));
}

#[tokio::test]
async fn production_layout_worker_fences_stale_theme_result() {
    let mut app = app_with_history(64, 240);
    app.enable_async_layout();
    let mut jobs = LocalJobs::new();

    let first = app.layout_request(WIDTH).expect("initial layout request");
    let first_identity = first.identity.clone();
    let first_cancel = std::sync::Arc::clone(&first.cancel);
    assert!(jobs.try_schedule_layout(first));
    app.mark_layout_pending(first_identity);
    let stale = jobs.events().recv().await.expect("first layout result");

    app.update(AppEvent::SetTheme(minicore_tui::theme::ThemeKind::Light));
    let second = app
        .layout_request(WIDTH)
        .expect("theme change must request a new generation");
    let second_identity = second.identity.clone();
    assert!(jobs.try_schedule_layout(second));
    app.mark_layout_pending(second_identity);
    assert!(first_cancel.load(std::sync::atomic::Ordering::Relaxed));

    app.update(stale);
    assert!(app.prepared_conversation(WIDTH).is_none());
    loop {
        let current = jobs.events().recv().await.expect("current layout result");
        let complete = matches!(
            &current,
            AppEvent::DurableLayoutPrepared(result) if result.complete
        );
        app.update(current);
        if complete {
            break;
        }
    }
    assert!(app.prepared_conversation(WIDTH).is_some());
    assert_eq!(app.theme, minicore_tui::theme::ThemeKind::Light);
    jobs.shutdown().await;
}

/// C2b acceptance probe: a real 50k-row durable history and 1000 real
/// `output_delta` notifications use the installed immutable layout. The
/// ignored marker keeps the normal suite quick; this is the command-line
/// acceptance evidence for the worker path.
#[tokio::test]
#[ignore = "C2b acceptance; run with --ignored --nocapture"]
async fn measure_c2b_worker_over_50k_rows_and_1000_output_deltas() {
    const HEIGHT: usize = 40;
    let mut app = app_with_history(7300, 240);
    app.enable_async_layout();
    let mut jobs = LocalJobs::new();
    install_worker_layout(&mut app, &mut jobs, WIDTH).await;
    let initial = app.prepared_conversation(WIDTH).unwrap();
    let initial_rows = initial.total_rows();
    assert!(initial_rows >= 50_000);

    app.update(AppEvent::SubmitTurn {
        session_id: "ses_perf".into(),
        text: "live turn".into(),
    });
    // The local pending user card is a durable section change. Settle that
    // one worker result before measuring the following live-only deltas.
    let turn = TurnRef {
        session_id: "ses_perf".into(),
        loop_id: "lup_live".into(),
    };
    install_worker_layout(&mut app, &mut jobs, WIDTH).await;
    // The first event binds the pending card to the real Loop and therefore
    // is also a durable identity change. Settle that transition out of band.
    push_live_deltas(&mut app, &turn, 1);
    install_worker_layout(&mut app, &mut jobs, WIDTH).await;
    let base = minicore_tui::perf::snapshot();
    let mut window_rows = 0usize;
    for index in 0..1000 {
        push_live_deltas(&mut app, &turn, 1);
        let prepared = minicore_tui::ui::transcript::prepare_conversation_with_durable(
            &app,
            WIDTH,
            app.cached_durable(WIDTH).expect("durable cache"),
        );
        window_rows += prepared
            .window(prepared.total_rows().saturating_sub(HEIGHT), HEIGHT)
            .len();
        app.update(AppEvent::ConversationPrepared(prepared));
        if index % 250 == 0 {
            assert!(app.cached_durable(WIDTH).is_some());
        }
    }
    let after = minicore_tui::perf::snapshot();
    println!(
        "c2b_worker: durable_rows={initial_rows} deltas=1000 layout_calls={} history_bytes_cloned={} viewport_rows={} viewport_bytes={}",
        after.layout_calls - base.layout_calls,
        after.historical_text_bytes_cloned - base.historical_text_bytes_cloned,
        after.viewport_rows_materialized - base.viewport_rows_materialized,
        after.viewport_text_bytes_cloned - base.viewport_text_bytes_cloned,
    );
    assert_eq!(after.layout_calls - base.layout_calls, 0);
    assert_eq!(
        after.historical_text_bytes_cloned - base.historical_text_bytes_cloned,
        0
    );
    assert_eq!(
        window_rows as u64,
        after.viewport_rows_materialized - base.viewport_rows_materialized
    );
    assert!(after.viewport_rows_materialized <= 1000 * HEIGHT as u64);
    jobs.shutdown().await;
}

#[tokio::test]
#[ignore = "C2c release workload; run with --ignored --nocapture"]
async fn measure_c2c_release_workload_120x40() {
    const WORKLOAD_WIDTH: u16 = 119;
    const HEIGHT: usize = 40;
    let mut app = app_with_history(7300, 240);
    app.enable_async_layout();
    let mut jobs = LocalJobs::new();
    install_worker_layout(&mut app, &mut jobs, WORKLOAD_WIDTH).await;
    let turn = TurnRef {
        session_id: "ses_perf".into(),
        loop_id: "lup_c2c_live".into(),
    };
    app.update(AppEvent::SubmitTurn {
        session_id: "ses_perf".into(),
        text: "c2c workload".into(),
    });
    install_worker_layout(&mut app, &mut jobs, WORKLOAD_WIDTH).await;
    push_live_deltas(&mut app, &turn, 1);
    install_worker_layout(&mut app, &mut jobs, WORKLOAD_WIDTH).await;

    let base = minicore_tui::perf::snapshot();
    let mut samples = Vec::with_capacity(1000);
    for index in 0..1000 {
        let started = std::time::Instant::now();
        push_live_deltas(&mut app, &turn, 1);
        let prepared = minicore_tui::ui::transcript::prepare_conversation_from_cache(
            &app,
            WORKLOAD_WIDTH,
            app.cached_durable(WORKLOAD_WIDTH).expect("durable cache"),
        );
        let _ = prepared.window(prepared.total_rows().saturating_sub(HEIGHT), HEIGHT);
        app.update(AppEvent::ConversationPrepared(prepared));
        samples.push(started.elapsed());
        if index % 250 == 0 {
            assert!(app.cached_durable(WORKLOAD_WIDTH).is_some());
        }
    }
    samples.sort_unstable();
    let p95 = samples[949];
    let p99 = samples[989];
    let counters = minicore_tui::perf::snapshot();
    let prepared = app
        .prepared_conversation(WORKLOAD_WIDTH)
        .expect("prepared workload frame");
    println!(
        "c2c_120x40: p95_us={} p99_us={} durable_rows={} history_bytes_cloned={} layout_calls={} viewport_rows={} viewport_bytes={} retained_layout_bytes_estimate={}",
        p95.as_micros(),
        p99.as_micros(),
        prepared.total_rows(),
        counters.historical_text_bytes_cloned - base.historical_text_bytes_cloned,
        counters.layout_calls - base.layout_calls,
        counters.viewport_rows_materialized - base.viewport_rows_materialized,
        counters.viewport_text_bytes_cloned - base.viewport_text_bytes_cloned,
        app.cached_durable(WORKLOAD_WIDTH)
            .map_or(0, |cache| cache.retained_bytes()),
    );
    jobs.shutdown().await;
}

/// Defect: the total prepared row count for a long history grows with the
/// number of messages. A viewport-only composition (stage C) must produce a
/// row count proportional to the viewport, not this total.
#[test]
fn baseline_prepared_rows_scale_with_total_history() {
    let small = app_with_history(20, 240);
    let large = app_with_history(200, 240);
    let small_rows = total_lines(&small, WIDTH);
    let large_rows = total_lines(&large, WIDTH);
    assert!(small_rows > 0);
    assert!(
        large_rows > small_rows * 5,
        "BASELINE: prepared rows grow with history ({small_rows} -> {large_rows})"
    );
    println!("baseline rows: 20 msgs={small_rows} 200 msgs={large_rows}");
}

/// Baseline: `all_lines` materializes the full transcript as owned rows. This
/// is the **diagnostic helper**, not the production frame path; it is recorded
/// so stage C removes the full clone from both.
#[test]
fn baseline_all_lines_materializes_full_transcript() {
    let app = app_with_history(200, 240);
    let lines = all_lines(&minicore_tui::theme::Theme::dark(), &app, WIDTH as usize);
    let prepared = prepare_conversation(&app, WIDTH);
    assert_eq!(
        lines.len(),
        prepared.total_rows(),
        "BASELINE: all_lines owns exactly the full prepared row set"
    );
    assert!(
        lines.len() > 500,
        "long history materializes {} rows",
        lines.len()
    );
    println!("baseline all_lines rows: {}", lines.len());
}

/// Ignored diagnostic: synchronous preparation over a 50,000-row history.
/// Production C2b uses the worker acceptance probe below; this helper remains
/// useful for comparing the explicit test-only fallback.
#[test]
#[ignore = "manual performance measurement; run with --ignored --nocapture"]
fn measure_prepare_frame_path_over_50k_rows() {
    // ~7 rows per 240-byte message; 7200 messages reaches ~50k rows.
    let app = app_with_history(7300, 240);
    let first = prepare_conversation(&app, WIDTH);
    assert!(
        first.total_rows() >= 50_000,
        "fixture must reach 50k rows, got {}",
        first.total_rows()
    );
    let start = std::time::Instant::now();
    let mut rows = 0;
    for _ in 0..5 {
        rows += prepare_conversation(&app, WIDTH).total_rows();
    }
    let elapsed = start.elapsed();
    println!(
        "measure_prepare_frame_path_over_50k_rows: total_rows={} per_call_ms={:.2}",
        first.total_rows(),
        elapsed.as_secs_f64() * 1000.0 / 5.0
    );
    let _ = rows;
}

/// Diagnostic live-delta baseline: start a loop, then push 1000 real
/// `output_delta` events and measure the synchronous helper. The production
/// worker/cache contract is asserted by the C2b probe above.
#[test]
#[ignore = "manual performance measurement; run with --ignored --nocapture"]
fn measure_live_delta_rebuild_cost() {
    let mut app = app_with_history(7300, 240);
    let theme = minicore_tui::theme::Theme::dark();
    let before_rows = all_lines(&theme, &app, WIDTH as usize).len();

    app.update(AppEvent::SubmitTurn {
        session_id: "ses_perf".into(),
        text: "live turn".into(),
    });
    let turn = TurnRef {
        session_id: "ses_perf".into(),
        loop_id: "lup_live".into(),
    };
    let start = std::time::Instant::now();
    push_live_deltas(&mut app, &turn, 1000);
    let elapsed = start.elapsed();
    let after_rows = all_lines(&theme, &app, WIDTH as usize).len();

    println!(
        "measure_live_delta_rebuild_cost: deltas=1000 history_rows_before={before_rows} \
         history_rows_after={after_rows} delta_rows={} push_ms={:.2}",
        after_rows as i64 - before_rows as i64,
        elapsed.as_secs_f64() * 1000.0
    );
}

/// C2 structural acceptance (spec §11.2/§11.7/§25.1): over a 50k-row
/// history, live deltas must not rebuild the durable layout or copy history
/// text, and each installed frame must materialize only its viewport window.
/// Counters are read from the real call points; the window byte count proves
/// they are not constants.
#[test]
#[ignore = "manual performance measurement; run with --ignored --nocapture"]
fn measure_c2_stable_layout_and_viewport_ownership() {
    const HEIGHT: usize = 40;
    let mut app = app_with_history(7300, 240);
    app.update(AppEvent::ConversationPrepared(prepare_conversation(
        &app, WIDTH,
    )));
    let first = prepare_conversation(&app, WIDTH);
    assert!(
        first.total_rows() >= 50_000,
        "fixture must reach 50k rows, got {}",
        first.total_rows()
    );
    // The turn-start durable change is settled before the measurement, so
    // the 1000 frames below measure only delta frames.
    app.update(AppEvent::SubmitTurn {
        session_id: "ses_perf".into(),
        text: "live turn".into(),
    });
    let turn = TurnRef {
        session_id: "ses_perf".into(),
        loop_id: "lup_live".into(),
    };
    // The first unattributed delta materializes a durable live block; it and
    // the turn-start change are settled before the measurement.
    push_live_deltas(&mut app, &turn, 1);
    app.update(AppEvent::ConversationPrepared(prepare_conversation(
        &app, WIDTH,
    )));
    let base = minicore_tui::perf::snapshot();
    let start = std::time::Instant::now();
    let mut window_rows = 0usize;
    let mut live_rows = 0usize;
    let mut rebuild_frames: Vec<usize> = Vec::new();
    for index in 0..1000 {
        if index % 4 == 0 {
            push_live_deltas(&mut app, &turn, 1);
        }
        let before_frame = minicore_tui::perf::snapshot().layout_calls;
        let prepared = prepare_conversation(&app, WIDTH);
        if minicore_tui::perf::snapshot().layout_calls != before_frame {
            rebuild_frames.push(index);
        }
        live_rows = prepared.live_rows();
        let offset = prepared.total_rows().saturating_sub(HEIGHT);
        window_rows += prepared.window(offset, HEIGHT).len();
        app.update(AppEvent::ConversationPrepared(prepared));
    }
    let elapsed = start.elapsed();
    let after = minicore_tui::perf::snapshot();
    let layout_delta = after.layout_calls - base.layout_calls;
    let history_bytes = after.historical_text_bytes_cloned - base.historical_text_bytes_cloned;
    let viewport_rows = after.viewport_rows_materialized - base.viewport_rows_materialized;
    let viewport_bytes = after.viewport_text_bytes_cloned - base.viewport_text_bytes_cloned;
    println!(
        "measure_c2: frames=1000 deltas=250 live_rows={live_rows} layout_calls_delta={layout_delta}          history_bytes_cloned={history_bytes} viewport_rows={viewport_rows}          viewport_bytes={viewport_bytes} window_rows={window_rows} rebuild_frames={rebuild_frames:?} elapsed_ms={:.2}",
        elapsed.as_secs_f64() * 1000.0
    );
    assert_eq!(
        layout_delta, 0,
        "live deltas must not rebuild the stable durable layout"
    );
    assert_eq!(
        history_bytes, 0,
        "live deltas must not clone history text into a frame"
    );
    assert_eq!(
        window_rows as u64, viewport_rows,
        "each frame materializes exactly its requested window"
    );
    assert!(
        viewport_rows <= 1000 * HEIGHT as u64,
        "viewport materialization is bounded by height, not history"
    );
    assert!(
        viewport_bytes > 0,
        "the window clone is measured at the real call point, not a constant"
    );
}

/// Ignored timing probe retained for the diagnostic helper, clearly labelled
/// as `all_lines` (not the production frame path).
#[test]
#[ignore = "manual performance measurement; run with --ignored --nocapture"]
fn measure_all_lines_clone_latency() {
    let app = app_with_history(1000, 240);
    let theme = minicore_tui::theme::Theme::dark();
    let _ = all_lines(&theme, &app, WIDTH as usize);
    let start = std::time::Instant::now();
    let mut rows = 0;
    for _ in 0..20 {
        rows += all_lines(&theme, &app, WIDTH as usize).len();
    }
    let elapsed = start.elapsed();
    println!(
        "measure_all_lines_clone_latency: helper 20 clones rows_total={rows} per_call_ms={:.2}",
        elapsed.as_secs_f64() * 1000.0 / 20.0
    );
}
