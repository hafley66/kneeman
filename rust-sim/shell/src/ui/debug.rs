use godot::classes::{
    Camera2D, CanvasLayer, HttpRequest, INode, Input, InputEvent, InputEventJoypadButton,
    InputEventKey, Node,
};
use godot::global::{JoyButton, Key};
use godot::prelude::*;

use futures_signals::signal::Mutable;

use crate::identity::Identity;
use crate::kneeman::KneeMan;
use crate::net::{push_enable, push_label, NetDebug};
use crate::sim::{Action, AttackData, Fighter, Hitbox, SimState, Tune};
use crate::ui::menu::router::{Intent, MenuCells, Route, Router};
use crate::ui::themes::{dark, xp::Xp, Theme};

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
    Server,
}

/// A CRUD action the Saves window emitted this frame; actuated after the draw via the KneeMan handle
/// (egui can't call Godot mid-draw, mirrors the `want_status` pattern).
enum SaveAction {
    Save,
    Load(String),
    Delete(String),
    Reset,
}

/// Initial delay (s) before dpad direction starts repeating when held.
const PAD_INITIAL_DELAY_S: f64 = 0.35;
/// Interval (s) between repeat firings once the initial delay has elapsed.
const PAD_REPEAT_INTERVAL_S: f64 = 0.10;

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
    http: Option<Gd<HttpRequest>>,    // fetches the relay's /status JSON
    server_status: Mutable<String>,   // latest status body (or a fetching/error message)
    router: Router,                   // XP menu nav state (memory-router); independent of the panel
    menu_esc: bool,                   // an Esc was pressed; the next process() resolves it (deferred)
    lobbies: Vec<crate::net::LobbyRow>, // relay-fed rows for OTHER hosts (P2 feed); never holds the "you" row
    my_lobby_key: Option<String>,       // room we dialed via Open/Join; the "you" row is DERIVED from (net.phase, this)
    push_status: String,                // web-push opt-in label mirrored from JS, shown on the Network page
    // Dpad hold-to-repeat state. Tracks which direction is currently held and drives
    // synthetic arrow key repeats from process(dt) at PAD_INITIAL_DELAY_S + PAD_REPEAT_INTERVAL_S cadence.
    pad_held: Option<JoyButton>,
    pad_hold_secs: f64,
    pad_repeats_fired: u32,
    save_label: String, // text field in the Saves window; the label a manual save writes under
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
            http: None,
            server_status: Mutable::new(String::from("(not fetched)")),
            router: Router::default(),
            menu_esc: false,
            lobbies: Vec::new(),
            my_lobby_key: None,
            push_status: String::new(),
            pad_held: None,
            pad_hold_secs: 0.0,
            pad_repeats_fired: 0,
            save_label: String::from("save 1"),
        }
    }

    fn ready(&mut self) {
        let bridge = gdext_egui::EguiBridge::new_alloc();
        bridge.bind().setup_context(|ctx| dark::Dark.install(ctx)); // stylesheet, once
        // Pin the egui CanvasLayer above the HUD (damage panel = layer 1) and the touch/MENU strip
        // (layer 50) so the pause menu + its scrim always sit on top instead of under the % boxes.
        bridge.clone().upcast::<CanvasLayer>().set_layer(100);
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

        // HTTP client for the server-status tab. Web export routes this through the browser fetch.
        let mut http = HttpRequest::new_alloc();
        let cb = self.to_gd();
        http.connect("request_completed", &Callable::from_object_method(&cb, "on_status"));
        self.base_mut().add_child(&http);
        self.http = Some(http);
    }

    fn input(&mut self, event: Gd<InputEvent>) {
        // Gamepad drives the pause menu. Start opens/backs it (like Esc); while it's open, dpad + A
        // + B become synthetic key events (Tab focus-nav / Enter activate / Esc back) that egui and
        // this handler already understand -- so no custom focus model and no bridge changes.
        if let Ok(pad) = event.clone().try_cast::<InputEventJoypadButton>() {
            if pad.is_pressed() && !pad.is_echo() {
                if pad.get_button_index() == JoyButton::START {
                    self.open_pause_menu();
                    return;
                }
                // ○/B closes the debug panel when it's the thing on screen (menu not open).
                if pad.get_button_index() == JoyButton::B && !self.is_menu_open() && self.show {
                    self.show = false;
                    return;
                }
            }
            if self.is_menu_open() {
                self.gamepad_menu_nav(pad);
            }
            return;
        }
        let Ok(key) = event.try_cast::<InputEventKey>() else {
            return;
        };
        if !key.is_pressed() || key.is_echo() {
            return;
        }
        // (set_open + is_open below give the on-screen MENU button the same control as backtick.)
        // Backtick toggles with no modifier — the web build can't use Cmd+Shift+J (Chrome eats it
        // as the devtools shortcut), and ` triggers no default browser action over the canvas.
        if key.get_keycode() == Key::QUOTELEFT {
            self.show = !self.show;
            return;
        }
        // Esc maps to the SAME semantic back/toggle intent as the gamepad (START/B): menu_esc ->
        // Intent::Esc, resolved in process(). One path for keyboard + controller so they can't
        // diverge. egui's consume_key in menu() only suppresses focus-release, it does not re-emit the
        // intent (that double-counted a keyboard Esc: open then instantly back out). The lone
        // keyboard-specific case is closing the debug panel when IT, not the menu, is the thing up.
        if key.get_keycode() == Key::ESCAPE {
            if !self.is_menu_open() && self.show {
                self.show = false;
            } else {
                self.menu_esc = true;
            }
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

    fn process(&mut self, dt: f64) {
        let (Some(bridge), Some(mut fighter)) = (self.egui.clone(), self.fighter.clone()) else {
            return;
        };
        let ctx = bridge.bind().current_frame().clone();
        // grab the shared BehaviorSubjects (cheap clones of the same cells)
        let (state_cell, tune_cell, gizmos_cell, net_cell, identity_cell, charsel_cell, toasts_cell) = {
            let f = fighter.bind();
            (f.state_cell(), f.tune_cell(), f.gizmos_cell(), f.net_cell(), f.identity_cell(), f.charsel_cell(), f.toasts_cell())
        };

        // --- XP menu: drawn every frame (independent of the debug panel toggle) ---
        {
            let cells = MenuCells {
                state: &state_cell,
                tune: &tune_cell,
                charsel: &charsel_cell,
                net: &net_cell,
            };
            // resolve a pending Esc BEFORE drawing so the route change shows this frame;
            // clear held dpad state so repeat timers don't fire after the menu closes.
            if std::mem::take(&mut self.menu_esc) {
                self.pad_held = None;
                self.pad_hold_secs = 0.0;
                self.pad_repeats_fired = 0;
                self.router.apply(vec![Intent::Esc], &cells);
            }
            // Drive dpad hold-to-repeat: inject synthetic arrow key events so egui
            // processes them in this frame's begin_pass before the menu is drawn.
            if self.is_menu_open() {
                self.tick_pad_repeat(dt);
            }
            // Mirror the JS push status into the menu while the Network page is open (skip the JS
            // bridge call on every other screen). No-op/empty on the native build.
            if matches!(self.router.location().base, Route::Network) {
                self.push_status = push_label();
            }
            let mut out: Vec<Intent> = Vec::new();
            // Derive the grid rows fresh each frame: relay-fed rows + a "you" row synthesized from the
            // live net phase, so going offline drops it (no persisted phantom).
            let display_lobbies = self.display_lobbies(net_cell.get().phase);
            crate::ui::menu::menu(
                &ctx,
                &Xp,
                &mut self.router,
                &cells,
                &display_lobbies,
                &self.push_status,
                &mut out,
            );
            // Intercept the shell-driven intents before Router::apply so the pure router never sees
            // them: OpenDebugPanel toggles this panel; Find/LeaveMatch drive KneeMan's netplay; the
            // lobby intents mutate the shell-held `self.lobbies` (local UI state, not the transport).
            let mut open_panel = false;
            let mut find_match = false;
            let mut leave_match = false;
            let mut open_lobby_key: Option<String> = None;
            let mut join_key: Option<String> = None;
            out.retain(|intent| match intent {
                Intent::OpenDebugPanel => {
                    open_panel = true;
                    false
                }
                Intent::FindMatch => {
                    find_match = true;
                    false
                }
                Intent::LeaveMatch => {
                    leave_match = true;
                    false
                }
                Intent::OpenLobby => {
                    open_lobby_key = Some(Self::lobby_key());
                    false
                }
                Intent::JoinLobby(key) => {
                    join_key = Some(key.clone());
                    false
                }
                Intent::PushSubscribe => {
                    push_enable(); // JS bridge (web-only); fires the browser permission prompt
                    false
                }
                _ => true,
            });
            if open_panel {
                self.show = true;
            }
            if find_match {
                fighter.bind_mut().find_match();
                crate::toast::push(&toasts_cell, crate::toast::ToastKind::Info, "Searching for a match…");
            }
            if leave_match {
                fighter.bind_mut().leave_match();
                self.my_lobby_key = None; // dropping the transport drops the derived "you" row
            }
            // Lobby = a named relay room. Opening hosts the versioned room AND shows a local grid row;
            // both actions just dial `matchmake_room`, so two clients on the same key pair over the
            // existing 1v1 transport. The mock `active` count bump is gone (no more self-join inflation).
            if let Some(key) = open_lobby_key {
                self.my_lobby_key = Some(key.clone());
                fighter.bind_mut().matchmake_room(&key);
            }
            if let Some(key) = join_key {
                self.my_lobby_key = Some(key.clone());
                fighter.bind_mut().matchmake_room(&key);
            }
            self.router.apply(out, &cells);
        }

        // Snackbar: drawn every frame OVER game + menu (foreground), so a disconnect/reconnect line
        // shows during play, not only in the pause menu. Ages itself off `dt`, above the `self.show` gate.
        crate::toast::render(&ctx, dt as f32, &toasts_cell);

        if !self.show {
            return;
        }
        // Saved-worlds CRUD. Read the slot list + cache size through the KneeMan handle, draw the window,
        // then actuate whatever button was pressed (bind_mut after the borrow ends).
        {
            let (slots, bytes) = {
                let f = fighter.bind();
                (f.world_slots(), f.world_cache_bytes())
            };
            let mut action: Option<SaveAction> = None;
            draw_saves_window(&ctx, &slots, bytes, &mut self.save_label, &mut action);
            match action {
                Some(SaveAction::Save) => {
                    let label = self.save_label.trim().to_string();
                    if !label.is_empty() {
                        fighter.bind_mut().world_save(&label);
                    }
                }
                Some(SaveAction::Load(l)) => fighter.bind_mut().world_load(&l),
                Some(SaveAction::Delete(l)) => fighter.bind_mut().world_delete(&l),
                Some(SaveAction::Reset) => fighter.bind_mut().world_reset(),
                None => {}
            }
        }
        let mut want_status = false;
        draw_panel(
            &ctx,
            &state_cell,
            &tune_cell,
            &gizmos_cell,
            &net_cell,
            &identity_cell,
            &charsel_cell,
            self.camera.clone(),
            &mut self.tab,
            &self.server_status,
            &mut want_status,
        );
        if want_status {
            self.fetch_status();
        }
    }
}

#[godot_api]
impl DebugUi {
    /// Show/hide the panel from outside (the on-screen MENU button drives this).
    #[func]
    pub fn set_open(&mut self, open: bool) {
        self.show = open;
    }

    /// Whether the panel is currently open (the MENU button toggles off this).
    #[func]
    fn is_open(&self) -> bool {
        self.show
    }

    /// Whether the XP pause menu is currently showing (route is not Closed).
    /// KneeMan reads this each frame to decide whether to freeze the sim.
    pub fn is_menu_open(&self) -> bool {
        !matches!(self.router.location().base, Route::Closed)
    }

    /// Queue an Esc intent for the menu router: from in-game (Closed) this opens the pause
    /// menu (Home); from inside the menu it backs out one level. Deferred to process() like
    /// menu_esc, so egui is not poked mid-input.
    pub fn open_pause_menu(&mut self) {
        self.menu_esc = true;
    }

    /// Handle a gamepad button event while the pause menu is open.
    ///
    /// Presses: dpad directions start hold tracking and emit the initial arrow key event;
    ///   A emits Enter (activate focused widget); B sets menu_esc (back one level, no synthetic
    ///   Esc injected so egui never clears focus inadvertently).
    /// Releases: clear the held direction so repeat timers stop.
    /// Echo events are ignored (Godot joypad buttons do not echo; guard is defensive).
    fn gamepad_menu_nav(&mut self, pad: Gd<InputEventJoypadButton>) {
        let btn = pad.get_button_index();
        if pad.is_pressed() && !pad.is_echo() {
            match btn {
                JoyButton::DPAD_DOWN | JoyButton::DPAD_UP
                | JoyButton::DPAD_LEFT | JoyButton::DPAD_RIGHT => {
                    self.pad_held = Some(btn);
                    self.pad_hold_secs = 0.0;
                    self.pad_repeats_fired = 0;
                    Self::emit_nav_arrow(btn);
                }
                JoyButton::A => Self::emit_key(Key::ENTER),
                JoyButton::B => { self.menu_esc = true; }
                _ => {}
            }
        } else if !pad.is_pressed() {
            if matches!(btn, JoyButton::DPAD_DOWN | JoyButton::DPAD_UP
                            | JoyButton::DPAD_LEFT | JoyButton::DPAD_RIGHT)
                && self.pad_held == Some(btn)
            {
                self.pad_held = None;
                self.pad_hold_secs = 0.0;
                self.pad_repeats_fired = 0;
            }
        }
    }

    /// Advance the hold-repeat timer by `dt` seconds and fire synthetic arrow key events for
    /// any repeat intervals that have elapsed. Called from process() while the menu is open.
    fn tick_pad_repeat(&mut self, dt: f64) {
        let Some(btn) = self.pad_held else { return; };
        self.pad_hold_secs += dt;
        let due: u32 = if self.pad_hold_secs < PAD_INITIAL_DELAY_S {
            0
        } else {
            1 + ((self.pad_hold_secs - PAD_INITIAL_DELAY_S) / PAD_REPEAT_INTERVAL_S) as u32
        };
        let to_fire = due.saturating_sub(self.pad_repeats_fired);
        for _ in 0..to_fire {
            Self::emit_nav_arrow(btn);
            self.pad_repeats_fired += 1;
        }
    }

    /// Inject a synthetic arrow key event (pressed=true) for a dpad direction. egui reads arrow
    /// keys as directional FocusDirection (Up/Down/Left/Right), enabling 2D geometric focus nav.
    fn emit_nav_arrow(btn: JoyButton) {
        let key = match btn {
            JoyButton::DPAD_DOWN => Key::DOWN,
            JoyButton::DPAD_UP => Key::UP,
            JoyButton::DPAD_LEFT => Key::LEFT,
            JoyButton::DPAD_RIGHT => Key::RIGHT,
            _ => return,
        };
        Self::emit_key(key);
    }

    /// Inject a synthetic key-press event into Godot's input pipeline (feeds the egui bridge).
    fn emit_key(keycode: Key) {
        let mut ev = InputEventKey::new_gd();
        ev.set_keycode(keycode);
        ev.set_pressed(true);
        Input::singleton().parse_input_event(&ev);
    }

    /// Kick off a GET of the relay's /status page (the server-status tab's refresh button).
    #[func]
    fn fetch_status(&mut self) {
        if let Some(http) = self.http.as_mut() {
            self.server_status.set(String::from("fetching…"));
            let _ = http.request(&crate::rtc::status_url());
        }
    }

    /// HttpRequest completion: stash the body (or an error) for the tab to render.
    #[func]
    fn on_status(&mut self, _result: i64, code: i64, _headers: PackedStringArray, body: PackedByteArray) {
        if code != 200 {
            self.server_status.set(format!("request failed (HTTP {code})"));
            return;
        }
        self.server_status.set(body.get_string_from_utf8().to_string());
    }
}

// Shell-held lobby list mutations, driven by the Network page's OpenLobby/JoinLobby intents. This is
// LOCAL UI state (never the KneeMan transport / SimState / checksum); the relay `list` feed replaces
// it in P2 (see lobby-netplay-plan.md §7).
impl DebugUi {
    /// The versioned lobby room code: same-build clients share it, so cross-build never pairs (which
    /// would desync). Doubles as the grid `key` and the relay `&room=` dial code.
    fn lobby_key() -> String {
        format!("lobby-{}", crate::rtc::BUILD_HASH)
    }

    /// Rows the Network grid renders: the relay-fed OTHER hosts (`self.lobbies`) with the "you" row
    /// DERIVED from live net state prepended. The you-row exists only while `net.phase != "offline"`
    /// and we hold a dialed key -- so a dropped connection removes it instead of leaving a phantom
    /// "Joined" row (the old bug: a persisted row that never cleared when we went offline).
    fn display_lobbies(&self, phase: &str) -> Vec<crate::net::LobbyRow> {
        let mut rows = Vec::with_capacity(self.lobbies.len() + 1);
        if phase != "offline" {
            if let Some(key) = &self.my_lobby_key {
                rows.push(crate::net::LobbyRow {
                    key: key.clone(),
                    host: "you".into(),
                    active: if phase == "running" { 2 } else { 1 },
                    cap: crate::sim::MAX_PLAYERS as u8,
                    empty_since_ms: None,
                });
            }
        }
        rows.extend(self.lobbies.iter().cloned());
        rows
    }
}

/// The Saved-worlds window: cache-size line, a name field + Save, a Reset-to-defaults button, and the
/// slot list with per-row Load/delete. Pure view — clicks flow out through `out`, the caller actuates.
fn draw_saves_window(
    ctx: &egui::Context,
    slots: &[smash_core::world::store::Slot],
    cache_bytes: u64,
    label: &mut String,
    out: &mut Option<SaveAction>,
) {
    egui::Window::new("Saved worlds")
        .default_size(egui::vec2(280.0, 320.0))
        .show(ctx, |ui| {
            let over = cache_bytes > crate::world_runtime::CACHE_WARN_BYTES;
            let mb = cache_bytes as f32 / (1024.0 * 1024.0);
            let col = if over { egui::Color32::from_rgb(0xE0, 0x80, 0x30) } else { dark::MUTED };
            ui.colored_label(col, format!("cache {mb:.1} MB  ·  auto-saves every 60s"));
            ui.separator();
            ui.horizontal(|ui| {
                egui::TextEdit::singleline(label).char_limit(24).desired_width(150.0).show(ui);
                if ui.button("Save").clicked() {
                    *out = Some(SaveAction::Save);
                }
            });
            if ui.button("Reset home to defaults").clicked() {
                *out = Some(SaveAction::Reset);
            }
            ui.separator();
            if slots.is_empty() {
                ui.colored_label(dark::MUTED, "no saves yet");
            }
            egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                for s in slots {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(&s.label).strong());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("✕").clicked() {
                                *out = Some(SaveAction::Delete(s.label.clone()));
                            }
                            if ui.small_button("Load").clicked() {
                                *out = Some(SaveAction::Load(s.label.clone()));
                            }
                            ui.colored_label(dark::MUTED, format!("#{}", s.seq.0));
                        });
                    });
                }
            });
        });
}

