# Real-Time Game Netcode: A Reference for a Godot/Rust Indie Project

Scope: internal reference for a networked game that starts as a rollback-based 2D fighter (Godot web client via emscripten + Rust simulation core using `ggrs`) and evolves toward a persistent, Terraria-style shared world on a single self-hosted VPS. Facts verified via web search as of July 2026. "→ Applies to this project" callouts map each concept to those two targets.

---

## 0. The one-sentence mental model

There are exactly two things you can put on the wire: **inputs** or **state**. Every netcode architecture is a choice about which one you send, who is authoritative over the result, and how you hide the speed of light. Input-sync (lockstep, rollback) sends inputs and requires bit-exact determinism; state-sync (client prediction + server reconciliation, snapshot interpolation, lag compensation) sends state and tolerates non-determinism because one authoritative machine owns the truth.

→ **Applies to this project.** You will end up running **both**, layered, not blended: rollback/input-sync for the fast fighter combat, authoritative state-sync for the durable world. §8 covers why layering is the only coherent way to do this.

---

## 1. The five netcode families

Not mutually exclusive. Fighting games use #2. Authoritative-server shooters stack #3 + #4 + #5. RTS uses #1.

### 1.1 Deterministic lockstep (input-sync)

Every client runs the identical simulation in parallel; only inputs cross the wire. A client may advance frame N only once it holds every player's input for N.

| Property | Value |
|---|---|
| On the wire | Inputs/commands only — never state |
| Bandwidth | Very low, ~**constant regardless of entity count** |
| CPU | Low — one sim pass/frame, no resim |
| Determinism | **Strict bitwise** across CPU/compiler/OS; float divergence = classic desync → fixed-point |
| Failure modes | (1) **slowest-peer coupling** (frame waits on highest-ping input); (2) **desync** — one differing tick diverges permanently, caught only by checksum |
| When | High-entity-count games (RTS: StarCraft, AoE, Supreme Commander); replays-as-input-logs |

AoE masked slowest-peer by scheduling inputs a fixed number of turns ahead (2-turn command delay).

### 1.2 Rollback netcode (GGPO / ggrs)

Determinism-based lockstep that refuses to stall: **predict** the remote input ("same as last frame"), advance immediately, and on misprediction **roll back** to the last correct frame and **re-simulate forward** without rendering. Held/repeated inputs make predictions usually correct, so rollbacks are usually invisible.

| Property | Value |
|---|---|
| On the wire | Inputs only |
| Bandwidth | Low; trades **CPU for latency-hiding**, not bandwidth |
| CPU | **Higher** — each rollback = state load + N frames resim inside one display frame; needs cheap save/load |
| Determinism | Strict; ships a **sync-test** (resim + checksum compare) to catch non-determinism |
| When | Small-N twitch games with deterministic sims — fighting games are the canonical case |

Key mechanics: **input delay** (fixed frame delay so some remote inputs arrive in time); **prediction window** (~1–9 frames @60fps, capped); **confirmed frame** (all inputs known → never rolled back); **integration contract** (game implements save/load/advance-without-render); **spiral of death** (a 60fps game supporting 300ms may resim ~15 frames in 16.66ms, ~1.1ms budget left — exceed it and the sim falls behind irrecoverably).

**`ggrs` (Rust) — current status, July 2026:**
- **v0.13.0** (~June 2026), 100% safe Rust, a reimagining of GGPO.
- **Request-based API** (not callbacks): each frame the session returns `GgrsRequest`s (`SaveGameState`, `LoadGameState`, `AdvanceFrame`) the caller fulfills.
- Session types `P2PSession` / `SpectatorSession` / `SyncTestSession` via `SessionBuilder`; a `Config` trait parameterizes `Input`, `State`, `Address`, `InputPredictor` (`PredictRepeatLast`, `PredictDefault`).
- Defaults: **max prediction = 8 frames** (0 = pure lockstep), **input delay = 0** (2–4 typical), sparse saving off, num_players = 2.
- **matchbox**: P2P WebRTC for Rust WASM + native, GGRS-compatible via feature flag — your browser transport. **backroll**: separate async Rust rollback lib. **bevy_ggrs**: v0.21–0.22.

→ **Applies to this project.** This is your current combat layer. The Rust sim core must be a pure deterministic `(state, inputs) → state` with cheap `save`/`load`. Godot collects local input, hands it to the Rust session, services the `GgrsRequest` list each frame; matchbox over WebRTC is the web transport (works native too). **Determinism is the whole ballgame**: no float drift, no wall-clock reads in the sim, no HashMap iteration order, seeded RNG only. Wire up `SyncTestSession` in CI from day one — cheapest desync insurance you'll buy.

