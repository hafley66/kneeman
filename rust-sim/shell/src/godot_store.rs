//! A `WorldStore` backed by Godot's `user://` filesystem (real disk on native, IndexedDB on the web
//! export — Godot bridges the platform gap, so one code path persists everywhere). This is the client
//! backend; the headless server uses `SqliteStore`. Both satisfy the same trait, so nothing above the
//! store cares which is mounted.
//!
//! Layout: `user://world/<hex>.seed` (canon Seed), `.log` (framed events), `.snap` (upto+blob);
//! `user://asset/<hex>` (raw content-addressed blobs). The event log is append-only, so it is just a
//! byte stream; no SQL needed. RAM-cached per world (load-once, append-through), and the whole log is
//! rewritten on append — fine early; compaction caps replay length before it matters.

use std::collections::HashMap;

use godot::classes::file_access::ModeFlags;
use godot::classes::{DirAccess, FileAccess};
use godot::prelude::{GString, PackedByteArray};

use smash_core::world::fold::chain;
use smash_core::world::store::{WorldStore, SCHEMA};
use smash_core::world::{
    asset_id, canon, decanon, event_id, world_id, AssetId, EventId, Node, Seed, WorldEvent, WorldId,
};

const WORLD_DIR: &str = "user://world";
const ASSET_DIR: &str = "user://asset";

struct Cache {
    seed: Seed,
    nodes: Vec<Node>,
    snap: Option<(EventId, Vec<u8>)>,
}

pub struct GodotStore {
    worlds: HashMap<WorldId, Cache>,
}

impl GodotStore {
    /// Ensure the `user://` subdirs exist and return an empty (lazy-loading) store.
    pub fn open() -> Self {
        if let Some(mut da) = DirAccess::open(&GString::from("user://")) {
            da.make_dir_recursive(&GString::from("world"));
            da.make_dir_recursive(&GString::from("asset"));
        }
        GodotStore { worlds: HashMap::new() }
    }

    fn hex(id: &[u8; 32]) -> String {
        id.iter().map(|b| format!("{:02x}", b)).collect()
    }
    fn world_path(id: WorldId, ext: &str) -> String {
        format!("{WORLD_DIR}/{}.{ext}", Self::hex(&id.0))
    }
    fn asset_path(id: AssetId) -> String {
        format!("{ASSET_DIR}/{}", Self::hex(&id.0))
    }

    fn read_file(path: &str) -> Option<Vec<u8>> {
        let f = FileAccess::open(&GString::from(path), ModeFlags::READ)?;
        Some(f.get_buffer(f.get_length() as i64).to_vec())
    }
    fn write_file(path: &str, bytes: &[u8]) {
        if let Some(mut f) = FileAccess::open(&GString::from(path), ModeFlags::WRITE) {
            f.store_buffer(&PackedByteArray::from(bytes));
            f.close();
        }
    }

    /// Frame the log as repeated `[u32 len][canon(ev)]` records.
    fn write_log(id: WorldId, nodes: &[Node]) {
        let path = Self::world_path(id, "log");
        let Some(mut f) = FileAccess::open(&GString::from(path.as_str()), ModeFlags::WRITE) else {
            return;
        };
        for n in nodes {
            let payload = canon(&n.ev);
            f.store_32(payload.len() as u32);
            f.store_buffer(&PackedByteArray::from(payload.as_slice()));
        }
        f.close();
    }
    fn read_log(id: WorldId) -> Vec<WorldEvent> {
        let path = Self::world_path(id, "log");
        let Some(mut f) = FileAccess::open(&GString::from(path.as_str()), ModeFlags::READ) else {
            return Vec::new();
        };
        let len = f.get_length();
        let mut out = Vec::new();
        while f.get_position() < len {
            let n = f.get_32() as i64;
            if n == 0 {
                break;
            }
            let bytes = f.get_buffer(n).to_vec();
            out.push(decanon::<WorldEvent>(&bytes));
        }
        f.close();
        out
    }

