use godot::prelude::*;

#[cfg(not(target_arch = "wasm32"))]
mod debug_ui; // egui UI layer (desktop only; egui/gdext-egui don't build for wasm)
mod kneeman; // impure shell: input -> step -> publish -> render
mod grid; // training-room grid backdrop
#[cfg(not(target_arch = "wasm32"))]
mod theme; // egui stylesheet + components (desktop only)

// Pure sim now lives in its own crate (core/). Re-export under `sim` so the shell modules
// keep referring to `crate::sim::*` unchanged. `gv()` is the glam->godot vector boundary.
pub use smash_core as sim;

// Extension entry (referenced by sim.gdextension as gdext_rust_init).
// GodotClass types register themselves wherever they live; the modules above just need compiling.
struct SmashSimExtension;

#[gdextension]
unsafe impl ExtensionLibrary for SmashSimExtension {}
