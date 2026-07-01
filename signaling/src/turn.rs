//! `GET /turn` — mints short-lived TURN credentials for the coturn relay using the REST API scheme
//! (coturn `use-auth-secret`). The shared secret lives only here (from TURN_SECRET) and in the box's
//! turnserver.conf; the client never sees it. Instead the client fetches an ephemeral
//! `{username, credential}` pair that coturn validates against the same secret:
//!
//!   username   = <unix expiry>             (coturn accepts creds only until this time)
//!   credential = base64( HMAC-SHA1(secret, username) )
//!
//! ICE tries direct (host/STUN) candidates first and only relays through TURN when direct fails, so
//! this is the fallback for symmetric-NAT / VPN peer pairs. Response mirrors the RTCIceServer shape
//! the client hands to WebRtcPeerConnection.
//!
//! Returns 404 when TURN is unconfigured (no host/secret) so a STUN-only deployment is a no-op.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use base64::Engine;
use openssl::hash::MessageDigest;
use openssl::pkey::PKey;
use openssl::sign::Signer;

use crate::{now_unix, Shared};

/// HMAC-SHA1(secret, msg) via openssl (already a dep for web-push); base64-encoded per the coturn
/// REST scheme. Infallible in practice — a valid HMAC key never fails to sign.
fn rest_credential(secret: &str, username: &str) -> String {
    let key = PKey::hmac(secret.as_bytes()).expect("hmac key");
    let mut signer = Signer::new(MessageDigest::sha1(), &key).expect("hmac signer");
    signer.update(username.as_bytes()).expect("hmac update");
    let mac = signer.sign_to_vec().expect("hmac sign");
    base64::engine::general_purpose::STANDARD.encode(mac)
}

/// `GET /turn` — one ephemeral credential pair, valid for TURN_TTL_SECS. The client caches it and
/// injects it into the ICE config; coturn re-derives the same HMAC to authenticate the relay session.
pub async fn turn(State(server): State<Shared>) -> Response {
    let cfg = match &server.turn {
        Some(c) => c,
        None => return (StatusCode::NOT_FOUND, "turn disabled").into_response(),
    };
    let username = (now_unix() + cfg.ttl_secs).to_string();
    let credential = rest_credential(&cfg.secret, &username);
    // Offer both UDP and TCP transports: UDP is the fast path, TCP survives UDP-blocking networks.
    let body = format!(
        r#"{{"urls":["turn:{h}:{p}?transport=udp","turn:{h}:{p}?transport=tcp"],"username":"{u}","credential":"{c}","ttl":{ttl}}}"#,
        h = cfg.host, p = cfg.port, u = username, c = credential, ttl = cfg.ttl_secs,
    );
    ([(axum::http::header::CONTENT_TYPE, "application/json")], body).into_response()
}