// the view is a function of the signal cells: read .get(), write .set() on change.
fn draw_panel(
    ctx: &egui::Context,
    state_cell: &Mutable<SimState>,
    tune_cell: &Mutable<Tune>,
    gizmos_cell: &Mutable<bool>,
    net_cell: &Mutable<NetDebug>,
    identity_cell: &Mutable<Identity>,
    charsel_cell: &Mutable<[i64; 2]>,
    camera: Option<Gd<Camera2D>>,
    tab: &mut Tab,
    server_status: &Mutable<String>,
    want_status: &mut bool,
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
          ui.selectable_value(tab, Tab::Server, "server");
      });
      ui.separator();
      let mut show_boxes = gizmos_cell.get();
      if ui.checkbox(&mut show_boxes, "show hitboxes / ECB").changed() {
          gizmos_cell.set(show_boxes);
      }
      ui.separator();
      egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
        match *tab {
        Tab::Status => {
        let p = &s.fighters[0]; // debug panel tracks player 0
        egui::CollapsingHeader::new("status").default_open(false).show(ui, |ui| {
            dark::card(ui, |ui| {
                dark::stat(ui, "state", p.state_name());
                dark::stat(ui, "frame", p.frame.to_string());
                dark::stat(ui, "facing", if p.facing < 0.0 { "◄ left" } else { "right ►" });
                dark::stat(ui, "pos", format!("{:.0}, {:.0}", p.pos.x, p.pos.y));
                dark::stat(ui, "vel", format!("{:.0}, {:.0}", p.vel.x, p.vel.y));
                dark::stat(ui, "air jumps", p.air_jumps.to_string());
                dark::stat(ui, "air dodges", p.air_dodges.to_string());
                dark::stat(ui, "fast fall", p.fast_falling.to_string());
                dark::stat(ui, "intangible", p.intangible.to_string());
                dark::stat(ui, "hitlag", p.hitlag.to_string());
                dark::stat(ui, "aerial buf", p.aerial_buffer_frames().to_string());
                dark::stat(ui, "attack buf", p.attack_buffer_frames().to_string());
                dark::stat(ui, "holding", if p.holding >= 0 { "gun" } else { "—" });
                dark::stat(ui, "autohop", if p.autohop_aerial { "yes (-dmg)" } else { "no" });
                dark::stat(ui, "own %", format!("{:.0}", p.damage));
                dark::stat(ui, "dummy %", format!("{:.0}", s.fighters[1].damage));
            });
        });

        egui::CollapsingHeader::new("input buffer").default_open(false).show(ui, |ui| {
            buffer_card(ui, &s.fighters[0], &t);
        });

        if let Some(mut cam) = camera {
            egui::CollapsingHeader::new("view scale").default_open(false).show(ui, |ui| {
                dark::card(ui, |ui| {
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
            c |= slider(ui, &mut t.dash_turn_accel, 100.0..=12000.0, "dash_turn_accel (reversal brake)");
        });
        egui::CollapsingHeader::new("jump").default_open(false).show(ui, |ui| {
            c |= slider(ui, &mut t.fullhop_v, -2500.0..=-100.0, "fullhop_v");
            c |= slider(ui, &mut t.shorthop_v, -1500.0..=-50.0, "shorthop_v");
            c |= slider(ui, &mut t.airjump_v, -2000.0..=-100.0, "airjump_v");
            c |= slider(ui, &mut t.airjump_h, 0.0..=1500.0, "airjump_h (DJ redirect)");
            c |= slider(ui, &mut t.jump_h_init, 0.0..=1000.0, "jump_h_init");
            c |= slider(ui, &mut t.jump_h_max, 0.0..=2000.0, "jump_h_max");
            c |= slider(ui, &mut t.momentum_carry, 0.0..=1.5, "momentum_carry");
            c |= islider(ui, &mut t.coyote_frames, 0..=12, "coyote_frames (edge grace)");
            c |= islider(ui, &mut t.plat_drop_window, 1..=10, "plat_drop_window (1=instant drop)");
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
        egui::CollapsingHeader::new("attack · dtilt (pothole)").default_open(false).show(ui, |ui| {
            c |= attack_sliders(ui, &mut t.dtilt);
        });
        for (idx, label) in [(0, "neutral-B"), (1, "side-B"), (2, "up-B"), (3, "down-B")] {
            egui::CollapsingHeader::new(format!("special · {label}")).default_open(false).show(
                ui,
                |ui| {
                    let m = &mut t.specials[idx];
                    c |= slider(ui, &mut m.move_x, -1500.0..=1500.0, "move_x (forward)");
                    c |= slider(ui, &mut m.move_y, -2500.0..=1500.0, "move_y (neg=up)");
                    c |= attack_sliders(ui, &mut m.hit);
                },
            );
        }
        egui::CollapsingHeader::new("stroke · default (registry row 0)").default_open(false).show(ui, |ui| {
            c |= slider(ui, &mut t.ink_budget, 100.0..=2000.0, "budget (px of line per pickup)");
            c |= slider(ui, &mut t.ink_spawn_weight, 0.0..=3.0, "spawn weight (0 = never)");
            c |= slider(ui, &mut t.ink_cursor_reach, 40.0..=300.0, "cursor reach (CursorBrush)");
            let d = &mut t.strokes.presets[0];
            let mut stroke_life = d.stroke_life as f32;
            if slider(ui, &mut stroke_life, 30.0..=900.0, "stroke life (frames before it exits)") {
                d.stroke_life = stroke_life as i64;
                c = true;
            }
            c |= slider(ui, &mut d.floor_tol, 0.0..=1.5, "floor tol (rad, ≤ = walkable)");
            c |= slider(ui, &mut d.wall_tol, 0.0..=1.57, "wall tol (rad, ≥ = wall)");
            c |= slider(ui, &mut d.ledge_curve, 0.0..=1.57, "ledge curve (rad corner = grabbable)");
            c |= slider(ui, &mut d.min_seg, 4.0..=60.0, "min segment (px)");
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
        egui::CollapsingHeader::new("items · pickup").default_open(false).show(ui, |ui| {
            c |= slider(ui, &mut t.pickup_reach, 20.0..=300.0, "pickup reach (px forward)");
            c |= slider(ui, &mut t.pickup_r, 10.0..=150.0, "pickup radius (px capsule)");
        });
        egui::CollapsingHeader::new("items · laser").default_open(false).show(ui, |ui| {
            c |= ui.checkbox(&mut t.items_on, "items on").changed();
            c |= ui.checkbox(&mut t.one_item_at_a_time, "one item at a time").changed();
            c |= islider(ui, &mut t.item_spawn_interval, 60..=1800, "spawn interval (f)");
            c |= slider(ui, &mut t.laser.spawn_weight, 0.0..=10.0, "spawn weight");
            c |= islider(ui, &mut t.laser.ammo, 1..=99, "ammo / gun");
            c |= islider(ui, &mut t.laser.cooldown, 1..=60, "tap cooldown (f)");
            c |= islider(ui, &mut t.laser.autofire_cd, 1..=60, "hold cooldown (f)");
            c |= slider(ui, &mut t.laser.autofire_dmg, 0.1..=1.0, "hold dmg x (weaker)");
            c |= slider(ui, &mut t.laser.speed, 200.0..=3000.0, "bolt speed");
            c |= islider(ui, &mut t.laser.range, 10..=240, "bolt range (f)");
            c |= hitbox_sliders(ui, &mut t.laser.hit);
        });

        egui::CollapsingHeader::new("items · bomb (red gun)").default_open(false).show(ui, |ui| {
            c |= slider(ui, &mut t.bomb.spawn_weight, 0.0..=10.0, "spawn weight");
            c |= islider(ui, &mut t.bomb.ammo, 1..=20, "ammo / gun");
            c |= islider(ui, &mut t.bomb.cooldown, 1..=90, "shot cooldown (f)");
            c |= slider(ui, &mut t.bomb.speed, 200.0..=2000.0, "lob speed");
            c |= slider(ui, &mut t.bomb.proj_gravity, 0.0..=6000.0, "lob gravity");
            c |= islider(ui, &mut t.bomb.range, 20..=300, "fuse (f)");
            c |= slider(ui, &mut t.bomb.blast_r, 40.0..=400.0, "blast radius");
            c |= hitbox_sliders(ui, &mut t.bomb.hit);
        });

        egui::CollapsingHeader::new("rules · kills + spawn").default_open(false).show(ui, |ui| {
            c |= slider(ui, &mut t.knockback_mult, 0.5..=3.0, "knockback x (fly more)");
            c |= islider(ui, &mut t.spawn_iframes, 0..=300, "spawn i-frames (f)");
        });

        if c {
            tune_cell.set(t);
        }
        }
        Tab::Net => net_card(ui, &net_cell.get()),
        Tab::Identity => identity_card(ui, identity_cell, charsel_cell),
        Tab::Gamepad => gamepad_card(ui),
        Tab::Server => server_card(ui, server_status, want_status),
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
    dark::card(ui, |ui| {
        dark::stat(ui, "phase", n.phase);
        dark::stat(ui, "role", format!("{} (handle {})", n.role, n.handle));
        dark::stat(ui, "signaling ws", n.ws);
        dark::stat(ui, "pc conn", n.conn);
        dark::stat(ui, "ice gather", n.gather);
        dark::stat(ui, "sdp signal", n.signal);
        dark::stat(ui, "data channel", n.channel);
    });
    ui.add_space(6.0);
    ui.label(egui::RichText::new("handshake frames  (out / in)").color(dark::MUTED));
    dark::card(ui, |ui| {
        dark::stat(ui, "offer", format!("{} / {}", n.offer.0, n.offer.1));
        dark::stat(ui, "answer", format!("{} / {}", n.answer.0, n.answer.1));
        dark::stat(ui, "ice", format!("{} / {}", n.ice.0, n.ice.1));
    });
}

/// Server-status tab: a refresh button that fetches the relay's /status JSON, plus the raw body
/// (build hash/time, uptime, connected/pending, each client's name + color). `want_status` is set
/// when the button is clicked; `process` actuates the fetch (egui can't call Godot mid-draw).
fn server_card(ui: &mut egui::Ui, status: &Mutable<String>, want_status: &mut bool) {
    ui.horizontal(|ui| {
        if ui.button("⟳ refresh").clicked() {
            *want_status = true;
        }
        ui.label(egui::RichText::new(crate::rtc::status_url()).color(dark::MUTED));
    });
    ui.add_space(6.0);
    let body = status.get_cloned();
    dark::card(ui, |ui| {
        for (k, v) in pretty_status(&body) {
            dark::stat(ui, &k, v);
        }
    });
    ui.add_space(6.0);
    ui.collapsing("raw json", |ui| {
        ui.add(egui::Label::new(egui::RichText::new(&body).monospace()).wrap());
    });
}

/// Best-effort flatten of the /status JSON into label rows. No serde in the shell, so this is a
/// forgiving hand parse: top-level "key": value pairs become rows, and the "clients" array is
/// summarized as one row per entry. Falls back to a single "status" row on anything unexpected.
fn pretty_status(body: &str) -> Vec<(String, String)> {
    let trimmed = body.trim();
    if !trimmed.starts_with('{') {
        return vec![("status".to_string(), trimmed.to_string())];
    }
    let mut rows = Vec::new();
    // pull the clients array out first so its commas don't confuse the scalar split.
    let (head, clients) = match trimmed.find("\"clients\"") {
        Some(i) => (&trimmed[..i], &trimmed[i..]),
        None => (trimmed, ""),
    };
    for field in head.trim_matches(|c| c == '{' || c == ',' || c == '}').split(',') {
        let Some((k, v)) = field.split_once(':') else { continue };
        let k = k.trim().trim_matches('"');
        let v = v.trim().trim_matches('"');
        if !k.is_empty() {
            rows.push((k.to_string(), v.to_string()));
        }
    }
    if !clients.is_empty() {
        let n = clients.matches('{').count();
        rows.push(("clients".to_string(), n.to_string()));
        // surface each client's name (and color if present) as its own row.
        for (idx, chunk) in clients.split('{').skip(1).enumerate() {
            let name = json_field(chunk, "name").unwrap_or_else(|| "?".to_string());
            let color = json_field(chunk, "color").unwrap_or_default();
            rows.push((format!("  [{idx}]"), format!("{name} {color}").trim().to_string()));
        }
    }
    rows
}

/// Tiny scalar field extractor: returns the string after `"key":` up to the next comma/brace.
fn json_field(chunk: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\"");
    let i = chunk.find(&pat)? + pat.len();
    let rest = chunk[i..].trim_start().strip_prefix(':')?.trim_start();
    let end = rest.find([',', '}']).unwrap_or(rest.len());
    Some(rest[..end].trim().trim_matches('"').to_string())
}

/// Player identity: name + color the sprite and nametag wear. Edits write back to the shared cell;
/// KneeMan persists them to localStorage (web) and refreshes the tag. The color is godot-side RGBA;
/// the picker works in RGB and rebuilds the color on change.
/// One "◀ name ▶" character cycle for a fighter slot. Writes the picked roster index into the
/// shared charsel cell; KneeMan::sync_charsel applies it to the live sprite next frame. Roster
/// names come from kneeman::roster_names (built-ins + assets/roster.json), so new packs show up here.
fn char_row(ui: &mut egui::Ui, label: &str, slot: usize, charsel: &Mutable<[i64; 2]>) {
    let names = crate::roster::roster_names();
    let n = names.len().max(1) as i64;
    let mut sel = charsel.get_cloned();
    let cur = sel[slot].rem_euclid(n);
    ui.horizontal(|ui| {
        ui.colored_label(dark::MUTED, label);
        if ui.small_button("◀").clicked() {
            sel[slot] = (cur - 1).rem_euclid(n);
            charsel.set(sel);
        }
        let name = names.get(cur as usize).map(String::as_str).unwrap_or("?");
        ui.label(egui::RichText::new(name).strong());
        if ui.small_button("▶").clicked() {
            sel[slot] = (cur + 1).rem_euclid(n);
            charsel.set(sel);
        }
    });
}

fn identity_card(ui: &mut egui::Ui, cell: &Mutable<Identity>, charsel: &Mutable<[i64; 2]>) {
    let mut id = cell.get_cloned();
    let mut changed = false;
    dark::card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.colored_label(dark::MUTED, "name");
            let resp = egui::TextEdit::singleline(&mut id.name)
                .char_limit(16)
                .desired_width(170.0)
                .show(ui)
                .response;
            changed |= resp.changed();
        });
        char_row(ui, "P1 char", 0, charsel);
        char_row(ui, "P2 char", 1, charsel);
        ui.horizontal(|ui| {
            ui.colored_label(dark::MUTED, "color");
            let mut rgb = [id.color.r, id.color.g, id.color.b];
            if ui.color_edit_button_rgb(&mut rgb).changed() {
                id.color = Color::from_rgb(rgb[0], rgb[1], rgb[2]);
                changed = true;
            }
        });
        ui.horizontal(|ui| {
            ui.colored_label(dark::MUTED, "tag size");
            let resp = ui.add(egui::Slider::new(&mut id.font_px, 10..=96).suffix("px"));
            changed |= resp.changed();
        });
    });
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new("saved to this browser · hovers over your fighter")
            .size(11.0)
            .color(dark::MUTED),
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
        dark::card(ui, |ui| {
            ui.colored_label(dark::MUTED, "no controller detected");
        });
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(
                "pair the pad, click the game, then press any button \
                 (browsers hide gamepads until they send input)",
            )
            .size(11.0)
            .color(dark::MUTED),
        );
        return;
    };
    let dev = dev as i32; // Input methods take i32 device ids

    dark::card(ui, |ui| {
        dark::stat(ui, "device", input.get_joy_name(dev).to_string());
    });
    ui.add_space(6.0);

    ui.horizontal(|ui| {
        stick_box(ui, "L stick (move/DI)",
            input.get_joy_axis(dev, JoyAxis::LEFT_X), input.get_joy_axis(dev, JoyAxis::LEFT_Y));
        stick_box(ui, "R stick",
            input.get_joy_axis(dev, JoyAxis::RIGHT_X), input.get_joy_axis(dev, JoyAxis::RIGHT_Y));
    });
    ui.add_space(6.0);

    dark::card(ui, |ui| {
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
    dark::card(ui, |ui| {
        for (b, label) in pips {
            let on = input.is_joy_button_pressed(dev, b);
            ui.horizontal(|ui| {
                let (resp, painter) = ui.allocate_painter(egui::vec2(12.0, 12.0), egui::Sense::hover());
                painter.circle_filled(resp.rect.center(), 5.0, if on { dark::ACCENT } else { dark::LINE });
                ui.colored_label(if on { dark::ACCENT } else { dark::MUTED }, label);
            });
        }
    });
}

/// One analog stick: a circular gate with crosshair and a dot at the stick position (-1..1 each axis).
fn stick_box(ui: &mut egui::Ui, label: &str, x: f32, y: f32) {
    ui.vertical(|ui| {
        ui.colored_label(dark::MUTED, label);
        let (resp, painter) = ui.allocate_painter(egui::vec2(72.0, 72.0), egui::Sense::hover());
        let ctr = resp.rect.center();
        let r = 28.0_f32;
        let grid = egui::Stroke::new(1.0_f32, dark::LINE);
        painter.circle_stroke(ctr, r, grid);
        painter.line_segment([egui::pos2(ctr.x - r, ctr.y), egui::pos2(ctr.x + r, ctr.y)], grid);
        painter.line_segment([egui::pos2(ctr.x, ctr.y - r), egui::pos2(ctr.x, ctr.y + r)], grid);
        let p = egui::pos2(ctr.x + x.clamp(-1.0, 1.0) * r, ctr.y + y.clamp(-1.0, 1.0) * r);
        painter.circle_filled(p, 4.0, dark::ACCENT);
        ui.colored_label(dark::MUTED, format!("{x:+.2}, {y:+.2}"));
    });
}

/// Live readout of the input buffer: what's queued, how long it stays valid, and the captured
/// aim (the diagonal that the air dodge / wavedash will fire with).
fn buffer_card(ui: &mut egui::Ui, f: &Fighter, t: &Tune) {
    dark::card(ui, |ui| {
        let slot = f.move_buffer();
        let active = slot.timer > 0 && slot.action != Action::None;
        let name = slot.action.name();
        let col = if active { dark::ACCENT } else { dark::MUTED };
        ui.horizontal(|ui| {
            ui.colored_label(dark::MUTED, "queued");
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
        let grid = egui::Stroke::new(1.0_f32, dark::LINE);
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
            painter.circle_filled(ctr, 2.5, dark::MUTED);
        }
    });
}

/// One attack's full data table: startup/recovery, then each windowed hitbox. Returns changed.
fn attack_sliders(ui: &mut egui::Ui, a: &mut AttackData) -> bool {
    let mut c = false;
    c |= islider(ui, &mut a.startup, 0..=30, "startup");
    c |= islider(ui, &mut a.recovery, 0..=40, "recovery");
    let nbox = a.nbox as usize;
    for (i, hb) in a.boxes[..nbox].iter_mut().enumerate() {
        ui.label(format!("hitbox {i} (id {})", hb.id));
        c |= hitbox_sliders(ui, hb);
    }
    c
}

/// One windowed hitbox: its frame window, geometry, and community/PM knockback. Returns changed.
fn hitbox_sliders(ui: &mut egui::Ui, hb: &mut Hitbox) -> bool {
    let mut c = false;
    c |= islider(ui, &mut hb.start, 0..=40, "start (f)");
    c |= islider(ui, &mut hb.len, 1..=30, "len (f)");
    c |= slider(ui, &mut hb.off.x, -20.0..=140.0, "off.x (forward)");
    c |= slider(ui, &mut hb.off.y, -130.0..=60.0, "off.y (up = -)");
    c |= slider(ui, &mut hb.r, 6.0..=90.0, "radius");
    c |= slider(ui, &mut hb.damage, 0.0..=40.0, "damage %");
    c |= slider(ui, &mut hb.angle, -120.0..=180.0, "angle° (- = spike)");
    c |= slider(ui, &mut hb.bkb, 0.0..=120.0, "bkb (base)");
    c |= slider(ui, &mut hb.kbg, 0.0..=160.0, "kbg (growth)");
    c
}

fn slider(ui: &mut egui::Ui, val: &mut f32, range: std::ops::RangeInclusive<f32>, label: &str) -> bool {
    ui.add(egui::Slider::new(val, range).text(label)).changed()
}

fn islider(ui: &mut egui::Ui, val: &mut i64, range: std::ops::RangeInclusive<i64>, label: &str) -> bool {
    ui.add(egui::Slider::new(val, range).text(label)).changed()
}
