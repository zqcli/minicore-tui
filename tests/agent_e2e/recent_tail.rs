//! Real stdio/loopback checks for the default complete-loop retained tail.
//! Clipboard assertions inspect the exact native-copy command payload; they
//! do not claim that a platform clipboard program is available.
use super::*;

const LATEST: &str = "LATEST_SHORT_ANSWER: é 👩🏽‍💻";

pub(super) fn widen_manual_window(env: &E2eEnvironment) {
    let mut config: toml::Value =
        toml::from_str(&std::fs::read_to_string(&env.config_path).unwrap()).unwrap();
    config["models"]["deep"]["physical_context_window"] = toml::Value::Integer(100_000);
    std::fs::write(&env.config_path, toml::to_string(&config).unwrap()).unwrap();
}

pub(super) fn enqueue_recent_history(env: &E2eEnvironment) {
    for index in 0..5 {
        env._server.enqueue_sse(sse_text_response(&format!(
            "ANSWER_{index}_ {}",
            "x".repeat(32_000)
        )));
    }
    env._server.enqueue_sse(sse_text_response(LATEST));
}

pub(super) async fn land_turn(
    process: &mut RpcProcess,
    app: &mut App,
    session_id: &str,
    text: &str,
) {
    dispatch(
        process,
        app,
        AppEvent::SubmitTurn {
            session_id: session_id.to_owned(),
            text: text.to_owned(),
        },
    )
    .await
    .unwrap();
    pump_until(process, app, |app| {
        app.sessions
            .known
            .get(session_id)
            .is_some_and(|view| view.live.is_none() && view.transcript.complete)
    })
    .await
    .unwrap();
    wait_post_turn_noop(process, app, session_id).await.unwrap();
}

pub(super) async fn land_recent_history(process: &mut RpcProcess, app: &mut App, session_id: &str) {
    for index in 0..6 {
        land_turn(process, app, session_id, &format!("Question {index}")).await;
    }
}

fn copy_last(app: &mut App) -> String {
    app.composer_mut().set_text("/copy last");
    let commands = app.submit_composer();
    assert_eq!(commands.len(), 1, "copy is one local effect, with no RPC");
    match &commands[0] {
        AppCommand::CopySelection(text) => text.as_str().to_owned(),
        command => panic!("expected exact retained reply clipboard payload, got {command:?}"),
    }
}

