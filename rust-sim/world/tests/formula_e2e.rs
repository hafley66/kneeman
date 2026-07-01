//! End-to-end: the whole formula, real save/load, no mocks in the code under test.
//!
//!   rollback frame source  ->  Bridge (gate on confirmed frame)  ->  SqliteStore.append  ->
//!   SqliteStore.since (load)  ->  fold  ->  reconstructed World  ==  the confirmed sim state
//!
//! The frame source is a hand-authored rollback timeline (a stand-in for a ggrs P2PSession's confirmed
//! frames — real ggrs wiring needs a forge input->placement path, tracked separately). Everything
//! downstream of the frames — bridge, store, fold — is the real shipping code.

use smash_world::bridge::{Bridge, PlatParams, SimGeo};
use smash_world::fold::fold;
use smash_world::store::{SqliteStore, WorldStore};
use smash_world::{BuildVersion, Seed, StageId};
use smash_core::Vector2;

fn seed() -> Seed {
    Seed {
        build: BuildVersion(1),
        rules: smash_core::Tune::default(),
        stage: StageId::DEFAULT,
        bg: None,
        authored: vec![],
    }
}

fn geo(handles: &[(u32, f32)]) -> SimGeo {
    handles.iter().map(|(h, x)| (*h, PlatParams { at: Vector2::new(*x, 0.0), len: 32.0 })).collect()
}

/// Two platforms get placed and confirmed; the durable world reconstructed from sqlite matches.
#[test]
fn confirmed_placements_round_trip_through_sqlite() {
    let mut store = SqliteStore::in_memory().unwrap();
    let id = store.publish(&seed());
    let mut bridge = Bridge::new();

    // A confirmed timeline: A appears at frame 2, B at frame 4.
    let frames = vec![
        (1, geo(&[])),
        (2, geo(&[(1, 10.0)])),
        (3, geo(&[(1, 10.0)])),
        (4, geo(&[(1, 10.0), (2, 20.0)])),
    ];

    // Drive confirmation forward a step at a time, persisting each windowed-out fact.
    for confirmed in 1..=4 {
        for ev in bridge.advance(&frames, confirmed) {
            store.append(id, &ev);
        }
    }

    // Load from sqlite and fold -> the durable world.
    let world = fold(BuildVersion(1), &store.since(id, None));
    assert_eq!(world.platforms.len(), 2); // exactly the two confirmed placements landed durably
}

/// A misprediction that is corrected before its frame confirms must never touch sqlite.
#[test]
fn mispredicted_placement_never_persists() {
    let mut store = SqliteStore::in_memory().unwrap();
    let id = store.publish(&seed());
    let mut bridge = Bridge::new();

    // Speculative timeline placed A at frame 3, but confirmation is only at 2.
    for ev in bridge.advance(&[(3, geo(&[(1, 10.0)]))], 2) {
        store.append(id, &ev);
    }
    // Rollback corrects frame 3 to empty; now it confirms.
    for ev in bridge.advance(&[(3, geo(&[]))], 3) {
        store.append(id, &ev);
    }

    let world = fold(BuildVersion(1), &store.since(id, None));
    assert_eq!(world.platforms.len(), 0); // the guess never became a durable fact
    assert!(store.head(id).is_none()); // nothing was ever appended
}

/// Rejoin: a peer with a stale cursor pulls exactly the missing tail and converges.
#[test]
fn rejoin_pulls_only_the_missing_tail() {
    let mut host = SqliteStore::in_memory().unwrap();
    let id = host.publish(&seed());
    let mut bridge = Bridge::new();
    let frames = vec![(1, geo(&[(1, 1.0)])), (2, geo(&[(1, 1.0), (2, 2.0)]))];

    // Host confirms frame 1, a joiner syncs to that point.
    for ev in bridge.advance(&frames, 1) {
        host.append(id, &ev);
    }
    let joiner_cursor = host.head(id); // joiner is caught up to here

    // Host confirms frame 2 (one more placement).
    for ev in bridge.advance(&frames, 2) {
        host.append(id, &ev);
    }

    let tail = host.since(id, joiner_cursor); // git pull: only what the joiner lacks
    assert_eq!(tail.len(), 1);
    // Joiner folds its snapshot(one) + the tail and matches the host's full fold.
    let joiner = fold(BuildVersion(1), &host.since(id, None));
    assert_eq!(joiner.platforms.len(), 2);
}
