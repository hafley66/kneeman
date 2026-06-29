//! Monotonic clock backed by SystemTime, replacing `instant::Instant`.
//!
//! Patched for wasm32-unknown-emscripten: `std::time::Instant::now()` (which the `instant`
//! crate falls through to on every non-`unknown` wasm target) lowers to the `emscripten_get_now`
//! symbol. Our SIDE_MODULE expects that symbol from the Godot main module, which does not export
//! it, so the wasm aborts the instant a P2P session starts. `SystemTime` works on every target we
//! ship (same path as `millis_since_epoch` below), so we base our own `Instant` on it. Resolution
//! is milliseconds, which is well under ggrs's 200ms network timers.

pub use std::time::Duration;

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Instant {
    millis: u128,
}

impl Instant {
    pub fn now() -> Self {
        Instant { millis: now_millis() }
    }
}

impl std::ops::Add<Duration> for Instant {
    type Output = Instant;
    fn add(self, rhs: Duration) -> Instant {
        Instant {
            millis: self.millis + rhs.as_millis(),
        }
    }
}

fn now_millis() -> u128 {
    // emscripten (target_os = "emscripten") has a working SystemTime clock, so only true
    // wasm-bindgen wasm (target_os = "unknown") needs the js-sys Date path.
    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("Time went backwards")
            .as_millis()
    }
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    {
        js_sys::Date::new_0().get_time() as u128
    }
}
