//! Side effects: `WorldStore` = the only IO surface (plans/world-protocol.md §3-4). The fold stays
//! pure; save/load lives here. Two impls: `MemStore` (tests / the RAM cache) and `SqliteStore` (real,
//! rusqlite bundled — the min-db, per-peer, SQLite-everywhere decision).

use crate::{canon, event_id, world_id, EventId, Node, Schema, Seed, Seq, WorldEvent, WorldId};

/// Current write schema. Read dispatches on the stored per-event schema (upcast); writes stamp this.
pub const SCHEMA: Schema = Schema(1);

/// The store contract. `append` chains onto the current head (content-addressed, so it is idempotent
/// and needs no seq allocator). `since`/`head` back P2P have/want sync.
pub trait WorldStore {
    fn publish(&mut self, seed: &Seed) -> WorldId;
    fn seed(&self, id: WorldId) -> Option<Seed>;
    fn append(&mut self, id: WorldId, ev: &WorldEvent) -> EventId;
    fn head(&self, id: WorldId) -> Option<EventId>;
    fn since(&self, id: WorldId, from: Option<EventId>) -> Vec<Node>;
    fn has(&self, id: WorldId, ev: EventId) -> bool;
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
}

// ─────────────────────────────────────────────────────────────────────────────────────────────────
// SqliteStore — the real, durable, per-peer store (rusqlite bundled). Schema mirrors §4.
// ─────────────────────────────────────────────────────────────────────────────────────────────────

pub struct SqliteStore {
    conn: rusqlite::Connection,
}

fn to32(v: Vec<u8>) -> [u8; 32] {
    let mut a = [0u8; 32];
    a.copy_from_slice(&v);
    a
}

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
             CREATE TABLE IF NOT EXISTS world_head(world_id BLOB PRIMARY KEY, head BLOB NOT NULL);",
        )
    }
}

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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fold::fold;
    use crate::{BuildVersion, RuleKey, RuleVal, StageId};

    fn seed() -> Seed {
        Seed {
            build: BuildVersion(1),
            rules: smash_core::Tune::default(),
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
