# Forge arena: wall jump, platform gun (Forge), growing blast zones, off-screen indicators

Batch directive (2026-07-01, pre-compaction). Five features that turn the fighter into a
build-and-fight arena. Two already have plans — this doc is the umbrella and captures the deltas:
- Platform gun extends **plans/wall-gun.md** (push-a-wall → build-a-platform / Forge).
- Camera/minimap/off-screen extends **plans/terraria-camera-minimap.md**.
- All terrain edits ride the stroke system — hard-depends on **plans/stage-strokes-migration.md**.

Split as always: sim (deterministic, rolled back, in the checksum) vs shell (render-only). Getting
each feature on the right side is the whole correctness story.

---

## 1. Wall jump  — SIM

Kick off a wall to regain a jump / redirect. New movement state, lives in the movement FSM.

- **Where:** `core/src/za_warudo.rs` (`reduce_next_state`). A wall jump is a transition triggered
  when airborne + pressed-toward a `Wall` surface within a small touch distance + jump pressed.
- **Detect the wall:** reuse the wall collision the launcher already does (today reflects off
  `FLOOR_LEFT/RIGHT`; after the strokes migration a wall is a `SegClass::Wall` segment in `paths`,
  `stage.rs:86`). Wall-jump wants the same nearest-`Wall` query the platform gun (§2) needs — build
  it once, share it.
- **State:** add a `WallCling`/`wall_jump` arm or a flag on the air state; grant an extra
  `airjump`-equivalent impulse `(wall_jump_vx away from wall, wall_jump_vy up)`. Consumes a
  wall-jump budget so you can't infinite-climb one wall (Melee-style: must touch a *different* wall,
  or a small refresh timer).
- **Tune:** `wall_jump_vx`, `wall_jump_vy`, `wall_cling_frames` (optional slide), `wall_jump_budget`
  in `core/tune.rs` `Tune`.
- **Determinism:** pure in `step`, rolls back free. `just net-test` after.
- **Open:** cling-and-slide (Metroid) vs instant kick-off (Smash)? Instant kick-off first, cling is a
  tune knob later.

---

## 2. Platform gun / Forge  — SIM

"Like the ink stuff." A held gun whose fire lays a **platform** (a real collision surface), not a
projectile. Generalizes the wall gun from "extend an existing wall" to "spawn new geometry" = a
build tool = Forge.

- **Basis already exists:** strokes ARE collision surfaces. `stage.rs:1-3` — "a drawn path and a
  stage are the SAME primitive." `Platform` (`stage.rs:35`), `InkPath` + `StrokeProps`
  (`stage.rs:96`), `SegClass{Floor,Wall,Ledge}` (`stage.rs:86`), `InkPath::push`/`classify`
  (`stage.rs:266`), `MAX_DRAWN = 6` (`stage.rs:78`). The pen already stamps surfaces with a
  `StrokeId` preset (`item.rs` `Item.stroke`).
- **Two build modes, same primitive:**
  - **Platform gun (aimed, quick):** fire → drop a short `Floor` segment at the muzzle/aim point,
    stamped `owner < 0` (permanent) or `owner >= 0` (decays like ink). Reuses the gun scaffold
    (`ItemKind`, `is_gun`, `fire_gun`, `Item.gas` ammo — see wall-gun.md §"Already true").
  - **Forge (deliberate builder):** a place/erase mode that lays platforms on a grid — the "platform
    builder" proper. Likely its own tool/route, not an item; emits the same stroke edits.
- **New `ItemKind::PlatformGun`** (+ exhaustive-match updates, `MENU_ITEMS` card) per wall-gun.md §1.
  Fire = a stroke *append*, not a spawn (wall-gun.md §2 is the same shape with a different edit).
