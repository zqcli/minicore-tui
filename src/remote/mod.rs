//! Remote transport and the reducer-owned controller connection.

pub mod auth;
pub mod server;
pub mod wire;

use serde_json::Value;
use tokio::sync::{mpsc, oneshot, watch};

use auth::Device;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RemoteMode {
    #[default]
    Off,
    Waiting,
    Connected,
}

pub struct Connection {
    pub id: u64,
    pub device: Device,
    pub outbound: mpsc::Sender<Value>,
    pub close: watch::Sender<Option<&'static str>>,
}

impl Connection {
    pub fn send(&self, message: Value) -> bool {
        if self.outbound.try_send(message).is_err() {
            let _ = self.close.send(Some("overflow"));
            return false;
        }
        true
    }

    pub fn bye(&self, reason: &'static str) {
        let _ = self.close.send(Some(reason));
    }
}

#[derive(Default)]
pub struct Remote {
    pub mode: RemoteMode,
    pub connection: Option<Connection>,
}

impl Remote {
    pub fn on_event(&mut self, event: RemoteEvent) {
        match event {
            RemoteEvent::Connect {
                connection,
                accepted,
            } => {
                if self.mode == RemoteMode::Off
                    || self.connection.as_ref().is_some_and(|old| {
                        old.device.token_sha256 != connection.device.token_sha256
                    })
                {
                    let _ = accepted.send(false);
                    return;
                }
                if accepted.send(true).is_err() {
                    return;
                }
                if let Some(old) = self.connection.replace(connection) {
                    old.bye("replaced");
                }
                self.mode = RemoteMode::Connected;
            }
            RemoteEvent::Disconnected(id) => {
                if self
                    .connection
                    .as_ref()
                    .is_some_and(|connection| connection.id == id)
                {
                    self.connection = None;
                    self.mode = RemoteMode::Waiting;
                }
            }
            RemoteEvent::Request {
                connection,
                id,
                done,
                ..
            } => {
                if let Some(phone) = self
                    .connection
                    .as_ref()
                    .filter(|phone| phone.id == connection)
                {
                    phone.send(wire::ack(
                        &id,
                        Err("Remote intents are not enabled yet".into()),
                    ));
                }
                let _ = done.send(());
            }
        }
    }
}

pub enum RemoteEvent {
    Connect {
        connection: Connection,
        accepted: oneshot::Sender<bool>,
    },
    Request {
        connection: u64,
        id: String,
        intent: wire::Intent,
        done: oneshot::Sender<()>,
    },
    Disconnected(u64),
}
