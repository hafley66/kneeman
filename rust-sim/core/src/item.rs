//! Items + projectiles: pickups (guns, pen), the things they fire (bolts, bombs), spawning, the
//! held-tool follow, and projectile resolution (bolt hits, bomb blast). `Item` is the single home
//! for the slot type carried in `SimState.items`. Pure; re-exported at the crate root.

use crate::{
    airborne, hurtbox, knockback_units, out_of_bounds, Fighter, Hitbox, SimState, StrokeId, ThrowDir,
    ToolKind, Tune, Vector2, DT, FLOOR_LEFT, FLOOR_RIGHT, GROUND_Y, HITLAG_PER_DMG, MAX_ITEMS,
};
use serde::{Deserialize, Serialize};

/// Per-item-kind config (spawn rate + behavior + model). Lives in Tune so the panel edits it live.
/// `hit` reuses AttackData for the projectile's damage/knockback (startup/active/recovery unused).
#[derive(Copy, Clone, Serialize, Deserialize)]
pub struct ItemConfig {
    pub spawn_weight: f32, // relative spawn chance vs other kinds (0 = never spawns)
    pub ammo: i64,         // shots a fresh gun carries
    pub cooldown: i64,     // frames between shots (a clean tap)
    pub autofire_cd: i64,  // frames between shots while holding (shorter = drains faster)
    pub autofire_dmg: f32, // damage multiplier for held auto-fire bolts (< 1 = weaker)
    pub speed: f32,        // projectile speed (px/s)
    pub range: i64,        // projectile lifetime in frames before it fizzles (for the bomb = its fuse)
    pub proj_gravity: f32, // px/s^2 pulling the projectile down (0 = straight laser; >0 = arcing lob)
    pub blast_r: f32,      // explosion radius on detonation (0 = single-target bolt, no AoE)
    pub model_id: u8,      // shell sprite key (rendering only; sim ignores it)
    pub hit: Hitbox,       // projectile / explosion damage + knockback (one box; transcendent)
}

impl ItemConfig {
    pub const LASER: Self = Self {
        spawn_weight: 1.0,
        ammo: 16,
        cooldown: 6,      // ~10 shots/sec on clean taps
        autofire_cd: 4,   // ~15 shots/sec while held — drains the mag faster
        autofire_dmg: 0.6, // held spray is weaker per bolt (the funny tax)
        speed: 1400.0,
        range: 70,
        proj_gravity: 0.0, // dead-straight
        blast_r: 0.0,      // single-target
        model_id: 0,
        hit: Hitbox {
            r: 12.0, damage: 2.5, angle: 12.0, // near-flat: lasers push, don't launch
            bkb: 10.0, kbg: 18.0, transcendent: true, ..Hitbox::NONE
        },
    };

    /// Red gun: low ammo, lobs a slow arcing bomb that detonates on contact or fuse and blasts
    /// everyone nearby (the funny "shoot it at your homies" weapon). Big radial knockback = a kill.
    pub const BOMB: Self = Self {
        spawn_weight: 0.7, // a bit rarer than the laser
        ammo: 4,           // four lobs and the gun is spent
        cooldown: 28,      // deliberate, ~2 shots/sec; no real autofire
        autofire_cd: 28,
        autofire_dmg: 1.0, // no auto-fire weakness; every lob is full power
        speed: 900.0,      // lobbed forward, gravity drags it into an arc
        range: 110,        // ~1.8s fuse if it never touches anyone
        proj_gravity: 2400.0,
        blast_r: 170.0,    // generous splash
        model_id: 1,       // red model key (shell)
        hit: Hitbox {
            r: 22.0,        // contact radius of the bomb body
            damage: 16.0, angle: 55.0, // up-and-out pop
            bkb: 28.0, kbg: 88.0, transcendent: true, ..Hitbox::NONE // launches hard -> kills at mid %
        },
    };
}