- **Budget:** `MAX_DRAWN = 6` caps live paths — Forge needs a bigger cap or a separate baked-geometry
  pool (permanent built platforms shouldn't compete with 6 live ink strokes). Decide: raise the cap
  vs a distinct `built: Vec<InkPath>` lane with its own budget.
- **Tune:** `platform_gun{ len, gas, spawn_weight, decays }`.
- **Determinism:** edit is pure `(item, aim, paths, tune)` inside `step`; the mutated `paths` is
  already in the checksum. Erase = the destructive twin (opposite edit), per wall-gun.md.
- **Open:** grid-snap for Forge (Halo Forge) vs freehand? Permanent vs decaying default? Who can
  build — both players always, or a build/fight phase toggle (world rule → `Tune`)?

---

## 3. Blast zones grow to some % away  — SIM

Blast zones expand as the built arena grows, staying "some percent away" from the furthest surface,
so Forge-built platforms don't instantly clip into a KO edge.

- **Today:** blast zones are consts `BLAST_*` (`stage.rs:21`), KO test `crossed_blast`
  (`stage.rs:26`). Static.
- **Change:** derive the blast rect each tick from the bounding box of all live surfaces
  (`PLATFORMS` + `paths`/built) expanded by `blast_margin_pct` (e.g. +30%). So blast zones = f(built
  geometry), not a const.
- **This is SIM** — KO detection is authoritative + rolled back. The derived rect must be a pure
  function of state (surfaces) + tune (margin), computed identically on both peers. Do NOT let the
  camera (shell) feed it. `crossed_blast` reads the derived rect instead of the const.
- **Tune:** `blast_margin_pct`, plus min/max clamps so a tiny stage still has a fair kill distance
  and a huge fort can still be edged out.
- **Open:** grow-only (never shrink, so removing platforms doesn't suddenly KO someone) vs
  symmetric? Grow-only with a slow shrink lerp is safest. Per-edge vs uniform.

---

## 4. Camera: mostly static, minimap kicks in sooner  — SHELL

Refines terraria-camera-minimap.md. The user's read: the camera basically shouldn't move much;
once it's zoomed out *too far* (arena/players spread wide), switch to the minimap effect **sooner**
than a naive shared-fit would.

- **Where:** `update_camera` (`kneeman.rs:1785`) already fits the pack with an inverse zoom + lerp
  ease; `cam` field `kneeman.rs:266`. Render-only, no `SimState`.
- **Change:** add a zoom-out threshold. When the fit-zoom needed to frame everyone drops below
  `minimap_zoom_threshold` (camera too far out → fighters become specks), stop zooming out and
  instead **show the minimap + off-screen indicators (§5)**. I.e. clamp max zoom-out and let players
  leave the frame, tracked by the edge indicators, rather than shrinking everyone to dots.
- **Minimap** per terraria-camera-minimap.md (corner overlay, fixed world→map transform, strokes +
  fighter dots). Tie its auto-show to the same threshold instead of (or in addition to) the
  FollowLocal mode toggle.
- **Open:** hard clamp (camera never zooms past X, always rely on indicators) vs soft (zoom out to X,
  then indicators). Start with the soft clamp + threshold.

---

## 5. Off-screen player indicator  — SHELL

Represent an off-screen fighter as a cute square with their **number + color**, pinned to the screen
edge, with **pixel numbering + an arrow** pointing toward them. (Smash off-screen bubble, pixel-art
styled.)

- **Where:** shell HUD draw (Godot `_draw` on the KneeMan node or a HUD `CanvasLayer`), reads the sim
  snapshot + camera transform. Zero sim state. Reuses the world→screen mapping cited in
  terraria-camera-minimap.md (`(world - cam_c) * zoom + view*0.5`).
- **Logic:** for each fighter, if its screen pos is outside the viewport rect, clamp the pos to the
  rect edge; draw a small square filled with the player color (`slot_color`, `identity.rs:26`), the
  player number in pixel font, and an arrow from the square toward the true (clamped-out) direction.
  Local player uses their `Identity` color; others use `slot_color(idx)`.
- **Distance cue (optional):** shrink the square with off-screen distance so far-away players read as
  smaller — pairs with §4 (the further the spread, the more this carries the read).
- **Ties to §4:** these indicators are what let the camera stop zooming out. They must be on before
  the zoom clamp bites, or players vanish with no tracker.
- **Open:** show for all players or only off-screen ones (only off-screen). Arrow vs also a thin
  tether line. Pixel-font source (reuse the nametag font at a small px).

---

## Build order

1. **stage-strokes-migration.md** — hard dep for §1 (wall query), §2 (build), §3 (surface bbox).
2. **§5 off-screen indicators** — pure shell, self-contained, no dep; ship anytime, needed by §4.
3. **§4 camera threshold + minimap** — pure shell; needs §5 to be safe.
4. **§1 wall jump** — sim, small; after migration so the wall query is real.
5. **§2 platform gun / Forge** — sim, biggest; the build loop.
6. **§3 growing blast zones** — sim; after §2 so there's built geometry to grow around.

Sim features (§1 §2 §3) each end with `just net-test` (determinism/rollback). Shell features (§4 §5)
touch neither `step` nor the checksum. Sibling: plans/wall-gun.md, plans/terraria-camera-minimap.md,
plans/stage-strokes-migration.md, plans/hitbox-modeling.md.
