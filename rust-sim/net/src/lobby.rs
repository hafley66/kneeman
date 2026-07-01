//! Pure lobby / signaling FSM: the deterministic core of the netplay handshake, lifted out of the
//! godot-coupled `KneeMan` node so the whole piping (role assignment, room minting, the tune/resume
//! gates, reconnect + resume) can be exercised headless.
//!
//! This is the same functional-core split as `za_warudo`: [`Lobby::reduce`] is a pure port of
//! `kneeman.rs`'s `matchmake_room` / `handle_signal` / the `pump_signaling` begin-session gate /
//! `begin_reconnect` / `reset_offline`. The shell adapts real godot I/O (`WebSocketPeer`,
//! `WebRtcPeerConnection`) into [`Event`]s and executes the returned [`Effect`]s; the test harness
//! swaps in a synchronous in-memory relay + PC model over the *same* interface, so the exact same
//! transition table that ships is what the harness validates.
//!
//! What is intentionally NOT modeled (cosmetic / not piping-critical): peer name/color signaling,
//! build-hash mismatch warnings, and the real async SDP/ICE payloads (here the SDP round-trip is
//! collapsed to synchronous `Send`/`Recv` of the right frame KINDS in the right ORDER, which is what
//! the handshake correctness actually turns on).

use smash_core::{SimState, Tune};

/// How long (ms) a match's room is held open after a transport drop, waiting for the peer to re-dial.
/// Mirrors `kneeman::RECONNECT_WINDOW_MS`.
pub const RECONNECT_WINDOW_MS: u64 = 12_000;

/// Which side of the pair we are. The relay assigns this in its `matched` frame (first dialer hosts).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Role {
    Host,
    Guest,
}

impl Role {
    /// ggrs local/remote handles, as in `kneeman`: host is player 0, guest is player 1.
    pub fn handles(self) -> (usize, usize) {
        match self {
            Role::Host => (0, 1),
            Role::Guest => (1, 0),
        }
    }
}

/// Which SDP slot a `SetRemote` effect targets.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SdpKind {
    Offer,
    Answer,
}

/// A relay frame (the typed form of `rtc`'s JSON `kind`s). Only these cross the signaling socket.
/// No `Debug`/`PartialEq`: the `Resume`/`Tune` payloads (`SimState`/`Tune`) don't implement them, and
/// the harness classifies frames with `matches!` rather than equality.
#[derive(Clone)]
pub enum Signal {
    /// Relay paired us; it tells each side which role to play.
    Matched { role: Role },
    /// Host -> guest SDP offer.
    Offer { sdp: String, hash: String },
    /// Guest -> host SDP answer.
    Answer { sdp: String, hash: String },
    /// A single ICE candidate (counted only; the transport handles the real thing).
    Ice { media: String, index: i32, name: String },
    /// Host mints the private reconnect room and ships the code; guest stores it.
    Room { code: String },
    /// Reconnect resume: the (new) host ships the authoritative sim snapshot to rebuild from.
    Resume { seed: SimState },
    /// Host ships its authoritative ruleset so both reducers run identical physics.
    Tune { tune: Tune },
    /// Peer left / relay closed the session.
    Bye,
}

/// Inbound events. The shell produces these from real I/O; the harness produces them synchronously.
#[derive(Clone)]
pub enum Event {
    /// User opened/joined a lobby. `Some(key)` dials that room (lobby key doubles as reconnect room);
    /// `None` is open matchmaking (relay's default room, host mints a private reconnect code on pair).
    Matchmake { room: Option<String> },
    /// User left the match.
    Leave,
    /// A relay frame arrived.
    Recv(Signal),
    /// The WebRTC data channel opened.
    ChannelOpen,
    /// The transport died mid-match. Carries the last sim state (shell reads it off the live ggrs
    /// session) so the reconnect can resume from it, plus the clock for the reconnect deadline.
    ChannelClosed { last_state: SimState, now_ms: u64 },
    /// The signaling socket closed while still handshaking (relay drop) -> fall to offline.
    WsClosed,
    /// Per-frame clock tick; only used to expire the reconnect window.
    Tick { now_ms: u64 },
}

