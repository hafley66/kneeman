# Ink billiards + growing blast zone

Canonical design for the collision-game ink subsystem (refined 2026-07-01 from
chat_log/20260701.5; supersedes the two earlier drafts that lived in this file).
Goal: ink as knockback-able world mass you fling, lock, and prune via the blast
zone. ONE entity — the existing `InkPath` polyline grows rigid-body state. No
second `Ink` array, no rotation, no state enum.

## The mechanic in one paragraph

Drawn/fired ink is a polyline with mass and HP. Hit it and it flies (Traveling);
traveling ink is billiard — elastic vs other traveling ink, momentum by mass.
Tetris-gun ink arcs, then LOCKS on contact with ground/still ink (merges into the
cluster). The blast zone is the AABB of all still player ink; it grows as you place
outward. Still ink that settles outside the zone dies. The eraser laser is the
un-lock: it flings still ink back into Traveling with big kb — knock enemy ink out
of the zone and it dies on settle. The zone is the weapon.

## Decisions already made (do not relitigate)

- ONE entity: `InkPath` gains body state. The hurtbox IS the polyline (segments as capsules).
- Rotation DROPPED (no angle/ang_vel/inertia): billiard is translation-only, tetris
  pieces axis-aligned; visual spin can be render-only later. Saves 3 fields + torque math.
- `InkState` enum DROPPED: `mass > 0.0 && vel != Vector2::ZERO` = Traveling, else Still.
- `mass == 0.0` = baked-stage sentinel (never flies, never takes kb, exempt from prune).
- Free fns, NOT traits: `resolve_hit_fighter` (extracted from `resolve_combat`) +
  `resolve_hit_ink` share `knockback_units`. `CombatTarget` trait only if a 3rd target appears.
- Blast zone reading A (grow): zone = AABB of Still mass>0 ink; prune what settles outside.

## 1. Type signatures

```rust
// stage.rs — InkPath gains exactly 4 fields:
pub struct InkPath {
    pub pts: [Vector2; MAX_PATH_PTS],   // CHANGED MEANING: local offsets from `pos` (was world)
    pub born: [u64; MAX_PATH_PTS],
    pub class: [SegClass; MAX_PATH_PTS],
    pub len: u8,
    pub kind: ToolKind,
    pub props: StrokeProps,
    pub owner: i8,
    pub drawing: bool,
    pub budget: f32,
    // ── new: rigid body (translation only) ──
    pub pos: Vector2,    // centroid, world space
    pub vel: Vector2,    // px/s; ZERO = Still, nonzero = Traveling
    pub hp: f32,         // damage %, scales kb taken (fighters' formula)
    pub mass: f32,       // Σ|seg| × density at finalize; 0.0 = baked stage, not a body
}

impl InkPath {
    pub fn world_pt(&self, i: usize) -> Vector2;          // pts[i] + pos — the ONLY world read
    pub fn world_seg(&self, i: usize) -> (Vector2, Vector2);
    pub fn bound_circle(&self) -> (Vector2, f32);          // pos, max|pts[i]| + props.radius
    pub fn traveling(&self) -> bool;                       // mass > 0 && vel != ZERO
}

// lib.rs / a new ink module:
pub fn resolve_hit_fighter(...);                            // extracted resolve_combat inner block
pub fn resolve_hit_ink(atk: &Fighter, hb: &Hitbox, ink: &mut InkPath, t: &Tune);
pub fn integrate_ink(p: &mut InkPath, stage: &Stage, t: &Tune);   // Traveling only
pub fn blast_zone(paths: &[InkPath; MAX_DRAWN]) -> Rect;    // AABB of Still, mass>0
pub fn prune_outside(s: &mut SimState);
pub fn resolve_ink_billiard(paths: &mut [InkPath; MAX_DRAWN]);    // Traveling↔Traveling
```

## 2. Pseudo-code

```rust
// resolve_hit_ink: the un-lock primitive (translation only, no torque)
//   contact P = closest point on hit segment to hitbox center (circle-vs-seg in geo.rs)
//   units = knockback_units(ink.hp + hb.damage, hb.damage, mass_as_weight(ink.mass), hb)
//   ink.hp += hb.damage
//   ink.vel = kb_dir(hb.angle, atk.facing) * units * t.kb_speed / ink.mass-ish scale
//   (now traveling; prune exemption is automatic — prune only looks at Still)

// integrate_ink (per Traveling path per tick):
//   vel.y += gravity*dt; vel *= air_friction; pos += vel*dt
//   if any world_seg crosses ground/platform/still-ink capsule:
//       vel = ZERO                      // settle = the lock; Still IS the cluster
//       recompute class                 // floor/wall/ledge classification
//
// resolve_ink_billiard (coarse, O(N²), N=6):
//   for each Traveling pair: bound_circle overlap → elastic 1D exchange along centers, by mass
//
// prune_outside (after resolve_ink each step):
//   zone = blast_zone(paths)            // empty → stage default bounds
//   Still, mass>0, owner≥0, centroid outside zone → *p = InkPath::EMPTY
```

