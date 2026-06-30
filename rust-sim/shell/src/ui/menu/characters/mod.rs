//! Characters page: per-slot roster cycle (writes the charsel cell) + an Edit route per fighter.
//! Will grow its own submodules (skins, attribute editors) -- hence the folder.

use super::router::{Intent, MenuCtx, Route};
use super::Screen;
use crate::roster::roster_names;
use crate::ui::themes::Theme;

pub struct Charss;

impl Screen for Charss {
    fn view<T: Theme>(&self, ui: &mut egui::Ui, theme: &T, cx: &MenuCtx, out: &mut Vec<Intent>) {
        let names = roster_names();
        let n = names.len().max(1) as i64;
        ui.label("Pick each fighter.");
        ui.add_space(6.0);
        for slot in 0..2usize {
            let cur = cx.charsel[slot].rem_euclid(n);
            let name = names.get(cur as usize).map(String::as_str).unwrap_or("?");
            ui.horizontal(|ui| {
                ui.label(format!("P{}", slot + 1));
                if theme.button(ui, "◀").clicked() {
                    out.push(Intent::SetChar {
                        slot,
                        idx: (cur - 1).rem_euclid(n),
                    });
                }
                ui.label(egui::RichText::new(name).strong());
                if theme.button(ui, "▶").clicked() {
                    out.push(Intent::SetChar {
                        slot,
                        idx: (cur + 1).rem_euclid(n),
                    });
                }
                if theme.button(ui, "Edit").clicked() {
                    out.push(Intent::Nav(Route::CharEdit { slot: slot as u8 }));
                }
            });
        }
    }
}

pub struct CharEdit;

impl Screen for CharEdit {
    fn view<T: Theme>(&self, ui: &mut egui::Ui, theme: &T, cx: &MenuCtx, out: &mut Vec<Intent>) {
        let slot = match cx.route {
            Route::CharEdit { slot } => slot,
            _ => 0,
        };
        ui.label(format!("Editing fighter in slot P{}", slot + 1));
        ui.small("Per-fighter editor lands here (skins, attributes).");
        ui.add_space(8.0);
        if theme.button(ui, "Back").clicked() {
            out.push(Intent::Back);
        }
    }
}
