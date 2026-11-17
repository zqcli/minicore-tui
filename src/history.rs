//! Fixed-window native history reads and complete streaming exports.

use std::io::Write;

use minicore_host_core::{HistoryRecordData, SessionHistorySnapshot};
use minicore_host_protocol::{
    CallError, CallOptions, HistoryDirection, HistoryReadRequest, SessionIdentity,
};
use minicore_host_transport::SessionClient;

#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    #[error("{0}")]
    Host(#[from] CallError),
    #[error("history pagination: {0}")]
    Invalid(String),
    #[error("history export: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Clone, Debug)]
pub struct HistoryWindow {
    pub source: SessionIdentity,
    pub upper_bound: u64,
    pub next_cursor: Option<String>,
    pub items: Vec<HistoryRecordData>,
    pub lower_bound: u64,
    boundary: u64,
    direction: HistoryDirection,
}

impl HistoryWindow {
    pub async fn tail(client: &SessionClient, limit: u64) -> Result<Self, HistoryError> {
        Self::open(client, limit, HistoryDirection::Backward).await
    }

    pub async fn forward(client: &SessionClient, limit: u64) -> Result<Self, HistoryError> {
        Self::open(client, limit, HistoryDirection::Forward).await
    }

    async fn open(
        client: &SessionClient,
        limit: u64,
        direction: HistoryDirection,
    ) -> Result<Self, HistoryError> {
        let page = client
            .history(
                HistoryReadRequest {
                    cursor: None,
                    limit,
                    direction,
                },
                CallOptions::default(),
            )
            .await?;
        if page.source.agent_id != client.target().agent_id
            || page.source.session_id != client.target().session_id
        {
            return Err(HistoryError::Invalid(
                "page belongs to another Session".into(),
            ));
        }
        Self::from_page(page, direction)
    }

    fn from_page(
        page: SessionHistorySnapshot,
        direction: HistoryDirection,
    ) -> Result<Self, HistoryError> {
        let boundary = match direction {
            HistoryDirection::Forward => 0,
            HistoryDirection::Backward => page.data.upper_bound,
        };
        let mut window = Self {
            source: page.source.clone(),
            upper_bound: page.data.upper_bound,
            next_cursor: None,
            items: Vec::new(),
            lower_bound: boundary,
            boundary,
            direction,
        };
        window.items = window.accept_page(page)?;
        Ok(window)
    }

    pub fn has_more(&self) -> bool {
        self.next_cursor.is_some()
    }

    pub async fn older(
        &mut self,
        client: &SessionClient,
        limit: u64,
    ) -> Result<Vec<HistoryRecordData>, HistoryError> {
        if self.direction != HistoryDirection::Backward {
            return Err(HistoryError::Invalid(
                "older requires a backward window".into(),
            ));
        }
        self.next_page(client, limit).await
    }

    pub async fn next_page(
        &mut self,
        client: &SessionClient,
        limit: u64,
    ) -> Result<Vec<HistoryRecordData>, HistoryError> {
        let Some(cursor) = self.next_cursor.clone() else {
            return Ok(Vec::new());
        };
        let page = client
            .history(
                HistoryReadRequest {
                    cursor: Some(cursor),
                    limit,
                    direction: self.direction,
                },
                CallOptions::default(),
            )
            .await?;
        self.accept_page(page)
    }

    fn accept_page(
        &mut self,
        page: SessionHistorySnapshot,
    ) -> Result<Vec<HistoryRecordData>, HistoryError> {
        if page.source != self.source || page.data.upper_bound != self.upper_bound {
            return Err(HistoryError::Invalid(
                "page changed its Session, incarnation or fixed upper bound".into(),
            ));
        }
        let count = page.data.items.len() as u64;
        let start = match self.direction {
            HistoryDirection::Forward => self.boundary,
            HistoryDirection::Backward => self.boundary.checked_sub(count).ok_or_else(|| {
                HistoryError::Invalid("backward page exceeds its boundary".into())
            })?,
        };
        let end = start
            .checked_add(count)
            .ok_or_else(|| HistoryError::Invalid("page position overflow".into()))?;
        if end > self.upper_bound
            || page
                .data
                .items
                .iter()
                .enumerate()
                .any(|(offset, record)| record.position != start + offset as u64)
        {
            return Err(HistoryError::Invalid(
                "page does not cover its contiguous ascending native positions".into(),
            ));
        }
        let next_boundary = match self.direction {
            HistoryDirection::Forward => end,
            HistoryDirection::Backward => start,
        };
        let more = match self.direction {
            HistoryDirection::Forward => end < self.upper_bound,
            HistoryDirection::Backward => start > 0,
        };
        if page.data.next_cursor.is_some() != more || (more && count == 0) {
            return Err(HistoryError::Invalid(
                "cursor does not cover the remaining fixed window".into(),
            ));
        }
        if page.data.next_cursor.is_some() && page.data.next_cursor == self.next_cursor {
            return Err(HistoryError::Invalid("cursor did not advance".into()));
        }
        self.boundary = next_boundary;
        self.lower_bound = self.lower_bound.min(start);
        self.next_cursor = page.data.next_cursor;
        Ok(page.data.items)
    }
}

#[derive(Clone, Debug)]
pub struct ExportSummary {
    pub source: SessionIdentity,
    pub upper_bound: u64,
    pub records: u64,
}

pub async fn export_markdown<W: Write>(
    client: &SessionClient,
    writer: &mut W,
    page_size: u64,
) -> Result<ExportSummary, HistoryError> {
    let mut window = HistoryWindow::forward(client, page_size).await?;
    let mut records = 0;
    let mut page = std::mem::take(&mut window.items);
    loop {
        crate::transcript::write_markdown(writer, &page)?;
        records += page.len() as u64;
        if !window.has_more() {
            break;
        }
        page = window.next_page(client, page_size).await?;
    }
    writer.flush()?;
    Ok(ExportSummary {
        source: window.source,
        upper_bound: window.upper_bound,
        records,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use minicore_host_core::HistoryItemData;
    use minicore_host_protocol::{HistoryPageData, Timestamp};

    fn source() -> SessionIdentity {
        SessionIdentity {
            host_id: "00000000000000000001".parse().unwrap(),
            host_incarnation: "00000000000000000002".parse().unwrap(),
            agent_id: "00000000000000000003".parse().unwrap(),
            session_id: "00000000000000000004".parse().unwrap(),
        }
    }

    fn page(
        range: std::ops::Range<u64>,
        upper_bound: u64,
        cursor: Option<&str>,
    ) -> SessionHistorySnapshot {
        SessionHistorySnapshot {
            source: source(),
            data: HistoryPageData {
                items: range
                    .map(|position| HistoryRecordData {
                        delegations: Vec::new(),
                        position,
                        recorded_at: Timestamp::from_unix_milliseconds(position as i64).unwrap(),
                        turn_id: None,
                        response_index: None,
                        item: HistoryItemData::Summary {
                            text: format!("summary {position}"),
                        },
                    })
                    .collect(),
                upper_bound,
                next_cursor: cursor.map(str::to_owned),
            },
        }
    }

    #[test]
    fn backward_pages_preserve_ascending_order_and_the_original_upper_bound() {
        let mut window =
            HistoryWindow::from_page(page(4..6, 6, Some("older-4")), HistoryDirection::Backward)
                .unwrap();
        assert_eq!(
            window
                .items
                .iter()
                .map(|row| row.position)
                .collect::<Vec<_>>(),
            [4, 5]
        );
        assert!(window.accept_page(page(2..4, 8, Some("older-2"))).is_err());
        let older = window.accept_page(page(2..4, 6, Some("older-2"))).unwrap();
        assert_eq!(
            older.iter().map(|row| row.position).collect::<Vec<_>>(),
            [2, 3]
        );
        assert_eq!(window.upper_bound, 6);
        window.accept_page(page(0..2, 6, None)).unwrap();
        assert!(!window.has_more());
    }

    #[test]
    fn a_gap_or_reversed_page_is_not_accepted_as_coverage() {
        let mut window =
            HistoryWindow::from_page(page(0..2, 6, Some("next-2")), HistoryDirection::Forward)
                .unwrap();
        assert!(window.accept_page(page(3..5, 6, Some("next-5"))).is_err());
        let mut reversed = page(2..4, 6, Some("next-4"));
        reversed.data.items.reverse();
        assert!(window.accept_page(reversed).is_err());
        assert_eq!(window.boundary, 2);
    }

    #[test]
    fn sdk_output_buffer_recovers_unicode_gaps_before_claiming_completion() {
        use minicore_host_core::{
            ActivityEvent, ModelFinishedData, ModelResponseData, ModelTextSnapshot,
            ObservationEvent,
        };
        use minicore_host_protocol::{
            ChannelPosition, ModelOutputIdentity, OutputDelta, OutputSource, TextRangeData,
            TurnIdentity,
        };
        use minicore_host_runtime::model::{ModelFinishReason, Usage};
        use minicore_host_transport::OutputBuffer;
        let identity = ModelOutputIdentity {
            turn: TurnIdentity {
                session: source(),
                turn_id: "00000000000000000005".parse().unwrap(),
                load_id: None,
            },
            response_index: 7,
        };
        let mut output = OutputBuffer::new(
            OutputSource::Model {
                source: identity.clone(),
            },
            "text".into(),
        );
        output
            .event(&ObservationEvent::Activity(ActivityEvent::ModelOutput {
                source: identity.clone(),
                data: OutputDelta {
                    channel: "text".into(),
                    start: 4,
                    end: 8,
                    text: "🙂".into(),
                },
            }))
            .unwrap();
        output
            .event(&ObservationEvent::Activity(ActivityEvent::ModelFinished {
                source: identity.clone(),
                data: ModelFinishedData {
                    history_position: 4,
                    recorded_at: Timestamp::from_unix_milliseconds(0).unwrap(),
                    result: ModelResponseData {
                        content: Vec::new(),
                        finish_reason: ModelFinishReason::Stop,
                        usage: Usage::default(),
                    },
                    output_positions: vec![ChannelPosition {
                        channel: "text".into(),
                        end: 8,
                    }],
                },
            }))
            .unwrap();
        assert!(!output.is_complete());
        let gap = output.next_read(4).unwrap();
        assert_eq!((gap.start, gap.end), (0, 4));
        output
            .model_read(&ModelTextSnapshot {
                source: identity.clone(),
                data: TextRangeData {
                    channel: "text".into(),
                    start: 0,
                    end: 4,
                    text: "αβ".into(),
                    has_more: false,
                },
            })
            .unwrap();
        output
            .model_read(&ModelTextSnapshot {
                source: identity,
                data: TextRangeData {
                    channel: "text".into(),
                    start: 2,
                    end: 8,
                    text: "β🙂".into(),
                    has_more: false,
                },
            })
            .unwrap();
        assert_eq!(output.text(), "αβ🙂");
        assert!(output.is_complete());
    }

    struct ScriptedModel(minicore_host_runtime::model::ModelDescriptor);
    impl minicore_host_runtime::model::Model for ScriptedModel {
        fn descriptor(&self) -> &minicore_host_runtime::model::ModelDescriptor {
            &self.0
        }
        fn start<'a>(
            &'a self,
            _: minicore_host_runtime::model::ModelRequest,
            _: minicore_host_runtime::model::ModelCallContext,
        ) -> minicore_host_runtime::model::ModelStartFuture<'a> {
            use minicore_host_runtime::model::{ModelEvent, ModelFinishReason, ModelStream};
            Box::pin(async {
                Ok(Box::pin(futures_util::stream::iter([
                    Ok(ModelEvent::text_delta("scripted native answer αβ🙂")),
                    Ok(ModelEvent::Finish {
                        reason: ModelFinishReason::Stop,
                    }),
                ])) as ModelStream)
            })
        }
    }

    struct ScriptedResources(std::sync::Arc<ScriptedModel>);
    impl minicore_host_core::ResourceResolver for ScriptedResources {
        fn validate(
            &self,
            _: &minicore_host_protocol::SessionConfig,
        ) -> Result<(), minicore_host_protocol::Fault> {
            Ok(())
        }
        fn resolve<'a>(
            &'a self,
            _: &'a minicore_host_protocol::SessionConfig,
        ) -> minicore_host_core::ResourceFuture<'a> {
            use minicore_host_runtime::{
                ExecutionConfig, ExecutionOptions, model::ReasoningPreference,
                prompt::DefaultPromptProvider, tools::ToolSet,
            };
            Box::pin(async {
                Ok(minicore_host_core::BoundResources {
                    execution: ExecutionConfig::new(
                        self.0.clone(),
                        ReasoningPreference::Auto,
                        ToolSet::default(),
                        None,
                        std::sync::Arc::new(DefaultPromptProvider::new(None)),
                    )
                    .unwrap(),
                    options: ExecutionOptions::default(),
                })
            })
        }
    }

    async fn send_and_finish(
        session: &SessionClient,
        subscription: &mut minicore_host_transport::ClientSubscription,
        text: &str,
    ) {
        use minicore_host_core::{ActivityEvent, ObservationEvent, ObservationNotification};
        use minicore_host_protocol::{MessageId, MessageInput, MessageMode};
        session
            .send(
                MessageInput {
                    message_id: MessageId::new().unwrap(),
                    mode: MessageMode::FollowUp,
                    text: text.into(),
                    in_reply_to: None,
                },
                CallOptions::default(),
            )
            .await
            .unwrap();
        loop {
            let event = subscription.recv().await.unwrap().unwrap();
            if matches!(event, ObservationNotification::Event(event) if matches!(event.event, ObservationEvent::Activity(ActivityEvent::TurnFinished { .. })))
            {
                break;
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn real_in_process_history_window_and_export_use_native_records() {
        use minicore_host_core::Host;
        use minicore_host_protocol::{
            AgentConfig, AgentCreate, EventCategory, OperationId, SessionRef, StorageMode,
            SubscribeRequest, SubscriptionScope,
        };
        use minicore_host_runtime::model::{ModelDescriptor, ReasoningPreference};
        use minicore_host_transport::HostClient;
        use std::{collections::BTreeSet, sync::Arc};
        // Only the model/resource provider is a named test double; Host and SDK are real.
        let model = ScriptedModel(
            ModelDescriptor::new(
                "fixture/model".parse().unwrap(),
                128_000,
                BTreeSet::from([ReasoningPreference::Auto]),
                true,
            )
            .unwrap(),
        );
        let host = Host::memory(Arc::new(ScriptedResources(Arc::new(model)))).unwrap();
        let client = HostClient::in_process(host).unwrap();
        let mut config = AgentConfig::default();
        config.session_defaults.model = "fixture/model".parse().unwrap();
        let created = client
            .create_agent(
                AgentCreate {
                    operation_id: OperationId::new().unwrap(),
                    name: "history fixture".into(),
                    mode: StorageMode::Memory,
                    config,
                    default_session: None,
                    initial_message: None,
                },
                CallOptions::default(),
            )
            .await
            .unwrap();
        let target = SessionRef {
            agent_id: created.agent_id.unwrap(),
            session_id: created.session_id.unwrap(),
        };
        let session = client.session(target.clone());
        let mut subscription = client
            .subscribe(
                SubscribeRequest {
                    scope: SubscriptionScope::Session {
                        agent_id: target.agent_id,
                        session_id: target.session_id,
                    },
                    categories: BTreeSet::from([EventCategory::Completion]),
                },
                CallOptions::default(),
            )
            .await
            .unwrap();
        send_and_finish(&session, &mut subscription, "first input").await;
        let mut tail = HistoryWindow::tail(&session, 2).await.unwrap();
        assert_eq!(tail.upper_bound, 3);
        assert_eq!(
            tail.items
                .iter()
                .map(|record| record.position)
                .collect::<Vec<_>>(),
            [1, 2]
        );
        send_and_finish(&session, &mut subscription, "second input").await;
        let older = tail.older(&session, 2).await.unwrap();
        assert_eq!(
            older
                .iter()
                .map(|record| record.position)
                .collect::<Vec<_>>(),
            [0]
        );
        assert_eq!(tail.upper_bound, 3);
        assert!(!tail.has_more());
        let mut exported = Vec::new();
        let summary = export_markdown(&session, &mut exported, 1).await.unwrap();
        let markdown = String::from_utf8(exported).unwrap();
        assert_eq!((summary.records, summary.upper_bound), (6, 6));
        assert!(markdown.contains("first input"));
        assert!(markdown.contains("second input"));
        assert_eq!(markdown.matches("scripted native answer αβ🙂").count(), 2);
    }
}