### 1.3 Client-side prediction + server reconciliation (authoritative server; Quake/Source)

Server is authoritative; the client applies its own inputs immediately and predicts, then reconciles on the server's authoritative update.

- **Wire:** client→server inputs, each tagged with a **sequence number**; server→client authoritative state + last-processed sequence number.
- **Reconciliation replay:** on an authoritative update, discard inputs with seq ≤ acked, then **replay** still-pending inputs on the authoritative state to recompute "now."
- **Determinism requirement: weak** — only the server's sim matters; the client's prediction may diverge and gets corrected. Tolerates float math and heterogeneous clients (opposite of lockstep/rollback).
- **Bandwidth:** inputs up tiny; state down dominates, scales with visible entity count (mitigated by delta compression, §1.4).
- **Failure:** misprediction → visible correction / rubber-banding.

→ **Applies to this project.** The model for the **persistent world** layer. A player mining a tile predicts locally; the VPS server validates and corrects. Payoff: this layer does **not** need cross-machine determinism, so the world sim can use ordinary float physics and non-deterministic libraries — the server is the single source of truth.

### 1.4 Snapshot / entity interpolation

The server sends full/delta **snapshots** at a fixed tick rate. For entities the client does *not* control, it renders **in the past**: buffers snapshots and interpolates between the two most recent *received* ones.

- **Interp delay buffer:** Source defaults to **100 ms** (`cl_interp 0.1`), sized so a single dropped snapshot still leaves two to interpolate between. Default client snapshot rate ≈ 20/s.
- **Delta compression:** send only changes since the last snapshot the client **acknowledged** (baseline + delta).
- **Interp vs extrapolation:** interpolation is between two known past states (accurate, adds delay); if ≥2 consecutive snapshots drop, fall back to linear **extrapolation** of last velocity, bounded ~0.25 s. Gaffer upgrades naive lerp to **Hermite** (position) + **slerp** (orientation).
- **Cost:** bandwidth = snapshot rate × delta size × entity count; CPU cheap (a lerp/entity).

→ **Applies to this project.** In the shared world: local player predicted (§1.3), everyone else interpolated from buffered server snapshots. 100 ms interp delay is the comfortable default for a cozy non-twitch world. Per-layer decision: fighter = zero-interp rollback; world = buffered interpolation.

### 1.5 Lag compensation (server-side hitbox rewind)

Since the client sees the world ~(latency + interp delay) in the past, the server **rewinds** other players' positions/hitboxes to where they were when the shooter saw them, tests the hit, restores the present.

- **History:** Source keeps ~1 s of positions; `Command Execution Time = Current Server Time − Packet Latency − Client View Interpolation`. Rewinds both origins and hitboxes.
- **Failure:** "shot around a corner" / "died behind cover" — the inherent cost of **favor-the-shooter**.
- No extra wire cost, no determinism requirement; always paired with §1.3 + §1.4.

→ **Applies to this project.** Only relevant if the world has server-adjudicated hit detection (PvP, projectiles). For cozy PvE it may be unnecessary. If added, it lives in the Rust world-server and reuses the position history you already keep for interpolation.

---

## 2. Concrete numbers to anchor design

| Game | Tick / update rate |
|---|---|
| CS:GO | 64 Hz (ESEA/FACEIT 128) |
| CS2 | 64 tick + **sub-tick** input timestamping |
| Valorant | 128 Hz every match |
| Overwatch | server ~63 Hz; client 21/s default, 60/s high-bandwidth |
| Source (TF2/CSS) | 66.67 tick; L4D 30 |

Anchors: tick-rate cost is **linear** (64→128 ≈ 2× CPU + bandwidth); interp delay Source default 100 ms, extrapolation cap 250 ms; rollback window GGPO ~1–9 frames, ggrs default max prediction 8; lag-comp history ~1 s.

→ **Applies to this project.** On one VPS, tick rate is your primary cost lever. A cozy world doesn't need 64 Hz; **10–20 Hz world tick + 100 ms client interpolation** is generous and cheap. Reserve high-frequency sim for the rollback fighter, which is P2P (matchbox) and costs the VPS only signaling.

---

## 3. Authority models

### 3.1 P2P host authority (listen server)

