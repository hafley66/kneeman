//! Controls: the ONLY place in the gameplay path that touches a raw device. Physical keys/buttons
//! bind to named actions in `project.godot [input]`; the analog stick + dpad are read here (Godot's
//! default `ui_*` actions don't carry the pad on web). Everything downstream — the whole sim — sees
//! only the semantic [`InputFrame`]. Nothing else may name `Input`, `JoyButton`, `JoyAxis`, or an
//! action string; the action universe is [`GameAction`].
//!
//! Lockdown invariant: `Input::singleton` / `JoyButton` / `JoyAxis` / `is_action_*` appear in this
//! file (and `ui/debug.rs`'s pad *readout*) only. Keep it that way.
//!
//! Couch co-op: `poll` is player one (keyboard `ui_*`/named actions + pad[0] + touch); `poll_p2` is
//! player two, read straight off the SECOND gamepad and a left-hand keyboard cluster. Godot's named
//! actions are global and can't tell two keyboards apart, so P2 bypasses `GameAction::names` and reads
//! raw keys/buttons here (still inside this one boundary). Netplay still supplies its P2 over the wire;
//! `poll_p2` is for the local/offline path only.

use godot::classes::Input;
use godot::global::{JoyAxis, JoyButton, Key};
use godot::prelude::*;

use crate::sim::InputFrame;

const STICK_DEADZONE: f32 = 0.22; // pad stick magnitude below this reads as neutral

thread_local! {
    // last frame's aim_y, for edge-detecting the stick-flick "tap jump".
    static PREV_AIM_Y: std::cell::Cell<f32> = const { std::cell::Cell::new(0.0) };
    // player two's edge trackers (held-button bitmask + aim_y), kept apart from P1's so neither
    // player's edges leak into the other.
    static P2_PREV_MASK: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
    static P2_PREV_AIM_Y: std::cell::Cell<f32> = const { std::cell::Cell::new(0.0) };
}

/// The action universe. Every game-meaningful input is one of these; nothing downstream names a
/// physical key or pad button. `names` lists the `project.godot` action(s) each maps to (multiple =
/// aliases OR'd together). This is the single source for action strings, incl. the touch buttons.
#[derive(Clone, Copy)]
pub enum GameAction {
    Jump,
    ShortHop,
    Attack,
    Shield,
    Grab,
    Special,
    Down,
}

impl GameAction {
    pub fn names(self) -> &'static [&'static str] {
        match self {
            GameAction::Jump => &["jump", "ui_accept", "ui_up"],
            GameAction::ShortHop => &["shorthop"],
            GameAction::Attack => &["attack"],
            GameAction::Shield => &["shield"],
            GameAction::Grab => &["grab"],
            GameAction::Special => &["special"],
            GameAction::Down => &["ui_down"],
        }
    }
}

/// True if any alias of `a` is currently held.
fn held(input: &mut Input, a: GameAction) -> bool {
    a.names().iter().any(|n| input.is_action_pressed(*n))
}

/// True if any alias of `a` had its rising edge this frame.
fn pressed(input: &mut Input, a: GameAction) -> bool {
    a.names().iter().any(|n| input.is_action_just_pressed(*n))
}

