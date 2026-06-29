//! Rollback glue for the pure sim. ggrs owns the frame loop; it calls `smash_core::step`
//! (possibly several times per frame when re-simulating after a late input). This crate has
//! NO engine and NO transport yet — just the ggrs `Config`, the wire input, the save/load/advance
//! handler, and a SyncTest that proves the sim is deterministic enough for rollback.
//!
//! Determinism gate: `cargo test -p smash_net` runs the SyncTest, which rolls back every frame
//! and compares state checksums. Any non-determinism in `step` makes it return MismatchedChecksum
//! and the test fails. Run it after touching the sim.

use bitflags::bitflags;
use ggrs::{Config, GgrsRequest, PlayerType, PredictRepeatLast, SessionBuilder, SyncTestSession};
use serde::{Deserialize, Serialize};
use smash_core::{step, InputFrame, SimState, Tune};

bitflags! {
    /// Every button edge/hold that travels over the wire, one flag each. `Buttons::empty()` = no
    /// input (also ggrs's disconnected-player default). bitflags' serde serializes the raw bits
    /// under bincode, so the packet stays a single `u16`.
    #[derive(Copy, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
    pub struct Buttons: u16 {
        const JUMP           = 1 << 0;
        const JUMP_HELD      = 1 << 1;
        const SHORTHOP       = 1 << 2;
        const SHIELD_HELD    = 1 << 3;
        const SHIELD_PRESSED = 1 << 4;
        const DOWN           = 1 << 5;
        const DOWN_PRESSED   = 1 << 6;
        const ATTACK         = 1 << 7;
        const GRAB           = 1 << 8;
        const ATTACK_HELD    = 1 << 9;
    }
}

/// The only game data sent over the wire. Quantize the analog stick to `i8` (kills float-input
/// divergence + shrinks packets); the buttons are a typed `Buttons` set. `Default` = no input,
/// which ggrs also uses for a disconnected player.
#[derive(Copy, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct NetInput {
    pub buttons: Buttons,
    pub stick_x: i8, // dir   * 127
    pub stick_y: i8, // aim_y * 127
}

#[inline]
fn q(v: f32) -> i8 {
    (v.clamp(-1.0, 1.0) * 127.0) as i8
}

/// Sample -> wire. The shell calls this on the locally sampled `InputFrame` before handing it to
/// the session; the result is what travels to the peer.
pub fn encode(i: &InputFrame) -> NetInput {
    let mut b = Buttons::empty();
    b.set(Buttons::JUMP, i.jump);
    b.set(Buttons::JUMP_HELD, i.jump_held);
    b.set(Buttons::SHORTHOP, i.shorthop);
    b.set(Buttons::SHIELD_HELD, i.shield_held);
    b.set(Buttons::SHIELD_PRESSED, i.shield_pressed);
    b.set(Buttons::DOWN, i.down);
    b.set(Buttons::DOWN_PRESSED, i.down_pressed);
    b.set(Buttons::ATTACK, i.attack);
    b.set(Buttons::GRAB, i.grab);
    b.set(Buttons::ATTACK_HELD, i.attack_held);
    NetInput { buttons: b, stick_x: q(i.dir), stick_y: q(i.aim_y) }
}

/// Wire -> sim input. Both peers decode identically, so the sim sees identical floats.
pub fn decode(n: NetInput) -> InputFrame {
    let b = n.buttons;
    InputFrame {
        dir: n.stick_x as f32 / 127.0,
        aim_y: n.stick_y as f32 / 127.0,
        jump: b.contains(Buttons::JUMP),
        jump_held: b.contains(Buttons::JUMP_HELD),
        shorthop: b.contains(Buttons::SHORTHOP),
        shield_held: b.contains(Buttons::SHIELD_HELD),
        shield_pressed: b.contains(Buttons::SHIELD_PRESSED),
        down: b.contains(Buttons::DOWN),
        down_pressed: b.contains(Buttons::DOWN_PRESSED),
        attack: b.contains(Buttons::ATTACK),
        attack_held: b.contains(Buttons::ATTACK_HELD),
        grab: b.contains(Buttons::GRAB),
    }
}

