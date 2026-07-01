//! Runtime config, entirely from the environment (12-factor). Nothing here names a host, a domain,
//! or a path baked at compile time -- every deployment knob is an env var with a sane local default,
//! so the same binary runs on a laptop, a VPS, or a container with only the environment changing.
//!
//!   BIND_ADDR          socket to listen on              (default 127.0.0.1:3537)
//!   VAPID_PRIVATE_PEM  path to the EC P-256 private key (push disabled if unset/missing)
//!   VAPID_SUBJECT      contact put in the VAPID JWT     (default mailto:admin@localhost)
//!   SUBS_PATH          file the push subscriptions live in (default ./subscriptions.json)
//!   PUSH_TTL_SECS      how long a push may queue at the push service (default 600)
//!   GAME_DIR           static root for the Godot web export at /game/ (default /var/www/smash-godot)

use std::env;

#[derive(Clone)]
pub struct Config {
    pub bind_addr: String,
    pub vapid_private_pem: Option<String>, // file CONTENTS, read at boot (None = push off)
    pub vapid_subject: String,
    pub subs_path: String,
    pub push_ttl_secs: u32,
    pub game_dir: String, // static root for the Godot web export served at /game/
}

impl Config {
    pub fn from_env() -> Self {
        let bind_addr = env::var("BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:3537".into());
        let vapid_private_pem = env::var("VAPID_PRIVATE_PEM").ok().and_then(|path| {
            match std::fs::read_to_string(&path) {
                Ok(pem) => Some(pem),
                Err(e) => {
                    eprintln!("[push] VAPID_PRIVATE_PEM={path} unreadable ({e}); push disabled");
                    None
                }
            }
        });
        Self {
            bind_addr,
            vapid_private_pem,
            vapid_subject: env::var("VAPID_SUBJECT").unwrap_or_else(|_| "mailto:admin@localhost".into()),
            subs_path: env::var("SUBS_PATH").unwrap_or_else(|_| "./subscriptions.json".into()),
            push_ttl_secs: env::var("PUSH_TTL_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(600),
            game_dir: env::var("GAME_DIR").unwrap_or_else(|_| "/var/www/smash-godot".into()),
        }
    }
}
