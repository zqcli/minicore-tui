//! A loopback provider and one named tool double around production resource policy.

use std::collections::BTreeMap;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use minicore_host_core::{Host, HostConfig, OpenAiModelApi, OpenAiModelSettings};
use minicore_host_protocol::{
    AgentConfig, AgentCreate, ApprovalMode, CallOptions, Fault, OperationId, ReasoningPreference,
    SessionRef, StorageMode, ToolBinding,
};
use minicore_host_provider::SecretApiKey;
use minicore_host_resources::{HostCredentials, HostResourceBuilder};
use minicore_host_runtime::tools::{
    Tool, ToolContext, ToolExecutionOutcome, ToolFuture, ToolInputAnswerKind, ToolInputRequest,
    ToolInvocation, ToolSpec,
};
use minicore_host_tools::{ToolFactory, ToolRegistry, TurnPort};
use minicore_host_transport::HostClient;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::mpsc,
};

use super::{NativeHost, real_bounded};

#[derive(Clone)]
struct InputToolDouble {
    spec: ToolSpec,
    calls: Arc<AtomicUsize>,
}
impl ToolFactory for InputToolDouble {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }
    fn bind(&self, _: Arc<dyn TurnPort>) -> Result<Arc<dyn Tool>, Fault> {
        Ok(Arc::new(self.clone()))
    }
}
impl Tool for InputToolDouble {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }
    fn execute<'a>(&'a self, _: ToolInvocation, _: ToolContext) -> ToolFuture<'a> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            Ok(ToolExecutionOutcome::RequestInput(
                ToolInputRequest::new(
                    "Enter a fixture answer",
                    Vec::new(),
                    ToolInputAnswerKind::Text,
                )
                .unwrap(),
            ))
        })
    }
}

pub struct ToolProvider {
    requests: mpsc::Receiver<Value>,
    task: Option<tokio::task::JoinHandle<()>>,
    calls: Arc<AtomicUsize>,
}
impl ToolProvider {
    pub fn executions(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
    pub async fn request(&mut self) -> Value {
        real_bounded(self.requests.recv())
            .await
            .expect("provider received a real request")
    }
    pub async fn finish(mut self) {
        real_bounded(self.task.take().unwrap()).await.unwrap();
    }
}
impl Drop for ToolProvider {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

pub async fn with_input_tool() -> (NativeHost, ToolProvider) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/chat/completions", listener.local_addr().unwrap());
    let (requests, incoming) = mpsc::channel(2);
    let task = tokio::spawn(async move {
        for body in [tool_response(), final_response()] {
            let (mut socket, _) = listener.accept().await.unwrap();
            let request = read_request(&mut socket).await;
            requests.send(request).await.unwrap();
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            socket.write_all(head.as_bytes()).await.unwrap();
            socket.write_all(body.as_bytes()).await.unwrap();
            socket.shutdown().await.unwrap();
        }
    });
    let calls = Arc::new(AtomicUsize::new(0));
    let mut registry = ToolRegistry::new();
    registry
        .register(Arc::new(InputToolDouble {
            spec: ToolSpec::new(
                "collect_input".parse().unwrap(),
                "Request a fixture answer",
                json!({"type":"object","properties":{},"additionalProperties":false}),
            )
            .unwrap(),
            calls: calls.clone(),
        }))
        .unwrap();
    let selection = vec![ToolBinding {
        name: "collect_input".into(),
    }];
    let native = configured_host(endpoint, registry, selection).await;
    (
        native,
        ToolProvider {
            requests: incoming,
            task: Some(task),
            calls,
        },
    )
}

pub async fn with_provider_error(message: &str, streaming: bool) -> (NativeHost, ToolProvider) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/chat/completions", listener.local_addr().unwrap());
    let (requests, incoming) = mpsc::channel(1);
    let error = json!({"error":{"type":"invalid_request_error","code":"invalid_request","message":message}});
    let (status, content_type, body) = if streaming {
        ("200 OK", "text/event-stream", format!("data: {error}\n\n"))
    } else {
        ("400 Bad Request", "application/json", error.to_string())
    };
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        requests
            .send(read_request(&mut socket).await)
            .await
            .unwrap();
        let head = format!(
            "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        );
        socket.write_all(head.as_bytes()).await.unwrap();
        socket.write_all(body.as_bytes()).await.unwrap();
        socket.shutdown().await.unwrap();
    });
    let native = configured_host(endpoint, ToolRegistry::new(), Vec::new()).await;
    (
        native,
        ToolProvider {
            requests: incoming,
            task: Some(task),
            calls: Arc::new(AtomicUsize::new(0)),
        },
    )
}

