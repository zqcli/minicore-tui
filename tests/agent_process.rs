//! A scripted minicore-agent stand-in, built only as a TEST target
//! (`harness = false`, non-installable — never a `[[bin]]`). It is spawned
//! by the in-crate harness in `src/rpc.rs` through the production
//! `RpcProcess::spawn`, which launches it as `agent_process --config <path>
//! --stdio`: the config file content selects the behavior mode. A bare run
//! (as `cargo test` executes this target's `main`) does nothing and exits 0.

use std::io::{self, BufRead, Write};
use std::process::ExitCode;
use std::time::Duration;

use serde_json::{Value, json};

fn fake_write_line(out: &mut impl Write, value: &Value) {
    let _ = writeln!(out, "{value}");
    let _ = out.flush();
}

fn fake_respond(out: &mut impl Write, id: &Value, result: Value) {
    fake_write_line(out, &json!({"jsonrpc": "2.0", "id": id, "result": result}));
}

fn fake_meta(session_id: &str, instance_id: &str) -> Value {
    json!({
        "session_id": session_id,
        "instance_id": instance_id,
        "dropped_before": 0
    })
}

fn fake_turn_ref(session_id: &str, instance_id: &str, turn_id: &str) -> Value {
    json!({
        "session_id": session_id,
        "instance_id": instance_id,
        "turn_id": turn_id
    })
}

fn fake_session_info(session_id: &str, workspace: &str) -> Value {
    json!({
        "session_id": session_id,
        "title": null,
        "profile": "coding",
        "workspace": workspace,
        "model": "gpt-4o",
        "reasoning": "auto",
        "loaded": true,
        "instance_id": "ins_fake",
        "created_at": "2026-01-02T03:04:05.006Z",
        "updated_at": "2026-01-02T03:04:05.006Z"
    })
}

fn fake_outcome(turn_id: &str) -> Value {
    json!({
        "turn_id": turn_id,
        "terminal": "completed",
        "usage": {"input_tokens": 10, "output_tokens": 20}
    })
}

fn fake_model_list() -> Value {
    json!({
        "models": [
            {
                "id": "gpt-4o",
                "model_ref": "openai/gpt-4o",
                "context_window": 128000,
                "supports_tools": true,
                "supported_reasoning": ["auto", "disabled", "low", "medium", "high"]
            },
            {
                "id": "fast",
                "model_ref": "acme/fast",
                "context_window": 32000,
                "supports_tools": true,
                "supported_reasoning": ["disabled"]
            }
        ]
    })
}

fn fake_profile_list() -> Value {
    json!({
        "profiles": [
            {
                "id": "coding",
                "model": "gpt-4o",
                "reasoning": "high",
                "tools": ["read", "write", "edit"]
            }
        ]
    })
}

/// One full turn: `output_delta` then `turn_finished`.
fn fake_emit_events(out: &mut impl Write, session_id: &str, turn_id: &str) {
    let turn = fake_turn_ref(session_id, "ins_fake", turn_id);
    let delta = json!({
        "jsonrpc": "2.0",
        "method": "agent.event",
        "params": {
            "type": "output_delta",
            "data": {
                "turn": turn.clone(),
                "channel": "text",
                "delta": "hello from the fake agent",
                "meta": fake_meta(session_id, "ins_fake")
            }
        }
    });
    let finished = json!({
        "jsonrpc": "2.0",
        "method": "agent.event",
        "params": {
            "type": "turn_finished",
            "data": {
                "turn": turn,
                "outcome": fake_outcome(turn_id),
                "meta": fake_meta(session_id, "ins_fake")
            }
        }
    });
    fake_write_line(out, &delta);
    fake_write_line(out, &finished);
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut config: Option<String> = None;
    while let Some(arg) = args.next() {
        if arg == "--config" {
            config = args.next();
        }
    }
    // A bare run (cargo test executing this target's main) is a no-op.
    let Some(config) = config else {
        return ExitCode::SUCCESS;
    };
    let mode = match std::fs::read_to_string(&config) {
        Ok(content) => content.trim().to_owned(),
        Err(error) => {
            eprintln!("agent_process: cannot read config {config}: {error}");
            return ExitCode::FAILURE;
        }
    };
    serve(&mode)
}

