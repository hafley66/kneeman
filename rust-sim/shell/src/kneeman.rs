use futures_signals::signal::Mutable;
use godot::classes::web_rtc_data_channel::ChannelState;
use godot::classes::web_rtc_peer_connection::{ConnectionState, GatheringState, SignalingState};
use godot::classes::web_socket_peer::State as WsState;
use godot::classes::{
    AnimatedSprite2D, AtlasTexture, CanvasLayer, ColorRect, INode2D, Input, InputEvent,
    InputEventKey, Label, Node2D, SpriteFrames, Texture2D, WebRtcDataChannel, WebRtcPeerConnection,
    WebSocketPeer,
};
use godot::global::Key;
use godot::prelude::*;
use godot::tools::load;

use crate::rtc::{self, Role, RtcSocket};
use crate::sim::{self, CharState, Fighter, InputFrame, SimState, Tune};
use smash_net::{encode, start_p2p, Game, GgrsConfig, GgrsError, P2PSession, SessionState};

/// Netplay lifecycle. Offline = local single-player (default). Signaling = dialing the relay +
/// doing the WebRTC handshake; still renders local play so the page isn't frozen. Running = ggrs
/// rollback drives the sim from both peers' inputs.
#[derive(Copy, Clone, PartialEq, Eq)]
enum Phase {
    Offline,
    Signaling,
    Running,
}

/// Read-only snapshot of the netplay transport machine for the debug panel. Every field is a
/// `&'static str` (Copy) so it rides a `Mutable` cheaply; the human names are resolved in the shell
/// (here) where the godot WebRTC enums are in scope. Counts are (sent, received) for each handshake
/// frame kind so a stall shows where it stuck (offer out but no answer in = guest/relay drop, etc).
#[derive(Clone, Copy)]
pub struct NetDebug {
    pub phase: &'static str,
    pub role: &'static str,
    pub handle: usize,
    pub ws: &'static str,      // signaling socket
    pub conn: &'static str,    // RTCPeerConnection.connectionState
    pub gather: &'static str,  // ICE gathering
    pub signal: &'static str,  // SDP signaling
    pub channel: &'static str, // ggrs data channel
    pub offer: (u32, u32),
    pub answer: (u32, u32),
    pub ice: (u32, u32),
}

impl Default for NetDebug {
    fn default() -> Self {
        Self {
            phase: "offline",
            role: "—",
            handle: 0,
            ws: "—",
            conn: "—",
            gather: "—",
            signal: "—",
            channel: "—",
            offer: (0, 0),
            answer: (0, 0),
            ice: (0, 0),
        }
    }
}

/// Signaling-frame tallies kept on the node; folded into `NetDebug` each frame.
#[derive(Default, Clone, Copy)]
struct SigCounts {
    offer_out: u32,
    offer_in: u32,
    answer_out: u32,
    answer_in: u32,
    ice_out: u32,
    ice_in: u32,
}

/// Local player presentation: the nametag text + color the sprite and tag wear. NOT sim state
/// (purely cosmetic, never rolled back). Lives on the node as a `Mutable` so the debug panel's
/// Identity tab edits it and the renderer reads it. Persisted to/from browser localStorage on the
/// web build; defaults on desktop.
#[derive(Clone, PartialEq)]
pub struct Identity {
    pub name: String,
    pub color: Color,
    pub font_px: i32, // nametag font size (HUD-wide; both tags share the local player's setting)
}

impl Default for Identity {
    fn default() -> Self {
        Self { name: "Player".into(), color: Color::from_rgb(0.35, 0.75, 1.0), font_px: 32 }
    }
}

/// Stable default for the remote slot until Cut 2 trades the real identity over the signaling ws.
fn p2_identity() -> Identity {
    Identity { name: "P2".into(), color: Color::from_rgb(1.0, 0.55, 0.35), font_px: 32 }
}

/// Cap the name length and trim; cosmetic only (the tag and the saved file both show this).
fn sanitize_name(s: &str) -> String {
    let t: String = s.chars().take(16).collect();
    let t = t.trim();
    if t.is_empty() { "Player".into() } else { t.to_string() }
}

/// Identity persistence. `user://` is Godot's per-user store: a real file on native, IndexedDB on
/// the web export — Godot bridges the platform difference, so one code path covers both (no
/// `JavaScriptBridge`, no platform `cfg`). `ConfigFile` serializes Variants, so the `Color` round-
/// trips natively without any hex conversion.
const IDENTITY_PATH: &str = "user://identity.cfg";

fn load_identity() -> Identity {
    let mut id = Identity::default();
    let mut cfg = godot::classes::ConfigFile::new_gd();
    if cfg.load(IDENTITY_PATH) != godot::global::Error::OK {
        return id;
    }
    if let Ok(g) = cfg.get_value("player", "name").try_to::<GString>() {
        let s = g.to_string();
        if !s.is_empty() {
            id.name = sanitize_name(&s);
        }
    }
    if let Ok(c) = cfg.get_value("player", "color").try_to::<Color>() {
        id.color = c;
    }
    if let Ok(px) = cfg.get_value("player", "font_px").try_to::<i64>() {
        id.font_px = (px as i32).clamp(10, 96);
    }
    id
}

