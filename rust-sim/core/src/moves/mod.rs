//! Moves, grouped by kind. This module holds the shared normal-attack data (frame data + hitbox +
//! knockback records) that grounded attacks, aerials, and tilts all draw from; `special` and `throw`
//! are submodules for those kinds. Moves are DATA indexed by `CharState` (see `attack_for`), so the
//! sim stays `Copy` + deterministic. Re-exported at the crate root.

pub mod special;
pub mod throw;
pub use special::*;
pub use throw::*;

use crate::{CharState, Fighter, Tune, Vector2, DUMMY_R, ECB_HALF_H};
use self::special::special_slot;

/// A weaker lingering tail on an attack: the second hitbox window that opens when the strong early
/// window closes (the "sex kick" — a jump kick that pops hard on the first frames, then hangs out as
/// a soft poke). `len == 0` means no tail (a one-window attack). Shape (off/r) is shared with the
/// primary window: the same limb stays out, only the payoff fades.
#[derive(Copy, Clone, PartialEq, Default)]
pub struct HitLate {
    pub len: i64,      // tail active frames after the primary window (0 = no tail)
    pub damage: f32,   // % on a late connect (weaker than the early hit)
    pub kb_base: f32,  // late base knockback (px/s)
    pub kb_scale: f32, // late knockback growth
    pub kb_angle: f32, // late launch angle° (sex-kicks usually send shallower than the early pop)
}

impl HitLate {
    pub const NONE: Self = Self { len: 0, damage: 0.0, kb_base: 0.0, kb_scale: 0.0, kb_angle: 0.0 };
}

/// One attack's frame data + hitbox + knockback, in pixel space (prototype values; tune freely).
/// The hitbox is a circle offset from the fighter (x flipped by facing); active only in its window.
/// `late` adds an optional second (weaker) window after `active`, sharing the same off/r — the
/// general two-window shape (sex-kick decay today; the seam for N-window multihit later).
#[derive(Copy, Clone, PartialEq)]
pub struct AttackData {
    pub startup: i64,  // wind-up frames before the hitbox turns on
    pub active: i64,   // frames the strong (early) hitbox is live
    pub recovery: i64, // cool-down frames after, then back to neutral
    pub off: Vector2,  // hitbox center offset from fighter pos (x is forward)
    pub r: f32,        // hitbox radius
    pub damage: f32,   // % added on an early hit
    pub kb_base: f32,  // base knockback speed (px/s)
    pub kb_scale: f32, // extra knockback per point of accumulated damage
    pub kb_angle: f32, // launch angle in degrees (0 = forward, 90 = straight up)
    pub late: HitLate, // weaker lingering tail (sex-kick); HitLate::NONE = single-window attack
}

impl AttackData {
    pub fn total(&self) -> i64 {
        self.startup + self.active + self.late.len + self.recovery
    }

    /// Frames the hitbox is live across BOTH windows (early `active` + the lingering `late.len`).
    pub fn active_span(&self) -> i64 {
        self.active + self.late.len
    }

    /// Which window is live at `frame` (relative to state start): the early strong hit, or the late
    /// tail. Returns the (damage, kb_base, kb_scale, kb_angle) for whichever is current. Caller has
    /// already checked the frame is inside the active span.
    pub fn hit_at(&self, frame: i64) -> (f32, f32, f32, f32) {
        if self.late.len > 0 && frame >= self.startup + self.active {
            (self.late.damage, self.late.kb_base, self.late.kb_scale, self.late.kb_angle)
        } else {
            (self.damage, self.kb_base, self.kb_scale, self.kb_angle)
        }
    }

