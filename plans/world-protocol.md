# World protocol: content-addressed seed, event log, automated storage

The spine that ties `plans/reducer-trait.md` (the transition layer) to durable storage without
hand-writing persistence per feature. Model:

```
Seed  = the frozen init content of a world. Content-addressed: WorldId = hash(canon(Seed)).
Sync  = handshake compares (WorldId, BuildVersion). Equal -> both already hold it, nothing transfers.
Log   = append-only [WorldEvent]. state = fold(apply, Seed, events). Only mutable thing.
Store = a trait over sqlx/Postgres CRUD. Generic: define Reduce + Serialize + SCHEMA, get persist+replay.
```

"Here is my world, I want to host it" = publish a Seed (get its `WorldId`), start a lobby on it.
"Join" = send `(WorldId, BuildVersion)`; host accepts iff both match; then fold from the shared Seed.
That is the entire init-sync. No world transfer on the happy path.

Hard requirement driving everything below: **no backward-breaking changes to persisted/wired types.**
So the type foundation is nailed first (§1), the non-breaking discipline is a CI rail (§2), and only
then does storage (§3-4) touch bytes that live forever.

---

## §0 Deps (reuse what's here, add two)

| need | crate | note |
|---|---|---|
| serialize | `serde` | already in core + net |
| canonical bytes | `bincode 1.3` | already in net (wire + replay). Positional, deterministic for struct/enum trees. Fix one config, use it for hashing. |
| content hash | `blake3` (add) | 32-byte, fast, no per-file config. `WorldId([u8;32])`. |
| durable store | **SQLite only** (`sqlx-sqlite` or `rusqlite`) | one backend everywhere — client and the always-on peer. Min db. No Postgres. |

No new format. bincode is the canonical encoder **and** the wire encoder. One caveat: bincode is
positional (§2) — that constraint is the whole reason §2 exists.

**SQLite everywhere, not `ConfigFile`, not Postgres.** One `WorldStore` trait, one backend. Every
peer — clients and the always-on VPS peer — holds its own SQLite of the worlds it carries; there is
no central system-of-record (§P2P below). Local worlds get snapshot/compact/replay offline (the home
room). Web-export reality: `rusqlite`/`sqlx-sqlite` bundle **C SQLite through emscripten** — C-via-
emscripten does **not** hit the wasm-bindgen wall (rust-godot-stack §2; the wall is Rust
`wasm32-unknown-unknown` crates). In-core path; needs a build spike (emscripten link + a wasm async
runtime, or run SQLite synchronously off the reducer thread).

---

## §1 Types first (strong, sized, serializable)

All in one crate `smash-world-types` (or a module in core), every type `Serialize + Deserialize`,
newtyped, fixed-size where it can be. No `String` keys, no paths, no ambient anything.

```rust
// ---- identities: fixed-size, self-verifying ----
#[derive(Copy, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WorldId([u8; 32]);          // = blake3(BuildVersion || canon(Seed))
#[derive(Copy, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AssetId([u8; 32]);          // background/gif/etc, fetched by hash over HTTP, never a path

#[derive(Copy, Clone, PartialEq, Eq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct BuildVersion(pub u32);      // a version tag. NOT one number: two distinct uses ->
pub const READS: BuildVersion = BuildVersion(env!("...").parse()); // binary's read-capability (git client version)
// the *world's* version is data, not a const: it's fold(log).head — see §2 (migrations are events).

#[derive(Copy, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EventId([u8; 32]);          // = blake3(parent ‖ schema ‖ payload). THE key. No central allocator (P2P).
#[derive(Copy, Clone, PartialEq, Eq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct Seq(pub u64);               // derived chain height (topo order), NOT identity. For cursors/UI.
#[derive(Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schema(pub u16);            // event-encoding version, per event kind

// ---- the frozen seed (content-addressed; a given WorldId's Seed never changes) ----
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Seed {
    pub build: BuildVersion,           // hashed in, so a schema bump = a new WorldId namespace
    pub rules: smash_core::Tune,       // the round config (World == Tune reference, per earlier design)
    pub stage: StageId,                // base stage selector
    pub bg: Option<AssetId>,           // content-addressed background, not a URL/path
    pub authored: Vec<WorldEvent>,     // "the defaults are the initial events" — author-placed geometry
}

// ---- the only mutable thing: an append-only event ----
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]                       // append-only; see §2
pub enum WorldEvent {
    PlacePlatform { at: Vec2, len: f32, class: SegClass, owner: Owner },
    ErasePlatform { id: StrokeId },
    SetRule { key: RuleKey, val: RuleVal },
    Upgrade { to: BuildVersion, mig: MigrationId },   // a migration IS an event (§2). fold's HEAD = world version.
    // append new variants at the END, never reorder, never remove.
}

// ---- a stored event: hash-chained (git object). id = content addr, parent = prev link ----
#[derive(Clone, Serialize, Deserialize)]
pub struct Node { pub id: EventId, pub parent: Option<EventId>, pub seq: Seq, pub ev: WorldEvent }
```

