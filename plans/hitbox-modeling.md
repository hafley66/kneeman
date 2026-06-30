# Hitbox / hurtbox modeling: plan + progress

Target: replace the single-circle hitbox model with a Brawl-shaped one — multi-hitbox per move
(id priority), swept (capsule) overlap so fast moves don't tunnel, capsule hurtboxes, and real
clank/rebound with a transcendent-priority opt-out. Must stay rollback-safe.

## Where we are today
`AttackData` (`core/src/moves/mod.rs:34`) is ONE circle `(off, r)` live for
`[startup, startup + active_span)`, with two TIME windows (early/late "sex kick", same shape).
`hurtbox` (`moves/mod.rs:177`) is ONE pose-dependent circle. Overlap is `dist ≤ hr+br`
(`lib.rs:1612`). Combat is an `n^2` ordered-pair loop, hitbox-vs-hurtbox only (`lib.rs:699`).

Consequences:
- **Every hitbox is effectively transcendent** — nothing ever clanks; simultaneous attacks just
  both land (trade by default).
- **Point-in-time** overlap — a hitbox that moves > its radius between frames tunnels through a
  target.
- **One disjoint circle per move** — no sweetspot/sourspot in space (only in time).
- **One hit per swing, lowest victim wins** — `attack_hit: bool` (`Fighter`) + `if a.attack_hit
  return` (`lib.rs:1605`). A wide hitbox over two opponents hits only the lower-indexed one.

## Brawl reference (the model we're aiming at)
| Term | Brawl meaning |
|---|---|
| Hitbox | sphere on a bone (offset + radius) with damage/angle/BKB/KBG + flags |
| Interpolation | sphere swept from last frame's pos to this frame's → a capsule (anti-tunnel) |
| Hitbox ID priority | a move has hitboxes id 0..N; against one target the **lowest overlapping id wins** |
| Clank / rebound | two opposing hitboxes overlap → compare damage; within a window both rebound (no dmg), else stronger cancels weaker |
| Transcendent priority | per-hitbox flag that **skips the clank check** — passes through other hitboxes (projectiles; aerials are de-facto transcendent) |

## Rollback constraints (non-negotiable)
`SimState` is `Copy` + serde, snapshotted every frame. So: fixed-size arrays only (`[Hitbox; 4]` +
`u8` count, never `Vec`), every new field `Copy`, all math f32-deterministic. `Tune` keeps editable
copies of the data so the debug panel still works.

## Type signatures (the target shapes)
The unit is a hitbox that owns its OWN frame window (`start`/`len` relative to state start). A move
is N of them on one shared `f.frame` clock — that is what makes multi-hit + sequenced-timing moves
(the Knee Man stomp) authorable. This SUBSUMES `HitLate`/`hit_at`/`active_span`/the move-global
`active`: the early/late "sex kick" is just two boxes with different windows + same shape.
```rust
const MAX_HB: usize = 4; // hitboxes per move (Brawl-ish)

#[derive(Copy, Clone, PartialEq)]
pub struct Hitbox {
    pub id: u8,            // priority; lowest overlapping id wins per victim (sweetspot beats sourspot)
    pub start: i64,       // first active frame, relative to state start
    pub len: i64,         // active duration (this box is live for [start, start+len))
    pub off: Vector2, pub r: f32,
    pub damage: f32,
    pub angle: f32,       // launch angle° (0 fwd, 90 up, negative = spike); Sakurai-angle handling later
    pub bkb: f32,         // base knockback (PM/community BKB)
    pub kbg: f32,         // knockback growth (PM/community KBG; % scaling)
    pub set_kb: f32,      // weight-set/fixed knockback; 0 = use the growth formula (jab-lock/multi-hit)
    pub transcendent: bool, // skips clank vs other hitboxes
    pub refresh: i64,     // frames a victim is immune to THIS box after a connect (multi-hit re-hit gap)
}

pub struct AttackData {
    pub startup: i64,     // animation lead-in (no box before this); boxes key off the same f.frame
    pub recovery: i64,    // endlag after the last box closes
    pub boxes: [Hitbox; MAX_HB], pub nbox: u8, // id-ordered, fixed cap
}
impl AttackData {
    // state length for the FSM timer = lead-in covered by box.start, last box close, then recovery
    pub fn total(&self) -> i64 {
        let last = self.boxes[..self.nbox as usize].iter().map(|b| b.start + b.len).max().unwrap_or(0);
        last + self.recovery
    }
}

// capsule (a==b ⇒ sphere)
pub struct Hurtbox { pub a: Vector2, pub b: Vector2, pub r: f32 }

// closest-point-on-segment vs circle — the one geometry primitive everything uses
fn seg_circle_hit(a: Vector2, b: Vector2, c: Vector2, r: f32) -> bool {
    let ab = b - a;
    let t = if ab.length_squared() > 0.0 {
        ((c - a).dot(ab) / ab.length_squared()).clamp(0.0, 1.0)
    } else { 0.0 };
    (a + ab * t - c).length() <= r
}
```
`Fighter` gains `prev_pos: Vector2` (set at the top of `advance`) so a hitbox center can be swept
`prev_pos+off → pos+off`; capsule-vs-capsule is then `seg`-vs-`seg` (or seg-vs-circle while
hurtboxes stay circles in slice A).

