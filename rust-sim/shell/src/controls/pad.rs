//! Pure controls core: device-agnostic, NO Godot. A frame of raw inputs (`RawPad`) plus a tiny
//! per-player memory fold into the sim's semantic [`InputFrame`]. Tap-jump and frame assembly live
//! here so they behave identically across keyboard/pad/touch and are testable without a device.
//!
//! Reusable: lift this file into another prototype as-is. The only game-specific thing it touches is
//! `InputFrame` (the input contract) — swap that and the mapping body for a new game; the RawPad +
//! cross-frame-edge pattern carries over unchanged. This is the "pure" half the impure `mod.rs`
//! (Godot device reads) feeds. See plans/controls-as-crate.md.

use crate::sim::InputFrame;

/// One player's raw controls for a single frame, already merged across that player's devices by the
/// impure layer. Movement is in [-1, 1] (`move_y` positive = down). Buttons split into `_held`
/// (level this frame) and `_pressed` (rising edge this frame); the impure layer owns edge detection
/// because that needs per-device history, the pure fold below only consumes the result.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct RawPad {
    pub move_x: f32,
    pub move_y: f32,
    pub jump_held: bool,
    pub jump_pressed: bool,
    pub shorthop_pressed: bool,
    pub shield_held: bool,
    pub shield_pressed: bool,
    pub down_held: bool,
    pub down_pressed: bool,
    pub attack_held: bool,
    pub attack_pressed: bool,
    pub grab_pressed: bool,
    pub special_pressed: bool,
}

/// Per-player cross-frame memory the pure mapping carries (just the tap-jump flick edge for now).
/// One instance per player; the impure layer owns the storage and passes it back in each frame.
#[derive(Clone, Copy, Default, Debug)]
pub struct PadMemory {
    prev_move_y: f32,
}

impl PadMemory {
    /// Pure: fold one `RawPad` into a semantic `InputFrame`, advancing `self`. Tap-jump fires when
    /// `move_y` crosses from not-up into hard-up this frame (stick/keys flicked up), so it works the
    /// same for every device and needs no jump button.
    pub fn frame(&mut self, raw: &RawPad) -> InputFrame {
        let prev = self.prev_move_y;
        self.prev_move_y = raw.move_y;
        let tap_jump = prev > -0.5 && raw.move_y <= -0.7;
        InputFrame {
            dir: raw.move_x,
            aim_y: raw.move_y,
            jump: tap_jump || raw.jump_pressed,
            jump_held: raw.jump_held,
            shorthop: raw.shorthop_pressed,
            shield_held: raw.shield_held,
            shield_pressed: raw.shield_pressed,
            down: raw.down_held,
            down_pressed: raw.down_pressed,
            attack: raw.attack_pressed,
            attack_held: raw.attack_held,
            grab: raw.grab_pressed,
            special: raw.special_pressed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tap_jump_fires_on_up_flick_then_not_on_hold() {
        let mut mem = PadMemory::default();
        assert!(!mem.frame(&RawPad::default()).jump, "neutral never jumps");
        let up = RawPad { move_y: -1.0, ..Default::default() };
        assert!(mem.frame(&up).jump, "flick into hard-up taps jump");
        assert!(!mem.frame(&up).jump, "held up has no fresh edge, no jump");
    }

    #[test]
    fn button_jump_passes_through() {
        let mut mem = PadMemory::default();
        let raw = RawPad { jump_pressed: true, ..Default::default() };
        assert!(mem.frame(&raw).jump);
    }

    #[test]
    fn held_and_pressed_map_independently() {
        let mut mem = PadMemory::default();
        let raw = RawPad {
            attack_held: true,
            attack_pressed: false,
            shield_held: true,
            shield_pressed: true,
            ..Default::default()
        };
        let f = mem.frame(&raw);
        assert!(f.attack_held && !f.attack, "held without a fresh press");
        assert!(f.shield_held && f.shield_pressed);
    }
}
