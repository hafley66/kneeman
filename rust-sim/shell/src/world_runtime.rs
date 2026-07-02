//! Shell adapter for the durable world (`smash_core::world`). Owns the sqlite store plus the current
//! world id/head, so the rest of the shell says "load my home", "set the background", "what does the
//! world look like" without ever touching event ids. This is the seam the sim writes durable facts
//! through (Stage 1). Save/load is just: `boot` (open + load-or-create) and `world` (fold the log).

use smash_core::world::fold::{fold, World};
use smash_core::world::store::{Slot, WorldStore};
use smash_core::world::{AssetId, BuildVersion, EventId, PlayerId, Seed, Seq, StageId, WorldEvent, WorldId};
use smash_core::Tune;

/// Genesis build of a home world. Bumping it is a namespace split (new `WorldId`), so it is pinned.
const HOME_BUILD: BuildVersion = BuildVersion(1);

/// Auto-save cadence: snapshot the current head every minute of session time.
pub const AUTOSAVE_SECS: f32 = 60.0;
/// Auto-save labels share this prefix; only these get pruned (manual saves are never evicted).
const AUTO_PREFIX: &str = "auto ";
/// Keep at most this many auto-saves (a ring); older ones are dropped.
const MAX_AUTOSAVES: usize = 10;
/// Soft cache cap. Crossing it (mostly gif blobs) raises a warn toast; nothing is deleted.
pub const CACHE_WARN_BYTES: u64 = 32 * 1024 * 1024;

/// The genesis marker for a reset: `Reset { to }` with a target not in the log folds back to genesis
/// (fold.rs), and an all-zero id is the None-parent sentinel — never a real event id, so always "unknown".
const GENESIS: EventId = EventId([0u8; 32]);

/// Generic over the storage backend so the client mounts `GodotStore` (user:// = disk/IndexedDB) and
/// the server mounts `SqliteStore`, with everything above the store unchanged.
pub struct WorldRuntime<S: WorldStore> {
    store: S,
    current: WorldId, // the loaded world (a player's home, for now)
    head: Option<EventId>,
    owner: PlayerId,
    auto_accum: f32, // seconds of session time toward the next auto-save
}

impl<S: WorldStore> WorldRuntime<S> {
    /// Load-or-create the caller's home world on the given backend. `owner` is the player's persistent
    /// key (KneeMan supplies + persists it; it distinguishes one home from another). Idempotent: same
    /// owner -> same home, re-attached.
    pub fn boot(mut store: S, owner: PlayerId) -> Self {
        // publish is idempotent (re-attach on disk / INSERT OR IGNORE): create once, re-open after.
        let current = store.publish(&home_seed(owner));
        let head = store.head(current);
        WorldRuntime { store, current, head, owner, auto_accum: 0.0 }
    }

    pub fn owner(&self) -> PlayerId {
        self.owner
    }
    pub fn world_id(&self) -> WorldId {
        self.current
    }
    pub fn head(&self) -> Option<EventId> {
        self.head
    }

    /// Fold the current world's whole log into live state (save/load: this IS load).
    pub fn world(&self) -> World {
        fold(HOME_BUILD, &self.store.since(self.current, None))
    }

    /// Append one event to the current world, advancing head. The single durable-write path — the
    /// bridge (confirmed geometry) and presence both funnel here.
    pub fn append(&mut self, ev: &WorldEvent) -> EventId {
        let id = self.store.append(self.current, ev);
        self.head = Some(id);
        id
    }

    /// Owner sets/changes the gif background: stash the bytes as a content-addressed asset (blob out of
    /// the event log), then record the pointer as a folded `SetBackground`. Returns the `AssetId`.
    pub fn set_background(&mut self, gif_bytes: &[u8]) -> AssetId {
        let asset = self.store.put_asset(gif_bytes);
        self.append(&WorldEvent::SetBackground { bg: Some(asset) });
        asset
    }