/// Directional item-throw config (Smash-style): the launch speed per stick direction plus the
/// `hit` the armed item deals to a non-thrower it touches. Lives in Tune so the panel edits it live.
/// Knockback runs through the same `knockback_units` formula as a fighter's hitboxes, so the box's
/// `bkb`/`kbg`/`angle` give "any amount of knockback" the user wants.
#[derive(Copy, Clone, Serialize, Deserialize)]
pub struct ThrowItem {
    pub fwd_speed: f32,  // px/s launched forward (toward facing)
    pub back_speed: f32, // px/s launched behind
    pub up_speed: f32,   // px/s launched up
    pub down_speed: f32, // px/s launched down (a spike toss)
    pub hit: Hitbox,     // damage + knockback the flying item deals on contact (transcendent)
}

impl ThrowItem {
    /// Strong default: a fast toss that launches hard. Panel-editable per field.
    pub const DEFAULT: Self = Self {
        fwd_speed: 1500.0,
        back_speed: 1200.0,
        up_speed: 1300.0,
        down_speed: 1700.0,
        hit: Hitbox {
            r: 34.0, damage: 9.0, angle: 45.0, // up-and-out pop
            bkb: 42.0, kbg: 92.0, transcendent: true, ..Hitbox::NONE // launches hard = a kill at mid %
        },
    };
}

/// What an item slot is. `None` = empty slot. Add kinds freely; behavior dispatches by `match`
/// (the "trait methods" are functions keyed on kind), config lives per-kind in Tune.
#[derive(Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ItemKind {
    None,
    LaserGun,  // pickup weapon: hold + attack to fire LaserBolts until ammo runs out
    LaserBolt, // the projectile a LaserGun fires
    BobGun,    // red pickup weapon: lobs an arcing explosive (Bob-omb-ish) per shot
    Bomb,      // the arcing explosive a BobGun fires; detonates on contact or fuse with radial knockback
    Pen,       // drawing tool: hold + attack to lay down an ink path (the tool is in `Item.tool`)
}

impl ItemKind {
    /// Projectiles are transient hit-effects, not pickups: they don't count toward the field's
    /// pickup cap and can never be grabbed. Every new emitted-hit kind goes here.
    pub fn is_projectile(self) -> bool {
        matches!(self, ItemKind::LaserBolt | ItemKind::Bomb)
    }

    /// A held weapon that fires on the attack button. Both guns count toward the one-pickup cap.
    pub fn is_gun(self) -> bool {
        matches!(self, ItemKind::LaserGun | ItemKind::BobGun)
    }

    /// A held drawing tool: attack lays ink instead of firing. Counts toward the pickup cap, follows
    /// the hand, and drops like a gun.
    pub fn is_pen(self) -> bool {
        matches!(self, ItemKind::Pen)
    }

    /// Held in hand on pickup (gun or pen): follows the hand, drops on grab/death.
    pub fn is_held_tool(self) -> bool {
        self.is_gun() || self.is_pen()
    }
}

/// One item OR projectile. Plain Copy data so it rolls back. `owner`: -1 = unowned ground item;
/// else the fighter index that holds it (gun) or fired it (bolt). `timer`: gun = fire cooldown,
/// `gas` is the item's first-dimension use measure (float, covers every kind): gun shots left, a
/// pen's remaining ink length, a bolt's weak-flag. `gas_max` is the spawn value, so a HUD can show
/// gas/gas_max as 0..1. `timer` is the projectile lifetime / gun cooldown.
#[derive(Copy, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub kind: ItemKind,
    pub pos: Vector2,
    pub vel: Vector2,
    pub owner: i8,
    pub gas: f32,
    pub gas_max: f32,
    pub timer: i64,
    pub facing: f32,
    pub tool: ToolKind, // which drawing tool, when `kind == Pen` (ignored otherwise)
    pub stroke: StrokeId, // which StrokeRegistry preset this pen stamps (row 0 = default)
    pub thrown: bool, // armed in flight from a directional throw: deals `throw_item.hit` to non-throwers.
                      // While thrown, `owner` still holds the thrower idx (skips self); disarms to a
                      // normal unowned ground item (owner -1, thrown false) once it settles.
}

impl Item {
    pub const EMPTY: Self = Self {
        kind: ItemKind::None,
        pos: Vector2::ZERO,
        vel: Vector2::ZERO,
        owner: -1,
        gas: 0.0,
        gas_max: 0.0,
        timer: 0,
        facing: 1.0,
        tool: ToolKind::TrailPen,
        stroke: 0,
        thrown: false,
    };
    pub fn active(&self) -> bool {
        !matches!(self.kind, ItemKind::None)
    }
}

