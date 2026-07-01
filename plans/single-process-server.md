# Single-process server (zoom 1)

Collapse `nginx` + `matchbox` (:3536 `/ws`) + `smash-signaling` (:3537 `/rtc`) + `setup-vps.sh`
into ONE static musl binary on :443. Retire the canvas `/play/` build, so matchbox dies.

Target box: Azure VM, 1 vCPU / 955 MB. One ~15 MB-RSS binary, one systemd unit.

## Crates

```
axum 0.7            # router + ws
tower-http          # ServeDir (precompressed_gzip), SetResponseHeader
axum-server         # TLS listener (rustls)
rustls-acme         # in-process Let's Encrypt (TLS-ALPN-01 on :443) -> no certbot
tokio, futures      # StreamExt for the relay
web-push, serde     # keep from today
```

## The whole server, rxjs framing

Think of it as three Observables wired into one process:

- **`connections$`** — the TCP/TLS accept loop is a `Stream<Conn>`. `.for_each_concurrent` spawns
  a task per connection (like `mergeMap`).
- **each socket** — `ws.split() -> (sink, stream)`. The `stream` is an `Observable<Message>`; the
  `sink` is an `Observer<Message>`. A relayed pair is just `merge` of two mapped streams.
- **the lobby** — a per-room single-slot **Subject/rendezvous**: first peer `park`s (publishes a
  waiting slot), second peer `snap`s it (the slot completes, handing back each other's `Observer`).

### Router (type signatures first)

```rust
struct AppState(Arc<Server>);           // process-lifetime; Lobby + clients + push + next_id

fn app(state: AppState) -> Router {
    Router::new()
        .route("/rtc",       get(ws_upgrade))     // the relay (StreamExt below)
        .route("/status",    get(status))
        .route("/vapid",     get(vapid))
        .route("/subscribe", post(subscribe))
        .nest_service("/game", game_static())     // static + gzip + COOP/COEP + cache
        .with_state(state)
}
```

### The relay = a per-room rendezvous + a bidirectional merge

```rust
// Lobby is a Subject keyed by room: at most one waiting host per room.
type Lobby = Mutex<HashMap<Room, oneshot::Sender<Peer>>>;   // Peer = mpsc::Sender<Message>

async fn ws_upgrade(ws: WebSocketUpgrade, State(s): State<AppState>, q: Query<Join>) -> Response {
    ws.on_upgrade(move |sock| relay(sock, s, q.room, q.identity))
}

async fn relay(sock: WebSocket, s: AppState, room: Room, id: Identity) {
    let (mut tx_ws, mut rx_ws) = sock.split();          // Observer, Observable
    let (to_me, mut from_partner) = mpsc::channel(64);   // my inbox (partner writes here)

    // rendezvous: snap a waiting host -> guest; else park as host and await a guest.
    let (role, partner): (Role, mpsc::Sender<Message>) = {
        let mut slot = s.lobby.lock().await;
        match slot.remove(&room) {
            Some(host_inbox) => {                         // second in -> GUEST
                // hand the host OUR inbox; take theirs
                let (give, _) = /* the host's parked oneshot */;
                give.send(to_me.clone());                 // Subject.next(guest)
                (Role::Guest, host_inbox)
            }
            None => {                                     // first in -> HOST, park + notify
                let (give, got) = oneshot::channel();
                slot.insert(room.clone(), give);
                drop(slot);
                s.push.notify(&room, "lobby waiting");     // side effect
                match got.await { Ok(guest_inbox) => (Role::Host, guest_inbox),
                                  Err(_) => return /* left before pairing */ }
            }
        }
    };
    tx_ws.send(matched(role)).await;                      // server -> peer: {matched, role}

    // rxjs: merge( ws$.subscribe(partner), partner$.subscribe(ws) ) until either completes.
    loop {
        select! {
            m = rx_ws.next()       => match m { Some(Ok(t)) => { partner.send(t).await; }, _ => break },
            m = from_partner.recv() => match m { Some(t) => { tx_ws.send(t).await; },      None => break },
        }
    }
    partner.send(bye()).await;                            // onComplete -> tell the other side
}
```

The `select!` loop IS `merge(a$.map(->b), b$.map(->a)).subscribe()`. The relay never parses
offer/answer/ice; it's a dumb pipe. Only the pairing handshake is server-driven (same as today).

### Static serving (the loading-screen + cache fix, in-process)

```rust
fn game_static() -> impl Service {
    let dir = ServeDir::new("/srv/smash-godot")
        .precompressed_gzip()                 // serve index.side.wasm.gz if present (no per-req CPU)
        .append_index_html_on_directories(true);
    ServiceBuilder::new()
        .layer(set_header("Cross-Origin-Opener-Policy",   "same-origin"))
        .layer(set_header("Cross-Origin-Embedder-Policy", "require-corp"))
        .layer(set_header("Cache-Control", "no-cache"))   // revalidate; 304-fast repeat
        .service(dir)
}
```

Precompress the big files at export time (`gzip -k index.side.wasm index.js …`) so ServeDir just
streams the `.gz`. This replaces the nginx gzip/COOP/COEP/cache block one-for-one.

### TLS (the last thing nginx did)

```rust
// rustls-acme: obtain + auto-renew Let's Encrypt on :443, account cached to /var/lib/smash/acme.
let acme = AcmeConfig::new(["hafley.codes"]).cache_dir("/var/lib/smash/acme").directory_lets_encrypt(true);
axum_server::from_tcp_rustls(listener_443, acme.rustls_config()).serve(app.into_make_service()).await;
// :80 -> 301 https (also serves ACME http-01 fallback if ALPN unavailable)
```

Fully self-contained: no certbot, no external timer. One process owns its own cert.

## State & lifetimes

| type | lifetime | holds |
|---|---|---|
| `Arc<Server>` | process | `Lobby`, clients registry, `PushService`, `next_id` |
| relay task | one connection | ws split halves, `from_partner` mpsc |
| parked `oneshot` | until guest snaps it or host drops | the waiting host's inbox sender |

Uniqueness: at most one parked host per `room` (the `HashMap` slot). A guest `remove`s it
atomically under the lock, so two guests can't both claim one host.

## Deploy (replaces vps-deploy + setup-vps.sh)

```
just ship:
  net-test                                              # determinism gate
  cargo zigbuild --release --target x86_64-unknown-linux-musl   # ONE static binary
  godot-export && gzip -k build/web/{index.side.wasm,index.js,index.wasm,index.pck}
  rsync build/web/  vps:/srv/smash-godot/
  scp target/.../smash  vps:/usr/local/bin/smash
  ssh systemctl restart smash
```

Delete: `deploy/nginx/*`, `deploy/setup-vps.sh`, matchbox unit, the second relay.
systemd unit needs `AmbientCapabilities=CAP_NET_BIND_SERVICE` to bind :80/:443 as non-root.

## Migration order (each step reversible)

1. Add `game_static()` + the routes to the existing binary; run it on :8443 BEHIND nginx; diff
   `/game/` responses vs nginx (headers, sizes, gzip). No user impact.
2. Add rustls-acme against the Let's Encrypt **staging** directory; verify a cert issues to a
   scratch port. Then switch to prod ACME.
3. Flip: stop nginx + matchbox, bind `smash` to :80/:443, restart.
4. Delete nginx snippets, `setup-vps.sh`, matchbox. `just ship` is now scp-binary + rsync-static.

## Open questions

- Keep `/play/` (matchbox canvas) at all, or hard-retire? Plan assumes retire.
- ACME in-process (rustls-acme) vs read certbot pem? In-process is the true "1 process"; certbot
  is lower-risk if ACME-on-:443 fights the accept loop. Recommend in-process, certbot as fallback.
- Static root `/srv/smash-godot` vs current `/var/www/smash-godot` — pick one, update rsync target.
