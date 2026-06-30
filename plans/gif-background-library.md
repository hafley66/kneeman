# GIF background + user gif library

## Context

The shell (Godot 4 + gdext, web export via wasm + native) should let the player set an
arbitrary animated GIF as the stage background, and keep a personal library of GIFs they
import once and pick from later. The library must persist in the same place on both the
browser build and the PC build.

This rides on the XP-Luna menu redesign: the gif picker is a route (`Route::Background`)
and the import step is a query-param dialog (`Dialog::GifImport`) layered over it. Nav is
the memory-router model: one `Mutable<Location>` BehaviorSubject, screens dispatched by
`match` each frame (switchScan).

## Why it is feasible (no blockers)

- **Storage is already solved.** `user://` is Godot's per-user store — a real file on
  native, IndexedDB-backed on web. The shell already uses it (`kneeman.rs:153`,
  `user://identity.cfg`). A gif library at `user://gifs/<name>.gif` persists on both
  targets with no extra plumbing — this is the "localStorage equivalent wherever they
  may be" the request asks for.
- **Decode is pure Rust.** The `image` crate's `GifDecoder` (or the `gif` crate) compiles
  to wasm32-unknown-emscripten, so the same decode path works native + web.
- **Render is a Godot node**, not egui. A background node behind the `Grid` draws the
  current frame; egui stays the menu layer only.

## Pieces

### 1. Decode (new dep: `image` with the `gif` feature, or `gif` directly)
- `fn decode_gif(bytes: &[u8]) -> GifAnim` -> frames as `Vec<(ImageTexture, delay_ms)>`.
- Cap dimensions / frame count (a large gif decodes to many full RGBA buffers in RAM).
  Decode lazily or downscale on import.

### 2. Background node (new: `shell/src/background.rs`)
- A Godot node drawn behind `Grid` (lower z / added earlier). Holds the active `GifAnim`,
  an elapsed accumulator, and the current frame index.
- `_process(dt)`: advance accumulator, step frame when its delay elapses, set the node's
  texture. Cover the viewport (`Sprite2D`/`TextureRect`, stretch to `screen_rect`).
- Reads the active-gif pick from a settings cell so a menu change applies live.

### 3. Game-time coupling (the gif freezes with the action)
The gif must run on **game time**, not wall-clock, so it freezes during hitlag (the impact
"pop") along with the fighters. We have a clock: `SimState.tick` (u64, +1 per `step()` at
60Hz, `lib.rs:645`). But `tick` does NOT pause on impact — hitlag is per-fighter
(`Fighter.hitlag`, `lib.rs:685`); a frozen fighter early-returns while `tick` keeps
counting. So "felt game time" = exclude freeze frames.

The gif is cosmetic (shell-side, not rollback state), so gate it on the rendered fighters:

- **Freeze-gate (start here):** each visual frame, `frozen = state.fighters.iter().any(|f|
  f.hitlag > 0)`. Hold the current gif frame while `frozen`; advance by `dt` otherwise. No
  sim change. Gives the full effect: gif stops on every hit, resumes after.
- **Sim-frame clock (later, if wanted):** advance the gif by `tick` deltas, skipping the
  deltas accumulated while `frozen`. Ties the gif to the 60Hz match clock so any future
  slow-mo / final-hit slowdown carries the background with it.

On a clean hit both fighters get the same `hitlag` (`lib.rs:1569-1570`), so they freeze
together and the background freeze reads as one beat. `any(hitlag > 0)` also catches the
one-sided cases.

### 4. Library storage (`user://gifs/`)
- `fn list_gifs() -> Vec<GifEntry>` — scan `user://gifs/`, entry = name + path + cached
  first-frame thumbnail.
- `fn save_gif(name, bytes)` — write bytes to `user://gifs/<name>.gif`.
- `fn load_gif(name) -> Vec<u8>` — read back for decode.
- Active pick persisted to a settings cfg (alongside identity), so the background restores
  on next launch.

### 5. Import flow (`Dialog::GifImport`, new dep: `rfd` for native)
- Native: `rfd::FileDialog` (async on web) -> bytes.
- Web: `<input type=file>` via `JavaScriptBridge` -> bytes (rfd's web path also works).
- On bytes: validate as gif, optional downscale, `save_gif`, refresh the picker list.

### 6. Menu wiring (in the XP menu redesign)
- `Route::Background` screen: grid of `list_gifs()` thumbnails + an "Import" tile that
  opens `Dialog::GifImport`; a "none" tile to clear the background. Selecting a tile emits
  `Intent::SetBackground(name)`.
- New intents: `SetBackground(String)`, `ImportGif` (open dialog), and the import-complete
  path that calls `save_gif`.
- The reducer writes the pick to the settings cell; the background node reads it.

## Open questions
- Thumbnail cost: decode-first-frame-only on list, or cache thumbnails to
  `user://gifs/.thumbs/`?
- Size policy: hard cap (reject > N MB / > M px) vs auto-downscale on import.
- Fit mode: cover / contain / tile / stretch for non-viewport-aspect gifs.

## Status
Planned, not started. Depends on the XP-Luna menu route/dialog scaffold landing first
(this feature is one route + one dialog inside it).
