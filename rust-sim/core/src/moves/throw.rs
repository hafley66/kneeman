//! Grabs + throws: the `ThrowData` launch records, the grab catch/hold/pummel/mash-out resolution,
//! and the throw release. Cross-fighter, so `resolve_grab` owns the held pair. Re-exported through `moves`.

use crate::{hurtbox, sign, CharState, Fighter, InputFrame, Tune, Vector2, ECB_HALF_H, HITLAG_PER_DMG};

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
    pub(crate) const FWD: Self = Self { damage: 8.0, kb_base: 520.0, kb_scale: 4.2, kb_angle: 42.0 };
    pub(crate) const BACK: Self = Self { damage: 10.0, kb_base: 620.0, kb_scale: 4.6, kb_angle: 44.0 };
    pub(crate) const UP: Self = Self { damage: 7.0, kb_base: 560.0, kb_scale: 4.4, kb_angle: 88.0 };
    pub(crate) const DOWN: Self = Self { damage: 6.0, kb_base: 440.0, kb_scale: 3.6, kb_angle: 72.0 };
}

// --- grabs ---------------------------------------------------------------------------------------

pub(crate) const GRAB_HELD_X: f32 = 64.0;  // how far in front of the grabber the victim is pinned
pub(crate) const GRAB_CATCH_R: f32 = 36.0; // slop added to the reach-vs-hurtbox catch test
pub(crate) const KNOCKDOWN_LOCK: i64 = 6;  // floored frames before any getup option is allowed

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
pub(crate) fn resolve_grab(g: &mut Fighter, v: &mut Fighter, gi: i8, vi: i8, g_in: &InputFrame, v_in: &InputFrame, t: &Tune) {
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



