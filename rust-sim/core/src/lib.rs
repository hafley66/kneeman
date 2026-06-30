// Engine-agnostic vectors. `Vector2` is kept as the local name (minimal churn from the
// godot original); the shell converts to godot::Vector2 at the render boundary.
pub use glam::Vec2 as Vector2;

pub mod geo; // deterministic collision geometry, API-shaped to mirror parry2d (swap-in later)

// fixed timestep; the sim never uses wall-clock delta (determinism).
pub const DT: f32 = 1.0 / 60.0;
pub const FPS: f32 = 60.0;

// World->screen scale. Source attributes are world-units; we render pixels.
// Spatial FEEL (jump-height : run-distance ratios, time-to-apex) is scale-invariant,
// so this just sets how big the world reads on screen. Bumped to fit the larger stage.
// Change it and every distance scales together.
pub const PX_PER_UNIT: f32 = 7.0;

// Battlefield-style stage: one solid main platform (with grabbable ledges) + soft platforms
// above that you land on from the top and drop through with down. All in pixel space.
const GROUND_Y: f32 = 760.0; // main platform top (resting feet-y)
const STAGE_BOTTOM: f32 = 900.0; // main platform underside (matches the Stage Main ColorRect)
const FLOOR_LEFT: f32 = 150.0;
const FLOOR_RIGHT: f32 = 1050.0; // main platform = 900 wide, centered on x=600

// Environment Collision Box: a diamond carried with the fighter, like classic platform fighters.
// `pos` is the BOTTOM vertex (the feet); the other three verts sit a half-height up and to the
// sides. The bottom vert lands on floors; the side verts collide with stage walls.
pub const ECB_HALF_W: f32 = 38.0; // left/right vert offset from center (x)
pub const ECB_HALF_H: f32 = 70.0; // top/bottom vert offset from center (y); body ~140px ≈ 3/4 the
                                  // ground→side-platform gap (185px). Was 42 (jiggly-sized).

/// The four ECB verts in WORLD space for a feet position, ordered [top, right, bottom, left].
pub fn ecb_verts(feet: Vector2) -> [Vector2; 4] {
    let cy = feet.y - ECB_HALF_H; // diamond center y (bottom vert = feet)
    [
        Vector2::new(feet.x, cy - ECB_HALF_H), // top
        Vector2::new(feet.x + ECB_HALF_W, cy), // right
        feet,                                  // bottom (feet)
        Vector2::new(feet.x - ECB_HALF_W, cy), // left
    ]
}
const LEDGE_REACH_X: f32 = 70.0; // how far past an edge the snap zone extends
const LEDGE_HANG_DY: f32 = 44.0; // hang this far below the lip while holding
// Blast zones: cross any edge = KO -> respawn. Side/top sit well outside the stage so only a
// launched (knocked-back) fighter reaches them; this is what makes horizontal/vertical knockback
// actually kill (kill moves). Bottom is the classic fall-off death.
pub const BLAST_Y: f32 = 1600.0; // below this = death (fall off the bottom)
pub const BLAST_TOP: f32 = -520.0; // above this = death (launched off the top)
pub const BLAST_LEFT: f32 = -420.0; // left of this = death
pub const BLAST_RIGHT: f32 = 1620.0; // right of this = death

/// True when a fighter has crossed any blast zone (all four edges = a real KO surface).
#[inline]
fn out_of_bounds(p: Vector2) -> bool {
    p.y > BLAST_Y || p.y < BLAST_TOP || p.x < BLAST_LEFT || p.x > BLAST_RIGHT
}

/// A stage platform. `solid` = the main stage (blocks, has ledges); else a soft platform
/// (land from above, drop through with down).
#[derive(Copy, Clone)]
pub struct Platform {
    pub left: f32,
    pub right: f32,
    pub y: f32,
    pub solid: bool,
}

/// Index 0 is always the solid main stage (ledges live on it). The rest are soft platforms.
pub const PLATFORMS: [Platform; 4] = [
    Platform { left: FLOOR_LEFT, right: FLOOR_RIGHT, y: GROUND_Y, solid: true },
    Platform { left: 280.0, right: 540.0, y: 575.0, solid: false }, // left
    Platform { left: 660.0, right: 920.0, y: 575.0, solid: false }, // right
    Platform { left: 470.0, right: 730.0, y: 410.0, solid: false }, // top center
];

const DASH_THRESH: f32 = 0.5; // |stick| past this from neutral = dash (keyboard digital is always 1.0)
const WALK_THRESH: f32 = 0.25; // |stick| past this but under DASH = walk (needs analog stick)
const STOP_EPS: f32 = 1.0; // |vel.x| under this in a braking state snaps to 0
const LEDGE_FALL_EPS: f32 = 150.0; // must be falling at least this fast to snap a ledge

pub const DUMMY_R: f32 = 48.0;    // body hurtbox radius (circle), scaled with the taller ECB
const DUMMY_FRICTION: f32 = 1200.0; // px/s^2 the dummy's knockback slide bleeds
const HITLAG_PER_DMG: f32 = 0.8;  // impact-freeze frames per point of damage

// units/frame      -> px/s    (a velocity)
fn vel(u: f32) -> f32 {
    u * FPS * PX_PER_UNIT
}
// units/frame^2    -> px/s^2  (an acceleration)
fn acc(u: f32) -> f32 {
    u * FPS * FPS * PX_PER_UNIT
}

fn sign(x: f32) -> f32 {
    if x > 0.0 {
        1.0
    } else if x < 0.0 {
        -1.0
    } else {
        0.0
    }
}

/// Ground/air/ledge action states. `frame` (in SimState) is the per-state timer that resets on
/// every transition, mirroring an animation frame — it gates the dash window, jumpsquat takeoff,
/// pivot, dodge length, landing lag, ledge intangibility, and getup.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum CharState {
    Stand,     // idle, grounded
    Walk,      // tilt-speed ground move
    Dash,      // initial dash (frame-windowed burst)
    Run,       // full-speed run (dash accelerates into this)
    Turn,      // standing pivot
    Skid,      // run brake / slide to a stop
    Crouch,    // hold down, grounded
    JumpSquat, // jump startup (universal 3f, shorthop decided here)
    Air,       // airborne: rising or falling
    Landing,   // touchdown lag
    Shield,    // guard
    SpotDodge, // dodge in place (intangible)
    Roll,      // rolling dodge (intangible)
    AirDodge,  // directional air dodge (intangible; into ground = wavedash)
    LedgeHold, // hanging on a ledge
    LedgeClimb,// ledge getup
    Jab,       // grounded quick attack
    Nair,      // neutral aerial
    Dair,      // down aerial: steep spike, drives the opponent down
    DashAttack,// attack out of dash/run: lunges forward, keeps momentum
    Grab,      // grab attempt: short reach, heavy whiff recovery on miss
    GrabHold,  // holding a grabbed opponent (pummel / throw / they mash out)
    Grabbed,   // being held: frozen, mash inputs to break free
    Knockdown, // floored after a hard launch (missed tech): lie, then get up
    Getup,     // rising from knockdown (intangible) -> Stand
    TechInPlace,// teched a landing in place (intangible recovery)
    TechRoll,  // teched/getup with a directional roll (intangible, moves)
    SpecialN,  // neutral-B
    SpecialS,  // side-B
    SpecialU,  // up-B (recovery; ends in Helpless if it finishes airborne)
    SpecialD,  // down-B
    Helpless,  // special-fall after an up-B: drift only, no actions until you land/ledge
}

/// Special states map to a slot in `Tune.specials` (the seed for swappable move loadouts).
fn special_slot(st: CharState) -> Option<usize> {
    match st {
        CharState::SpecialN => Some(0),
        CharState::SpecialS => Some(1),
        CharState::SpecialU => Some(2),
        CharState::SpecialD => Some(3),
        _ => None,
    }
}
fn is_special(st: CharState) -> bool {
    special_slot(st).is_some()
}

/// Which special the stick selects at the press: up / down / side / neutral.
fn special_dir(aim: Vector2) -> usize {
    if aim.y <= -0.5 {
        2 // up
    } else if aim.y >= 0.5 {
        3 // down
    } else if aim.x.abs() >= 0.5 {
        1 // side
    } else {
        0 // neutral
    }
}

/// The "next action" a fighter's state machine emits each frame: a pure descriptor of an effect it
/// can't apply alone because it needs the whole `SimState` (the item array). `advance` returns one;
/// `step` actuates it via `apply_act`. The input stream is never mutated — the FSM just describes.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Act {
    None,
    Fire { auto: bool }, // held gun + attack: spawn a bolt (auto = held, not a fresh tap: weaker)
    Drop,                // held item + grab: detach to the ground
    Pickup,              // empty hands + attack over an item: claim it (else attack jabs)
}

fn airborne(st: CharState) -> bool {
    matches!(
        st,
        CharState::Air | CharState::AirDodge | CharState::Nair | CharState::Dair | CharState::Helpless
    )
}

fn is_ledge(st: CharState) -> bool {
    matches!(st, CharState::LedgeHold | CharState::LedgeClimb)
}

/// One attack's frame data + hitbox + knockback, in pixel space (prototype values; tune freely).
/// The hitbox is a circle offset from the fighter (x flipped by facing); active only in its window.
#[derive(Copy, Clone, PartialEq)]
pub struct AttackData {
    pub startup: i64,  // wind-up frames before the hitbox turns on
    pub active: i64,   // frames the hitbox is live
    pub recovery: i64, // cool-down frames after, then back to neutral
    pub off: Vector2,  // hitbox center offset from fighter pos (x is forward)
    pub r: f32,        // hitbox radius
    pub damage: f32,   // % added on hit
    pub kb_base: f32,  // base knockback speed (px/s)
    pub kb_scale: f32, // extra knockback per point of accumulated damage
    pub kb_angle: f32, // launch angle in degrees (0 = forward, 90 = straight up)
}

/// A throw's launch: the grab's payoff. No frame windows (the throw fires the frame it's chosen);
/// just damage + the knockback curve, indexed fwd/back/up/down in `Tune.throws`.
#[derive(Copy, Clone, PartialEq)]
pub struct ThrowData {
    pub damage: f32,
    pub kb_base: f32,
    pub kb_scale: f32,
    pub kb_angle: f32, // degrees, 0 = forward, 90 = straight up
}

impl ThrowData {
    const FWD: Self = Self { damage: 8.0, kb_base: 520.0, kb_scale: 4.2, kb_angle: 42.0 };
    const BACK: Self = Self { damage: 10.0, kb_base: 620.0, kb_scale: 4.6, kb_angle: 44.0 };
    const UP: Self = Self { damage: 7.0, kb_base: 560.0, kb_scale: 4.4, kb_angle: 88.0 };
    const DOWN: Self = Self { damage: 6.0, kb_base: 440.0, kb_scale: 3.6, kb_angle: 72.0 };
}

impl AttackData {
    pub fn total(&self) -> i64 {
        self.startup + self.active + self.recovery
    }

    // baseline definitions; live copies live in Tune so the panel can edit them.
    const JAB: Self = Self {
        startup: 3,
        active: 3,
        recovery: 9,
        off: Vector2::new(44.0, -64.0),
        r: 32.0,
        damage: 3.0,
        kb_base: 320.0,
        kb_scale: 3.0,
        kb_angle: 35.0,
    };
    const NAIR: Self = Self {
        startup: 5,
        active: 9,
        recovery: 14,
        off: Vector2::new(26.0, -60.0), // centered on the taller body so a jump-in connects
        r: 52.0,
        damage: 8.0,
        kb_base: 520.0,
        kb_scale: 4.2,
        kb_angle: 45.0,
    };
    const DAIR: Self = Self {
        startup: 10,
        active: 6,
        recovery: 18,
        off: Vector2::new(10.0, 24.0), // hitbox below the feet (down + slightly forward)
        r: 40.0,
        damage: 11.0,
        kb_base: 460.0,
        kb_scale: 3.8,
        kb_angle: -72.0, // negative = downward launch: the spike
    };
    const DASH_ATTACK: Self = Self {
        startup: 8,
        active: 6,    // lingering horizontal swipe
        recovery: 38, // heavy endlag — whiff it and you are wide open (the commitment)
        off: Vector2::new(100.0, -58.0), // reaches far out front
        r: 54.0,                          // big horizontal hitbox
        damage: 11.0,
        kb_base: 540.0,
        kb_scale: 3.9,
        kb_angle: 30.0, // low, horizontal launch — sends them flying sideways toward the blast zone
    };
}

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
        },
    };
}