fn save_identity(id: &Identity) {
    let mut cfg = godot::classes::ConfigFile::new_gd();
    let _ = cfg.load(IDENTITY_PATH); // keep any other keys already on disk
    cfg.set_value("player", "name", &GString::from(sanitize_name(&id.name).as_str()).to_variant());
    cfg.set_value("player", "color", &id.color.to_variant());
    cfg.set_value("player", "font_px", &(id.font_px as i64).to_variant());
    cfg.save(IDENTITY_PATH);
}

/// CC0 "Pixel Adventure" character (Pixel Frog). Each animation is one horizontal strip of
/// 32x32 frames; we slice them into SpriteFrames clips. Swap to any sibling under
/// assets/pixelfrog ("ninjafrog", "maskdude", "pinkman", "virtualguy") — same strips/counts.
/// Test art only — CC0, not a shipping character.
// Character roster. Cosmetic only -- a character's art is never folded into smash_net::checksum,
// so adding or reordering the roster cannot desync a netplay session. Each fighter slot holds an
// index into ROSTER (KneeMan::characters); char-select (later) just writes those indices.
struct Character {
    dir: &'static str,        // asset subdir under res://assets/
    scale: f32,               // node scale so the art lands ~105px tall (near the ECB height)
    offset_y: f32,            // sprite offset (texture px) so the feet sit on pos
    sheet: Sheet,             // how this character's PNGs are laid out on disk
    clips: &'static [Clip],   // one per CharState clip name (see clip_for)
}

/// How a character's frames are stored.
enum Sheet {
    /// One horizontal strip per clip, sliced into `frame_px` square cells. File = `<clip.files[0]>.png`.
    Strip { frame_px: f32 },
    /// One whole PNG per pose, named `<prefix>_<file>.png`. Each entry in `clip.files` is one frame.
    Poses { prefix: &'static str },
}

/// One animation clip. For Strip, `files` holds the single strip name and `frames` is the cell
/// count; for Poses, `files` is the per-frame pose list and `frames` is ignored.
struct Clip {
    name: &'static str,
    files: &'static [&'static str],
    frames: i32,
    fps: f64,
    looped: bool,
}

const ROSTER: &[Character] = &[FROG, ZOMBIE];

/// P1 default: the Kenney/PixelFrog ninja frog (32px strips).
const FROG: Character = Character {
    dir: "pixelfrog/ninjafrog",
    scale: 4.4, // 32px art -> ~140px tall, matching the ECB body
    offset_y: -12.0,
    sheet: Sheet::Strip { frame_px: 32.0 },
    clips: &[
        Clip { name: "idle", files: &["idle"], frames: 11, fps: 14.0, looped: true },
        Clip { name: "walk", files: &["run"], frames: 12, fps: 14.0, looped: true },
        Clip { name: "run", files: &["run"], frames: 12, fps: 20.0, looped: true },
        Clip { name: "crouch", files: &["fall"], frames: 1, fps: 1.0, looped: false },
        Clip { name: "skid", files: &["fall"], frames: 1, fps: 1.0, looped: false },
        Clip { name: "jump", files: &["jump"], frames: 1, fps: 1.0, looped: false },
        Clip { name: "fall", files: &["fall"], frames: 1, fps: 1.0, looped: false },
        Clip { name: "hang", files: &["wall_jump"], frames: 5, fps: 12.0, looped: true },
        Clip { name: "climb", files: &["double_jump"], frames: 6, fps: 14.0, looped: true },
        Clip { name: "jab", files: &["hit"], frames: 7, fps: 20.0, looped: false },
        Clip { name: "nair", files: &["double_jump"], frames: 6, fps: 18.0, looped: true },
    ],
};

/// P2 default: the Kenney zombie (80x110 single-pose PNGs). Different silhouette from the frog.
const ZOMBIE: Character = Character {
    dir: "kenney/zombie",
    scale: 1.27, // 110px art -> ~140px tall, matching the ECB body
    offset_y: -55.0,
    sheet: Sheet::Poses { prefix: "zombie" },
    clips: &[
        Clip { name: "idle", files: &["idle"], frames: 1, fps: 1.0, looped: false },
        Clip { name: "walk", files: &["walk1", "walk2"], frames: 2, fps: 8.0, looped: true },
        Clip { name: "run", files: &["walk1", "walk2"], frames: 2, fps: 13.0, looped: true },
        Clip { name: "skid", files: &["skid"], frames: 1, fps: 1.0, looped: false },
        Clip { name: "crouch", files: &["duck"], frames: 1, fps: 1.0, looped: false },
        Clip { name: "jump", files: &["jump"], frames: 1, fps: 1.0, looped: false },
        Clip { name: "fall", files: &["fall"], frames: 1, fps: 1.0, looped: false },
        Clip { name: "hang", files: &["hang"], frames: 1, fps: 1.0, looped: false },
        Clip { name: "climb", files: &["climb1", "climb2"], frames: 2, fps: 8.0, looped: true },
        Clip { name: "jab", files: &["action1"], frames: 1, fps: 1.0, looped: false },
        Clip { name: "nair", files: &["kick"], frames: 1, fps: 1.0, looped: false },
    ],
};

/// Boundary: the pure sim speaks glam::Vec2; Godot wants its own Vector2. Convert on the way out.
#[inline]
fn gv(v: sim::Vector2) -> Vector2 {
    Vector2::new(v.x, v.y)
}

