// Engine-agnostic vectors. `Vector2` is kept as the local name (minimal churn from the
// godot original); the shell converts to godot::Vector2 at the render boundary.
pub use glam::Vec2 as Vector2;

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
pub const ECB_HALF_W: f32 = 24.0; // left/right vert offset from center (x)
pub const ECB_HALF_H: f32 = 42.0; // top/bottom vert offset from center (y)

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
const BLAST_Y: f32 = 1600.0; // fall past this = death -> respawn

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

pub const DUMMY_R: f32 = 28.0;    // training dummy hurtbox radius (circle)
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
}

fn airborne(st: CharState) -> bool {
    matches!(st, CharState::Air | CharState::AirDodge | CharState::Nair)
}

fn is_ledge(st: CharState) -> bool {
    matches!(st, CharState::LedgeHold | CharState::LedgeClimb)
}

/// One attack's frame data + hitbox + knockback, in pixel space (prototype values; tune freely).
/// The hitbox is a circle offset from the fighter (x flipped by facing); active only in its window.
#[derive(Copy, Clone)]
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

impl AttackData {
    pub fn total(&self) -> i64 {
        self.startup + self.active + self.recovery
    }

    // baseline definitions; live copies live in Tune so the panel can edit them.
    const JAB: Self = Self {
        startup: 3,
        active: 3,
        recovery: 9,
        off: Vector2::new(40.0, -48.0),
        r: 24.0,
        damage: 3.0,
        kb_base: 320.0,
        kb_scale: 3.0,
        kb_angle: 35.0,
    };
    const NAIR: Self = Self {
        startup: 5,
        active: 9,
        recovery: 14,
        off: Vector2::new(28.0, -48.0),
        r: 34.0,
        damage: 8.0,
        kb_base: 520.0,
        kb_scale: 4.2,
        kb_angle: 45.0,
    };
}

/// The (live) attack definition for a state, if it is one.
pub fn attack_for(t: &Tune, st: CharState) -> Option<AttackData> {
    match st {
        CharState::Jab => Some(t.jab),
        CharState::Nair => Some(t.nair),
        _ => None,
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

/// A queued action from the input buffer. Recorded on the button edge with the aim at that
/// moment, then consumed when the state machine reaches a point where it can act. This is what
/// makes wavedash / jump-out-of-lag / the down-diagonal feel reliable instead of frame-perfect.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum Buffered {
    #[default]
    None,
    Jump,
    ShortHop,
    AirDodge,
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
    pub buffered: Buffered,// queued action from the input buffer
    pub buf_timer: i64,    // frames the buffered action stays valid
    pub buf_aim: Vector2,  // aim captured/refreshed for the buffered action (the diagonal)
    pub aerial_buf: i64,   // dedicated aerial buffer (separate slot so jump+attack holds both)
    pub autohop_aerial: bool, // current aerial came from the jump+attack auto-short-hop (reduced dmg)
    pub intangible: bool,  // dodge / ledge i-frames (drives the debug color)
    pub regrab_lock: i64,  // frames before a ledge can be re-grabbed
    pub ground_plat: i32,  // index into PLATFORMS the fighter stands on (-1 = airborne)
    pub attack_hit: bool,  // current attack already connected (one hit per swing)
    pub hitlag: i64,       // impact freeze on connect (this fighter held)
    pub damage: f32,       // accumulated % (knockback scales with this)
    pub hitstun: i64,      // frames launched/can't act (drives the hit flash + knockback slide)
}

/// The entire sim state as a plain value: two fighters. This is what the BehaviorSubject holds,
/// what ggrs saves/rolls back, and what egui renders. `Copy` so snapshots are free.
#[derive(Copy, Clone, PartialEq)]
pub struct SimState {
    pub fighters: [Fighter; 2],
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
            buffered: Buffered::None,
            buf_timer: 0,
            buf_aim: Vector2::ZERO,
            aerial_buf: 0,
            autohop_aerial: false,
            intangible: false,
            regrab_lock: 0,
            ground_plat: -1,
            attack_hit: false,
            hitlag: 0,
            damage: 0.0,
            hitstun: 0,
        }
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
        }
    }
}

