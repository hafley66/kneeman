//! Netcode event firehose. `log(kind, extra)` buffers one JSON event; `flush(http, url)` POSTs the
//! buffer as a JSON array via an `HttpRequest` node -- cross-platform: on the web export Godot backs
//! that with the browser fetch, so the same code ships to phone + desktop with no `#[cfg]` and no JS.
//!
//! The client stamps a per-session id (`sid`) and a monotonic client-send tick (`cs`); the relay adds
//! `t` (recv unix ms), `cip` (client IP via X-Forwarded-For), and `sip` (server id). So a two-device
//! match is one grep over the log: same room, two `sid`s, two `cip`s, interleaved by `t`.

use std::cell::RefCell;

use godot::classes::http_client::Method;
use godot::classes::HttpRequest;
use godot::global::Error;
use godot::prelude::*;

/// Bound the buffer so a dead network can't grow it without limit; drop oldest past this.
const BUF_CAP: usize = 4096;

thread_local! {
    static BUF: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static SID: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// This run's session id, minted once (wall clock * 1e6 + boot micros -> unlikely to collide).
fn session_id() -> String {
    SID.with(|s| s.borrow_mut().get_or_insert_with(mint_sid).clone())
}

fn mint_sid() -> String {
    let time = godot::classes::Time::singleton();
    let usec = time.get_ticks_usec();
    let wall = time.get_unix_time_from_system() as u64;
    format!("s{:x}", wall.wrapping_mul(1_000_000).wrapping_add(usec))
}

/// A JSON string literal (WITH surrounding quotes), escaped, for embedding an untrusted value like an
/// error message into an `extra` fragment: `&format!(r#","err":{}"#, jstr(&msg))`.
pub fn jstr(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' | '\r' | '\t' => out.push(' '),
            c if (c as u32) < 0x20 => {}
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Buffer one event. `extra` is a JSON fragment of extra fields WITH a leading comma, e.g.
/// `r#","phase":"running","room":"lobby-x""#`, or `""` for none. Values must be pre-escaped.
pub fn log(kind: &str, extra: &str) {
    let cs = crate::net::now_ms();
    let line = format!(r#"{{"ev":"{kind}","sid":"{}","cs":{cs}{extra}}}"#, session_id());
    BUF.with(|b| {
        let mut v = b.borrow_mut();
        if v.len() >= BUF_CAP {
            v.remove(0);
        }
        v.push(line);
    });
}

/// Drain the buffer into one `[...]` POST. On a busy (request in flight) or offline node the batch is
/// re-buffered in order for the next flush, so a transient stall loses nothing.
pub fn flush(http: &mut Gd<HttpRequest>, url: &str) {
    let batch: Vec<String> = BUF.with(|b| std::mem::take(&mut *b.borrow_mut()));
    if batch.is_empty() {
        return;
    }
    let body = format!("[{}]", batch.join(","));
    let headers = PackedStringArray::from(&[GString::from("Content-Type: application/json")]);
    let err = http
        .request_ex(url)
        .method(Method::POST)
        .custom_headers(&headers)
        .request_data(&GString::from(body.as_str()))
        .done();
    if err != Error::OK {
        BUF.with(|b| {
            let mut v = b.borrow_mut();
            let mut restored = batch;
            restored.append(&mut v);
            *v = restored;
        });
    }
}
