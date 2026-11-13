//! Real embedded Host/Runtime/provider tests with an explicitly scripted loopback server.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use minicore_host_core::{
    HistoryItemData, HostConfig, LogOutput, LogSettings, OpenAiModelApi, OpenAiModelSettings,
};
use minicore_host_protocol::{
    AgentConfig, AgentCreate, CallOptions, HistoryDirection, HistoryReadRequest, HostLifecycle,
    HostShutdownStrategy, MessageId, MessageInput, MessageMode, OperationId, SessionRef,
    StorageMode, TurnState,
};
use minicore_host_runtime::model::AssistantPart;
use minicore_tui::{
    config::{self, Home, Launch, Prepared, Request},
    host::Hosted,
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    process::Command,
    sync::oneshot,
};

const KEY_NAME: &str = "MINICORE_TUI_EMBED_FIXTURE_KEY";
const ANSWER: &str = "Native in-process answer 🐈";

#[tokio::test]
async fn embedded_chat_completion_survives_shutdown_and_reopen_with_saved_config() {
    let temp = tempfile::tempdir().unwrap();
    probe("chat", temp.path()).await;
    probe("reopen", temp.path()).await;
}

#[tokio::test]
async fn embedded_responses_uses_the_selected_protocol_without_fallback() {
    let temp = tempfile::tempdir().unwrap();
    probe("responses", temp.path()).await;
}

#[tokio::test]
async fn embedded_shutdown_waits_for_accepted_work_and_actual_persistent_cleanup() {
    let temp = tempfile::tempdir().unwrap();
    probe("drain", temp.path()).await;
    probe("reopen", temp.path()).await;
}

#[tokio::test]
async fn frontend_reload_follows_native_application_restart_and_failure_outcomes() {
    let temp = tempfile::tempdir().unwrap();
    probe("reload", temp.path()).await;
}