/// Outbound side effects the shell (or harness) executes. Pure data; no I/O in this crate. No
/// `Debug`/`PartialEq` for the same reason as [`Signal`] (the `BeginSession` payload); classify with
/// `matches!`.
#[derive(Clone)]
pub enum Effect {
    /// Open a signaling socket to the relay for the given room (`None` = default matchmaking room).
    Dial { room: Option<String> },
    /// Close the signaling socket.
    CloseWs,
    /// Send a frame up the signaling socket.
    Send(Signal),
    /// Build the peer connection + data channel for this role.
    SetupPeer(Role),
    /// Apply a received SDP as the remote description.
    SetRemote { kind: SdpKind },
    /// Add a received ICE candidate.
    AddIce,
    /// Channel is open and all gates cleared: start the rollback session from `seed` under `tune`.
    BeginSession { seed: SimState, tune: Tune, role: Role },
    /// Drop the live transport (pc / channel / ggrs session).
    Teardown,
}

/// The transport lifecycle phase. Mirrors `kneeman::Phase`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    Offline,
    Signaling,
    Running,
    Reconnecting,
}

/// The match's room identity. `code` is the private room both peers re-dial to re-pair after a drop;
/// `deadline_ms` is `Some` only inside the reconnect window.
#[derive(Clone, Debug, PartialEq)]
pub struct Room {
    pub code: String,
    pub deadline_ms: Option<u64>,
}

/// The pure transport/lobby state machine. One instance per peer. Holds exactly the fields
/// `KneeMan` keeps for netplay, minus the godot handles (those live in the shell adapter).
#[derive(Clone)]
pub struct Lobby {
    pub phase: Phase,
    pub role: Option<Role>,
    pub room: Option<Room>,
    /// Our ruleset. The host ships this; the guest overwrites its own with the host's on `Tune`.
    pub tune: Tune,
    /// The snapshot to resume a rebuilt session from (reconnect only); `None` = spawn fresh.
    pub resume: Option<SimState>,
    pub got_tune: bool,
    pub got_resume: bool,
    pub channel_open: bool,
    /// Seed for deterministic room-code minting (the real code mixes a microsecond clock + name hash;
    /// here it's a fixed seed so the host mints a reproducible code the tests can assert on).
    mint_seed: u64,
    /// Our build hash, carried on Offer/Answer. Ideal conditions -> both sides equal; not acted on.
    build: String,
}

impl Lobby {
    /// A fresh offline lobby with our starting ruleset and a room-mint seed (host only uses the seed).
    pub fn new(tune: Tune, mint_seed: u64, build: impl Into<String>) -> Self {
        Self {
            phase: Phase::Offline,
            role: None,
            room: None,
            tune,
            resume: None,
            got_tune: false,
            got_resume: false,
            channel_open: false,
            mint_seed,
            build: build.into(),
        }
    }

    /// Advance the machine by one event, appending any side effects to `out`. Pure: the only state is
    /// `self`; all I/O is deferred to the returned effects.
    pub fn reduce(&mut self, ev: Event, out: &mut Vec<Effect>) {
        match ev {
            Event::Matchmake { room } => {
                // Ignore unless idle (mirrors `matchmake_room`'s `phase != Offline` guard).
                if self.phase != Phase::Offline {
                    return;
                }
                self.room = room.clone().map(|code| Room { code, deadline_ms: None });
                self.resume = None;
                self.got_resume = false;
                self.got_tune = false;
                self.channel_open = false;
                self.role = None;
                out.push(Effect::Dial { room });
                self.phase = Phase::Signaling;
            }
            Event::Leave | Event::WsClosed => self.reset_offline(out),
            Event::Recv(sig) => self.on_signal(sig, out),
            Event::ChannelOpen => {
                self.channel_open = true;
                self.try_begin(out);
            }
            Event::ChannelClosed { last_state, now_ms } => self.begin_reconnect(last_state, now_ms, out),
            Event::Tick { now_ms } => {
                if self.phase == Phase::Reconnecting {
                    let expired = self
                        .room
                        .as_ref()
                        .and_then(|r| r.deadline_ms)
                        .map(|d| now_ms > d)
                        .unwrap_or(true);
                    if expired {
                        self.reset_offline(out);
                    }
                }
            }
        }
    }

