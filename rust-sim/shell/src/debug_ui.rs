use godot::classes::{Camera2D, INode, InputEvent, InputEventKey, Node};
use godot::global::Key;
use godot::prelude::*;

use futures_signals::signal::Mutable;

use crate::kneeman::{Identity, KneeMan, NetDebug};
use crate::sim::{Action, AttackData, Fighter, SimState, Tune};
use crate::theme;

/// Which group of collapsers the panel is showing. Persisted on the node so it survives the
/// per-frame immediate-mode redraw.
#[derive(Clone, Copy, PartialEq, Default)]
enum Tab {
    #[default]
    Status,
    Feel,
    Net,
    Identity,
    Gamepad,
}

/// Hosts the egui bridge and draws "our stuff" panel by reading/writing the KneeMan
/// BehaviorSubjects. Cmd+Shift+J toggles the panel; Cmd+Shift+R reloads the scene.
#[derive(GodotClass)]
#[class(base = Node)]
pub struct DebugUi {
    base: Base<Node>,
    #[export]
    fighter_path: NodePath,
    egui: Option<Gd<gdext_egui::EguiBridge>>,
    fighter: Option<Gd<KneeMan>>,
    camera: Option<Gd<Camera2D>>,
    show: bool,
    tab: Tab,
}

#[godot_api]
impl INode for DebugUi {
    fn init(base: Base<Node>) -> Self {
        Self {
            base,
            fighter_path: NodePath::default(),
            egui: None,
            fighter: None,
            camera: None,
            show: false, // start hidden; Cmd+Shift+J toggles
            tab: Tab::default(),
        }
    }

    fn ready(&mut self) {
        let bridge = gdext_egui::EguiBridge::new_alloc();
        bridge.bind().setup_context(|ctx| theme::apply(ctx)); // stylesheet, once
        let node = bridge.clone().upcast::<Node>();
        self.base_mut().add_child(&node);
        self.egui = Some(bridge);

        let fp = self.fighter_path.clone();
        if !fp.is_empty() {
            if let Some(n) = self.base().get_node_or_null(&fp) {
                self.fighter = n.try_cast::<KneeMan>().ok();
            }
        }
        self.camera = self
            .base()
            .get_node_or_null("../Camera2D")
            .and_then(|n| n.try_cast::<Camera2D>().ok());
    }

    fn input(&mut self, event: Gd<InputEvent>) {
        let Ok(key) = event.try_cast::<InputEventKey>() else {
            return;
        };
        if !key.is_pressed() || key.is_echo() {
            return;
        }
        // Backtick toggles with no modifier — the web build can't use Cmd+Shift+J (Chrome eats it
        // as the devtools shortcut), and ` triggers no default browser action over the canvas.
        if key.get_keycode() == Key::QUOTELEFT {
            self.show = !self.show;
            return;
        }
        if key.is_meta_pressed() && key.is_shift_pressed() {
            match key.get_keycode() {
                Key::J => self.show = !self.show,
                Key::R => {
                    if let Some(mut tree) = self.base().get_tree() {
                        tree.reload_current_scene();
                    }
                }
                _ => {}
            }
        }
    }

    fn process(&mut self, _dt: f64) {
        let (Some(bridge), Some(fighter)) = (self.egui.clone(), self.fighter.clone()) else {
            return;
        };
        if !self.show {
            return;
        }
        let ctx = bridge.bind().current_frame().clone();
        // grab the shared BehaviorSubjects (cheap clones of the same cells)
        let (state_cell, tune_cell, net_cell, identity_cell) = {
            let f = fighter.bind();
            (f.state_cell(), f.tune_cell(), f.net_cell(), f.identity_cell())
        };
        draw_panel(
            &ctx,
            &state_cell,
            &tune_cell,
            &net_cell,
            &identity_cell,
            self.camera.clone(),
            &mut self.tab,
        );
    }
}