// --- transport-state names for the debug panel. gdext models these as newtype structs (not real
// enums), so resolve by `==` rather than match patterns. ----------------------------------------
fn ws_name(s: WsState) -> &'static str {
    if s == WsState::CONNECTING {
        "connecting"
    } else if s == WsState::OPEN {
        "open"
    } else if s == WsState::CLOSING {
        "closing"
    } else {
        "closed"
    }
}
fn chan_name(s: ChannelState) -> &'static str {
    if s == ChannelState::CONNECTING {
        "connecting"
    } else if s == ChannelState::OPEN {
        "open"
    } else if s == ChannelState::CLOSING {
        "closing"
    } else {
        "closed"
    }
}
fn conn_name(s: ConnectionState) -> &'static str {
    if s == ConnectionState::NEW {
        "new"
    } else if s == ConnectionState::CONNECTING {
        "connecting"
    } else if s == ConnectionState::CONNECTED {
        "connected"
    } else if s == ConnectionState::DISCONNECTED {
        "disconnected"
    } else if s == ConnectionState::FAILED {
        "failed"
    } else {
        "closed"
    }
}
fn gather_name(s: GatheringState) -> &'static str {
    if s == GatheringState::NEW {
        "new"
    } else if s == GatheringState::GATHERING {
        "gathering"
    } else {
        "complete"
    }
}
fn signal_name(s: SignalingState) -> &'static str {
    if s == SignalingState::STABLE {
        "stable"
    } else if s == SignalingState::HAVE_LOCAL_OFFER {
        "have-local-offer"
    } else if s == SignalingState::HAVE_REMOTE_OFFER {
        "have-remote-offer"
    } else if s == SignalingState::HAVE_LOCAL_PRANSWER {
        "have-local-pranswer"
    } else if s == SignalingState::HAVE_REMOTE_PRANSWER {
        "have-remote-pranswer"
    } else {
        "closed"
    }
}

/// KneeMan: the impure SHELL around the pure sim.
/// Owns the BehaviorSubjects (state + tune). Each tick: sample input -> pure step ->
/// publish into the state cell -> render (position + sprite clip). That is the only
/// place effects live; `sim::step` itself is pure.
#[derive(GodotClass)]
#[class(base = Node2D)]
pub struct KneeMan {
    base: Base<Node2D>,
    state: Mutable<SimState>, // source of truth, observed everywhere
    tune: Mutable<Tune>,      // live config, written by egui
    anim: Option<Gd<AnimatedSprite2D>>, // P1 sprite, driven by CharState
    dummy: Option<Gd<ColorRect>>,       // legacy P2 block (hidden once the P2 sprite exists)
    p2: Option<Gd<AnimatedSprite2D>>,   // P2 sprite (world-space sibling), driven by fighters[1]
    tag_p1: Option<Gd<Label>>,          // world-space nametag hovering over P1
    tag_p2: Option<Gd<Label>>,          // world-space nametag hovering over P2
    status: Option<Gd<Label>>,          // screen-space netplay status line (built in ready)
    netdbg: Mutable<NetDebug>,          // transport snapshot, read by the debug panel
    sig: SigCounts,                     // handshake-frame tallies feeding netdbg
    identity: Mutable<Identity>,        // local player name+color, edited by the panel, persisted
    saved_identity: Identity,           // last value written to localStorage (change detection)
    characters: [usize; 2],             // per-slot index into ROSTER; char-select writes these later

    // --- netplay (Godot WebRTC). All None/Offline until the player joins a match (Enter). ---
    phase: Phase,
    role: Option<Role>,
    local_handle: usize,                       // ggrs handle for this peer (host 0 / guest 1)
    ws: Option<Gd<WebSocketPeer>>,             // signaling socket to the relay
    pc: Option<Gd<WebRtcPeerConnection>>,      // the P2P connection
    channel: Option<Gd<WebRtcDataChannel>>,    // negotiated data channel ggrs rides
    session: Option<P2PSession<GgrsConfig>>,   // rollback session (once the channel opens)
    game: Option<Game>,                        // ggrs-authoritative sim state during a match
}

#[godot_api]
impl INode2D for KneeMan {
    fn init(base: Base<Node2D>) -> Self {
        Self {
            base,
            state: Mutable::new(SimState::spawn()),
            tune: Mutable::new(Tune::default()),
            anim: None,
            dummy: None,
            p2: None,
            tag_p1: None,
            tag_p2: None,
            status: None,
            netdbg: Mutable::new(NetDebug::default()),
            sig: SigCounts::default(),
            identity: Mutable::new(Identity::default()),
            saved_identity: Identity::default(),
            characters: [0, 1], // P1 frog, P2 zombie until char-select sets them
            phase: Phase::Offline,
            role: None,
            local_handle: 0,
            ws: None,
            pc: None,
            channel: None,
            session: None,
            game: None,
        }
    }

