//! Pure-sim tests + a deterministic replay harness. `step` is state×inputs→state with no I/O, so
//! the whole sim is a function we can drive frame-by-frame and assert against. The replay harness
//! doubles as a determinism oracle independent of ggrs: drive a scripted input log twice and the
//! per-frame trace must be bit-identical. Capturing a real session's input log (NetInput stream,
//! which already round-trips) and replaying it here is the regression/replay-validation path.

use smash_core::*;

// --- input builders -----------------------------------------------------------------------------

/// Neutral frame (no buttons, centered stick).
fn idle() -> InputFrame {
    InputFrame::default()
}

/// Build a frame by mutating the neutral default — `press(|i| i.attack = true)`.
fn press(f: impl FnOnce(&mut InputFrame)) -> InputFrame {
    let mut i = InputFrame::default();
    f(&mut i);
    i
}

/// P1 frame + neutral P2.
fn solo(i: InputFrame) -> [InputFrame; 2] {
    [i, InputFrame::default()]
}

// --- replay harness -----------------------------------------------------------------------------

/// One frame's observable scalars for both fighters. Enough to catch any divergence without
/// requiring `PartialEq` on the whole `SimState` (whose f32s would make NaN-equality brittle; normal
/// play produces none). `state as u8` is valid: `CharState` is a fieldless enum.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Snap {
    tick: u64,
    state: [u8; 2],
    px: [f32; 2],
    py: [f32; 2],
    dmg: [f32; 2],
    holding: [i8; 2],
}

fn snap(s: &SimState) -> Snap {
    let f = &s.fighters;
    Snap {
        tick: s.tick,
        state: [f[0].state as u8, f[1].state as u8],
        px: [f[0].pos.x, f[1].pos.x],
        py: [f[0].pos.y, f[1].pos.y],
        dmg: [f[0].damage, f[1].damage],
        holding: [f[0].holding, f[1].holding],
    }
}

/// Run a scripted input log from a fresh spawn under default tuning; return the per-frame trace.
fn drive(script: &[[InputFrame; 2]]) -> Vec<Snap> {
    let t = Tune::default();
    let mut s = SimState::spawn();
    let mut trace = Vec::with_capacity(script.len());
    for inputs in script {
        s = step(&s, &[&inputs[0], &inputs[1]], &t);
        trace.push(snap(&s));
    }
    trace
}

/// Fighters spawn airborne (drop-in). Run neutral input until they land and settle, so behavior
/// tests start from a known grounded `Stand`.
fn settled() -> (SimState, Tune) {
    let t = Tune::default();
    let mut s = SimState::spawn();
    for _ in 0..120 {
        s = step(&s, &[&idle(), &idle()], &t);
    }
    assert_eq!(s.fighters[0].state, CharState::Stand, "fighter should settle to Stand");
    (s, t)
}

/// A varied script that exercises movement, jumping, and an attack — the determinism fixture.
fn mixed_script() -> Vec<[InputFrame; 2]> {
    let mut v = Vec::new();
    for _ in 0..20 {
        v.push(solo(press(|i| i.dir = 1.0))); // walk right
    }
    for _ in 0..30 {
        v.push(solo(press(|i| {
            i.jump = true;
            i.jump_held = true;
            i.dir = -1.0;
        }))); // jump + drift left
    }
    v.push(solo(press(|i| i.attack = true))); // swing
    for _ in 0..20 {
        v.push(solo(idle())); // settle
    }
    v
}

// --- determinism oracle -------------------------------------------------------------------------

#[test]
fn replay_is_deterministic() {
    let script = mixed_script();
    let a = drive(&script);
    let b = drive(&script);
    assert_eq!(a, b, "same input log must produce an identical trace");
    assert!(
        a.iter().all(|s| s.px.iter().chain(&s.py).all(|v| v.is_finite())),
        "sim produced a non-finite position"
    );
}

