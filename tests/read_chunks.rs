//! `ChunkAssembler` decoding of the real Protocol v1 read fixtures.
//!
//! These tests feed the byte-exact `session.read` pages (captured from the
//! pinned Agent 0.5.0 process) through the production decoder, so the wire
//! shape and the decoder can never drift apart silently (spec §6.2, §24.2).

use std::path::PathBuf;

use minicore_tui::protocol::read::{
    Assembled, ChunkAssembler, ReadError, ReadSessionResult, RuntimeAssistantPart, RuntimeItem,
    RuntimeUserKind, TurnAvailability, TurnResultPage,
};
use serde_json::Value;

fn fixture(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/agent-v1")
        .join(format!("{name}.json"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("missing fixture {name}: {error}"));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("invalid fixture {name}: {error}"))
}

fn read_chunks(chunk: Value) -> minicore_tui::protocol::read::ReadChunk {
    serde_json::from_value(chunk).expect("fixture chunk is a ReadChunk")
}

/// A whole multi-page long assistant item must reconstruct byte-exactly and
/// decode into a real Runtime item, across UTF-8 boundaries.
#[test]
fn paged_chunks_reconstruct_real_runtime_items_exactly() {
    let pages = fixture("session-read-paged-chunks");
    let pages = pages["pages"].as_array().expect("pages array");
    let mut assembler = ChunkAssembler::new();
    let mut items = Vec::new();
    for page in pages {
        for chunk in page["items"].as_array().unwrap() {
            match assembler.push(read_chunks(chunk.clone())).unwrap() {
                Assembled::Pending => {}
                Assembled::Item { index, item } => items.push((index, item)),
                Assembled::LargeItem { .. } => panic!("long fixture must fit the auto budget"),
            }
        }
    }
    assert!(assembler.current_index().is_none(), "no partial item left");
    assert_eq!(items.len(), 2, "two items: one user, one long assistant");
    assert_eq!(items[0].0, 0);
    assert_eq!(items[1].0, 1);

    match &items[0].1.item {
        RuntimeItem::User(user) => {
            assert_eq!(user.kind, RuntimeUserKind::Prompt);
            assert_eq!(user.input.text, "long answer");
        }
        other => panic!("expected a user item, got {other:?}"),
    }
    match &items[1].1.item {
        RuntimeItem::Assistant(assistant) => {
            assert_eq!(assistant.request_index, 0);
            assert_eq!(assistant.model, "deep");
            // Real runtime content parts, including non-ASCII text.
            let text = match assistant.content.first() {
                Some(RuntimeAssistantPart::Text(text)) => text,
                other => panic!("expected a text part, got {other:?}"),
            };
            assert!(text.contains("alpha βeta 🙂 gamma"));
        }
        other => panic!("expected an assistant item, got {other:?}"),
    }
}

/// A decoded item must equal the backend's own line for that item: the
/// reassembly is the canonical JSON, not a re-encoding with different bytes.
#[test]
fn assembled_item_round_trips_the_canonical_bytes() {
    let page: ReadSessionResult =
        serde_json::from_value(fixture("session-read-first-page")["result"].clone()).unwrap();
    let mut assembler = ChunkAssembler::new();
    let mut decoded = Vec::new();
    for chunk in page.items.iter().cloned() {
        if let Assembled::Item { item, .. } = assembler.push(chunk).unwrap() {
            decoded.push(item);
        }
    }
    assert_eq!(decoded.len(), 2);
    // The user envelope carries its real acceptance timestamp.
    assert!(decoded[0].timestamp.as_deref().is_some_and(|t| t.starts_with("20")));
    assert!(decoded[1].timestamp.is_none());
}

/// A mid-item continuation cursor (offset > 0) must be accepted only with the
/// exact preceding offset; a mismatched or out-of-order chunk is a definite
/// protocol error, never spliced.
#[test]
fn continuation_offset_is_strict() {
    let pages = fixture("session-read-paged-chunks");
    let pages = pages["pages"].as_array().expect("pages array");
    let long_chunk = pages
        .iter()
        .flat_map(|page| page["items"].as_array().unwrap().iter())
        .find(|chunk| chunk["total_bytes"].as_u64().unwrap() > 500)
        .map(|chunk| read_chunks(chunk.clone()))
        .expect("a multi-chunk item");

    // A non-zero first offset with no preceding bytes is rejected.
    let mut fresh = ChunkAssembler::new();
    let mut orphan = long_chunk.clone();
    orphan.offset = 10;
    assert!(matches!(
        fresh.push(orphan),
        Err(ReadError::OffsetMismatch { expected: 0, .. })
    ));

    // A continuation that does not land on the delivered byte count is
    // rejected rather than appended.
    let split = long_chunk.clone();
    let half = split.data.len() / 2;
    let head = minicore_tui::protocol::read::ReadChunk {
        offset: long_chunk.offset,
        total_bytes: long_chunk.total_bytes,
        data: split.data[..half].to_owned(),
        complete: false,
        ..split.clone()
    };
    assert_eq!(fresh.push(head).unwrap(), Assembled::Pending);
    let mut jump = split.clone();
    jump.offset = long_chunk.offset + half + 1;
    assert!(matches!(fresh.push(jump), Err(ReadError::OffsetMismatch { .. })));
}

