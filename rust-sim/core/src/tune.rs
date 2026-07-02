//! Character + feel configuration. `CharData` is the canonical per-character definition in source
//! units (units/frame @ 60fps); `Tune` is its pixel-space derivation that the egui sliders edit
//! live. Split out of `lib` as plain config -- not sim state, not history.

use crate::physics::{acc, vel};
use crate::{AttackData, ItemConfig, SpecialMove, StrokeRegistry, ThrowData, ThrowItem};
use serde::{Deserialize, Serialize};

/// Per-character attributes in SOURCE UNITS (units/frame @ 60fps; frames are integers).
/// This is the canonical character definition; `Tune` (pixel-space) is derived from it.
///   csv = value taken from the reference physics table
///   est = community-derived value (not in any text dump) — tune freely
///   ult = modern-platform-fighter idea applied on purpose
#[derive(Copy, Clone)]
pub struct CharData {
    pub gravity: f32,         // csv 0.13
    pub max_fall: f32,        // csv 2.9   (TerminalVelocity)
    pub fastfall: f32,        // est 3.5   (csv lists 2.9, same as fall — looks like a dup)
    pub walk_max: f32,        // csv 0.85
    pub dash_init: f32,       // est 1.9   (initial dash burst)
    pub run_max: f32,         // est 2.34  (run top speed, dash accelerates toward this)
    pub ground_accel: f32,    // est 0.10
    pub ground_friction: f32, // csv 0.08  (Friction)
    pub fullhop_v: f32,       // est 3.68
    pub shorthop_v: f32,      // est 1.80
    pub airjump_v: f32,       // csv 2.66  (InitDJSpeed)
    pub airjump_h: f32,       // est 1.40  (double-jump horizontal redirect; lets you reverse)
    pub jump_h_init: f32,     // est 0.90  (stick contribution at takeoff)
    pub jump_h_max: f32,      // est 2.50  (takeoff h cap; ABOVE run so momentum survives the jump)
    pub air_speed: f32,       // est 1.60  (drift cap; raised from csv 1.12 for control, not momentum cap)
    pub air_accel: f32,       // est 0.18  (air mobility; raised from csv 0.06 — classic air is crusty)
    pub air_friction: f32,    // csv 0.01  (aerial drag; bleeds excess momentum slowly)
    pub momentum_carry: f32,  // est 1.0   (ground->air horizontal momentum mult; 1.0 = full carry)
    pub max_air_jumps: u8,    // csv 1     (Jumps)
    pub max_air_dodges: u8,   // est 1
    pub roll_speed: f32,      // est 1.8
    pub airdodge_speed: f32,  // est 3.1   (universal air-dodge burst)
    pub airdodge_drag: f32,   // est 0.15  (burst decay so it lunges + settles)
    pub ledgejump_v: f32,     // est 2.70
    // frame data (integer frames, not scaled)
    pub jumpsquat: i64,       // ult 3 (universal)
    pub landing_lag: i64,     // est 4
    pub dash_window: i64,     // est 12 (dash-dance window; dash -> run after this)
    pub pivot_frames: i64,    // ult 1
    pub dash_turn_accel: f32, // accel that fights your own momentum when reversing a dash/run/skid
    pub dashstop_friction: f32, // braking friction when a dash/run slides to a stop (Skid)
    pub spotdodge_frames: i64,// est 22
    pub roll_frames: i64,     // est 22
    pub airdodge_frames: i64, // est 28 (then actionable again — Ultimate-style, not helpless)
    pub ledge_intang: i64,    // est 30 (i-frames on grab)
    pub climb_frames: i64,    // est 24 (getup duration)
    pub buffer_frames: i64,   // ult 12 (Ultimate input buffer window)
}