// the view is a function of the signal cells: read .get(), write .set() on change.
fn draw_panel(
    ctx: &egui::Context,
    state_cell: &Mutable<SimState>,
    tune_cell: &Mutable<Tune>,
    net_cell: &Mutable<NetDebug>,
    identity_cell: &Mutable<Identity>,
    camera: Option<Gd<Camera2D>>,
    tab: &mut Tab,
) {
    let s = state_cell.get();
    let mut t = tune_cell.get();

    egui::Window::new("KneeMan  ·  our stuff")
        .default_size(egui::vec2(300.0, 440.0))
        .show(ctx, |ui| {
      ui.horizontal(|ui| {
          ui.selectable_value(tab, Tab::Status, "status");
          ui.selectable_value(tab, Tab::Feel, "feel");
          ui.selectable_value(tab, Tab::Net, "net");
          ui.selectable_value(tab, Tab::Identity, "identity");
          ui.selectable_value(tab, Tab::Gamepad, "pad");
      });
      ui.separator();
      egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
        match *tab {
        Tab::Status => {
        let p = &s.fighters[0]; // debug panel tracks player 0
        egui::CollapsingHeader::new("status").default_open(false).show(ui, |ui| {
            theme::card(ui, |ui| {
                theme::stat(ui, "state", p.state_name());
                theme::stat(ui, "frame", p.frame.to_string());
                theme::stat(ui, "facing", if p.facing < 0.0 { "◄ left" } else { "right ►" });
                theme::stat(ui, "pos", format!("{:.0}, {:.0}", p.pos.x, p.pos.y));
                theme::stat(ui, "vel", format!("{:.0}, {:.0}", p.vel.x, p.vel.y));
                theme::stat(ui, "air jumps", p.air_jumps.to_string());
                theme::stat(ui, "air dodges", p.air_dodges.to_string());
                theme::stat(ui, "fast fall", p.fast_falling.to_string());
                theme::stat(ui, "intangible", p.intangible.to_string());
                theme::stat(ui, "hitlag", p.hitlag.to_string());
                theme::stat(ui, "aerial buf", p.aerial_buffer_frames().to_string());
                theme::stat(ui, "attack buf", p.attack_buffer_frames().to_string());
                theme::stat(ui, "holding", if p.holding >= 0 { "gun" } else { "—" });
                theme::stat(ui, "autohop", if p.autohop_aerial { "yes (-dmg)" } else { "no" });
                theme::stat(ui, "own %", format!("{:.0}", p.damage));
                theme::stat(ui, "dummy %", format!("{:.0}", s.fighters[1].damage));
            });
        });

        egui::CollapsingHeader::new("input buffer").default_open(false).show(ui, |ui| {
            buffer_card(ui, &s.fighters[0], &t);
        });

        if let Some(mut cam) = camera {
            egui::CollapsingHeader::new("view scale").default_open(false).show(ui, |ui| {
                theme::card(ui, |ui| {
                    let mut z = cam.get_zoom().x;
                    if ui
                        .add(egui::Slider::new(&mut z, 0.4..=2.5).text("camera zoom"))
                        .changed()
                    {
                        cam.set_zoom(Vector2::new(z, z));
                    }
                });
            });
        }
        }
        Tab::Feel => {
        let mut c = false;

        egui::CollapsingHeader::new("ground").default_open(false).show(ui, |ui| {
            c |= slider(ui, &mut t.walk_speed, 0.0..=1500.0, "walk_speed");
            c |= slider(ui, &mut t.dash_init, 0.0..=1500.0, "dash_init");
            c |= slider(ui, &mut t.run_speed, 0.0..=1500.0, "run_speed");
            c |= slider(ui, &mut t.ground_accel, 200.0..=8000.0, "ground_accel");
            c |= slider(ui, &mut t.ground_friction, 100.0..=8000.0, "ground_friction");
            c |= slider(ui, &mut t.dashstop_friction, 100.0..=8000.0, "dashstop_friction");
            c |= slider(ui, &mut t.dash_pivot_keep, 0.0..=1.0, "dash_pivot_keep (1=free dashdance)");
        });
        egui::CollapsingHeader::new("jump").default_open(false).show(ui, |ui| {
            c |= slider(ui, &mut t.fullhop_v, -2500.0..=-100.0, "fullhop_v");
            c |= slider(ui, &mut t.shorthop_v, -1500.0..=-50.0, "shorthop_v");
            c |= slider(ui, &mut t.airjump_v, -2000.0..=-100.0, "airjump_v");
            c |= slider(ui, &mut t.airjump_h, 0.0..=1500.0, "airjump_h (DJ redirect)");
            c |= slider(ui, &mut t.jump_h_init, 0.0..=1000.0, "jump_h_init");
            c |= slider(ui, &mut t.jump_h_max, 0.0..=2000.0, "jump_h_max");
            c |= slider(ui, &mut t.momentum_carry, 0.0..=1.5, "momentum_carry");
        });
        egui::CollapsingHeader::new("air / fall").default_open(false).show(ui, |ui| {
            c |= slider(ui, &mut t.gravity, 200.0..=6000.0, "gravity");
            c |= slider(ui, &mut t.max_fall, 200.0..=2500.0, "max_fall");
            c |= slider(ui, &mut t.fastfall, 200.0..=3000.0, "fastfall");
            c |= slider(ui, &mut t.air_speed, 0.0..=1500.0, "air_speed (drift cap)");
            c |= slider(ui, &mut t.air_accel, 100.0..=8000.0, "air_accel (mobility)");
            c |= slider(ui, &mut t.air_friction, 0.0..=2000.0, "air_friction (drag)");
            c |= slider(ui, &mut t.fastfall_threshold, 0.1..=1.0, "fastfall threshold (down vs side)");
        });
        egui::CollapsingHeader::new("dodge / ledge").default_open(false).show(ui, |ui| {
            c |= slider(ui, &mut t.roll_speed, 0.0..=1500.0, "roll_speed");
            c |= slider(ui, &mut t.airdodge_speed, 0.0..=2500.0, "airdodge_speed");
            c |= slider(ui, &mut t.airdodge_drag, 0.0..=8000.0, "airdodge_drag");
            c |= slider(ui, &mut t.ledgejump_v, -2500.0..=-100.0, "ledgejump_v");
        });
        egui::CollapsingHeader::new("attack · jab").default_open(false).show(ui, |ui| {
            c |= attack_sliders(ui, &mut t.jab);
        });
        egui::CollapsingHeader::new("attack · nair").default_open(false).show(ui, |ui| {
            c |= attack_sliders(ui, &mut t.nair);
            c |= slider(ui, &mut t.autohop_dmg, 0.5..=1.0, "autohop dmg x (jump+atk)");
        });
        egui::CollapsingHeader::new("attack · dair").default_open(false).show(ui, |ui| {
            c |= slider(ui, &mut t.dair_threshold, 0.1..=1.0, "dair threshold (down vs side)");
            c |= attack_sliders(ui, &mut t.dair);
        });
        egui::CollapsingHeader::new("attack · dash").default_open(false).show(ui, |ui| {
            c |= attack_sliders(ui, &mut t.dash_attack);
        });
        egui::CollapsingHeader::new("knockback").default_open(false).show(ui, |ui| {
            c |= slider(ui, &mut t.di_max_angle, 0.0..=30.0, "di_max_angle° (survival DI)");
        });
        egui::CollapsingHeader::new("frames").default_open(false).show(ui, |ui| {
            c |= islider(ui, &mut t.jumpsquat, 1..=10, "jumpsquat");
            c |= islider(ui, &mut t.landing_lag, 1..=20, "landing_lag");
            c |= islider(ui, &mut t.dash_window, 1..=30, "dash_window");
            c |= islider(ui, &mut t.pivot_frames, 0..=10, "pivot_frames");
            c |= islider(ui, &mut t.spotdodge_frames, 1..=40, "spotdodge_frames");
            c |= islider(ui, &mut t.roll_frames, 1..=40, "roll_frames");
            c |= islider(ui, &mut t.airdodge_frames, 1..=50, "airdodge_frames");
            c |= islider(ui, &mut t.ledge_intang, 0..=60, "ledge_intang");
            c |= islider(ui, &mut t.climb_frames, 1..=50, "climb_frames");
            c |= islider(ui, &mut t.buffer_frames, 0..=20, "buffer_frames");
            c |= islider(ui, &mut t.max_air_jumps, 0..=5, "max_air_jumps");
            c |= islider(ui, &mut t.max_air_dodges, 0..=5, "max_air_dodges");
        });
        egui::CollapsingHeader::new("items · laser").default_open(false).show(ui, |ui| {
            c |= ui.checkbox(&mut t.items_on, "items on").changed();
            c |= islider(ui, &mut t.item_spawn_interval, 60..=1800, "spawn interval (f)");
            c |= slider(ui, &mut t.laser.spawn_weight, 0.0..=10.0, "spawn weight");
            c |= islider(ui, &mut t.laser.ammo, 1..=99, "ammo / gun");
            c |= islider(ui, &mut t.laser.cooldown, 1..=60, "tap cooldown (f)");
            c |= islider(ui, &mut t.laser.autofire_cd, 1..=60, "hold cooldown (f)");
            c |= slider(ui, &mut t.laser.autofire_dmg, 0.1..=1.0, "hold dmg x (weaker)");
            c |= slider(ui, &mut t.laser.speed, 200.0..=3000.0, "bolt speed");
            c |= islider(ui, &mut t.laser.range, 10..=240, "bolt range (f)");
            c |= attack_sliders(ui, &mut t.laser.hit);
        });

        if c {
            tune_cell.set(t);
        }
        }
        Tab::Net => net_card(ui, &net_cell.get()),
        Tab::Identity => identity_card(ui, identity_cell),
        Tab::Gamepad => gamepad_card(ui),
        }

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("Respawn").clicked() {
                state_cell.set(SimState::spawn());
            }
            if ui.button("Reset feel (KneeMan)").clicked() {
                tune_cell.set(Tune::default());
            }
        });
        ui.small("space/↑ jump · X shorthop · Z shield · C attack/grab/hold-fire · V drop · arrows move · ↓ fastfall");
        ui.small("` toggle panel · Cmd+Shift+J hide · Cmd+Shift+R reset scene · Enter find match");
      });
    });
}

