# Reducer / Event-Sourcing / Functional-Core Architecture for Games

A survey of the prior art behind "state is a fold over an event log," mapped onto a Rust simulation core built as a pure `reduce_next_state` FSM plus a World modeled as a fold over a durable event log.

---

## 0. The one-sentence thesis, stated four ways

The same equation shows up independently in databases, UI frameworks, RTS netcode, and functional programming:

```
state = fold(reduce, seed, events)
```

| Tradition | seed | event | reduce | Name for it |
|---|---|---|---|---|
| Event sourcing (Fowler / Young) | empty aggregate | domain event | apply | "rebuild by replaying the log" |
| Elm / Redux | `init` model | action/msg | `update` / reducer | The Elm Architecture |
| RTS lockstep (AoE) | map seed | player command | simulation tick | "sync the commands, not the state" |
| Rollback netcode (GGPO / ggrs) | last confirmed frame | input | `advance_frame` | speculative re-execution |

Each community rediscovered that if the transition function is **pure and deterministic**, you get replay, audit, time-travel, undo, cheap network sync, and testability as *corollaries of the same property*, not separate features.

---

## 1. Event Sourcing + CQRS

### 1.1 The append-only log as source of truth

Fowler: *"Event Sourcing ensures that all changes to application state are stored as a sequence of events."* Events are the durable truth; current state is a derived, disposable projection: *"We can discard the application state completely and rebuild it by re-running the events from the event log on an empty application."*

```rust
fn current_state(events: &[WorldEvent]) -> World {
    events.iter().fold(World::seed(), |w, ev| apply(w, ev))
}
```

Never UPDATE "the world"; append `PlayerJoined`, `StageChosen`, `MatchFinished`, and the world *is* the reduction. "Defaults" are just seed events prepended to the log — which makes the initial condition itself auditable and replayable.

> **→ Applies to this project.** Your World-as-event-log plan *is* event sourcing, unnamed. `apply` is the aggregate reducer; `World::seed()` is the empty aggregate; "defaults = initial events" is the standard seeding trick. Adopt the vocabulary (`aggregate`, `projection`, `command`, `event`, `snapshot`).

### 1.2 Command vs. Event — the distinction that prevents most bugs

- A **command** is a *request*: imperative, may be rejected, expresses intent. `ChooseStage(Battlefield)`. Can fail validation.
- An **event** is a *fact*: past tense, already happened, never invalid. `StageChosen(Battlefield)`.

The command handler validates and *decides* which events to emit. The event fold must be **total** — applying a stored event can never fail, or the log is un-replayable.

```rust
// Command side: decide. May reject. Produces events.
fn decide(w: &World, cmd: Command) -> Result<Vec<WorldEvent>, Reject>;
// Event side: apply. Total. Never fails. This is what fold() uses.
fn apply(w: World, ev: &WorldEvent) -> World;
```

> **→ Applies to this project.** Your lobby FSM already returns a `Vec<Effect>` — it's behaving like a command handler (`decide`). Split cleanly: lobby FSM ingests *commands*, emits *world events + effects*; the World reducer only *folds events* and is total. Do not let the World-fold return `Result`.

### 1.3 CQRS and projections / read-models

CQRS separates the write model (commands → events) from **read models** (projections) optimized for querying. A projection is itself a fold into a query-shaped structure:

```rust
fn lobby_list_projection(events: &[WorldEvent]) -> Vec<LobbyRow> { … }
fn player_stats_projection(events: &[WorldEvent]) -> HashMap<PlayerId, Stats> { … }
```

Multiple projections from one log is the payoff: new stat/leaderboard/"home room" summary = a new fold + replay history. No mutable-store migration.

> **→ Applies to this project.** The "home room" and any lobby-browser UI are **projections** over the durable world log, not separate stored state. Model each read surface as `fold(events) -> ViewModel`; backfill new views by replaying.

### 1.4 Snapshots to bound replay cost

Folding from genesis is O(events). Snapshots checkpoint the aggregate at a known version: `state = fold(reduce, snapshot_at(v), events[v..])`. Rules: a snapshot is a **cache, never truth** (you must be able to delete all snapshots and rebuild from events); tag it with the event offset it was taken at; regenerate it when reducer logic changes.

> **→ Applies to this project.** Add snapshots keyed by log offset *once fold time is measurable* — not before (premature snapshotting couples you to a serialization format). Keep a test that periodically rebuilds from genesis and asserts equality.

