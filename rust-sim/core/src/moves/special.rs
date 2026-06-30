//! Special moves (the B-slots): the `SpecialKind`/`SpecialMove` loadout records, the stick->slot
//! routing, and the per-frame run logic. Re-exported through `moves`.

use crate::{
    airborne, move_toward, sign, Action, AttackData, CharState, Fighter, Hitbox, InputFrame, Lane,
    Tune, Vector2, DT,
};

/// Special states map to a slot in `Tune.specials` (the seed for swappable move loadouts).
pub(crate) fn special_slot(st: CharState) -> Option<usize> {
    match st {
        CharState::SpecialN => Some(0),
        CharState::SpecialS => Some(1),
        CharState::SpecialU => Some(2),
        CharState::SpecialD => Some(3),
        _ => None,
    }
}
pub(crate) fn is_special(st: CharState) -> bool {
    special_slot(st).is_some()
}

/// Which special the stick selects at the press: up / down / side / neutral.
pub(crate) fn special_dir(aim: Vector2) -> usize {
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
    // While airborne and running this move, gravity does not pull: the fighter holds its burst
    // velocity (Ness/Lucas-style floaty up-B). Off => normal gravity + air drift, so the move arcs
    // and the fighter falls. No move "air-stalls" implicitly anymore; it's opt-in per loadout slot.
    pub no_gravity: bool,
}

impl SpecialMove {
    // Default kit (Falcon-ish): heavy neutral-B punch, a side lunge, a rising recovery, a down drive.
    pub(crate) const PUNCH: Self = Self {
        kind: SpecialKind::Punch,
        hit: AttackData::one(14, 4, 26, Hitbox {
            off: Vector2::new(58.0, -60.0), r: 46.0, damage: 22.0,
            angle: 38.0, bkb: 30.0, kbg: 84.0, ..Hitbox::NONE
        }),
        // Falcon-punch surge: lunge forward into the hit, not a mid-air hover. Grounded, friction
        // bleeds it to the planted step; aerial, gravity arcs him down after the lunge.
        move_x: 380.0,
        move_y: -60.0,
        no_gravity: false,
    };
    pub(crate) const LUNGE: Self = Self {
        kind: SpecialKind::Lunge,
        hit: AttackData::one(8, 6, 22, Hitbox {
            off: Vector2::new(60.0, -58.0), r: 42.0, damage: 9.0,
            angle: 55.0, bkb: 20.0, kbg: 52.0, ..Hitbox::NONE
        }),
        move_x: 900.0,
        move_y: -120.0,
        no_gravity: false,
    };
    pub(crate) const RISE: Self = Self {
        kind: SpecialKind::Rise,
        hit: AttackData::one(6, 8, 22, Hitbox {
            off: Vector2::new(20.0, -90.0), r: 44.0, damage: 7.0,
            angle: 80.0, bkb: 24.0, kbg: 38.0, ..Hitbox::NONE
        }),
        move_x: 380.0,
        move_y: -1500.0,
        no_gravity: false,
    };
    pub(crate) const DROP: Self = Self {
        kind: SpecialKind::Fall,
        hit: AttackData::one(8, 10, 18, Hitbox {
            off: Vector2::new(24.0, 10.0), r: 44.0, damage: 10.0,
            angle: -68.0, bkb: 22.0, kbg: 42.0, ..Hitbox::NONE // downward: a spike
        }),
        move_x: 220.0,
        move_y: 700.0,
        no_gravity: false,
    };
}

/// Consume a buffered special if one is live: pick the slot from the captured stick, enter the
/// matching state, face the stick on a side-B. Available from every actionable ground/air state.
pub(crate) fn try_special(n: &mut Fighter) -> bool {
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
    n.arm_hits();
    true
}

/// Run one frame of a special. The launch burst lands when the active window opens; gravity/friction
/// run by whether we're airborne (`ground_plat < 0`). Up-B ends in Helpless if it finishes in the air.
pub(crate) fn run_special(n: &mut Fighter, slot: usize, i: &InputFrame, t: &Tune) {
    let m = t.specials[slot];
    // Up-B lifts off on frame 0 (instant recovery, no ground-snap); the rest burst at the active
    // window. The hitbox window (startup..) is independent of this movement timing.
    let launch_frame = if m.kind == SpecialKind::Rise { 0 } else { m.hit.startup };
    if n.frame == launch_frame {
        // Every kind fires its impulse vector (facing-relative, or stick-relative for the recovery).
        // Punch/Lunge/Fall drive along facing; Rise aims with the stick. Lunge/Rise leave the ground.
        match m.kind {
            SpecialKind::Punch => n.vel = Vector2::new(n.facing * m.move_x, m.move_y),
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
        if m.no_gravity {
            // floaty move (e.g. a PSI recovery): hold the burst, only bleed horizontal slowly. No
            // gravity, so it hangs instead of arcing — the opt-in air-stall, never the default.
            n.vel.x = move_toward(n.vel.x, 0.0, t.air_friction * DT);
        } else {
            n.vel.x = move_toward(n.vel.x, i.dir * t.air_speed * 0.6, t.air_accel * DT);
            n.vel.y += t.gravity * DT;
            if n.vel.y > t.max_fall {
                n.vel.y = t.max_fall;
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