/// A menu-facing description of a spawnable item: which kind, plus the name + one-line blurb the
/// item screen shows. Host-independent so the shell renders the roster without knowing the kinds.
pub struct ItemCard {
    pub kind: ItemKind,
    pub name: &'static str,
    pub blurb: &'static str,
}

/// The items the menu offers to spawn. Order here is the order the screen lists them.
pub const MENU_ITEMS: &[ItemCard] = &[
    ItemCard { kind: ItemKind::LaserGun, name: "Laser Gun", blurb: "hold attack to spray flat bolts" },
    ItemCard { kind: ItemKind::BobGun, name: "Bob Gun", blurb: "lobs an arcing bomb that blasts" },
    ItemCard { kind: ItemKind::Pen, name: "Pen", blurb: "draw ink terrain to stand on" },
];

// --- items ---------------------------------------------------------------------------------------

const HOLD_OFFSET: Vector2 = Vector2::new(34.0, -56.0); // held item position relative to fighter feet
const BOLT_R: f32 = 12.0;  // laser bolt collision radius
const ITEM_R: f32 = 30.0;  // pickup reach: ground item within this of the body is grabbable
const DROP_TOSS_X: f32 = 180.0; // forward velocity given to a dropped item
const DROP_TOSS_Y: f32 = -120.0; // small upward pop on drop (negative = up); gravity arcs it down

/// Deterministic LCG step (same constants as the SyncTest's generator). Advances `state` and
/// returns the high bits. Pure + integer, so both peers stay in lockstep.
fn next_rng(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    *state >> 33
}

/// Every `item_spawn_interval` ticks, drop a weighted-random item into a free slot. Position is
/// chosen from the LCG so it is identical on both peers.
pub(crate) fn maybe_spawn_item(n: &mut SimState, t: &Tune) {
    if !t.items_on || t.item_spawn_interval <= 0 || n.tick == 0 {
        return;
    }
    if n.tick % (t.item_spawn_interval as u64) != 0 {
        return;
    }
    // one item at a time (default on): skip the drop if any pickup already exists (ground OR held).
    // Projectiles don't count, so a bolt in flight never blocks the next gun. Generic over kinds.
    if t.one_item_at_a_time
        && n.items
            .iter()
            .any(|it| it.active() && !it.kind.is_projectile())
    {
        return;
    }
    let Some(slot) = n.items.iter().position(|it| !it.active()) else {
        return; // field full
    };
    // weighted kind pick across the gun table. Add kinds here as they land.
    let table = [
        (ItemKind::LaserGun, t.laser.spawn_weight),
        (ItemKind::BobGun, t.bomb.spawn_weight),
        (ItemKind::Pen, t.ink_spawn_weight),
    ];
    let total: f32 = table.iter().map(|&(_, w)| w.max(0.0)).sum();
    if total <= 0.0 {
        return;
    }
    let roll = (next_rng(&mut n.rng) % 100_000) as f32 / 100_000.0 * total;
    let mut acc = 0.0;
    let mut kind = table[0].0;
    for &(k, w) in &table {
        acc += w.max(0.0);
        if roll < acc {
            kind = k;
            break;
        }
    }
    let gas = fresh_gas(kind, t);

    let span = (FLOOR_RIGHT - FLOOR_LEFT - 120.0).max(0.0);
    let frac = (next_rng(&mut n.rng) % 1000) as f32 / 1000.0;
    let x = FLOOR_LEFT + 60.0 + frac * span;
    n.items[slot] = Item {
        kind,
        pos: Vector2::new(x, GROUND_Y - 240.0), // drop in from above
        vel: Vector2::ZERO,
        owner: -1,
        gas,
        gas_max: gas,
        timer: 0,
        facing: 1.0,
        tool: ToolKind::TrailPen, // todo: roll a random tool when more than one pen ships
        stroke: 0, // default preset; roll/assign a StrokeId when more materials ship
        thrown: false,
    };
}