    /// Dispatch one relay frame (port of `handle_signal`).
    fn on_signal(&mut self, sig: Signal, out: &mut Vec<Effect>) {
        match sig {
            Signal::Matched { role } => self.setup_peer(role, out),
            // Guest applies the host's offer; the transport auto-generates the answer, which we send.
            Signal::Offer { .. } => {
                out.push(Effect::SetRemote { kind: SdpKind::Offer });
                out.push(Effect::Send(Signal::Answer {
                    sdp: "answer".into(),
                    hash: self.build.clone(),
                }));
            }
            // Host applies the guest's answer.
            Signal::Answer { .. } => out.push(Effect::SetRemote { kind: SdpKind::Answer }),
            Signal::Ice { .. } => out.push(Effect::AddIce),
            // Guest stores the host's minted reconnect room (ignore once we already have one).
            Signal::Room { code } => {
                if !code.is_empty() && self.room.is_none() {
                    self.room = Some(Room { code, deadline_ms: None });
                }
            }
            // Host's authoritative resume snapshot releases the guest's reconnect gate.
            Signal::Resume { seed } => {
                self.resume = Some(seed);
                self.got_resume = true;
                self.try_begin(out); // channel may already be open (snapshot arrived late)
            }
            // Host's authoritative ruleset releases the guest's tune gate.
            Signal::Tune { tune } => {
                self.tune = tune;
                self.got_tune = true;
                self.try_begin(out);
            }
            Signal::Bye => self.reset_offline(out),
        }
    }

    /// Build the peer + data channel; the host additionally ships room/resume/tune and the offer that
    /// starts the exchange (port of `setup_peer`). On a reconnect the room already exists, so the host
    /// ships the resume snapshot instead of minting a new room.
    fn setup_peer(&mut self, role: Role, out: &mut Vec<Effect>) {
        self.role = Some(role);
        out.push(Effect::SetupPeer(role));
        if role == Role::Host {
            if self.room.is_none() {
                let code = self.mint_code();
                out.push(Effect::Send(Signal::Room { code: code.clone() }));
                self.room = Some(Room { code, deadline_ms: None });
            } else if let Some(seed) = self.resume {
                out.push(Effect::Send(Signal::Resume { seed }));
            }
            // Always ship the ruleset so the guest's reducer runs identical physics (the desync fix).
            out.push(Effect::Send(Signal::Tune { tune: self.tune }));
            // create_offer -> (async in real life) the SDP offer we relay.
            out.push(Effect::Send(Signal::Offer {
                sdp: "offer".into(),
                hash: self.build.clone(),
            }));
        }
    }

    /// The begin-session gate from `pump_signaling`: the channel must be open AND (guest only) the
    /// tune must have landed, plus the resume on a reconnect. The host has nothing to wait for.
    fn try_begin(&mut self, out: &mut Vec<Effect>) {
        if !self.channel_open || self.phase == Phase::Running {
            return;
        }
        let waiting_resume =
            self.phase == Phase::Reconnecting && self.role == Some(Role::Guest) && !self.got_resume;
        let waiting_tune = self.role == Some(Role::Guest) && !self.got_tune;
        if waiting_resume || waiting_tune {
            return;
        }
        self.begin_session(out);
    }

    /// Flip to Running from the agreed seed (`resume` on reconnect, else spawn) under the current tune
    /// (the host's, which the guest adopted before this gate opened). Port of `begin_session`.
    fn begin_session(&mut self, out: &mut Vec<Effect>) {
        let Some(role) = self.role else { return };
        let seed = self.resume.unwrap_or_else(SimState::spawn);
        self.phase = Phase::Running;
        if let Some(r) = self.room.as_mut() {
            r.deadline_ms = None; // back in a match; close the reconnect window
        }
        out.push(Effect::BeginSession { seed, tune: self.tune, role });
    }

    /// Peer dropped mid-match: keep the room, capture the resume seed, re-dial, open the reconnect
    /// window (port of `begin_reconnect`). No shared room -> straight to offline.
    fn begin_reconnect(&mut self, last_state: SimState, now_ms: u64, out: &mut Vec<Effect>) {
        let Some(code) = self.room.as_ref().map(|r| r.code.clone()) else {
            self.reset_offline(out);
            return;
        };
        self.resume = Some(last_state);
        self.got_resume = false;
        self.got_tune = false;
        self.channel_open = false;
        self.role = None;
        out.push(Effect::Teardown);
        out.push(Effect::CloseWs);
        out.push(Effect::Dial { room: Some(code.clone()) });
        self.room = Some(Room { code, deadline_ms: Some(now_ms + RECONNECT_WINDOW_MS) });
        self.phase = Phase::Reconnecting;
    }

    /// Tear everything down and return to local play, freeing the room (port of `reset_offline`).
    fn reset_offline(&mut self, out: &mut Vec<Effect>) {
        out.push(Effect::Teardown);
        out.push(Effect::CloseWs);
        self.role = None;
        self.room = None;
        self.resume = None;
        self.got_resume = false;
        self.got_tune = false;
        self.channel_open = false;
        self.phase = Phase::Offline;
    }

    /// Deterministic reconnect-room code for the host (the shell mixes a real clock + name hash).
    fn mint_code(&self) -> String {
        format!("room-{:016x}", self.mint_seed)
    }
}
