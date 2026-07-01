//! The XP-themed menu: a memory-router state machine ([`router`]) drawn through a [`Theme`]. Each
//! base [`Route`] maps to a [`Screen`] (a ZST unit, dispatched by `match` -> monomorphized, no dyn).
//! Screens are read-only over [`MenuCtx`] and push [`Intent`]s; the router applies them after the frame.

pub mod router;

mod background;
mod characters;
mod controls;
mod feel;
mod home;
mod items;
mod network;
mod rules;

use crate::sim::{self, ItemKind};
use crate::ui::themes::Theme;
use router::{Dialog, DialogState, Intent, MenuCells, MenuCtx, Route, Router};

/// A page body. Generic over the theme so screens draw their buttons through it (still monomorphized).
pub trait Screen {
    fn view<T: Theme>(&self, ui: &mut egui::Ui, theme: &T, cx: &MenuCtx, out: &mut Vec<Intent>);
}

/// Draw the whole menu for the frame and collect intents into `out`. No-op while the base is Closed.
pub fn menu<T: Theme>(
    ctx: &egui::Context,
    theme: &T,
    router: &mut Router,
    cells: &MenuCells,
    lobbies: &[crate::net::LobbyRow],
    out: &mut Vec<Intent>,
) {
    let loc = router.location();
    if matches!(loc.base, Route::Closed) {
        return;
    }
    let state = cells.state.get();
    let tune = cells.tune.get();
    let net = cells.net.get();
    let cx = MenuCtx {
        state: &state,
        tune: &tune,
        charsel: cells.charsel.get(),
        route: loc.base,
        require_both: router.require_both(),
        net: &net,
        lobbies,
    };

    theme.window(ctx, title_of(loc.base), |ui| {
        // tiny build stamp, top-right — so you can see at a glance which wasm is actually live.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
            ui.label(egui::RichText::new(format!("build {}", env!("BUILD_HASH"))).small().weak());
        });
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(132.0);
                rail(ui, theme, &cx, out);
            });
            ui.separator();
            ui.vertical(|ui| {
                ui.set_min_width(248.0);
                render_base(loc.base, ui, theme, &cx, out);
            });
        });
    });

    if let Some(ds) = loc.dialog {
        dialog_layer(ctx, theme, ds, &cx, out);
    }
}

/// Left task-pane: one entry per page, the current one highlighted, plus a resume row.
fn rail<T: Theme>(ui: &mut egui::Ui, theme: &T, cx: &MenuCtx, out: &mut Vec<Intent>) {
    const NAV: &[(Route, &str)] = &[
        (Route::Home, "Home"),
        (Route::Items, "Items"),
        (Route::Charss, "Characters"),
        (Route::Rules, "Rules"),
        (Route::Background, "Background"),
        (Route::Feel, "Feel"),
        (Route::Controls, "Controls"),
        (Route::Network, "Network"),
    ];
    for &(r, label) in NAV {
        let sel = same_page(cx.route, r);
        if theme.nav_item(ui, label, sel).clicked() && !sel {
            out.push(Intent::Nav(r));
        }
    }
    // Debug opens the egui panel (intercepted by the shell) and closes the menu so the panel shows.
    if theme.nav_item(ui, "Debug", false).clicked() {
        out.push(Intent::OpenDebugPanel);
        out.push(Intent::Nav(Route::Closed));
    }
    ui.add_space(10.0);
    if theme.nav_item(ui, "▸ Resume game", false).clicked() {
        out.push(Intent::Nav(Route::Closed));
    }
}

/// Whether `nav`'s rail entry should look active given the current route (CharEdit -> Characters).
fn same_page(cur: Route, nav: Route) -> bool {
    cur == nav || matches!((cur, nav), (Route::CharEdit { .. }, Route::Charss))
}

fn render_base<T: Theme>(
    route: Route,
    ui: &mut egui::Ui,
    theme: &T,
    cx: &MenuCtx,
    out: &mut Vec<Intent>,
) {
    match route {
        Route::Home => home::Home.view(ui, theme, cx, out),
        Route::Items => items::Items.view(ui, theme, cx, out),
        Route::Charss => characters::Charss.view(ui, theme, cx, out),
        Route::CharEdit { .. } => characters::CharEdit.view(ui, theme, cx, out),
        Route::Rules => rules::Rules.view(ui, theme, cx, out),
        Route::Background => background::Background.view(ui, theme, cx, out),
        Route::Feel => feel::Feel.view(ui, theme, cx, out),
        Route::Controls => controls::Controls.view(ui, theme, cx, out),
        Route::Network => network::Network.view(ui, theme, cx, out),
        Route::Closed => {}
    }
}

fn title_of(route: Route) -> &'static str {
    match route {
        Route::Home => "Smash",
        Route::Items => "Items",
        Route::Charss => "Characters",
        Route::CharEdit { .. } => "Edit Fighter",
        Route::Rules => "Rules",
        Route::Background => "Background",
        Route::Feel => "Feel",
        Route::Controls => "Controls",
        Route::Network => "Network",
        Route::Closed => "",
    }
}

/// The two-player modal: scrim + framed body with a confirm button per player and a cancel.
fn dialog_layer<T: Theme>(
    ctx: &egui::Context,
    theme: &T,
    ds: DialogState,
    cx: &MenuCtx,
    out: &mut Vec<Intent>,
) {
    theme.dialog(ctx, dialog_title(ds.kind), |ui| {
        ui.label(dialog_body(ds.kind));
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            for p in 0..2usize {
                let label = if ds.confirms[p] {
                    format!("P{} ✓", p + 1)
                } else {
                    format!("P{} OK", p + 1)
                };
                if theme.button(ui, &label).clicked() {
                    out.push(Intent::DialogConfirm(p));
                }
            }
            if theme.button(ui, "Cancel").clicked() {
                out.push(Intent::DialogCancel);
            }
        });
        ui.add_space(4.0);
        ui.small(if cx.require_both {
            "both players must confirm"
        } else {
            "either player confirms"
        });
    });
}

fn dialog_title(d: Dialog) -> &'static str {
    match d {
        Dialog::ConfirmReset => "Reset feel?",
        Dialog::SpawnConfirm(_) => "Spawn item?",
        Dialog::GifImport => "Import GIF",
    }
}

fn dialog_body(d: Dialog) -> String {
    match d {
        Dialog::ConfirmReset => "Restore all physics tuning to defaults.".to_string(),
        Dialog::SpawnConfirm(k) => format!("Drop a {} onto the stage.", item_name(k)),
        Dialog::GifImport => "Pick a GIF for the stage background.".to_string(),
    }
}

fn item_name(k: ItemKind) -> &'static str {
    sim::MENU_ITEMS
        .iter()
        .find(|c| c.kind == k)
        .map(|c| c.name)
        .unwrap_or("item")
}
