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

impl Screen for Network {
    fn view<T: Theme>(&self, ui: &mut egui::Ui, theme: &T, cx: &MenuCtx, out: &mut Vec<Intent>) {
        let net = cx.net;
        let offline = net.phase == "offline";

        ui.label(RichText::new(format!("Status: {}", net.phase)).strong());
        if !offline {
            ui.label(format!("role: {}   ·   handle: {}", net.role, net.handle));
            ui.label(format!("signaling: {}   ·   channel: {}", net.ws, net.channel));
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
                    if ui.add_enabled(!full, egui::Button::new("Join").small()).clicked() {
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
