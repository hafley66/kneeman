//! Minimal WebRTC signaling relay. Pairs WebSocket clients two at a time and forwards every text
//! frame from one peer to the other, unread. The client (the gdext shell) speaks JSON:
//!
//!   server -> peer  : {"kind":"matched","role":"host"}   (first in a pair = host, second = guest)
//!   peer   -> server: {"kind":"offer"|"answer"|"ice", ...}   (relayed verbatim to the partner)
//!   server -> peer  : {"kind":"bye"}                     (partner left)
//!
//! The host creates the WebRTC offer; the guest answers. The relay is deliberately dumb: it does
//! not parse offer/answer/ice payloads, only the pairing handshake is server-driven.
//!
//! Pairing is a single waiting slot (the matchbox `?next=2` behavior): the first connection waits,
//! the second one snaps to it, both get a partner channel, the slot clears for the next two.
//!
//! Observability: a plain HTTP GET to the same route (no WebSocket upgrade) returns a JSON status
//! snapshot — build hash + time, uptime, the pending/waiting count, and every connected client's
//! name + color (sent as `?name=&color=` query params on the WS URL). Any browser can read it.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio_tungstenite::tungstenite::Message;
use web_push::SubscriptionInfo;

mod config;
mod push;
use config::Config;
use push::PushState;

const BUILD_HASH: &str = env!("BUILD_HASH");
const BUILD_UNIX: &str = env!("BUILD_UNIX");

/// Lobby holds at most one waiting host PER ROOM, so a named `?room=code` lets two specific players
/// meet instead of pairing with whoever's first. Empty room normalizes to "default".
fn norm_room(r: &str) -> String {
    let r = r.trim();
    if r.is_empty() { "default".into() } else { r.to_string() }
}

/// What a waiting (host) connection parks in the lobby: a sender that delivers text TO it, and a
/// oneshot the arriving guest fires to hand back ITS sender (so the host can reach the guest).
struct Pending {
    to_host: mpsc::UnboundedSender<String>,
    give_guest: oneshot::Sender<mpsc::UnboundedSender<String>>,
}

type Lobby = Arc<Mutex<HashMap<String, Pending>>>;

/// One connected client, as the status endpoint sees it. `role`/`matched` fill in once paired.
#[derive(Clone)]
struct ClientInfo {
    who: String,    // peer socket address
    name: String,   // from ?name= (the player's localStorage name)
    color: String,  // from ?color= (their localStorage color, e.g. "#aabbcc")
    build: String,  // from ?hash= (their git build hash; compare to build_hash to spot mismatches)
    role: String,   // "" until matched, then "host"/"guest"
    matched: bool,
    since_unix: u64,
}

/// Process-wide server state, shared by every connection task + the status responder.
struct Server {
    started: Instant,
    started_unix: u64,
    next_id: AtomicU64,
    clients: Mutex<BTreeMap<u64, ClientInfo>>,
    lobby: Lobby,
    push: PushState,
}

/// POST /subscribe body: a browser PushSubscription plus the room it wants pings for.
#[derive(Deserialize)]
struct SubscribeReq {
    #[serde(default)]
    room: String,
    subscription: SubscriptionInfo,
}

#[tokio::main]
async fn main() {
    let cfg = Config::from_env();
    let listener = TcpListener::bind(&cfg.bind_addr).await.expect("bind signaling port");
    println!(
        "smash-signaling listening on ws://{} (proxy wss://host/rtc) — build {BUILD_HASH}",
        cfg.bind_addr
    );
    let server = Arc::new(Server {
        started: Instant::now(),
        started_unix: now_unix(),
        next_id: AtomicU64::new(1),
        clients: Mutex::new(BTreeMap::new()),
        lobby: Arc::new(Mutex::new(HashMap::new())),
        push: PushState::new(cfg),
    });

    loop {
        let Ok((stream, _)) = listener.accept().await else { continue };
        let server = server.clone();
        tokio::spawn(async move {
            if let Err(e) = handle(stream, server).await {
                eprintln!("peer ended: {e}");
            }
        });
    }
}

fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Peek the request head and route. A WebSocket upgrade (carries `Sec-WebSocket-Key`) is the relay;
/// everything else is plain HTTP routed by method + path. Peeking (not reading) leaves the bytes for
/// `accept_async` on the WS path; the HTTP handlers own the stream and read it themselves.
async fn handle(stream: tokio::net::TcpStream, server: Arc<Server>) -> Result<(), String> {
    let mut buf = [0u8; 2048];
    let n = stream.peek(&mut buf).await.map_err(|e| e.to_string())?;
    let head = String::from_utf8_lossy(&buf[..n]);
    let lower = head.to_ascii_lowercase();

    if lower.contains("sec-websocket-key") {
        let (name, color, room, build) = parse_query(&head);
        return relay(stream, server, name, color, room, build).await;
    }

    let (method, path) = request_line(&head);
    match (method.as_str(), path.as_str()) {
        ("OPTIONS", _) => write_http(stream, "204 No Content", "text/plain", "").await,
        ("GET", "/vapid") => serve_vapid(stream, &server).await,
        ("POST", "/subscribe") => handle_subscribe(stream, &server).await,
        ("GET", "/status") | ("GET", "/") => serve_status(stream, &server).await,
        _ => write_http(stream, "404 Not Found", "text/plain", "not found").await,
    }
}

