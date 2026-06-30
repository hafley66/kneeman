# Footnote: netplay as a reusable rollback crate

Goal: reuse the rollback netplay layer in other prototypes (game jams) without dragging this game's
sim along. `smash_net` is already a standalone crate (no Godot); this makes its core generic over the
game so a new prototype implements one trait and gets the whole rollback layer.

## Already true
- `smash_net` is engine-agnostic: deps are `smash_core` + `ggrs` + `serde` + `bincode`, no Godot.
- Clean seams: `Netplay` (shell-facing session model) over `Transport` (= ggrs `NonBlockingSocket`).
  Mesh p2p vs central relay are both just `NonBlockingSocket` impls.
- The rollback machinery is generic over `RollbackSim`:
  - `RollbackSim` — the game's contract: `State` (snapshot), `Input` (wire), `Config` (locked tuning),
    `initial`/`advance`/`checksum`.
  - `GgrsConfig<S>`, `Game<S>`, `GgrsNetplay<S>`, `start_p2p_n<S>` are all generic over it.
  - This game implements it as `Smash`; `SmashConfig`/`SmashGame`/`SmashNetplay` are the concrete
    aliases the shell + web crate name. The web prototype (separate crate) already reuses it this way.

## What lifting to a jam game would take
1. Move `RollbackSim` + `GgrsConfig`/`Game`/`GgrsNetplay`/`start_p2p_n` + `Netplay`/`Advance` into a
   `rollback_net` crate that does NOT depend on `smash_core`. Today they're generic but still live
   next to the Smash-specific `NetInput`/`encode`/`decode`/`checksum`/`Smash` impl in one file; split
   the file: generic core in `rollback_net`, the `Smash impl RollbackSim` stays in the game.
2. The wire `Input` (`NetInput` + bitflags `Buttons` + `encode`/`decode`) is game-specific — it stays
   with the game's `RollbackSim` impl, not in the core crate.
3. `checksum` is game-specific (folds the sim's fields). The core only needs `RollbackSim::checksum`;
   the field-folding lives in the game.
4. `synctest_session` + the `replay` module are this game's determinism harness — leave them in the
   game crate (they name `Smash`/`SimState` directly).
5. The `transport::` module (matchbox WebRTC) is reusable as-is; it only needs `SmashConfig` swapped
   for the core's generic `GgrsConfig<S>`.

Sibling notes: plans/sim-as-library.md, plans/controls-as-crate.md, plans/router-as-crate.md. Same
shape every time: a pure/generic core crate + a thin game/engine-specific adapter.