impl CharData {
    pub const KNEEMAN: Self = Self {
        gravity: 0.17, // raised from csv 0.13 — snappier Ultimate-style arc, less hang
        max_fall: 2.9,
        fastfall: 4.2, // raised from csv 2.9 — crisper fast fall, less watery
        walk_max: 0.85,
        dash_init: 1.9,
        run_max: 2.34,
        ground_accel: 0.12,
        ground_friction: 0.22, // raised hard for stopping power (was 0.08 = ice)
        fullhop_v: 3.68,
        shorthop_v: 1.80,
        airjump_v: 3.30, // taller DJ; raised gravity had shrunk every jump's apex
        airjump_h: 1.40,
        jump_h_init: 0.90,
        jump_h_max: 2.50,
        air_speed: 1.60,
        air_accel: 0.22,
        air_friction: 0.01,
        momentum_carry: 1.0,
        max_air_jumps: 1,
        max_air_dodges: 1,
        roll_speed: 1.8,
        airdodge_speed: 3.1,
        airdodge_drag: 0.15,
        ledgejump_v: 2.70,
        jumpsquat: 3,
        landing_lag: 4,
        dash_window: 12,
        pivot_frames: 1,
        dash_turn_accel: 0.50, // reversal brake: bleeds old momentum through 0, no instant flip
        dashstop_friction: 0.30, // grippier than ground_friction (0.22) so a dash brakes hard
        spotdodge_frames: 22,
        roll_frames: 22,
        airdodge_frames: 28,
        ledge_intang: 30,
        climb_frames: 24,
        buffer_frames: 12,
    };
}