    fn ready(&mut self) {
        let pos = self.state.get().fighters[0].pos;
        self.base_mut().set_position(gv(pos));

        // Resolve the AnimatedSprite2D and hand it a SpriteFrames built in code
        // (one clip per movement state). Building it here, not in a .tres, keeps the
        // CharState->clip wiring readable and avoids hand-authoring resource uids.
        if let Some(mut a) = self
            .base()
            .get_node_or_null("Anim")
            .and_then(|n| n.try_cast::<AnimatedSprite2D>().ok())
        {
            let c = &ROSTER[self.characters[0]];
            a.set_sprite_frames(&build_frames(c));
            a.set_scale(Vector2::splat(c.scale));
            a.set_offset(Vector2::new(0.0, c.offset_y)); // feet on pos
            a.set_texture_filter(godot::classes::canvas_item::TextureFilter::NEAREST); // crisp pixels
            a.play_ex().name("idle").done();
            self.anim = Some(a);
        }

        // Legacy P2 block: hide it, the P2 sprite replaces it.
        self.dummy = self
            .base()
            .get_node_or_null("../Dummy")
            .and_then(|n| n.try_cast::<ColorRect>().ok());
        if let Some(d) = self.dummy.as_mut() {
            d.set_visible(false);
        }

        // Load the saved identity (web) before building tags so P1's tag wears the right name/color.
        let id = load_identity();
        self.identity.set(id.clone());
        self.saved_identity = id.clone();

        // P2 as a real sprite (world-space sibling under the parent, positioned each frame).
        let c2 = &ROSTER[self.characters[1]];
        let mut p2 = AnimatedSprite2D::new_alloc();
        p2.set_sprite_frames(&build_frames(c2));
        p2.set_scale(Vector2::splat(c2.scale));
        p2.set_offset(Vector2::new(0.0, c2.offset_y));
        p2.set_texture_filter(godot::classes::canvas_item::TextureFilter::NEAREST);
        p2.play_ex().name("idle").done();
        // Nametags: world-space labels that hover over each head, wearing the player's color.
        let tag_p1 = make_tag(&id.name, id.color, id.font_px);
        let tag_p2 = make_tag(&p2_identity().name, p2_identity().color, id.font_px);
        // Add as world-space siblings under our parent. Deferred: during ready() the parent is still
        // "busy setting up children", so an immediate add_child is rejected (re-entrant child setup).
        // call_deferred runs it the moment setup finishes, before the first frame draws.
        if let Some(mut parent) = self.base().get_parent() {
            parent.call_deferred("add_child", &[p2.to_variant()]);
            parent.call_deferred("add_child", &[tag_p1.to_variant()]);
            parent.call_deferred("add_child", &[tag_p2.to_variant()]);
        }
        self.p2 = Some(p2);
        self.tag_p1 = Some(tag_p1);
        self.tag_p2 = Some(tag_p2);

        // Always-on netplay status line. A CanvasLayer pins it to the screen (not the world), so it
        // stays put as the camera tracks the fighter. Built in code to avoid a hand-authored scene.
        let mut layer = CanvasLayer::new_alloc();
        let mut label = Label::new_alloc();
        label.set_position(Vector2::new(14.0, 10.0));
        label.add_theme_font_size_override("font_size", 20);
        label.add_theme_color_override("font_color", Color::from_rgb(0.92, 0.96, 1.0));
        // Dark rounded chip behind the text so it reads on the white stage (clear color is white).
        let mut bg = godot::classes::StyleBoxFlat::new_gd();
        bg.set_bg_color(Color::from_rgba(0.07, 0.09, 0.14, 0.85));
        bg.set_corner_radius_all(6);
        bg.set_content_margin_all(8.0);
        label.add_theme_stylebox_override("normal", &bg);
        layer.add_child(&label);
        self.base_mut().add_child(&layer);
        self.status = Some(label);
        self.update_status();
    }

    fn physics_process(&mut self, _delta: f64) {
        match self.phase {
            Phase::Offline => self.step_local(),
            // Keep rendering local play while the WebRTC handshake completes, then flip to rollback.
            Phase::Signaling => {
                self.pump_signaling();
                if self.phase != Phase::Running {
                    self.step_local();
                }
            }
            Phase::Running => self.step_net(),
        }
        self.update_status();
        self.publish_netdbg();
        self.sync_identity();
        self.place_tags();
    }

    /// Enter joins a match (only from Offline). The relay pairs two dialers; see `start_matchmaking`.
    fn input(&mut self, event: Gd<InputEvent>) {
        let Ok(key) = event.try_cast::<InputEventKey>() else {
            return;
        };
        if !key.is_pressed() || key.is_echo() {
            return;
        }
        if key.get_keycode() == Key::ENTER && self.phase == Phase::Offline {
            self.start_matchmaking();
        }
    }

