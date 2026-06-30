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
use serde::de::DeserializeOwned;
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
        const SPECIAL        = 1 << 10;
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
    b.set(Buttons::SPECIAL, i.special);
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
        special: b.contains(Buttons::SPECIAL),
    }
}

/// Peer address type. With the `matchbox` feature this is matchbox's `PeerId` (real P2P); without
/// it (the SyncTest / default build) it is a plain `usize`, since SyncTest only uses local players
/// and never touches the address.
#[cfg(feature = "matchbox")]
pub type PeerAddr = matchbox_socket::PeerId;
#[cfg(not(feature = "matchbox"))]
pub type PeerAddr = usize;

/// The game-specific half of a rollback session, so the machinery below (`Game`, `GgrsConfig`,
/// `GgrsNetplay`, `start_p2p_n`) is generic and lifts into ANOTHER prototype unchanged: a new game
/// implements this once and gets the whole rollback layer. `State`/`Input` carry ggrs's bounds (the
/// snapshot is `Clone`; the wire input is `Copy + Default + serde`). `advance` is the deterministic
/// step both peers run on decoded inputs. See plans/netplay-as-crate.md.
pub trait RollbackSim: 'static {
    /// Rolled-back snapshot. Cheap to clone (Copy in practice) so saves don't allocate.
    type State: Clone + Send + Sync;
    /// One player's input on the wire (what ggrs transmits + predicts).
    type Input: Copy + Clone + PartialEq + Default + Serialize + DeserializeOwned + Send + Sync;
    /// Locked per-match config both peers share (e.g. tuning); never diverges mid-match.
    type Config: Clone;
    /// Fresh state for a new match.
    fn initial(cfg: &Self::Config) -> Self::State;
    /// Deterministic advance: same `(state, inputs, cfg)` -> same next state on every peer.
    fn advance(state: &Self::State, inputs: &[Self::Input], cfg: &Self::Config) -> Self::State;
    /// Determinism checksum ggrs compares across rollbacks.
    fn checksum(state: &Self::State) -> u128;
}

/// ggrs session config, generic over the game via [`RollbackSim`]: what travels (`S::Input`), what
/// gets snapshotted (`S::State`), how peers are addressed (`PeerAddr`).
pub struct GgrsConfig<S: RollbackSim>(std::marker::PhantomData<S>);

impl<S: RollbackSim> Config for GgrsConfig<S> {
    type Input = S::Input;
    type InputPredictor = PredictRepeatLast;
    type State = S::State;
    type Address = PeerAddr;
}

// ggrs needs Config: Debug (for GgrsEvent's derive). PhantomData can't derive it generically without
// S: Debug, so write it by hand — the config carries no data to print.
impl<S: RollbackSim> std::fmt::Debug for GgrsConfig<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GgrsConfig")
    }
}

/// This game (Smash) as a [`RollbackSim`]. Keeps the wire format (`NetInput`/`encode`/`decode`), the
/// pure `step`, and the field-fold `checksum` game-specific; everything else in this crate is generic.
pub struct Smash;

impl RollbackSim for Smash {
    type State = SimState;
    type Input = NetInput;
    type Config = Tune;
    fn initial(_cfg: &Tune) -> SimState {
        SimState::spawn()
    }
    fn advance(state: &SimState, inputs: &[NetInput], cfg: &Tune) -> SimState {
        // wire -> sim input (both peers decode identically), then the pure step over N players.
        let decoded: Vec<InputFrame> = inputs.iter().map(|n| decode(*n)).collect();
        let refs: Vec<&InputFrame> = decoded.iter().collect();
        step(state, &refs, cfg)
    }
    fn checksum(state: &SimState) -> u128 {
        checksum(state)
    }
}

/// Concrete instantiations for this game, so the shell names short aliases instead of `<Smash>`.
pub type SmashConfig = GgrsConfig<Smash>;
pub type SmashGame = Game<Smash>;
pub type SmashNetplay = GgrsNetplay<Smash>;

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
        fold(f.invuln as u64);
        fold(f.grab_link as u64);
        fold(f.grab_timer as u64);
        fold(f.tech_buf as u64);
        fold(f.tumble as u64);
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

/// The frontend-agnostic game wrapper ggrs drives, generic over the game via [`RollbackSim`]. Holds
/// the authoritative state + the locked config (both peers MUST share it or they desync — lock it at
/// match start). Apply the requests ggrs returns from `advance_frame`.
pub struct Game<S: RollbackSim> {
    pub state: S::State,
    pub cfg: S::Config,
}