`StageId`, `Owner`, `RuleKey/RuleVal`, `MigrationId` are newtypes too — no bare `usize`/`String` on
the wire. `Vec2`/`f32` for geometry, matching the existing `f32` sim checksum (decision in §7; the
float-determinism guard rails live there).

### `WorldId` derivation (the whole sync)
```rust
pub fn world_id(seed: &Seed) -> WorldId {
    // let mut h = blake3::Hasher::new();
    // h.update(&seed.build.0.to_le_bytes());        // build first: schema bump changes the id
    // bincode::serialize_into(HashWriter(&mut h), seed).unwrap(); // canonical, deterministic
    // WorldId(h.finalize().into())
}
```
Content-addressed => idempotent publish, self-verifying fetch: a joiner who fetched the seed blob
recomputes `world_id` and refuses if it doesn't match the id it asked for.

---

## §2 Non-breaking discipline (the CI rail, not vibes)

bincode is positional: **reordering or removing an enum variant / struct field silently reinterprets
old bytes.** So the rules are mechanical and enforced, not remembered:

1. **Append-only.** New enum variants go last; new struct fields go last. Never reorder, never delete.
   `#[non_exhaustive]` on wired enums so downstream `match` must have a `_ =>` (forward-tolerant).
2. **Two axes, don't conflate them (the git-HEAD framing):**
   - *decode* (bytes -> typed event): `StoredEvent { schema: Schema, payload: Vec<u8> }`, dispatch on
     `schema`. This is "can my binary parse this row" = git client format support. A breaking event
     encoding = a new `WorldEventV2` + `Schema(2)` + a decoder; old rows still parse via their own
     schema forever.
   - *migrate* (state transform): **a migration is an event, not an out-of-band on-read pass.**
     `WorldEvent::Upgrade { to, mig }` sits in the log like any input; its reducer arm applies the
     known transform. The world's current version = `fold(log)`'s latest `Upgrade` = **HEAD**, exactly
     like git HEAD is the fold of commits. No separate upcast registry keyed on a binary const; the
     upgrade history is *in* the log, self-describing and replayable (event-sourcing.md §versioning).
   So: `READS` (binary) is a *capability*; the world's version is *data*. They meet only at handshake.
3. **Golden-bytes test.** Check in a fixture: a `Seed` and a `[WorldEvent]` and their exact bincode
   bytes. A test asserts `serialize(fixture) == golden` and `deserialize(golden) == fixture`. Any
   accidental reorder fails CI. This is the "nail the foundation" guarantee made testable.
4. **`dl` rail (optional).** A `.dl/*.dl` rule fencing `WorldEvent`/`Seed` edits: warn on any diff
   that isn't a pure append (reuses the confinement-lint recipe pattern already in memory).

`Seed` needs no upcaster: a `WorldId` pins one `Seed` forever (it is the genesis commit). All
evolution — including schema migrations — lives in the event stream as `Upgrade` events. The Seed's
`build` is only the *genesis* version; HEAD moves past it via the log.

---

## §3 The store trait (automate CRUD; don't write the universe)

One trait, ~6 methods, all thin `sqlx::query!`. Reducer stays pure; this is the only IO surface.
Relay-side (the VPS process holds the single `PgPool`), never the Godot client.

