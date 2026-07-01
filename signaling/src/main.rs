//! Single-process game server: a WebRTC signaling relay + web-push endpoints + the static host for
//! the Godot web export. Built on axum so one binary replaces nginx + matchbox + the old hand-rolled
//! HTTP loop (see plans/single-process-server.md).
//!
//! Routes:
//!   GET  /rtc        WebSocket relay. Pairs clients two at a time PER ROOM and forwards every text
//!                    frame between the pair, unread. The client (gdext shell) speaks JSON:
//!                      server -> peer : {"kind":"matched","role":"host"}   (first in a pair = host)
//!                      peer   -> peer : {"kind":"offer"|"answer"|"ice",..} (relayed verbatim)
//!                      server -> peer : {"kind":"bye"}                     (partner left)
//!                    Deliberately dumb: it never parses the SDP/ICE; gameplay is direct P2P after.
//!   GET  /status     JSON snapshot: build, uptime, waiting/pending counts, every connected client.
//!   GET  /vapid      the browser's applicationServerKey (push), or null when push is off.
//!   POST /subscribe  store a browser PushSubscription for a room.
//!   /game/*          static Godot web export (ServeDir), with COOP/COEP + gzip + no-cache headers.
//!
//! Pairing is a single waiting slot per room (matchbox `?next=2` behavior): the first connection
//! parks, the second snaps to it, both get the other's channel, the slot clears for the next two.

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Query, State};
use axum::http::{header, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use serde::Deserialize;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot, Mutex};
use tower::ServiceBuilder;
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;
use web_push::SubscriptionInfo;

mod config;
mod events;
mod push;
use config::Config;
use events::EventLog;
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

type Lobby = Mutex<HashMap<String, Pending>>;

/// One connected client, as the status endpoint sees it. `role`/`matched` fill in once paired.
#[derive(Clone)]
struct ClientInfo {
    who: String,    // peer socket address (loopback when behind a proxy)
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
    events: EventLog,   // netcode event sink (POST /ev -> rotating JSON-lines file)
    server_id: String,  // stamped as `sip` on every event
}

type Shared = Arc<Server>;

/// Query params on the `/rtc` WebSocket URL (percent-decoded by serde_urlencoded). All optional.
#[derive(Deserialize, Default)]
struct Join {
    #[serde(default)]
    name: String,
    #[serde(default)]
    color: String,
    #[serde(default)]
    room: String,
    #[serde(default)]
    hash: String,
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
    let bind = cfg.bind_addr.clone();
    let game_dir = cfg.game_dir.clone();
    let tls = (!cfg.tls_domains.is_empty()).then(|| AcmeOpts {
        domains: cfg.tls_domains.clone(),
        contact: cfg.acme_contact.clone(),
        cache_dir: cfg.acme_cache_dir.clone(),
        production: cfg.acme_production,
    });
    let events = EventLog::spawn(cfg.ev_log_path.clone(), cfg.ev_log_cap_bytes);
    let server_id = cfg.server_id.clone();
    let server: Shared = Arc::new(Server {
        started: Instant::now(),
        started_unix: now_unix(),
        next_id: AtomicU64::new(1),
        clients: Mutex::new(BTreeMap::new()),
        lobby: Mutex::new(HashMap::new()),
        push: PushState::new(cfg),
        events,
        server_id,
    });

    // Static host for the Godot export: serve precompressed `.gz` when present (the 40MB engine wasm
    // is gzipped at export time), and stamp the cross-origin-isolation + revalidation headers that the
    // threaded wasm build needs. This is the nginx `godot.conf` block, in-process.
    let coop = HeaderName::from_static("cross-origin-opener-policy");
    let coep = HeaderName::from_static("cross-origin-embedder-policy");
    let game = ServiceBuilder::new()
        .layer(SetResponseHeaderLayer::overriding(coop, HeaderValue::from_static("same-origin")))
        .layer(SetResponseHeaderLayer::overriding(coep, HeaderValue::from_static("require-corp")))
        .layer(SetResponseHeaderLayer::overriding(header::CACHE_CONTROL, HeaderValue::from_static("no-cache")))
        .service(
            ServeDir::new(&game_dir)
                .precompressed_gzip()
                .append_index_html_on_directories(true),
        );

