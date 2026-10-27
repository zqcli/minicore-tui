//! Bounded HTTP and WebSocket connections, with reducer-authorized admission.

use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpListener;
use tokio::sync::{Semaphore, mpsc, oneshot, watch};
use tokio::task::JoinSet;
use tokio::time::{Instant, timeout};
use tokio_rustls::TlsAcceptor;
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
    wire::{self, FsRequest, Handshake, Query},
};
use crate::app::{Msg, Tx};

const HEAD_LIMIT: usize = 8192;
const FIRST: Duration = Duration::from_secs(10);
const PING: Duration = Duration::from_secs(20);
const IDLE: Duration = Duration::from_secs(60);
pub const CSP: &str =
    "default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'";
const HEADERS: [(&str, &str); 3] = [
    ("X-Content-Type-Options", "nosniff"),
    ("Referrer-Policy", "no-referrer"),
    ("Content-Security-Policy", CSP),
];
// `no-transform` keeps proxies such as Cloudflare from rewriting the page and its scripts.
const NO_STORE: &str = "no-store, no-transform";
/// Files are revalidated on every load, so a reload over a slow link costs a `304` per file.
const REVALIDATE: &str = "Cache-Control: no-cache, no-transform\r\n";
/// Replies are written in pieces of this size, each given `FIRST` to drain.
const CHUNK: usize = 4096;
const HTML: &str = "text/html; charset=utf-8";
const JS: &str = "text/javascript; charset=utf-8";
const CSS: &str = "text/css; charset=utf-8";
const PNG: &str = "image/png";

macro_rules! files {
    ($(($path:literal, $mime:expr, $file:literal)),* $(,)?) => {
        &[$(($path, $mime, include_bytes!(concat!("web/", $file)) as &[u8])),*]
    };
}
/// The web client's files: path, MIME type and body.
const FILES: &[(&str, &str, &[u8])] = files![
    ("/", HTML, "index.html"),
    ("/app.css", CSS, "app.css"),
    ("/theme.js", JS, "theme.js"),
    ("/main.js", JS, "main.js"),
    ("/core.js", JS, "core.js"),
    ("/ui.js", JS, "ui.js"),
    ("/net.js", JS, "net.js"),
    ("/sessions.js", JS, "sessions.js"),
    ("/conversation.js", JS, "conversation.js"),
    ("/sw.js", JS, "sw.js"),
    (
        "/manifest.webmanifest",
        "application/manifest+json",
        "manifest.webmanifest"
    ),
    ("/icon.svg", "image/svg+xml", "icon.svg"),
    ("/icon-192.png", PNG, "icon-192.png"),
    ("/icon-512.png", PNG, "icon-512.png"),
    ("/icon-maskable.png", PNG, "icon-maskable.png"),
    ("/apple-touch-icon.png", PNG, "apple-touch-icon.png"),
];

/// What a file's bytes give once: the validator and, for text, the gzipped copy.
struct Derived {
    hash: String,
    gzip: Option<Vec<u8>>,
}

static DERIVED: [OnceLock<Derived>; FILES.len()] = [const { OnceLock::new() }; FILES.len()];

fn derive(index: usize) -> &'static Derived {
    DERIVED[index].get_or_init(|| {
        let (_, mime, body) = FILES[index];
        Derived {
            hash: format!("{:x}", Sha256::digest(body))[..16].to_owned(),
            gzip: compressible(mime).then(|| gzip(body)),
        }
    })
}

/// PNGs are compressed already.
fn compressible(mime: &str) -> bool {
    mime != PNG
}

fn gzip(body: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    encoder.write_all(body).unwrap();
    encoder.finish().unwrap()
}

fn accepts_gzip(value: Option<&[u8]>) -> bool {
    value
        .and_then(|value| std::str::from_utf8(value).ok())
        .is_some_and(|value| {
            value.split(',').any(|item| {
                let mut parts = item.split(';');
                parts.next().unwrap().trim().eq_ignore_ascii_case("gzip")
                    && parts.all(|part| {
                        part.trim()
                            .strip_prefix("q=")
                            .is_none_or(|q| q.parse::<f32>().is_ok_and(|q| q > 0.0))
                    })
            })
        })
}

/// Whether an `If-None-Match` list holds the validator (weak comparison, as `304` allows).
fn none_match(value: Option<&[u8]>, etag: &str) -> bool {
    value
        .and_then(|value| std::str::from_utf8(value).ok())
        .is_some_and(|value| {
            value
                .split(',')
                .any(|item| item.trim().trim_start_matches("W/") == etag)
        })
}