/// The (live) attack definition for a state, if it is one.
/// How a special moves the fighter while it runs. The hitbox/frames/knockback live in `hit`
/// (reuses the whole attack pipeline: `attack_for` -> `active_hitbox` -> `resolve_combat`).
/// This is the seed for swappable move loadouts: a character's 4 B-slots are just `SpecialMove`s.
#[derive(Copy, Clone, PartialEq)]
pub enum SpecialKind {
    Punch, // planted heavy hit (neutral-B): brakes to a stop, no travel
    Lunge, // forward burst (side-B)
    Rise,  // upward recovery burst (up-B); ends in Helpless if still airborne
    Fall,  // downward drive (down-B)
}

#[derive(Copy, Clone, PartialEq)]
pub struct SpecialMove {
    pub kind: SpecialKind,
    pub hit: AttackData, // frames + hitbox + knockback
    pub move_x: f32,     // forward-relative horizontal burst at the active window (px/s)
    pub move_y: f32,     // vertical burst (negative = up) at the active window (px/s)
}

impl SpecialMove {
    // Default kit (Falcon-ish): heavy neutral-B punch, a side lunge, a rising recovery, a down drive.
    const PUNCH: Self = Self {
        kind: SpecialKind::Punch,
        hit: AttackData {
            startup: 14,
            active: 4,
            recovery: 26,
            off: Vector2::new(58.0, -60.0),
            r: 46.0,
            damage: 22.0,
            kb_base: 900.0,
            kb_scale: 5.0,
            kb_angle: 38.0,
        },
        move_x: 0.0,
        move_y: 0.0,
    };
    const LUNGE: Self = Self {
        kind: SpecialKind::Lunge,
        hit: AttackData {
            startup: 8,
            active: 6,
            recovery: 22,
            off: Vector2::new(60.0, -58.0),
            r: 42.0,
            damage: 9.0,
            kb_base: 520.0,
            kb_scale: 3.6,
            kb_angle: 55.0,
        },
        move_x: 900.0,
        move_y: -120.0,
    };
    const RISE: Self = Self {
        kind: SpecialKind::Rise,
        hit: AttackData {
            startup: 6,
            active: 8,
            recovery: 22,
            off: Vector2::new(20.0, -90.0),
            r: 44.0,
            damage: 7.0,
            kb_base: 480.0,
            kb_scale: 3.0,
            kb_angle: 80.0,
        },
        move_x: 380.0,
        move_y: -1500.0,
    };
    const DROP: Self = Self {
        kind: SpecialKind::Fall,
        hit: AttackData {
            startup: 8,
            active: 10,
            recovery: 18,
            off: Vector2::new(24.0, 10.0),
            r: 44.0,
            damage: 10.0,
            kb_base: 460.0,
            kb_scale: 3.4,
            kb_angle: -68.0, // downward: a spike
        },
        move_x: 220.0,
        move_y: 700.0,
    };
}

pub fn attack_for(t: &Tune, st: CharState) -> Option<AttackData> {
    match st {
        CharState::Jab => Some(t.jab),
        CharState::Nair => Some(t.nair),
        CharState::Dair => Some(t.dair),
        CharState::DashAttack => Some(t.dash_attack),
        _ => special_slot(st).map(|s| t.specials[s].hit),
    }
}

/// Pick which aerial comes out from the aim captured at the attack press. Down past the threshold
/// picks dair; otherwise nair. No diagonal-vs-cardinal gate yet: with only nair + dair, holding a
/// horizontal for drift must NOT steal the dair (that gate returns when fair/bair land).
pub fn aerial_for(aim: Vector2, t: &Tune) -> CharState {
    if aim.y >= t.dair_threshold {
        CharState::Dair
    } else {
        CharState::Nair
    }
}

/// Active hitbox center in world space for an attacking state (None if not in the active window).
pub fn active_hitbox(f: &Fighter, t: &Tune) -> Option<(Vector2, f32)> {
    let atk = attack_for(t, f.state)?;
    if f.frame >= atk.startup && f.frame < atk.startup + atk.active {
        let c = f.pos + Vector2::new(atk.off.x * f.facing, atk.off.y);
        Some((c, atk.r))
    } else {
        None
    }
}

/// A fighter's hurtbox: a circle centered mid-body (one ECB half-height above the feet).
/// Radius is the shared body radius (was the training dummy's radius).
pub fn hurtbox(f: &Fighter) -> (Vector2, f32) {
    (f.pos + Vector2::new(0.0, -ECB_HALF_H), DUMMY_R)
}

/// Every input edge that can be buffered, as one type. Recorded on the button edge with the aim at
/// that moment, consumed when the state machine reaches a point where it can act — this is what
/// makes wavedash / jump-out-of-lag / the down-diagonal feel reliable instead of frame-perfect.
/// `window` is the only place a buffer length is decided, dispatched by a match (the enum's job —
/// no bit tricks): the lookahead edges share the live Tune window; `Grab` is 0 (press-frame only,
/// as today) but expressible, the seam to give it a real buffer later.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum Action {
    #[default]
    None,
    Jump,
    ShortHop,
    AirDodge,
    Aerial,
    Attack,
    Grab,
    Special,
}

impl Action {
    /// Display label for the debug panel.
    pub fn name(self) -> &'static str {
        match self {
            Action::None => "—",
            Action::Jump => "JUMP",
            Action::ShortHop => "SHORTHOP",
            Action::AirDodge => "AIRDODGE",
            Action::Aerial => "AERIAL",
            Action::Attack => "ATTACK",
            Action::Grab => "GRAB",
            Action::Special => "SPECIAL",
        }
    }

    fn window(self, t: &Tune) -> i64 {
        match self {
            Action::None | Action::Grab => 0,
            Action::Jump
            | Action::ShortHop
            | Action::AirDodge
            | Action::Aerial
            | Action::Attack
            | Action::Special => t.buffer_frames,
        }
    }
}

/// Buffer lanes. Each lane holds at most one pending action and lanes coexist, so a jump and an
/// aerial pressed together for the auto-short-hop both survive. The `Movement` lane carries
/// whichever of jump / short-hop / air-dodge was pressed most recently (they're mutually-exclusive
/// intents — newest wins); the rest are single-action.
#[repr(usize)]
#[derive(Copy, Clone)]
enum Lane {
    Movement,
    Aerial,
    Attack,
    Grab,
    Special,
}
const N_LANE: usize = 5;

/// One lane's pending action. `timer == 0` (or `action == None`) means empty; while live, `aim` is
/// the stick captured at the press and, on the movement lane, refreshed within the window — the
/// diagonal that lets a buffered air-dodge keep its latest direction.
#[derive(Copy, Clone, PartialEq, Default, Debug)]
pub struct Slot {
    pub action: Action,
    pub timer: i64,
    pub aim: Vector2,
}

/// One fighter as a plain value. Two of these make a `SimState`. Everything here is
/// per-fighter (the old single-player SimState fields); `damage`/`hitstun` were the old
/// `dummy_*` fields, now owned by every fighter (each can take and deal hits).
/// `frame` is the per-fighter STATE timer (reset on every transition), not a global clock.
#[derive(Copy, Clone, PartialEq)]
pub struct Fighter {
    pub frame: i64,
    pub pos: Vector2,
    pub vel: Vector2,
    pub state: CharState,
    pub facing: f32,       // +1 right, -1 left
    pub air_jumps: u8,     // remaining air jumps (refreshed on ground/ledge contact)
    pub air_dodges: u8,    // remaining air dodges
    pub fast_falling: bool,
    pub full_hop: bool,    // decided during JumpSquat (jump still held at takeoff)
    pub buf: [Slot; N_LANE], // input buffer, one lane each (Movement/Aerial/Attack/Grab); see Lane
    pub autohop_aerial: bool, // current aerial came from the jump+attack auto-short-hop (reduced dmg)
    pub intangible: bool,  // dodge / ledge i-frames (drives the debug color)
    pub regrab_lock: i64,  // frames before a ledge can be re-grabbed
    pub ground_plat: i32,  // index into PLATFORMS the fighter stands on (-1 = airborne)
    pub attack_hit: bool,  // current attack already connected (one hit per swing)
    pub hitlag: i64,       // impact freeze on connect (this fighter held)
    pub damage: f32,       // accumulated % (knockback scales with this)
    pub hitstun: i64,      // frames launched/can't act (drives the hit flash + knockback slide)
    pub holding: i8,       // index into SimState.items of the held item, or -1 (empty-handed)
    pub coyote: u8,        // grace frames after walking off an edge where jump = full grounded jump
    pub invuln: u8,        // spawn/respawn i-frames: ignore incoming hits while > 0
    pub grab_link: i8,     // grab partner index (victim if GrabHold, grabber if Grabbed), else -1
    pub grab_timer: i64,   // hold countdown: ticks down + victim mash chips it; <= 0 = break free
    pub tech_buf: u8,      // tech window: a shield press during hitstun arms a tech for N frames
    pub tumble: bool,      // this launch is hard enough to knock down (or be teched) on landing
}

/// Max simultaneous items+projectiles on screen (fixed so SimState stays Copy + checksums cheaply).
pub const MAX_ITEMS: usize = 8;

/// What an item slot is. `None` = empty slot. Add kinds freely; behavior dispatches by `match`
/// (the "trait methods" are functions keyed on kind), config lives per-kind in Tune.
#[derive(Copy, Clone, PartialEq, Eq)]
pub enum ItemKind {
    None,
    LaserGun,  // pickup weapon: hold + attack to fire LaserBolts until ammo runs out
    LaserBolt, // the projectile a LaserGun fires
    BobGun,    // red pickup weapon: lobs an arcing explosive (Bob-omb-ish) per shot
    Bomb,      // the arcing explosive a BobGun fires; detonates on contact or fuse with radial knockback
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
}

/// One item OR projectile. Plain Copy data so it rolls back. `owner`: -1 = unowned ground item;
/// else the fighter index that holds it (gun) or fired it (bolt). `timer`: gun = fire cooldown,
/// bolt = remaining lifetime. `ammo`: gun shots left.
#[derive(Copy, Clone, PartialEq)]
pub struct Item {
    pub kind: ItemKind,
    pub pos: Vector2,
    pub vel: Vector2,
    pub owner: i8,
    pub ammo: i64,
    pub timer: i64,
    pub facing: f32,
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
    };
    pub fn active(&self) -> bool {
        !matches!(self.kind, ItemKind::None)
    }
}

/// The entire sim state as a plain value: two fighters + the item field. This is what the
/// BehaviorSubject holds, what ggrs saves/rolls back, and what egui renders. `Copy` so snapshots
/// are free.
#[derive(Copy, Clone, PartialEq)]
pub struct SimState {
    pub fighters: [Fighter; 2],
    pub items: [Item; MAX_ITEMS],
    pub tick: u64, // global frame counter (drives item spawn cadence)
    pub rng: u64,  // deterministic LCG state (item spawn positions/kinds; rolls back with state)
}

impl Fighter {
    /// One fighter spawned airborne above the stage at `x`, facing `facing` (+1/-1).
    pub fn spawn(x: f32, facing: f32) -> Self {
        Self {
            frame: 0,
            pos: Vector2::new(x, 250.0),
            vel: Vector2::ZERO,
            state: CharState::Air,
            facing,
            air_jumps: 1,
            air_dodges: 1,
            fast_falling: false,
            full_hop: true,
            buf: [Slot::default(); N_LANE],
            autohop_aerial: false,
            intangible: false,
            regrab_lock: 0,
            ground_plat: -1,
            attack_hit: false,
            hitlag: 0,
            damage: 0.0,
            hitstun: 0,
            holding: -1,
            coyote: 0,
            invuln: 0,
            grab_link: -1,
            grab_timer: 0,
            tech_buf: 0,
            tumble: false,
        }
    }

    /// The action currently buffered in a lane, or `None` if the lane is empty/expired.
    #[inline]
    fn live(&self, l: Lane) -> Action {
        let s = &self.buf[l as usize];
        if s.timer > 0 {
            s.action
        } else {
            Action::None
        }
    }

    /// Record an edge into its lane: timer = the action's window + 1 (so the press frame always
    /// counts, since aging has already run this frame). Newer presses overwrite the lane.
    #[inline]
    fn record(&mut self, l: Lane, a: Action, aim: Vector2, t: &Tune) {
        self.buf[l as usize] = Slot { action: a, timer: a.window(t) + 1, aim };
    }