    /// Debug overlay: each fighter's ECB (cyan), hurtbox (yellow), and active hitbox (red).
    /// Drawn for both players so P2's attacks show their boxes too. Coordinates are world,
    /// converted to this node's local space (the node sits at the player position).
    fn draw(&mut self) {
        let s = self.state.get();
        let t = self.tune.get();
        let origin = self.base().get_position();

        let ecb = Color::from_rgba(0.20, 0.85, 0.95, 0.85);
        let hurt_col = Color::from_rgba(0.95, 0.85, 0.20, 0.30);
        let hit_col = Color::from_rgba(0.95, 0.25, 0.25, 0.45);
        for f in &s.fighters {
            // ECB diamond: the actual collision shape — bottom vert = feet, side verts = walls.
            let v = sim::ecb_verts(f.pos);
            for k in 0..4 {
                let a = gv(v[k]) - origin;
                let b = gv(v[(k + 1) % 4]) - origin;
                self.base_mut().draw_line_ex(a, b, ecb).width(2.0).done();
            }
            // hurtbox: the circle an attack lands on.
            let (bc, br) = sim::hurtbox(f);
            let hurt = gv(bc) - origin;
            self.base_mut().draw_circle(hurt, br, hurt_col);
            // active hitbox: present only while this fighter's attack is live.
            if let Some((hc, hr)) = sim::active_hitbox(f, &t) {
                let c = gv(hc) - origin;
                self.base_mut().draw_circle(c, hr, hit_col);
            }
        }

        // items + projectiles (debug shapes for now; model_id -> sprite is the later polish)
        for it in &s.items {
            if !it.active() {
                continue;
            }
            let c = gv(it.pos) - origin;
            match it.kind {
                sim::ItemKind::LaserGun => {
                    let size = Vector2::new(38.0, 16.0);
                    self.base_mut().draw_rect(
                        Rect2::new(c - size * 0.5, size),
                        Color::from_rgb(0.25, 0.95, 0.45),
                    );
                }
                sim::ItemKind::LaserBolt => {
                    let half = Vector2::new(20.0 * it.facing, 0.0);
                    self.base_mut()
                        .draw_line_ex(c - half, c + half, Color::from_rgb(1.0, 0.25, 0.20))
                        .width(6.0)
                        .done();
                }
                sim::ItemKind::None => {}
            }
        }
    }
}

#[godot_api]
impl KneeMan {
    /// WebRTC fired our local description (offer for host, answer for guest). Set it locally and
    /// relay it to the peer through the signaling socket.
    #[func]
    fn on_sdp_created(&mut self, sdp_type: GString, sdp: GString) {
        if let Some(mut pc) = self.pc.clone() {
            pc.set_local_description(&sdp_type, &sdp);
        }
        if sdp_type == GString::from("offer") {
            self.sig.offer_out += 1;
        } else {
            self.sig.answer_out += 1;
        }
        let mut d = VarDictionary::new();
        d.set("kind", sdp_type); // "offer" | "answer"
        d.set("sdp", sdp);
        if let Some(mut ws) = self.ws.clone() {
            ws.send_text(&rtc::to_json(&d));
        }
    }

    /// WebRTC found a local ICE candidate. Relay it to the peer.
    #[func]
    fn on_ice_created(&mut self, media: GString, index: i32, name: GString) {
        self.sig.ice_out += 1;
        let mut d = VarDictionary::new();
        d.set("kind", "ice");
        d.set("media", media);
        d.set("index", index);
        d.set("name", name);
        if let Some(mut ws) = self.ws.clone() {
            ws.send_text(&rtc::to_json(&d));
        }
    }
}

impl KneeMan {
    /// Hand out the shared cells (clones point at the same BehaviorSubject).
    pub fn state_cell(&self) -> Mutable<SimState> {
        self.state.clone()
    }

    pub fn tune_cell(&self) -> Mutable<Tune> {
        self.tune.clone()
    }

    pub fn net_cell(&self) -> Mutable<NetDebug> {
        self.netdbg.clone()
    }

    pub fn identity_cell(&self) -> Mutable<Identity> {
        self.identity.clone()
    }

    /// Read the live transport states off the ws/pc/channel handles and publish them for the panel.
    fn publish_netdbg(&self) {
        let ws = self
            .ws
            .as_ref()
            .map(|w| ws_name(w.get_ready_state()))
            .unwrap_or("—");
        let (conn, gather, signal) = match self.pc.as_ref() {
            Some(pc) => (
                conn_name(pc.get_connection_state()),
                gather_name(pc.get_gathering_state()),
                signal_name(pc.get_signaling_state()),
            ),
            None => ("—", "—", "—"),
        };
        let channel = self
            .channel
            .as_ref()
            .map(|c| chan_name(c.get_ready_state()))
            .unwrap_or("—");
        self.netdbg.set(NetDebug {
            phase: match self.phase {
                Phase::Offline => "offline",
                Phase::Signaling => "signaling",
                Phase::Running => "running",
            },
            role: match self.role {
                Some(Role::Host) => "host",
                Some(Role::Guest) => "guest",
                None => "—",
            },
            handle: self.local_handle,
            ws,
            conn,
            gather,
            signal,
            channel,
            offer: (self.sig.offer_out, self.sig.offer_in),
            answer: (self.sig.answer_out, self.sig.answer_in),
            ice: (self.sig.ice_out, self.sig.ice_in),
        });
    }

    // --- frame loop (local + netplay) -----------------------------------------------------------