pub async fn serve(
    listeners: Vec<TcpListener>,
    tls: Option<TlsAcceptor>,
    auth: Arc<Mutex<Auth>>,
    tx: Tx,
    mut stop: oneshot::Receiver<()>,
    generation: u64,
    roots: Arc<[PathBuf]>,
) {
    let permits = Arc::new(Semaphore::new(32));
    let mut tasks = JoinSet::new();
    // Polling every listener registers a wakeup on each.
    let accept = |cx: &mut Context| {
        listeners
            .iter()
            .find_map(|listener| match listener.poll_accept(cx) {
                Poll::Ready(accepted) => Some(accepted),
                Poll::Pending => None,
            })
            .map_or(Poll::Pending, Poll::Ready)
    };
    loop {
        tokio::select! {
            _ = &mut stop => break,
            accepted = std::future::poll_fn(accept) => {
                let Ok((stream, _)) = accepted else {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                };
                let Ok(permit) = permits.clone().try_acquire_owned() else { continue };
                let auth = auth.clone();
                let tx = tx.clone();
                let roots = roots.clone();
                let tls = tls.clone();
                tasks.spawn(async move {
                    let _ = match tls {
                        None => connection(stream, auth, tx, permit, generation, roots).await,
                        // A failed handshake, such as plain HTTP to this port, just closes.
                        Some(tls) => match timeout(FIRST, tls.accept(stream)).await {
                            Ok(Ok(stream)) => connection(stream, auth, tx, permit, generation, roots).await,
                            _ => Ok(()),
                        },
                    };
                });
            }
            _ = tasks.join_next(), if !tasks.is_empty() => {}
        }
    }
    drop(listeners);
    let _ = timeout(FIRST, async { while tasks.join_next().await.is_some() {} }).await;
}

/// Reads until `buffer` holds a whole request head and returns its length; the bytes may run past it.
async fn head(stream: &mut (impl AsyncRead + Unpin), buffer: &mut Vec<u8>) -> io::Result<usize> {
    loop {
        if let Some(end) = buffer.windows(4).position(|s| s == b"\r\n\r\n") {
            return Ok(end + 4);
        }
        if buffer.len() >= HEAD_LIMIT {
            return Err(io::Error::other("request head too large"));
        }
        let mut chunk = [0; 2048];
        match stream.read(&mut chunk).await? {
            0 => return Err(io::Error::from(io::ErrorKind::UnexpectedEof)),
            n => buffer.extend_from_slice(&chunk[..n]),
        }
    }
}

/// A stream that yields the bytes already read from it before reading on.
struct Replay<S> {
    read: Vec<u8>,
    at: usize,
    inner: S,
}

impl<S: AsyncRead + Unpin> AsyncRead for Replay<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let rest = &this.read[this.at..];
        if rest.is_empty() {
            return Pin::new(&mut this.inner).poll_read(cx, buf);
        }
        let n = rest.len().min(buf.remaining());
        buf.put_slice(&rest[..n]);
        this.at += n;
        Poll::Ready(Ok(()))
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Replay<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

/// Writes a reply, dropping the link only when it stops moving: a slow proxy hop may need minutes.
/// The connection ends after a reply that says `close`; otherwise it stays for the next request.
async fn http(
    stream: &mut (impl AsyncWrite + Unpin),
    status: &str,
    headers: &str,
    body: &[u8],
    close: bool,
) -> io::Result<()> {
    let security: String = HEADERS
        .iter()
        .map(|(name, value)| format!("{name}: {value}\r\n"))
        .collect();
    // A `304` carries no body and no length of its own.
    let length = if status.starts_with("304") {
        String::new()
    } else {
        format!("Content-Length: {}\r\n", body.len())
    };
    let ending = if close { "Connection: close\r\n" } else { "" };
    let head = format!("HTTP/1.1 {status}\r\n{security}{headers}{length}{ending}\r\n");
    let stalled = || io::Error::from(io::ErrorKind::TimedOut);
    for chunk in std::iter::once(head.as_bytes()).chain(body.chunks(CHUNK)) {
        timeout(FIRST, stream.write_all(chunk))
            .await
            .map_err(|_| stalled())??;
    }
    // Over TLS a closing shutdown sends close_notify, without which the reply looks truncated.
    timeout(FIRST, async {
        if close {
            stream.shutdown().await
        } else {
            stream.flush().await
        }
    })
    .await
    .map_err(|_| stalled())?
}

/// What the web client's files and the refusals answer: everything but the socket.
struct Reply {
    status: &'static str,
    headers: String,
    body: &'static [u8],
    /// Whether the connection stays open for the next request.
    keep: bool,
}

