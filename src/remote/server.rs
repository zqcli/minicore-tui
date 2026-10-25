//! Bounded HTTP and WebSocket connections, with reducer-authorized admission.

use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Semaphore, mpsc, oneshot, watch};
use tokio::task::JoinSet;
use tokio::time::{Instant, timeout};
use tokio_tungstenite::tungstenite::{
    Message,
    handshake::server::{Request, Response},
    protocol::WebSocketConfig,
};
use tokio_tungstenite::{WebSocketStream, accept_hdr_async_with_config};

use super::{
    Connection, RemoteEvent,
    auth::Auth,
    wire::{self, Handshake},
};
use crate::app::{Msg, Tx};

const HEAD_LIMIT: usize = 8192;
const FIRST: Duration = Duration::from_secs(10);
const PING: Duration = Duration::from_secs(20);
const IDLE: Duration = Duration::from_secs(60);
pub const CSP: &str = "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; connect-src 'self'; img-src 'self' data:";
const HEADERS: [(&str, &str); 4] = [
    ("Cache-Control", "no-store"),
    ("X-Content-Type-Options", "nosniff"),
    ("Referrer-Policy", "no-referrer"),
    ("Content-Security-Policy", CSP),
];

pub async fn serve(
    listener: TcpListener,
    auth: Arc<Mutex<Auth>>,
    tx: Tx,
    mut stop: oneshot::Receiver<()>,
) {
    let permits = Arc::new(Semaphore::new(8));
    let mut tasks = JoinSet::new();
    loop {
        tokio::select! {
            _ = &mut stop => break,
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { return };
                let Ok(permit) = permits.clone().try_acquire_owned() else { continue };
                let auth = auth.clone();
                let tx = tx.clone();
                tasks.spawn(async move {
                    let _ = connection(stream, auth, tx, permit).await;
                });
            }
            _ = tasks.join_next(), if !tasks.is_empty() => {}
        }
    }
    drop(listener);
    let _ = timeout(FIRST, async { while tasks.join_next().await.is_some() {} }).await;
}