### 1.5 Idempotency

- **Command idempotency:** client-generated command id, dedupe on ingest so a resent `ChooseStage` doesn't append a second `StageChosen`.
- **Event application idempotency** is automatic if `apply` is pure over `(state, event)` and events carry monotonic sequence numbers.
- **Effect idempotency** is the hard one: the shell executing effects must tolerate re-execution, because a crash between "append event" and "run effect" forces replay. Gate outbound calls so replay doesn't re-fire side effects.

> **→ Applies to this project.** Give every lobby command a client id and dedupe. When an `Effect` is externally visible (spawn a live lobby, send a packet, write a file), make its executor idempotent or gate on "already ran effect for event N?". Pure/internal effects need no gating.

### 1.6 Event versioning, schema evolution, upcasting

The log is forever, so event *shapes* must evolve without breaking old folds. Tactics: versioned events, weak schema, upcasting, in-place transformation, copy-and-transform. Key rules:

- **Additive-only within a version** (Young: *"a new version of an event must be convertible from the old version. If not, it is a new event."*).
- **Upcasting** lifts `WorldEventV1` → `WorldEventV2` *as old events are read*, so the reducer only knows the newest shape:

```rust
enum StoredEvent { V1(EvV1), V2(EvV2) }        // what's on disk
fn upcast(e: StoredEvent) -> WorldEvent {       // runs before fold
    match e {
        StoredEvent::V1(v1) => WorldEvent::from(v1), // fill defaults
        StoredEvent::V2(v2) => WorldEvent::from(v2),
    }
}
```

- **Weak schema** (tolerant reader): in Rust, `#[serde(default)]` on new fields is weak-schema-by-default.

> **→ Applies to this project.** Your durable `WorldEvent` enum needs versioning discipline *before* the first deployment writes events you can't delete: (1) tag persisted events with a version, (2) `#[serde(default)]` for additive fields, (3) reserve an `upcast()` seam between deserialization and the fold. High-frequency *inputs* (§6) are ephemeral and exempt.

### 1.7 Game-specific uses

Event sourcing appears in games under other names: **replays/saved films** (input log + seed, §3), **killcams** (bounded re-fold of recent inputs), **server-authoritative rewind** for lag comp (fold to an earlier offset, re-fold with corrected inputs), **audit/anti-cheat/crash repro** ("the log deterministically reproduces the bug").

---

## 2. Functional Core / Imperative Shell (Gary Bernhardt)

### 2.1 The pattern

*Boundaries* (SCNA 2012): put all decisions and logic in a **pure functional core** (values in, values out, no I/O); push all I/O and mutation to a thin **imperative shell**. Core "does the thinking"; shell "does the doing." Testability: the core has many paths but zero dependencies (many fast unit tests); the shell has few paths but real dependencies (few integration tests).

### 2.2 Mapping to `reduce(State, Event) -> (State, Vec<Effect>)`

The reducer is the functional core. `Effect` is a **description of an action, not the action**:

```rust
// FUNCTIONAL CORE — pure, total, deterministic. No I/O, no clock, no ambient RNG.
fn reduce(state: State, ev: Event) -> (State, Vec<Effect>);
// IMPERATIVE SHELL — the only place effects touch the world.
fn run(effects: Vec<Effect>) { for e in effects { execute(e); } }
```

Purity rules: no wall-clock reads in the core (time is an input field); no ambient RNG (seed is state, `(value, next_seed) = rng(seed)` folded through); no I/O or global mutation.

### 2.3 Why this is exactly what deterministic netcode and replay need

Determinism is what a functional core *is* when you also forbid ambient nondeterminism (clock, RNG, hash-map iteration order, FP drift). Given that: **replay** = re-run the core over the recorded stream; **rollback** = re-run forward over corrected inputs (safe because the shell's effects aren't re-run); **cheap sync** = ship inputs not state. All three from one property.

> **→ Applies to this project.** `reduce_next_state` is the functional core; the lobby FSM returning `Vec<Effect>` is core-returns-effect-descriptions. Enforce mechanically: (1) the core crate has no I/O dependency; (2) time, RNG seed, peer inputs enter only as event fields; (3) the shell is the sole `Effect` executor. Worth a `dl`/lint rail forbidding `std::time`, `rand::thread_rng`, and `HashMap` iteration in the core crate.

---

## 3. Command Pattern / Deterministic Simulation / "Sync the commands, not the state"

### 3.1 The RTS lineage: Age of Empires