    let app = Router::new()
        .route("/rtc", get(ws_upgrade))
        .route("/status", get(status))
        .route("/", get(status))
        .route("/vapid", get(vapid))
        .route("/subscribe", post(subscribe))
        .route("/ev", post(events::ev))
        .nest_service("/game", game)
        // Browser reads /status etc. cross-origin; keep the old permissive CORS. Also answers the
        // OPTIONS preflight the hand-rolled server used to special-case.
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        )
        .with_state(server);

    let make = app.into_make_service_with_connect_info::<SocketAddr>();
    match tls {
        // Behind nginx (or local): plain HTTP on BIND_ADDR. The current, default mode.
        None => {
            let listener = TcpListener::bind(&bind).await.expect("bind server port");
            println!("smash server: http://{bind}  (game_dir={game_dir}) — build {BUILD_HASH}");
            axum::serve(listener, make).await.expect("serve");
        }
        // Flip mode: terminate TLS in-process on :443 via ACME, plus an :80 -> :443 redirect.
        Some(opts) => {
            println!("smash server: TLS :443 via ACME {:?} (game_dir={game_dir}) — build {BUILD_HASH}", opts.domains);
            tokio::spawn(redirect_http_to_https());
            serve_tls(make, opts).await;
        }
    }
}

/// ACME/TLS settings, present only when `TLS_DOMAINS` is set.
struct AcmeOpts {
    domains: Vec<String>,
    contact: String,
    cache_dir: String,
    production: bool,
}

/// Serve the app over HTTPS on :443, obtaining + renewing the Let's Encrypt cert in-process
/// (TLS-ALPN-01 on the same port). The `AcmeState` stream must be polled to drive the ACME order,
/// so it's spawned onto its own task; the acceptor shares the resolver it feeds.
async fn serve_tls(make: MakeSvc, opts: AcmeOpts) {
    use futures::StreamExt;
    use rustls_acme::{caches::DirCache, AcmeConfig};

    let mut state = AcmeConfig::new(opts.domains)
        .contact_push(format!("mailto:{}", opts.contact))
        .cache(DirCache::new(opts.cache_dir))
        .directory_lets_encrypt(opts.production)
        .state();
    let acceptor = state.axum_acceptor(state.default_rustls_config());
    tokio::spawn(async move {
        loop {
            match state.next().await {
                Some(Ok(ok)) => println!("acme: {ok:?}"),
                Some(Err(e)) => eprintln!("acme error: {e:?}"),
                None => break,
            }
        }
    });
    axum_server::bind(SocketAddr::from(([0, 0, 0, 0], 443)))
        .acceptor(acceptor)
        .serve(make)
        .await
        .expect("tls serve");
}

/// Minimal :80 listener that 308-redirects every request to its https:// equivalent.
async fn redirect_http_to_https() {
    use axum::extract::Host;
    use axum::http::Uri;
    use axum::response::Redirect;
    let app = Router::<()>::new().fallback(|Host(host): Host, uri: Uri| async move {
        let host = host.split(':').next().unwrap_or(&host).to_string();
        let pq = uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");
        Redirect::permanent(&format!("https://{host}{pq}"))
    });
    if let Ok(l) = TcpListener::bind("0.0.0.0:80").await {
        let _ = axum::serve(l, app).await;
    }
}

/// The concrete make-service type shared by both serve paths (axum + axum-server).
type MakeSvc = axum::extract::connect_info::IntoMakeServiceWithConnectInfo<Router, SocketAddr>;

fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

// --- WebSocket relay -----------------------------------------------------------------------------

