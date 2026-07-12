//! Browser native-messaging host: a thin, short-lived bridge.
//!
//! The browser launches this per captured job (via `sendNativeMessage`), owns
//! its stdio, and kills it after one reply. The real work runs in the
//! long-lived Tauri app, which this process reaches over a local socket (Unix
//! domain socket on Linux, named pipe on Windows) — launching the app
//! first if it is not already running.
//!
//! Framing:
//! - Browser side (stdio): uint32 length prefix in **native** byte order + JSON.
//! - App side (local socket): uint32 length prefix in **little-endian** + JSON
//!   (both ends are ours).

use std::io::{Read, Write};
use std::time::Duration;

use interprocess::local_socket::{prelude::*, Stream};

use minidl_ipc::{
    app_path_file, bridge_socket_name, BridgeReply, BridgeRequest, BrowserFamily, CaptureJob,
    CHROME_EXTENSION_ID, CHROME_STORE_EXTENSION_ID, EXTENSION_ID,
};

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

/// Connect to an already-running app. Connector heartbeats deliberately use
/// this path so a browser opening in the background does not start the GUI.
fn connect_if_running() -> Option<Stream> {
    let name = bridge_socket_name().ok()?;
    Stream::connect(name).ok()
}

/// Connect to the running app; if it is not up, launch it and poll the socket.
/// This remains the capture/manual-ping path.
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

enum NativeMessage {
    Presence(Option<BrowserFamily>),
    Ping(Option<BrowserFamily>),
    Capture(CaptureJob, Option<BrowserFamily>),
}

/// Native-messaging does not tell the host which browser launched it. The
/// connector includes its family in heartbeat messages, and we accept a few
/// stable aliases so Firefox/Chromium forks can use the same wire contract.
fn browser_family(value: &serde_json::Value) -> Option<BrowserFamily> {
    let raw = value
        .get("browserFamily")
        .or_else(|| value.get("browser_family"))
        .and_then(serde_json::Value::as_str)?
        .trim()
        .to_ascii_lowercase();
    match raw.as_str() {
        "firefox" | "gecko" | "mozilla" => Some(BrowserFamily::Firefox),
        "chromium" | "chrome" | "chromium-based" | "chrome-based" => Some(BrowserFamily::Chromium),
        _ => None,
    }
}

/// Chrome passes its calling extension origin as an argument. Firefox passes
/// its native-manifest path and (since Firefox 55) the calling add-on ID.
/// Use those browser-provided arguments first, with a connector-provided
/// family only as a fallback for forks whose invocation shape differs.
fn browser_family_from_invocation<I, S>(args: I) -> Option<BrowserFamily>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    for arg in args {
        let arg = arg.as_ref().trim();
        if arg.eq_ignore_ascii_case(EXTENSION_ID) {
            return Some(BrowserFamily::Firefox);
        }
        let origin = arg
            .strip_prefix("chrome-extension://")
            .or_else(|| arg.strip_prefix("CHROME-EXTENSION://"))
            .unwrap_or(arg)
            .trim_end_matches('/');
        if origin.eq_ignore_ascii_case(CHROME_EXTENSION_ID)
            || origin.eq_ignore_ascii_case(CHROME_STORE_EXTENSION_ID)
        {
            return Some(BrowserFamily::Chromium);
        }
    }
    None
}

fn parse_native_message(job_bytes: &[u8]) -> Option<NativeMessage> {
    let value = serde_json::from_slice::<serde_json::Value>(job_bytes).ok()?;
    let reported_family = browser_family(&value);
    if value
        .get("presence")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        return Some(NativeMessage::Presence(reported_family));
    }
    if value
        .get("ping")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        return Some(NativeMessage::Ping(reported_family));
    }
    serde_json::from_value::<CaptureJob>(value)
        .ok()
        .map(|job| NativeMessage::Capture(job, reported_family))
}

/// Forward one browser message to the app and return its reply bytes. Presence
/// heartbeats only connect to an already-running app; normal captures and the
/// manual options-page ping retain the historical launch-on-demand behavior.
fn forward(job_bytes: &[u8], invoking_family: Option<BrowserFamily>) -> Option<Vec<u8>> {
    let message = parse_native_message(job_bytes)?;
    let (req, presence_only) = match message {
        NativeMessage::Presence(reported_family) => {
            let family = invoking_family.or(reported_family)?;
            (BridgeRequest::presence(family), true)
        }
        NativeMessage::Ping(reported_family) => (
            BridgeRequest::ping().with_browser_family(invoking_family.or(reported_family)),
            false,
        ),
        NativeMessage::Capture(job, reported_family) => (
            BridgeRequest::new(job).with_browser_family(invoking_family.or(reported_family)),
            false,
        ),
    };
    let req_bytes = serde_json::to_vec(&req).ok()?;

    let mut sock = if presence_only {
        connect_if_running()?
    } else {
        connect_or_launch()?
    };
    write_frame(&mut sock, &req_bytes, false).ok()?;
    read_frame(&mut sock, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_presence_family_from_camel_case_wire_message() {
        let msg = parse_native_message(br#"{"presence":true,"browserFamily":"firefox"}"#);
        assert!(matches!(
            msg,
            Some(NativeMessage::Presence(Some(BrowserFamily::Firefox)))
        ));
    }

    #[test]
    fn accepts_chromium_family_aliases() {
        let msg = parse_native_message(br#"{"presence":true,"browser_family":"chrome-based"}"#);
        assert!(matches!(
            msg,
            Some(NativeMessage::Presence(Some(BrowserFamily::Chromium)))
        ));
    }

    #[test]
    fn invalid_presence_requires_a_family_before_forwarding() {
        assert!(matches!(
            parse_native_message(br#"{"presence":true,"browserFamily":"unknown"}"#),
            Some(NativeMessage::Presence(None))
        ));
    }

    #[test]
    fn invocation_arguments_identify_supported_browser_families() {
        assert_eq!(
            browser_family_from_invocation(["/path/host.json", EXTENSION_ID]),
            Some(BrowserFamily::Firefox)
        );
        assert_eq!(
            browser_family_from_invocation([
                "chrome-extension://hhaobmkdgijodfieadeeanjmnneckafj/"
            ]),
            Some(BrowserFamily::Chromium)
        );
    }
}

fn main() {
    let invoking_family = browser_family_from_invocation(std::env::args().skip(1));
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let stdout = std::io::stdout();

    // For `sendNativeMessage` this loop runs once; for `connectNative`, per message.
    while let Some(job_bytes) = read_frame(&mut input, true) {
        let reply = forward(&job_bytes, invoking_family).unwrap_or_else(|| {
            serde_json::to_vec(&BridgeReply::rejected("Mini Downloader app unavailable"))
                .unwrap_or_default()
        });
        let mut out = stdout.lock();
        if write_frame(&mut out, &reply, true).is_err() {
            break;
        }
    }
}