    #[inline]
    fn clear_lane(&mut self, l: Lane) {
        self.buf[l as usize] = Slot::default();
    }

    /// Debug/inspection accessors (the panel reads these; nothing else needs the lane indices).
    pub fn move_buffer(&self) -> Slot {
        self.buf[Lane::Movement as usize]
    }
    pub fn aerial_buffer_frames(&self) -> i64 {
        self.buf[Lane::Aerial as usize].timer
    }
    pub fn attack_buffer_frames(&self) -> i64 {
        self.buf[Lane::Attack as usize].timer
    }

    pub fn state_name(&self) -> &'static str {
        match self.state {
            CharState::Stand => "STAND",
            CharState::Walk => "WALK",
            CharState::Dash => "DASH",
            CharState::Run => "RUN",
            CharState::Turn => "TURN",
            CharState::Skid => "SKID",
            CharState::Crouch => "CROUCH",
            CharState::JumpSquat => "JUMPSQUAT",
            CharState::Air => "AIR",
            CharState::Landing => "LANDING",
            CharState::Shield => "SHIELD",
            CharState::SpotDodge => "SPOTDODGE",
            CharState::Roll => "ROLL",
            CharState::AirDodge => "AIRDODGE",
            CharState::LedgeHold => "LEDGE_HOLD",
            CharState::LedgeClimb => "LEDGE_CLIMB",
            CharState::Jab => "JAB",
            CharState::Nair => "NAIR",
            CharState::Dair => "DAIR",
            CharState::DashAttack => "DASHATK",
            CharState::Grab => "GRAB",
            CharState::GrabHold => "GRAB_HOLD",
            CharState::Grabbed => "GRABBED",
            CharState::Knockdown => "KNOCKDOWN",
            CharState::Getup => "GETUP",
            CharState::TechInPlace => "TECH",
            CharState::TechRoll => "TECH_ROLL",
            CharState::SpecialN => "SPECIAL_N",
            CharState::SpecialS => "SPECIAL_S",
            CharState::SpecialU => "SPECIAL_U",
            CharState::SpecialD => "SPECIAL_D",
            CharState::Helpless => "HELPLESS",
        }
    }
}

impl SimState {
    /// Two fighters facing each other on the main stage (airborne drop-in).
    pub fn spawn() -> Self {
        Self {
            fighters: [Fighter::spawn(480.0, 1.0), Fighter::spawn(720.0, -1.0)],
            items: [Item::EMPTY; MAX_ITEMS],
            tick: 0,
            rng: 0x9E37_79B9_7F4A_7C15, // fixed seed: both peers spawn identical items
        }
    }
}

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
#[derive(Copy, Clone)]
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
    pub dash_attack: AttackData,
    pub dair_threshold: f32, // aim_y past this (and steeper than horizontal) picks dair over nair
    pub autohop_dmg: f32, // damage multiplier for auto-short-hop aerials (jump+attack macro)
    pub di_max_angle: f32, // max degrees the victim's stick can rotate a launch trajectory (survival DI)
    pub coyote_frames: i64, // grace window after walking off an edge to still get a full grounded jump
    pub specials: [SpecialMove; 4], // B-move loadout, indexed by special_slot (N/Side/Up/Down)
    // items (match settings, not character-derived)
    pub items_on: bool,           // master switch for item spawns
    pub item_spawn_interval: i64, // frames between spawn attempts (0 = off)
    pub one_item_at_a_time: bool, // only ever one pickup on the field (projectiles don't count)
    pub spawn_iframes: i64,       // respawn invulnerability window (frames)
    pub knockback_mult: f32,      // global launch-speed multiplier (>1 = everything flies further)
    pub laser: ItemConfig,
    pub bomb: ItemConfig,         // the red gun's arcing explosive (Bob-omb-ish)
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
            dash_attack: AttackData::DASH_ATTACK,
            dair_threshold: 0.5,
            autohop_dmg: 0.85, // Ultimate-ish 15% cut on the easy jump+attack aerial
            di_max_angle: 18.0, // ~18 deg of trajectory DI, the survival-DI ceiling
            coyote_frames: 9, // walk off the lip and you keep your real jump for ~9f (forgiving edge grace)
            specials: [SpecialMove::PUNCH, SpecialMove::LUNGE, SpecialMove::RISE, SpecialMove::DROP],
            items_on: true,
            item_spawn_interval: 1200, // ~20s between spawns (one item at a time, so keep it rare)
            one_item_at_a_time: true,
            spawn_iframes: 120, // ~2s of respawn invulnerability
            knockback_mult: 1.4, // everything flies ~40% further (kills happen, kill moves matter)
            laser: ItemConfig::LASER,
            bomb: ItemConfig::BOMB,
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
        }
    }
}

impl Default for Tune {
    fn default() -> Self {
        Self::from_char(&CharData::KNEEMAN)
    }
}

/// One frame of input, sampled at the edge and fed to the pure step.
#[derive(Copy, Clone, Default)]
pub struct InputFrame {
    pub dir: f32,            // stick x, -1..1
    pub aim_y: f32,          // stick y, -1 up .. +1 down (air-dodge / wavedash aim)
    pub jump: bool,          // jump pressed THIS frame (rising edge) -> full hop
    pub jump_held: bool,     // jump currently held (release before takeoff = short hop)
    pub shorthop: bool,      // dedicated short-hop pressed THIS frame
    pub shield_held: bool,   // shield button held (grounded -> Shield)
    pub shield_pressed: bool,// shield pressed THIS frame (airborne -> AirDodge)
    pub down: bool,          // down held (fast fall / spot dodge / soft-platform drop)
    pub down_pressed: bool,  // down pressed THIS frame (deliberate ledge drop)
    pub attack: bool,        // attack pressed THIS frame (jab / aerial / pickup / fire)
    pub attack_held: bool,   // attack currently held (full-auto gun fire)
    pub grab: bool,          // grab pressed THIS frame (drop a held item)
    pub special: bool,       // special (B) pressed THIS frame; stick at press picks N/Side/Up/Down
}

/// PURE scan step: (state, input, tune) -> next state.
/// No engine calls, no IO, no &mut self. Deterministic given the same inputs.
/// `states = inputs.scan(SimState::spawn(), step)`.
/// One tick of the whole sim: advance each fighter from its own input, then resolve combat
/// both directions. Pure value-in/value-out — this is what ggrs calls (possibly N times per
/// frame during rollback). `inputs[k]` drives `fighters[k]`.
pub fn step(s: &SimState, inputs: [&InputFrame; 2], t: &Tune) -> SimState {
    let mut n = *s;
    n.tick = n.tick.wrapping_add(1);
    maybe_spawn_item(&mut n, t);

    // Each fighter's FSM scans its raw input and emits a pure "next action" (Act). The input is never
    // mutated; `apply_act` actuates the descriptor into the whole SimState (spawn bolt / drop / grab).
    let items0 = n.items; // read-only snapshot so the FSM can decide pickup-vs-jab without borrowing
    let act0 = advance(&mut n.fighters[0], &items0, inputs[0], t);
    let items1 = n.items;
    let act1 = advance(&mut n.fighters[1], &items1, inputs[1], t);
    apply_act(&mut n, 0, act0, t);
    apply_act(&mut n, 1, act1, t);
    // split the array so both fighters can be borrowed mutably at once. The victim's stick this frame
    // feeds trajectory DI, so each call passes the defender's aim.
    let aim0 = Vector2::new(inputs[0].dir, inputs[0].aim_y);
    let aim1 = Vector2::new(inputs[1].dir, inputs[1].aim_y);
    let (l, r) = n.fighters.split_at_mut(1);
    resolve_combat(&mut l[0], &mut r[0], aim1, t); // p0 attacks p1 (victim p1 DIs)
    resolve_combat(&mut r[0], &mut l[0], aim0, t); // p1 attacks p0 (victim p0 DIs)
    // grabs: catch / hold / pummel / throw / mash-out. Cross-fighter, so it owns the held pair.
    resolve_grab(&mut l[0], &mut r[0], 0, 1, inputs[0], inputs[1], t); // p0 grabs p1
    resolve_grab(&mut r[0], &mut l[0], 1, 0, inputs[1], inputs[0], t); // p1 grabs p0

    update_items(&mut n, t); // move bolts, follow held guns, resolve bolt hits
    n
}

