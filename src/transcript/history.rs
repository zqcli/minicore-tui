//! Durable history items as blocks, and the Markdown export of them.

use super::{Block, RequestKey, ToolCall, ToolState, Transcript};
use crate::protocol::ToolRef;
use crate::protocol::read::{
    InterruptedRequest, RawHistoryItem, RuntimeAssistantPart, RuntimeItem, TurnRecord, UserKind,
};
use crate::protocol::stamp::unix_millis;

/// The blocks of a turn's durable items with what only the live turn showed
/// put back: card facts, thinking and text the items lack, and the blocks no
/// item covers, such as the request an interrupt cut short.
pub fn merge_live(mut durable: Vec<Block>, live: &[Block]) -> Vec<Block> {
    for block in &mut durable {
        match block {
            Block::Tool(call) => {
                if let Some(known) = live.iter().find_map(|live| match live {
                    Block::Tool(known) if known.key == call.key => Some(known),
                    _ => None,
                }) {
                    call.keep_live_facts(known);
                }
            }
            Block::Assistant {
                key,
                reasoning,
                text,
                expanded,
                ..
            } => {
                if let Some((known_reasoning, known_text, known_expanded)) =
                    live.iter().find_map(|live| match live {
                        Block::Assistant {
                            key: known,
                            reasoning,
                            text,
                            expanded,
                            ..
                        } if known == key => Some((reasoning, text, expanded)),
                        _ => None,
                    })
                {
                    if reasoning.is_empty() {
                        reasoning.clone_from(known_reasoning);
                    }
                    if text.is_empty() {
                        text.clone_from(known_text);
                    }
                    *expanded = expanded.or(*known_expanded);
                }
            }
            // The turn's own read omits the time, which only the live block has.
            Block::User { text, at, .. } if at.is_none() => {
                *at = live.iter().find_map(|live| match live {
                    Block::User {
                        text: known,
                        at: known_at,
                        ..
                    } if known == text => *known_at,
                    _ => None,
                });
            }
            _ => {}
        }
    }
    let uncovered: Vec<Block> = live
        .iter()
        .filter(|block| match block {
            Block::Tool(call) => !durable
                .iter()
                .any(|kept| matches!(kept, Block::Tool(kept) if kept.key == call.key)),
            Block::Assistant { key, .. } => !durable
                .iter()
                .any(|kept| matches!(kept, Block::Assistant { key: kept, .. } if kept == key)),
            Block::User { text, .. } => !durable
                .iter()
                .any(|kept| matches!(kept, Block::User { text: kept, .. } if kept == text)),
            Block::Compaction { .. } => true,
            _ => false,
        })
        .cloned()
        .collect();
    durable.extend(uncovered);
    // Blank blocks stayed until now so live text could fill them in place.
    durable.retain(|block| !block.is_blank_assistant());
    durable
}

/// Appends history items, closing each turn that has a record with its
/// turn-end line; the duration and changes are not stored, and the tool count
/// is left out for a turn whose start is not among `items`.
pub fn append_history(
    transcript: &mut Transcript,
    items: &[RawHistoryItem],
    records: &[TurnRecord],
    session_id: &str,
) {
    let close = |transcript: &mut Transcript, loop_id: Option<&str>, whole: bool| {
        let Some(record) = loop_id.and_then(|id| records.iter().find(|r| r.loop_id == id)) else {
            return;
        };
        let tools = match whole {
            true => transcript
                .tool_calls()
                .filter(|call| call.key.loop_id == record.loop_id)
                .count() as u64,
            false => 0,
        };
        if let Some(partial) = &record.interrupted {
            push_interrupted(transcript, &record.loop_id, partial);
        }
        transcript.push(Block::TurnEnd {
            loop_id: record.loop_id.clone(),
            outcome: record.outcome.clone(),
            duration: None,
            requests: record.requests,
            tools,
            usage: record.usage.clone(),
            changes: None,
        });
    };
    let mut open: Option<&str> = None;
    // Whether the open turn's prompt is among `items`, so its cards are all here.
    let mut whole = false;
    for item in items {
        match &item.item {
            RuntimeItem::User(user) if user.kind != UserKind::Steering => {
                close(transcript, open.take(), whole);
                whole = true;
                // A turn cut in its first request has no assistant item to open it.
                open = records
                    .iter()
                    .find(|r| r.loop_id == user.loop_id && r.interrupted.is_some())
                    .map(|r| r.loop_id.as_str());
            }
            RuntimeItem::Summary(_) => {
                close(transcript, open.take(), whole);
                whole = false;
            }
            RuntimeItem::Assistant(assistant) => open = Some(&assistant.loop_id),
            _ => {}
        }
        let first = transcript.len();
        append_history_item(transcript, item, session_id);
        // A request that only called tools would add a second separator row.
        if transcript
            .blocks()
            .get(first)
            .is_some_and(Block::is_blank_assistant)
        {
            transcript.remove(first);
        }
    }
    close(transcript, open, whole);
}

