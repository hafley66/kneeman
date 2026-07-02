//! Side effects: `WorldStore` = the only IO surface (plans/world-protocol.md §3-4). The fold stays
//! pure; save/load lives here. Two impls: `MemStore` (tests / the RAM cache) and `SqliteStore` (real,
//! rusqlite bundled — the min-db, per-peer, SQLite-everywhere decision).

use crate::world::{canon, event_id, world_id, AssetId, EventId, Node, Schema, Seed, Seq, WorldEvent, WorldId};
use serde::{Deserialize, Serialize};

/// Current write schema. Read dispatches on the stored per-event schema (upcast); writes stamp this.
pub const SCHEMA: Schema = Schema(1);

/// A named save = a bookmark to a retained log point. Restore is just `append(Reset { to: head })`,
/// so a slot carries no state blob (the log holds it) — cheap while compaction is deferred. `at_ms` is
/// the client's wall clock at save (display/sort only); `seq` is the chain height then (display only).
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Slot {
    pub label: String,
    pub at_ms: u64,
    pub head: EventId,
    pub seq: Seq,
}

/// The store contract. `append` chains onto the current head (content-addressed, so it is idempotent
/// and needs no seq allocator). `since`/`head` back P2P have/want sync.
pub trait WorldStore {
    fn publish(&mut self, seed: &Seed) -> WorldId;
    fn seed(&self, id: WorldId) -> Option<Seed>;
    fn append(&mut self, id: WorldId, ev: &WorldEvent) -> EventId;
    fn head(&self, id: WorldId) -> Option<EventId>;
    fn since(&self, id: WorldId, from: Option<EventId>) -> Vec<Node>;
    fn has(&self, id: WorldId, ev: EventId) -> bool;

    // --- compaction (§4.5): a snapshot is a deletable cache; compact moves the replay floor ---
    /// Cache the folded state through `upto` (blob = `canon(&World)`). Non-destructive.
    fn put_snapshot(&mut self, id: WorldId, upto: EventId, blob: &[u8]);
    /// Latest snapshot: `(upto, blob)`.
    fn get_snapshot(&self, id: WorldId) -> Option<(EventId, Vec<u8>)>;
    /// Drop every event at or before `upto` (the low-water mark). Caller guarantees a snapshot through
    /// `upto` exists and that `upto <= min(live participants' head)`. `since(None)` then returns the tail.
    fn compact(&mut self, id: WorldId, upto: EventId);

    // --- content-addressed asset blobs (gif backgrounds): id = blake3(bytes), stored out of the log ---
    /// Store bytes under their own hash; idempotent. Returns the `AssetId` a `SetBackground` records.
    fn put_asset(&mut self, bytes: &[u8]) -> AssetId;
    /// Fetch by id (self-verifying: the id IS the content hash). `None` = not held.
    fn get_asset(&self, id: AssetId) -> Option<Vec<u8>>;

    // --- named save slots: bookmarks into the retained log (restore = Reset{to: head}) ---
    /// Upsert a slot by label (re-saving a label overwrites it).
    fn put_slot(&mut self, id: WorldId, slot: &Slot);
    /// All slots for a world, oldest first.
    fn slots(&self, id: WorldId) -> Vec<Slot>;
    /// Remove a slot by label (no-op if absent). Does not touch the log.
    fn del_slot(&mut self, id: WorldId, label: &str);
    /// Rough stored footprint (log payloads + assets + snapshot) for the size watch, in bytes.
    fn cache_bytes(&self, id: WorldId) -> u64;
}

/// Compute the id an append onto `parent` would get. Shared by both impls so their ids match.
fn next_id(parent: Option<EventId>, payload: &[u8]) -> EventId {
    event_id(parent, SCHEMA, payload)
}

// ─────────────────────────────────────────────────────────────────────────────────────────────────
// MemStore — the RAM cache / test double.
// ─────────────────────────────────────────────────────────────────────────────────────────────────

#[derive(Default)]
pub struct MemStore {
    seeds: std::collections::HashMap<WorldId, Seed>,
    logs: std::collections::HashMap<WorldId, Vec<Node>>,
    snaps: std::collections::HashMap<WorldId, (EventId, Vec<u8>)>,
    assets: std::collections::HashMap<AssetId, Vec<u8>>,
    slots: std::collections::HashMap<WorldId, Vec<Slot>>,
}

