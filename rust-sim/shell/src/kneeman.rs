use futures_signals::signal::Mutable;
use godot::classes::web_rtc_data_channel::ChannelState;
use godot::classes::web_rtc_peer_connection::{ConnectionState, GatheringState, SignalingState};
use godot::classes::web_socket_peer::State as WsState;
use godot::classes::{
    AnimatedSprite2D, AtlasTexture, Button, Camera2D, CanvasLayer, ColorRect, FileAccess, Gradient,
    GradientTexture2D, HttpRequest, INode2D, Input, InputEvent, InputEventKey, InputEventScreenDrag,
    InputEventScreenTouch, Json, Label, Node2D, Panel, Polygon2D, SpriteFrames, StyleBoxFlat,
    Texture2D, TextureRect, WebRtcDataChannel, WebRtcPeerConnection, WebSocketPeer,
};
use godot::global::Key;
use godot::prelude::*;
use godot::tools::try_load;

use crate::rtc::{self, Role, RtcSocket};
use crate::sim::{self, CharState, Fighter, InputFrame, SimState, Tune};
use smash_net::{encode, start_p2p, Advance, GgrsNetplay, NetInput, Netplay, Smash, SmashGame};

/// Netplay lifecycle. Offline = local single-player (default). Signaling = dialing the relay +
/// doing the WebRTC handshake; still renders local play so the page isn't frozen. Running = ggrs
/// rollback drives the sim from both peers' inputs. Reconnecting = the peer dropped mid-match; we
/// re-dial the private room and hold a window for them to come back before falling to Offline.
#[derive(Copy, Clone, PartialEq, Eq)]
enum Phase {
    Offline,
    Signaling,
    Running,
    Reconnecting,
}

/// How long (ms) to keep a match's room alive after a transport drop, waiting for the dropped peer
/// to re-dial and re-pair. Past this, the room is freed and we return to local play ("turn off").
const RECONNECT_WINDOW_MS: u64 = 12_000;

/// The match's room identity on the client (the "lobby entity"). `code` is the private room both
/// peers re-dial to find EACH OTHER again after a transport drop — the host mints it once paired and
/// ships it to the guest over the signaling socket; the relay forwards it verbatim, so no server
/// change is needed. `deadline_ms` is `Some` only while we're inside the reconnect window.
struct Room {
    code: String,
    deadline_ms: Option<u64>,
}

/// Monotonic ms clock (Godot's, so it works the same on native + the emscripten web build).
fn now_ms() -> u64 {
    godot::classes::Time::singleton().get_ticks_msec()
}

/// Mint a private room code for reconnect. Only the host calls this, once, so it just needs to be
/// unlikely to collide with another pair's room at the same instant: microsecond clock xor'd with a
/// hash of the host's name. Both peers then share THIS code (host sends it over the relay).
fn mint_room_code(name: &str) -> String {
    let t = godot::classes::Time::singleton().get_ticks_usec();
    let salt = name.bytes().fold(0u64, |a, b| a.wrapping_mul(131).wrapping_add(b as u64));
    format!("rm{:x}", t ^ salt.rotate_left(17))
}

/// Serialize a sim snapshot for the signaling channel: bincode bytes (same wire as ggrs messages),
/// base64'd via Godot's Marshalls so it rides inside a JSON text frame.
fn encode_state(s: &SimState) -> GString {
    let bytes = bincode::serialize(s).expect("serialize SimState snapshot");
    godot::classes::Marshalls::singleton().raw_to_base64(&PackedByteArray::from(bytes.as_slice()))
}

