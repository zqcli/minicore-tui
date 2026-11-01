//! Literal protocol frames shared by tests.

use serde_json::{Value, json};

use super::Reply;

/// A session catalog entry the fake Agent returns.
pub fn session_info(session_id: &str) -> Value {
    json!({
        "session_id": session_id,
        "title": null,
        "profile": "coding",
        "workspace": "/tmp/ws",
        "model": "mock",
        "reasoning": "medium",
        "loaded": true,
        "created_at": "2024-01-01T00:00:00Z",
        "updated_at": "2024-01-01T00:00:00Z",
    })
}

pub fn turn(loop_id: &str) -> Value {
    json!({"session_id": "s1", "loop_id": loop_id})
}

/// A control snapshot with nothing running and an empty queue.
pub fn snapshot(session_id: &str, load: &str, revision: u64) -> Value {
    json!({
        "session_id": session_id,
        "load": load,
        "revision": revision,
        "execution": {"type": "none"},
        "queue": {"items": [], "paused": false},
        "pending_interaction": null, "compaction": null, "block_reason": null,
        "completions": {
            "through_seq": 0, "retained_from_seq": 1, "entries": [], "truncated": false
        }
    })
}

/// A control snapshot with one running turn.
pub fn snapshot_running(
    session_id: &str,
    load: &str,
    revision: u64,
    loop_id: &str,
    input: Value,
) -> Value {
    let mut state = snapshot(session_id, load, revision);
    state["execution"] = json!({
        "type": "running", "loop_id": loop_id, "input": input, "request_index": 0,
        "phase": "running_model"
    });
    state
}

/// One history chunk as the Agent frames it: the item JSON lives in `data`, and
/// a small item is a single complete chunk.
pub fn chunk(index: usize, item: Value) -> Value {
    chunk_envelope(index, json!({"item": item}))
}

/// A history chunk whose item JSON is the whole `envelope`, e.g. with a timestamp.
pub fn chunk_envelope(index: usize, envelope: Value) -> Value {
    let data = envelope.to_string();
    json!({
        "index": index,
        "offset": 0,
        "total_bytes": data.len(),
        "encoding": "utf8_json",
        "data": data,
        "complete": true,
    })
}

/// A history projection with `first_item` older items covered by its summary.
pub fn display_read(total: usize, first_item: usize) -> Value {
    json!({
        "session": session_info("s1"),
        "items": [],
        "next_cursor": null,
        "total": total,
        "records": [],
        "history_revision": "h1",
        "captured_end": 1,
        "projection": {
            "revision": "p1",
            "first_item": first_item,
            "covered_item_count": first_item,
            "covered_usage": {"usage": {}, "partial": true},
        },
    })
}

/// A display `session.read` page holding `items`.
pub fn page(total: usize, first_item: usize, items: Vec<Value>) -> Value {
    let mut page = display_read(total, first_item);
    page["items"] = json!(items);
    page
}

pub fn user(index: usize, loop_id: &str, text: &str) -> Value {
    chunk(
        index,
        json!({"type": "user", "data": {"loop_id": loop_id, "kind": "prompt",
                                        "input": {"text": text}}}),
    )
}

pub fn user_at(index: usize, loop_id: &str, text: &str, timestamp: &str) -> Value {
    chunk_envelope(
        index,
        json!({"item": {"type": "user", "data": {"loop_id": loop_id, "kind": "prompt",
                                                 "input": {"text": text}}},
               "timestamp": timestamp}),
    )
}

pub fn assistant(index: usize, loop_id: &str, text: &str) -> Value {
    chunk(
        index,
        json!({"type": "assistant", "data": {"loop_id": loop_id, "request_index": 0,
            "model": "mock", "content": [{"type": "text", "data": text}]}}),
    )
}

/// An assistant item that only calls the tool `call_id`.
pub fn assistant_tool(index: usize, loop_id: &str, call_id: &str, name: &str) -> Value {
    chunk(
        index,
        json!({"type": "assistant", "data": {"loop_id": loop_id, "request_index": 0,
            "model": "mock", "content": [
                {"type": "tool_call", "data": {"tool_call_id": call_id, "name": name}}]}}),
    )
}

/// The compaction summary the display projection puts at the floor.
pub fn summary(index: usize) -> Value {
    chunk(
        index,
        json!({"type": "summary", "data": {"content": "what came before"}}),
    )
}

/// A `turn.wait` answer for `loop_id` with nothing queued and no successor.
pub fn waited(loop_id: &str, outcome: Value) -> Value {
    waited_then(loop_id, outcome, None, queue(vec![], false))
}

/// A `turn.wait` answer for `loop_id` that also says which turn the Agent started next and what still waits.
pub fn waited_then(loop_id: &str, outcome: Value, next: Option<Value>, queued: Value) -> Value {
    json!({"turn": turn(loop_id), "outcome": outcome, "next": next, "queue": queued})
}

pub fn completed() -> Value {
    json!({"type": "completed"})
}

pub fn cancelled() -> Value {
    json!({"type": "cancelled", "reason": "user"})
}

/// The immutable identity a test result body carries.
pub const RESULT_ID: &str = "r1:0000000000000000000000000000000000000000000000000000000000000001";

/// A complete result page; callers specify pagination and may patch its facts.
pub fn result_page(turn: Value, items: Vec<Value>, total: usize, next: Value) -> Value {
    json!({"turn":turn,"load":super::LOAD,"result_id":RESULT_ID,
        "outcome":completed(),"availability":"stored","persistence":"persisted",
        "total":total,"items":items,"next_cursor":next})
}

