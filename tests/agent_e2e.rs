//! End-to-end test driving a REAL `minicore-agent` through the production
//! `RpcProcess` + `App` (not a bespoke protocol client). Offline by default:
//!
//! ```text
//! MINICORE_AGENT_BIN=~/minicore-agent-e2e/target/release/minicore-agent \
//! MINICORE_AGENT_CONFIG=/tmp/mct-e2e/agent.toml \
//! cargo test --test agent_e2e -- --ignored
//! ```
//!
//! The config must point at a loopback mock provider (localhost / 127.0.0.1)
//! and must not embed real credentials; this test refuses anything else and
//! never touches the user's real config or data directory — it uses its own
//! RAII temp workspace (removed on drop, including panic unwinds).

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use minicore_tui::app::{App, ConnectionState, RequestKind};
use minicore_tui::command::AppCommand;
use minicore_tui::event::{AppEvent, RpcEvent};
use minicore_tui::protocol::{IncomingFrame, RequestId};
use minicore_tui::rpc::RpcProcess;
use minicore_tui::state::TranscriptBlock;
use minicore_tui::theme::ThemeKind;
use url::Url;

const E2E_TIMEOUT: Duration = Duration::from_secs(120);

/// Unique per-run temp workspace with RAII cleanup; never the user's real
/// directories. `Drop` removes it even when a test panics.
struct E2eWorkspace {
    path: PathBuf,
}

impl E2eWorkspace {
    fn new() -> Self {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "minicore-tui-e2e-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).expect("create the E2E temp workspace");
        Self { path }
    }

    fn data_dir(&self) -> PathBuf {
        self.path.join("data")
    }

    fn workspace_dir(&self) -> PathBuf {
        self.path.join("workspace")
    }

    fn derived_config_path(&self) -> PathBuf {
        self.path.join("agent.toml")
    }
}

impl Drop for E2eWorkspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn assistant_count(app: &App, session_id: &str) -> usize {
    app.sessions
        .known
        .get(session_id)
        .map(|view| {
            view.transcript
                .blocks
                .iter()
                .filter(|block| matches!(block, TranscriptBlock::Assistant(_)))
                .count()
        })
        .unwrap_or(0)
}

