//! Decoding of the pinned Agent 0.5.0 / Protocol v1 fixtures in
//! `tests/fixtures/agent-v1/`.
//!
//! These fixtures were captured by `scripts/generate_agent_v1_fixtures.py`
//! from a real `minicore-agent` process (see `manifest.json`). Stage A pins
//! the recorded fields here so the stage B protocol migration starts from the
//! real wire shapes instead of a guessed DTO. The `session.read` chunks are
//! real Runtime `HistoryItem` envelopes, not the legacy `HistoryItemView`
//! display DTO; the assertions below check the raw JSON directly so a
//! mistaken reuse of the display DTO cannot silently pass.

use std::path::PathBuf;

use serde_json::Value;

fn fixture(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/agent-v1")
        .join(format!("{name}.json"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("missing fixture {name}: {error}"));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("invalid fixture {name}: {error}"))
}

fn manifest() -> Value {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/agent-v1/manifest.json");
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

#[test]
fn manifest_pins_the_fixed_backend_and_protocol() {
    let manifest = manifest();
    assert_eq!(
        manifest["agent"]["head"],
        "061743369459299e66be97bf97d2b27352a39914"
    );
    assert_eq!(
        manifest["runtime"]["head"],
        "6cd2bdbc634437dea925495c61c7eb0be10ba171"
    );
    assert_eq!(manifest["protocol_version"], 1);
    assert_eq!(manifest["agent"]["package_version"], "0.5.0");

    let capabilities: Vec<&str> = manifest["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect();
    for required in [
        "session.read",
        "turn.result",
        "session.context",
        "tool.read",
        "tool.output",
        "workspace.read",
        "workspace.files",
        "workspace.search",
        "workspace.status",
        "changes.list",
        "changes.diff",
        "deferred.waiter_limit",
    ] {
        assert!(
            capabilities.contains(&required),
            "missing capability {required}"
        );
    }
}

#[test]
fn ping_reports_protocol_version_and_capabilities() {
    let result = &fixture("ping")["result"];
    assert_eq!(result["version"], "0.5.0");
    assert_eq!(result["protocol_version"], 1);
    assert!(result["capabilities"].as_array().unwrap().len() >= 13);
}

#[test]
fn session_read_chunks_are_runtime_item_envelopes() {
    let page = &fixture("session-read-first-page")["result"];
    let items = page["items"].as_array().unwrap();
    assert!(!items.is_empty());
    assert!(page["history_revision"].as_str().unwrap().len() == 64);
    assert!(page["captured_end"].is_u64());
    assert!(page["trailing_incomplete"].is_boolean());

    for chunk in items {
        assert_eq!(chunk["encoding"], "utf8_json");
        let data = chunk["data"].as_str().unwrap();
        let envelope: Value = serde_json::from_str(data).unwrap();
        let item = &envelope["item"];
        // Runtime HistoryItem: tagged enum with a `type` and a `data` object.
        // The legacy display DTO has `text`/`reasoning`/`tool_calls`, not
        // `input`/`content`, so these fields prove the envelope is not a
        // repackaged `HistoryItemView`.
        match item["type"].as_str().unwrap() {
            "user" => {
                assert!(item["data"]["input"].is_object() || item["data"]["input"].is_string());
                assert!(item["data"].get("text").is_none());
            }
            "assistant" => {
                assert!(item["data"]["content"].is_array());
                assert!(item["data"].get("text").is_none());
            }
            "tool_result" => {
                assert!(item["data"]["call_id"].is_string());
                assert!(item["data"]["tool_name"].is_string());
            }
            "summary" => assert!(item["data"]["content"].is_string()),
            other => panic!("unexpected history item type {other}"),
        }
        assert_eq!(chunk["offset"], 0);
        assert_eq!(chunk["total_bytes"], data.len());
        assert_eq!(chunk["complete"], true);
    }
}

#[test]
fn read_pages_reconstruct_exactly_with_continuation_cursors() {
    let paged = &fixture("session-read-paged-chunks")["pages"];
    let pages = paged.as_array().unwrap();
    assert!(pages.len() >= 2, "a 4 KiB budget must force multiple pages");

    let mut fragments: std::collections::BTreeMap<usize, String> =
        std::collections::BTreeMap::new();
    let mut expected_totals: std::collections::BTreeMap<usize, usize> =
        std::collections::BTreeMap::new();
    for page in pages {
        for chunk in page["items"].as_array().unwrap() {
            let index = chunk["index"].as_u64().unwrap() as usize;
            expected_totals.insert(index, chunk["total_bytes"].as_u64().unwrap() as usize);
            let prior = fragments.entry(index).or_default();
            assert_eq!(chunk["offset"].as_u64().unwrap() as usize, prior.len());
            prior.push_str(chunk["data"].as_str().unwrap());
        }
    }
    for (index, json) in &fragments {
        assert_eq!(
            json.len(),
            expected_totals[index],
            "item {index} byte count"
        );
        let envelope: Value = serde_json::from_str(json).unwrap();
        assert!(envelope["item"]["type"].is_string());
    }
    let continuation = &fixture("session-read-continuation-cursor")["cursor_offsets"];
    assert!(
        continuation
            .as_array()
            .unwrap()
            .iter()
            .any(|offset| offset["offset"].as_u64().unwrap() > 0),
        "at least one chunk must continue inside an item"
    );
}

#[test]
fn trailing_incomplete_history_is_reported_not_repaired() {
    let page = &fixture("session-read-trailing-incomplete")["result"];
    assert_eq!(page["trailing_incomplete"], true);
    assert!(page["total"].as_u64().unwrap() >= 1);
}

#[test]
fn turn_result_availability_covers_pending_and_stored() {
    let pending = &fixture("turn-result-pending")["result"];
    assert_eq!(pending["availability"], "pending");
    assert!(pending["items"].as_array().unwrap().is_empty());

    let stored = &fixture("turn-result-stored")["result"];
    assert_eq!(stored["availability"], "stored");
    assert_eq!(stored["persistence"], "persisted");
    assert!(!stored["items"].as_array().unwrap().is_empty());
}

#[test]
fn turn_wait_is_a_direct_result_view() {
    let wait = &fixture("turn-wait")["result"];
    assert!(wait["turn"]["session_id"].is_string());
    assert_eq!(wait["persistence"], "persisted");
    assert_eq!(wait["outcome"]["type"], "completed");
}

#[test]
fn tool_read_distinguishes_awaiting_policy_running_and_terminal() {
    let awaiting = &fixture("tool-read-awaiting-policy")["result"]["execution"];
    assert_eq!(awaiting["state"], "awaiting_policy");
    assert!(awaiting.get("started_at").is_none() || awaiting["started_at"].is_null());

    let running = &fixture("tool-read-running")["result"]["execution"];
    assert_eq!(running["state"], "running");

    let terminal = &fixture("tool-read-terminal")["result"]["execution"];
    assert_eq!(terminal["state"], "succeeded");
    assert!(terminal["command"].is_object());
    assert_eq!(terminal["command"]["termination_confirmed"], true);
}

#[test]
fn tool_output_streams_use_raw_byte_offsets() {
    let stdout = &fixture("tool-output-stdout")["result"];
    assert_eq!(stdout["encoding"], "base64");
    assert_eq!(stdout["stream"], "stdout");
    // base64 data length is not the offset: `next_offset` counts decoded bytes.
    let decoded = {
        use std::io::Read;
        // No base64 crate dependency in stage A; manual decode via a tiny table.
        const TABLE: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let text = stdout["data"].as_str().unwrap().trim_end_matches('=');
        let mut out = Vec::new();
        let mut buffer = 0u32;
        let mut bits = 0u32;
        for byte in text.bytes() {
            let value = TABLE.iter().position(|b| *b == byte).unwrap() as u32;
            buffer = (buffer << 6) | value;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((buffer >> bits) as u8);
            }
        }
        let _ = &mut std::io::empty().read(&mut []);
        out
    };
    assert_eq!(
        decoded.len() as u64,
        stdout["next_offset"].as_u64().unwrap()
    );
    assert_eq!(stdout["base_offset"], 0);
    assert_eq!(stdout["eof"], true);

    let output = &fixture("tool-output-output")["result"];
    assert_eq!(output["encoding"], "utf8");
    assert_eq!(
        output["next_offset"].as_u64().unwrap() as usize,
        output["data"].as_str().unwrap().len()
    );
}

#[test]
fn workspace_read_statuses_are_distinct() {
    assert_eq!(fixture("workspace-read-ok")["result"]["status"], "ok");
    assert_eq!(
        fixture("workspace-read-binary")["result"]["status"],
        "binary"
    );
    assert_eq!(
        fixture("workspace-read-too-large")["result"]["status"],
        "too_large"
    );
    assert_eq!(
        fixture("workspace-read-changed")["result"]["status"],
        "changed"
    );
    assert!(
        fixture("workspace-read-ok")["result"]["revision"]
            .as_str()
            .unwrap()
            .len()
            == 64
    );
}

#[test]
fn workspace_files_and_search_report_partial_pages() {
    let paged = &fixture("workspace-files-paged")["result"];
    assert_eq!(paged["stopped_by"], "page");
    assert!(paged["next_cursor"].is_object());
    assert_eq!(paged["scan_complete"], false);

    let search = &fixture("workspace-search")["result"];
    assert!(!search["matches"].as_array().unwrap().is_empty());
    let first = &search["matches"][0];
    assert!(first["line_number"].as_u64().unwrap() >= 1);
    assert!(first["match_byte_ranges"].is_array());
}

#[test]
fn workspace_status_covers_repo_detached_and_unavailable() {
    let repo = &fixture("workspace-status")["result"];
    assert_eq!(repo["repo_available"], true);
    assert!(repo["head_oid"].is_string());

    let detached = &fixture("workspace-status-detached")["result"];
    assert_eq!(detached["detached"], true);
    assert!(detached["branch"].is_null());

    let unavailable = &fixture("workspace-status-unavailable")["result"];
    assert_eq!(unavailable["repo_available"], false);
    assert_eq!(unavailable["complete"], false);
    assert!(
        unavailable["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning == "status_failed")
    );
}

#[test]
fn changes_list_and_diff_keep_opaque_refs_and_structured_hunks() {
    let list = &fixture("changes-list-workspace")["result"];
    assert_eq!(list["scope"], "workspace");
    let record = &list["records"][0];
    assert_eq!(record["origin"], "workspace_unknown");
    // The token is opaque base64url; the fixture must not have parsed it.
    assert!(
        record["change_ref"]
            .as_str()
            .unwrap()
            .starts_with("workspace:")
    );

    let diff = &fixture("changes-diff-workspace")["result"];
    assert_eq!(diff["origin"], "workspace_unknown");
    assert!(diff["hunks"].is_array());
    assert!(diff["versions_refreshed"].is_boolean());
    for hunk in diff["hunks"].as_array().unwrap() {
        for line in hunk["lines"].as_array().unwrap() {
            assert!(line["kind"].is_string());
            assert!(line["text"].is_string());
            assert!(line["line_complete"].is_boolean());
            assert!(line["line_byte_offset"].is_number());
        }
    }
}

#[test]
fn session_context_separates_estimates_from_unknowns() {
    let context = &fixture("session-context-idle")["result"];
    assert!(context["coverage"].is_object());
    assert!(context["budget"].is_object());
    // No reliable source means null, never a fabricated zero.
    assert!(context["budget"]["estimated_request_context_tokens"].is_null());
    assert!(context["automatic"]["current"].is_null());
}