One-hit-per-window for N players + multi-hit: replace `attack_hit: bool` with a per-box, per-victim
record. Fixed-size + Copy: `hit_cd: [[i16; MAX_PLAYERS]; MAX_HB]` on the `Fighter` (countdown per
box×victim; a box can re-hit a victim once its `refresh` countdown reaches 0). A wide box then hits
every overlapping victim once; a 3-box stomp lands three sequenced pops; same victim isn't double-
hit by one box inside its window.

## Knockback (PM / community frame data — NOT the Melee decomp)
Source the model from the community/SmashWiki knockback formula and Project M feel (PM tables), not
the decomp. The current linear `speed = (kb_base + kb_scale*d) * mult` (`lib.rs:1626`) gets replaced
by the canonical nonlinear formula with weight:
```rust
// p = target % AFTER the hit's damage is added; d = hit damage; w = target weight
// bkb/kbg/set_kb come off the Hitbox; constants are the universal community ones.
fn knockback(p: f32, d: f32, w: f32, hb: &Hitbox) -> f32 {
    if hb.set_kb > 0.0 {
        // weight-independent (fixed) knockback: jab-lock / multi-hit links stay reliable
        hb.set_kb * KB_FIXED + hb.bkb
    } else {
        ((p / 10.0 + p * d / 20.0) * (200.0 / (w + 100.0)) * 1.4 + 18.0) * (hb.kbg / 100.0) + hb.bkb
    }
}
```
- `Fighter` gains `weight: f32` (Falcon-ish ~104 for Knee Man). PM's lighter combo weight + stronger
  growth is what made it "goated" — author moves with PM-flavored `bkb`/`kbg`/`angle`, tune live.
- **Hitstun** scales off the result, PM/community constant `0.4`: `hitstun = floor(0.4 * KB)`
  (replaces the ad-hoc `speed * 0.12` at `lib.rs:1630`). Tumble/knockdown keys off the same KB.
- `Tune` carries `kb_hitstun (0.4)`, `kb_fixed`, and the per-move PM tables so the panel edits them.
- Aggressive feel = bigger `kbg` + the weight term doing the % ramp; it stays deterministic (all f32,
  same on every peer).

## Hit interrupts the move → hitstun animation
A hit must cancel the victim's in-progress attack: today `resolve_combat` sets `hitstun`/`vel` but
NOT `state`, so `advance` early-returns during hitstun (`lib.rs:876`) with the state frozen on the
attack (e.g. `Nair`) — harmless with one static circle, but with WINDOWED boxes a frozen `f.frame`
would keep reporting that move's later hitboxes as live. Fix (part of the combat refactor):

- On connect, force `victim.state = CharState::Launched` (new) and reset `victim.frame = 0`. That
  (a) plays the launch/hitstun clip in the shell instead of the frozen attack pose, and (b) makes
  `attack_for(Launched) == None`, so the interrupted move's remaining windows never fire — the
  interrupt is free.