impl<S: RollbackSim> Game<S> {
    /// Fresh match from the locked config.
    pub fn new(cfg: S::Config) -> Self {
        Self { state: S::initial(&cfg), cfg }
    }

    /// Resume from a given snapshot (reconnect: both peers rebuild from the same state).
    pub fn from_state(state: S::State, cfg: S::Config) -> Self {
        Self { state, cfg }
    }

    /// Service one batch of ggrs requests: save (clone state into the cell), load (restore), advance
    /// (every player's input by handle -> the deterministic `S::advance`). Player count comes from
    /// ggrs (`inputs.len()` == the session's num_players), so this is arity-agnostic.
    pub fn handle(&mut self, requests: Vec<GgrsRequest<GgrsConfig<S>>>) {
        for req in requests {
            match req {
                GgrsRequest::SaveGameState { cell, frame } => {
                    cell.save(frame, Some(self.state.clone()), Some(S::checksum(&self.state)));
                }
                GgrsRequest::LoadGameState { cell, .. } => {
                    self.state = cell.load().expect("ggrs load on a saved frame");
                }
                GgrsRequest::AdvanceFrame { inputs } => {
                    let wires: Vec<S::Input> = inputs.iter().map(|(i, _)| *i).collect();
                    self.state = S::advance(&self.state, &wires, &self.cfg);
                }
            }
        }
    }
}

/// One frame's result from driving a netplayed session forward.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Advance {
    /// Nothing advanced this frame (still synchronizing, or ggrs is too far ahead to predict).
    Stalled,
    /// The authoritative state moved one frame (read it via [`Netplay::state`]).
    Stepped,
    /// A peer dropped — the caller should open its reconnect window.
    PeerGone,
}

/// The shell-facing netplay seam: drive a networked match without knowing the model underneath.
/// [`GgrsNetplay`] is the rollback-p2p impl used today; a future server-authoritative client would
/// be a second impl, swapped in without touching the shell. The transport (mesh vs central relay)
/// is a lower seam — ggrs's `NonBlockingSocket` — and does not surface here. See plans/n-player.md.
pub trait Netplay {
    /// The rolled-back state the caller renders.
    type State;
    /// This peer's local input per frame (the wire form).
    type Input;
    /// Pump the transport + drain session events (must run every frame).
    fn poll(&mut self);
    /// True once the session is synchronized and actually stepping.
    fn running(&self) -> bool;
    /// Feed this peer's local input, advance one frame, and apply the resulting rollback requests.
    fn advance(&mut self, local: Self::Input) -> Advance;
    /// The latest authoritative state to render.
    fn state(&self) -> &Self::State;
}

/// Rollback-p2p netplay over ggrs, generic over the game. Owns the session + the authoritative
/// [`Game`] for the match's lifetime (created when the data channel opens, dropped on reset/reconnect).
pub struct GgrsNetplay<S: RollbackSim> {
    session: P2PSession<GgrsConfig<S>>,
    game: Game<S>,
    local_handle: usize,
    peer_gone: bool,
}

impl<S: RollbackSim> GgrsNetplay<S> {
    pub fn new(session: P2PSession<GgrsConfig<S>>, game: Game<S>, local_handle: usize) -> Self {
        Self { session, game, local_handle, peer_gone: false }
    }
}

impl<S: RollbackSim> Netplay for GgrsNetplay<S> {
    type State = S::State;
    type Input = S::Input;

    fn poll(&mut self) {
        self.session.poll_remote_clients();
        for ev in self.session.events() {
            if matches!(ev, GgrsEvent::Disconnected { .. }) {
                self.peer_gone = true;
            }
        }
    }

    fn running(&self) -> bool {
        self.session.current_state() == SessionState::Running
    }

    fn advance(&mut self, local: S::Input) -> Advance {
        if self.peer_gone {
            return Advance::PeerGone;
        }
        if !self.running() {
            return Advance::Stalled; // still synchronizing; hold the last rendered frame
        }
        if self.session.add_local_input(self.local_handle, local).is_err() {
            return Advance::Stalled;
        }
        match self.session.advance_frame() {
            Ok(reqs) => {
                self.game.handle(reqs);
                Advance::Stepped
            }
            Err(GgrsError::PredictionThreshold) => Advance::Stalled, // too far ahead; skip a frame
            Err(_) => Advance::Stalled,
        }
    }

