# Stage → baked strokes migration (the keystone)

Target: make the stage geometry the SAME primitive as drawn ink. Seed the platforms/walls as
`owner < 0` `InkPath`s at `SimState` construction, then collapse the special-cased platform + wall
+ ledge collision in `za_warudo` into ONE loop over `paths`. Retire the `PLATFORMS` array and the
`FLOOR_*` / `GROUND_Y` / `STAGE_BOTTOM` consts as live collision inputs. This unlocks the wall-gun
(mutable stage walls) and the minimap ("everything is strokes → draw the strokes"), and is the
concrete step toward "start as Smash, become any 2D game" (the stage becomes editable data).

## Already true (the data model reserved for this)
- `InkPath` (`stage.rs:224`) already carries `owner: i8` where **`-1` = baked stage stroke, never
  expires, never redraws** (`stage.rs:231`). `advance_paths` already skips `owner < 0` for decay
  (`stage.rs:479`).
- `SegClass::{Floor, Wall, Ledge}` + `classify` already produce exactly the surfaces the stage needs
  (walkable top, reflecting wall, grabbable lip). `StrokeProps.solid` already distinguishes the solid
  main stage from soft (drop-through) platforms.
- The ink landing branch (`za_warudo.rs:743`) and ink wall block (`ink_wall_block`) already do
  crossed-from-above landing + wall reflection against `paths`. **The stage's collision is a strict
  subset of what ink collision already handles.**
- `SimState::spawn_n` (`lib.rs:400`) is the single construction site; `paths: [InkPath::EMPTY; MAX_DRAWN]`
  at `lib.rs:411` is the one line to seed instead.

## The one hard constraint: slot budget
`MAX_DRAWN = 6` (`stage.rs:78`) is the whole `paths` array, and its comment already says it means
"drawn ink + loaded stage strokes". Baking the stage costs slots:
- main solid stage = 1 path (top segment + 2 wall segments + 2 ledge tips, all in one polyline), and
- 3 soft platforms = 3 paths (each a single top segment).
That is **4 of 6 slots gone**, leaving 2 for player ink. Two ways out (decide before coding):
1. **Bump `MAX_DRAWN`** to e.g. 12. Cheapest code change; costs `SimState` `Copy` size + every
   rollback snapshot + the checksum fold walks more slots. `InkPath` is large (`[Vector2;24]` ×3-ish).
2. **Split arrays**: `baked: [InkPath; STAGE_STROKES]` + `drawn: [InkPath; MAX_DRAWN]`, pass both to
   `reduce_next_state`. Keeps ink budget intact, makes "which are permanent" a type-level fact, but
   touches every `paths` call site + the checksum. Preferred long-term; more surface area.

Recommendation: (1) first (smallest diff, unblocks wall-gun/minimap), migrate to (2) if snapshot size
bites. Record the choice in the net checksum note either way.

## What the migration takes
1. **`stage.rs`: a `fn stage_strokes() -> [InkPath; N]`** (or push into a `&mut SimState`) that builds
   the baked paths from the current `PLATFORMS` geometry: main stage as a closed-ish polyline
   (top-left ledge → top-right ledge → down the right wall → underside → up the left wall), soft
   platforms as 2-point top segments. Set `owner = -1`, `drawing = false`, `props.solid` per platform,
   run `classify` once so `class[]`/ledge tips are cached at load. `born = 0` (never expires).
2. **`lib.rs:411`**: seed those into `paths` (append after index 0, or fill the first N slots) in
   `spawn_n`. Every peer builds identical baked strokes from consts → still deterministic, no RNG.
3. **`za_warudo.rs`: collapse the landing/wall/ledge reads.** Today there are parallel branches:
   platform landing (`711-726`), ink landing (`743-`), stage wall block (`775-`) vs `ink_wall_block`,
   ledge grab against `FLOOR_LEFT/RIGHT` (`698-704`) vs ink ledges, and ground-follow that indexes
   `PLATFORMS[n.ground_plat]` (`813`, `840`) vs `paths[n.ground_ink]` (`824`, `883`). Migrate each to
   the `paths` version and delete the `PLATFORMS`-indexed twin. `ground_plat` folds into `ground_ink`
   (one "what am I standing on" index into `paths`); keep the field name or rename in a follow-up.
4. **The hitstun launch branch (`za_warudo.rs:54-160`)** still hard-codes `GROUND_Y`/`FLOOR_*` for the
   launched-body floor/wall (incl. the floor-bounce I just added at ~118). Point it at the baked main
   stage stroke instead, or leave `GROUND_Y` as a fast-path constant for the main floor ONLY and note
   it. Simplest first pass: keep the main-floor constant for the launch arc, migrate platforms/ink;
   full unification of the launch branch is a second slice.
5. **Retire consts**: once nothing reads `PLATFORMS`, delete it; keep `FLOOR_LEFT/RIGHT`, `GROUND_Y`,
   `STAGE_BOTTOM`, `BLAST_*` as *authoring* consts used only to BUILD the strokes + blast zones, not as
   live per-tick collision. Blast zones stay consts (they are not a surface).
6. **Net gate**: `paths` is already in the checksum fold path (verify `net/lib.rs` folds every `InkPath`
   node, or that baked strokes with `born=0` don't perturb it). Run `just net-test` — determinism must
   stay green. If `MAX_DRAWN` changed, the SyncTest byte layout changes; that is expected, tests just
   need to pass, not match old bytes.

## Sequencing (small, reversible steps)
1. Add `stage_strokes()` + seed in `spawn_n`, but DON'T delete anything yet — baked strokes coexist
   with `PLATFORMS`. Fighters now land on ink-baked platforms first (or double-land; gate carefully).
2. Flip platform landing + ledge grab to `paths`, delete the `PLATFORMS` landing twin. Gate green.
3. Flip wall block, delete the stage-wall twin. Gate green.
4. Flip ground-follow indexing, unify `ground_plat`/`ground_ink`. Gate green.
5. Delete `PLATFORMS`, demote consts to authoring-only. Gate green + eyeball in-game.

Each step is independently shippable and independently revertible.

## Unlocks
- **plans/wall-gun.md** — a stage wall is now a mutable `InkPath` Wall segment; a gun can grow it.
- **minimap** — one render pass over `paths` (baked + drawn) + fighter dots; no separate stage-draw.
- **arbitrary stages** — a stage is just a `[InkPath]` blob; author/load them like ink, not code.

Sibling notes: plans/sim-as-library.md (the pure-core split this rides on), plans/hitbox-modeling.md
(the other big core migration), plans/gif-background-library.md (the shell-side stage backdrop).