impl MemStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl WorldStore for MemStore {
    fn publish(&mut self, seed: &Seed) -> WorldId {
        let id = world_id(seed);
        self.seeds.entry(id).or_insert_with(|| seed.clone());
        self.logs.entry(id).or_default();
        id
    }
    fn seed(&self, id: WorldId) -> Option<Seed> {
        self.seeds.get(&id).cloned()
    }
    fn append(&mut self, id: WorldId, ev: &WorldEvent) -> EventId {
        let log = self.logs.entry(id).or_default();
        let parent = log.last().map(|n| n.id);
        let eid = next_id(parent, &canon(ev));
        if log.iter().all(|n| n.id != eid) {
            log.push(Node { id: eid, parent, seq: Seq(log.len() as u64), ev: ev.clone() });
        }
        eid
    }
    fn head(&self, id: WorldId) -> Option<EventId> {
        self.logs.get(&id).and_then(|l| l.last()).map(|n| n.id)
    }
    fn since(&self, id: WorldId, from: Option<EventId>) -> Vec<Node> {
        let log = match self.logs.get(&id) {
            Some(l) => l,
            None => return Vec::new(),
        };
        match from {
            None => log.clone(),
            Some(f) => match log.iter().position(|n| n.id == f) {
                Some(i) => log[i + 1..].to_vec(),
                None => log.clone(), // unknown cursor -> hand back everything (peer re-folds)
            },
        }
    }
    fn has(&self, id: WorldId, ev: EventId) -> bool {
        self.logs.get(&id).is_some_and(|l| l.iter().any(|n| n.id == ev))
    }
    fn put_snapshot(&mut self, id: WorldId, upto: EventId, blob: &[u8]) {
        self.snaps.insert(id, (upto, blob.to_vec()));
    }
    fn get_snapshot(&self, id: WorldId) -> Option<(EventId, Vec<u8>)> {
        self.snaps.get(&id).cloned()
    }
    fn compact(&mut self, id: WorldId, upto: EventId) {
        if let Some(log) = self.logs.get_mut(&id) {
            if let Some(cut) = log.iter().position(|n| n.id == upto) {
                log.drain(..=cut); // keep events strictly after the low-water mark
            }
        }
    }
    fn put_asset(&mut self, bytes: &[u8]) -> AssetId {
        let id = AssetId(*blake3::hash(bytes).as_bytes());
        self.assets.entry(id).or_insert_with(|| bytes.to_vec());
        id
    }
    fn get_asset(&self, id: AssetId) -> Option<Vec<u8>> {
        self.assets.get(&id).cloned()
    }
    fn put_slot(&mut self, id: WorldId, slot: &Slot) {
        let v = self.slots.entry(id).or_default();
        v.retain(|s| s.label != slot.label); // upsert by label
        v.push(slot.clone());
    }
    fn slots(&self, id: WorldId) -> Vec<Slot> {
        self.slots.get(&id).cloned().unwrap_or_default()
    }
    fn del_slot(&mut self, id: WorldId, label: &str) {
        if let Some(v) = self.slots.get_mut(&id) {
            v.retain(|s| s.label != label);
        }
    }
    fn cache_bytes(&self, id: WorldId) -> u64 {
        let log: usize = self.logs.get(&id).map_or(0, |l| l.iter().map(|n| canon(&n.ev).len()).sum());
        let snap: usize = self.snaps.get(&id).map_or(0, |(_, b)| b.len());
        let assets: usize = self.assets.values().map(|b| b.len()).sum(); // assets are shared, not per-world
        (log + snap + assets) as u64
    }
}

// ─────────────────────────────────────────────────────────────────────────────────────────────────
// SqliteStore — the real, durable, per-peer store (rusqlite bundled). Schema mirrors §4.
// ─────────────────────────────────────────────────────────────────────────────────────────────────

#[cfg(feature = "storage")]
pub struct SqliteStore {
    conn: rusqlite::Connection,
}

#[cfg(feature = "storage")]
fn to32(v: Vec<u8>) -> [u8; 32] {
    let mut a = [0u8; 32];
    a.copy_from_slice(&v);
    a
}

