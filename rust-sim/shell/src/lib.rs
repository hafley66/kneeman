use godot::prelude::*;

mod ui; // egui debug panel + XP menu nav system + themes (builds on wasm via patched gdext-egui)
mod controls; // sole device->InputFrame boundary (GameAction universe)
mod kneeman; // impure shell: input -> step -> publish -> render
mod grid; // training-room grid backdrop
mod rtc; // Godot WebRTC netplay transport (ggrs over a browser data channel)

// Pure sim now lives in its own crate (core/). Re-export under `sim` so the shell modules
// keep referring to `crate::sim::*` unchanged. `gv()` is the glam->godot vector boundary.
pub use smash_core as sim;

// Extension entry (referenced by sim.gdextension as gdext_rust_init).
// GodotClass types register themselves wherever they live; the modules above just need compiling.
struct SmashSimExtension;

#[gdextension]
unsafe impl ExtensionLibrary for SmashSimExtension {}