async fn configured_host(
    endpoint: String,
    registry: ToolRegistry,
    selection: Vec<ToolBinding>,
) -> NativeHost {
    let model = "fixture/interaction".parse().unwrap();
    let config = HostConfig {
        models: vec![OpenAiModelSettings {
            model,
            api: OpenAiModelApi::ChatCompletions,
            endpoint,
            provider_model: "interaction-fixture".into(),
            api_key_env: "MINICORE_UNUSED_FIXTURE_KEY".into(),
            context_window: 128_000,
            supported_reasoning: vec![ReasoningPreference::Auto],
            supports_tools: true,
            request_timeout_ms: None,
        }],
        tools: selection.clone(),
        ..HostConfig::default()
    };
    let credentials = HostCredentials {
        model_keys: BTreeMap::from([(
            config.models[0].model.clone(),
            SecretApiKey::new("loopback-fixture-value").unwrap(),
        )]),
        network_access_key: None,
    };
    let prepared = HostResourceBuilder::new(Arc::new(registry))
        .prepare_with_credentials(config, credentials)
        .await
        .unwrap();
    let host = Host::memory_configured(prepared).unwrap();
    let client = HostClient::in_process(host.clone()).unwrap();
    let mut config = AgentConfig::default();
    config.session_defaults.model = "fixture/interaction".parse().unwrap();
    config.session_defaults.tools = selection;
    config.session_defaults.execution.approval = ApprovalMode::Ask;
    config.session_defaults.execution.compaction.enabled = false;
    let receipt = client
        .create_agent(
            AgentCreate {
                operation_id: OperationId::new().unwrap(),
                name: "Native interaction fixture".into(),
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
    let (_, unused_calls) = mpsc::unbounded_channel();
    NativeHost {
        host,
        client,
        target,
        calls: unused_calls,
    }
}

async fn read_request(socket: &mut TcpStream) -> Value {
    let mut bytes = Vec::new();
    let boundary = loop {
        if let Some(at) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            break at + 4;
        }
        let mut chunk = [0; 4096];
        let count = socket.read(&mut chunk).await.unwrap();
        assert_ne!(count, 0);
        bytes.extend_from_slice(&chunk[..count]);
        assert!(
            bytes.len() < 1_000_000,
            "fixture request header exceeded its bound"
        );
    };
    let head = std::str::from_utf8(&bytes[..boundary]).unwrap();
    assert!(head.starts_with("POST /chat/completions HTTP/1.1\r\n"));
    let length: usize = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().unwrap())
        })
        .unwrap();
    assert!(length < 1_000_000, "fixture request exceeded its bound");
    while bytes.len() < boundary + length {
        let mut chunk = [0; 4096];
        let count = socket.read(&mut chunk).await.unwrap();
        assert_ne!(count, 0);
        bytes.extend_from_slice(&chunk[..count]);
    }
    serde_json::from_slice(&bytes[boundary..boundary + length]).unwrap()
}

fn tool_response() -> String {
    format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_fixture","type":"function","function":{"name":"collect_input","arguments":"{}"}}]},"finish_reason":null}]}),
        json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]})
    )
}
fn final_response() -> String {
    format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"index":0,"delta":{"content":"interaction completed"},"finish_reason":null}]}),
        json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]})
    )
}