/// A fresh item's starting `gas` -- the general first-dimension use measure: gun shots, or a pen's
/// ink-length budget. Spawn sets both `gas` and `gas_max` to this so a HUD can normalize gas/gas_max.
fn fresh_gas(kind: ItemKind, t: &Tune) -> f32 {
    match kind {
        ItemKind::BobGun => t.bomb.ammo as f32,
        ItemKind::Pen => t.ink_budget,
        _ => t.laser.ammo as f32,
    }
}

/// Force-drop one item of a chosen kind into a free slot (the menu's debug spawn). Lands mid-stage,
/// dropping in from above like a natural spawn. No-op when the field is full.
pub fn spawn_kind(n: &mut SimState, kind: ItemKind, t: &Tune) {
    let Some(slot) = n.items.iter().position(|it| !it.active()) else {
        return;
    };
    let gas = fresh_gas(kind, t);
    let x = (FLOOR_LEFT + FLOOR_RIGHT) * 0.5;
    n.items[slot] = Item {
        kind,
        pos: Vector2::new(x, GROUND_Y - 240.0),
        vel: Vector2::ZERO,
        owner: -1,
        gas,
        gas_max: gas,
        timer: 0,
        facing: 1.0,
        tool: ToolKind::TrailPen,
        stroke: 0,
        thrown: false,
    };
}

/// Clear every item slot and unhand both fighters (the menu's "clear field" button).
pub fn clear_items(n: &mut SimState) {
    n.items = [Item::EMPTY; MAX_ITEMS];
    for f in &mut n.fighters {
        f.holding = -1;
    }
}

/// True if point `c` lies within `r` of the segment `a`→`b` (capsule overlap test).
#[inline]
fn seg_circle_hit(a: Vector2, b: Vector2, c: Vector2, r: f32) -> bool {
    (c - crate::geo::closest_on_seg(c, a, b)).length() <= r
}

/// Nearest unowned ground pickup (gun OR pen) reachable by a grounded, actionable fighter via a
/// forward capsule (PM/Ultimate feel: generous cone ahead of the body, not just a tight circle at
/// the feet). None in the air / during hitstun so attack stays an aerial.
pub(crate) fn nearest_pickup(f: &Fighter, items: &[Item; MAX_ITEMS], t: &Tune) -> Option<usize> {
    if airborne(f.state) || f.hitstun != 0 || f.hitlag != 0 {
        return None;
    }
    let (bc, _br) = hurtbox(f);
    let facing = Vector2::new(f.facing, 0.0);
    let end = bc + facing * t.pickup_reach;
    items.iter().position(|it| {
        it.active()
            && it.owner < 0
            && it.kind.is_held_tool()
            && seg_circle_hit(bc, end, it.pos, t.pickup_r + ITEM_R)
    })
}

/// Held gun + fire intent: spawn the gun's projectile if off cooldown with ammo, decrement, vanish
/// when spent. Laser fires a flat bolt (auto-fire = weak); the red gun lobs an arcing bomb.
/// `auto` (held, not a fresh tap) marks a laser bolt weak via its `ammo` slot.
pub(crate) fn fire_gun(n: &mut SimState, idx: usize, auto: bool, t: &Tune) {
    let holding = n.fighters[idx].holding;
    if holding < 0 {
        return;
    }
    let k = holding as usize;
    let gun = n.items[k].kind;
    if !gun.is_gun() || n.items[k].timer > 0 || n.items[k].gas < 1.0 {
        return; // wrong item, on cooldown, or empty — the intent fired but nothing comes out
    }
    let cfg = if gun == ItemKind::BobGun { &t.bomb } else { &t.laser };
    let f = n.fighters[idx];
    let muzzle = f.pos + Vector2::new((HOLD_OFFSET.x + 20.0) * f.facing, HOLD_OFFSET.y);
    if let Some(slot) = n.items.iter().position(|x| !x.active()) {
        let (kind, vel) = if gun == ItemKind::BobGun {
            // lob up-and-forward; gravity in update_items bends it into an arc.
            (ItemKind::Bomb, Vector2::new(f.facing * cfg.speed, -cfg.speed * 0.5))
        } else {
            (ItemKind::LaserBolt, Vector2::new(f.facing * cfg.speed, 0.0))
        };
        n.items[slot] = Item {
            kind,
            pos: muzzle,
            vel,
            owner: idx as i8,
            gas: auto as i64 as f32, // laser: 1 = auto-fire (weak), 0 = full power; bomb ignores this
            gas_max: 1.0,
            timer: cfg.range,
            facing: f.facing,
            tool: ToolKind::TrailPen,
            stroke: 0,
            thrown: false,
        };
    }
    n.items[k].gas -= 1.0;
    n.items[k].timer = if auto { cfg.autofire_cd } else { cfg.cooldown };
    if n.items[k].gas < 1.0 {
        n.items[k] = Item::EMPTY; // spent gun vanishes
        n.fighters[idx].holding = -1;
    }
}