```rust
#[async_trait]
pub trait WorldStore {
    // publish a seed; idempotent on its hash. INSERT OR IGNORE; return the id either way.
    async fn publish(&self, seed: &Seed) -> Result<WorldId>;
    async fn seed(&self, id: WorldId) -> Result<Option<Seed>>;

    // append onto the current head: id = blake3(head ‖ schema ‖ payload). INSERT OR IGNORE on id
    // (content-addressed => append is idempotent, no race window). Only the session host appends (v1).
    async fn append(&self, id: WorldId, ev: &WorldEvent) -> Result<EventId>;

    // --- P2P sync = git have/want, scoped to a world both peers hold ---
    async fn head(&self, id: WorldId) -> Result<Option<EventId>>;        // my HEAD for this world
    async fn has(&self, id: WorldId, ev: EventId) -> Result<bool>;       // ancestor probe
    async fn since(&self, id: WorldId, from: Option<EventId>) -> Result<Vec<Node>>; // chain after a common ancestor
    async fn ingest(&self, id: WorldId, nodes: &[Node]) -> Result<()>;   // accept peer's nodes; verify each id == blake3(..)

    // snapshots deferred until fold time is measurable (event-sourcing.md).
    // async fn snapshot(&self, id: WorldId, upto: Seq, blob: &[u8]) -> Result<()>;
}
```

### Automation: fold the store into the `Reduce` layer
`plans/reducer-trait.md` gives `Reduce { type Event; type Effect; reduce(&mut self, ev, out) }`.
Persistence rides that for free when the event is serializable:

```rust
pub trait Journaled: Reduce
where Self::Event: Serialize + DeserializeOwned {
    const SCHEMA: Schema;
}

// generic runner: dispatch -> append event -> return effects. Written once, works for every world FSM.
pub async fn commit<R: Journaled, S: WorldStore>(
    store: &S, id: WorldId, st: &mut R, ev: R::Event,   // R::Event == WorldEvent here
) -> Result<(EventId, Vec<R::Effect>)> {
    // let mut out = Vec::new();
    // st.reduce(ev.clone(), &mut out);            // pure transition (rollback-safe)
    // let eid = store.append(id, &ev).await?;     // content-addressed onto head; idempotent
    // Ok((eid, out))                              // shell runs the effects
}
```
That is the "I don't want to write the universe" payoff: implement `Reduce + Serialize + SCHEMA`
for a world FSM and persistence + replay + catch-up are generic. The store never interprets an
`Effect` (same boundary as reducer-trait.md §boundaries) and the reducer never touches sqlx.

---

## §4 Storage layout, then reads/writes, then uniqueness (Postgres)

```sql
-- content-addressed seed blob. id IS the hash -> publish is idempotent. (SQLite; BLOB not BYTEA.)
CREATE TABLE world (
  id    BLOB PRIMARY KEY,            -- WorldId, 32 bytes
  build INTEGER NOT NULL,           -- BuildVersion (also hashed into id; stored for query)
  seed  BLOB NOT NULL               -- bincode(Seed)
);

-- hash-chained event log (git objects). id = content addr; parent = prev link. Append-only.
CREATE TABLE world_event (
  id       BLOB PRIMARY KEY,         -- EventId = blake3(parent ‖ schema ‖ payload). identity, not seq.
  world_id BLOB    NOT NULL REFERENCES world(id),
  parent   BLOB,                     -- prev EventId; NULL = first after seed (the chain link)
  seq      INTEGER NOT NULL,         -- derived chain height, for ORDER BY / cursors only
  schema   INTEGER NOT NULL,         -- for upcast dispatch
  payload  BLOB    NOT NULL          -- bincode(WorldEvent) at that schema
);
CREATE INDEX world_event_by_world ON world_event(world_id, seq);

-- one row per world: the current chain tip. Advanced on append/ingest.
CREATE TABLE world_head (world_id BLOB PRIMARY KEY REFERENCES world(id), head BLOB NOT NULL);

-- materialized fold cache: state@head. Deletable + rebuildable -> NOT truth (§4.5).
CREATE TABLE world_snapshot (
  world_id BLOB NOT NULL REFERENCES world(id),
  upto     BLOB NOT NULL,            -- EventId this snapshot folds through
  hash     BLOB NOT NULL,            -- blake3(blob), self-verifying fetch
  blob     BLOB NOT NULL,            -- bincode(folded state)
  PRIMARY KEY (world_id, upto)
);
```

