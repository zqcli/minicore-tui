//! Remote transport and the reducer-owned phone connections.

pub mod auth;
pub mod fs;
pub mod server;
pub mod view;
pub mod wire;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use crate::app::{App, LaunchSettings, NoticeLevel};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot, watch};

use auth::{Auth, Device};

/// A duration in seconds, rounded up, as sign-in pauses are reported.
pub fn whole_seconds(duration: std::time::Duration) -> u64 {
    duration.as_millis().div_ceil(1000) as u64
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RemoteMode {
    #[default]
    Off,
    Waiting,
    Connected,
}

pub struct Connection {
    pub id: u64,
    pub generation: u64,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftSettings {
    pub workspace: PathBuf,
    pub launch: LaunchSettings,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PhoneView {
    Session(String),
    Draft(DraftSettings),
}

impl PhoneView {
    pub fn foreground(app: &App) -> Self {
        match &app.session {
            Some(session) => Self::Session(session.info.session_id.clone()),
            None => Self::Draft(DraftSettings {
                workspace: app.workspace.clone(),
                launch: app.draft_launch().clone(),
            }),
        }
    }

    pub fn session_id(&self) -> Option<&str> {
        match self {
            Self::Session(id) => Some(id),
            Self::Draft(_) => None,
        }
    }
}

/// An admitted connection with its own view and projection tracker.
pub struct Phone {
    pub connection: Connection,
    pub view: PhoneView,
    pub projection: view::View,
    pub connected_at: jiff::Timestamp,
}

impl Phone {
    pub fn set_view(&mut self, view: PhoneView) {
        if self.view != view {
            self.view = view;
            self.projection = view::View::default();
        }
    }
}

#[derive(Default)]
pub enum NoticeTarget {
    #[default]
    All,
    Phone(u64),
    Session(String),
}

#[derive(Default)]
pub struct Remote {
    /// Admitted connections, oldest first.
    pub phones: Vec<Phone>,
    pub catalog: view::Catalog,
    /// The connection whose session open or create is pending.
    pub opening: Option<u64>,
    pub notice_target: NoticeTarget,
    /// The foreground draft's settings while `App::launch` stays neutral.
    pub draft: Option<DraftSettings>,
    pub config: Option<crate::config::Remote>,
    pub auth: Option<Arc<Mutex<Auth>>>,
    pub url: Option<String>,
    pub generation: u64,
    pub starting: Option<tokio::task::JoinHandle<()>>,
    pub quit_armed: Option<tokio::time::Instant>,
    /// Wrong passwords since remote control started or the password changed.
    pub failed_logins: u32,
    pub paused_until: Option<tokio::time::Instant>,
    stop: Option<oneshot::Sender<()>>,
    server_task: Option<tokio::task::JoinHandle<()>>,
}

impl Remote {
    pub fn viewing(&self, id: &str) -> bool {
        self.phones
            .iter()
            .any(|phone| phone.view.session_id() == Some(id))
    }

    pub fn notice(&mut self, level: NoticeLevel, text: &str, label: Option<&str>) {
        self.phones.retain(|phone| {
            let text = match &self.notice_target {
                NoticeTarget::Phone(id) if phone.connection.id != *id => return true,
                NoticeTarget::Session(id) if phone.view.session_id() != Some(id) => {
                    format!(
                        "\"{}\": {text}",
                        label.expect("a session notice has a label")
                    )
                }
                _ => text.to_owned(),
            };
            phone
                .connection
                .send(json!({"t":"notice", "level":match level {
                NoticeLevel::Info => "info", NoticeLevel::Error => "error",
            }, "text":text}))
        });
    }

    pub fn send_viewers(&mut self, id: &str, viewing: bool, message: Value) -> bool {
        let mut sent = false;
        self.phones.retain(|phone| {
            if (phone.view.session_id() == Some(id)) != viewing {
                return true;
            }
            let delivered = phone.connection.send(message.clone());
            sent |= delivered;
            delivered
        });
        sent
    }

    pub fn returned(&mut self, id: &str, sender: Option<u64>, text: &str) -> bool {
        let message = json!({"t":"returned", "text":text});
        if let Some(phone) = self
            .phones
            .iter()
            .find(|phone| Some(phone.connection.id) == sender)
        {
            if phone.connection.send(message.clone()) {
                return true;
            }
            self.phones
                .retain(|phone| Some(phone.connection.id) != sender);
        }
        self.send_viewers(id, true, message)
    }

    pub fn mode(&self) -> RemoteMode {
        if self.url.is_none() {
            RemoteMode::Off
        } else if !self.phones.is_empty() {
            RemoteMode::Connected
        } else {
            RemoteMode::Waiting
        }
    }
    pub fn url(&self) -> Option<&str> {
        self.url.as_deref()
    }
    /// The connection password, shown on the status screen.
    pub fn password(&self) -> Option<String> {
        Some(self.auth.as_ref()?.lock().unwrap().password().to_owned())
    }

    pub fn active(&self) -> bool {
        self.mode() != RemoteMode::Off
    }
    pub fn roots(&self) -> &[PathBuf] {
        &self
            .config
            .as_ref()
            .expect("remote settings initialized at startup")
            .roots
    }

    pub fn start(&mut self, bound: Bound, tx: crate::app::Tx) {
        let (stop, stopped) = oneshot::channel();
        self.server_task = Some(tokio::spawn(server::serve(
            bound.listener,
            bound.auth.clone(),
            tx,
            stopped,
            self.generation,
            self.roots().to_vec().into(),
        )));
        self.stop = Some(stop);
        self.auth = Some(bound.auth);
        self.url = Some(bound.url);
        self.starting = None;
        self.failed_logins = 0;
        self.paused_until = None;
    }

    pub fn stop(&mut self, reason: &'static str) {
        self.generation += 1;
        if let Some(starting) = self.starting.take() {
            starting.abort();
        }
        self.bye(reason);
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        self.auth = None;
        self.url = None;
    }

    pub async fn shutdown(&mut self) {
        self.stop("tui_exit");
        if let Some(task) = self.server_task.take() {
            let _ = tokio::time::timeout(std::time::Duration::from_millis(500), task).await;
        }
    }

    /// Says `bye` to every phone and forgets them.
    pub fn bye(&mut self, reason: &'static str) {
        for phone in self.phones.drain(..) {
            phone.connection.bye(reason);
        }
    }

    /// Sends `message` to connection `to`, or to every phone for `None`; a full queue drops its phone.
    pub fn send(&mut self, to: Option<u64>, message: Value) {
        self.phones.retain(|phone| {
            to.is_some_and(|id| id != phone.connection.id) || phone.connection.send(message.clone())
        });
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
    /// A wrong password; `locked` is the pause it started or ran into.
    LoginRefused {
        locked: Option<std::time::Duration>,
    },
}
