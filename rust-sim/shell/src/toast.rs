//! Global snackbar/toast mechanic. A `Mutable<Vec<Toast>>` cell (the same BehaviorSubject idiom as
//! the other shell cells) that anyone in the shell pushes into; the egui frame drains, ages, and draws
//! them over everything -- independent of the menu's `Closed` gate, so a "you were disconnected" line
//! shows during gameplay, not just in the pause menu. No I/O, no clock: expiry rides the per-frame
//! `dt` already threaded through `DebugUi::process`.

use egui::Color32;
use futures_signals::signal::Mutable;

/// Cap the on-screen stack so a flapping transport can't bury the view in toasts.
const MAX_ON_SCREEN: usize = 4;
/// Seconds over which a toast fades out at the end of its life.
const FADE_SECS: f32 = 0.35;

/// Severity: picks the accent color and how long the message dwells.
#[derive(Clone, Copy, PartialEq)]
pub enum ToastKind {
    Info,
    Success,
    Warn,
    Error,
}

impl ToastKind {
    fn accent(self) -> Color32 {
        match self {
            ToastKind::Info => Color32::from_rgb(90, 160, 235),
            ToastKind::Success => Color32::from_rgb(90, 220, 130),
            ToastKind::Warn => Color32::from_rgb(235, 190, 70),
            ToastKind::Error => Color32::from_rgb(235, 95, 95),
        }
    }

    /// Errors linger; routine info clears fast.
    fn ttl(self) -> f32 {
        match self {
            ToastKind::Info => 3.0,
            ToastKind::Success => 3.0,
            ToastKind::Warn => 5.0,
            ToastKind::Error => 6.0,
        }
    }
}

/// One message. `remaining` counts down in seconds; the toast is drawn while it is above zero.
#[derive(Clone)]
pub struct Toast {
    pub text: String,
    pub kind: ToastKind,
    pub remaining: f32,
}

/// The shared cell. Cloned into `DebugUi` from `KneeMan` like the other cells.
pub type Toasts = Mutable<Vec<Toast>>;

/// Push a message onto the shared cell. Coalesces a repeat of the current tail (a phase that flaps
/// Running<->Reconnecting won't stack duplicates) and trims the oldest past `MAX_ON_SCREEN`.
pub fn push(cell: &Toasts, kind: ToastKind, text: impl Into<String>) {
    let text = text.into();
    let mut q = cell.lock_mut();
    if q.last().is_some_and(|t| t.text == text) {
        return; // same message already showing; leave its timer alone
    }
    q.push(Toast { text, kind, remaining: kind.ttl() });
    let overflow = q.len().saturating_sub(MAX_ON_SCREEN);
    if overflow > 0 {
        q.drain(0..overflow);
    }
}

/// Age every toast by `dt`, drop the expired, and draw the survivors bottom-center (newest on top).
/// Call once per frame from the egui pass, before any early return, so toasts show over the game too.
pub fn render(ctx: &egui::Context, dt: f32, cell: &Toasts) {
    let toasts = {
        let mut q = cell.lock_mut();
        for t in q.iter_mut() {
            t.remaining -= dt;
        }
        q.retain(|t| t.remaining > 0.0);
        q.clone()
    };
    if toasts.is_empty() {
        return;
    }

    egui::Area::new(egui::Id::new("snackbar"))
        .order(egui::Order::Foreground) // above the menu window and its scrim
        .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -28.0))
        .interactable(false)
        .show(ctx, |ui| {
            ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
                for t in toasts.iter().rev() {
                    toast_row(ui, t);
                }
            });
        });

    // Keep egui repainting while toasts live so the countdown advances even when the game is idle.
    ctx.request_repaint();
}

fn toast_row(ui: &mut egui::Ui, t: &Toast) {
    let alpha = (t.remaining / FADE_SECS).min(1.0); // fade only over the last FADE_SECS
    let accent = t.kind.accent().gamma_multiply(alpha);
    let bg = Color32::from_rgb(28, 32, 42).gamma_multiply(alpha);
    let text = Color32::from_rgb(238, 240, 245).gamma_multiply(alpha);

    egui::Frame::NONE
        .fill(bg)
        .corner_radius(egui::CornerRadius::same(7))
        .stroke(egui::Stroke::new(1.0_f32, accent))
        .inner_margin(egui::Margin::symmetric(12, 8))
        .outer_margin(egui::Margin::symmetric(0, 3))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let (rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                ui.painter().circle_filled(rect.center(), 4.0, accent);
                ui.label(egui::RichText::new(&t.text).color(text).strong());
            });
        });
}