/// The long assistant item in the trailing-incomplete fixture is delivered
/// whole; a JSONL tail that never completed does not become a fabricated item.
#[test]
fn trailing_incomplete_tail_is_not_fabricated() {
    let page: ReadSessionResult =
        serde_json::from_value(fixture("session-read-trailing-incomplete")["result"].clone())
            .unwrap();
    assert!(page.trailing_incomplete);
    let mut assembler = ChunkAssembler::new();
    let mut items = 0;
    for chunk in page.items.iter().cloned() {
        if let Assembled::Item { .. } = assembler.push(chunk).unwrap() {
            items += 1;
        }
    }
    assert_eq!(items, 2, "only the two complete items are decoded");
    assert_eq!(page.total, 2);
    // The captured_end is the backend's JSONL prefix boundary, not a count.
    assert!(page.captured_end > page.total as u64);
}

/// `records_truncated` is surfaced, not silently dropped.
#[test]
fn records_truncated_is_reported() {
    let page: ReadSessionResult =
        serde_json::from_value(fixture("session-read-records-truncated")["result"].clone()).unwrap();
    assert!(page.records_truncated);
    assert!(!page.records.is_empty());
}

/// A `total_bytes` above the auto-decode budget becomes a visible placeholder,
/// never a silently truncated item.
#[test]
fn oversized_item_becomes_a_placeholder() {
    let total = minicore_tui::protocol::MAX_AUTO_ITEM_BYTES + 1;
    let head = "{\"item\":{\"type\":\"user\",\"data\":{\"loop_id\":\"x\",\"kind\":\"prompt\",\"input\":{\"text\":\"";
    let mut assembler = ChunkAssembler::new();
    let first = minicore_tui::protocol::read::ReadChunk {
        index: 0,
        offset: 0,
        total_bytes: total,
        encoding: "utf8_json".into(),
        data: head.into(),
        complete: false,
    };
    assert_eq!(assembler.push(first).unwrap(), Assembled::Pending);
    let last = minicore_tui::protocol::read::ReadChunk {
        index: 0,
        offset: head.len(),
        total_bytes: total,
        encoding: "utf8_json".into(),
        data: "x".into(),
        complete: true,
    };
    match assembler.push(last).unwrap() {
        Assembled::LargeItem { index, total_bytes } => {
            assert_eq!(index, 0);
            assert_eq!(total_bytes, total);
        }
        other => panic!("expected a LargeItem placeholder, got {other:?}"),
    }
    assert!(assembler.current_index().is_none(), "placeholder released");
}

/// A chunk whose declared total does not match the assembled byte count is a
/// protocol error even when it is flagged complete.
#[test]
fn complete_chunk_with_wrong_byte_count_is_rejected() {
    let mut assembler = ChunkAssembler::new();
    let chunk = minicore_tui::protocol::read::ReadChunk {
        index: 0,
        offset: 0,
        total_bytes: 999,
        encoding: "utf8_json".into(),
        data: "{\"item\":{\"type\":\"summary\",\"data\":{\"content\":\"hi\"}}}".into(),
        complete: true,
    };
    assert!(matches!(
        assembler.push(chunk),
        Err(ReadError::ByteCountMismatch { .. })
    ));
}

/// A non-`utf8_json` encoding is refused; the decoder never guesses.
#[test]
fn unknown_encoding_is_refused() {
    let mut assembler = ChunkAssembler::new();
    let chunk = minicore_tui::protocol::read::ReadChunk {
        index: 0,
        offset: 0,
        total_bytes: 1,
        encoding: "base64".into(),
        data: "e30=".into(),
        complete: true,
    };
    assert!(matches!(assembler.push(chunk), Err(ReadError::Encoding(_))));
}

/// `turn.result` availability covers pending/live/stored, and its item indices
/// are turn-local. The stored page must decode to a real Runtime item.
#[test]
fn turn_result_pages_decode_all_availabilities() {
    let stored: TurnResultPage =
        serde_json::from_value(fixture("turn-result-stored")["result"].clone()).unwrap();
    assert_eq!(stored.availability, TurnAvailability::Stored);
    assert_eq!(
        stored.persistence,
        Some(minicore_tui::protocol::TurnPersistenceWire::Persisted)
    );
    let mut assembler = ChunkAssembler::new();
    let mut items = Vec::new();
    for chunk in stored.items.iter().cloned() {
        if let Assembled::Item { index, item } = assembler.push(chunk).unwrap() {
            items.push((index, item));
        }
    }
    assert_eq!(items.len(), 2);
    // Turn-local indices start at zero and are contiguous for this turn.
    assert_eq!(items[0].0, 0);
    assert_eq!(items[1].0, 1);
    assert!(matches!(items[1].1.item, RuntimeItem::Assistant(_)));

    let pending: TurnResultPage =
        serde_json::from_value(fixture("turn-result-pending")["result"].clone()).unwrap();
    assert_eq!(pending.availability, TurnAvailability::Pending);
    assert!(pending.outcome.is_none());

    let live_failed: TurnResultPage =
        serde_json::from_value(fixture("turn-result-live-failed")["result"].clone()).unwrap();
    assert_eq!(live_failed.availability, TurnAvailability::Live);
    assert_eq!(
        live_failed.persistence,
        Some(minicore_tui::protocol::TurnPersistenceWire::Failed)
    );
    assert!(live_failed.completed_at.is_none());
}