One peer runs the authoritative game while also playing (Halo: Reach = 1 of 16 authoritative). **Pros:** no server cost, scales with population, no central failure. **Cons:** host advantage (zero-latency host), host can cheat (trusted party), host-leaving kills the game without migration, DDoS via exposed peer IPs, unusable for economies.

### 3.2 Dedicated-server authority

Neutral server runs the authoritative sim; clients are "privileged spectators." Rule: "if players can gain advantage by lying, it runs on the server." **Anti-cheat payoff:** server validates every action, killing speed hacks/teleport/dupe/hit-injection at the architecture level. **Limits:** real infra cost, single point of failure, doesn't stop info cheats (wallhack/aimbot).

### 3.3 Host migration (the Halo case)

When the host degrades/leaves, the session migrates to a better peer promoted to authoritative. Aldridge: "powerful when it works" but "a significant engineering undertaking… Dedicated servers sidestep the problem entirely." Halo's model beneath: host-adjudicated, clients predict then ask permission (grenade = local predict → host request → confirm ~1 RTT), over three replication channels (§4).

### 3.4 Cheating implications by model

| Model | Trust boundary | Prevents | Enables |
|---|---|---|---|
| Client authority | client-asserted state | nothing | full cheating |
| P2P host authority | the host peer | non-host clients' fabricated state | host cheating; DDoS via IPs |
| Dedicated server | nothing from clients | speed/teleport/dupe/hit-injection | wallhack/aimbot (info leaks) |
| Lockstep P2P (RTS) | all peers equal | cheap many-unit sync | **maphacks** (client-side fog) + **look-ahead cheating** |
| Rollback P2P (fighting) | peers trusted | input-reading (inputs predicted) | connection/input-delay manipulation |

Lockstep's structural leak: every client simulates everything, so it must *know* everything → maphacks trivial. **Look-ahead cheating**: a client delays committing until it's seen others' inputs; commit-hash-then-reveal is a partial defense.

→ **Applies to this project.** Two trust postures:
- **Fighter (P2P rollback):** peers exchange inputs directly, no arbiter. Cheating surface = connection/input-delay abuse, not state fabrication — fine for a friends fighter. VPS pays only signaling.
- **Durable world (dedicated server on your VPS):** must be server-authoritative for anything persistent (inventories, terrain, economy). Terraria rule: "world data should only ever be changed on the server," clients never talk directly. No kernel anti-cheat needed for a cozy hangout, but never let a client assert world state.

---

## 4. The canonical literature

**Fiedler — Gaffer On Games, Networked Physics.** *Deterministic Lockstep*: send only inputs so bandwidth is independent of world complexity, requiring bit-exact determinism; use UDP re-sending all unacked inputs each packet (~90 bytes worst case), smooth even at 2 s latency / 25% loss. *Snapshot Interpolation*: server sends periodic full-state snapshots, clients run no physics and interpolate the two latest (Hermite + slerp). **Idea:** inputs are tiny — resend redundantly over UDP and trade bandwidth for latency immunity; or buffer state and interpolate.

**Bernier / Valve — Latency Compensating Methods (GDC 2001) + Source Multiplayer Networking.** Founding text of authoritative-server FPS netcode. Three compensations: entity interpolation (render others in the past), client prediction (run your movement immediately from last-acked state, replay unacked commands), lag compensation (rewind other players to where the shooter saw them, ray-cast, restore). **Idea:** rewind the world on the server to the instant the shooter saw it, and compute client interp delay + server rewind from the *same* latency budget.

**Aldridge — I Shot You First: Networking Halo: Reach (GDC 2011).** Peer-hosted 16-player model over three replication channels: **state** (eventual consistency of final values — positions/health/timers), **events** (fully droppable, explain *why* state changed), compressed **control/input** (~20 bits/player, sent often). A per-client priority function scores every object by distance/on-screen/threat and fills packets in priority order. The "state buckets" phrase is his object-index bit-packing trick — a cautionary tale, not the headline; the headline is the state/event split. **Idea:** separate authoritative eventually-consistent state from droppable cosmetic events, and let a per-client priority function decide what fits each packet.

**Ford — Overwatch Gameplay Architecture and Netcode (GDC 2017).** Built on an ECS; of ~46 client systems and ~103 component types, only **three** touch networking. 16 ms command frames (~60 Hz); clients predict, the server does **rollback-and-replay reconciliation** (client runs ahead buffering commands; server ground-truth triggers rollback + resim on mismatch), plus favor-the-shooter lag comp and adaptive input buffer. **Idea:** confine mispredictable/networked behavior to a tiny set of ECS systems, then run high-tick prediction + server rollback only over those.