- The hitstun branch (`lib.rs:773`) keys its animation off `Launched`; `tumble` picks the
  tumble/knockdown clip. On hitstun end it already routes to `Air`/`Stand` (`lib.rs:858`).
- Clank rebound is the same shape: force a short `CharState::Rebound` so the clanked move is
  interrupted and its boxes stop, matching the "interrupt into hitstun, of course" rule.
- Super-armor / heavy-hit later = a flag that suppresses the state force while still taking damage.

## Build order
1. **A — windowed multi-hitbox + id priority** (foundation; refactors `active_hitbox`/
   `resolve_combat` once; deletes `HitLate`/`hit_at`/`active_span`). `AttackData.boxes: [Hitbox; 4]`,
   each box owning `start`/`len`. Port JAB/NAIR/DAIR/DTILT/DASH_ATTACK: the single-window moves
   become one box (`start = old startup`); NAIR/DTILT's sex-kick becomes two boxes (strong early
   window + weak late window, same `off/r`) — keep the SAME numbers so the existing tests stay
   byte-identical. `resolve_combat`: for victim `b`, among `a`'s boxes live this frame pick the
   **lowest-id** that overlaps, apply it, stamp `hit_cd[box][b] = refresh`. `hit_cd` replaces
   `attack_hit`. **Includes the hit-interrupt**: connect forces `victim.state = Launched`,
   `frame = 0` (cancels remaining windows + drives the hitstun clip). Author the Knee Man stomp here
   as the first move that needs 3 windows.
2. **E — PM knockback** (pairs with A; can land same slice). Add `Fighter.weight`; swap the linear
   speed for the community formula; `hitstun = floor(0.4 * KB)`; PM-table `bkb/kbg/angle/set_kb` per
   box, all in `Tune`. This is where "aggressive, goated" gets dialed.
3. **B — swept (capsule) hits**. Add `Fighter.prev_pos`; `seg_circle_hit`. Removes the tunneling
   class of bugs at dash/knockback speed. Cheap, independent correctness win.
4. **D — clank + transcendent** (needs A). New phase 3a BEFORE the hit pass: see N-player section.
   Rebound forces `CharState::Rebound` (same interrupt shape as the hit).
5. **C — capsule hurtbox**. `hurtbox -> Hurtbox`; both passes use `seg`-vs-`seg`. Polish; independent.

Geometric algebra note: 2D circle/capsule overlap is just the closest-point test above — GA buys
nothing until hitboxes need ORIENTATION (rotate with `wall_tilt`), where a rotor is a clean
representation. Defer GA until then.

## Animation sequencing of N hitboxes (the Knee Man stomp)
One state clock — `f.frame`, ticked in `advance` (skipped during hitlag) — drives BOTH the sprite
and the hitboxes. The animation samples `f.frame` (the shell's `sync_attack_frame` already maps the
attack timeline → sprite cell); each hitbox lights when `f.frame ∈ [start, start+len)`. They are
coregistered by frame number and authored side by side, so a box "belongs to" a pose without any
extra wiring. `active_hitbox` (single, by-frame) becomes `live_boxes(f, t) -> impl Iterator<Hitbox>`
returning every box whose window contains `f.frame`.

The Knee Man aerial stomp — Captain-Falcon-flavored, three timed hitboxes on one drop:
```
state DairStomp, total() = 30  (startup 6, then the three boxes, then recovery)
box 0  id 0  start 6  len 3   off( 8,  30) r 44  dmg 12 angle -85  // the STOMP: deep spike, tiny window
box 1  id 1  start 9  len 4   off(14,  10) r 40  dmg  4 angle  20  refresh 0 // drag-down linger, weak
box 2  id 2  start 20 len 4   off(20, -50) r 46  dmg  9 angle  60  // the follow-up knee pop on the way out
```
- Frames 6-8 are the meteor sweetspot (lowest id ⇒ wins if it overlaps). Whiff the window and you
  drop into the weak linger (9-12), then the move recovers into a late upward pop (20-23).
