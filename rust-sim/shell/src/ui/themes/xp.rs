//! Windows XP "Luna" chrome. Worn by the menu, applied PER WINDOW via Frame + painter, never through
//! global egui Visuals, so the dark debug panel keeps its own look. `Xp` implements [`super::Theme`].

use egui::{Align2, Color32, CornerRadius, FontFamily, FontId, Margin, Order, Rect, Sense, Stroke};

// Luna palette -------------------------------------------------------------------------------------
pub const TITLE_TOP: Color32 = Color32::from_rgb(0x3A, 0x93, 0xFF); // active title gradient, light at top
pub const TITLE_BOT: Color32 = Color32::from_rgb(0x00, 0x54, 0xE3); // ... deep blue at the bottom
pub const FRAME: Color32 = Color32::from_rgb(0x00, 0x3C, 0xC8); // blue window border
pub const FACE: Color32 = Color32::from_rgb(0xEC, 0xE9, 0xD8); // classic beige-gray body
pub const FACE_HI: Color32 = Color32::from_rgb(0xFD, 0xFD, 0xF6); // hovered button face
pub const FACE_DN: Color32 = Color32::from_rgb(0xD8, 0xD4, 0xC0); // pressed button face
pub const INK: Color32 = Color32::from_rgb(0x10, 0x10, 0x10); // near-black body text
pub const BEVEL_LO: Color32 = Color32::from_rgb(0x91, 0x8E, 0x78); // button shadow edge
pub const RAIL_HOVER: Color32 = Color32::from_rgb(0xEF, 0xF3, 0xFF); // nav hover wash
pub const RAIL_SEL: Color32 = Color32::from_rgb(0xFF, 0xE9, 0x9E); // current-page highlight

/// The XP theme as a unit value. Carries no state.
pub struct Xp;

impl super::Theme for Xp {
    fn window(&self, ctx: &egui::Context, title: &str, add: impl FnOnce(&mut egui::Ui)) {
        frame_window(ctx, "xp_win", title, Order::Middle, add);
    }

    fn dialog(&self, ctx: &egui::Context, title: &str, add: impl FnOnce(&mut egui::Ui)) {
        // dim the base behind the modal, then float the framed dialog above the scrim.
        let screen = ctx.content_rect();
        egui::Area::new(egui::Id::new("xp_scrim"))
            .order(Order::Foreground)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                ui.painter()
                    .rect_filled(screen, 0.0, Color32::from_black_alpha(120));
            });
        frame_window(ctx, "xp_dialog", title, Order::Foreground, add);
    }

    fn button(&self, ui: &mut egui::Ui, label: &str) -> egui::Response {
        button(ui, label)
    }

    fn nav_item(&self, ui: &mut egui::Ui, label: &str, selected: bool) -> egui::Response {
        nav_item(ui, label, selected)
    }
}

