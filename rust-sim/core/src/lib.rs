// Engine-agnostic vectors. `Vector2` is kept as the local name (minimal churn from the
// godot original); the shell converts to godot::Vector2 at the render boundary.
pub use glam::Vec2 as Vector2;

use serde::{Deserialize, Serialize};

pub mod geo; // deterministic collision geometry, API-shaped to mirror parry2d (swap-in later)
pub mod stage; // all surfaces: static stage geometry + drawn ink paths
pub use stage::*;
pub mod physics; // kinematics: DI, drift, dodge, ledge snap, math helpers
pub(crate) use physics::*; // all crate-internal helpers; nothing here is part of the public API
pub mod item; // items + projectiles: pickups, bolts/bombs, spawning, projectile resolution
pub use item::*;
pub mod moves; // moves by kind: shared attack data + special + throw
pub use moves::*;

// fixed timestep; the sim never uses wall-clock delta (determinism).
pub const DT: f32 = 1.0 / 60.0;
pub const FPS: f32 = 60.0;

// World->screen scale. Source attributes are world-units; we render pixels.
// Spatial FEEL (jump-height : run-distance ratios, time-to-apex) is scale-invariant,
// so this just sets how big the world reads on screen. Bumped to fit the larger stage.
// Change it and every distance scales together.
pub const PX_PER_UNIT: f32 = 7.0;

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


pub const DUMMY_R: f32 = 48.0;    // body hurtbox radius (circle), scaled with the taller ECB


/// Ground/air/ledge action states. `frame` (in SimState) is the per-state timer that resets on
/// every transition, mirroring an animation frame — it gates the dash window, jumpsquat takeoff,
/// pivot, dodge length, landing lag, ledge intangibility, and getup.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
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
    Dtilt,     // down-tilt (down + attack, grounded): low pothole poke that pops them up
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
    Launched,  // taking a hit: knockback slide + hitstun. Forced on connect so the interrupted
               // attacker's remaining hitbox windows never fire (attack_for(Launched) == None).
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
    Draw,                // held pen + attack: lay a node on the owner's ink path this frame
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


/// Every input edge that can be buffered, as one type. Recorded on the button edge with the aim at
/// that moment, consumed when the state machine reaches a point where it can act — this is what
/// makes wavedash / jump-out-of-lag / the down-diagonal feel reliable instead of frame-perfect.
/// `window` is the only place a buffer length is decided, dispatched by a match (the enum's job —
/// no bit tricks): the lookahead edges share the live Tune window; `Grab` is 0 (press-frame only,
/// as today) but expressible, the seam to give it a real buffer later.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
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
#[derive(Copy, Clone, PartialEq, Default, Debug, Serialize, Deserialize)]
pub struct Slot {
    pub action: Action,
    pub timer: i64,
    pub aim: Vector2,
}

/// One fighter as a plain value. Two of these make a `SimState`. Everything here is
/// per-fighter (the old single-player SimState fields); `damage`/`hitstun` were the old
/// `dummy_*` fields, now owned by every fighter (each can take and deal hits).
/// `frame` is the per-fighter STATE timer (reset on every transition), not a global clock.
#[derive(Copy, Clone, PartialEq, Serialize, Deserialize)]
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
    pub ground_ink: i8,    // index into SimState.paths when standing on drawn ink (-1 = not on ink)
    // Per-hitbox, per-victim re-hit countdown: `hit_cd[box][victim] > 0` means that box of THIS
    // fighter's current move can't hit that victim yet (it just connected, or is mid-window). A box
    // re-arms after its `refresh`; a fresh swing zeroes the whole grid (`arm_hits`). Replaces the old
    // `attack_hit: bool` so a 3-box jab combo / multi-hit stomp each land their own sequenced hits,
    // and a wide box hits every overlapping victim once.
    pub hit_cd: [[i16; MAX_PLAYERS]; MAX_HB],
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
    pub wall_hit: i64,     // frames left in the wall-bounce tilt window (cosmetic: shell tilts + swaps clip)
}

/// Max simultaneous items+projectiles on screen (fixed so SimState stays Copy + checksums cheaply).
pub const MAX_ITEMS: usize = 8;

/// Max fighters in one match. Fixed (not a `Vec`) so `SimState` stays `Copy` and rollback snapshots
/// don't heap-allocate; `SimState::active` says how many of the slots are actually in play. 4 is the
/// canonical platform-fighter cap. See plans/n-player.md.
pub const MAX_PLAYERS: usize = 4;

/// The entire sim state as a plain value: the fighters (slots `0..active`) + the item field. This is
/// what the BehaviorSubject holds, what ggrs saves/rolls back, and what egui renders. `Copy` so
/// snapshots are free.
#[derive(Copy, Clone, PartialEq, Serialize, Deserialize)]
pub struct SimState {
    pub fighters: [Fighter; MAX_PLAYERS],
    pub active: u8, // fighters[0..active] are live; the rest are dormant (not stepped, not drawn)
    pub items: [Item; MAX_ITEMS],
    pub paths: [InkPath; MAX_DRAWN], // drawn ink + baked stage strokes; same polyline primitive
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
            ground_ink: -1,
            hit_cd: [[0; MAX_PLAYERS]; MAX_HB],
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
            wall_hit: 0,
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

    /// Re-arm every hitbox of a fresh swing: zero the per-box, per-victim re-hit grid so the new
    /// move's boxes can all connect. Called on entering any attack state (replaces `attack_hit=false`).
    #[inline]
    pub(crate) fn arm_hits(&mut self) {
        self.hit_cd = [[0; MAX_PLAYERS]; MAX_HB];
    }