Precision split (the sturdy part): capsule math ONLY where the contact point matters
(fighter hitbox → ink segment); bounding circles for the billiard momentum pass. No
capsule-vs-capsule GJK — that is the research-project trap.

## 3. Instance lifetimes

- **Baked stage stroke** (owner -1, mass 0): created by stage bake, immortal, never
  Traveling, never pruned, never takes kb. Behaves exactly as today.
- **Drawn ink** (owner ≥0): born via tools (drawing=true, mass 0 while authored) →
  finalize computes mass (Σ|seg|×density) and pos (centroid), pts rebased local →
  Still → possibly Traveling (hit) → Still (settle) → dies by per-node expiry (born),
  budget, or prune_outside.
- **Fired ink** (guns, steps 6-8): spawned finalized (mass>0, vel≠0, Traveling from
  birth) → settles/locks → same afterlife as drawn ink.
- Slot reuse: `SimState.paths[MAX_DRAWN=6]`, EMPTY slot = len 0; guns claim the first
  inactive slot (tetris-as-4-entities pressure on MAX_DRAWN is an open question).

## 4. Storage, reads/writes, uniqueness

Layout: no new arrays. `SimState.paths` stays `[InkPath; 6]`, `Copy`, fixed-cap;
4 new fields ride inside each element (rollback snapshots unchanged in shape).

Write order inside `step()` per tick:
1. existing input/FSM/physics passes (unchanged)
2. `resolve_combat` — fighter↔fighter (unchanged) + fighter hitbox → Still ink (`resolve_hit_ink`)
3. `integrate_ink` — Traveling paths fly/settle
4. `resolve_ink_billiard` — Traveling↔Traveling
5. `prune_outside` — Still outside zone dies
(2 writes vel; 3 consumes it; 4 mutates vel of pairs; 5 only despawns Still — no
same-tick write conflict between passes.)

Uniqueness/invariants (the pure-mode test set per step):
- world geometry invariant under the schema break: `world_pt(i)` after == `pts[i]` before
- `mass == 0.0` ⟺ owner -1 baked stroke; mass>0 ⟺ finalized player ink
- Traveling ⟹ mass > 0 (baked never flies); settle sets vel exactly ZERO
- prune never touches Traveling, mass 0, or owner -1
- checksum equality across a rollback replay (SyncTest) after every step

## Checksum (standing gate + a latent bug to fix in step 0)

`smash_net::checksum` (net/src/lib.rs:167) folds fighters and items but does NOT fold
`paths` AT ALL today — a latent desync if ink ever diverged between peers. Step 0 adds
the paths fold loop (pts up to len, born, class, len, owner, drawing, budget, + the 4
new fields' bits). `Tune` fields (density, gravity, friction) do NOT fold. Gate:
`cargo test -p smash_net` after every step.

## Implementation order (pure-mode test first, then wire into step())

0. **Schema break**: pts → local, add pos/vel/hp/mass (zeroed), helpers
   (world_pt/world_seg/bound_circle/traveling), migrate read sites — stage.rs
   260, 269, 282, 306, 327, 330, 355, 390; kneeman.rs 916, 917, 945, 947. Baked
   stage bakes pos=centroid, pts local, rest 0. ADD the checksum paths fold. SyncTest green.
1. mass computed at finalize (populated, no behavior). Test.
2. Traveling integrator + ground settle (no collisions; flies, lands, locks). Test.
3. `blast_zone()` + `prune_outside()` (two still inks, one outside → pruned). Test.
4. Fighter hitbox → Still ink kb (`resolve_hit_ink`; extract `resolve_hit_fighter` same PR).
5. Traveling↔Traveling billiard (bounding-circle elastic). Test.
6. Ink Gun ItemKind (blob, settles Still on ground). Test + render.
7. Tetris Gun (tetromino cluster, locks on contact with ground OR still ink). Test + render.
8. Eraser Laser (high-kb hit on Still ink = un-lock). Test + render.
9. Traveling ink → fighter impulse (0 damage push). Optional; gate off if it fights the FSM.

Items reuse the existing `ItemKind` + `ItemConfig` + match-dispatch pipeline
(item.rs:97/:22) — three new variants, no new plumbing.

## Open questions (parked, none block step 0)

- Ally pass-through: does your own ink bump you? (Melee platform no; Splatoon yes.)
- Tetris representation: 4 InkPath cells that lock together (billiard code free, but
  MAX_DRAWN=6 pressure) vs one path with a shape bitmap. Lean: 4 entities, grow MAX_DRAWN.
- Still ink as stand-on platform: `Fighter.ground_ink` already indexes InkPath;
  extending to settled ink is a follow-up, not a blocker.
- Blast zone viz in shell: debug rect first.
- `mass_as_weight` mapping for the kb formula (ink mass → the formula's `w`): pick a
  constant scale in step 4, tune later.
