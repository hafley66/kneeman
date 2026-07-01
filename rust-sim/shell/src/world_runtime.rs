//! Shell adapter for the durable world (`smash_core::world`). Owns the sqlite store plus the current
//! world id/head, so the rest of the shell says "load my home", "set the background", "what does the
//! world look like" without ever touching event ids. This is the seam the sim writes durable facts
//! through (Stage 1). Save/load is just: `boot` (open + load-or-create) and `world` (fold the log).

use smash_core::world::fold::{fold, World};
use smash_core::world::store::WorldStore;
use smash_core::world::{AssetId, BuildVersion, EventId, PlayerId, Seed, StageId, WorldEvent, WorldId};
use smash_core::Tune;

/// Genesis build of a home world. Bumping it is a namespace split (new `WorldId`), so it is pinned.
const HOME_BUILD: BuildVersion = BuildVersion(1);

/// Generic over the storage backend so the client mounts `GodotStore` (user:// = disk/IndexedDB) and
/// the server mounts `SqliteStore`, with everything above the store unchanged.
pub struct WorldRuntime<S: WorldStore> {
    store: S,
    current: WorldId, // the loaded world (a player's home, for now)
    head: Option<EventId>,
    owner: PlayerId,
}

impl<S: WorldStore> WorldRuntime<S> {
    /// Load-or-create the caller's home world on the given backend. `owner` is the player's persistent
    /// key (KneeMan supplies + persists it; it distinguishes one home from another). Idempotent: same
    /// owner -> same home, re-attached.
    pub fn boot(mut store: S, owner: PlayerId) -> Self {
        // publish is idempotent (re-attach on disk / INSERT OR IGNORE): create once, re-open after.
        let current = store.publish(&home_seed(owner));
        let head = store.head(current);
        WorldRuntime { store, current, head, owner }
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
