use super::*;
use futures_util::FutureExt;
use minicore_tui::protocol::CommandStatusWire;
use minicore_tui::state::tool::ToolFacts;
use std::panic::AssertUnwindSafe;

const CALL_ID: &str = "inline_bash_stream";
const GATES: [&str; 4] = [
    ".inline-gate-bulk",
    ".inline-gate-count",
    ".inline-gate-expanded",
    ".inline-gate-finish",
];

fn facts<'a>(app: &'a App, key: &ToolKey) -> &'a ToolFacts {
    &app.sessions.known[&key.session_id].tool_presentations[key]
}

fn transcript_text(app: &App) -> String {
    minicore_tui::ui::transcript::prepare_conversation(app, 100)
        .lines()
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

async fn wait_for_preview(
    process: &mut RpcProcess,
    app: &mut App,
    key: &ToolKey,
    stdout: &str,
    stderr: &str,
) {
    pump_until(process, app, |app| {
        facts(app, key)
            .process_output
            .as_ref()
            .is_some_and(|streams| {
                streams[0].display_text() == stdout && streams[1].display_text() == stderr
            })
            && facts(app, key)
                .command
                .as_ref()
                .is_some_and(|command| command.status == CommandStatusWire::Running)
    })
    .await
    .expect("real process notifications must reach the inline preview before exit");

    let facts = facts(app, key);
    assert!(!facts.is_terminal());
    assert!(
        facts.result.is_none(),
        "a live preview is not a tool result"
    );
    assert!(
        facts.inline.is_none(),
        "preview must not need an output read"
    );
    assert!(app.tool_detail().is_none(), "no detail panel was opened");
    assert_eq!(facts.input_line_count(), Some((1, false)));
    let streams = facts.process_output.as_ref().unwrap();
    assert_eq!(streams[0].next_offset, stdout.len() as u64);
    assert_eq!(streams[1].next_offset, stderr.len() as u64);
    assert!(!facts.process_count_partial());
    assert!(!facts.process_output_partial());
}

fn release(workspace: &Path, gate: &str) {
    std::fs::write(workspace.join(gate), b"release\n").expect("release Bash stage");
}

/// Every output stage is held by a file the test owns. No duration-based race
/// can turn a final result into false evidence of live inline streaming. The
/// script lives in a fixture file so its printed markers cannot be mistaken
/// for the command's displayed input.
#[test]
#[ignore = "requires MINICORE_AGENT_BIN; real file-gated Bash and loopback model"]
fn e2e_bash_inline_streams_before_finish_and_collapsed_count_keeps_growing() {
    let agent_bin = require_agent_bin();
    let (env, _) = E2eEnvironment::setup();
    enable_bash_profile(&env);
    std::fs::write(
        env.workspace_path.join("inline-stream.sh"),
        r#"set -eu
wait_gate() {
    while [ ! -f "$1" ]; do sleep 0.02; done
}
printf 'stdout-start\n'
printf 'stderr-start\n' >&2
wait_gate .inline-gate-bulk
i=1
while [ "$i" -le 17 ]; do
    printf 'build-%02d\n' "$i"
    i=$((i + 1))
done
wait_gate .inline-gate-count
printf 'build-18\n'
wait_gate .inline-gate-expanded
printf 'stdout-latest\n'
printf 'stderr-latest\n' >&2
wait_gate .inline-gate-finish
printf 'stdout-final\n'
printf 'stderr-final\n' >&2
: > .inline-command-finished
"#,
    )
    .unwrap();
    env._server.enqueue_sse(sse_tool_call_response(
        CALL_ID,
        "bash",
        &json!({"command": "bash inline-stream.sh"}).to_string(),
    ));
    // Inspect the authoritative tool result before the next model response
    // seals the turn and switches the transcript to its durable projection.
    let model_gate = Arc::new(AtomicBool::new(false));
    env._server.enqueue_gated(
        sse_text_response("inline stream complete"),
        Arc::clone(&model_gate),
        Some("deep-model"),
    );

    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(async move {
            let mut process = env.spawn_agent(&agent_bin);
            let mut app = App::new(env.workspace_path.clone());
            // Keep the RPC owner alive through cleanup even when an assertion
            // fails. Releasing every gate lets Bash reap its own sleep child.
            let outcome = AssertUnwindSafe(async {
                dispatch(&mut process, &mut app, AppEvent::Bootstrap)
                    .await
                    .unwrap();
                pump_until(&mut process, &mut app, |app| {
                    app.connection == ConnectionState::Ready
                })
                .await
                .unwrap();
                let session = create_additional_session(
                    &mut process,
                    &mut app,
                    &env.workspace_path,
                    "inline Bash stream",
                )
                .await;
                dispatch(
                    &mut process,
                    &mut app,
                    AppEvent::SubmitTurn {
                        session_id: session.clone(),
                        text: "run the gated build fixture".into(),
                    },
                )
                .await
                .unwrap();
                pump_until(&mut process, &mut app, |app| {
                    app.sessions.known[&session]
                        .tool_presentations
                        .keys()
                        .any(|key| key.tool_call_id == CALL_ID)
                })
                .await
                .unwrap();
                let key = app.sessions.known[&session]
                    .tool_presentations
                    .keys()
                    .find(|key| key.tool_call_id == CALL_ID)
                    .unwrap()
                    .clone();
                assert_eq!(key.session_id, session);
                assert_eq!(key.request_index, 0);
                assert!(!key.loop_id.is_empty());

                let mut stdout = "stdout-start\n".to_owned();
                let mut stderr = "stderr-start\n".to_owned();
                wait_for_preview(&mut process, &mut app, &key, &stdout, &stderr).await;
                let text = transcript_text(&app);
                assert!(text.contains("stdout:"), "{text}");
                assert!(text.contains("stderr:"), "{text}");
                assert!(text.contains("stdout-start"), "{text}");
                assert!(text.contains("stderr-start"), "{text}");
                assert!(!text.contains("lines hidden"), "{text}");
                assert_eq!(env._server.recorded_requests().len(), 1);
                assert!(!env.workspace_path.join(".inline-command-finished").exists());

                release(&env.workspace_path, GATES[0]);
                for line in 1..=17 {
                    stdout.push_str(&format!("build-{line:02}\n"));
                }
                wait_for_preview(&mut process, &mut app, &key, &stdout, &stderr).await;
                assert_eq!(facts(&app, &key).output_line_count, Some(21));
                assert!(!app.sessions.known[&session].tool_folds.contains_key(&key));
                let folded = transcript_text(&app);
                assert!(folded.contains("14 earlier lines"), "{folded}");
                assert!(!folded.contains("stdout-start"), "{folded}");
                assert!(folded.contains("build-17"), "{folded}");
                assert!(folded.contains("stderr-start"), "{folded}");

                release(&env.workspace_path, GATES[1]);
                stdout.push_str("build-18\n");
                wait_for_preview(&mut process, &mut app, &key, &stdout, &stderr).await;
                assert_eq!(facts(&app, &key).output_line_count, Some(22));
                let folded = transcript_text(&app);
                assert!(folded.contains("15 earlier lines"), "{folded}");
                assert!(!folded.contains("14 earlier lines"), "{folded}");
                assert!(folded.contains("build-18"), "{folded}");

                dispatch(
                    &mut process,
                    &mut app,
                    AppEvent::ToggleTool {
                        session_id: key.session_id.clone(),
                        loop_id: key.loop_id.clone(),
                        request_index: key.request_index,
                        tool_call_id: key.tool_call_id.clone(),
                    },
                )
                .await
                .unwrap();
                assert_eq!(
                    app.sessions.known[&session].tool_folds.get(&key),
                    Some(&FoldOverride::Expanded)
                );
                assert!(transcript_text(&app).contains("build-18"));

                release(&env.workspace_path, GATES[2]);
                stdout.push_str("stdout-latest\n");
                stderr.push_str("stderr-latest\n");
                wait_for_preview(&mut process, &mut app, &key, &stdout, &stderr).await;
                assert_eq!(
                    app.sessions.known[&session].tool_folds.get(&key),
                    Some(&FoldOverride::Expanded)
                );
                let expanded = transcript_text(&app);
                assert!(expanded.contains("stdout-latest"), "{expanded}");
                assert!(expanded.contains("stderr-latest"), "{expanded}");
                assert!(!expanded.contains("lines hidden"), "{expanded}");
                assert!(!env.workspace_path.join(GATES[3]).exists());
                assert!(!env.workspace_path.join(".inline-command-finished").exists());
                assert_eq!(env._server.recorded_requests().len(), 1);

                release(&env.workspace_path, GATES[3]);
                pump_until(&mut process, &mut app, |app| {
                    let facts = facts(app, &key);
                    facts.is_terminal()
                        && facts.result.as_deref().is_some_and(|result| {
                            result.contains("stdout-final") && result.contains("stderr-final")
                        })
                })
                .await
                .unwrap();
                let final_facts = facts(&app, &key);
                assert!(final_facts.process_output.is_none());
                let result = final_facts.result.as_deref().unwrap();
                for marker in [
                    "stdout-start",
                    "stderr-start",
                    "build-18",
                    "stdout-latest",
                    "stderr-latest",
                ] {
                    assert!(result.contains(marker), "missing {marker} in {result}");
                }
                let command = final_facts.command.as_ref().unwrap();
                assert_eq!(command.status, CommandStatusWire::Exited);
                assert_eq!(command.exit_code, Some(0));
                assert!(command.output_complete);
                assert!(env.workspace_path.join(".inline-command-finished").exists());
                assert!(app.tool_detail().is_none());
                let final_text = transcript_text(&app);
                assert_eq!(
                    final_text.matches("stdout-start").count(),
                    1,
                    "{final_text}"
                );
                assert_eq!(
                    final_text.matches("stderr-start").count(),
                    1,
                    "{final_text}"
                );
                assert!(final_text.contains("stdout-final"), "{final_text}");
                assert!(final_text.contains("stderr-final"), "{final_text}");

                model_gate.store(true, Ordering::Relaxed);
                pump_until_with_decode(&mut process, &mut app, |app| {
                    app.active_view()
                        .is_some_and(|view| view.live.is_none() && view.transcript.complete)
                })
                .await
                .unwrap();
                assert_eq!(env._server.recorded_requests().len(), 2);
            })
            .catch_unwind()
            .await;

            for gate in GATES {
                // Do not let a cleanup write error skip terminating the Agent.
                let _ = std::fs::write(env.workspace_path.join(gate), b"release\n");
            }
            model_gate.store(true, Ordering::Relaxed);
            let shutdown = drain_shutdown_strict(&mut process, &mut app).await;
            process.terminate_with_observer(|_| {}).await;
            if let Err(panic) = outcome {
                if let Err(error) = shutdown {
                    eprintln!("Bash-stream assertion failed; cleanup also failed: {error}");
                }
                std::panic::resume_unwind(panic);
            }
            let report = shutdown.expect("Bash-stream Agent must shut down and drain cleanly");
            assert!(report.shutdown_ok && report.seen_eof && report.seen_exit);
        });
}

/// A real, initially silent Bash is held before its first output byte. The
/// normal production reducer must obtain the recorded start itself; the test
/// never injects an execution timestamp or asks for tool.read on its behalf.
#[test]
#[ignore = "requires MINICORE_AGENT_BIN; real silent Bash and loopback model"]
fn e2e_bash_elapsed_uses_recorded_start_before_silent_process_output() {
    let agent_bin = require_agent_bin();
    let (env, _) = E2eEnvironment::setup();
    enable_bash_profile(&env);
    const FIRST: &str = ".timing-first-output";
    const FINISH: &str = ".timing-finish";
    const CALL: &str = "recorded_bash_timing";
    std::fs::write(
        env.workspace_path.join("timing-stream.sh"),
        r#"set -eu
while [ ! -f .timing-first-output ]; do sleep 0.02; done
printf 'timing-output\n'
while [ ! -f .timing-finish ]; do sleep 0.02; done
"#,
    )
    .unwrap();
    env._server.enqueue_sse(sse_tool_call_response(
        CALL,
        "bash",
        &json!({"command":"bash timing-stream.sh"}).to_string(),
    ));
    let model_gate = Arc::new(AtomicBool::new(false));
    env._server.enqueue_gated(
        sse_text_response("timing complete"),
        Arc::clone(&model_gate),
        Some("deep-model"),
    );
    tokio::runtime::Runtime::new().unwrap().block_on(async move {
        let mut process = env.spawn_agent(&agent_bin);
        let mut app = App::new(env.workspace_path.clone());
        let outcome = AssertUnwindSafe(async {
            dispatch(&mut process, &mut app, AppEvent::Bootstrap).await.unwrap();
            pump_until(&mut process, &mut app, |app| app.connection == ConnectionState::Ready).await.unwrap();
            let session = create_additional_session(&mut process, &mut app, &env.workspace_path, "recorded Bash timing").await;
            dispatch(&mut process, &mut app, AppEvent::SubmitTurn { session_id: session.clone(), text: "run the silent timing fixture".into() }).await.unwrap();
            pump_until(&mut process, &mut app, |app| app.sessions.known[&session].tool_presentations.iter()
                .any(|(key, facts)| key.tool_call_id == CALL && facts.timing.is_some_and(|timing| timing.running))).await.unwrap();
            let key = app.sessions.known[&session].tool_presentations.keys().find(|key| key.tool_call_id == CALL).unwrap().clone();
            let initial = facts(&app, &key);
            assert!(!initial.is_terminal() && initial.result.is_none());
            assert!(initial.inline.is_none() && initial.process_output.is_none(), "the recorded clock precedes stdout/stderr and needs no output read");
            assert!(initial.timing_read_epoch.is_some(), "production issued the one metadata read");
            let recorded_start = initial.execution.as_ref().unwrap().started_at.clone().unwrap();
            let first = initial.timing.unwrap().elapsed;
            assert!(transcript_text(&app).contains("Elapsed"));
            assert!(!env.workspace_path.join(FIRST).exists());
            pump_until(&mut process, &mut app, |app| facts(app, &key).timing.is_some_and(|timing| timing.running && timing.elapsed >= first + Duration::from_secs(2))).await.unwrap();
            let later = facts(&app, &key).timing.unwrap().elapsed;
            assert!(!facts(&app, &key).is_terminal());
            assert!(facts(&app, &key).process_output.is_none());
            assert_eq!(facts(&app, &key).execution.as_ref().unwrap().started_at.as_deref(), Some(recorded_start.as_str()));
            assert!(transcript_text(&app).contains("Elapsed"));
            eprintln!("real silent Bash: recorded_start={recorded_start}; elapsed_before_output_ms={} -> {}; result_absent=true; output_absent=true", first.as_millis(), later.as_millis());

            release(&env.workspace_path, FIRST);
            pump_until(&mut process, &mut app, |app| facts(app, &key).process_output.as_ref().is_some_and(|streams| streams[0].display_text().contains("timing-output"))).await.unwrap();
            assert!(facts(&app, &key).timing.is_some_and(|timing| timing.running));
            assert!(facts(&app, &key).inline.is_none());
            release(&env.workspace_path, FINISH);
            pump_until(&mut process, &mut app, |app| facts(app, &key).timing.is_some_and(|timing| !timing.running)).await.unwrap();
            let final_timing = facts(&app, &key).timing.unwrap();
            let execution = facts(&app, &key).execution.as_ref().unwrap();
            let start = minicore_tui::state::selection::parse_rfc3339(execution.started_at.as_deref().unwrap()).unwrap();
            let finish = minicore_tui::state::selection::parse_rfc3339(execution.finished_at.as_deref().unwrap()).unwrap();
            assert_eq!(final_timing.elapsed, finish.duration_since(start).unwrap());
            assert!(transcript_text(&app).contains("Took"));
            assert!(!transcript_text(&app).contains("Elapsed"));
            dispatch(&mut process, &mut app, AppEvent::Tick).await.unwrap();
            assert_eq!(facts(&app, &key).timing, Some(final_timing));
            eprintln!("real Bash completion: recorded_duration_ms={}; Took matches endpoints; no clock restart", final_timing.elapsed.as_millis());
            model_gate.store(true, Ordering::Relaxed);
            pump_until_with_decode(&mut process, &mut app, |app| app.active_view().is_some_and(|view| view.live.is_none() && view.transcript.complete)).await.unwrap();
        }).catch_unwind().await;
        for gate in [FIRST, FINISH] { let _ = std::fs::write(env.workspace_path.join(gate), b"release\n"); }
        model_gate.store(true, Ordering::Relaxed);
        let shutdown = drain_shutdown_strict(&mut process, &mut app).await;
        process.terminate_with_observer(|_| {}).await;
        if let Err(panic) = outcome {
            if let Err(error) = shutdown { eprintln!("Bash timing cleanup also failed: {error}"); }
            std::panic::resume_unwind(panic);
        }
        let report = shutdown.expect("Bash timing Agent must shut down and drain cleanly");
        assert!(report.shutdown_ok && report.seen_eof && report.seen_exit);
    });
}