/// Netplay transport readout. The handshake order is: ws open -> matched (role) -> host offer ->
/// guest answer -> ICE both ways -> conn `connected` -> channel `open` -> rollback. A stall shows
/// here: e.g. signal stuck at `have-local-offer` with answer in = 0 means the peer never answered.
fn net_card(ui: &mut egui::Ui, n: &NetDebug) {
    theme::card(ui, |ui| {
        theme::stat(ui, "phase", n.phase);
        theme::stat(ui, "role", format!("{} (handle {})", n.role, n.handle));
        theme::stat(ui, "signaling ws", n.ws);
        theme::stat(ui, "pc conn", n.conn);
        theme::stat(ui, "ice gather", n.gather);
        theme::stat(ui, "sdp signal", n.signal);
        theme::stat(ui, "data channel", n.channel);
    });
    ui.add_space(6.0);
    ui.label(egui::RichText::new("handshake frames  (out / in)").color(theme::MUTED));
    theme::card(ui, |ui| {
        theme::stat(ui, "offer", format!("{} / {}", n.offer.0, n.offer.1));
        theme::stat(ui, "answer", format!("{} / {}", n.answer.0, n.answer.1));
        theme::stat(ui, "ice", format!("{} / {}", n.ice.0, n.ice.1));
    });
}