Read/write sequences:
- **publish(seed):** `INSERT OR IGNORE INTO world ..`; return `world_id(seed)`. Idempotent; hash is
  client-side.
- **append(id, ev):** read `world_head.head`, compute `EventId = blake3(head ‖ schema ‖ bincode(ev))`,
  `INSERT OR IGNORE INTO world_event`, `UPDATE world_head SET head = new`. Content-addressed => no
  race window: the same append computed twice yields the same id and the second is a silent no-op.
- **sync (have/want, per shared world):** A sends `head(id)`. B checks `has`; if unknown, B walks its
  own chain sending HEAD backward until A reports a common ancestor, then A sends `since(ancestor)`;
  B `ingest`s (verifying each `id == blake3(parent ‖ schema ‖ payload)`), fast-forwards its head.
  Git fetch, scoped to `world_id` both hold. **Sync exactly what you share, nothing else.**
- **since(id, from):** walk `parent` links from head back to `from` (or genesis), reverse, decode by
  `schema`, return `[Node]`.

Uniqueness / integrity:
- one `Seed` <-> one `WorldId`; `world.id` PK makes duplicate publish a no-op.
- an event's `id` IS its content hash -> the PK makes dedupe automatic and tamper self-evident: a
  peer sending a mutated payload produces a different id, and `ingest` recomputes and rejects it.
- **one writer per live session (v1):** the lobby host is the sole appender, so the chain stays
  linear (no sibling children of a parent = no fork = no merge). Two independent hosts extending the
  same world offline = a DAG fork; **forbidden in v1** (detected via divergent HEAD at handshake).
  Multi-writer merge (logical-clock linearization / CRDT, non-valuable state only) is deferred
  (state-sync-consensus.md §2.6).

---

## §5 Instance lifetimes

| type | holds state? | lifetime |
|---|---|---|
| `Seed` | no (immutable value) | forever once published; equals its `WorldId` |
| `WorldId`/`AssetId`/`BuildVersion` | no | value types, `Copy` |
| live `World` (folded `Seed`+events) | **yes** | per lobby instance, in RAM; rebuilt by `since()` fold on (re)join; never the source of truth |
| `WorldStore` impl (`SqliteStore{ conn }`) | yes (the SQLite handle) | one per peer (every client + the always-on VPS peer); no central owner |
| `Store<World>` (reducer-trait) | yes | per live lobby; wraps the live `World` + pending effects |
| `StoredEvent` rows | yes (durable) | append-only, forever (until a snapshot compacts a prefix) |

The live `World` is a cache of the log. Truth is `Seed + world_event`, replicated per-peer (no single
owner). That is what makes rejoin, replay, and desync-detection fall out (README synthesis).

---

## §6 Handshake wiring (extend the lobby, don't invent a channel)

`net/src/lobby.rs` already has `Signal` (Matched/Offer/Answer/Ice/Room/Resume/**Tune**/Bye).
Init-sync is one new signal + a typed refusal:

```rust
enum Signal { /* ...existing... */
    Hello  { world: WorldId, reads: BuildVersion, head: Option<EventId> }, // seed id, read-cap, my chain tip
    Refuse { reason: Refuse },
}
enum Refuse { TooOld { world_head: BuildVersion }, WorldMismatch { host: WorldId }, Forked, Full }
```

Flow: joiner sends `Hello`. Host checks, in order:
- **seed match:** `world == host.world` (same genesis). Differs -> `WorldMismatch`; joiner may
  `GET /world/{id}` (bincode Seed blob over the nginx/axum front), verify `world_id(seed) == id`,
  re-`Hello`.