/// Peer address type. With the `matchbox` feature this is matchbox's `PeerId` (real P2P); without
/// it (the SyncTest / default build) it is a plain `usize`, since SyncTest only uses local players
/// and never touches the address.
#[cfg(feature = "matchbox")]
pub type PeerAddr = matchbox_socket::PeerId;
#[cfg(not(feature = "matchbox"))]
pub type PeerAddr = usize;

/// ggrs session config: what travels (NetInput), what gets snapshotted (SimState), how peers are
/// addressed (PeerAddr).
#[derive(Debug)]
pub struct GgrsConfig;

impl Config for GgrsConfig {
    type Input = NetInput;
    type InputPredictor = PredictRepeatLast;
    type State = SimState;
    type Address = PeerAddr;
}

/// Deterministic checksum over the whole state. ggrs compares these across rollbacks to catch
/// non-determinism. Folds every field's raw bits (floats via `to_bits`) through FNV-1a.
pub fn checksum(s: &SimState) -> u128 {
    let mut h: u64 = 0xcbf29ce4_84222325;
    let mut fold = |x: u64| {
        h ^= x;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    };
    for f in &s.fighters {
        fold(f.frame as u64);
        fold(f.pos.x.to_bits() as u64);
        fold(f.pos.y.to_bits() as u64);
        fold(f.vel.x.to_bits() as u64);
        fold(f.vel.y.to_bits() as u64);
        fold(f.state as u64);
        fold(f.facing.to_bits() as u64);
        fold(f.air_jumps as u64);
        fold(f.air_dodges as u64);
        fold(f.fast_falling as u64);
        fold(f.full_hop as u64);
        for s in &f.buf {
            fold(s.action as u64);
            fold(s.timer as u64);
            fold(s.aim.x.to_bits() as u64);
            fold(s.aim.y.to_bits() as u64);
        }
        fold(f.autohop_aerial as u64);
        fold(f.intangible as u64);
        fold(f.regrab_lock as u64);
        fold(f.ground_plat as u64);
        fold(f.attack_hit as u64);
        fold(f.hitlag as u64);
        fold(f.damage.to_bits() as u64);
        fold(f.hitstun as u64);
        fold(f.holding as u64);
        fold(f.coyote as u64);
    }
    fold(s.tick);
    fold(s.rng);
    for it in &s.items {
        fold(it.kind as u64);
        fold(it.pos.x.to_bits() as u64);
        fold(it.pos.y.to_bits() as u64);
        fold(it.vel.x.to_bits() as u64);
        fold(it.vel.y.to_bits() as u64);
        fold(it.owner as u64);
        fold(it.ammo as u64);
        fold(it.timer as u64);
        fold(it.facing.to_bits() as u64);
    }
    h as u128
}

/// The frontend-agnostic game wrapper ggrs drives. Holds the authoritative state + the locked
/// Tune (both peers MUST share the same Tune or they desync — lock it at match start). Apply the
/// requests ggrs returns from `advance_frame`.
pub struct Game {
    pub state: SimState,
    pub tune: Tune,
}

impl Game {
    pub fn new(tune: Tune) -> Self {
        Self { state: SimState::spawn(), tune }
    }

    /// Service one batch of ggrs requests: save (clone state into the cell), load (restore),
    /// advance (decode both inputs, call the pure step).
    pub fn handle(&mut self, requests: Vec<GgrsRequest<GgrsConfig>>) {
        for req in requests {
            match req {
                GgrsRequest::SaveGameState { cell, frame } => {
                    cell.save(frame, Some(self.state), Some(checksum(&self.state)));
                }
                GgrsRequest::LoadGameState { cell, .. } => {
                    self.state = cell.load().expect("ggrs load on a saved frame");
                }
                GgrsRequest::AdvanceFrame { inputs } => {
                    let i0 = decode(inputs[0].0);
                    let i1 = decode(inputs[1].0);
                    self.state = step(&self.state, [&i0, &i1], &self.tune);
                }
            }
        }
    }
}

