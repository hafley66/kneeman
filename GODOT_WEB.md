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

## Session status (left here for next pickup)
- emsdk 3.1.74 + `wasm32-unknown-emscripten` target + nightly `rust-src`: installed.
- `rust-sim/.cargo/config.toml` (emscripten link flags) + `shell/Cargo.toml` godot
  `experimental-wasm,experimental-wasm-nothreads` features: added. Desktop build still green
  (verified — both are no-ops off the wasm target).
- `cargo +nightly build -p smash_sim --target wasm32-unknown-emscripten -Zbuild-std=std,panic_abort`
  ATTEMPTED. Two concrete blockers hit (both must be fixed before an export is possible):

  1. **`gdext-egui` is not wasm-portable.** It pulls the `open` crate, which hard-errors:
     `error: open is not supported on this platform`. The debug overlay (egui/gdext-egui) is
     desktop-only. FIX: cfg-gate egui out of the wasm build — move `egui`, `gdext-egui`,
     `futures-signals` and the `debug_ui` module behind `#[cfg(not(target_arch = "wasm32"))]`
     (or a `desktop` feature that's default-on, off for web). The sim + renderer don't need egui.

  2. **`godot-ffi` const-eval overflows on 32-bit wasm.** The generated `gdextension_interface.rs`
     computes struct field offsets that underflow on `wasm32` (32-bit pointers), e.g.
     `attempt to compute 12_usize - 24_usize, which would overflow`, repeated across the interface.
     This is a gdext-0.4 + wasm32 pointer-width issue in the bindings, independent of our code.
     NEXT: check the godot-rust book's current "Export to web" page + gdext issue tracker for the
     supported gdext/Godot/emscripten matrix; may need a newer gdext (git) or a specific
     Godot-4.7 binding regen. This is the real gate — resolve before sinking more time into export
     templates/preset (steps 4-6).

Bottom line: Phase 1 is blocked on (1) a mechanical egui cfg-gate we control, and (2) a gdext/wasm32
binding bug we don't. The matchbox/emscripten incompatibility (Phase 2) is unchanged. Until gdext
web is unblocked, the working browser build remains the canvas client at `/play/` (frozen, not
maintained per the latest call).