- **capability >= HEAD:** joiner's `reads` >= the world's HEAD version (max `Upgrade.to` folded).
  Too old -> `Refuse::TooOld` = "update to play", like a git client too old for the repo.
- **reconcile heads (git fetch):** if `joiner.head` is an ancestor of host's -> host streams the
  missing chain (`since(joiner.head)`), joiner fast-forwards (rejoin = `git pull`). If host's is an
  ancestor of joiner's -> host `ingest`s from the joiner. If neither is an ancestor of the other ->
  a real fork -> `Refuse::Forked` (v1 forbids concurrent hosts; merge is deferred). Equal -> in sync.

Live world events after the handshake take the **reliable side channel** (like `Signal::Tune` is
applied once, not rolled back) — the recommended fork from README open-questions. Terrain-as-rolled-
back-ggrs-input stays deferred.

---

## §4.5 Lifecycle: reduce · scan · snapshot · migrate · compact (nail this before bytes freeze)

Five operations, three truth-levels. Getting which-is-truth right up front is the whole "don't mess
up state as a habit" discipline; relational modeling is what enforces it (each op = one transaction
with PK/FK invariants).

| op | definition | truth level |
|---|---|---|
| **reduce** | `apply(state, ev) -> state`, one fold step (the `Reduce` arm) | derives state |
| **scan** | fold that yields every intermediate `state@k` (rxjs `scan`); replay/rollback/debug | derives states |
| **snapshot** | materialize `state@N`, cache in `world_snapshot` | **cache** — delete + rebuild anytime, never truth |
| **migrate** (`Upgrade`) | an event whose apply-arm transforms schema v→v+1; **forces a snapshot@its seq** | **truth** — real history, must replay |
| **compact** | promote `snapshot@N` to the new replay floor, GC `world_event` where `seq < N` | **truth-moving** — a `Compact{upto,snap}` event |

Invariants (the messy-avoidance rules):
- **snapshot ≠ event, compaction = event.** A snapshot changes nothing you can't rebuild; a compaction
  changes what is *required* to rebuild, so it is recorded (git shallow-graft / Raft log compaction).
- **`WorldId` is immutable across compaction.** The snapshot is a new *base*, not a new *identity*;
  `WorldId = hash(genesis Seed)` forever. Late joiner: fetch `snapshot@N` (verify `blake3(blob)==hash`)
  then `since(N)`. Re-seeding to a new hash = a *fork*, a deliberate different act.
- **compact only below the low-water mark** = `min(live participants' folded HEAD)`; single-writer
  host = the host's persisted HEAD. Never GC an event someone still needs to catch up through.
- **budget bounds the tail, not the world.** Durable size = `snapshot blob + events since snapshot`.
  Triggers for the *same* compact op: watermark (bytes/count of the tail) ∥ migration-forced ∥ timer.
  Recommended: watermark routine + migration forced. There is no hard per-world cap that rejects writes.

Compaction as a transaction: `INSERT world_snapshot(upto=N) ; DELETE world_event WHERE seq<N ;
append Compact{upto:N, snap:hash}`. Atomic; the snapshot must exist before the delete.

## §P2P: storage is per-peer, sync is git-fetch, the VPS is just always-on

No central system-of-record. Each peer holds its own SQLite of the worlds it carries. The VPS is
**not an authority** — it is one more peer that happens to always be online, so a world survives when
no human holding it is connected (a seed / archival peer). Same SQLite, same `WorldStore`, no
privilege. This is the deliberate revision of the `docs/game-architecture` "world = VPS-authoritative"
decision; keep that doc's authority path in your pocket for the day valuable/economy state needs it.

- **"Sync what you share."** The sync unit is a world both peers hold. have/want by `EventId`
  (git fetch): exchange only the chain the other lacks, reachable from HEAD, scoped to that `world_id`.
  Private worlds never leave disk. No global replication, no gossip of everything.
- **Ordering without a master:** the live lobby host linearizes appends for the session, so the chain
  is single-writer *while live* even though storage is P2P. Host role is per-session, not a server.
- **Trust:** content-addressed events are tamper-evident (`ingest` recomputes every id). That covers
  integrity, not authority — a malicious *host* can still author bad events. Acceptable for a cozy
  shared sandbox (home-room vision); a valuable-state slice would re-introduce a checking authority.
