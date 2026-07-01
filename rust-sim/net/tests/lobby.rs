//! Deterministic end-to-end test of the lobby / signaling piping.
//!
//! Two [`Lobby`] FSMs are driven through an in-memory relay + a synchronous "PC model" that stands in
//! for the browser WebRTC stack: it forwards `Send` frames as `Recv` to the other peer, and fires
//! `ChannelOpen` on each side once that side's offer/answer round-trip is complete. No sockets, no
//! threads, no clock -- the whole handshake runs to a fixed point in a work-queue loop, so every run
//! is bit-identical. This exercises the exact transition table `kneeman.rs` will delegate to.

use smash_core::{SimState, Tune};
use smash_net::lobby::{Effect, Event, Lobby, Phase, Role, SdpKind, Signal};
use std::collections::VecDeque;

/// The two peers, indexed 0/1.
type PeerId = usize;

/// Per-peer transport bookkeeping the relay uses to decide when the data channel "opens". Mirrors the
/// real completion condition: a side is connected once it has both its local and remote SDP set.
#[derive(Default)]
struct PcModel {
    created_offer: bool, // host: emitted its offer
    got_answer: bool,    // host: applied the guest's answer
    got_offer: bool,     // guest: applied the host's offer
    sent_answer: bool,   // guest: emitted its answer
    opened: bool,        // ChannelOpen already fired (don't double-fire)
}

impl PcModel {
    /// Host completes when offer sent + answer applied; guest when offer applied + answer sent.
    fn complete(&self, role: Option<Role>) -> bool {
        match role {
            Some(Role::Host) => self.created_offer && self.got_answer,
            Some(Role::Guest) => self.got_offer && self.sent_answer,
            None => false,
        }
    }

    fn reset(&mut self) {
        *self = PcModel::default();
    }
}

/// Records what each peer started its session with, for the convergence assertions.
#[derive(Clone)]
struct Began {
    seed: SimState,
    tune_bytes: Vec<u8>,
    role: Role,
}

/// The whole two-peer world: FSMs, the relay's room registry, the PC models, and a work queue of
/// pending events. Draining the queue to empty is one "settle".
struct Harness {
    peers: [Lobby; 2],
    pc: [PcModel; 2],
    /// room code -> peers currently dialed into it, in dial order (first = host).
    rooms: std::collections::HashMap<String, Vec<PeerId>>,
    queue: VecDeque<(PeerId, Event)>,
    began: [Option<Began>; 2],
    /// Current sim state per peer, so a `ChannelClosed` can carry a realistic `last_state`. Bumped a
    /// little after BeginSession so a reconnect resumes from something other than spawn.
    live_state: [SimState; 2],
    clock: u64,
}

impl Harness {
    fn new(host_tune: Tune, guest_tune: Tune) -> Self {
        Harness {
            // mint_seed differs per peer but only the host's is used; fixed -> reproducible code.
            peers: [
                Lobby::new(host_tune, 0xA1, "build-1"),
                Lobby::new(guest_tune, 0xB2, "build-1"),
            ],
            pc: [PcModel::default(), PcModel::default()],
            rooms: std::collections::HashMap::new(),
            queue: VecDeque::new(),
            began: [None, None],
            live_state: [SimState::spawn(), SimState::spawn()],
            clock: 1_000,
        }
    }

    fn push(&mut self, peer: PeerId, ev: Event) {
        self.queue.push_back((peer, ev));
    }

    /// Drain the work queue, interpreting each peer's effects until nothing is pending.
    fn settle(&mut self) {
        while let Some((peer, ev)) = self.queue.pop_front() {
            let mut out = Vec::new();
            self.peers[peer].reduce(ev, &mut out);
            for eff in out {
                self.apply(peer, eff);
            }
        }
    }

    /// Interpret one effect from `peer`: this is the shell adapter's job, done synchronously.
    fn apply(&mut self, peer: PeerId, eff: Effect) {
        match eff {
            Effect::Dial { room } => {
                let code = room.unwrap_or_else(|| "default".to_string());
                let occupants = self.rooms.entry(code.clone()).or_default();
                if !occupants.contains(&peer) {
                    occupants.push(peer);
                }
                // Two peers in the room -> relay pairs them: first dialer hosts, second guests.
                if occupants.len() == 2 {
                    let (h, g) = (occupants[0], occupants[1]);
                    self.pc[h].reset();
                    self.pc[g].reset();
                    self.push(h, Event::Recv(Signal::Matched { role: Role::Host }));
                    self.push(g, Event::Recv(Signal::Matched { role: Role::Guest }));
                }
            }
            Effect::CloseWs => {
                // Leave every room this peer was dialed into.
                for occ in self.rooms.values_mut() {
                    occ.retain(|&p| p != peer);
                }
            }
            Effect::Send(sig) => self.relay(peer, sig),
            Effect::SetupPeer(_role) => {}
            Effect::SetRemote { kind } => match kind {
                SdpKind::Offer => {
                    self.pc[peer].got_offer = true;
                    self.maybe_open(peer);
                }
                SdpKind::Answer => {
                    self.pc[peer].got_answer = true;
                    self.maybe_open(peer);
                }
            },
            Effect::AddIce => {}
            Effect::Teardown => {}
            Effect::BeginSession { seed, tune, role } => {
                self.began[peer] = Some(Began {
                    seed,
                    tune_bytes: bincode::serialize(&tune).unwrap(),
                    role,
                });
                // Adopt the agreed seed, then advance a hair so a later reconnect resumes from a
                // non-spawn state we can distinguish.
                let mut s = seed;
                s.tick = s.tick.wrapping_add(7);
                self.live_state[peer] = s;
            }
        }
    }

