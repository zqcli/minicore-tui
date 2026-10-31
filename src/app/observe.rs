//! Opt-in observations of the installed projection for process acceptance tests.

use std::fs::File;
use std::io::{self, Write};
use std::time::Instant;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::tasks::TurnContent;
use super::{App, Session};
use crate::protocol::{CompletedTurn, SessionExecution};
use crate::transcript::Block;

pub struct Observer {
    file: File,
    started: Instant,
    seq: u64,
    bytes: usize,
    limits: Limits,
}

#[derive(Clone, Copy)]
struct Limits {
    row: usize,
    events: u64,
    file: usize,
}

const TRACE_LIMITS: Limits = Limits {
    row: 1024 * 1024,
    events: 20_000,
    file: 32 * 1024 * 1024,
};

struct RowBuffer {
    bytes: Vec<u8>,
    limit: usize,
}

impl Write for RowBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit - self.bytes.len() {
            return Err(io::Error::other("acceptance trace row limit exceeded"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub struct Installed {
    loop_id: String,
    seq: Option<u64>,
    input_id: Option<String>,
    result_id: String,
    read_item_count: usize,
}

impl Observer {
    fn emit(&mut self, event: &str, mut row: Value) {
        assert!(
            self.seq < self.limits.events,
            "acceptance trace event limit exceeded"
        );
        self.seq = self
            .seq
            .checked_add(1)
            .expect("observation sequence exhausted");
        row["schema"] = json!(1);
        row["event_seq"] = json!(self.seq);
        row["event"] = json!(event);
        row["mono_ns"] = json!(self.started.elapsed().as_nanos() as u64);
        let mut buffer = RowBuffer {
            bytes: Vec::new(),
            limit: self.limits.row,
        };
        serde_json::to_writer(&mut buffer, &row).expect("acceptance trace row limit exceeded");
        let bytes = buffer.bytes.len() + 1;
        assert!(
            bytes <= self.limits.file - self.bytes,
            "acceptance trace file limit exceeded"
        );
        self.file
            .write_all(&buffer.bytes)
            .expect("acceptance trace write failed");
        self.file
            .write_all(b"\n")
            .expect("acceptance trace write failed");
        self.file.flush().expect("acceptance trace flush failed");
        self.bytes += bytes;
    }
}

impl Installed {
    fn new(content: &TurnContent, completion: Option<&CompletedTurn>) -> Self {
        Self {
            loop_id: content.turn.loop_id.clone(),
            seq: completion.map(|turn| turn.seq),
            input_id: completion.and_then(|turn| turn.input_id.clone()),
            result_id: content.result_id.clone(),
            read_item_count: content.items.len(),
        }
    }

    fn parts(&self, session: &Session) -> Vec<Value> {
        let mut parts = Vec::new();
        for (index, block) in session.transcript.blocks().iter().enumerate() {
            if block.loop_id() != Some(self.loop_id.as_str())
                || session.transcript.is_preserved(index)
            {
                continue;
            }
            match block {
                Block::Assistant {
                    key,
                    reasoning,
                    text,
                    ..
                } => {
                    for (channel, text) in [("reasoning", reasoning), ("text", text)] {
                        if text.is_empty()
                            || session
                                .transcript
                                .is_preserved_channel(key, channel == "reasoning")
                        {
                            continue;
                        }
                        let text = text.replace("\r\n", "\n");
                        parts.push(json!({"kind":"assistant", "request_index":key.request_index, "channel":channel, "bytes":text.len(), "sha256":format!("{:x}",Sha256::digest(text.as_bytes()))}));
                    }
                }
                Block::Tool(call) => {
                    parts.push(json!({"kind":"tool", "request_index":call.key.request_index, "tool_call_id":call.key.tool_call_id, "state":format!("{:?}",call.state).to_lowercase()}));
                }
                _ => {}
            }
        }
        parts
    }

    fn row(&self, session: &Session) -> Value {
        let saving = session
            .transcript
            .blocks()
            .iter()
            .find_map(|block| match block {
                Block::TurnEnd {
                    loop_id, saving, ..
                } if loop_id == &self.loop_id => *saving,
                _ => None,
            });
        let (persistence, uncertain) = match saving {
            Some(crate::transcript::Saving::Saved) => (Some("persisted"), Some(false)),
            Some(crate::transcript::Saving::Unsaved) => (Some("failed"), Some(false)),
            Some(crate::transcript::Saving::Uncertain) => (Some("failed"), Some(true)),
            None => (None, None),
        };
        json!({"session_id":session.info.session_id,"load":session.load,"loop_id":self.loop_id,
            "completion_seq":self.seq,"input_id":self.input_id,"result_id":self.result_id,"read_item_count":self.read_item_count,
            "persistence":persistence,"persistence_uncertain":uncertain,
            "recovery_status":"complete","parts":self.parts(session)})
    }
}

impl App {
    pub(super) fn observe_phone_gate(
        &mut self,
        observation: &crate::remote::phone_gate::Observation,
    ) {
        self.observer
            .as_mut()
            .expect("phone gate requires an acceptance observer")
            .emit(observation.event(), observation.row());
    }

    pub fn observe_application_gate(
        &mut self,
        resumed: bool,
        id: &str,
        load: &str,
        held_kind: &str,
    ) {
        if let Some(observer) = &mut self.observer {
            observer.emit(
                if resumed {
                    "application_resumed"
                } else {
                    "application_paused"
                },
                json!({"session_id":id,"load":load,"held_kind":held_kind}),
            );
        }
        self.observe_checkpoint(false);
    }
    pub fn start_observer(&mut self, file: File) {
        let mut observer = Observer {
            file,
            started: Instant::now(),
            seq: 0,
            bytes: 0,
            limits: TRACE_LIMITS,
        };
        observer.emit("observer_ready", json!({}));
        self.observer = Some(observer);
    }

    pub(super) fn observe_control(&mut self, snapshot: &crate::protocol::SessionSnapshot) {
        if self.observer.is_none() {
            return;
        }
        let id = &snapshot.session_id;
        let session = self.live(id).expect("applied session is resident");
        let execution = match session.execution.as_ref() {
            Some(SessionExecution::None) | None => json!({"kind":"none"}),
            Some(SessionExecution::Starting { input }) => {
                json!({"kind":"starting","input_id":input.id})
            }
            Some(SessionExecution::Running {
                loop_id,
                input,
                request_index,
                ..
            }) => {
                json!({"kind":"running","loop_id":loop_id,"input_id":input.as_ref().map(|input|&input.id),"request_index":request_index})
            }
            Some(SessionExecution::Finishing {
                loop_id,
                input,
                request_index,
            }) => {
                json!({"kind":"finishing","loop_id":loop_id,"input_id":input.as_ref().map(|input|&input.id),"request_index":request_index})
            }
        };
        let interaction = session.interaction.as_ref().map(|pending|json!({"loop_id":pending.turn.loop_id,"request_index":snapshot.pending_interaction.as_ref().unwrap().request_index,"tool_call_id":pending.tool_call_id,"interaction_id":pending.interaction_id,"kind":match &pending.kind { super::InteractionKind::Approval=>"approval",super::InteractionKind::Choice(_)=>"single_choice",super::InteractionKind::Text=>"text" }}));
        let row = json!({"session_id":id,"load":session.load,"revision":session.recovery.revision,
            "queue_ids":session.queue.items.iter().map(|item|&item.id).collect::<Vec<_>>(),"paused":session.queue.paused,
            "execution":execution,"interaction":interaction,"block_reason":session.block_reason.map(|reason|match reason { crate::protocol::BlockReason::Persistence=>"persistence",crate::protocol::BlockReason::Internal=>"internal" }),"observed_through":session.recovery.observed_through,"settled_through":session.recovery.settled_through});
        self.observer.as_mut().unwrap().emit("control_applied", row);
    }

    pub(super) fn observe_install(
        &mut self,
        content: &TurnContent,
        completion: Option<&CompletedTurn>,
    ) {
        if self.observer.is_none() {
            return;
        }
        let installed = Installed::new(content, completion);
        let row = installed.row(self.session.as_ref().unwrap());
        let session = self.session.as_mut().unwrap();
        session
            .observed_installs
            .retain(|known| known.loop_id != installed.loop_id);
        session.observed_installs.push(installed);
        if session.observed_installs.len() > 256 {
            session.observed_installs.remove(0);
        }
        self.observer
            .as_mut()
            .unwrap()
            .emit("canonical_install", row);
    }

    pub fn observe_checkpoint(&mut self, ending: bool) {
        if self.observer.is_none() {
            return;
        }
        let mut rows = Vec::new();
        let metrics = self.client.metrics.snapshot();
        let mut totals = json!({});
        for (key, value) in serde_json::to_value(metrics).unwrap().as_object().unwrap() {
            totals[format!("client_{key}")] = value.clone();
        }
        totals["client_early_body_bytes"] = json!(
            self.session
                .iter()
                .chain(&self.background)
                .map(|session| session.early_bytes)
                .sum::<usize>()
        );
        totals["client_early_body_peak_bytes"] = json!(self.early_body_peak_bytes);
        for session in self.session.iter().chain(&self.background) {
            let turns: Vec<Value> = session
                .transcript
                .blocks()
                .iter()
                .filter_map(|block| {
                    let Block::TurnEnd { loop_id, .. } = block else {
                        return None;
                    };
                    session
                        .observed_installs
                        .iter()
                        .find(|turn| turn.loop_id == *loop_id)
                        .map(|turn| turn.row(session))
                })
                .collect();
            rows.push((
                "canonical_view",
                json!({"session_id":session.info.session_id,"load":session.load,"turns":turns,"inventory":inventory(session),"delivered_inputs":session.delivered.iter().map(|input|json!({"loop_id":input.loop_id,"input_id":input.input_id,"request_index":input.request_index})).collect::<Vec<_>>(),"local_input_state":self.local_input_projection()}),
            ));
            let mut values = totals.clone();
            values["early_body_bytes"] = json!(session.early_bytes);
            values["early_body_peak_bytes"] = json!(session.early_peak_bytes);
            values["pending_recovery_count"] = json!(session.recovery_pending());
            values["recovery_in_flight"] = json!(session.recovery_tasks.len());
            rows.push((
                "counters",
                json!({"session_id":session.info.session_id,"load":session.load,
                "scope":"client_counters_and_session_gauges","values":values}),
            ));
        }
        let observer = self.observer.as_mut().unwrap();
        for (kind, row) in rows {
            observer.emit(kind, row);
        }
        if ending {
            let state = self.local_input_projection();
            self.observer
                .as_mut()
                .unwrap()
                .emit("observer_end", json!({"local_input_state":state}));
        }
    }

    pub(super) fn observe_recovery(&mut self, loop_id: &str) {
        if self.observer.is_none() {
            return;
        }
        let session = self.session.as_ref().unwrap();
        let Some(note) = session
            .body_recovery
            .iter()
            .find(|note| note.loop_id == loop_id)
        else {
            return;
        };
        let terminal = session.transcript.blocks().iter().enumerate().find_map(|(index, block)| {
            let Block::TurnEnd { loop_id: id, outcome, saving, .. } = block else { return None; };
            (id == loop_id).then(||json!({"block_index":index,"outcome":outcome_kind(outcome),"saving":saving.map(|saving|saving.label())}))
        });
        let status = match note.status {
            super::recovery::RecoveryStatus::Partial => "partial",
            super::recovery::RecoveryStatus::Unavailable => "unavailable",
            _ => "retryable_error",
        };
        let completion = session
            .catalog
            .entries
            .iter()
            .find(|turn| turn.loop_id == loop_id);
        let row = json!({"session_id":session.info.session_id,"load":session.load,"loop_id":loop_id,
            "completion_seq":completion.map(|turn|turn.seq),"input_id":completion.and_then(|turn|turn.input_id.as_ref()),
            "status":status,"canonical":session.is_closed(loop_id),"terminal":terminal,"reason":if status == "retryable_error" { "read_failed" } else { &note.message }});
        self.observer.as_mut().unwrap().emit("recovery_status", row);
    }
}

impl App {
    fn local_input_projection(&self) -> Value {
        let rows=self.local_inputs.iter().map(|input|json!({"operation_id":format!("tui-{}",input.id),"session_id":input.target.session_id,"load":input.target.load,"epoch":input.target.epoch,"mode":input.mode,"status":input.status.label(),"retry_of":input.retry_of.map(|id|format!("tui-{id}")),"body":digest(input.entry.text().trim_end())})).collect::<Vec<_>>();
        let review = match self.overlay {
            super::Overlay::Unconfirmed { mode, .. } => {
                json!({"operation_id":self.selected_local_input().map(|input|format!("tui-{}",input.id)),"mode":mode.label(),"edited_body":matches!(mode,super::unconfirmed::ReviewMode::Edit(_)|super::unconfirmed::ReviewMode::ResendEdited(_)).then(||digest(self.unconfirmed_editor.text()))})
            }
            _ => Value::Null,
        };
        json!({"operations":rows,"retained_bytes":self.local_input_bytes(),"max_operations":super::unconfirmed::MAX_OPERATIONS,"max_bytes":super::unconfirmed::MAX_BYTES,"review":review})
    }
}

fn outcome_kind(outcome: &crate::protocol::LoopOutcome) -> &'static str {
    match outcome {
        crate::protocol::LoopOutcome::Completed => "completed",
        crate::protocol::LoopOutcome::Cancelled { .. } => "cancelled",
        crate::protocol::LoopOutcome::Failed { .. } => "failed",
        crate::protocol::LoopOutcome::Unknown => "unknown",
    }
}

fn digest(text: &str) -> Value {
    let normalized = text.replace("\r\n", "\n");
    json!({"bytes":normalized.len(),"sha256":format!("{:x}",Sha256::digest(normalized.as_bytes()))})
}

fn inventory(session: &Session) -> Vec<Value> {
    let mut rows = Vec::new();
    for (index, block) in session.transcript.blocks().iter().enumerate() {
        let preserved = session.transcript.is_preserved(index);
        let mut add = |mut row: Value| {
            row["block_index"] = json!(index);
            row["preserved"] = json!(preserved);
            rows.push(row);
        };
        match block {
            Block::User { text, kind, .. } => {
                let identity = session.transcript.user_identity(index);
                let input_id = identity
                    .and_then(|user| user.input_id.as_ref())
                    .or_else(|| {
                        let user = identity?;
                        if *kind != crate::protocol::read::UserKind::Prompt {
                            return None;
                        }
                        session
                            .catalog
                            .entries
                            .iter()
                            .find(|turn| turn.loop_id == user.loop_id)
                            .and_then(|turn| turn.input_id.as_ref())
                            .or_else(|| {
                                session
                                    .observed_installs
                                    .iter()
                                    .find(|turn| turn.loop_id == user.loop_id)
                                    .and_then(|turn| turn.input_id.as_ref())
                            })
                    });
                add(
                    json!({"kind":"user","loop_id":identity.map(|user|&user.loop_id),"input_id":input_id,"request_index":identity.and_then(|user|user.request_index),
                    "user_kind":match kind {crate::protocol::read::UserKind::Prompt=>"prompt",crate::protocol::read::UserKind::Steering=>"steering",crate::protocol::read::UserKind::Unknown=>"unknown"},"body":digest(text)}),
                );
            }
            Block::Assistant {
                key,
                reasoning,
                text,
                ..
            } => {
                for (channel, text) in [("reasoning", reasoning), ("text", text)] {
                    if text.is_empty() {
                        continue;
                    }
                    let mut row = json!({"kind":"assistant","loop_id":key.loop_id,"request_index":key.request_index,"channel":channel,"body":digest(text)});
                    row["preserved_channel"] = json!(
                        session
                            .transcript
                            .is_preserved_channel(key, channel == "reasoning")
                    );
                    add(row);
                }
            }
            Block::Tool(call) => add(
                json!({"kind":"tool","loop_id":call.key.loop_id,"request_index":call.key.request_index,"tool_call_id":call.key.tool_call_id,"state":format!("{:?}",call.state).to_lowercase()}),
            ),
            Block::TurnEnd {
                loop_id,
                outcome,
                saving,
                ..
            } => add(
                json!({"kind":"turn_end","loop_id":loop_id,"outcome":outcome_kind(outcome),"saving":saving.map(|saving|saving.label()),"canonical":session.is_closed(loop_id)}),
            ),
            Block::EarlierHistory { .. } => add(json!({"kind":"earlier_history"})),
            Block::Compaction { operation_id, .. } => {
                add(json!({"kind":"compaction","operation_id":operation_id}))
            }
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::LoopOutcome;
    use crate::transcript::{RequestKey, Saving};

    #[test]
    fn observations_hash_installed_bytes_and_read_the_displayed_saving_fact() {
        let mut session = Session::new(
            serde_json::from_value(crate::testing::session_info("s1")).unwrap(),
            1,
        );
        session.load = Some("load_1".into());
        session.transcript.push(Block::Assistant {
            key: RequestKey {
                loop_id: "A".into(),
                request_index: 0,
            },
            reasoning: String::new(),
            text: "installed\r\nbody".into(),
            streaming: false,
            expanded: None,
        });
        session.transcript.push(Block::TurnEnd {
            loop_id: "A".into(),
            outcome: LoopOutcome::Completed,
            duration: None,
            requests: 1,
            tools: 0,
            usage: Default::default(),
            changes: None,
            saving: Some(Saving::Unsaved),
        });
        let installed = Installed {
            loop_id: "A".into(),
            seq: Some(1),
            input_id: Some("iA".into()),
            result_id: "r1:test".into(),
            read_item_count: 2,
        };
        let first = installed.row(&session);
        assert_eq!(
            first["parts"][0]["sha256"],
            format!("{:x}", Sha256::digest(b"installed\nbody"))
        );
        assert_eq!(first["persistence"], "failed");
        assert_eq!(first["persistence_uncertain"], false);
        if let Some(Block::Assistant { text, .. }) = session.transcript.block_mut(0) {
            text.push_str(" changed after install");
        }
        if let Some(Block::TurnEnd { saving, .. }) = session.transcript.block_mut(1) {
            *saving = Some(Saving::Uncertain);
        }
        let last = installed.row(&session);
        assert_ne!(first["parts"], last["parts"]);
        assert_eq!(last["persistence_uncertain"], true);
    }

    #[test]
    fn observations_expose_every_unexpected_non_preserved_request_channel_and_tool() {
        let mut session = Session::new(
            serde_json::from_value(crate::testing::session_info("s1")).unwrap(),
            1,
        );
        session.load = Some("load_1".into());
        let assistant = |request_index, text: &str, reasoning: &str| Block::Assistant {
            key: RequestKey {
                loop_id: "A".into(),
                request_index,
            },
            text: text.into(),
            reasoning: reasoning.into(),
            streaming: false,
            expanded: None,
        };
        session
            .transcript
            .push(assistant(0, "expected", "unexpected channel"));
        session
            .transcript
            .push(assistant(99, "unexpected request", "unexpected thinking"));
        session
            .transcript
            .push(Block::Tool(Box::new(crate::transcript::ToolCall::new(
                crate::protocol::ToolRef {
                    session_id: "s1".into(),
                    loop_id: "A".into(),
                    request_index: 99,
                    tool_call_id: "unexpected-tool".into(),
                },
                "bash",
            ))));
        session
            .transcript
            .push(assistant(100, "explicitly transient", ""));
        session
            .transcript
            .mark_preserved([crate::transcript::PreservedKey::Assistant(RequestKey {
                loop_id: "A".into(),
                request_index: 100,
            })]);
        session.transcript.push(Block::TurnEnd {
            loop_id: "A".into(),
            outcome: LoopOutcome::Completed,
            duration: None,
            requests: 1,
            tools: 1,
            usage: Default::default(),
            changes: None,
            saving: Some(Saving::Saved),
        });
        let installed = Installed {
            loop_id: "A".into(),
            seq: Some(1),
            input_id: None,
            result_id: "r1:example".into(),
            read_item_count: 3,
        };
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut observer = Observer {
            file: file.reopen().unwrap(),
            started: Instant::now(),
            seq: 0,
            bytes: 0,
            limits: TRACE_LIMITS,
        };
        observer.emit("canonical_install", installed.row(&session));
        let emitted: Value =
            serde_json::from_str(&std::fs::read_to_string(file.path()).unwrap()).unwrap();
        let parts = emitted["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 5);
        assert_eq!(parts[0]["channel"], "reasoning");
        assert_eq!(parts[2]["request_index"], 99);
        assert_eq!(parts[3]["request_index"], 99);
        assert_eq!(parts[4]["tool_call_id"], "unexpected-tool");
        assert!(parts.iter().all(|part| part["request_index"] != 100));
        session
            .transcript
            .mark_preserved([crate::transcript::PreservedKey::AssistantReasoning(
                RequestKey {
                    loop_id: "A".into(),
                    request_index: 0,
                },
            )]);
        let explicitly_marked = installed.parts(&session);
        assert_eq!(explicitly_marked.len(), 4);
        assert_eq!(
            explicitly_marked[0]["channel"], "text",
            "only an explicit transient channel marker may exclude the other channel"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_global_inventory_exposes_cross_region_body_swaps_and_unexpected_loops() {
        let (tx, _) = tokio::sync::mpsc::unbounded_channel();
        let (_fake, mut app) = crate::testing::FakeAgent::start(tx);
        let mut session = Session::new(
            serde_json::from_value(crate::testing::session_info("s1")).unwrap(),
            1,
        );
        session.load = Some("load_1".into());
        for (seq, id) in [(1, "A"), (2, "B")] {
            let at = session.transcript.push(Block::User {
                text: format!("prompt {id}"),
                kind: crate::protocol::read::UserKind::Prompt,
                at: None,
            });
            session
                .transcript
                .identify_user(at, id, Some(&format!("input-{id}")));
            session.transcript.push(Block::Assistant {
                key: RequestKey {
                    loop_id: id.into(),
                    request_index: 0,
                },
                reasoning: String::new(),
                text: format!("body {id}"),
                streaming: false,
                expanded: None,
            });
            session.transcript.push(Block::TurnEnd {
                loop_id: id.into(),
                outcome: LoopOutcome::Completed,
                duration: None,
                requests: 1,
                tools: 0,
                usage: Default::default(),
                changes: None,
                saving: Some(Saving::Saved),
            });
            session.observed_installs.push(Installed {
                loop_id: id.into(),
                seq: Some(seq),
                input_id: Some(format!("input-{id}")),
                result_id: format!("r1:{id}"),
                read_item_count: 2,
            });
        }
        let before: Vec<_> = session
            .observed_installs
            .iter()
            .map(|turn| turn.row(&session))
            .collect();
        let a = session.transcript.blocks()[1].clone();
        let b = session.transcript.blocks()[4].clone();
        *session.transcript.block_mut(1).unwrap() = b;
        *session.transcript.block_mut(4).unwrap() = a;
        let after: Vec<_> = session
            .observed_installs
            .iter()
            .map(|turn| turn.row(&session))
            .collect();
        assert_eq!(
            before, after,
            "per-loop hashes and TurnEnd order cannot detect this actual layout corruption"
        );
        session.transcript.push(Block::Assistant {
            key: RequestKey {
                loop_id: "uninstalled-loop".into(),
                request_index: 99,
            },
            reasoning: String::new(),
            text: "unexpected installed bytes".into(),
            streaming: false,
            expanded: None,
        });
        let at = session.transcript.push(Block::User {
            text: "steering bytes".into(),
            kind: crate::protocol::read::UserKind::Steering,
            at: None,
        });
        session.transcript.identify_user(at, "B", Some("steer-B"));
        app.session = Some(session);
        let file = tempfile::NamedTempFile::new().unwrap();
        app.start_observer(file.reopen().unwrap());
        app.observe_checkpoint(false);
        let rows: Vec<Value> = std::fs::read_to_string(file.path())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let inventory = rows
            .iter()
            .find(|row| row["event"] == "canonical_view")
            .unwrap()["inventory"]
            .as_array()
            .unwrap();
        let shape: Vec<_> = inventory
            .iter()
            .filter(|row| row["kind"] == "assistant" || row["kind"] == "turn_end")
            .map(|row| {
                (
                    row["kind"].as_str().unwrap(),
                    row["loop_id"].as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            shape,
            [
                ("assistant", "B"),
                ("turn_end", "A"),
                ("assistant", "A"),
                ("turn_end", "B"),
                ("assistant", "uninstalled-loop")
            ]
        );
        assert_eq!(inventory[0]["loop_id"], "A");
        assert_eq!(inventory[0]["input_id"], "input-A");
        let steer = inventory.last().unwrap();
        assert_eq!(steer["user_kind"], "steering");
        assert_eq!(steer["input_id"], "steer-B");
        assert_eq!(steer["body"], digest("steering bytes"));
        let trace = std::fs::read_to_string(file.path()).unwrap();
        assert!(!trace.contains("steering bytes") && !trace.contains("unexpected installed bytes"));
    }

    #[test]
    fn trace_producer_fails_closed_before_row_event_or_file_overflow() {
        for limits in [
            Limits {
                row: 8,
                ..TRACE_LIMITS
            },
            Limits {
                events: 0,
                ..TRACE_LIMITS
            },
            Limits {
                file: 8,
                ..TRACE_LIMITS
            },
        ] {
            let file = tempfile::NamedTempFile::new().unwrap();
            let mut observer = Observer {
                file: file.reopen().unwrap(),
                started: Instant::now(),
                seq: 0,
                bytes: 0,
                limits,
            };
            assert!(
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                    || observer.emit("observer_ready", json!({}))
                ))
                .is_err()
            );
            assert!(
                std::fs::read(file.path()).unwrap().is_empty(),
                "overflow must not produce a truncated success row"
            );
        }
    }
}
