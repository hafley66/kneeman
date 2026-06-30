# Footnote: extract the menu router into its own crate

The menu nav lives in `rust-sim/shell/src/ui/menu/router.rs`, already split into:

- **pure reducer** (`Nav`, `NavCmd`, `NavOut`, `Nav::reduce`) — no I/O, no cells, no egui. Reducer-pure
  like `smash_core::step`: a total `(state, cmd) -> state` function, returning a `NavOut::Fire(dialog)`
  when a dialog passes its two-player gate.
- **effect layer** (`Router`, `Intent`, `MenuCells`) — interprets app intents against the game cells.

## Why it can be its own crate

Routing is a string state machine of two input kinds:

- **positional** inputs = routes (the path: `Home`, `Items`, `CharEdit{slot}`).
- **unpositional** inputs = dialogs (query-param overlays, reachable from many bases).

`Nav` is that machine. Parameterized over the app's `Route`/`Dialog` types (generics or a trait), it
is a reusable router with zero game ties — pure, `Clone`, trivially unit-testable, and rollback-friendly
the same way the sim is.

## What extraction needs

1. New crate `rust-sim/router/` (workspace member), `#![no_std]`-friendly if we drop `Vec` history for
   a fixed stack. Pure: depends on nothing game-specific.
2. Make `Route` / `Dialog` type parameters: `Nav<R, D>` with `R: Copy + Eq`, `D: Copy`. The
   `CharEdit { slot }` payload rides on `R`; the `SpawnConfirm(ItemKind)` payload rides on `D`.
3. Move the two-player gate (`require_both`) in as a field; keep `NavOut::Fire(D)` as the only output.
4. Shell keeps the effect layer (`Intent` -> `NavCmd` mapping + cell writes); it depends on the new crate.

Held concrete + in-tree until a second consumer wants it. Trigger to extract: anything else needs a
pure route machine, or we want property tests over nav transitions.
