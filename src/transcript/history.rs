//! Native accepted records as blocks and streaming Markdown.

use std::io::{self, Write};

use minicore_host_core::{HistoryItemData, HistoryRecordData, TurnResultSnapshot};
use minicore_host_protocol::{MessageMode, PersistenceState, ToolCallId, ToolCallRef};
use minicore_host_runtime::model::{AssistantPart, Usage};

use super::{Block, HistorySource, RequestKey, ToolCall, ToolKey, Transcript, UserKind};

impl Transcript {
    pub fn from_history(records: &[HistoryRecordData]) -> Self {
        let mut transcript = Self::default();
        transcript.install_history_records(records);
        transcript
    }

    /// Installs accepted records by position without rebuilding streamed bodies.
    pub fn install_history_records(&mut self, records: &[HistoryRecordData]) {
        for record in records {
            append_record(self, record);
        }
        self.sort_history();
    }

    pub fn from_turn_result(result: &TurnResultSnapshot) -> Self {
        let turn = &result.source.turn_id;
        let mut records: Vec<_> = result
            .data
            .history
            .iter()
            .map(|item| HistoryRecordData {
                position: item.position,
                recorded_at: item.recorded_at,
                turn_id: Some(turn.clone()),
                response_index: item.response_index,
                item: item.item.clone(),
            })
            .chain(
                result
                    .data
                    .partials
                    .iter()
                    .map(|partial| HistoryRecordData {
                        position: partial.position,
                        recorded_at: partial.recorded_at,
                        turn_id: Some(turn.clone()),
                        response_index: Some(partial.response_index),
                        item: HistoryItemData::Partial {
                            partial: partial.partial.clone(),
                        },
                    }),
            )
            .collect();
        if let Some(saved) = &result.data.saved_terminal {
            records.push(HistoryRecordData {
                position: saved.position,
                recorded_at: saved.recorded_at,
                turn_id: Some(turn.clone()),
                response_index: None,
                item: HistoryItemData::TurnFinished {
                    terminal: saved.terminal.clone(),
                },
            });
        }
        records.sort_by_key(|record| record.position);
        let mut transcript = Self::from_history(&records);
        if result.data.saved_terminal.is_some()
            && let Some(Block::TurnEnd { saving, .. }) = transcript
                .blocks
                .iter_mut()
                .find(|block| matches!(block, Block::TurnEnd { turn_id, .. } if turn_id == turn))
        {
            *saving = Some(PersistenceState::Persisted);
        }
        if let Some(info) = &result.data.info
            && let Some(outcome) = info.outcome
        {
            let block = Block::TurnEnd {
                turn_id: turn.clone(),
                outcome,
                reason: info.fault.as_ref().map(|fault| fault.message().to_owned()),
                duration: None,
                requests: info.requests,
                tool_rounds: info.tool_rounds,
                usage: Usage::default(),
                changes: None,
                saving: Some(info.persistence),
            };
            let at = match transcript.blocks.iter().position(
                |block| matches!(block, Block::TurnEnd { turn_id, .. } if turn_id == turn),
            ) {
                Some(at) => {
                    transcript.blocks[at] = block;
                    at
                }
                None => transcript.push(block),
            };
            if let (Some(position), Some(recorded_at)) = (info.history_position, info.recorded_at) {
                transcript.add_history_source(
                    at,
                    HistorySource {
                        position,
                        recorded_at,
                        turn_id: Some(turn.clone()),
                        response_index: None,
                        part: 0,
                    },
                );
            }
        }
        transcript.sort_history();
        transcript
    }
}

pub fn append_history_record(transcript: &mut Transcript, record: &HistoryRecordData) {
    transcript.install_history_records(std::slice::from_ref(record));
}

fn source(record: &HistoryRecordData, part: usize) -> HistorySource {
    HistorySource {
        position: record.position,
        recorded_at: record.recorded_at,
        turn_id: record.turn_id.clone(),
        response_index: record.response_index,
        part,
    }
}

fn request_key(record: &HistoryRecordData) -> RequestKey {
    match (&record.turn_id, record.response_index) {
        (Some(turn_id), Some(response_index)) => {
            RequestKey::response(turn_id.clone(), response_index)
        }
        _ => RequestKey::History {
            position: record.position,
        },
    }
}

