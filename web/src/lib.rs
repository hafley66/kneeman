//! Browser frontend. Connects two tabs through the matchbox signaling server, runs ggrs rollback
//! over a direct WebRTC channel, and renders the pure `smash_core` sim to a 2D canvas. The whole
//! gameplay path is the same deterministic `step` the SyncTest gates; this crate only adds I/O:
//! keyboard in, canvas out, packets over the wire.
//!
//! Flow: build the matchbox socket -> spawn its message loop -> each animation frame pump
//! `update_peers`; once two players are present build the P2P session; then run a fixed-60Hz
//! rollback loop (sample keys -> add_local_input -> advance_frame -> render).

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use smash_core::{
    active_hitbox, ecb_verts, hurtbox, step, Fighter, InputFrame, SimState, Tune, PLATFORMS,
};
use smash_net::transport::{self, P2PSession, SessionState, WebRtcSocket};
use smash_net::{encode, SmashConfig, SmashGame};

use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{
    CanvasRenderingContext2d, HtmlCanvasElement, HtmlElement, KeyboardEvent, UrlSearchParams,
};

const STEP: f64 = 1.0 / 60.0; // fixed sim timestep (seconds)
const INPUT_DELAY: usize = 2; // frames of local input delay (fewer rollbacks vs. responsiveness)
const SCALE: f64 = 0.5; // world px -> canvas px
const CANVAS_W: u32 = 640;
const CANVAS_H: u32 = 470;
// Signaling room URL, baked at build time. Override per-build with the MATCHBOX_URL env var
// (the justfile passes it); at runtime `?url=` in the page query wins for local testing.
// matchbox uses the URL path as the room id, so keep it a single segment; `?next=2` pairs peers
// two at a time.
const DEFAULT_ROOM_URL: &str = match option_env!("MATCHBOX_URL") {
    Some(u) => u,
    None => "wss://hafley.codes/ws?next=2",
};

/// Keyboard state. `held` = currently-down physical keys (`KeyboardEvent.code`); `edges` = keys
/// that went down since the last `sample()`, captured in the keydown handler so a fast tap between
/// two animation frames is never missed (the bug a frame-diff approach has).
#[derive(Default)]
struct Keys {
    held: HashSet<String>,
    edges: HashSet<String>,
}

impl Keys {
    /// keydown: mark held, and record a rising edge if it wasn't already down.
    fn key_down(&mut self, code: String) {
        if self.held.insert(code.clone()) {
            self.edges.insert(code);
        }
    }

    fn key_up(&mut self, code: &str) {
        self.held.remove(code);
    }

    /// Sample the keyboard into a sim input, then drain the edge set. WASD/arrows move, Space/W
    /// jump, Shift shield, J attack, C short-hop.
    fn sample(&mut self) -> InputFrame {
        let held = |c: &str| self.held.contains(c);
        let hit = |c: &str| self.edges.contains(c);
        let any_held = |cs: &[&str]| cs.iter().any(|c| held(c));
        let any_hit = |cs: &[&str]| cs.iter().any(|c| hit(c));

        let right = held("KeyD") || held("ArrowRight");
        let left = held("KeyA") || held("ArrowLeft");
        let dn = held("KeyS") || held("ArrowDown");
        let up = held("KeyW") || held("ArrowUp");

        let frame = InputFrame {
            dir: (right as i32 - left as i32) as f32,
            aim_y: (dn as i32 - up as i32) as f32, // -1 up .. +1 down
            jump: any_hit(&["Space", "ArrowUp", "KeyW"]),
            jump_held: any_held(&["Space", "ArrowUp", "KeyW"]),
            shorthop: hit("KeyC"),
            shield_held: any_held(&["ShiftLeft", "ShiftRight", "KeyL"]),
            shield_pressed: any_hit(&["ShiftLeft", "ShiftRight", "KeyL"]),
            down: dn,
            down_pressed: any_hit(&["KeyS", "ArrowDown"]),
            attack: any_hit(&["KeyJ", "KeyF"]),
            attack_held: any_held(&["KeyJ", "KeyF"]),
            grab: any_hit(&["KeyK"]),
            special: any_hit(&["KeyH"]),
        };
        self.edges.clear();
        frame
    }
}

