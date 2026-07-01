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

use crate::{SegClass, StrokeId, Tune, Vector2};
use serde::{Deserialize, Serialize};

pub mod bridge; // ggrs -> durable gate (§D.5): confirmed-frame diff, pure
pub mod fold; // the World scan: apply/fold/fold_from/chain/ingest + relation/ingest_topo, pure
pub mod migrate; // schema decode + upcast (Envelope -> WorldEvent), pure
pub mod store; // WorldStore trait + MemStore (always); SqliteStore gated behind `storage` (rusqlite)

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

/// A player's stable identity across sessions, sockets, and reconnects (NOT the ggrs handle, NOT the
/// ws). A player is a world ENTITY: they own the ink/geometry they place, and their presence is world
/// state. `= blake3` of the durable identity key (today the local `Identity`; later a claimed pubkey).
/// `WORLD` = system/authored geometry that no player owns.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct PlayerId(pub [u8; 32]);

impl StageId {
    pub const DEFAULT: StageId = StageId(0);
}
impl PlayerId {
    pub const WORLD: PlayerId = PlayerId([0u8; 32]);
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
    /// Lay a built platform/wall segment (Forge / platform-gun). `class` decides collision role;
    /// `owner` is the player who placed it (`PlayerId::WORLD` = authored/system geometry).
    PlacePlatform { at: Vector2, len: f32, class: SegClass, stroke: StrokeId, owner: PlayerId },
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
    /// Jump effective state back to the fold-through-`to` (git reset, recorded FORWARD — history kept).
    /// Handled in `fold` (it re-folds the prefix); `apply` treats it as a no-op (one node has no prefix).
    Reset { to: EventId },
    /// A player enters the world (first join, or a SETTLED reconnect): binds their id to display data;
    /// fold marks them online. `name` is human text (len-prefixed bincode, hash-stable) — the only
    /// variable-length field on the wire. DEBOUNCED upstream: a transient wire blip must NOT emit this.
    PlayerJoin { player: PlayerId, name: String, color: u32, char_pick: u16 },
    /// A player leaves SETTLED (clean quit, or the reconnect window expired). Fold marks them offline
    /// and KEEPS their owned geometry — logging off is not a delete. Debounced upstream.
    PlayerLeave { player: PlayerId },
    /// The world OWNER sets/changes the background (content-addressed gif/image; `None` = stage default).
    /// A folded event, NOT the frozen `Seed.bg`, so changing it doesn't fork the `WorldId`; last write wins.
    SetBackground { bg: Option<AssetId> },
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

/// Inverse of `canon` for the current schema. Lets a non-`bincode` crate (the shell's GodotStore)
/// decode `Seed`/`WorldEvent` blobs it wrote. Old-schema rows still go through `migrate::decode`.
pub fn decanon<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> T {
    bincode::deserialize(bytes).expect("canon bytes decode")
}

/// An asset's identity = `blake3(bytes)`. The id IS the content, so a put is idempotent and a fetch
/// self-verifies. Shared by every `WorldStore` backend so their asset ids match.
pub fn asset_id(bytes: &[u8]) -> AssetId {
    AssetId(*blake3::hash(bytes).as_bytes())
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
                owner: PlayerId::WORLD,
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

/// Byte-freeze: the exact on-wire encoding of every `WorldEvent` variant and the `event_id` chain over
/// them. Tune-independent (no `Seed`). If a variant is reordered or a field moved/removed/retyped,
/// these digests change and CI fails — the mechanical enforcement of the §2 append-only rule. To add a
/// variant at the END: keep all lines below, append the new variant + its two golden lines.
#[cfg(test)]
mod golden {
    use super::*;
    use crate::{SegClass, Vector2};

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{:02x}", x)).collect()
    }

    fn frozen_events() -> Vec<WorldEvent> {
        vec![
            WorldEvent::PlacePlatform { at: Vector2::new(1.0, 2.0), len: 32.0, class: SegClass::Floor, stroke: 0, owner: PlayerId::WORLD },
            WorldEvent::ErasePlatform { placed: EventId([1u8; 32]) },
            WorldEvent::Revert { target: EventId([2u8; 32]) },
            WorldEvent::SetRule { key: RuleKey(3), val: RuleVal(1.5) },
            WorldEvent::Upgrade { to: BuildVersion(2), mig: MigrationId(1) },
            WorldEvent::Reset { to: EventId([3u8; 32]) },
            WorldEvent::PlayerJoin { player: PlayerId([4u8; 32]), name: "ann".to_string(), color: 0x11223344, char_pick: 5 },
            WorldEvent::PlayerLeave { player: PlayerId([4u8; 32]) },
            WorldEvent::SetBackground { bg: Some(AssetId([5u8; 32])) },
        ]
    }

    #[test]
    fn worldevent_bytes_are_frozen() {
        let expect = [
            "000000000000803f000000400000004201000000000000000000000000000000000000000000000000000000000000000000000000",
            "010000000101010101010101010101010101010101010101010101010101010101010101",
            "020000000202020202020202020202020202020202020202020202020202020202020202",
            "0300000003000000c03f",
            "040000000200000001000000",
            "050000000303030303030303030303030303030303030303030303030303030303030303",
            "0600000004040404040404040404040404040404040404040404040404040404040404040300000000000000616e6e443322110500",
            "070000000404040404040404040404040404040404040404040404040404040404040404",
            "08000000010505050505050505050505050505050505050505050505050505050505050505",
        ];
        for (e, want) in frozen_events().iter().zip(expect) {
            assert_eq!(hex(&canon(e)), want); // reordering/moving a field breaks this exact byte string
        }
    }

    #[test]
    fn event_id_chain_is_frozen() {
        let expect = [
            "916ae96b60edf5cbec536fb6ba7329f550e84c7298e5975a1e9ccbcb1ce3438e",
            "1fd16723ec5303f0b7f01f079da0a954037a91d35fb79679a471ee1131b6f2f9",
            "71d86c5a062f45cfbe77c4c336e538cf58a244e4c9b1d6f6d11626b846808fc1",
            "341dc9a520ce237f00922eb7bae27766a08d38aa5b2af299b12df2ad6936c9e3",
            "d02cb0ccb1b0122462f5d1afaff806ab0412db8c790d5104b11706e752fe5b1b",
            "783c0a9c2fee12416e408e9040ca6ba2182025abca7b47dcc69deae59b86a22f",
            "02e2ed3e3b8d8007887eaeb58037eb8fafe6700b6f463d3dd6b3263586cab4c7",
            "92b948b3846a0b087d169d9d482a6bf384a24497eef046281431b8c48e73f0dc",
            "3f3fe3308babe4d9c2cccb3b9540ec394ce64f34ffa5e148073f318cfa6d728d",
        ];
        let mut parent = None;
        for (e, want) in frozen_events().iter().zip(expect) {
            let id = event_id(parent, Schema(1), &canon(e));
            assert_eq!(hex(&id.0), want); // the whole hash chain is pinned end to end
            parent = Some(id);
        }
    }
}