async fn head(stream: &TcpStream) -> io::Result<Vec<u8>> {
    let mut bytes = vec![0; HEAD_LIMIT];
    let mut seen = 0;
    loop {
        let n = stream.peek(&mut bytes).await?;
        if n == 0 {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
        }
        if let Some(end) = bytes[..n].windows(4).position(|s| s == b"\r\n\r\n") {
            bytes.truncate(end + 4);
            return Ok(bytes);
        }
        if n == HEAD_LIMIT {
            return Err(io::Error::other("request head too large"));
        }
        // Peek leaves existing bytes readable; wait briefly rather than spinning on them.
        if n == seen {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        seen = n;
    }
}

async fn http(stream: &mut TcpStream, status: &str, mime: &str, body: &str) -> io::Result<()> {
    let headers: String = HEADERS
        .iter()
        .map(|(name, value)| format!("{name}: {value}\r\n"))
        .collect();
    let reply = format!(
        "HTTP/1.1 {status}\r\n{headers}Content-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(reply.as_bytes()).await
}

fn same_origin(origin: &[u8], host: Option<&[u8]>) -> bool {
    let Ok(origin) = std::str::from_utf8(origin) else {
        return false;
    };
    let Some(authority) = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
    else {
        return false;
    };
    !authority.contains(['/', '?', '#', '@'])
        && host.is_some_and(|host| authority.as_bytes().eq_ignore_ascii_case(host))
}

async fn connection(
    mut stream: TcpStream,
    auth: Arc<Mutex<Auth>>,
    tx: Tx,
    permit: tokio::sync::OwnedSemaphorePermit,
) -> Result<(), String> {
    let bytes = timeout(FIRST, head(&stream))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    let mut headers = [httparse::EMPTY_HEADER; 128];
    let mut request = httparse::Request::new(&mut headers);
    request.parse(&bytes).map_err(|e| e.to_string())?;
    let header = |name: &str| {
        request
            .headers
            .iter()
            .find(|header| header.name.eq_ignore_ascii_case(name))
            .map(|header| header.value)
    };
    let websocket = request.method == Some("GET")
        && request.path == Some("/ws")
        && header("upgrade").is_some_and(|v| v.eq_ignore_ascii_case(b"websocket"));
    if websocket {
        if header("origin").is_some_and(|origin| !same_origin(origin, header("host"))) {
            stream
                .read_exact(&mut vec![0; bytes.len()])
                .await
                .map_err(|e| e.to_string())?;
            return http(&mut stream, "403 Forbidden", "text/plain", "Forbidden")
                .await
                .map_err(|e| e.to_string());
        }
    } else {
        let (status, mime, body) = match (request.method, request.path) {
            (Some("GET"), Some("/")) => (
                "200 OK",
                "text/html; charset=utf-8",
                include_str!("web/index.html"),
            ),
            (Some("GET"), Some("/manifest.webmanifest")) => (
                "200 OK",
                "application/manifest+json",
                include_str!("web/manifest.webmanifest"),
            ),
            (Some("GET"), Some("/icon.svg")) => {
                ("200 OK", "image/svg+xml", include_str!("web/icon.svg"))
            }
            _ => ("404 Not Found", "text/plain", "Not found"),
        };
        stream
            .read_exact(&mut vec![0; bytes.len()])
            .await
            .map_err(|e| e.to_string())?;
        return timeout(FIRST, http(&mut stream, status, mime, body))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string());
    }
    let config = WebSocketConfig::default()
        .max_message_size(Some(1024 * 1024))
        .max_frame_size(Some(1024 * 1024));
    let mut socket = timeout(
        FIRST,
        accept_hdr_async_with_config(
            stream,
            #[allow(clippy::result_large_err)] // Tungstenite fixes the callback's error type.
            |_: &Request, mut response: Response| {
                for (name, value) in HEADERS {
                    response.headers_mut().insert(
                        tokio_tungstenite::tungstenite::http::HeaderName::from_bytes(
                            name.as_bytes(),
                        )
                        .unwrap(),
                        value.parse().unwrap(),
                    );
                }
                Ok(response)
            },
            Some(config),
        ),
    )
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())?;
    let handshake = match timeout(FIRST, socket.next()).await {
        Ok(Some(Ok(Message::Text(text)))) => serde_json::from_str::<Handshake>(&text).ok(),
        _ => None,
    };
    let Some(handshake) = handshake else {
        return reject(&mut socket, "bad_request").await;
    };
    if handshake.version() != wire::VERSION {
        return reject(&mut socket, "version").await;
    }
    let authenticated = tokio::task::spawn_blocking(move || {
        let mut auth = auth.lock().unwrap();
        match handshake {
            Handshake::Pair { code, device, .. } => auth
                .pair(&code, device)
                .map(|(device, token)| (device, Some(token))),
            Handshake::Hello { token, .. } => auth
                .verify(&token)
                .map(|device| (device, None))
                .ok_or_else(|| "unauthorized".to_owned()),
        }
    })
    .await
    .map_err(|e| e.to_string())?;
    let Ok((device, token)) = authenticated else {
        return reject(&mut socket, "unauthorized").await;
    };
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let (outbound, mut output) = mpsc::channel(512);
    let (close, mut closing) = watch::channel(None);
    let (accepted, admission) = oneshot::channel();
    let name = device.name.clone();
    tx.send(Msg::Remote(RemoteEvent::Connect {
        connection: Connection {
            id,
            device,
            outbound,
            close,
        },
        accepted,
    }))
    .map_err(|_| "TUI closed")?;
    if !admission.await.unwrap_or(false) {
        return reject(&mut socket, "busy").await;
    }
    drop(permit);
    let result = tokio::select! {
        biased;
        _ = closing.changed() => Ok(()),
        result = async {
        if let Some(token) = token { send(&mut socket, json!({"t":"paired", "token": token})).await?; }
        send(&mut socket, json!({"t":"welcome", "v":wire::VERSION, "version":env!("CARGO_PKG_VERSION"), "device":name})).await?;
        let mut ping = tokio::time::interval_at(Instant::now() + PING, PING);
        let mut last = Instant::now();
        loop {
            tokio::select! {
                biased;
                _ = tokio::time::sleep_until(last + IDLE) => break,
                _ = ping.tick() => { socket.send(Message::Ping(Vec::new().into())).await.map_err(|e| e.to_string())?; }
                message = output.recv() => match message {
                    Some(value) => send(&mut socket, value).await?,
                    None => break,
                },
                frame = socket.next() => {
                    last = Instant::now();
                    match frame {
                        Some(Ok(Message::Text(text))) => {
                            let value = serde_json::from_str::<Value>(&text).unwrap_or(Value::Null);
                            let request = value.get("id").and_then(Value::as_str).filter(|id| id.len() <= 64)
                                .filter(|_| value["t"] == "intent")
                                .and_then(|id| serde_json::from_value::<wire::Intent>(value.clone()).ok().map(|intent| (id.to_owned(), intent)));
                            if let Some((request_id, intent)) = request {
                                let (done, finished) = oneshot::channel();
                                tx.send(Msg::Remote(RemoteEvent::Request { connection: id, id: request_id, intent, done })).map_err(|_| "TUI closed")?;
                                finished.await.map_err(|_| "TUI closed")?;
                            } else { send(&mut socket, wire::bad_request(&value)).await?; }
                        }
                        Some(Ok(Message::Ping(_) | Message::Pong(_))) => { socket.flush().await.map_err(|e| e.to_string())?; }
                        Some(Ok(Message::Binary(_))) => send(&mut socket, wire::bad_request(&Value::Null)).await?,
                        _ => break,
                    }
                }
            }
        }
        Ok(())
        } => result,
    };
    let reason = *closing.borrow();
    if let Some(reason) = reason.filter(|reason| *reason != "overflow") {
        let _ = timeout(
            FIRST,
            send(&mut socket, json!({"t":"bye", "reason":reason})),
        )
        .await;
    }
    let _ = tx.send(Msg::Remote(RemoteEvent::Disconnected(id)));
    result
}