    /// Clear back to the stage's own backdrop (another folded event, not a delete).
    pub fn clear_background(&mut self) {
        self.append(&WorldEvent::SetBackground { bg: None });
    }

    /// Background pixels for the current world (fold -> `AssetId` -> blob). `None` = stage default.
    pub fn background_bytes(&self) -> Option<Vec<u8>> {
        self.world().bg.and_then(|a| self.store.get_asset(a))
    }

    // --- save slots (bookmarks) + reset. Restore reuses the frozen `Reset` event; nothing is deleted ---

    /// All saves for the current world, oldest first.
    pub fn slots(&self) -> Vec<Slot> {
        self.store.slots(self.current)
    }

    /// Rough stored footprint of the current world, for the size watch.
    pub fn cache_bytes(&self) -> u64 {
        self.store.cache_bytes(self.current)
    }

    /// Bookmark the current head under `label` (re-saving a label overwrites it). `at_ms` = caller's
    /// wall clock (the store has no clock). An empty log bookmarks genesis, so a restore still works.
    pub fn save_slot(&mut self, label: &str, at_ms: u64) -> Slot {
        let head = self.head.unwrap_or(GENESIS);
        let seq = Seq(self.store.since(self.current, None).len() as u64);
        let slot = Slot { label: label.to_string(), at_ms, head, seq };
        self.store.put_slot(self.current, &slot);
        slot
    }

    /// Restore a saved point: append `Reset { to: head }` (fold jumps state back, history is kept).
    pub fn load_slot(&mut self, label: &str) {
        if let Some(s) = self.slots().into_iter().find(|s| s.label == label) {
            self.append(&WorldEvent::Reset { to: s.head });
        }
    }

    /// Forget a save (the log point it referenced stays in the log).
    pub fn delete_slot(&mut self, label: &str) {
        self.store.del_slot(self.current, label);
    }

    /// Reset home to the blank static defaults: `Reset { to: GENESIS }` clears the fold to genesis.
    /// Undo-able (it is a forward event) and it syncs like any other edit.
    pub fn reset_home(&mut self) {
        self.append(&WorldEvent::Reset { to: GENESIS });
    }

    /// Tick the auto-save clock by `dt` seconds; every `AUTOSAVE_SECS` it saves an `auto <ms>` slot,
    /// prunes the auto ring to `MAX_AUTOSAVES`, and returns the new cache size (so the caller can warn
    /// past the cap). Returns `None` on the frames that do not save.
    pub fn autosave(&mut self, dt: f32, now_ms: u64) -> Option<u64> {
        self.auto_accum += dt;
        if self.auto_accum < AUTOSAVE_SECS {
            return None;
        }
        self.auto_accum = 0.0;
        self.save_slot(&format!("{AUTO_PREFIX}{now_ms}"), now_ms);
        self.prune_autos();
        Some(self.cache_bytes())
    }

    /// Drop the oldest `auto ` saves beyond `MAX_AUTOSAVES`. Manual saves are untouched.
    fn prune_autos(&mut self) {
        let mut autos: Vec<Slot> =
            self.slots().into_iter().filter(|s| s.label.starts_with(AUTO_PREFIX)).collect();
        autos.sort_by_key(|s| s.at_ms);
        let excess = autos.len().saturating_sub(MAX_AUTOSAVES);
        for s in autos.into_iter().take(excess) {
            self.store.del_slot(self.current, &s.label);
        }
    }
}

/// A player's home world seed. Owner rides in `authored` so each home gets a distinct `WorldId`
/// (without an owner in the hash, every default home would collide on one id). The placeholder display
/// fields get overwritten by a live `PlayerJoin` carrying the real name/color once connected.
fn home_seed(owner: PlayerId) -> Seed {
    Seed {
        build: HOME_BUILD,
        rules: Tune::default(),
        stage: StageId::DEFAULT,
        bg: None,
        authored: vec![WorldEvent::PlayerJoin { player: owner, name: String::new(), color: 0, char_pick: 0 }],
    }
}
