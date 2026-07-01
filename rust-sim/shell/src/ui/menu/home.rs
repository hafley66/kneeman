use super::router::{Intent, MenuCtx};
use super::Screen;
use crate::ui::themes::Theme;

/// Pause-menu landing pane. The left rail is the nav (one menu); this pane is just the content for
/// the Home entry — a short hint + resume — so it never duplicates the rail's list.
pub struct Home;

impl Screen for Home {
    fn view<T: Theme>(&self, ui: &mut egui::Ui, theme: &T, _cx: &MenuCtx, out: &mut Vec<Intent>) {
        ui.label(egui::RichText::new("Pause Menu").size(16.0).strong());
        ui.add_space(8.0);
        ui.label("Pick a page from the rail on the left.");
        ui.label(egui::RichText::new("Esc or ○/B closes the menu and resumes the fight.").weak());
        ui.add_space(14.0);
        if theme.button(ui, "Resume (Esc)").clicked() {
            out.push(Intent::Back);
        }
    }
}