    /// Sample the keyboard into the engine-agnostic `InputFrame`. Associated (no `self`) so the
    /// netplay loop can call it while a session is borrowed.
    fn sample_input() -> InputFrame {
        use godot::global::{JoyAxis, JoyButton};
        let mut input = Input::singleton();
        // Keyboard movement (the default ui_* actions carry the arrow keys).
        let mut dir = input.get_axis("ui_left", "ui_right");
        let mut aim_y = input.get_axis("ui_up", "ui_down"); // -1 up .. +1 down
        let mut pad_down = false;
        // Web: the default ui_* movement actions don't carry the pad's stick/dpad, so read the first
        // connected joypad directly and merge it in (keyboard still works; pad wins when held).
        if let Some(dev) = input.get_connected_joypads().get(0) {
            let dev = dev as i32;
            let dz = 0.2;
            let sx = input.get_joy_axis(dev, JoyAxis::LEFT_X);
            let sy = input.get_joy_axis(dev, JoyAxis::LEFT_Y);
            let dpx = input.is_joy_button_pressed(dev, JoyButton::DPAD_RIGHT) as i32 as f32
                - input.is_joy_button_pressed(dev, JoyButton::DPAD_LEFT) as i32 as f32;
            let dpy = input.is_joy_button_pressed(dev, JoyButton::DPAD_DOWN) as i32 as f32
                - input.is_joy_button_pressed(dev, JoyButton::DPAD_UP) as i32 as f32;
            let px = if dpx != 0.0 { dpx } else if sx.abs() > dz { sx } else { 0.0 };
            let py = if dpy != 0.0 { dpy } else if sy.abs() > dz { sy } else { 0.0 };
            if dir == 0.0 {
                dir = px;
            }
            if aim_y == 0.0 {
                aim_y = py;
            }
            pad_down = py > 0.4;
        }
        InputFrame {
            dir,
            aim_y,
            jump: input.is_action_just_pressed("ui_accept")
                || input.is_action_just_pressed("ui_up"),
            jump_held: input.is_action_pressed("ui_accept") || input.is_action_pressed("ui_up"),
            shorthop: input.is_action_just_pressed("shorthop"),
            shield_held: input.is_action_pressed("shield"),
            shield_pressed: input.is_action_just_pressed("shield"),
            down: input.is_action_pressed("ui_down") || pad_down,
            down_pressed: input.is_action_just_pressed("ui_down"),
            attack: input.is_action_just_pressed("attack"),
            attack_held: input.is_action_pressed("attack"),
            grab: input.is_action_just_pressed("grab"),
            special: input.is_action_just_pressed("special"),
        }
    }

    /// Single-player: step the pure sim with [local input, neutral P2] and render.
    fn step_local(&mut self) {
        let frame = Self::sample_input();
        let p1 = InputFrame::default();
        let next = sim::step(&self.state.get(), [&frame, &p1], &self.tune.get()); // pure scan
        self.state.set(next);
        self.base_mut().set_position(gv(next.fighters[0].pos));
        self.render_anim(&next.fighters[0]);
        self.render_p2(&next.fighters[1]);
        self.base_mut().queue_redraw();
    }

    /// Netplay: ggrs owns the loop. Poll the transport, feed local input, advance (rolling back as
    /// needed via `Game::handle`), then mirror the rollback state into `self.state` for rendering.
    fn step_net(&mut self) {
        if let Some(mut pc) = self.pc.clone() {
            pc.poll();
        }
        let requests = {
            let Some(session) = self.session.as_mut() else { return };
            session.poll_remote_clients();
            if session.current_state() != SessionState::Running {
                None // still synchronizing with the peer; hold the last rendered frame
            } else {
                let net = encode(&Self::sample_input());
                if session.add_local_input(self.local_handle, net).is_err() {
                    None
                } else {
                    match session.advance_frame() {
                        Ok(reqs) => Some(reqs),
                        Err(GgrsError::PredictionThreshold) => None, // too far ahead; skip a frame
                        Err(e) => {
                            godot_error!("netplay: advance_frame: {e:?}");
                            None
                        }
                    }
                }
            }
        };
        if let (Some(reqs), Some(game)) = (requests, self.game.as_mut()) {
            game.handle(reqs);
        }
        if let Some(game) = self.game.as_ref() {
            self.state.set(game.state);
        }
        let s = self.state.get();
        self.base_mut().set_position(gv(s.fighters[0].pos));
        self.render_anim(&s.fighters[0]);
        self.render_p2(&s.fighters[1]);
        self.base_mut().queue_redraw();
    }

    // --- netplay setup / signaling --------------------------------------------------------------

    /// Dial the signaling relay. The relay replies `matched` with a role, kicking off the handshake.
    fn start_matchmaking(&mut self) {
        let mut ws = WebSocketPeer::new_gd();
        if ws.connect_to_url(rtc::SIGNALING_URL) != godot::global::Error::OK {
            godot_error!("netplay: signaling dial failed");
            return;
        }
        self.ws = Some(ws);
        self.sig = SigCounts::default(); // fresh tallies per match attempt
        self.phase = Phase::Signaling;
        godot_print!("netplay: dialing {} ...", rtc::SIGNALING_URL);
    }

    /// Per-frame while Signaling: drain inbound relay frames, drive the peer connection, and start
    /// the rollback session the moment the data channel opens.
    fn pump_signaling(&mut self) {
        let texts = {
            let Some(ws) = self.ws.as_mut() else { return };
            ws.poll();
            match ws.get_ready_state() {
                WsState::OPEN => {}
                WsState::CONNECTING | WsState::CLOSING => return, // not ready / shutting down
                _ => {
                    // CLOSED (or unknown): bail to offline below.
                    self.reset_offline();
                    return;
                }
            }
            let mut v = Vec::new();
            for _ in 0..ws.get_available_packet_count() {
                let pkt = ws.get_packet();
                v.push(GString::from(String::from_utf8_lossy(pkt.as_slice()).as_ref()));
            }
            v
        };
        for t in texts {
            self.handle_signal(&t);
        }
        if let Some(mut pc) = self.pc.clone() {
            pc.poll();
        }
        if let Some(ch) = self.channel.clone() {
            if ch.get_ready_state() == ChannelState::OPEN && self.session.is_none() {
                self.begin_session();
            }
        }
    }