#[cfg(feature = "storage")]
impl SqliteStore {
    /// Open (file path or ":memory:") and create the schema if absent.
    pub fn open(path: &str) -> rusqlite::Result<Self> {
        let conn = rusqlite::Connection::open(path)?;
        Self::init(&conn)?;
        Ok(SqliteStore { conn })
    }
    pub fn in_memory() -> rusqlite::Result<Self> {
        let conn = rusqlite::Connection::open_in_memory()?;
        Self::init(&conn)?;
        Ok(SqliteStore { conn })
    }
    fn init(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS world(
                 id BLOB PRIMARY KEY, build INTEGER NOT NULL, seed BLOB NOT NULL);
             CREATE TABLE IF NOT EXISTS world_event(
                 id BLOB PRIMARY KEY, world_id BLOB NOT NULL, parent BLOB,
                 seq INTEGER NOT NULL, schema INTEGER NOT NULL, payload BLOB NOT NULL);
             CREATE INDEX IF NOT EXISTS world_event_by_world ON world_event(world_id, seq);
             CREATE TABLE IF NOT EXISTS world_head(world_id BLOB PRIMARY KEY, head BLOB NOT NULL);
             CREATE TABLE IF NOT EXISTS world_snapshot(
                 world_id BLOB PRIMARY KEY, upto BLOB NOT NULL, blob BLOB NOT NULL);
             CREATE TABLE IF NOT EXISTS asset(id BLOB PRIMARY KEY, bytes BLOB NOT NULL);
             CREATE TABLE IF NOT EXISTS world_slot(
                 world_id BLOB NOT NULL, label TEXT NOT NULL, at_ms INTEGER NOT NULL,
                 head BLOB NOT NULL, seq INTEGER NOT NULL, PRIMARY KEY(world_id, label));",
        )
    }
}