    /// Tick the re-hit grid down one frame (called once per active frame in `advance`).
    #[inline]
    fn tick_hit_cd(&mut self) {
        for row in &mut self.hit_cd {
            for c in row {
                if *c > 0 {
                    *c -= 1;
                }
            }
        }
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
            CharState::Dtilt => "DTILT",
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
            CharState::Launched => "LAUNCHED",
        }
    }
}

impl SimState {
    /// Two fighters facing each other on the main stage (airborne drop-in). The default match size.
    pub fn spawn() -> Self {
        Self::spawn_n(2)
    }

    /// `count` fighters (clamped to `1..=MAX_PLAYERS`) dropped in evenly across the stage, each
    /// facing the stage centre. Dormant slots still hold a valid `Fighter` (so the array is sound)
    /// but `active` excludes them from stepping, combat, and rendering.
    pub fn spawn_n(count: usize) -> Self {
        let count = count.clamp(1, MAX_PLAYERS);
        let mut fighters = [Fighter::spawn(480.0, 1.0); MAX_PLAYERS];
        for (p, f) in fighters.iter_mut().enumerate() {
            let (x, facing) = spawn_slot(p, count);
            *f = Fighter::spawn(x, facing);
        }
        Self {
            fighters,
            active: count as u8,
            items: [Item::EMPTY; MAX_ITEMS],
            paths: [InkPath::EMPTY; MAX_DRAWN],
            tick: 0,
            rng: 0x9E37_79B9_7F4A_7C15, // fixed seed: every peer spawns identical items
        }
    }
}

