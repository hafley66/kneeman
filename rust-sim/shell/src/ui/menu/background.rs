use super::router::{Dialog, Intent, MenuCtx};
use super::Screen;
use crate::ui::themes::Theme;

/// Stage background page. The GIF library + import land here (see plans/gif-background-library.md).
pub struct Background;

impl Screen for Background {
    fn view<T: Theme>(&self, ui: &mut egui::Ui, theme: &T, _cx: &MenuCtx, out: &mut Vec<Intent>) {
        ui.label("Stage background.");
        ui.small("GIF library + import land here (see plans/gif-background-library.md).");
        ui.add_space(8.0);
        if theme.button(ui, "Import GIF…").clicked() {
            out.push(Intent::OpenDialog(Dialog::GifImport));
        }
    }
}