/// Zero the one-shot (rising-edge) fields so a press applies to exactly one sim frame even when a
/// tick advances several frames. Held state (dir/aim_y/jump_held/shield_held/down) survives.
fn clear_edges(i: &mut InputFrame) {
    i.jump = false;
    i.shorthop = false;
    i.shield_pressed = false;
    i.down_pressed = false;
    i.attack = false;
}

/// Whole app state, owned by an `Rc<RefCell<_>>` the animation-frame closure re-enters each tick.
/// `socket`/`session` are `None` in solo practice mode (no matchbox, no opponent — local sim only).
struct App {
    solo: bool,
    socket: Option<WebRtcSocket>,
    session: Option<P2PSession<SmashConfig>>,
    game: SmashGame,
    keys: Rc<RefCell<Keys>>,
    ctx: CanvasRenderingContext2d,
    status: HtmlElement,
    last_ms: f64,
    acc: f64,
}

#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    let _ = console_log::init_with_level(log::Level::Info);

    let document = web_sys::window().unwrap().document().unwrap();
    let canvas: HtmlCanvasElement = document
        .get_element_by_id("game")
        .expect("a <canvas id=\"game\"> must exist")
        .dyn_into()
        .unwrap();
    canvas.set_width(CANVAS_W);
    canvas.set_height(CANVAS_H);
    let ctx: CanvasRenderingContext2d = canvas
        .get_context("2d")
        .unwrap()
        .unwrap()
        .dyn_into()
        .unwrap();
    let status: HtmlElement = document
        .get_element_by_id("status")
        .expect("a #status element must exist")
        .dyn_into()
        .unwrap();

    let keys = Rc::new(RefCell::new(Keys::default()));
    install_key_listeners(&keys);

    // Solo practice (`?solo` in the page query): no matchbox, single-player local sim so one tab
    // runs + responds immediately. Otherwise connect to the signaling server and wait for a peer.
    let solo = query_flag("solo");
    let socket = if solo {
        status.set_inner_text("solo practice — WASD move · Space jump · J attack · Shift shield");
        None
    } else {
        let url = room_url();
        log::info!("connecting to {url}");
        status.set_inner_text(&format!(
            "connecting… open this URL in a second tab to pair\n{url}"
        ));
        let (socket, loop_fut) = transport::connect(&url);
        wasm_bindgen_futures::spawn_local(async move {
            // Resolves only when the socket closes; an error here just means the loop ended.
            let _ = loop_fut.await;
            log::warn!("matchbox message loop ended");
        });
        Some(socket)
    };

    let app = Rc::new(RefCell::new(App {
        solo,
        socket,
        session: None,
        game: SmashGame::new(Tune::default()),
        keys,
        ctx,
        status,
        last_ms: now_ms(),
        acc: 0.0,
    }));

    run_animation_loop(app);
}

/// True if `?<name>` is present in the page query (value ignored).
fn query_flag(name: &str) -> bool {
    let search = web_sys::window().unwrap().location().search().unwrap_or_default();
    UrlSearchParams::new_with_str(&search)
        .map(|p| p.has(name))
        .unwrap_or(false)
}

/// The matchbox room URL. Defaults to the live signaling server; `?url=` overrides the whole thing
/// (e.g. `?url=ws://localhost:3536/x?next=2` against a locally-run matchbox_server).
fn room_url() -> String {
    let loc = web_sys::window().unwrap().location();
    let search = loc.search().unwrap_or_default();
    if let Ok(params) = UrlSearchParams::new_with_str(&search) {
        if let Some(u) = params.get("url") {
            return u;
        }
    }
    DEFAULT_ROOM_URL.to_string()
}

