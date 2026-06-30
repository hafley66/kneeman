//! Capture the git short hash at build time into `BUILD_HASH`, read via `env!` (see rtc.rs). Two
//! web clients compare this to detect a stale cached wasm before it desyncs a match; the same hash
//! goes to the signaling relay's /status. Mirrors signaling/build.rs.
use std::process::Command;

fn main() {
    let hash = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=BUILD_HASH={hash}");

    // Rebuild stamp moves whenever HEAD does, so the hash never goes stale across commits.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
}