    /// Load a world's cache from disk (seed + log + snapshot) if the seed file is present.
    fn load(id: WorldId) -> Option<Cache> {
        let seed: Seed = decanon(&Self::read_file(&Self::world_path(id, "seed"))?);
        let nodes = chain(&Self::read_log(id));
        let snap = Self::read_file(&Self::world_path(id, "snap")).and_then(|b| {
            (b.len() >= 32).then(|| {
                let mut upto = [0u8; 32];
                upto.copy_from_slice(&b[..32]);
                (EventId(upto), b[32..].to_vec())
            })
        });
        Some(Cache { seed, nodes, snap })
    }
}

impl WorldStore for GodotStore {
    fn publish(&mut self, seed: &Seed) -> WorldId {
        let id = world_id(seed);
        if !self.worlds.contains_key(&id) {
            // re-attach to an on-disk world, else create it (write the seed, empty log).
            let cache = Self::load(id).unwrap_or_else(|| {
                Self::write_file(&Self::world_path(id, "seed"), &canon(seed));
                Cache { seed: seed.clone(), nodes: Vec::new(), snap: None }
            });
            self.worlds.insert(id, cache);
        }
        id
    }
    fn seed(&self, id: WorldId) -> Option<Seed> {
        self.worlds.get(&id).map(|c| c.seed.clone())
    }
    fn append(&mut self, id: WorldId, ev: &WorldEvent) -> EventId {
        let Some(c) = self.worlds.get_mut(&id) else { return event_id(None, SCHEMA, &canon(ev)) };
        let parent = c.nodes.last().map(|n| n.id);
        let eid = event_id(parent, SCHEMA, &canon(ev));
        if c.nodes.iter().all(|n| n.id != eid) {
            // rebuild the chain from the full event list so seq/parent stay consistent, then persist.
            let mut evs: Vec<WorldEvent> = c.nodes.iter().map(|n| n.ev.clone()).collect();
            evs.push(ev.clone());
            c.nodes = chain(&evs);
            Self::write_log(id, &c.nodes);
        }
        eid
    }
    fn head(&self, id: WorldId) -> Option<EventId> {
        self.worlds.get(&id).and_then(|c| c.nodes.last()).map(|n| n.id)
    }
    fn since(&self, id: WorldId, from: Option<EventId>) -> Vec<Node> {
        let Some(c) = self.worlds.get(&id) else { return Vec::new() };
        match from {
            None => c.nodes.clone(),
            Some(f) => match c.nodes.iter().position(|n| n.id == f) {
                Some(i) => c.nodes[i + 1..].to_vec(),
                None => c.nodes.clone(), // unknown cursor -> hand back all (peer re-folds)
            },
        }
    }
    fn has(&self, id: WorldId, ev: EventId) -> bool {
        self.worlds.get(&id).is_some_and(|c| c.nodes.iter().any(|n| n.id == ev))
    }

    fn put_snapshot(&mut self, id: WorldId, upto: EventId, blob: &[u8]) {
        if let Some(c) = self.worlds.get_mut(&id) {
            c.snap = Some((upto, blob.to_vec()));
            let mut bytes = upto.0.to_vec();
            bytes.extend_from_slice(blob);
            Self::write_file(&Self::world_path(id, "snap"), &bytes);
        }
    }
    fn get_snapshot(&self, id: WorldId) -> Option<(EventId, Vec<u8>)> {
        self.worlds.get(&id).and_then(|c| c.snap.clone())
    }
    fn compact(&mut self, id: WorldId, upto: EventId) {
        if let Some(c) = self.worlds.get_mut(&id) {
            if let Some(cut) = c.nodes.iter().position(|n| n.id == upto) {
                c.nodes.drain(..=cut);
                Self::write_log(id, &c.nodes);
            }
        }
    }

    fn put_asset(&mut self, bytes: &[u8]) -> AssetId {
        let id = asset_id(bytes);
        Self::write_file(&Self::asset_path(id), bytes);
        id
    }
    fn get_asset(&self, id: AssetId) -> Option<Vec<u8>> {
        Self::read_file(&Self::asset_path(id))
    }
}