fn install_key_listeners(keys: &Rc<RefCell<Keys>>) {
    let window = web_sys::window().unwrap();

    let down = keys.clone();
    let on_down = Closure::<dyn FnMut(KeyboardEvent)>::new(move |e: KeyboardEvent| {
        if !e.repeat() {
            down.borrow_mut().key_down(e.code());
        }
        // Stop Space/arrows from scrolling the page while playing.
        if matches!(e.code().as_str(), "Space" | "ArrowUp" | "ArrowDown" | "ArrowLeft" | "ArrowRight") {
            e.prevent_default();
        }
    });
    window
        .add_event_listener_with_callback("keydown", on_down.as_ref().unchecked_ref())
        .unwrap();
    on_down.forget();

    let up = keys.clone();
    let on_up = Closure::<dyn FnMut(KeyboardEvent)>::new(move |e: KeyboardEvent| {
        up.borrow_mut().key_up(&e.code());
    });
    window
        .add_event_listener_with_callback("keyup", on_up.as_ref().unchecked_ref())
        .unwrap();
    on_up.forget();
}

/// Standard wasm requestAnimationFrame trampoline: a closure that reschedules itself.
fn run_animation_loop(app: Rc<RefCell<App>>) {
    let f = Rc::new(RefCell::new(None::<Closure<dyn FnMut()>>));
    let g = f.clone();
    *g.borrow_mut() = Some(Closure::new(move || {
        tick(&app);
        request_animation_frame(f.borrow().as_ref().unwrap());
    }));
    request_animation_frame(g.borrow().as_ref().unwrap());
}

fn tick(app: &Rc<RefCell<App>>) {
    let mut app = app.borrow_mut();

    // Solo practice: drive the local sim, no networking.
    if app.solo {
        step_solo(&mut app);
        render(&app);
        return;
    }

    // 1. Pump matchbox: process signaling + (dis)connections.
    let _ = app.socket.as_mut().unwrap().update_peers();

    // 2. No session yet -> wait for the second player, then build the rollback session.
    if app.session.is_none() {
        let players = transport::players(app.socket.as_mut().unwrap());
        if players.len() >= 2 {
            match app.socket.as_mut().unwrap().take_channel(0) {
                Ok(channel) => match transport::start_session(players, channel, INPUT_DELAY) {
                    Ok(sess) => {
                        log::info!("opponent connected — session started");
                        app.status.set_inner_text("opponent connected");
                        app.game.state = SimState::spawn();
                        app.session = Some(sess);
                        app.last_ms = now_ms();
                        app.acc = 0.0;
                    }
                    Err(e) => log::error!("start_session: {e:?}"),
                },
                Err(e) => log::error!("take_channel: {e:?}"),
            }
        }
    }

    // 3. Drive the fixed-timestep rollback loop.
    if app.session.is_some() {
        step_session(&mut app);
    }

    // 4. Render the latest state regardless of phase.
    render(&app);
}

/// Single-player local sim: fixed-timestep `step` with your input on fighter 0, a neutral input on
/// fighter 1. No rollback, no networking — just enough to move around and feel the physics.
fn step_solo(app: &mut App) {
    let now = now_ms();
    app.acc += ((now - app.last_ms) / 1000.0).min(0.25);
    app.last_ms = now;

    let mut input = app.keys.borrow_mut().sample();
    let idle = InputFrame::default();
    let mut first = true;
    while app.acc >= STEP {
        app.game.state = step(&app.game.state, &[&input, &idle], &app.game.cfg);
        app.acc -= STEP;
        if first {
            clear_edges(&mut input); // a press counts for one frame only
            first = false;
        }
    }
}

fn step_session(app: &mut App) {
    let now = now_ms();
    app.acc += ((now - app.last_ms) / 1000.0).min(0.25); // clamp to avoid a spiral of death
    app.last_ms = now;

    let session = app.session.as_mut().unwrap();
    session.poll_remote_clients();
    for event in session.events() {
        log::info!("ggrs event: {event:?}");
    }

    if session.current_state() != SessionState::Running {
        app.acc = 0.0; // still synchronizing; don't bank time
        return;
    }

    let mut input = app.keys.borrow_mut().sample();
    let mut first = true;
    while app.acc >= STEP {
        let net = encode(&input);
        for handle in session.local_player_handles() {
            if let Err(e) = session.add_local_input(handle, net) {
                log::error!("add_local_input: {e:?}");
            }
        }
        match session.advance_frame() {
            Ok(requests) => app.game.handle(requests),
            Err(transport::GgrsError::PredictionThreshold) => break, // too far ahead; wait for peer
            Err(e) => {
                log::error!("advance_frame: {e:?}");
                break;
            }
        }
        app.acc -= STEP;
        if first {
            clear_edges(&mut input);
            first = false;
        }
    }
}