/// Player identity: name + color the sprite and nametag wear. Edits write back to the shared cell;
/// KneeMan persists them to localStorage (web) and refreshes the tag. The color is godot-side RGBA;
/// the picker works in RGB and rebuilds the color on change.
fn identity_card(ui: &mut egui::Ui, cell: &Mutable<Identity>) {
    let mut id = cell.get_cloned();
    let mut changed = false;
    theme::card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.colored_label(theme::MUTED, "name");
            let resp = egui::TextEdit::singleline(&mut id.name)
                .char_limit(16)
                .desired_width(170.0)
                .show(ui)
                .response;
            changed |= resp.changed();
        });
        ui.horizontal(|ui| {
            ui.colored_label(theme::MUTED, "color");
            let mut rgb = [id.color.r, id.color.g, id.color.b];
            if ui.color_edit_button_rgb(&mut rgb).changed() {
                id.color = Color::from_rgb(rgb[0], rgb[1], rgb[2]);
                changed = true;
            }
        });
    });
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new("saved to this browser · hovers over your fighter")
            .size(11.0)
            .color(theme::MUTED),
    );
    if changed {
        cell.set(id);
    }
}

/// Live controller readout: connected pad name, both sticks as dots in a gate, triggers as bars, and
/// a pip per button lit when held (labeled with the action it drives). Reads Godot's Input singleton,
/// which on web is fed by the browser Gamepad API through the SDL mapping DB. No pad showing usually
/// means the browser hasn't seen input yet -- click the canvas and press a button.
fn gamepad_card(ui: &mut egui::Ui) {
    use godot::classes::Input;
    use godot::global::{JoyAxis, JoyButton};
    let mut input = Input::singleton();
    let Some(dev) = input.get_connected_joypads().get(0) else {
        theme::card(ui, |ui| {
            ui.colored_label(theme::MUTED, "no controller detected");
        });
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(
                "pair the pad, click the game, then press any button \
                 (browsers hide gamepads until they send input)",
            )
            .size(11.0)
            .color(theme::MUTED),
        );
        return;
    };
    let dev = dev as i32; // Input methods take i32 device ids

    theme::card(ui, |ui| {
        theme::stat(ui, "device", input.get_joy_name(dev).to_string());
    });
    ui.add_space(6.0);

    ui.horizontal(|ui| {
        stick_box(ui, "L stick (move/DI)",
            input.get_joy_axis(dev, JoyAxis::LEFT_X), input.get_joy_axis(dev, JoyAxis::LEFT_Y));
        stick_box(ui, "R stick",
            input.get_joy_axis(dev, JoyAxis::RIGHT_X), input.get_joy_axis(dev, JoyAxis::RIGHT_Y));
    });
    ui.add_space(6.0);

    theme::card(ui, |ui| {
        let lt = input.get_joy_axis(dev, JoyAxis::TRIGGER_LEFT).clamp(0.0, 1.0);
        let rt = input.get_joy_axis(dev, JoyAxis::TRIGGER_RIGHT).clamp(0.0, 1.0);
        ui.add(egui::ProgressBar::new(lt).desired_height(8.0).text("L2"));
        ui.add(egui::ProgressBar::new(rt).desired_height(8.0).text("R2"));
    });
    ui.add_space(6.0);

    // button = our action where one is bound (see project.godot [input]).
    let pips = [
        (JoyButton::A, "✕ jump"),
        (JoyButton::X, "□ attack"),
        (JoyButton::LEFT_SHOULDER, "L1 shield"),
        (JoyButton::RIGHT_SHOULDER, "R1 shorthop"),
        (JoyButton::BACK, "create · grab"),
        (JoyButton::B, "○"),
        (JoyButton::Y, "△"),
        (JoyButton::START, "options"),
        (JoyButton::DPAD_UP, "d-up"),
        (JoyButton::DPAD_DOWN, "d-down"),
        (JoyButton::DPAD_LEFT, "d-left"),
        (JoyButton::DPAD_RIGHT, "d-right"),
    ];
    theme::card(ui, |ui| {
        for (b, label) in pips {
            let on = input.is_joy_button_pressed(dev, b);
            ui.horizontal(|ui| {
                let (resp, painter) = ui.allocate_painter(egui::vec2(12.0, 12.0), egui::Sense::hover());
                painter.circle_filled(resp.rect.center(), 5.0, if on { theme::ACCENT } else { theme::LINE });
                ui.colored_label(if on { theme::ACCENT } else { theme::MUTED }, label);
            });
        }
    });
}