/// Spawn position + facing for player `p` of `count`. Two players keep the historical 480/720 split
/// (so existing behavior/tests are byte-identical); more players spread evenly over the same band,
/// each turned toward stage centre.
fn spawn_slot(p: usize, count: usize) -> (f32, f32) {
    const CENTER: f32 = 600.0;
    if count <= 2 {
        return if p == 0 { (480.0, 1.0) } else { (720.0, -1.0) };
    }
    const LEFT: f32 = 360.0;
    const RIGHT: f32 = 840.0;
    let t = p as f32 / (count - 1) as f32;
    let x = LEFT + (RIGHT - LEFT) * t;
    (x, if x < CENTER { 1.0 } else { -1.0 })
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
    pub dtilt: AttackData,
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
    // drawn ink paths (see the `ink-paths` skill)
    pub pen: StrokeProps,         // baseline stroke material every drawing tool stamps (panel-editable)
    pub ink_budget: f32,          // total path length (px) a fresh ink item can lay before it's spent
    pub ink_cursor_reach: f32,    // CursorBrush: how far the drawing cursor floats off the body (px)
    pub ink_spawn_weight: f32,    // relative spawn chance of a pen vs the guns (0 = never random-spawns)
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
            pen: StrokeProps::PEN,
            ink_budget: 900.0, // ~the main stage width of drawable line per pickup
            ink_cursor_reach: 140.0,
            ink_spawn_weight: 0.6,
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
pub fn step(s: &SimState, inputs: &[&InputFrame], t: &Tune) -> SimState {
    let mut n = *s;
    n.tick = n.tick.wrapping_add(1);
    maybe_spawn_item(&mut n, t);
    let np = (n.active as usize).min(inputs.len());

    // Phase 1: each fighter's FSM scans its raw input and emits a pure "next action" (Act). The input
    // is never mutated. All FSMs run BEFORE any actuation so no fighter's advance sees another's
    // already-applied effect (preserves the old two-then-two ordering for any player count).
    let paths = n.paths; // ink is fixed for the frame (it mutates last, in update_paths); snapshot to collide
    let mut acts = [Act::None; MAX_PLAYERS];
    for p in 0..np {
        let items = n.items; // read-only snapshot so the FSM decides pickup-vs-jab without borrowing
        acts[p] = advance(&mut n.fighters[p], &items, &paths, inputs[p], t);
    }
    // Phase 2: actuate in handle order (spawns bolts / drops / picks up on the shared item array).
    for p in 0..np {
        apply_act(&mut n, p, acts[p], t);
    }
    // Phase 3: pairwise combat + grabs over every ordered (attacker, victim) pair. The victim's stick
    // this frame feeds trajectory DI, so each call passes the defender's aim. `pair_mut` borrows the
    // two distinct fighters at once (generalizes the old hand split).
    for a in 0..np {
        for b in 0..np {
            if a == b {
                continue;
            }
            let aim_b = Vector2::new(inputs[b].dir, inputs[b].aim_y);
            let (fa, fb) = pair_mut(&mut n.fighters, a, b);
            resolve_combat(fa, b, fb, aim_b, t); // a attacks b (victim b DIs)
        }
    }
    for a in 0..np {
        for b in 0..np {
            if a == b {
                continue;
            }
            let (fa, fb) = pair_mut(&mut n.fighters, a, b);
            resolve_grab(fa, fb, a as i8, b as i8, inputs[a], inputs[b], t); // a grabs b
        }
    }

    update_items(&mut n, t); // move bolts, follow held guns, resolve bolt hits
    update_paths(&mut n, inputs, t); // lay/extend/finalize drawn ink, decay old nodes
    n
}

/// Two distinct fighters (`a != b`) borrowed mutably at once, via one `split_at_mut`. Replaces the
/// old hand-rolled `split_at_mut(1)` now that the pair is dynamic.
fn pair_mut(fs: &mut [Fighter], a: usize, b: usize) -> (&mut Fighter, &mut Fighter) {
    debug_assert_ne!(a, b, "pair_mut needs two distinct fighters");
    if a < b {
        let (l, r) = fs.split_at_mut(b);
        (&mut l[a], &mut r[0])
    } else {
        let (l, r) = fs.split_at_mut(a);
        (&mut r[0], &mut l[b])
    }
}

/// Advance ONE fighter by one frame from its own input: buffer, state machine, integrate +
/// stage collision. No cross-fighter combat (that is `resolve_combat`). Mutates in place.
fn advance(f: &mut Fighter, items: &[Item; MAX_ITEMS], paths: &[InkPath; MAX_DRAWN], i: &InputFrame, t: &Tune) -> Act {
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

        // launched into a stage wall: tech it (same 20f window as the floor, PM/Ultimate-style) or
        // bounce off with `wall_bounce` restitution. Only while the body is below the top lip (a real
        // wall hit, not skimming the stage top). Bounce arms a short tilt window the shell reads.
        let cy = n.pos.y - ECB_HALF_H;
        if n.tumble && cy > GROUND_Y && cy < STAGE_BOTTOM {
            let hit_left = n.pos.x < FLOOR_LEFT && n.pos.x + ECB_HALF_W > FLOOR_LEFT && n.vel.x > 0.0;
            let hit_right = n.pos.x > FLOOR_RIGHT && n.pos.x - ECB_HALF_W < FLOOR_RIGHT && n.vel.x < 0.0;
            if hit_left || hit_right {
                let (px, normal) = if hit_left {
                    (FLOOR_LEFT - ECB_HALF_W, Vector2::new(-1.0, 0.0))
                } else {
                    (FLOOR_RIGHT + ECB_HALF_W, Vector2::new(1.0, 0.0))
                };
                n.pos.x = px;
                if n.tech_buf > 0 {
                    // wall tech: kill the launch, stick the landing, intangible recovery in place.
                    n.tech_buf = 0;
                    n.hitstun = 0;
                    n.tumble = false;
                    n.wall_hit = 0;
                    n.vel = Vector2::ZERO;
                    n.intangible = true;
                    n.frame = 0;
                    n.state = CharState::TechInPlace;
                    *f = n;
                    return Act::None;
                }
                // missed the tech: bounce off, and flag the tilt window for the shell.
                n.vel = geo::reflect(n.vel, normal, t.wall_bounce);
                n.wall_hit = WALL_TILT_FRAMES;
            }
        }
        if n.wall_hit > 0 {
            n.wall_hit -= 1;
        }
        n.hitstun -= 1;

        // hard launch hitting the floor: tech it (intangible recovery) or eat a knockdown.
        if landed && n.tumble {
            n.hitstun = 0;
            n.tumble = false;
            n.ground_plat = 0;
            n.ground_ink = -1;
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
            n.wall_hit = 0;
            if n.pos.y < GROUND_Y {
                n.state = CharState::Air;
                n.ground_plat = -1;
            } else {
                n.state = CharState::Stand;
                n.ground_plat = 0;
                n.ground_ink = -1;
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
    n.tick_hit_cd(); // age the per-box re-hit grid once per active (non-frozen) frame
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
    let pickup_target = if holding { None } else { nearest_pickup(&n, items, t) };
    let grab = n.live(Lane::Grab) == Action::Grab;
    let mut act = Act::None;
    let held_pen = holding && items[n.holding as usize].kind.is_pen();
    if holding {
        if grab {
            act = Act::Drop;
        } else if held_pen && (i.attack || i.attack_held) {
            act = Act::Draw; // a pen draws instead of firing
        } else if !held_pen && (i.attack || i.attack_held) {
            act = Act::Fire { auto: i.attack_held && !i.attack };
        }
    } else if (i.attack || grab) && pickup_target.is_some() {
        // item grab: attack OR the grab button claims an item you're standing over (empty-handed).
        // The grab press routes here instead of a fighter-grab whenever there's an item to take.
        act = Act::Pickup;
    }
    let fire = matches!(act, Act::Fire { .. });
    let grabbing = matches!(act, Act::Pickup | Act::Drop);
    let drawing_act = matches!(act, Act::Draw);
    let atk = i.attack && !fire && !grabbing && !drawing_act; // effective attack for jab/aerial
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
                n.arm_hits();
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
                n.ground_ink = -1;
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
        CharState::Dtilt => {
            // crouched pothole swing: planted (feet stay put), run the frame data, then back to a
            // crouch if down is still held, else stand. Same brake as a jab.
            n.vel.x = move_toward(n.vel.x, 0.0, t.ground_friction * 3.0 * DT);
            let atk = attack_for(t, CharState::Dtilt).unwrap();
            if n.frame >= atk.total() - 1 {
                n.state = if i.down { CharState::Crouch } else { CharState::Stand };
            }
        }
        CharState::DashAttack => {
            // lunge: slide through the swipe carrying the lunge speed (barely any friction), then
            // brake hard once the endlag starts so the commitment still plants you. No steering.
            let atk = attack_for(t, CharState::DashAttack).unwrap();
            let sliding = n.frame < atk.active_end(); // still swinging = the drive; then brake
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
                    n.arm_hits();
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
        CharState::Launched => {
            // Reached only if a connect set Launched but hitstun floored to 0 (a feather tap): the
            // hitstun branch above never ran, so recover here the same way it exits (air -> Air,
            // grounded -> Stand). Normal launches spend their time in the `hitstun > 0` branch.
            n.tumble = false;
            if n.pos.y < GROUND_Y {
                n.state = CharState::Air;
                n.ground_plat = -1;
            } else {
                n.state = CharState::Stand;
                n.ground_plat = 0;
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
                    n.ground_ink = -1; // on a platform now, not ink
                    n.state = CharState::Landing; // carries vel.x -> Landing friction = slide
                    break;
                }
            }
        }

        // ink landing: same crossed-from-above test against drawn ink surfaces (the cached Floor/Ledge
        // segments), only if a platform didn't already catch us this frame. Soft ink drops through on
        // held down, like a soft platform.
        if airborne(n.state) && n.vel.y >= 0.0 {
            for (idx, p) in paths.iter().enumerate() {
                let Some(surf) = ink_floor_y_at(p, n.pos.x) else { continue };
                let crossed = prev_y <= surf + 1.0 && n.pos.y >= surf;
                let land = p.props.solid || !i.down || n.state == CharState::AirDodge;
                if crossed && land {
                    n.pos.y = surf;
                    n.vel.y = 0.0;
                    n.fast_falling = false;
                    n.air_jumps = t.max_air_jumps as u8;
                    n.air_dodges = t.max_air_dodges as u8;
                    n.coyote = 0;
                    n.ground_plat = 0; // reads as grounded to special/run-special logic
                    n.ground_ink = idx as i8;
                    n.state = CharState::Landing;
                    break;
                }
            }
        }

        // solid-stage walls: keep the ECB's side verts out of the main platform's vertical faces.
        // Only engages when the diamond CENTER is below the top lip (recovering from the side),
        // so it never blocks you while standing on top. A LAUNCHED body (tumbling) kicks back off
        // the wall via geo::reflect with `wall_bounce` restitution; everyone else dead-stops (e=0),
        // which is exactly reflect with e=0 — same code path, no special case.
        if airborne(n.state) {
            let cy = n.pos.y - ECB_HALF_H; // diamond center y
            if cy > GROUND_Y && cy < STAGE_BOTTOM {
                let e = if n.tumble { t.wall_bounce } else { 0.0 };
                if n.pos.x < FLOOR_LEFT && n.pos.x + ECB_HALF_W > FLOOR_LEFT {
                    n.pos.x = FLOOR_LEFT - ECB_HALF_W; // right vert stops on the left wall
                    if n.vel.x > 0.0 {
                        // left-wall normal points -x (out of the stage toward the fighter)
                        n.vel = geo::reflect(n.vel, Vector2::new(-1.0, 0.0), e);
                    }
                } else if n.pos.x > FLOOR_RIGHT && n.pos.x - ECB_HALF_W < FLOOR_RIGHT {
                    n.pos.x = FLOOR_RIGHT + ECB_HALF_W; // left vert stops on the right wall
                    if n.vel.x < 0.0 {
                        n.vel = geo::reflect(n.vel, Vector2::new(1.0, 0.0), e);
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
                        n.ground_ink = -1;
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
    } else if n.ground_ink >= 0 {
        // grounded on drawn ink: pin feet to the surface under us, walk off the ends, drop through
        // soft ink with held down (mirrors the soft-platform branch below). If the ink decayed out
        // from under us (no spanning segment), fall.
        let p = paths[n.ground_ink as usize];
        n.pos.x += n.vel.x * DT;
        // deliberate "down, down" drop: require a fresh Down press while ALREADY crouching
        // (prev==Crouch). The entry tap that caused Stand->Crouch has prev=Stand, so it never
        // drops. JumpSquat/Dtilt are implicitly excluded because n.state must be Crouch.
        let drop = !p.props.solid && i.down_pressed
            && n.state == CharState::Crouch && prev == CharState::Crouch;
        match (drop, ink_floor_y_at(&p, n.pos.x)) {
            (false, Some(y)) => {
                n.pos.y = y;
                n.vel.y = 0.0;
            }
            _ => {
                n.state = CharState::Air;
                n.ground_ink = -1;
                n.coyote = t.coyote_frames as u8;
            }
        }
    } else {
        // grounded: pinned to its platform, no vertical motion
        let p = PLATFORMS[n.ground_plat.clamp(0, PLATFORMS.len() as i32 - 1) as usize];
        n.pos.x += n.vel.x * DT;
        if !p.solid && i.down_pressed && n.state == CharState::Crouch && prev == CharState::Crouch {
            // deliberate "down, down" drop: a fresh Down press while ALREADY crouching (prev==Crouch).
            // The entry tap that caused Stand->Crouch has prev=Stand so it never drops.
            // JumpSquat/Dtilt are implicitly excluded because n.state must be Crouch.
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


/// Cross-fighter combat: `a`'s live hitboxes vs `b`'s hurtbox (circle/circle). `vb` is `b`'s slot
/// index (keys the per-box re-hit grid). Among `a`'s boxes live this frame, off cooldown for `b`,
/// and overlapping, the LOWEST id wins (sweetspot beats sourspot). On connect: damage + community/PM
/// knockback + hitstun to `b`, impact freeze (hitlag) to BOTH, and the hit forces `b` into
/// `Launched` (cancels any move `b` was mid-swing — the interrupt). `b` re-hittable per box per
/// `refresh`, so a 3-box jab combo / multi-hit stomp each land their own pops.
fn resolve_combat(a: &mut Fighter, vb: usize, b: &mut Fighter, b_aim: Vector2, t: &Tune) {
    if b.invuln > 0 || b.intangible {
        return; // spawn i-frames / active dodge: no hit lands
    }
    let Some(atk) = attack_for(t, a.state) else { return };
    let (bc, br) = hurtbox(b);
    // id-priority pick: lowest-id live box that is off this victim's cooldown AND overlaps.
    let mut chosen: Option<usize> = None;
    let mut best_id = u8::MAX;
    for (bi, hb) in atk.live_boxes().iter().enumerate() {
        if !hb.live_at(a.frame) || a.hit_cd[bi][vb] > 0 {
            continue;
        }
        let (hc, hr) = hitbox_center(a, hb);
        if (hc - bc).length() > hr + br {
            continue; // no overlap
        }
        if hb.id < best_id {
            best_id = hb.id;
            chosen = Some(bi);
        }
    }
    let Some(bi) = chosen else { return };
    let hb = atk.boxes[bi];
    // re-arm: a box can't re-hit this victim until `refresh` frames pass; with refresh 0 it locks for
    // the rest of its own window (one hit per box per swing). A later box (different index) still hits.
    let cd = if hb.refresh > 0 { hb.refresh } else { (hb.start + hb.len) - a.frame };
    a.hit_cd[bi][vb] = cd.max(1) as i16;

    let dmg = if matches!(a.state, CharState::Nair | CharState::Dair) && a.autohop_aerial {
        hb.damage * t.autohop_dmg // auto short-hop aerial: reduced damage (Ultimate)
    } else {
        hb.damage
    };
    b.damage += dmg;
    // community / Project-M knockback: units -> px/s via kb_speed, then the global multiplier.
    let kb = knockback_units(b.damage, dmg, t.weight, &hb);
    let speed = kb * t.kb_speed * t.knockback_mult;
    let ang = hb.angle.to_radians();
    b.vel = Vector2::new(ang.cos() * a.facing, -ang.sin()) * speed; // launch away from attacker
    b.vel = apply_di(b.vel, b_aim, t.di_max_angle); // victim angles the trajectory (survival DI)
    b.hitstun = (kb * t.kb_hitstun) as i64; // floor(0.4 * KB)
    b.tumble = speed > t.tumble_speed; // hard enough to knock down (or be teched) on landing
    // hit interrupt: launch the victim, cancelling whatever move it was mid-swing. attack_for(Launched)
    // is None, so the interrupted move's remaining windows never fire, and the shell plays the launch.
    b.state = CharState::Launched;
    b.frame = 0;
    let freeze = (dmg * HITLAG_PER_DMG) as i64 + 4; // both fighters pop on impact
    a.hitlag = freeze;
    b.hitlag = freeze;
}


/// Actuate one fighter's emitted `Act` into the SimState. The only place item intents become item
/// effects; `advance` decided WHAT to do (purely), this carries it out on the shared item array.
fn apply_act(n: &mut SimState, idx: usize, act: Act, t: &Tune) {
    match act {
        Act::None => {}
        Act::Fire { auto } => fire_gun(n, idx, auto, t),
        Act::Drop => drop_item(n, idx),
        Act::Pickup => pickup_item(n, idx, t),
        // node-laying needs the raw stick (cursor/ruler aim), which apply_act doesn't have — it runs
        // in `update_paths`. Act::Draw exists only so `advance` suppressed the jab; nothing to do here.
        Act::Draw => {}
    }
}

/// Post-step ink: lay/extend each drawing fighter's path (tool-specific, budget-capped), finalize a
/// path the moment its owner stops drawing or runs out of budget (running `classify` once to cache
/// grabbability), then decay old nodes per-node and free spent slots. Baked stage strokes (owner < 0)
/// never draw or decay. Pure; the only place ink paths mutate. See the `ink-paths` skill.
fn update_paths(n: &mut SimState, inputs: &[&InputFrame], t: &Tune) {
    let tick = n.tick;
    let np = (n.active as usize).min(inputs.len());
    for idx in 0..np {
        let f = n.fighters[idx];
        let holding = f.holding;
        let pen = holding >= 0 && n.items[holding as usize].kind.is_pen();
        let inp = inputs[idx];
        let want = pen && (inp.attack || inp.attack_held);
        let active_slot = n.paths.iter().position(|p| p.drawing && p.owner == idx as i8);
        if want {
            let tool = n.items[holding as usize].tool;
            let slot = match active_slot {
                Some(s) => s,
                None => {
                    let Some(s) = n.paths.iter().position(|p| !p.active() && !p.drawing) else {
                        continue; // no free path slot — drop the stroke
                    };
                    let mut fresh = InkPath::EMPTY;
                    fresh.kind = tool;
                    fresh.props = tool_props(tool, t);
                    fresh.owner = idx as i8;
                    fresh.drawing = true;
                    fresh.budget = t.ink_budget;
                    n.paths[s] = fresh;
                    s
                }
            };
            let path = n.paths[slot];
            if let Some(p) = tool_sample(tool, &f, inp, &path, t) {
                let add = path.last().map_or(0.0, |prev| (p - prev).length());
                if path.len == 0 || (add > 0.0 && path.budget - add >= 0.0) {
                    n.paths[slot].push(p, tick);
                    n.paths[slot].budget -= add;
                }
                if n.paths[slot].budget <= 0.0 {
                    finalize_path(&mut n.paths[slot]); // budget spent: solidify
                }
            }
        } else if let Some(s) = active_slot {
            finalize_path(&mut n.paths[s]); // released the button: solidify
        }
    }

    // per-node decay: the oldest nodes fade first ("nodes go away after time N"); reclassify a
    // finalized path whose geometry changed, and free a slot that emptied out.
    for p in n.paths.iter_mut() {
        if !p.active() || p.owner < 0 {
            continue; // owner<0 = baked stage stroke: permanent, classified at load
        }
        let before = p.len;
        let life = p.props.node_life as u64;
        let mut dead = 0;
        while (dead as usize) < p.len as usize && tick.saturating_sub(p.born[dead as usize]) > life {
            dead += 1;
        }
        if dead > 0 {
            p.trim_front(dead);
        }
        if p.len == 0 {
            *p = InkPath::EMPTY;
        } else if p.len != before && !p.drawing {
            classify(p);
        }
    }
}

/// Stop drawing a path and cache its per-segment surface classes (the grabbability the collision read
/// consumes). Called on button release or budget exhaustion.
fn finalize_path(p: &mut InkPath) {
    p.drawing = false;
    classify(p);
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
        n.arm_hits();
        n.grab_link = -1;
        n.vel.x *= 0.25; // plant feet for the reach
        n.state = CharState::Grab;
        true
    } else if atk || n.live(Lane::Attack) == Action::Attack {
        n.clear_lane(Lane::Attack);
        n.arm_hits();
        if matches!(n.state, CharState::Dash | CharState::Run) {
            // attacking out of momentum = a dash attack: drive a forward lunge (faster than a plain
            // run) so it carries even from a standing dash, then the arm slides it out.
            n.vel.x = n.facing * t.run_speed * 1.25;
            n.state = CharState::DashAttack;
        } else if i.down {
            // down + attack from a standing/crouching pose = the down-tilt pothole.
            n.state = CharState::Dtilt;
            n.vel.x *= 0.25;
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
        let out = step(&s, &[&jump, &idle], &t);
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
        let out = step(&s, &[&jump, &idle], &t);
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
        let s1 = step(&s, &[&left, &idle], &t);
        assert_eq!(s1.fighters[0].facing, -1.0, "facing flips on the reversal frame");
        assert!(s1.fighters[0].vel.x > 0.0, "velocity must NOT teleport to the new direction");

        // hold left a few more frames: momentum bleeds through 0 and goes negative.
        let mut cur = s1;
        for _ in 0..8 {
            cur = step(&cur, &[&left, &idle], &t);
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
        let mut cur = step(&s, &[&up_b, &Default::default()], &t);
        assert_eq!(cur.fighters[0].state, CharState::SpecialU, "stick-up special = up-B");

        // run it out: it leaves the ground rising, then becomes Helpless
        let mut saw_rise = false;
        let mut saw_helpless = false;
        for _ in 0..60 {
            cur = step(&cur, &[&hold, &Default::default()], &t);
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
            let mut cur = step(&s, &[&press, &Default::default()], &t);
            let mut prev = cur.fighters[0].pos;
            for _ in 0..40 {
                cur = step(&cur, &[&hold, &Default::default()], &t);
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
        let cur = step(&s, &[&nb, &Default::default()], &t);
        assert_eq!(cur.fighters[0].state, CharState::SpecialN, "neutral stick + special = neutral-B");
        // a few frames in, still grounded and still in the punch (not launched into the air)
        let mut c = cur;
        for _ in 0..6 {
            c = step(&c, &[&Default::default(), &Default::default()], &t);
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
            let cur = step(&s, &[&press, &Default::default()], &t);
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
        let mut c = step(&s, &[&grab, &idle], &t);
        for _ in 0..(t.grab_startup + t.grab_active) {
            c = step(&c, &[&idle, &idle], &t);
        }
        assert_eq!(c.fighters[0].state, CharState::GrabHold, "grabber holds");
        assert_eq!(c.fighters[1].state, CharState::Grabbed, "victim held");
        assert_eq!(c.fighters[0].grab_link, 1);
        assert_eq!(c.fighters[1].grab_link, 0);

        // pummel raises the victim's damage without releasing.
        let pummel = InputFrame { attack: true, ..Default::default() };
        let before = c.fighters[1].damage;
        c = step(&c, &[&pummel, &idle], &t);
        assert!(c.fighters[1].damage > before, "pummel deals damage");
        assert_eq!(c.fighters[0].state, CharState::GrabHold, "still holding after pummel");

        // re-press grab = throw: victim launched, both unlinked.
        c = step(&c, &[&grab, &idle], &t);
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
        let mut c = step(&s, &[&grab, &idle], &t);
        assert_eq!(c.fighters[0].state, CharState::Grab, "entered grab");
        for _ in 0..(t.grab_startup + t.grab_active + t.grab_recovery + 1) {
            c = step(&c, &[&idle, &idle], &t);
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
        let c = step(&s, &[&idle, &idle], &t);
        assert_eq!(c.fighters[0].state, CharState::Knockdown, "missed tech -> floored");
        assert!(!c.fighters[0].tumble, "tumble cleared on knockdown");
    }

    #[test]
    fn launched_victim_falls_back_to_ground() {
        // A fighter popped up with hitstun must arc back down under gravity, not hover. Regression
        // for the "opponent floats after a hit" bug.
        let t = Tune::from_char(&CharData::KNEEMAN);
        let mut s = SimState::spawn();
        let f = &mut s.fighters[0];
        f.state = CharState::Air;
        f.ground_plat = -1;
        f.pos = Vector2::new(600.0, GROUND_Y);
        f.vel = Vector2::new(0.0, -900.0); // straight up (won't blast off the top: apex ~95px)
        f.hitstun = 24;
        let idle = InputFrame::default();
        let mut c = s;
        let apex = {
            let mut hi = c.fighters[0].pos.y;
            for _ in 0..120 {
                c = step(&c, &[&idle, &idle], &t);
                hi = hi.min(c.fighters[0].pos.y); // smaller y = higher
            }
            hi
        };
        assert!(apex < GROUND_Y - 50.0, "victim actually rose off the launch");
        assert!(
            c.fighters[0].pos.y >= GROUND_Y - 1.0,
            "victim fell back to the floor (no hover): y={}",
            c.fighters[0].pos.y
        );
    }

    #[test]
    fn aerial_neutral_b_does_not_air_stall() {
        // Neutral-B in the air must impulse + fall, not hover in place. Regression for Falcon-B
        // hovering; also pins the forward impulse direction.
        let t = Tune::from_char(&CharData::KNEEMAN);
        let mut s = SimState::spawn();
        let f = &mut s.fighters[0];
        f.state = CharState::SpecialN;
        f.frame = 0;
        f.ground_plat = -1; // airborne
        f.facing = 1.0;
        f.pos = Vector2::new(600.0, GROUND_Y - 600.0); // high up, room to fall
        f.vel = Vector2::ZERO;
        let idle = InputFrame::default();
        let start_y = f.pos.y;
        let mut c = s;
        // run past the launch frame (Punch startup) plus a bit
        for _ in 0..30 {
            c = step(&c, &[&idle, &idle], &t);
        }
        assert!(
            c.fighters[0].pos.y > start_y + 100.0,
            "aerial neutral-B descends instead of hovering: dy={}",
            c.fighters[0].pos.y - start_y
        );
        assert!(
            c.fighters[0].vel.x.abs() > 1.0 || c.fighters[0].pos.x > 600.0,
            "neutral-B carries a forward impulse, not a planted stall"
        );
    }

    #[test]
    fn shield_at_impact_techs_in_place() {
        let t = Tune::from_char(&CharData::KNEEMAN);
        let s = launched(&t);
        let tech = InputFrame { shield_pressed: true, ..Default::default() };
        let idle = InputFrame::default();
        let c = step(&s, &[&tech, &idle], &t);
        assert_eq!(c.fighters[0].state, CharState::TechInPlace, "teched the landing");
        assert!(c.fighters[0].intangible, "tech is intangible");
    }

    #[test]
    fn directional_tech_rolls() {
        let t = Tune::from_char(&CharData::KNEEMAN);
        let s = launched(&t);
        let tech_left = InputFrame { shield_pressed: true, dir: -1.0, ..Default::default() };
        let idle = InputFrame::default();
        let c = step(&s, &[&tech_left, &idle], &t);
        assert_eq!(c.fighters[0].state, CharState::TechRoll, "held a direction -> tech roll");
        assert!(c.fighters[0].vel.x < 0.0, "rolls in the held direction");
    }

    #[test]
    fn knockdown_auto_getups_to_stand() {
        let t = Tune::from_char(&CharData::KNEEMAN);
        let s = launched(&t);
        let idle = InputFrame::default();
        let mut c = step(&s, &[&idle, &idle], &t);
        assert_eq!(c.fighters[0].state, CharState::Knockdown);
        for _ in 0..(t.knockdown_frames + t.getup_frames + 2) {
            c = step(&c, &[&idle, &idle], &t);
        }
        assert_eq!(c.fighters[0].state, CharState::Stand, "floored -> getup -> stand");
    }
}

#[cfg(test)]
mod art_pass_tests {
    use super::*;

    // Sex-kick nair: a strong early box (id 0) then a weaker, shallower tail box (id 1) at the same
    // limb. Two windowed boxes, sequenced on the shared frame clock. One swing, two payoffs.
    #[test]
    fn nair_sex_kick_has_strong_early_weak_late() {
        let t = Tune::from_char(&CharData::KNEEMAN);
        let n = t.nair;
        assert_eq!(n.nbox, 2, "nair is a two-box sex kick");
        let early = &n.boxes[0];
        let late = &n.boxes[1];
        // the tail opens after the early window closes (sequenced, not overlapping).
        assert_eq!(late.start, early.start + early.len, "tail follows the early window");
        assert!(early.damage > late.damage, "early hit must beat the tail");
        assert!(early.angle > late.angle, "tail launches shallower than the early pop");
        // first box opens at startup; total spans both windows + recovery.
        let early_frame = n.startup;
        assert!(n.box_at(early_frame).is_some(), "a box is live on the first active frame");
    }

    // A single-window attack reports the same box across its whole window (no tail to switch to).
    #[test]
    fn single_window_attack_is_constant() {
        let t = Tune::from_char(&CharData::KNEEMAN);
        let d = t.dash_attack;
        assert_eq!(d.nbox, 1, "dash attack is one window");
        let b0 = d.boxes[0];
        let a = d.box_at(b0.start).copied();
        let b = d.box_at(b0.start + b0.len - 1).copied();
        assert_eq!(a, b, "one box -> identical payoff every active frame");
        assert!(d.box_at(b0.start + b0.len).is_none(), "window closes after len frames");
    }

    /// Two grounded fighters facing each other, p1 a hair in front of p0 (inside jab reach).
    fn sparring() -> (SimState, Tune) {
        let t = Tune::from_char(&CharData::KNEEMAN);
        let mut s = SimState::spawn();
        for (k, (x, face)) in [(600.0_f32, 1.0_f32), (660.0_f32, -1.0_f32)].into_iter().enumerate() {
            let f = &mut s.fighters[k];
            f.state = CharState::Stand;
            f.ground_plat = 0;
            f.pos = Vector2::new(x, GROUND_Y);
            f.facing = face;
        }
        (s, t)
    }

    // 3-punch jab: one attack press lands three sequenced hits (count the victim's damage jumps).
    #[test]
    fn jab_autocombo_lands_three_hits() {
        let (s, t) = sparring();
        let idle = InputFrame::default();
        let attack = InputFrame { attack: true, ..Default::default() };
        let mut c = step(&s, &[&attack, &idle], &t);
        assert_eq!(c.fighters[0].state, CharState::Jab, "press enters the jab");
        let mut hits = 0;
        let mut prev = c.fighters[1].damage;
        for _ in 0..(t.jab.total() + 4) {
            c = step(&c, &[&idle, &idle], &t);
            if c.fighters[1].damage > prev + 0.001 {
                hits += 1;
                prev = c.fighters[1].damage;
            }
        }
        assert_eq!(hits, 3, "the 3-punch jab connects three times");
    }

    // A hit forces the victim into Launched and zeroes its frame, cancelling a move it was mid-swing.
    #[test]
    fn hit_interrupts_into_launched() {
        let (mut s, t) = sparring();
        // p1 is mid-nair (its own hitbox window open) when p0's jab lands.
        s.fighters[1].state = CharState::Nair;
        s.fighters[1].frame = 6;
        s.fighters[1].ground_plat = -1;
        s.fighters[1].arm_hits();
        let idle = InputFrame::default();
        let attack = InputFrame { attack: true, ..Default::default() };
        let mut c = step(&s, &[&attack, &idle], &t);
        // run until the first jab box connects (within startup+a few frames).
        for _ in 0..8 {
            if c.fighters[1].state == CharState::Launched {
                break;
            }
            c = step(&c, &[&idle, &idle], &t);
        }
        assert_eq!(c.fighters[1].state, CharState::Launched, "the hit launches the victim");
        assert_eq!(c.fighters[1].frame, 0, "launch resets the victim's state frame");
        assert!(c.fighters[1].hitstun > 0, "victim is in hitstun");
    }

    // Lowest live id wins per victim: when two boxes overlap a target, the sweetspot (id 0) pays out.
    #[test]
    fn lowest_id_box_wins() {
        let t = Tune::from_char(&CharData::KNEEMAN);
        // craft a 2-box move where both boxes are live + overlapping on the same frame.
        let mut atk = AttackData::one(0, 4, 4, Hitbox {
            off: Vector2::new(40.0, -64.0), r: 40.0, damage: 12.0, angle: 90.0,
            bkb: 30.0, kbg: 60.0, ..Hitbox::NONE
        });
        atk.boxes[1] = Hitbox { id: 1, start: 0, len: 4, off: Vector2::new(40.0, -64.0), r: 40.0,
            damage: 2.0, angle: 10.0, bkb: 4.0, kbg: 8.0, set_kb: 0.0, transcendent: false, refresh: 0 };
        atk.nbox = 2;
        // both windows contain frame 1; box_at must return the id-0 (12%) box, not the id-1 (2%) one.
        let chosen = atk.box_at(1).copied().unwrap();
        assert_eq!(chosen.id, 0, "lowest id wins");
        assert_eq!(chosen.damage, 12.0, "the sweetspot pays out, not the sourspot");
        let _ = t;
    }

    // Per-state hurtbox: crouching pulls the circle lower and smaller than standing, so a high jab
    // can sail over a duck. Knockdown is lower still.
    #[test]
    fn crouch_hurtbox_ducks_under_standing() {
        let mut f = Fighter::spawn(600.0, 1.0);
        f.state = CharState::Stand;
        let (sc, sr) = hurtbox(&f);
        f.state = CharState::Crouch;
        let (cc, cr) = hurtbox(&f);
        assert!(cc.y > sc.y, "crouch center sits lower (closer to the feet)");
        assert!(cr < sr, "crouch body shrinks");
        f.state = CharState::Knockdown;
        let (kc, _) = hurtbox(&f);
        assert!(kc.y > cc.y, "floored is lower than a crouch");
    }
}

#[cfg(test)]
mod geo_wiring_tests {
    use super::*;
    use geo::{Geometry, NaiveGeom};

    // Place an airborne fighter just left of the stage's left wall, in the wall band, moving INTO it.
    fn at_left_wall(vel: Vector2, tumble: bool) -> SimState {
        let mut s = SimState::spawn();
        let f = &mut s.fighters[0];
        f.state = CharState::Air;
        f.air_jumps = 0;
        f.pos = Vector2::new(FLOOR_LEFT - ECB_HALF_W + 4.0, GROUND_Y + 90.0); // overlaps face, cy in band
        f.vel = vel;
        f.tumble = tumble;
        s
    }

    // A launched (tumbling) body driven into the wall reflects back off it (geo::reflect, e>0).
    #[test]
    fn tumbling_body_bounces_off_wall() {
        let t = Tune::default();
        assert!(t.wall_bounce > 0.0);
        let s = at_left_wall(Vector2::new(900.0, 0.0), true);
        let idle = InputFrame::default();
        let out = step(&s, &[&idle, &idle], &t);
        let f = &out.fighters[0];
        assert!(f.vel.x < 0.0, "tumbling into the wall kicks back the other way, got {}", f.vel.x);
        assert!((f.pos.x + ECB_HALF_W) <= FLOOR_LEFT + 1.0, "still depenetrated out of the wall");
    }

    // A non-launched body dead-stops on the wall (reflect with e=0): no horizontal velocity left.
    #[test]
    fn neutral_body_dead_stops_on_wall() {
        let t = Tune::default();
        let s = at_left_wall(Vector2::new(900.0, 0.0), false);
        let idle = InputFrame::default();
        let out = step(&s, &[&idle, &idle], &t);
        assert!(out.fighters[0].vel.x.abs() < 1e-3, "no bounce without tumble (e=0 dead stop)");
    }

    // The swept landing primitive rides the actual stage surface: a feet-circle falling onto the
    // main platform's geo `platform_top` segment reports a forward time-of-impact with an upward
    // normal. This proves the geo path matches the live AABB landing surface (drawn-stage seam).
    #[test]
    fn swept_landing_rides_platform_top_segment() {
        let g = NaiveGeom;
        let top = platform_top(&PLATFORMS[0]); // solid main stage top at GROUND_Y
        let feet = (geo::Iso::at(Vector2::new(600.0, GROUND_Y - 100.0)), geo::Shape::Ball { r: 6.0 });
        let hit = g
            .cast_shapes(feet, Vector2::new(0.0, 100.0), top, Vector2::ZERO, 2.0)
            .expect("feet falling onto the main stage must register a landing");
        assert!(hit.time_of_impact > 0.0 && hit.time_of_impact <= 2.0);
        assert!(hit.normal1.y < 0.0, "landing normal points up out of the platform");
    }
}