#[test]
fn neutral_input_settles_on_the_floor() {
    let trace = drive(&vec![solo(idle()); 150]);
    let a = trace[trace.len() - 2];
    let b = trace[trace.len() - 1];
    // ignore `tick` (a free-running counter); everything physical should be at a fixed point.
    assert_eq!((a.state, a.px, a.py, a.dmg, a.holding), (b.state, b.px, b.py, b.dmg, b.holding),
        "with neutral input the sim should reach a fixed point");
    assert_eq!(b.state[0], CharState::Stand as u8, "settles into Stand");
}

// --- behavior units -----------------------------------------------------------------------------

#[test]
fn jump_leaves_the_ground() {
    let (mut s, t) = settled();
    let ground = s.fighters[0].pos.y;
    // full hop: press + hold for the jumpsquat, then keep holding through takeoff.
    s = step(&s, &[&press(|i| { i.jump = true; i.jump_held = true; }), &idle()], &t);
    let mut lowest = s.fighters[0].pos.y;
    for _ in 0..40 {
        s = step(&s, &[&press(|i| i.jump_held = true), &idle()], &t);
        lowest = lowest.min(s.fighters[0].pos.y);
    }
    assert!(lowest < ground - 50.0, "fighter should rise well above the floor (got {lowest} vs {ground})");
}

// These three lock the buffer feel the unit tests above don't reach: the auto-short-hop aerial
// (jump+attack held), the air jump, and the wavedash. They are the golden coverage for the
// Action-model refactor — behavior must be identical before and after.

#[test]
fn jump_plus_attack_autohops_into_an_aerial() {
    let (mut s, t) = settled();
    // same-frame jump + attack with empty hands = auto short-hop aerial
    s = step(&s, &[&press(|i| { i.jump = true; i.jump_held = true; i.attack = true; }), &idle()], &t);
    let mut saw_aerial = false;
    for _ in 0..14 {
        s = step(&s, &[&press(|i| i.jump_held = true), &idle()], &t);
        if matches!(s.fighters[0].state, CharState::Nair | CharState::Dair) {
            saw_aerial = true;
            break;
        }
    }
    assert!(saw_aerial, "jump+attack should auto-hop into an aerial");
    assert!(s.fighters[0].autohop_aerial, "the auto-hop aerial should be tagged for reduced damage");
}

#[test]
fn second_jump_in_air_is_an_air_jump() {
    let (mut s, t) = settled();
    s = step(&s, &[&press(|i| { i.jump = true; i.jump_held = true; }), &idle()], &t);
    for _ in 0..8 {
        s = step(&s, &[&press(|i| i.jump_held = true), &idle()], &t);
    }
    assert_eq!(s.fighters[0].state, CharState::Air, "should be airborne after the hop");
    let before = s.fighters[0].air_jumps;
    s = step(&s, &[&idle(), &idle()], &t); // release
    s = step(&s, &[&press(|i| { i.jump = true; i.jump_held = true; }), &idle()], &t);
    assert_eq!(s.fighters[0].air_jumps, before - 1, "the second jump should spend an air jump");
    assert!(s.fighters[0].vel.y < 0.0, "the air jump should drive the fighter upward");
}

#[test]
fn airdodge_into_the_ground_wavedashes() {
    let (mut s, t) = settled();
    // jump, then airdodge down-toward during the jumpsquat = wavedash out of the squat
    s = step(&s, &[&press(|i| { i.jump = true; i.jump_held = true; }), &idle()], &t);
    s = step(&s, &[&press(|i| {
        i.shield_pressed = true;
        i.dir = 1.0;
        i.aim_y = 1.0; // down-forward (screen y is positive downward)
    }), &idle()], &t);
    let mut grounded_with_slide = false;
    for _ in 0..20 {
        s = step(&s, &[&idle(), &idle()], &t);
        let f = &s.fighters[0];
        if !matches!(f.state, CharState::Air | CharState::AirDodge | CharState::JumpSquat)
            && f.vel.x.abs() > 1.0
        {
            grounded_with_slide = true;
            break;
        }
    }
    assert!(grounded_with_slide, "an airdodge into the floor should slide along the ground");
}

#[test]
fn grounded_attack_enters_jab() {
    let (s, t) = settled();
    let after = step(&s, &[&press(|i| i.attack = true), &idle()], &t);
    assert_eq!(after.fighters[0].state, CharState::Jab, "grounded attack should start a jab");
}

