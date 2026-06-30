//! Moves, grouped by kind. This module holds the shared normal-attack data (frame data + hitboxes +
//! knockback records) that grounded attacks, aerials, and tilts all draw from; `special` and `throw`
//! are submodules for those kinds. Moves are DATA indexed by `CharState` (see `attack_for`), so the
//! sim stays `Copy` + deterministic. Re-exported at the crate root.
//!
//! Hitbox model (Brawl-shaped): a move is N hitboxes, each owning its OWN frame window
//! (`start`/`len` relative to state start) on the shared `f.frame` clock. That is what makes
//! multi-hit + sequenced-timing moves (the 3-punch jab, the Knee Man stomp) authorable without a
//! per-move FSM state. Lowest live `id` wins per victim (sweetspot beats sourspot). Knockback is the
//! community / Project-M formula (see `knockback_units`), NOT the Melee decomp. See
//! `plans/hitbox-modeling.md`.

pub mod special;
pub mod throw;
pub use special::*;
pub use throw::*;

use crate::{CharState, Fighter, Tune, Vector2, DUMMY_R, ECB_HALF_H};
use self::special::special_slot;

/// Hitboxes per move (Brawl-ish cap). Fixed so `AttackData` stays `Copy` + snapshot-cheap.
pub const MAX_HB: usize = 4;

/// One hitbox: a circle offset from the fighter (x flipped by facing), live only on its own frame
/// window `[start, start + len)`. A move is up to `MAX_HB` of these on the shared state clock, so
/// sweetspot/sourspot, sex-kicks, and rapid multi-hit are all "more boxes", never new states.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct Hitbox {
    pub id: u8,             // priority; lowest LIVE overlapping id wins per victim (sweetspot first)
    pub start: i64,         // first active frame, relative to state start
    pub len: i64,           // active duration; live for [start, start + len). 0 = inert box
    pub off: Vector2,       // center offset from fighter pos (x is forward, flipped by facing)
    pub r: f32,             // radius
    pub damage: f32,        // % added on connect
    pub angle: f32,         // launch angle° (0 fwd, 90 up, negative = spike)
    pub bkb: f32,           // base knockback (community/PM BKB), in KB units
    pub kbg: f32,           // knockback growth (community/PM KBG; % scaling of the ramp)
    pub set_kb: f32,        // weight-independent fixed knockback (KB units); 0 = use the growth formula
    pub transcendent: bool, // skips the clank check (projectiles; aerials are de-facto transcendent)
    pub refresh: i64,       // frames a victim is immune to THIS box after a connect (multi-hit gap)
}

impl Hitbox {
    pub const NONE: Self = Self {
        id: 0, start: 0, len: 0, off: Vector2::ZERO, r: 0.0, damage: 0.0,
        angle: 0.0, bkb: 0.0, kbg: 0.0, set_kb: 0.0, transcendent: false, refresh: 0,
    };

    /// Is this box live at `frame` (relative to state start)? Inert boxes (`len == 0`) never are.
    #[inline]
    pub fn live_at(&self, frame: i64) -> bool {
        self.len > 0 && frame >= self.start && frame < self.start + self.len
    }
}

/// One attack's frame data: a lead-in, up to `MAX_HB` windowed hitboxes (id-ordered), and endlag.
/// The boxes drive both the hits and (via `f.frame`) the animation; `total()` sets the FSM state
/// length so the timer and the hitboxes stay in lockstep.
#[derive(Copy, Clone, PartialEq)]
pub struct AttackData {
    pub startup: i64,            // animation lead-in (no box before this); boxes key off the same f.frame
    pub recovery: i64,          // endlag after the last box closes
    pub boxes: [Hitbox; MAX_HB],// id-ordered, fixed cap
    pub nbox: u8,               // how many of `boxes` are real (the rest are Hitbox::NONE)
}

impl AttackData {
    /// Helper: build from a slice of boxes, padding to `MAX_HB` with inert boxes.
    pub const fn new(startup: i64, recovery: i64, boxes: [Hitbox; MAX_HB], nbox: u8) -> Self {
        Self { startup, recovery, boxes, nbox }
    }

    /// Single-window attack: one box opening at `startup`. The common shape (jab/dair/dash/specials).
    pub const fn one(startup: i64, active: i64, recovery: i64, hb: Hitbox) -> Self {
        let mut b = hb;
        b.id = 0;
        b.start = startup;
        b.len = active;
        Self { startup, recovery, boxes: [b, Hitbox::NONE, Hitbox::NONE, Hitbox::NONE], nbox: 1 }
    }

    /// The real boxes (drops the inert padding).
    pub fn live_boxes(&self) -> &[Hitbox] {
        &self.boxes[..self.nbox as usize]
    }

    /// Last frame any box is still live (= max start+len). Used both by `total()` and by moves that
    /// gate motion on "still swinging" (the dash-attack slide).
    pub fn active_end(&self) -> i64 {
        self.live_boxes().iter().map(|b| b.start + b.len).max().unwrap_or(self.startup)
    }

    /// FSM state length: last box close, then recovery.
    pub fn total(&self) -> i64 {
        self.active_end() + self.recovery
    }

    /// The lowest-id box live at `frame` whose window contains it. `None` outside every window.
    /// This is the per-victim "which window pays out" pick (id priority).
    pub fn box_at(&self, frame: i64) -> Option<&Hitbox> {
        self.live_boxes()
            .iter()
            .filter(|b| b.live_at(frame))
            .min_by_key(|b| b.id)
    }