async fn send(socket: &mut WebSocketStream<TcpStream>, value: Value) -> Result<(), String> {
    timeout(IDLE, socket.send(Message::Text(value.to_string().into())))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

async fn reject(socket: &mut WebSocketStream<TcpStream>, code: &str) -> Result<(), String> {
    send(socket, wire::error(code, code)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    async fn start() -> (
        tempfile::TempDir,
        std::net::SocketAddr,
        Arc<Mutex<Auth>>,
        mpsc::UnboundedReceiver<Msg>,
        tokio::task::JoinHandle<()>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let auth = Arc::new(Mutex::new(
            Auth::load(dir.path().join("remote-devices.json")).unwrap(),
        ));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (tx, rx) = mpsc::unbounded_channel();
        let server_auth = auth.clone();
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(async move {
            let _stop = stop;
            serve(listener, server_auth, tx, stopped).await;
        });
        (dir, address, auth, rx, task)
    }

    #[tokio::test]
    async fn http_serves_the_client_with_security_headers_and_rejects_foreign_origins() {
        let (_dir, address, _auth, _rx, task) = start().await;
        for (request, status) in [
            ("GET / HTTP/1.1\r\nHost: localhost\r\n\r\n", "200 OK"),
            (
                "GET /ws HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nOrigin: https://foreign.example\r\n\r\n",
                "403 Forbidden",
            ),
        ] {
            let mut socket = TcpStream::connect(address).await.unwrap();
            socket.write_all(request.as_bytes()).await.unwrap();
            let mut reply = String::new();
            socket.read_to_string(&mut reply).await.unwrap();
            assert!(reply.contains(status), "{reply}");
            assert!(reply.contains(CSP));
            if status == "200 OK" {
                assert!(reply.ends_with(include_str!("web/index.html")));
            }
        }
        task.abort();
    }

    #[tokio::test]
    async fn pairing_precedes_welcome_and_the_reducer_snapshot_while_wrong_tokens_are_refused() {
        let (_dir, address, auth, mut rx, task) = start().await;
        let connect = || async {
            let stream = TcpStream::connect(address).await.unwrap();
            tokio_tungstenite::client_async(format!("ws://{address}/ws"), stream)
                .await
                .unwrap()
                .0
        };
        let mut socket = connect().await;
        send(&mut socket, json!({"t":"hello", "v":1, "token":"wrong"}))
            .await
            .unwrap();
        let reply: Value =
            serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(reply["code"], "unauthorized");
        let mut socket = connect().await;
        let code = auth.lock().unwrap().pairing.code.clone();
        send(
            &mut socket,
            json!({"t":"pair", "v":1, "code":code, "device":"phone"}),
        )
        .await
        .unwrap();
        let Msg::Remote(RemoteEvent::Connect {
            connection,
            accepted,
        }) = rx.recv().await.unwrap()
        else {
            panic!("admission")
        };
        accepted.send(true).unwrap();
        connection.send(json!({"t":"snapshot", "state":{}, "blocks":[]}));
        for kind in ["paired", "welcome", "snapshot"] {
            let reply: Value =
                serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap())
                    .unwrap();
            assert_eq!(reply["t"], kind);
        }
        task.abort();
    }
}
