use egui::{Color32, CornerRadius, FontFamily, FontId, Margin, Stroke, Style, TextStyle, Visuals};

// design tokens (your :root variables) ------------------------------------------------
pub const PANEL: Color32 = Color32::from_rgb(40, 42, 58);
pub const SURFACE: Color32 = Color32::from_rgb(49, 50, 68);
pub const SURFACE_HI: Color32 = Color32::from_rgb(59, 61, 82);
pub const ACCENT: Color32 = Color32::from_rgb(137, 180, 250);
pub const TEXT: Color32 = Color32::from_rgb(205, 214, 244);
pub const MUTED: Color32 = Color32::from_rgb(154, 160, 184);
pub const LINE: Color32 = Color32::from_rgb(69, 71, 90);

const R_WIDGET: CornerRadius = CornerRadius::same(6);

/// The dark theme as a unit value. Carries no state; methods route to the free fns below.
pub struct Dark;

impl super::Theme for Dark {
    fn install(&self, ctx: &egui::Context) {
        apply(ctx);
    }

    fn window(&self, ctx: &egui::Context, title: &str, add: impl FnOnce(&mut egui::Ui)) {
        egui::Window::new(title).show(ctx, |ui| add(ui));
    }

    fn dialog(&self, ctx: &egui::Context, title: &str, add: impl FnOnce(&mut egui::Ui)) {
        egui::Modal::new(egui::Id::new(("dark_dlg", title))).show(ctx, |ui| {
            ui.set_min_width(280.0);
            ui.heading(title);
            ui.add_space(6.0);
            add(ui);
        });
    }

    fn button(&self, ui: &mut egui::Ui, label: &str) -> egui::Response {
        ui.button(label)
    }

    fn nav_item(&self, ui: &mut egui::Ui, label: &str, selected: bool) -> egui::Response {
        ui.selectable_label(selected, label)
    }
}

/// The stylesheet. Apply once via EguiBridge::setup_context.
pub fn apply(ctx: &egui::Context) {
    let mut v = Visuals::dark();
    v.override_text_color = Some(TEXT);
    v.window_fill = PANEL;
    v.panel_fill = PANEL;
    v.window_stroke = Stroke::new(1.0_f32, LINE);
    v.window_corner_radius = CornerRadius::same(10);
    v.selection.bg_fill = Color32::from_rgba_unmultiplied(137, 180, 250, 90);
    v.selection.stroke = Stroke::new(1.0_f32, ACCENT);

    // widget states (egui's "no classnames" :hover/:active, set per WidgetVisuals)
    v.widgets.inactive.bg_fill = SURFACE;
    v.widgets.inactive.weak_bg_fill = SURFACE;
    v.widgets.inactive.corner_radius = R_WIDGET;
    v.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, TEXT);
    v.widgets.hovered.bg_fill = SURFACE_HI;
    v.widgets.hovered.weak_bg_fill = SURFACE_HI;
    v.widgets.hovered.corner_radius = R_WIDGET;
    v.widgets.active.bg_fill = ACCENT;
    v.widgets.active.weak_bg_fill = ACCENT;
    v.widgets.active.corner_radius = R_WIDGET;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, LINE);

    // spreads: everything else stays at Default
    let mut style = Style {
        visuals: v,
        ..Default::default()
    };
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(10.0, 4.0);
    style.spacing.window_margin = Margin::same(12);
    style.spacing.slider_width = 150.0;
    style.text_styles = [
        (TextStyle::Heading, FontId::new(15.0, FontFamily::Proportional)),
        (TextStyle::Body, FontId::new(13.0, FontFamily::Proportional)),
        (TextStyle::Monospace, FontId::new(12.0, FontFamily::Monospace)),
        (TextStyle::Button, FontId::new(13.0, FontFamily::Proportional)),
        (TextStyle::Small, FontId::new(11.0, FontFamily::Proportional)),
    ]
    .into();

    ctx.set_style(style);
}

/// A styled container (a "div"). Base off the group frame, override via spreads-as-methods.
pub fn card(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::group(ui.style())
        .fill(SURFACE)
        .stroke(Stroke::new(1.0_f32, LINE))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(10))
        .show(ui, add);
}

/// A stat row: muted label left, mono value right-aligned.
pub fn stat(ui: &mut egui::Ui, label: &str, value: impl Into<String>) {
    ui.horizontal(|ui| {
        ui.colored_label(MUTED, label);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.monospace(value.into());
        });
    });
}
