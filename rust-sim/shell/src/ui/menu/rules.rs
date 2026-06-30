use super::router::{Dialog, Intent, MenuCtx};
use super::Screen;
use crate::ui::themes::Theme;

/// Match rules. For now: the dialog require-both toggle + a reset-feel confirm. Grows into items
/// on/off, spawn interval, knockback, i-frames (mirrors the debug panel's "rules" group).
pub struct Rules;

impl Screen for Rules {
    fn view<T: Theme>(&self, ui: &mut egui::Ui, theme: &T, cx: &MenuCtx, out: &mut Vec<Intent>) {
        ui.label("Match rules.");
        ui.add_space(6.0);
        let mut both = cx.require_both;
        if ui
            .checkbox(&mut both, "dialogs need both players")
            .changed()
        {
            out.push(Intent::SetRequireBoth(both));
        }
        ui.add_space(8.0);
        if theme.button(ui, "Reset feel…").clicked() {
            out.push(Intent::OpenDialog(Dialog::ConfirmReset));
        }
    }
}
