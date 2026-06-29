# Godot web export — plan + status

Goal: one engine for desktop AND browser. Retire the hand-rolled `web/` canvas client; the
browser page loads the real Godot game (same scene, same gdext sim, same look as desktop).

## The catch that shapes everything

Two different wasm targets are in play and they do not mix:

| | target | transport that works |
|---|---|---|
| canvas client (`web/`) | `wasm32-unknown-unknown` | matchbox_socket (wasm-bindgen/web-sys) ✅ |
| Godot web export | `wasm32-unknown-emscripten` | matchbox ❌ (wasm-bindgen can't build for emscripten) |

`matchbox_socket`'s browser backend is built on `wasm-bindgen` + `web-sys`, which only compile for
`wasm32-unknown-unknown`. Godot's web export (and any gdext `.wasm` it loads) is
`wasm32-unknown-emscripten`. So the moment we move rendering into Godot-web, the matchbox transport
we used for the canvas client stops compiling.

Consequence: split the work in two phases. Phase 1 gets the *game* into the browser (no netplay).
Phase 2 restores netplay using a transport that works under emscripten.

## Phase 1 — Godot web export rendering the game (single-player / solo)

This is "a separate page that loads the actual game." No networking yet; proves the engine path.

Prereqas (DONE this session):
- emscripten 3.1.74 installed at `~/emsdk` (matches Godot 4.7's build). `source ~/emsdk/emsdk_env.sh`.
- `rustup target add wasm32-unknown-emscripten`.

Remaining steps:
1. **gdext web feature** — `shell/Cargo.toml`: enable the godot crate's web feature. Single-threaded
   is far easier (threaded needs SharedArrayBuffer + COOP/COEP headers on the host):
   ```toml
   godot = { version = "0.4", features = ["experimental-wasm", "experimental-wasm-nothreads"] }
   ```
2. **emscripten link flags** — `rust-sim/.cargo/config.toml` (target-scoped, does not touch desktop):
   ```toml
   [target.wasm32-unknown-emscripten]
   rustflags = ["-Clink-args=-sSIDE_MODULE=2", "-Zlink-native-libraries=no"]
   ```
   gdext web currently needs **nightly** + build-std:
   ```
   cargo +nightly build -p smash_sim --target wasm32-unknown-emscripten \
     -Zbuild-std=std,panic_abort --release
   ```
   (The cdylib lands at `rust-sim/target/wasm32-unknown-emscripten/release/smash_sim.wasm`.)
3. **`.gdextension` web entry** — add a web library line so Godot loads the wasm side-module:
   ```
   [libraries]
   web.debug.wasm32   = "res://path/to/smash_sim.wasm"
   web.release.wasm32 = "res://path/to/smash_sim.wasm"
   ```
4. **Godot web export template** — install matching templates (4.7.stable). In the editor:
   Project > Install Export Templates, or `godot --install-templates`, or drop the
   `.export_templates/4.7.stable/` tpz contents in place. Add a "Web" export preset (nothreads).
5. **Export** — `godot --headless --path . --export-release "Web" build/web/index.html`.
   Output: `index.html` + `.wasm` (engine) + `.pck` (game) + `.js` + the gdext `.wasm` side module.
6. **Host** — rsync `build/web/` to the VPS at `/var/www/smash-godot`, nginx `location /game/`.
   Keep the canvas client at `/play/` until Phase 1 is verified, then swap.

Phase-1 risks (in order):
- gdext + emscripten toolchain match (nightly, build-std, emscripten version). #1 time sink.
- nothreads export vs. a threaded gdext build — must match (both nothreads).
- `.gdextension` path resolution for the web side-module.

## Phase 2 — browser netplay under Godot (the transport fork)

matchbox is out (emscripten). Options, best first:

A. **Godot WebRTC + ggrs core kept in Rust.** Use Godot's `WebRTCPeerConnection` /
   `WebRTCDataChannel` for the data path and `WebSocketPeer` for signaling. Implement
   `ggrs::NonBlockingSocket` in the gdext shell by draining/feeding Godot's data channel through
   gdext calls. Signaling: matchbox_server speaks its own protocol, so either (a) stand up a tiny
   generic WS signaling server (Godot's webrtc-minimal example pattern) next to matchbox, or (b)
   port the matchbox handshake to GDScript/Rust-over-Godot. The ggrs 0.13 core + `smash_net` Game
   loop are reused unchanged — only the socket impl changes.

B. **Two-wasm split.** Keep the matchbox+ggrs netcode in a `wasm32-unknown-unknown` wasm-bindgen
   module, run Godot-web for rendering, marshal inputs/state across via JS. Brittle; avoid.

C. **Desktop-only netplay for now.** Native matchbox already works; ship browser as single-player,
   netplay stays desktop. Lowest effort, defers the fork.

Recommendation: Phase 1 first (get the game in the browser at all), then Phase 2 option A.

## What to delete once Phase 1 lands
- `web/` crate (canvas client), its justfile targets (`web`, `web-dev`, `web-deploy`),
  `deploy/nginx/web.conf` -> replace with the Godot static host snippet.
- Keep `smash_net` (the ggrs core is reused; only the matchbox transport module gets swapped).

## Phase 1 status — the gdext wasm cdylib BUILDS

The blocker was version skew, not a wasm pointer-width bug. gdext 0.4.x is the **Godot 4.5** API
level (gdext Changelog: 0.4.0 added 4.5; 0.5.0 added 4.6; 4.7 is newer still). Building against the
Godot **4.7** binary made `api-custom` dump a 4.7 API that gdext 0.4.5 can't compile — first a
codegen panic (`mode_flags ... can only replace int with enum`), then godot-core source mismatches
(`&GString` vs `AsArg<StringName>`, `.ok_or_else` on a now-non-Option `get_root()`). Those are
4.6/4.7 API changes; godot-core compiles its whole source against the dumped API, so the tail is
unbounded — not worth hand-patching.

Fix: **match the versions — build against Godot 4.5.** Done, and it compiles clean.

Installed/configured (all verified):
- emsdk 3.1.74, `wasm32-unknown-emscripten` target, nightly + `rust-src`.
- Godot **4.5** standalone (do not disturb a system 4.7): binary + export templates.
- `shell/Cargo.toml`: egui/gdext-egui are `cfg(not(target_arch = "wasm32"))` (gdext-egui pulls the
  non-wasm `open` crate); the wasm target adds godot features `api-custom, experimental-wasm,
  lazy-function-tables`. `debug_ui`/`theme` modules cfg-gated off wasm in `shell/src/lib.rs`.
- `rust-sim/.cargo/config.toml`: emscripten link flags from the godot-rust web docs.

Build (compiles clean → `target/wasm32-unknown-emscripten/debug/smash_sim.wasm`):
```
source ~/emsdk/emsdk_env.sh
GODOT4_BIN="$HOME/godot45/Godot.app/Contents/MacOS/Godot" \
  cargo +nightly build -p smash_sim -Zbuild-std --target wasm32-unknown-emscripten   # add --release
```
Desktop build stays green (gdext's prebuilt 4.5 API; runs under a 4.7 editor too, runtime ≥ api).

Remaining to a playable export: add `web.debug.wasm32`/`web.release.wasm32` to `sim.gdextension`;
install the 4.5 templates; add a Web export preset; export; host. Then Phase 2 (Godot WebRTC
transport) for browser netplay.
