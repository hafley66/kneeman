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
    push_status: &str,
    out: &mut Vec<Intent>,
) {
    let loc = router.location();
    if matches!(loc.base, Route::Closed) {
        return;
    }

    // Escape closes/backs the menu. Consume it HERE (before any widget) so egui's built-in
    // focus-release-on-Escape can't swallow it first now that a nav item is always focused. Routed as
    // Intent::Esc: cancels a dialog, else backs out (Home -> Closed = resume).
    if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
        out.push(Intent::Esc);
    }

    // Modal backdrop: a full-screen scrim UNDER the window (Order::Background < the window's Middle)
    // so the paused game + HUD read as dimmed behind the menu.
    egui::Area::new(egui::Id::new("xp_menu_scrim"))
        .order(egui::Order::Background)
        .fixed_pos(egui::Pos2::ZERO)
        .show(ctx, |ui| {
            ui.painter()
                .rect_filled(ctx.content_rect(), 0.0, egui::Color32::from_black_alpha(150));
        });

    let state = cells.state.get();
    let tune = cells.tune.get();
    let net = cells.net.get();
    let cx = MenuCtx {
        state: &state,
        tune: &tune,
        charsel: cells.charsel.get(),
        route: loc.base,
        net: &net,
        lobbies,
        push_status,
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
    let mut first_resp: Option<egui::Response> = None;
    for &(r, label) in NAV {
        let sel = same_page(cx.route, r);
        let resp = theme.nav_item(ui, label, sel);
        if first_resp.is_none() {
            first_resp = Some(resp.clone());
        }
        if resp.clicked() && !sel {
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
    // Focus bootstrap: when nothing has focus (first open, or after body widgets disappeared on
    // a route change) give it to the first rail entry. Arrow keys need an existing focused widget
    // to move FROM; without this, directional nav is a no-op until something is manually focused.
    if ui.ctx().memory(|m| m.focused()).is_none() {
        if let Some(r) = first_resp {
            r.request_focus();
        }
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

/// A single-confirm modal: scrim + framed body with one Confirm and one Cancel.
fn dialog_layer<T: Theme>(
    ctx: &egui::Context,
    theme: &T,
    ds: DialogState,
    _cx: &MenuCtx,
    out: &mut Vec<Intent>,
) {
    theme.dialog(ctx, dialog_title(ds.kind), |ui| {
        ui.label(dialog_body(ds.kind));
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if theme.button(ui, "Confirm").clicked() {
                out.push(Intent::DialogConfirm);
            }
            if theme.button(ui, "Cancel").clicked() {
                out.push(Intent::DialogCancel);
            }
        });
    });
}

fn dialog_title(d: Dialog) -> &'static str {
    match d {
        Dialog::ConfirmReset => "Reset feel?",
        Dialog::GifImport => "Import GIF",
    }
}

fn dialog_body(d: Dialog) -> String {
    match d {
        Dialog::ConfirmReset => "Restore all physics tuning to defaults.".to_string(),
        Dialog::GifImport => "Pick a GIF for the stage background.".to_string(),
    }
}