- **Tradeoff named:** dropping VPS authority drops server-side anti-cheat. In scope for friends'
  worlds; out of scope is ranked/economy, which would pin one authority for that slice only.

## §6.5 "Just run git (wasm)?" — no; keep the semantics, not the implementation

The model is git-shaped (Seed=genesis, log=commits, HEAD=fold, rejoin=pull, Upgrade=migration commit,
blake3=object hashing). Running actual git is still the wrong call:
- **Granularity/push:** commits are coarse + pull-based; events are fine-grained + need relay push.
  Per-event commits = pack/gc churn; one `INSERT (world_id, seq)` beats it. You'd rebuild a live
  channel on top anyway.
- **Merge is git's value and we don't use it:** host is single-writer (§4), history is linear, zero
  merges. We want the cheap half (hash-chained append, ~30 lines), not the object db / branches / gc.
- **The WASM wall (again):** `gix`/libgit2 -> `wasm32-unknown-unknown` won't link into Godot's
  emscripten export (rust-godot-stack.md §2). Only isomorphic-git (JS) dodges it, but that strands
  sync in JS outside the Rust core.
- **Auth:** git push is whole-ref; "only host appends valid event kinds" = a custom pre-receive hook
  = the relay rebuilt in git plumbing.

**Channel split (the "git vs git-lfs" instinct, resolved):** the *event log* is a Postgres append
table (fine-grained/realtime/validated/single-writer). *Authored content blobs* (Seeds, gif bg,
authored stages — static, content-addressed, big) are git-lfs-shaped: a by-hash HTTP blob store
(`GET /world/{id}`, `GET /asset/{hash}`) is git-lfs minus git, and covers it. Real git-lfs only if
authored-world history/diffing ever becomes a feature.

## §D.5 The ggrs ↔ world-log bridge (resolves the event-sourced-vs-ggrs fork)

ggrs counts in **frames** (60 Hz = 16.7 ms); TURN only adds RTT, never touches the frame model. Two
bounds: `max_prediction_window` (~8 frames ≈ 133 ms = the rollback horizon) and the **confirmed
frame** (latest frame all inputs are known for — settled, never rolls back again).

**The confirmed frame IS a compaction low-water mark.** Frames in the window are speculative; a frame
"windows out" when it confirms. So the two-tier handoff is mechanical:

- A forge edit lives in **`SimState.paths`** (strokes are already in the frame checksum), so it rolls
  back for free during combat — the ggrs side of the old fork.
- Its **durable `WorldEvent`** is staged, not written, until the producing frame confirms:
  `pending: Vec<(Frame, WorldEvent)>`; each tick drain `f <= session.confirmed_frame()` into
  `commit()` → the world log. **Never persist a fact a rollback could erase.** The window is a free
  debounce (~8 speculative frames collapse to ≤1 settled append per action).

So it is not ggrs *vs* event-sourcing: the same edit is live-in-`SimState` **and** shadowed to the log
at confirmation. The log is a *settled shadow*, not a competing driver; it exists for persistence +
rejoin + peers outside the session. Durable log lags realtime by ≤ window + RTT (~133 ms), the exact
price of durability-only-on-settled.

Consequence: if a `WorldEvent` is a **deterministic function of the confirmed sim frame**, both peers
mint the same `EventId` at the same frame -> `ingest` dedupes -> **no host broadcast for sim-born
events**; the frame number is a free total order. Host broadcast stays only for non-sim events
(`SetRule` etc.). Verify the exact ggrs 0.13 confirmed-frame accessor before relying on it.

**Two "rollbacks" — don't conflate.** ggrs's rollback reverts a *misprediction* (a guess about a late
input): in-place, discards the guess, never durable — a wrong guess is not a fact. A **revert-as-fact**
is the opposite: `git revert` / an accounting reversal — append the inverse *forward*, HEAD moves on,
history kept. So a durable undo is a `WorldEvent`, not a deletion: `ErasePlatform{placed}` and
`Revert{target}` negate a prior event forward; `Reset{to}` records "state is now X" forward, never by
deleting. History is append-only; an undo is itself immutable history. The only backward op is
*deletion* = compaction, gated on the low-water mark (§4.5), never gameplay. The fast tier eats
mispredictions in place; the durable tier only ever sees settled facts + intentional forward reverts.

