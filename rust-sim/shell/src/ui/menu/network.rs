use super::router::{Intent, MenuCtx};
use super::Screen;
use crate::ui::themes::Theme;

/// Netplay page: the matchmaking control moved off the play area into the menu. Shows the live
/// transport snapshot ([`crate::net::NetDebug`]) and a Find/Leave button keyed off the phase. The
/// on-screen chip stays as a passive indicator; joining/leaving happens here.
pub struct Network;

impl Screen for Network {
    fn view<T: Theme>(&self, ui: &mut egui::Ui, theme: &T, cx: &MenuCtx, out: &mut Vec<Intent>) {
        let net = cx.net;
        let offline = net.phase == "offline";

        ui.label(egui::RichText::new(format!("Status: {}", net.phase)).strong());
        ui.add_space(6.0);

        if offline {
            ui.label("Not connected. Find a match to play someone online.");
        } else {
            ui.label(format!("role: {}   ·   handle: {}", net.role, net.handle));
            ui.label(format!("signaling: {}   ·   connection: {}", net.ws, net.conn));
            ui.label(format!("ice: {}   ·   channel: {}", net.gather, net.channel));
        }

        ui.add_space(10.0);
        if offline {
            if theme.button(ui, "Find match").clicked() {
                out.push(Intent::FindMatch);
            }
        } else if theme.button(ui, "Leave match").clicked() {
            out.push(Intent::LeaveMatch);
        }
    }
}
