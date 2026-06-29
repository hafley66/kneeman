//! Rollback glue for the pure sim. ggrs owns the frame loop; it calls `smash_core::step`
//! (possibly several times per frame when re-simulating after a late input). This crate has
//! NO engine and NO transport yet — just the ggrs `Config`, the wire input, the save/load/advance
//! handler, and a SyncTest that proves the sim is deterministic enough for rollback.
//!
//! Determinism gate: `cargo test -p smash_net` runs the SyncTest, which rolls back every frame
//! and compares state checksums. Any non-determinism in `step` makes it return MismatchedChecksum
//! and the test fails. Run it after touching the sim.

use ggrs::{Config, GgrsRequest, PlayerType, PredictRepeatLast, SessionBuilder, SyncTestSession};
use serde::{Deserialize, Serialize};
use smash_core::{step, InputFrame, SimState, Tune};

/// The only game data sent over the wire. Quantize the analog stick to `i8` (kills float-input
/// divergence + shrinks packets); pack the eight buttons into a bitmask. `Default` = no input,
/// which ggrs also uses for a disconnected player.
#[derive(Copy, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct NetInput {
    pub buttons: u8, // bit flags, see B_* below
    pub stick_x: i8, // dir   * 127
    pub stick_y: i8, // aim_y * 127
}

const B_JUMP: u8 = 1 << 0;
const B_JUMP_HELD: u8 = 1 << 1;
const B_SHORTHOP: u8 = 1 << 2;
const B_SHIELD_HELD: u8 = 1 << 3;
const B_SHIELD_PRESSED: u8 = 1 << 4;
const B_DOWN: u8 = 1 << 5;
const B_DOWN_PRESSED: u8 = 1 << 6;
const B_ATTACK: u8 = 1 << 7;

#[inline]
fn q(v: f32) -> i8 {
    (v.clamp(-1.0, 1.0) * 127.0) as i8
}

/// Sample -> wire. The shell calls this on the locally sampled `InputFrame` before handing it to
/// the session; the result is what travels to the peer.
pub fn encode(i: &InputFrame) -> NetInput {
    let mut b = 0u8;
    if i.jump {
        b |= B_JUMP;
    }
    if i.jump_held {
        b |= B_JUMP_HELD;
    }
    if i.shorthop {
        b |= B_SHORTHOP;
    }
    if i.shield_held {
        b |= B_SHIELD_HELD;
    }
    if i.shield_pressed {
        b |= B_SHIELD_PRESSED;
    }
    if i.down {
        b |= B_DOWN;
    }
    if i.down_pressed {
        b |= B_DOWN_PRESSED;
    }
    if i.attack {
        b |= B_ATTACK;
    }
    NetInput { buttons: b, stick_x: q(i.dir), stick_y: q(i.aim_y) }
}

/// Wire -> sim input. Both peers decode identically, so the sim sees identical floats.
pub fn decode(n: NetInput) -> InputFrame {
    let b = n.buttons;
    InputFrame {
        dir: n.stick_x as f32 / 127.0,
        aim_y: n.stick_y as f32 / 127.0,
        jump: b & B_JUMP != 0,
        jump_held: b & B_JUMP_HELD != 0,
        shorthop: b & B_SHORTHOP != 0,
        shield_held: b & B_SHIELD_HELD != 0,
        shield_pressed: b & B_SHIELD_PRESSED != 0,
        down: b & B_DOWN != 0,
        down_pressed: b & B_DOWN_PRESSED != 0,
        attack: b & B_ATTACK != 0,
    }
}

/// ggrs session config: what travels (NetInput), what gets snapshotted (SimState), how peers are
/// addressed (matchbox PeerId in M3 — for SyncTest the address type is unused, so any Hash+Eq).
#[derive(Debug)]
pub struct GgrsConfig;

impl Config for GgrsConfig {
    type Input = NetInput;
    type InputPredictor = PredictRepeatLast;
    type State = SimState;
    type Address = usize; // placeholder; becomes matchbox::PeerId for real P2P
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
        fold(f.buffered as u64);
        fold(f.buf_timer as u64);
        fold(f.buf_aim.x.to_bits() as u64);
        fold(f.buf_aim.y.to_bits() as u64);
        fold(f.aerial_buf as u64);
        fold(f.autohop_aerial as u64);
        fold(f.intangible as u64);
        fold(f.regrab_lock as u64);
        fold(f.ground_plat as u64);
        fold(f.attack_hit as u64);
        fold(f.hitlag as u64);
        fold(f.damage.to_bits() as u64);
        fold(f.hitstun as u64);
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
            buttons: (r & 0xff) as u8,
            stick_x: ((r >> 8) & 0xff) as i8,
            stick_y: ((r >> 16) & 0xff) as i8,
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
        };
        let d = decode(encode(&i));
        assert_eq!(d.jump, i.jump);
        assert_eq!(d.attack, i.attack);
        assert_eq!(d.shield_held, i.shield_held);
        assert_eq!(d.down, i.down);
        assert!((d.dir - 1.0).abs() < 0.02);
        assert!((d.aim_y + 1.0).abs() < 0.02);
    }
}