// --- rendering -------------------------------------------------------------------------------

fn render(app: &App) {
    let ctx = &app.ctx;
    ctx.set_fill_style_str("#0d0f17");
    ctx.fill_rect(0.0, 0.0, CANVAS_W as f64, CANVAS_H as f64);

    // Platforms: solid main stage thick + white, soft platforms thin + gray.
    for p in PLATFORMS.iter() {
        ctx.set_stroke_style_str(if p.solid { "#9fb3c8" } else { "#41506b" });
        ctx.set_line_width(if p.solid { 4.0 } else { 2.0 });
        ctx.begin_path();
        ctx.move_to(p.left as f64 * SCALE, p.y as f64 * SCALE);
        ctx.line_to(p.right as f64 * SCALE, p.y as f64 * SCALE);
        ctx.stroke();
    }

    let s = &app.game.state;
    let local = if app.solo {
        Some(0) // you control fighter 0 in solo practice
    } else {
        app.session
            .as_ref()
            .and_then(|sess| sess.local_player_handles().first().copied())
    };
    let colors = ["#5ad1ff", "#ff7a7a", "#9affc0", "#ffd166"];
    for (i, f) in s.fighters.iter().take(s.active as usize).enumerate() {
        draw_fighter(ctx, f, colors[i], local == Some(i), &app.game.cfg);
    }

    // HUD: both damage percentages.
    ctx.set_font("16px monospace");
    ctx.set_fill_style_str(colors[0]);
    let _ = ctx.fill_text(&format!("P1 {:.0}%", s.fighters[0].damage), 16.0, 24.0);
    ctx.set_fill_style_str(colors[1]);
    let _ = ctx.fill_text(
        &format!("P2 {:.0}%", s.fighters[1].damage),
        CANVAS_W as f64 - 90.0,
        24.0,
    );
}

fn draw_fighter(ctx: &CanvasRenderingContext2d, f: &Fighter, color: &str, is_local: bool, t: &Tune) {
    // ECB diamond.
    let verts = ecb_verts(f.pos);
    ctx.set_stroke_style_str(color);
    ctx.set_line_width(if is_local { 3.0 } else { 1.5 });
    ctx.begin_path();
    ctx.move_to(verts[0].x as f64 * SCALE, verts[0].y as f64 * SCALE);
    for v in &verts[1..] {
        ctx.line_to(v.x as f64 * SCALE, v.y as f64 * SCALE);
    }
    ctx.close_path();
    ctx.stroke();

    // Hurtbox circle.
    let (hc, hr) = hurtbox(f);
    ctx.begin_path();
    let _ = ctx.arc(
        hc.x as f64 * SCALE,
        hc.y as f64 * SCALE,
        hr as f64 * SCALE,
        0.0,
        std::f64::consts::TAU,
    );
    ctx.stroke();

    // Active hitbox (if swinging): translucent red fill.
    if let Some((c, r)) = active_hitbox(f, t) {
        ctx.set_fill_style_str("rgba(255,64,64,0.45)");
        ctx.begin_path();
        let _ = ctx.arc(
            c.x as f64 * SCALE,
            c.y as f64 * SCALE,
            r as f64 * SCALE,
            0.0,
            std::f64::consts::TAU,
        );
        ctx.fill();
    }
}

// --- small wasm helpers ----------------------------------------------------------------------

fn now_ms() -> f64 {
    web_sys::window().unwrap().performance().unwrap().now()
}

fn request_animation_frame(f: &Closure<dyn FnMut()>) {
    web_sys::window()
        .unwrap()
        .request_animation_frame(f.as_ref().unchecked_ref())
        .expect("requestAnimationFrame");
}
