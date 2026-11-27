use crossterm::event::KeyCode;
use minicore_host_core::NetworkStatus;
use ratatui::{Terminal, backend::TestBackend};

use super::harness::Harness;
use crate::app::{Action, Msg, Overlay, actions::RemoteStartup};

fn render_screen(harness: &mut Harness, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| {
            crate::ui::draw(frame, &mut harness.app);
        })
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    buffer
        .content()
        .chunks(width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn remote_command_hands_off_control_rotates_key_and_reclaims_on_escape() {
    let mut harness = Harness::opened().await;
    assert!(harness.app.can_operate());

    // Start plain local WS on an ephemeral loopback port (no TLS paths configured).
    harness
        .app
        .on_action(Action::RemoteStart(Some("127.0.0.1:0".into())));
    harness.until(|app| app.remote.is_some()).await;

    assert!(matches!(
        harness.app.overlay,
        Overlay::Remote { show_key: true, .. }
    ));
    assert!(!harness.app.can_operate());
    let key1 = harness.app.remote.as_ref().unwrap().access_key.clone();
    assert_eq!(key1.len(), 24);

    // Overlay renders the address, listening mode, and plaintext access key.
    let screen = render_screen(&mut harness, 100, 28);
    assert!(screen.contains("Remote WS · waiting for web"), "{screen}");
    assert!(screen.contains(&key1), "{screen}");
    assert!(screen.contains("reverse proxy / CF -> WSS"), "{screen}");

    // Press 's' to hide the key.
    harness.press(KeyCode::Char('s'));
    let masked = render_screen(&mut harness, 100, 28);
    assert!(!masked.contains(&key1), "{masked}");
    assert!(masked.contains("••••"), "{masked}");

    // A remote network peer connects with the generated key and becomes operator.
    let host = harness.app.client.host().unwrap();
    let remote_conn = host.connect_network(&key1).unwrap();
    assert!(remote_conn.initial_info().can_operate);
    harness
        .until(|app| {
            app.remote.as_ref().is_some_and(|r| {
                r.status
                    == NetworkStatus {
                        connections: 1,
                        operator_is_network: true,
                    }
            })
        })
        .await;
    let active = render_screen(&mut harness, 100, 28);
    assert!(active.contains("Remote WS · web operating"), "{active}");

    // Press 'r' to rotate key: generates a fresh key and kicks the old network peer.
    harness.press(KeyCode::Char('r'));
    let key2 = harness.app.remote.as_ref().unwrap().access_key.clone();
    assert_ne!(key1, key2);
    assert!(host.connect_network(&key1).is_err());
    let remote_conn2 = host.connect_network(&key2).unwrap();
    assert!(remote_conn2.initial_info().can_operate);

    // Press Esc to stop remote access, revoke remote peer, and reclaim terminal operator control.
    harness.press(KeyCode::Esc);
    assert!(harness.app.remote.is_none());
    assert!(harness.app.overlay.is_none());
    assert!(harness.app.can_operate());
    assert!(host.connect_network(&key2).is_err());
}

#[tokio::test]
async fn remote_rejects_mismatched_tls_pair() {
    let mut harness = Harness::opened().await;
    harness.app.remote_config.tls_cert = Some("/nonexistent/cert.pem".into());
    harness.app.remote_config.tls_key = None;

    harness
        .app
        .on_action(Action::RemoteStart(Some("127.0.0.1:0".into())));
    harness
        .until(|app| app.remote_start == RemoteStartup::Idle)
        .await;

    assert!(harness.app.remote.is_none());
    assert!(harness.app.can_operate());
    assert!(
        harness
            .notices()
            .iter()
            .any(|msg| msg.contains("tls_cert") && msg.contains("tls_key")),
        "{:?}",
        harness.notices()
    );
}

#[tokio::test]
async fn remote_off_cancels_pending_start_before_handoff_and_key_replacement() {
    let mut h = Harness::opened().await;
    let host = h.app.client.host().unwrap();
    h.app
        .on_action(Action::RemoteStart(Some("127.0.0.1:0".into())));
    assert_eq!(h.app.remote_start, RemoteStartup::Pending);

    // Hold the completed loopback startup outside the reducer until after /remote off.
    let result = crate::testing::real_bounded(async {
        loop {
            match h.messages.recv().await.unwrap() {
                Msg::RemoteStarted(result) => break result,
                message => h.app.on_msg(message),
            }
        }
    })
    .await;
    let cancelled_key = result.as_ref().unwrap().access_key.clone();
    h.app.on_action(Action::RemoteStop);
    assert_eq!(h.app.remote_start, RemoteStartup::Cancelling);
    assert!(h.app.remote.is_none());
    assert!(h.app.can_operate());

    // Do not overlap startups: dropping the old server revokes its Host key override.
    h.app
        .on_action(Action::RemoteStart(Some("127.0.0.1:0".into())));
    assert_eq!(h.app.remote_start, RemoteStartup::Cancelling);
    h.app.on_msg(Msg::RemoteStarted(result));
    assert_eq!(h.app.remote_start, RemoteStartup::Idle);
    assert!(h.app.remote.is_none());
    assert!(!matches!(h.app.overlay, Overlay::Remote { .. }));
    assert!(host.connect_network(&cancelled_key).is_err());
    h.another_session().await; // The actual Host still recognizes terminal operation rights.

    h.app
        .on_action(Action::RemoteStart(Some("127.0.0.1:0".into())));
    h.until(|app| app.remote.is_some()).await;
    let fresh_key = h.app.remote.as_ref().unwrap().access_key.clone();
    assert_ne!(fresh_key, cancelled_key);
    let connection = host.connect_network(&fresh_key).unwrap();
    assert!(connection.initial_info().can_operate);
    h.app.on_action(Action::RemoteStop);
    assert!(host.connect_network(&fresh_key).is_err());
    h.another_session().await;
    h.finish().await;
}
