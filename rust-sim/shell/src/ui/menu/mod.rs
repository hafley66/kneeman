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

    // Swallow egui's Escape (before any widget) so its built-in focus-release-on-Escape can't fire
    // while a nav item is focused. This is UI suppression ONLY -- the back/toggle INTENT comes from
    // the shell's menu_esc (Intent::Esc), the same path the gamepad uses, so keyboard and controller
    // share one semantic mapping. Emitting Intent::Esc here too would double-count a keyboard press.
    ctx.input_mut(|i| {
        i.consume_key(egui::Modifiers::NONE, egui::Key::Escape);
    });

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
        // Top bar: condensed netplay status + a Find/Leave shortcut + a jump to the lobby page on the
        // left; the tiny build stamp stays pinned right. Present on every page so match state and the
        // quick-match button are one glance away without navigating to Network.
        ui.horizontal(|ui| {
            top_bar(ui, theme, &cx, out);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
                ui.label(egui::RichText::new(format!("build {}", env!("BUILD_HASH"))).small().weak());
            });
        });
        ui.separator();
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

/// Condensed inline netplay status + shortcuts, shown on every page. A colored pill (fill = online/
/// offline, leading dot = live channel health) reads the same data as the Network page's chip, then a
/// Find/Leave quick-match button and a jump to the Network page for lobby selection. `out`-only, so it
/// routes through the same shell interception as the Network screen's buttons.
fn top_bar<T: Theme>(ui: &mut egui::Ui, theme: &T, cx: &MenuCtx, out: &mut Vec<Intent>) {
    let net = cx.net;
    let offline = net.phase == "offline";
    // Dot = channel health at a glance: green once the data channel is live, amber while it negotiates.
    let dot = if offline {
        egui::Color32::from_rgb(120, 130, 145)
    } else if matches!(net.channel, "open" | "connected") {
        egui::Color32::from_rgb(90, 220, 130)
    } else {
        egui::Color32::from_rgb(235, 190, 70)
    };
    let fill = if offline {
        egui::Color32::from_rgb(60, 65, 80)
    } else {
        egui::Color32::from_rgb(40, 140, 80)
    };

    ui.horizontal(|ui| {
        egui::Frame::NONE
            .fill(fill)
            .corner_radius(egui::CornerRadius::same(6))
            .stroke(egui::Stroke::new(1.0_f32, egui::Color32::from_black_alpha(70)))
            .inner_margin(egui::Margin::symmetric(8, 3))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let (rect, _) = ui.allocate_exact_size(egui::vec2(9.0, 9.0), egui::Sense::hover());
                ui.painter().circle_filled(rect.center(), 4.5, dot);
                // Text-only when offline (nothing to condense); online packs phase · channel · handle.
                let text = if offline {
                    "offline".to_string()
                } else {
                    format!("{}  ·  ch:{}  ·  h{}", net.phase, net.channel, net.handle)
                };
                ui.label(egui::RichText::new(text).small().strong().color(egui::Color32::WHITE));
            });

        // Quick-match shortcut: Find when offline, Leave once in a match.
        if offline {
            if theme.button(ui, "⚔ Find match").clicked() {
                out.push(Intent::FindMatch);
            }
        } else if theme.button(ui, "Leave").clicked() {
            out.push(Intent::LeaveMatch);
        }
        // Jump to the Network page for lobby selection (hidden when already there).
        if !matches!(cx.route, Route::Network) && theme.button(ui, "🌐 Lobbies").clicked() {
            out.push(Intent::Nav(Route::Network));
        }
    });
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
