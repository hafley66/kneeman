//! Web Push: stores browser push subscriptions per room and fans out a notification when someone
//! parks in that room's lobby. Self-contained and host-agnostic -- the VAPID public key the client
//! needs is DERIVED from the private key at boot and served from `/vapid`, so no key is ever baked
//! into the client or pinned to a domain. With no VAPID key configured the whole module no-ops
//! (subscribe still records, notify silently does nothing), so the relay runs fine without push.

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::Arc;

use base64::Engine;
use tokio::sync::Mutex;
use web_push::{
    ContentEncoding, HyperWebPushClient, SubscriptionInfo, VapidSignatureBuilder, WebPushClient,
    WebPushError, WebPushMessageBuilder,
};

use crate::config::Config;

/// Cheap-clone handle (Arc inside) so a `notify` can be spawned onto its own task with a clone.
#[derive(Clone)]
pub struct PushState {
    cfg: Config,
    public_key_b64: Option<String>, // the client's applicationServerKey, derived from the private key
    store: Arc<Mutex<HashMap<String, Vec<SubscriptionInfo>>>>, // room -> subscriptions
}

impl PushState {
    pub fn new(cfg: Config) -> Self {
        let public_key_b64 = cfg.vapid_private_pem.as_ref().and_then(|pem| {
            match VapidSignatureBuilder::from_pem_no_sub(Cursor::new(pem.as_bytes())) {
                Ok(builder) => Some(
                    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(builder.get_public_key()),
                ),
                Err(e) => {
                    eprintln!("[push] bad VAPID private key ({e}); push disabled");
                    None
                }
            }
        });
        let store = load_store(&cfg.subs_path);
        if public_key_b64.is_some() {
            println!("[push] enabled — {} rooms loaded from {}", store.len(), cfg.subs_path);
        } else {
            println!("[push] disabled (no VAPID_PRIVATE_PEM)");
        }
        Self { cfg, public_key_b64, store: Arc::new(Mutex::new(store)) }
    }

    /// The base64url public key for the JS client, or None when push is off.
    pub fn public_key(&self) -> Option<&str> {
        self.public_key_b64.as_deref()
    }

    /// Record a browser subscription for a room (dedup by endpoint), then persist to disk.
    pub async fn subscribe(&self, room: String, sub: SubscriptionInfo) {
        let mut store = self.store.lock().await;
        let subs = store.entry(room).or_default();
        if !subs.iter().any(|s| s.endpoint == sub.endpoint) {
            subs.push(sub);
        }
        let snapshot = store.clone();
        drop(store);
        persist_store(&self.cfg.subs_path, &snapshot);
    }

    /// Fire a notification to everyone subscribed to `room`. Non-blocking: spawns its own task so the
    /// relay's hot path never waits on the push services. Prunes subscriptions the push service
    /// reports as gone (404/410) so dead endpoints don't accumulate.
    pub fn notify(&self, room: String, title: String, body: String) {
        if self.public_key_b64.is_none() {
            return; // push disabled
        }
        let this = self.clone();
        tokio::spawn(async move {
            let subs: Vec<SubscriptionInfo> = {
                let store = this.store.lock().await;
                store.get(&room).cloned().unwrap_or_default()
            };
            if subs.is_empty() {
                return;
            }
            let pem = this.cfg.vapid_private_pem.as_deref().unwrap_or_default();
            let payload = serde_json::json!({ "title": title, "body": body, "room": room }).to_string();
            let client = HyperWebPushClient::new();
            let mut dead: Vec<String> = Vec::new();
            for sub in &subs {
                match send_one(&client, pem, &this.cfg.vapid_subject, this.cfg.push_ttl_secs, sub, &payload).await {
                    Ok(()) => {}
                    Err(WebPushError::EndpointNotValid(_)) | Err(WebPushError::EndpointNotFound(_)) => {
                        dead.push(sub.endpoint.clone());
                    }
                    Err(e) => eprintln!("[push] send to {} failed: {e}", short(&sub.endpoint)),
                }
            }
            if !dead.is_empty() {
                let mut store = this.store.lock().await;
                if let Some(subs) = store.get_mut(&room) {
                    subs.retain(|s| !dead.contains(&s.endpoint));
                }
                let snapshot = store.clone();
                drop(store);
                persist_store(&this.cfg.subs_path, &snapshot);
                println!("[push] pruned {} dead endpoint(s) in room '{room}'", dead.len());
            }
        });
    }
}

/// Build a VAPID-signed, encrypted push for one subscription and send it.
async fn send_one(
    client: &HyperWebPushClient,
    pem: &str,
    subject: &str,
    ttl: u32,
    sub: &SubscriptionInfo,
    payload: &str,
) -> Result<(), WebPushError> {
    let mut sig = VapidSignatureBuilder::from_pem(Cursor::new(pem.as_bytes()), sub)?;
    sig.add_claim("sub", subject);
    let signature = sig.build()?;

    let mut builder = WebPushMessageBuilder::new(sub);
    builder.set_payload(ContentEncoding::Aes128Gcm, payload.as_bytes());
    builder.set_ttl(ttl);
    builder.set_vapid_signature(signature);
    client.send(builder.build()?).await
}

fn short(endpoint: &str) -> &str {
    endpoint.split('/').nth(2).unwrap_or(endpoint) // the push service host, enough to identify
}

/// Load the room->subscriptions map from disk (empty if absent/corrupt -- never fatal).
fn load_store(path: &str) -> HashMap<String, Vec<SubscriptionInfo>> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
            eprintln!("[push] {path} is not valid subscription JSON ({e}); starting empty");
            HashMap::new()
        }),
        Err(_) => HashMap::new(),
    }
}

/// Persist the store. Writes a temp file then renames, so a crash mid-write can't truncate the store.
fn persist_store(path: &str, store: &HashMap<String, Vec<SubscriptionInfo>>) {
    let Ok(json) = serde_json::to_string(store) else { return };
    let tmp = format!("{path}.tmp");
    if std::fs::write(&tmp, json).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}