/// Advance ONE fighter by one frame from its own input: buffer, state machine, integrate +
/// stage collision. No cross-fighter combat (that is `resolve_combat`). Mutates in place.
fn advance(f: &mut Fighter, items: &[Item; MAX_ITEMS], i: &InputFrame, t: &Tune) -> Act {
    let mut n = *f;
    let prev = n.state;
    let mut force_reset = false; // re-enter same state (dash-dance) -> reset the frame timer
    let sgn = sign(i.dir);
    let mag = i.dir.abs();
    let aim = Vector2::new(i.dir, i.aim_y);

    // impact freeze: on a connect a fighter holds for a few frames (hit "pop"). Nothing
    // advances during hitlag — not the frame timer, not motion, not the buffer.
    if n.hitlag > 0 {
        n.hitlag -= 1;
        *f = n;
        return Act::None;
    }

    // held (grabber or victim): freeze the FSM entirely. `resolve_grab` (cross-fighter) owns the
    // hold: it repositions the victim, runs pummel/throw/mash, and releases. If the link is gone
    // (released this frame), fall back to neutral and let the normal machine resume.
    if matches!(n.state, CharState::GrabHold | CharState::Grabbed) {
        if n.grab_link < 0 {
            n.state = CharState::Stand;
            n.frame = 0;
            *f = n;
            return Act::None;
        }
        n.vel = Vector2::ZERO;
        n.frame += 1;
        *f = n;
        return Act::None;
    }

    // launched: skip the state machine, run the knockback slide (the old training-dummy physics).
    // Friction bleeds horizontal, gravity arcs it down, feet settle on the floor, hitstun ticks.
    if n.hitstun > 0 {
        // tech buffer: a shield press during hitstun arms a tech for `tech_window` frames.
        if n.tech_buf > 0 {
            n.tech_buf -= 1;
        }
        if i.shield_pressed {
            n.tech_buf = t.tech_window as u8;
        }
        let was_air = f.pos.y < GROUND_Y - 0.5; // above the floor at the start of this frame
        n.pos += n.vel * DT;
        n.vel.x = move_toward(n.vel.x, 0.0, DUMMY_FRICTION * DT);
        let mut landed = false;
        if n.pos.y < GROUND_Y {
            n.vel.y += t.gravity * DT; // arc back down
        } else {
            if was_air {
                landed = true; // crossed into the floor this frame
            }
            n.pos.y = GROUND_Y;
            n.vel.y = 0.0;
        }
        n.hitstun -= 1;

        // hard launch hitting the floor: tech it (intangible recovery) or eat a knockdown.
        if landed && n.tumble {
            n.hitstun = 0;
            n.tumble = false;
            n.ground_plat = 0;
            n.frame = 0;
            if n.tech_buf > 0 {
                n.tech_buf = 0;
                n.intangible = true; // tech i-frames start immediately (this branch early-returns)
                if sgn != 0.0 && mag >= DASH_THRESH {
                    n.facing = sgn;
                    n.vel.x = sgn * t.techroll_speed;
                    n.state = CharState::TechRoll; // teched with a roll
                } else {
                    n.vel.x = 0.0;
                    n.state = CharState::TechInPlace; // teched in place
                }
            } else {
                n.vel.x = 0.0;
                n.state = CharState::Knockdown; // missed the tech -> floored
            }
            *f = n;
            return Act::None;
        }

        if n.hitstun == 0 {
            // light launch / drifted out: recover normally (airborne -> Air, floor -> Stand).
            n.tumble = false;
            if n.pos.y < GROUND_Y {
                n.state = CharState::Air;
                n.ground_plat = -1;
            } else {
                n.state = CharState::Stand;
                n.ground_plat = 0;
            }
        }
        if out_of_bounds(n.pos) {
            *f = respawn(n.pos.x, n.facing, t);
            return Act::None;
        }
        *f = n;
        return Act::None;
    }

    if n.regrab_lock > 0 {
        n.regrab_lock -= 1;
    }

    // ── input buffer (part 1): age every lane once, refresh the movement-lane diagonal, and record
    // the edges that don't depend on item context (movement + grab). Lanes coexist; within the
    // movement lane the newest of jump / short-hop / air-dodge wins. ──
    for s in &mut n.buf {
        if s.timer > 0 {
            s.timer -= 1;
        }
    }
    if n.coyote > 0 {
        n.coyote -= 1;
    }
    if n.invuln > 0 {
        n.invuln -= 1;
    }
    {
        let m = &mut n.buf[Lane::Movement as usize];
        if m.timer > 0 && aim.length() > 0.3 {
            m.aim = aim; // latest non-neutral aim within the window wins (the diagonal)
        }
    }
    if i.grab {
        n.record(Lane::Grab, Action::Grab, aim, t);
    }
    if i.special {
        n.record(Lane::Special, Action::Special, aim, t);
    }
    if i.shorthop {
        n.record(Lane::Movement, Action::ShortHop, aim, t);
    } else if i.jump {
        n.record(Lane::Movement, Action::Jump, aim, t);
    }
    // shield press only buffers an air dodge when airborne or mid-jumpsquat (else it's a shield)
    if i.shield_pressed && (airborne(n.state) || n.state == CharState::JumpSquat) {
        n.record(Lane::Movement, Action::AirDodge, aim, t);
    }

    // ── item intents: emit a pure descriptor; do NOT mutate the input. `advance` only reads `items`
    // (to know if an attack should grab vs jab); `apply_act` does the mutation. ──
    //   holding: grab=drop, attack(held)=fire (full-auto, weaker when held not freshly tapped)
    //   empty + grounded over an item: attack=pickup; else attack=jab/aerial as normal
    let holding = n.holding >= 0;
    let pickup_target = if holding { None } else { nearest_pickup(&n, items) };
    let grab = n.live(Lane::Grab) == Action::Grab;
    let mut act = Act::None;
    if holding {
        if grab {
            act = Act::Drop;
        } else if i.attack || i.attack_held {
            act = Act::Fire { auto: i.attack_held && !i.attack };
        }
    } else if i.attack && pickup_target.is_some() {
        act = Act::Pickup;
    }
    let fire = matches!(act, Act::Fire { .. });
    let grabbing = matches!(act, Act::Pickup | Act::Drop);
    let atk = i.attack && !fire && !grabbing; // effective attack for jab/aerial
    // grab button with empty hands + no item interaction = a fighter-grab attempt (grounded only).
    let grab_now = grab && !grabbing && !holding;

    // ── input buffer (part 2): the attack edge, now that item context (fire/grab) is resolved.
    // Aerial and Attack are separate lanes, so a jump+attack combo holds both at once (the
    // auto-short-hop macro). Queue an aerial when airborne, mid-jumpsquat, or pressed together with
    // a jump; a grounded attack alone queues a jab that fires on the next actionable ground frame. ──
    let jumping_now = i.jump || i.shorthop;
    if atk {
        if airborne(n.state) || n.state == CharState::JumpSquat || jumping_now {
            n.record(Lane::Aerial, Action::Aerial, aim, t);
        } else {
            n.record(Lane::Attack, Action::Attack, aim, t);
        }
    }

    // match on a Copy of the state so arm guards (e.g. try_special) can mutate `n`.
    let cur_state = n.state;
    match cur_state {
        CharState::Stand => {
            if !try_ground_action(&mut n, i, atk, grab_now, t) {
                if i.down {
                    if sgn != 0.0 {
                        n.facing = sgn;
                    }
                    n.state = CharState::Crouch;
                } else if sgn != 0.0 && mag >= DASH_THRESH {
                    n.facing = sgn;
                    n.vel.x = sgn * t.dash_init; // initial dash burst impulse
                    n.state = CharState::Dash;
                } else if sgn != 0.0 && mag >= WALK_THRESH {
                    n.facing = sgn;
                    n.state = CharState::Walk;
                } else {
                    n.vel.x = move_toward(n.vel.x, 0.0, t.ground_friction * DT);
                }
            }
        }
        CharState::Walk => {
            if !try_ground_action(&mut n, i, atk, grab_now, t) {
                if mag < WALK_THRESH {
                    n.state = CharState::Stand;
                } else if sgn != n.facing {
                    n.state = CharState::Turn; // standing pivot
                } else {
                    n.vel.x = move_toward(n.vel.x, sgn * t.walk_speed, t.ground_accel * DT);
                }
            }
        }
        CharState::Dash => {
            if !try_ground_action(&mut n, i, atk, grab_now, t) {
                if sgn != 0.0 && sgn != n.facing && mag >= DASH_THRESH {
                    // dash-dance: flip facing + restart the window, but DON'T teleport velocity.
                    // Old momentum bleeds across 0 in the accel branch below, so a fast wrong-way
                    // flick costs distance/time. No more instant free reversal.
                    n.facing = sgn;
                    force_reset = true;
                } else if mag < WALK_THRESH {
                    n.state = CharState::Skid; // release mid-dash -> slide to a stop (dashstop)
                } else {
                    // fighting your own momentum (vel still points the old way) brakes at
                    // dash_turn_accel; once vel agrees with facing, normal dash accel toward run.
                    let a = if sign(n.vel.x) == -n.facing { t.dash_turn_accel } else { t.ground_accel };
                    n.vel.x = move_toward(n.vel.x, n.facing * t.run_speed, a * DT);
                    if n.frame >= t.dash_window {
                        n.state = CharState::Run;
                    }
                }
            }
        }
        CharState::Run => {
            if !try_ground_action(&mut n, i, atk, grab_now, t) {
                if mag < WALK_THRESH || sgn != n.facing {
                    n.state = CharState::Skid; // release or reverse -> run brake
                } else {
                    n.vel.x = move_toward(n.vel.x, n.facing * t.run_speed, t.ground_accel * DT);
                }
            }
        }
        CharState::Turn => {
            n.vel.x = move_toward(n.vel.x, 0.0, t.ground_friction * DT);
            if !try_ground_action(&mut n, i, atk, grab_now, t) && n.frame >= t.pivot_frames {
                n.facing = -n.facing;
                if sgn != 0.0 && mag >= DASH_THRESH {
                    // standing pivot already bled momentum to ~0 over pivot_frames, so this is a
                    // fresh dash from rest: full initial burst, same as dashing from neutral.
                    n.vel.x = n.facing * t.dash_init;
                    n.state = CharState::Dash;
                } else if sgn != 0.0 && mag >= WALK_THRESH {
                    n.state = CharState::Walk;
                } else {
                    n.state = CharState::Stand;
                }
            }
        }
        CharState::Skid => {
            if !try_ground_action(&mut n, i, atk, grab_now, t) {
                if sgn != 0.0 && sgn != n.facing && mag >= DASH_THRESH {
                    // pivot out of the skid: flip into Dash and let it bleed the leftover braking
                    // momentum through 0 at dash_turn_accel (no teleport). The state change resets
                    // the dash window.
                    n.facing = sgn;
                    n.state = CharState::Dash;
                } else {
                    n.vel.x = move_toward(n.vel.x, 0.0, t.dashstop_friction * DT);
                    if n.vel.x.abs() < STOP_EPS {
                        n.vel.x = 0.0;
                        n.state = CharState::Stand;
                    }
                }
            }
        }
        CharState::Crouch => {
            // hold down to stay crouched; jump/shield available; release down -> stand.
            // bleed any residual run momentum to a stop while squatting.
            n.vel.x = move_toward(n.vel.x, 0.0, t.ground_friction * DT);
            if !try_ground_action(&mut n, i, atk, grab_now, t) && !i.down {
                n.state = CharState::Stand;
            }
        }
        CharState::Landing => {
            n.vel.x = move_toward(n.vel.x, 0.0, t.ground_friction * DT);
            if let Some(full) = take_jump(&mut n) {
                n.state = CharState::JumpSquat;
                n.full_hop = full;
            } else if n.frame >= t.landing_lag {
                n.state = CharState::Stand;
            }
        }
        CharState::Shield => {
            // jump out of shield, drop shield, roll, or spot dodge
            if let Some(full) = take_jump(&mut n) {
                n.state = CharState::JumpSquat;
                n.full_hop = full;
            } else if !i.shield_held {
                n.state = CharState::Stand;
            } else if sgn != 0.0 && mag >= DASH_THRESH {
                n.facing = sgn;
                n.vel.x = sgn * t.roll_speed;
                n.state = CharState::Roll;
            } else if i.down {
                n.vel.x = 0.0;
                n.state = CharState::SpotDodge;
            } else {
                n.vel.x = move_toward(n.vel.x, 0.0, t.ground_friction * DT);
            }
        }
        CharState::SpotDodge => {
            n.vel.x = move_toward(n.vel.x, 0.0, t.ground_friction * DT);
            if n.frame >= t.spotdodge_frames {
                n.state = if i.shield_held { CharState::Shield } else { CharState::Stand };
            }
        }
        CharState::Roll => {
            // hold the roll velocity, then end (intangible mid-roll via the i-frame window)
            if n.frame >= t.roll_frames {
                n.vel.x = 0.0;
                n.state = if i.shield_held { CharState::Shield } else { CharState::Stand };
            }
        }
        CharState::JumpSquat => {
            // ground physics keep running during the squat. Hold the dash dir -> accelerate
            // toward run speed (full dash-jump carry); go neutral -> friction bleeds vel.x,
            // so jumping out of a dash-stop transfers little momentum. This is the last
            // actionable window to set direction before the air locks it.
            if sgn != 0.0 {
                n.facing = sgn;
            }
            if sgn == 0.0 {
                n.vel.x = move_toward(n.vel.x, 0.0, t.ground_friction * DT);
            } else {
                n.vel.x = move_toward(n.vel.x, sgn * t.run_speed, t.ground_accel * DT);
            }
            if !i.jump_held && n.full_hop {
                n.full_hop = false; // released before takeoff -> short hop
            }
            if n.frame >= t.jumpsquat - 1 {
                let wavedash = n.live(Lane::Movement) == Action::AirDodge;
                if wavedash {
                    let aim = dodge_aim(&n, i);
                    n.clear_lane(Lane::Movement);
                    do_airdodge(&mut n, aim, t); // wavedash: airdodge straight out of jumpsquat
                } else {
                    // jump+attack combo = auto short-hop aerial (Ultimate): force a short hop and
                    // tag the aerial for reduced damage. Set before vel.y so the hop comes out short.
                    if n.live(Lane::Aerial) == Action::Aerial {
                        n.full_hop = false;
                        n.autohop_aerial = true;
                    }
                    n.vel.y = if n.full_hop { t.fullhop_v } else { t.shorthop_v };
                    // keep ground momentum * carry, ADD stick contribution, clamp to a cap
                    // that sits ABOVE run speed so a dash-jump does NOT lose speed.
                    let h = n.vel.x * t.momentum_carry + i.dir * t.jump_h_init;
                    n.vel.x = h.clamp(-t.jump_h_max, t.jump_h_max);
                    n.state = CharState::Air; // air_jumps/dodges already set from ground contact
                }
            }
        }
        CharState::Air if try_special(&mut n) => {
            // entered a special from the air; the SpecialX arm runs from frame 0 next tick
        }
        CharState::Air => {
            let want_dodge = n.live(Lane::Movement) == Action::AirDodge;
            let buffered_aerial = n.live(Lane::Aerial) == Action::Aerial;
            let want_aerial = atk || buffered_aerial;
            if want_aerial {
                // a same-frame press has no captured aim yet; read it live in that case.
                let aim = if buffered_aerial {
                    n.buf[Lane::Aerial as usize].aim
                } else {
                    Vector2::new(i.dir, i.aim_y)
                };
                n.clear_lane(Lane::Aerial);
                n.state = aerial_for(aim, t); // nair or dair, by the captured stick direction
                n.attack_hit = false;
            } else if want_dodge && n.air_dodges > 0 {
                let a = dodge_aim(&n, i);
                n.clear_lane(Lane::Movement);
                do_airdodge(&mut n, a, t); // directional burst; into the ground = wavedash
            } else {
                // double jump: cancels fall (crisp upward pop even while falling fast) and
                // REDIRECTS horizontal from the stick — hold back to reverse momentum.
                let want_djump = matches!(n.live(Lane::Movement), Action::Jump | Action::ShortHop);
                if want_djump && n.coyote > 0 {
                    // coyote jump: walked off the lip a few frames ago, so this is still the
                    // GROUNDED jump (full/short by which lane), instant (no jumpsquat), and it
                    // does NOT spend the air jump. Fixes "lost a jump the instant I left the edge".
                    let full = n.live(Lane::Movement) == Action::Jump;
                    n.clear_lane(Lane::Movement);
                    n.coyote = 0;
                    n.vel.y = if full { t.fullhop_v } else { t.shorthop_v };
                    n.fast_falling = false;
                    let h = n.vel.x * t.momentum_carry + i.dir * t.jump_h_init;
                    n.vel.x = h.clamp(-t.jump_h_max, t.jump_h_max);
                } else if want_djump && n.air_jumps > 0 {
                    n.clear_lane(Lane::Movement);
                    n.air_jumps -= 1;
                    n.vel.y = t.airjump_v;
                    n.fast_falling = false;
                    if sgn != 0.0 {
                        // momentum redirects with the stick, but facing does NOT flip: an air jump
                        // can't turn you around (only a turnaround special could). Ult-style.
                        let dj = sgn * t.airjump_h;
                        // hold AWAY -> reverse to fresh horizontal; hold TOWARD -> keep your
                        // speed, never slow below airjump_h.
                        n.vel.x = if sign(n.vel.x) != sgn {
                            dj
                        } else {
                            sgn * n.vel.x.abs().max(t.airjump_h.abs())
                        };
                    }
                    // neutral stick: keep current horizontal momentum
                }
                air_drift(&mut n, i, t, sgn);
                // fast fall (instant snap) + gravity. Gate on a STEEP-down stick: aim_y past the
                // threshold AND more vertical than horizontal, so down-forward drifting doesn't
                // accidentally fast fall (digital down alone still triggers: dir=0).
                if !n.fast_falling
                    && n.vel.y > 0.0
                    && i.aim_y >= t.fastfall_threshold
                    && i.aim_y > i.dir.abs()
                {
                    n.fast_falling = true;
                }
                if n.fast_falling {
                    n.vel.y = t.fastfall;
                } else {
                    n.vel.y += t.gravity * DT;
                    if n.vel.y > t.max_fall {
                        n.vel.y = t.max_fall;
                    }
                }
            }
        }
        CharState::AirDodge => {
            // burst decays (drag) so an open-air dodge lunges and settles instead of flying;
            // a wavedash lands within a frame or two so its horizontal is still mostly intact.
            n.vel.x = move_toward(n.vel.x, 0.0, t.airdodge_drag * DT);
            n.vel.y = move_toward(n.vel.y, 0.0, t.airdodge_drag * DT);
            if n.frame >= t.airdodge_frames {
                n.vel.y = 0.0;
                n.state = CharState::Air; // actionable again (Ultimate-style, not helpless)
            }
        }
        CharState::LedgeHold => {
            if take_jump(&mut n).is_some() {
                n.vel.y = t.ledgejump_v;
                n.vel.x = n.facing * t.jump_h_init; // hop toward the stage
                n.state = CharState::Air;
                n.regrab_lock = 20;
            } else if (sgn == n.facing && mag >= WALK_THRESH) || i.shield_held {
                n.state = CharState::LedgeClimb; // hold toward stage (or shield) = getup
            } else if (sgn == -n.facing && mag >= WALK_THRESH) || i.down_pressed {
                // away from the stage, or a DELIBERATE down tap — not a held-down from the
                // fast-fall into the grab (that would slip you straight off the lip).
                n.state = CharState::Air; // drop off
                n.regrab_lock = 20;
            }
            // else keep hanging (position is fixed by the integrate block)
        }
        CharState::LedgeClimb => {
            if n.frame >= t.climb_frames {
                // teleport onto the platform just inside the edge we were facing
                n.pos.x = if n.facing > 0.0 {
                    FLOOR_LEFT + 30.0
                } else {
                    FLOOR_RIGHT - 30.0
                };
                n.pos.y = GROUND_Y;
                n.vel = Vector2::ZERO;
                n.ground_plat = 0;
                n.state = CharState::Stand;
            }
        }
        CharState::Jab => {
            // grounded swing: hard brake to a planted stop, run out the frame data, then neutral.
            n.vel.x = move_toward(n.vel.x, 0.0, t.ground_friction * 3.0 * DT);
            let atk = attack_for(t, CharState::Jab).unwrap();
            if n.frame >= atk.total() - 1 {
                n.state = if i.shield_held { CharState::Shield } else { CharState::Stand };
            }
        }
        CharState::DashAttack => {
            // lunge: slide through the swipe carrying the lunge speed (barely any friction), then
            // brake hard once the endlag starts so the commitment still plants you. No steering.
            let atk = attack_for(t, CharState::DashAttack).unwrap();
            let sliding = n.frame < atk.startup + atk.active; // startup + active window = the drive
            let fric = if sliding { t.dashstop_friction * 0.12 } else { t.dashstop_friction };
            n.vel.x = move_toward(n.vel.x, 0.0, fric * DT);
            if n.frame >= atk.total() - 1 {
                n.state = if i.shield_held { CharState::Shield } else { CharState::Stand };
            }
        }
        CharState::Grab => {
            // reach planted in place; run startup + active + heavy whiff recovery, then neutral.
            // The catch itself lives in `resolve_grab` (it needs the other fighter); on a catch that
            // flips this fighter to GrabHold before the next frame reaches this arm.
            n.vel.x = move_toward(n.vel.x, 0.0, t.ground_friction * 3.0 * DT);
            let total = t.grab_startup + t.grab_active + t.grab_recovery;
            if n.frame >= total - 1 {
                n.state = if i.shield_held { CharState::Shield } else { CharState::Stand };
            }
        }
        // held states are early-returned above; arms exist only for match exhaustiveness.
        CharState::GrabHold | CharState::Grabbed => {}
        CharState::Knockdown => {
            // floored: slide to a stop, unactionable briefly, then getup options / auto-getup.
            n.vel.x = move_toward(n.vel.x, 0.0, t.ground_friction * DT);
            if n.frame >= t.knockdown_frames {
                n.state = CharState::Getup; // lay too long -> stand up automatically
            } else if n.frame >= KNOCKDOWN_LOCK {
                if i.attack {
                    n.attack_hit = false;
                    n.state = CharState::Jab; // getup attack
                } else if sgn != 0.0 && mag >= DASH_THRESH {
                    n.facing = sgn;
                    n.vel.x = sgn * t.techroll_speed;
                    n.state = CharState::TechRoll; // getup roll
                } else if i.jump || i.shorthop || i.shield_pressed || i.aim_y <= -0.4 {
                    n.state = CharState::Getup; // neutral getup
                }
            }
        }
        CharState::Getup => {
            n.vel.x = move_toward(n.vel.x, 0.0, t.ground_friction * DT);
            if n.frame >= t.getup_frames {
                n.state = CharState::Stand;
            }
        }
        CharState::TechInPlace => {
            n.vel.x = move_toward(n.vel.x, 0.0, t.ground_friction * 2.0 * DT);
            if n.frame >= t.tech_intang {
                n.state = CharState::Stand;
            }
        }
        CharState::TechRoll => {
            // roll across the ground (intangible), then settle to neutral.
            if n.frame >= t.techroll_frames {
                n.vel.x = 0.0;
                n.state = CharState::Stand;
            }
        }
        CharState::Nair => {
            // aerial: drift + gravity still apply; ends back to Air (or lands via integrate).
            air_drift(&mut n, i, t, sgn);
            n.vel.y += t.gravity * DT;
            if n.vel.y > t.max_fall {
                n.vel.y = t.max_fall;
            }
            let atk = attack_for(t, CharState::Nair).unwrap();
            if n.frame >= atk.total() - 1 {
                n.state = CharState::Air;
                n.autohop_aerial = false;
            }
        }
        CharState::Dair => {
            // down aerial: reduced air control (it's a commitment), gravity still pulls, ends to Air.
            air_drift(&mut n, i, t, sgn);
            n.vel.y += t.gravity * DT;
            if n.vel.y > t.max_fall {
                n.vel.y = t.max_fall;
            }
            let atk = attack_for(t, CharState::Dair).unwrap();
            if n.frame >= atk.total() - 1 {
                n.state = CharState::Air;
                n.autohop_aerial = false;
            }
        }
        CharState::SpecialN => run_special(&mut n, 0, i, t),
        CharState::SpecialS => run_special(&mut n, 1, i, t),
        CharState::SpecialU => run_special(&mut n, 2, i, t),
        CharState::SpecialD => run_special(&mut n, 3, i, t),
        CharState::Helpless => {
            // special-fall: drift only, gravity pulls, no actions until you land (integrate -> Landing)
            air_drift(&mut n, i, t, sgn);
            n.vel.y += t.gravity * DT;
            if n.vel.y > t.max_fall {
                n.vel.y = t.max_fall;
            }
        }
    }

    // ── integrate + collide ─────────────────────────────────────────────────
    let prev_y = f.pos.y; // feet-y before this frame's motion (for platform crossing tests)
    if airborne(n.state) {
        n.pos += n.vel * DT;

        // ledge snap: only while actually falling, off the side, in the lip's y-window (main stage)
        if n.state == CharState::Air && n.vel.y > LEDGE_FALL_EPS && n.regrab_lock == 0 {
            let in_y = n.pos.y >= GROUND_Y - 20.0 && n.pos.y <= GROUND_Y + 90.0;
            let near_right = n.pos.x >= FLOOR_RIGHT && n.pos.x <= FLOOR_RIGHT + LEDGE_REACH_X;
            let near_left = n.pos.x <= FLOOR_LEFT && n.pos.x >= FLOOR_LEFT - LEDGE_REACH_X;
            if in_y && near_right && sgn <= 0.5 {
                grab_ledge(&mut n, t, FLOOR_RIGHT, -1.0);
            } else if in_y && near_left && sgn >= -0.5 {
                grab_ledge(&mut n, t, FLOOR_LEFT, 1.0);
            }
        }

        // platform landing: crossed a platform top from above while descending. Soft platforms
        // are skipped while holding down (drop-through); the solid main stage always catches.
        if airborne(n.state) && n.vel.y >= 0.0 {
            for (idx, p) in PLATFORMS.iter().enumerate() {
                let in_x = n.pos.x >= p.left && n.pos.x <= p.right;
                let crossed = prev_y <= p.y + 1.0 && n.pos.y >= p.y;
                // soft platforms drop through while holding down — UNLESS this is an air dodge
                // (wavedash), where the down is the dodge aim, not a drop command.
                let land = p.solid || !i.down || n.state == CharState::AirDodge;
                if in_x && crossed && land {
                    n.pos.y = p.y;
                    n.vel.y = 0.0;
                    n.fast_falling = false;
                    n.air_jumps = t.max_air_jumps as u8;
                    n.air_dodges = t.max_air_dodges as u8;
                    n.coyote = 0; // landed: the grace window is spent
                    n.ground_plat = idx as i32;
                    n.state = CharState::Landing; // carries vel.x -> Landing friction = slide
                    break;
                }
            }
        }

        // solid-stage walls: keep the ECB's side verts out of the main platform's vertical faces.
        // Only engages when the diamond CENTER is below the top lip (recovering from the side),
        // so it never blocks you while standing on top.
        if airborne(n.state) {
            let cy = n.pos.y - ECB_HALF_H; // diamond center y
            if cy > GROUND_Y && cy < STAGE_BOTTOM {
                if n.pos.x < FLOOR_LEFT && n.pos.x + ECB_HALF_W > FLOOR_LEFT {
                    n.pos.x = FLOOR_LEFT - ECB_HALF_W; // right vert stops on the left wall
                    if n.vel.x > 0.0 {
                        n.vel.x = 0.0;
                    }
                } else if n.pos.x > FLOOR_RIGHT && n.pos.x - ECB_HALF_W < FLOOR_RIGHT {
                    n.pos.x = FLOOR_RIGHT + ECB_HALF_W; // left vert stops on the right wall
                    if n.vel.x < 0.0 {
                        n.vel.x = 0.0;
                    }
                }
            }
        }
    } else if is_special(n.state) {
        // specials integrate by where they launched: aerial (ground_plat < 0) falls + lands on a
        // platform top -> Landing; grounded stays pinned (the planted punch). Main floor + soft tops.
        if n.ground_plat < 0 {
            n.pos += n.vel * DT;
            if n.vel.y >= 0.0 {
                for (idx, p) in PLATFORMS.iter().enumerate() {
                    let in_x = n.pos.x >= p.left && n.pos.x <= p.right;
                    let crossed = prev_y <= p.y + 1.0 && n.pos.y >= p.y;
                    if in_x && crossed && (p.solid || !i.down) {
                        n.pos.y = p.y;
                        n.vel.y = 0.0;
                        n.air_jumps = t.max_air_jumps as u8;
                        n.air_dodges = t.max_air_dodges as u8;
                        n.ground_plat = idx as i32;
                        n.state = CharState::Landing;
                        break;
                    }
                }
            }
        } else {
            let p = PLATFORMS[n.ground_plat.clamp(0, PLATFORMS.len() as i32 - 1) as usize];
            n.pos.x += n.vel.x * DT;
            n.pos.y = p.y;
            n.vel.y = 0.0;
        }
    } else if is_ledge(n.state) {
        // hanging / climbing: position is fixed (set on grab and at climb end)
    } else {
        // grounded: pinned to its platform, no vertical motion
        let p = PLATFORMS[n.ground_plat.clamp(0, PLATFORMS.len() as i32 - 1) as usize];
        n.pos.x += n.vel.x * DT;
        if !p.solid && i.down && n.state != CharState::JumpSquat {
            // drop through the soft platform we're standing on (but not mid-jumpsquat: the
            // down there is a wavedash aim, and we're committed to the jump)
            n.state = CharState::Air;
            n.ground_plat = -1;
            n.coyote = t.coyote_frames as u8;
        } else {
            n.pos.y = p.y;
            n.vel.y = 0.0;
            // edges are sticky: only walk off when actively holding toward the edge, else
            // stop at the lip. Falling off no longer happens just from sliding momentum.
            if n.pos.x < p.left {
                if sgn < 0.0 {
                    n.state = CharState::Air;
                    n.ground_plat = -1;
                    n.coyote = t.coyote_frames as u8;
                    if p.solid {
                        n.regrab_lock = 12;
                    }
                } else {
                    n.pos.x = p.left;
                    n.vel.x = 0.0;
                }
            } else if n.pos.x > p.right {
                if sgn > 0.0 {
                    n.state = CharState::Air;
                    n.ground_plat = -1;
                    n.coyote = t.coyote_frames as u8;
                    if p.solid {
                        n.regrab_lock = 12;
                    }
                } else {
                    n.pos.x = p.right;
                    n.vel.x = 0.0;
                }
            }
        }
    }

    // the auto-short-hop tag lives only for its one aerial; clear it the moment we touch down.
    if !airborne(n.state) {
        n.autohop_aerial = false;
    }

    // blast zone (any of 4 edges) -> respawn this fighter (combat lives in resolve_combat, not here)
    if out_of_bounds(n.pos) {
        *f = respawn(n.pos.x, n.facing, t);
        return Act::None;
    }

    // frame counter resets on transition (or a forced re-enter), else advances
    n.frame = if n.state != prev || force_reset {
        0
    } else {
        n.frame + 1
    };

    // i-frames drive the debug color
    n.intangible = match n.state {
        CharState::SpotDodge | CharState::Roll | CharState::AirDodge => true,
        CharState::TechInPlace | CharState::TechRoll => true, // teching is fully intangible
        CharState::Getup => n.frame < t.tech_intang,          // getup i-frames taper off
        CharState::LedgeHold => n.frame < t.ledge_intang,
        _ => false,
    };
    *f = n;
    act
}