/// (METHOD, PATH-without-query) from the request line, e.g. "POST /subscribe?x HTTP/1.1" -> ("POST","/subscribe").
fn request_line(head: &str) -> (String, String) {
    let mut parts = head.lines().next().unwrap_or("").split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("/");
    let path = target.split('?').next().unwrap_or("/").to_string();
    (method, path)
}

/// Extract `name`/`color`/`room`/`hash` from the request line's query string (URL-decoded, lightly).
/// `hash` is the client's git build hash, surfaced in /status so mismatched clients are visible.
fn parse_query(head: &str) -> (String, String, String, String) {
    let query = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1)) // "GET /rtc?name=..&color=..&room=..&hash=.. HTTP/1.1"
        .and_then(|path| path.split_once('?').map(|(_, q)| q.to_string()))
        .unwrap_or_default();
    let (mut name, mut color, mut room, mut build) =
        (String::new(), String::new(), String::new(), String::new());
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            match k {
                "name" => name = url_decode(v),
                "color" => color = url_decode(v),
                "room" => room = url_decode(v),
                "hash" => build = url_decode(v),
                _ => {}
            }
        }
    }
    (name, color, room, build)
}

/// Just enough percent-decoding for `%23` (#) and `+` spaces; identity strings are short + tame.
fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    out.push(b as char);
                    i += 3;
                    continue;
                }
                out.push('%');
                i += 1;
            }
            b'+' => {
                out.push(' ');
                i += 1;
            }
            c => {
                out.push(c as char);
                i += 1;
            }
        }
    }
    out
}

/// Write the JSON status snapshot as a tiny HTTP/1.1 response, then close.
async fn serve_status(mut stream: tokio::net::TcpStream, server: &Arc<Server>) -> Result<(), String> {
    let clients = server.clients.lock().await;
    let pending = !server.lobby.lock().await.is_empty();
    let waiting = clients.values().filter(|c| !c.matched).count();
    let mut rows = String::new();
    for (i, c) in clients.values().enumerate() {
        if i > 0 {
            rows.push(',');
        }
        rows.push_str(&format!(
            r#"{{"who":{},"name":{},"color":{},"build":{},"role":{},"matched":{},"since":{}}}"#,
            json_str(&c.who),
            json_str(&c.name),
            json_str(&c.color),
            json_str(&c.build),
            json_str(&c.role),
            c.matched,
            c.since_unix,
        ));
    }
    let body = format!(
        r#"{{"build_hash":"{}","build_unix":{},"started_unix":{},"uptime_secs":{},"now_unix":{},"connected":{},"waiting":{},"pending_handshake":{},"clients":[{}]}}"#,
        BUILD_HASH,
        BUILD_UNIX,
        server.started_unix,
        server.started.elapsed().as_secs(),
        now_unix(),
        clients.len(),
        waiting,
        pending,
        rows,
    );
    drop(clients);
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(resp.as_bytes()).await.map_err(|e| e.to_string())?;
    stream.shutdown().await.ok();
    Ok(())
}

/// Minimal JSON string escaping for the few fields we echo (name/color/addr).
fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' | '\r' | '\t' => {}
            c if (c as u32) < 0x20 => {}
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Generic tiny HTTP response (always CORS-open, always Connection: close). `status` is the full
/// status line text, e.g. "200 OK" / "204 No Content" / "404 Not Found".
async fn write_http(
    mut stream: tokio::net::TcpStream,
    status: &str,
    content_type: &str,
    body: &str,
) -> Result<(), String> {
    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nAccess-Control-Allow-Origin: *\r\n\
         Access-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Headers: content-type\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(resp.as_bytes()).await.map_err(|e| e.to_string())?;
    stream.shutdown().await.ok();
    Ok(())
}