/// Live "feel" config in PIXEL SPACE (egui sliders write this). Derived from CharData by the
/// unit conversion; jump velocities are negative (up). Separate from SimState: tuning, not history.
#[derive(Copy, Clone, Serialize, Deserialize)]
pub struct Tune {
    pub gravity: f32,
    pub max_fall: f32,
    pub fastfall: f32,
    pub walk_speed: f32,
    pub dash_init: f32,
    pub run_speed: f32,
    pub ground_accel: f32,
    pub ground_friction: f32,
    pub fullhop_v: f32,  // negative
    pub shorthop_v: f32, // negative
    pub airjump_v: f32,  // negative
    pub airjump_h: f32,
    pub jump_h_init: f32,
    pub jump_h_max: f32,
    pub air_speed: f32,
    pub air_accel: f32,
    pub air_friction: f32,
    pub momentum_carry: f32,
    pub roll_speed: f32,
    pub airdodge_speed: f32,
    pub airdodge_drag: f32,
    pub ledgejump_v: f32, // negative
    pub max_air_jumps: i64,
    pub max_air_dodges: i64,
    pub jumpsquat: i64,
    pub landing_lag: i64,
    pub dash_window: i64,
    pub pivot_frames: i64,
    pub dash_turn_accel: f32,
    pub dashstop_friction: f32,
    pub spotdodge_frames: i64,
    pub roll_frames: i64,
    pub airdodge_frames: i64,
    pub ledge_intang: i64,
    pub climb_frames: i64,
    pub buffer_frames: i64,
    pub jab: AttackData,
    pub nair: AttackData,
    pub dair: AttackData,
    pub dtilt: AttackData,
    pub dash_attack: AttackData,
    pub dair_threshold: f32, // aim_y past this (and steeper than horizontal) picks dair over nair
    pub autohop_dmg: f32, // damage multiplier for auto-short-hop aerials (jump+attack macro)
    pub di_max_angle: f32, // max degrees the victim's stick can rotate a launch trajectory (survival DI)
    pub coyote_frames: i64, // grace window after walking off an edge to still get a full grounded jump
    pub plat_drop_window: i64, // soft-platform drop tilt-window: frames a Down tap waits before
                            // dropping, so a Down+Attack inside it reads as a Dtilt. 1 = instant
                            // drop / frame-perfect tilt (Melee); larger = more lenient tilt (PM).
    pub specials: [SpecialMove; 4], // B-move loadout, indexed by special_slot (N/Side/Up/Down)
    // items (match settings, not character-derived)
    pub items_on: bool,           // master switch for item spawns
    pub item_spawn_interval: i64, // frames between spawn attempts (0 = off)
    pub one_item_at_a_time: bool, // only ever one pickup on the field (projectiles don't count)
    pub pickup_reach: f32,        // forward cone length: capsule extends this far ahead of the body
    pub pickup_r: f32,            // capsule radius for the pickup zone (in addition to the item's ITEM_R)
    pub spawn_iframes: i64,       // respawn invulnerability window (frames)
    pub knockback_mult: f32,      // global launch-speed multiplier (>1 = everything flies further)
    // knockback model (community / Project-M formula; see `knockback_units`). NOT the Melee decomp.
    pub weight: f32,              // victim weight in the KB formula (Falcon/KneeMan-ish ~104)
    pub kb_speed: f32,            // px/s per KB unit (turns formula units into launch velocity)
    pub kb_hitstun: f32,         // hitstun frames per KB unit (community 0.4: floor(0.4 * KB))
    pub laser: ItemConfig,
    pub bomb: ItemConfig,         // the red gun's arcing explosive (Bob-omb-ish)
    pub throw_item: ThrowItem,    // directional item-throw speeds + the armed item's contact hitbox
    // drawn stroke paths
    pub strokes: StrokeRegistry,  // named stroke-material presets; row 0 = default (panel-editable)
    pub ink_budget: f32,          // total path length (px) a fresh ink item can lay before it's spent
    pub ink_cursor_reach: f32,    // CursorBrush: how far the drawing cursor floats off the body (px)
    pub ink_spawn_weight: f32,    // relative spawn chance of a pen vs the guns (0 = never random-spawns)
    pub ink_launch_speed: f32,    // computed launch px/s below this only shakes struck ink (no un-lock)
    pub fastfall_threshold: f32,  // stick aim_y must reach this (and beat |dir|) to fast fall
    // grab -> pummel -> throw
    pub grab_startup: i64,        // wind-up before the grab reach turns on
    pub grab_active: i64,         // frames the grab can catch
    pub grab_recovery: i64,       // whiff cool-down (heavy: a missed grab is punishable)
    pub grab_range: f32,          // forward reach of the grab from the body
    pub grab_hold: i64,           // auto-release countdown once a hold lands
    pub grab_mash: i64,           // extra countdown removed per fresh victim input (mash to escape)
    pub pummel_damage: f32,       // damage per pummel tap while holding
    pub pummel_bonus: i64,        // hold extended per pummel (capped at grab_hold)
    pub throws: [ThrowData; 4],   // fwd / back / up / down
    // tech / knockdown / getup
    pub tumble_speed: f32,        // launches faster than this knock down (or can be teched) on landing
    pub tech_window: i64,         // frames a shield press stays valid as a tech before impact
    pub tech_intang: i64,         // i-frames granted by a successful tech / getup
    pub techroll_speed: f32,      // horizontal speed of a tech-roll / getup-roll
    pub techroll_frames: i64,     // duration of a tech-roll / getup-roll
    pub knockdown_frames: i64,    // floored lie time before you can act / auto-getup
    pub getup_frames: i64,        // neutral getup rise duration (intangible)
    // stage geometry
    pub wall_bounce: f32,         // restitution when a LAUNCHED (tumbling) body hits a stage wall;
                                  // 0 = dead stop (normal recovery), >0 = bounce off (geo::reflect)
    pub floor_bounce: f32,        // restitution when a fast LAUNCHED (tumbling) body hits the floor;
                                  // 0 = dead stop (land), >0 = bounce up (dair spike -> funny bounce)
}