#[test]
fn down_plus_attack_enters_dtilt() {
    let (s, t) = settled();
    let after = step(&s, &[&press(|i| { i.attack = true; i.down = true; }), &idle()], &t);
    assert_eq!(
        after.fighters[0].state,
        CharState::Dtilt,
        "down + attack on the ground should start the dtilt pothole"
    );
}

/// Land P1 straight down onto the left SOFT platform (index 1) and settle to Stand there, so the
/// soft-platform drop-buffer tests start from a known "crouch-able on a soft platform" pose.
fn on_soft_platform() -> (SimState, Tune) {
    let t = Tune::default();
    let mut s = SimState::spawn();
    s.fighters[0].pos.x = 410.0; // center of PLATFORMS[1] (left soft, x 280..540, y 575)
    s.fighters[0].pos.y = 480.0; // above the platform top, below the top-center platform's x-range
    s.fighters[0].vel.x = 0.0;
    s.fighters[0].vel.y = 0.0;
    s.fighters[0].state = CharState::Air;
    for _ in 0..60 {
        s = step(&s, &[&idle(), &idle()], &t);
    }
    assert_eq!(s.fighters[0].state, CharState::Stand, "should land + settle on the soft platform");
    assert_eq!(s.fighters[0].ground_plat, 1, "should be standing on the left soft platform");
    (s, t)
}

#[test]
fn down_tap_drops_through_a_soft_platform() {
    let (mut s, t) = on_soft_platform();
    // A single Down TAP (down_pressed on the first frame only) crouches + arms the drop buffer,
    // then drops through within plat_drop_window frames — no re-tap required (Melee/PM feel).
    let mut dropped = false;
    for f in 0..(t.plat_drop_window as usize + 4) {
        let tap = f == 0; // rising edge only on the first frame
        s = step(&s, &[&press(|i| { i.down = true; i.down_pressed = tap; }), &idle()], &t);
        if s.fighters[0].state == CharState::Air && s.fighters[0].ground_plat < 0 {
            dropped = true;
            break;
        }
    }
    assert!(dropped, "a Down tap on a soft platform should drop through within the tilt-window");
}

#[test]
fn holding_down_drops_even_without_a_press_edge() {
    // Regression: the shell only fires down_pressed for the digital ui_down action, so a controller
    // or touch stick sets down (held) but never the edge. The drop must still fire off the held bit.
    let (mut s, t) = on_soft_platform();
    let mut dropped = false;
    for _ in 0..(t.plat_drop_window as usize + 6) {
        s = step(&s, &[&press(|i| i.down = true), &idle()], &t); // down HELD, down_pressed never set
        if s.fighters[0].state == CharState::Air && s.fighters[0].ground_plat < 0 {
            dropped = true;
            break;
        }
    }
    assert!(dropped, "holding Down on a soft platform must drop even if down_pressed never fired");
}

#[test]
fn down_attack_in_the_window_dtilts_instead_of_dropping() {
    let (mut s, t) = on_soft_platform();
    // Frame 0: the Down tap crouches and arms the buffer — it must NOT drop yet.
    s = step(&s, &[&press(|i| { i.down = true; i.down_pressed = true; }), &idle()], &t);
    assert_eq!(s.fighters[0].state, CharState::Crouch, "the Down tap crouches first, no instant drop");
    assert_eq!(s.fighters[0].ground_plat, 1, "still on the platform after the entry tap");
    // Frame 1 (inside plat_drop_window): Down + Attack converts to a Dtilt and cancels the drop.
    s = step(&s, &[&press(|i| { i.down = true; i.attack = true; }), &idle()], &t);
    assert_eq!(s.fighters[0].state, CharState::Dtilt, "Down+Attack in the window should dtilt");
    assert_eq!(s.fighters[0].ground_plat, 1, "the dtilt must NOT drop through the platform");
}