fn tool_key(record: &HistoryRecordData, tool_call_id: ToolCallId) -> ToolKey {
    match &record.turn_id {
        Some(turn_id) => ToolKey::Call(ToolCallRef {
            turn_id: turn_id.clone(),
            tool_call_id,
        }),
        None => ToolKey::History {
            position: record.position,
            tool_call_id,
        },
    }
}

fn append_record(transcript: &mut Transcript, record: &HistoryRecordData) {
    if transcript.contains_history_position(record.position) {
        for at in 0..transcript.metadata.len() {
            let mut changed = false;
            for source in &mut transcript.metadata[at].history {
                if source.position == record.position
                    && source.response_index.is_none()
                    && record.response_index.is_some()
                {
                    source.response_index = record.response_index;
                    changed = true;
                }
            }
            if changed {
                if let Block::Tool(call) = &mut transcript.blocks[at] {
                    call.response_index = record.response_index;
                }
                transcript.bump(at);
            }
        }
        return;
    }
    match &record.item {
        HistoryItemData::User {
            received_at,
            text,
            mode,
            message_id,
            ..
        } => {
            let at = transcript.push(Block::User {
                text: text.clone(),
                kind: match mode {
                    Some(MessageMode::Steer) => UserKind::Steering,
                    Some(MessageMode::FollowUp) => UserKind::FollowUp,
                    None => UserKind::Prompt,
                },
                at: Some(received_at.unwrap_or(record.recorded_at)),
            });
            transcript.identify_user(at, record.turn_id.clone(), message_id.clone());
            transcript.add_history_source(at, source(record, 0));
        }
        HistoryItemData::Assistant { response } => {
            append_response(transcript, record, &response.content, false);
        }
        HistoryItemData::Partial { partial } => {
            append_response(transcript, record, &partial.content, true);
        }
        HistoryItemData::ToolResult {
            tool_call_id,
            name,
            result,
        } => {
            let key = tool_key(record, tool_call_id.clone());
            let known = transcript.blocks.iter().rposition(|block| match block {
                Block::Tool(call) => {
                    call.key == key
                        || (record.turn_id.is_none()
                            && call.key.turn_id().is_none()
                            && call.key.tool_call_id() == tool_call_id)
                }
                _ => false,
            });
            let at = known.unwrap_or_else(|| {
                transcript.push(Block::Tool(Box::new(ToolCall::with_key(key, name))))
            });
            if let Block::Tool(call) = &mut transcript.blocks[at] {
                call.response_index = record.response_index;
                call.apply_result(result);
            }
            transcript.add_history_source(at, source(record, 0));
        }
        HistoryItemData::Summary { text } => {
            let at = transcript.push(Block::Summary { text: text.clone() });
            transcript.add_history_source(at, source(record, 0));
        }
        HistoryItemData::Compaction {
            text,
            source_position,
            covered_items,
        } => {
            let at = transcript.push(Block::Compaction {
                source_position: *source_position,
                covered_items: *covered_items,
                summary: text.clone(),
            });
            transcript.add_history_source(at, source(record, 0));
        }
        HistoryItemData::TurnFinished { terminal } => {
            let Some(turn_id) = &record.turn_id else {
                return;
            };
            let block = Block::TurnEnd {
                turn_id: turn_id.clone(),
                outcome: terminal.outcome,
                reason: terminal.reason.clone(),
                duration: None,
                requests: terminal.requests,
                tool_rounds: terminal.tool_rounds,
                usage: Usage::default(),
                changes: None,
                saving: None,
            };
            let at = match transcript.blocks.iter().position(
                |block| matches!(block, Block::TurnEnd { turn_id: known, .. } if known == turn_id),
            ) {
                Some(at) => {
                    let mut block = block;
                    if let (
                        Block::TurnEnd {
                            duration,
                            changes,
                            saving,
                            ..
                        },
                        Block::TurnEnd {
                            duration: old_duration,
                            changes: old_changes,
                            saving: old_saving,
                            ..
                        },
                    ) = (&mut block, &transcript.blocks[at])
                    {
                        *duration = *old_duration;
                        *changes = *old_changes;
                        *saving = *old_saving;
                    }
                    transcript.blocks[at] = block;
                    at
                }
                None => transcript.push(block),
            };
            transcript.add_history_source(at, source(record, 0));
        }
    }
}

