//! Browser-native netplay transport: Godot WebRTC instead of matchbox. matchbox is wasm-bindgen, so
//! it cannot compile into the emscripten web export; Godot's `WebRtcPeerConnection` maps straight to
//! the browser's RTCPeerConnection and ships in the web template. The ggrs core (`smash_net`) is
//! reused unchanged — this only supplies the socket + the signaling handshake. See AGENTS.md / the
//! GODOT_WEB.md Phase 2 notes.
//!
//! Topology: two browsers reach the signaling relay (`wss://.../rtc`), get paired host+guest, trade
//! SDP/ICE, then open ONE negotiated data channel (both sides create id=1, so no
//! `data_channel_received` plumbing). ggrs runs peer-to-peer over that channel; the relay sees no
//! gameplay. Handle order is fixed host=0 / guest=1 so both peers agree (see `smash_net::start_p2p`).

use godot::classes::web_rtc_data_channel::WriteMode;
use godot::classes::{Json, WebRtcDataChannel};
use godot::prelude::*;

use smash_net::{Message, NonBlockingSocket};

/// The relay origin, derived not baked: on the web export it is the page origin (whatever host served
/// the game -- serve from staging and it follows), on native it is `SMASH_RELAY` else a dev default.
/// Memoized (the web path is a JS `location.origin` eval), so per-frame callers pay it once.
pub fn relay_base() -> String {
    thread_local! {
        static BASE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
    }
    BASE.with(|b| {
        b.borrow_mut()
            .get_or_insert_with(|| {
                crate::net::page_origin()
                    .or_else(|| std::env::var("SMASH_RELAY").ok())
                    .unwrap_or_else(|| "https://hafley.codes".into())
            })
            .clone()
    })
}

/// WebSocket signaling endpoint (`/rtc`); scheme follows the origin (https->wss, http->ws). The relay
/// pairs two dialers and forwards their SDP/ICE.
pub fn signaling_url() -> String {
    let ws = relay_base().replacen("https://", "wss://", 1).replacen("http://", "ws://", 1);
    format!("{ws}/rtc")
}

/// The relay's plain-HTTP status/JSON page (same host+route as signaling; it answers JSON without the
/// WebSocket upgrade header). The debug panel fetches this.
pub fn status_url() -> String {
    format!("{}/rtc", relay_base())
}

/// Netcode event firehose sink (POST). nginx forwards this to the signaling binary's `/ev`, which
/// stamps client IP + recv time and appends to a rotating JSON-lines log. See `analytics`.
pub fn event_url() -> String {
    format!("{}/ev", relay_base())
}

/// This build's git short hash, stamped by build.rs. Sent in the dial URL (so the relay's /status
/// lists which build each client runs) and in the SDP offer/answer (so the peer can flag a version
/// mismatch before it desyncs). On refocus the web client refetches /status and compares its own
/// `build_hash` to this to catch a stale cached wasm. "unknown" if built outside a git checkout.
pub const BUILD_HASH: &str = env!("BUILD_HASH");

/// Frames of input delay fed to ggrs. Higher = fewer rollbacks but more felt latency. 2 is a sane
/// LAN/decent-connection default.
pub const INPUT_DELAY: usize = 2;

/// Which side of the pair this peer is. The relay assigns it (first dialer = host). Fixes the ggrs
/// handle: host is player 0, guest is player 1.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Role {
    Host,
    Guest,
}

impl Role {
    pub fn from_str(s: &str) -> Option<Role> {
        match s {
            "host" => Some(Role::Host),
            "guest" => Some(Role::Guest),
            _ => None,
        }
    }
    /// (local ggrs handle, the remote's address as this peer's socket tags it).
    pub fn handles(self) -> (usize, usize) {
        match self {
            Role::Host => (0, 1),
            Role::Guest => (1, 0),
        }
    }
}

/// ggrs `NonBlockingSocket` over one Godot data channel. There is exactly one remote, so the address
/// is trivial: every inbound packet is tagged with `remote`, and `send_to` ignores its address arg
/// (only one place to send). bincode wire, same as the matchbox impl.
pub struct RtcSocket {
    pub channel: Gd<WebRtcDataChannel>,
    pub remote: usize,
}

impl NonBlockingSocket<usize> for RtcSocket {
    fn send_to(&mut self, msg: &Message, _addr: &usize) {
        let bytes = bincode::serialize(msg).expect("serialize ggrs message");
        let packet = PackedByteArray::from(bytes.as_slice());
        self.channel.put_packet(&packet);
    }

    fn receive_all_messages(&mut self) -> Vec<(usize, Message)> {
        let mut out = Vec::new();
        let n = self.channel.get_available_packet_count();
        for _ in 0..n {
            let packet = self.channel.get_packet();
            if let Ok(msg) = bincode::deserialize::<Message>(packet.as_slice()) {
                out.push((self.remote, msg));
            }
        }
        out
    }
}

/// Build the negotiated data-channel options dict: id 1, both sides create it (no signaling of the
/// channel itself), unreliable + unordered (ggrs has its own reliability layer).
pub fn data_channel_options() -> Dictionary {
    let mut d = Dictionary::new();
    d.set("negotiated", true);
    d.set("id", 1);
    d.set("ordered", false);
    d.set("maxRetransmits", 0);
    d
}

/// ICE config dict for `WebRtcPeerConnection::initialize`. One public STUN server — enough for most
/// home NATs. Symmetric NATs would need TURN (not provisioned).
pub fn ice_config() -> Dictionary {
    let mut server = Dictionary::new();
    server.set("urls", "stun:stun.l.google.com:19302");
    let mut cfg = Dictionary::new();
    cfg.set("iceServers", varray![server]);
    cfg
}

/// Make a channel binary-mode (ggrs ships raw bytes, not strings).
pub fn set_binary(channel: &mut Gd<WebRtcDataChannel>) {
    channel.set_write_mode(WriteMode::BINARY);
}

// --- tiny JSON helpers over Godot's Json (handles SDP newlines/escaping for us) ---------------

/// Serialize a `{kind: ...}` signaling message to a JSON string for the WebSocket.
pub fn to_json(d: &Dictionary) -> GString {
    Json::stringify(&d.to_variant())
}

/// Parse an inbound signaling frame into a Dictionary (empty on malformed input).
pub fn parse_json(text: &GString) -> Dictionary {
    let v = Json::parse_string(text);
    v.try_to::<Dictionary>().unwrap_or_default()
}

/// Read a string field, defaulting to "" so callers can match on it directly.
pub fn dget_str(d: &Dictionary, key: &str) -> String {
    d.get(key)
        .and_then(|v| v.try_to::<GString>().ok())
        .map(|g| g.to_string())
        .unwrap_or_default()
}

/// Read an int field (ICE candidate index), defaulting to 0.
pub fn dget_int(d: &Dictionary, key: &str) -> i64 {
    d.get(key).and_then(|v| v.try_to::<i64>().ok()).unwrap_or(0)
}