#[test]
fn classify_caches_floor_wall_and_grabbable_lip() {
    // a flat shelf (0,0)->(100,0) then a sharp drop (100,0)->(120,200).
    let mut p = InkPath::EMPTY;
    p.props = StrokeProps::PEN;
    p.pts[0] = Vector2::new(0.0, 0.0);
    p.pts[1] = Vector2::new(100.0, 0.0);
    p.pts[2] = Vector2::new(120.0, 200.0);
    p.len = 3;
    classify(&mut p);
    assert_eq!(p.class[0], SegClass::Ledge, "the flat open-end shelf is a grabbable lip");
    assert_eq!(p.class[1], SegClass::Wall, "the steep drop classifies as a wall");
}

#[test]
fn fighter_lands_and_stands_on_a_drawn_ink_floor() {
    let t = Tune::default();
    let mut s = SimState::spawn();
    // drop fighter 0 over open air (no soft platform spans x=180; main floor is far below at 760).
    s.fighters[0].pos = Vector2::new(180.0, 250.0);
    // a flat finalized ink shelf directly under the drop, well above the main floor.
    let mut shelf = InkPath::EMPTY;
    shelf.props = StrokeProps::PEN;
    shelf.owner = 0;
    shelf.pts[0] = Vector2::new(100.0, 400.0);
    shelf.pts[1] = Vector2::new(260.0, 400.0);
    shelf.len = 2;
    classify(&mut shelf); // caches Floor/Ledge so collision can read it
    assert!(matches!(shelf.class[0], SegClass::Floor | SegClass::Ledge), "flat shelf is walkable");
    s.paths[0] = shelf;
    // fighter 0 now drops straight down onto the shelf and settles.
    for _ in 0..120 {
        s = step(&s, &[&idle(), &idle()], &t);
    }
    let f = &s.fighters[0];
    assert_eq!(f.ground_ink, 0, "should be standing on the ink path, not fallen through");
    assert_eq!(f.state, CharState::Stand, "settles into Stand on the ink");
    assert!((f.pos.y - 400.0).abs() < 2.0, "feet pinned to the ink surface (400), got {}", f.pos.y);
}

#[test]
fn a_drawn_ink_wall_blocks_horizontal_movement() {
    let (mut s, t) = settled();
    let start = s.fighters[0].pos;
    let wall_x = start.x + 60.0; // just to the right, within a few walk-frames
    // a near-vertical finalized ink stroke spanning the fighter's torso (feet at start.y, ECB ~140 tall).
    let mut wall = InkPath::EMPTY;
    wall.props = StrokeProps::PEN;
    wall.owner = 0;
    wall.pts[0] = Vector2::new(wall_x, start.y - 200.0);
    wall.pts[1] = Vector2::new(wall_x, start.y + 40.0);
    wall.len = 2;
    classify(&mut wall);
    assert_eq!(wall.class[0], SegClass::Wall, "vertical stroke classifies as a wall");
    s.paths[0] = wall;
    // walk right into the wall for a while; the ECB right vert (38px) must stop at wall_x.
    for _ in 0..40 {
        s = step(&s, &[&press(|i| i.dir = 1.0), &idle()], &t);
    }
    let f = &s.fighters[0];
    assert!(
        f.pos.x <= wall_x - 38.0 + 1.0,
        "fighter should be pinned left of the ink wall (wall {wall_x}, got {})",
        f.pos.x
    );
    assert!(f.pos.x > start.x, "but should have walked right up to the wall, not stayed put");
}

#[test]
fn stroke_registry_resolves_by_id_and_falls_back_to_default() {
    let mut reg = StrokeRegistry::DEFAULT;
    reg.presets[1] = StrokeProps { solid: true, ..StrokeProps::PEN };
    assert!(reg.get(1).solid, "id 1 resolves to its own preset");
    assert!(!reg.get(0).solid, "row 0 is the untouched default");
    assert!(!reg.get(99).solid, "an out-of-range id falls back to the default row");
}

#[test]
fn a_pen_stamps_its_registry_preset_onto_the_path() {
    let (mut s, mut t) = settled();
    // preset row 2 differs from the default (solid); the pen selects it via StrokeId.
    t.strokes.presets[2] = StrokeProps { solid: true, ..StrokeProps::PEN };
    s.fighters[0].holding = 0;
    s.items[0] = Item {
        kind: ItemKind::Pen,
        owner: 0,
        gas: t.ink_budget,
        gas_max: t.ink_budget,
        stroke: 2,
        ..Item::EMPTY
    };
    for _ in 0..20 {
        s = step(&s, &[&press(|i| { i.attack_held = true; i.dir = 1.0; }), &idle()], &t);
    }
    let path = s.paths.iter().find(|p| p.active() && p.owner == 0).expect("pen laid a path");
    assert!(path.props.solid, "the path should inherit preset 2's material (solid), not the default");
}

