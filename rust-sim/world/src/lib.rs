//! Durable-world types: the byte-frozen foundation (plans/world-protocol.md).
//!
//! Model in one breath: a `Seed` is the frozen genesis of a world, content-addressed as a `WorldId`.
//! Everything after is a hash-chained log of `WorldEvent`s (git objects: each `EventId` =
//! blake3(parent ‖ schema ‖ payload)). `state = fold(apply, Seed, events)`; HEAD = the chain tip.
//! Storage is per-peer SQLite; P2P sync = git have/want scoped to a shared `WorldId`.
//!
//! Two rules keep the bytes non-breaking (positional bincode is unforgiving):
//!   1. Append-only. New enum variants / struct fields go LAST. Never reorder, never remove.
//!   2. Migrate via events, not on-read magic: a `WorldEvent::Upgrade` is the schema transition, and
//!      the world's version is `fold(log).head`, not a compile-time const (git HEAD, not the binary).
//!
//! `serde` everywhere, newtypes everywhere, fixed-size where possible. No bare `usize`/`String`/path
//! on the wire.

use serde::{Deserialize, Serialize};
use smash_core::{SegClass, StrokeId, Tune, Vector2};

pub mod bridge; // ggrs -> durable gate (§D.5): confirmed-frame diff, pure
pub mod fold; // the World scan: apply/fold/fold_from/chain/ingest, pure
pub mod store; // side effects: WorldStore save/load (MemStore + SqliteStore)

// ─────────────────────────────────────────────────────────────────────────────────────────────────
// Identities — fixed-size, self-verifying. A hash IS the name; equality is byte equality.
// ─────────────────────────────────────────────────────────────────────────────────────────────────

/// A world's identity = `blake3(build ‖ canon(Seed))`. Folding `build` into the hash makes a build
/// bump a hard namespace split: two builds can never share a `WorldId` or cross-join a lobby.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct WorldId(pub [u8; 32]);

/// A content-addressed asset (gif background, authored stage blob): fetched by hash over HTTP, never
/// a path/URL, so it rides inside a `Seed`'s hash and is self-verifying on fetch.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct AssetId(pub [u8; 32]);

/// An event's identity = `blake3(parent ‖ schema ‖ payload)`. THE key (not a seq): needs no central
/// allocator, so P2P peers mint ids with no coordination; identical appends dedupe; a mutated payload
/// yields a different id, so `ingest` catches tampering by recompute.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct EventId(pub [u8; 32]);

/// A version tag. Two distinct uses, never conflate (plans/world-protocol.md §2):
///   - a binary's READ-capability (the max it can decode) — a compile-time const, like a git client.
///   - a world's version — DATA, `fold(log)`'s latest `Upgrade.to` = HEAD. They meet at handshake.
#[derive(Copy, Clone, PartialEq, Eq, Ord, PartialOrd, Debug, Serialize, Deserialize)]
pub struct BuildVersion(pub u32);

/// Derived chain height (topological order along `parent` links). For `ORDER BY` / cursors / UI only
/// — NOT identity (that's `EventId`) and NOT gapless-authoritative across a fork.
#[derive(Copy, Clone, PartialEq, Eq, Ord, PartialOrd, Debug, Serialize, Deserialize)]
pub struct Seq(pub u64);

/// The event-encoding version, per stored event. Decode dispatches on it (upcast); orthogonal to the
/// world's `BuildVersion`. "Can my binary parse this row" vs "what version is the world at".
#[derive(Copy, Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Schema(pub u16);

/// Names one migration step (the transform an `Upgrade` event applies). The apply-arm resolves it to
/// a known state transform; kept as data so an old log replays its upgrades from bytes alone.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct MigrationId(pub u32);

/// Which base stage a world seeds from. `0` is the default stage. Sized so it rides in a `Seed` hash.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct StageId(pub u16);

/// Who laid a piece of built geometry. `>= 0` = a player slot (decays like ink); `< 0` = permanent
/// (author/forge-baked). Mirrors the stroke-owner convention in the sim.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Owner(pub i8);

impl StageId {
    pub const DEFAULT: StageId = StageId(0);
}
impl Owner {
    pub const PERMANENT: Owner = Owner(-1);
}

// ─────────────────────────────────────────────────────────────────────────────────────────────────
// Seed — the frozen genesis. A `WorldId` pins ONE `Seed` forever; it is the git genesis commit.
// Needs no upcaster: a schema bump changes `build` -> changes the `WorldId` -> a new namespace.
// ─────────────────────────────────────────────────────────────────────────────────────────────────

// No Debug/PartialEq: `Tune` derives neither, and `Seed` embeds it. Compare seeds by `canon()` bytes.
#[derive(Clone, Serialize, Deserialize)]
pub struct Seed {
    /// Genesis version. HEAD moves PAST this via `Upgrade` events; this is only where it started.
    pub build: BuildVersion,
    /// The round config. "World == Tune" by reference: the seed carries the rules, live edits are
    /// `SetRule` events folded on top.
    pub rules: Tune,
    /// Base stage selector.
    pub stage: StageId,
    /// Content-addressed background (gif/image), not a path. `None` = the stage's own backdrop.
    pub bg: Option<AssetId>,
    /// "The defaults are the initial events": author-placed geometry, expressed as the same events a
    /// live edit produces. Fully covered by the `WorldId` hash.
    pub authored: Vec<WorldEvent>,
}