async fn ws_upgrade(
    ws: WebSocketUpgrade,
    State(server): State<Shared>,
    ConnectInfo(who): ConnectInfo<SocketAddr>,
    Query(j): Query<Join>,
) -> Response {
    ws.on_upgrade(move |sock| relay(sock, server, who.to_string(), j))
}

/// Register the client, run the pair-and-forward loop, then always deregister.
async fn relay(mut sock: WebSocket, server: Shared, who: String, j: Join) {
    let room = norm_room(&j.room);
    println!("connect {who} ({}) room '{room}' build '{}'", j.name, j.hash);
    let id = server.next_id.fetch_add(1, Ordering::Relaxed);
    server.clients.lock().await.insert(
        id,
        ClientInfo {
            who,
            name: j.name.clone(),
            color: j.color,
            build: j.hash,
            role: String::new(),
            matched: false,
            since_unix: now_unix(),
        },
    );
    relay_inner(&mut sock, &server, id, &j.name, &room).await;
    server.clients.lock().await.remove(&id);
}

async fn relay_inner(sock: &mut WebSocket, server: &Shared, id: u64, name: &str, room: &str) {
    // Messages destined FOR this peer (written by the partner's relay task) arrive on this channel.
    let (to_me, mut from_partner) = mpsc::unbounded_channel::<String>();

    // Claim a partner within this ROOM: snap to a waiting host (we become guest), else park as host
    // and ping anyone subscribed to the room that a match is now waiting.
    let (role, to_partner): (&str, mpsc::UnboundedSender<String>) = {
        let mut slot = server.lobby.lock().await;
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
                        // We left before a guest arrived: clear our own parked slot if it's still ours.
                        let mut s = server.lobby.lock().await;
                        if s.get(room).map(|p| p.to_host.same_channel(&to_me)).unwrap_or(false) {
                            s.remove(room);
                        }
                        return;
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

    println!("matched {id} as {role}");
    if sock.send(Message::Text(format!(r#"{{"kind":"matched","role":"{role}"}}"#))).await.is_err() {
        let _ = to_partner.send(r#"{"kind":"bye"}"#.to_string());
        return;
    }

    // rxjs: merge( ws$.subscribe(partner), partner$.subscribe(ws) ) until either side completes.
    loop {
        tokio::select! {
            msg = sock.recv() => match msg {
                Some(Ok(Message::Text(t))) => { let _ = to_partner.send(t); }
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => {}          // ignore binary/ping/pong
                Some(Err(_)) => break,
            },
            out = from_partner.recv() => match out {
                Some(t) => { if sock.send(Message::Text(t)).await.is_err() { break; } }
                None => break,
            },
        }
    }

    let _ = to_partner.send(r#"{"kind":"bye"}"#.to_string());
    println!("disconnect {id} ({role})");
}

// --- HTTP handlers -------------------------------------------------------------------------------

/// JSON status snapshot for the observability page.
async fn status(State(server): State<Shared>) -> Response {
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
    ([(header::CONTENT_TYPE, "application/json")], body).into_response()
}

/// GET /vapid -> the client's applicationServerKey (base64url), or null when push is off.
async fn vapid(State(server): State<Shared>) -> Response {
    let body = match server.push.public_key() {
        Some(key) => format!(r#"{{"publicKey":"{key}"}}"#),
        None => r#"{"publicKey":null}"#.to_string(),
    };
    ([(header::CONTENT_TYPE, "application/json")], body).into_response()
}

/// POST /subscribe -> store a browser PushSubscription for a room. Body: {room, subscription}.
async fn subscribe(State(server): State<Shared>, body: Bytes) -> Response {
    match serde_json::from_slice::<SubscribeReq>(&body) {
        Ok(req) => {
            let room = norm_room(&req.room);
            server.push.subscribe(room.clone(), req.subscription).await;
            println!("[push] subscribed a device to room '{room}'");
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => (StatusCode::BAD_REQUEST, format!("bad json: {e}")).into_response(),
    }
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
