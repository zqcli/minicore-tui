//! Durable history items as blocks, and the Markdown export of them.

use super::{Block, RequestKey, ToolCall, ToolState, Transcript};
use crate::protocol::ToolRef;
use crate::protocol::read::{
    RawHistoryItem, RuntimeAssistantPart, RuntimeItem, TurnRecord, UserKind,
};

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
            _ => {}
        }
    }
    let uncovered: Vec<Block> =
        live.iter()
            .filter(|block| match block {
                Block::Tool(call) => !durable
                    .iter()
                    .any(|kept| matches!(kept, Block::Tool(kept) if kept.key == call.key)),
                Block::Assistant {
                    key,
                    reasoning,
                    text,
                    ..
                } => !(reasoning.is_empty() && text.is_empty())
                    && !durable.iter().any(
                        |kept| matches!(kept, Block::Assistant { key: kept, .. } if kept == key),
                    ),
                Block::User { text, .. } => !durable
                    .iter()
                    .any(|kept| matches!(kept, Block::User { text: kept, .. } if kept == text)),
                Block::Compaction { .. } => true,
                _ => false,
            })
            .cloned()
            .collect();
    durable.extend(uncovered);
    durable
}

/// Appends history items, closing each turn that has a record with its
/// turn-end line; the duration and changes are not stored.
pub fn append_history(
    transcript: &mut Transcript,
    items: &[RawHistoryItem],
    records: &[TurnRecord],
    session_id: &str,
) {
    let close = |transcript: &mut Transcript, loop_id: Option<&str>| {
        let Some(record) = loop_id.and_then(|id| records.iter().find(|r| r.loop_id == id)) else {
            return;
        };
        let tools = transcript
            .tool_calls()
            .filter(|call| call.key.loop_id == record.loop_id)
            .count() as u64;
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
    for item in items {
        match &item.item {
            RuntimeItem::User(user) if user.kind != UserKind::Steering => {
                close(transcript, open.take());
            }
            RuntimeItem::Summary(_) => close(transcript, open.take()),
            RuntimeItem::Assistant(assistant) => open = Some(&assistant.loop_id),
            _ => {}
        }
        append_history_item(transcript, item, session_id);
    }
    close(transcript, open);
}

/// Converts one decoded history item into blocks appended to the transcript.
pub fn append_history_item(transcript: &mut Transcript, item: &RawHistoryItem, session_id: &str) {
    match &item.item {
        RuntimeItem::User(user) => {
            transcript.push(Block::User {
                text: user.input.text.clone(),
                kind: user.kind,
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
    use serde_json::json;

    fn key(call: &str) -> ToolRef {
        ToolRef {
            session_id: "s".into(),
            loop_id: "l".into(),
            request_index: 1,
            tool_call_id: call.into(),
        }
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
