//! Firefox native-messaging host: a thin, short-lived bridge.
//!
//! Firefox launches this per captured job (via `sendNativeMessage`), owns its
//! stdio, and kills it after one reply. The real work runs in the long-lived
//! Tauri app, which this process reaches over a Unix domain socket — launching
//! the app first if it is not already running.
//!
//! Framing:
//! - Firefox side (stdio): uint32 length prefix in **native** byte order + JSON.
//! - App side (UDS): uint32 length prefix in **little-endian** + JSON (both ends
//!   are ours).

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use ldm_ipc::{app_path_file, bridge_socket_path, BridgeReply, BridgeRequest, CaptureJob};

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
fn connect_or_launch() -> Option<UnixStream> {
    if let Ok(s) = UnixStream::connect(bridge_socket_path()) {
        return Some(s);
    }
    if let Ok(path) = std::fs::read_to_string(app_path_file()) {
        let path = path.trim();
        if !path.is_empty() {
            let _ = std::process::Command::new(path).arg("--background").spawn();
        }
    }
    for _ in 0..50 {
        if let Ok(s) = UnixStream::connect(bridge_socket_path()) {
            return Some(s);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    None
}

/// Forward one captured job to the app and return its reply bytes.
fn forward(job_bytes: &[u8]) -> Option<Vec<u8>> {
    let job: CaptureJob = serde_json::from_slice(job_bytes).ok()?;
    let req = BridgeRequest::new(job);
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
            serde_json::to_vec(&BridgeReply::rejected("LDM app unavailable")).unwrap_or_default()
        });
        let mut out = stdout.lock();
        if write_frame(&mut out, &reply, true).is_err() {
            break;
        }
    }
}
