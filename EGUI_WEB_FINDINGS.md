# egui debug overlay on Godot web export (wasm32-unknown-emscripten)

## Status: APPLIED, compiles + links

A `[patch.crates-io]` fork of gdext-egui is applied in this worktree. The wasm build now links
with egui and the `debug_ui` overlay compiled in. Both targets build clean:

- Desktop: `cargo build -p smash_sim` -> green (vendored gdext-egui, unchanged behavior).
- Web: `source ~/emsdk/emsdk_env.sh && GODOT4_BIN="$HOME/godot45/Godot.app/Contents/MacOS/Godot"
  cargo +nightly build -p smash_sim -Zbuild-std --target wasm32-unknown-emscripten`
  -> produces `rust-sim/target/wasm32-unknown-emscripten/debug/smash_sim.wasm` (~139 MB debug).

Verified:
- `cargo tree --target wasm32-unknown-emscripten -i open` -> "nothing to print" (open gone from wasm).
- `cargo tree -i open` (native) -> still present (desktop dep tree unchanged).

NOT verified: actual in-browser rendering/input at runtime. That needs the remaining export pipeline
(web `.gdextension` entry, 4.5 web templates, COOP/COEP host headers for the threaded build). The
compile/link blocker that gated egui off wasm is resolved; runtime is the next step, untested here.

---

## 1. Root cause: where `open` enters and why it breaks wasm

`cargo tree` (native), from `rust-sim/`:

```
open v5.3.5
└── gdext-egui v0.4.1
    └── smash_sim v0.1.0 (shell)
```

- `open` is a hard dependency of gdext-egui 0.4.1. Declared unconditionally in its
  `Cargo.toml` (`[dependencies.open] version = "5"`). No feature flag gates it.
- Used at exactly ONE call site:
  `gdext-egui-0.4.1/src/context.rs:1459` ->
  ```rust
  egui::OutputCommand::OpenUrl(open_url) => {
      open::that(open_url.url).ok();
  }
  ```
  This is the handler for egui asking the host to open a URL in a browser. A debug overlay never
  needs it.
- Why it fails to build for wasm: `open` 5.3.5 dispatches its platform backend by `target_os` and,
  for any unlisted OS, hits a hard `compile_error!`:
  `open-5.3.5/src/lib.rs:139` -> `compile_error!("open is not supported on this platform");`
  `wasm32-unknown-emscripten` has `target_os = "emscripten"`, which is not in the supported list
  (windows/macos/ios/visionos/haiku/redox/linux/android/the BSDs/illumos/solaris/aix/hurd). So the
  crate refuses to compile. This is a compile_error, not a feature toggle, so `default-features=false`
  cannot help; the dependency must be removed from the wasm dep tree entirely.

## 2. Is the rest of gdext-egui's path wasm-hostile? No.

- Rendering: gdext-egui paints through Godot's own `RenderingServer` canvas_item API
  (`src/surface.rs`: `canvas_item_create`, `canvas_item_add_triangle_array_ex`, a `canvas_item`
  shader at `surface.rs:608`). It does NOT use `glow`, `egui_glow`, WebGL, or any GL context from
  Rust. The engine does the GPU work, so the emscripten WebGL/eframe-glow problems that block
  `eframe` on emscripten (egui issue #7732) do not apply here.
- egui core (`egui`, `epaint`, `emath`, `ecolor`, `ab_glyph`, `parking_lot`, `ahash`) all compiled
  clean for wasm32-unknown-emscripten in this build. Confirmed by the successful link.
- `std::time::Instant` (context.rs:148/779, _widget.rs) and `std::thread` (context.rs:131/561):
  fine here because this project's web build is THREADED (`rust-sim/.cargo/config.toml`: `-pthread`,
  `+atomics`, `-sSIDE_MODULE=2`). emscripten provides clock + pthreads; both resolved at link.
- Other deps (itertools, oneshot, tap, with_drop, crossbeam-queue, derive_setters, educe) are pure
  Rust, no platform backend.

Conclusion: `open` was the single blocker.

## 3. Prior art: has anyone rendered egui inside a gdext web export?

No public confirmation found of egui-in-gdext on web specifically.

| Source | Finding | Status |
|---|---|---|
| github.com/kang-sw/gdext-egui issues | Only open issues are #2 (hot-reload crash) and #3 (remaining features). No issue/PR mentions wasm, web, emscripten, or `open`. | no wasm discussion |
| godot-rust/gdext #438 "WebAssembly support" | Umbrella issue for gdext wasm. General toolchain (nightly, build-std, emscripten). Does not cover egui. | unconfirmed re egui |
| godot-rust/gdext #968 | Multiple Rust GDExtensions cannot co-load on web export. Relevant only if shipping >1 gdext .wasm. This project ships one. | constraint, not blocker here |
| emilk/egui #7732 "Support target wasm32-unknown-emscripten" | egui core builds for emscripten; the failures are in the `eframe`/`egui_glow` WebGL path (glow has no emscripten WebGL ctx). gdext-egui does not use that path. | core OK, glow path N/A |
| godot-rust book "Export to Web" | Canonical gdext web toolchain. Says nothing about egui. | toolchain only |

