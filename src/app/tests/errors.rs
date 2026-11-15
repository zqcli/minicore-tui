use crossterm::event::KeyCode;
use minicore_host_core::HistoryItemData;
use minicore_host_protocol::TurnOutcome;
use ratatui::{Terminal, backend::TestBackend};

use super::harness::Harness;
use crate::app::{Action, App, Msg, NoticeLevel, Overlay, Panel};
use crate::history::{HistoryWindow, export_markdown};
use crate::testing::{production::with_provider_error, real_bounded};
use crate::transcript::Block;

fn failure(app: &App) -> Option<&str> {
    app.transcript_blocks()
        .iter()
        .find_map(|block| match block {
            Block::TurnEnd {
                outcome: TurnOutcome::Failed,
                reason,
                ..
            } => reason.as_deref(),
            _ => None,
        })
}

fn screen(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| {
            crate::ui::draw(frame, app);
        })
        .unwrap();
    terminal
        .backend()
        .buffer()
        .content()
        .chunks(usize::from(width))
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn provider_error_reaches_live_display_reopened_history_and_export() {
    let message = "Provider rejected the request.\nThe deployment policy requires an additional setting.\n请检查此部署的参数；错误内容保持完整。\nThe request was rejected before generation.\nFinal diagnostic token: E0123_END.";
    for streaming in [false, true] {
        let (native, mut provider) = real_bounded(with_provider_error(message, streaming)).await;
        let mut h = Harness::from_native(native).await;
        h.app
            .composer
            .set_text("trigger the loopback provider error");
        h.app.on_action(Action::Submit { follow: false });
        h.until(|app| failure(app).is_some() && !app.is_busy())
            .await;
        provider.request().await;
        let reason = failure(&h.app).unwrap().to_owned();
        assert!(reason.contains(message), "{reason}");
        let folded = screen(&mut h.app, 80, 18);
        assert!(folded.contains("full message"), "{folded}");
        assert!(!folded.contains("E0123_END"), "{folded}");
        h.ctrl(KeyCode::Char('o'));
        let expanded = screen(&mut h.app, 120, 35);
        assert!(expanded.contains("E0123_END"), "{expanded}");
        // TestBackend exposes each wide character's continuation cell as a space.
        assert!(expanded.contains("错 误 内 容 保 持 完 整"), "{expanded}");

        let client = h.native.client.session(h.native.target.clone());
        let history = real_bounded(HistoryWindow::tail(&client, 60))
            .await
            .unwrap();
        assert!(history.items.iter().any(|record| matches!(&record.item,
            HistoryItemData::TurnFinished { terminal } if terminal.reason.as_deref() == Some(reason.as_str())
        )));
        let mut exported = Vec::new();
        real_bounded(export_markdown(&client, &mut exported, 1))
            .await
            .unwrap();
        assert!(String::from_utf8(exported).unwrap().contains(&reason));

        let Harness {
            mut app,
            native,
            mut messages,
        } = h;
        messages.close();
        app.local_files.finish().await;
        drop(app);
        let mut reopened = Harness::from_native(native).await;
        assert_eq!(failure(&reopened.app), Some(reason.as_str()));
        reopened.ctrl(KeyCode::Char('o'));
        assert!(screen(&mut reopened.app, 120, 35).contains("E0123_END"));
        reopened.finish().await;
        provider.finish().await;
    }
}

