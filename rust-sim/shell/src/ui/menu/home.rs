use super::router::{Intent, MenuCtx, Route};
use super::Screen;
use crate::ui::themes::Theme;

/// Pause-menu root: big buttons to each page, plus resume.
pub struct Home;

impl Screen for Home {
    fn view<T: Theme>(&self, ui: &mut egui::Ui, theme: &T, _cx: &MenuCtx, out: &mut Vec<Intent>) {
        ui.label(egui::RichText::new("Pause Menu").size(16.0).strong());
        ui.add_space(8.0);
        for (r, label) in [
            (Route::Items, "Items"),
            (Route::Charss, "Characters"),
            (Route::Rules, "Rules"),
            (Route::Background, "Background"),
            (Route::Feel, "Feel"),
        ] {
            if theme.button(ui, label).clicked() {
                out.push(Intent::Nav(r));
            }
        }
        ui.add_space(10.0);
        if theme.button(ui, "Resume (Esc)").clicked() {
            out.push(Intent::Back);
        }
    }
}
