use minicore_host_core::{HistoryItemData, InteractionRequestData};
use minicore_host_protocol::TurnOutcome;
use minicore_host_runtime::tools::ToolResultOutcome;

use super::harness::Harness;
use crate::app::{Action, Answer};
use crate::history::HistoryWindow;
use crate::testing::{production::with_input_tool, real_bounded};
use crate::transcript::{Block, ToolState};

fn completed(app: &crate::app::App) -> bool {
    app.resident.current().is_some_and(|session| {
        !session.is_busy()
            && session.interaction.is_none()
            && session.transcript.blocks().iter().any(|block| {
                matches!(
                    block,
                    Block::TurnEnd {
                        outcome: TurnOutcome::Completed,
                        ..
                    }
                )
            })
    })
}

// The loopback socket uses real I/O; correctness is gated by native observations.
#[tokio::test]
async fn native_approval_then_tool_input_answer_reaches_the_model_and_history() {
    let (native, mut provider) = real_bounded(with_input_tool()).await;
    let mut h = Harness::from_native(native).await;
    h.app.composer.set_text("request interactive fixture tool");
    h.app.on_action(Action::Submit { follow: false });
    h.until(|app| {
        app.resident
            .current()
            .unwrap()
            .interaction
            .as_ref()
            .is_some_and(|interaction| {
                matches!(
                    interaction.request(),
                    InteractionRequestData::Approval { .. }
                )
            })
    })
    .await;
    assert_eq!(
        provider.executions(),
        0,
        "Ask must gate actual tool execution"
    );
    let approval = h
        .app
        .resident
        .current()
        .unwrap()
        .interaction
        .as_ref()
        .unwrap()
        .data
        .interaction_id
        .clone();
    let request = provider.request().await;
    assert_eq!(request["tools"][0]["function"]["name"], "collect_input");
    h.app.on_action(Action::Answer(Answer::Allow));
    h.until(|app| {
        app.resident
            .current()
            .unwrap()
            .interaction
            .as_ref()
            .is_some_and(|interaction| {
                matches!(
                    interaction.request(),
                    InteractionRequestData::ToolInput { .. }
                )
            })
    })
    .await;
    assert_eq!(provider.executions(), 1);
    let input = h
        .app
        .resident
        .current()
        .unwrap()
        .interaction
        .as_ref()
        .unwrap();
    assert_ne!(input.data.interaction_id, approval);
    assert_eq!(input.prompt(), "Enter a fixture answer");
    let typed = "typed fixture answer αβ🙂";
    h.app.on_action(Action::Answer(Answer::Text(typed.into())));
    h.until(completed).await;
    let request = provider.request().await;
    let forwarded = request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "tool")
        .unwrap();
    let payload: serde_json::Value =
        serde_json::from_str(forwarded["content"].as_str().unwrap()).unwrap();
    assert_eq!(payload["answer"], typed);
    let session = h.native.client.session(h.native.target.clone());
    let history = HistoryWindow::tail(&session, 60).await.unwrap();
    let results = history
        .items
        .iter()
        .filter_map(|record| match &record.item {
            HistoryItemData::ToolResult { result, .. } => Some(result),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].outcome, ToolResultOutcome::InputProvided);
    assert_eq!(
        results[0].output.content().as_str(),
        forwarded["content"].as_str().unwrap()
    );
    assert!(
        session
            .state(Default::default())
            .await
            .unwrap()
            .data
            .pending_interactions
            .is_empty()
    );
    let tools = h
        .app
        .resident
        .current()
        .unwrap()
        .transcript
        .blocks()
        .iter()
        .filter_map(|block| match block {
            Block::Tool(tool) => Some(tool),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].state, ToolState::Succeeded);
    assert!(tools[0].output_content().join("\n").contains(typed));
    h.finish().await;
    provider.finish().await;
}

#[tokio::test]
async fn denying_native_approval_never_executes_the_tool_or_opens_tool_input() {
    let (native, mut provider) = real_bounded(with_input_tool()).await;
    let mut h = Harness::from_native(native).await;
    h.app.composer.set_text("deny interactive fixture tool");
    h.app.on_action(Action::Submit { follow: false });
    h.until(|app| {
        app.resident
            .current()
            .unwrap()
            .interaction
            .as_ref()
            .is_some_and(|interaction| {
                matches!(
                    interaction.request(),
                    InteractionRequestData::Approval { .. }
                )
            })
    })
    .await;
    assert_eq!(provider.executions(), 0);
    provider.request().await;
    h.app.on_action(Action::Answer(Answer::Deny));
    h.until(completed).await;
    assert_eq!(provider.executions(), 0);
    let request = provider.request().await;
    let forwarded = request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "tool")
        .unwrap();
    let session = h.native.client.session(h.native.target.clone());
    let history = HistoryWindow::tail(&session, 60).await.unwrap();
    let result = history
        .items
        .iter()
        .find_map(|record| match &record.item {
            HistoryItemData::ToolResult { result, .. } => Some(result),
            _ => None,
        })
        .unwrap();
    assert_eq!(result.outcome, ToolResultOutcome::Denied);
    assert_eq!(
        result.output.content().as_str(),
        forwarded["content"].as_str().unwrap()
    );
    assert!(
        session
            .state(Default::default())
            .await
            .unwrap()
            .data
            .pending_interactions
            .is_empty()
    );
    assert!(
        h.app
            .resident
            .current()
            .unwrap()
            .transcript
            .blocks()
            .iter()
            .any(|block| { matches!(block, Block::Tool(tool) if tool.state == ToolState::Denied) })
    );
    h.finish().await;
    provider.finish().await;
}