async fn probe(case: &str, root: &Path) {
    // Each child owns one process-global production diagnostics subscriber.
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--ignored", "--exact", "embedded_host_probe", "--nocapture"])
        .env("MINICORE_TUI_EMBED_CASE", case)
        .env("MINICORE_TUI_EMBED_ROOT", root)
        .env(KEY_NAME, "explicit-unused-fixture-credential")
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(30), command.output())
        .await
        .expect("bounded real Host integration probe")
        .unwrap();
    assert!(
        output.status.success(),
        "{case}:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "isolated production diagnostics; invoked by the integration cases"]
async fn embedded_host_probe() {
    let root = PathBuf::from(std::env::var_os("MINICORE_TUI_EMBED_ROOT").unwrap());
    let case = std::env::var("MINICORE_TUI_EMBED_CASE").unwrap();
    if case == "reload" {
        reload_frontend(&root).await;
        return;
    }
    if case == "reopen" {
        reopen(&root).await;
        return;
    }
    let api = if case == "responses" {
        OpenAiModelApi::Responses
    } else {
        OpenAiModelApi::ChatCompletions
    };
    let server = ScriptedServer::start(api).await;
    let home = fixture(&root, &server.endpoint, api);
    let launch = launch(&home);
    let mut hosted = Hosted::open(&launch).await.unwrap();
    let client = hosted.client();
    let initial = client.initial_info().clone();
    let effective = client.config(CallOptions::default()).await.unwrap();
    assert_eq!(effective.config, launch.initial_config);
    assert!(
        Hosted::open(&launch).await.is_err(),
        "a second owner must not acquire the same persistent root"
    );
    let receipt = client
        .create_agent(
            AgentCreate {
                operation_id: OperationId::new().unwrap(),
                name: "Native integration".into(),
                mode: StorageMode::Persistent,
                config: launch.new_agent.clone(),
                default_session: None,
                initial_message: None,
            },
            CallOptions::default(),
        )
        .await
        .unwrap();
    assert!(receipt.result.steps.agent.succeeded());
    assert!(receipt.result.steps.session.succeeded());
    let target = SessionRef {
        agent_id: receipt.agent_id.unwrap(),
        session_id: receipt.session_id.unwrap(),
    };
    let session = client.session(target.clone());
    let info = session.info(CallOptions::default()).await.unwrap();
    assert_eq!(info.config.role, launch.new_agent.role);
    let accepted = session
        .send(
            MessageInput {
                message_id: MessageId::new().unwrap(),
                mode: MessageMode::FollowUp,
                text: "fixture input".into(),
                in_reply_to: None,
            },
            CallOptions::default(),
        )
        .await
        .unwrap();
    let request = server.reached.await.unwrap();
    assert_eq!(request["model"], "fixture-provider-model");
    assert_eq!(request["stream"], true);
    if api == OpenAiModelApi::Responses {
        assert_eq!(request["store"], false);
        assert!(request.get("input").is_some());
        assert!(request.get("messages").is_none());
    } else {
        assert!(request.get("messages").is_some());
        assert!(request.get("input").is_none());
    }
    let state = session.state(CallOptions::default()).await.unwrap();
    let turn = state
        .data
        .active_turn_id
        .expect("the gated provider owns a real active Turn");
    if case == "drain" {
        hosted
            .request_shutdown(HostShutdownStrategy::Graceful)
            .unwrap();
        assert_eq!(
            hosted.snapshot().unwrap().lifecycle,
            HostLifecycle::Draining
        );
        let stopped = hosted.wait_stopped();
        tokio::pin!(stopped);
        assert!(
            futures_util::poll!(stopped.as_mut()).is_pending(),
            "acceptance is not cleanup completion"
        );
        server.release.send(()).unwrap();
        stopped.await.unwrap();
        server.task.await.unwrap();
    } else {
        server.release.send(()).unwrap();
        let result = session
            .wait_turn(turn.clone(), CallOptions::default())
            .await
            .unwrap();
        assert_eq!(
            result.data.info.as_ref().unwrap().state,
            TurnState::Finished
        );
        assert!(
            result.data.info.as_ref().unwrap().fault.is_none(),
            "scripted provider response must complete successfully: {:?}",
            result.data.info
        );
        assert!(
            result
                .data
                .history
                .iter()
                .any(|entry| assistant_answer(&entry.item))
        );
        server.task.await.unwrap();
    }
    std::fs::write(
        root.join("observed.json"),
        serde_json::to_vec(&json!({
            "host_id": initial.host_id,
            "incarnation": initial.host_incarnation,
            "target": target,
            "turn": turn,
            "message": accepted.message_id,
        }))
        .unwrap(),
    )
    .unwrap();
    hosted
        .shutdown(HostShutdownStrategy::Graceful)
        .await
        .unwrap();
    assert_eq!(hosted.snapshot().unwrap().lifecycle, HostLifecycle::Stopped);
    let log = std::fs::read_to_string(root.join("diagnostics.jsonl")).unwrap();
    assert!(!log.contains("explicit-unused-fixture-credential"));
    assert!(!log.contains("fixture input"));
    assert!(
        log.lines()
            .all(|line| serde_json::from_str::<Value>(line).is_ok())
    );
    assert_eq!(
        std::fs::read_to_string(root.join("session/old-agent.data")).unwrap(),
        "not migrated"
    );
}

async fn reopen(root: &Path) {
    let home = Home::resolve(Some(root)).unwrap();
    let saved: Value =
        serde_json::from_slice(&std::fs::read(root.join("observed.json")).unwrap()).unwrap();
    let original = config::load_host_config(&home.host_config_file()).unwrap();
    std::fs::write(home.host_config_file(), "{ invalid default candidate").unwrap();
    let mut hosted = Hosted::open(&launch(&home)).await.unwrap();
    let client = hosted.client();
    assert_eq!(
        serde_json::to_value(&client.initial_info().host_id).unwrap(),
        saved["host_id"]
    );
    assert_ne!(
        serde_json::to_value(&client.initial_info().host_incarnation).unwrap(),
        saved["incarnation"]
    );
    assert_eq!(
        client.config(CallOptions::default()).await.unwrap().config,
        original
    );
    let target: SessionRef = serde_json::from_value(saved["target"].clone()).unwrap();
    let session = client.session(target);
    let state = session.state(CallOptions::default()).await.unwrap();
    assert!(
        state.data.active_turn_id.is_none(),
        "reopen must not reconstruct execution"
    );
    assert!(state.data.next_turn.is_empty() && state.data.next_step.is_empty());
    let history = session
        .history(
            HistoryReadRequest {
                direction: HistoryDirection::Forward,
                cursor: None,
                limit: 100,
            },
            CallOptions::default(),
        )
        .await
        .unwrap();
    assert!(
        history
            .data
            .items
            .iter()
            .any(|entry| assistant_answer(&entry.item))
    );
    assert!(history.data.items.iter().any(|entry| matches!(&entry.item, HistoryItemData::User { message_id: Some(message_id), text, .. } if text == "fixture input" && serde_json::to_value(message_id).unwrap() == saved["message"])));
    let turn = serde_json::from_value(saved["turn"].clone()).unwrap();
    let result = session
        .turn_result(turn, CallOptions::default())
        .await
        .unwrap();
    assert!(result.data.info.is_none());
    assert!(result.data.saved_terminal.is_some());
    hosted
        .shutdown(HostShutdownStrategy::Graceful)
        .await
        .unwrap();
}

fn assistant_answer(item: &HistoryItemData) -> bool {
    matches!(item, HistoryItemData::Assistant { response } if response.content.iter().any(|part| matches!(part, AssistantPart::Text(text) if text.as_ref() == ANSWER)))
}

fn launch(home: &Home) -> Box<Launch> {
    let Prepared::Launch(launch) = config::prepare(home, &Request::default()).unwrap() else {
        panic!("configured fixture must launch");
    };
    launch
}

fn fixture(root: &Path, endpoint: &str, api: OpenAiModelApi) -> Home {
    let home = Home::resolve(Some(root)).unwrap();
    let model = "fixture/native".parse().unwrap();
    let config = HostConfig {
        name: "Embedded integration fixture".into(),
        models: vec![OpenAiModelSettings {
            model,
            api,
            endpoint: endpoint.into(),
            provider_model: "fixture-provider-model".into(),
            api_key_env: KEY_NAME.into(),
            context_window: 128_000,
            supported_reasoning: vec![Default::default()],
            supports_tools: true,
            request_timeout_ms: None,
        }],
        logging: LogSettings {
            output: LogOutput::File {
                path: root.join("diagnostics.jsonl").to_string_lossy().into(),
            },
            ..LogSettings::default()
        },
        ..HostConfig::default()
    };
    let mut agent = AgentConfig::default();
    agent.role.instructions = "Native Role fixture".into();
    agent.session_defaults.model = config.models[0].model.clone();
    agent.session_defaults.execution.model_retry_attempts = 1;
    agent.session_defaults.execution.compaction.enabled = false;
    std::fs::write(home.config_file(), "[ui]\nbell = false\n").unwrap();
    std::fs::write(
        home.host_config_file(),
        serde_json::to_vec_pretty(&config).unwrap(),
    )
    .unwrap();
    std::fs::write(
        home.agent_defaults_file(),
        serde_json::to_vec_pretty(&agent).unwrap(),
    )
    .unwrap();
    std::fs::create_dir_all(root.join("session")).unwrap();
    std::fs::write(root.join("session/old-agent.data"), "not migrated").unwrap();
    home
}

async fn reload_frontend(root: &Path) {
    use minicore_host_core::LogLevel;
    use minicore_host_protocol::HostReloadResult;
    use minicore_tui::app::{App, MESSAGE_CAPACITY, Msg};

    let home = fixture(
        root,
        "http://127.0.0.1:1/v1/chat/completions",
        OpenAiModelApi::ChatCompletions,
    );
    let launch = launch(&home);
    let mut hosted = Hosted::open(&launch).await.unwrap();
    let client = hosted.client();
    let (tx, mut messages) = tokio::sync::mpsc::channel(MESSAGE_CAPACITY);
    let mut app = App::new(client.clone(), tx, root.into());
    app.home = Some(home.clone());
    app.host_config_file = Some(home.host_config_file());
    app.new_agent = launch.new_agent.clone();

    let mut candidate = launch.initial_config.clone();
    candidate.name = "Applied through the real frontend".into();
    candidate.models[0].model = "fixture/reloaded".parse().unwrap();
    let mut defaults = launch.new_agent.clone();
    defaults.session_defaults.model = candidate.models[0].model.clone();
    defaults.role.instructions = "Reloaded creation instructions".into();
    std::fs::write(
        home.host_config_file(),
        serde_json::to_vec(&candidate).unwrap(),
    )
    .unwrap();
    std::fs::write(
        home.agent_defaults_file(),
        serde_json::to_vec(&defaults).unwrap(),
    )
    .unwrap();
    std::fs::create_dir_all(home.commands_dir()).unwrap();
    std::fs::write(home.commands_dir().join("review-fixture.md"), "Review $1").unwrap();
    assert!(matches!(
        run_reload(&mut app, &mut messages).await.unwrap(),
        HostReloadResult::Applied { .. }
    ));
    assert_eq!(app.new_agent, defaults);
    assert_eq!(app.templates[0].name, "review-fixture");
    while app.models.is_empty() {
        app.on_msg(messages.recv().await.unwrap());
    }
    assert_eq!(app.models[0].model.as_str(), "fixture/reloaded");
    let applied = client.config(CallOptions::default()).await.unwrap();
    assert_eq!(applied.config, candidate);

    let mut restart = candidate.clone();
    restart.logging.level = LogLevel::Debug;
    std::fs::write(
        home.host_config_file(),
        serde_json::to_vec(&restart).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        run_reload(&mut app, &mut messages).await.unwrap(),
        HostReloadResult::RestartRequired { .. }
    ));
    assert!(
        app.notices
            .iter()
            .any(|notice| notice.text.contains("requires a restart"))
    );
    assert_eq!(
        client.config(CallOptions::default()).await.unwrap(),
        applied
    );

    let mut failing = candidate.clone();
    failing.models[0].endpoint = "not a provider URL".into();
    std::fs::write(
        home.host_config_file(),
        serde_json::to_vec(&failing).unwrap(),
    )
    .unwrap();
    assert!(run_reload(&mut app, &mut messages).await.is_err());
    assert_eq!(
        client.config(CallOptions::default()).await.unwrap(),
        applied
    );
    assert_eq!(app.new_agent, defaults);
    assert!(
        app.notices
            .iter()
            .any(|notice| notice.level == minicore_tui::app::NoticeLevel::Error)
    );
    assert!(
        !app.notices
            .iter()
            .any(|notice| notice.text.contains("not a provider URL"))
    );

    messages.close();
    while let Ok(message) = messages.try_recv() {
        if let Msg::Catalogs(result) = message {
            result.unwrap();
        }
    }
    drop(app);
    hosted
        .shutdown(HostShutdownStrategy::Graceful)
        .await
        .unwrap();
    assert_eq!(hosted.snapshot().unwrap().lifecycle, HostLifecycle::Stopped);
}