Bettner & Terrano, *"1500 Archers on a 28.8"* (GDC 2001): rather than passing the status of each unit, run the **exact same simulation** on each machine, passing each an **identical set of commands** executed at the same simulation time. You can't send 1500 units' state; you *can* send "player 2 ordered these 6 units to attack." **Lockstep**: advance a turn only once all peers' commands for that turn arrived. Determinism is absolute — identical inputs must produce byte-identical state or the sims "desync." Classic desync sources: floating point across compilers/CPUs, hash-map iteration order, uninitialized memory. AoE ran a **checksum** of world state each turn to detect divergence early.

### 3.2 One property, three payoffs

Because the sim is deterministic and a recording is just the command stream: **sync** (ship commands, bandwidth ∝ input rate), **replay** (persist commands + seed, re-fold), **rollback** (re-fold from a confirmed frame).

### 3.3 Halo saved films

Bungie's Blam! engine stores **saved films as recorded inputs plus initial state**, not video; playback deterministically re-simulates. A 16-player match ≈ 25 MB because only inputs are stored. Event sourcing applied to gameplay: seed + input-event-log, current frame = fold.

### 3.4 GGPO / rollback

GGPO removed the "wait for all inputs" stall: local inputs advance immediately, missing remote inputs are **predicted**, and on misprediction the engine **rolls back** to the last confirmed frame and **re-simulates forward** — invisibly, up to ~150 ms. Precondition: a **deterministic engine**.

> **→ Applies to this project.** `ggrs` (Rust GGPO) calls your `advance_frame(inputs)` — that *is* `reduce_next_state`. Requirements: (1) deterministic sim (no wall clock, no `thread_rng`, no nondeterministic float paths, no unordered iteration affecting state); (2) cheap serialize/restore of sim state; (3) only *inputs* cross the wire. Add a per-frame **checksum** of sim state and ship mismatches to your existing event firehose (`/ev`) — desync detection is not optional in a rollback game.

---

## 4. ECS as Alternative / Complement

### 4.1 What ECS changes

ECS is a *data layout* choice (entities = bags of plain-data components; systems iterate over queries) — orthogonal to the reducer/event-sourcing question. It's about *how state is stored*, not *how transitions are described*.

### 4.2 ECS × rollback: bevy_ggrs

`bevy_ggrs` advances/rolls back in a **dedicated schedule** and **snapshots only components/resources you explicitly register** (`rollback_component_with_clone::<Transform>()`). Unregistered state is *not* rolled back. Rollback entities get a stable `RollbackId`; snapshots are per-frame maps. Documented pitfall: **every rollback restore triggers Bevy change-detection**, so `Changed<T>` systems fire on every rollback. Deep point: in ECS "what is sim state" is whatever you *registered*; in a hand-rolled reducer the state *is* the `State` struct — explicit and total by construction.

### 4.3 ECS × event sourcing

Different layers: ECS is the *current-state* representation, event sourcing is *how you got there*. A rollback ECS is essentially event-sourced inputs materialized into component storage each frame.

### 4.4 When ECS vs. a hand-rolled reducer

| Prefer hand-rolled reducer (`enum State` + `match`) | Prefer ECS |
|---|---|
| Small, bounded sim state (a match, a lobby FSM) | Many heterogeneous entities |
| Determinism + trivial snapshot paramount | Iteration perf over thousands of entities |
| State boundary total/explicit for rollback | Loose system/plugin composition |
| Rollback = clone a small struct | Rollback = register components, accept change-detection cost |

For a 2–8 player Smash-like with a compact per-match sim, a hand-rolled `enum` + `match` reducer is usually the better rollback substrate: cloning the whole `State` is cheap and the boundary is total by definition.

> **→ Applies to this project.** Your memory already records the deliberate choice to hand-roll the FSM as `enum` + `match` for rollback. Right call for the *match sim* under `ggrs`. Reserve ECS (if ever) for rendering/VFX entities *outside* the rolled-back sim — the shell's presentation layer, which must never feed back into sim state.

---

## 5. boardgame.io — the cleanest published reducer/event-sourced multiplayer framework

### 5.1 The architecture

- State splits into **`G`** (your game state) and **`ctx`** (framework turn/phase/player context).
- **Moves** (you write) change `G`. **Events** (framework: `endTurn`, `endPhase`…) change `ctx`.
- **The game reducer stays pure.** Moves that touch `ctx` *queue* framework events applied after the move — same "return effect descriptions, apply at the boundary" discipline. Randomness goes through a framework `Random` plugin so replays stay reproducible.
- **Client and server run the same reducer.** Client applies moves optimistically; server is authoritative; they reconcile. One transition function, two locations.