/// Inverse of `encode_state`. `None` if the base64/bincode doesn't decode (malformed frame), so the
/// guest keeps waiting for a good one rather than resuming from garbage.
fn decode_state(b64: &str) -> Option<SimState> {
    let raw = godot::classes::Marshalls::singleton().base64_to_raw(b64);
    bincode::deserialize::<SimState>(raw.as_slice()).ok()
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

/// Presentation color for a non-local fighter (slot 0 wears the local identity color instead).
/// Cosmetic only — never folded into the netplay checksum, so it can never desync a session.
fn slot_color(idx: usize) -> Color {
    match idx {
        1 => Color::from_rgb(1.0, 0.55, 0.35),  // orange
        2 => Color::from_rgb(0.45, 0.92, 0.50), // green
        3 => Color::from_rgb(0.80, 0.55, 0.95), // purple
        _ => Color::from_rgb(0.35, 0.75, 1.0),  // blue (slot-0 fallback)
    }
}

/// Nametag text for a non-local fighter ("P2".."P4"); slot 0 wears the local identity name.
fn slot_name(idx: usize) -> String {
    format!("P{}", idx + 1)
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

// Character roster. Cosmetic only -- a character's art is never folded into smash_net::checksum,
// so adding or reordering the roster cannot desync a netplay session. Each fighter slot holds an
// index into the roster (KneeMan::characters); char-select (later) just writes those indices.
//
// The roster is two built-ins (frog/zombie) followed by whatever `assets/roster.json` declares.
// That JSON is written by `tools/fetch_packs.py`, which fetches + converts sprite packs and drops
// `<clip>_strip<N>.png` files into `assets/<dir>/`. So adding a character is: edit tools/packs.toml,
// run `just packs`. The runtime never hardcodes a fetched character.
struct Character {
    dir: String,      // asset subdir under res://assets/
    scale: f32,       // node scale so the art lands ~140px tall (near the ECB height)
    offset_y: f32,    // sprite offset (texture px) so the feet sit on pos
    sheet: Sheet,     // how this character's PNGs are laid out on disk
    clips: Vec<Clip>, // one per CharState clip name (see clip_for)
}

/// How a character's frames are stored.
enum Sheet {
    /// One horizontal strip per clip, sliced into `frames` cells. `frame_px` is the cell width; 0
    /// means "derive from texture width / frame count" (the fetch script leaves it 0). Cell height
    /// is the full strip height, so non-square Rivals frames slice correctly. File = `<file>.png`.
    Strip { frame_px: f32 },
    /// One whole PNG per pose, named `<prefix>_<file>.png`. Each entry in `clip.files` is one frame.
    Poses { prefix: String },
}

/// One animation clip. For Strip, `files` holds the single strip name and `frames` is the cell
/// count; for Poses, `files` is the per-frame pose list and `frames` is ignored.
struct Clip {
    name: String,
    files: Vec<String>,
    frames: i32,
    fps: f64,
    looped: bool,
}

fn clip(name: &str, files: &[&str], frames: i32, fps: f64, looped: bool) -> Clip {
    Clip {
        name: name.to_string(),
        files: files.iter().map(|s| s.to_string()).collect(),
        frames,
        fps,
        looped,
    }
}

/// Display names for the roster, in index order (the menu char-select labels each pick with these).
pub(crate) fn roster_names() -> Vec<String> {
    roster().into_iter().map(|c| c.dir).collect()
}

/// The live roster: the two built-ins, then any characters declared in `assets/roster.json`.
fn roster() -> Vec<Character> {
    let mut v = vec![frog(), zombie()];
    v.extend(load_roster_json());
    v
}

/// P1 default: the Kenney/PixelFrog ninja frog (32px strips). CC0 placeholder art.
fn frog() -> Character {
    Character {
        dir: "pixelfrog/ninjafrog".to_string(),
        scale: 4.4, // 32px art -> ~140px tall, matching the ECB body
        offset_y: -12.0,
        sheet: Sheet::Strip { frame_px: 32.0 },
        clips: vec![
            clip("idle", &["idle"], 11, 14.0, true),
            clip("walk", &["run"], 12, 14.0, true),
            clip("run", &["run"], 12, 20.0, true),
            clip("crouch", &["fall"], 1, 1.0, false),
            clip("skid", &["fall"], 1, 1.0, false),
            clip("jump", &["jump"], 1, 1.0, false),
            clip("fall", &["fall"], 1, 1.0, false),
            clip("hang", &["wall_jump"], 5, 12.0, true),
            clip("climb", &["double_jump"], 6, 14.0, true),
            clip("jab", &["hit"], 7, 20.0, false),
            clip("nair", &["double_jump"], 6, 18.0, true),
            clip("dtilt", &["hit"], 7, 26.0, false), // pothole swing reuses the punch sheet, one-shot
            clip("dair", &["hit"], 7, 26.0, false), // the stomp: reuse the swing sheet (per-box scrub replays it)
            clip("wallbounce", &["fall"], 1, 1.0, false), // wall hit: a single frozen frame, tilted in render
        ],
    }
}

/// P2 default: the Kenney zombie (single-pose PNGs). Different silhouette from the frog.
fn zombie() -> Character {
    Character {
        dir: "kenney/zombie".to_string(),
        scale: 1.27, // 110px art -> ~140px tall, matching the ECB body
        offset_y: -55.0,
        sheet: Sheet::Poses { prefix: "zombie".to_string() },
        clips: vec![
            clip("idle", &["idle"], 1, 1.0, false),
            clip("walk", &["walk1", "walk2"], 2, 8.0, true),
            clip("run", &["walk1", "walk2"], 2, 13.0, true),
            clip("skid", &["skid"], 1, 1.0, false),
            clip("crouch", &["duck"], 1, 1.0, false),
            clip("jump", &["jump"], 1, 1.0, false),
            clip("fall", &["fall"], 1, 1.0, false),
            clip("hang", &["hang"], 1, 1.0, false),
            clip("climb", &["climb1", "climb2"], 2, 8.0, true),
            clip("jab", &["action1"], 1, 1.0, false),
            clip("nair", &["kick"], 1, 1.0, false),
            clip("dtilt", &["duck"], 1, 1.0, false),     // pothole reuses the duck pose
            clip("wallbounce", &["hurt"], 1, 1.0, false), // wall hit reuses the hurt pose
        ],
    }
}

/// Parse `res://assets/roster.json` (written by `tools/fetch_packs.py`) into extra characters.
/// Missing file or malformed JSON yields an empty list -- the built-ins always work, so a bad
/// roster never bricks the game. Schema (per character):
/// `{ "dir","scale","offset_y","sheet":"strip"|"poses","prefix"?,"frame_px"?,
///    "clips":[{ "name","files":[..],"frames","fps","loop" }] }`
fn load_roster_json() -> Vec<Character> {
    let path = "res://assets/roster.json";
    if !FileAccess::file_exists(path) {
        return Vec::new();
    }
    let Some(text) = FileAccess::open(path, godot::classes::file_access::ModeFlags::READ)
        .map(|f| f.get_as_text().to_string())
    else {
        return Vec::new();
    };
    let parsed = Json::parse_string(text.as_str());
    let Ok(root) = parsed.try_to::<Dictionary>() else {
        return Vec::new();
    };
    let Some(list) = root.get("characters").and_then(|v| v.try_to::<VariantArray>().ok()) else {
        return Vec::new();
    };
    list.iter_shared()
        .filter_map(|v| v.try_to::<Dictionary>().ok())
        .filter_map(parse_character)
        .collect()
}

/// One character dict -> Character. Returns None on a missing required field so one bad entry is
/// skipped rather than poisoning the whole roster.
fn parse_character(d: Dictionary) -> Option<Character> {
    let dir = jstr(&d, "dir")?;
    let scale = jnum(&d, "scale").unwrap_or(1.0) as f32;
    let offset_y = jnum(&d, "offset_y").unwrap_or(0.0) as f32;
    let sheet = match jstr(&d, "sheet").as_deref() {
        Some("poses") => Sheet::Poses { prefix: jstr(&d, "prefix").unwrap_or_default() },
        _ => Sheet::Strip { frame_px: jnum(&d, "frame_px").unwrap_or(0.0) as f32 },
    };
    let clips = d
        .get("clips")
        .and_then(|v| v.try_to::<VariantArray>().ok())
        .map(|arr| {
            arr.iter_shared()
                .filter_map(|c| c.try_to::<Dictionary>().ok())
                .filter_map(parse_clip)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if clips.is_empty() {
        return None;
    }
    Some(Character { dir, scale, offset_y, sheet, clips })
}

fn parse_clip(d: Dictionary) -> Option<Clip> {
    let name = jstr(&d, "name")?;
    let files = d
        .get("files")
        .and_then(|v| v.try_to::<VariantArray>().ok())
        .map(|arr| arr.iter_shared().filter_map(|f| f.try_to::<GString>().ok().map(|g| g.to_string())).collect())
        .unwrap_or_else(|| vec![name.clone()]);
    Some(Clip {
        name,
        files,
        frames: jnum(&d, "frames").unwrap_or(1.0) as i32,
        fps: jnum(&d, "fps").unwrap_or(12.0),
        looped: d.get("loop").and_then(|v| v.try_to::<bool>().ok()).unwrap_or(false),
    })
}

/// Read a string field from a Json-parsed Dictionary.
fn jstr(d: &Dictionary, key: &str) -> Option<String> {
    d.get(key).and_then(|v| v.try_to::<GString>().ok()).map(|g| g.to_string())
}

/// Read a number field (Json numbers come back as f64).
fn jnum(d: &Dictionary, key: &str) -> Option<f64> {
    d.get(key).and_then(|v| v.try_to::<f64>().ok())
}

/// Boundary: the pure sim speaks glam::Vec2; Godot wants its own Vector2. Convert on the way out.
#[inline]
fn gv(v: sim::Vector2) -> Vector2 {
    Vector2::new(v.x, v.y)
}

// --- On-screen touch gamepad (mobile), GameCube-proportioned. Everything is laid out from the LIVE
// viewport size (not fixed design coords) so the cluster anchors to the bottom corners under the thumbs
// no matter the aspect (portrait `aspect=expand` balloons height). The stick (left) drives dir/aim_y;
// the buttons drive the same Input actions the keyboard/pad use. Hit-tests read the same resolved
// layout the visuals use (TOUCH_LAYOUT), so touch + visual never drift. --

/// One quadrant of the diamond face cluster: the wedge meeting at the center, pointing one cardinal
/// way (top/left/right/bottom). The whole wedge is the hit area. `actions` are the Input actions a
/// press drives -- usually one, but the TOP wedge is multi-loaded (grab + shield) so a tap grabs / Z
/// / drops and a hold guards (and rolls/spotdodges with a stick tilt, air-dodges in the air).
struct Quad {
    actions: &'static [&'static str],
    letter: &'static str,
    color: (f32, f32, f32),
}

/// Diamond wedges, indexed by `Dir` (Top, Left, Right, Bottom). Top is the multi-loaded
/// grab/guard/Z/drop/dodge wedge; left attack, right special, bottom jump. Colors: purple guard,
/// green attack, red special, grey jump.
const QUADS: [Quad; 4] = [
    Quad { actions: &["grab", "shield"], letter: "Z\nGUARD", color: (0.62, 0.42, 0.86) }, // Top
    Quad { actions: &["attack"], letter: "A", color: (0.36, 0.82, 0.45) },                // Left
    Quad { actions: &["special"], letter: "B", color: (0.90, 0.30, 0.30) },               // Right
    Quad { actions: &["jump"], letter: "JUMP", color: (0.86, 0.88, 0.93) },               // Bottom
];
const DIR_TOP: usize = 0;
const DIR_LEFT: usize = 1;
const DIR_RIGHT: usize = 2;
const DIR_BOTTOM: usize = 3;

/// Resolved diamond geometry in live screen coords for one frame. `input` hit-tests against this:
/// a point is in the diamond when `|dx|+|dy| <= radius` (L1), and the wedge is whichever axis
/// dominates. The shorthop is a plain rect below the bottom tip.
#[derive(Clone, Copy)]
struct TouchLayout {
    center: Vector2,
    radius: f32, // center-to-tip (half-diagonal of the rotated square)
    shorthop: Rect2,
    stick_center: Vector2,
    stick_radius: f32,
    stick_zone_x: f32, // touches with screen-x below this (left side) grab the stick
}

/// Which wedge a screen point falls in, or None if outside the diamond. Matches the Polygon2D tiling
/// exactly (the wedges are split by the 45° lines through the center).
fn quad_at(p: Vector2, center: Vector2, radius: f32) -> Option<usize> {
    let d = p - center;
    if d.x.abs() + d.y.abs() > radius {
        return None;
    }
    Some(if d.y.abs() >= d.x.abs() {
        if d.y < 0.0 { DIR_TOP } else { DIR_BOTTOM }
    } else if d.x < 0.0 {
        DIR_LEFT
    } else {
        DIR_RIGHT
    })
}

/// The 4 vertices (relative to center) of one wedge polygon: center, two edge-midpoints, the tip.
/// `r` is center-to-tip. These four wedges tile the diamond perfectly and match `quad_at`.
fn quad_poly(dir: usize, r: f32) -> [Vector2; 4] {
    let h = r * 0.5;
    match dir {
        DIR_TOP => [Vector2::ZERO, Vector2::new(h, -h), Vector2::new(0.0, -r), Vector2::new(-h, -h)],
        DIR_BOTTOM => [Vector2::ZERO, Vector2::new(h, h), Vector2::new(0.0, r), Vector2::new(-h, h)],
        DIR_LEFT => [Vector2::ZERO, Vector2::new(-h, -h), Vector2::new(-r, 0.0), Vector2::new(-h, h)],
        _ => [Vector2::ZERO, Vector2::new(h, -h), Vector2::new(r, 0.0), Vector2::new(h, h)],
    }
}

/// Label anchor (center of the wedge) relative to the diamond center.
fn quad_label_pos(dir: usize, r: f32) -> Vector2 {
    let k = r * 0.52;
    match dir {
        DIR_TOP => Vector2::new(0.0, -k),
        DIR_BOTTOM => Vector2::new(0.0, k),
        DIR_LEFT => Vector2::new(-k, 0.0),
        _ => Vector2::new(k, 0.0),
    }
}

/// Build the layout from the current viewport. `u` scales to the SHORTER screen edge (thumb-sized in
/// any aspect). The diamond anchors to the bottom-right, lifted clear of the reserved HUD strip; the
/// shorthop rect sits just under its bottom tip; the stick floats on the left.
fn touch_layout(view: Vector2) -> TouchLayout {
    // Thumb-sized to the short edge, but also capped by the available width so the left stick and the
    // right diamond never collide into a centered clump (the portrait/narrow "smushed in the middle"
    // bug). The two clusters then hug their own screen edge with an equal margin = space-around.
    // Width budget: stick spans ~3.7u from the left, diamond ~5.7u from the right; >=9.6u keeps a gap.
    let u = (view.x.min(view.y) * 0.105).min(view.x / 9.6).clamp(44.0, 150.0);
    let radius = u * 2.6;
    let hud_clear = 150.0; // bottom strip reserved for the % HUD / menu button + shorthop
    let sh_h = u * 0.9; // shorthop rect height
    let cy = view.y - hud_clear - sh_h - radius; // diamond center, lifted so the bottom tip + rect clear the HUD
    let side = u * 0.5; // equal breathing room from each screen edge
    let cx = view.x - radius - side; // diamond hugs the right edge
    let center = Vector2::new(cx, cy);
    let sh_w = radius * 1.3;
    let shorthop = Rect2::new(
        Vector2::new(cx - sh_w * 0.5, cy + radius + u * 0.15),
        Vector2::new(sh_w, sh_h),
    );
    let stick_radius = u * 1.6;
    TouchLayout {
        center,
        radius,
        shorthop,
        stick_center: Vector2::new(side + stick_radius, cy), // stick hugs the left edge, mirror of the diamond
        stick_radius,
        stick_zone_x: view.x * 0.46,
    }
}

/// Percent-encode the few characters that would break a query string. Names are short + tame, so
/// anything outside the unreserved set becomes %XX (the relay's `url_decode` reverses it).
fn url_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// A round Panel (corner radius huge so it stays circular/stadium at any later resize), used for the
/// stick + face buttons. Visuals are repositioned/resized every frame in `update_touch`.
fn circle_panel(d: f32, fill: Color, border: Color) -> Gd<Panel> {
    let mut p = Panel::new_alloc();
    p.set_size(Vector2::splat(d));
    let mut sb = StyleBoxFlat::new_gd();
    sb.set_bg_color(fill);
    sb.set_corner_radius_all(400); // >= any radius we use -> always a circle/pill
    sb.set_border_width_all(3);
    sb.set_border_color(border);
    p.add_theme_stylebox_override("panel", &sb);
    p
}

/// Resize a Panel to diameter `d` and center it on `c` (Control positions are top-left).
fn place_circle(p: &mut Gd<Panel>, c: Vector2, d: f32) {
    p.set_size(Vector2::splat(d));
    p.set_position(c - Vector2::splat(d * 0.5));
}

thread_local! {
    /// Analog stick output in [-1,1] per axis, written by the touch handler, read by `sample_input`.
    static TOUCH_STICK: std::cell::Cell<(f32, f32)> = const { std::cell::Cell::new((0.0, 0.0)) };
    /// Finger index that owns the stick (-1 = none) + its screen origin for floating-stick math.
    static TOUCH_FINGER: std::cell::Cell<i64> = const { std::cell::Cell::new(-1) };
    static TOUCH_ORIGIN: std::cell::Cell<(f32, f32)> = const { std::cell::Cell::new((0.0, 0.0)) };
    /// Full-tilt throw distance for the stick in px (scales with viewport; set each frame).
    static TOUCH_STICK_RAD: std::cell::Cell<f32> = const { std::cell::Cell::new(95.0) };
    /// Fingers currently holding a wedge: (finger_index, actions) so multi-touch releases the right
    /// Input actions (the top wedge presses two; the rest one).
    static TOUCH_BTNS: std::cell::RefCell<Vec<(i64, &'static [&'static str])>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// Resolved diamond geometry for this frame; `input` hit-tests against it. None until first frame.
    static TOUCH_DIAMOND: std::cell::Cell<Option<TouchLayout>> =
        const { std::cell::Cell::new(None) };
    static TOUCH_STICK_ZONE_X: std::cell::Cell<f32> = const { std::cell::Cell::new(736.0) };
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
    // Per-fighter render slots, indexed 0..active. `sprites[0]` is the node's own "Anim" child
    // (positioned via the node); `sprites[1..]` are world-space siblings positioned each frame.
    sprites: [Option<Gd<AnimatedSprite2D>>; sim::MAX_PLAYERS], // driven by CharState per fighter
    dummy: Option<Gd<ColorRect>>,       // legacy P2 block (hidden once the P2 sprite exists)
    tags: [Option<Gd<Label>>; sim::MAX_PLAYERS], // world-space nametags hovering over each head
    edge_tags: [Option<Gd<Label>>; sim::MAX_PLAYERS], // screen-space "off-stage" chips: name+arrow+dist
    prev_pos: [sim::Vector2; sim::MAX_PLAYERS], // last frame's feet pos, for KO teleport detection (bangs)
    trails: [Vec<sim::Vector2>; sim::MAX_PLAYERS], // recent feet positions per fighter, for the fast-move smear
    bangs: Vec<(Vector2, f32)>,         // active blast flashes: world pos + age (0..1), drawn in draw()
    status: Option<Gd<Button>>,         // screen-space netplay status chip; tap it to find a match
    hud: [Option<Gd<Label>>; sim::MAX_PLAYERS], // bottom damage panel: each fighter's name + %
    cam: Option<Gd<Camera2D>>,          // sibling Camera2D, tracked to fit both fighters each frame
    stick_base: Option<Gd<Panel>>,      // touch stick ring (follows the active finger origin)
    stick_knob: Option<Gd<Panel>>,      // touch stick knob (offset by the current tilt)
    quad_polys: Vec<Gd<Polygon2D>>,     // 4 diamond wedge fills (indexed by Dir: Top,Left,Right,Bottom)
    quad_labels: Vec<Gd<Label>>,        // wedge letter labels (same order)
    shorthop_panel: Option<Gd<Panel>>,  // rectangle under the bottom tip
    shorthop_label: Option<Gd<Label>>,
    menu_btn: Option<Gd<Button>>,       // bottom-center MENU tab: opens debug panel + pauses
    debug_ui: Option<Gd<crate::ui::debug::DebugUi>>, // sibling egui panel, toggled by the MENU button
    paused: bool,                       // MENU pause: freeze the sim while the panel is open
    netdbg: Mutable<NetDebug>,          // transport snapshot, read by the debug panel
    sig: SigCounts,                     // handshake-frame tallies feeding netdbg
    identity: Mutable<Identity>,        // local player name+color, edited by the panel, persisted
    saved_identity: Identity,           // last value written to localStorage (change detection)
    charsel: Mutable<[i64; 2]>,         // P1/P2 roster pick, written by the menu, applied live
    characters: [usize; sim::MAX_PLAYERS], // per-fighter index into ROSTER; charsel drives slots 0..2
    base_scale: [f32; sim::MAX_PLAYERS],   // each sprite's resting scale (impact-pop multiplies it)

    // --- netplay (Godot WebRTC). All None/Offline until the player taps the status chip to join. ---
    phase: Phase,
    role: Option<Role>,
    local_handle: usize,                       // ggrs handle for this peer (host 0 / guest 1)
    ws: Option<Gd<WebSocketPeer>>,             // signaling socket to the relay
    pc: Option<Gd<WebRtcPeerConnection>>,      // the P2P connection
    channel: Option<Gd<WebRtcDataChannel>>,    // negotiated data channel ggrs rides
    net: Option<Box<dyn Netplay<State = SimState, Input = NetInput>>>, // model-agnostic session seam (rollback today)
    room: Option<Room>,                        // match's room identity; survives a drop so we can rejoin
    resume_snapshot: Option<SimState>,         // sim state captured at a drop, to resume the rebuilt session from
    got_resume: bool,                          // guest: received the host's resume snapshot this reconnect

    // --- version compatibility (build-hash ping; see rtc::BUILD_HASH) ---
    http: Option<Gd<HttpRequest>>,             // refetches the relay /status on refocus to spot a stale build
    peer_build: Option<String>,                // opponent's build hash from the SDP handshake (None until traded)
    stale_build: bool,                         // our wasm is older than the live server build -> reload
}

#[godot_api]
impl INode2D for KneeMan {
    fn init(base: Base<Node2D>) -> Self {
        Self {
            base,
            state: Mutable::new(SimState::spawn()),
            tune: Mutable::new(Tune::default()),
            sprites: Default::default(),
            dummy: None,
            tags: Default::default(),
            edge_tags: Default::default(),
            prev_pos: [sim::Vector2::new(0.0, 0.0); sim::MAX_PLAYERS],
            trails: Default::default(),
            bangs: Vec::new(),
            status: None,
            hud: Default::default(),
            cam: None,
            stick_base: None,
            stick_knob: None,
            quad_polys: Vec::new(),
            quad_labels: Vec::new(),
            shorthop_panel: None,
            shorthop_label: None,
            menu_btn: None,
            debug_ui: None,
            paused: false,
            netdbg: Mutable::new(NetDebug::default()),
            sig: SigCounts::default(),
            identity: Mutable::new(Identity::default()),
            saved_identity: Identity::default(),
            charsel: Mutable::new([0, 1]),
            characters: [0, 1, 0, 1], // default: frog/zombie alternating; charsel overrides slots 0..2
            base_scale: [1.0; sim::MAX_PLAYERS],
            phase: Phase::Offline,
            role: None,
            local_handle: 0,
            ws: None,
            pc: None,
            channel: None,
            net: None,
            room: None,
            resume_snapshot: None,
            got_resume: false,
            http: None,
            peer_build: None,
            stale_build: false,
        }
    }

    fn ready(&mut self) {
        let pos = self.state.get().fighters[0].pos;
        self.base_mut().set_position(gv(pos));

        // Load the saved identity (web) before building tags so slot 0's tag wears the right name/color.
        let id = load_identity();
        self.identity.set(id.clone());
        self.saved_identity = id.clone();

        // Legacy P2 block: hide it, the per-fighter sprites replace it.
        self.dummy = self
            .base()
            .get_node_or_null("../Dummy")
            .and_then(|n| n.try_cast::<ColorRect>().ok());
        if let Some(d) = self.dummy.as_mut() {
            d.set_visible(false);
        }

        // Per-fighter sprites + nametags, one slot per possible player. Slot 0 is the node's own
        // "Anim" child (positioned via the node itself); slots 1.. are world-space siblings under the
        // parent, positioned each frame in `render_fighter`. Tags are world-space labels hovering over
        // each head, wearing the slot color. Built here, not in a .tres, so the CharState->clip wiring
        // stays readable. Dormant slots (>= active) are hidden each frame by the render loop.
        let roster = roster();
        for k in 0..sim::MAX_PLAYERS {
            let c = &roster[self.characters[k].min(roster.len() - 1)];
            let color = if k == 0 { id.color } else { slot_color(k) };
            let name = if k == 0 { id.name.clone() } else { slot_name(k) };
            let tag = make_tag(&name, color, id.font_px);

            let sprite = if k == 0 {
                // Slot 0: the authored "Anim" child; it tracks the node, so no per-frame position.
                self.base()
                    .get_node_or_null("Anim")
                    .and_then(|n| n.try_cast::<AnimatedSprite2D>().ok())
            } else {
                Some(AnimatedSprite2D::new_alloc())
            };
            if let Some(mut a) = sprite {
                apply_character(&mut a, c);
                self.base_scale[k] = c.scale;
                // World-space siblings (slots 1..) add deferred: during ready() the parent is still
                // "busy setting up children", so an immediate add_child is rejected. The tag is a
                // world-space sibling for every slot.
                if let Some(mut parent) = self.base().get_parent() {
                    if k != 0 {
                        parent.call_deferred("add_child", &[a.to_variant()]);
                    }
                    parent.call_deferred("add_child", &[tag.to_variant()]);
                }
                self.sprites[k] = Some(a);
                self.tags[k] = Some(tag);
            }
        }

        // Always-on netplay status chip. A CanvasLayer pins it to the screen (not the world), so it
        // stays put as the camera tracks the fighter. It's a Button, not a Label: tapping/clicking it
        // is the one way to find a match (replaces the old hold-Enter path + the big CONNECT button).
        let mut layer = CanvasLayer::new_alloc();
        let mut label = Button::new_alloc();
        label.set_position(Vector2::new(14.0, 10.0));
        label.add_theme_font_size_override("font_size", 20);
        label.add_theme_color_override("font_color", Color::from_rgb(0.92, 0.96, 1.0));
        // Dark rounded chip behind the text so it reads on the white stage (clear color is white).
        // Override every button state to the same chip so it doesn't flash default button chrome.
        let mut bg = godot::classes::StyleBoxFlat::new_gd();
        bg.set_bg_color(Color::from_rgba(0.07, 0.09, 0.14, 0.85));
        bg.set_corner_radius_all(6);
        bg.set_content_margin_all(8.0);
        for st in ["normal", "hover", "pressed", "focus", "disabled"] {
            label.add_theme_stylebox_override(st, &bg);
        }
        let cb = self.to_gd();
        label.connect("pressed", &Callable::from_object_method(&cb, "on_connect"));
        layer.add_child(&label);

        // Bottom damage panel: each fighter's name + %, wearing the slot color. Same screen-pinned
        // CanvasLayer. Positioned/filled every frame in `update_hud` (handles window resize + active count).
        for k in 0..sim::MAX_PLAYERS {
            let color = if k == 0 { self.identity.get_cloned().color } else { slot_color(k) };
            let l = make_hud_label(color);
            layer.add_child(&l);
            self.hud[k] = Some(l);
        }

        self.base_mut().add_child(&layer);
        self.status = Some(label);
        // Sibling Camera2D (authored in game.tscn). We drive it each frame to keep both fighters
        // framed; without this it sits at its static authored transform and fighters leave the view.
        self.cam = self.base().try_get_node_as::<Camera2D>("../Camera2D");
        self.debug_ui = self.base().try_get_node_as::<crate::ui::debug::DebugUi>("../DebugUi");

        // Version-check HTTP client: refetches the relay /status on refocus to spot a stale cached
        // web build (web routes this through the browser fetch). Ping once now to catch a stale load.
        let mut http = HttpRequest::new_alloc();
        let vcb = self.to_gd();
        http.connect("request_completed", &Callable::from_object_method(&vcb, "on_status_fetched"));
        self.base_mut().add_child(&http);
        self.http = Some(http);
        self.ping_version();

        // Skybox: a screen-pinned vertical gradient behind the world (deep space-blue -> horizon
        // glow). CanvasLayer at a negative layer keeps it under the stage + fighters, and a
        // full-rect anchor lets it cover any window without per-frame resizing.
        let mut sky_layer = CanvasLayer::new_alloc();
        sky_layer.set_layer(-10);
        let mut grad = Gradient::new_gd();
        grad.set_offsets(&PackedFloat32Array::from(&[0.0, 0.55, 1.0]));
        grad.set_colors(&PackedColorArray::from(&[
            Color::from_rgb(0.04, 0.05, 0.12), // top of sky
            Color::from_rgb(0.10, 0.13, 0.28), // mid
            Color::from_rgb(0.22, 0.20, 0.34), // horizon haze
        ]));
        let mut tex = GradientTexture2D::new_gd();
        tex.set_gradient(&grad);
        tex.set_fill_from(Vector2::new(0.0, 0.0));
        tex.set_fill_to(Vector2::new(0.0, 1.0)); // vertical
        let mut sky = TextureRect::new_alloc();
        sky.set_texture(&tex);
        sky.set_anchors_preset(godot::classes::control::LayoutPreset::FULL_RECT);
        sky.set_stretch_mode(godot::classes::texture_rect::StretchMode::SCALE);
        sky_layer.add_child(&sky);
        self.base_mut().add_child(&sky_layer);

        // Off-stage chips: screen-pinned labels that appear at the screen edge when a fighter is
        // launched out of view, showing name + a pointer arrow + the off-screen distance.
        let mut edge_layer = CanvasLayer::new_alloc();
        edge_layer.set_layer(40);
        for k in 0..sim::MAX_PLAYERS {
            let color = if k == 0 { self.identity.get_cloned().color } else { slot_color(k) };
            let chip = make_edge_tag(color);
            edge_layer.add_child(&chip);
            self.edge_tags[k] = Some(chip);
        }
        self.base_mut().add_child(&edge_layer);

        self.build_touch_ui();
        self.update_status();
        self.update_hud();
    }

    fn physics_process(&mut self, _delta: f64) {
        // MENU pause freezes the sim locally. Netplay (Running) keeps advancing so ggrs
        // doesn't stall the session; a synced pause needs a dedicated pause-input.
        if self.paused && self.phase != Phase::Running {
            self.update_status();
            return;
        }
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
            // Re-pair through the same handshake pump; keep showing local play meanwhile. Give up
            // and free the room once the window elapses with no opponent back.
            Phase::Reconnecting => {
                self.pump_signaling();
                if self.phase == Phase::Reconnecting {
                    let expired = self
                        .room
                        .as_ref()
                        .and_then(|r| r.deadline_ms)
                        .map(|d| now_ms() > d)
                        .unwrap_or(true);
                    if expired {
                        godot_print!("netplay: reconnect window elapsed — turning off");
                        self.reset_offline();
                    } else {
                        self.step_local();
                    }
                }
            }
        }
        self.update_status();
        self.update_hud();
        self.publish_netdbg();
        self.sync_identity();
        self.sync_charsel();
        self.place_tags();
    }

    /// Window/tab regained focus (desktop WM focus or browser tab focus on web). Re-ping the relay
    /// to check whether a deploy left this client running a stale build. The browser tab case is the
    /// one that matters: a tab backgrounded across a redeploy comes back on cached, mismatched wasm.
    fn on_notification(&mut self, what: godot::classes::notify::CanvasItemNotification) {
        use godot::classes::notify::CanvasItemNotification as N;
        if matches!(what, N::WM_WINDOW_FOCUS_IN | N::APPLICATION_FOCUS_IN) {
            self.ping_version();
        }
    }

    /// Touch gamepad. Screen touches feed the on-screen stick/buttons; the stick writes the
    /// thread-local read by `sample_input`; the buttons press/release the same Input actions the
    /// keyboard binds. (Finding a match is the status chip's `on_connect`, not a key here.)
    fn input(&mut self, event: Gd<InputEvent>) {
        // Finger down/up: claim a face button (right) or the floating stick (left).
        if let Ok(touch) = event.clone().try_cast::<InputEventScreenTouch>() {
            let pos = touch.get_position();
            let finger = touch.get_index() as i64;
            if touch.is_pressed() {
                // hit-test the shorthop rect, then the diamond wedges, against this frame's layout
                let hit = TOUCH_DIAMOND.get().and_then(|lay| {
                    if lay.shorthop.contains_point(pos) {
                        Some(crate::controls::GameAction::ShortHop.names())
                    } else {
                        quad_at(pos, lay.center, lay.radius).map(|q| QUADS[q].actions)
                    }
                });
                if let Some(actions) = hit {
                    for a in actions {
                        Input::singleton().action_press(*a);
                    }
                    TOUCH_BTNS.with_borrow_mut(|v| v.push((finger, actions)));
                } else if TOUCH_FINGER.get() < 0 && pos.x < TOUCH_STICK_ZONE_X.get() {
                    TOUCH_FINGER.set(finger);
                    TOUCH_ORIGIN.set((pos.x, pos.y));
                    TOUCH_STICK.set((0.0, 0.0));
                }
            } else {
                // release: drop any wedge this finger held, and free the stick if it owned it.
                TOUCH_BTNS.with_borrow_mut(|v| {
                    v.retain(|&(f, actions)| {
                        if f == finger {
                            for a in actions {
                                Input::singleton().action_release(*a);
                            }
                            false
                        } else {
                            true
                        }
                    })
                });
                if TOUCH_FINGER.get() == finger {
                    TOUCH_FINGER.set(-1);
                    TOUCH_STICK.set((0.0, 0.0));
                }
            }
            return;
        }
        // Finger drag: if it owns the stick, update the tilt from its travel off the origin.
        if let Ok(drag) = event.clone().try_cast::<InputEventScreenDrag>() {
            if drag.get_index() as i64 == TOUCH_FINGER.get() {
                let (ox, oy) = TOUCH_ORIGIN.get();
                let p = drag.get_position();
                let rad = TOUCH_STICK_RAD.get();
                let sx = ((p.x - ox) / rad).clamp(-1.0, 1.0);
                let sy = ((p.y - oy) / rad).clamp(-1.0, 1.0);
                TOUCH_STICK.set((sx, sy));
            }
            return;
        }
        // Escape toggles the MENU (pause + debug panel), same as the on-screen ☰ tab.
        if let Ok(key) = event.try_cast::<InputEventKey>() {
            if key.is_pressed() && !key.is_echo() && key.get_keycode() == Key::ESCAPE {
                self.on_menu();
            }
        }
    }

    /// Debug overlay: each fighter's ECB (cyan), hurtbox (yellow), and active hitbox (red).
    /// Drawn for both players so P2's attacks show their boxes too. Coordinates are world,
    /// converted to this node's local space (the node sits at the player position).
    fn draw(&mut self) {
        let s = self.state.get();
        let t = self.tune.get();
        let active = s.active as usize;
        let origin = self.base().get_position();

        // Blast bangs: a KO teleports the fighter from a blast edge back to spawn in one frame.
        // Detect that jump, drop a flash on the boundary they flew through, age the rest out.
        for k in 0..active {
            let p = s.fighters[k].pos;
            let prev = self.prev_pos[k];
            if (p - prev).length() > 700.0 {
                let edge = sim::Vector2::new(
                    prev.x.clamp(sim::BLAST_LEFT, sim::BLAST_RIGHT),
                    prev.y.clamp(sim::BLAST_TOP, sim::BLAST_Y),
                );
                self.bangs.push((gv(edge), 0.0));
            }
            self.prev_pos[k] = p;
        }
        self.bangs.retain(|(_, age)| *age < 1.0);
        // draw + age each bang: an expanding ring plus radiating spokes, hot orange fading out.
        let bangs: Vec<(Vector2, f32)> = self.bangs.clone();
        for (i, (wp, age)) in bangs.iter().enumerate() {
            let c = *wp - origin;
            let a = *age;
            let r = 30.0 + a * 230.0;
            let alpha = (1.0 - a).powf(1.4);
            let col = Color::from_rgba(1.0, 0.55 + 0.35 * (1.0 - a), 0.12, alpha);
            self.base_mut().draw_arc_ex(c, r, 0.0, std::f32::consts::TAU, 28, col).width(6.0 * (1.0 - a) + 1.0).done();
            for spoke in 0..8 {
                let ang = spoke as f32 / 8.0 * std::f32::consts::TAU + a * 0.6;
                let dir = Vector2::new(ang.cos(), ang.sin());
                self.base_mut().draw_line_ex(c + dir * (r * 0.5), c + dir * (r + 40.0 * (1.0 - a)), col).width(5.0 * (1.0 - a) + 1.0).done();
            }
            self.bangs[i].1 = a + 0.045;
        }

        // Motion smear: fast bursts (up-B / side-B / a hard launch) move a frozen single-frame
        // sprite far enough per frame that the eye reads it as a teleport. Trail a few fading
        // ghost discs along the recent path so the movement reads as motion instead of a pop.
        // Purely cosmetic (shell-side), never touches the sim or the netplay checksum.
        for k in 0..active {
            let p = s.fighters[k].pos;
            let trail = &mut self.trails[k];
            trail.push(p);
            if trail.len() > 6 {
                trail.remove(0);
            }
            // speed = last per-frame step. Below ~9px/frame (a normal run) draw nothing.
            let speed = trail
                .last()
                .zip(trail.get(trail.len().wrapping_sub(2)))
                .map(|(a, b)| (*a - *b).length())
                .unwrap_or(0.0);
            if speed < 9.0 {
                continue;
            }
            let body = 46.0_f32; // body half-height, in world px (lift the disc to torso level)
            let intensity = ((speed - 9.0) / 26.0).clamp(0.0, 1.0); // 9..35 px/frame -> 0..1
            let pts: Vec<sim::Vector2> = trail.clone(); // drop the &mut self.trails borrow before drawing
            let n = pts.len();
            for (j, gp) in pts.iter().enumerate() {
                let f = j as f32 / (n.max(2) - 1) as f32; // 0 oldest .. 1 newest
                let c = gv(*gp) - origin - Vector2::new(0.0, body); // lift to body center
                let alpha = (0.30 * intensity) * f * f; // fade hard toward the tail
                let col = Color::from_rgba(0.75, 0.88, 1.0, alpha);
                self.base_mut().draw_circle(c, body * (0.55 + 0.35 * f), col);
            }
        }

        let ecb = Color::from_rgba(0.20, 0.85, 0.95, 0.85);
        let hurt_col = Color::from_rgba(0.95, 0.85, 0.20, 0.30);
        let hit_col = Color::from_rgba(0.95, 0.25, 0.25, 0.45);
        for f in &s.fighters[..active] {
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
            // active hitboxes: every box live this frame (a multi-box move shows all its windows).
            for hb in sim::live_hitboxes(f, &t).into_iter().flatten() {
                let (hc, hr) = hb;
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
                sim::ItemKind::BobGun => {
                    let size = Vector2::new(40.0, 18.0);
                    self.base_mut().draw_rect(
                        Rect2::new(c - size * 0.5, size),
                        Color::from_rgb(0.92, 0.16, 0.16), // red gun
                    );
                }
                sim::ItemKind::Bomb => {
                    // dark body with a red fuse-glow ring, so the lobbed Bob-omb reads in the air.
                    self.base_mut().draw_circle(c, 14.0, Color::from_rgb(0.08, 0.08, 0.10));
                    self.base_mut()
                        .draw_arc_ex(c, 18.0, 0.0, std::f32::consts::TAU, 20, Color::from_rgb(1.0, 0.4, 0.15))
                        .width(3.0)
                        .done();
                }
                sim::ItemKind::Pen => {
                    // drawing tool pickup: a bright nib so it reads as ink.
                    let size = Vector2::new(30.0, 30.0);
                    self.base_mut().draw_rect(
                        Rect2::new(c - size * 0.5, size),
                        Color::from_rgb(0.20, 0.55, 1.0),
                    );
                }
                sim::ItemKind::None => {}
            }
        }

        // drawn ink paths: stroke each live polyline. Cosmetic read of the sim's cached classes —
        // grabbable lips get a hotter tint so the playable surface is legible.
        for p in &s.paths {
            if !p.active() {
                continue;
            }
            let n = p.len as usize;
            for seg in 0..n.saturating_sub(1) {
                let a = gv(p.pts[seg]) - origin;
                let b = gv(p.pts[seg + 1]) - origin;
                let col = match p.class[seg] {
                    sim::SegClass::Ledge => Color::from_rgb(1.0, 0.85, 0.2), // grabbable lip
                    sim::SegClass::Floor => Color::from_rgb(0.3, 0.7, 1.0),
                    sim::SegClass::Wall => Color::from_rgb(0.6, 0.4, 1.0),
                    sim::SegClass::None => Color::from_rgba(0.5, 0.8, 1.0, 0.5), // not yet a surface
                };
                self.base_mut().draw_line_ex(a, b, col).width(7.0).done();
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
        d.set("hash", rtc::BUILD_HASH); // peer flags a version mismatch from this
        if let Some(mut ws) = self.ws.clone() {
            ws.send_text(&rtc::to_json(&d));
        }
    }

    /// Status-chip tap handler: find a match. Guarded to Offline (a no-op once connecting/connected).
    #[func]
    fn on_connect(&mut self) {
        if self.phase == Phase::Offline {
            self.start_matchmaking();
        }
    }

    /// MENU tab handler: toggle pause + the debug panel together.
    #[func]
    fn on_menu(&mut self) {
        self.paused = !self.paused;
        if let Some(dbg) = self.debug_ui.as_mut() {
            dbg.bind_mut().set_open(self.paused);
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

    /// Relay /status fetched (on focus-in / startup): compare the live server's `build_hash` to ours.
    /// A mismatch means our wasm is stale (a deploy happened) -- flag it so the status line says reload.
    #[func]
    fn on_status_fetched(
        &mut self,
        _result: i64,
        code: i64,
        _headers: PackedStringArray,
        body: PackedByteArray,
    ) {
        if code != 200 {
            return; // server unreachable; leave the prior verdict untouched
        }
        let text = body.get_string_from_utf8();
        let server = rtc::dget_str(&rtc::parse_json(&text), "build_hash");
        // Only call it stale when both hashes are real and differ; "unknown" (dev build) never warns.
        self.stale_build = !server.is_empty()
            && server != "unknown"
            && rtc::BUILD_HASH != "unknown"
            && server != rtc::BUILD_HASH;
    }
}

impl KneeMan {
    /// Ping the relay's /status to learn the live build, and (if mid-match) re-trade the peer hash.
    /// Called on focus-in so a tab woken after a deploy notices it's running stale code.
    fn ping_version(&mut self) {
        if let Some(http) = self.http.as_mut() {
            let _ = http.request(rtc::STATUS_URL);
        }
    }

    /// Record the opponent's build hash from the handshake. Surfaced by `status_text` so a mismatched
    /// pair sees it before the differing sims desync. Empty/"unknown" hashes are ignored.
    fn note_peer_build(&mut self, hash: String) {
        if !hash.is_empty() && hash != "unknown" {
            self.peer_build = Some(hash);
        }
    }

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

    pub fn charsel_cell(&self) -> Mutable<[i64; 2]> {
        self.charsel.clone()
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
                Phase::Reconnecting => "reconnecting",
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

    /// Sample the local player's controls into the engine-agnostic `InputFrame`. Delegates to the
    /// `controls` boundary (the only raw-device site); the touch stick is merged in from our widget.
    fn sample_input() -> InputFrame {
        crate::controls::poll(TOUCH_STICK.get())
    }

    /// Local play: step the pure sim with both players' frames and render. P1 is this machine's main
    /// controls; P2 is couch co-op (second gamepad / WASD), neutral until someone grabs it.
    fn step_local(&mut self) {
        let frame = Self::sample_input();
        let p2 = crate::controls::poll_p2();
        let next = sim::step(&self.state.get(), &[&frame, &p2], &self.tune.get()); // pure scan
        self.state.set(next);
        self.base_mut().set_position(gv(next.fighters[0].pos));
        self.render_fighters(&next);
        self.update_camera();
        self.update_touch();
        self.base_mut().queue_redraw();
    }

    /// Netplay: ggrs owns the loop. Poll the transport, feed local input, advance (rolling back as
    /// needed via `Game::handle`), then mirror the rollback state into `self.state` for rendering.
    fn step_net(&mut self) {
        if let Some(mut pc) = self.pc.clone() {
            pc.poll();
        }
        // Transport-level drop: ICE failed or the data channel closed. ggrs also reports the peer
        // gone (its packets stopped) via a Disconnected event. Either one opens the reconnect window.
        let mut peer_gone = self.transport_dropped();
        {
            let Some(net) = self.net.as_mut() else { return };
            net.poll(); // pump transport + drain session events (may flag a peer drop)
            if !peer_gone {
                let local = encode(&Self::sample_input());
                if net.advance(local) == Advance::PeerGone {
                    peer_gone = true;
                }
            }
        }
        if peer_gone {
            self.begin_reconnect();
            return;
        }
        if let Some(net) = self.net.as_ref() {
            self.state.set(*net.state()); // mirror the authoritative frame for rendering
        }
        let s = self.state.get();
        self.base_mut().set_position(gv(s.fighters[0].pos));
        self.render_fighters(&s);
        self.update_camera();
        self.update_touch();
        self.base_mut().queue_redraw();
    }

    // --- netplay setup / signaling --------------------------------------------------------------

    /// Dial the signaling relay. The relay replies `matched` with a role, kicking off the handshake.
    /// A fresh match has no room yet (the host mints one once paired); reconnects re-dial with it.
    fn start_matchmaking(&mut self) {
        self.room = None;
        self.resume_snapshot = None; // fresh match starts from spawn, not a stale snapshot
        self.got_resume = false;
        if !self.dial(None) {
            return;
        }
        self.phase = Phase::Signaling;
        godot_print!("netplay: dialing {} ...", rtc::SIGNALING_URL);
    }

    /// Open a signaling socket, tagged with our identity (so the relay's /status lists us) and an
    /// optional `room` code. `None` = open matchmaking (relay's "default" room); `Some(code)` =
    /// re-pair with a specific opponent on reconnect. Returns false if the dial failed.
    fn dial(&mut self, room: Option<&str>) -> bool {
        let mut ws = WebSocketPeer::new_gd();
        let id = self.identity.get_cloned();
        let c = id.color;
        let hex = format!(
            "%23{:02x}{:02x}{:02x}",
            (c.r * 255.0) as u8,
            (c.g * 255.0) as u8,
            (c.b * 255.0) as u8
        );
        let mut url = format!(
            "{}?name={}&color={}&hash={}",
            rtc::SIGNALING_URL,
            url_escape(&id.name),
            hex,
            rtc::BUILD_HASH,
        );
        if let Some(code) = room {
            url.push_str(&format!("&room={}", url_escape(code)));
        }
        if ws.connect_to_url(&url) != godot::global::Error::OK {
            godot_error!("netplay: signaling dial failed");
            return false;
        }
        self.ws = Some(ws);
        self.sig = SigCounts::default(); // fresh tallies per match attempt
        true
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
            if ch.get_ready_state() == ChannelState::OPEN && self.net.is_none() {
                // On reconnect the guest must hold until the host's resume snapshot lands, or the two
                // rebuilt sessions would start from different states. First match (or host) has nothing
                // to wait for.
                let waiting_for_resume = self.phase == Phase::Reconnecting
                    && self.role == Some(Role::Guest)
                    && !self.got_resume;
                if !waiting_for_resume {
                    self.begin_session();
                }
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
                self.note_peer_build(rtc::dget_str(&d, "hash"));
                let sdp = rtc::dget_str(&d, "sdp");
                if let Some(mut pc) = self.pc.clone() {
                    pc.set_remote_description("offer", &sdp);
                }
            }
            // Host receives the guest's answer.
            "answer" => {
                self.sig.answer_in += 1;
                self.note_peer_build(rtc::dget_str(&d, "hash"));
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
            // Host mints the private reconnect room and relays the code; guest stores it so both
            // sides re-dial the SAME room if the transport drops later. Ignore once we already have one.
            "room" => {
                let code = rtc::dget_str(&d, "code");
                if !code.is_empty() && self.room.is_none() {
                    self.room = Some(Room { code, deadline_ms: None });
                }
            }
            // Reconnect resume: the host ships the sim state to start the rebuilt session from. The
            // host's snapshot is authoritative, so it overwrites ours; `got_resume` releases the
            // guest's `begin_session` gate.
            "resume" => {
                let b64 = rtc::dget_str(&d, "state");
                if let Some(snap) = decode_state(&b64) {
                    self.resume_snapshot = Some(snap);
                    self.got_resume = true;
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
            // First time as host: mint the private reconnect room and ship the code to the guest
            // over the relay (it forwards unknown frames verbatim). On a reconnect the room already
            // exists, so don't re-mint — both sides keep the original code.
            if self.room.is_none() {
                let code = mint_room_code(&self.identity.get_cloned().name);
                let mut d = VarDictionary::new();
                d.set("kind", "room");
                d.set("code", code.clone());
                if let Some(mut ws) = self.ws.clone() {
                    ws.send_text(&rtc::to_json(&d));
                }
                self.room = Some(Room { code, deadline_ms: None });
            } else if let Some(snap) = self.resume_snapshot {
                // Reconnect: ship our snapshot so both peers rebuild the session from the same state.
                let mut d = VarDictionary::new();
                d.set("kind", "resume");
                d.set("state", encode_state(&snap));
                if let Some(mut ws) = self.ws.clone() {
                    ws.send_text(&rtc::to_json(&d));
                }
            }
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
        match start_p2p::<Smash, _>(local_handle, remote, socket, rtc::INPUT_DELAY) {
            Ok(session) => {
                // Resume from the agreed snapshot on a reconnect; spawn fresh on a first match. Both
                // peers reach this with the SAME snapshot (the host's), so frame 0 of the rebuilt
                // session is identical and ggrs stays in sync.
                let state = self.resume_snapshot.unwrap_or_else(SimState::spawn);
                let game = SmashGame::from_state(state, self.tune.get());
                self.net = Some(Box::new(GgrsNetplay::new(session, game, local_handle)));
                self.state.set(state);
                self.local_handle = local_handle;
                self.phase = Phase::Running;
                if let Some(r) = self.room.as_mut() {
                    r.deadline_ms = None; // back in a match; close the reconnect window
                }
                godot_print!("netplay: channel open, rollback running (handle {local_handle})");
            }
            Err(e) => {
                godot_error!("netplay: session start failed: {e:?}");
                self.reset_offline();
            }
        }
    }

    /// Tear down all networking and return to single-player. Frees the room ("turn off").
    fn reset_offline(&mut self) {
        self.net = None;
        self.channel = None;
        self.pc = None;
        if let Some(mut ws) = self.ws.take() {
            ws.close();
        }
        self.role = None;
        self.room = None;
        self.resume_snapshot = None;
        self.got_resume = false;
        self.phase = Phase::Offline;
        godot_print!("netplay: offline");
    }

    /// Has the live transport died? ICE went to `failed`, or the ggrs data channel closed. (A merely
    /// `disconnected` connection can still recover ICE on its own, so it does NOT count here — only a
    /// definitive failure triggers the heavier room reconnect.)
    fn transport_dropped(&self) -> bool {
        let chan_closed = self
            .channel
            .as_ref()
            .map(|c| c.get_ready_state() == ChannelState::CLOSED)
            .unwrap_or(false);
        let conn_failed = self
            .pc
            .as_ref()
            .map(|p| p.get_connection_state() == ConnectionState::FAILED)
            .unwrap_or(false);
        chan_closed || conn_failed
    }

    /// Peer dropped mid-match: drop the dead transport but KEEP the room identity, re-dial the
    /// private room, and open the reconnect window. Both peers do this and re-pair with each other
    /// (only they know the code). Without a shared code (drop before it was exchanged) we can't
    /// rejoin, so fall straight to offline.
    fn begin_reconnect(&mut self) {
        let Some(code) = self.room.as_ref().map(|r| r.code.clone()) else {
            self.reset_offline();
            return;
        };
        godot_print!("netplay: peer dropped — reconnecting to room {code}");
        // Capture the latest sim state so the rebuilt session resumes here instead of from spawn.
        // Both peers capture; the new host's snapshot wins (it ships it over the relay).
        self.resume_snapshot = self
            .net
            .as_ref()
            .map(|n| *n.state())
            .or_else(|| Some(self.state.get()));
        self.got_resume = false;
        self.net = None;
        self.channel = None;
        self.pc = None;
        if let Some(mut ws) = self.ws.take() {
            ws.close();
        }
        self.role = None;
        if !self.dial(Some(&code)) {
            self.reset_offline();
            return;
        }
        if let Some(r) = self.room.as_mut() {
            r.deadline_ms = Some(now_ms() + RECONNECT_WINDOW_MS);
        }
        self.phase = Phase::Reconnecting;
    }

    /// One line describing where we are in the netplay lifecycle, shown top-left every frame.
    fn status_text(&self) -> String {
        // Version warnings ride in front of the lifecycle text: a stale local build (deploy happened
        // while this tab slept) or an opponent on a different build (their sim will diverge from ours).
        if self.stale_build {
            return "⚠ NEW BUILD LIVE  ·  reload the page to update".to_string();
        }
        if let Some(peer) = self.peer_build.as_ref() {
            if peer != rtc::BUILD_HASH {
                return format!(
                    "⚠ VERSION MISMATCH  ·  you {} vs opponent {} — both reload",
                    rtc::BUILD_HASH,
                    peer,
                );
            }
        }
        match self.phase {
            Phase::Offline => "OFFLINE  ·  tap to find a match".to_string(),
            Phase::Signaling => "SIGNALING…  ·  waiting for an opponent".to_string(),
            Phase::Running => {
                let who = match self.role {
                    Some(Role::Host) => "host",
                    Some(Role::Guest) => "guest",
                    None => "?",
                };
                format!("NETPLAY  ·  {who} (handle {})", self.local_handle)
            }
            Phase::Reconnecting => {
                let secs = self
                    .room
                    .as_ref()
                    .and_then(|r| r.deadline_ms)
                    .map(|d| d.saturating_sub(now_ms()).div_ceil(1000))
                    .unwrap_or(0);
                format!("RECONNECTING…  ·  waiting {secs}s for your opponent")
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

    /// Bottom damage panel: name + % per fighter, P1 anchored bottom-left, P2 bottom-right. The %
    /// tints from white toward red as damage climbs (the "about to die" read).
    /// Track the camera to keep both fighters framed: center on their midpoint, zoom out so the
    /// pair (plus margin) fits the viewport, clamp loosely to the stage, and ease toward the target
    /// so it glides instead of snapping. Mirrors melee/PM "shared camera". Render-only (no sim state).
    fn update_camera(&mut self) {
        let Some(mut cam) = self.cam.clone() else { return };
        let s = self.state.get();
        let active = (s.active as usize).max(1);
        // Bounding box over every live fighter; the camera frames the whole pack, not just a pair.
        let mut lo = gv(s.fighters[0].pos);
        let mut hi = lo;
        for f in &s.fighters[..active] {
            let p = gv(f.pos);
            lo = Vector2::new(lo.x.min(p.x), lo.y.min(p.y));
            hi = Vector2::new(hi.x.max(p.x), hi.y.max(p.y));
        }
        let mid = (lo + hi) * 0.5;
        let view = self.base().get_viewport_rect().size;
        let aspect = if view.y > 0.0 { view.x / view.y } else { 1.78 };
        // Portrait / near-square (a phone held upright, or a tall window): a wide whole-pack frame
        // zooms the action to a speck. Lean the focus onto the local player and tighten the zoom so
        // they stay big; landscape keeps the classic shared-camera framing.
        let portrait = aspect < 1.3;
        let local = gv(s.fighters[self.local_handle.min(active - 1)].pos);
        let focus = if portrait { mid.lerp(local, 0.7) } else { mid };

        // world span the camera must show: spread of the pack + breathing room.
        let span_x = (hi.x - lo.x) + 700.0;
        let span_y = (hi.y - lo.y) + 450.0;
        // Camera2D zoom is inverse: smaller zoom = wider view. Fit both axes, take the tighter.
        // Portrait floors the zoom higher (don't shrink to fit a far-away dummy) and allows a
        // closer max so a lone player on mobile fills the screen.
        let (zmin, zmax) = if portrait { (0.9, 1.7) } else { (0.65, 1.35) };
        let fit = (view.x / span_x).min(view.y / span_y).clamp(zmin, zmax);
        // Keep the focus near the stage so a launched player doesn't drag the view into the void.
        // Portrait pans further toward the edges so following the local player actually works.
        let (cx0, cx1) = if portrait { (250.0, 950.0) } else { (350.0, 850.0) };
        let target = Vector2::new(focus.x.clamp(cx0, cx1), focus.y.clamp(250.0, 640.0));
        let zoom = Vector2::splat(fit);
        let k = 0.12; // ease factor per frame
        let next_pos = cam.get_position().lerp(target, k);
        let next_zoom = cam.get_zoom().lerp(zoom, k);
        cam.set_position(next_pos);
        cam.set_zoom(next_zoom);
    }

    /// Build the on-screen touch gamepad: the floating analog stick (left) and the GameCube face
    /// cluster (right). Visuals only, built once; `update_touch` lays them out against the live
    /// viewport every frame and the `input` handler drives the sim. (Joining a match is the status
    /// chip's job now, top-left.)
    fn build_touch_ui(&mut self) {
        let mut layer = CanvasLayer::new_alloc();
        layer.set_layer(50); // above the world, below the egui debug panel

        // MENU tab: bottom-center, between the stick + face cluster. Opens the debug panel + pauses.
        let mut menu = Button::new_alloc();
        menu.set_text("☰ MENU");
        menu.add_theme_font_size_override("font_size", 28);
        let mcb = self.to_gd();
        menu.connect("pressed", &Callable::from_object_method(&mcb, "on_menu"));
        layer.add_child(&menu);
        self.menu_btn = Some(menu);

        // Floating stick: a faint ring + grey GameCube-ish knob, parked bottom-left until grabbed.
        let base = circle_panel(120.0, Color::from_rgba(0.1, 0.12, 0.18, 0.32), Color::from_rgba(0.8, 0.85, 1.0, 0.45));
        let knob = circle_panel(80.0, Color::from_rgba(0.62, 0.66, 0.74, 0.7), Color::from_rgba(0.9, 0.93, 1.0, 0.85));
        layer.add_child(&base);
        layer.add_child(&knob);
        self.stick_base = Some(base);
        self.stick_knob = Some(knob);

        // Diamond face cluster: a Polygon2D wedge + centered Label per quadrant. Polygons are
        // re-pointed each frame in update_touch (geometry is viewport-relative); here we just create
        // the nodes and color them. Labels are screen-space Controls positioned each frame.
        for q in &QUADS {
            let (r, g, bl) = q.color;
            let mut poly = Polygon2D::new_alloc();
            poly.set_color(Color::from_rgba(r, g, bl, 0.9));
            layer.add_child(&poly);
            self.quad_polys.push(poly);

            let mut lbl = Label::new_alloc();
            lbl.set_text(q.letter);
            lbl.set_horizontal_alignment(godot::global::HorizontalAlignment::CENTER);
            lbl.set_vertical_alignment(godot::global::VerticalAlignment::CENTER);
            lbl.add_theme_color_override("font_color", Color::from_rgb(0.06, 0.06, 0.08));
            layer.add_child(&lbl);
            self.quad_labels.push(lbl);
        }

        // Shorthop rectangle under the bottom (jump) tip.
        let mut sh = Panel::new_alloc();
        let mut sb = StyleBoxFlat::new_gd();
        sb.set_bg_color(Color::from_rgba(0.55, 0.60, 0.70, 0.9));
        sb.set_corner_radius_all(16);
        sb.set_border_width_all(3);
        sb.set_border_color(Color::from_rgba(0.0, 0.0, 0.0, 0.35));
        sh.add_theme_stylebox_override("panel", &sb);
        let mut shl = Label::new_alloc();
        shl.set_text("SHORTHOP");
        shl.set_horizontal_alignment(godot::global::HorizontalAlignment::CENTER);
        shl.set_vertical_alignment(godot::global::VerticalAlignment::CENTER);
        shl.add_theme_color_override("font_color", Color::from_rgb(0.06, 0.06, 0.08));
        sh.add_child(&shl);
        layer.add_child(&sh);
        self.shorthop_panel = Some(sh);
        self.shorthop_label = Some(shl);

        self.base_mut().add_child(&layer);
    }

    /// Per-frame: resolve the GameCube layout against the live viewport (anchors to the bottom
    /// corners under the thumbs), position/size every widget, publish hitboxes for `input`, and
    /// float the stick visual at the active finger.
    fn update_touch(&mut self) {
        let view = self.base().get_viewport_rect().size;
        let lay = touch_layout(view);

        // Only show the on-screen gamepad when the device actually needs it: a touchscreen is present
        // AND no controller is paired. Desktop (no touchscreen) or "touchscreen + gamepad" hides it,
        // so it never clutters a keyboard/pad session. Hidden -> no diamond published, so a stray
        // touch can't fire a button either.
        let show_touch = godot::classes::DisplayServer::singleton().is_touchscreen_available()
            && Input::singleton().get_connected_joypads().is_empty();
        for poly in self.quad_polys.iter_mut() {
            poly.set_visible(show_touch);
        }
        for label in self.quad_labels.iter_mut() {
            label.set_visible(show_touch);
        }
        if let Some(p) = self.shorthop_panel.as_mut() {
            p.set_visible(show_touch);
        }
        if let Some(b) = self.stick_base.as_mut() {
            b.set_visible(show_touch);
        }
        if let Some(k) = self.stick_knob.as_mut() {
            k.set_visible(show_touch);
        }
        if !show_touch {
            TOUCH_DIAMOND.set(None);
            // still update the menu button below, then bail out of the gamepad layout.
            if let Some(menu) = self.menu_btn.as_mut() {
                let w = (view.x * 0.16).clamp(150.0, 340.0);
                let h = 60.0_f32.max(view.y.min(view.x) * 0.06);
                menu.set_size(Vector2::new(w, h));
                menu.set_position(Vector2::new(view.x * 0.5 - w * 0.5, view.y - h - 12.0));
            }
            return;
        }

        // publish for the input handler's hit-tests
        TOUCH_DIAMOND.set(Some(lay));
        TOUCH_STICK_RAD.set(lay.stick_radius);
        TOUCH_STICK_ZONE_X.set(lay.stick_zone_x);

        // diamond wedges: re-point each polygon to its wedge, place + size each label at the wedge
        // centroid. Font scales with the diamond so it reads on any screen.
        let font = (lay.radius * 0.22) as i32;
        for i in 0..QUADS.len() {
            if let Some(poly) = self.quad_polys.get_mut(i) {
                let pts: Vec<Vector2> = quad_poly(i, lay.radius).into_iter().collect();
                poly.set_position(lay.center);
                poly.set_polygon(&PackedVector2Array::from(pts.as_slice()));
            }
            if let Some(label) = self.quad_labels.get_mut(i) {
                let lp = lay.center + quad_label_pos(i, lay.radius);
                let half = lay.radius * 0.5;
                label.set_size(Vector2::new(lay.radius, half));
                label.set_position(lp - Vector2::new(lay.radius * 0.5, half * 0.5));
                label.add_theme_font_size_override("font_size", font);
            }
        }
        // shorthop rectangle
        if let Some(panel) = self.shorthop_panel.as_mut() {
            panel.set_position(lay.shorthop.position);
            panel.set_size(lay.shorthop.size);
        }
        if let Some(label) = self.shorthop_label.as_mut() {
            label.set_size(lay.shorthop.size);
            label.add_theme_font_size_override("font_size", (lay.shorthop.size.y * 0.5) as i32);
        }

        // floating stick
        let active = TOUCH_FINGER.get() >= 0;
        let (sx, sy) = TOUCH_STICK.get();
        let origin = if active {
            let (ox, oy) = TOUCH_ORIGIN.get();
            Vector2::new(ox, oy)
        } else {
            lay.stick_center
        };
        if let Some(base) = self.stick_base.as_mut() {
            place_circle(base, origin, lay.stick_radius * 2.0);
        }
        if let Some(knob) = self.stick_knob.as_mut() {
            place_circle(knob, origin + Vector2::new(sx, sy) * lay.stick_radius, lay.stick_radius * 0.9);
        }
        // MENU tab pinned to the very bottom-center, in the HUD strip between the two clusters.
        if let Some(menu) = self.menu_btn.as_mut() {
            let w = (view.x * 0.16).clamp(150.0, 340.0);
            let h = 60.0_f32.max(view.y.min(view.x) * 0.06);
            menu.set_size(Vector2::new(w, h));
            menu.set_position(Vector2::new(view.x * 0.5 - w * 0.5, view.y - h - 12.0));
        }
    }

    fn update_hud(&mut self) {
        let s = self.state.get();
        let active = s.active as usize;
        let view = self.base().get_viewport_rect().size;
        // Lay the panels out evenly across the bottom strip; slot 0 hugs the left, the last slot the
        // right, the rest spaced between. Dormant slots (>= active) are hidden.
        let y = view.y - 150.0;
        let span = (view.x - 300.0).max(0.0); // inset both ends so the chips clear the screen edges
        for k in 0..sim::MAX_PLAYERS {
            let Some(mut l) = self.hud[k].clone() else { continue };
            if k >= active {
                l.set_visible(false);
                continue;
            }
            l.set_visible(true);
            let name = self.player_name(k);
            let pct = s.fighters[k].damage.round() as i32;
            l.set_text(&format!("{name}\n{pct}%"));
            let danger = (s.fighters[k].damage / 150.0).clamp(0.0, 1.0);
            l.add_theme_color_override("font_color", Color::from_rgb(1.0, 1.0 - danger, 1.0 - danger));
            let t = if active <= 1 { 0.0 } else { k as f32 / (active - 1) as f32 };
            l.set_position(Vector2::new(70.0 + t * span, y));
        }
    }

    /// Nametag/HUD text for fighter `idx`: slot 0 is the live local identity name, the rest are "Pn".
    fn player_name(&self, idx: usize) -> String {
        if idx == 0 { self.identity.get_cloned().name } else { slot_name(idx) }
    }

    /// Slot color for fighter `idx`: slot 0 wears the live local identity color; the rest take the
    /// fixed slot palette. Cosmetic only (never folded into the netplay checksum).
    fn slot_tint(&self, idx: usize) -> Color {
        if idx == 0 { self.identity.get_cloned().color } else { slot_color(idx) }
    }

    /// Drive every live fighter's sprite, hiding the dormant slots (>= active). Slot 0 is the node's
    /// own child, so it tracks the node position; slots 1.. are world-space siblings positioned here.
    fn render_fighters(&mut self, s: &SimState) {
        let active = s.active as usize;
        for k in 0..active {
            self.render_fighter(k, &s.fighters[k]);
        }
        for k in active..sim::MAX_PLAYERS {
            if let Some(mut a) = self.sprites[k].clone() {
                a.set_visible(false);
            }
        }
    }

    /// Drive one fighter's sprite: clip for the state, flip by facing, slot tint, and (for the
    /// world-space siblings, slot != 0) the feet position. Green tint while intangible — the
    /// universal "you can't be hit" read.
    fn render_fighter(&mut self, idx: usize, f: &Fighter) {
        let Some(mut a) = self.sprites[idx].clone() else { return };
        a.set_visible(true);
        if idx != 0 {
            a.set_global_position(gv(f.pos)); // slot 0 tracks the node; siblings position here
        }
        let clip = resolve_clip(&a, clip_for(f));
        if a.get_animation() != clip {
            a.play_ex().name(&clip).done(); // only restart when the clip actually changes
        }
        sync_attack_frame(&mut a, f, &self.tune.get());
        a.set_flip_h(f.facing < 0.0); // frog faces right by default
        a.set_rotation(wall_tilt(f));
        a.set_scale(Vector2::splat(self.base_scale[idx] * impact_pop(f))); // squash-pop on a connect
        a.set_modulate(sprite_tint(f, self.slot_tint(idx)));
    }

    /// Persist the identity when the panel changes it, and refresh P1's nametag to match.
    fn sync_identity(&mut self) {
        let id = self.identity.get_cloned();
        if id == self.saved_identity {
            return;
        }
        save_identity(&id);
        // Slot 0's tag wears the local name + color; every tag shares the local player's font size.
        for (k, tag) in self.tags.iter().enumerate() {
            let Some(mut tag) = tag.clone() else { continue };
            if k == 0 {
                tag.set_text(&id.name);
                tag.add_theme_color_override("font_color", id.color);
            }
            tag.add_theme_font_size_override("font_size", id.font_px);
        }
        self.saved_identity = id;
    }

    /// Apply a menu character pick to the live sprite. Cosmetic: roster index isn't networked, so
    /// each peer shows whatever skin it picked; swapping mid-match can't desync (art never folds
    /// into the checksum). Clamps to the roster and rebuilds frames/scale/offset on the right sprite.
    fn sync_charsel(&mut self) {
        let want = self.charsel.get_cloned();
        let roster = roster();
        for slot in 0..want.len() {
            let idx = (want[slot].max(0) as usize).min(roster.len() - 1);
            if idx == self.characters[slot] {
                continue;
            }
            self.characters[slot] = idx;
            let c = &roster[idx];
            if let Some(mut a) = self.sprites[slot].clone() {
                apply_character(&mut a, c);
                self.base_scale[slot] = c.scale;
            }
        }
    }

    /// Hover each live fighter's nametag a fixed height over its head; hide the dormant slots.
    fn place_tags(&mut self) {
        let s = self.state.get();
        let active = s.active as usize;
        for k in 0..sim::MAX_PLAYERS {
            let Some(mut tag) = self.tags[k].clone() else { continue };
            if k >= active {
                tag.set_visible(false);
                continue;
            }
            tag.set_visible(true);
            place_tag(&mut tag, s.fighters[k].pos);
        }
        self.place_edge_tags();
    }

    /// Show a screen-edge chip for any fighter launched out of view: name + a pointer arrow toward
    /// them + the off-screen distance. Hidden while the fighter is on-screen (the world nametag
    /// covers that case). Uses the live camera transform to map world feet -> screen pixels.
    fn place_edge_tags(&mut self) {
        let Some(cam) = self.cam.clone() else { return };
        let s = self.state.get();
        let view = self.base().get_viewport_rect().size;
        let cam_c = cam.get_position();
        let zoom = cam.get_zoom();
        let active = s.active as usize;
        let names: Vec<String> = (0..sim::MAX_PLAYERS).map(|k| self.player_name(k)).collect();
        const M: f32 = 56.0; // keep the chip this far inside the screen edge
        for k in 0..sim::MAX_PLAYERS {
            let Some(tag) = self.edge_tags[k].as_mut() else { continue };
            if k >= active {
                tag.set_visible(false);
                continue;
            }
            let world = gv(s.fighters[k].pos);
            let screen = (world - cam_c) * zoom + view * 0.5;
            let off = screen.x < M || screen.x > view.x - M || screen.y < M || screen.y > view.y - M;
            if !off {
                tag.set_visible(false);
                continue;
            }
            // dominant off-screen direction picks the arrow; distance is the raw off-stage pixels.
            let dx = if screen.x < M { M - screen.x } else if screen.x > view.x - M { screen.x - (view.x - M) } else { 0.0 };
            let dy = if screen.y < M { M - screen.y } else if screen.y > view.y - M { screen.y - (view.y - M) } else { 0.0 };
            let arrow = if dy >= dx {
                if screen.y < M { "▲" } else { "▼" }
            } else if screen.x < M {
                "◀"
            } else {
                "▶"
            };
            let dist = dx.max(dy).round() as i32;
            tag.set_text(&format!("{arrow} {} {dist}", names[k]));
            tag.set_visible(true);
            let sz = tag.get_size();
            let px = (screen.x - sz.x * 0.5).clamp(M, view.x - M - sz.x);
            let py = (screen.y - sz.y * 0.5).clamp(M, view.y - M - sz.y);
            tag.set_position(Vector2::new(px, py));
        }
    }
}

/// The sprite's modulate: hit flash > intangible green > the player's color.
fn sprite_tint(f: &Fighter, color: Color) -> Color {
    if f.hitlag > 0 {
        Color::from_rgb(1.0, 1.0, 1.0) // impact freeze: blow out to white (the hit "pop")
    } else if f.hitstun > 0 {
        Color::from_rgb(1.0, 0.95, 0.55) // hit flash while launched
    } else if f.intangible {
        Color::from_rgb(0.30, 0.95, 0.40)
    } else {
        color
    }
}

/// Impact squash-pop: on a connect both fighters freeze (`hitlag`) and we briefly scale the sprite up,
/// easing back as the freeze decays — the classic platform-fighter "pop". 1.0 (no change) otherwise.
/// Pure function of sim state, so it stays rollback-consistent.
fn impact_pop(f: &Fighter) -> f32 {
    if f.hitlag <= 0 {
        return 1.0;
    }
    1.0 + 0.20 * (f.hitlag as f32 / 8.0).min(1.0)
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

/// An off-stage chip: screen-pinned, the player's color on a dark rounded chip, hidden until the
/// fighter leaves the view. Text (name + arrow + distance) and position are set in `place_edge_tags`.
fn make_edge_tag(color: Color) -> Gd<Label> {
    let mut l = Label::new_alloc();
    l.add_theme_font_size_override("font_size", 26);
    l.add_theme_color_override("font_color", color);
    l.add_theme_constant_override("outline_size", 5);
    l.add_theme_color_override("font_outline_color", Color::from_rgba(0.04, 0.05, 0.09, 0.95));
    let mut bg = godot::classes::StyleBoxFlat::new_gd();
    bg.set_bg_color(Color::from_rgba(0.08, 0.10, 0.16, 0.85));
    bg.set_corner_radius_all(8);
    bg.set_content_margin_all(8.0);
    l.add_theme_stylebox_override("normal", &bg);
    l.set_visible(false);
    l
}

/// A bottom-HUD damage label: big, outlined, on a dark chip so the % reads over the white stage.
/// Color + text are refreshed every frame in `update_hud`.
fn make_hud_label(color: Color) -> Gd<Label> {
    let mut l = Label::new_alloc();
    l.add_theme_font_size_override("font_size", 38);
    l.add_theme_color_override("font_color", color);
    l.add_theme_constant_override("outline_size", 8);
    l.add_theme_color_override("font_outline_color", Color::from_rgba(0.04, 0.05, 0.09, 0.95));
    let mut bg = godot::classes::StyleBoxFlat::new_gd();
    bg.set_bg_color(Color::from_rgba(0.07, 0.09, 0.14, 0.80));
    bg.set_corner_radius_all(8);
    bg.set_content_margin_all(10.0);
    l.add_theme_stylebox_override("normal", &bg);
    l.set_z_index(100);
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
    // A fresh wall bounce overrides the launch state: show the tilted bounce frame while the window
    // (set in the sim's hitstun block) is open. Cosmetic only — the sim drives the physics.
    if f.wall_hit > 0 {
        return "wallbounce";
    }
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
        Dtilt => "dtilt",
        Nair => "nair",
        Dair => "dair",
        DashAttack => "run",
        Grab | GrabHold => "jab",   // reach / hold reuse the swing pose
        Grabbed => "skid",          // held: a stumbled pose
        Knockdown => "crouch",      // floored: low pose
        Getup => "crouch",          // rising
        TechInPlace => "skid",      // braced recovery
        TechRoll => "walk",         // rolling across the ground

        SpecialN | SpecialS | SpecialD => "jab", // reuse the swing pose until specials get art
        SpecialU => "jump",
        Helpless => "fall",
        // launched/hitstun: tumble through the air (the sim drives the slide); rising vs falling pose.
        Launched => {
            if f.vel.y < 0.0 {
                "jump"
            } else {
                "fall"
            }
        }
    }
}

/// Every clip name `clip_for` can ever return. The checker walks this so a character missing art for
/// one of them is reported once at load, not discovered as a blank frame mid-match. Keep in sync with
/// `clip_for` (adding a state there with a new clip name means adding it here).
const ALL_CLIPS: &[&str] = &[
    "idle", "walk", "run", "skid", "crouch", "jump", "fall", "hang", "climb", "jab", "nair", "dtilt",
    "dair", "wallbounce",
];

/// Where a missing clip falls back to. `clip_for` names a rich set of poses; a character need only
/// supply art for some of them, and the rest degrade down this chain to "idle" (which every
/// character must define) instead of rendering an empty frame. A name mapping to itself terminates.
fn clip_fallback(name: &str) -> &'static str {
    match name {
        "dtilt" => "crouch",
        "crouch" => "skid",
        "skid" => "idle",
        "nair" => "jab",
        "jab" => "idle",
        "dair" => "fall",
        "wallbounce" => "fall",
        "fall" => "jump",
        "jump" => "idle",
        "run" => "walk",
        "walk" => "idle",
        "hang" => "idle",
        "climb" => "idle",
        _ => "idle",
    }
}

/// Resolve a desired clip against what the sprite's SpriteFrames actually contains, walking the
/// fallback chain until a present animation is found ("idle" is the guaranteed terminal). Bounded so
/// a malformed chain can never spin. This is the generic safety net: `clip_for` may name a clip the
/// current character has no art for, and this picks the nearest pose it does have.
fn resolve_clip(a: &Gd<AnimatedSprite2D>, want: &str) -> StringName {
    let Some(sf) = a.get_sprite_frames() else { return StringName::from(want) };
    let mut name = want;
    for _ in 0..ALL_CLIPS.len() + 1 {
        if sf.has_animation(&StringName::from(name)) {
            return StringName::from(name);
        }
        let next = clip_fallback(name);
        if next == name {
            break;
        }
        name = next;
    }
    StringName::from("idle")
}

/// Multi-frame attack sync: lock the sprite's shown frame to the sim's per-state frame counter, so a
/// swing's art tracks its frame data (startup -> active -> recovery) instead of the clip's own fps
/// clock. Rollback-correct, since the shown frame is a pure function of sim state. No-op for
/// non-attacks (they keep their looping playback) and for single-frame clips. This is the hook that
/// makes genuine multi-frame attack animations land on-window; richer per-attack art just drops in.
fn sync_attack_frame(a: &mut Gd<AnimatedSprite2D>, f: &Fighter, t: &Tune) {
    let Some(atk) = sim::attack_for(t, f.state) else { return };
    let Some(sf) = a.get_sprite_frames() else { return };
    let name = a.get_animation();
    let n = sf.get_frame_count(&name);
    if n <= 1 {
        return; // a single-pose clip has nothing to step through
    }
    // Scrub the swing across the CURRENT live hitbox window, so a multi-box move replays the
    // animation once PER box: the 3-punch jab throws three visible punches, the stomp swings on each
    // of its three timed boxes. Before the first box: hold the wind-up frame; between/after boxes:
    // hold the last frame (the recoil) until the next box opens.
    let last = n as i64 - 1;
    let idx = if let Some(hb) = atk.box_at(f.frame) {
        let local = f.frame - hb.start; // 0..len within this box's own window
        ((local * n as i64) / hb.len.max(1)).clamp(0, last)
    } else if f.frame < atk.startup {
        0
    } else {
        last
    };
    a.set_frame(idx as i32);
}

/// Sprite tilt for a wall bounce: while the `wall_hit` window is open, lean the body off-vertical
/// (in the direction it's now travelling) so the bounce reads as a hard ricochet, easing back to
/// upright as the window decays. 0 outside the window. Cosmetic; never touches the sim.
fn wall_tilt(f: &Fighter) -> f32 {
    if f.wall_hit <= 0 {
        return 0.0;
    }
    let dir = if f.vel.x >= 0.0 { 1.0 } else { -1.0 };
    let decay = (f.wall_hit as f32 / 12.0).clamp(0.0, 1.0); // WALL_TILT_FRAMES in the sim
    dir * 0.45 * decay // up to ~25° at impact, unwinding to 0
}

/// Point an AnimatedSprite2D at a roster character: frames, scale, feet-offset, crisp filter, idle.
/// The single place sprite + character are wired, so ready() build and live char-swap stay in sync.
fn apply_character(a: &mut Gd<AnimatedSprite2D>, c: &Character) {
    let sf = build_frames(c);
    validate_character(c, &sf);
    a.set_sprite_frames(&sf);
    a.set_scale(Vector2::splat(c.scale));
    a.set_offset(Vector2::new(0.0, c.offset_y)); // feet on pos
    a.set_texture_filter(godot::classes::canvas_item::TextureFilter::NEAREST); // crisp pixels
    a.play_ex().name("idle").done();
}

/// Load a texture by path, returning None (with a warning) instead of aborting when the file is
/// missing. Lets `build_frames` skip a bad frame and fall back rather than killing the whole sprite.
fn try_tex(path: &str) -> Option<Gd<Texture2D>> {
    match try_load::<Texture2D>(path) {
        Ok(t) => Some(t),
        Err(_) => {
            godot_warn!("anim: missing texture {path}");
            None
        }
    }
}

/// Build a character's SpriteFrames from its clip table. Strip clips slice a sheet into cells;
/// Poses clips take one whole PNG per frame. Clip names match `clip_for` (choppy by design; the
/// art has no per-state poses, so several CharStates reuse one clip). Missing files are skipped with
/// a warning; any clip that ends up with zero frames is dropped so `resolve_clip` falls back past it.
fn build_frames(c: &Character) -> Gd<SpriteFrames> {
    let mut sf = SpriteFrames::new_gd();
    for clip in &c.clips {
        let name = clip.name.as_str();
        sf.add_animation(name);
        sf.set_animation_speed(name, clip.fps);
        sf.set_animation_loop(name, clip.looped);
        match &c.sheet {
            Sheet::Strip { frame_px } => {
                if let Some(sheet) = try_tex(&format!("res://assets/{}/{}.png", c.dir, clip.files[0])) {
                    // Cell width: explicit, else texture width / frame count. Cell height = full strip
                    // height, so non-square frames (common in Rivals rips) slice correctly.
                    let frames = clip.frames.max(1);
                    let fw = if *frame_px > 0.0 { *frame_px } else { sheet.get_width() as f32 / frames as f32 };
                    let fh = sheet.get_height() as f32;
                    for i in 0..frames {
                        let mut at = AtlasTexture::new_gd();
                        at.set_atlas(&sheet);
                        at.set_region(Rect2::new(Vector2::new(i as f32 * fw, 0.0), Vector2::new(fw, fh)));
                        sf.add_frame(name, &at.upcast::<Texture2D>());
                    }
                }
            }
            Sheet::Poses { prefix } => {
                for f in &clip.files {
                    if let Some(tex) = try_tex(&format!("res://assets/{}/{}_{}.png", c.dir, prefix, f)) {
                        sf.add_frame(name, &tex);
                    }
                }
            }
        }
        // a clip that loaded no frames is worse than absent: an empty animation renders nothing.
        // Drop it so `resolve_clip` walks past to a pose that has art.
        if sf.get_frame_count(name) == 0 {
            sf.remove_animation(name);
        }
    }
    sf
}

/// Startup checker: report any `clip_for` clip the built frames can't satisfy even after fallback,
/// and confirm "idle" exists (the terminal every fallback chain lands on). Logs once per character
/// at build; never panics — the game still runs on whatever art is present.
fn validate_character(c: &Character, sf: &Gd<SpriteFrames>) {
    if !sf.has_animation(&StringName::from("idle")) {
        godot_error!("anim: character '{}' has no 'idle' clip — fallbacks have no terminal", c.dir);
    }
    for &want in ALL_CLIPS {
        if sf.has_animation(&StringName::from(want)) {
            continue;
        }
        // walk the same chain resolve_clip uses; report what it will substitute.
        let mut name = want;
        for _ in 0..ALL_CLIPS.len() + 1 {
            let next = clip_fallback(name);
            if next == name || sf.has_animation(&StringName::from(next)) {
                name = next;
                break;
            }
            name = next;
        }
        godot_print!("anim: '{}' missing clip '{want}' -> using '{name}'", c.dir);
    }
}
