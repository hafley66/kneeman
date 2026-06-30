use super::router::{Intent, MenuCtx};
use super::Screen;
use crate::ui::themes::Theme;

/// Feel / physics tuning page. Stub: the debug panel's sliders move here next.
pub struct Feel;

impl Screen for Feel {
    fn view<T: Theme>(&self, ui: &mut egui::Ui, _theme: &T, cx: &MenuCtx, _out: &mut Vec<Intent>) {
        ui.label("Feel / physics tuning.");
        ui.small("Mirrors the debug panel's sliders; lands here next.");
        ui.add_space(6.0);
        ui.label(format!(
            "gravity {:.0}  ·  air speed {:.0}",
            cx.tune.gravity, cx.tune.air_speed
        ));
    }
}