**Bettner & Terrano — 1500 Archers on a 28.8 (GDC 2001).** Per-unit state would cap AoE at ~250 units on a 28.8 modem, so they run an identical deterministic sim everywhere and transmit only *commands*, scheduled two turns ahead (turns decoupled from render, turn length adapting to latency + frame time). Absolute determinism mandatory; divergence detected via per-turn checksums. **Idea:** P2P deterministic lockstep — sync the command stream, not the world — makes bandwidth independent of unit count.

**Frohnmayer & Gift — The TRIBES Engine Networking Model (1998).** 128 players over modems by classifying all traffic by delivery requirement across three layers: a Connection layer with a packet **notification** protocol (sender learns what arrived and reacts), a Stream layer of managers (Move, Event, Ghost, Datablock, String), a Simulation layer doing scoping + prediction. The **Ghost manager** mirrors only in-scope objects, ordered by status change then a sim-assigned **priority**, packing a fixed-priority size-bounded packet. **Idea:** classify every datum by delivery requirement and pack a fixed-priority, size-bounded packet over a notification protocol — the ancestor of Unreal replication.

**Gambetta — Fast-Paced Multiplayer.** Four articles + live JS demos: client-server architecture, prediction & reconciliation, entity interpolation, lag compensation. The engine-agnostic, visual walkthrough of the Valve model. **Idea:** the canonical four-technique stack (authority + prediction + reconciliation + interpolation) as one coherent mental model — the standard onboarding reference.

**Infil — Fight the Lag (2019).** The definitive layperson explainer of fighting-game netcode. Delay-based (buffer local input a few frames — constant added lag scaling with ping, stutters on jitter) vs rollback (never wait: run local input immediately, predict opponent as "same as last frame," roll back + resim on wrong prediction). **Idea:** never stall on a missing remote input — predict, keep simulating, silently roll back + resim when truth arrives.

---

## 5. Why input-sync is cheaper than state-sync

