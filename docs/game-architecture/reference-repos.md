# Indie Multiplayer / Game Architecture — Curated Catalog

A reading list of open-source "cookie-cutter" repos, templates, and deep writeups for learning indie multiplayer/game architecture, curated for **this** stack:

> **Target stack:** Godot 4 (web export) + Rust core · `ggrs` rollback fighter now · persistent shared world later · reducer / event-sourced style (`za_warudo.rs`, `reduce_next_state`) · self-hosted on one VPS (nginx TLS front + coturn TURN relay).

**Fit column key** — how close a resource is to the target stack:

- **High** — same language/engine or same architectural shape (Rust, rollback, reducer/event-sourced, self-host-on-one-box).
- **Med** — different engine/language but the pattern ports directly.
- **Low** — reference/study only, wrong scale, or archived; steal an idea, not the code.

Verified July 2026. Star counts and dates are as read off each source page/API this month; treat them as approximate. Sources at the bottom.

---

## 1. Rollback / p2p netcode references

The core of the fighter. `ggrs` is already the target dependency; the rest show how to wire it, how to keep a sim deterministic, and where the callback-vs-request control flow comes from.

| Resource | Lang / engine | Stars | Last activity | Fit |
|---|---|---|---|---|
| [gschup/ggrs](https://github.com/gschup/ggrs) | Rust, engine-agnostic | ~663 | v0.13.0, 2026-06-26 | **High** |
| [johanhelsing/extreme_bevy](https://github.com/johanhelsing/extreme_bevy) (+ tutorial) | Rust, Bevy, WASM | ~151 | tracks Bevy releases | **High** |
| [johanhelsing/matchbox](https://github.com/johanhelsing/matchbox) | Rust, WebRTC, WASM+native | ~1.1k | v0.14.0, 2026-02-13 | **High** |
| [gschup/bevy_ggrs](https://github.com/gschup/bevy_ggrs) | Rust, Bevy plugin | ~361 | v0.22.0, 2026-06-26 | **Med** |
| [foxssake/netfox](https://github.com/foxssake/netfox) | GDScript, Godot 4 | ~1.0k | v1.35.3, 2025-11-23 | **Med** |
| [snopek-games/godot-rollback-netcode](https://gitlab.com/snopek-games/godot-rollback-netcode) | GDScript, Godot 3.x (G4 forks) | n/a (GitLab) | created 2021, dormant | **Med** |
| [pond3r/ggpo](https://github.com/pond3r/ggpo) | C++/C, Windows | ~3.5k | dormant (~2019-2021) | **Low** |

### gschup/ggrs
Rust · engine-agnostic · https://github.com/gschup/ggrs · ~663 stars · v0.13.0 (2026-06-26)

A safe-Rust reimagining of GGPO. Instead of GGPO's callbacks it returns a **list of requests** (`SaveGameState`, `LoadGameState`, `AdvanceFrame`) for the caller to fulfill, which keeps your deterministic sim in control of the loop. Supports multiple players and spectators. This is the exact crate the fighter targets.

**Steal:**
- The request-not-callback control flow is the design idea for a pure reducer — ggrs tells you *what* to do, your `reduce_next_state` executes it deterministically.
- Built-in `sync-test` mode re-simulates and asserts state hashes match; wire it as a determinism CI gate for the reducer.
- Input-only networking: only inputs cross the wire, state is derived by replay. That replay invariant is what you also want for world persistence. Spectator support models read-only observers of the later shared world.

### johanhelsing/extreme_bevy (repo + tutorial)
Rust · Bevy · WASM · https://github.com/johanhelsing/extreme_bevy · tutorial: https://johanhelsing.studio/posts/extreme-bevy · ~151 stars · CC0

The closest published analog to the target stack minus Godot: a low-latency p2p **web** fighter built to teach the full `ggrs` + `matchbox` (WebRTC) rollback stack end to end. The multi-part writeup goes from empty project to browser-playable rollback; the repo is the finished code. Playable at helsing.studio/extreme.

**Steal:**
- Canonical wiring of `ggrs` + `matchbox` for a WASM/browser fighter — read this before you wire your own.
- Input-only sync model and rollback loop structure to mirror in the reducer/event-sourced core.
- The matchbox signaling-server + p2p handshake flow, which you self-host on the one VPS.

### johanhelsing/matchbox (+ bevy_matchbox)
Rust · WebRTC · WASM+native · https://github.com/johanhelsing/matchbox · ~1.1k stars · v0.14.0 (2026-02-13)

Painless p2p WebRTC for Rust native+WASM, giving UDP-like unreliable/unordered channels in the browser. Four crates: `matchbox_socket` (GGRS-compatible socket), `matchbox_signaling` (build-your-own), `matchbox_server` (ready full-mesh signaling server), `bevy_matchbox` (Bevy glue).

**Steal:**
- `matchbox_server` is a single self-hostable binary for signaling on the VPS — after handshake data flows peer-to-peer, so the box only brokers connections (complements the coturn relay).
- `matchbox_socket` is GGRS-compatible and drops straight under ggrs, giving browser-export p2p without hand-writing WebRTC.
- Configurable reliable + unreliable channels: unreliable for rollback inputs, reliable for the event-sourced/persistent-world sync. Note: Godot 4 web export consumes matchbox via its signaling protocol or a Rust bridge, not `bevy_matchbox` directly.

### gschup/bevy_ggrs
Rust · Bevy plugin · https://github.com/gschup/bevy_ggrs · ~361 stars · v0.22.0 (2026-06-26)

Bevy plugin wrapping GGRS: runs advance/rollback on a dedicated schedule, snapshotting only registered components/resources, and solves entity-ID instability with `Rollback` / `RollbackId` markers. Bevy-specific, but the snapshot discipline is engine-agnostic.

**Steal:**
- The save/load/advance request pattern (one `SaveGameState` + `AdvanceFrame` per normal frame; `LoadGameState` then re-sim pairs on rollback) maps onto a reducer that snapshots state.
- `docs/pitfalls.md`, `docs/debugging-desyncs.md`, `docs/architecture.md` are direct guidance for keeping a reducer deterministic.
- The "register only what rolls back" discipline bounds snapshot size — useful once the world grows persistent (non-rolled-back) state that must stay *out* of the rollback set.

### foxssake/netfox
GDScript · Godot 4.x · https://github.com/foxssake/netfox · ~1.0k stars · v1.35.3 (2025-11-23)

The dominant, actively maintained Godot 4 addon suite for rollback + prediction: `netfox` (`RollbackSynchronizer`, `TickInterpolator`, tick/timing), `netfox.noray` (NAT punch-through/relay), `netfox.extras` (helpers + a fighter-ish "Forest Brawl" demo). The most on-point maintained **GDScript** rollback prior art, even though the sim core here is Rust.

**Steal:**
- `rollback/rollback-synchronizer.gd` is a working Godot-4-native rollback loop to study for the Godot side.
- `netfox.noray` is a self-hostable relay/connection-broker pattern relevant to the coturn/TURN VPS setup.
- The tick/timing layer for consistent cross-machine stepping, to align with the ggrs frame counter.

### snopek-games/godot-rollback-netcode
GDScript · Godot 3.x (G4 forks exist) · https://gitlab.com/snopek-games/godot-rollback-netcode · created 2021, dormant · MIT

David Snopek's rollback addon built around a `SyncManager` autoload. Goes beyond save/load/input to cover the hard determinism cases: timers, animation, RNG, sound, plus desync tooling. Backed by a well-known YouTube tutorial series. Original is Godot 3.x-first; for Godot 4 web export prefer the forks ([Kethku/gdrollback](https://github.com/Kethku/gdrollback), [V-Sekai/godot-network-rollback](https://github.com/V-Sekai/godot-network-rollback)) or netfox.

**Steal:**
- Its treatment of RNG seeding, animation frames, and sound — the determinism cases a naive reducer forgets.
- The desync-detection / state-hashing tooling as a template for validating reducer determinism.
- `SyncManager` as a reference for the Godot-side authority that owns tick/state, mappable onto the `za_warudo` FSM.

### pond3r/ggpo
C++/C · Windows · https://github.com/pond3r/ggpo · ~3.5k stars · dormant

The original 2009 rollback SDK that pioneered input-prediction + speculative execution for p2p fighting games. Callback-based API; ships the "Vector War" sample syncing two clients. Study source, not a dependency.

**Steal:**
- `doc/DeveloperGuide.md` is the primary-source explanation of prediction window, input delay, and the save/advance/load contract every later crate reimplements.
- Vector War as the minimal mental model of a synchronized 2-client sim.
- Historical grounding for why ggrs chose a request-list over callbacks — informs whether your reducer pulls (ggrs style) or is pushed (ggpo style).

---

## 2. Reducer / event-sourced / authoritative state-sync frameworks

Different languages, but these are the architectural cousins of the `reduce_next_state` design: pure move/reducer functions, an append-only event/delta log, and client-vs-server authority splits.

| Resource | Lang | Stars | Last activity | Fit |
|---|---|---|---|---|
| [boardgameio/boardgame.io](https://github.com/boardgameio/boardgame.io) | TypeScript | ~12.4k | active | **Med** |
| [colyseus/colyseus](https://github.com/colyseus/colyseus) | TypeScript / Node | ~7.0k | v0.17, 2026-02-06 | **Med** |
| [Antman261/es-reduxed](https://github.com/Antman261/es-reduxed) | TypeScript / PL/pgSQL | ~5 | small/illustrative | **Low** |
| (concept) Redux-as-event-store, Fowler, Factorio FFF | — | — | — | **Med** |

### boardgame.io
TypeScript · https://github.com/boardgameio/boardgame.io · ~12.4k stars · actively maintained

An engine for turn-based multiplayer where you write pure move functions `(G, ctx) => G`; the engine handles networking, storage, turn/phase flow, and sync so game code never touches sockets. Its reducer core (`src/core/reducer.ts`) is a switch over actions (`MAKE_MOVE`, `GAME_EVENT`, `UNDO`, `REDO`, `PATCH`, `PLUGIN`); Immer `produce()` is applied inside move execution via the default Immer plugin; every applied action appends to a `deltalog`, giving replayable logs and undo/redo. Determinism comes from a seeded-PRNG plugin.

**Steal:**
- The pure-move contract `reduce(state, action) -> state` plus a per-tick `deltalog` is exactly an event log you can replay for rollback and for persisting the shared world.
- The `isClient` split in `CreateGameReducer`: the *same* reducer runs on client (prediction) and server (authoritative), with a `NoClient` gate marking actions the client must not resolve locally (hidden info, server RNG). Mirror this for server-authoritative moves in the persistent world.
- Seeded-PRNG-as-plugin: keep all nondeterminism behind an explicit injected RNG so replays and rollbacks are bit-identical.

### Colyseus
TypeScript / Node · https://github.com/colyseus/colyseus · ~7.0k stars · v0.17 (2026-02-06)

Authoritative multiplayer framework where game logic runs server-side in "Rooms"; clients send input, the server mutates state and pushes it out. Its `@colyseus/schema` defines typed server state and auto-synchronizes it to clients as **delta-compressed binary patches** (only changed fields on the wire). Ships matchmaking, reconnection, and Redis-backed horizontal scale.

**Steal:**
- The `@colyseus/schema` delta encoder (schema-declared fields, binary diff per tick) is the pattern for streaming only changed world regions to each client cheaply in the persistent world.
- Room-as-authoritative-boundary: one room owns one deterministic sim instance — fits a single-VPS deployment where each match/zone is its own reducer loop.
- Reconnection + queuing built into the room lifecycle, directly relevant to a persistent world where players drop and rejoin the same authoritative state.

### es-reduxed + event-sourcing concepts
TypeScript / PL/pgSQL · https://github.com/Antman261/es-reduxed · ~5 stars · illustrative only

A Redux `eventStoreReduxEnhancer` that persists dispatched **events** (past-tense `AccountCreated` extending `EventBase`, not `Action`) to a PostgreSQL event store, then replays them through reducers to rebuild state. Tiny project — a readable pattern, not a dependency.

**Steal:**
- The events-vs-actions discipline (past-tense facts, discriminated unions for type-safe replay) and the "reducer + append-only Postgres log" split — a concrete shape for persisting the shared world on one VPS.

**Supporting concept reads:** [Martin Fowler — Event Sourcing](https://martinfowler.com/eaaDev/EventSourcing.html) (canonical definition) · [Tableau Eng — Redux: Command Bus or Event Store?](https://engineering.tableau.com/redux-command-bus-or-event-store-2c4c044cd481) · [Eric Elliott — Command pattern, Event Sourcing, and Redux](https://medium.com/@_ericelliott/the-command-pattern-event-sourcing-and-redux-are-all-different-architectures-but-they-all-3e36b70cbc60). All argue Redux is already client-side event sourcing (replay actions → deterministic state), directly analogous to replaying inputs in rollback. For a game-specific gold standard, see the Factorio FFF series in §6.

---

## 3. Godot authoritative-server / client-prediction templates

Prior art for the Godot side and for the later authoritative shared world. **Web-export reality:** none of these document web-export support, and Godot web export cannot use `ENetMultiplayerPeer` (UDP) — only **WebRTC** (`WebRTCMultiplayerPeer`, UDP-like) or **WebSocket** (TCP, reliable). This matches the existing TURNS/443 constraint. A Rust-owned sim also sidesteps MonkeNet's custom-fork problem (below).

| Resource | Lang / engine | Stars | Last activity | Fit |
|---|---|---|---|---|
| [grazianobolla/godot4-multiplayer-template (MonkeNet)](https://github.com/grazianobolla/godot4-multiplayer-template) | C#, Godot 4 (custom fork) | ~243 | 2026-04-03 | **Med** |
| [LazerCube/godot-multiplayer](https://github.com/LazerCube/godot-multiplayer) | C#, Godot 4 (beta) | ~40 | 2023-01-31 (stale) | **Med** |
| [devmoreir4/godot-3d-multiplayer-template](https://github.com/devmoreir4/godot-3d-multiplayer-template) | GDScript, Godot 4.6 | ~146 | 2026-06-12 | **Med** |
| [Godot official multiplayer docs](https://docs.godotengine.org/en/stable/tutorials/networking/high_level_multiplayer.html) | GDScript/C# | — | current | **Med** |
| [Svengali/gd_multiplayer_template](https://github.com/Svengali/gd_multiplayer_template) | C#, Godot 4 | 0 | 2024-07-01 | **Low** |
| [AlixBarreaux/godot-authoritative-server](https://github.com/AlixBarreaux/godot-authoritative-server) | GDScript, Godot 3.x | ~4 | 2020-10-02 (archived) | **Low** |

### grazianobolla/godot4-multiplayer-template (MonkeNet)
C# · Godot 4 · https://github.com/grazianobolla/godot4-multiplayer-template · ~243 stars · pushed 2026-04-03

The best-known Godot 4 client-authoritative-server addon. Copy `addons/monke-net/`; `MonkeNetManager` starts server or client. Full stack: CharacterBody client-side prediction + reconciliation, snapshot interpolation, clock sync, state replication, delta compression of inputs and entity state, and player-to-player lag compensation. **Requires a custom Godot fork** for manual physics stepping — a hard blocker for web export.

**Steal:**
- The delta-compression scheme for inputs and entity states.
- The lag-compensation (rewind) model for hit validation in a fighter.
- The manual-physics-step insight itself: a Rust rollback core owns the sim tick, which is exactly why MonkeNet needed a fork and you won't. Read it to understand what you're avoiding.

### LazerCube/godot-multiplayer
C# · Godot 4 (beta) · https://github.com/LazerCube/godot-multiplayer · ~40 stars · 2023-01-31 (stale, reference-only)

Authoritative-server demo with a 3D character controller. Implements client-side prediction, remote-entity interpolation, backwards reconciliation + replay, server-side lag compensation, and — notably — **Overwatch-style real-time client sim-speed adjustment** to tune the server's input buffer. Includes RCON server management.

**Steal:**
- The Overwatch input-buffer / dynamic clock-scaling technique (adjust client tick rate so inputs arrive just-in-time) — directly applicable to rollback netcode pacing.
- The reconcile-and-replay loop.
- The RCON pattern for admin control of the single-VPS authoritative world.

### devmoreir4/godot-3d-multiplayer-template
GDScript · Godot 4.6 · https://github.com/devmoreir4/godot-3d-multiplayer-template · ~146 stars · pushed 2026-06-12

A maintained foundational 3D template with **server-authoritative combat validation** (server checks attack window, equipped weapon, target, hit distance, duplicate hits) and **server-authoritative inventory** (20-slot backpack, stacking, drag-drop, equip slots). Uses Godot's high-level sync — **no** client-side prediction/reconciliation.

**Steal:**
- The server-authoritative hit-validation checklist (attack window, weapon, distance, duplicate-hit guard) — a clean spec for authoritative fighter hit registration.
- The server-authoritative inventory model for the persistent world's items — maps onto the `Item` gas/stroke model.

### Godot official high-level multiplayer docs
GDScript/C# · docs.godotengine.org · current

- [High-level multiplayer / RPC](https://docs.godotengine.org/en/stable/tutorials/networking/high_level_multiplayer.html) — `ENetMultiplayerPeer`, the `@rpc` annotation (`authority`/`any_peer`, `call_local`/`call_remote`, reliable/unreliable). Explicitly says treat all client input as untrusted and keep gameplay-critical decisions server-side. Notes HTML5 lacks raw TCP/UDP, so ENet does **not** work in web export.
- [MultiplayerSpawner](https://docs.godotengine.org/en/stable/classes/class_multiplayerspawner.html) — auto-replicates spawnable nodes from authority to peers.
- [MultiplayerSynchronizer](https://docs.godotengine.org/en/stable/classes/class_multiplayersynchronizer.html) — replicates node property state on a configurable interval/authority.
- [WebSocket](https://docs.godotengine.org/en/stable/tutorials/networking/websocket.html) — `WebSocketMultiplayerPeer` works in both native and web exports and is High-Level-Multiplayer-compatible (TCP, reliable/ordered only).

**Steal:** the untrusted-client posture and the web-export transport matrix (WebRTC for latency, WebSocket/443 as the VPN-proof fallback). For the low-frequency shared-world state, `MultiplayerSynchronizer` + `MultiplayerSpawner` may be enough without hand-rolling replication.

### Low-fit / flagged
- **Svengali/gd_multiplayer_template** (https://github.com/Svengali/gd_multiplayer_template, 0 stars, 2024-07-01): real, C#, does manual byte-packing + prediction/reconcile + clock sync, but essentially a personal repo with near-zero footprint. Steal the manual byte-packing-instead-of-RPC idea (matters when a Rust core owns serialization). Confirm this is the intended entry — the popular maintained option is netfox (§1).
- **AlixBarreaux/godot-authoritative-server** (https://github.com/AlixBarreaux/godot-authoritative-server, ~4 stars, **archived** 2020, Godot 3.x, localhost-only): a bare separate-server-binary + runtime-join skeleton. Listed for completeness; superseded.

---

## 4. Self-hostable backends with examples

For the persistent shared world: accounts, storage, presence, realtime sync. **SpacetimeDB is the standout** — its server model is literally Rust reducers over tables, the same shape as `reduce_next_state`.

| Resource | Lang | Stars | Last activity | One VPS | Fit |
|---|---|---|---|---|---|
| [clockworklabs/SpacetimeDB](https://github.com/clockworklabs/SpacetimeDB) | Rust | ~24.8k | v2.6.0, 2026-06-16 | yes | **High** |
| [heroiclabs/nakama](https://github.com/heroiclabs/nakama) | Go | ~12.8k | active 2026 | yes | **Med** |
| [supabase/supabase](https://github.com/supabase/supabase) | TS + polyglot | ~100k | very active | yes (docker) | **Med** |
| [rivet-dev/rivet](https://github.com/rivet-dev/rivet) (pivoted) | Rust / TS | ~5.7k | v2.3.2, 2026-06 | yes | **Low/Med** |

### SpacetimeDB
Rust · https://github.com/clockworklabs/SpacetimeDB · ~24.8k stars · v2.6.0 (2026-06-16)

A relational database that *is* the server. You upload a WASM **module** defining **tables** (data) and **reducers** (logic); clients connect directly, call reducers, and subscribe to tables, with state syncing in realtime. No separate app server or orchestration. Self-hosts as `spacetimedb-standalone` or `docker run ... clockworklabs/spacetime start` — single-host is the intended small-scale deployment.

**Steal (strongest fit in this section):**
- The reducer *is* the model: a Rust function taking an event/args that atomically mutates table rows — a transactional `reduce_next_state`. Mental port cost from `za_warudo.rs` is low.
- Event-sourced by construction: every reducer call is a transaction in the commit log; table subscriptions are the derived view. Use it for the durable world half (accounts, home rooms, inventory, strokes).
- **Tension to flag:** it's authoritative-server + subscription, *not* rollback/prediction. Keep your own ggrs rollback for the 60fps combat loop; use SpacetimeDB for durable world state, not per-frame inputs.
- **Rust example to clone:** [demo/Blackholio `server-rust/`](https://github.com/clockworklabs/SpacetimeDB/tree/master/demo/Blackholio) — an agar.io-style MMO demo with a full Rust module (`publish.sh` deploy + client codegen). This is the canonical Rust reducer/table reference. (BitCraft is Clockwork Labs' proprietary showcase MMO, **not** cloneable — Blackholio is the open example.)

### Nakama
Go · https://github.com/heroiclabs/nakama · ~12.8k stars · active 2026

Scalable open-source game backend: accounts, chat, social graph, matchmaker, leaderboards, storage engine, authoritative realtime multiplayer. Single Go binary + Postgres-wire DB (CockroachDB/Postgres); demos ship a `docker-compose.yml` that stands the whole thing up on one host. Comfortable on one VPS for a small persistent world.

**Steal:**
- The authoritative match-handler loop (server-owned tick loop with per-match state) as a reference for structuring a server-authoritative room — analogous to the `za_warudo` FSM.
- The storage-engine API (versioned key-value with per-user/public ACLs) as a pattern for persisting home-room state, char/color/name, stroke presets.
- The matchmaker ticket/query model for pairing rollback opponents.

**Godot pieces:** [heroiclabs/nakama-godot](https://github.com/heroiclabs/nakama-godot) — GDScript client SDK, **Godot 4.0+**, ~758 stars, last release 3.4.0 (2024-03-19), stable but infrequently updated, integrates with Godot's High-Level Multiplayer. [heroiclabs/nakama-godot-demo](https://github.com/heroiclabs/nakama-godot-demo) — full demo (auth, storage, sockets, chat, char color customization) but **targets Godot 3.4**, so it's an architecture reference that needs a Godot 4 port.

### Supabase
TS + polyglot (Postgres/C, GoTrue/Go, Realtime/Elixir) · https://github.com/supabase/supabase · ~100k stars · very active

A self-hostable bundle around Postgres: PostgREST (auto REST), GoTrue (auth), Realtime (Postgres change streams over websockets), Storage, Edge Functions, Studio UI. Runs on one VPS via the official `docker/` compose stack. Self-host trade-offs: no managed backups/PITR/branching; upgrades manual — plan a backup cron.

**Steal:**
- GoTrue as a drop-in accounts/identity layer so you don't hand-roll player accounts.
- Realtime (logical replication → websocket broadcast) as a pattern for pushing home-room/presence updates to Godot without inventing pub/sub.
- Row-Level Security as the authorization model for "each player owns their home room" data isolation.

### Rivet (pivoted — verify before slotting)
Rust / TS · https://github.com/rivet-dev/rivet · ~5.7k stars · v2.3.2 (2026-06)

Org renamed `rivet-gg` → `rivet-dev`. **Rivet pivoted away from being a game backend** — it now markets "Rivet Actors, the primitive for stateful workloads" (AI agents, collaborative apps, durable execution). The matchmaking/game-server framing is no longer the product. Single Rust binary, Docker-deployable, backs onto Postgres/filesystem/FoundationDB.

**Steal (given the pivot):**
- The actor model (long-running lightweight process with in-memory state + automatic persistence) maps onto "one persistent home room = one actor" — a reference architecture for a stateful shared world even though it's no longer game-branded.
- Rust single-binary + Postgres deploy shape is close to the target stack.
- **Caveat:** no longer fits a "game backends" slot cleanly; treat as a "stateful actor runtime" reference.

---

## 5. Infra / orchestration (for scale-out, not one VPS)

Cluster-scale Kubernetes tools. On one VPS both are **overkill** — steal the design patterns, implement them directly in Rust against the existing coturn relay. They matter only if you move to server-authoritative dedicated servers across multiple nodes and/or high-volume ranked matchmaking.

| Resource | Lang | Stars | Last activity | Fit |
|---|---|---|---|---|
| [googleforgames/agones](https://github.com/googleforgames/agones) | Go | ~6.9k | v1.59.0, 2026-07-01 | **Low** (future) |
| [googleforgames/open-match](https://github.com/googleforgames/open-match) (v1) | Go | ~3.4k | v1.8.1, 2023-12 (stalled) | **Low** |
| [googleforgames/open-match2](https://github.com/googleforgames/open-match2) | Go | ~56 | commits to 2026-01 (preview) | **Low** |

### Google Agones
Go · https://github.com/googleforgames/agones · ~6.9k stars · v1.59.0 (2026-07-01), actively maintained

A Kubernetes-native library for hosting, running, and autoscaling dedicated game servers via `GameServer` and `Fleet` custom resources: lifecycle + health checking, fleet autoscaling, per-server metrics.

**Steal (as portable design, no k8s):**
- The allocation state machine `Ready → Allocated → Shutdown` per instance, with an SDK sidecar the process calls to mark itself healthy/ready/allocated. Portable to a plain systemd/process-per-match design on one VPS.
- The Fleet + FleetAutoscaler idea (buffer of warm, pre-booted servers) as a model for keeping N idle rollback sessions hot so matchmaking hands out an already-running instance.
- **When it matters:** only past one box, hundreds+ of concurrent matches across a cluster. A rollback fighter is mostly p2p/relay (your coturn), so you may never need authoritative dedicated servers at all.

### Open Match — status flagged
Go · https://github.com/googleforgames/open-match · ~3.4k stars · **v1 stalled** (last release v1.8.1, Dec 2023)

A matchmaking *framework* (not a matchmaker): you supply match logic as gRPC "Match Functions"; it handles the scalable ticket pool, queries, and evaluation. **Maintenance status matters here:** v1 has had no release in ~2.5 years (README still says "in active development" but the cadence contradicts it — de facto unmaintained, not formally archived). Development moved to [open-match2](https://github.com/googleforgames/open-match2) (~56 stars, commits into Jan 2026, public preview, low adoption).

**Steal (as design, in Rust):**
- The ticket + pool + match-function decomposition: players submit a ticket with attributes (rank, region, latency), a pool query filters candidates, a pure function scores/forms matches. Implementable in a few hundred lines of Rust against your relay, no k8s.
- The evaluator concept (dedupe when multiple proposed matches claim the same ticket) — relevant the moment you add ranked queues or party matchmaking.
- **When it matters:** high concurrent-queue volume on k8s/Redis. On one VPS, a hand-rolled Rust matchmaker (in-memory queue + latency/rank buckets) beats standing this up. **Do not adopt v1**; if you ever want this, evaluate open-match2.

---

## 6. Deep writeups / blogs / talks (not repos)

The theory and shipped-game war stories. Read Overwatch and the Factorio FFFs first for architecture; Infil and Gambetta for the mental model; Gaffer for the from-scratch detail.

| Resource | Author | Fit |
|---|---|---|
| [Gaffer On Games](https://gafferongames.com/) | Glenn Fiedler | **High** |
| [Johan Helsing blog](https://johanhelsing.studio/posts/extreme-bevy) | Johan Helsing | **High** |
| [metabrew — Rock and Rollback](https://www.metabrew.com/article/rock-and-rollback-realtime-multiplayer-games-with-bevy) | Richard Jones | **High** |
| [Factorio Friday Facts (deterministic lockstep)](https://www.factorio.com/blog/post/fff-76) | Factorio devs | **High** |
| [Infil — Fight the Lag / netcode explainer](https://words.infil.net/w02-netcode-p1.html) | Infil | **High** |
| [Gabriel Gambetta — Fast-Paced Multiplayer](https://www.gabrielgambetta.com/client-server-game-architecture.html) | Gabriel Gambetta | **Med** |
| [Valve — Source Multiplayer Networking](https://developer.valvesoftware.com/wiki/Source_Multiplayer_Networking) | Valve | **Med** |
| [Overwatch Gameplay Architecture and Netcode (GDC 2017)](https://www.youtube.com/watch?v=W3aieHjyNvw) | Tim Ford | **Med** |

### Gaffer On Games — Glenn Fiedler
https://gafferongames.com/ — the canonical from-scratch treatment of networking a physics/game sim three ways. Fiedler no longer posts here (banner points to mas-bandwidth.com) but the classic articles remain live.
- [Deterministic Lockstep](https://gafferongames.com/post/deterministic_lockstep/) + [Floating Point Determinism](https://gafferongames.com/post/floating_point_determinism/) — why you must avoid float nondeterminism (or go fixed-point) so rollback re-sim matches across peers. Directly load-bearing for the ggrs fighter.
- [Snapshot Interpolation](https://gafferongames.com/post/snapshot_interpolation/) + [State Synchronization](https://gafferongames.com/post/state_synchronization/) — the blueprint for the persistent shared world tier where full determinism across many clients is impractical.
- [Snapshot Compression](https://gafferongames.com/post/snapshot_compression/) + [Reliable UDP](https://gafferongames.com/post/reliability_ordering_and_congestion_avoidance_over_udp/) — quantization/delta/bit-packing to keep the event/reducer stream cheap on one VPS.

### Johan Helsing — Bevy rollback / Matchbox
https://johanhelsing.studio/posts/extreme-bevy — hands-on Rust series building a p2p rollback web game with Bevy + GGRS + Matchbox, including [desync detection](https://johanhelsing.studio/posts/extreme-bevy-desync-detection/) and [procedural gen](https://johanhelsing.studio/posts/extreme-bevy-5). The most directly relevant Rust-ecosystem material. Also the [Matchbox intro](https://johanhelsing.studio/posts/introducing-matchbox) posts. **Steal:** input-serialization + rollback-component patterns (transfer to Godot), the per-frame checksum desync detection, and the WebRTC signaling design (pairs with your TURNS/443 work).

### metabrew — Rock and Rollback
https://www.metabrew.com/article/rock-and-rollback-realtime-multiplayer-games-with-bevy — a candid solo-dev writeup: building browser asteroids in Rust + Bevy, hand-rolling rollback (a mess), then adopting the Lightyear crate. **Steal:** honest cost/benefit of hand-rolled rollback vs. a library (sanity check for the shared-world tier), WebTransport/WASM transport notes, and the concrete state-ownership failure modes a Rust newcomer hits.

### Factorio — Friday Facts (deterministic lockstep)
[FFF #76 (MP inside out)](https://www.factorio.com/blog/post/fff-76) · [FFF #302 (megapacket)](https://www.factorio.com/blog/post/fff-302) · [FFF #188 (desync)](https://factorio.com/blog/post/fff-188) · [FFF #47 (CRC fun)](https://www.factorio.com/blog/post/fff-47). The gold-standard running blog on shipping deterministic lockstep: send only inputs, every peer simulates identically, and the war against desyncs. **Steal:** the input-only sync + CRC/checksum desync pipeline (log inputs, hash state, diff on divergence — exactly the reducer/event-sourced design), latency-hiding (FFF #302), and the "determinism is hard" checklist (iteration order, floats, hash maps) for the Rust sim.

### Infil — "Fight the Lag" netcode explainer
https://words.infil.net/w02-netcode-p1.html (through [p4](https://words.infil.net/w02-netcode-p4.html)) + [Rollback glossary](https://glossary.infil.net/?t=Rollback+Netcode). The definitive lay-audience explainer of delay-based vs. rollback netcode for fighting games. **Steal:** the clearest articulation of the rollback UX contract (local input never delayed, remote corrections hidden when inputs don't change) to hold the fighter's feel to; framing for tuning input delay vs. rollback frames; a good onboarding doc for players/testers.

### Gabriel Gambetta — Fast-Paced Multiplayer
https://www.gabrielgambetta.com/client-server-game-architecture.html — the most approachable intro to authoritative-server multiplayer, with interactive JS demos: [prediction + reconciliation](https://www.gabrielgambetta.com/client-side-prediction-server-reconciliation.html), [entity interpolation](https://www.gabrielgambetta.com/entity-interpolation.html), lag compensation, [live demo](https://www.gabrielgambetta.com/client-side-prediction-live-demo.html). **Steal:** the server-reconciliation loop (apply authoritative state, re-apply pending unacked inputs) is structurally identical to ggrs rollback — good for framing rollback to contributors; entity interpolation for rendering non-rolled-back players in the shared world.

### Valve — Source Multiplayer Networking
[Source Multiplayer Networking](https://developer.valvesoftware.com/wiki/Source_Multiplayer_Networking) · [Lag Compensation](https://developer.valvesoftware.com/wiki/Lag_Compensation) · [Bernier paper](https://developer.valvesoftware.com/wiki/Latency_Compensating_Methods_in_Client/Server_In-game_Protocol_Design_and_Optimization). Production description of tick-based sim, entity snapshots, client interpolation, input prediction, and server-side "rewind time" lag compensation. **Steal:** the fixed-tick server loop with `interp` delay for the shared-world server; lag compensation if the world ever needs hit registration against moving players; the usercmd framing maps onto a reducer consuming ordered input events per tick.

### GDC talks
- **Overwatch Gameplay Architecture and Netcode** — Tim Ford, GDC 2017. [Vault](https://www.gdcvault.com/play/1024001/-Overwatch-Gameplay-Architecture-and) · [YouTube](https://www.youtube.com/watch?v=W3aieHjyNvw). ECS with pure-data components enabling snapshot + rollback; predicts abilities/projectiles; ~60Hz command frames. **The closest architectural match** — ECS pure-data ↔ reducer/event-sourced state, snapshot+rollback ↔ ggrs. Watch first.
- **I Shot You First: Networking Halo: Reach** — David Aldridge, Bungie, GDC 2011. [Vault](https://www.gdcvault.com/play/1014345/I-Shot-You-First-Networking) · [YouTube](https://www.youtube.com/watch?v=h47zZrqjgLc). Priority-based replication for 16-player P2P — the model for bandwidth-budgeting many entities in the shared world.
- **It IS Rocket Science! Rocket League** — Jared Cone, Psyonix, GDC 2018. [Vault](https://www.gdcvault.com/play/1024972/It-IS-Rocket-Science-The) · [YouTube](https://www.youtube.com/watch?v=ueEmiDM94IE). Keeping a chaotic authoritative-server physics object feel local via prediction.
- **For Honor:** no canonical GDC networking talk exists. Its P2P → dedicated-server (AWS GameLift) migration is documented via [AWS GameTech blog](https://aws.amazon.com/blogs/gametech/for-honor-friday-the-13th-the-game-move-from-p2p-to-the-cloud-to-improve-player-experience/), not GDC. Treat as secondary.

---

## Quick "where do I start" map

1. **Wire the fighter:** read [extreme_bevy](https://github.com/johanhelsing/extreme_bevy) + [tutorial](https://johanhelsing.studio/posts/extreme-bevy), then [ggrs](https://github.com/gschup/ggrs) API + `bevy_ggrs` `docs/`. Watch [Overwatch GDC](https://www.youtube.com/watch?v=W3aieHjyNvw). Read [Infil](https://words.infil.net/w02-netcode-p1.html) for the UX contract.
2. **Keep the sim deterministic:** [Gaffer floating-point determinism](https://gafferongames.com/post/floating_point_determinism/) + [Factorio FFF desync](https://factorio.com/blog/post/fff-188). Adopt ggrs `sync-test` as CI.
3. **Godot side / web transport:** [Godot high-level multiplayer docs](https://docs.godotengine.org/en/stable/tutorials/networking/high_level_multiplayer.html) (web = WebRTC/WebSocket, not ENet) + [netfox](https://github.com/foxssake/netfox) for a working GDScript rollback loop.
4. **Persistent shared world:** [SpacetimeDB Blackholio Rust module](https://github.com/clockworklabs/SpacetimeDB/tree/master/demo/Blackholio) (reducers-as-server, closest fit), plus [Gaffer state sync](https://gafferongames.com/post/state_synchronization/) + [Colyseus schema](https://github.com/colyseus/colyseus) for delta-streaming ideas.
5. **Ignore for now:** Agones, Open Match — cluster scale, not one VPS.

---

## Sources

Rollback / p2p: [gschup/ggrs](https://github.com/gschup/ggrs) · [johanhelsing/extreme_bevy](https://github.com/johanhelsing/extreme_bevy) · [Extreme Bevy tutorial](https://johanhelsing.studio/posts/extreme-bevy) · [gschup/bevy_ggrs](https://github.com/gschup/bevy_ggrs) · [johanhelsing/matchbox](https://github.com/johanhelsing/matchbox) · [bevy_matchbox crate](https://crates.io/crates/bevy_matchbox) · [foxssake/netfox](https://github.com/foxssake/netfox) · [snopek-games/godot-rollback-netcode](https://gitlab.com/snopek-games/godot-rollback-netcode) · [Kethku/gdrollback](https://github.com/Kethku/gdrollback) · [pond3r/ggpo](https://github.com/pond3r/ggpo)

Reducer / event-sourced: [boardgameio/boardgame.io](https://github.com/boardgameio/boardgame.io) · [colyseus/colyseus](https://github.com/colyseus/colyseus) · [Antman261/es-reduxed](https://github.com/Antman261/es-reduxed) · [Martin Fowler — Event Sourcing](https://martinfowler.com/eaaDev/EventSourcing.html) · [Tableau — Redux: Command Bus or Event Store?](https://engineering.tableau.com/redux-command-bus-or-event-store-2c4c044cd481)

Godot templates: [grazianobolla/godot4-multiplayer-template](https://github.com/grazianobolla/godot4-multiplayer-template) · [LazerCube/godot-multiplayer](https://github.com/LazerCube/godot-multiplayer) · [devmoreir4/godot-3d-multiplayer-template](https://github.com/devmoreir4/godot-3d-multiplayer-template) · [Svengali/gd_multiplayer_template](https://github.com/Svengali/gd_multiplayer_template) · [AlixBarreaux/godot-authoritative-server](https://github.com/AlixBarreaux/godot-authoritative-server) · [Godot high-level multiplayer docs](https://docs.godotengine.org/en/stable/tutorials/networking/high_level_multiplayer.html) · [MultiplayerSpawner](https://docs.godotengine.org/en/stable/classes/class_multiplayerspawner.html) · [MultiplayerSynchronizer](https://docs.godotengine.org/en/stable/classes/class_multiplayersynchronizer.html) · [WebSocket](https://docs.godotengine.org/en/stable/tutorials/networking/websocket.html)

Self-hostable backends: [clockworklabs/SpacetimeDB](https://github.com/clockworklabs/SpacetimeDB) · [Blackholio Rust module](https://github.com/clockworklabs/SpacetimeDB/tree/master/demo/Blackholio) · [heroiclabs/nakama](https://github.com/heroiclabs/nakama) · [heroiclabs/nakama-godot](https://github.com/heroiclabs/nakama-godot) · [heroiclabs/nakama-godot-demo](https://github.com/heroiclabs/nakama-godot-demo) · [rivet-dev/rivet](https://github.com/rivet-dev/rivet) · [supabase/supabase](https://github.com/supabase/supabase) · [Supabase self-hosting](https://supabase.com/docs/guides/self-hosting/docker)

Infra: [googleforgames/agones](https://github.com/googleforgames/agones) · [googleforgames/open-match](https://github.com/googleforgames/open-match) · [googleforgames/open-match2](https://github.com/googleforgames/open-match2)

Writeups / talks: [Gaffer On Games](https://gafferongames.com/) · [Gabriel Gambetta](https://www.gabrielgambetta.com/client-server-game-architecture.html) · [Valve Source Multiplayer Networking](https://developer.valvesoftware.com/wiki/Source_Multiplayer_Networking) · [Overwatch GDC 2017](https://www.youtube.com/watch?v=W3aieHjyNvw) · [Halo Reach GDC 2011](https://www.gdcvault.com/play/1014345/I-Shot-You-First-Networking) · [Rocket League GDC 2018](https://www.gdcvault.com/play/1024972/It-IS-Rocket-Science-The) · [Johan Helsing blog](https://johanhelsing.studio/posts/extreme-bevy) · [metabrew — Rock and Rollback](https://www.metabrew.com/article/rock-and-rollback-realtime-multiplayer-games-with-bevy) · [Infil — netcode explainer](https://words.infil.net/w02-netcode-p1.html) · [Factorio FFF #76](https://www.factorio.com/blog/post/fff-76)