#[test]
fn holding_a_pen_and_attacking_lays_an_ink_path() {
    let (mut s, t) = settled();
    s.fighters[0].holding = 0;
    s.items[0] = Item {
        kind: ItemKind::Pen,
        owner: 0,
        gas: t.ink_budget,
        gas_max: t.ink_budget,
        ..Item::EMPTY
    };
    // hold attack + walk right; the trail pen lays nodes along the movement.
    for _ in 0..40 {
        let p0 = press(|i| {
            i.attack_held = true;
            i.dir = 1.0;
        });
        s = step(&s, &[&p0, &idle()], &t);
    }
    let path = s.paths.iter().find(|p| p.active() && p.owner == 0);
    assert!(path.is_some(), "holding a pen and attacking should lay an ink path");
    assert!(path.unwrap().len >= 2, "a moving trail pen should plant multiple nodes");
}

#[test]
fn attack_over_gun_picks_it_up() {
    let (mut s, t) = settled();
    s.items[0] = Item {
        kind: ItemKind::LaserGun,
        pos: s.fighters[0].pos, // overlap the body
        vel: Vector2::ZERO,
        owner: -1,
        gas: 16.0,
        gas_max: 16.0,
        timer: 0,
        facing: 1.0,
        tool: ToolKind::TrailPen,
        stroke: 0,
    };
    let after = step(&s, &[&press(|i| i.attack = true), &idle()], &t);
    assert_eq!(after.fighters[0].holding, 0, "attack over an unowned gun should pick it up");
    assert_ne!(after.fighters[0].state, CharState::Jab, "pickup should not also jab");
}

#[test]
fn grab_over_an_item_picks_it_up() {
    let (mut s, t) = settled();
    s.items[0] = Item {
        kind: ItemKind::LaserGun,
        pos: s.fighters[0].pos, // standing over it
        vel: Vector2::ZERO,
        owner: -1,
        gas: 16.0,
        gas_max: 16.0,
        timer: 0,
        facing: 1.0,
        tool: ToolKind::TrailPen,
        stroke: 0,
    };
    let after = step(&s, &[&press(|i| i.grab = true), &idle()], &t);
    assert_eq!(after.fighters[0].holding, 0, "grab over an unowned item should pick it up");
    assert_ne!(after.fighters[0].state, CharState::Grab, "item grab should not start a fighter-grab");
}

#[test]
fn firing_a_held_gun_spawns_a_bolt_and_spends_ammo() {
    let t = Tune::default();
    let mut s = SimState::spawn();
    s.fighters[0].holding = 0;
    s.items[0] = Item {
        kind: ItemKind::LaserGun,
        pos: s.fighters[0].pos,
        vel: Vector2::ZERO,
        owner: 0,
        gas: 16.0,
        gas_max: 16.0,
        timer: 0,
        facing: 1.0,
        tool: ToolKind::TrailPen,
        stroke: 0,
    };
    let after = step(&s, &[&press(|i| i.attack = true), &idle()], &t);
    let bolts = after.items.iter().filter(|x| x.kind == ItemKind::LaserBolt).count();
    assert_eq!(bolts, 1, "one bolt should spawn");
    assert_eq!(after.items[0].gas, 15.0, "gas should decrement by one shot");
}

#[test]
fn grab_drops_a_held_gun() {
    let t = Tune::default();
    let mut s = SimState::spawn();
    s.fighters[0].holding = 0;
    s.items[0] = Item {
        kind: ItemKind::LaserGun,
        pos: s.fighters[0].pos,
        vel: Vector2::ZERO,
        owner: 0,
        gas: 16.0,
        gas_max: 16.0,
        timer: 0,
        facing: 1.0,
        tool: ToolKind::TrailPen,
        stroke: 0,
    };
    let after = step(&s, &[&press(|i| i.grab = true), &idle()], &t);
    assert_eq!(after.fighters[0].holding, -1, "grab should drop the held item");
    assert!(after.items[0].owner < 0, "dropped gun becomes unowned");
}