#[tokio::test]
async fn last_operation_error_survives_typing_and_navigation_with_its_original_target() {
    let mut h = Harness::opened().await;
    let target = h.native.target.clone();
    let message = format!(
        "First cause\n{}\nFinal cause\u{1b}[2J",
        "Detailed cause 中文\n".repeat(30)
    );
    h.app
        .with_notice_for(&target, |app| app.notice(NoticeLevel::Error, &message));
    h.press(KeyCode::Char('x'));
    assert!(h.app.notices.is_empty());
    let next = h.another_session().await;
    h.open(&next).await;
    h.app
        .notice(NoticeLevel::Info, "Unrelated successful operation");
    h.app.open_panel(Panel::Error);
    let complete = h.app.view_text();
    assert!(complete.contains(&format!("Session: {}", target.session_id)));
    assert!(!complete.contains(next.session_id.as_str()));
    assert!(complete.contains("Final cause"));
    assert!(!complete.contains('\u{1b}'));
    let first = screen(&mut h.app, 80, 15);
    assert!(first.contains("Error details"));
    assert!(!first.contains("Final cause"));
    h.press(KeyCode::End);
    let last = screen(&mut h.app, 80, 15);
    assert!(last.contains("Final cause"), "{last}");
    assert_eq!(h.app.view_text(), complete);
    h.press(KeyCode::PageUp);
    assert!(!screen(&mut h.app, 80, 15).contains("Final cause"));
    h.press(KeyCode::Esc);
    assert!(matches!(h.app.overlay, Overlay::None));
    assert_eq!(h.app.resident.current().unwrap().address(), next);
    h.finish().await;
}

#[tokio::test]
async fn delayed_operation_errors_keep_the_original_target() {
    for operation in [
        "rename",
        "send",
        "delete",
        "default",
        "start-agent",
        "stop-agent",
        "delete-agent",
    ] {
        let mut h = Harness::opened().await;
        let original = h.native.target.clone();
        let other = h.another_session().await;
        h.open(&other).await;
        h.open(&original).await;
        // The real in-process client rejects these calls after its connection closes.
        h.app.client.close();
        let action = match operation {
            "rename" => Action::RenameSession {
                target: original.clone(),
                title: "Rejected rename".into(),
            },
            "send" => {
                h.app
                    .composer
                    .set_text("Rejected input for the original session");
                Action::Submit { follow: false }
            }
            "delete" => Action::DeleteSession(original.clone()),
            "default" => Action::SetDefaultSession,
            "start-agent" => Action::StartAgent,
            "stop-agent" => Action::StopAgent,
            "delete-agent" => Action::DeleteAgent {
                agent_id: original.agent_id.clone(),
                session_ids: vec![original.session_id.clone(), other.session_id.clone()],
            },
            _ => unreachable!(),
        };
        h.app.on_action(action);
        let completion = real_bounded(async {
            loop {
                let message = h.messages.recv().await.unwrap();
                if matches!(
                    &message,
                    Msg::SessionRenamed(_, Err(_))
                        | Msg::LocalPushed(_, Err(_))
                        | Msg::RefreshCatalogs(Err(_))
                ) {
                    break message;
                }
                h.app.on_msg(message);
            }
        })
        .await;
        h.open(&other).await;
        h.app.on_msg(completion);
        h.app.open_panel(Panel::Error);
        let details = h.app.view_text();
        if !operation.ends_with("agent") {
            assert!(
                details.contains(&format!("Session: {}", original.session_id)),
                "{details}"
            );
        }
        assert!(details.contains(original.agent_id.as_str()), "{details}");
        assert!(!details.contains(other.session_id.as_str()), "{details}");
        assert_eq!(h.app.resident.current().unwrap().address(), other);
        if operation == "send" {
            assert_eq!(
                h.app.local_inputs[0].status,
                crate::app::unconfirmed::Status::Rejected
            );
            assert!(h.app.composer.is_empty());
        }
        h.finish().await;
    }
}

#[tokio::test]
async fn export_error_keeps_its_selected_session_after_navigation() {
    let mut h = Harness::opened().await;
    let original = h.native.target.clone();
    let other = h.another_session().await;
    let directory = tempfile::tempdir().unwrap();
    h.app.on_action(Action::Export(Some(
        directory.path().to_str().unwrap().to_owned(),
    )));
    let completion = real_bounded(async {
        loop {
            let message = h.messages.recv().await.unwrap();
            if matches!(&message, Msg::Notice(NoticeLevel::Error, _)) {
                break message;
            }
            h.app.on_msg(message);
        }
    })
    .await;
    h.open(&other).await;
    h.app.on_msg(completion);
    h.app.open_panel(Panel::Error);
    let details = h.app.view_text();
    assert!(details.contains(original.session_id.as_str()), "{details}");
    assert!(!details.contains(other.session_id.as_str()), "{details}");
    assert!(
        details.contains(directory.path().to_str().unwrap()),
        "{details}"
    );
    h.finish().await;
}