/// ggrs types a frontend needs to drive a P2P session, re-exported so a shell depends only on
/// `smash_net` (plus `ggrs` itself for the `NonBlockingSocket` trait it implements). Available on
/// every build — NOT gated on the matchbox feature, since the Godot WebRTC transport lives in the
/// shell and supplies its own socket.
pub use ggrs::{GgrsError, Message, NonBlockingSocket, P2PSession, SessionState};

/// Build a 2-player rollback session over a caller-supplied socket (the transport). Handle order is
/// FIXED so both peers agree: handle 0 = host, handle 1 = guest. `local_handle` says which one this
/// peer is; `remote_addr` is the address the socket tags inbound packets with (host sees the guest
/// as `remote_addr`, guest sees the host). `input_delay` frames trade latency for fewer rollbacks.
pub fn start_p2p<S>(
    local_handle: usize,
    remote_addr: PeerAddr,
    socket: S,
    input_delay: usize,
) -> Result<P2PSession<GgrsConfig>, GgrsError>
where
    S: NonBlockingSocket<PeerAddr> + 'static,
{
    let mut b = SessionBuilder::<GgrsConfig>::new()
        .with_num_players(2)?
        .with_input_delay(input_delay);
    for handle in 0..2 {
        let player = if handle == local_handle {
            PlayerType::Local
        } else {
            PlayerType::Remote(remote_addr)
        };
        b = b.add_player(player, handle)?;
    }
    b.start_p2p_session(socket)
}

// ---------------------------------------------------------------------------------------------
// matchbox transport (M3). Only the WebRTC glue. matchbox's own `ggrs` feature pins ggrs 0.11, so
// we take its RAW channel and implement ggrs 0.13's `NonBlockingSocket` ourselves (the pattern the
// ggrs 0.13 docs spell out). The browser app (web crate) drives the message loop + frame loop.
// ---------------------------------------------------------------------------------------------
#[cfg(feature = "matchbox")]
pub mod transport {
    use super::GgrsConfig;
    use ggrs::{Message, NonBlockingSocket, SessionBuilder};
    // Re-export the ggrs + matchbox types a frontend names, so it only needs to depend on smash_net.
    pub use ggrs::{GgrsError, P2PSession, PlayerType, SessionState};
    pub use matchbox_socket::{MessageLoopFuture, PeerId, PeerState, WebRtcChannel, WebRtcSocket};
    use matchbox_socket::ChannelConfig;

    /// Wraps a matchbox channel so ggrs can send/receive its `Message`s over WebRTC. bincode for
    /// the wire; an unreliable+unordered channel (ggrs has its own reliability layer).
    pub struct Socket(pub WebRtcChannel);

    impl NonBlockingSocket<PeerId> for Socket {
        fn send_to(&mut self, msg: &Message, addr: &PeerId) {
            let bytes = bincode::serialize(msg).expect("serialize ggrs message");
            self.0.send(bytes.into_boxed_slice(), *addr);
        }

        fn receive_all_messages(&mut self) -> Vec<(PeerId, Message)> {
            self.0
                .receive()
                .into_iter()
                .filter_map(|(peer, packet)| bincode::deserialize(&packet).ok().map(|m| (peer, m)))
                .collect()
        }
    }

    /// Open the matchbox socket for a room. Returns the socket (poll `update_peers` each frame) and
    /// the message-loop future the caller MUST drive (`spawn_local` on wasm).
    pub fn connect(room_url: &str) -> (WebRtcSocket, MessageLoopFuture) {
        WebRtcSocket::builder(room_url)
            .add_channel(ChannelConfig::unreliable())
            .build()
    }