async fn run_reload(
    app: &mut minicore_tui::app::App,
    messages: &mut tokio::sync::mpsc::Receiver<minicore_tui::app::Msg>,
) -> Result<minicore_host_protocol::HostReloadResult, String> {
    use minicore_tui::app::{Action, Msg};
    app.on_action(Action::ReloadHost);
    loop {
        let message = messages.recv().await.unwrap();
        if let Msg::Reloaded(result) = message {
            let outcome = result
                .as_ref()
                .map(|reloaded| reloaded.result.clone())
                .map_err(Clone::clone);
            app.on_msg(Msg::Reloaded(result));
            return outcome;
        }
        app.on_msg(message);
    }
}

struct ScriptedServer {
    endpoint: String,
    reached: oneshot::Receiver<Value>,
    release: oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

impl ScriptedServer {
    async fn start(api: OpenAiModelApi) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let path = if api == OpenAiModelApi::Responses {
            "/v1/responses"
        } else {
            "/v1/chat/completions"
        };
        let endpoint = format!("http://{}{path}", listener.local_addr().unwrap());
        let (request_tx, reached) = oneshot::channel();
        let (release, response_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let (actual_path, request) = read_request(&mut socket).await;
            assert_eq!(actual_path, path);
            request_tx.send(request).unwrap();
            response_rx.await.unwrap();
            let body = response(api);
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            socket.write_all(headers.as_bytes()).await.unwrap();
            socket.write_all(body.as_bytes()).await.unwrap();
            socket.shutdown().await.unwrap();
        });
        Self {
            endpoint,
            reached,
            release,
            task,
        }
    }
}