#[test]
#[ignore = "requires MINICORE_AGENT_BIN; real stdio, compaction, cold reload and loopback model"]
fn e2e_default_compact_keeps_latest_copy_and_next_request_after_cold_reopen() {
    let agent_bin = require_agent_bin();
    let (env, _) = E2eEnvironment::setup();
    widen_manual_window(&env);
    enqueue_recent_history(&env);
    env._server
        .enqueue_sse(sse_text_response("PREFIX_CHECKPOINT"));
    env._server.enqueue_sse(sse_text_response("NEXT_ANSWER"));
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async move {
        let mut process = env.spawn_agent(&agent_bin);
        let mut app = App::new(env.workspace_path.clone());
        dispatch(&mut process, &mut app, AppEvent::Bootstrap)
            .await
            .unwrap();
        pump_until(&mut process, &mut app, |app| {
            app.connection == ConnectionState::Ready
        })
        .await
        .unwrap();
        let id = create_compact_session(
            &mut process,
            &mut app,
            &env.workspace_path,
            "Retained tail E2E",
        )
        .await;
        land_recent_history(&mut process, &mut app, &id).await;
        let original_copy = copy_last(&mut app);
        // The existing cell-to-text copy projection may append trailing
        // padding for a joined emoji. This regression isolates compaction:
        // retain exactly the same copy payload across both hot and cold
        // projections, while the actual model request below keeps raw text.
        assert_eq!(original_copy.trim_end(), LATEST);
        let directory = env.temp_dir.join("agent_data/sessions").join(&id);
        let history_path = directory.join("history.jsonl");
        let history_before = std::fs::read(&history_path).unwrap();
        submit_slash_command(&mut process, &mut app, "/compact")
            .await
            .unwrap();
        pump_until(&mut process, &mut app, |app| {
            let view = &app.sessions.known[&id];
            compact_status(app, &id) == Some(CompactStatusWire::Compacted)
                && !view.is_preparing()
                && !view.history_read.is_loading()
                && view.transcript.complete
                && view
                    .transcript
                    .window
                    .items()
                    .any(|(_, entry)| matches!(entry.as_ref(), TranscriptBlock::Summary(_)))
        })
        .await
        .unwrap();
        assert_eq!(copy_last(&mut app), original_copy);
        assert_eq!(std::fs::read(&history_path).unwrap(), history_before);
        let summary_before = std::fs::read(directory.join("summary.json")).unwrap();
        let snapshot: serde_json::Value = serde_json::from_slice(&summary_before).unwrap();
        assert_eq!(snapshot["format_version"], 1);
        assert_eq!(snapshot["source"]["covered_item_count"], 4);
        assert_eq!(snapshot["source"]["covered_loop_count"], 2);
        assert_eq!(env._server.recorded_requests().len(), 7);

        submit_slash_command(&mut process, &mut app, "/compact")
            .await
            .unwrap();
        pump_until(&mut process, &mut app, |app| {
            compact_status(app, &id) == Some(CompactStatusWire::Noop)
                && !app.sessions.known[&id].is_preparing()
        })
        .await
        .unwrap();
        assert_eq!(env._server.recorded_requests().len(), 7);
        assert_eq!(
            std::fs::read(directory.join("summary.json")).unwrap(),
            summary_before
        );
        assert_eq!(copy_last(&mut app), original_copy);

        run_slash_command(&mut process, &mut app, "/close confirm")
            .await
            .unwrap();
        pump_until(&mut process, &mut app, |app| {
            app.sessions.closed.contains(&id) && !app.sessions.known[&id].info.loaded
        })
        .await
        .unwrap();
        let report = drain_shutdown_strict(&mut process, &mut app).await.unwrap();
        assert!(report.shutdown_ok && report.seen_eof && report.seen_exit);
        process.terminate().await;

        // A genuinely new process reconstructs the v1 prefix and retained
        // suffix from disk. No in-memory copy card survives this boundary.
        let mut process = env.spawn_agent(&agent_bin);
        let mut app = App::new(env.workspace_path.clone());
        dispatch(&mut process, &mut app, AppEvent::Bootstrap)
            .await
            .unwrap();
        pump_until(&mut process, &mut app, |app| {
            app.connection == ConnectionState::Ready
        })
        .await
        .unwrap();
        dispatch(
            &mut process,
            &mut app,
            AppEvent::OpenSession {
                session_id: id.clone(),
            },
        )
        .await
        .unwrap();
        wait_for_session_ready(&mut process, &mut app, &id)
            .await
            .unwrap();
        assert_eq!(copy_last(&mut app), original_copy);
        assert_eq!(std::fs::read(&history_path).unwrap(), history_before);
        land_turn(&mut process, &mut app, &id, "after compact").await;
        let requests = env._server.recorded_requests();
        assert_eq!(requests.len(), 8);
        let request = requests.last().unwrap();
        let input = request.json["input"]
            .as_array()
            .expect("Responses input array");
        let assistant_texts = input
            .iter()
            .filter(|item| item["role"] == "assistant")
            .filter_map(|item| item["content"].as_array())
            .flatten()
            .filter_map(|part| part["text"].as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            assistant_texts
                .iter()
                .filter(|text| **text == LATEST)
                .count(),
            1,
            "the complete Unicode latest answer must survive exactly once"
        );
        assert_eq!(request.body.matches("LATEST_SHORT_ANSWER").count(), 1);
        assert!(request.body.contains("PREFIX_CHECKPOINT"));
        for old in ["ANSWER_0_", "ANSWER_1_"] {
            assert!(
                !request.body.contains(old),
                "summarized prefix leaked into tail"
            );
        }
        assert!(
            std::fs::read(&history_path)
                .unwrap()
                .starts_with(&history_before)
        );
        assert_eq!(copy_last(&mut app), "NEXT_ANSWER");
        let report = drain_shutdown_strict(&mut process, &mut app).await.unwrap();
        assert!(report.shutdown_ok && report.seen_eof && report.seen_exit);
        process.terminate().await;
    });
}