/// Which side to respawn on after a blast-zone KO (keep the fighter on its half of the stage).
fn spawn_x(x: f32) -> f32 {
    if x < 600.0 {
        480.0
    } else {
        720.0
    }
}

/// Fresh fighter after a KO: clean spawn (0%) plus a window of i-frames so the player isn't combo'd
/// off the respawn point. `t.spawn_iframes` sizes the window.
fn respawn(x: f32, facing: f32, t: &Tune) -> Fighter {
    let mut f = Fighter::spawn(spawn_x(x), facing);
    f.invuln = t.spawn_iframes as u8;
    f
}

/// Trajectory DI: the victim's stick rotates a launch toward its component perpendicular to the
/// knockback, up to `max_deg`. Speed is untouched -- only the angle -- so you can steer a launch
/// toward the stage to live, but never cancel your own knockback. Pure, so it rolls back cleanly.
fn apply_di(vel: Vector2, stick: Vector2, max_deg: f32) -> Vector2 {
    let speed = vel.length();
    if speed < 1.0 || stick.length() < 0.3 {
        return vel; // no knockback worth steering, or stick inside the deadzone
    }
    let u = vel / speed; // unit trajectory
    let s = stick.clamp_length_max(1.0);
    let cross = (u.x * s.y - u.y * s.x).clamp(-1.0, 1.0); // signed perpendicular component
    let (sin, cos) = (max_deg.to_radians() * cross).sin_cos();
    Vector2::new(vel.x * cos - vel.y * sin, vel.x * sin + vel.y * cos)
}