/// Drop intent: detach the held item to the ground with a small forward toss (update_items arcs it).
pub(crate) fn drop_item(n: &mut SimState, idx: usize) {
    let holding = n.fighters[idx].holding;
    if holding < 0 {
        return;
    }
    let k = holding as usize;
    let f = n.fighters[idx];
    n.items[k].owner = -1;
    n.items[k].vel = Vector2::new(f.facing * DROP_TOSS_X, DROP_TOSS_Y);
    n.fighters[idx].holding = -1;
}

/// Throw intent (directional grab): launch the held item as a live projectile in the stick
/// direction. `owner` stays the thrower so the armed item skips its own body; `thrown` arms it.
/// Speeds + the contact hitbox are `Tune.throw_item` (panel-editable, generous by default).
pub(crate) fn throw_item(n: &mut SimState, idx: usize, dir: ThrowDir, t: &Tune) {
    let holding = n.fighters[idx].holding;
    if holding < 0 {
        return;
    }
    let k = holding as usize;
    let f = n.fighters[idx];
    let ti = t.throw_item;
    let vel = match dir {
        ThrowDir::Up => Vector2::new(0.0, -ti.up_speed),
        ThrowDir::Down => Vector2::new(0.0, ti.down_speed),
        ThrowDir::Forward => Vector2::new(f.facing * ti.fwd_speed, 0.0),
        ThrowDir::Back => Vector2::new(-f.facing * ti.back_speed, 0.0),
    };
    n.items[k].vel = vel;
    n.items[k].facing = f.facing;
    n.items[k].thrown = true; // owner kept = thrower: the armed item passes through its own thrower
    n.fighters[idx].holding = -1;
}

/// An armed thrown item connecting with a non-thrower: damage + knockback from `throw_item.hit`,
/// launch direction = the throw's travel direction (the item's `facing`). Mirrors `apply_bolt_hit`.
fn apply_item_throw_hit(b: &mut Fighter, facing: f32, t: &Tune) {
    if b.invuln > 0 || b.intangible {
        return; // spawn i-frames / active dodge
    }
    let atk = t.throw_item.hit;
    b.damage += atk.damage;
    let kb = knockback_units(b.damage, atk.damage, t.weight, &atk);
    let speed = kb * t.kb_speed * t.knockback_mult;
    let ang = atk.angle.to_radians();
    b.vel = Vector2::new(ang.cos() * facing, -ang.sin()) * speed;
    b.hitstun = (kb * t.kb_hitstun) as i64;
    b.tumble = speed > t.tumble_speed;
    b.hitlag = (atk.damage * HITLAG_PER_DMG) as i64 + 2;
}

/// Pickup intent: claim the nearest reachable unowned ground item.
pub(crate) fn pickup_item(n: &mut SimState, idx: usize, t: &Tune) {
    let f = n.fighters[idx];
    if let Some(k) = nearest_pickup(&f, &n.items, t) {
        n.items[k].owner = idx as i8;
        n.fighters[idx].holding = k as i8;
    }
}