#[cfg(feature = "storage")]
impl WorldStore for SqliteStore {
    fn publish(&mut self, seed: &Seed) -> WorldId {
        let id = world_id(seed);
        self.conn
            .execute(
                "INSERT OR IGNORE INTO world(id, build, seed) VALUES(?1, ?2, ?3)",
                rusqlite::params![&id.0[..], seed.build.0, canon(seed)],
            )
            .expect("publish");
        id
    }
    fn seed(&self, id: WorldId) -> Option<Seed> {
        self.conn
            .query_row("SELECT seed FROM world WHERE id=?1", [&id.0[..]], |r| r.get::<_, Vec<u8>>(0))
            .ok()
            .map(|b| bincode::deserialize(&b).expect("seed decode"))
    }
    fn append(&mut self, id: WorldId, ev: &WorldEvent) -> EventId {
        let parent = self.head(id);
        let payload = canon(ev);
        let eid = next_id(parent, &payload);
        let seq: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM world_event WHERE world_id=?1", [&id.0[..]], |r| r.get(0))
            .unwrap_or(0);
        let parent_blob: Option<&[u8]> = parent.as_ref().map(|p| &p.0[..]);
        self.conn
            .execute(
                "INSERT OR IGNORE INTO world_event(id, world_id, parent, seq, schema, payload)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![&eid.0[..], &id.0[..], parent_blob, seq, SCHEMA.0, payload],
            )
            .expect("append");
        self.conn
            .execute(
                "INSERT INTO world_head(world_id, head) VALUES(?1, ?2)
                 ON CONFLICT(world_id) DO UPDATE SET head=?2",
                rusqlite::params![&id.0[..], &eid.0[..]],
            )
            .expect("advance head");
        eid
    }
    fn head(&self, id: WorldId) -> Option<EventId> {
        self.conn
            .query_row("SELECT head FROM world_head WHERE world_id=?1", [&id.0[..]], |r| r.get::<_, Vec<u8>>(0))
            .ok()
            .map(|b| EventId(to32(b)))
    }
    fn since(&self, id: WorldId, from: Option<EventId>) -> Vec<Node> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, parent, seq, payload FROM world_event WHERE world_id=?1 ORDER BY seq")
            .expect("prepare since");
        let all: Vec<Node> = stmt
            .query_map([&id.0[..]], |r| {
                let id: Vec<u8> = r.get(0)?;
                let parent: Option<Vec<u8>> = r.get(1)?;
                let seq: i64 = r.get(2)?;
                let payload: Vec<u8> = r.get(3)?;
                Ok(Node {
                    id: EventId(to32(id)),
                    parent: parent.map(|p| EventId(to32(p))),
                    seq: Seq(seq as u64),
                    ev: bincode::deserialize(&payload).expect("event decode"),
                })
            })
            .expect("query since")
            .map(|r| r.expect("row"))
            .collect();
        match from {
            None => all,
            Some(f) => match all.iter().position(|n| n.id == f) {
                Some(i) => all[i + 1..].to_vec(),
                None => all,
            },
        }
    }
    fn has(&self, id: WorldId, ev: EventId) -> bool {
        self.conn
            .query_row(
                "SELECT 1 FROM world_event WHERE world_id=?1 AND id=?2",
                rusqlite::params![&id.0[..], &ev.0[..]],
                |_| Ok(()),
            )
            .is_ok()
    }
    fn put_snapshot(&mut self, id: WorldId, upto: EventId, blob: &[u8]) {
        self.conn
            .execute(
                "INSERT INTO world_snapshot(world_id, upto, blob) VALUES(?1, ?2, ?3)
                 ON CONFLICT(world_id) DO UPDATE SET upto=?2, blob=?3",
                rusqlite::params![&id.0[..], &upto.0[..], blob],
            )
            .expect("put_snapshot");
    }
    fn get_snapshot(&self, id: WorldId) -> Option<(EventId, Vec<u8>)> {
        self.conn
            .query_row("SELECT upto, blob FROM world_snapshot WHERE world_id=?1", [&id.0[..]], |r| {
                Ok((EventId(to32(r.get::<_, Vec<u8>>(0)?)), r.get::<_, Vec<u8>>(1)?))
            })
            .ok()
    }
    fn compact(&mut self, id: WorldId, upto: EventId) {
        // resolve the low-water mark's seq, then delete everything at or below it.
        let seq: Option<i64> = self
            .conn
            .query_row(
                "SELECT seq FROM world_event WHERE world_id=?1 AND id=?2",
                rusqlite::params![&id.0[..], &upto.0[..]],
                |r| r.get(0),
            )
            .ok();
        if let Some(seq) = seq {
            self.conn
                .execute(
                    "DELETE FROM world_event WHERE world_id=?1 AND seq<=?2",
                    rusqlite::params![&id.0[..], seq],
                )
                .expect("compact");
        }
    }
    fn put_asset(&mut self, bytes: &[u8]) -> AssetId {
        let id = AssetId(*blake3::hash(bytes).as_bytes());
        self.conn
            .execute("INSERT OR IGNORE INTO asset(id, bytes) VALUES(?1, ?2)", rusqlite::params![&id.0[..], bytes])
            .expect("put_asset");
        id
    }
    fn get_asset(&self, id: AssetId) -> Option<Vec<u8>> {
        self.conn
            .query_row("SELECT bytes FROM asset WHERE id=?1", [&id.0[..]], |r| r.get::<_, Vec<u8>>(0))
            .ok()
    }
    fn put_slot(&mut self, id: WorldId, slot: &Slot) {
        self.conn
            .execute(
                "INSERT INTO world_slot(world_id, label, at_ms, head, seq) VALUES(?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(world_id, label) DO UPDATE SET at_ms=?3, head=?4, seq=?5",
                rusqlite::params![&id.0[..], slot.label, slot.at_ms, &slot.head.0[..], slot.seq.0],
            )
            .expect("put_slot");
    }
    fn slots(&self, id: WorldId) -> Vec<Slot> {
        let mut stmt = self
            .conn
            .prepare("SELECT label, at_ms, head, seq FROM world_slot WHERE world_id=?1 ORDER BY at_ms")
            .expect("prepare slots");
        stmt.query_map([&id.0[..]], |r| {
            Ok(Slot {
                label: r.get(0)?,
                at_ms: r.get::<_, i64>(1)? as u64,
                head: EventId(to32(r.get::<_, Vec<u8>>(2)?)),
                seq: Seq(r.get::<_, i64>(3)? as u64),
            })
        })
        .expect("query slots")
        .map(|r| r.expect("slot row"))
        .collect()
    }
    fn del_slot(&mut self, id: WorldId, label: &str) {
        self.conn
            .execute("DELETE FROM world_slot WHERE world_id=?1 AND label=?2", rusqlite::params![&id.0[..], label])
            .expect("del_slot");
    }
    fn cache_bytes(&self, id: WorldId) -> u64 {
        let log: i64 = self
            .conn
            .query_row("SELECT COALESCE(SUM(LENGTH(payload)),0) FROM world_event WHERE world_id=?1", [&id.0[..]], |r| r.get(0))
            .unwrap_or(0);
        let snap: i64 = self
            .conn
            .query_row("SELECT COALESCE(LENGTH(blob),0) FROM world_snapshot WHERE world_id=?1", [&id.0[..]], |r| r.get(0))
            .unwrap_or(0);
        let assets: i64 = self
            .conn
            .query_row("SELECT COALESCE(SUM(LENGTH(bytes)),0) FROM asset", [], |r| r.get(0))
            .unwrap_or(0);
        (log + snap + assets).max(0) as u64
    }
}

#[cfg(all(test, feature = "storage"))]
mod tests {
    use super::*;
    use crate::world::fold::fold;
    use crate::world::{BuildVersion, RuleKey, RuleVal, StageId};