/// Cross-fighter combat: `a`'s active hitbox vs `b`'s hurtbox (circle/circle), one hit per swing.
/// On connect: damage + knockback + hitstun to `b`, impact freeze (hitlag) to BOTH (the hit "pop").
fn resolve_combat(a: &mut Fighter, b: &mut Fighter, b_aim: Vector2, t: &Tune) {
    let Some((hc, hr)) = active_hitbox(a, t) else { return };
    if a.attack_hit {
        return; // already connected this swing
    }
    if b.invuln > 0 || b.intangible {
        return; // spawn i-frames / active dodge: no hit lands
    }
    let (bc, br) = hurtbox(b);
    if (hc - bc).length() > hr + br {
        return; // no overlap
    }
    let atk = attack_for(t, a.state).unwrap();
    a.attack_hit = true;
    let dmg = if matches!(a.state, CharState::Nair | CharState::Dair) && a.autohop_aerial {
        atk.damage * t.autohop_dmg // auto short-hop aerial: reduced damage (Ultimate)
    } else {
        atk.damage
    };
    b.damage += dmg;
    // knockback scales with accumulated %, then the global multiplier (tuned > 1 so kills happen)
    let speed = (atk.kb_base + atk.kb_scale * b.damage) * t.knockback_mult;
    let ang = atk.kb_angle.to_radians();
    b.vel = Vector2::new(ang.cos() * a.facing, -ang.sin()) * speed; // launch away from attacker
    b.vel = apply_di(b.vel, b_aim, t.di_max_angle); // victim angles the trajectory (survival DI)
    b.hitstun = (speed * 0.12) as i64; // stun scales with knockback
    b.tumble = speed > t.tumble_speed; // hard enough to knock down (or be teched) on landing
    let freeze = (atk.damage * HITLAG_PER_DMG) as i64 + 4; // both fighters pop on impact
    a.hitlag = freeze;
    b.hitlag = freeze;
}

// --- grabs ---------------------------------------------------------------------------------------

const GRAB_HELD_X: f32 = 64.0;  // how far in front of the grabber the victim is pinned
const GRAB_CATCH_R: f32 = 36.0; // slop added to the reach-vs-hurtbox catch test
const KNOCKDOWN_LOCK: i64 = 6;  // floored frames before any getup option is allowed

/// Count fresh button edges this frame — the victim "mashes" these to shorten the hold.
fn mash_count(i: &InputFrame) -> i64 {
    (i.attack as i64) + (i.jump as i64) + (i.shorthop as i64) + (i.special as i64)
        + (i.grab as i64) + (i.shield_pressed as i64)
}

/// Throw direction from the grabber's stick at release: up / down / back / forward (default).
fn throw_dir(i: &InputFrame, facing: f32) -> usize {
    if i.aim_y <= -0.4 {
        2 // up
    } else if i.aim_y >= 0.4 {
        3 // down
    } else if sign(i.dir) == -facing && i.dir.abs() >= 0.4 {
        1 // back
    } else {
        0 // forward
    }
}

/// Cut both fighters loose from a hold and stand them up (victim mashed out, or the grabber let go).
fn release_grab(g: &mut Fighter, v: &mut Fighter) {
    g.state = CharState::Stand;
    g.frame = 0;
    g.grab_link = -1;
    g.grab_timer = 0;
    v.state = CharState::Stand;
    v.frame = 0;
    v.grab_link = -1;
    v.grab_timer = 0;
    v.vel = Vector2::ZERO;
}

/// Launch the victim out of a throw, then unlink both. Grabber recovers to neutral.
fn do_throw(g: &mut Fighter, v: &mut Fighter, g_in: &InputFrame, t: &Tune) {
    let dir = throw_dir(g_in, g.facing);
    let td = t.throws[dir];
    v.damage += td.damage;
    let speed = (td.kb_base + td.kb_scale * v.damage) * t.knockback_mult;
    let sign_x = if dir == 1 { -g.facing } else { g.facing }; // back-throw fires behind the grabber
    let ang = td.kb_angle.to_radians();
    v.vel = Vector2::new(ang.cos() * sign_x, -ang.sin()) * speed;
    v.hitstun = (speed * 0.12) as i64;
    v.tumble = speed > t.tumble_speed;
    let freeze = (td.damage * HITLAG_PER_DMG) as i64 + 4;
    v.hitlag = freeze;
    g.hitlag = freeze;
    v.state = CharState::Air;
    v.ground_plat = -1;
    v.grab_link = -1;
    v.grab_timer = 0;
    g.state = CharState::Stand;
    g.frame = 0;
    g.grab_link = -1;
    g.grab_timer = 0;
}

