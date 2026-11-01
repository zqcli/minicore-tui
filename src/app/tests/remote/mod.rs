//! Phone connection, input ownership and independent session views.

use std::sync::{Arc, Mutex};

use crossterm::event::KeyCode;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot, watch};

use super::harness::*;
use super::phone::*;
use crate::app::{Action, Msg, Overlay, SessionOp};
use crate::remote::{Bound, Connection, RemoteEvent, RemoteMode, auth::Auth};

mod connection;
mod input;
mod sessions;

/// A running turn `l1` in the foreground waiting on an approval or a choice.
fn pending(h: &mut Harness, approval: bool) {
    let session = h.session_mut();
    session.live = Some(crate::app::LiveTurn::new(
        crate::protocol::TurnRef::new("s1", "l1"),
        0,
    ));
    session.interaction = Some(crate::app::PendingInteraction {
        turn: crate::protocol::TurnRef::new("s1", "l1"),
        interaction_id: "interaction".into(),
        tool_call_id: "call".into(),
        tool_name: "bash".into(),
        prompt: "choose".into(),
        kind: if approval {
            crate::app::InteractionKind::Approval
        } else {
            crate::app::InteractionKind::Choice(vec!["one".into(), "two".into()])
        },
        selected: 0,
    });
    // The phone must hold the interaction in its own projection, as it would
    // after the snapshot that carried it.
    h.app.remote.config = Some(config(h.app.workspace.clone()));
    h.app.sync_remote();
}

async fn text_control(h: &mut Harness, loop_id: &str, interaction_id: Option<&str>) {
    h.fake
        .script_held("turn.wait", waited(loop_id, completed()));
    let pending = interaction_id.map(|id| json!({"loop_id":loop_id, "request_index":0,
        "interaction_id":id, "tool_call_id":"text_call", "tool_name":"ask",
        "kind":{"type":"tool_input", "data":{"prompt":"Your answer?", "answer_kind":"text", "choices":[]}}}));
    let control = h.control(json!({
        "execution":{"type":"running", "loop_id":loop_id, "input":null, "request_index":0, "phase":"waiting_for_input"},
        "pending_interaction":pending,
    }));
    h.fake
        .script("session.state@s1", json!({"snapshot":control.clone()}));
    h.snapshot_event(control).await;
    h.settle().await;
}