    /// Replicates matchbox's `players()` (which lives behind its ggrs-0.11 feature) for ggrs 0.13:
    /// our id plus every connected peer, sorted for a stable handle order across both peers.
    pub fn players(socket: &mut WebRtcSocket) -> Vec<PlayerType<PeerId>> {
        let Some(me) = socket.id() else {
            return vec![PlayerType::Local];
        };
        let mut ids: Vec<PeerId> = socket.connected_peers().chain(std::iter::once(me)).collect();
        ids.sort();
        ids.into_iter()
            .map(|id| if id == me { PlayerType::Local } else { PlayerType::Remote(id) })
            .collect()
    }

    /// Build the rollback session once `players()` reports everyone. Handle = index in the sorted
    /// player list (identical on both peers). `input_delay` frames trade latency for fewer rollbacks.
    pub fn start_session(
        players: Vec<PlayerType<PeerId>>,
        channel: WebRtcChannel,
        input_delay: usize,
    ) -> Result<P2PSession<GgrsConfig>, ggrs::GgrsError> {
        let mut builder = SessionBuilder::<GgrsConfig>::new()
            .with_num_players(players.len())?
            .with_input_delay(input_delay);
        for (handle, player) in players.into_iter().enumerate() {
            builder = builder.add_player(player, handle)?;
        }
        builder.start_p2p_session(Socket(channel))
    }
}

/// Build a 2-player SyncTest session (both players local, rolls back `check_distance` frames each
/// step and checksums). This is the determinism harness, not real networking.
pub fn synctest_session(check_distance: usize) -> SyncTestSession<GgrsConfig> {
    SessionBuilder::<GgrsConfig>::new()
        .with_num_players(2)
        .expect("2 players")
        .with_check_distance(check_distance)
        .add_player(PlayerType::Local, 0)
        .expect("p0")
        .add_player(PlayerType::Local, 1)
        .expect("p1")
        .start_synctest_session()
        .expect("synctest session")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic pseudo-random input stream so the sim visits many states (move, jump, dash,
    /// shield, attack, dodge) under rollback. Same seed -> same stream on both "peers".
    fn gen_input(seed: &mut u64) -> NetInput {
        *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let r = (*seed >> 33) as u32;
        NetInput {
            buttons: Buttons::from_bits_truncate((r & 0x3ff) as u16), // 10 button bits now
            stick_x: ((r >> 10) & 0xff) as i8,
            stick_y: ((r >> 18) & 0xff) as i8,
        }
    }

    #[test]
    fn synctest_runs_deterministic() {
        let mut sess = synctest_session(2);
        let mut game = Game::new(Tune::default());
        let mut s0 = 0x1234_5678u64;
        let mut s1 = 0x9abc_def0u64;

        // 1200 frames = 20s. Any non-determinism in step() trips MismatchedChecksum.
        for frame in 0..1200 {
            sess.add_local_input(0, gen_input(&mut s0)).expect("p0 input");
            sess.add_local_input(1, gen_input(&mut s1)).expect("p1 input");
            let requests = sess
                .advance_frame()
                .unwrap_or_else(|e| panic!("desync at frame {frame}: {e:?}"));
            game.handle(requests);
        }
    }

    #[test]
    fn encode_decode_roundtrip() {
        let i = InputFrame {
            dir: 1.0,
            aim_y: -1.0,
            jump: true,
            jump_held: true,
            shorthop: false,
            shield_held: true,
            shield_pressed: false,
            down: true,
            down_pressed: false,
            attack: true,
            attack_held: true,
            grab: true,
        };
        let d = decode(encode(&i));
        assert_eq!(d.jump, i.jump);
        assert_eq!(d.attack, i.attack);
        assert_eq!(d.grab, i.grab);
        assert_eq!(d.shield_held, i.shield_held);
        assert_eq!(d.down, i.down);
        assert!((d.dir - 1.0).abs() < 0.02);
        assert!((d.aim_y + 1.0).abs() < 0.02);
    }
}