async fn read_request(socket: &mut TcpStream) -> (String, Value) {
    let mut bytes = Vec::new();
    let boundary = loop {
        if let Some(at) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            break at + 4;
        }
        let mut chunk = [0; 4096];
        let count = socket.read(&mut chunk).await.unwrap();
        assert_ne!(count, 0);
        bytes.extend_from_slice(&chunk[..count]);
    };
    let head = std::str::from_utf8(&bytes[..boundary]).unwrap();
    let path = head
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .to_owned();
    let length: usize = head
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().unwrap())
        })
        .unwrap();
    while bytes.len() < boundary + length {
        let mut chunk = [0; 4096];
        let count = socket.read(&mut chunk).await.unwrap();
        assert_ne!(count, 0);
        bytes.extend_from_slice(&chunk[..count]);
    }
    (
        path,
        serde_json::from_slice(&bytes[boundary..boundary + length]).unwrap(),
    )
}

fn response(api: OpenAiModelApi) -> String {
    if api == OpenAiModelApi::ChatCompletions {
        return format!(
            "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
            json!({"choices":[{"index":0,"delta":{"content":ANSWER},"finish_reason":null}]}),
            json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]})
        );
    }
    let part = json!({"type":"output_text","text":ANSWER});
    let item = json!({"type":"message","id":"msg_fixture","role":"assistant","status":"completed","content":[part]});
    [
        json!({"type":"response.created","response":{"id":"resp_fixture","status":"in_progress","output":[]}}),
        json!({"type":"response.output_item.added","output_index":0,"item":{"type":"message","id":"msg_fixture","role":"assistant","status":"in_progress","content":[]}}),
        json!({"type":"response.content_part.added","output_index":0,"item_id":"msg_fixture","content_index":0,"part":{"type":"output_text","text":""}}),
        json!({"type":"response.output_text.delta","output_index":0,"item_id":"msg_fixture","content_index":0,"delta":ANSWER}),
        json!({"type":"response.output_text.done","output_index":0,"item_id":"msg_fixture","content_index":0,"text":ANSWER}),
        json!({"type":"response.content_part.done","output_index":0,"item_id":"msg_fixture","content_index":0,"part":part}),
        json!({"type":"response.output_item.done","output_index":0,"item":item}),
        json!({"type":"response.completed","response":{"id":"resp_fixture","status":"completed","output":[item]}}),
    ].into_iter().enumerate().map(|(sequence, mut event)| {
        event["sequence_number"] = json!(sequence);
        format!("event: {}\ndata: {event}\n\n", event["type"].as_str().unwrap())
    }).collect()
}