fn serve(mode: &str) -> ExitCode {
    let stdin = io::stdin();
    let mut out = io::stdout().lock();
    let mut input = stdin.lock();
    let mut line = String::new();
    let mut session_counter = 0u64;
    let mut turn_counter = 0u64;
    let mut session_ids: Vec<String> = Vec::new();
    let mut buffered: Vec<(Value, Value)> = Vec::new();

    loop {
        line.clear();
        if input.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        let frame: Value = match serde_json::from_str(line.trim()) {
            Ok(frame) => frame,
            Err(_) => continue,
        };
        let id = frame.get("id").cloned().unwrap_or(Value::Null);
        let method = match frame.get("method").and_then(Value::as_str) {
            Some(method) => method.to_owned(),
            None => continue,
        };
        let params = frame.get("params").cloned().unwrap_or_else(|| json!({}));

        match method.as_str() {
            "agent.ping" => {
                if mode == "crash" {
                    return ExitCode::from(1);
                }
                let result = json!({"version": "0.2.0"});
                if mode == "out_of_order" {
                    buffered.push((id, result));
                    if buffered.len() == 2 {
                        while let Some((late_id, late_result)) = buffered.pop() {
                            fake_respond(&mut out, &late_id, late_result);
                        }
                    }
                } else {
                    fake_respond(&mut out, &id, result);
                }
            }
            "model.list" => {
                let result = fake_model_list();
                if mode == "out_of_order" {
                    buffered.push((id, result));
                    if buffered.len() == 2 {
                        while let Some((late_id, late_result)) = buffered.pop() {
                            fake_respond(&mut out, &late_id, late_result);
                        }
                    }
                } else {
                    fake_respond(&mut out, &id, result);
                }
            }
            "profile.list" => fake_respond(&mut out, &id, fake_profile_list()),
            "session.list" => {
                let sessions: Vec<Value> = session_ids
                    .iter()
                    .map(|session_id| fake_session_info(session_id, "/ws/fake"))
                    .collect();
                fake_respond(&mut out, &id, json!({"sessions": sessions}));
            }
            "session.create" => {
                session_counter += 1;
                let session_id = format!("ses_fake_{session_counter}");
                session_ids.push(session_id.clone());
                let workspace = params
                    .get("workspace")
                    .and_then(Value::as_str)
                    .unwrap_or("/ws/fake");
                fake_respond(
                    &mut out,
                    &id,
                    json!({"session": fake_session_info(&session_id, workspace)}),
                );
            }
            "session.open" => {
                let session_id = params
                    .get("session_id")
                    .and_then(Value::as_str)
                    .unwrap_or("ses_fake_1");
                fake_respond(
                    &mut out,
                    &id,
                    json!({"session": fake_session_info(session_id, "/ws/fake")}),
                );
            }
            "session.state" => {
                let session_id = params
                    .get("session_id")
                    .and_then(Value::as_str)
                    .unwrap_or("ses_fake_1");
                let result = json!({
                    "session_id": session_id,
                    "instance_id": "ins_fake",
                    "status": "idle",
                    "health": "healthy",
                    "active_turn": null,
                    "pending_interaction": null,
                    "conversation_seq": 0,
                    "last_terminal": null
                });
                fake_respond(&mut out, &id, result);
            }
            "session.transcript" => {
                let result = json!({
                    "entries": [],
                    "next_after": null,
                    "observed_head": 0,
                    "complete": true
                });
                fake_respond(&mut out, &id, result);
            }
            "turn.send" => {
                turn_counter += 1;
                let session_id = params
                    .get("session_id")
                    .and_then(Value::as_str)
                    .unwrap_or("ses_fake_1");
                let turn_id = format!("trn_fake_{turn_counter}");
                let turn = fake_turn_ref(session_id, "ins_fake", &turn_id);
                if mode == "events_first" {
                    fake_emit_events(&mut out, session_id, &turn_id);
                    fake_respond(&mut out, &id, json!({"turn": turn}));
                } else {
                    fake_respond(&mut out, &id, json!({"turn": turn}));
                    fake_emit_events(&mut out, session_id, &turn_id);
                }
            }
            "turn.wait" => {
                let turn_id = params
                    .get("turn_id")
                    .and_then(Value::as_str)
                    .unwrap_or("trn_fake_1");
                fake_respond(&mut out, &id, fake_outcome(turn_id));
            }
            "turn.cancel" => fake_respond(&mut out, &id, json!({"cancelled": true})),
            "agent.shutdown" => {
                if mode == "hang" {
                    std::thread::sleep(Duration::from_secs(60));
                    return ExitCode::from(1);
                }
                fake_respond(&mut out, &id, json!({"ok": true}));
                return ExitCode::SUCCESS;
            }
            // Unknown methods answer a TOP-LEVEL JSON-RPC error (never an
            // error smuggled inside `result`).
            _ => {
                fake_write_line(
                    &mut out,
                    &json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": {"code": -32601, "message": "method not found"}
                    }),
                );
            }
        }
    }
    ExitCode::SUCCESS
}