/// A short reply that must not be cached.
fn refusal(status: &'static str, text: &'static str, keep: bool) -> Reply {
    Reply {
        status,
        headers: format!("Cache-Control: {NO_STORE}\r\nContent-Type: text/plain\r\n"),
        body: text.as_bytes(),
        keep,
    }
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

/// The reply to a request, or `None` for a socket upgrade that passed the origin check.
fn route(request: &httparse::Request) -> Option<Reply> {
    let header = |name: &str| {
        request
            .headers
            .iter()
            .find(|header| header.name.eq_ignore_ascii_case(name))
            .map(|header| header.value)
    };
    let path = request
        .path
        .map(|path| path.split_once('?').map_or(path, |(path, _)| path));
    let get = request.method == Some("GET");
    // Only a GET has no body to skip, and HTTP/1.0 closes unless asked otherwise.
    let keep = get
        && request.version == Some(1)
        && !header("connection").is_some_and(|value| {
            String::from_utf8_lossy(value)
                .split(',')
                .any(|token| token.trim().eq_ignore_ascii_case("close"))
        });
    if get
        && path == Some("/ws")
        && header("upgrade").is_some_and(|v| v.eq_ignore_ascii_case(b"websocket"))
    {
        return header("origin")
            .is_some_and(|origin| !same_origin(origin, header("host")))
            .then(|| refusal("403 Forbidden", "Forbidden", false));
    }
    let Some(index) = FILES.iter().position(|file| get && path == Some(file.0)) else {
        return Some(refusal("404 Not Found", "Not found", keep));
    };
    let (_, mime, body) = FILES[index];
    let derived = derive(index);
    let gzip = derived
        .gzip
        .as_deref()
        .filter(|_| accepts_gzip(header("accept-encoding")));
    // A strong validator names one representation, so the gzipped bytes get their own.
    let etag = format!(
        "\"{}{}\"",
        derived.hash,
        if gzip.is_some() { "-gzip" } else { "" }
    );
    let mut headers = format!("{REVALIDATE}ETag: {etag}\r\n");
    if compressible(mime) {
        headers += "Vary: Accept-Encoding\r\n";
    }
    if none_match(header("if-none-match"), &etag) {
        return Some(Reply {
            status: "304 Not Modified",
            headers,
            body: b"",
            keep,
        });
    }
    headers += &format!("Content-Type: {mime}\r\n");
    if gzip.is_some() {
        headers += "Content-Encoding: gzip\r\n";
    }
    Some(Reply {
        status: "200 OK",
        headers,
        body: gzip.unwrap_or(body),
        keep,
    })
}

async fn connection<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    auth: Arc<Mutex<Auth>>,
    tx: Tx,
    permit: tokio::sync::OwnedSemaphorePermit,
    generation: u64,
    roots: Arc<[PathBuf]>,
) -> Result<(), String> {
    // Requests are served one after another on the connection until one asks for the socket; a client
    // that sends nothing for `FIRST` between them is dropped.
    let mut buffer = Vec::new();
    loop {
        let end = timeout(FIRST, head(&mut stream, &mut buffer))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
        let mut headers = [httparse::EMPTY_HEADER; 128];
        let mut request = httparse::Request::new(&mut headers);
        request.parse(&buffer[..end]).map_err(|e| e.to_string())?;
        let Some(reply) = route(&request) else { break };
        http(
            &mut stream,
            reply.status,
            &reply.headers,
            reply.body,
            !reply.keep,
        )
        .await
        .map_err(|e| e.to_string())?;
        if !reply.keep {
            return Ok(());
        }
        buffer.drain(..end);
    }
    let config = WebSocketConfig::default()
        .max_message_size(Some(1024 * 1024))
        .max_frame_size(Some(1024 * 1024));
    // Tungstenite parses the request head itself.
    let stream = Replay {
        read: buffer,
        at: 0,
        inner: stream,
    };
    let mut socket = timeout(
        FIRST,
        accept_hdr_async_with_config(
            stream,
            #[allow(clippy::result_large_err)] // Tungstenite fixes the callback's error type.
            |_: &Request, mut response: Response| {
                for (name, value) in HEADERS.into_iter().chain([("Cache-Control", NO_STORE)]) {
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
    // Only a listener that is stopping refuses an authenticated device.
    if !admission.await.unwrap_or(false) {
        return send(&mut socket, json!({"t":"bye", "reason":"remote_off"})).await;
    }
    drop(permit);
    let revoked = closing.clone();
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
                                } else if value["t"] == "query" {
                                    if let Ok(query) = Query::deserialize(&value) {
                                        tx.send(Msg::Remote(RemoteEvent::Query { connection: id, id: request_id.to_owned(), query })).map_err(|_| "TUI closed")?;
                                        continue;
                                    }
                                } else if let Ok(request) = FsRequest::deserialize(&value) {
                                    let reply = filesystem(request_id.to_owned(), request, roots.clone(), revoked.clone()).await?;
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
    revoked: watch::Receiver<Option<&'static str>>,
) -> Result<Option<Value>, String> {
    let worker = revoked.clone();
    let reply = tokio::task::spawn_blocking(move || {
        // A queued blocking task must not begin work after its connection was revoked.
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
    if revoked.has_changed().is_err() || revoked.borrow().is_some() {
        return Ok(None);
    }
    Ok(reply)
}

async fn send<S: AsyncRead + AsyncWrite + Unpin>(
    socket: &mut WebSocketStream<S>,
    value: Value,
) -> Result<(), String> {
    timeout(IDLE, socket.send(Message::Text(value.to_string().into())))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

async fn reject<S: AsyncRead + AsyncWrite + Unpin>(
    socket: &mut WebSocketStream<S>,
    code: &str,
) -> Result<(), String> {
    send(socket, wire::error(code, code)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpStream;

    async fn start() -> (
        tempfile::TempDir,
        std::net::SocketAddr,
        Arc<Mutex<Auth>>,
        mpsc::UnboundedReceiver<Msg>,
        tokio::task::JoinHandle<()>,
    ) {
        serving(None).await
    }

    async fn serving(
        tls: Option<Arc<rustls::ServerConfig>>,
    ) -> (
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
            let tls = tls.map(TlsAcceptor::from);
            serve(vec![listener], tls, server_auth, tx, stopped, 0, roots).await;
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
            json!({"t":"login", "v":wire::VERSION, "password":password, "device":device}),
        )
        .await
        .unwrap();
        socket
    }

    async fn receive<S: AsyncRead + AsyncWrite + Unpin>(socket: &mut WebSocketStream<S>) -> Value {
        serde_json::from_str(socket.next().await.unwrap().unwrap().to_text().unwrap()).unwrap()
    }

    /// One request on its own connection, read until the server closes it.
    async fn get(address: std::net::SocketAddr, request: &str) -> Vec<u8> {
        let mut socket = TcpStream::connect(address).await.unwrap();
        let request = request.replacen("\r\n\r\n", "\r\nConnection: close\r\n\r\n", 1);
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
            (
                "GET /ws?v=1 HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nOrigin: https://foreign.example\r\n\r\n",
                "403 Forbidden",
            ),
        ] {
            let reply = String::from_utf8(get(address, request).await).unwrap();
            assert!(reply.contains(status), "{reply}");
            assert!(reply.contains(CSP));
            if status == "200 OK" {
                assert!(reply.ends_with(include_str!("web/index.html")));
                assert!(reply.contains(r#"<script type="module" src="/main.js">"#));
            }
        }
        task.abort();
    }

    #[tokio::test]
    async fn every_file_in_the_web_directory_is_served_with_its_type_and_bytes_even_with_a_query_string()
     {
        let (_dir, address, _auth, _rx, task) = start().await;
        let mut seen = 0;
        for entry in std::fs::read_dir("src/remote/web").unwrap() {
            let name = entry.unwrap().file_name().into_string().unwrap();
            let body = std::fs::read(format!("src/remote/web/{name}")).unwrap();
            let path = if name == "index.html" {
                "/".to_owned()
            } else {
                format!("/{name}")
            };
            let mime = match name.rsplit('.').next().unwrap() {
                "html" => "text/html; charset=utf-8",
                "js" | "mjs" => "text/javascript; charset=utf-8",
                "css" => "text/css; charset=utf-8",
                "webmanifest" => "application/manifest+json",
                "svg" => "image/svg+xml",
                "png" => "image/png",
                other => panic!("no type for .{other}"),
            };
            let request = format!("GET {path}?v=2 HTTP/1.1\r\nHost: localhost\r\n\r\n");
            let reply = get(address, &request).await;
            let end = reply.windows(4).position(|s| s == b"\r\n\r\n").unwrap();
            let head = String::from_utf8_lossy(&reply[..end]);
            assert!(head.starts_with("HTTP/1.1 200 OK"), "{path}: {head}");
            assert!(
                head.contains(&format!("Content-Type: {mime}\r\n")),
                "{head}"
            );
            assert_eq!(&reply[end + 4..], body, "{path}");
            seen += 1;
        }
        assert_eq!(
            seen,
            FILES.len(),
            "a file is missing from FILES or the directory"
        );
        task.abort();
    }

    #[tokio::test]
    async fn a_file_revalidates_to_a_bodyless_304_by_its_own_validator_for_each_encoding() {
        let (_dir, address, _auth, _rx, task) = start().await;
        let fetch = |path: &'static str, encoding: &'static str, none_match: Option<String>| async move {
            let condition =
                none_match.map_or(String::new(), |etag| format!("If-None-Match: {etag}\r\n"));
            let request = format!(
                "GET {path} HTTP/1.1\r\nHost: localhost\r\nAccept-Encoding: {encoding}\r\n{condition}\r\n"
            );
            let reply = get(address, &request).await;
            let end = reply.windows(4).position(|s| s == b"\r\n\r\n").unwrap();
            (
                String::from_utf8(reply[..end].to_vec()).unwrap(),
                reply[end + 4..].to_vec(),
            )
        };
        let etag = |head: &str| {
            head.lines()
                .find_map(|line| line.strip_prefix("ETag: "))
                .unwrap()
                .to_owned()
        };
        for path in ["/", "/sw.js", "/icon-192.png"] {
            let (head, body) = fetch(path, "identity", None).await;
            let plain = etag(&head);
            assert!(head.starts_with("HTTP/1.1 200 OK"), "{head}");
            assert!(
                head.contains("Cache-Control: no-cache, no-transform\r\n"),
                "{head}"
            );
            assert_eq!(plain.len(), 18, "a quoted 16-digit hash: {plain}");
            assert!(!body.is_empty());
            for condition in [
                plain.clone(),
                format!("W/{plain}"),
                format!("\"other\", {plain}"),
            ] {
                let (head, body) = fetch(path, "identity", Some(condition)).await;
                assert!(
                    head.starts_with("HTTP/1.1 304 Not Modified"),
                    "{path}: {head}"
                );
                assert_eq!(etag(&head), plain);
                assert!(head.contains("Cache-Control: no-cache, no-transform\r\n"));
                assert!(!head.contains("Content-Length"), "{head}");
                assert!(body.is_empty());
            }
            let (head, _) = fetch(path, "identity", Some("\"stale\"".into())).await;
            assert!(head.starts_with("HTTP/1.1 200 OK"), "{path}: {head}");
            if path == "/icon-192.png" {
                continue;
            }
            // The gzipped bytes are another representation with another validator.
            let (head, _) = fetch(path, "gzip", None).await;
            let gzipped = etag(&head);
            assert_ne!(gzipped, plain);
            assert!(head.contains("Content-Encoding: gzip\r\n"));
            let (head, body) = fetch(path, "gzip", Some(gzipped.clone())).await;
            assert!(head.starts_with("HTTP/1.1 304 Not Modified"), "{head}");
            assert!(body.is_empty());
            assert!(head.contains("Vary: Accept-Encoding\r\n"));
            let (head, _) = fetch(path, "gzip", Some(plain)).await;
            assert!(head.starts_with("HTTP/1.1 200 OK"), "{head}");
        }
        task.abort();
    }

    #[tokio::test]
    async fn a_first_load_may_open_every_connection_at_once() {
        let (_dir, address, _auth, _rx, task) = start().await;
        // Behind a tunnel every file of a first load arrives on its own connection, all at once.
        let mut sockets = Vec::new();
        // The socket and the service worker come on top of the files.
        for _ in 0..FILES.len() + 2 {
            sockets.push(TcpStream::connect(address).await.unwrap());
        }
        for socket in &mut sockets {
            let request =
                b"GET /manifest.webmanifest HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";
            socket.write_all(request).await.unwrap();
        }
        for socket in &mut sockets {
            let mut reply = Vec::new();
            socket.read_to_end(&mut reply).await.unwrap();
            assert!(reply.starts_with(b"HTTP/1.1 200 OK"));
        }
        task.abort();
    }

    /// One reply read off a connection that stays open: the head and a body of the announced length.
    async fn reply(socket: &mut (impl AsyncRead + Unpin)) -> (String, Vec<u8>) {
        let mut bytes = Vec::new();
        let end = head(socket, &mut bytes).await.unwrap();
        let head = String::from_utf8(bytes[..end].to_vec()).unwrap();
        let length = head
            .lines()
            .find_map(|line| line.strip_prefix("Content-Length: "))
            .map_or(0, |length| length.parse::<usize>().unwrap());
        let mut body = bytes.split_off(end);
        let have = body.len();
        body.resize(length, 0);
        socket.read_exact(&mut body[have..]).await.unwrap();
        (head, body)
    }

    #[tokio::test]
    async fn several_requests_share_one_connection_until_one_says_close_or_asks_for_the_socket() {
        let (_dir, address, _auth, _rx, task) = start().await;
        let mut socket = TcpStream::connect(address).await.unwrap();
        let ask = |path: &str, extra: &str| {
            format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n{extra}\r\n")
        };
        socket
            .write_all(ask("/icon.svg", "").as_bytes())
            .await
            .unwrap();
        let (first, body) = reply(&mut socket).await;
        assert!(first.starts_with("HTTP/1.1 200 OK"), "{first}");
        assert!(!first.contains("Connection:"), "{first}");
        assert_eq!(body, include_bytes!("web/icon.svg"));
        // The next request on the same connection revalidates and is answered by a bodyless 304.
        let etag = first
            .lines()
            .find_map(|line| line.strip_prefix("ETag: "))
            .unwrap();
        let condition = format!("If-None-Match: {etag}\r\n");
        socket
            .write_all(ask("/icon.svg", &condition).as_bytes())
            .await
            .unwrap();
        let (second, body) = reply(&mut socket).await;
        assert!(second.starts_with("HTTP/1.1 304 Not Modified"), "{second}");
        assert!(body.is_empty() && !second.contains("Connection:"));
        // An unknown path is answered and the connection stays.
        socket
            .write_all(ask("/missing", "").as_bytes())
            .await
            .unwrap();
        assert!(
            reply(&mut socket)
                .await
                .0
                .starts_with("HTTP/1.1 404 Not Found")
        );
        // The socket upgrade comes on the same connection too.
        let (mut ws, _) = tokio_tungstenite::client_async(format!("ws://{address}/ws"), socket)
            .await
            .unwrap();
        send(
            &mut ws,
            json!({"t":"hello", "v":wire::VERSION, "token":"unknown"}),
        )
        .await
        .unwrap();
        assert_eq!(receive(&mut ws).await["code"], "unauthorized");

        // `Connection: close` is answered with the same header and the end of the connection.
        let mut socket = TcpStream::connect(address).await.unwrap();
        socket
            .write_all(ask("/icon.svg", "").as_bytes())
            .await
            .unwrap();
        reply(&mut socket).await;
        socket
            .write_all(ask("/icon.svg", "Connection: close\r\n").as_bytes())
            .await
            .unwrap();
        let mut rest = Vec::new();
        socket.read_to_end(&mut rest).await.unwrap();
        let rest = String::from_utf8(rest).unwrap();
        assert!(rest.starts_with("HTTP/1.1 200 OK") && rest.contains("Connection: close\r\n"));
        task.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn a_connection_that_sends_no_next_request_is_closed_after_the_idle_wait() {
        let dir = tempfile::tempdir().unwrap();
        let auth = Arc::new(Mutex::new(
            Auth::load(dir.path().join("remote.json")).unwrap(),
        ));
        let (tx, _rx) = mpsc::unbounded_channel();
        let permit = Arc::new(Semaphore::new(1)).try_acquire_owned().unwrap();
        let (server, mut client) = tokio::io::duplex(64 * 1024);
        let task = tokio::spawn(connection(server, auth, tx, permit, 0, Vec::new().into()));
        client
            .write_all(b"GET /icon.svg HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        assert!(reply(&mut client).await.0.starts_with("HTTP/1.1 200 OK"));
        tokio::time::sleep(FIRST - Duration::from_secs(1)).await;
        assert!(!task.is_finished(), "closed before the idle wait ran out");
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert!(task.is_finished());
        let mut rest = Vec::new();
        client.read_to_end(&mut rest).await.unwrap();
        assert!(rest.is_empty());
    }

    #[test]
    fn the_policy_forbids_inline_code_because_the_page_has_none() {
        assert!(!CSP.contains("unsafe-inline"));
        let page = include_str!("web/index.html");
        assert!(!page.contains("<style") && !page.contains(" style="));
        for script in page.split("<script").skip(1) {
            let tag = &script[..script.find('>').unwrap()];
            assert!(tag.contains(" src="), "an inline script: <script{tag}>");
        }
    }

    #[test]
    fn the_service_worker_precaches_every_file_it_does_not_cache_at_runtime() {
        let worker = include_str!("web/sw.js");
        for (path, ..) in FILES.iter().filter(|file| file.0 != "/sw.js") {
            assert!(
                worker.contains(&format!("'{path}'")),
                "{path} is not in SHELL"
            );
        }
    }

    #[tokio::test]
    async fn text_files_are_gzipped_for_clients_that_accept_it_and_pngs_never_are() {
        use std::io::Read;
        let (_dir, address, _auth, _rx, task) = start().await;
        for (path, encoding, gzipped) in [
            ("/", "gzip, deflate, br", true),
            ("/sw.js", "br;q=1, GZIP;q=0.5", true),
            ("/", "gzip;q=0", false),
            ("/", "br", false),
            ("/icon-192.png", "gzip", false),
        ] {
            let request = format!(
                "GET {path} HTTP/1.1\r\nHost: localhost\r\nAccept-Encoding: {encoding}\r\n\r\n"
            );
            let reply = get(address, &request).await;
            let end = reply.windows(4).position(|s| s == b"\r\n\r\n").unwrap();
            let head = String::from_utf8_lossy(&reply[..end]);
            let file = FILES.iter().find(|file| file.0 == path).unwrap().2;
            assert_eq!(
                head.contains("Content-Encoding: gzip\r\n"),
                gzipped,
                "{path} {encoding}"
            );
            let mut body = Vec::new();
            if gzipped {
                let mut decoder = flate2::read::GzDecoder::new(&reply[end + 4..]);
                decoder.read_to_end(&mut body).unwrap();
                assert!(reply.len() - end - 4 < file.len(), "{path} is not smaller");
            } else {
                body.extend_from_slice(&reply[end + 4..]);
            }
            assert_eq!(body, file, "{path} {encoding}");
        }
        task.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn a_slow_link_gets_the_whole_reply_and_only_a_stalled_one_is_dropped() {
        let (mut server, mut client) = tokio::io::duplex(1024);
        let writer =
            tokio::spawn(
                async move { http(&mut server, "200 OK", "", &[b'x'; 64 * 1024], true).await },
            );
        // 512 bytes per second: the reply takes over two minutes but never stops moving.
        let (mut reply, mut buffer) = (Vec::new(), [0; 1024]);
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            match client.read(&mut buffer).await.unwrap() {
                0 => break,
                n => reply.extend_from_slice(&buffer[..n]),
            }
        }
        writer.await.unwrap().unwrap();
        assert!(reply.ends_with(&[b'x'; 64 * 1024]));

        let (mut server, _client) = tokio::io::duplex(1024);
        let error = http(&mut server, "200 OK", "", &[b'x'; 64 * 1024], true)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[tokio::test]
    async fn a_tunnelled_browser_whose_origin_matches_the_forwarded_host_reaches_sign_in() {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        let (_dir, address, _auth, _rx, task) = start().await;
        // cloudflared forwards the public Host; the browser sends the public https Origin.
        let mut request = "ws://minicore.example.com/ws"
            .into_client_request()
            .unwrap();
        let origin = "https://minicore.example.com".parse().unwrap();
        request.headers_mut().insert("Origin", origin);
        let stream = TcpStream::connect(address).await.unwrap();
        let mut socket = tokio_tungstenite::client_async(request, stream)
            .await
            .unwrap()
            .0;
        send(
            &mut socket,
            json!({"t":"hello", "v":wire::VERSION, "token":"unknown"}),
        )
        .await
        .unwrap();
        assert_eq!(receive(&mut socket).await["code"], "unauthorized");
        task.abort();
    }

    #[tokio::test]
    async fn a_replayed_stream_reads_the_consumed_bytes_first_and_writes_through() {
        let (inner, mut peer) = tokio::io::duplex(64);
        let mut stream = Replay {
            read: b"GET / ".to_vec(),
            at: 0,
            inner,
        };
        peer.write_all(b"HTTP/1.1").await.unwrap();
        peer.shutdown().await.unwrap();
        let mut start = [0; 4];
        assert_eq!(stream.read(&mut start).await.unwrap(), 4);
        assert_eq!(&start, b"GET ");
        let mut rest = Vec::new();
        stream.read_to_end(&mut rest).await.unwrap();
        assert_eq!(rest, b"/ HTTP/1.1");
        stream.write_all(b"reply").await.unwrap();
        let mut written = [0; 5];
        peer.read_exact(&mut written).await.unwrap();
        assert_eq!(&written, b"reply");
    }

    /// A TLS connection to `address` that trusts only `cert`.
    async fn tls_connect(
        address: std::net::SocketAddr,
        cert: &std::path::Path,
    ) -> tokio_rustls::client::TlsStream<TcpStream> {
        use rustls::pki_types::{CertificateDer, ServerName, pem::PemObject};
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(CertificateDer::from_pem_file(cert).unwrap())
            .unwrap();
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let stream = TcpStream::connect(address).await.unwrap();
        tokio_rustls::TlsConnector::from(Arc::new(config))
            .connect(ServerName::try_from("localhost").unwrap(), stream)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn over_tls_the_client_and_its_socket_are_served_and_plain_http_gets_no_reply() {
        use tokio_tungstenite::tungstenite::{self, client::IntoClientRequest};
        let files = tempfile::tempdir().unwrap();
        let (cert, key) = super::super::tls::self_signed(files.path(), "server");
        let config = super::super::tls::load(&cert, &key).unwrap();
        let (_dir, address, _auth, _rx, task) = serving(Some(config)).await;

        let mut stream = tls_connect(address, &cert).await;
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut reply = Vec::new();
        stream.read_to_end(&mut reply).await.unwrap();
        let reply = String::from_utf8(reply).unwrap();
        assert!(reply.starts_with("HTTP/1.1 200 OK\r\n"), "{reply}");
        assert!(reply.contains("Cache-Control: no-cache, no-transform\r\n"));
        assert!(reply.contains(CSP));
        assert!(reply.ends_with(include_str!("web/index.html")));

        let port = address.port();
        for origin in [
            format!("https://localhost:{port}"),
            "https://foreign.example".into(),
        ] {
            let mut request = format!("wss://localhost:{port}/ws")
                .into_client_request()
                .unwrap();
            request
                .headers_mut()
                .insert("Origin", origin.parse().unwrap());
            let stream = tls_connect(address, &cert).await;
            match tokio_tungstenite::client_async(request, stream).await {
                Ok((mut socket, _)) if origin.contains("localhost") => {
                    send(
                        &mut socket,
                        json!({"t":"hello", "v":wire::VERSION, "token":"unknown"}),
                    )
                    .await
                    .unwrap();
                    assert_eq!(receive(&mut socket).await["code"], "unauthorized");
                }
                Err(tungstenite::Error::Http(response)) if origin.contains("foreign") => {
                    assert_eq!(response.status(), 403);
                }
                other => panic!("{origin}: {:?}", other.map(|(_, response)| response)),
            }
        }

        let mut plain = TcpStream::connect(address).await.unwrap();
        plain
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        let mut reply = Vec::new();
        // The handshake failure may reset the connection as well as end it.
        let _ = plain.read_to_end(&mut reply).await;
        assert!(!reply.starts_with(b"HTTP/"), "{reply:?}");
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
    async fn a_second_device_is_admitted_beside_the_first_and_a_stopped_listener_remembers_none() {
        let (dir, address, auth, mut rx, task) = start().await;
        let password = auth.lock().unwrap().password().to_owned();
        let (tx, _messages) = mpsc::unbounded_channel();
        let (_fake, mut app) = crate::testing::FakeAgent::start(tx);
        app.remote.url = Some(format!("http://{address}"));
        let (outbound, _output) = mpsc::channel(512);
        let (close, _closing) = watch::channel(None);
        app.remote.phones.push(super::super::Phone {
            connection: Connection {
                id: 0,
                generation: 0,
                device: super::super::auth::Device {
                    name: "existing phone".into(),
                    token_sha256: "existing hash".into(),
                    paired_at: String::new(),
                },
                outbound,
                close,
            },
            view: super::super::PhoneView::foreground(&app),
            projection: Default::default(),
            connected_at: jiff::Timestamp::UNIX_EPOCH,
            queries: 0,
        });
        let stored = || -> Value {
            serde_json::from_str(&std::fs::read_to_string(dir.path().join("remote.json")).unwrap())
                .unwrap()
        };
        let mut socket = login(address, &password, "other phone").await;
        app.on_msg(rx.recv().await.unwrap());
        for kind in ["authorized", "welcome"] {
            assert_eq!(receive(&mut socket).await["t"], kind);
        }
        assert_eq!(app.remote.phones.len(), 2);
        assert_eq!(stored()["devices"][0]["name"], "other phone");
        app.remote.generation += 1;
        let mut late = login(address, &password, "late phone").await;
        app.on_msg(rx.recv().await.unwrap());
        assert_eq!(
            receive(&mut late).await,
            json!({"t":"bye", "reason":"remote_off"})
        );
        assert_eq!(stored()["devices"].as_array().unwrap().len(), 1);
        task.abort();
    }

    #[tokio::test]
    async fn invalid_device_names_and_pre_auth_filesystem_requests_are_refused() {
        let (_dir, address, auth, mut rx, task) = start().await;
        let password = auth.lock().unwrap().password().to_owned();
        let mut requests = vec![json!({"t":"fs_roots", "id":"early"})];
        for device in ["".to_owned(), "bad\nname".into(), "x".repeat(65)] {
            requests.push(
                json!({"t":"login", "v":wire::VERSION, "password":password, "device":device}),
            );
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
    async fn an_admitted_phone_browses_and_creates_directories_only_inside_roots() {
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

    #[tokio::test]
    async fn a_query_reaches_the_reducer_and_a_malformed_one_is_a_failed_result() {
        let (_dir, address, auth, mut rx, task) = start().await;
        let (mut socket, _connection) = admitted(address, &auth, &mut rx).await;
        for kind in ["authorized", "welcome", "snapshot"] {
            assert_eq!(receive(&mut socket).await["t"], kind);
        }
        send(&mut socket, json!({"t":"query", "id":"q1", "kind":"diff"}))
            .await
            .unwrap();
        assert_eq!(
            receive(&mut socket).await,
            json!({"t":"result", "id":"q1", "ok":false, "error":"bad_request"})
        );
        send(
            &mut socket,
            json!({"t":"query", "id":"q2", "kind":"diff", "path":"src/a.rs"}),
        )
        .await
        .unwrap();
        let Msg::Remote(RemoteEvent::Query {
            id,
            query: Query::Diff { path },
            ..
        }) = rx.recv().await.unwrap()
        else {
            panic!("a query")
        };
        assert_eq!((id.as_str(), path.as_str()), ("q2", "src/a.rs"));
        task.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn revoked_connections_cannot_start_queued_filesystem_work() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let roots: Arc<[PathBuf]> = vec![root.clone()].into();
        for reason in ["replaced", "remote_off"] {
            let (close, revoked) = watch::channel(None);
            close.send(Some(reason)).unwrap();
            let reply = filesystem(
                "stale".into(),
                FsRequest::Mkdir {
                    parent: fs::display(&root),
                    name: reason.into(),
                },
                roots.clone(),
                revoked,
            )
            .await
            .unwrap();
            assert!(reply.is_none());
            assert!(!root.join(reason).exists());
        }
    }

    #[tokio::test]
    async fn a_version_1_client_is_refused_before_sign_in_with_the_version_error() {
        let (_dir, address, auth, mut rx, task) = start().await;
        let password = auth.lock().unwrap().password().to_owned();
        for request in [
            json!({"t":"login", "v":1, "password":password, "device":"phone"}),
            json!({"t":"hello", "v":1, "token":"unknown"}),
        ] {
            let mut socket = connect(address).await;
            send(&mut socket, request).await.unwrap();
            assert_eq!(receive(&mut socket).await["code"], "version");
        }
        assert_eq!(wire::VERSION, 2);
        assert!(rx.try_recv().is_err(), "neither admitted nor counted");
        task.abort();
    }

    #[tokio::test]
    async fn the_right_password_is_authorized_before_welcome_and_the_snapshot_and_unknown_tokens_are_not()
     {
        let (_dir, address, auth, mut rx, task) = start().await;
        let mut socket = connect(address).await;
        send(
            &mut socket,
            json!({"t":"hello", "v":wire::VERSION, "token":"unknown"}),
        )
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
