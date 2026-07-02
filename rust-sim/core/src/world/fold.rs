//! The World fold — the `scan` at the heart of the design (plans/world-protocol.md).
//!
//! Pure: `apply(state, node)` is one fold step; `fold(build, nodes)` is the whole scan; `fold_from`
//! restarts from a cached World (that is snapshot restore AND compaction). No IO, no clock, no RNG,
//! deterministic iteration (BTreeMap). The rxjs analogue is `events$.pipe(scan(apply, genesis))`.

use crate::world::{canon, event_id, AssetId, BuildVersion, EventId, Node, PlayerId, Schema, Seq, WorldEvent};
use serde::{Deserialize, Serialize};
use crate::{SegClass, Vector2};
use std::collections::{BTreeMap, HashMap, HashSet};

/// MVP built segment (subset of a real forge stroke): enough to prove place/erase/revert fold.
#[derive(Copy, Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Plat {
    pub at: Vector2,
    pub len: f32,
    pub class: SegClass,
    pub owner: PlayerId,
}

/// A folded permanent ink stroke: who laid it, its material row, and the simplified world-space
/// polyline (bend vertices only; loaders re-finalize into a live `InkPath`).
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Stroke {
    pub owner: PlayerId,
    pub stroke: crate::StrokeId,
    pub pts: Vec<Vector2>,
}

/// A folded player entity: their durable identity + display data + presence. Built by `PlayerJoin`,
/// flipped offline by `PlayerLeave` (the entry stays — logging off is not a delete).
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Player {
    pub id: PlayerId,
    pub name: String,
    pub color: u32,
    pub char_pick: u16,
    pub online: bool,
}

/// The folded world state. Keyed by the placing event's id so erase/revert are O(1) and deterministic.
/// `Serialize` so a snapshot blob (lock 6 / §4.5) is just `canon(&World)`.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct World {
    pub version: BuildVersion,          // moves on `Upgrade` — fold HEAD == the world's version
    pub players: BTreeMap<PlayerId, Player>, // players ARE world state; ink is owned by them
    pub platforms: BTreeMap<EventId, Plat>,
    pub rules: BTreeMap<u16, f32>,
    pub bg: Option<AssetId>,            // content-addressed background; None = the stage's own backdrop
    pub strokes: BTreeMap<EventId, Stroke>, // permanent player ink, keyed like platforms (appended LAST: positional bincode)
}

impl World {
    pub fn genesis(build: BuildVersion) -> Self {
        World {
            version: build,
            players: BTreeMap::new(),
            platforms: BTreeMap::new(),
            rules: BTreeMap::new(),
            bg: None,
            strokes: BTreeMap::new(),
        }
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
            w.strokes.remove(target); // a stroke placement's inverse: knocked out / moved away
        }
        WorldEvent::SetRule { key, val } => {
            w.rules.insert(key.0, val.0);
        }
        WorldEvent::Upgrade { to, .. } => {
            w.version = *to;
        }
        WorldEvent::Reset { .. } => {} // no-op here; `fold` re-folds the prefix (needs the whole log)
        WorldEvent::PlayerJoin { player, name, color, char_pick } => {
            w.players.insert(
                *player,
                Player { id: *player, name: name.clone(), color: *color, char_pick: *char_pick, online: true },
            );
        }
        WorldEvent::PlayerLeave { player } => {
            if let Some(p) = w.players.get_mut(player) {
                p.online = false; // keep the entity + their owned geometry; just mark offline
            }
        }
        WorldEvent::SetBackground { bg } => {
            w.bg = *bg; // owner changed the backdrop; last write wins (folded, not the frozen seed)
        }
        WorldEvent::AddStroke { owner, stroke, pts } => {
            w.strokes.insert(n.id, Stroke { owner: *owner, stroke: *stroke, pts: pts.clone() });
        }
    }
}

