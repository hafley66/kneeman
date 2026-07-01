//! Netcode event sink. `POST /ev` takes a JSON object or array of objects from the game client, the
//! server stamps each with `t` (recv unix ms), `cip` (client IP, via nginx's X-Forwarded-For), and
//! `sip` (this box's SERVER_ID), then appends one JSON line per event to a size-capped rotating file.
//!
//! Rotation keeps the on-disk footprint under EV_LOG_CAP_BYTES: the active file rotates to `<path>.1`
//! (replacing the prior backup) once it passes cap/2, so current + backup <= cap. Writes happen on a
//! dedicated OS thread (blocking std::fs, off the tokio workers); the async handler only does a
//! non-blocking channel send, so a slow disk never stalls a game POST.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::Value;
use std::net::SocketAddr;

use crate::Shared;

/// Handle to the writer thread. Cloneable-cheap (just a channel sender); a disabled log (empty path)
/// carries `None` and drops every event.
#[derive(Clone)]
pub struct EventLog {
    tx: Option<Sender<Vec<String>>>,
}

impl EventLog {
    /// Spawn the writer thread. Empty `path` = logging off (a no-op sink).
    pub fn spawn(path: String, cap_bytes: u64) -> Self {
        if path.trim().is_empty() {
            return Self { tx: None };
        }
        let (tx, rx) = mpsc::channel::<Vec<String>>();
        std::thread::Builder::new()
            .name("ev-log".into())
            .spawn(move || writer_loop(rx, PathBuf::from(path), cap_bytes.max(2)))
            .expect("spawn ev-log thread");
        Self { tx: Some(tx) }
    }

    /// Queue a batch of already-serialized JSON lines. Non-blocking; drops on a dead writer.
    fn submit(&self, lines: Vec<String>) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(lines);
        }
    }
}

/// Owns the file + rotation. `rotate_at` is cap/2 so active + one backup stays under the cap.
fn writer_loop(rx: mpsc::Receiver<Vec<String>>, path: PathBuf, cap_bytes: u64) {
    let rotate_at = (cap_bytes / 2).max(1);
    let bak = path.with_extension("1");
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let mut file = open_append(&path);
    let mut len = file.as_ref().and_then(|f| f.metadata().ok()).map(|m| m.len()).unwrap_or(0);

    while let Ok(lines) = rx.recv() {
        for line in &lines {
            if len >= rotate_at {
                if let Some(f) = file.as_mut() {
                    let _ = f.flush();
                }
                let _ = fs::rename(&path, &bak); // replaces the prior backup
                file = open_append(&path);
                len = 0;
            }
            if let Some(f) = file.as_mut() {
                if writeln!(f, "{line}").is_ok() {
                    len += line.len() as u64 + 1;
                }
            }
        }
        if let Some(f) = file.as_mut() {
            let _ = f.flush();
        }
    }
}

fn open_append(path: &PathBuf) -> Option<File> {
    match OpenOptions::new().create(true).append(true).open(path) {
        Ok(f) => Some(f),
        Err(e) => {
            eprintln!("[ev] cannot open {}: {e}", path.display());
            None
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Real client IP: nginx sets X-Forwarded-For; take the first hop. Falls back to the socket peer
/// (loopback when proxied, so the header is what matters in prod).
fn client_ip(headers: &HeaderMap, who: SocketAddr) -> String {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| who.ip().to_string())
}

/// `POST /ev` — accept one event object or an array; stamp server fields; append one line each.
pub async fn ev(
    State(server): State<Shared>,
    ConnectInfo(who): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let val: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("bad json: {e}")).into_response(),
    };
    let cip = client_ip(&headers, who);
    let t = now_ms();
    let batch = match val {
        Value::Array(a) => a,
        other => vec![other],
    };
    let mut lines = Vec::with_capacity(batch.len());
    for mut e in batch {
        if let Value::Object(map) = &mut e {
            map.insert("t".into(), Value::from(t));
            map.insert("cip".into(), Value::from(cip.clone()));
            map.insert("sip".into(), Value::from(server.server_id.clone()));
        }
        lines.push(e.to_string());
    }
    server.events.submit(lines);
    StatusCode::NO_CONTENT.into_response()
}