    fn state(&self) -> &S::State {
        &self.game.state
    }
}

/// ggrs types a frontend needs to drive a P2P session, re-exported so a shell depends only on
/// `smash_net` (plus `ggrs` itself for the `NonBlockingSocket` trait it implements). Available on
/// every build — NOT gated on the matchbox feature, since the Godot WebRTC transport lives in the
/// shell and supplies its own socket.
pub use ggrs::{GgrsError, GgrsEvent, Message, NonBlockingSocket, P2PSession, SessionState};

/// Build a 2-player rollback session over a caller-supplied socket (the transport). Handle order is
/// FIXED so both peers agree: handle 0 = host, handle 1 = guest. `local_handle` says which one this
/// peer is; `remote_addr` is the address the socket tags inbound packets with (host sees the guest
/// as `remote_addr`, guest sees the host). `input_delay` frames trade latency for fewer rollbacks.
pub fn start_p2p<Sim, Sock>(
    local_handle: usize,
    remote_addr: PeerAddr,
    socket: Sock,
    input_delay: usize,
) -> Result<P2PSession<GgrsConfig<Sim>>, GgrsError>
where
    Sim: RollbackSim,
    Sock: NonBlockingSocket<PeerAddr> + 'static,
{
    start_p2p_n::<Sim, Sock>(local_handle, &[remote_addr; 2], socket, input_delay)
}