Deterministic lockstep networks a sim "by sending only the inputs," so "bandwidth is proportional to the size of the input, not the number of objects… you can network one million objects with the same bandwidth as one." State-sync sends both input and state, so cost scales with entity count (Gaffer's demo: 901 cubes but a packet carries max 64 state updates, forcing prioritization). RTS proof: passing coordinates would cap AoE at 250 units; passing only commands synced ~1,500 for 8 players over dial-up. "You'll never see an MMO networked [via lockstep]."

**What you pay for that cheapness:** hard determinism (float behavior varies across compilers/OS/ISA — "almost impossible to guarantee"; AoE seeded/synced RNGs, matched the *number* of random() calls, checksummed); full input history + jitter buffer; for rollback the save/load/resim CPU with spiral-of-death risk; slowest-peer coupling in pure lockstep (why fighters moved to rollback).

---

## 6. When you are forced into state-sync anyway

Input-sync's precondition is determinism + full-world resimulation. When either breaks:
- **Non-deterministic physics** — "most physics simulations are not deterministic"; rollback/resim becomes impractical.
- **Can't be made deterministic** — "open world games with streaming levels usually are not possible to make deterministic."
- **Worlds too large to resimulate / arbitrary join-leave** — state-sync hands a late joiner current state, not a replay of all history.

A persistent, mutable-terrain world hits all three. **Terraria** is the concrete pattern: "world data should only ever be changed on the server"; "clients CANNOT send or receive things DIRECTLY from other clients — the server acts as middleman."

---

## 7. Area-of-interest / interest management

Replicate only the slice near each client. Zone-based, aura-based, visibility filtering.
- **Terraria** — tiles are server-owned, sent to a client only when it first visits that world section; `NetMessage.SendData()` keyed by `MessageID`; tile updates per-rectangle; client edits relayed by the server; liquid changes pushed only to clients with that section loaded. Delta-style, server-authoritative, section-scoped.
- **Minecraft** — `ServerWorld` authoritative, split into `ServerChunkManager`, `ChunkLoadTicketManager` (priority loading around players), `ChunkSyncManager` (tracks which chunks each client has). Chunks send on entering view distance; `EntityTracker` sends position updates only past a movement/rotation threshold; `simulation-distance` / `entity-broadcast-range-percentage` cap ticking + broadcast.

Underneath: a **priority accumulator** — add each object's priority each frame, sort largest-first, send top-N against a delta baseline through a jitter buffer; interest management is the spatial pre-filter deciding which objects enter the pool. Same idea as TRIBES' Ghost manager and Halo's per-client priority function.

→ **Applies to this project.** On one VPS, interest management is what keeps a single box viable: replicate only tile sections + entities near each player, send tile edits per-rectangle Terraria-style, gate entity updates by distance + movement threshold. The "each player's room is their personalized home" design maps cleanly onto section/chunk scoping — a home is one interest region, cheap to keep resident and to sync only to visitors.

---

## 8. The hybrid: layer, don't blend

"The two network models are fundamentally different. Pick one." Shipped "hybrids" resolve this by **layering** — a deterministic/predicted combat session sits inside a separately state-replicated persistent world. **Photon Quantum** runs deterministic rollback for combat but is explicitly *not* the persistence layer ("a backend service handles everything that persists beyond the session"). **Overwatch** layers client prediction + server rollback on a server-authoritative ECS.

→ **Applies to this project — the target architecture.**

| Layer | Model | Authority | Transport | Determinism | Tick |
|---|---|---|---|---|---|
| **Fighter combat** | Rollback (ggrs) | P2P peers | matchbox WebRTC | **Required** (bit-exact Rust core) | 60 Hz |
| **Persistent world** | Prediction + reconciliation + snapshot interpolation | Dedicated server on your VPS | WebRTC/WebSocket to server | **Not required** (server is truth) | 10–20 Hz |

Keep the two Rust sim cores separate. The fighter core is a pure `(state, inputs) → state` with cheap save/load and zero non-determinism, driven by ggrs `GgrsRequest`s. The world core is an authoritative server owning the save file, replicating interest-scoped tile sections + entities, correcting predicted clients — free to use ordinary float physics because nothing resimulates across machines. A match is a deterministic P2P session spun up *inside* a spot in the state-replicated world; the world server records only the outcome. The VPS never simulates fights — it signals them and persists results.

---

## Sources

**Families / general**
- [Gaffer — Deterministic Lockstep](https://gafferongames.com/post/deterministic_lockstep/) · [Snapshot Interpolation](https://gafferongames.com/post/snapshot_interpolation/) · [State Synchronization](https://gafferongames.com/post/state_synchronization/) · [Networked Physics index](https://gafferongames.com/categories/networked-physics/)
- [SnapNet — Lockstep](https://www.snapnet.dev/blog/netcode-architectures-part-1-lockstep/) · [Rollback](https://www.snapnet.dev/blog/netcode-architectures-part-2-rollback/)
- [mas-bandwidth — Choosing the right network model](https://mas-bandwidth.com/choosing-the-right-network-model-for-your-multiplayer-game/) · [yal.cc — Preparing for deterministic netcode](https://yal.cc/preparing-your-game-for-deterministic-netcode/)

**Rollback / GGPO / ggrs**
- [GGPO — Wikipedia](https://en.wikipedia.org/wiki/GGPO) · [GGPO README](https://github.com/pond3r/ggpo/blob/master/doc/README.md) · [outof.pizza — Rollback](https://outof.pizza/posts/rollback/)
- [ggrs — GitHub](https://github.com/gschup/ggrs) · [docs.rs](https://docs.rs/ggrs/latest/ggrs/) · [SessionBuilder](https://docs.rs/ggrs/latest/ggrs/struct.SessionBuilder.html) · [lib.rs](https://lib.rs/crates/ggrs) · [matchbox releases](https://github.com/johanhelsing/matchbox/releases) · [bevy_ggrs](https://docs.rs/crate/bevy_ggrs/latest)
- [Infil — Fight the Lag](https://words.infil.net/w02-netcode.html) · [antsstyle — why rollback is overrated](https://antsstyle.medium.com/netcode-in-games-an-explanation-and-why-rollback-is-overrated-b76ee54ac2bb)

**Valve / prediction / lag comp**
- [Source Multiplayer Networking](https://developer.valvesoftware.com/wiki/Source_Multiplayer_Networking) · [Interpolation](https://developer.valvesoftware.com/wiki/Interpolation) · [Lag Compensation](https://developer.valvesoftware.com/wiki/Lag_Compensation) · [Latency Compensating Methods](https://developer.valvesoftware.com/wiki/Latency_Compensating_Methods_in_Client/Server_In-game_Protocol_Design_and_Optimization) · [Bernier GDC 2001 PDF](http://web.cs.wpi.edu/~claypool/courses/4513-B03/papers/games/bernier.pdf) · [settings gist](https://gist.github.com/CoolOppo/fe0586836de3fb2f90f9)
- [Gambetta — Fast-Paced Multiplayer](https://gabrielgambetta.com/client-server-game-architecture.html) · [Prediction & Reconciliation](https://www.gabrielgambetta.com/client-side-prediction-server-reconciliation.html) · [CS2 lag comp update](https://www.talkesport.com/news/cs2/valve-improves-lag-compensation-in-latest-cs2-update/)

**Authority / Halo / Overwatch / literature**
- [Aldridge — I Shot You First (GDC Vault)](https://www.gdcvault.com/play/1014345/I-Shot-You-First-Networking) · [video](https://www.youtube.com/watch?v=h47zZrqjgLc) · [edgegap — Halo: Reach deep dive](https://edgegap.com/blog/game-backend-deep-dive-halo-reach-netcode-host-migration) · [Wolfire GDC summary](https://www.wolfire.com/blog/2011/03/GDC-Session-Summary-Halo-networking)
- [Ford — Overwatch (GDC Vault)](https://www.gdcvault.com/play/1024001/-Overwatch-Gameplay-Architecture-and) · [edgegap — Overwatch deep dive](https://edgegap.com/blog/game-backend-deep-dive-overwatch-2016-netcode-architecture-rollback) · [Tim Ford detail](https://us.forums.blizzard.com/en/overwatch/t/tim-ford-2019-intimately-details-ow-enginenetcode/612832)
- [1500 Archers](https://www.gamedeveloper.com/programming/1500-archers-on-a-28-8-network-programming-in-age-of-empires-and-beyond) · [paper PDF](https://zoo.cs.yale.edu/classes/cs538/readings/papers/terrano_1500arch.pdf) · [TRIBES Networking Model PDF](https://www.gamedevs.org/uploads/tribes-networking-model.pdf)
- [edgegap — authoritative vs relay vs P2P](https://edgegap.com/blog/explainer-series-authoritative-servers-relays-peer-to-peer-understanding-networking-types-and-their-benefits-for-each-game-types) · [Crux — server-authoritative anti-cheat](https://crux.supercraft.host/blog/server-authoritative-anti-cheat-backend/) · [AccelByte](https://accelbyte.io/blog/server-authoritative-logic-to-prevent-cheating)
- [Lockstep protocol — Wikipedia](https://en.wikipedia.org/wiki/Lockstep_protocol) · [treeform — Don't use Lockstep in RTS](https://medium.com/@treeform/dont-use-lockstep-in-rts-games-b40f3dd6fddb)

**State-sync worlds / interest management / hybrids**
- [tModLoader — Basic Netcode](https://github.com/tModLoader/tModLoader/wiki/Basic-Netcode) · [NetMessage docs](https://github.com/tModLoader/tModLoader/wiki/NetMessage-Class-Documentation)
- [Minecraft Wiki — Chunk](https://minecraft.wiki/w/Chunk) · [Simulation distance](https://minecraft.fandom.com/wiki/Simulation_distance) · [DeepWiki — Server World Management](https://deepwiki.com/g122622/minecraft-reborn/4.3-server-world-management)
- [Anvari & Rio (UCL) — Spatial Interest Management PDF](https://www.ee.ucl.ac.uk/lcs/previous/LCS2011/LCS1121.pdf) · [Photon Quantum](https://www.photonengine.com/quantum) · [Crux — Photon vs backend](https://crux.supercraft.host/blog/photon-real-time-networking-vs-backend-services/)

**Tick-rate figures**
- [win.gg — 64 vs 128 tick](https://win.gg/news/explaining-tick-rates-in-fps-games-difference-between-64-and-128-tick/) · [CS2 tick rate](https://tradeit.gg/blog/cs2-tick-rate/) · [Valorant 128 tick](https://upcomer.com/what-do-128-tick-servers-mean-for-valorant-competitive-play/) · [Overwatch 60 Hz](https://gamesbeat.com/overwatch-netcode-tick-rate-60hz/) · [edgegap — tick rate cost](https://edgegap.com/blog/game-server-tick-rate-explained-gameplay-precision-vs-infrastructure-cost)

*Accuracy caveats: some Halo: Reach bandwidth figures come from GDC-talk snippets, not a primary transcript (cross-check the video); Minecraft `EntityTracker` thresholds come from a docs summary (confirm against the live protocol page); the two Valve wiki pages 403 automated fetchers but the URLs are canonical (Bernier read from the WPI PDF mirror).*
