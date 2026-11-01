//! Session mutations shared by terminal and phone adapters.

use serde_json::{Value, json};

use super::{Answer, App, InteractionKind, PendingInteraction, SessionOp};
use crate::protocol::{self, CompactionResult, RpcError, SessionInfo, SessionResult};
use crate::rpc::Client;

pub enum Operation {
    Update(Value),
    Rename(String),
    Compact(String),
    CancelTurn(String),
    CancelCompaction(String),
    Answer {
        loop_id: String,
        interaction_id: String,
        answer: Value,
        allow_tool: Option<String>,
    },
    Delete,
}

pub enum Outcome {
    Done,
    Info(SessionInfo),
    Compacted(CompactionResult),
    Answered(Option<String>),
    Deleted(String),
}

impl Operation {
    pub async fn execute(
        self,
        client: &Client,
        session_id: &str,
        load: Option<&str>,
    ) -> Result<Outcome, RpcError> {
        let (method, mut fields) = match &self {
            Self::Update(fields) => (protocol::METHOD_SESSION_UPDATE, fields.clone()),
            Self::Rename(title) => (
                protocol::METHOD_SESSION_RENAME,
                json!({"title":title.trim()}),
            ),
            Self::Compact(id) => (protocol::METHOD_SESSION_COMPACT, json!({"operation_id":id})),
            Self::CancelTurn(id) => (protocol::METHOD_TURN_CANCEL, json!({"loop_id":id})),
            Self::CancelCompaction(id) => (
                protocol::METHOD_SESSION_COMPACT_CANCEL,
                json!({"operation_id":id}),
            ),
            Self::Answer {
                loop_id,
                interaction_id,
                answer,
                ..
            } => (
                protocol::METHOD_INTERACTION_ANSWER,
                json!({"loop_id":loop_id,"interaction_id":interaction_id,"answer":answer}),
            ),
            Self::Delete => (protocol::METHOD_SESSION_DELETE, json!({})),
        };
        fields["session_id"] = json!(session_id);
        if let Some(load) = load {
            fields["load"] = json!(load);
        }
        match self {
            Self::Update(_) | Self::Rename(_) => {
                let result: SessionResult = client.call(method, &fields).await?;
                if result.session.session_id != session_id {
                    return Err(malformed("Session operation identity changed"));
                }
                Ok(Outcome::Info(result.session))
            }
            Self::Compact(operation_id) => {
                let result: CompactionResult = client.call(method, &fields).await?;
                if result.operation_id != operation_id {
                    return Err(malformed("Compaction operation identity changed"));
                }
                Ok(Outcome::Compacted(result))
            }
            Self::Delete => {
                if load.is_some() {
                    client
                        .call::<_, Value>(protocol::METHOD_SESSION_CLOSE, &fields)
                        .await?;
                }
                client
                    .call::<_, Value>(method, &json!({"session_id": session_id}))
                    .await?;
                Ok(Outcome::Deleted(session_id.to_owned()))
            }
            operation => {
                client.call::<_, Value>(method, &fields).await?;
                Ok(match operation {
                    Self::Answer { allow_tool, .. } => Outcome::Answered(allow_tool),
                    _ => Outcome::Done,
                })
            }
        }
    }
}

pub fn answer(
    interaction: &PendingInteraction,
    answer: Answer,
) -> Result<Option<Operation>, &'static str> {
    let mut allow_tool = None;
    let decision = |decision| json!({"type":"approval","decision":decision});
    let answer = match (answer, &interaction.kind) {
        (Answer::Text(text), InteractionKind::Text) => {
            if text.is_empty()
                || text.len() > super::session::TEXT_ANSWER_MAX_BYTES
                || text.chars().any(char::is_control)
            {
                return Err("An answer must contain 1–8192 UTF-8 bytes without control characters");
            }
            json!({"type":"text","text":text})
        }
        (Answer::Text(_), _) => return Err("This interaction does not accept text"),
        (Answer::Deny, InteractionKind::Text) => {
            return Ok(Some(Operation::CancelTurn(
                interaction.turn.loop_id.clone(),
            )));
        }
        (_, InteractionKind::Text) => return Ok(None),
        (Answer::Choice(index), InteractionKind::Choice(_)) => {
            json!({"type":"choice","index":index})
        }
        (Answer::AllowTool | Answer::Choice(2), InteractionKind::Approval) => {
            allow_tool = Some(interaction.tool_name.clone());
            decision("allow_once")
        }
        (Answer::Allow | Answer::AllowTool | Answer::Choice(0), _) => decision("allow_once"),
        (Answer::Deny | Answer::Choice(_), _) => decision("deny"),
    };
    Ok(Some(Operation::Answer {
        loop_id: interaction.turn.loop_id.clone(),
        interaction_id: interaction.interaction_id.clone(),
        answer,
        allow_tool,
    }))
}

fn malformed(message: &str) -> RpcError {
    RpcError::new(protocol::INVALID_PARAMS, "malformed_result", message)
}

impl App {
    pub(super) fn next_operation(&mut self) -> u64 {
        self.op_seq = self
            .op_seq
            .checked_add(1)
            .expect("operation identity exhausted");
        self.op_seq
    }

    pub(super) fn new_compaction(&mut self) -> Operation {
        Operation::Compact(format!("tui-{}", self.next_operation()))
    }

    pub(super) fn operate(&self, epoch: u64, operation: Operation) {
        let session = self
            .resident
            .get(epoch)
            .expect("a resident operation target");
        let (id, load) = (session.info.session_id.clone(), session.load.clone());
        let client = self.client.clone();
        self.spawn_for(epoch, async move {
            SessionOp::Operated(operation.execute(&client, &id, Some(&load)).await)
        });
    }

    pub(super) fn apply_operation(&mut self, epoch: Option<u64>, outcome: Outcome) {
        if epoch.is_some_and(|epoch| self.resident.get(epoch).is_none()) {
            return;
        }
        match outcome {
            Outcome::Deleted(id) => self.on_msg(super::Msg::Deleted(id, Ok(()))),
            Outcome::Info(info) => {
                if let Some(epoch) = epoch {
                    self.session_mut(epoch).info = info.clone();
                }
                self.merge_catalog_info(info);
            }
            Outcome::Compacted(result) => {
                if let Some(epoch) = epoch {
                    self.on_compacted(epoch, result);
                }
            }
            Outcome::Answered(Some(tool)) => {
                if let Some(session) = epoch.and_then(|epoch| self.resident.get_mut(epoch)) {
                    session.allowed_tools.insert(tool);
                }
            }
            Outcome::Answered(None) | Outcome::Done => {}
        }
    }
}