    /// Dispatch one signaling frame from the relay (already JSON-parsed by key).
    fn handle_signal(&mut self, text: &GString) {
        let d = rtc::parse_json(text);
        match rtc::dget_str(&d, "kind").as_str() {
            "matched" => {
                if let Some(role) = Role::from_str(&rtc::dget_str(&d, "role")) {
                    self.setup_peer(role);
                }
            }
            // Guest receives the host's offer. Setting it auto-generates the answer, which fires
            // session_description_created -> on_sdp_created -> relayed back. No explicit create_answer.
            "offer" => {
                self.sig.offer_in += 1;
                let sdp = rtc::dget_str(&d, "sdp");
                if let Some(mut pc) = self.pc.clone() {
                    pc.set_remote_description("offer", &sdp);
                }
            }
            // Host receives the guest's answer.
            "answer" => {
                self.sig.answer_in += 1;
                let sdp = rtc::dget_str(&d, "sdp");
                if let Some(mut pc) = self.pc.clone() {
                    pc.set_remote_description("answer", &sdp);
                }
            }
            "ice" => {
                self.sig.ice_in += 1;
                let media = rtc::dget_str(&d, "media");
                let index = rtc::dget_int(&d, "index") as i32;
                let name = rtc::dget_str(&d, "name");
                if let Some(mut pc) = self.pc.clone() {
                    pc.add_ice_candidate(&media, index, &name);
                }
            }
            "bye" => self.reset_offline(),
            _ => {}
        }
    }

    /// Build the peer connection + negotiated data channel, wire the SDP/ICE signals back to our
    /// `#[func]` handlers. The host additionally creates the offer to start the exchange.
    fn setup_peer(&mut self, role: Role) {
        self.role = Some(role);
        self.local_handle = role.handles().0;

        let mut pc = WebRtcPeerConnection::new_gd();
        pc.initialize_ex().configuration(&rtc::ice_config()).done();

        let gd = self.to_gd();
        pc.connect(
            "session_description_created",
            &Callable::from_object_method(&gd, "on_sdp_created"),
        );
        pc.connect(
            "ice_candidate_created",
            &Callable::from_object_method(&gd, "on_ice_created"),
        );

        let mut channel = pc
            .create_data_channel_ex("ggrs")
            .options(&rtc::data_channel_options())
            .done()
            .expect("create negotiated data channel");
        rtc::set_binary(&mut channel);
        self.channel = Some(channel);
        self.pc = Some(pc);

        if role == Role::Host {
            if let Some(mut pc) = self.pc.clone() {
                pc.create_offer();
            }
        }
        godot_print!("netplay: matched as {role:?}");
    }

    /// Channel is open: hand it to a fresh ggrs session and flip to Running. Both peers MUST share
    /// the same Tune (default here); editing it mid-match would desync.
    fn begin_session(&mut self) {
        let Some(role) = self.role else { return };
        let (local_handle, remote) = role.handles();
        let Some(channel) = self.channel.clone() else { return };
        let socket = RtcSocket { channel, remote };
        match start_p2p(local_handle, remote, socket, rtc::INPUT_DELAY) {
            Ok(session) => {
                self.session = Some(session);
                self.game = Some(Game::new(self.tune.get()));
                self.local_handle = local_handle;
                self.phase = Phase::Running;
                godot_print!("netplay: channel open, rollback running (handle {local_handle})");
            }
            Err(e) => {
                godot_error!("netplay: session start failed: {e:?}");
                self.reset_offline();
            }
        }
    }

    /// Tear down all networking and return to single-player.
    fn reset_offline(&mut self) {
        self.session = None;
        self.game = None;
        self.channel = None;
        self.pc = None;
        if let Some(mut ws) = self.ws.take() {
            ws.close();
        }
        self.role = None;
        self.phase = Phase::Offline;
        godot_print!("netplay: offline");
    }

    /// One line describing where we are in the netplay lifecycle, shown top-left every frame.
    fn status_text(&self) -> String {
        match self.phase {
            Phase::Offline => "OFFLINE  ·  press Enter to find a match".to_string(),
            Phase::Signaling => "SIGNALING…  ·  waiting for an opponent".to_string(),
            Phase::Running => {
                let who = match self.role {
                    Some(Role::Host) => "host",
                    Some(Role::Guest) => "guest",
                    None => "?",
                };
                format!("NETPLAY  ·  {who} (handle {})", self.local_handle)
            }
        }
    }

    /// Push the current phase into the on-screen label.
    fn update_status(&mut self) {
        let txt = self.status_text();
        if let Some(mut l) = self.status.clone() {
            l.set_text(&txt);
        }
    }

    /// Drive the P1 sprite: clip for the state, flip by facing, and wear the player's color (green
    /// while intangible — the universal "you can't be hit" read).
    fn render_anim(&mut self, f: &Fighter) {
        let Some(mut a) = self.anim.clone() else { return };
        let clip = clip_for(f);
        if a.get_animation() != StringName::from(clip) {
            a.play_ex().name(clip).done(); // only restart when the clip actually changes
        }
        a.set_flip_h(f.facing < 0.0); // frog faces right by default
        a.set_modulate(sprite_tint(f, self.identity.get_cloned().color));
    }

