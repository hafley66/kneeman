//! Capture build identity (git hash + build time) into env vars the binary reads via `env!`.
//! Lets `/status` report exactly which build is live without a separate version file.
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

    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    println!("cargo:rustc-env=BUILD_UNIX={unix}");

    // Rebuild stamp moves whenever HEAD does, so the hash never goes stale across commits.
    println!("cargo:rerun-if-changed=.git/HEAD");
}
