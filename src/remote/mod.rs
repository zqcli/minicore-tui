//! Remote transport and the reducer-owned controller connection.

pub mod auth;
pub mod server;
pub mod wire;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot, watch};

use auth::{Auth, Device};

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

pub struct Bound {
    pub listener: TcpListener,
    pub auth: Arc<Mutex<Auth>>,
    pub url: String,
}

impl Bound {
    pub async fn bind(config: &crate::config::Remote, file: PathBuf) -> Result<Self, String> {
        let listener = TcpListener::bind(config.bind)
            .await
            .map_err(|error| error.to_string())?;
        let address = listener.local_addr().map_err(|error| error.to_string())?;
        let configured_url = config.url.clone();
        let (auth, url) = tokio::task::spawn_blocking(move || {
            let auth = Auth::load(file)?;
            let url = match configured_url {
                Some(url) => url,
                None => {
                    let mut address = address;
                    if address.ip().is_unspecified() {
                        let (bind, destination) = if address.is_ipv4() {
                            ("0.0.0.0:0", "192.0.2.1:80")
                        } else {
                            ("[::]:0", "[2001:db8::1]:80")
                        };
                        let route = std::net::UdpSocket::bind(bind).map_err(|e| e.to_string())?;
                        route.connect(destination).map_err(|e| {
                            format!("cannot find LAN address; configure remote.url: {e}")
                        })?;
                        address.set_ip(route.local_addr().map_err(|e| e.to_string())?.ip());
                    }
                    format!("http://{address}")
                }
            };
            Ok::<_, String>((auth, url))
        })
        .await
        .map_err(|e| e.to_string())??;
        Ok(Self {
            listener,
            auth: Arc::new(Mutex::new(auth)),
            url,
        })
    }
}

#[derive(Default)]
pub struct Remote {
    pub mode: RemoteMode,
    pub connection: Option<Connection>,
    pub config: Option<crate::config::Remote>,
    pub auth: Option<Arc<Mutex<Auth>>>,
    pub url: Option<String>,
    pub generation: u64,
    pub starting: Option<tokio::task::JoinHandle<()>>,
    pub quit_armed: Option<tokio::time::Instant>,
    stop: Option<oneshot::Sender<()>>,
}

impl Remote {
    pub fn mode(&self) -> RemoteMode {
        self.mode
    }
    pub fn url(&self) -> Option<&str> {
        self.url.as_deref()
    }
    pub fn device_name(&self) -> Option<&str> {
        self.connection
            .as_ref()
            .map(|phone| phone.device.name.as_str())
    }
    pub fn pairing(&self) -> Option<(String, tokio::time::Instant)> {
        let auth = self.auth.as_ref()?.lock().unwrap();
        auth.pairing
            .active()
            .then(|| (auth.pairing.code.clone(), auth.pairing.expires))
    }
    pub fn active(&self) -> bool {
        self.mode != RemoteMode::Off
    }
    pub fn roots(&self) -> &[PathBuf] {
        self.config
            .as_ref()
            .map_or(&[], |config| config.roots.as_slice())
    }

    pub fn start(&mut self, bound: Bound, tx: crate::app::Tx) {
        let (stop, stopped) = oneshot::channel();
        tokio::spawn(server::serve(
            bound.listener,
            bound.auth.clone(),
            tx,
            stopped,
        ));
        self.stop = Some(stop);
        self.auth = Some(bound.auth);
        self.url = Some(bound.url);
        self.mode = RemoteMode::Waiting;
        self.starting = None;
    }

    pub fn stop(&mut self, reason: &'static str) {
        self.generation += 1;
        if let Some(starting) = self.starting.take() {
            starting.abort();
        }
        if let Some(connection) = self.connection.take() {
            connection.bye(reason);
        }
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        self.mode = RemoteMode::Off;
        self.auth = None;
        self.url = None;
    }

    pub fn send(&mut self, message: Value) {
        if self
            .connection
            .as_ref()
            .is_some_and(|phone| !phone.send(message))
        {
            self.connection = None;
            self.mode = RemoteMode::Waiting;
        }
    }
}

pub enum RemoteEvent {
    Bound(u64, Result<Bound, String>),
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
