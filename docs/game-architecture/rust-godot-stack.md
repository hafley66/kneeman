# Rust + Godot + Web (WASM) + Backend: Stack Reality Report (July 2026)

Field guide for an indie Godot 4 web-export game with a Rust simulation core (gdext), p2p rollback netplay (ggrs) over Godot's `WebRtcPeerConnection`, an axum signaling relay, coturn, and Postgres for durable state. Versions and claims dated and cited; integration costs called out honestly.

## TL;DR

- **gdext is production-usable but still 0.x** (v0.5.4, 23 Jun 2026) and its **web/emscripten export is officially "experimental"** with a fragile, pinned toolchain. It works; it is not turnkey.
- **The WASM wall is real and structural.** Anything reaching the browser through `wasm32-unknown-unknown` + wasm-bindgen (matchbox, most Rust-native WebRTC/WebSocket clients, the SpacetimeDB Rust client) cannot link into Godot's `wasm32-unknown-emscripten` export. What works: calling Godot's own `WebRtcPeerConnection` / `WebSocketPeer` from Rust through gdext. ggrs runs over that transport because its socket is a trait you implement.
- **For Postgres, default to sqlx** (0.9.0, 6 May 2026): raw SQL with compile-time verification, built-in `PgPool`, built-in migrations, first-class tokio, drops straight into axum `State`. Keep the reducer pure; put all IO behind a `WorldStore` trait.
- **On backends, "adopt" mostly means Nakama or Colyseus** for a Godot web client. SpacetimeDB has no viable Godot-web path and is BSL, not AGPL. Rivet left the game-backend business. Given you already have axum + coturn + Postgres, **build** is defensible.
- **For hostile networks, force relay-only over TURNS/tcp/443** — the one transport a VPN treats like HTTPS.

---

## 1. godot-rust (gdext): current state

| Fact | Value |
|---|---|
| Latest release | **`godot` v0.5.4, 23 Jun 2026** |
| v0.5 line | announced 27 Mar 2026, incremental, **not** 1.0 |
| Minimum Godot | **4.2** (4.1 lacks Rust callables, typed signals, hot reload) |
| Tracks up to | Godot 4.6 API features |
| Support window | ~1–2 years per Godot release |