/// A cancelled turn's single result page with sequential item indices.
pub fn turn_result(loop_id: &str, mut items: Vec<Value>) -> Value {
    for (index, item) in items.iter_mut().enumerate() {
        item["index"] = json!(index);
    }
    let total = items.len();
    let mut result = result_page(turn(loop_id), items, total, Value::Null);
    result["outcome"] = cancelled();
    result
}

/// One queue entry as the Agent reports it.
pub fn entry(id: &str, mode: &str, text: &str) -> Value {
    json!({"id": id, "mode": mode, "text": text})
}

/// A queue as the Agent reports it.
pub fn queue(items: Vec<Value>, paused: bool) -> Value {
    json!({"items": items, "paused": paused})
}

/// An `input.push` answer for an entry that waits in `queued`.
pub fn pushed(queued: Vec<Value>) -> Value {
    json!({"session_id":"s1","load":crate::testing::LOAD,"accepted":true,"id": "i1", "started": null, "queue": queue(queued, false)})
}

/// A turn the Agent started from the queue with `text` as its input.
pub fn started_turn(turn: Value, text: &str) -> Value {
    json!({"turn": turn, "input": entry("n1", "follow_up", text)})
}

/// `started_turn` for `loop_id` of `s1`.
pub fn next_turn(loop_id: &str, text: &str) -> Value {
    started_turn(turn(loop_id), text)
}

/// An `input.push` answer for an entry that started `loop_id` with nothing else waiting.
pub fn started(loop_id: &str) -> Value {
    started_in(turn(loop_id), vec![])
}

/// `started` with the time the Agent accepted the entry.
pub fn started_at(loop_id: &str, accepted_at: &str) -> Value {
    let mut push = started(loop_id);
    push["started"]["input"]["accepted_at"] = json!(accepted_at);
    push
}

/// An `input.push` answer that started `turn` with the pushed text and left `queued` waiting.
pub fn started_in(turn: Value, queued: Vec<Value>) -> Value {
    let started = json!({"turn": turn, "input": entry("i1", "steer", "")});
    json!({"session_id":started["turn"]["session_id"],"load":crate::testing::LOAD,"accepted":true,
           "id": "i1", "started": started, "queue": queue(queued, false)})
}

/// A refusal in the shape `FakeAgent::script_sequence` accepts.
pub fn refusal(code: i64, kind: &str) -> Value {
    json!({"error": {"code": code, "message": kind, "data": {"kind": kind,"acceptance":(kind == "input_queue_full").then_some("rejected")}}})
}

/// An explicitly paged fixture; plain FakeAgent scripts remain literal.
pub fn history(page: Value) -> Reply {
    Reply::from(page).history()
}

pub fn state_response(snapshot: Value) -> Value {
    json!({"session_id":snapshot["session_id"], "snapshot":snapshot})
}

pub fn opened_session(session: Value) -> Value {
    json!({"load":crate::testing::LOAD,
           "snapshot":snapshot(session["session_id"].as_str().unwrap(),crate::testing::LOAD,1),
           "session":session})
}

/// Publishes exactly this snapshot before the receipt and serves it to later queries.
pub fn after_snapshot(receipt: Value, snapshot: Value) -> Reply {
    let event = json!({"type":"session_snapshot","data":{
        "meta":{"session_id":snapshot["session_id"],"load":snapshot["load"],"dropped_before":0},
        "snapshot":snapshot
    }});
    Reply::result(receipt)
        .setting(
            format!("session.state@{}", snapshot["session_id"].as_str().unwrap()),
            state_response(snapshot),
        )
        .events(vec![event])
}

pub fn running_snapshot(revision: u64, loop_id: &str, text: &str) -> Value {
    crate::testing::snapshot_running(
        "s1",
        crate::testing::LOAD,
        revision,
        loop_id,
        entry("i1", "steer", text),
    )
}

pub fn completion(seq: u64, loop_id: &str, outcome: &str) -> Value {
    json!({"seq":seq,"loop_id":loop_id,"completion_kind":"reported","outcome_kind":outcome,
           "persistence":"persisted","availability":"stored"})
}

pub fn finished_snapshot(revision: u64, entries: Vec<Value>) -> Value {
    let mut state = snapshot("s1", crate::testing::LOAD, revision);
    state["completions"] = json!({"through_seq":entries.last().unwrap()["seq"],
        "retained_from_seq":entries[0]["seq"],"entries":entries,"truncated":false});
    state
}

/// An explicit running snapshot published with an accepted push.
pub fn starting_reply(loop_id: &str, revision: u64) -> Reply {
    let mut receipt = started(loop_id);
    receipt["started"]["input"]["text"] = json!("$text");
    after_snapshot(receipt, running_snapshot(revision, loop_id, "$text")).with_request_text()
}

/// A running turn and the explicitly ordered queue observed beside it.
pub fn running_queue(revision: u64, loop_id: &str, prompt: &str, items: Vec<Value>) -> Value {
    let mut state = running_snapshot(revision, loop_id, prompt);
    state["queue"] = queue(items, false);
    state
}

pub fn completed_result(loop_id: &str, items: Vec<Value>) -> Value {
    let mut result = turn_result(loop_id, items);
    result["outcome"] = completed();
    result
}

/// One completed turn with a fully specified canonical body.
pub fn completed_reply(loop_id: &str, revision: u64, items: Vec<Value>) -> Reply {
    after_snapshot(
        waited(loop_id, completed()),
        finished_snapshot(revision, vec![completion(1, loop_id, "completed")]),
    )
    .setting(
        format!("turn.result@s1/{loop_id}"),
        completed_result(loop_id, items),
    )
}
