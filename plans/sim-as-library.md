# Footnote: smash_core as a standalone rollback-sim library

`rust-sim/core/` (`smash_core`) is already a separate crate and already pure: its only deps are
`glam` + `serde`, with no godot, no I/O, no clock, no threads (the shell converts to `godot::Vector2`
at the render boundary; `lib.rs:2`). The whole sim is a deterministic `step(state, inputs) -> state`
over Copy/serde state — the exact shape ggrs/rollback wants, and the exact shape a reusable
platform-fighter sim library wants. So "make it a library someday" is mostly packaging, not surgery.

## Already true (the hard part)
- Pure reducer core: `step` has no side effects; same inputs -> same state on every peer.
- Rollback-ready: `SimState` is Copy + `Serialize`/`Deserialize`; `Tune` (live config) serializes too.
- Deterministic RNG: integer LCG (`item.rs`), no float-order or wall-clock dependence.
- No engine ties: nothing imports godot; Vector2 is `glam::Vec2` re-exported.

## What publishing would take (someday, for some poor soul)
1. Pin the public API: decide what's `pub` vs `pub(crate)`; today `pub use item::*` / `stage::*` /
   `moves::*` flatten everything. A library wants a curated surface (`prelude`, hidden internals).
2. Feature-gate `serde` (`features = ["rollback"]`) so a non-rollback consumer skips the derives.
3. Abstract the vector type, or document `glam` as the contract. Re-exporting `Vector2` is fine; just
   make it intentional, not incidental.
4. Doc the determinism contract loudly: no `f32` NaN paths, no iteration-order reliance, fixed `DT`.
   This is the promise a rollback library lives or dies on.
5. Decouple the `Tune` "live slider" assumption from the sim: a library consumer supplies attributes;
   the egui editing lives in the shell (it already does).

Sibling note: the menu router has the same "extract to a pure crate" trajectory -- see
plans/router-as-crate.md. Two pure reducers (sim + nav), one impure shell stitching them to Godot/egui.