impl Tune {
    pub fn from_char(c: &CharData) -> Self {
        Self {
            gravity: acc(c.gravity),
            max_fall: vel(c.max_fall),
            fastfall: vel(c.fastfall),
            walk_speed: vel(c.walk_max),
            dash_init: vel(c.dash_init),
            run_speed: vel(c.run_max),
            ground_accel: acc(c.ground_accel),
            ground_friction: acc(c.ground_friction),
            fullhop_v: -vel(c.fullhop_v),
            shorthop_v: -vel(c.shorthop_v),
            airjump_v: -vel(c.airjump_v),
            airjump_h: vel(c.airjump_h),
            jump_h_init: vel(c.jump_h_init),
            jump_h_max: vel(c.jump_h_max),
            air_speed: vel(c.air_speed),
            air_accel: acc(c.air_accel),
            air_friction: acc(c.air_friction),
            momentum_carry: c.momentum_carry,
            roll_speed: vel(c.roll_speed),
            airdodge_speed: vel(c.airdodge_speed),
            airdodge_drag: acc(c.airdodge_drag),
            ledgejump_v: -vel(c.ledgejump_v),
            max_air_jumps: c.max_air_jumps as i64,
            max_air_dodges: c.max_air_dodges as i64,
            jumpsquat: c.jumpsquat,
            landing_lag: c.landing_lag,
            dash_window: c.dash_window,
            pivot_frames: c.pivot_frames,
            dash_turn_accel: acc(c.dash_turn_accel), // an acceleration, like ground_accel
            dashstop_friction: acc(c.dashstop_friction),
            spotdodge_frames: c.spotdodge_frames,
            roll_frames: c.roll_frames,
            airdodge_frames: c.airdodge_frames,
            ledge_intang: c.ledge_intang,
            climb_frames: c.climb_frames,
            buffer_frames: c.buffer_frames,
            jab: AttackData::JAB,
            nair: AttackData::NAIR,
            dair: AttackData::DAIR,
            dtilt: AttackData::DTILT,
            dash_attack: AttackData::DASH_ATTACK,
            dair_threshold: 0.5,
            autohop_dmg: 0.85, // Ultimate-ish 15% cut on the easy jump+attack aerial
            di_max_angle: 18.0, // ~18 deg of trajectory DI, the survival-DI ceiling
            coyote_frames: 9, // walk off the lip and you keep your real jump for ~9f (forgiving edge grace)
            plat_drop_window: 3, // PM-ish: 3f to convert a platform Down tap into a Dtilt before it drops
            specials: [SpecialMove::PUNCH, SpecialMove::LUNGE, SpecialMove::RISE, SpecialMove::DROP],
            items_on: true,
            item_spawn_interval: 1200, // ~20s between spawns (one item at a time, so keep it rare)
            one_item_at_a_time: true,
            pickup_reach: 100.0, // forward cone: capsule extends ~100px ahead (PM/Ult generous feel)
            pickup_r: 50.0,      // capsule radius: items within ~50px of the line segment are grabbed
            spawn_iframes: 120, // ~2s of respawn invulnerability
            knockback_mult: 1.4, // everything flies ~40% further (kills happen, kill moves matter)
            weight: 104.0,       // Falcon/KneeMan-ish; lighter = flies further (the PM combo weight)
            kb_speed: 6.0,       // KB units -> px/s (tuned so kill moves send ~kill distance)
            kb_hitstun: 0.4,     // community/PM constant: hitstun = floor(0.4 * KB)
            laser: ItemConfig::LASER,
            bomb: ItemConfig::BOMB,
            throw_item: ThrowItem::DEFAULT,
            strokes: StrokeRegistry::DEFAULT,
            ink_budget: 1.0e9, // effectively infinite ink while testing (was 900.0 ~ stage width);
                               // the pen out-of-gas unload still works, it just won't trigger here
            ink_cursor_reach: 140.0,
            ink_spawn_weight: 0.6,
            ink_launch_speed: 240.0, // lasers chip+shake (~115 px/s computed); throws/bombs launch (500+)
            fastfall_threshold: 0.6,
            grab_startup: 6,
            grab_active: 4,
            grab_recovery: 28, // whiff lag: missing a grab leaves you open
            grab_range: 100.0,
            grab_hold: 140,
            grab_mash: 9,
            pummel_damage: 2.4,
            pummel_bonus: 14,
            throws: [ThrowData::FWD, ThrowData::BACK, ThrowData::UP, ThrowData::DOWN],
            tumble_speed: 620.0, // ~a mid-% launch; below this you just land on your feet
            tech_window: 20,
            tech_intang: 26,
            techroll_speed: vel(2.4), // a touch faster than a roll (roll_speed ~1.8)
            techroll_frames: 24,
            knockdown_frames: 40,
            getup_frames: 24,
            wall_bounce: 0.45, // launched into the stage wall, a tumbling body kicks back off it
            floor_bounce: 0.55, // spiked into the floor, a fast tumbling body bounces back up
        }
    }
}

impl Default for Tune {
    fn default() -> Self {
        Self::from_char(&CharData::KNEEMAN)
    }
}
