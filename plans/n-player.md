# N-player (up to 4): plan + progress

Target: up to 4 fighters in one match, local (couch) and netplay. Cap `MAX_PLAYERS = 4`
(canonical platform-fighter size). Central/server-authoritative netcode must be swappable in
later behind a trait — we are NOT committing to p2p mesh forever.

## Why a fixed array, not a Vec
`SimState` is `Copy` + serde and ggrs snapshots it every frame for rollback (`let mut n = *s`).
A `Vec` would kill `Copy` and heap-allocate every snapshot. So fighters stay a fixed
`[Fighter; MAX_PLAYERS]` with `active: u8` saying how many slots are live. `step` loops `0..active`.

## Two seams (so central-server swaps in without touching the shell)
```
shell (godot) ── drives ──► trait Netplay      (session MODEL)
                            ├─ GgrsNetplay      rollback p2p / star — exists today
                            └─ ServerNetplay    server-authoritative — later
                                  │
                                  └─ uses ──► trait Transport = ggrs::NonBlockingSocket<PeerAddr>
                                                ├─ MeshSocket    N data channels (p2p)
                                                └─ RelaySocket   one server addr, star
```
- **Transport** seam already exists: `start_p2p<S: NonBlockingSocket>` (net/lib.rs). A central RELAY
  is just a second `NonBlockingSocket` impl — still ggrs rollback, zero shell change.
- **Netplay** seam (new, slice 2): the shell holds `Box<dyn Netplay>` instead of
  `Option<P2PSession> + Option<Game>`. Lets a future server-authoritative model (no rollback) drop in.

## Build order
1. [DONE] **smash_core arity** — `MAX_PLAYERS=4`, `[Fighter; N]` + `active`, `step(&[&InputFrame])`,
   two-phase FSM (all advances, then all apply_acts) preserved, pairwise combat/grab as `n^2` loop
   over ordered pairs via `pair_mut`. `update_paths` loops `0..active`. 2-player path is
   byte-identical (spawn_slot keeps 480/720, `spawn()` = `spawn_n(2)`). 45 core + 6 net tests green.
2. **smash_net N + Netplay trait** — `start_session(N)`, extract `GgrsNetplay: Netplay`, generalize
   `Game::handle` decode loop from 2 to N, keep synctest + pure-replay determinism tests passing.
3. **shell local N** — render loop over `0..active` (kill the P1/P2 `render_anim`/`render_p2` split),
   per-fighter `[_; MAX_PLAYERS]` for sprites/hud/tags/trails, `controls::poll_player(n)` for couch
   3-4P. Netplay still 2 but routed through `Box<dyn Netplay>`.
4. **mesh transport + signaling** — `MeshSocket` (channel per peer), K-fill lobby in the relay
   (accumulate K then start), addressed SDP/ICE routing (who->whom, not "the one partner"). The big
   one; hardest to test (needs 3+ clients). `RelaySocket` (central star) is a cheap follow-on once
   the lobby speaks N.

## Notes / gotchas
- `grab_link` is a partner INDEX (`i8`), already N-safe; only the borrow-split helper changed.
- No fighter-vs-fighter body pushout exists (ECB collides stage only), so per-fighter `advance` is a
  clean loop; only combat + grab are `n^2`.
- Rollback cost grows with players (snapshot size, K-1 input streams, any peer's misprediction rolls
  everyone). Practical netplay ceiling ~4, good connections.
- KO/respawn: revisit blast-zone + stock logic for N (shell concern; sim `respawn` is per-fighter).
