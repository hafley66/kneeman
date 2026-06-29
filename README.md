# platform fighter (prototype)

A deterministic 2D platform-fighter sim with browser rollback netplay. Pure Rust simulation
core + a Godot 4 shell for rendering and tuning. Built so two people can fight from their
browsers over peer-to-peer WebRTC.

## Layout

```
rust-sim/            cargo workspace
  core/  smash_core  PURE sim: step(state, [input; 2], tune) -> state. No engine, no IO.
                     glam vectors; compiles on native + wasm. The rollback-safe core.
  net/   smash_net   ggrs rollback glue: Config, wire input, save/load/advance, SyncTest,
                     matchbox WebRTC transport (behind the `matchbox`/`wasm` features).
  shell/ smash_sim   gdext cdylib Godot loads: samples input -> step -> renders.
scenes/              Godot scene (stage + fighters + debug panel)
deploy/              signaling server: nginx /ws + /play snippets, systemd unit, setup script
web/                 browser frontend: wasm + canvas, ggrs over matchbox (built by trunk)
```

## Determinism / rollback

The sim is a pure `state in -> state out` function on a fixed 60 Hz timestep, so it can be
snapshotted and re-simulated. Rollback is provided by [ggrs](https://crates.io/crates/ggrs);
transport is [matchbox](https://crates.io/crates/matchbox_socket) WebRTC P2P with a small
signaling server (the only server process; gameplay is direct peer-to-peer).

Run the determinism gate after any change to the sim:

```
cargo test -p smash_net      # SyncTest: rolls back every frame, checksums, fails on desync
```

## Build / run (desktop)

```
just rust                    # build the gdext crate
just run                     # launch the Godot scene
```

## Browser netplay (two tabs)

The web client runs the same deterministic sim in wasm, pairs two peers through the signaling
server, and rolls back over a direct WebRTC channel.

```
just web-dev                 # local: trunk serve, open two tabs at localhost:8080 to pair
just web                     # build web/dist (release); MATCHBOX_URL bakes the signaling room
just web-deploy              # build + rsync to the VPS, served at /play/
```

Live: open <https://hafley.codes/play/> in two tabs (or two machines); they auto-pair via
`?next=2` and fight. Controls: WASD/arrows move, Space jump, J attack, Shift shield, C short-hop.
Point at a local signaling server with `?url=ws://localhost:3536/x?next=2`.

## Signaling server

```
just vps-deploy              # installs matchbox_server + nginx /ws proxy + /play static, systemd unit
just vps-logs                # tail the service
```

The transport is matchbox WebRTC. ggrs is pinned at 0.13 while matchbox's own ggrs integration
targets 0.11, so `smash_net` uses matchbox's raw channel and implements ggrs 0.13's
`NonBlockingSocket` directly (see `net/src/lib.rs::transport`).

## Assets

Character art is CC0 (Pixel Adventure / Pixel Frog) and is not committed here; drop the
sprite strips under `assets/` locally. Test art only.