### 5.2 What transfers to a Rust sim

- **Two-state split** `G` vs `ctx` maps onto *match sim state* vs *lobby/turn-order FSM state* — distinct reducers, distinct event vocabularies.
- **Player moves vs framework events** ≈ your players' actions vs your lobby FSM's phase transitions; both pure, neither does I/O.
- **Determinism plumbing** — thread the RNG seed through state; boardgame.io's `Random` plugin exists specifically to keep the reducer pure.
- **Same reducer client + server** — share the exact reducer crate so both fold identically; divergence is an RTS-desync-class bug.

### 5.3 Lineage: Redux and the Elm Architecture

The reducer-plus-effects shape descends from **Elm** (`update : (Msg, Model) -> Model`) and **Redux** (`(state, action) => newState`).

| Elm | Redux | Your sim |
|---|---|---|
| `Model` | state tree | `State` / `World` |
| `Msg` | action | `Event` / command |
| `update` | reducer | `reduce_next_state` / `apply` |
| `Cmd`/`Sub` | middleware / effects | `Vec<Effect>` |

Where do effects go in a pure reducer? Elm returns `(Model, Cmd Msg)`; the runtime performs the `Cmd`. **redux-loop** ports this to Redux — reducers *return* effects the middleware runs. That signature — `update(state, msg) -> (state, effects)` — **is your `reduce(State, Event) -> (State, Vec<Effect>)`**, and it is Bernhardt's boundary in the Elm/Redux idiom. Three traditions, one signature.

> **→ Applies to this project.** You are building "the Elm Architecture for a Rust game sim." Your lobby-FSM-returns-`Vec<Effect>` is redux-loop's `(state, effects)`. Read boardgame.io's `G`/`ctx` split and `Random`/`events` plugins as a reference for keeping the reducer pure while doing turn management + randomness. Keep one shared reducer crate for any client/server split.

---

## 6. Two-Tier Event Logs — the unifying decision

Not all "events" have the same lifetime; conflating them is the most common mistake here.

| | **Tier 1: Inputs (ephemeral)** | **Tier 2: World events (durable)** |
|---|---|---|
| Frequency | High (30–60 Hz) | Low (lobby lifecycle: joined, stage chosen, result) |
| Persisted? | No — discarded after frame confirmed | Yes — append-only, forever, versioned |
| Rolled back? | **Yes** — predicted, corrected, re-simulated | **No** — facts, never rewound |
| Network checksum? | derived sim state checksummed each frame | reconciled at the world layer, coarser |
| Owner | `ggrs` / rollback loop | World event store |
| Analogy | GGPO input frames, Halo saved-film inputs | Event-sourced domain events |
| Reducer | `reduce_next_state` (match sim) | `apply` (World fold) |

