//! The World fold — the `scan` at the heart of the design (plans/world-protocol.md).
//!
//! Pure: `apply(state, node)` is one fold step; `fold(build, nodes)` is the whole scan; `fold_from`
//! restarts from a cached World (that is snapshot restore AND compaction). No IO, no clock, no RNG,
//! deterministic iteration (BTreeMap). The rxjs analogue is `events$.pipe(scan(apply, genesis))`.

use crate::{canon, event_id, BuildVersion, EventId, Node, Owner, Schema, Seq, WorldEvent};
use smash_core::{SegClass, Vector2};
use std::collections::{BTreeMap, HashSet};

/// MVP built segment (subset of a real forge stroke): enough to prove place/erase/revert fold.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct Plat {
    pub at: Vector2,
    pub len: f32,
    pub class: SegClass,
    pub owner: Owner,
}

/// The folded world state. Keyed by the placing event's id so erase/revert are O(1) and deterministic.
#[derive(Clone, PartialEq, Debug)]
pub struct World {
    pub version: BuildVersion,          // moves on `Upgrade` — fold HEAD == the world's version
    pub platforms: BTreeMap<EventId, Plat>,
    pub rules: BTreeMap<u16, f32>,
}

impl World {
    pub fn genesis(build: BuildVersion) -> Self {
        World { version: build, platforms: BTreeMap::new(), rules: BTreeMap::new() }
    }
}

/// One fold step. `self IS the state` (the `Reduce` shape). A `Revert`/`Erase` is a forward inverse
/// (git-revert), never a mutation of history.
pub fn apply(w: &mut World, n: &Node) {
    match &n.ev {
        WorldEvent::PlacePlatform { at, len, class, owner, .. } => {
            w.platforms.insert(n.id, Plat { at: *at, len: *len, class: *class, owner: *owner });
        }
        WorldEvent::ErasePlatform { placed } => {
            w.platforms.remove(placed);
        }
        WorldEvent::Revert { target } => {
            w.platforms.remove(target); // MVP: revert of a placement (its deterministic inverse)
        }
        WorldEvent::SetRule { key, val } => {
            w.rules.insert(key.0, val.0);
        }
        WorldEvent::Upgrade { to, .. } => {
            w.version = *to;
        }
    }
}

/// Full fold from genesis.
pub fn fold(build: BuildVersion, nodes: &[Node]) -> World {
    let mut w = World::genesis(build);
    for n in nodes {
        apply(&mut w, n);
    }
    w
}

/// Fold starting from a cached World instead of genesis — snapshot restore and compaction share this.
pub fn fold_from(base: &World, nodes: &[Node]) -> World {
    let mut w = base.clone();
    for n in nodes {
        apply(&mut w, n);
    }
    w
}

/// Build a hash-linked chain of nodes from bare events: each id = blake3(parent ‖ schema ‖ payload).
pub fn chain(events: &[WorldEvent]) -> Vec<Node> {
    let mut parent = None;
    let mut out = Vec::with_capacity(events.len());
    for (i, ev) in events.iter().enumerate() {
        let payload = canon(ev);
        let id = event_id(parent, Schema(1), &payload);
        out.push(Node { id, parent, seq: Seq(i as u64), ev: ev.clone() });
        parent = Some(id);
    }
    out
}

/// P2P ingest = git have/want reduce: append only nodes we lack (dedupe by content id). Idempotent.
pub fn ingest(local: &mut Vec<Node>, incoming: &[Node]) {
    let have: HashSet<EventId> = local.iter().map(|n| n.id).collect();
    for n in incoming {
        if !have.contains(&n.id) {
            local.push(n.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RuleKey, RuleVal};

    const B: BuildVersion = BuildVersion(1);

    fn place(x: f32) -> WorldEvent {
        WorldEvent::PlacePlatform {
            at: Vector2::new(x, 0.0),
            len: 32.0,
            class: SegClass::Floor,
            stroke: 0,
            owner: Owner::PERMANENT,
        }
    }

    #[test]
    fn fold_is_deterministic() {
        let nodes = chain(&[place(1.0), place(2.0), place(3.0)]);
        assert_eq!(fold(B, &nodes), fold(B, &nodes)); // same events -> same world
    }

    #[test]
    fn snapshot_equals_replay_and_compaction_preserves_state() {
        let nodes = chain(&[place(1.0), place(2.0), place(3.0), place(4.0)]);
        let full = fold(B, &nodes);
        // snapshot@2 is a pure cache of the prefix; compaction regrafts it as the base and drops the prefix.
        let snap = fold(B, &nodes[..2]);
        let compacted = fold_from(&snap, &nodes[2..]);
        assert_eq!(full, compacted); // THE compaction invariant: identical folded state
    }

    #[test]
    fn erase_is_the_forward_inverse_of_place() {
        let p = place(1.0);
        let pid = chain(&[p.clone()])[0].id; // the placement's content id
        let with = fold(B, &chain(&[p.clone()]));
        let erased = fold(B, &chain(&[p, WorldEvent::ErasePlatform { placed: pid }]));
        assert_eq!(erased, fold(B, &[])); // place then erase == never placed (baseline)
        assert_ne!(with, erased);
    }

    #[test]
    fn revert_negates_its_target() {
        let p = place(9.0);
        let pid = chain(&[p.clone()])[0].id;
        let reverted = fold(B, &chain(&[p, WorldEvent::Revert { target: pid }]));
        assert_eq!(reverted, fold(B, &[])); // git-revert: forward event, baseline state
    }

    #[test]
    fn upgrade_moves_the_version() {
        let nodes = chain(&[WorldEvent::Upgrade { to: BuildVersion(7), mig: crate::MigrationId(1) }]);
        assert_eq!(fold(B, &nodes).version, BuildVersion(7)); // fold HEAD == world version
    }

    #[test]
    fn setrule_folds() {
        let nodes = chain(&[
            WorldEvent::SetRule { key: RuleKey(3), val: RuleVal(1.0) },
            WorldEvent::SetRule { key: RuleKey(3), val: RuleVal(2.0) }, // last write wins
        ]);
        assert_eq!(fold(B, &nodes).rules.get(&3), Some(&2.0));
    }

    #[test]
    fn ingest_dedupes_and_is_idempotent() {
        let nodes = chain(&[place(1.0), place(2.0), place(3.0)]);
        let mut local = nodes[..1].to_vec(); // peer has only the first
        ingest(&mut local, &nodes); // pulls the missing tail
        ingest(&mut local, &nodes); // all duplicates -> no-op
        assert_eq!(local.len(), nodes.len()); // no dupes accumulated
        assert_eq!(fold(B, &local), fold(B, &nodes)); // converged to the same world
    }
}
