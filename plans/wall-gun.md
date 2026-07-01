# Wall gun: an item that reshapes stage strokes

Target: "start as Smash, but with a gun that pushes a wall bigger." A held item whose fire does not
spawn a projectile — it MUTATES a nearby stage stroke's `Wall` segment (extends / raises / thickens
it). The first weapon that edits terrain instead of hitting a body; the seed of a build-vs-destroy
loop on top of the fighter.

## Hard dependency
Requires **plans/stage-strokes-migration.md** shipped first. Today the stage walls are the const
segments `main_walls()` (`stage.rs:62`) reflected off hard-coded `FLOOR_LEFT/RIGHT` in `za_warudo`.
There is nothing mutable to push. After the migration a wall is an `InkPath` with an `owner < 0`
`SegClass::Wall` segment — a real, editable data structure. Do NOT start this before the walls are
strokes.

## Already true
- Guns are a solved item shape: `ItemKind::{LaserGun, BobGun}` (`item.rs:97`), `is_gun` (`item.rs:115`),
  fire on the attack button routed through `fire_gun` (`item.rs:342`), ammo/gas metered by `Item.gas`
  (`item.rs:141`). A wall gun reuses the gun scaffold; only the fire EFFECT differs.
- `Item` already carries `stroke: StrokeId` (`item.rs:146`) — the pen uses it to pick which registry
  preset to stamp. The wall gun can reuse it to pick which material the pushed wall becomes.
- `InkPath::push` / `trim_front` (`stage.rs:266`) already grow/shrink a polyline node-by-node, and
  `classify` (`stage.rs`) re-derives `SegClass` after an edit. Mutating a wall = move/append a node +
  re-classify that path.

## What it takes
1. **`item.rs`: new `ItemKind::WallGun`.** Add the variant, extend the exhaustive matches: `is_gun`
   (or a new `is_terraformer`), `Item::EMPTY` is unaffected (kind lives in the literal already),
   `spawn_kind` weight arm, `MENU_ITEMS` card (`item.rs:180`) so it is spawnable from the menu + a
   number key. **Net checksum**: `kind` already folds via `f.state`/item `kind as u64` — confirm the
   item fold in `net/lib.rs` covers `kind`; no new field means no new fold line, but verify.
2. **Fire = a stroke edit, not a spawn.** Branch `fire_gun` (or a sibling `fire_wall`): on the attack
   edge, find the nearest baked `Wall` segment in `paths` within reach of the muzzle (reuse the
   nearest-surface search the pen/ledge code already does), then push its top node up by a step
   (`* t.wall_gun_step`), capped at a max height. Re-run `classify` on that path. Consumes `gas`.
3. **`tune.rs`: `wall_gun` params** — `step` (px per shot), `max_extra` (height cap), `gas`/ammo,
   `spawn_weight`. Keep it in the same `AttackData`-adjacent shape as `laser`/`bomb` for consistency.
4. **Determinism**: the edit is a pure function of (item, paths, tune) run inside `step`, so it rolls
   back with everything else. The mutated path is `owner < 0` and already in the checksum via `paths`.
   Run `just net-test`.
5. **Shell**: nothing new to render — the wall is already drawn by the stroke renderer (that is the
   point of the migration). The muzzle/aim reuses the existing gun visuals.

## Open design questions (decide at build time)
- **Which wall, and which direction?** Nearest wall + push "up" (taller) is the literal reading. A
  richer version aims: push the wall toward the stick direction, or push the wall the player is facing.
- **Permanent or decaying?** `owner < 0` never expires, so pushed walls are permanent (build up a
  fort). A decaying variant would set `owner >= 0` + a `born` so it ages out like ink — but then it is
  really just "the pen with a wall preset," which the pen (`ItemKind::Pen` + a `Wall` `StrokeProps`)
  can ALREADY do. The wall gun earns its own kind only if it edits BAKED (permanent stage) geometry.
- **Destructive twin?** A "wall breaker" that shrinks/deletes a segment is the same edit with the
  opposite sign — natural second item once the push works.

Sibling notes: plans/stage-strokes-migration.md (the hard dep), item.rs (the gun scaffold to clone),
plans/hitbox-modeling.md (how item hitboxes are modeled, if the gun ever also hits bodies).