// ─────────────────────────────────────────────────────────────────────────────────────────────────
// WorldEvent — the ONLY mutable thing. PROVISIONAL: variants below are not byte-frozen until the
// "event-sourced vs ggrs-frame" split is decided (plans/world-protocol.md open question). The infra
// around it (ids, envelope, node, hashing) IS frozen and won't move under it.
// ─────────────────────────────────────────────────────────────────────────────────────────────────

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[non_exhaustive] // forward-tolerant: downstream `match` must carry a `_ =>`
pub enum WorldEvent {
    /// Lay a built platform/wall segment (Forge / platform-gun). `class` decides collision role.
    PlacePlatform { at: Vector2, len: f32, class: SegClass, stroke: StrokeId, owner: Owner },
    /// Remove a previously placed segment by its event id. This IS git-revert for a placement: a new
    /// forward event that negates a prior one; both stay in history. `Revert`/`Reset` generalize it.
    ErasePlatform { placed: EventId },
    /// Undo an arbitrary prior event by appending its deterministic inverse (git revert, forward-only).
    Revert { target: EventId },
    /// Change a round rule (folds into the live `Tune`). Payload shape provisional.
    SetRule { key: RuleKey, val: RuleVal },
    /// A migration IS an event: its apply-arm transforms state schema v -> v+1, and it moves the
    /// world's version (fold HEAD). Forces a snapshot right after (plans/world-protocol.md §4.5).
    Upgrade { to: BuildVersion, mig: MigrationId },
    // APPEND NEW VARIANTS HERE, at the end. Never reorder, never remove.
}

/// PROVISIONAL rule addressing. A `u16` index into a rule table beats a `String` key (sized, no
/// realloc, stable bytes), but the final mapping to `Tune` fields is the open decision.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct RuleKey(pub u16);
#[derive(Copy, Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct RuleVal(pub f32);

// ─────────────────────────────────────────────────────────────────────────────────────────────────
// Storage forms.
//   Envelope = the bytes at rest / on the wire: schema tag + the positional payload. Decode by schema.
//   Node     = a decoded, chain-linked event: id (content addr) + parent link + derived height + ev.
// ─────────────────────────────────────────────────────────────────────────────────────────────────

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Envelope {
    pub schema: Schema,
    pub payload: Vec<u8>, // bincode(WorldEvent) at `schema`
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Node {
    pub id: EventId,
    pub parent: Option<EventId>, // None = first event after the seed (chain root)
    pub seq: Seq,                // derived height, for order/cursor only
    pub ev: WorldEvent,
}

// ─────────────────────────────────────────────────────────────────────────────────────────────────
// The two hash functions — a world's identity and an event's identity. Canonical bytes = bincode
// (deterministic, positional). blake3 = 32-byte digest.
// ─────────────────────────────────────────────────────────────────────────────────────────────────

/// Canonical bytes of any serializable value. ONE config, used for hashing AND storage so the hash a
/// peer computes matches the hash the author committed. (bincode 1.x default is fixint LE, stable.)
pub fn canon<T: Serialize>(v: &T) -> Vec<u8> {
    bincode::serialize(v).expect("world types are infallible to encode")
}

/// A world's identity. `build` first (namespace split), then the canonical seed bytes.
pub fn world_id(seed: &Seed) -> WorldId {
    // let mut h = blake3::Hasher::new();
    // h.update(&seed.build.0.to_le_bytes());
    // h.update(&canon(seed));
    // WorldId(*h.finalize().as_bytes())
    let mut h = blake3::Hasher::new();
    h.update(&seed.build.0.to_le_bytes());
    h.update(&canon(seed));
    WorldId(*h.finalize().as_bytes())
}

/// An event's identity, chaining onto its parent. Genesis parent (None) hashes as 32 zero bytes — a
/// real event digest is never all-zero, so None and a hypothetical zero-id do not collide in practice.
pub fn event_id(parent: Option<EventId>, schema: Schema, payload: &[u8]) -> EventId {
    let mut h = blake3::Hasher::new();
    h.update(&parent.map(|p| p.0).unwrap_or([0u8; 32]));
    h.update(&schema.0.to_le_bytes());
    h.update(payload);
    EventId(*h.finalize().as_bytes())
}

// ─────────────────────────────────────────────────────────────────────────────────────────────────
// Byte-stability guards. The full "golden bytes" fixture waits until WorldEvent is frozen; until
// then these lock the properties that must hold regardless: determinism, roundtrip, id stability.
// ─────────────────────────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_seed() -> Seed {
        Seed {
            build: BuildVersion(1),
            rules: Tune::default(),
            stage: StageId::DEFAULT,
            bg: None,
            authored: vec![WorldEvent::PlacePlatform {
                at: Vector2::new(100.0, 200.0),
                len: 64.0,
                class: SegClass::Floor,
                stroke: 0,
                owner: Owner::PERMANENT,
            }],
        }
    }

    #[test]
    fn canon_is_deterministic() {
        let s = fixture_seed();
        assert_eq!(canon(&s), canon(&s)); // same value -> same bytes, every time
    }

    #[test]
    fn seed_roundtrips() {
        let bytes = canon(&fixture_seed());
        let back: Seed = bincode::deserialize(&bytes).unwrap();
        assert_eq!(bytes, canon(&back)); // decode(encode(x)) re-encodes to the same bytes
    }

    #[test]
    fn world_id_stable_for_fixed_seed() {
        let a = world_id(&fixture_seed());
        let b = world_id(&fixture_seed());
        assert_eq!(a, b); // the identity is a pure function of the seed
    }

    #[test]
    fn event_id_chains_and_dedupes() {
        let payload = canon(&WorldEvent::SetRule { key: RuleKey(3), val: RuleVal(1.5) });
        let a = event_id(None, Schema(1), &payload);
        let a2 = event_id(None, Schema(1), &payload);
        assert_eq!(a, a2); // identical append -> identical id (dedupe)
        let child = event_id(Some(a), Schema(1), &payload);
        assert_ne!(a, child); // same payload, different parent -> different id (chain)
    }
}