    // baseline definitions; live copies live in Tune so the panel can edit them.
    // PM/community-flavored bkb/kbg/angle (NOT the Melee decomp). Knockback runs through
    // `knockback_units`; aerials author `transcendent: true` (aerials don't clank).
    pub(crate) const JAB: Self = Self::one(
        3, 3, 9,
        Hitbox { damage: 3.0, off: Vector2::new(44.0, -64.0), r: 32.0,
            angle: 35.0, bkb: 18.0, kbg: 30.0, ..Hitbox::NONE },
    );
    // neutral aerial — a sex-kick: a strong early pop, then a weak lingering tail at the same limb.
    // Two boxes, same off/r, different windows + payoff. Aerial => transcendent.
    pub(crate) const NAIR: Self = Self {
        startup: 5,
        recovery: 14,
        boxes: [
            Hitbox { id: 0, start: 5, len: 5, off: Vector2::new(26.0, -60.0), r: 52.0,
                damage: 8.0, angle: 45.0, bkb: 22.0, kbg: 42.0, set_kb: 0.0,
                transcendent: true, refresh: 0 },
            Hitbox { id: 1, start: 10, len: 7, off: Vector2::new(26.0, -60.0), r: 52.0,
                damage: 5.0, angle: 30.0, bkb: 14.0, kbg: 26.0, set_kb: 0.0,
                transcendent: true, refresh: 0 },
            Hitbox::NONE, Hitbox::NONE,
        ],
        nbox: 2,
    };
    pub(crate) const DAIR: Self = Self::one(
        10, 6, 18,
        Hitbox { damage: 11.0, off: Vector2::new(10.0, 24.0), r: 40.0,
            angle: -72.0, bkb: 20.0, kbg: 38.0, transcendent: true, ..Hitbox::NONE },
    );
    pub(crate) const DASH_ATTACK: Self = Self::one(
        8, 38,
        6,
        Hitbox { damage: 11.0, off: Vector2::new(100.0, -58.0), r: 54.0,
            angle: 30.0, bkb: 24.0, kbg: 39.0, ..Hitbox::NONE },
    );
    // Game & Watch "Manhole" down-tilt: a fast low pop with a lingering second window (juggle setup).
    pub(crate) const DTILT: Self = Self {
        startup: 5,
        recovery: 10,
        boxes: [
            Hitbox { id: 0, start: 5, len: 3, off: Vector2::new(40.0, 2.0), r: 30.0,
                damage: 6.0, angle: 80.0, bkb: 14.0, kbg: 32.0, set_kb: 0.0,
                transcendent: false, refresh: 0 },
            Hitbox { id: 1, start: 8, len: 6, off: Vector2::new(40.0, 2.0), r: 30.0,
                damage: 4.0, angle: 88.0, bkb: 10.0, kbg: 22.0, set_kb: 0.0,
                transcendent: false, refresh: 0 },
            Hitbox::NONE, Hitbox::NONE,
        ],
        nbox: 2,
    };
}

/// Community / Project-M knockback in KB units (NOT the Melee decomp). `p` = victim % AFTER the hit's
/// damage is added, `d` = hit damage, `w` = victim weight. `bkb`/`kbg`/`set_kb` come off the box.
/// The caller turns units into px/s (`* Tune.kb_speed`) and hitstun (`floor(units * kb_hitstun)`),
/// so "hitstun = floor(0.4 * KB)" stays literal.
pub fn knockback_units(p: f32, d: f32, w: f32, hb: &Hitbox) -> f32 {
    if hb.set_kb > 0.0 {
        // weight-independent fixed knockback: jab-lock / multi-hit links stay reliable.
        return hb.set_kb;
    }
    (((p / 10.0 + p * d / 20.0) * (200.0 / (w + 100.0)) * 1.4 + 18.0) * (hb.kbg / 100.0)) + hb.bkb
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

/// World-space center + radius of a hitbox for the attacker's current facing.
#[inline]
pub fn hitbox_center(f: &Fighter, hb: &Hitbox) -> (Vector2, f32) {
    (f.pos + Vector2::new(hb.off.x * f.facing, hb.off.y), hb.r)
}

/// Every hitbox live THIS frame for an attacking state, in world space (id-ordered). Slots past
/// `nbox`/inactive windows are `None`. The shell/debug draw iterates this; `resolve_combat` uses
/// `box_at` for the id-priority pick.
pub fn live_hitboxes(f: &Fighter, t: &Tune) -> [Option<(Vector2, f32)>; MAX_HB] {
    let mut out = [None; MAX_HB];
    if let Some(atk) = attack_for(t, f.state) {
        for (i, b) in atk.live_boxes().iter().enumerate() {
            if b.live_at(f.frame) {
                out[i] = Some(hitbox_center(f, b));
            }
        }
    }
    out
}

/// The lowest-id hitbox live this frame, in world space (None if the move has no live box now).
/// Kept for the single-shape debug draw (shell + web); combat uses `box_at` directly.
pub fn active_hitbox(f: &Fighter, t: &Tune) -> Option<(Vector2, f32)> {
    let atk = attack_for(t, f.state)?;
    atk.box_at(f.frame).map(|b| hitbox_center(f, b))
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
