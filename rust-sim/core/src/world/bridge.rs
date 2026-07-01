//! The ggrs → durable bridge (plans/world-protocol.md §D.5). Pure.
//!
//! ggrs eats mispredictions in place; this layer only ever reads the CONFIRMED prefix and emits the
//! diff between consecutive confirmed sim states as `WorldEvent`s. Gate = "windowed out": a frame is
//! drained only once `frame <= confirmed`, so a mispredicted (later-corrected) frame can never become
//! a durable fact. rxjs: `simState$.pipe(gateByConfirmed(confirmed$), pairwise(), map(diff))`.

use crate::world::{PlayerId, WorldEvent};
use crate::{SegClass, Vector2};
use std::collections::BTreeMap;

pub type Handle = u32;

#[derive(Copy, Clone, PartialEq, Debug)]
pub struct PlatParams {
    pub at: Vector2,
    pub len: f32,
}

/// A frame's durable-relevant sim state: the built platforms present, keyed by a sim handle.
pub type SimGeo = BTreeMap<Handle, PlatParams>;

pub struct Bridge {
    last_drained: i64, // highest frame already flushed; -1 = nothing yet
    base: SimGeo,      // confirmed sim geometry as of last_drained (the `pairwise` left element)
}

impl Default for Bridge {
    fn default() -> Self {
        Bridge { last_drained: -1, base: SimGeo::new() }
    }
}

impl Bridge {
    pub fn new() -> Self {
        Self::default()
    }

    /// Drain every frame that is confirmed (`<= confirmed`) and not yet drained, emitting the diff vs
    /// the last confirmed state. Idempotent for a fixed `confirmed`. `frames` must be sorted by frame.
    ///
    /// MVP emits additions as `PlacePlatform` (erase/revert are proven in fold.rs); the gating logic
    /// is the point.
    pub fn advance(&mut self, frames: &[(i64, SimGeo)], confirmed: i64) -> Vec<WorldEvent> {
        let mut out = Vec::new();
        for (f, geo) in frames {
            if *f <= confirmed && *f > self.last_drained {
                for (h, p) in geo {
                    if !self.base.contains_key(h) {
                        out.push(WorldEvent::PlacePlatform {
                            at: p.at,
                            len: p.len,
                            class: SegClass::Floor,
                            stroke: 0,
                            // MVP: sim-born geometry is attributed to WORLD; real per-player owner
                            // gets threaded from the placing handle when the shell wires stage 1.
                            owner: PlayerId::WORLD,
                        });
                    }
                }
                self.base = geo.clone();
                self.last_drained = *f;
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geo(handles: &[(Handle, f32)]) -> SimGeo {
        handles.iter().map(|(h, x)| (*h, PlatParams { at: Vector2::new(*x, 0.0), len: 32.0 })).collect()
    }

    #[test]
    fn gate_blocks_an_unconfirmed_frame() {
        let mut b = Bridge::new();
        // frame 3 speculatively places platform A, but confirmed is only 2 -> nothing durable.
        let frames = vec![(1, geo(&[])), (2, geo(&[])), (3, geo(&[(1, 10.0)]))];
        let out = b.advance(&frames, 2);
        assert!(out.is_empty()); // A is a guess, not a fact
    }

    #[test]
    fn misprediction_corrected_before_confirming_never_persists() {
        let mut b = Bridge::new();
        // speculative history had A at frame 3; confirmed still 2 -> emit nothing.
        assert!(b.advance(&[(3, geo(&[(1, 10.0)]))], 2).is_empty());
        // rollback corrects frame 3 to have NO platform; now it confirms -> still nothing.
        let out = b.advance(&[(3, geo(&[]))], 3);
        assert!(out.is_empty()); // the mispredicted A never became durable
    }

    #[test]
    fn confirmed_additions_emit_once_and_are_idempotent() {
        let mut b = Bridge::new();
        let frames = vec![(1, geo(&[])), (2, geo(&[(1, 10.0)])), (3, geo(&[(1, 10.0), (2, 20.0)]))];
        let out = b.advance(&frames, 3);
        assert_eq!(out.len(), 2); // A at f2, B at f3
        // re-draining the same confirmed frame yields nothing (already windowed out).
        assert!(b.advance(&frames, 3).is_empty());
    }
}