#[test]
fn falling_past_the_blast_zone_respawns() {
    let t = Tune::default();
    let mut s = SimState::spawn();
    let spawn_y = s.fighters[0].pos.y;
    s.fighters[0].pos.y = 5000.0; // way past BLAST_Y
    s.fighters[0].damage = 88.0;
    let after = step(&s, &[&idle(), &idle()], &t);
    assert!(after.fighters[0].pos.y <= spawn_y + 1.0, "should respawn back at the top");
    assert_eq!(after.fighters[0].damage, 0.0, "respawn resets damage");
}

#[test]
fn an_unowned_gun_off_stage_keeps_falling_and_despawns_at_the_blast_zone() {
    // FLOOR_RIGHT = 1050, GROUND_Y = 760, BLAST_Y = 1600 (all pixel space). Place a gun well past
    // the right ledge, above the pit, at rest.
    let (mut s, t) = settled();
    s.items[0] = Item {
        kind: ItemKind::LaserGun,
        pos: Vector2::new(1400.0, 700.0), // off the span (x > FLOOR_RIGHT), above the floor y
        gas: 16.0,
        gas_max: 16.0,
        ..Item::EMPTY
    };
    // one step must NOT snap it to the invisible floor at GROUND_Y (760) — off the span it accrues
    // downward velocity instead of dead-stopping.
    s = step(&s, &[&idle(), &idle()], &t);
    assert!(s.items[0].active(), "still in flight after one step");
    assert!(s.items[0].vel.y > 0.0, "gravity pulls it down instead of resting on an invisible floor");
    let mut fell_past_floor = false;
    let mut gone = false;
    for _ in 0..600 {
        s = step(&s, &[&idle(), &idle()], &t);
        if s.items[0].active() && s.items[0].pos.y > 760.0 + 1.0 {
            fell_past_floor = true; // dropped below the old invisible floor rather than settling on it
        }
        if !s.items[0].active() {
            gone = true;
            break;
        }
    }
    assert!(fell_past_floor, "the gun should fall past GROUND_Y (no invisible floor off the span)");
    assert!(gone, "the gun should despawn after crossing the blast zone");
    assert!(!s.items[0].active(), "despawn = empty slot, quietly");
}

#[test]
fn a_bomb_off_stage_despawns_quietly_without_exploding() {
    // A live bomb lobbed over the pit (x well past FLOOR_RIGHT = 1050) must fall past GROUND_Y and
    // cross BLAST_Y (1600) BEFORE its fuse (t.bomb.range) ends, then vanish with NO explosion.
    let (mut s, t) = settled();
    s.items[0] = Item {
        kind: ItemKind::Bomb,
        pos: Vector2::new(1400.0, 700.0),
        vel: Vector2::ZERO,
        owner: -1,
        timer: t.bomb.range, // full fuse; it should die by falling, not by fusing out
        gas: 0.0,
        gas_max: 1.0,
        ..Item::EMPTY
    };
    let dmg0 = [s.fighters[0].damage, s.fighters[1].damage];
    // first step: it arcs down, it does not detonate on the invisible floor off the span.
    s = step(&s, &[&idle(), &idle()], &t);
    assert!(s.items[0].active(), "bomb still falling after one step (no invisible-floor detonation)");
    assert!(s.items[0].pos.y > 700.0, "bomb falls off the span instead of resting/detonating");
    let mut gone = false;
    for _ in 0..600 {
        s = step(&s, &[&idle(), &idle()], &t);
        if !s.items[0].active() {
            gone = true;
            break;
        }
    }
    assert!(gone, "the bomb should despawn after crossing the blast zone");
    assert!(!s.items[0].active(), "quiet despawn = empty slot");
    assert_eq!(
        [s.fighters[0].damage, s.fighters[1].damage], dmg0,
        "a bomb that fell out of bounds must not explode or damage anyone"
    );
}
