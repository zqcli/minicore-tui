//! Real embedded Host fixtures with an explicitly controlled model provider.

pub(crate) mod production;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use minicore_host_core::{BoundResources, Host, ResourceFuture, ResourceResolver};
use minicore_host_protocol::{
    AgentConfig, AgentCreate, CallOptions, Fault, HostLifecycle, HostShutdownRequest,
    HostShutdownStrategy, OperationId, SessionConfig, SessionRef, StorageMode,
};
use minicore_host_runtime::{
    ExecutionConfig, ExecutionOptions,
    model::{
        Model, ModelCallContext, ModelDescriptor, ModelError, ModelEvent, ModelFinishReason,
        ModelMessage, ModelRequest, ModelStartFuture, ModelStream, ReasoningPreference,
    },
    prompt::DefaultPromptProvider,
    tools::ToolSet,
};
use minicore_host_transport::HostClient;
use tokio::sync::{mpsc, oneshot};

/// Bounds a test wait even when Tokio's clock is paused or kept runnable.
pub async fn real_bounded<T>(work: impl Future<Output = T>) -> T {
    let (cancel, cancelled) = std::sync::mpsc::channel::<()>();
    let (expired, timeout) = oneshot::channel();
    std::thread::spawn(move || {
        if matches!(
            cancelled.recv_timeout(Duration::from_secs(5)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ) {
            let _ = expired.send(());
        }
    });
    let result = tokio::select! {
        result = work => result,
        _ = timeout => panic!("test wait exceeded five real seconds"),
    };
    drop(cancel);
    result
}

pub struct ModelCall {
    pub context: ModelCallContext,
    request: ModelRequest,
    events: mpsc::UnboundedSender<Result<ModelEvent, ModelError>>,
}

impl ModelCall {
    pub fn users(&self) -> Vec<&str> {
        self.request
            .messages()
            .iter()
            .filter_map(|message| match message {
                ModelMessage::User(text) => Some(text.as_ref()),
                _ => None,
            })
            .collect()
    }
    pub fn text(&self, text: &str) {
        self.events.send(Ok(ModelEvent::text_delta(text))).unwrap();
    }
    pub fn finish(self) {
        self.events
            .send(Ok(ModelEvent::Finish {
                reason: ModelFinishReason::Stop,
            }))
            .unwrap();
    }
}

struct GatedModelDouble {
    descriptor: ModelDescriptor,
    calls: mpsc::UnboundedSender<ModelCall>,
}
impl Model for GatedModelDouble {
    fn descriptor(&self) -> &ModelDescriptor {
        &self.descriptor
    }
    fn start<'a>(
        &'a self,
        request: ModelRequest,
        context: ModelCallContext,
    ) -> ModelStartFuture<'a> {
        Box::pin(async move {
            let (events, incoming) = mpsc::unbounded_channel();
            self.calls
                .send(ModelCall {
                    context,
                    request,
                    events,
                })
                .unwrap_or_else(|_| panic!("model fixture owner dropped before shutdown"));
            Ok(Box::pin(futures_util::stream::unfold(
                incoming,
                |mut incoming| async { incoming.recv().await.map(|event| (event, incoming)) },
            )) as ModelStream)
        })
    }
}

struct TestResourcesDouble(Arc<GatedModelDouble>);
impl ResourceResolver for TestResourcesDouble {
    fn validate(&self, _: &SessionConfig) -> Result<(), Fault> {
        Ok(())
    }
    fn resolve<'a>(&'a self, _: &'a SessionConfig) -> ResourceFuture<'a> {
        Box::pin(async {
            Ok(BoundResources {
                execution: ExecutionConfig::new(
                    self.0.clone(),
                    ReasoningPreference::Auto,
                    ToolSet::default(),
                    None,
                    Arc::new(DefaultPromptProvider::new(None)),
                )
                .unwrap(),
                options: ExecutionOptions::default(),
            })
        })
    }
}

pub struct NativeHost {
    host: Arc<Host>,
    pub client: HostClient,
    pub target: SessionRef,
    pub calls: mpsc::UnboundedReceiver<ModelCall>,
}

impl NativeHost {
    pub async fn new() -> Self {
        let (requests, calls) = mpsc::unbounded_channel();
        let model = GatedModelDouble {
            descriptor: ModelDescriptor::new(
                "fixture/gated".parse().unwrap(),
                128_000,
                BTreeSet::from([ReasoningPreference::Auto]),
                true,
            )
            .unwrap(),
            calls: requests,
        };
        let host = Host::memory(Arc::new(TestResourcesDouble(Arc::new(model)))).unwrap();
        let client = HostClient::in_process(Arc::clone(&host)).unwrap();
        let mut config = AgentConfig::default();
        config.session_defaults.model = "fixture/gated".parse().unwrap();
        let receipt = client
            .create_agent(
                AgentCreate {
                    operation_id: OperationId::new().unwrap(),
                    name: "Native terminal fixture".into(),
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
            agent_id: receipt.agent_id.unwrap(),
            session_id: receipt.session_id.unwrap(),
        };
        Self {
            host,
            client,
            target,
            calls,
        }
    }

    pub async fn finish(self) {
        let control = self.host.process_control();
        control
            .shutdown(HostShutdownRequest {
                operation_id: OperationId::new().unwrap(),
                strategy: HostShutdownStrategy::Graceful,
            })
            .unwrap();
        real_bounded(async {
            let mut snapshot = control.snapshot().unwrap();
            while snapshot.lifecycle != HostLifecycle::Stopped {
                assert!(
                    snapshot.shutdown_fault.is_none(),
                    "{:?}",
                    snapshot.shutdown_fault
                );
                snapshot = control.changed(&snapshot).await.unwrap();
            }
        })
        .await;
        self.client.close();
    }
}
