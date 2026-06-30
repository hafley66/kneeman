//! Items + projectiles: pickups (guns, pen), the things they fire (bolts, bombs), spawning, the
//! held-tool follow, and projectile resolution (bolt hits, bomb blast). `Item` is the single home
//! for the slot type carried in `SimState.items`. Pure; re-exported at the crate root.

use crate::{
    airborne, hurtbox, AttackData, Fighter, HitLate, SimState, ToolKind, Tune, Vector2, DT,
    FLOOR_LEFT, FLOOR_RIGHT, GROUND_Y, HITLAG_PER_DMG, MAX_ITEMS,
};
use serde::{Deserialize, Serialize};

/// Per-item-kind config (spawn rate + behavior + model). Lives in Tune so the panel edits it live.
/// `hit` reuses AttackData for the projectile's damage/knockback (startup/active/recovery unused).
#[derive(Copy, Clone)]
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
    pub hit: AttackData,   // projectile / explosion damage + knockback
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
        hit: AttackData {
            startup: 0,
            active: 1,
            recovery: 0,
            off: Vector2::ZERO,
            r: 12.0,
            damage: 2.5,
            kb_base: 180.0,
            kb_scale: 1.2,
            kb_angle: 12.0, // near-flat: lasers push, don't launch
            late: HitLate::NONE,
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
        hit: AttackData {
            startup: 0,
            active: 1,
            recovery: 0,
            off: Vector2::ZERO,
            r: 22.0,        // contact radius of the bomb body
            damage: 16.0,
            kb_base: 760.0, // launches hard -> kills at mid %
            kb_scale: 5.0,
            kb_angle: 55.0, // up-and-out pop
            late: HitLate::NONE,
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
/// bolt = remaining lifetime. `ammo`: gun shots left.
#[derive(Copy, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub kind: ItemKind,
    pub pos: Vector2,
    pub vel: Vector2,
    pub owner: i8,
    pub ammo: i64,
    pub timer: i64,
    pub facing: f32,
    pub tool: ToolKind, // which drawing tool, when `kind == Pen` (ignored otherwise)
}

impl Item {
    pub const EMPTY: Self = Self {
        kind: ItemKind::None,
        pos: Vector2::ZERO,
        vel: Vector2::ZERO,
        owner: -1,
        ammo: 0,
        timer: 0,
        facing: 1.0,
        tool: ToolKind::TrailPen,
    };
    pub fn active(&self) -> bool {
        !matches!(self.kind, ItemKind::None)
    }
}

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
    let ammo = match kind {
        ItemKind::BobGun => t.bomb.ammo,
        ItemKind::Pen => 0, // a pen's budget lives on its InkPath at draw start, not in ammo
        _ => t.laser.ammo,
    };

    let span = (FLOOR_RIGHT - FLOOR_LEFT - 120.0).max(0.0);
    let frac = (next_rng(&mut n.rng) % 1000) as f32 / 1000.0;
    let x = FLOOR_LEFT + 60.0 + frac * span;
    n.items[slot] = Item {
        kind,
        pos: Vector2::new(x, GROUND_Y - 240.0), // drop in from above
        vel: Vector2::ZERO,
        owner: -1,
        ammo,
        timer: 0,
        facing: 1.0,
        tool: ToolKind::TrailPen, // todo: roll a random tool when more than one pen ships
    };
}

/// Nearest unowned ground gun overlapping a grounded, actionable fighter (the pickup the attack
/// button claims instead of jabbing). None in the air / during hitstun so attack stays an aerial.
pub(crate) fn nearest_pickup(f: &Fighter, items: &[Item; MAX_ITEMS]) -> Option<usize> {
    if airborne(f.state) || f.hitstun != 0 || f.hitlag != 0 {
        return None;
    }
    let (bc, br) = hurtbox(f);
    items.iter().position(|it| {
        it.active() && it.owner < 0 && it.kind.is_gun() && (it.pos - bc).length() <= br + ITEM_R
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
    if !gun.is_gun() || n.items[k].timer > 0 || n.items[k].ammo <= 0 {
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
            ammo: auto as i64, // laser: 1 = auto-fire (weak), 0 = full power; bomb ignores this
            timer: cfg.range,
            facing: f.facing,
            tool: ToolKind::TrailPen,
        };
    }
    n.items[k].ammo -= 1;
    n.items[k].timer = if auto { cfg.autofire_cd } else { cfg.cooldown };
    if n.items[k].ammo <= 0 {
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

/// Pickup intent: claim the nearest overlapping unowned ground item.
pub(crate) fn pickup_item(n: &mut SimState, idx: usize) {
    let f = n.fighters[idx];
    if let Some(k) = nearest_pickup(&f, &n.items) {
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
                // unowned: gravity, settle on the floor
                let mut p = it.pos;
                let mut v = it.vel;
                p += v * DT;
                if p.y < GROUND_Y {
                    v.y += t.gravity * DT;
                } else {
                    p.y = GROUND_Y;
                    v = Vector2::ZERO;
                }
                n.items[k].pos = p;
                n.items[k].vel = v;
            }
            ItemKind::LaserBolt => {
                let p = it.pos + it.vel * DT;
                n.items[k].pos = p;
                n.items[k].timer -= 1;
                let mut spent = n.items[k].timer <= 0
                    || p.x < FLOOR_LEFT - 400.0
                    || p.x > FLOOR_RIGHT + 400.0;
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
                let mut boom = n.items[k].timer <= 0 || p.y >= GROUND_Y;
                for fi in 0..2 {
                    if fi as i8 == it.owner {
                        continue; // doesn't detonate on its own thrower's body in flight
                    }
                    let (bc, br) = hurtbox(&n.fighters[fi]);
                    if (p - bc).length() <= br + t.bomb.hit.r {
                        boom = true;
                    }
                }
                if boom {
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
        f.damage += atk.damage * falloff;
        let speed = (atk.kb_base + atk.kb_scale * f.damage) * t.knockback_mult * falloff;
        let radial = if dist > 1.0 { d / dist } else { Vector2::new(0.0, -1.0) };
        f.vel = (radial + Vector2::new(0.0, -0.4)).normalize_or_zero() * speed; // up-biased pop
        f.hitstun = (speed * 0.12) as i64;
        f.tumble = speed > t.tumble_speed;
        f.hitlag = (atk.damage * HITLAG_PER_DMG) as i64 + 4;
        f.attack_hit = false;
    }
}

/// A laser bolt connecting: damage + flat push + brief hitstun/hitlag. Mirrors `resolve_combat`'s
/// tail but sourced from a projectile (launch direction = the bolt's travel direction).
fn apply_bolt_hit(b: &mut Fighter, bolt: &Item, t: &Tune) {
    if b.invuln > 0 || b.intangible {
        return; // spawn i-frames / active dodge
    }
    let atk = t.laser.hit;
    let scale = if bolt.ammo == 1 { t.laser.autofire_dmg } else { 1.0 }; // auto-fire bolts are weaker
    b.damage += atk.damage * scale;
    let speed = (atk.kb_base + atk.kb_scale * b.damage) * t.knockback_mult;
    let ang = atk.kb_angle.to_radians();
    b.vel = Vector2::new(ang.cos() * bolt.facing, -ang.sin()) * speed;
    b.hitstun = (speed * 0.12) as i64;
    b.tumble = speed > t.tumble_speed;
    b.hitlag = (atk.damage * HITLAG_PER_DMG) as i64 + 2;
}
