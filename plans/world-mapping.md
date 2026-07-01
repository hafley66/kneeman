# World types ↔ live code: the stress test (before we prop it up)

Maps `smash_world` (pure, 20 tests green) onto what already ships, and ranks the impedance
mismatches so we lock the pure core, then iterate outward. Read with plans/world-protocol.md.

## Direct maps (these seat cleanly)

| world type | live type | note |
|---|---|---|
| `Seed.rules: Tune` | `Signal::Tune{tune}` `net/lobby.rs:65`; `Lobby.tune` `:139` | host already ships whole `Tune`, applied-once. The world's rules channel EXISTS. |
| `Owner(i8)` | `InkPath.owner: i8` `stage.rs:236` (`-1` = baked) | identical convention. |
| `SegClass` | `smash_core::SegClass` `stage.rs:84` | same type, re-exported. |
| `StrokeId` | `smash_core::StrokeId=u8` `stage.rs:127` | same, but see gap #2. |
| `BuildVersion` | `Lobby.build: String` `:149` ("not acted on") | concept exists; type differs (gap #4). |
| a lobby | "a live instance on a world" | `Room{code,deadline_ms}` `:126` — needs a `WorldId` (gap #5). |
| world fold snapshot | `Signal::Resume{seed: SimState}` `:63` | **different snapshot** — Resume = frame-tier (SimState) for rollback resume; `world_snapshot` = durable-tier fold cache. Don't conflate. |

## Impedance mismatches (ranked; each needs one decision)

### 1. `PlacePlatform{at,len,class}` is a toy — the real fact is an `InkPath` — CENTRAL
`SimState.paths: [InkPath; MAX_DRAWN]` `lib.rs:258`; `InkPath` `stage.rs:228` is a fat fixed polyline
(`pts[MAX_PATH_PTS]`, `born`, per-seg `class`, `len`, `kind`, `props`, `owner`, `drawing`, `budget`).
The durable fact of a forge stroke is the **finalized** `InkPath` (drawing:false), not `{at,len}`.
- Decision: `WorldEvent::PlaceStroke` carries either (a) the settled polyline (self-contained, fat but
  Copy+Serialize already) or (b) a compact spec `{tool, anchor, aim, budget, props}` a pure fn expands
  to the polyline. (a) is byte-heavy but replay-free and cross-build stable; (b) is small but its
  expander must stay deterministic forever. **Lean (a)** — durable facts should be self-contained.
- Bridge already diffs "confirmed geometry"; concretely it fires when a `paths` slot finalizes at a
  confirmed frame -> `PlaceStroke(that InkPath)`, and when a slot leaves -> `ErasePlatform`.

### 2. `StrokeId` vs resolved `StrokeProps` — cross-build durability
`InkPath` stores resolved `props: StrokeProps` `:235`; a `StrokeId` only resolves against a
`StrokeRegistry` whose rows could change between builds. A log keyed on `StrokeId` silently reskins
old strokes if row N moves.
- Decision: durable events store **resolved `StrokeProps`** (self-contained), not a registry index.
  `StrokeId` stays a live-authoring convenience; the settled fact carries the material.

### 3. `MAX_DRAWN = 6` caps LIVE geometry; the durable world is unbounded — ARCHITECTURAL
`SimState.paths` holds 6 ink slots + static `PLATFORMS` const. The world log records every placement
(SQLite, unbounded), but the sim can only hold 6 live strokes. Forge-built permanent geometry has
nowhere to live in the rolled-back state today (forge-arena.md §2 flagged this).
- Decision: the sim needs a **baked-geometry lane** the world fold materializes into — either raise
  the cap + a `built: Vec/array` distinct from the 6 decaying ink slots, or bake the folded world into
  the static stage geometry at session start and only stream *new* built strokes through ggrs. This is
  the biggest prop-up: `SimState` grows a durable-geometry region the fold owns.

### 4. `build: String` (hash, equality-only) vs `BuildVersion(u32)` (ordered) — handshake
`Lobby.build` is a build hash carried on Offer/Answer, not acted on `:148-149`. The `Refuse::TooOld`
capability check needs an **ordered** version, not a hash equality.
- Decision: keep the string hash for exact-build match, add `BuildVersion` for the `>= HEAD` ordering.
  Handshake sends both; hash mismatch = "different build", version-too-low = "update to play".

### 5. `Lobby`/`Room` carry no `WorldId` — identity
"You're in one world at a time" but `Lobby` holds only `tune` + `resume: SimState` `:139-141`; `Room`
is just a reconnect code `:126`. Nothing names the world.
- Decision: `Lobby` gains `world: WorldId` + the world-log sync state; `Room` (or a new `Signal::Hello`)
  carries `WorldId` so join = "join THIS world". `Signal` gains `Hello{world,reads,head}` / `Refuse`.

### 6. `SetRule` addressing into `Tune` — unresolved
`Tune` is ~40 Copy fields + `AttackData`; there is no field-index. `RuleKey(u16)` addresses nothing yet.
- Decision (MVP): **`SetRule` ships a whole `Tune`** — mirrors `Signal::Tune` exactly, dodges the
  addressing problem, one write per settings change (settings changes are rare). Field-level `RuleKey`
  is a later optimization, not a foundation.

## Pure-mode lock: test/feature matrix (before iterating outward)

| capability | status | note |
|---|---|---|
| fold determinism / snapshot==replay / compaction preserves state | ✅ tested | fold.rs |
| erase + revert = forward inverse | ✅ tested | git-revert semantics |
| `Upgrade` moves version | ✅ tested | but no actual state *transform* yet (see below) |
| `ingest` dedupe / idempotent | ✅ tested | assumes **in-order** delivery (gap) |
| bridge gate / mispredict-never-persists / idempotent | ✅ tested | modeled frames (real ggrs = step 1) |
| ids/hashing/roundtrip, sqlite save/load, mem↔sqlite parity, cursor | ✅ tested | store.rs |
| e2e frames→sqlite→fold, rejoin tail | ✅ tested | formula_e2e.rs |
| **golden-bytes fixture** (byte-freeze `WorldEvent`+`Seed`) | ❌ missing | §2 promise; write a checked-in byte vector, not just a determinism test |
| **fork detection** (two divergent heads) | ❌ missing | `Refuse::Forked` path; needs a "is-ancestor" walk + test |
| **out-of-order ingest** (parent-buffering / topological) | ❌ missing | real P2P delivers out of order; current `ingest` assumes order |
| **compaction + `world_snapshot`** (low-water-mark, snapshot table) | ❌ missing | §4.5 transaction; store has no snapshot methods |
| **schema upcast** (`Schema(2)` decode → upcaster) | ❌ missing | `Envelope.schema` exists but no decode-by-schema; `Upgrade` folds version but transforms nothing |
| **`Reset{to: SnapshotId}`** event | ❌ missing | mentioned; not in enum |
| **`PlaceStroke(InkPath)`** real geometry event | ❌ missing | gap #1; current `PlacePlatform` is the toy |

"Lock pure mode" = turn the six ❌ into tested green in `smash_world` (no engine, no net) BEFORE any
prop-up into `core`/`net`/`shell`. Then the outward iteration is: gap #3 (`SimState` baked lane) →
gap #1 (`PlaceStroke`) → gaps #4/#5 (`Signal::Hello`+`WorldId` in `Lobby`) → gap #6 (`SetRule`=whole
`Tune`).

## Minimal prop-up shims (once pure core is locked)
1. `Signal::Hello{world:WorldId, reads:BuildVersion, head:Option<EventId>}` + `Refuse` into `lobby.rs`.
2. `Lobby.world: WorldId` + world-log sync alongside the existing `resume`/`tune` machinery.
3. `SimState` baked-geometry lane the world fold materializes (gap #3).
4. `WorldEvent::PlaceStroke` carrying settled `StrokeProps`+polyline; bridge diffs `SimState.paths`.
