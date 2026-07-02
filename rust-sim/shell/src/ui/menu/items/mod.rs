//! Items page. Lists the spawnable roster from `core`'s `MENU_ITEMS` and offers a direct spawn plus
//! a two-player confirm-dialog spawn. Will grow its own submodules (drop tables, per-item config).

use super::router::{Intent, MenuCtx};
use super::Screen;
use crate::sim::MENU_ITEMS;
use crate::ui::themes::Theme;

pub struct Items;

impl Screen for Items {
    fn view<T: Theme>(&self, ui: &mut egui::Ui, theme: &T, cx: &MenuCtx, out: &mut Vec<Intent>) {
        let on_stage = cx.state.items.iter().filter(|i| i.active()).count();
        ui.label(format!("Spawn items onto the stage.  ({on_stage} live)"));
        ui.add_space(6.0);
        for card in MENU_ITEMS {
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(egui::RichText::new(card.name).strong());
                        ui.small(card.blurb);
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if theme.button(ui, "Spawn").clicked() {
                            out.push(Intent::SpawnItem(card.kind, card.tool, card.stroke));
                        }
                    });
                });
            });
        }
        ui.add_space(8.0);
        if theme.button(ui, "Clear field").clicked() {
            out.push(Intent::ClearItems);
        }
    }
}