Why the split matters: different determinism/persistence contracts (never store 60 Hz inputs; Tier 2 needs versioning/upcasting, Tier 1 doesn't); only Tier 1's derived state enters the per-frame checksum; **a Tier-1 session produces Tier-2 events** (a match runs entirely in Tier 1, and on end appends one `MatchFinished { winner, stage }` to the world log); **a lobby is a live Tier-1 instance on a Tier-2 world.**

> **→ Applies to this project.** Enforce the line in code:
> - **Tier 1 (`InputEvent`):** consumed by `reduce_next_state` under `ggrs`; never serialized to the durable store; derived sim state checksummed each frame; desyncs → `/ev`.
> - **Tier 2 (`WorldEvent`):** append-only, `#[serde(default)]` + `upcast()` versioning, folded by `apply`, projected into read-models (lobby browser, home room, stats).
> - **The seam:** a running lobby is a Tier-1 instance parameterized by a Tier-2 `World` (fold of its log up to now); it emits a small number of Tier-2 events back into the log. Lint rail: nothing in the `WorldEvent` fold may read a clock or RNG; nothing in the per-frame input path may write the durable store.

---

## 7. Consolidated mapping

| This project | Named pattern | Primary source |
|---|---|---|
| `reduce_next_state` FSM | Functional core / reducer / `update` | Bernhardt; Elm/Redux |
| Lobby FSM returning `Vec<Effect>` | redux-loop `(state, effects)`; `decide` | redux-loop; Fowler CQRS |
| World = `fold(events)`; defaults = seed events | Event sourcing | Fowler |
| Home room / lobby views | Projections / read-models (CQRS) | Fowler; Young |
| Durable `WorldEvent` evolving | Event versioning / upcasting / weak schema | Young; Overeem et al. |
| Match sim under `ggrs` | Deterministic sim + rollback (GGPO) | Bettner/Terrano; GGPO |
| "Ship inputs not world state" | Lockstep "sync the commands" | Bettner/Terrano |
| Per-frame checksum + firehose | Desync detection | Bettner/Terrano |
| Recorded input + seed replay | Saved films | Bungie Blam! |
| Hand-rolled `enum`+`match` over ECS | Reducer-over-ECS tradeoff | bevy_ggrs |
| `G`/`ctx` ≈ match-sim / lobby-FSM split | boardgame.io reducer model | boardgame.io |
| Two-tier logs | Ephemeral inputs vs durable events | synthesis |

The single discipline behind all of it: **keep the core pure and deterministic, express side effects as returned data, execute them only at the shell, and never let a clock, ambient RNG, or unordered iteration into a fold.** Replay, rollback, cheap sync, audit, time-travel, testability, desync detection — all corollaries of that one rule.

---

## Sources

**Event Sourcing / CQRS / versioning**
- [Fowler — *Event Sourcing*](https://martinfowler.com/eaaDev/EventSourcing.html) · [Azure — *Event Sourcing pattern*](https://learn.microsoft.com/en-us/azure/architecture/patterns/event-sourcing)
- [Greg Young Q&A — CodeOpinion](https://codeopinion.com/greg-young-answers-your-event-sourcing-questions/) · [Notes on Young, *Versioning in an Event Sourced System*](https://github.com/luque/Notes--Versioning-Event-Sourced-System)
- [Overeem et al. — *The Dark Side of Event Sourcing* (SANER 2017, PDF)](https://www.movereem.nl/files/2017SANER-eventsourcing.pdf) · [Schema evolution empirical study (JSS 2021)](https://www.sciencedirect.com/science/article/pii/S0164121221000674)
- [How to (not) do event versioning — Event-Driven.io](https://event-driven.io/en/how_to_do_event_versioning/) · [Events Versioning — Marten](https://martendb.io/events/versioning.html)

**Functional Core / Imperative Shell**
- [Bernhardt — *Boundaries*](https://www.destroyallsoftware.com/talks/boundaries) · [Casas — *Functional Core, Imperative Shell*](http://www.javiercasas.com/articles/functional-programming-patterns-functional-core-imperative-shell/)

**Deterministic sim / lockstep / rollback / replays**
- [Bettner & Terrano — *1500 Archers on a 28.8* (PDF)](https://zoo.cs.yale.edu/classes/cs538/readings/papers/terrano_1500arch.pdf) · [mirror](https://www.gamedeveloper.com/programming/1500-archers-on-a-28-8-network-programming-in-age-of-empires-and-beyond)
- [Lockstep protocol — Wikipedia](https://en.wikipedia.org/wiki/Lockstep_protocol) · [GGPO README](https://github.com/pond3r/ggpo/blob/master/doc/README.md) · [GGPO — Wikipedia](https://en.wikipedia.org/wiki/GGPO) · [SnapNet — Rollback](https://www.snapnet.dev/blog/netcode-architectures-part-2-rollback/)
- [Halo 3 Saved Films](https://halo.fandom.com/wiki/Saved_Films) · [Blam! engine — c20](https://c20.reclaimers.net/general/blam/)

**ECS / rollback in Rust**
- [bevy_ggrs README](https://github.com/gschup/bevy_ggrs) · [architecture](https://github.com/gschup/bevy_ggrs/blob/main/docs/architecture.md) · [pitfalls](https://github.com/gschup/bevy_ggrs/blob/main/docs/pitfalls.md)

**boardgame.io / Redux / Elm**
- [boardgame.io — Events](https://github.com/boardgameio/boardgame.io/blob/main/docs/documentation/events.md) · [Plugins](https://github.com/boardgameio/boardgame.io/blob/main/docs/documentation/plugins.md)
- [Redux — *Prior Art*](https://redux.js.org/understanding/history-and-design/prior-art) · [Redux — Reducers](https://redux.js.org/tutorials/fundamentals/part-3-state-actions-reducers) · [redux-loop](https://github.com/redux-loop/redux-loop)