/// GET /vapid -> the client's applicationServerKey (base64url), or null when push is off. The client
/// fetches this instead of hard-coding a key, so nothing is pinned to a host or baked at build time.
async fn serve_vapid(stream: tokio::net::TcpStream, server: &Arc<Server>) -> Result<(), String> {
    let body = match server.push.public_key() {
        Some(key) => format!(r#"{{"publicKey":"{key}"}}"#),
        None => r#"{"publicKey":null}"#.to_string(),
    };
    write_http(stream, "200 OK", "application/json", &body).await
}

/// POST /subscribe -> store a browser PushSubscription for a room. Body: {room, subscription}.
async fn handle_subscribe(stream: tokio::net::TcpStream, server: &Arc<Server>) -> Result<(), String> {
    let (stream, raw) = read_http_body(stream).await?;
    match serde_json::from_slice::<SubscribeReq>(&raw) {
        Ok(req) => {
            let room = norm_room(&req.room);
            server.push.subscribe(room.clone(), req.subscription).await;
            println!("[push] subscribed a device to room '{room}'");
            write_http(stream, "204 No Content", "text/plain", "").await
        }
        Err(e) => write_http(stream, "400 Bad Request", "text/plain", &format!("bad json: {e}")).await,
    }
}

/// Read an HTTP request fully off the stream and return (stream, body-bytes). Reads headers to find
/// Content-Length, then the body. The stream is returned so the caller can still write the response.
async fn read_http_body(
    mut stream: tokio::net::TcpStream,
) -> Result<(tokio::net::TcpStream, Vec<u8>), String> {
    let mut buf = Vec::with_capacity(2048);
    let mut tmp = [0u8; 2048];
    // read until headers complete
    let headers_end = loop {
        if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
            break pos + 4;
        }
        let n = stream.read(&mut tmp).await.map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("connection closed before headers".into());
        }
        buf.extend_from_slice(&tmp[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..headers_end]).to_ascii_lowercase();
    let want = head
        .lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    while buf.len() - headers_end < want {
        let n = stream.read(&mut tmp).await.map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    let body = buf[headers_end..(headers_end + want).min(buf.len())].to_vec();
    Ok((stream, body))
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// The original relay: pair two peers and forward text between them. Now also registers the client
/// in the shared map (for `/status`) and tears the entry down on disconnect.
async fn relay(
    stream: tokio::net::TcpStream,
    server: Arc<Server>,
    name: String,
    color: String,
    room: String,
    build: String,
) -> Result<(), String> {
    let who = stream.peer_addr().map(|a| a.to_string()).unwrap_or_else(|_| "?".into());
    let room = norm_room(&room);
    println!("connect {who} ({name}) room '{room}' build '{build}'");
    let id = server.next_id.fetch_add(1, Ordering::Relaxed);
    server.clients.lock().await.insert(
        id,
        ClientInfo {
            who: who.clone(),
            name: name.clone(),
            color,
            build,
            role: String::new(),
            matched: false,
            since_unix: now_unix(),
        },
    );
    // Always drop the registry entry, however this task exits.
    let result = relay_inner(stream, &server, id, &who, &name, &room).await;
    server.clients.lock().await.remove(&id);
    result
}

async fn relay_inner(
    stream: tokio::net::TcpStream,
    server: &Arc<Server>,
    id: u64,
    who: &str,
    name: &str,
    room: &str,
) -> Result<(), String> {
    let lobby = server.lobby.clone();
    let ws = tokio_tungstenite::accept_async(stream).await.map_err(|e| e.to_string())?;
    let (mut tx_ws, mut rx_ws) = ws.split();

    // Messages destined FOR this peer (written by the partner's relay task) arrive on this channel.
    let (to_me, mut from_partner) = mpsc::unbounded_channel::<String>();

    // Claim a partner within this ROOM: snap to a waiting host (we become guest), else park as host
    // and ping anyone subscribed to the room that a match is now waiting.
    let (role, to_partner): (&str, mpsc::UnboundedSender<String>) = {
        let mut slot = lobby.lock().await;
        match slot.remove(room) {
            Some(p) => {
                let _ = p.give_guest.send(to_me.clone());
                ("guest", p.to_host)
            }
            None => {
                let (give, got) = oneshot::channel();
                slot.insert(room.to_string(), Pending { to_host: to_me.clone(), give_guest: give });
                drop(slot);
                let who_label = if name.is_empty() { "someone".to_string() } else { name.to_string() };
                server.push.notify(
                    room.to_string(),
                    "🥊 lobby waiting".to_string(),
                    format!("{who_label} is waiting in '{room}' — tap to fight"),
                );
                match got.await {
                    Ok(guest_tx) => ("host", guest_tx),
                    Err(_) => {
                        let mut s = lobby.lock().await;
                        if s.get(room).map(|p| p.to_host.same_channel(&to_me)).unwrap_or(false) {
                            s.remove(room);
                        }
                        return Ok(());
                    }
                }
            }
        }
    };

    // Mark this client matched in the registry now that it has a partner + role.
    if let Some(c) = server.clients.lock().await.get_mut(&id) {
        c.role = role.to_string();
        c.matched = true;
    }

    println!("matched {who} as {role}");
    tx_ws
        .send(Message::Text(format!(r#"{{"kind":"matched","role":"{role}"}}"#).into()))
        .await
        .map_err(|e| e.to_string())?;

    // Relay loop: forward our inbound WS text to the partner; write partner text out to our WS.
    loop {
        tokio::select! {
            msg = rx_ws.next() => match msg {
                Some(Ok(Message::Text(t))) => { let _ = to_partner.send(t.to_string()); }
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => {}
                Some(Err(_)) => break,
            },
            out = from_partner.recv() => match out {
                Some(t) => tx_ws.send(Message::Text(t.into())).await.map_err(|e| e.to_string())?,
                None => break,
            },
        }
    }

    println!("disconnect {who} ({role})");
    let _ = to_partner.send(r#"{"kind":"bye"}"#.to_string());
    Ok(())
}