    /// Drive the P2 sprite: same as P1 but positioned in world space each frame (it's a sibling, not
    /// a child of this node), wearing the P2 color.
    fn render_p2(&mut self, f: &Fighter) {
        let Some(mut a) = self.p2.clone() else { return };
        a.set_global_position(gv(f.pos));
        let clip = clip_for(f);
        if a.get_animation() != StringName::from(clip) {
            a.play_ex().name(clip).done();
        }
        a.set_flip_h(f.facing < 0.0);
        a.set_modulate(sprite_tint(f, p2_identity().color));
    }

    /// Persist the identity when the panel changes it, and refresh P1's nametag to match.
    fn sync_identity(&mut self) {
        let id = self.identity.get_cloned();
        if id == self.saved_identity {
            return;
        }
        save_identity(&id);
        if let Some(mut tag) = self.tag_p1.clone() {
            tag.set_text(&id.name);
            tag.add_theme_color_override("font_color", id.color);
            tag.add_theme_font_size_override("font_size", id.font_px);
        }
        if let Some(mut tag) = self.tag_p2.clone() {
            tag.add_theme_font_size_override("font_size", id.font_px);
        }
        self.saved_identity = id;
    }

    /// Hover each nametag a fixed height over its fighter's head, centered on the body.
    fn place_tags(&mut self) {
        let s = self.state.get();
        if let Some(mut tag) = self.tag_p1.clone() {
            place_tag(&mut tag, s.fighters[0].pos);
        }
        if let Some(mut tag) = self.tag_p2.clone() {
            place_tag(&mut tag, s.fighters[1].pos);
        }
    }
}

/// The sprite's modulate: hit flash > intangible green > the player's color.
fn sprite_tint(f: &Fighter, color: Color) -> Color {
    if f.hitstun > 0 {
        Color::from_rgb(1.0, 0.95, 0.55) // hit flash
    } else if f.intangible {
        Color::from_rgb(0.30, 0.95, 0.40)
    } else {
        color
    }
}

/// A world-space nametag: small, the player's color, with a dark outline so it reads over the
/// light stage. Centered horizontally each frame in `place_tag` (Label origin is top-left).
fn make_tag(name: &str, color: Color, font_px: i32) -> Gd<Label> {
    let mut l = Label::new_alloc();
    l.set_text(name);
    l.add_theme_font_size_override("font_size", font_px);
    l.add_theme_color_override("font_color", color);
    l.add_theme_constant_override("outline_size", 6);
    l.add_theme_color_override("font_outline_color", Color::from_rgba(0.05, 0.06, 0.10, 0.92));
    l.set_z_index(100); // above the sprites
    l
}

/// Position a nametag above a fighter's feet position, centered on the body.
fn place_tag(tag: &mut Gd<Label>, feet: sim::Vector2) {
    const TAG_RISE: f32 = 168.0; // above the feet, clear of the ~140px-tall sprite's head
    let half_w = tag.get_size().x * 0.5;
    let head = gv(feet) + Vector2::new(-half_w, -TAG_RISE);
    tag.set_global_position(head);
}

/// CharState -> SpriteFrames clip. 15 states collapse onto ~9 clips (choppy by design;
/// the Kenney pose set has no per-state art). Air splits rise/fall by vertical velocity.
fn clip_for(f: &Fighter) -> &'static str {
    use CharState::*;
    match f.state {
        Stand => "idle",
        Walk | Roll => "walk",
        Dash | Run => "run",
        Turn | Skid => "skid",
        Crouch | JumpSquat | SpotDodge | Landing | Shield => "crouch",
        Air | AirDodge => {
            if f.vel.y < 0.0 {
                "jump"
            } else {
                "fall"
            }
        }
        LedgeHold => "hang",
        LedgeClimb => "climb",
        Jab => "jab",
        Nair => "nair",
        Dair => "fall",
        DashAttack => "run",
        SpecialN | SpecialS | SpecialD => "jab", // reuse the swing pose until specials get art
        SpecialU => "jump",
        Helpless => "fall",
    }
}

/// Build a character's SpriteFrames from its clip table. Strip clips slice a sheet into cells;
/// Poses clips take one whole PNG per frame. Clip names match `clip_for` (choppy by design; the
/// art has no per-state poses, so several CharStates reuse one clip).
fn build_frames(c: &Character) -> Gd<SpriteFrames> {
    let mut sf = SpriteFrames::new_gd();
    for clip in c.clips {
        sf.add_animation(clip.name);
        sf.set_animation_speed(clip.name, clip.fps);
        sf.set_animation_loop(clip.name, clip.looped);
        match &c.sheet {
            Sheet::Strip { frame_px } => {
                let sheet = load::<Texture2D>(&format!("res://assets/{}/{}.png", c.dir, clip.files[0]));
                for i in 0..clip.frames {
                    let mut at = AtlasTexture::new_gd();
                    at.set_atlas(&sheet);
                    at.set_region(Rect2::new(
                        Vector2::new(i as f32 * frame_px, 0.0),
                        Vector2::splat(*frame_px),
                    ));
                    sf.add_frame(clip.name, &at.upcast::<Texture2D>());
                }
            }
            Sheet::Poses { prefix } => {
                for f in clip.files {
                    let tex = load::<Texture2D>(&format!("res://assets/{}/{}_{}.png", c.dir, prefix, f));
                    sf.add_frame(clip.name, &tex);
                }
            }
        }
    }
    sf
}
