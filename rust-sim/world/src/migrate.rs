//! Schema decode + upcast (plans/world-protocol.md §2). Storage/wire holds `Envelope{schema,payload}`;
//! `decode` dispatches on `schema` and upcasts older encodings to the current `WorldEvent`, so a log
//! written by an old build stays replayable forever. "Can my binary parse this row" — separate from
//! the world's version (which is fold HEAD).

use crate::{Envelope, Owner, Schema, WorldEvent};
use serde::{Deserialize, Serialize};
use smash_core::{SegClass, Vector2};

/// A hypothetical schema-0 encoding: placement before `class`/`stroke` existed. Kept only to prove the
/// upcast path; a real old schema would be frozen the same way `WorldEvent` is.
#[derive(Serialize, Deserialize)]
enum WorldEventV0 {
    PlacePlatform { at: Vector2, len: f32, owner: Owner },
}

fn upcast_v0(old: WorldEventV0) -> WorldEvent {
    match old {
        // fill the fields added since v0 with their defaults (Floor surface, default stroke).
        WorldEventV0::PlacePlatform { at, len, owner } => {
            WorldEvent::PlacePlatform { at, len, class: SegClass::Floor, stroke: 0, owner }
        }
    }
}

/// Decode a stored envelope to the current `WorldEvent`, upcasting older schemas. Unknown (future)
/// schema = this binary is too old to read the row (handshake would have refused; panic is a bug).
pub fn decode(env: &Envelope) -> WorldEvent {
    match env.schema {
        Schema(0) => upcast_v0(bincode::deserialize(&env.payload).expect("v0 decode")),
        Schema(1) => bincode::deserialize(&env.payload).expect("v1 decode"),
        Schema(s) => panic!("unknown event schema {s}: binary too old (should have refused at handshake)"),
    }
}

/// Encode current `WorldEvent` at the current schema (what a fresh append writes).
pub fn encode(ev: &WorldEvent) -> Envelope {
    Envelope { schema: Schema(1), payload: crate::canon(ev) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EventId, WorldEvent};

    #[test]
    fn old_schema_upcasts_to_current() {
        // a row written by an old build (schema 0, no class/stroke fields).
        let old = WorldEventV0::PlacePlatform { at: Vector2::new(5.0, 6.0), len: 20.0, owner: Owner(2) };
        let env = Envelope { schema: Schema(0), payload: bincode::serialize(&old).unwrap() };
        let got = decode(&env);
        assert_eq!(
            got,
            WorldEvent::PlacePlatform { at: Vector2::new(5.0, 6.0), len: 20.0, class: SegClass::Floor, stroke: 0, owner: Owner(2) }
        ); // added fields filled with defaults -> old log still folds
    }

    #[test]
    fn current_schema_roundtrips() {
        let ev = WorldEvent::ErasePlatform { placed: EventId([7u8; 32]) };
        assert_eq!(decode(&encode(&ev)), ev);
    }
}