impl SimState {
    /// Two fighters facing each other on the main stage (airborne drop-in).
    pub fn spawn() -> Self {
        Self {
            fighters: [Fighter::spawn(480.0, 1.0), Fighter::spawn(720.0, -1.0)],
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
    pub spotdodge_frames: i64,
    pub roll_frames: i64,
    pub airdodge_frames: i64,
    pub ledge_intang: i64,
    pub climb_frames: i64,
    pub buffer_frames: i64,
    pub jab: AttackData,
    pub nair: AttackData,
    pub autohop_dmg: f32, // damage multiplier for auto-short-hop aerials (jump+attack macro)
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
            spotdodge_frames: c.spotdodge_frames,
            roll_frames: c.roll_frames,
            airdodge_frames: c.airdodge_frames,
            ledge_intang: c.ledge_intang,
            climb_frames: c.climb_frames,
            buffer_frames: c.buffer_frames,
            jab: AttackData::JAB,
            nair: AttackData::NAIR,
            autohop_dmg: 0.85, // Ultimate-ish 15% cut on the easy jump+attack aerial
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
    pub attack: bool,        // attack pressed THIS frame (jab / aerial)
}

/// PURE scan step: (state, input, tune) -> next state.
/// No engine calls, no IO, no &mut self. Deterministic given the same inputs.
/// `states = inputs.scan(SimState::spawn(), step)`.
/// One tick of the whole sim: advance each fighter from its own input, then resolve combat
/// both directions. Pure value-in/value-out — this is what ggrs calls (possibly N times per
/// frame during rollback). `inputs[k]` drives `fighters[k]`.
pub fn step(s: &SimState, inputs: [&InputFrame; 2], t: &Tune) -> SimState {
    let mut n = *s;
    advance(&mut n.fighters[0], inputs[0], t);
    advance(&mut n.fighters[1], inputs[1], t);
    // split the array so both fighters can be borrowed mutably at once
    let (l, r) = n.fighters.split_at_mut(1);
    resolve_combat(&mut l[0], &mut r[0], t); // p0 attacks p1
    resolve_combat(&mut r[0], &mut l[0], t); // p1 attacks p0
    n
}

/// Advance ONE fighter by one frame from its own input: buffer, state machine, integrate +
/// stage collision. No cross-fighter combat (that is `resolve_combat`). Mutates in place.
fn advance(f: &mut Fighter, i: &InputFrame, t: &Tune) {
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
        return;
    }

    // launched: skip the state machine, run the knockback slide (the old training-dummy physics).
    // Friction bleeds horizontal, gravity arcs it down, feet settle on the floor, hitstun ticks.
    if n.hitstun > 0 {
        n.pos += n.vel * DT;
        n.vel.x = move_toward(n.vel.x, 0.0, DUMMY_FRICTION * DT);
        if n.pos.y < GROUND_Y {
            n.vel.y += t.gravity * DT; // arc back down
        } else {
            n.pos.y = GROUND_Y;
            n.vel.y = 0.0;
        }
        n.hitstun -= 1;
        if n.hitstun == 0 {
            // recover to an actionable state: airborne -> Air, on the floor -> Stand
            if n.pos.y < GROUND_Y {
                n.state = CharState::Air;
                n.ground_plat = -1;
            } else {
                n.state = CharState::Stand;
                n.ground_plat = 0;
            }
        }
        if n.pos.y > BLAST_Y {
            *f = Fighter::spawn(spawn_x(n.pos.x), n.facing);
            return;
        }
        *f = n;
        return;
    }

    if n.regrab_lock > 0 {
        n.regrab_lock -= 1;
    }

    // ── input buffer: age the current entry (keeping its aim fresh), then record new edges ──
    if n.buf_timer > 0 {
        n.buf_timer -= 1;
        if n.buf_timer == 0 {
            n.buffered = Buffered::None;
        } else if aim.length() > 0.3 {
            n.buf_aim = aim; // latest non-neutral aim within the window wins (the diagonal)
        }
    }
    if i.shorthop {
        n.buffered = Buffered::ShortHop;
        n.buf_timer = t.buffer_frames + 1; // +1 so the press frame always counts; 0 = no lookahead
        n.buf_aim = aim;
    } else if i.jump {
        n.buffered = Buffered::Jump;
        n.buf_timer = t.buffer_frames + 1; // +1 so the press frame always counts; 0 = no lookahead
        n.buf_aim = aim;
    }
    // shield press only buffers an air dodge when airborne or mid-jumpsquat (else it's a shield)
    if i.shield_pressed && (airborne(n.state) || n.state == CharState::JumpSquat) {
        n.buffered = Buffered::AirDodge;
        n.buf_timer = t.buffer_frames + 1; // +1 so the press frame always counts; 0 = no lookahead
        n.buf_aim = aim;
    }
    // aerial buffer — its OWN slot, separate from the jump/dodge buffer, so a jump+attack combo
    // can hold both at once. Queue an aerial when airborne, mid-jumpsquat, or when attack is
    // pressed together with a jump (the auto-short-hop macro). Grounded attack alone stays a jab.
    if n.aerial_buf > 0 {
        n.aerial_buf -= 1;
    }
    let jumping_now = i.jump || i.shorthop;
    if i.attack && (airborne(n.state) || n.state == CharState::JumpSquat || jumping_now) {
        n.aerial_buf = t.buffer_frames + 1;
    }

    match n.state {
        CharState::Stand => {
            if !try_ground_action(&mut n, i) {
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
            if !try_ground_action(&mut n, i) {
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
            if !try_ground_action(&mut n, i) {
                if sgn != 0.0 && sgn != n.facing && mag >= DASH_THRESH {
                    // dash-dance: flip and re-burst, restart the dash window
                    n.facing = sgn;
                    n.vel.x = sgn * t.dash_init;
                    force_reset = true;
                } else if mag < WALK_THRESH {
                    n.state = CharState::Stand;
                } else {
                    n.vel.x = move_toward(n.vel.x, n.facing * t.run_speed, t.ground_accel * DT);
                    if n.frame >= t.dash_window {
                        n.state = CharState::Run;
                    }
                }
            }
        }
        CharState::Run => {
            if !try_ground_action(&mut n, i) {
                if mag < WALK_THRESH || sgn != n.facing {
                    n.state = CharState::Skid; // release or reverse -> run brake
                } else {
                    n.vel.x = move_toward(n.vel.x, n.facing * t.run_speed, t.ground_accel * DT);
                }
            }
        }
        CharState::Turn => {
            n.vel.x = move_toward(n.vel.x, 0.0, t.ground_friction * DT);
            if !try_ground_action(&mut n, i) && n.frame >= t.pivot_frames {
                n.facing = -n.facing;
                if sgn != 0.0 && mag >= DASH_THRESH {
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
            if !try_ground_action(&mut n, i) {
                if sgn != 0.0 && sgn != n.facing && mag >= DASH_THRESH {
                    n.facing = sgn; // pivot out of the skid
                    n.vel.x = sgn * t.dash_init;
                    n.state = CharState::Dash;
                } else {
                    n.vel.x = move_toward(n.vel.x, 0.0, t.ground_friction * DT);
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
            if !try_ground_action(&mut n, i) && !i.down {
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
                let wavedash = n.buffered == Buffered::AirDodge && n.buf_timer > 0;
                if wavedash {
                    let aim = dodge_aim(&n, i);
                    clear_buffer(&mut n);
                    do_airdodge(&mut n, aim, t); // wavedash: airdodge straight out of jumpsquat
                } else {
                    // jump+attack combo = auto short-hop aerial (Ultimate): force a short hop and
                    // tag the aerial for reduced damage. Set before vel.y so the hop comes out short.
                    if n.aerial_buf > 0 {
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
        CharState::Air => {
            let want_dodge = n.buffered == Buffered::AirDodge && n.buf_timer > 0;
            let want_nair = i.attack || n.aerial_buf > 0;
            if want_nair {
                n.aerial_buf = 0;
                n.state = CharState::Nair; // aerial; keeps drifting, lands into Landing
                n.attack_hit = false;
            } else if want_dodge && n.air_dodges > 0 {
                let a = dodge_aim(&n, i);
                clear_buffer(&mut n);
                do_airdodge(&mut n, a, t); // directional burst; into the ground = wavedash
            } else {
                // double jump: cancels fall (crisp upward pop even while falling fast) and
                // REDIRECTS horizontal from the stick — hold back to reverse momentum.
                let want_djump =
                    matches!(n.buffered, Buffered::Jump | Buffered::ShortHop) && n.buf_timer > 0;
                if want_djump && n.air_jumps > 0 {
                    clear_buffer(&mut n);
                    n.air_jumps -= 1;
                    n.vel.y = t.airjump_v;
                    n.fast_falling = false;
                    if sgn != 0.0 {
                        n.facing = sgn;
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
                // fast fall (instant snap) + gravity
                if !n.fast_falling && n.vel.y > 0.0 && i.down {
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
        } else {
            n.pos.y = p.y;
            n.vel.y = 0.0;
            // edges are sticky: only walk off when actively holding toward the edge, else
            // stop at the lip. Falling off no longer happens just from sliding momentum.
            if n.pos.x < p.left {
                if sgn < 0.0 {
                    n.state = CharState::Air;
                    n.ground_plat = -1;
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

    // blast zone -> respawn this fighter (combat lives in resolve_combat, not here)
    if n.pos.y > BLAST_Y {
        *f = Fighter::spawn(spawn_x(n.pos.x), n.facing);
        return;
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
        CharState::LedgeHold => n.frame < t.ledge_intang,
        _ => false,
    };
    *f = n;
}

/// Which side to respawn on after a blast-zone KO (keep the fighter on its half of the stage).
fn spawn_x(x: f32) -> f32 {
    if x < 600.0 {
        480.0
    } else {
        720.0
    }
}

/// Cross-fighter combat: `a`'s active hitbox vs `b`'s hurtbox (circle/circle), one hit per swing.
/// On connect: damage + knockback + hitstun to `b`, impact freeze (hitlag) to BOTH (the hit "pop").
fn resolve_combat(a: &mut Fighter, b: &mut Fighter, t: &Tune) {
    let Some((hc, hr)) = active_hitbox(a, t) else { return };
    if a.attack_hit {
        return; // already connected this swing
    }
    let (bc, br) = hurtbox(b);
    if (hc - bc).length() > hr + br {
        return; // no overlap
    }
    let atk = attack_for(t, a.state).unwrap();
    a.attack_hit = true;
    let dmg = if a.state == CharState::Nair && a.autohop_aerial {
        atk.damage * t.autohop_dmg // auto short-hop aerial: reduced damage (Ultimate)
    } else {
        atk.damage
    };
    b.damage += dmg;
    let speed = atk.kb_base + atk.kb_scale * b.damage; // knockback scales with accumulated %
    let ang = atk.kb_angle.to_radians();
    b.vel = Vector2::new(ang.cos() * a.facing, -ang.sin()) * speed; // launch away from attacker
    b.hitstun = (speed * 0.12) as i64; // stun scales with knockback
    let freeze = (atk.damage * HITLAG_PER_DMG) as i64 + 4; // both fighters pop on impact
    a.hitlag = freeze;
    b.hitlag = freeze;
}

/// Jump / shield are available from every actionable ground state; factor them out.
/// Jump comes from the buffer so a slightly-early press still fires.
fn try_ground_action(n: &mut Fighter, i: &InputFrame) -> bool {
    if let Some(full) = take_jump(n) {
        n.state = CharState::JumpSquat;
        n.full_hop = full;
        true
    } else if i.attack {
        n.state = CharState::Jab;
        n.attack_hit = false;
        n.vel.x *= 0.25; // plant feet: a dash/run jab mostly kills momentum on startup
        true
    } else if i.shield_held {
        n.state = CharState::Shield;
        true
    } else {
        false
    }
}

fn clear_buffer(n: &mut Fighter) {
    n.buffered = Buffered::None;
    n.buf_timer = 0;
}

/// Consume a buffered jump/shorthop if one is live. Returns Some(full_hop) when taken.
fn take_jump(n: &mut Fighter) -> Option<bool> {
    if n.buf_timer == 0 {
        return None;
    }
    match n.buffered {
        Buffered::Jump => {
            clear_buffer(n);
            Some(true)
        }
        Buffered::ShortHop => {
            clear_buffer(n);
            Some(false)
        }
        _ => None,
    }
}

/// The aim to use for a buffered air dodge: the buffered diagonal if set, else the live stick.
fn dodge_aim(n: &Fighter, i: &InputFrame) -> Vector2 {
    if n.buf_aim.length() > 0.3 {
        n.buf_aim
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