    /// Forward a frame to the OTHER peer sharing a room, and drive PC completion bookkeeping.
    fn relay(&mut self, from: PeerId, sig: Signal) {
        // Track the sender's local-SDP progress.
        match &sig {
            Signal::Offer { .. } => self.pc[from].created_offer = true,
            Signal::Answer { .. } => self.pc[from].sent_answer = true,
            _ => {}
        }
        self.maybe_open(from);

        let Some(&to) = self
            .rooms
            .values()
            .find(|occ| occ.contains(&from) && occ.len() == 2)
            .map(|occ| occ.iter().find(|&&p| p != from).unwrap())
        else {
            return; // peer not present (already left) -> drop, as a real relay would
        };
        self.push(to, Event::Recv(sig));
    }

    /// Fire ChannelOpen for a peer the moment its SDP round-trip is complete (once).
    fn maybe_open(&mut self, peer: PeerId) {
        if !self.pc[peer].opened && self.pc[peer].complete(self.peers[peer].role) {
            self.pc[peer].opened = true;
            self.push(peer, Event::ChannelOpen);
        }
    }

    // --- assertion helpers ---------------------------------------------------------------------

    fn phase(&self, peer: PeerId) -> Phase {
        self.peers[peer].phase
    }
}

/// Happy path: two same-key peers pair, exchange the whole handshake, and both reach Running with the
/// host's role/tune/seed. This is the "made the same lobby on both, neither joined" bug's regression.
#[test]
fn two_peers_same_lobby_both_reach_running() {
    let mut h = Harness::new(Tune::default(), Tune::default());
    h.push(0, Event::Matchmake { room: Some("lobby-v1".into()) });
    h.push(1, Event::Matchmake { room: Some("lobby-v1".into()) });
    h.settle();

    assert_eq!(h.phase(0), Phase::Running, "host never reached Running");
    assert_eq!(h.phase(1), Phase::Running, "guest never reached Running");

    let host = h.began[0].as_ref().expect("host never began a session");
    let guest = h.began[1].as_ref().expect("guest never began a session");
    assert_eq!(host.role, Role::Host);
    assert_eq!(guest.role, Role::Guest);
    // Both start from the SAME seed (fresh spawn here) -> ggrs frame 0 is identical. `SimState` has
    // no `Debug`, so compare with `==` rather than `assert_eq!`.
    assert!(host.seed == guest.seed, "peers seeded from different states");
}

/// The tune handshake: the guest boots with a DIFFERENT ruleset, and must adopt the host's before it
/// starts its session. This is the desync fix (option 2: host ships Tune for custom rulesets).
#[test]
fn guest_adopts_host_tune_before_session_start() {
    let host_tune = Tune::default();
    let mut custom = Tune::default();
    custom.gravity *= 1.5; // guest's local (Feel-edited) ruleset differs
    assert_ne!(
        bincode::serialize(&host_tune).unwrap(),
        bincode::serialize(&custom).unwrap(),
        "test setup: tunes must differ"
    );

    let mut h = Harness::new(host_tune, custom);
    h.push(0, Event::Matchmake { room: Some("lobby-v1".into()) });
    h.push(1, Event::Matchmake { room: Some("lobby-v1".into()) });
    h.settle();

    let host = h.began[0].as_ref().unwrap();
    let guest = h.began[1].as_ref().unwrap();
    assert_eq!(
        host.tune_bytes, guest.tune_bytes,
        "guest started its session on its own tune, not the host's -> desync"
    );
    assert_eq!(
        guest.tune_bytes,
        bincode::serialize(&host_tune).unwrap(),
        "the adopted tune is not the host's"
    );
}

/// The gate must HOLD: if the channel opens before the tune frame lands, the guest may not begin.
/// Drives the events in the adverse order (ChannelOpen first) and checks the guest stays in Signaling
/// until Tune arrives.
#[test]
fn guest_waits_for_tune_even_if_channel_opens_first() {
    let mut guest = Lobby::new(Tune::default(), 0xB2, "build-1");
    let mut out = Vec::new();

    guest.reduce(Event::Matchmake { room: Some("lobby-v1".into()) }, &mut out);
    guest.reduce(Event::Recv(Signal::Matched { role: Role::Guest }), &mut out);
    guest.reduce(Event::ChannelOpen, &mut out); // channel up, but no tune yet
    assert_eq!(guest.phase, Phase::Signaling, "guest began before tune landed");
    assert!(
        !out.iter().any(|e| matches!(e, Effect::BeginSession { .. })),
        "guest emitted BeginSession before the tune gate cleared"
    );

    guest.reduce(Event::Recv(Signal::Tune { tune: Tune::default() }), &mut out);
    assert_eq!(guest.phase, Phase::Running, "tune landed but guest never began");
    assert!(out.iter().any(|e| matches!(e, Effect::BeginSession { .. })));
}