- A true rapid multi-hit (drill) is the same shape with several short same-id boxes spaced by `len`,
  each with a small `refresh` so the victim is re-hittable on the next pop but not twice in one.
- Because windows are per-box, the FSM never needs special-case states per move: author the boxes,
  `total()` sets the state length, the animation and the hits stay in lockstep on `f.frame`.

This is why slice A's unit is the **windowed hitbox**, not a move-global active span: "attacks need
to sequence N hitboxes over the animation" is exactly `[Hitbox; 4]` each carrying `start`/`len`.

## N-player clank
Clank is hitbox-vs-hitbox and **symmetric**, so it is NOT the ordered (a,b)/(b,a) hit loop. It is a
separate pass over **unordered pairs of live hitboxes**, run BEFORE the hit pass so a clanked hitbox
can't also deal damage.

Enumerate every live hitbox this frame as `(fighter_index, box_id)`; flag each `alive = true`.
Iterate unordered pairs in a FIXED total order — `(fighter, box_id)` ascending — exactly the same
"impose a well-ordering for determinism" rule `step` already uses for combat. This is the answer to
"axiom of choice": the simultaneous N-way interaction is made well-defined by index+id order.

```
for each unordered pair (x, y) of live hitboxes, in (fighter,id) order:
    if x.fighter == y.fighter        : continue   // a move never clanks itself
    if !alive[x] || !alive[y]        : continue   // already knocked out this frame
    if x.transcendent || y.transcendent : continue // either side opts out → no clank
    if !hitboxes_overlap(x, y)       : continue   // capsule/capsule (swept) geometry
    match damage_window(x, y, tune.clank_window):
        Both   => { alive[x] = alive[y] = false; rebound(x.fighter); rebound(y.fighter); }
        XWins  => { alive[y] = false; }           // stronger passes through, keeps its hit
        YWins  => { alive[x] = false; }
// hit pass: resolve_combat only fires for boxes still `alive`
```

Sequential-kill semantics (deterministic, rollback-safe): once a hitbox is knocked out it can't
clank anything later in the pass. With 3+ fighters swinging at once this means a chain like
A(10) vs B(5) vs C(20) resolves in index order — A and B settle first, then whatever's left meets C.
Not perfectly simultaneous, but total-ordered and reproducible, which is the requirement.

Rules that map cleanly onto the model:
- **Aerials don't clank** → author their boxes `transcendent: true`. Classic clank is grounded-vs-
  grounded; this falls out for free.
- **Projectiles** (the item bolts, `update_items`) → `transcendent: true`, so a melee swing and a
  bolt both come out. (Projectile-vs-projectile clank, if ever wanted, is the same pass over the
  item array.)
- **Rebound** = short clank lag + small backward push on both fighters (`Tune.clank_lag`,
  `Tune.clank_push`); optionally interrupt the move into a rebound state later.
- **`clank_window`** lives in `Tune` (≈9% is the Melee feel); start there and tune.

Storage/lifetime: `alive` is a stack `[[bool; MAX_HB]; MAX_PLAYERS]` (Copy, no alloc) built each
frame from the live attacks; nothing persists across frames except `Fighter.prev_pos` and
`hit_mask` (both reset/derived per frame, both Copy).

## Tests to hold the line
- Existing 30 core + 6 net stay green after slice A (port the 5 moves 1-box, values unchanged →
  byte-identical sim, synctest + pure-replay-matches-ggrs still pass).
- New: `seg_circle_hit` unit cases; a tunneling regression (fast hitbox past a thin hurtbox lands
  with sweep, misses without); clank truth table (Both/XWins/YWins/transcendent-skip); N-player
  clank order determinism (same inputs → same `alive` set); wide-hitbox multi-victim (`hit_cd`
  hits P2+P3+P4 once each); multi-hit sequencing (the stomp lands 3 sequenced pops, none double);
  hit-interrupt (attacker hit mid-windows → `Launched`, remaining boxes never fire); knockback
  formula (known p/d/w/bkb/kbg → expected KB and `hitstun = floor(0.4*KB)`); set-kb stays
  weight-independent.
```