/// Post-step item physics: ground guns fall + rest, held guns follow their owner (dropping if the
/// owner died/respawned), bolts fly + hit + expire.
pub(crate) fn update_items(n: &mut SimState, t: &Tune) {
    for k in 0..MAX_ITEMS {
        let it = n.items[k];
        if !it.active() {
            continue;
        }
        match it.kind {
            ItemKind::LaserGun | ItemKind::BobGun | ItemKind::Pen if it.thrown => {
                // armed throw in flight: arc under gravity, damage the first non-thrower it grazes,
                // then despawn. Landing without a hit disarms it into a normal unowned ground pickup.
                let mut v = it.vel;
                v.y += t.gravity * DT;
                let p = it.pos + v * DT;
                n.items[k].pos = p;
                n.items[k].vel = v;
                let over_stage = p.x >= FLOOR_LEFT && p.x <= FLOOR_RIGHT;
                let mut hit_someone = false;
                for fi in 0..2 {
                    if fi as i8 == it.owner {
                        continue; // your own throw passes through you
                    }
                    let (bc, br) = hurtbox(&n.fighters[fi]);
                    if (p - bc).length() <= br + t.throw_item.hit.r {
                        apply_item_throw_hit(&mut n.fighters[fi], it.facing, t);
                        hit_someone = true;
                    }
                }
                if out_of_bounds(p) {
                    n.items[k] = Item::EMPTY; // crossed a blast zone: quiet despawn
                } else if hit_someone {
                    n.items[k] = Item::EMPTY; // spent on impact
                } else if over_stage && p.y >= GROUND_Y {
                    // landed harmlessly: disarm back into a normal unowned ground item
                    n.items[k].pos = Vector2::new(p.x, GROUND_Y);
                    n.items[k].vel = Vector2::ZERO;
                    n.items[k].thrown = false;
                    n.items[k].owner = -1;
                }
            }
            ItemKind::LaserGun | ItemKind::BobGun | ItemKind::Pen if it.owner >= 0 => {
                let o = it.owner as usize;
                if n.fighters[o].holding != k as i8 {
                    n.items[k].owner = -1; // owner let go / died: drop to the ground where it is
                } else {
                    let f = n.fighters[o];
                    n.items[k].pos = f.pos + Vector2::new(HOLD_OFFSET.x * f.facing, HOLD_OFFSET.y);
                    n.items[k].facing = f.facing;
                    if n.items[k].timer > 0 {
                        n.items[k].timer -= 1; // tick the fire cooldown while held
                    }
                }
            }
            ItemKind::LaserGun | ItemKind::BobGun | ItemKind::Pen => {
                // unowned: gravity, settle on the floor — but ONLY over the stage span. Off the edge
                // (past a wall) there is no floor, so it keeps falling and despawns at the blast zone.
                let mut p = it.pos;
                let mut v = it.vel;
                p += v * DT;
                let over_stage = p.x >= FLOOR_LEFT && p.x <= FLOOR_RIGHT;
                let mut rested = false;
                if over_stage && p.y >= GROUND_Y {
                    p.y = GROUND_Y;
                    v = Vector2::ZERO;
                    rested = true;
                } else {
                    v.y += t.gravity * DT; // off the span (or above the floor): keep falling
                }
                n.items[k].pos = p;
                n.items[k].vel = v;
                if out_of_bounds(p) {
                    n.items[k] = Item::EMPTY; // crossed a blast zone: quiet despawn
                } else if rested && it.kind == ItemKind::Pen && it.gas < 1.0 {
                    // an empty pen's "unload": once it's idle on the ground with no ink left it
                    // despawns (unlike a spent gun, which vanishes the instant it empties). It stays
                    // pickup-able while it falls/settles; guns keep resting here forever.
                    n.items[k] = Item::EMPTY;
                }
            }
            ItemKind::LaserBolt => {
                let p = it.pos + it.vel * DT;
                n.items[k].pos = p;
                n.items[k].timer -= 1;
                let mut spent = n.items[k].timer <= 0
                    || p.x < FLOOR_LEFT - 400.0
                    || p.x > FLOOR_RIGHT + 400.0
                    || out_of_bounds(p);
                for fi in 0..2 {
                    if fi as i8 == it.owner {
                        continue; // your own bolts pass through you
                    }
                    let (bc, br) = hurtbox(&n.fighters[fi]);
                    if (p - bc).length() <= br + BOLT_R {
                        apply_bolt_hit(&mut n.fighters[fi], &it, t);
                        spent = true;
                    }
                }
                if spent {
                    n.items[k] = Item::EMPTY;
                }
            }
            ItemKind::Bomb => {
                // Arc: gravity drags the lob into a parabola. Detonate on fuse-out, touching the
                // floor, or grazing any non-owner fighter. Then blast everyone in radius.
                let mut v = it.vel;
                v.y += t.bomb.proj_gravity * DT;
                let p = it.pos + v * DT;
                n.items[k].pos = p;
                n.items[k].vel = v;
                n.items[k].timer -= 1;
                // Off the stage span there is no floor to touch, so ground-contact only detonates
                // when the bomb is actually over the platform.
                let over_stage = p.x >= FLOOR_LEFT && p.x <= FLOOR_RIGHT;
                let mut boom = n.items[k].timer <= 0 || (over_stage && p.y >= GROUND_Y);
                for fi in 0..2 {
                    if fi as i8 == it.owner {
                        continue; // doesn't detonate on its own thrower's body in flight
                    }
                    let (bc, br) = hurtbox(&n.fighters[fi]);
                    if (p - bc).length() <= br + t.bomb.hit.r {
                        boom = true;
                    }
                }
                if out_of_bounds(p) {
                    n.items[k] = Item::EMPTY; // fell past a blast zone: quiet despawn, NO explosion
                } else if boom {
                    explode(n, p, t);
                    n.items[k] = Item::EMPTY;
                }
            }
            ItemKind::None => {}
        }
    }
}