/// Appends the partial reply of a cancelled request as an assistant block.
pub fn push_interrupted(transcript: &mut Transcript, loop_id: &str, partial: &InterruptedRequest) {
    transcript.push(Block::Assistant {
        key: RequestKey {
            loop_id: loop_id.to_owned(),
            request_index: partial.request_index,
        },
        reasoning: partial.reasoning.clone(),
        text: partial.text.clone(),
        streaming: false,
        expanded: None,
    });
}

/// Converts one decoded history item into blocks appended to the transcript,
/// blank assistant blocks included.
pub fn append_history_item(transcript: &mut Transcript, item: &RawHistoryItem, session_id: &str) {
    match &item.item {
        RuntimeItem::User(user) => {
            transcript.push(Block::User {
                text: user.input.text.clone(),
                kind: user.kind,
                at: item.timestamp.as_deref().and_then(unix_millis),
            });
        }
        RuntimeItem::Assistant(assistant) => {
            let mut reasoning = String::new();
            let mut text = String::new();
            let mut calls = Vec::new();
            for part in &assistant.content {
                match part {
                    RuntimeAssistantPart::Text(chunk) => text.push_str(chunk),
                    RuntimeAssistantPart::Reasoning {
                        text: chunk,
                        summary,
                    } => {
                        if let Some(chunk) = chunk.as_ref().or(summary.as_ref()) {
                            reasoning.push_str(chunk);
                        }
                    }
                    RuntimeAssistantPart::ToolCall {
                        tool_call_id, name, ..
                    } => {
                        let key = ToolRef {
                            session_id: session_id.to_owned(),
                            loop_id: assistant.loop_id.clone(),
                            request_index: assistant.request_index,
                            tool_call_id: tool_call_id.clone(),
                        };
                        let mut call = ToolCall::new(key, name.clone());
                        call.state = ToolState::Succeeded;
                        if let Some(summary) = item.summary(tool_call_id) {
                            call.apply_summary(summary);
                        }
                        calls.push(call);
                    }
                }
            }
            transcript.push(Block::Assistant {
                key: RequestKey {
                    loop_id: assistant.loop_id.clone(),
                    request_index: assistant.request_index,
                },
                reasoning,
                text,
                streaming: false,
                expanded: None,
            });
            for call in calls {
                transcript.push(Block::Tool(Box::new(call)));
            }
        }
        RuntimeItem::ToolResult(result) => {
            let Some(call) = transcript.blocks.iter_mut().find_map(|block| match block {
                Block::Tool(call)
                    if call.key.loop_id == result.loop_id
                        && call.key.request_index == result.request_index
                        && call.key.tool_call_id == result.call_id =>
                {
                    Some(call.as_mut())
                }
                _ => None,
            }) else {
                return;
            };
            call.state = match result.outcome.as_str() {
                "success" => ToolState::Succeeded,
                "denied" => ToolState::Denied,
                "cancelled" => ToolState::Cancelled,
                _ => ToolState::Failed,
            };
            if let Some(output) = &result.output {
                call.apply_result(&output.content);
            }
            call.settle();
        }
        RuntimeItem::Summary(summary) => {
            transcript.push(Block::Compaction {
                operation_id: String::new(),
                before: None,
                after: None,
                coverage: None,
                summary: Some(summary.content.clone()),
            });
        }
    }
}