/// Reconnect: kill the channel mid-match. Both peers re-dial the SAME room, re-pair, and resume from
/// the host's captured snapshot (not spawn). Exercises begin_reconnect + the resume gate.
#[test]
fn channel_drop_reconnects_and_resumes_from_host_snapshot() {
    let mut h = Harness::new(Tune::default(), Tune::default());
    h.push(0, Event::Matchmake { room: Some("lobby-v1".into()) });
    h.push(1, Event::Matchmake { room: Some("lobby-v1".into()) });
    h.settle();
    assert_eq!(h.phase(0), Phase::Running);
    assert_eq!(h.phase(1), Phase::Running);
    let host_state = h.live_state[0];

    // Transport dies on both sides at the same tick.
    let now = h.clock;
    h.push(0, Event::ChannelClosed { last_state: h.live_state[0], now_ms: now });
    h.push(1, Event::ChannelClosed { last_state: h.live_state[1], now_ms: now });
    h.settle();

    // Back in a match, resumed from the host's authoritative snapshot.
    assert_eq!(h.phase(0), Phase::Running, "host didn't recover");
    assert_eq!(h.phase(1), Phase::Running, "guest didn't recover");
    let guest = h.began[1].as_ref().unwrap();
    assert!(
        guest.seed == host_state,
        "guest resumed from its own state, not the host's snapshot"
    );
}

/// The reconnect window: peer never comes back. A Tick past the deadline drops the lone peer to
/// Offline instead of hanging in Reconnecting forever.
#[test]
fn reconnect_window_expires_to_offline() {
    let mut lobby = Lobby::new(Tune::default(), 0xA1, "build-1");
    let mut out = Vec::new();

    // Get to Running as host (mint a room), then lose the channel.
    lobby.reduce(Event::Matchmake { room: None }, &mut out);
    lobby.reduce(Event::Recv(Signal::Matched { role: Role::Host }), &mut out);
    lobby.reduce(Event::ChannelOpen, &mut out);
    assert_eq!(lobby.phase, Phase::Running);
    out.clear();

    lobby.reduce(Event::ChannelClosed { last_state: SimState::spawn(), now_ms: 1_000 }, &mut out);
    assert_eq!(lobby.phase, Phase::Reconnecting);

    // Within the window: still reconnecting.
    lobby.reduce(Event::Tick { now_ms: 1_000 + smash_net::lobby::RECONNECT_WINDOW_MS - 1 }, &mut out);
    assert_eq!(lobby.phase, Phase::Reconnecting);

    // Past the window: offline.
    lobby.reduce(Event::Tick { now_ms: 1_000 + smash_net::lobby::RECONNECT_WINDOW_MS + 1 }, &mut out);
    assert_eq!(lobby.phase, Phase::Offline, "reconnect window never expired");
    assert!(lobby.room.is_none(), "room not freed on give-up");
}

/// Open matchmaking (room = None): the host mints a private reconnect room and ships it; the guest
/// stores the same code, so a later reconnect can re-pair.
#[test]
fn open_matchmaking_mints_and_propagates_room_code() {
    let mut h = Harness::new(Tune::default(), Tune::default());
    h.push(0, Event::Matchmake { room: None });
    h.push(1, Event::Matchmake { room: None });
    h.settle();

    assert_eq!(h.phase(0), Phase::Running);
    assert_eq!(h.phase(1), Phase::Running);
    let host_room = h.peers[0].room.as_ref().map(|r| r.code.clone());
    let guest_room = h.peers[1].room.as_ref().map(|r| r.code.clone());
    assert!(host_room.is_some(), "host never minted a room");
    assert_eq!(host_room, guest_room, "guest didn't adopt the host's reconnect room");
}

/// Determinism: the happy path replayed twice lands on byte-identical begin-session seeds. Guards the
/// whole harness against hidden ordering nondeterminism (HashMap iteration, etc.).
#[test]
fn handshake_is_deterministic_across_runs() {
    fn run() -> (SimState, SimState) {
        let mut h = Harness::new(Tune::default(), Tune::default());
        h.push(0, Event::Matchmake { room: Some("lobby-v1".into()) });
        h.push(1, Event::Matchmake { room: Some("lobby-v1".into()) });
        h.settle();
        (h.began[0].as_ref().unwrap().seed, h.began[1].as_ref().unwrap().seed)
    }
    assert!(run() == run(), "handshake seeds differ across identical runs");
}
