//! Minimal WebRTC signaling relay. Pairs WebSocket clients two at a time and forwards every text
//! frame from one peer to the other, unread. The client (the gdext shell) speaks JSON:
//!
//!   server -> peer  : {"kind":"matched","role":"host"}   (first in a pair = host, second = guest)
//!   peer   -> server: {"kind":"offer"|"answer"|"ice", ...}   (relayed verbatim to the partner)
//!   server -> peer  : {"kind":"bye"}                     (partner left)
//!
//! The host creates the WebRTC offer; the guest answers. The relay is deliberately dumb: it does
//! not parse offer/answer/ice payloads, only the pairing handshake is server-driven.
//!
//! Pairing is a single waiting slot (the matchbox `?next=2` behavior): the first connection waits,
//! the second one snaps to it, both get a partner channel, the slot clears for the next two.

use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio_tungstenite::tungstenite::Message;

/// What a waiting (host) connection parks in the lobby: a sender that delivers text TO it, and a
/// oneshot the arriving guest fires to hand back ITS sender (so the host can reach the guest).
struct Pending {
    to_host: mpsc::UnboundedSender<String>,
    give_guest: oneshot::Sender<mpsc::UnboundedSender<String>>,
}

type Lobby = Arc<Mutex<Option<Pending>>>;

#[tokio::main]
async fn main() {
    let addr = "127.0.0.1:3537";
    let listener = TcpListener::bind(addr).await.expect("bind signaling port");
    println!("smash-signaling listening on ws://{addr} (proxy wss://host/rtc)");
    let lobby: Lobby = Arc::new(Mutex::new(None));

    loop {
        let Ok((stream, _)) = listener.accept().await else { continue };
        let lobby = lobby.clone();
        tokio::spawn(async move {
            if let Err(e) = handle(stream, lobby).await {
                eprintln!("peer ended: {e}");
            }
        });
    }
}

async fn handle(stream: tokio::net::TcpStream, lobby: Lobby) -> Result<(), String> {
    let who = stream.peer_addr().map(|a| a.to_string()).unwrap_or_else(|_| "?".into());
    println!("connect {who}");
    let ws = tokio_tungstenite::accept_async(stream)
        .await
        .map_err(|e| e.to_string())?;
    let (mut tx_ws, mut rx_ws) = ws.split();

    // Messages destined FOR this peer (written by the partner's relay task) arrive on this channel.
    let (to_me, mut from_partner) = mpsc::unbounded_channel::<String>();

    // Claim a partner: either snap to a waiting host (we become guest) or park as host and wait.
    let (role, to_partner): (&str, mpsc::UnboundedSender<String>) = {
        let mut slot = lobby.lock().await;
        match slot.take() {
            Some(p) => {
                // We are the guest. Hand the host our sender, take theirs.
                let _ = p.give_guest.send(to_me.clone());
                ("guest", p.to_host)
            }
            None => {
                // We are the host. Park, then wait for a guest to deliver its sender.
                let (give, got) = oneshot::channel();
                *slot = Some(Pending { to_host: to_me.clone(), give_guest: give });
                drop(slot);
                match got.await {
                    Ok(guest_tx) => ("host", guest_tx),
                    // Connection dropped while waiting: clear our parked slot if it is still ours.
                    Err(_) => {
                        let mut s = lobby.lock().await;
                        if s.as_ref().map(|p| p.to_host.same_channel(&to_me)).unwrap_or(false) {
                            *s = None;
                        }
                        return Ok(());
                    }
                }
            }
        }
    };

    println!("matched {who} as {role}");
    tx_ws
        .send(Message::Text(format!(r#"{{"kind":"matched","role":"{role}"}}"#).into()))
        .await
        .map_err(|e| e.to_string())?;

    // Relay loop: forward our inbound WS text to the partner; write partner text out to our WS.
    loop {
        tokio::select! {
            msg = rx_ws.next() => match msg {
                Some(Ok(Message::Text(t))) => { let _ = to_partner.send(t.to_string()); }
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => {}        // ignore binary/ping/pong
                Some(Err(_)) => break,
            },
            out = from_partner.recv() => match out {
                Some(t) => tx_ws.send(Message::Text(t.into())).await.map_err(|e| e.to_string())?,
                None => break,           // partner gone
            },
        }
    }

    println!("disconnect {who} ({role})");
    let _ = to_partner.send(r#"{"kind":"bye"}"#.to_string());
    Ok(())
}
