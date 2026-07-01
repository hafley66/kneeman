use super::router::{Dialog, Intent, MenuCtx};
use super::Screen;
use crate::ui::themes::Theme;

/// Match rules. For now: a reset-feel confirm. Grows into items on/off, spawn interval, knockback,
/// i-frames (mirrors the debug panel's "rules" group).
pub struct Rules;

impl Screen for Rules {
    fn view<T: Theme>(&self, ui: &mut egui::Ui, theme: &T, _cx: &MenuCtx, out: &mut Vec<Intent>) {
        ui.label("Match rules.");
        ui.add_space(8.0);
        if theme.button(ui, "Reset feel…").clicked() {
            out.push(Intent::OpenDialog(Dialog::ConfirmReset));
        }
    }
}