/// N-player rollback session. `addrs[h]` is the transport address ggrs tags handle `h`'s packets
/// with; the entry at `local_handle` is ignored (that handle is us, played locally). `num_players`
/// = `addrs.len()`. The socket is the transport — a p2p mesh or a central relay, both just impls of
/// `NonBlockingSocket`. Both peers MUST build the same `addrs` order so handles agree.
pub fn start_p2p_n<Sim, Sock>(
    local_handle: usize,
    addrs: &[PeerAddr],
    socket: Sock,
    input_delay: usize,
) -> Result<P2PSession<GgrsConfig<Sim>>, GgrsError>
where
    Sim: RollbackSim,
    Sock: NonBlockingSocket<PeerAddr> + 'static,
{
    let mut b = SessionBuilder::<GgrsConfig<Sim>>::new()
        .with_num_players(addrs.len())?
        .with_input_delay(input_delay);
    for (handle, &addr) in addrs.iter().enumerate() {
        let player = if handle == local_handle {
            PlayerType::Local
        } else {
            PlayerType::Remote(addr)
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
    use super::SmashConfig;
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
    ) -> Result<P2PSession<SmashConfig>, ggrs::GgrsError> {
        let mut builder = SessionBuilder::<SmashConfig>::new()
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
pub fn synctest_session(check_distance: usize) -> SyncTestSession<SmashConfig> {
    SessionBuilder::<SmashConfig>::new()
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

/// Input capture + deterministic replay. A live session dumps an `InputLog` (both peers' wire
/// inputs, one entry per frame); replaying it through the pure sim reproduces the match exactly.
/// Used for regression fixtures and as a second determinism check alongside the SyncTest.
pub mod replay {
    use super::{checksum, decode, step, NetInput, SimState, Tune};
    use serde::{Deserialize, Serialize};

    /// A recorded match: both players' wire inputs per frame. Serializable (bincode) so a captured
    /// session can be saved to bytes and replayed later without the engine or transport.
    #[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
    pub struct InputLog {
        pub frames: Vec<(NetInput, NetInput)>,
    }

    impl InputLog {
        /// Append one frame of both peers' inputs (the shell calls this each tick to capture).
        pub fn push(&mut self, p0: NetInput, p1: NetInput) {
            self.frames.push((p0, p1));
        }
        pub fn len(&self) -> usize {
            self.frames.len()
        }
        pub fn is_empty(&self) -> bool {
            self.frames.is_empty()
        }
        /// Serialize to a compact byte blob (a fixture file or a network/debug dump).
        pub fn to_bytes(&self) -> Vec<u8> {
            bincode::serialize(self).expect("serialize input log")
        }
        /// Reload a captured log; `None` if the bytes aren't a valid log.
        pub fn from_bytes(b: &[u8]) -> Option<Self> {
            bincode::deserialize(b).ok()
        }
    }

    /// Replay a log through the pure sim from spawn, returning the per-frame state checksums. Pure
    /// and deterministic: the same log + Tune yields the same checksum stream on every run/machine.
    pub fn replay(log: &InputLog, tune: &Tune) -> Vec<u128> {
        let mut s = SimState::spawn();
        let mut out = Vec::with_capacity(log.frames.len());
        for &(p0, p1) in &log.frames {
            let i0 = decode(p0);
            let i1 = decode(p1);
            s = step(&s, &[&i0, &i1], tune);
            out.push(checksum(&s));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replay::{replay, InputLog};

    /// Deterministic pseudo-random input stream so the sim visits many states (move, jump, dash,
    /// shield, attack, dodge) under rollback. Same seed -> same stream on both "peers".
    fn gen_input(seed: &mut u64) -> NetInput {
        *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let r = (*seed >> 33) as u32;
        NetInput {
            buttons: Buttons::from_bits_truncate((r & 0x7ff) as u16), // 11 button bits now
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

    /// Build a deterministic two-player input log (the "captured session" fixture).
    fn fixture_log(frames: usize) -> InputLog {
        let mut log = InputLog::default();
        let mut s0 = 0xfeed_face_u64;
        let mut s1 = 0x0bad_c0de_u64;
        for _ in 0..frames {
            log.push(gen_input(&mut s0), gen_input(&mut s1));
        }
        log
    }

    #[test]
    fn replay_is_deterministic_across_runs() {
        let t = Tune::default();
        let log = fixture_log(900);
        let a = replay(&log, &t);
        let b = replay(&log, &t);
        assert_eq!(a, b, "same log + tune must replay to the same checksum stream");
        assert_eq!(a.len(), 900);
    }

    #[test]
    fn fixture_survives_serialize_roundtrip_and_replays_identically() {
        let t = Tune::default();
        let log = fixture_log(600);
        // serialize the captured log to bytes and reload it (a fixture file would do the same).
        let bytes = log.to_bytes();
        let reloaded = InputLog::from_bytes(&bytes).expect("reload captured log");
        assert!(reloaded == log, "log survives a bincode round-trip"); // InputLog has no Debug
        assert_eq!(
            replay(&log, &t),
            replay(&reloaded, &t),
            "the reloaded fixture replays to the identical state stream",
        );
    }

    #[test]
    fn pure_replay_matches_the_ggrs_handler() {
        // The rollback handler (Game::handle) and a straight pure replay must agree frame-for-frame,
        // so the SyncTest path and offline play can never diverge.
        let t = Tune::default();
        let log = fixture_log(500);
        let pure = replay(&log, &t);

        let mut sess = synctest_session(2);
        let mut game = Game::new(t);
        let mut handler = Vec::with_capacity(log.frames.len());
        for (frame, &(p0, p1)) in log.frames.iter().enumerate() {
            sess.add_local_input(0, p0).expect("p0 input");
            sess.add_local_input(1, p1).expect("p1 input");
            let requests = sess
                .advance_frame()
                .unwrap_or_else(|e| panic!("desync at frame {frame}: {e:?}"));
            game.handle(requests);
            handler.push(checksum(&game.state));
        }
        assert_eq!(pure, handler, "pure replay and the ggrs handler must produce identical states");
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
            special: true,
        };
        let d = decode(encode(&i));
        assert_eq!(d.jump, i.jump);
        assert_eq!(d.attack, i.attack);
        assert_eq!(d.grab, i.grab);
        assert_eq!(d.special, i.special);
        assert_eq!(d.shield_held, i.shield_held);
        assert_eq!(d.down, i.down);
        assert!((d.dir - 1.0).abs() < 0.02);
        assert!((d.aim_y + 1.0).abs() < 0.02);
    }

    /// The reconnect resume ships a `SimState` snapshot as bincode; a mid-match state must survive
    /// the round-trip byte-for-byte, or the two rebuilt sessions start from different states.
    #[test]
    fn simstate_bincode_roundtrip_resumes_identically() {
        // Run a stepped, non-spawn state so fighters, items, tick and rng are all populated.
        let t = Tune::default();
        let log = fixture_log(300);
        let mut s = SimState::spawn();
        for &(p0, p1) in &log.frames {
            s = step(&s, &[&decode(p0), &decode(p1)], &t);
        }
        let bytes = bincode::serialize(&s).expect("serialize SimState");
        let back: SimState = bincode::deserialize(&bytes).expect("deserialize SimState");
        assert!(s == back, "snapshot must round-trip exactly"); // SimState has no Debug
        assert_eq!(checksum(&s), checksum(&back), "checksum must match after resume decode");
    }
}