    fn seed() -> Seed {
        Seed {
            build: BuildVersion(1),
            rules: crate::Tune::default(),
            stage: StageId::DEFAULT,
            bg: None,
            authored: vec![],
        }
    }

    fn sample_events() -> Vec<WorldEvent> {
        vec![
            WorldEvent::SetRule { key: RuleKey(1), val: RuleVal(0.5) },
            WorldEvent::SetRule { key: RuleKey(2), val: RuleVal(9.0) },
        ]
    }

    // Gif background bytes round-trip through the content-addressed asset store, keyed by their hash.
    #[test]
    fn asset_blob_roundtrips_by_content_hash() {
        let mut s = SqliteStore::in_memory().unwrap();
        let gif = b"GIF89a...fake pixels...";
        let id = s.put_asset(gif);
        assert_eq!(s.put_asset(gif), id); // same bytes -> same id (idempotent put)
        assert_eq!(s.get_asset(id).as_deref(), Some(&gif[..]));
        assert_eq!(s.get_asset(crate::world::AssetId([0u8; 32])), None); // unknown id -> miss
    }

    // Save then load, on the real sqlite backend, reconstructs the same folded world.
    #[test]
    fn sqlite_save_load_roundtrip() {
        let mut s = SqliteStore::in_memory().unwrap();
        let id = s.publish(&seed());
        for ev in sample_events() {
            s.append(id, &ev);
        }
        assert!(s.seed(id).is_some()); // seed blob survived the round-trip
        let log = s.since(id, None); // load
        let w = fold(BuildVersion(1), &log);
        assert_eq!(w.rules.get(&1), Some(&0.5));
        assert_eq!(w.rules.get(&2), Some(&9.0));
    }

    // The two impls are observationally identical: same ids, same log, same fold.
    #[test]
    fn mem_and_sqlite_agree() {
        let (mut mem, mut sql) = (MemStore::new(), SqliteStore::in_memory().unwrap());
        let (a, b) = (mem.publish(&seed()), sql.publish(&seed()));
        assert_eq!(a, b); // same content -> same WorldId
        for ev in sample_events() {
            assert_eq!(mem.append(a, &ev), sql.append(b, &ev)); // same chained EventId
        }
        let (lm, ls) = (mem.since(a, None), sql.since(b, None));
        assert_eq!(fold(BuildVersion(1), &lm), fold(BuildVersion(1), &ls));
    }

    // Compaction is lossless: snapshot@k + the surviving tail folds to the same world as the full log.
    #[test]
    fn compaction_preserves_state_through_the_store() {
        use crate::world::WorldEvent;
        let mut s = SqliteStore::in_memory().unwrap();
        let id = s.publish(&seed());
        let evs: Vec<WorldEvent> = (0..5).map(|k| WorldEvent::SetRule { key: RuleKey(k), val: RuleVal(k as f32) }).collect();
        for ev in &evs {
            s.append(id, ev);
        }
        let full = fold(BuildVersion(1), &s.since(id, None)); // truth before compaction
        let log = s.since(id, None);
        let cut = log[2].id; // low-water mark = 3rd event
        let snap_state = fold(BuildVersion(1), &log[..3]); // state through the cut
        s.put_snapshot(id, cut, &smash_world_canon(&snap_state));
        s.compact(id, cut); // drop events 0..=2

        assert_eq!(s.since(id, None).len(), 2); // only the tail survives on disk
        let (upto, blob) = s.get_snapshot(id).unwrap();
        assert_eq!(upto, cut);
        let base: crate::world::fold::World = bincode::deserialize(&blob).unwrap();
        let rebuilt = crate::world::fold::fold_from(&base, &s.since(id, None)); // snapshot + tail
        assert_eq!(rebuilt, full); // identical to the pre-compaction world
    }

    fn smash_world_canon<T: serde::Serialize>(v: &T) -> Vec<u8> {
        bincode::serialize(v).unwrap()
    }

    // `since(head)` returns nothing (a caught-up peer pulls no diff); `since(None)` returns all.
    #[test]
    fn since_cursor_bounds_the_diff() {
        let mut s = SqliteStore::in_memory().unwrap();
        let id = s.publish(&seed());
        for ev in sample_events() {
            s.append(id, &ev);
        }
        let head = s.head(id).unwrap();
        assert!(s.since(id, Some(head)).is_empty()); // already have head -> no diff
        assert_eq!(s.since(id, None).len(), 2);
    }
}