/// A centered XP window at the given layer: beige body, blue border, rounded top, captioned bar.
/// Body text is forced dark so labels read on the beige face despite the global dark Visuals.
///
/// `area_id` is a stable string key for the egui Area; it must NOT include the route title so
/// that widget IDs remain stable across route changes (otherwise focus resets on every nav).
fn frame_window(ctx: &egui::Context, area_id: &'static str, title: &str, order: Order, add: impl FnOnce(&mut egui::Ui)) {
    egui::Area::new(egui::Id::new(area_id))
        .anchor(Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .order(order)
        .show(ctx, |ui| {
            egui::Frame::NONE
                .fill(FACE)
                .stroke(Stroke::new(1.0_f32, FRAME))
                .corner_radius(CornerRadius {
                    nw: 8,
                    ne: 8,
                    sw: 3,
                    se: 3,
                })
                .inner_margin(Margin {
                    left: 3,
                    right: 3,
                    top: 3,
                    bottom: 10,
                })
                .show(ui, |ui| {
                    ui.visuals_mut().override_text_color = Some(INK);
                    // `.strong()` captions (e.g. the "Pause Menu" heading) resolve to
                    // `widgets.active`/`noninteractive` text color, NOT override_text_color, so the
                    // global dark Visuals paint them near-white on the beige face. Pin both dark.
                    ui.visuals_mut().widgets.active.fg_stroke.color = INK;
                    ui.visuals_mut().widgets.noninteractive.fg_stroke.color = INK;
                    ui.set_min_width(380.0);
                    title_bar(ui, title);
                    ui.add_space(8.0);
                    add(ui);
                });
        });
}

/// Vertical two-stop gradient fill (egui has no gradient primitive, so paint a 4-vert mesh).
fn vgrad(painter: &egui::Painter, rect: Rect, top: Color32, bot: Color32) {
    use egui::epaint::{Mesh, Vertex, WHITE_UV};
    let mut mesh = Mesh::default();
    mesh.vertices.push(Vertex {
        pos: rect.left_top(),
        uv: WHITE_UV,
        color: top,
    });
    mesh.vertices.push(Vertex {
        pos: rect.right_top(),
        uv: WHITE_UV,
        color: top,
    });
    mesh.vertices.push(Vertex {
        pos: rect.right_bottom(),
        uv: WHITE_UV,
        color: bot,
    });
    mesh.vertices.push(Vertex {
        pos: rect.left_bottom(),
        uv: WHITE_UV,
        color: bot,
    });
    mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
    painter.add(egui::Shape::mesh(mesh));
}

/// The Luna title bar: blue gradient strip, top gloss line, white bold caption. Spans the row.
fn title_bar(ui: &mut egui::Ui, title: &str) {
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, 26.0), Sense::hover());
    let p = ui.painter();
    vgrad(p, rect, TITLE_TOP, TITLE_BOT);
    p.line_segment(
        [
            rect.left_top() + egui::vec2(3.0, 1.5),
            rect.right_top() + egui::vec2(-3.0, 1.5),
        ],
        Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(255, 255, 255, 120)),
    );
    p.text(
        rect.left_center() + egui::vec2(10.0, 0.0),
        Align2::LEFT_CENTER,
        title,
        FontId::new(14.0, FontFamily::Proportional),
        Color32::WHITE,
    );
}

/// A raised XP push button. Restyles locally via a scope so the global dark theme is untouched.
fn button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    ui.scope(|ui| {
        let v = ui.visuals_mut();
        let r = CornerRadius::same(3);
        v.widgets.inactive.weak_bg_fill = FACE;
        v.widgets.inactive.bg_fill = FACE;
        v.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, INK);
        v.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, BEVEL_LO);
        v.widgets.inactive.corner_radius = r;
        v.widgets.hovered.weak_bg_fill = FACE_HI;
        v.widgets.hovered.bg_fill = FACE_HI;
        v.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, INK);
        v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, TITLE_BOT);
        v.widgets.hovered.corner_radius = r;
        v.widgets.active.weak_bg_fill = FACE_DN;
        v.widgets.active.bg_fill = FACE_DN;
        v.widgets.active.fg_stroke = Stroke::new(1.0_f32, INK);
        v.widgets.active.bg_stroke = Stroke::new(1.0_f32, TITLE_BOT);
        v.widgets.active.corner_radius = r;
        ui.add(
            egui::Button::new(egui::RichText::new(label).color(INK))
                .min_size(egui::vec2(88.0, 24.0)),
        )
    })
    .inner
}

/// One left-rail nav entry (XP task pane row). `selected` paints the current page warm + bold.
fn nav_item(ui: &mut egui::Ui, label: &str, selected: bool) -> egui::Response {
    ui.scope(|ui| {
        let v = ui.visuals_mut();
        let r = CornerRadius::same(3);
        let face = if selected {
            RAIL_SEL
        } else {
            Color32::TRANSPARENT
        };
        let fg = if selected { INK } else { TITLE_BOT };
        v.widgets.inactive.weak_bg_fill = face;
        v.widgets.inactive.bg_fill = face;
        v.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, fg);
        v.widgets.inactive.bg_stroke = Stroke::NONE;
        v.widgets.inactive.corner_radius = r;
        v.widgets.hovered.weak_bg_fill = RAIL_HOVER;
        v.widgets.hovered.bg_fill = RAIL_HOVER;
        v.widgets.hovered.fg_stroke = Stroke::new(1.0_f32, INK);
        v.widgets.hovered.bg_stroke = Stroke::NONE;
        v.widgets.hovered.corner_radius = r;
        let txt = egui::RichText::new(label).color(fg).strong();
        ui.add_sized([ui.available_width(), 22.0], egui::Button::new(txt))
    })
    .inner
}
