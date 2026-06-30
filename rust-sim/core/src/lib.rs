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
pub mod tune; // character attributes (CharData) + derived pixel-space feel config (Tune)
pub use tune::*;
mod za_warudo; // the per-fighter state machine (reduce_next_state): freeze, re-derive, resume
use za_warudo::reduce_next_state;

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
/// can't apply alone because it needs the whole `SimState` (the item array). `reduce_next_state` returns one;
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

    /// Tick the re-hit grid down one frame (called once per active frame in `reduce_next_state`).
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
        acts[p] = reduce_next_state(&mut n.fighters[p], &items, &paths, inputs[p], t);
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
/// effects; `reduce_next_state` decided WHAT to do (purely), this carries it out on the shared item array.
fn apply_act(n: &mut SimState, idx: usize, act: Act, t: &Tune) {
    match act {
        Act::None => {}
        Act::Fire { auto } => fire_gun(n, idx, auto, t),
        Act::Drop => drop_item(n, idx),
        Act::Pickup => pickup_item(n, idx, t),
        // node-laying needs the raw stick (cursor/ruler aim), which apply_act doesn't have — it runs
        // in `update_paths`. Act::Draw exists only so `reduce_next_state` suppressed the jab; nothing to do here.
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
