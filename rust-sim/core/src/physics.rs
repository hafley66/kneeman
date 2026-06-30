//! Kinematics: the motion morphisms the FSM and integrator apply once a `CharState` is chosen.
//! Trajectory DI, air drift, air dodge, ledge snap, plus the unit/threshold constants and the
//! scalar math helpers. Pure value-in/value-out; re-exported at the crate root.

use crate::{CharState, Fighter, InputFrame, Lane, Tune, Vector2, DT, FPS, GROUND_Y, LEDGE_HANG_DY, PX_PER_UNIT};

pub(crate) const DASH_THRESH: f32 = 0.5; // |stick| past this from neutral = dash (keyboard digital is always 1.0)
pub(crate) const WALK_THRESH: f32 = 0.25; // |stick| past this but under DASH = walk (needs analog stick)
pub(crate) const STOP_EPS: f32 = 1.0; // |vel.x| under this in a braking state snaps to 0
pub(crate) const WALL_TILT_FRAMES: i64 = 12; // how long after a wall bounce the shell tilts the sprite (cosmetic)
pub(crate) const LEDGE_FALL_EPS: f32 = 150.0; // must be falling at least this fast to snap a ledge
pub(crate) const DUMMY_FRICTION: f32 = 1200.0; // px/s^2 the dummy's knockback slide bleeds
pub(crate) const HITLAG_PER_DMG: f32 = 0.8;  // impact-freeze frames per point of damage
// units/frame      -> px/s    (a velocity)
pub(crate) fn vel(u: f32) -> f32 {
    u * FPS * PX_PER_UNIT
}
// units/frame^2    -> px/s^2  (an acceleration)
pub(crate) fn acc(u: f32) -> f32 {
    u * FPS * FPS * PX_PER_UNIT
}

pub(crate) fn sign(x: f32) -> f32 {
    if x > 0.0 {
        1.0
    } else if x < 0.0 {
        -1.0
    } else {
        0.0
    }
}

/// Trajectory DI: the victim's stick rotates a launch toward its component perpendicular to the
/// knockback, up to `max_deg`. Speed is untouched -- only the angle -- so you can steer a launch
/// toward the stage to live, but never cancel your own knockback. Pure, so it rolls back cleanly.
pub(crate) fn apply_di(vel: Vector2, stick: Vector2, max_deg: f32) -> Vector2 {
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

/// The aim to use for a buffered air dodge: the movement lane's captured diagonal if set, else the
/// live stick.
pub(crate) fn dodge_aim(n: &Fighter, i: &InputFrame) -> Vector2 {
    let m = &n.buf[Lane::Movement as usize];
    if m.aim.length() > 0.3 {
        m.aim
    } else {
        Vector2::new(i.dir, i.aim_y)
    }
}

/// Directional air-dodge burst from a 2D aim (digital diagonals included). Neutral aim = a
/// dodge in place. Into the ground a frame later, the surviving horizontal becomes a wavedash.
pub(crate) fn do_airdodge(n: &mut Fighter, aim: Vector2, t: &Tune) {
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

pub(crate) fn grab_ledge(n: &mut Fighter, t: &Tune, edge_x: f32, face: f32) {
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
pub(crate) fn air_drift(n: &mut Fighter, i: &InputFrame, t: &Tune, sgn: f32) {
    let target = i.dir * t.air_speed;
    if sgn == 0.0 {
        n.vel.x = move_toward(n.vel.x, 0.0, t.air_friction * DT); // coast
    } else if n.vel.x.abs() <= t.air_speed || sign(n.vel.x) != sgn {
        n.vel.x = move_toward(n.vel.x, target, t.air_accel * DT); // turn / accel
    } else {
        n.vel.x = move_toward(n.vel.x, target, t.air_friction * DT); // keep momentum
    }
}

pub(crate) fn move_toward(from: f32, to: f32, delta: f32) -> f32 {
    if (to - from).abs() <= delta {
        to
    } else {
        from + (to - from).signum() * delta
    }
}
