//! Durable history items as blocks, and the Markdown export of them.

use super::{Block, HistorySource, RequestKey, ToolCall, ToolState, Transcript};
use crate::protocol::ToolRef;
use crate::protocol::read::{
    InterruptedRequest, RawHistoryItem, RuntimeAssistantPart, RuntimeItem, TurnRecord, UserKind,
};
use crate::protocol::stamp::unix_millis;

// Records and the display summary share an item position, but are not that raw item.
const INTERRUPTED_PART: usize = usize::MAX - 2;
const TERMINAL_PART: usize = usize::MAX - 1;
const SUMMARY_PART: usize = usize::MAX;

impl Transcript {
    /// Maps a verified history window, keeping its source items separate from turn-result indices.
    pub fn from_history(
        items: &[RawHistoryItem],
        records: &[TurnRecord],
        session_id: &str,
        from: usize,
    ) -> Self {
        let mut transcript = Self::default();
        append_history_from(&mut transcript, items, records, session_id, Some(from));
        transcript
    }

    /// Keeps previously displayed source rows outside the incoming verified window.
    pub fn with_previous_history(self, previous: &Self, covered: std::ops::Range<usize>) -> Self {
        let mut rows: Vec<_> = previous
            .origins
            .iter()
            .enumerate()
            .filter_map(|(at, origin)| {
                origin
                    .history
                    .filter(|source| {
                        !covered.contains(&source.item) || source.part >= INTERRUPTED_PART
                    })
                    .map(|source| (source, false, at))
            })
            .chain(
                self.origins
                    .iter()
                    .enumerate()
                    .filter_map(|(at, origin)| origin.history.map(|source| (source, true, at))),
            )
            .collect();
        let mut last_items = std::collections::HashMap::new();
        for (source, fresh, at) in &rows {
            let transcript = if *fresh { &self } else { previous };
            let loop_id = transcript.blocks[*at].loop_id().or_else(|| {
                transcript
                    .user_identity(*at)
                    .map(|user| user.loop_id.as_str())
            });
            if let Some(id) = loop_id {
                let last = last_items.entry(id).or_insert(source.item);
                *last = (*last).max(source.item);
            }
        }
        for (source, fresh, at) in &mut rows {
            if let Block::TurnEnd { loop_id, .. } =
                &if *fresh { &self } else { previous }.blocks[*at]
            {
                source.item = last_items[loop_id.as_str()];
            }
        }
        rows.sort_by_key(|(source, _, _)| *source);
        let mut ends = std::collections::HashSet::new();
        let mut replies = std::collections::HashSet::new();
        let mut tools = std::collections::HashSet::new();
        let mut summaries = std::collections::BTreeSet::new();
        // A page's record can repeat the interrupted reply and terminal of a turn it cuts.
        rows.reverse();
        rows.retain(|(source, fresh, at)| {
            match &if *fresh { &self } else { previous }.blocks[*at] {
                Block::TurnEnd { loop_id, .. } => ends.insert(loop_id.clone()),
                Block::Assistant { key, .. } => replies.insert(key.clone()),
                Block::Tool(call) => tools.insert(call.key.clone()),
                Block::Compaction { .. } => summaries.insert(*source),
                _ => true,
            }
        });
        rows.reverse();
        let mut result = Self::default();
        for (history, fresh, at) in rows {
            let source = if fresh { &self } else { previous };
            let inserted = result.len();
            result.splice_copy(inserted..inserted, source, at..at + 1);
            result.origins[inserted].history = Some(history);
        }
        result
    }
}

/// Identifies a block a reconcile kept although the canonical body omits it, so
/// the reader is told that text is temporary rather than authoritative.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PreservedKey {
    Assistant(RequestKey),
    AssistantText(RequestKey),
    AssistantReasoning(RequestKey),
    User(String),
    Tool(ToolRef),
}