/// One analog stick: a circular gate with crosshair and a dot at the stick position (-1..1 each axis).
fn stick_box(ui: &mut egui::Ui, label: &str, x: f32, y: f32) {
    ui.vertical(|ui| {
        ui.colored_label(theme::MUTED, label);
        let (resp, painter) = ui.allocate_painter(egui::vec2(72.0, 72.0), egui::Sense::hover());
        let ctr = resp.rect.center();
        let r = 28.0_f32;
        let grid = egui::Stroke::new(1.0_f32, theme::LINE);
        painter.circle_stroke(ctr, r, grid);
        painter.line_segment([egui::pos2(ctr.x - r, ctr.y), egui::pos2(ctr.x + r, ctr.y)], grid);
        painter.line_segment([egui::pos2(ctr.x, ctr.y - r), egui::pos2(ctr.x, ctr.y + r)], grid);
        let p = egui::pos2(ctr.x + x.clamp(-1.0, 1.0) * r, ctr.y + y.clamp(-1.0, 1.0) * r);
        painter.circle_filled(p, 4.0, theme::ACCENT);
        ui.colored_label(theme::MUTED, format!("{x:+.2}, {y:+.2}"));
    });
}

/// Live readout of the input buffer: what's queued, how long it stays valid, and the captured
/// aim (the diagonal that the air dodge / wavedash will fire with).
fn buffer_card(ui: &mut egui::Ui, f: &Fighter, t: &Tune) {
    theme::card(ui, |ui| {
        let slot = f.move_buffer();
        let active = slot.timer > 0 && slot.action != Action::None;
        let name = slot.action.name();
        let col = if active { theme::ACCENT } else { theme::MUTED };
        ui.horizontal(|ui| {
            ui.colored_label(theme::MUTED, "queued");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.colored_label(col, name);
            });
        });

        let denom = (t.buffer_frames + 1).max(1) as f32;
        let frac = (slot.timer as f32 / denom).clamp(0.0, 1.0);
        ui.add(
            egui::ProgressBar::new(frac)
                .desired_height(8.0)
                .text(format!("{} / {} f", slot.timer, t.buffer_frames)),
        );

        // aim compass: line points to the buffered aim (the diagonal that will be used)
        let (resp, painter) =
            ui.allocate_painter(egui::vec2(72.0, 72.0), egui::Sense::hover());
        let ctr = resp.rect.center();
        let r = 28.0_f32;
        let grid = egui::Stroke::new(1.0_f32, theme::LINE);
        painter.circle_stroke(ctr, r, grid);
        painter.line_segment([egui::pos2(ctr.x - r, ctr.y), egui::pos2(ctr.x + r, ctr.y)], grid);
        painter.line_segment([egui::pos2(ctr.x, ctr.y - r), egui::pos2(ctr.x, ctr.y + r)], grid);
        let a = slot.aim;
        if a.length() > 0.01 {
            let n = a.normalize_or_zero();
            let end = egui::pos2(ctr.x + n.x * r, ctr.y + n.y * r);
            painter.line_segment([ctr, end], egui::Stroke::new(2.0_f32, col));
            painter.circle_filled(end, 3.5, col);
        } else {
            painter.circle_filled(ctr, 2.5, theme::MUTED);
        }
    });
}