So this worktree's working combo is, as far as found sources show, novel. Concrete combo that links:

| component | version |
|---|---|
| gdext (godot crate) | 0.4.5 (Godot 4.5 API level), features api-custom + experimental-wasm + lazy-function-tables |
| gdext-egui | 0.4.1, patched (open cfg-gated off wasm) |
| egui / epaint | 0.33.3 |
| target | wasm32-unknown-emscripten, threaded (-pthread, +atomics, -sSIDE_MODULE=2) |
| emsdk | 3.1.74 |
| toolchain | nightly + rust-src, -Zbuild-std |

## 4. The patch (cheapest fix, applied)

gdext-egui exposes no feature knob for `open`, so a minimal vendored fork was applied via
`[patch.crates-io]`. Three edits; desktop dep tree and behavior unchanged.

Vendored at `rust-sim/vendor/gdext-egui/` (copy of registry 0.4.1).

**a. `rust-sim/Cargo.toml`** (workspace root): exclude the vendor dir, patch the crate.
```toml
[workspace]
resolver = "2"
members = ["core", "net", "shell"]
exclude = ["vendor/gdext-egui"]

[patch.crates-io]
gdext-egui = { path = "vendor/gdext-egui" }
```

**b. `rust-sim/vendor/gdext-egui/Cargo.toml`**: move `open` to a non-wasm target table.
```toml
# was: [dependencies.open] version = "5"
[target.'cfg(not(target_arch = "wasm32"))'.dependencies.open]
version = "5"
```
(Also added a standalone `[workspace]` table to the vendored crate so the path-patch is not
absorbed into the consuming workspace.)

**c. `rust-sim/vendor/gdext-egui/src/context.rs:1458`**: cfg-gate the only call site.
```rust
egui::OutputCommand::OpenUrl(open_url) => {
    #[cfg(not(target_arch = "wasm32"))]
    open::that(open_url.url).ok();
    #[cfg(target_arch = "wasm32")]
    let _ = open_url;
}
```

**d. `rust-sim/shell/Cargo.toml`**: egui + gdext-egui are now plain deps (both targets) instead of
`cfg(not(target_arch = "wasm32"))`-only. The wasm-specific godot feature block is unchanged.

**e. `rust-sim/shell/src/lib.rs`**: `mod debug_ui;` and `mod theme;` no longer cfg-gated off wasm.

## 5. Alternatives (not needed; ranked for the record)

1. Vendored gdext-egui patch (above). Chosen. Cheapest, keeps the existing egui overlay code
   (`shell/src/debug_ui.rs`, ~290 lines) verbatim, one engine, one wasm module. Cost: carry a
   ~5-line fork until upstream gates `open` (worth filing a PR: make `open` an optional dep behind
   a default feature, or target-gate it).
2. In-engine debug draw via Godot `Control`/`CanvasItem` nodes from Rust. Always works on web, no
   egui. Cost: rewrite the overlay; lose egui widgets/sliders. Fallback only if the patch were
   rejected.
3. egui + egui_glow painting onto Godot's GL context. Blocked: glow has no emscripten WebGL ctx
   (egui #7732), and gdext does not hand out a usable GL context anyway. Strictly worse than (1).
4. Separate eframe canvas (wasm32-unknown-unknown) layered over the Godot canvas via DOM. Two wasm
   targets, two modules, marshal state through JS. Brittle; same shape the project already rejected
   for netplay transport. Avoid.

## 6. Remaining work to see it on screen (not done here)

1. Add web library lines to `rust-sim/sim.gdextension`:
   ```
   web.debug.wasm32   = "res://rust-sim/target/wasm32-unknown-emscripten/debug/smash_sim.wasm"
   web.release.wasm32 = "res://rust-sim/target/wasm32-unknown-emscripten/release/smash_sim.wasm"
   ```
2. Install Godot 4.5 web export templates; add a Web export preset (threaded, to match the
   `-pthread` build).
3. Host with COOP/COEP headers (SharedArrayBuffer requirement for the threaded build).
4. Export and load the page; toggle the overlay (Cmd+Shift+J) and confirm egui renders + takes input.

## Sources

- gdext-egui 0.4.1 source: registry crate, `src/context.rs:1459`, `Cargo.toml`.
- open 5.3.5 source: registry crate, `src/lib.rs:139` (`compile_error!`).
- https://github.com/kang-sw/gdext-egui/issues
- https://github.com/godot-rust/gdext/issues/438
- https://github.com/godot-rust/gdext/issues/968
- https://github.com/emilk/egui/issues/7732
- https://godot-rust.github.io/book/toolchain/export-web.html
