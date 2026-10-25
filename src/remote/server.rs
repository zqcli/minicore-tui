//! Bounded HTTP and WebSocket connections, with reducer-authorized admission.

use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
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
    auth::{Auth, Refusal},
    fs,
    wire::{self, FsRequest, Handshake},
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
    generation: u64,
    roots: Arc<[PathBuf]>,
) {
    let permits = Arc::new(Semaphore::new(8));
    let mut tasks = JoinSet::new();
    loop {
        tokio::select! {
            _ = &mut stop => break,
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                };
                let Ok(permit) = permits.clone().try_acquire_owned() else { continue };
                let auth = auth.clone();
                let tx = tx.clone();
                let roots = roots.clone();
                tasks.spawn(async move {
                    let _ = connection(stream, auth, tx, permit, generation, roots).await;
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

async fn http(stream: &mut TcpStream, status: &str, mime: &str, body: &[u8]) -> io::Result<()> {
    let headers: String = HEADERS
        .iter()
        .map(|(name, value)| format!("{name}: {value}\r\n"))
        .collect();
    let head = format!(
        "HTTP/1.1 {status}\r\n{headers}Content-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(&[head.as_bytes(), body].concat()).await
}

fn same_origin(origin: &[u8], host: Option<&[u8]>) -> bool {
    let Ok(origin) = std::str::from_utf8(origin) else {
        return false;
    };
    let Some((scheme, authority)) = origin.split_once("://") else {
        return false;
    };
    (scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https"))
        && !authority.contains(['/', '?', '#', '@'])
        && host.is_some_and(|host| authority.as_bytes().eq_ignore_ascii_case(host))
}

async fn connection(
    mut stream: TcpStream,
    auth: Arc<Mutex<Auth>>,
    tx: Tx,
    permit: tokio::sync::OwnedSemaphorePermit,
    generation: u64,
    roots: Arc<[PathBuf]>,
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
            return http(&mut stream, "403 Forbidden", "text/plain", b"Forbidden")
                .await
                .map_err(|e| e.to_string());
        }
    } else {
        let (status, mime, body) = match (request.method, request.path) {
            (Some("GET"), Some("/")) => (
                "200 OK",
                "text/html; charset=utf-8",
                include_str!("web/index.html").as_bytes(),
            ),
            (Some("GET"), Some("/manifest.webmanifest")) => (
                "200 OK",
                "application/manifest+json",
                include_str!("web/manifest.webmanifest").as_bytes(),
            ),
            (Some("GET"), Some("/icon.svg")) => (
                "200 OK",
                "image/svg+xml",
                include_str!("web/icon.svg").as_bytes(),
            ),
            (Some("GET"), Some("/icon.png")) => (
                "200 OK",
                "image/png",
                include_bytes!("web/icon.png").as_slice(),
            ),
            _ => ("404 Not Found", "text/plain", b"Not found".as_slice()),
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
    if let Handshake::Login { device, .. } = &handshake
        && (device.is_empty()
            || device.chars().count() > 64
            || device.chars().any(char::is_control))
    {
        return reject(&mut socket, "bad_request").await;
    }
    let credentials = auth.clone();
    let authenticated = tokio::task::spawn_blocking(move || {
        let mut auth = credentials.lock().unwrap();
        match handshake {
            Handshake::Login {
                password, device, ..
            } => auth
                .prepare_login(&password, device)
                .map(|(device, token)| (device, Some(token)))
                .map_err(Some),
            Handshake::Hello { token, .. } => {
                auth.verify(&token).map(|device| (device, None)).ok_or(None)
            }
        }
    })
    .await
    .map_err(|e| e.to_string())?;
    let (device, token) = match authenticated {
        Ok(authenticated) => authenticated,
        Err(None) => return reject(&mut socket, "unauthorized").await,
        Err(Some(refusal)) => {
            let locked = match refusal {
                Refusal::Wrong => None,
                Refusal::Locked(pause) => Some(pause),
            };
            let _ = tx.send(Msg::Remote(RemoteEvent::LoginRefused { locked }));
            let reply = match locked {
                None => wire::error("wrong_password", "Wrong password"),
                Some(pause) => {
                    let mut reply = wire::error("locked", "Too many wrong passwords");
                    reply["retry_after"] = json!(super::whole_seconds(pause));
                    reply
                }
            };
            return send(&mut socket, reply).await;
        }
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
            generation,
            device: device.clone(),
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
    let controller = closing.clone();
    let result = tokio::select! {
        biased;
        _ = closing.changed() => Ok(()),
        result = async {
        if let Some(token) = token {
            let remembered = tokio::task::spawn_blocking(move || auth.lock().unwrap().remember(device))
                .await.map_err(|error| error.to_string())?;
            if remembered.is_err() { return reject(&mut socket, "unauthorized").await; }
            send(&mut socket, json!({"t":"authorized", "token": token})).await?;
        }
        send(&mut socket, json!({"t":"welcome", "v":wire::VERSION, "version":env!("CARGO_PKG_VERSION"), "device":name, "now_ms":crate::protocol::stamp::now_millis()})).await?;
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
                            let request_id = value.get("id").and_then(Value::as_str).filter(|id| id.len() <= 64);
                            if let Some(request_id) = request_id {
                                if value["t"] == "intent" {
                                    if let Ok(intent) = serde::Deserialize::deserialize(&value) {
                                        let (done, finished) = oneshot::channel();
                                        tx.send(Msg::Remote(RemoteEvent::Request { connection: id, id: request_id.to_owned(), intent, done })).map_err(|_| "TUI closed")?;
                                        finished.await.map_err(|_| "TUI closed")?;
                                        continue;
                                    }
                                } else if let Ok(request) = FsRequest::deserialize(&value) {
                                    let reply = filesystem(request_id.to_owned(), request, roots.clone(), controller.clone()).await?;
                                    let Some(reply) = reply else { break };
                                    send(&mut socket, reply).await?;
                                    continue;
                                }
                            }
                            send(&mut socket, wire::bad_request(&value)).await?;
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
            Duration::from_millis(250),
            send(&mut socket, json!({"t":"bye", "reason":reason})),
        )
        .await;
    }
    let _ = timeout(Duration::from_millis(250), async {
        socket.close(None).await?;
        while let Some(frame) = socket.next().await {
            if matches!(frame?, Message::Close(_)) {
                break;
            }
        }
        Ok::<_, tokio_tungstenite::tungstenite::Error>(())
    })
    .await;
    let _ = tx.send(Msg::Remote(RemoteEvent::Disconnected(id)));
    result
}

async fn filesystem(
    id: String,
    request: FsRequest,
    roots: Arc<[PathBuf]>,
    controller: watch::Receiver<Option<&'static str>>,
) -> Result<Option<Value>, String> {
    let worker = controller.clone();
    let reply = tokio::task::spawn_blocking(move || {
        // A queued blocking task must not begin work after its controller was revoked.
        if worker.has_changed().is_err() || worker.borrow().is_some() {
            return None;
        }
        let result = match request {
            FsRequest::Roots => Ok(json!({"roots": fs::roots(&roots)})),
            FsRequest::List { path, hidden } => {
                fs::list(&roots, &path, hidden).map(|listing| json!({"listing": listing}))
            }
            FsRequest::Mkdir { parent, name } => {
                fs::mkdir(&roots, &parent, &name).map(|created| json!({"created": created}))
            }
        };
        let mut reply = match result {
            Ok(fields) => fields,
            Err(error) => json!({"error": error.code(), "message": error.to_string()}),
        };
        reply["t"] = json!("fs");
        reply["id"] = json!(id);
        reply["ok"] = json!(reply.get("error").is_none());
        Some(reply)
    })
    .await
    .map_err(|error| error.to_string())?;
    // Already-running filesystem calls cannot be cancelled; never deliver their late results.
    if controller.has_changed().is_err() || controller.borrow().is_some() {
        return Ok(None);
    }
    Ok(reply)
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
            Auth::load(dir.path().join("remote.json")).unwrap(),
        ));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (tx, rx) = mpsc::unbounded_channel();
        let server_auth = auth.clone();
        let (stop, stopped) = oneshot::channel();
        let roots = vec![dir.path().canonicalize().unwrap()].into();
        let task = tokio::spawn(async move {
            let _stop = stop;
            serve(listener, server_auth, tx, stopped, 0, roots).await;
        });
        (dir, address, auth, rx, task)
    }

    async fn connect(address: std::net::SocketAddr) -> WebSocketStream<TcpStream> {
        let stream = TcpStream::connect(address).await.unwrap();
        tokio_tungstenite::client_async(format!("ws://{address}/ws"), stream)
            .await
            .unwrap()
            .0
    }

    /// A socket that has sent a password sign-in.
    async fn login(
        address: std::net::SocketAddr,
        password: &str,
        device: &str,
    ) -> WebSocketStream<TcpStream> {
        let mut socket = connect(address).await;
        send(
            &mut socket,
            json!({"t":"login", "v":1, "password":password, "device":device}),
        )
        .await
        .unwrap();
        socket
    }

    async fn receive(socket: &mut WebSocketStream<TcpStream>) -> Value {
        serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap()
    }

    async fn get(address: std::net::SocketAddr, request: &str) -> Vec<u8> {
        let mut socket = TcpStream::connect(address).await.unwrap();
        socket.write_all(request.as_bytes()).await.unwrap();
        let mut reply = Vec::new();
        socket.read_to_end(&mut reply).await.unwrap();
        reply
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
            let reply = String::from_utf8(get(address, request).await).unwrap();
            assert!(reply.contains(status), "{reply}");
            assert!(reply.contains(CSP));
            if status == "200 OK" {
                assert!(reply.ends_with(include_str!("web/index.html")));
                assert!(reply.contains("minicore.token"));
            }
        }
        task.abort();
    }

    #[tokio::test]
    async fn the_home_screen_icon_is_a_png() {
        let (_dir, address, _auth, _rx, task) = start().await;
        let reply = get(address, "GET /icon.png HTTP/1.1\r\nHost: localhost\r\n\r\n").await;
        let end = reply.windows(4).position(|s| s == b"\r\n\r\n").unwrap();
        let head = String::from_utf8_lossy(&reply[..end]);
        assert!(head.starts_with("HTTP/1.1 200 OK"), "{head}");
        assert!(head.contains("Content-Type: image/png"), "{head}");
        assert!(reply[end + 4..].starts_with(b"\x89PNG\r\n\x1a\n"));
        task.abort();
    }

    #[test]
    fn origins_match_host_port_ipv6_and_case_without_accepting_foreign_authorities() {
        for (origin, host, allowed) in [
            ("https://host:8765", "host:8765", true),
            ("HTTP://HOST:8765", "host:8765", true),
            ("http://[::1]:8765", "[::1]:8765", true),
            ("http://[::1]:8765", "[::1]:80", false),
            ("https://host:8765", "host", false),
            ("https://evil@host:8765", "host:8765", false),
            ("null", "host", false),
        ] {
            assert_eq!(
                same_origin(origin.as_bytes(), Some(host.as_bytes())),
                allowed,
                "{origin}"
            );
        }
        assert!(!same_origin(b"https://host", None));
    }

    #[tokio::test]
    async fn wrong_passwords_are_refused_reported_and_then_paused() {
        let (_dir, address, auth, mut rx, task) = start().await;
        for attempt in 1..=5 {
            let mut socket = login(address, "wrong-password", "phone").await;
            let reply = receive(&mut socket).await;
            let Msg::Remote(RemoteEvent::LoginRefused { locked }) = rx.recv().await.unwrap() else {
                panic!("a refusal event")
            };
            if attempt < 5 {
                assert_eq!(reply["code"], "wrong_password", "{reply}");
                assert_eq!(reply["message"], "Wrong password");
                assert!(locked.is_none());
            } else {
                assert_eq!(reply["code"], "locked", "{reply}");
                assert_eq!(reply["retry_after"], 30);
                assert_eq!(locked, Some(Duration::from_secs(30)));
            }
        }
        let right = auth.lock().unwrap().password().to_owned();
        let mut socket = login(address, &right, "phone").await;
        assert_eq!(receive(&mut socket).await["code"], "locked");
        task.abort();
    }

    #[tokio::test]
    async fn a_busy_refusal_remembers_no_phone() {
        let (dir, address, auth, mut rx, task) = start().await;
        let password = auth.lock().unwrap().password().to_owned();
        let mut socket = login(address, &password, "other phone").await;
        let candidate = rx.recv().await.unwrap();
        let (tx, _messages) = mpsc::unbounded_channel();
        let (_fake, mut app) = crate::testing::FakeAgent::start(tx);
        app.remote.url = Some(format!("http://{address}"));
        let (outbound, _output) = mpsc::channel(512);
        let (close, _closing) = watch::channel(None);
        app.remote.connection = Some(Connection {
            id: 0,
            generation: 0,
            device: super::super::auth::Device {
                name: "existing phone".into(),
                token_sha256: "existing hash".into(),
                paired_at: String::new(),
            },
            outbound,
            close,
        });
        app.on_msg(candidate);
        assert_eq!(receive(&mut socket).await["code"], "busy");
        let stored: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join("remote.json")).unwrap())
                .unwrap();
        assert_eq!(stored["devices"], json!([]));
        task.abort();
    }

    #[tokio::test]
    async fn invalid_device_names_and_pre_auth_filesystem_requests_are_refused() {
        let (_dir, address, auth, mut rx, task) = start().await;
        let password = auth.lock().unwrap().password().to_owned();
        let mut requests = vec![json!({"t":"fs_roots", "id":"early"})];
        for device in ["".to_owned(), "bad\nname".into(), "x".repeat(65)] {
            requests.push(json!({"t":"login", "v":1, "password":password, "device":device}));
        }
        for request in requests {
            let mut socket = connect(address).await;
            send(&mut socket, request).await.unwrap();
            assert_eq!(receive(&mut socket).await["code"], "bad_request");
        }
        assert!(rx.try_recv().is_err(), "not counted as wrong passwords");
        task.abort();
    }

    /// Signs in with the right password and admits the phone as the reducer would.
    async fn admitted(
        address: std::net::SocketAddr,
        auth: &Arc<Mutex<Auth>>,
        rx: &mut mpsc::UnboundedReceiver<Msg>,
    ) -> (WebSocketStream<TcpStream>, Connection) {
        let password = auth.lock().unwrap().password().replace('-', " ");
        let socket = login(address, &password, "phone").await;
        let Msg::Remote(RemoteEvent::Connect {
            connection,
            accepted,
        }) = rx.recv().await.unwrap()
        else {
            panic!("admission")
        };
        accepted.send(true).unwrap();
        connection.send(json!({"t":"snapshot", "state":{}, "blocks":[]}));
        (socket, connection)
    }

    #[tokio::test]
    async fn an_authenticated_controller_browses_and_creates_directories_only_inside_roots() {
        let (dir, address, auth, mut rx, task) = start().await;
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("existing")).unwrap();
        let outside = tempfile::tempdir().unwrap();
        let (mut socket, connection) = admitted(address, &auth, &mut rx).await;
        for kind in ["authorized", "welcome", "snapshot"] {
            assert_eq!(receive(&mut socket).await["t"], kind);
        }
        send(&mut socket, json!({"t":"fs_roots", "id":"roots"}))
            .await
            .unwrap();
        assert_eq!(
            receive(&mut socket).await,
            json!({"t":"fs", "id":"roots", "ok":true, "roots":fs::roots(std::slice::from_ref(&root))})
        );
        send(
            &mut socket,
            json!({"t":"fs_list", "id":"list", "path":fs::display(&root), "hidden":false}),
        )
        .await
        .unwrap();
        let listing = receive(&mut socket).await;
        assert_eq!(listing["t"], "fs");
        assert_eq!(listing["id"], "list");
        assert_eq!(listing["ok"], true);
        assert_eq!(
            listing["listing"]["entries"],
            json!([{"name":"existing", "path":fs::display(&root.join("existing"))}])
        );
        send(
            &mut socket,
            json!({"t":"fs_mkdir", "id":"mkdir", "parent":fs::display(&root), "name":"created"}),
        )
        .await
        .unwrap();
        assert_eq!(
            receive(&mut socket).await,
            json!({"t":"fs", "id":"mkdir", "ok":true, "created":fs::display(&root.join("created"))})
        );
        assert!(root.join("created").is_dir());
        for request in [
            json!({"t":"fs_list", "id":"outside", "path":fs::display(outside.path()), "hidden":false}),
            json!({"t":"fs_mkdir", "id":"outside", "parent":fs::display(outside.path()), "name":"escape"}),
        ] {
            send(&mut socket, request).await.unwrap();
            assert_eq!(
                receive(&mut socket).await,
                json!({"t":"fs", "id":"outside", "ok":false, "error":"outside_roots", "message":fs::FsError::OutsideRoots.to_string()})
            );
        }
        assert!(!outside.path().join("escape").exists());
        for request in [
            json!({"t":"fs_list", "id":"bad", "path":fs::display(&root)}),
            json!({"t":"fs_mkdir", "id":"bad", "parent":fs::display(&root)}),
            json!({"t":"fs_unknown", "id":"bad"}),
        ] {
            send(&mut socket, request).await.unwrap();
            assert_eq!(
                receive(&mut socket).await,
                json!({"t":"fs", "id":"bad", "ok":false, "error":"bad_request"})
            );
        }
        connection.bye("replaced");
        let _ = send(
            &mut socket,
            json!({"t":"fs_mkdir", "id":"stale", "parent":fs::display(&root), "name":"stale"}),
        )
        .await;
        assert_eq!(
            receive(&mut socket).await,
            json!({"t":"bye", "reason":"replaced"})
        );
        assert!(matches!(socket.next().await, Some(Ok(Message::Close(_)))));
        assert!(!root.join("stale").exists());
        task.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn revoked_controllers_cannot_start_queued_filesystem_work() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let roots: Arc<[PathBuf]> = vec![root.clone()].into();
        for reason in ["replaced", "remote_off"] {
            let (close, controller) = watch::channel(None);
            close.send(Some(reason)).unwrap();
            let reply = filesystem(
                "stale".into(),
                FsRequest::Mkdir {
                    parent: fs::display(&root),
                    name: reason.into(),
                },
                roots.clone(),
                controller,
            )
            .await
            .unwrap();
            assert!(reply.is_none());
            assert!(!root.join(reason).exists());
        }
    }

    #[tokio::test]
    async fn the_right_password_is_authorized_before_welcome_and_the_snapshot_and_unknown_tokens_are_not()
     {
        let (_dir, address, auth, mut rx, task) = start().await;
        let mut socket = connect(address).await;
        send(&mut socket, json!({"t":"hello", "v":1, "token":"unknown"}))
            .await
            .unwrap();
        assert_eq!(receive(&mut socket).await["code"], "unauthorized");
        let (mut socket, _connection) = admitted(address, &auth, &mut rx).await;
        let mut token = String::new();
        for kind in ["authorized", "welcome", "snapshot"] {
            let reply = receive(&mut socket).await;
            assert_eq!(reply["t"], kind);
            if kind == "authorized" {
                token = reply["token"].as_str().unwrap().to_owned();
            } else if kind == "welcome" {
                assert_eq!(reply["v"], wire::VERSION);
                assert_eq!(reply["version"], env!("CARGO_PKG_VERSION"));
                assert_eq!(reply["device"], "phone");
                assert!(reply["now_ms"].as_u64().expect("host Unix milliseconds") > 0);
            } else {
                assert!(reply.get("now_ms").is_none());
            }
        }
        assert_eq!(auth.lock().unwrap().verify(&token).unwrap().name, "phone");
        task.abort();
    }
}