    // baseline definitions; live copies live in Tune so the panel can edit them.
    pub(crate) const JAB: Self = Self {
        startup: 3,
        active: 3,
        recovery: 9,
        off: Vector2::new(44.0, -64.0),
        r: 32.0,
        damage: 3.0,
        kb_base: 320.0,
        kb_scale: 3.0,
        kb_angle: 35.0,
        late: HitLate::NONE,
    };
    pub(crate) const NAIR: Self = Self {
        startup: 5,
        active: 5, // strong early pop shortened to make room for the lingering tail
        recovery: 14,
        off: Vector2::new(26.0, -60.0), // centered on the taller body so a jump-in connects
        r: 52.0,
        damage: 8.0,
        kb_base: 520.0,
        kb_scale: 4.2,
        kb_angle: 45.0,
        // sex kick: hangs out for 7 more frames as a weak shallow poke (sets up combos, not kills)
        late: HitLate { len: 7, damage: 5.0, kb_base: 320.0, kb_scale: 2.6, kb_angle: 30.0 },
    };
    pub(crate) const DAIR: Self = Self {
        startup: 10,
        active: 6,
        recovery: 18,
        off: Vector2::new(10.0, 24.0), // hitbox below the feet (down + slightly forward)
        r: 40.0,
        damage: 11.0,
        kb_base: 460.0,
        kb_scale: 3.8,
        kb_angle: -72.0, // negative = downward launch: the spike
        late: HitLate::NONE,
    };
    pub(crate) const DASH_ATTACK: Self = Self {
        startup: 8,
        active: 6,    // lingering horizontal swipe
        recovery: 38, // heavy endlag — whiff it and you are wide open (the commitment)
        off: Vector2::new(100.0, -58.0), // reaches far out front
        r: 54.0,                          // big horizontal hitbox
        damage: 11.0,
        kb_base: 540.0,
        kb_scale: 3.9,
        kb_angle: 30.0, // low, horizontal launch — sends them flying sideways toward the blast zone
        late: HitLate::NONE,
    };
    // Game & Watch "Manhole" down-tilt: digs a pothole at the feet. Fast, low, and pops the opponent
    // nearly straight up out of the ground; a lingering second window (the cover hangs open) catches
    // late so it sets up juggles instead of killing.
    pub(crate) const DTILT: Self = Self {
        startup: 5,
        active: 3,
        recovery: 10,
        off: Vector2::new(40.0, 2.0), // low and just in front of the feet — a hole in the floor
        r: 30.0,
        damage: 6.0,
        kb_base: 280.0,
        kb_scale: 3.2,
        kb_angle: 80.0, // nearly vertical pop — the manhole flings them upward
        late: HitLate { len: 6, damage: 4.0, kb_base: 200.0, kb_scale: 2.2, kb_angle: 88.0 },
    };
}

pub fn attack_for(t: &Tune, st: CharState) -> Option<AttackData> {
    match st {
        CharState::Jab => Some(t.jab),
        CharState::Nair => Some(t.nair),
        CharState::Dair => Some(t.dair),
        CharState::Dtilt => Some(t.dtilt),
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

/// Active hitbox center in world space for an attacking state (None outside the active span).
/// The span covers BOTH windows (early `active` + the lingering `late.len`); the shape (off/r) is
/// shared, so the drawn/overlap circle is the same the whole time — only the payoff fades.
pub fn active_hitbox(f: &Fighter, t: &Tune) -> Option<(Vector2, f32)> {
    let atk = attack_for(t, f.state)?;
    if f.frame >= atk.startup && f.frame < atk.startup + atk.active_span() {
        let c = f.pos + Vector2::new(atk.off.x * f.facing, atk.off.y);
        Some((c, atk.r))
    } else {
        None
    }
}

/// A fighter's hurtbox: a circle whose center height + radius shift with the current state, so the
/// silhouette an attack lands on matches the pose. Base is mid-body (one ECB half-height above the
/// feet) at the full body radius; crouch/knockdown duck low and shrink (you can whiff a jab over a
/// crouch), aerials tuck the body a touch higher. Single circle keeps the overlap test cheap and the
/// debug draw one shape.
pub fn hurtbox(f: &Fighter) -> (Vector2, f32) {
    let base = -ECB_HALF_H;
    let (dy, r) = match f.state {
        // ducking: center drops toward the feet, body pulls in — the classic crouch-under.
        CharState::Crouch | CharState::Dtilt => (base * 0.55, DUMMY_R * 0.82),
        // floored: lying low and compact until getup.
        CharState::Knockdown => (base * 0.40, DUMMY_R * 0.88),
        // airborne poses tuck the legs up: center rides a little higher than standing.
        CharState::Nair | CharState::Dair | CharState::Air | CharState::Helpless => (base - 6.0, DUMMY_R),
        _ => (base, DUMMY_R),
    };
    (f.pos + Vector2::new(0.0, dy), r)
}