/// The last durable assistant text, if any (a known loopback response can be
/// asserted against this; user-provided input text must never be relied on).
fn last_assistant_text(app: &App, session_id: &str) -> Option<String> {
    let view = app.sessions.known.get(session_id)?;
    view.transcript
        .blocks
        .iter()
        .rev()
        .find_map(|block| match block {
            TranscriptBlock::Assistant(card) => Some(
                card.parts
                    .iter()
                    .filter_map(|part| match part {
                        minicore_tui::state::AssistantPart::Text(text) => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            _ => None,
        })
}

/// Count only durable user entries: a local optimistic card has neither a
/// sequence nor a cleared `pending` flag.
fn durable_user_count(app: &App, session_id: &str) -> usize {
    app.sessions
        .known
        .get(session_id)
        .map(|view| {
            view.transcript
                .blocks
                .iter()
                .filter(|block| {
                    matches!(
                        block,
                        TranscriptBlock::User(card)
                            if !card.pending && card.seq.is_some()
                    )
                })
                .count()
        })
        .unwrap_or(0)
}

fn durable_user_exists(app: &App, session_id: &str, input: &str, turn_id: &str) -> bool {
    app.sessions.known.get(session_id).is_some_and(|view| {
        view.transcript.blocks.iter().any(|block| {
            matches!(
                block,
                TranscriptBlock::User(card)
                    if !card.pending
                        && card.seq.is_some()
                        && card.text == input
                        && card.turn_id.as_deref() == Some(turn_id)
            )
        })
    })
}

fn last_assistant_turn_id(app: &App, session_id: &str) -> Option<String> {
    app.sessions
        .known
        .get(session_id)?
        .transcript
        .blocks
        .iter()
        .rev()
        .find_map(|block| match block {
            TranscriptBlock::Assistant(card) => Some(card.turn_id.clone()),
            _ => None,
        })
}

fn validate_model_credentials(value: &toml::Value, path: &str) -> Result<(), String> {
    match value {
        toml::Value::Table(table) => {
            for (key, value) in table {
                let lower = key.to_ascii_lowercase();
                let normalized = lower.replace('-', "_");
                let forbidden = matches!(
                    normalized.as_str(),
                    "api_key"
                        | "apikey"
                        | "api_key_value"
                        | "token"
                        | "password"
                        | "secret"
                        | "authorization"
                        | "bearer"
                        | "credential"
                        | "credentials"
                ) || normalized.ends_with("_token")
                    || normalized.contains("credential");
                if forbidden && normalized != "api_key_env" {
                    return Err(format!(
                        "model field `{path}.{key}` may not contain credentials"
                    ));
                }
                if normalized == "api_key_env"
                    && value.as_str().is_none_or(|name| !is_environment_name(name))
                {
                    return Err(format!(
                        "model field `{path}.{key}` must name an environment variable"
                    ));
                }
                validate_model_credentials(value, &format!("{path}.{key}"))?;
            }
        }
        toml::Value::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                validate_model_credentials(value, &format!("{path}[{index}]"))?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn is_environment_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

fn validate_loopback_config(content: &str) -> Result<toml::Value, String> {
    let value: toml::Value =
        toml::from_str(content).map_err(|error| format!("invalid TOML: {error}"))?;
    let models = value
        .get("models")
        .and_then(toml::Value::as_table)
        .ok_or_else(|| "E2E config must contain a [models] table".to_owned())?;
    if models.is_empty() {
        return Err("E2E config must contain at least one model".to_owned());
    }
    for (model_id, model) in models {
        let model = model
            .as_table()
            .ok_or_else(|| format!("models.{model_id} must be a TOML table"))?;
        let base_url = model
            .get("base_url")
            .and_then(toml::Value::as_str)
            .ok_or_else(|| format!("models.{model_id}.base_url is required"))?;
        let url = Url::parse(base_url)
            .map_err(|error| format!("models.{model_id}.base_url is invalid: {error}"))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(format!("models.{model_id}.base_url must use http or https"));
        }
        let authority = base_url
            .split_once("://")
            .and_then(|(_, rest)| rest.split(['/', '?', '#']).next())
            .unwrap_or_default();
        if authority.contains('@') || !url.username().is_empty() || url.password().is_some() {
            return Err(format!(
                "models.{model_id}.base_url may not contain userinfo"
            ));
        }
        let host = url
            .host_str()
            .ok_or_else(|| format!("models.{model_id}.base_url must contain a host"))?;
        let ip_host = host
            .strip_prefix('[')
            .and_then(|host| host.strip_suffix(']'))
            .unwrap_or(host);
        let loopback = host.eq_ignore_ascii_case("localhost")
            || ip_host
                .parse::<IpAddr>()
                .map(|address| address.is_loopback())
                .unwrap_or(false);
        if !loopback {
            return Err(format!(
                "models.{model_id}.base_url must target localhost or a loopback IP"
            ));
        }
        if !model.contains_key("api_key_env") {
            return Err(format!(
                "models.{model_id}.api_key_env is required; credentials must come from the environment"
            ));
        }
        validate_model_credentials(
            &toml::Value::Table(model.clone()),
            &format!("models.{model_id}"),
        )?;
    }
    Ok(value)
}

#[cfg(test)]
mod config_tests {
    use super::*;

    fn model_config(urls: &[(&str, &str)]) -> String {
        let models = urls
            .iter()
            .map(|(id, url)| {
                format!("[models.{id}]\nbase_url = \"{url}\"\napi_key_env = \"MCT_MOCK_KEY\"\n")
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "data_dir = \"/user/store\"\n\n{models}\n[profiles.mock]\nmodel = \"{}\"\nreasoning = \"low\"\n",
            urls[0].0
        )
    }

    #[test]
    fn config_validation_uses_parsed_urls_not_comments_or_substrings() {
        let commented_remote = r#"
# localhost is only a comment
[models.remote]
base_url = "https://example.com/v1" # localhost
api_key_env = "MCT_MOCK_KEY"
"#;
        assert!(validate_loopback_config(commented_remote).is_err());

        let mixed = model_config(&[
            ("local", "http://127.0.0.1:8100/v1"),
            ("remote", "https://example.com/v1"),
        ]);
        assert!(validate_loopback_config(&mixed).is_err());

        let hardcoded = r#"
[models.local]
base_url = "http://localhost:8100/v1"
api_key_env = "MCT_MOCK_KEY"
api_key = "not-allowed"
"#;
        assert!(validate_loopback_config(hardcoded).is_err());
    }

    #[test]
    fn config_validation_accepts_localhost_ipv4_and_ipv6_only() {
        for url in [
            "http://localhost:8100/v1",
            "https://LOCALHOST/v1",
            "http://127.0.0.1:8100/v1",
            "http://[::1]:8100/v1",
        ] {
            let config = model_config(&[("local", url)]);
            assert!(
                validate_loopback_config(&config).is_ok(),
                "expected loopback URL to pass: {url}"
            );
        }
        let userinfo = model_config(&[("local", "http://user:pass@localhost/v1")]);
        assert!(validate_loopback_config(&userinfo).is_err());
        let unsupported = model_config(&[("local", "ftp://localhost/v1")]);
        assert!(validate_loopback_config(&unsupported).is_err());
        let missing_host = model_config(&[("local", "http:///v1")]);
        assert!(validate_loopback_config(&missing_host).is_err());
    }

    #[test]
    fn derived_config_is_isolated_and_does_not_modify_the_original() {
        let workspace = E2eWorkspace::new();
        let original_path = workspace.path.join("original.toml");
        let original = model_config(&[("local", "http://127.0.0.1:8100/v1")]);
        std::fs::write(&original_path, &original).expect("write original config");
        let derived = derive_e2e_config(
            original_path.to_str().expect("utf8 original path"),
            &workspace,
        )
        .expect("derive isolated config");
        let derived_value: toml::Value =
            toml::from_str(&std::fs::read_to_string(&derived).expect("read derived config"))
                .expect("derived TOML parses");
        let original_value = validate_loopback_config(&original).expect("original parses");
        let expected_data_dir = workspace.data_dir().to_string_lossy().into_owned();
        assert_eq!(
            derived_value.get("data_dir").and_then(toml::Value::as_str),
            Some(expected_data_dir.as_str())
        );
        assert_eq!(derived_value.get("models"), original_value.get("models"));
        assert_eq!(
            derived_value.get("profiles"),
            original_value.get("profiles")
        );
        assert_eq!(
            std::fs::read_to_string(&original_path).expect("original remains readable"),
            original
        );
        assert!(derived.starts_with(&workspace.path));
        assert!(workspace.data_dir().starts_with(&workspace.path));
        assert!(workspace.workspace_dir().starts_with(&workspace.path));
    }
}

fn derive_e2e_config(original_path: &str, workspace: &E2eWorkspace) -> Result<PathBuf, String> {
    let original = std::fs::read_to_string(original_path)
        .map_err(|error| format!("read E2E config: {error}"))?;
    let mut value = validate_loopback_config(&original)?;
    std::fs::create_dir_all(workspace.data_dir())
        .map_err(|error| format!("create E2E data dir: {error}"))?;
    std::fs::create_dir_all(workspace.workspace_dir())
        .map_err(|error| format!("create E2E workspace: {error}"))?;
    value
        .as_table_mut()
        .expect("validated TOML root is a table")
        .insert(
            "data_dir".to_owned(),
            toml::Value::String(workspace.data_dir().to_string_lossy().into_owned()),
        );
    let derived_path = workspace.derived_config_path();
    let rendered =
        toml::to_string(&value).map_err(|error| format!("serialize E2E config: {error}"))?;
    std::fs::write(&derived_path, rendered)
        .map_err(|error| format!("write derived E2E config: {error}"))?;
    Ok(derived_path)
}

/// Sends the RPC side effects of one update — the same
/// `App::update` → executor → `RpcProcess::send` wire flow as the real
/// main loop, minus the terminal. Returns whether the app wants to exit.
async fn send_commands(
    process: &mut RpcProcess,
    commands: Vec<AppCommand>,
) -> Result<bool, String> {
    let mut exit = false;
    for command in commands {
        match command {
            AppCommand::Rpc(request) => process
                .send(request)
                .await
                .map_err(|error| format!("agent.send failed: {error}"))?,
            AppCommand::KillChild => process.kill_child(),
            AppCommand::Exit => exit = true,
        }
    }
    Ok(exit)
}

/// Pumps events until `predicate` holds, feeding every update's follow-up
/// requests back into the process. On the way it pins the same-update wait
/// contract: a `turn.send` response must produce exactly one `turn.wait` in
/// the same `App::update`, and that id must already be registered in the
/// pending map before the request is written — so a race can never beat the
/// registration (spec 13.3).
async fn pump_until<F>(
    app: &mut App,
    process: &mut RpcProcess,
    checks: &mut TurnChecks,
    predicate: F,
) -> Result<(), String>
where
    F: Fn(&App) -> bool,
{
    let deadline = Instant::now() + E2E_TIMEOUT;
    loop {
        if predicate(app) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "e2e timed out; connection={:?} active={:?}",
                app.connection, app.sessions.active
            ));
        }
        let event = tokio::time::timeout(Duration::from_secs(20), process.recv())
            .await
            .map_err(|_| "timed out waiting for an agent event".to_owned())?
            .ok_or_else(|| "the agent channel closed before the flow finished".to_owned())?;
        if let RpcEvent::Exited(_) = &event {
            return Err(format!("the agent exited early: {event:?}"));
        }

        // Which pending kind (if any) the response is resolving, captured
        // before the update consumes it.
        let resolved = match &event {
            RpcEvent::Frame(IncomingFrame::Response(response)) => {
                app.pending_request_kind(response.id).cloned()
            }
            _ => None,
        };
        let commands = app.update(AppEvent::Rpc(event));
        let wait_ids: Vec<RequestId> = commands
            .iter()
            .filter_map(|command| match command {
                AppCommand::Rpc(request) if request.method == "turn.wait" => Some(request.id),
                _ => None,
            })
            .collect();
        let send_resolved = matches!(&resolved, Some(RequestKind::SendTurn { .. }));
        if send_resolved {
            assert_eq!(
                wait_ids.len(),
                1,
                "a turn.send response must issue exactly one turn.wait in the same update"
            );
        }
        for wait_id in wait_ids {
            assert!(
                app.request_is_pending(wait_id),
                "the turn.wait id {} must be registered before it is sent",
                wait_id.0,
            );
            checks.waits_registered += 1;
            if send_resolved {
                checks.waits_after_send += 1;
            }
        }
        if send_commands(process, commands).await? {
            return Err("the app asked to exit before the flow finished".to_owned());
        }
    }
}

#[derive(Default)]
struct TurnChecks {
    /// turn.wait commands issued in the same update that resolved a send.
    waits_after_send: usize,
    /// turn.wait ids already registered in the pending map when they left
    /// update.
    waits_registered: usize,
}

#[test]
#[ignore = "offline by default: requires a real loopback-mocked minicore-agent; run with `cargo test --test agent_e2e -- --ignored`"]
fn real_agent_multi_turn_flow() {
    let agent_bin = std::env::var("MINICORE_AGENT_BIN").unwrap_or_else(|_| {
        panic!("agent_e2e needs MINICORE_AGENT_BIN (see tests/agent_e2e.rs for the run command)")
    });
    let agent_config = std::env::var("MINICORE_AGENT_CONFIG").unwrap_or_else(|_| {
        panic!(
            "agent_e2e needs MINICORE_AGENT_CONFIG pointing at a loopback mock (never a real OpenAI key)"
        )
    });
    let workspace = E2eWorkspace::new();
    let derived_config = derive_e2e_config(&agent_config, &workspace)
        .unwrap_or_else(|error| panic!("agent_e2e config rejected: {error}"));

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    runtime
        .block_on(run_e2e(&agent_bin, &derived_config, &workspace))
        .expect("agent e2e failed");
}

async fn run_e2e(
    agent_bin: &str,
    agent_config: &Path,
    workspace: &E2eWorkspace,
) -> Result<(), String> {
    let mut process = RpcProcess::spawn(std::path::Path::new(agent_bin), agent_config)
        .map_err(|error| format!("spawn: {error}"))?;

    let result = run_e2e_flow(&mut process, workspace).await;
    process.terminate().await;
    result
}

async fn run_e2e_flow(process: &mut RpcProcess, workspace: &E2eWorkspace) -> Result<(), String> {
    let mut app = App::new(workspace.workspace_dir());
    app.update(AppEvent::SetTheme(ThemeKind::Dark));
    let mut checks = TurnChecks::default();

    // Discovery: the four responses may arrive in any order; Ready only
    // after all of them (the pending map correlates by id).
    send_commands(process, app.update(AppEvent::Bootstrap)).await?;
    pump_until(&mut app, process, &mut checks, |app| {
        app.connection == ConnectionState::Ready
    })
    .await?;
    assert!(!app.catalogs.models.is_empty(), "model.list populated");
    assert!(!app.catalogs.profiles.is_empty(), "profile.list populated");

    // Create a session in the RAII temp workspace.
    send_commands(
        process,
        app.update(AppEvent::CreateSession {
            workspace: workspace.workspace_dir().to_string_lossy().into_owned(),
            profile: None,
            model: None,
            reasoning: None,
            title: Some("e2e session".to_owned()),
        }),
    )
    .await?;
    pump_until(&mut app, process, &mut checks, |app| {
        app.sessions.active.is_some()
    })
    .await?;
    let session_id = app.sessions.active.clone().expect("active session");

    // Two full turns. Each waits for a NEW durable assistant block and a
    // cleared live turn — never for the user text alone, which a durable
    // UserBlock would satisfy spuriously.
    for marker in ["e2e-first-marker", "e2e-second-marker"] {
        let assistant_before = assistant_count(&app, &session_id);
        let user_before = durable_user_count(&app, &session_id);
        send_commands(
            process,
            app.update(AppEvent::SubmitTurn {
                session_id: session_id.clone(),
                text: marker.to_owned(),
            }),
        )
        .await?;
        pump_until(&mut app, process, &mut checks, |app| {
            assistant_count(app, &session_id) > assistant_before
                && app
                    .sessions
                    .known
                    .get(&session_id)
                    .is_none_or(|view| view.live.is_none())
        })
        .await?;

        // The turn is durably reconciled: one new assistant block, the
        // durable user entry for this input, a complete transcript, and no
        // live turn left behind.
        let view = app.sessions.known.get(&session_id).expect("session view");
        assert_eq!(
            assistant_count(&app, &session_id),
            assistant_before + 1,
            "turn `{marker}` adds exactly one durable assistant block"
        );
        assert_eq!(
            durable_user_count(&app, &session_id),
            user_before + 1,
            "turn `{marker}` adds exactly one durable user entry"
        );
        let turn_id = last_assistant_turn_id(&app, &session_id)
            .expect("the new assistant block has a turn id");
        assert!(
            durable_user_exists(&app, &session_id, marker, &turn_id),
            "the durable user message for `{marker}` has a sequence, is not pending, and matches the assistant turn"
        );
        assert!(
            view.transcript.complete,
            "the transcript is durable and complete after the turn"
        );
        assert!(view.live.is_none(), "the live turn cleared after reconcile");
        let assistant_text =
            last_assistant_text(&app, &session_id).expect("the new assistant block has text");
        assert!(
            !assistant_text.trim().is_empty(),
            "the new assistant block carries non-empty text"
        );
        assert!(
            assistant_text.contains("e2e mock answered"),
            "the loopback mock's known answer reached the durable transcript: {assistant_text:?}"
        );
    }

    // Every send response issued its wait in the same update and every wait
    // id was registered before it was written.
    assert_eq!(
        checks.waits_after_send, 2,
        "both turns resolved their send with a same-update turn.wait"
    );
    assert_eq!(
        checks.waits_registered, 2,
        "both turn.wait ids were registered in the pending map before being sent"
    );

    // Orderly shutdown: agent.shutdown response, stdout close and child
    // exit may race; the app exits and the waiter reaps the child.
    let shutdown = app
        .update(AppEvent::ShutdownRequested)
        .into_iter()
        .find_map(|command| match command {
            AppCommand::Rpc(request)
                if request.method == minicore_tui::protocol::METHOD_SHUTDOWN =>
            {
                Some(request)
            }
            _ => None,
        })
        .ok_or_else(|| "the app did not issue agent.shutdown".to_owned())?;
    let shutdown_id = shutdown.id;
    send_commands(process, vec![AppCommand::Rpc(shutdown)]).await?;
    let mut saw_shutdown_response = false;
    let mut saw_exit_success = false;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if Instant::now() >= deadline {
            return Err("agent.shutdown did not finish in time".to_owned());
        }
        let event = match tokio::time::timeout(Duration::from_secs(10), process.recv()).await {
            Err(_) => return Err("timed out waiting for the agent shutdown".to_owned()),
            Ok(None) => break,
            Ok(Some(event)) => event,
        };
        if let RpcEvent::Frame(IncomingFrame::Response(response)) = &event {
            if response.id == shutdown_id {
                assert_eq!(
                    app.pending_request_kind(response.id),
                    Some(&RequestKind::Shutdown),
                    "the shutdown response must resolve the registered shutdown request"
                );
                let result = response
                    .parse_shutdown()
                    .map_err(|error| format!("agent.shutdown response failed: {error}"))?;
                assert!(result.ok, "agent.shutdown must return ok=true");
                saw_shutdown_response = true;
            }
        }
        let exited_success = match &event {
            RpcEvent::Exited(Some(status)) => Some(status.success()),
            _ => None,
        };
        let commands = app.update(AppEvent::Rpc(event));
        if let Some(success) = exited_success {
            assert!(
                success,
                "the agent must exit successfully after agent.shutdown"
            );
            saw_exit_success = true;
        }
        if send_commands(process, commands).await? {
            break;
        }
    }
    assert_eq!(app.connection, ConnectionState::ShuttingDown);
    assert!(
        saw_shutdown_response,
        "the registered agent.shutdown response was observed"
    );
    assert!(saw_exit_success, "the agent exited successfully");
    assert_eq!(app.child_exit_status.as_deref(), Some("exit code 0"));
    Ok(())
}