/// Cross-fighter grab resolution for one ordered pair (grabber `g`, would-be victim `v`). Handles
/// the catch during the grab's active window, then maintains the hold: slaves the victim to the
/// grabber, runs pummel / throw on the grabber's inputs, and the victim's mash-out. Called both
/// orderings each frame (like `resolve_combat`); only the side actually grabbing does work.
fn resolve_grab(g: &mut Fighter, v: &mut Fighter, gi: i8, vi: i8, g_in: &InputFrame, v_in: &InputFrame, t: &Tune) {
    // 1) catch: the grab's reach overlaps a catchable victim during the active window.
    if g.state == CharState::Grab && g.grab_link < 0 {
        let active = g.frame >= t.grab_startup && g.frame < t.grab_startup + t.grab_active;
        let catchable = !matches!(v.state, CharState::Grabbed | CharState::GrabHold)
            && v.invuln == 0
            && !v.intangible
            && v.hitstun == 0;
        let reach = g.pos + Vector2::new(g.facing * t.grab_range, -ECB_HALF_H);
        let (vc, vr) = hurtbox(v);
        if active && catchable && (reach - vc).length() <= vr + GRAB_CATCH_R {
            g.state = CharState::GrabHold;
            g.frame = 0;
            g.grab_link = vi;
            g.grab_timer = t.grab_hold;
            g.vel = Vector2::ZERO;
            v.state = CharState::Grabbed;
            v.frame = 0;
            v.grab_link = gi;
            v.grab_timer = t.grab_hold;
            v.vel = Vector2::ZERO;
            v.hitstun = 0;
        }
        return;
    }

    // 2) maintain an existing hold (only the matching linked pair).
    if g.state == CharState::GrabHold && g.grab_link == vi && v.grab_link == gi {
        // slave the victim to the grabber's front, facing back at them.
        v.state = CharState::Grabbed;
        v.pos = Vector2::new(g.pos.x + g.facing * GRAB_HELD_X, g.pos.y);
        v.vel = Vector2::ZERO;
        v.facing = -g.facing;

        // victim mashes out: every fresh input chips the hold down faster.
        g.grab_timer -= 1 + mash_count(v_in) * t.grab_mash;

        // grabber intents: re-press grab = throw; tap attack = pummel; shield = let go.
        if g_in.grab {
            do_throw(g, v, g_in, t);
        } else if g_in.shield_pressed {
            release_grab(g, v);
        } else {
            if g_in.attack {
                v.damage += t.pummel_damage;
                g.grab_timer = (g.grab_timer + t.pummel_bonus).min(t.grab_hold);
            }
            if g.grab_timer <= 0 {
                release_grab(g, v); // mashed free / timed out
            }
        }
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
fn maybe_spawn_item(n: &mut SimState, t: &Tune) {
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
    let ammo = if kind == ItemKind::BobGun { t.bomb.ammo } else { t.laser.ammo };

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
    };
}

/// Actuate one fighter's emitted `Act` into the SimState. The only place item intents become item
/// effects; `advance` decided WHAT to do (purely), this carries it out on the shared item array.
fn apply_act(n: &mut SimState, idx: usize, act: Act, t: &Tune) {
    match act {
        Act::None => {}
        Act::Fire { auto } => fire_gun(n, idx, auto, t),
        Act::Drop => drop_item(n, idx),
        Act::Pickup => pickup_item(n, idx),
    }
}

/// Nearest unowned ground gun overlapping a grounded, actionable fighter (the pickup the attack
/// button claims instead of jabbing). None in the air / during hitstun so attack stays an aerial.
fn nearest_pickup(f: &Fighter, items: &[Item; MAX_ITEMS]) -> Option<usize> {
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
fn fire_gun(n: &mut SimState, idx: usize, auto: bool, t: &Tune) {
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
fn drop_item(n: &mut SimState, idx: usize) {
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
fn pickup_item(n: &mut SimState, idx: usize) {
    let f = n.fighters[idx];
    if let Some(k) = nearest_pickup(&f, &n.items) {
        n.items[k].owner = idx as i8;
        n.fighters[idx].holding = k as i8;
    }
}

/// Post-step item physics: ground guns fall + rest, held guns follow their owner (dropping if the
/// owner died/respawned), bolts fly + hit + expire.
fn update_items(n: &mut SimState, t: &Tune) {
    for k in 0..MAX_ITEMS {
        let it = n.items[k];
        if !it.active() {
            continue;
        }
        match it.kind {
            ItemKind::LaserGun | ItemKind::BobGun if it.owner >= 0 => {
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
            ItemKind::LaserGun | ItemKind::BobGun => {
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

/// Consume a buffered special if one is live: pick the slot from the captured stick, enter the
/// matching state, face the stick on a side-B. Available from every actionable ground/air state.
fn try_special(n: &mut Fighter) -> bool {
    if n.live(Lane::Special) != Action::Special {
        return false;
    }
    let aim = n.buf[Lane::Special as usize].aim;
    n.clear_lane(Lane::Special);
    // ground_plat lingers at its grounded platform index after a normal jump (it's only cleared on
    // walk-off / drop-through), so a special entered from the air would read as grounded: the punch
    // plants and the integrator pins pos.y to the platform (the "B-air teleports me to ground" bug).
    // Re-derive it from whether we're actually airborne at the press.
    if airborne(n.state) {
        n.ground_plat = -1;
    }
    let slot = special_dir(aim);
    if slot == 1 && aim.x != 0.0 {
        n.facing = sign(aim.x); // side-B turns you toward the stick
    }
    n.state = match slot {
        0 => CharState::SpecialN,
        1 => CharState::SpecialS,
        2 => CharState::SpecialU,
        _ => CharState::SpecialD,
    };
    n.attack_hit = false;
    true
}

/// Run one frame of a special. The launch burst lands when the active window opens; gravity/friction
/// run by whether we're airborne (`ground_plat < 0`). Up-B ends in Helpless if it finishes in the air.
fn run_special(n: &mut Fighter, slot: usize, i: &InputFrame, t: &Tune) {
    let m = t.specials[slot];
    // Up-B lifts off on frame 0 (instant recovery, no ground-snap); the rest burst at the active
    // window. The hitbox window (startup..) is independent of this movement timing.
    let launch_frame = if m.kind == SpecialKind::Rise { 0 } else { m.hit.startup };
    if n.frame == launch_frame {
        match m.kind {
            SpecialKind::Punch => n.vel.x = 0.0, // planted; vertical handled below (hover if aerial)
            SpecialKind::Lunge => {
                n.vel = Vector2::new(n.facing * m.move_x, m.move_y);
                n.ground_plat = -1;
            }
            SpecialKind::Rise => {
                n.vel = Vector2::new(i.dir * m.move_x, m.move_y);
                n.fast_falling = false;
                n.ground_plat = -1;
            }
            SpecialKind::Fall => n.vel = Vector2::new(n.facing * m.move_x, m.move_y),
        }
    }
    if n.ground_plat < 0 {
        match m.kind {
            // aerial neutral-B HOVERS in place (no snap-to-ground): bleed horizontal, almost no fall.
            SpecialKind::Punch => {
                n.vel.x = move_toward(n.vel.x, 0.0, t.air_friction * DT);
                n.vel.y = move_toward(n.vel.y, 0.0, t.gravity * 0.5 * DT);
            }
            _ => {
                n.vel.x = move_toward(n.vel.x, i.dir * t.air_speed * 0.6, t.air_accel * DT);
                n.vel.y += t.gravity * DT;
                if n.vel.y > t.max_fall {
                    n.vel.y = t.max_fall;
                }
            }
        }
    } else {
        // grounded: bleed horizontal to a planted stop
        n.vel.x = move_toward(n.vel.x, 0.0, t.ground_friction * DT);
    }
    if n.frame >= m.hit.total() - 1 {
        n.state = if m.kind == SpecialKind::Rise && n.ground_plat < 0 {
            CharState::Helpless
        } else if n.ground_plat < 0 {
            CharState::Air
        } else {
            CharState::Stand
        };
    }
}

/// Jump / shield are available from every actionable ground state; factor them out.
/// Jump comes from the buffer so a slightly-early press still fires.
fn try_ground_action(n: &mut Fighter, i: &InputFrame, atk: bool, grab: bool, t: &Tune) -> bool {
    if try_special(n) {
        return true;
    }
    if let Some(full) = take_jump(n) {
        n.state = CharState::JumpSquat;
        n.full_hop = full;
        true
    } else if grab {
        n.clear_lane(Lane::Grab);
        n.attack_hit = false;
        n.grab_link = -1;
        n.vel.x *= 0.25; // plant feet for the reach
        n.state = CharState::Grab;
        true
    } else if atk || n.live(Lane::Attack) == Action::Attack {
        n.clear_lane(Lane::Attack);
        n.attack_hit = false;
        if matches!(n.state, CharState::Dash | CharState::Run) {
            // attacking out of momentum = a dash attack: drive a forward lunge (faster than a plain
            // run) so it carries even from a standing dash, then the arm slides it out.
            n.vel.x = n.facing * t.run_speed * 1.25;
            n.state = CharState::DashAttack;
        } else {
            n.state = CharState::Jab;
            n.vel.x *= 0.25; // plant feet: a standing jab mostly kills momentum on startup
        }
        true
    } else if i.shield_held {
        n.state = CharState::Shield;
        true
    } else {
        false
    }
}

/// Consume a buffered jump/shorthop if one is live in the movement lane. Returns Some(full_hop).
fn take_jump(n: &mut Fighter) -> Option<bool> {
    match n.live(Lane::Movement) {
        Action::Jump => {
            n.clear_lane(Lane::Movement);
            Some(true)
        }
        Action::ShortHop => {
            n.clear_lane(Lane::Movement);
            Some(false)
        }
        _ => None,
    }
}

/// The aim to use for a buffered air dodge: the movement lane's captured diagonal if set, else the
/// live stick.
fn dodge_aim(n: &Fighter, i: &InputFrame) -> Vector2 {
    let m = &n.buf[Lane::Movement as usize];
    if m.aim.length() > 0.3 {
        m.aim
    } else {
        Vector2::new(i.dir, i.aim_y)
    }
}

/// Directional air-dodge burst from a 2D aim (digital diagonals included). Neutral aim = a
/// dodge in place. Into the ground a frame later, the surviving horizontal becomes a wavedash.
fn do_airdodge(n: &mut Fighter, aim: Vector2, t: &Tune) {
    if n.air_dodges > 0 {
        n.air_dodges -= 1;
    }
    n.vel = if aim.length() > 0.01 {
        aim.normalize_or_zero() * t.airdodge_speed
    } else {
        Vector2::ZERO
    };
    n.fast_falling = false;
    n.state = CharState::AirDodge;
}

fn grab_ledge(n: &mut Fighter, t: &Tune, edge_x: f32, face: f32) {
    n.pos = Vector2::new(edge_x, GROUND_Y + LEDGE_HANG_DY);
    n.vel = Vector2::ZERO;
    n.facing = face;
    n.fast_falling = false;
    n.air_jumps = t.max_air_jumps as u8;
    n.air_dodges = t.max_air_dodges as u8;
    n.state = CharState::LedgeHold;
}

/// Horizontal air drift (Ultimate-style, full bidirectional control): hold a direction to
/// accelerate toward the drift cap at air_accel (crisp turn, full strength when reversing);
/// momentum ABOVE the cap in the held direction is preserved (light drag only), so dash-jumps
/// keep their speed.
fn air_drift(n: &mut Fighter, i: &InputFrame, t: &Tune, sgn: f32) {
    let target = i.dir * t.air_speed;
    if sgn == 0.0 {
        n.vel.x = move_toward(n.vel.x, 0.0, t.air_friction * DT); // coast
    } else if n.vel.x.abs() <= t.air_speed || sign(n.vel.x) != sgn {
        n.vel.x = move_toward(n.vel.x, target, t.air_accel * DT); // turn / accel
    } else {
        n.vel.x = move_toward(n.vel.x, target, t.air_friction * DT); // keep momentum
    }
}

fn move_toward(from: f32, to: f32, delta: f32) -> f32 {
    if (to - from).abs() <= delta {
        to
    } else {
        from + (to - from).signum() * delta
    }
}

#[cfg(test)]
mod di_tests {
    use super::*;

    // Holding right while launched straight up should bend the trajectory toward +x by di_max_angle,
    // leaving the speed untouched (survival DI steers the angle, never the magnitude).
    #[test]
    fn di_rotates_angle_keeps_speed() {
        let up = Vector2::new(0.0, -100.0); // screen-up launch
        let out = apply_di(up, Vector2::new(1.0, 0.0), 18.0);
        assert!((out.length() - 100.0).abs() < 1e-3, "speed must be preserved");
        assert!(out.x > 0.0, "stick right bends the launch toward +x");
        let deg = (out.x).atan2(-out.y).to_degrees(); // angle off vertical
        assert!((deg - 18.0).abs() < 0.5, "rotation should hit the 18 deg cap, got {deg}");
    }

    // Neutral stick (and stick inside the deadzone) leaves the trajectory alone.
    #[test]
    fn di_neutral_is_identity() {
        let v = Vector2::new(40.0, -90.0);
        assert_eq!(apply_di(v, Vector2::ZERO, 18.0), v);
        assert_eq!(apply_di(v, Vector2::new(0.1, 0.1), 18.0), v); // below the 0.3 deadzone
    }

    // Walked off the lip (Air + coyote window) and pressed jump: it's the GROUNDED jump
    // (fullhop velocity), it does NOT spend the air jump, and the window closes.
    #[test]
    fn coyote_jump_is_full_and_keeps_air_jump() {
        let t = Tune::from_char(&CharData::KNEEMAN);
        let mut s = SimState::spawn();
        let f = &mut s.fighters[0];
        f.state = CharState::Air;
        f.pos = Vector2::new(600.0, 200.0); // airborne over center, far from any platform/ledge
        f.vel = Vector2::new(0.0, 40.0); // falling
        f.air_jumps = 1;
        f.coyote = 4;
        let jump = InputFrame { jump: true, jump_held: true, ..Default::default() };
        let idle = InputFrame::default();
        let out = step(&s, [&jump, &idle], &t);
        let g = &out.fighters[0];
        // one frame of gravity is integrated after takeoff, so compare against fullhop + g*DT.
        let expect = t.fullhop_v + t.gravity * DT;
        assert!((g.vel.y - expect).abs() < 1e-3, "coyote jump uses fullhop velocity");
        assert_eq!(g.air_jumps, 1, "coyote jump must not consume the air jump");
        assert_eq!(g.coyote, 0, "the grace window closes after the jump");
    }

    // Same airborne state but the window has expired: jump spends the air jump instead.
    #[test]
    fn expired_coyote_spends_air_jump() {
        let t = Tune::from_char(&CharData::KNEEMAN);
        let mut s = SimState::spawn();
        let f = &mut s.fighters[0];
        f.state = CharState::Air;
        f.pos = Vector2::new(600.0, 200.0);
        f.vel = Vector2::new(0.0, 40.0);
        f.air_jumps = 1;
        f.coyote = 0;
        let jump = InputFrame { jump: true, jump_held: true, ..Default::default() };
        let idle = InputFrame::default();
        let out = step(&s, [&jump, &idle], &t);
        let g = &out.fighters[0];
        let expect = t.airjump_v + t.gravity * DT;
        assert!((g.vel.y - expect).abs() < 1e-3, "no coyote -> air jump velocity");
        assert_eq!(g.air_jumps, 0, "air jump is consumed");
    }

    // Reversing a dash flips facing immediately but must NOT teleport velocity: the old momentum
    // bleeds through 0 at dash_turn_accel over a few frames (continuous, Melee-style).
    #[test]
    fn dash_reversal_keeps_momentum_then_crosses() {
        let t = Tune::from_char(&CharData::KNEEMAN);
        let mut s = SimState::spawn();
        let f = &mut s.fighters[0];
        f.state = CharState::Dash;
        f.facing = 1.0;
        f.ground_plat = 0;
        f.pos = Vector2::new(600.0, GROUND_Y); // mid main floor, won't walk off
        f.vel = Vector2::new(t.run_speed, 0.0); // moving right at run speed
        let left = InputFrame { dir: -1.0, ..Default::default() };
        let idle = InputFrame::default();

        // frame 1: facing flips, but velocity is still rightward (no instant reversal).
        let s1 = step(&s, [&left, &idle], &t);
        assert_eq!(s1.fighters[0].facing, -1.0, "facing flips on the reversal frame");
        assert!(s1.fighters[0].vel.x > 0.0, "velocity must NOT teleport to the new direction");

        // hold left a few more frames: momentum bleeds through 0 and goes negative.
        let mut cur = s1;
        for _ in 0..8 {
            cur = step(&cur, [&left, &idle], &t);
        }
        assert!(cur.fighters[0].vel.x < 0.0, "sustained reversal eventually crosses 0 to the left");
    }

    // Pressing special with the stick up enters up-B, launches the fighter upward, and finishes in
    // Helpless (special-fall) once it's airborne.
    #[test]
    fn up_special_rises_then_helpless() {
        let t = Tune::from_char(&CharData::KNEEMAN);
        let mut s = SimState::spawn();
        let f = &mut s.fighters[0];
        f.state = CharState::Stand;
        f.ground_plat = 0;
        f.pos = Vector2::new(600.0, GROUND_Y);
        f.vel = Vector2::ZERO;
        let up_b = InputFrame { special: true, aim_y: -1.0, ..Default::default() };
        let hold = InputFrame { aim_y: -1.0, ..Default::default() };

        // press frame enters the up-B state
        let mut cur = step(&s, [&up_b, &Default::default()], &t);
        assert_eq!(cur.fighters[0].state, CharState::SpecialU, "stick-up special = up-B");

        // run it out: it leaves the ground rising, then becomes Helpless
        let mut saw_rise = false;
        let mut saw_helpless = false;
        for _ in 0..60 {
            cur = step(&cur, [&hold, &Default::default()], &t);
            if cur.fighters[0].vel.y < 0.0 {
                saw_rise = true;
            }
            if cur.fighters[0].state == CharState::Helpless {
                saw_helpless = true;
                break;
            }
        }
        assert!(saw_rise, "up-B should drive the fighter upward");
        assert!(saw_helpless, "up-B ends in Helpless while airborne");
    }

    // Every B special must MOVE by integrating velocity, never by snapping position. This guards the
    // reported "B teleports me": the largest legit burst is up-B at ~24px/frame, so any single-frame
    // jump past 40px would be a teleport bug (a one-frame position write). The visible "blink" with
    // the static test characters is missing air animation, not a position snap -- this proves it.
    #[test]
    fn specials_never_teleport() {
        let t = Tune::from_char(&CharData::KNEEMAN);
        const MAX_STEP: f32 = 40.0;
        for (aim_y, dir, label) in [(-1.0f32, 0.0f32, "up-B"), (0.0, 1.0, "side-B"), (1.0, 0.0, "down-B")] {
            let mut s = SimState::spawn();
            {
                let f = &mut s.fighters[0];
                f.state = CharState::Stand;
                f.ground_plat = 0;
                f.pos = Vector2::new(600.0, GROUND_Y);
                f.vel = Vector2::ZERO;
            }
            let press = InputFrame { special: true, aim_y, dir, ..Default::default() };
            let hold = InputFrame { aim_y, dir, ..Default::default() };
            let mut cur = step(&s, [&press, &Default::default()], &t);
            let mut prev = cur.fighters[0].pos;
            for _ in 0..40 {
                cur = step(&cur, [&hold, &Default::default()], &t);
                let p = cur.fighters[0].pos;
                let d = (p - prev).length();
                assert!(d <= MAX_STEP, "{label}: single-frame jump {d:.1}px > {MAX_STEP} (teleport)");
                prev = p;
            }
        }
    }

    // Neutral-B from standing enters the planted punch and stays grounded (no launch).
    #[test]
    fn neutral_special_is_grounded_punch() {
        let t = Tune::from_char(&CharData::KNEEMAN);
        let mut s = SimState::spawn();
        let f = &mut s.fighters[0];
        f.state = CharState::Stand;
        f.ground_plat = 0;
        f.pos = Vector2::new(600.0, GROUND_Y);
        let nb = InputFrame { special: true, ..Default::default() };
        let cur = step(&s, [&nb, &Default::default()], &t);
        assert_eq!(cur.fighters[0].state, CharState::SpecialN, "neutral stick + special = neutral-B");
        // a few frames in, still grounded and still in the punch (not launched into the air)
        let mut c = cur;
        for _ in 0..6 {
            c = step(&c, [&Default::default(), &Default::default()], &t);
        }
        assert!(c.fighters[0].ground_plat >= 0, "neutral-B stays grounded");
    }

    // A special pressed in the air must STAY airborne. Regression for stale ground_plat (lingering
    // from a jump) making run_special + the integrator treat the move as grounded — which planted
    // the punch and snapped pos.y to the platform ("B-air teleports me to ground").
    #[test]
    fn aerial_special_stays_airborne() {
        let t = Tune::from_char(&CharData::KNEEMAN);
        for (aim_y, dir, label) in
            [(0.0f32, 0.0f32, "N-air"), (0.0, 1.0, "side-air"), (-1.0, 0.0, "up-air"), (1.0, 0.0, "down-air")]
        {
            let mut s = SimState::spawn();
            {
                let f = &mut s.fighters[0];
                f.state = CharState::Air;
                f.ground_plat = 0; // stale grounded index lingering from a jump
                f.pos = Vector2::new(600.0, GROUND_Y - 200.0);
                f.vel = Vector2::ZERO;
            }
            let press = InputFrame { special: true, aim_y, dir, ..Default::default() };
            let cur = step(&s, [&press, &Default::default()], &t);
            assert!(cur.fighters[0].pos.y < GROUND_Y - 50.0, "{label}: snapped to ground");
            assert_eq!(cur.fighters[0].ground_plat, -1, "{label}: must read as airborne");
        }
    }

    #[test]
    fn grab_catches_holds_and_throws() {
        let t = Tune::from_char(&CharData::KNEEMAN);
        let mut s = SimState::spawn();
        // two fighters face-to-face, grounded, within grab range.
        for (k, x, face) in [(0usize, 600.0_f32, 1.0_f32), (1, 600.0 + 80.0, -1.0)] {
            let f = &mut s.fighters[k];
            f.state = CharState::Stand;
            f.ground_plat = 0;
            f.pos = Vector2::new(x, GROUND_Y);
            f.facing = face;
        }
        let grab = InputFrame { grab: true, ..Default::default() };
        let idle = InputFrame::default();

        // p0 presses grab; within startup+active it should catch p1.
        let mut c = step(&s, [&grab, &idle], &t);
        for _ in 0..(t.grab_startup + t.grab_active) {
            c = step(&c, [&idle, &idle], &t);
        }
        assert_eq!(c.fighters[0].state, CharState::GrabHold, "grabber holds");
        assert_eq!(c.fighters[1].state, CharState::Grabbed, "victim held");
        assert_eq!(c.fighters[0].grab_link, 1);
        assert_eq!(c.fighters[1].grab_link, 0);

        // pummel raises the victim's damage without releasing.
        let pummel = InputFrame { attack: true, ..Default::default() };
        let before = c.fighters[1].damage;
        c = step(&c, [&pummel, &idle], &t);
        assert!(c.fighters[1].damage > before, "pummel deals damage");
        assert_eq!(c.fighters[0].state, CharState::GrabHold, "still holding after pummel");

        // re-press grab = throw: victim launched, both unlinked.
        c = step(&c, [&grab, &idle], &t);
        assert_eq!(c.fighters[0].grab_link, -1, "grabber released on throw");
        assert!(c.fighters[1].hitstun > 0, "victim launched with hitstun");
        assert_ne!(c.fighters[1].state, CharState::Grabbed, "victim no longer held");
    }

    #[test]
    fn grab_whiffs_to_neutral_when_out_of_range() {
        let t = Tune::from_char(&CharData::KNEEMAN);
        let mut s = SimState::spawn();
        let f0 = &mut s.fighters[0];
        f0.state = CharState::Stand;
        f0.ground_plat = 0;
        f0.pos = Vector2::new(300.0, GROUND_Y);
        f0.facing = 1.0;
        let f1 = &mut s.fighters[1];
        f1.state = CharState::Stand;
        f1.ground_plat = 0;
        f1.pos = Vector2::new(900.0, GROUND_Y); // far away
        let grab = InputFrame { grab: true, ..Default::default() };
        let idle = InputFrame::default();
        let mut c = step(&s, [&grab, &idle], &t);
        assert_eq!(c.fighters[0].state, CharState::Grab, "entered grab");
        for _ in 0..(t.grab_startup + t.grab_active + t.grab_recovery + 1) {
            c = step(&c, [&idle, &idle], &t);
        }
        assert_eq!(c.fighters[0].state, CharState::Stand, "whiffed grab returns to neutral");
        assert_eq!(c.fighters[1].state, CharState::Stand, "victim untouched");
    }

    /// Helper: a fighter hovering one frame above the floor, launched downward in tumble.
    fn launched(t: &Tune) -> SimState {
        let mut s = SimState::spawn();
        let f = &mut s.fighters[0];
        f.state = CharState::Air;
        f.ground_plat = -1;
        f.pos = Vector2::new(600.0, GROUND_Y - 5.0);
        f.vel = Vector2::new(120.0, 600.0); // moving down hard: crosses the floor next frame
        f.hitstun = 30;
        f.tumble = true;
        let _ = t;
        s
    }

    #[test]
    fn hard_launch_knocks_down_without_tech() {
        let t = Tune::from_char(&CharData::KNEEMAN);
        let s = launched(&t);
        let idle = InputFrame::default();
        let c = step(&s, [&idle, &idle], &t);
        assert_eq!(c.fighters[0].state, CharState::Knockdown, "missed tech -> floored");
        assert!(!c.fighters[0].tumble, "tumble cleared on knockdown");
    }

    #[test]
    fn shield_at_impact_techs_in_place() {
        let t = Tune::from_char(&CharData::KNEEMAN);
        let s = launched(&t);
        let tech = InputFrame { shield_pressed: true, ..Default::default() };
        let idle = InputFrame::default();
        let c = step(&s, [&tech, &idle], &t);
        assert_eq!(c.fighters[0].state, CharState::TechInPlace, "teched the landing");
        assert!(c.fighters[0].intangible, "tech is intangible");
    }

    #[test]
    fn directional_tech_rolls() {
        let t = Tune::from_char(&CharData::KNEEMAN);
        let s = launched(&t);
        let tech_left = InputFrame { shield_pressed: true, dir: -1.0, ..Default::default() };
        let idle = InputFrame::default();
        let c = step(&s, [&tech_left, &idle], &t);
        assert_eq!(c.fighters[0].state, CharState::TechRoll, "held a direction -> tech roll");
        assert!(c.fighters[0].vel.x < 0.0, "rolls in the held direction");
    }

    #[test]
    fn knockdown_auto_getups_to_stand() {
        let t = Tune::from_char(&CharData::KNEEMAN);
        let s = launched(&t);
        let idle = InputFrame::default();
        let mut c = step(&s, [&idle, &idle], &t);
        assert_eq!(c.fighters[0].state, CharState::Knockdown);
        for _ in 0..(t.knockdown_frames + t.getup_frames + 2) {
            c = step(&c, [&idle, &idle], &t);
        }
        assert_eq!(c.fighters[0].state, CharState::Stand, "floored -> getup -> stand");
    }
}