/// Detonate the bomb: every fighter inside `blast_r` takes damage + radial knockback (away from the
/// center, biased upward so it pops), scaled by `knockback_mult` and distance falloff. Spawn i-frames
/// and active dodges shrug it off. Hits the thrower too -- standing in your own blast is on you.
fn explode(n: &mut SimState, center: Vector2, t: &Tune) {
    let atk = t.bomb.hit;
    for fi in 0..2 {
        if n.fighters[fi].invuln > 0 || n.fighters[fi].intangible {
            continue;
        }
        let (bc, _) = hurtbox(&n.fighters[fi]);
        let d = bc - center;
        let dist = d.length();
        if dist > t.bomb.blast_r {
            continue;
        }
        let falloff = 1.0 - 0.5 * (dist / t.bomb.blast_r); // full at center, ~half at the rim
        let f = &mut n.fighters[fi];
        let dmg = atk.damage * falloff;
        f.damage += dmg;
        let kb = knockback_units(f.damage, dmg, t.weight, &atk) * falloff;
        let speed = kb * t.kb_speed * t.knockback_mult;
        let radial = if dist > 1.0 { d / dist } else { Vector2::new(0.0, -1.0) };
        f.vel = (radial + Vector2::new(0.0, -0.4)).normalize_or_zero() * speed; // up-biased pop
        f.hitstun = (kb * t.kb_hitstun) as i64;
        f.tumble = speed > t.tumble_speed;
        f.hitlag = (dmg * HITLAG_PER_DMG) as i64 + 4;
        f.arm_hits();
    }
}

/// A laser bolt connecting: damage + flat push + brief hitstun/hitlag. Mirrors `resolve_combat`'s
/// tail but sourced from a projectile (launch direction = the bolt's travel direction).
fn apply_bolt_hit(b: &mut Fighter, bolt: &Item, t: &Tune) {
    if b.invuln > 0 || b.intangible {
        return; // spawn i-frames / active dodge
    }
    let atk = t.laser.hit;
    let scale = if bolt.gas == 1.0 { t.laser.autofire_dmg } else { 1.0 }; // auto-fire bolts are weaker
    let dmg = atk.damage * scale;
    b.damage += dmg;
    let kb = knockback_units(b.damage, dmg, t.weight, &atk);
    let speed = kb * t.kb_speed * t.knockback_mult;
    let ang = atk.angle.to_radians();
    b.vel = Vector2::new(ang.cos() * bolt.facing, -ang.sin()) * speed;
    b.hitstun = (kb * t.kb_hitstun) as i64;
    b.tumble = speed > t.tumble_speed;
    b.hitlag = (dmg * HITLAG_PER_DMG) as i64 + 2;
}
