use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

pub struct LoopbackHttp {
    pub url: String,
    pub payload: Arc<Vec<u8>>,
    stop: Option<oneshot::Sender<()>>,
}

impl LoopbackHttp {
    pub async fn start(size: usize) -> Self {
        let payload = Arc::new(
            (0..size)
                .map(|index| ((index * 31 + 17) % 251) as u8)
                .collect::<Vec<_>>(),
        );
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback HTTP fixture");
        let address = listener.local_addr().expect("loopback fixture address");
        let (stop, mut stopped) = oneshot::channel();
        let served_payload = payload.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut stopped => break,
                    accepted = listener.accept() => {
                        let Ok((socket, _)) = accepted else { break };
                        let payload = served_payload.clone();
                        tokio::spawn(async move { serve(socket, payload).await });
                    }
                }
            }
        });
        Self {
            url: format!("http://{address}/fixture.bin"),
            payload,
            stop: Some(stop),
        }
    }

    pub fn shutdown(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

async fn serve(mut socket: tokio::net::TcpStream, payload: Arc<Vec<u8>>) {
    let mut request = Vec::with_capacity(1_024);
    let mut chunk = [0_u8; 1_024];
    while request.len() < 64 * 1_024 {
        let Ok(read) = socket.read(&mut chunk).await else {
            return;
        };
        if read == 0 {
            return;
        }
        request.extend_from_slice(&chunk[..read]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }

    let request_text = String::from_utf8_lossy(&request);
    let head_only = request_text.starts_with("HEAD ");
    let range = request_text.lines().find_map(|line| {
        line.to_ascii_lowercase()
            .strip_prefix("range: bytes=")
            .and_then(parse_range)
    });
    let (status, start, end) = match range {
        Some((start, end)) if start < payload.len() => {
            let end = end.unwrap_or(payload.len() - 1).min(payload.len() - 1);
            ("206 Partial Content", start, end.max(start))
        }
        _ => ("200 OK", 0, payload.len().saturating_sub(1)),
    };
    let length = if payload.is_empty() {
        0
    } else {
        end - start + 1
    };
    let mut headers = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {length}\r\nContent-Type: application/octet-stream\r\nAccept-Ranges: bytes\r\nConnection: close\r\n"
    );
    if status.starts_with("206") {
        headers.push_str(&format!(
            "Content-Range: bytes {start}-{end}/{}\r\n",
            payload.len()
        ));
    }
    headers.push_str("\r\n");
    if socket.write_all(headers.as_bytes()).await.is_err() {
        return;
    }
    if !head_only && !payload.is_empty() {
        let _ = socket.write_all(&payload[start..=end]).await;
    }
}

fn parse_range(value: &str) -> Option<(usize, Option<usize>)> {
    let (start, end) = value.trim().split_once('-')?;
    Some((
        start.parse().ok()?,
        (!end.is_empty()).then(|| end.parse().ok()).flatten(),
    ))
}