Crate-name trap: the published crate is **`godot`**; `gdext` is the repo. The bare `gdext` crate on crates.io is a stale 0.0.0 placeholder — do not depend on it. Compatibility rule: an extension loads where **runtime version ≥ API version** (a 4.3-built extension won't load in 4.2.1).

**Can do:** class registration, typed signals (`#[signal]`), Rust callables, editor plugins, tool scripts, full apps, hot reload. v0.5 added safeguard tiers (Strict/Balanced/Disengaged), dropped the `Gd<T>` internal mutex, added typed `Dictionary` access.

**Can't / gotchas:** Web and mobile are **experimental** ("documentation and tooling still lacking"); crates.io lags master; breaking changes land periodically; build complexity rises sharply once you target web.

> **→ Applies to this project.** Pin the exact `godot` crate version and Godot editor version together; treat an editor upgrade as a coordinated bump of both + export templates. You're on a 0.x dependency for a shipped title — budget a breaking-change migration every few releases.

---

## 2. The WASM wall

The section you already paid for in blood with matchbox. Two different WebAssembly worlds that don't mix.

| Target | Runtime model | Who uses it |
|---|---|---|
| `wasm32-unknown-unknown` + **wasm-bindgen** | wasm-bindgen JS shims + wasm-bindgen host loader | matchbox, most browser Rust SDKs |
| `wasm32-unknown-emscripten` | **Emscripten** supplies its own JS runtime + loader | **Godot's web export** |

**Why wasm-bindgen crates can't compile into a Godot emscripten export** (maintainers, verbatim): "Emscripten wants its own JS shims and all that, and having two systems of managing shims won't mix well." Under `wasm32-unknown-emscripten`, wasm-bindgen fails to emit browser shims and emits imports against `env`/`wasi_snapshot_preview1` the browser doesn't provide; Emscripten also loads the module async, breaking wasm-bindgen's sync assumptions.

**matchbox specifically** targets `wasm32-unknown-unknown` with the `wasm-bindgen` feature; that artifact won't link into a Godot emscripten export. Same reasoning condemns most tokio-based Rust WebRTC/WebSocket clients and the SpacetimeDB Rust client when the vehicle is a Godot web export.

**What works — bridge to Godot's own networking from Rust:**
- `WebRtcPeerConnection`, `WebRtcDataChannel`, `WebRtcMultiplayerPeer` (P2P mesh, `MultiplayerAPI`-compatible) — automatic in web exports; native needs the `webrtc-native` GDExtension.
- `WebSocketPeer` (client + server, `poll()` in `_process`).
- All exposed to Rust via gdext (`godot::classes::WebRtcPeerConnection`). Drive engine networking from Rust rather than linking a Rust-native WebRTC stack (which drags in wasm-bindgen and hits the wall).

**ggrs is transport-agnostic — the thing that saves you.** Custom transports implement the **`NonBlockingSocket`** trait; ggrs ships a custom-socket example. Implement `NonBlockingSocket` over Godot's `WebRtcDataChannel` (via gdext) and ggrs neither knows nor cares. No prebuilt Godot adapter ships — the wiring is yours, but the extension point is small and documented.

**The web export toolchain to pin:**
- Rust **nightly** + `rust-src` (for `-Zbuild-std`); target `wasm32-unknown-emscripten`; `cargo +nightly build -Zbuild-std --target wasm32-unknown-emscripten`; enable crate feature `experimental-wasm`.
- **Emscripten must match the Godot export template exactly.** Book: **emscripten 3.1.74 for Godot 4.3+**; 4.2 needed emcc ≤ 3.1.39. No stable ABI between emscripten versions — a mismatch silently breaks the build.
- **Threads:** multi-threaded needs `-pthread`/`+atomics` and the host must serve **COOP/COEP** (Cross-Origin Isolation) for SharedArrayBuffer; single-threaded uses `experimental-wasm-nothreads` (Godot 4.3+ Thread Support toggle).
- Extra flags: `-Z default-visibility=hidden`, disable Wasm exception handling (`-Z emscripten-wasm-eh=false`). Firefox GDExtension web needs 4.3+ and is more limited than Chromium. Open issue: multiple Rust GDExtensions in one Wasm export can conflict (gdext #968). New in 2026: prebuilt Wasm artifacts (no `api-custom`), setup still "elaborate."

> **→ Applies to this project.** The matchbox failure was the wall, not a bug. Lock the Rust-nightly + emscripten-3.1.74 + Godot-4.3+ triple in CI; treat drift as release-blocking. Netplay transport is `WebRtcDataChannel` driven from Rust via gdext, with ggrs on a hand-written `NonBlockingSocket`. If you serve threaded Wasm, nginx must send COOP/COEP on the game route or SharedArrayBuffer is disabled.

---

## 3. Rust + Postgres tooling

| Library | Latest | Model | Compile-time safety | Async |
|---|---|---|---|---|
| **sqlx** | **0.9.0** (6 May 2026), MSRV 1.94 | Raw SQL toolkit, no ORM | SQL verified against a live/cached schema | tokio first-class |
| **diesel** | 2.3.x; diesel-async 0.9.x | Type-safe query-builder DSL | DSL checked by compiler | sync default; async via diesel-async |
| **sea-orm** | 2.0 (12 Jan 2026), on sqlx 0.9 | ActiveRecord ORM | Entity API | native (built on sqlx) |

**sqlx (default recommendation):** `query!`/`query_as!` verify SQL against a dev DB at compile time; `cargo sqlx prepare` writes a `.sqlx/` cache so CI/Docker build without a live DB; migrations built in (`sqlx migrate` + `migrate!()` embed); `PgPool` is a cheap-to-clone ref-counted handle (don't wrap in `Arc`). 0.9 notes: per-crate `sqlx.toml`, `runtime-smol`, breaking changes to `Arguments` lifetimes / `Encode` / nullability inference; repo moved `launchbadge` → `transact-rs` (crate name unchanged). Known gap: **no query pipelining**.

**diesel:** sync-first, best compile times; dynamic/optional-filter queries fight the type system; `diesel-async` adds `AsyncPgConnection` (bb8/deadpool/mobc). **sea-orm:** async ActiveRecord on sqlx+SeaQuery; migrations are Rust code; largest dep tree, slowest compile; verify the exact 2.0 tag (RCs were still emitting mid-2026).

**Pooling:** with sqlx use its built-in `PgPool` and add nothing; with diesel-async or tokio-postgres supply bb8 or deadpool.

**axum + sqlx wiring** (axum 0.8.x stable):

```rust
let pool = PgPoolOptions::new()
    .max_connections(5)
    .acquire_timeout(Duration::from_secs(3))
    .connect(&db_url).await?;

let app = Router::new()
    .route("/", get(handler))
    .with_state(pool);          // PgPool IS the state; cheap clone, no Arc

async fn handler(State(pool): State<PgPool>) -> impl IntoResponse {
    sqlx::query_scalar("select 1").fetch_one(&pool).await // ...
}
```
For a larger `AppState`, implement `FromRef<AppState> for PgPool`.

**Swappable `WorldStore` with a pure reducer.** async-fn-in-trait stabilized in Rust 1.75. Two caveats: native async fn in traits doesn't auto-propagate `Send` (add `+ Send` bounds or use `async_trait`), and isn't `dyn`-compatible (a runtime-swapped `Arc<dyn WorldStore>` needs `#[async_trait]` or `trait-variant`). Decision: store the impl as a generic `S: WorldStore` for zero-boxing native async-fn; take `#[async_trait]` only if you need a trait object.

```rust
// Pure core: no IO, no async. (state, event) -> state'
fn reduce(state: World, ev: &Event) -> World { /* deterministic */ }

// Effect boundary (dyn-object form, runtime-swappable backend):
#[async_trait::async_trait]
pub trait WorldStore: Send + Sync {
    type Error: std::error::Error + Send + Sync + 'static;
    async fn load(&self, id: WorldId) -> Result<World, Self::Error>;
    async fn save(&self, id: WorldId, state: &World) -> Result<(), Self::Error>;
    async fn append_event(&self, id: WorldId, ev: &Event) -> Result<u64, Self::Error>;
    async fn events_since(&self, id: WorldId, since: u64) -> Result<Vec<(u64, Event)>, Self::Error>;
}
```
`PgWorldStore { pool: PgPool }` is production; `InMemoryWorldStore` is the test impl. Per-tick: `load` → `reduce` (pure) → `append_event`, with a periodic `save` snapshot. Put the store in axum `State` like the pool.

> **→ Applies to this project.** sqlx is the right call: no ORM tax, compile-time-checked SQL, `PgPool` straight into axum `State` next to your signaling handlers, offline `.sqlx` prepare for CI. `WorldStore` keeps the reducer pure/testable and lets an in-memory store back rollback while Postgres holds durable state. One `PgPool` in shared state covers signaling + DB since it's the same axum process.

---

## 4. Self-hostable game backends: support matrix

Two brief premises were wrong and are corrected below (SpacetimeDB license; SpacetimeDB Godot SDK).

| Backend | Godot SDK | Self-host? | Realtime model | Persistence | License (2026) | Fits Godot-web? |
|---|---|---|---|---|---|---|
| **Nakama** | Yes, GDScript (last tag v3.4.0, Mar 2024) | Yes: Go binary + Postgres/CockroachDB | WebSocket; relayed + authoritative match; matchmaker | Postgres; JSON collections | **Apache-2.0** server | **Yes** — GDScript = `WebSocketPeer` + `HTTPRequest` |
| **Rivet** | No (plugin dead; docs 404) | Yes: Rust binary/Docker | Actors: RPC/state over WS (matchmaking removed) | actor SQLite/KV; PG/FS/FDB | **Apache-2.0** (pivoted to AI actors) | Partial: no binding, hand-roll WS |
| **SpacetimeDB** | **C# SDK, desktop only** | Yes: single binary | DB-is-server; WASM reducers; WS sync | in-memory + commit log | **BSL 1.1** (→ AGPLv3 on 2031-06-18) | **No** — Rust SDK = wasm-bindgen wall; no C# web export; only TS via `JavaScriptBridge` |
| **Colyseus** | Yes, GDExtension (beta) + community GDScript | Yes: Node/TS; Redis to scale | Authoritative rooms; binary delta state sync | **none built-in** (BYO DB) | **MIT** | **Yes** — WebSocket |
| **PlayFab** | No (community only) | **No** — Azure SaaS | Multiplayer Servers, matchmaking, Party | Player/Title/Entity/Economy | proprietary managed | Partial: REST works; Party not web/Godot |
| **Supabase** | No (stale community addon) | Yes: ~13-container Compose | Phoenix Channels: PG Changes, Broadcast, Presence | **PostgreSQL** | **Apache-2.0** core | **Yes** — `HTTPRequest`+`WebSocketPeer` or JSBridge → `supabase-js` |
| **Firebase** | No (community MIT plugin) | **No** (emulator = dev only) | RTDB + Firestore listeners | NoSQL, Google-managed | proprietary managed | Likely via REST plugin |

**Notes:**
- **Nakama** is the cleanest official-SDK web fit (GDScript client is literally `WebSocketPeer` + `HTTPRequest`, both web-safe). Server genuinely Apache-2.0; paid product separate. Godot client's last tag is >2 years old — verify master.
- **Rivet pivoted out of games** (examples archived Dec 2024, `/docs/godot` 404s, SDKs are JS/React/Rust/Swift). Still self-hostable, but no maintained Godot story.
- **SpacetimeDB, two corrections.** (1) Still **BSL 1.1**, not AGPL; AGPLv3-with-linking-exception is the scheduled 2031-06-18 conversion, and BSL restricts production to a single instance / forbids multi-tenant DBaaS. (2) First-party Godot support exists but via the **C# SDK, desktop/mobile only**; the web door is doubly shut (Rust SDK is wasm-bindgen; Godot 4 can't export C# to web — godot#70796). Only Godot-web path is the TypeScript SDK via `JavaScriptBridge`.
- **Colyseus** MIT, WebSocket, official GDExtension (beta) + community GDScript; no built-in persistence. **Supabase** self-host is a ~13-container stack but gives real Postgres + S3 storage + auth + Realtime; for web-only, `JavaScriptBridge` → `supabase-js` beats the stale addon. **PlayFab/Firebase** managed-only.

> **→ Applies to this project.** You already have axum + coturn + Postgres + a pure reducer. None slots under that without displacing something. The two that'd fit a Godot web client from scratch are **Nakama** and **Colyseus** — both authoritative-server, neither gives rollback, so their realtime layer is redundant with your ggrs choice. SpacetimeDB is a dead end for your web client + BSL. Honest read: your bespoke axum relay is small, written, and avoids importing an authoritative-server model that fights rollback. Adopt only for the account/storage/matchmaking side (leaderboards, profiles, cloud saves) — and then **Nakama** is least-friction because its Godot client is plain WebSocket + HTTP that survives web export. Everything web-facing terminates `wss://`/HTTPS on your existing nginx TLS front.

---

## 5. Signaling, TURN, WebRTC (brief — you already built coturn)

- **STUN vs TURN.** ICE gathers host, server-reflexive (STUN), and relay (TURN) candidates. STUN **fails against symmetric NAT**; ICE then falls back to a **TURN relay** (always works, lowest priority because it spends your bandwidth).
- **coturn** — standard open-source STUN/TURN, BSD-3-Clause, TLS/DTLS.
- **TURN REST ephemeral creds** (draft-uberti-behave-turn-rest): one shared secret; `username = <expiry-unix-ts>[:userid]`, `password = base64(HMAC-SHA1(secret, username))`; coturn `use-auth-secret` + `static-auth-secret` recomputes + checks expiry, no per-user DB lookup, self-expiring. This is your existing `/turn` mint. Short TTL because creds are exposed in client JS.
- **TURNS over TCP 443** is the hostile-network transport — TLS on the HTTPS port, indistinguishable from web traffic, passes where UDP-TURN and TCP-TURN/3478 are blocked.
- **The VPN black-hole.** ICE probes raw interfaces at the app layer while a VPN operates at the network layer; UDP ICE probes get misrouted/dropped and srflx candidates point at unreachable addresses, so media dies even with TURN configured. Fix: force a single relay path — `iceTransportPolicy: "relay"` pointed at **TURNS/tcp/443**. Cost: added latency + relay bandwidth, so it's a deliberate hostile-network mode, not the default.
- **Godot:** `WebRTCPeerConnection.initialize(config)` takes `iceServers: [{ urls, username, credential }]`, so REST-minted creds drop straight in:

```gdscript
peer.initialize({
  "iceServers": [
    { "urls": ["stun:stun.example.com:3478"] },
    { "urls": ["turns:turn.example.com:443?transport=tcp"],
      "username": "<expiry>:<userid>", "credential": "<hmac-b64>" }
  ]
})
```
Caveat: confirm your Godot version's native WebRTC forwards `iceTransportPolicy` (browsers honor it natively; native forwards to libwebrtc). If not, force relay-only by advertising **only** the TURNS server (omit STUN). An older `webrtc-native` build crashed when initialized with a TURN server carrying username/credential (webrtc-native#7) — verify on your version.

> **→ Applies to this project.** On web this runs in the browser's WebRTC stack, so `iceTransportPolicy: "relay"` is honored natively and TURNS/443 is your VPN-proof mode. On native desktop you depend on `webrtc-native`, where `iceTransportPolicy` forwarding and the old credential crash both need checking against your pinned Godot version. Keep STUN-first as default; expose a "having trouble connecting?" toggle that switches to relay-only TURNS/443.

---

## Sources

**gdext / web export / WASM wall**
- [gdext (GitHub)](https://github.com/godot-rust/gdext) · [godot crate](https://crates.io/crates/godot) · [March 2026 dev update](https://godot-rust.github.io/dev/march-2026-update/) · [compatibility](https://godot-rust.github.io/book/toolchain/compatibility.html) · [export to web](https://godot-rust.github.io/book/toolchain/export-web.html) · [gdext #968](https://github.com/godot-rust/gdext/issues/968) · [Godot #82865](https://github.com/godotengine/godot/issues/82865)
- [wasm32-unknown-unknown target](https://doc.rust-lang.org/beta/rustc/platform-support/wasm32-unknown-unknown.html) · [wasm32-unknown-emscripten target](https://doc.rust-lang.org/beta/rustc/platform-support/wasm32-unknown-emscripten.html) · [wasm-bindgen #2722](https://github.com/wasm-bindgen/wasm-bindgen/issues/2722)
- [matchbox](https://github.com/johanhelsing/matchbox) · [Godot WebRTC tutorial](https://docs.godotengine.org/en/stable/tutorials/networking/webrtc.html) · [WebRTCMultiplayerPeer](https://docs.godotengine.org/en/stable/classes/class_webrtcmultiplayerpeer.html) · [ggrs (NonBlockingSocket)](https://docs.rs/ggrs/latest/ggrs/)

**Rust + Postgres**
- [sqlx CHANGELOG](https://raw.githubusercontent.com/launchbadge/sqlx/main/CHANGELOG.md) · [sqlx](https://github.com/launchbadge/sqlx) · [Pool](https://docs.rs/sqlx/latest/sqlx/struct.Pool.html) · [sqlx-cli](https://crates.io/crates/sqlx-cli)
- [diesel.rs](https://diesel.rs/) · [diesel-async](https://docs.rs/diesel-async/latest/diesel_async/) · [SeaORM 2.0](https://www.sea-ql.org/blog/2026-01-12-sea-orm-2.0/) · [Rust ORMs 2026 comparison](https://rustify.rs/articles/rust-sqlx-vs-diesel-vs-seaorm-2026)
- [axum 0.8 announcement](https://tokio.rs/blog/2025-01-01-announcing-axum-0-8-0) · [axum sqlx-postgres example](https://github.com/tokio-rs/axum/blob/main/examples/sqlx-postgres/src/main.rs) · [async fn in traits (Rust 1.75)](https://blog.rust-lang.org/2023/12/21/async-fn-rpit-in-traits/) · [async-trait](https://docs.rs/async-trait)

**Backends**
- [Nakama](https://github.com/heroiclabs/nakama) · [nakama-godot](https://github.com/heroiclabs/nakama-godot) · [Rivet](https://github.com/rivet-dev/rivet) · [Rivet clients (no Godot)](https://rivet.dev/docs/clients/)
- [SpacetimeDB](https://github.com/clockworklabs/SpacetimeDB) · [LICENSE (BSL 1.1)](https://github.com/clockworklabs/SpacetimeDB/blob/master/LICENSE.txt) · [Godot tutorial](https://spacetimedb.com/docs/tutorials/godot/) · [Godot #70796 (no C# web export)](https://github.com/godotengine/godot/issues/70796)
- [Colyseus native-sdk](https://github.com/colyseus/native-sdk) · [Colyseus Godot](https://docs.colyseus.io/getting-started/godot) · [Supabase self-host](https://supabase.com/docs/guides/self-hosting/docker) · [godot-playfab](https://github.com/Structed/godot-playfab) · [GodotFirebase](https://github.com/GodotNuts/GodotFirebase) · [JavaScriptBridge](https://docs.godotengine.org/en/stable/tutorials/platform/web/javascript_bridge.html)

**WebRTC / TURN / coturn**
- [coturn](https://github.com/coturn/coturn) · [draft-uberti-behave-turn-rest](https://datatracker.ietf.org/doc/html/draft-uberti-behave-turn-rest-00) · [STUN/TURN/ICE explained](https://developer.liveswitch.io/liveswitch-server/guides/what-are-stun-turn-and-ice.html) · [WebRTC production TURN guide](https://celloip.com/blog/webrtc-turn-server-production-guide/) · [Godot WebRTCPeerConnection](https://docs.godotengine.org/en/stable/classes/class_webrtcpeerconnection.html) · [webrtc-native #7](https://github.com/godotengine/webrtc-native/issues/7)

*Corrections to brief premises: (1) SpacetimeDB is still BSL 1.1, not AGPL (AGPL is the 2031 auto-conversion). (2) SpacetimeDB has first-party Godot support via C# SDK, but that can't web-export, so the emscripten-wall conclusion for a Godot web client holds regardless.*