/// Read every game action for the local player into the sim's semantic [`InputFrame`]. `touch_stick`
/// is the on-screen stick's (x, y) in [-1, 1] (the mobile UI owns that widget; we just merge it in).
pub fn poll(touch_stick: (f32, f32)) -> InputFrame {
    let mut input = Input::singleton();
    // Keyboard movement (the default ui_* actions carry the arrow keys).
    let mut dir = input.get_axis("ui_left", "ui_right");
    let mut aim_y = input.get_axis("ui_up", "ui_down"); // -1 up .. +1 down
    let mut pad_down = false;
    // Web: the default ui_* movement actions don't carry the pad's stick/dpad, so read the first
    // connected joypad directly and merge it in (keyboard still works; pad wins when held).
    if let Some(dev) = input.get_connected_joypads().get(0) {
        let dev = dev as i32;
        let dz = 0.2;
        let sx = input.get_joy_axis(dev, JoyAxis::LEFT_X);
        let sy = input.get_joy_axis(dev, JoyAxis::LEFT_Y);
        let dpx = input.is_joy_button_pressed(dev, JoyButton::DPAD_RIGHT) as i32 as f32
            - input.is_joy_button_pressed(dev, JoyButton::DPAD_LEFT) as i32 as f32;
        let dpy = input.is_joy_button_pressed(dev, JoyButton::DPAD_DOWN) as i32 as f32
            - input.is_joy_button_pressed(dev, JoyButton::DPAD_UP) as i32 as f32;
        let px = if dpx != 0.0 {
            dpx
        } else if sx.abs() > dz {
            sx
        } else {
            0.0
        };
        let py = if dpy != 0.0 {
            dpy
        } else if sy.abs() > dz {
            sy
        } else {
            0.0
        };
        if dir == 0.0 {
            dir = px;
        }
        if aim_y == 0.0 {
            aim_y = py;
        }
        pad_down = py > 0.4;
    }
    // On-screen touch stick (mobile). Lowest priority: only fills axes the keyboard/pad left at 0.
    let (tsx, tsy) = touch_stick;
    if dir == 0.0 && tsx.abs() > STICK_DEADZONE {
        dir = tsx;
    }
    if aim_y == 0.0 && tsy.abs() > STICK_DEADZONE {
        aim_y = tsy;
    }
    if tsy > 0.4 {
        pad_down = true;
    }
    // Tap-jump (for the pleebs): flick the stick up and you jump, no button needed. Edge-detected
    // off the merged aim_y so it works for touch + pad + keyboard up.
    let prev_sy = PREV_AIM_Y.get();
    PREV_AIM_Y.set(aim_y);
    let tap_jump = prev_sy > -0.5 && aim_y <= -0.7;
    InputFrame {
        dir,
        aim_y,
        jump: tap_jump || pressed(&mut input, GameAction::Jump),
        jump_held: held(&mut input, GameAction::Jump),
        shorthop: pressed(&mut input, GameAction::ShortHop),
        shield_held: held(&mut input, GameAction::Shield),
        shield_pressed: pressed(&mut input, GameAction::Shield),
        down: held(&mut input, GameAction::Down) || pad_down,
        down_pressed: pressed(&mut input, GameAction::Down),
        attack: pressed(&mut input, GameAction::Attack),
        attack_held: held(&mut input, GameAction::Attack),
        grab: pressed(&mut input, GameAction::Grab),
        special: pressed(&mut input, GameAction::Special),
    }
}

// --- player two (couch co-op) ------------------------------------------------------------------
// P2 = the SECOND connected gamepad and/or a left-hand keyboard set (WASD move, G/H/J/K/L/Y buttons),
// chosen to dodge P1's keyboard keys. Read raw because named actions can't separate the two players.
// Bits in the held mask, for edge detection across frames:
const B_JUMP: u8 = 1 << 0;
const B_SHORTHOP: u8 = 1 << 1;
const B_ATTACK: u8 = 1 << 2;
const B_SHIELD: u8 = 1 << 3;
const B_GRAB: u8 = 1 << 4;
const B_SPECIAL: u8 = 1 << 5;
const B_DOWN: u8 = 1 << 6;

/// P2 gamepad button per action (same physical layout as the project.godot P1 bindings).
fn p2_pad_button(a: GameAction) -> JoyButton {
    match a {
        GameAction::Jump => JoyButton::A,
        GameAction::ShortHop => JoyButton::RIGHT_SHOULDER,
        GameAction::Attack => JoyButton::X,
        GameAction::Shield => JoyButton::LEFT_SHOULDER,
        GameAction::Grab => JoyButton::BACK,
        GameAction::Special => JoyButton::B,
        GameAction::Down => JoyButton::DPAD_DOWN,
    }
}

