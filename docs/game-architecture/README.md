# Game Architecture Reference

Aggregated deep-research (verified July 2026) on how to build a networked indie game: a Godot 4 web-export client + Rust simulation core, a rollback fighter now, a persistent Terraria-esque shared world later, self-hosted on one VPS. Six documents, each standalone, each with a Sources section and `→ Applies to this project` callouts.

## The documents

| File | Question it answers |
|---|---|
| [netcode.md](netcode.md) | The five netcode families (lockstep, rollback, prediction+reconciliation, snapshot interpolation, lag comp), authority models, the canonical literature (Gaffer/Valve/Halo/Overwatch/AoE/TRIBES/Gambetta/Infil), and why you **layer** rollback-fighter over state-sync-world rather than blend. |
| [state-sync-consensus.md](state-sync-consensus.md) | "Is timestamp-sync Raft?" → no, it's LWW-CRDT. Consensus (Raft/Paxos) vs CRDTs vs deterministic lockstep vs host-authority, with a decision matrix. When each fits a game. |
| [event-sourcing.md](event-sourcing.md) | The reducer / event-sourcing / functional-core lineage behind "World = fold over an event log." Command-vs-event, CQRS projections, snapshots, versioning/upcasting, boardgame.io, Elm/Redux, and the two-tier log split. |
| [datastores.md](datastores.md) | The tiered datastore model (realtime RAM / Redis / relational / analytics / blobs), real shipped-game backends (EVE, Discord, Minecraft, WoW, Supercell, Fortnite…), Postgres failure modes, economy anti-dupe, and when to add each tier. |
| [rust-godot-stack.md](rust-godot-stack.md) | gdext status, **the WASM/emscripten wall** (why matchbox can't web-export), sqlx vs diesel vs sea-orm, the `WorldStore` trait, a self-hostable-backend support matrix (Nakama/Colyseus/SpacetimeDB/…), and TURN/TURNS-443 for hostile networks. |
| [reference-repos.md](reference-repos.md) | Curated open-source repos + writeups to read and steal from, with fit-to-project ratings (extreme_bevy, bevy_ggrs, boardgame.io, Godot templates, Nakama, SpacetimeDB Blackholio…). |

## The one-paragraph synthesis

**Layer, don't blend.** There are two things you can put on the wire — inputs or state — and you use both, in separate layers, never mixed on one simulation:

```
Fighter combat   : rollback (ggrs), P2P, matchbox/WebRTC, bit-exact deterministic Rust core, 60 Hz
                   -> the VPS never simulates fights, only signals them
Persistent world : client prediction + server reconciliation + snapshot interpolation,
                   authoritative on the VPS, ~10-20 Hz, ordinary float physics (no determinism needed),
                   interest-managed (only replicate tile sections + entities near each player)
```

Both layers are the **same equation** — `state = fold(reduce, seed, events)` — at different timescales: frame (ggrs), round (lobby), world (durable log). Keep the core **pure and deterministic** (no clock, no ambient RNG, no unordered iteration in a fold) and express side effects as returned `Effect` data executed only at the shell; then replay, rollback, cheap sync, audit, and desync-detection all fall out of that one property.

## Settled decisions (and why)

| Decision | Rationale | Doc |
|---|---|---|
| **Fighter = deterministic rollback (ggrs), P2P** | Industry standard for twitch; cheapest sync; matches the pure Rust core | netcode, state-sync-consensus |
| **World = authoritative on the VPS** (not P2P, not CRDT for valuable state) | Anti-cheat + real transactions need one owner; sidesteps host migration | netcode §3, state-sync-consensus §4 |
| **Timestamp/LWW sync is NOT Raft** and NOT for economy | LWW silently drops concurrent edits; Raft is CP consensus, wrong shape, hates churn | state-sync-consensus §0–2 |
| **Terrain/decoration MAY use a real CRDT** (yrs/Automerge), never bare timestamps | Mutable non-valuable state can converge offline; inventory/currency cannot | state-sync-consensus §2.6 |
| **Postgres via sqlx** for durable state; ggrs in RAM; blobs on HTTP/nginx | Right tier separation dodges every classic PG failure mode | datastores, rust-godot-stack §3 |
| **Reducer stays pure; IO behind a `WorldStore` trait** | Testability, swappable backend, precondition for rollback + replay | event-sourcing §2, rust-godot-stack §3 |
| **Two-tier event log**: ephemeral inputs (rolled back) vs durable world events (persisted, versioned) | Different determinism/persistence contracts; only inputs' derived state enters the frame checksum | event-sourcing §6 |
| **Netplay transport = Godot `WebRtcDataChannel` from Rust** (not matchbox) | The emscripten WASM wall blocks wasm-bindgen crates in Godot web export | rust-godot-stack §2 |
| **SpacetimeDB rejected** for the web client | No Godot-web path (Rust SDK hits the wall; no C# web export); BSL single-instance restriction | rust-godot-stack §4 |
| **Hostile-network mode = relay-only TURNS/tcp/443** | The only transport a VPN treats like HTTPS | rust-godot-stack §5, netcode |
| **Interest management** (Terraria section / Minecraft chunk scoping) keeps one VPS viable | Replicate only what's near each player; a "home room" is one interest region | netcode §7 |

## Open questions still to resolve in design

- Whether durable **world events** ride their own reliable ordered channel (host-authoritative, applied once — like `Signal::Tune` today) **or** go through ggrs as a second rolled-back input kind. Terraria-terrain-affecting-physics wants the latter; smash-settings-for-a-round wants the former. (netcode §8, event-sourcing §6)
- `WorldEvent` enum shape + versioning discipline before the first deployment writes un-deletable events.
- When to introduce snapshots for the world fold (only once fold time is measurable).