/// One attack's full data table: frame windows, hitbox geometry, knockback. Returns changed.
fn attack_sliders(ui: &mut egui::Ui, a: &mut AttackData) -> bool {
    let mut c = false;
    c |= islider(ui, &mut a.startup, 0..=30, "startup");
    c |= islider(ui, &mut a.active, 1..=30, "active");
    c |= islider(ui, &mut a.recovery, 0..=40, "recovery");
    c |= slider(ui, &mut a.off.x, -20.0..=140.0, "off.x (forward)");
    c |= slider(ui, &mut a.off.y, -130.0..=40.0, "off.y (up = -)");
    c |= slider(ui, &mut a.r, 6.0..=90.0, "radius");
    c |= slider(ui, &mut a.damage, 0.0..=40.0, "damage %");
    c |= slider(ui, &mut a.kb_base, 0.0..=1600.0, "kb_base");
    c |= slider(ui, &mut a.kb_scale, 0.0..=20.0, "kb_scale / %");
    c |= slider(ui, &mut a.kb_angle, 0.0..=180.0, "kb_angle°");
    c
}

fn slider(ui: &mut egui::Ui, val: &mut f32, range: std::ops::RangeInclusive<f32>, label: &str) -> bool {
    ui.add(egui::Slider::new(val, range).text(label)).changed()
}

fn islider(ui: &mut egui::Ui, val: &mut i64, range: std::ops::RangeInclusive<i64>, label: &str) -> bool {
    ui.add(egui::Slider::new(val, range).text(label)).changed()
}
