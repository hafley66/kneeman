# Terraria camera mode + minimap

Target: a second camera MODE that follows the local player at a fixed zoom (Terraria/Metroidvania
feel), toggleable against the current melee/PM shared-fit camera, plus a minimap overlay that shows
the whole stage + fighter positions. Both are shell-only, render-time, ZERO sim state — so no
determinism/rollback exposure.

## Already true
- **The shared camera exists.** `update_camera` (`kneeman.rs:1562`) already: builds a bounding box
  over every live fighter, centers on the midpoint, computes an inverse-zoom `fit` that frames the
  pack (`kneeman.rs:1591`), leans toward the local player in portrait, and eases (`lerp`) toward the
  target so it glides. A follow-local mode is a variant of the SAME function, not a rewrite.
- **World→screen mapping exists.** `kneeman.rs:1938` maps world feet → screen pixels via the live
  camera transform (`(world - cam_c) * zoom + view*0.5`). The minimap is the same math at a fixed mini
  scale into a corner rect.
- **Stage is (or will be) strokes.** After plans/stage-strokes-migration.md the whole stage is
  `paths`; the minimap draws stage + ink in one pass. Before the migration the minimap can draw
  `PLATFORMS` + `paths` in two passes (works today, just less unified).

## Camera mode: what it takes
1. **`kneeman.rs`: a `CameraMode` enum** `{ SharedFit, FollowLocal }` held on `KneeMan` (a plain field,
   render-only — NOT in `SimState`). Default `SharedFit` (today's behavior).
2. **Branch `update_camera`**: `SharedFit` = current code unchanged. `FollowLocal` = center on the
   local player's feet only, set a FIXED zoom (a tune/const, not fit-derived), keep the same `lerp`
   ease and the loose stage clamp. Reuse the existing local-player-index the portrait lean already
   resolves.
3. **Toggle**: a menu control (Feel or a new "Camera" row) or a debug-panel toggle. Since it is pure
   shell state, it can also be a hotkey. No intent/router change needed if it lives on the debug panel
   like the gizmo toggles; a menu toggle would add one `Intent` + a cell (follow the existing
   `Intent`/cell pattern, do not write cells from screens).
4. **Clamp to blast/stage bounds** so follow mode does not scroll into the void — reuse `BLAST_*`
   (`stage.rs:21`) as the pan limit rect.

## Minimap: what it takes
1. **A corner overlay** — a `Control`/`CanvasLayer` node drawn on top, OR an egui panel. Given the menu
   is egui and the HUD is Godot nodes, match the HUD (Godot `_draw`) so it renders in-match without the
   menu open. A small fixed rect (e.g. top-right, ~200×120).
2. **Fixed world→map transform**: map the stage bounds rect (`BLAST_*` or the tighter stage extents)
   into the mini rect. Draw: stage strokes as thin lines (iterate `paths`; pre-migration also
   `PLATFORMS` as rects), each live fighter as a dot colored by player index, the local player
   highlighted. Reuse the world→screen shape from `kneeman.rs:1938` with the mini transform.
3. **"SVG is fine" (the user's note)**: the minimap does not need pixel-fidelity — a vector line
   drawing (strokes + dots) is the whole ask. `_draw` line/circle calls, or bake an SVG string if a
   crisp scalable asset is wanted. Vector `_draw` is the least plumbing.
4. **Toggle + placement**: default on in follow-local mode (you can't see the whole stage, so you want
   the map), optional in shared-fit (you already see everything). Tie its visibility to `CameraMode`.

## Why this is safe to do anytime
Neither touches `SimState`, `step`, or the checksum — both read the sim snapshot and the camera and
paint. They can land before OR after the stage-strokes migration; the migration only makes the minimap
stage-draw a single unified loop instead of two. Do the camera mode first (self-contained, ~one
function branch + a toggle), minimap second.

Sibling notes: plans/stage-strokes-migration.md (makes the minimap stage-draw one pass),
plans/gif-background-library.md (the other shell render layer behind the stage).
