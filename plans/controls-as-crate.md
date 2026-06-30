# Footnote: controls as a reusable pure core

Goal: reuse the controls layer in other prototypes (game jams) without dragging Godot or this game's
specifics along. Same pure/impure split as the sim and the nav router.

## Already true
- `controls/pad.rs` is PURE: no Godot, no I/O. `RawPad` (device-agnostic raw snapshot) + `PadMemory`
  (cross-frame tap-jump edge) -> `InputFrame` via `PadMemory::frame`. Unit-tested without a device.
- `controls/mod.rs` is the IMPURE adapter: it reads Godot devices (keyboard actions, pad axes/buttons,
  touch stick), merges them per player, and hands a `RawPad` to the pure core. The lockdown invariant
  (raw-device names live in one file) holds; `pad.rs` has zero Godot.

## What lifting to a crate would take
1. The only game-specific type the pure core names is `InputFrame` (the sim's input contract). Either
   (a) move `InputFrame` into a tiny shared `input` crate both the sim and a jam game depend on, or
   (b) make `pad` generic: `PadMemory::frame<F: From<RawPad>>() -> F` and let each game define the
   mapping. (a) is simpler for jams; (b) is purer.
2. `RawPad` + `PadMemory` move verbatim into `controls_core` (deps: none, or just the input crate).
3. The impure adapter stays per-engine: a Godot version (this file) and, for a jam on another engine,
   a thin equivalent that fills `RawPad`. The pure core is the shared part.
4. Keep the action universe (`GameAction` + `names`) in the impure adapter — it's binding config, not
   pure logic.

Sibling notes: plans/sim-as-library.md, plans/router-as-crate.md, plans/netplay-as-crate.md. The
pattern repeats: a pure reducer/core crate + a thin impure Godot shell.
