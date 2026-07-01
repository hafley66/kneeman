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
//!   TLS_DOMAINS        comma-sep domains -> in-process ACME/TLS on :443 (empty = plain HTTP, default)
//!   ACME_CONTACT       email in the ACME account                       (default admin@localhost)
//!   ACME_CACHE_DIR     where the ACME account + certs persist          (default /var/lib/smash/acme)
//!   ACME_PRODUCTION    "1"/"true" = real Let's Encrypt, else staging   (default staging)
//!   EV_LOG_PATH        newline-JSON netcode event log                  (default /var/log/smash/ev.log)
//!   EV_LOG_CAP_BYTES   TOTAL on-disk budget; rotates at half, keeps 1  (default 1073741824 = 1 GiB)
//!   SERVER_ID          stamped on every event as `sip` (server tag)    (default the box hostname)

use std::env;

#[derive(Clone)]
pub struct Config {
    pub bind_addr: String,
    pub vapid_private_pem: Option<String>, // file CONTENTS, read at boot (None = push off)
    pub vapid_subject: String,
    pub subs_path: String,
    pub push_ttl_secs: u32,
    pub game_dir: String,        // static root for the Godot web export served at /game/
    pub tls_domains: Vec<String>, // non-empty => terminate TLS in-process on :443 via ACME
    pub acme_contact: String,
    pub acme_cache_dir: String,
    pub acme_production: bool,
    pub ev_log_path: String,     // newline-JSON netcode event log (empty => event logging off)
    pub ev_log_cap_bytes: u64,   // TOTAL disk budget: rotate active file at cap/2, keep one .1 backup
    pub server_id: String,       // stamped as `sip` on every event
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
            tls_domains: env::var("TLS_DOMAINS")
                .ok()
                .map(|s| s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect())
                .unwrap_or_default(),
            acme_contact: env::var("ACME_CONTACT").unwrap_or_else(|_| "admin@localhost".into()),
            acme_cache_dir: env::var("ACME_CACHE_DIR").unwrap_or_else(|_| "/var/lib/smash/acme".into()),
            acme_production: env::var("ACME_PRODUCTION").map(|v| v == "1" || v == "true").unwrap_or(false),
            ev_log_path: env::var("EV_LOG_PATH").unwrap_or_else(|_| "/var/log/smash/ev.log".into()),
            ev_log_cap_bytes: env::var("EV_LOG_CAP_BYTES")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(1024 * 1024 * 1024), // 1 GiB total
            server_id: env::var("SERVER_ID")
                .ok()
                .or_else(|| std::fs::read_to_string("/etc/hostname").ok().map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "server".into()),
        }
    }
}