impl PreservedKey {
    pub fn belongs_to(&self, block: &Block) -> bool {
        match (self, block) {
            (
                Self::AssistantText(key) | Self::AssistantReasoning(key),
                Block::Assistant { key: other, .. },
            ) => key == other,
            _ => Self::of(block).as_ref() == Some(self),
        }
    }
    pub fn of(block: &Block) -> Option<Self> {
        match block {
            Block::Assistant { key, .. } => Some(Self::Assistant(key.clone())),
            Block::User { text, .. } => Some(Self::User(text.clone())),
            Block::Tool(call) => Some(Self::Tool(call.key.clone())),
            _ => None,
        }
    }
}

/// The canonical body with the live-only text kept, and the keys of that text.
///
/// The blocks of a turn's durable items with what only the live turn showed
/// put back: card facts, thinking and text the items lack, and the blocks no
/// item covers, such as the request an interrupt cut short.
pub fn merge_live(
    mut durable: Vec<Block>,
    live: &[Block],
) -> (Vec<Block>, Vec<PreservedKey>, Vec<usize>) {
    let mut preserved = Vec::new();
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
                    if reasoning.is_empty() && !known_reasoning.is_empty() {
                        reasoning.clone_from(known_reasoning);
                        preserved.push(PreservedKey::AssistantReasoning(key.clone()));
                    }
                    if text.is_empty() && !known_text.is_empty() {
                        text.clone_from(known_text);
                        preserved.push(PreservedKey::AssistantText(key.clone()));
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
    let mut sources: Vec<usize> = (0..durable.len()).collect();
    let uncovered: Vec<(usize, Block)> = live
        .iter()
        .enumerate()
        .filter(|(_, block)| match block {
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
        .map(|(index, block)| (durable.len() + index, block.clone()))
        .collect();
    // What no canonical item covers is text only the live turn had.
    preserved.extend(
        uncovered
            .iter()
            .filter_map(|(_, block)| PreservedKey::of(block)),
    );
    for (source, block) in uncovered {
        sources.push(source);
        durable.push(block);
    }
    let (durable, sources) = durable
        .into_iter()
        .zip(sources)
        .filter(|(block, _)| !block.is_blank_assistant())
        .unzip();
    (durable, preserved, sources)
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
    append_history_from(transcript, items, records, session_id, None);
}

fn append_history_from(
    transcript: &mut Transcript,
    items: &[RawHistoryItem],
    records: &[TurnRecord],
    session_id: &str,
    from: Option<usize>,
) {
    let close =
        |transcript: &mut Transcript, loop_id: Option<&str>, whole: bool, item: Option<usize>| {
            let Some(record) = loop_id.and_then(|id| records.iter().find(|r| r.loop_id == id))
            else {
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
                transcript.origins.last_mut().unwrap().history = item.map(|item| HistorySource {
                    item,
                    part: INTERRUPTED_PART,
                });
            }
            let terminal = transcript.push(Block::TurnEnd {
                loop_id: record.loop_id.clone(),
                outcome: record.outcome.clone(),
                duration: None,
                requests: record.requests,
                tools,
                usage: record.usage.clone(),
                changes: None,
                // A stored record exists, so the turn is in committed history.
                saving: Some(crate::transcript::Saving::Saved),
            });
            transcript.origins[terminal].history = item.map(|item| HistorySource {
                item,
                part: TERMINAL_PART,
            });
        };
    let mut open: Option<&str> = None;
    // Whether the open turn's prompt is among `items`, so its cards are all here.
    let mut whole = false;
    let mut last = None;
    for (offset, item) in items.iter().enumerate() {
        match &item.item {
            RuntimeItem::User(user) if user.kind != UserKind::Steering => {
                close(transcript, open.take(), whole, last);
                whole = true;
                // A turn cut in its first request has no assistant item to open it.
                open = records
                    .iter()
                    .find(|r| r.loop_id == user.loop_id && r.interrupted.is_some())
                    .map(|r| r.loop_id.as_str());
            }
            RuntimeItem::Summary(_) => {
                close(transcript, open.take(), whole, last);
                whole = false;
            }
            RuntimeItem::Assistant(assistant) => open = Some(&assistant.loop_id),
            _ => {}
        }
        let first = transcript.len();
        append_history_item(transcript, item, session_id);
        last = from.map(|from| from + offset);
        if let Some(index) = last {
            for (part, origin) in transcript.origins[first..].iter_mut().enumerate() {
                origin.history = Some(HistorySource {
                    item: index,
                    part: if matches!(item.item, RuntimeItem::Summary(_)) {
                        SUMMARY_PART
                    } else {
                        part
                    },
                });
            }
        }
        // A request that only called tools would add a second separator row.
        if transcript
            .blocks()
            .get(first)
            .is_some_and(Block::is_blank_assistant)
        {
            transcript.remove(first);
        }
    }
    close(transcript, open, whole, last);
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
            let index = transcript.len();
            transcript.push(Block::User {
                text: user.input.text.clone(),
                kind: user.kind,
                at: item.timestamp.as_deref().and_then(unix_millis),
            });
            transcript.identify_user(index, &user.loop_id, None);
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
                "success" | "input_provided" => ToolState::Succeeded,
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
        let (merged, _, _) = merge_live(durable.into_blocks(), &live);
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
        assert_eq!(
            call.state,
            ToolState::Pending,
            "a call with no result or final state is not known succeeded"
        );
    }

    #[test]
    fn tool_result_updates_the_card_it_belongs_to() {
        for outcome in ["success", "input_provided"] {
            let mut transcript = Transcript::default();
            let mut call = ToolCall::new(key("c1"), "bash");
            call.state = ToolState::Running;
            transcript.push(Block::Tool(Box::new(call)));
            let result = decode_item(
                &json!({"item": {"type": "tool_result", "data": {
                    "loop_id": "l", "request_index": 1, "call_id": "c1",
                    "tool_name": "bash", "outcome": outcome,
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
    }

    #[test]
    fn only_text_the_canonical_body_lacks_is_marked_preserved() {
        let durable = vec![Block::Assistant {
            key: RequestKey {
                loop_id: "l".into(),
                request_index: 0,
            },
            reasoning: String::new(),
            text: "the canonical answer".into(),
            streaming: false,
            expanded: None,
        }];
        let live = vec![
            Block::Assistant {
                key: RequestKey {
                    loop_id: "l".into(),
                    request_index: 0,
                },
                reasoning: String::new(),
                text: "the canonical answer".into(),
                streaming: false,
                expanded: None,
            },
            // A request the canonical body does not contain, such as the one an
            // interrupt cut short.
            Block::Assistant {
                key: RequestKey {
                    loop_id: "l".into(),
                    request_index: 1,
                },
                reasoning: String::new(),
                text: "the interrupted reply".into(),
                streaming: false,
                expanded: None,
            },
        ];
        let (merged, preserved, _) = merge_live(durable, &live);
        assert_eq!(merged.len(), 2, "the live-only text is kept");
        assert_eq!(
            preserved,
            [PreservedKey::Assistant(RequestKey {
                loop_id: "l".into(),
                request_index: 1,
            })],
            "only the text canonical does not contain is marked"
        );
    }

    #[test]
    fn a_live_only_channel_is_temporary_without_relabeling_the_canonical_channel() {
        let key = RequestKey {
            loop_id: "l".into(),
            request_index: 0,
        };
        let block = |reasoning: &str, text: &str| Block::Assistant {
            key: key.clone(),
            reasoning: reasoning.into(),
            text: text.into(),
            streaming: false,
            expanded: None,
        };
        let (blocks, preserved, _) = merge_live(
            vec![block("", "canonical")],
            &[block("live-only reasoning", "streamed")],
        );
        assert_eq!(
            preserved,
            vec![PreservedKey::AssistantReasoning(key.clone())]
        );
        let mut transcript = Transcript::default();
        transcript.splice(0..0, blocks);
        transcript.mark_preserved(preserved);
        assert!(!transcript.is_preserved(0));
        assert!(!transcript.is_preserved_channel(&key, false));
        assert!(transcript.is_preserved_channel(&key, true));
        transcript.splice(0..1, vec![block("canonical reasoning", "canonical")]);
        assert!(
            !transcript.is_preserved_channel(&key, true),
            "a later verified channel clears its old temporary marker"
        );
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
        let (merged, _, _) = merge_live(durable.into_blocks(), &live);
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