## §7 Build order

1. **`smash-world-types` crate** — §1 types + `world_id()` + the §2 golden-bytes test + `dl` rail.
   Nothing persists yet; this is the foundation that must not break later. Land first, alone.
2. **`WorldStore` trait + `SqliteStore` locally** (§3-4) — the §4 schema in local SQLite via sqlx;
   build a `Seed` from current `Tune`+stage, publish + fold, render the offline scene as Home. No
   network. Run the emscripten-SQLite build spike here (the one real toolchain risk). Replaces the
   `identity.rs` ConfigFile pattern with the same relational schema the relay uses.
3. **P2P sync over the same `SqliteStore`** (§3-4, §P2P) — the have/want protocol (`head`/`has`/
   `since`/`ingest`) across the WebRtc channel; the VPS runs the identical `SqliteStore` as an
   always-on peer (no separate server DB, no Postgres). Rejoin = fast-forward the chain.
4. **`Journaled` + `commit`** (§3 automation) — once `reducer-trait.md` lands its `Reduce` base.
5. **Handshake** (§6) — `Hello`/`Refuse` into `lobby.rs`, the `GET /world/{id}` blob route.
6. **First real `WorldEvent` producers** — the Forge platform edits (`plans/forge-arena.md` §2) become
   `WorldEvent::PlacePlatform`, now durable + replayable through this spine.

Sibling: plans/reducer-trait.md (the transition layer this persists), plans/forge-arena.md (first
event producer), plans/gif-background-library.md (`AssetId` blobs), docs/game-architecture/
{event-sourcing.md, datastores.md, state-sync-consensus.md, rust-godot-stack.md}.

## Decisions (resolved 2026-07-01)
- **Numerics: `f32`, matching the existing sim.** `SimState` already checksums over `f32` (glam +
  `physics.rs`) and rollback already works across current peers, so float determinism is already
  relied on and proven here; a lone `Fixed` island for world events would diverge from the sim it
  edits. Convention: lockstep RTS uses fixed-point dogmatically (cross-CPU, huge logs); rollback
  fighters use floats in practice because every peer runs the *same binary on the same ISA*. That
  holds **iff** identical build + same arch + only IEEE-754 basic ops (`+ - * / sqrt`) + **no
  transcendentals / fast-math / FMA**. Guard rail: keep the sim off `sin/cos/exp` and fast-math, and
  do not ship a native build that cross-plays a wasm build. **Revisit `Fixed` only if native+wasm
  cross-play becomes a target** (that is the one case where same-source peers can still round apart).
- **Envelope: per-event `Schema(u16)`.** ~2 bytes/event; evolve one event kind without touching the
  rest.
- **`WorldId = blake3(build ‖ canon(Seed))`.** Build folded into the hash => a build bump is a hard
  namespace split; two builds can never share an id or cross-join.
- **SQLite everywhere, one `WorldStore` trait, no Postgres, no `ConfigFile`.** Min db. Every peer
  (clients + the always-on VPS peer) holds its own copy; storage is **P2P** ("sync what you share",
  git have/want by `EventId`). VPS is a peer, not an authority — revises the `docs/game-architecture`
  VPS-authoritative decision; re-add an authority only for a future valuable-state slice. Toolchain
  risk = emscripten SQLite link, spiked in build step 2.
- **Event id = content hash (`blake3(parent ‖ schema ‖ payload)`), chain not seq.** No central id
  allocator (P2P), automatic dedupe, tamper-evident `ingest`. `Seq` is derived height only. One writer
  per live session (host linearizes); concurrent-host forks forbidden in v1.
- **Snapshot = cache (not an event); migration + compaction = events (§4.5).** Migration forces a
  snapshot; compaction moves the replay floor under a stable `WorldId`; budget bounds the tail, not
  the world. Compact only below the low-water mark.
