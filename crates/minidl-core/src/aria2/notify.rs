//! Read-only WebSocket listener for aria2 push notifications.
//!
//! aria2 broadcasts `aria2.onDownload*` notifications to *every* connected
//! WebSocket client, whether or not it has issued requests. So we connect a
//! read-only socket and never send on it. Request/reply goes over HTTP (see
//! `rpc.rs`) — this keeps the listener trivial: no id correlation, and reconnect
//! is just "open the socket again".

use std::time::Duration;

use futures_util::StreamExt;
use serde_json::Value;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

use super::event::Aria2Event;

pub fn spawn_listener(port: u16, events: broadcast::Sender<Aria2Event>) -> JoinHandle<()> {
    let url = format!("ws://127.0.0.1:{port}/jsonrpc");
    tokio::spawn(async move {
        let mut backoff = 1u64;
        loop {
            match connect_async(url.as_str()).await {
                Ok((ws, _resp)) => {
                    let started = tokio::time::Instant::now();
                    // Keep the write half bound (not dropped) so the socket stays open.
                    let (_write, mut read) = ws.split();
                    while let Some(msg) = read.next().await {
                        match msg {
                            Ok(Message::Text(t)) => {
                                if let Some(ev) = parse_notification(t.as_str()) {
                                    let _ = events.send(ev);
                                }
                            }
                            Ok(Message::Close(_)) => break,
                            Err(_) => break,
                            _ => {}
                        }
                    }
                    // Only treat this as a healthy session (reset backoff) if it
                    // lasted a while. A socket that is accepted then immediately
                    // closed would otherwise busy-loop reconnecting with no delay.
                    if started.elapsed() >= Duration::from_secs(1) {
                        backoff = 1;
                    } else {
                        tokio::time::sleep(Duration::from_secs(backoff)).await;
                        backoff = (backoff * 2).min(30);
                    }
                }
                Err(_) => {
                    tokio::time::sleep(Duration::from_secs(backoff)).await;
                    backoff = (backoff * 2).min(30);
                }
            }
        }
    })
}

/// A JSON-RPC notification frame has a `method` and no `id`.
fn parse_notification(text: &str) -> Option<Aria2Event> {
    let v: Value = serde_json::from_str(text).ok()?;
    if v.get("id").is_some() {
        return None;
    }
    let method = v.get("method")?.as_str()?;
    let gid = v
        .get("params")?
        .as_array()?
        .first()?
        .get("gid")?
        .as_str()?
        .to_string();
    Aria2Event::from_method(method, gid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_complete_notification() {
        let frame = r#"{"jsonrpc":"2.0","method":"aria2.onDownloadComplete","params":[{"gid":"2089b05ecca3d829"}]}"#;
        assert_eq!(
            parse_notification(frame),
            Some(Aria2Event::Complete("2089b05ecca3d829".into()))
        );
    }

    #[test]
    fn ignores_replies() {
        let reply = r#"{"jsonrpc":"2.0","id":"1","result":"OK"}"#;
        assert_eq!(parse_notification(reply), None);
    }
}
