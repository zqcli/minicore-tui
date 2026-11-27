//! On-demand remote WebSocket (`wss://` or reverse-proxied local `ws://`)
//! listener backed by the embedded Host and a one-time in-memory access key.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use minicore_host_core::{Host, NetworkStatus};
use minicore_host_protocol::OperationId;
use minicore_host_transport::{load_tls_server_config, serve_ws, serve_wss};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use crate::app::{Msg, Tx};
use crate::config::Remote;

pub const DEFAULT_LISTEN: &str = "127.0.0.1:8443";

/// Live remote listener started by `/remote`. Dropping stops the
/// listener and clears the runtime access key override on the embedded Host.
pub struct RemoteServer {
    host: Arc<Host>,
    pub listen_addr: SocketAddr,
    pub display_url: String,
    pub tls: bool,
    pub access_key: String,
    pub status: NetworkStatus,
    pub workspace_roots_empty: bool,
    server_task: JoinHandle<()>,
    watch_task: JoinHandle<()>,
}

impl RemoteServer {
    pub async fn start(
        host: Arc<Host>,
        config: &Remote,
        listen_override: Option<String>,
        tx: Tx,
        workspace_roots_empty: bool,
    ) -> Result<Self, String> {
        let listen_raw = listen_override
            .as_deref()
            .or(config.listen.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_LISTEN);
        let addr: SocketAddr = listen_raw.parse().map_err(|_| {
            format!(
                "invalid remote listen address {listen_raw:?}; expected host:port (e.g. 127.0.0.1:8443 or [::]:8443)"
            )
        })?;

        let tls_config = match (
            config
                .tls_cert
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty()),
            config
                .tls_key
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty()),
        ) {
            (Some(cert), Some(key)) => {
                let cert = PathBuf::from(cert);
                let key = PathBuf::from(key);
                let loaded =
                    tokio::task::spawn_blocking(move || load_tls_server_config(&cert, &key))
                        .await
                        .map_err(|_| "TLS preparation worker failed".to_owned())?
                        .map_err(str::to_owned)?;
                Some(loaded)
            }
            (None, None) => None,
            _ => {
                return Err(
                    "remote.tls_cert and remote.tls_key must both be set for WSS, or both omitted for plain local WS"
                        .to_owned(),
                );
            }
        };

        let listener = TcpListener::bind(addr)
            .await
            .map_err(|error| format!("cannot bind remote listener on {addr}: {error}"))?;
        let listen_addr = listener
            .local_addr()
            .map_err(|error| format!("cannot inspect bound remote address: {error}"))?;
        let access_key = generate_access_key()?;
        host.set_network_access_override(Some(access_key.clone()))
            .map_err(|fault| fault.to_string())?;

        let tls = tls_config.is_some();
        let display_url = config
            .public_url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{}://{listen_addr}/", if tls { "wss" } else { "ws" }));

        let status = host.network_status();
        let mut status_rx = host.subscribe_network_status();

        let server_host = Arc::clone(&host);
        let server_tx = tx.clone();
        let server_task = tokio::spawn(async move {
            let result = match tls_config {
                Some(tls) => serve_wss(server_host, listener, tls).await,
                None => serve_ws(server_host, listener).await,
            };
            if let Err(error) = result {
                let _ = server_tx.send(Msg::RemoteFailed(error.to_string())).await;
            }
        });

        let watch_task = tokio::spawn(async move {
            while status_rx.changed().await.is_ok() {
                let next = *status_rx.borrow();
                if tx.send(Msg::RemoteStatus(next)).await.is_err() {
                    break;
                }
            }
        });

        Ok(Self {
            host,
            listen_addr,
            display_url,
            tls,
            access_key,
            status,
            workspace_roots_empty,
            server_task,
            watch_task,
        })
    }

    pub fn rotate_key(&mut self) -> Result<(), String> {
        let next = generate_access_key()?;
        self.host
            .set_network_access_override(Some(next.clone()))
            .map_err(|fault| fault.to_string())?;
        self.access_key = next;
        self.status = self.host.network_status();
        Ok(())
    }

    pub fn stop(self) {
        // Drop handles abort + revoking the override key.
        drop(self);
    }
}

impl Drop for RemoteServer {
    fn drop(&mut self) {
        self.server_task.abort();
        self.watch_task.abort();
        let _ = self.host.set_network_access_override(None);
    }
}

/// Generates a 24-character Crockford Base32 one-time key (120 bits of OS CSPRNG).
pub fn generate_access_key() -> Result<String, String> {
    let a = OperationId::new().map_err(|error| error.to_string())?;
    let b = OperationId::new().map_err(|error| error.to_string())?;
    let combined = format!("{a}{b}");
    Ok(combined[..24].to_owned())
}