/// Full fold from genesis. `Reset{to}` re-folds the prefix through `to` (git reset, forward-recorded).
pub fn fold(build: BuildVersion, nodes: &[Node]) -> World {
    let mut w = World::genesis(build);
    for (i, n) in nodes.iter().enumerate() {
        if let WorldEvent::Reset { to } = &n.ev {
            // cut = index just past `to` in the already-seen prefix; unknown target -> back to genesis.
            let cut = nodes[..i].iter().position(|m| m.id == *to).map_or(0, |p| p + 1);
            w = fold(build, &nodes[..cut]);
        } else {
            apply(&mut w, n);
        }
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
/// Assumes in-order delivery; use `ingest_topo` when packets can arrive out of order.
pub fn ingest(local: &mut Vec<Node>, incoming: &[Node]) {
    let have: HashSet<EventId> = local.iter().map(|n| n.id).collect();
    for n in incoming {
        if !have.contains(&n.id) {
            local.push(n.clone());
        }
    }
}

/// Accepts possibly-out-of-order P2P delivery. `chain` is the ordered reachable prefix; `orphans` holds
/// nodes whose parent hasn't landed yet. A child that arrives early is BUFFERED (retained, not dropped)
/// and flushed onto the chain the moment its parent appears. This, not `ingest`, is what a live peer
/// runs (real delivery is unordered).
#[derive(Default)]
pub struct Ingestor {
    pub chain: Vec<Node>,
    orphans: Vec<Node>,
}

impl Ingestor {
    pub fn ingest(&mut self, incoming: &[Node]) {
        let known: HashSet<EventId> =
            self.chain.iter().chain(self.orphans.iter()).map(|n| n.id).collect();
        for n in incoming {
            if !known.contains(&n.id) {
                self.orphans.push(n.clone());
            }
        }
        // pull any orphan whose parent is now the chain tip (parent None when the chain is empty).
        loop {
            let tip = self.chain.last().map(|n| n.id);
            match self.orphans.iter().position(|n| n.parent == tip) {
                Some(pos) => self.chain.push(self.orphans.remove(pos)),
                None => break,
            }
        }
    }
}

/// How two chain tips relate, by walking `parent` links in a node pool. Drives handshake head-reconcile:
/// `Ancestor` -> the other fast-forwards; `Fork` -> `Refuse::Forked` (v1 forbids concurrent hosts).
#[derive(PartialEq, Eq, Debug)]
pub enum Relation {
    Same,
    Ancestor,   // a is an ancestor of b (b can fast-forward past a)
    Descendant, // b is an ancestor of a
    Fork,       // neither is an ancestor of the other — divergent history
}

pub fn relation(pool: &[Node], a: EventId, b: EventId) -> Relation {
    if a == b {
        return Relation::Same;
    }
    let parent: HashMap<EventId, Option<EventId>> = pool.iter().map(|n| (n.id, n.parent)).collect();
    let ancestry = |tip: EventId| -> HashSet<EventId> {
        let mut cur = Some(tip);
        let mut seen = HashSet::new();
        while let Some(id) = cur {
            seen.insert(id);
            cur = parent.get(&id).copied().flatten();
        }
        seen
    };
    if ancestry(b).contains(&a) {
        Relation::Ancestor
    } else if ancestry(a).contains(&b) {
        Relation::Descendant
    } else {
        Relation::Fork
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::{RuleKey, RuleVal};

    const B: BuildVersion = BuildVersion(1);

    fn place(x: f32) -> WorldEvent {
        WorldEvent::PlacePlatform {
            at: Vector2::new(x, 0.0),
            len: 32.0,
            class: SegClass::Floor,
            stroke: 0,
            owner: PlayerId::WORLD,
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
    fn reset_jumps_state_back_but_is_recorded_forward() {
        let a = place(1.0);
        let aid = chain(&[a.clone()])[0].id;
        // place A, place B, then Reset back to A -> state has only A, yet the log still holds 3 events.
        let nodes = chain(&[a, place(2.0), WorldEvent::Reset { to: aid }]);
        let w = fold(B, &nodes);
        assert_eq!(w.platforms.len(), 1); // rolled back to just-A
        assert_eq!(nodes.len(), 3); // history is kept (forward-recorded reset)
    }

    #[test]
    fn upgrade_moves_the_version() {
        let nodes = chain(&[WorldEvent::Upgrade { to: BuildVersion(7), mig: crate::world::MigrationId(1) }]);
        assert_eq!(fold(B, &nodes).version, BuildVersion(7)); // fold HEAD == world version
    }

    #[test]
    fn player_join_then_leave_keeps_entity_and_owned_ink() {
        let pid = PlayerId([9u8; 32]);
        let owned = WorldEvent::PlacePlatform {
            at: Vector2::new(3.0, 0.0),
            len: 16.0,
            class: SegClass::Floor,
            stroke: 0,
            owner: pid,
        };
        let nodes = chain(&[
            WorldEvent::PlayerJoin { player: pid, name: "kip".into(), color: 0xAABBCCDD, char_pick: 2 },
            owned,
            WorldEvent::PlayerLeave { player: pid },
        ]);
        let w = fold(B, &nodes);
        let p = w.players.get(&pid).expect("player still an entity after leaving");
        assert!(!p.online); // settled leave marks offline
        assert_eq!(p.name, "kip"); // display data preserved
        assert_eq!(w.platforms.len(), 1); // logging off is NOT a delete — their ink stays
        assert_eq!(w.platforms.values().next().unwrap().owner, pid); // still owned by them
    }

    #[test]
    fn set_background_folds_last_write_wins() {
        let a = AssetId([1u8; 32]);
        let b = AssetId([2u8; 32]);
        let nodes = chain(&[
            WorldEvent::SetBackground { bg: Some(a) },
            WorldEvent::SetBackground { bg: Some(b) }, // owner changes it -> newer wins
        ]);
        assert_eq!(fold(B, &nodes).bg, Some(b));
        // clearing back to the stage default is just another event
        let cleared = chain(&[WorldEvent::SetBackground { bg: Some(a) }, WorldEvent::SetBackground { bg: None }]);
        assert_eq!(fold(B, &cleared).bg, None);
    }

    #[test]
    fn addstroke_folds_and_revert_removes_it() {
        let pid = PlayerId([7u8; 32]);
        let add = WorldEvent::AddStroke {
            owner: pid,
            stroke: 1,
            pts: vec![Vector2::new(100.0, 500.0), Vector2::new(300.0, 500.0)],
        };
        let placed = chain(&[add.clone()])[0].id;
        let w = fold(B, &chain(&[add.clone()]));
        assert_eq!(w.strokes.len(), 1);
        let st = w.strokes.get(&placed).unwrap();
        assert_eq!(st.owner, pid);
        assert_eq!(st.stroke, 1);
        assert_eq!(st.pts.len(), 2);
        // knock it away: Revert of the placement is its inverse (same shape as platform erase)
        let gone = fold(B, &chain(&[add, WorldEvent::Revert { target: placed }]));
        assert!(gone.strokes.is_empty());
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
    fn ingestor_reorders_shuffled_delivery() {
        let nodes = chain(&[place(1.0), place(2.0), place(3.0)]);
        let mut ing = Ingestor::default();
        ing.ingest(&[nodes[2].clone(), nodes[0].clone(), nodes[1].clone()]); // out of order
        assert_eq!(ing.chain, nodes); // rebuilt in chain order
        assert_eq!(fold(B, &ing.chain), fold(B, &nodes));
    }

    #[test]
    fn ingestor_buffers_an_orphan_until_its_parent_lands() {
        let nodes = chain(&[place(1.0), place(2.0), place(3.0)]);
        let mut ing = Ingestor::default();
        ing.ingest(&[nodes[0].clone(), nodes[2].clone()]); // n1 missing -> n2 buffered
        assert_eq!(ing.chain, vec![nodes[0].clone()]); // only the reachable prefix is applied
        ing.ingest(&[nodes[1].clone()]); // parent arrives -> orphan flushes onto the chain
        assert_eq!(ing.chain, nodes);
    }

    #[test]
    fn relation_classifies_ancestor_and_fork() {
        use crate::world::{event_id, Schema};
        let base = chain(&[place(1.0), place(2.0)]); // n0 <- n1
        assert_eq!(relation(&base, base[0].id, base[1].id), Relation::Ancestor);
        // two divergent children of n1 = a fork.
        let mk = |x: f32| {
            let ev = place(x);
            let id = event_id(Some(base[1].id), Schema(1), &crate::world::canon(&ev));
            Node { id, parent: Some(base[1].id), seq: crate::world::Seq(2), ev }
        };
        let (a, b) = (mk(3.0), mk(4.0));
        let mut pool = base.clone();
        pool.push(a.clone());
        pool.push(b.clone());
        assert_eq!(relation(&pool, a.id, b.id), Relation::Fork);
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
