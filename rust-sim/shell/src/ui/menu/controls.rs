use super::router::{Intent, MenuCtx};
use super::Screen;
use crate::ui::themes::Theme;

/// Keyboard controls reference (display only -- no rebinding).
pub struct Controls;

// P1: arrow keys move, Space jump, X shorthop, Z shield, C attack/grab/hold-fire, V drop, B special.
// P2: WASD move, G jump, Y shorthop, H attack, K shield, L grab, J special.
// Keycodes from project.godot [input]: shorthop=88(X) shield=90(Z) attack=67(C) grab=86(V) special=66(B) jump=32(Space).
// P2 raw keys from controls/mod.rs p2_key().

const P1: &[(&str, &str)] = &[
    ("Arrows", "move"),
    ("Space", "jump"),
    ("X", "shorthop"),
    ("Z", "shield"),
    ("C", "attack / grab / hold to fire"),
    ("V", "drop item"),
    ("B", "special"),
];

const P2: &[(&str, &str)] = &[
    ("WASD", "move"),
    ("G", "jump"),
    ("Y", "shorthop"),
    ("H", "attack"),
    ("K", "shield"),
    ("L", "grab"),
    ("J", "special"),
];

impl Screen for Controls {
    fn view<T: Theme>(&self, ui: &mut egui::Ui, _theme: &T, _cx: &MenuCtx, _out: &mut Vec<Intent>) {
        ui.label("Keyboard controls. Both players also support a connected gamepad.");
        ui.add_space(6.0);
        ui.columns(2, |cols| {
            cols[0].label(egui::RichText::new("Player 1").strong());
            for (key, action) in P1 {
                cols[0].label(format!("{key}  -  {action}"));
            }
            cols[1].label(egui::RichText::new("Player 2").strong());
            for (key, action) in P2 {
                cols[1].label(format!("{key}  -  {action}"));
            }
        });
    }
}