fn append_response(
    transcript: &mut Transcript,
    record: &HistoryRecordData,
    content: &[AssistantPart],
    partial: bool,
) {
    let key = request_key(record);
    let mut text = String::new();
    let mut reasoning = String::new();
    for part in content {
        match part {
            AssistantPart::Text(chunk) => text.push_str(chunk),
            AssistantPart::Reasoning(chunk) => {
                if let Some(chunk) = chunk.text().or(chunk.summary()) {
                    reasoning.push_str(chunk);
                }
            }
            AssistantPart::ToolCall(_) => {}
        }
    }
    let at =
        match transcript.blocks.iter().rposition(
            |block| matches!(block, Block::Assistant { key: known, .. } if known == &key),
        ) {
            Some(at) => {
                if let Block::Assistant {
                    reasoning: known_reasoning,
                    text: known_text,
                    streaming,
                    partial: known_partial,
                    ..
                } = &mut transcript.blocks[at]
                {
                    *known_reasoning = reasoning;
                    *known_text = text;
                    *streaming = false;
                    *known_partial = partial;
                }
                at
            }
            None => transcript.push(Block::Assistant {
                key,
                reasoning,
                text,
                streaming: false,
                partial,
                expanded: None,
            }),
        };
    transcript.add_history_source(at, source(record, 0));
    if partial {
        return;
    }
    for (part_index, part) in content.iter().enumerate() {
        let AssistantPart::ToolCall(call) = part else {
            continue;
        };
        let key = tool_key(record, ToolCallId(call.tool_call_id().as_str().to_owned()));
        let known = transcript
            .blocks
            .iter()
            .rposition(|block| matches!(block, Block::Tool(known) if known.key == key));
        let at = known.unwrap_or_else(|| {
            transcript.push(Block::Tool(Box::new(ToolCall::with_key(
                key,
                call.name().as_str(),
            ))))
        });
        if let Block::Tool(card) = &mut transcript.blocks[at] {
            card.response_index = record.response_index;
            card.set_arguments(call.arguments());
        }
        transcript.add_history_source(at, source(record, part_index + 1));
    }
}

pub fn to_markdown(records: &[HistoryRecordData]) -> String {
    let mut out = Vec::new();
    write_markdown(&mut out, records).expect("writing to a vector succeeds");
    String::from_utf8(out).expect("Markdown contains UTF-8")
}

/// Writes native records directly, without bounded tool-card output or screen state.
pub fn write_markdown(writer: &mut impl Write, records: &[HistoryRecordData]) -> io::Result<()> {
    for record in records {
        let stamp = serde_json::to_value(record.recorded_at).expect("Timestamp serializes");
        write!(
            writer,
            "<!-- record {} · {}",
            record.position,
            stamp.as_str().expect("Timestamp is text")
        )?;
        if let Some(turn) = &record.turn_id {
            write!(writer, " · turn {turn}")?;
        }
        if let Some(response) = record.response_index {
            write!(writer, " · response {response}")?;
        }
        writeln!(writer, " -->\n")?;
        match &record.item {
            HistoryItemData::User { text, mode, .. } => {
                writeln!(
                    writer,
                    "**{}**\n\n{text}\n",
                    if *mode == Some(MessageMode::Steer) {
                        "Steering"
                    } else {
                        "You"
                    }
                )?;
            }
            HistoryItemData::Assistant { response } => {
                write_parts(writer, &response.content, false)?
            }
            HistoryItemData::Partial { partial } => {
                writeln!(writer, "**Incomplete assistant response**\n")?;
                write_parts(writer, &partial.content, true)?;
                for call in &partial.incomplete_tool_calls {
                    writeln!(
                        writer,
                        "*Unfinished tool call `{}` (`{}`); not executed.*\n",
                        call.tool_name, call.tool_call_id
                    )?;
                    write_fenced(writer, &call.arguments_text, "text")?;
                }
            }
            HistoryItemData::ToolResult {
                tool_call_id,
                name,
                result,
            } => {
                writeln!(
                    writer,
                    "**Tool result: {name}** (`{tool_call_id}`, {:?})\n",
                    result.outcome
                )?;
                write_fenced(writer, result.output.content().as_str(), "text")?;
            }
            HistoryItemData::Summary { text } => {
                writeln!(writer, "---\n\n**Context summary**\n\n{text}\n")?
            }
            HistoryItemData::Compaction {
                text,
                source_position,
                covered_items,
            } => {
                writeln!(
                    writer,
                    "---\n\n**Context compacted** · {covered_items} items · source position {source_position}\n\n{text}\n"
                )?;
            }
            HistoryItemData::TurnFinished { terminal } => {
                writeln!(
                    writer,
                    "*Turn {:?} · {} requests · {} tool rounds*\n",
                    terminal.outcome, terminal.requests, terminal.tool_rounds
                )?;
                if let Some(reason) = &terminal.reason {
                    writeln!(writer, "{reason}\n")?;
                }
            }
        }
    }
    Ok(())
}

