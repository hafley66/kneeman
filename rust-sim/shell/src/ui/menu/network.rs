use super::router::{Intent, MenuCtx};
use super::Screen;
use crate::net::now_ms;
use crate::ui::themes::Theme;
use egui::{Color32, RichText};

/// Netplay page: 1v1 quick-match plus the versioned lobby browser. Lobbies are grouped by build
/// version ("floating rooms by version"); an empty one is culled after a TTL, shown ticking down in
/// the grid. Rows come from shell-held state (`cx.lobbies`), fed by the relay's `list` in P2.
pub struct Network;

/// Color the TTL countdown hot as it runs out.
fn ttl_color(secs: u32) -> Color32 {
    match secs {
        0..=5 => Color32::from_rgb(230, 90, 80),
        6..=20 => Color32::from_rgb(230, 180, 70),
        _ => Color32::from_rgb(140, 150, 165),
    }
}

/// Background color for the status chip: green when online, dark-slate when offline.
fn phase_chip_color(offline: bool) -> Color32 {
    if offline {
        Color32::from_rgb(60, 65, 80)
    } else {
        Color32::from_rgb(40, 140, 80)
    }
}

impl Screen for Network {
    fn view<T: Theme>(&self, ui: &mut egui::Ui, theme: &T, cx: &MenuCtx, out: &mut Vec<Intent>) {
        let net = cx.net;
        let offline = net.phase == "offline";

        // Prominent online/offline chip.
        let chip_label = if offline { "OFFLINE" } else { &format!("ONLINE  ·  {}", net.phase) };
        ui.horizontal(|ui| {
            egui::Frame::NONE
                .fill(phase_chip_color(offline))
                .corner_radius(egui::CornerRadius::same(6))
                .inner_margin(egui::Margin { left: 10, right: 10, top: 4, bottom: 4 })
                .show(ui, |ui| {
                    ui.label(RichText::new(chip_label).strong().color(Color32::WHITE));
                });
        });

        ui.add_space(4.0);
        if !offline {
            ui.label(format!("role: {}   ·   handle: {}", net.role, net.handle));
            ui.label(format!("signaling: {}   ·   channel: {}", net.ws, net.channel));
        }

        // Version / relay ping status line.
        ui.add_space(6.0);
        if net.stale_build {
            ui.label(
                RichText::new("WARNING: new build live -- reload the page to update")
                    .color(Color32::from_rgb(230, 160, 40))
                    .strong(),
            );
        } else if net.peer_build_mismatch {
            ui.label(
                RichText::new("VERSION MISMATCH: opponent is on a different build -- both reload")
                    .color(Color32::from_rgb(230, 90, 80))
                    .strong(),
            );
        } else {
            ui.label(
                RichText::new(format!("build {}  ·  relay {}", net.build_hash, crate::rtc::STATUS_URL))
                    .small()
                    .weak(),
            );
        }

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if offline {
                if theme.button(ui, "Find match").clicked() {
                    out.push(Intent::FindMatch);
                }
                if theme.button(ui, "Open lobby").clicked() {
                    out.push(Intent::OpenLobby);
                }
            } else if theme.button(ui, "Leave match").clicked() {
                out.push(Intent::LeaveMatch);
            }
        });

        ui.add_space(12.0);
        ui.label(RichText::new("Lobbies").strong());
        ui.add_space(4.0);

        let now = now_ms();
        let lobbies = cx.lobbies;
        egui::Grid::new("lobby_grid")
            .striped(true)
            .num_columns(5)
            .spacing([14.0, 6.0])
            .show(ui, |ui| {
                for h in ["Version", "Host", "Players", "TTL", ""] {
                    ui.label(RichText::new(h).strong().weak());
                }
                ui.end_row();

                for row in lobbies {
                    ui.label(&row.key);
                    ui.label(&row.host);
                    let full = row.active >= row.cap;
                    ui.label(
                        RichText::new(format!("{}/{}", row.active, row.cap))
                            .color(if full { Color32::from_rgb(230, 90, 80) } else { Color32::GRAY }),
                    );
                    match row.ttl_remaining_secs(now) {
                        Some(s) => ui.label(RichText::new(format!("culls {s}s")).color(ttl_color(s))),
                        None => ui.label(RichText::new("live").color(Color32::from_rgb(120, 200, 130))),
                    };
                    if row.host == "you" {
                        // Own lobby: render a disabled chip so the column is still filled.
                        ui.add_enabled(false, egui::Button::new("Joined").small());
                    } else if ui.add_enabled(!full, egui::Button::new("Join").small()).clicked() {
                        out.push(Intent::JoinLobby(row.key.clone()));
                    }
                    ui.end_row();
                }
            });

        if lobbies.is_empty() {
            ui.add_space(4.0);
            ui.label(RichText::new("No lobbies for this version yet — open one.").weak());
        }
    }
}
