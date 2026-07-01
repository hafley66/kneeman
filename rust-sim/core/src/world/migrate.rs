//! Schema decode + upcast (plans/world-protocol.md §2). Storage/wire holds `Envelope{schema,payload}`;
//! `decode` dispatches on `schema` and upcasts older encodings to the current `WorldEvent`, so a log
//! written by an old build stays replayable forever. "Can my binary parse this row" — separate from
//! the world's version (which is fold HEAD).

use crate::world::{Envelope, PlayerId, Schema, WorldEvent};
use serde::{Deserialize, Serialize};
use crate::{SegClass, Vector2};

/// A hypothetical schema-0 encoding: placement before `class`/`stroke` existed and `owner` was a bare
/// slot byte. Kept only to prove the upcast path; a real old schema would be frozen like `WorldEvent`.
#[derive(Serialize, Deserialize)]
enum WorldEventV0 {
    PlacePlatform { at: Vector2, len: f32, owner: i8 },
}

fn upcast_v0(old: WorldEventV0) -> WorldEvent {
    match old {
        // fill fields added since v0 with defaults; the old slot byte has no PlayerId, so it maps to
        // WORLD (a real migration would resolve slots to ids from a join table, out of scope here).
        WorldEventV0::PlacePlatform { at, len, owner: _ } => {
            WorldEvent::PlacePlatform { at, len, class: SegClass::Floor, stroke: 0, owner: PlayerId::WORLD }
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
    Envelope { schema: Schema(1), payload: crate::world::canon(ev) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::{EventId, WorldEvent};

    #[test]
    fn old_schema_upcasts_to_current() {
        // a row written by an old build (schema 0, no class/stroke fields).
        let old = WorldEventV0::PlacePlatform { at: Vector2::new(5.0, 6.0), len: 20.0, owner: 2 };
        let env = Envelope { schema: Schema(0), payload: bincode::serialize(&old).unwrap() };
        let got = decode(&env);
        assert_eq!(
            got,
            WorldEvent::PlacePlatform { at: Vector2::new(5.0, 6.0), len: 20.0, class: SegClass::Floor, stroke: 0, owner: PlayerId::WORLD }
        ); // added fields filled with defaults, old slot -> WORLD -> old log still folds
    }

    #[test]
    fn current_schema_roundtrips() {
        let ev = WorldEvent::ErasePlatform { placed: EventId([7u8; 32]) };
        assert_eq!(decode(&encode(&ev)), ev);
    }
}