fn write_parts(writer: &mut impl Write, parts: &[AssistantPart], partial: bool) -> io::Result<()> {
    for part in parts {
        match part {
            AssistantPart::Text(text) => writeln!(writer, "{text}\n")?,
            AssistantPart::Reasoning(reasoning) => {
                if let Some(text) = reasoning.text().or(reasoning.summary()) {
                    writeln!(writer, "> {}\n", text.replace('\n', "\n> "))?;
                }
            }
            AssistantPart::ToolCall(call) => {
                writeln!(
                    writer,
                    "**{}: {}** (`{}`)\n",
                    if partial {
                        "Unexecuted tool call in partial response"
                    } else {
                        "Tool call"
                    },
                    call.name(),
                    call.tool_call_id()
                )?;
                write_fenced(
                    writer,
                    &serde_json::to_string_pretty(call.arguments()).expect("JSON serializes"),
                    "json",
                )?;
            }
        }
    }
    Ok(())
}

fn write_fenced(writer: &mut impl Write, text: &str, language: &str) -> io::Result<()> {
    let fence = "`".repeat(
        text.lines()
            .map(|line| line.chars().take_while(|ch| *ch == '`').count())
            .max()
            .unwrap_or(0)
            .max(2)
            + 1,
    );
    writeln!(writer, "{fence}{language}\n{text}\n{fence}\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn record(position: u64, item: Value) -> HistoryRecordData {
        let mut value = json!({"position":position,"recorded_at":"2026-11-13T10:00:00Z",
            "turn_id":"00000000000000000001","response_index":0});
        value
            .as_object_mut()
            .unwrap()
            .extend(item.as_object().unwrap().clone());
        serde_json::from_value(value).unwrap()
    }

    fn assistant(position: u64) -> HistoryRecordData {
        record(
            position,
            json!({"kind":"assistant","response":{"content":[
            {"type":"text","data":"working"},
            {"type":"tool_call","data":{"tool_call_id":"call","name":"bash","arguments":{"command":"printf '你好'\necho done"},"call_index":0}}
        ],"finish_reason":"tool_calls","usage":{}}}),
        )
    }

    #[test]
    fn repeated_message_ids_and_equal_text_keep_distinct_native_positions() {
        let user =
            json!({"kind":"user","text":"same","message_id":"00000000000000000002","mode":"steer"});
        let records = [record(4, user.clone()), record(5, user)];
        let mut transcript = Transcript::from_history(&records);
        let revisions: Vec<_> = transcript.revisions().collect();
        transcript.install_history_records(&records);
        assert_eq!(transcript.len(), 2);
        assert_eq!(transcript.history_source(0).unwrap().position, 4);
        assert_eq!(transcript.history_source(1).unwrap().position, 5);
        assert_eq!(transcript.revisions().collect::<Vec<_>>(), revisions);
    }

    #[test]
    fn a_result_cut_by_the_tail_page_joins_its_later_loaded_invocation() {
        let result = record(
            2,
            json!({"kind":"tool_result","tool_call_id":"call","name":"bash",
            "result":{"outcome":"success","output":{"content":"你好\ndone"}}}),
        );
        let mut transcript = Transcript::from_history(&[result]);
        assert_eq!(transcript.tool_calls().count(), 1);
        transcript.install_history_records(&[assistant(1)]);
        let call = transcript.tool_calls().next().unwrap();
        assert_eq!(call.state, super::super::ToolState::Succeeded);
        assert_eq!(
            call.arguments.as_ref().unwrap()["command"],
            "printf '你好'\necho done"
        );
        assert_eq!(call.output_content(), ["你好", "done"]);
        assert_eq!(transcript.tool_calls().count(), 1);
        let tool = transcript
            .blocks
            .iter()
            .position(|block| matches!(block, Block::Tool(_)))
            .unwrap();
        assert_eq!(
            transcript
                .history_sources(tool)
                .iter()
                .map(|source| source.position)
                .collect::<Vec<_>>(),
            [1, 2]
        );
    }

    #[test]
    fn a_partial_is_neither_a_complete_response_nor_a_tool_execution() {
        let partial = record(
            3,
            json!({"kind":"partial","partial":{"content":[{"type":"text","data":"unfinished α"}],
            "incomplete_tool_calls":[{"part_index":1,"call_index":0,"tool_call_id":"fragment","tool_name":"bash","arguments_text":"{\"command\":"}],"usage":null}}),
        );
        let transcript = Transcript::from_history(std::slice::from_ref(&partial));
        assert!(
            matches!(&transcript.blocks()[0], Block::Assistant { partial: true, streaming: false, text, .. } if text == "unfinished α")
        );
        assert_eq!(transcript.tool_calls().count(), 0);
        assert!(to_markdown(&[partial]).contains("not executed"));
    }

    #[test]
    fn summaries_and_compactions_keep_all_prior_transcript_content() {
        let records = [
            record(0, json!({"kind":"user","text":"keep me"})),
            record(1, json!({"kind":"summary","text":"literal summary"})),
            record(
                2,
                json!({"kind":"compaction","text":"replacement context","source_position":2,"covered_items":2}),
            ),
        ];
        let transcript = Transcript::from_history(&records);
        assert_eq!(transcript.len(), 3);
        assert!(matches!(transcript.blocks()[0], Block::User { .. }));
        assert!(matches!(transcript.blocks()[1], Block::Summary { .. }));
        assert!(matches!(
            transcript.blocks()[2],
            Block::Compaction {
                source_position: 2,
                covered_items: 2,
                ..
            }
        ));
    }

    #[test]
    fn seed_responses_keep_position_identity_without_inventing_a_turn() {
        let mut record = assistant(8);
        record.turn_id = None;
        record.response_index = None;
        let transcript = Transcript::from_history(&[record]);
        assert!(matches!(
            &transcript.blocks()[0],
            Block::Assistant {
                key: RequestKey::History { position: 8 },
                ..
            }
        ));
        assert!(
            transcript
                .tool_calls()
                .next()
                .unwrap()
                .key
                .call_ref()
                .is_none()
        );
    }

    #[test]
    fn export_uses_complete_native_results_even_when_the_card_has_a_bounded_tail() {
        let output = (0..900)
            .map(|line| format!("line {line}\n"))
            .collect::<String>();
        let records = [
            assistant(1),
            record(
                2,
                json!({"kind":"tool_result","tool_call_id":"call","name":"bash",
            "result":{"outcome":"success","output":{"content":output}}}),
            ),
        ];
        let transcript = Transcript::from_history(&records);
        let call = transcript.tool_calls().next().unwrap();
        assert!(!call.output_content().contains(&"line 0"));
        let markdown = to_markdown(&records);
        assert!(markdown.contains("line 0\n"));
        assert!(markdown.contains("line 899\n"));
        assert!(markdown.contains("printf '你好'\\necho done"));
        assert!(markdown.contains("record 2 · 2026-11-13T10:00:00Z"));
    }

    #[test]
    fn history_completes_the_response_identity_missing_from_a_tool_finish_event() {
        let mut event = record(
            2,
            json!({"kind":"tool_result","tool_call_id":"call","name":"bash",
            "result":{"outcome":"success","output":{"content":"done"}}}),
        );
        event.response_index = None;
        let mut transcript = Transcript::from_history(&[event.clone()]);
        let anchor = transcript.history_source(0).unwrap().clone();
        let mut canonical = event;
        canonical.response_index = Some(7);
        transcript.install_history_records(&[canonical]);
        assert_eq!(transcript.len(), 1);
        assert_eq!(transcript.find_history_source(&anchor), Some(0));
        assert_eq!(
            transcript.history_source(0).unwrap().response_index,
            Some(7)
        );
        assert_eq!(
            transcript.tool_calls().next().unwrap().response_index,
            Some(7)
        );
    }
}