/// P2 keyboard key per action. Movement is WASD; the action cluster sits clear of P1's C/X/Z/V/B.
fn p2_key(a: GameAction) -> Key {
    match a {
        GameAction::Jump => Key::G,
        GameAction::ShortHop => Key::Y,
        GameAction::Attack => Key::H,
        GameAction::Shield => Key::K,
        GameAction::Grab => Key::L,
        GameAction::Special => Key::J,
        GameAction::Down => Key::S,
    }
}

/// Player two's frame for local two-player. All-neutral when no second device is touched, so the
/// caller feeds it every frame for free; couch co-op "turns on" the moment someone grabs the second
/// gamepad or the WASD/GHJKL keys. Netplay does NOT use this (its P2 arrives over the wire).
pub fn poll_p2() -> InputFrame {
    let mut input = Input::singleton();
    let pad2 = input.get_connected_joypads().get(1).map(|d| d as i32);

    // movement: WASD keyboard first, then pad[1] stick/dpad fills any axis the keys left at 0.
    let mut dir = input.is_physical_key_pressed(Key::D) as i32 as f32
        - input.is_physical_key_pressed(Key::A) as i32 as f32;
    let mut aim_y = input.is_physical_key_pressed(Key::S) as i32 as f32
        - input.is_physical_key_pressed(Key::W) as i32 as f32;
    if let Some(dev) = pad2 {
        let sx = input.get_joy_axis(dev, JoyAxis::LEFT_X);
        let sy = input.get_joy_axis(dev, JoyAxis::LEFT_Y);
        let dpx = input.is_joy_button_pressed(dev, JoyButton::DPAD_RIGHT) as i32 as f32
            - input.is_joy_button_pressed(dev, JoyButton::DPAD_LEFT) as i32 as f32;
        let dpy = input.is_joy_button_pressed(dev, JoyButton::DPAD_DOWN) as i32 as f32
            - input.is_joy_button_pressed(dev, JoyButton::DPAD_UP) as i32 as f32;
        if dir == 0.0 {
            dir = if dpx != 0.0 {
                dpx
            } else if sx.abs() > STICK_DEADZONE {
                sx
            } else {
                0.0
            };
        }
        if aim_y == 0.0 {
            aim_y = if dpy != 0.0 {
                dpy
            } else if sy.abs() > STICK_DEADZONE {
                sy
            } else {
                0.0
            };
        }
    }

    // held mask: a button is "held" if its key OR its pad[1] button is down this frame.
    let mut mask = 0u8;
    for (a, bit) in [
        (GameAction::Jump, B_JUMP),
        (GameAction::ShortHop, B_SHORTHOP),
        (GameAction::Attack, B_ATTACK),
        (GameAction::Shield, B_SHIELD),
        (GameAction::Grab, B_GRAB),
        (GameAction::Special, B_SPECIAL),
    ] {
        let down = input.is_physical_key_pressed(p2_key(a))
            || pad2.is_some_and(|d| input.is_joy_button_pressed(d, p2_pad_button(a)));
        if down {
            mask |= bit;
        }
    }
    if aim_y > 0.4 {
        mask |= B_DOWN;
    }

    let prev = P2_PREV_MASK.get();
    P2_PREV_MASK.set(mask);
    let edge = |bit: u8| (mask & bit != 0) && (prev & bit == 0);
    let held = |bit: u8| mask & bit != 0;

    // tap-jump: flick the stick/W up, no button needed (own edge tracker, never P1's).
    let prev_sy = P2_PREV_AIM_Y.get();
    P2_PREV_AIM_Y.set(aim_y);
    let tap_jump = prev_sy > -0.5 && aim_y <= -0.7;

    InputFrame {
        dir,
        aim_y,
        jump: tap_jump || edge(B_JUMP),
        jump_held: held(B_JUMP),
        shorthop: edge(B_SHORTHOP),
        shield_held: held(B_SHIELD),
        shield_pressed: edge(B_SHIELD),
        down: held(B_DOWN),
        down_pressed: edge(B_DOWN),
        attack: edge(B_ATTACK),
        attack_held: held(B_ATTACK),
        grab: edge(B_GRAB),
        special: edge(B_SPECIAL),
    }
}