/// Renders decoded canonical items as a Markdown conversation.
pub fn to_markdown(items: &[RawHistoryItem]) -> String {
    let mut out = String::with_capacity(4096);
    for item in items {
        match &item.item {
            RuntimeItem::User(user) => {
                let label = match user.kind {
                    UserKind::Steering => "**Steering**",
                    _ => "**You**",
                };
                out.push_str(label);
                out.push_str("\n\n");
                out.push_str(user.input.text.trim_end());
                out.push_str("\n\n");
            }
            RuntimeItem::Assistant(assistant) => {
                for part in &assistant.content {
                    match part {
                        RuntimeAssistantPart::Reasoning { text: thinking, .. } => {
                            if let Some(thinking) = thinking {
                                out.push_str("> ");
                                out.push_str(&thinking.replace('\n', "\n> "));
                                out.push_str("\n\n");
                            }
                        }
                        RuntimeAssistantPart::Text(text) => {
                            out.push_str(text.trim_end());
                            out.push_str("\n\n");
                        }
                        RuntimeAssistantPart::ToolCall { name, .. } => {
                            out.push_str(&format!("`{name}`\n\n"));
                        }
                    }
                }
            }
            RuntimeItem::ToolResult(result) => {
                if let Some(output) = &result.output
                    && !output.content.trim().is_empty()
                {
                    out.push_str("```\n");
                    out.push_str(output.content.trim_end());
                    out.push_str("\n```\n\n");
                }
            }
            RuntimeItem::Summary(summary) => {
                out.push_str("---\n\n*Context summary*\n\n");
                out.push_str(summary.content.trim_end());
                out.push_str("\n\n");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::read::decode_item;
    use serde_json::{Value, json};

    fn key(call: &str) -> ToolRef {
        ToolRef {
            session_id: "s".into(),
            loop_id: "l".into(),
            request_index: 1,
            tool_call_id: call.into(),
        }
    }

    fn user_item(timestamp: Value) -> RawHistoryItem {
        decode_item(
            &json!({"item": {"type": "user", "data": {
                "loop_id": "l", "kind": "prompt", "input": {"text": "hello"}
            }}, "timestamp": timestamp})
            .to_string(),
        )
        .unwrap()
    }

    #[test]
    fn a_user_block_carries_the_item_time_and_old_history_has_none() {
        let mut transcript = Transcript::default();
        append_history_item(
            &mut transcript,
            &user_item(json!("2026-10-20T21:38:00Z")),
            "s",
        );
        append_history_item(&mut transcript, &user_item(json!(null)), "s");
        let times: Vec<Option<u64>> = transcript
            .blocks()
            .iter()
            .map(|block| match block {
                Block::User { at, .. } => *at,
                _ => panic!("expected user"),
            })
            .collect();
        assert_eq!(times, [unix_millis("2026-10-20T21:38:00Z"), None]);
    }

    #[test]
    fn merging_keeps_the_live_prompt_time_the_turn_read_lacks() {
        let mut durable = Transcript::default();
        append_history_item(&mut durable, &user_item(json!(null)), "s");
        let live = [Block::User {
            text: "hello".into(),
            kind: UserKind::Prompt,
            at: Some(42),
        }];
        let merged = merge_live(durable.into_blocks(), &live);
        assert!(matches!(
            merged.as_slice(),
            [Block::User { at: Some(42), .. }]
        ));
    }

    #[test]
    fn history_items_produce_user_assistant_and_tool_blocks() {
        let mut transcript = Transcript::default();
        let user = decode_item(
            &json!({"item": {"type": "user", "data": {
                "loop_id": "l", "kind": "prompt", "input": {"text": "hello"}
            }}})
            .to_string(),
        )
        .unwrap();
        append_history_item(&mut transcript, &user, "s");

        let assistant = decode_item(
            &json!({"item": {"type": "assistant", "data": {
                "loop_id": "l", "request_index": 1, "model": "m",
                "content": [
                    {"type": "text", "data": "running tests"},
                    {"type": "tool_call", "data": {"tool_call_id": "c1", "name": "bash"}}
                ],
                "finish_reason": "tool_calls"
            }}, "tool_summaries": [{
                "tool_ref": {"session_id": "s", "loop_id": "l", "request_index": 1, "tool_call_id": "c1"},
                "tool_call_id": "c1", "name": "bash",
                "display": {"detail": "$ pytest", "truncated": false},
                "output_truncated": false
            }]})
            .to_string(),
        )
        .unwrap();
        append_history_item(&mut transcript, &assistant, "s");

        assert_eq!(transcript.len(), 3);
        let Block::Tool(call) = &transcript.blocks()[2] else {
            panic!("expected a tool card");
        };
        assert_eq!(call.detail, "$ pytest");
    }

    #[test]
    fn tool_result_updates_the_card_it_belongs_to() {
        let mut transcript = Transcript::default();
        let mut call = ToolCall::new(key("c1"), "bash");
        call.state = ToolState::Running;
        transcript.push(Block::Tool(Box::new(call)));
        let result = decode_item(
            &json!({"item": {"type": "tool_result", "data": {
                "loop_id": "l", "request_index": 1, "call_id": "c1",
                "tool_name": "bash", "outcome": "success",
                "output": {"content": "ok"}
            }}})
            .to_string(),
        )
        .unwrap();
        append_history_item(&mut transcript, &result, "s");
        let Block::Tool(call) = &transcript.blocks()[0] else {
            panic!("expected a tool card");
        };
        assert_eq!(call.state, ToolState::Succeeded);
        assert_eq!(call.output, vec!["ok".to_owned()]);
    }

    fn request_item(request_index: u64, content: Value) -> RawHistoryItem {
        decode_item(
            &json!({"item": {"type": "assistant", "data": {
                "loop_id": "l", "request_index": request_index, "model": "m",
                "content": content
            }}})
            .to_string(),
        )
        .unwrap()
    }

    fn call(id: &str) -> Value {
        json!({"type": "tool_call", "data": {"tool_call_id": id, "name": "bash"}})
    }

    #[test]
    fn merging_fills_a_blank_durable_block_in_place_and_drops_the_ones_left_blank() {
        let mut durable = Transcript::default();
        for (request, call_id) in [(0, "c0"), (1, "c1"), (2, "c2")] {
            let reasoning = json!({"type": "reasoning", "data": {"text": null, "summary": null}});
            let item = request_item(request, json!([reasoning, call(call_id)]));
            append_history_item(&mut durable, &item, "s");
        }
        let live = [Block::Assistant {
            key: RequestKey {
                loop_id: "l".into(),
                request_index: 1,
            },
            reasoning: String::new(),
            text: "live words".into(),
            streaming: false,
            expanded: None,
        }];
        let merged = merge_live(durable.into_blocks(), &live);
        let shape: Vec<String> = merged
            .iter()
            .map(|block| match block {
                Block::Assistant { text, .. } => format!("assistant {text}"),
                Block::Tool(call) => format!("tool {}", call.key.tool_call_id),
                _ => "other".into(),
            })
            .collect();
        assert_eq!(
            shape,
            ["tool c0", "assistant live words", "tool c1", "tool c2"]
        );
    }

    #[test]
    fn reloading_history_keeps_no_assistant_block_without_thinking_or_text() {
        let items = [
            request_item(0, json!([{"type": "text", "data": "start"}, call("c0")])),
            request_item(1, json!([call("c1")])),
            request_item(2, json!([{"type": "text", "data": " \n"}, call("c2")])),
        ];
        let mut transcript = Transcript::default();
        append_history(&mut transcript, &items, &[], "s");
        let kinds: Vec<&str> = transcript
            .blocks()
            .iter()
            .map(|block| match block {
                Block::Assistant { .. } => "assistant",
                Block::Tool(_) => "tool",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, ["assistant", "tool", "tool", "tool"]);
    }

    #[test]
    fn an_assistant_item_with_a_text_part_becomes_an_assistant_block() {
        let raw = r#"{"item":{"type":"assistant","data":{"loop_id":"l0",
            "request_index":0,"model":"mock","content":[
            {"type":"text","data":"earlier answer"}]}}}"#;
        let item = decode_item(raw).unwrap();
        let mut transcript = Transcript::default();
        append_history_item(&mut transcript, &item, "s1");
        assert!(
            matches!(transcript.blocks(), [Block::Assistant { text, .. }] if text == "earlier answer"),
            "{:?}",
            transcript.blocks()
        );
    }
}
