//! `reduce_next_state` -- the per-fighter state machine. One pure step: read the input buffer, run
//! the `CharState` transition table, integrate velocity, resolve stage collision. Cross-fighter
//! combat is NOT here (that is `resolve_combat` in `lib`). Named for the frame-freeze: time stops,
//! every fighter is re-derived, then the world moves again. ZA WARUDO.

use crate::geo;
use crate::{
    Act, Action, CharState, Fighter, InkPath, InputFrame, Item, Lane, Tune, Vector2,
    DASH_THRESH, DT, DUMMY_FRICTION, ECB_HALF_H, ECB_HALF_W, FLOOR_LEFT, FLOOR_RIGHT, GROUND_Y,
    KNOCKDOWN_LOCK, LEDGE_FALL_EPS, LEDGE_REACH_X, MAX_DRAWN, MAX_ITEMS, PLATFORMS, STAGE_BOTTOM,
    STOP_EPS, WALK_THRESH, WALL_TILT_FRAMES,
    aerial_for, air_drift, airborne, attack_for, do_airdodge, dodge_aim, grab_ledge,
    ink_floor_y_at, ink_wall_block, is_special, move_toward, nearest_pickup, out_of_bounds, respawn,
    run_special, sign, try_special,
};


/// Advance ONE fighter by one frame from its own input: buffer, state machine, integrate +
/// stage collision. No cross-fighter combat (that is `resolve_combat`). Mutates in place.
pub(crate) fn reduce_next_state(f: &mut Fighter, items: &[Item; MAX_ITEMS], paths: &[InkPath; MAX_DRAWN], i: &InputFrame, t: &Tune) -> Act {
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

    // ── item intents: emit a pure descriptor; do NOT mutate the input. `reduce_next_state` only reads `items`
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
        // soft ink drops through via the same tilt-window buffer as a soft platform.
        match (drop_through(&mut n, i, t, !p.props.solid), ink_floor_y_at(&p, n.pos.x)) {
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
        if drop_through(&mut n, i, t, !p.solid) {
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

    // drawn ink walls: block horizontal like the solid-stage side faces. A near-vertical Wall
    // segment stops the ECB's leading side vert; a launched (tumbling) body bounces off with
    // wall_bounce restitution, everyone else dead-stops (reflect e=0). Skipped while hanging a ledge.
    if !is_ledge(n.state) {
        for p in paths.iter() {
            if let Some((px, nx)) = ink_wall_block(p, f.pos.x, n.pos, ECB_HALF_W, ECB_HALF_H) {
                n.pos.x = px;
                if n.vel.x * nx < 0.0 {
                    // moving into the wall: reflect (outward normal (nx,0)); e=0 dead-stops that axis
                    let e = if n.tumble { t.wall_bounce } else { 0.0 };
                    n.vel = geo::reflect(n.vel, Vector2::new(nx, 0.0), e);
                }
                break;
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

// ---- FSM-local helpers (relocated from lib: used only by reduce_next_state) ----

fn is_ledge(st: CharState) -> bool {
    matches!(st, CharState::LedgeHold | CharState::LedgeClimb)
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

/// Soft-platform / soft-ink drop-through with a tilt-window buffer (Melee/PM feel). Crouch-holding
/// Down on a soft surface arms `drop_buf = plat_drop_window` and counts it down; while it counts the
/// fighter stays crouched, so a Down+Attack in the window converts to a Dtilt — that leaves Crouch
/// and cancels the pending drop. Releasing Down (Crouch -> Stand) or any non-Crouch state also
/// cancels it. Returns true on the frame the window expires still crouch-holding: that's the drop.
///
/// Keyed on `down` HELD, not the `down_pressed` edge: the shell only fires `down_pressed` for the
/// digital `ui_down` action, so a controller stick / touch stick sets `down` (via pad_down) but
/// never the edge. Reading the held bit makes the drop fire the same from every input source.
/// `soft` is false for the solid main stage / solid ink (never drops).
///
/// Tune `plat_drop_window`: 1 = drop on the crouch frame (near-instant, Melee-ish); larger = more
/// grace to convert into a Dtilt (PM-ish), at the cost of that many frames of drop latency.
fn drop_through(n: &mut Fighter, i: &InputFrame, t: &Tune, soft: bool) -> bool {
    // only a crouch-hold on a soft surface drops. Standing up, releasing Down, or converting to a
    // Dtilt (any non-Crouch state) clears the pending drop. Solid surfaces never drop.
    if !soft || n.state != CharState::Crouch || !i.down {
        n.drop_buf = 0;
        return false;
    }
    if n.drop_buf == 0 {
        n.drop_buf = t.plat_drop_window.max(1); // (re)arm the tilt-window on a fresh crouch-hold
    }
    n.drop_buf -= 1;
    n.drop_buf == 0 // window elapsed with no attack to convert it -> drop through
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
