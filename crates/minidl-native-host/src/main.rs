//! Browser native-messaging host: a thin, short-lived bridge.
//!
//! The browser launches this per captured job (via `sendNativeMessage`), owns
//! its stdio, and kills it after one reply. The real work runs in the
//! long-lived Tauri app, which this process reaches over a local socket (Unix
//! domain socket on Linux/macOS, named pipe on Windows) — launching the app
//! first if it is not already running.
//!
//! Framing:
//! - Browser side (stdio): uint32 length prefix in **native** byte order + JSON.
//! - App side (local socket): uint32 length prefix in **little-endian** + JSON
//!   (both ends are ours).

use std::io::{Read, Write};
use std::time::Duration;

use interprocess::local_socket::{prelude::*, Stream};

use minidl_ipc::{app_path_file, bridge_socket_name, BridgeReply, BridgeRequest, CaptureJob};

const MAX_MSG: usize = 64 * 1024 * 1024;

fn read_frame<R: Read>(r: &mut R, native: bool) -> Option<Vec<u8>> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len).ok()?;
    let n = if native {
        u32::from_ne_bytes(len)
    } else {
        u32::from_le_bytes(len)
    } as usize;
    if n == 0 || n > MAX_MSG {
        return None;
    }
    let mut buf = vec![0u8; n];
    r.read_exact(&mut buf).ok()?;
    Some(buf)
}

fn write_frame<W: Write>(w: &mut W, msg: &[u8], native: bool) -> std::io::Result<()> {
    let len = msg.len() as u32;
    let prefix = if native {
        len.to_ne_bytes()
    } else {
        len.to_le_bytes()
    };
    w.write_all(&prefix)?;
    w.write_all(msg)?;
    w.flush()
}

/// Connect to the running app; if it is not up, launch it and poll the socket.
fn connect_or_launch() -> Option<Stream> {
    let name = bridge_socket_name().ok()?;
    if let Ok(s) = Stream::connect(name.clone()) {
        return Some(s);
    }
    if let Ok(path) = std::fs::read_to_string(app_path_file()) {
        let path = path.trim();
        if !path.is_empty() {
            // Detach the app's stdio. If it inherited ours, the long-lived GUI
            // would hold the browser's native-messaging stdout pipe (the browser
            // never sees EOF → leaked fd every cold start) and any byte the GUI
            // writes to stdout would corrupt this process's length-prefixed reply
            // frames. The job travels over the UDS, not these pipes.
            use std::process::Stdio;
            let _ = std::process::Command::new(path)
                .arg("--background")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
        }
    }
    for _ in 0..50 {
        if let Ok(s) = Stream::connect(name.clone()) {
            return Some(s);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    None
}

/// Forward one captured job (or a `{"ping":true}` health check) to the app and
/// return its reply bytes.
fn forward(job_bytes: &[u8]) -> Option<Vec<u8>> {
    let is_ping = serde_json::from_slice::<serde_json::Value>(job_bytes)
        .ok()
        .and_then(|v| v.get("ping").and_then(|p| p.as_bool()))
        .unwrap_or(false);
    let req = if is_ping {
        BridgeRequest::ping()
    } else {
        BridgeRequest::new(serde_json::from_slice::<CaptureJob>(job_bytes).ok()?)
    };
    let req_bytes = serde_json::to_vec(&req).ok()?;

    let mut sock = connect_or_launch()?;
    write_frame(&mut sock, &req_bytes, false).ok()?;
    read_frame(&mut sock, false)
}

fn main() {
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let stdout = std::io::stdout();

    // For `sendNativeMessage` this loop runs once; for `connectNative`, per message.
    while let Some(job_bytes) = read_frame(&mut input, true) {
        let reply = forward(&job_bytes).unwrap_or_else(|| {
            serde_json::to_vec(&BridgeReply::rejected("Mini Downloader app unavailable")).unwrap_or_default()
        });
        let mut out = stdout.lock();
        if write_frame(&mut out, &reply, true).is_err() {
            break;
        }
    }
}
