// Native messaging uses inherited stdin/stdout pipes and never needs a visible
// console. Keep debug/test builds as console apps so local failures stay visible.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

//! Browser native-messaging host: a thin, short-lived bridge.
//!
//! The browser launches this per captured job (via `sendNativeMessage`), owns
//! its stdio, and kills it after one reply. The real work runs in the
//! long-lived native controller, which this process reaches over a local socket (Unix
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
    app_path_file, bridge_socket_name, Bridge, BridgeReply, BridgeRequest, BrowserFamily,
    CaptureJob, CHROME_EXTENSION_ID, CHROME_STORE_EXTENSION_ID, EXTENSION_ID,
};

const MAX_MSG: usize = 16 * 1024 * 1024;

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
    if msg.is_empty() || msg.len() > MAX_MSG {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid native-message length",
        ));
    }
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
    Stream::connect(name).ok().and_then(owned_by_us)
}

/// On Windows the pipe name is not a secret: only talk to a server that runs
/// as the current user. (A Unix socket lives in the user's private runtime dir.)
fn owned_by_us(stream: Stream) -> Option<Stream> {
    #[cfg(windows)]
    {
        let pid = stream.peer_creds().ok()?.pid()?;
        minidl_ipc::ensure_same_user(pid).ok()?;
    }
    Some(stream)
}

/// Connect to the running app; if it is not up, launch it and poll the socket.
/// This remains the capture/manual-ping path.
fn connect_or_launch() -> Option<Stream> {
    let name = bridge_socket_name().ok()?;
    if let Ok(s) = Stream::connect(name.clone()) {
        return owned_by_us(s);
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
            let mut command = std::process::Command::new(path);
            hide_console_window(&mut command);
            let _ = command
                .arg("--background")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
        }
    }
    for _ in 0..50 {
        if let Ok(s) = Stream::connect(name.clone()) {
            return owned_by_us(s);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    None
}

fn hide_console_window(command: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    #[cfg(not(windows))]
    let _ = command;
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

/// Read one browser message into its [`Bridge`] kind. The browser-provided
/// invocation family wins over the one the connector reports. A presence
/// heartbeat without any family is dropped.
fn parse_native_message(job_bytes: &[u8], invoking: Option<BrowserFamily>) -> Option<Bridge> {
    let value = serde_json::from_slice::<serde_json::Value>(job_bytes).ok()?;
    let family = invoking.or_else(|| browser_family(&value));
    let flag = |name| value.get(name).and_then(serde_json::Value::as_bool) == Some(true);
    if flag("presence") {
        return family.map(Bridge::Presence);
    }
    if flag("ping") {
        return Some(Bridge::Ping(family));
    }
    serde_json::from_value::<CaptureJob>(value)
        .ok()
        .map(|job| Bridge::Capture(Box::new(job), family))
}

/// Forward one browser message to the app and return its reply bytes. Presence
/// heartbeats only connect to an already-running app; normal captures and the
/// manual options-page ping retain the historical launch-on-demand behavior.
fn forward(job_bytes: &[u8], invoking_family: Option<BrowserFamily>) -> Option<Vec<u8>> {
    let message = parse_native_message(job_bytes, invoking_family)?;
    let presence_only = matches!(message, Bridge::Presence(_));
    let req_bytes = serde_json::to_vec(&BridgeRequest::from(message)).ok()?;

    let mut sock = if presence_only {
        connect_if_running()?
    } else {
        connect_or_launch()?
    };
    write_frame(&mut sock, &req_bytes, false).ok()?;
    read_frame(&mut sock, false)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_message_limit_is_bounded() {
        assert_eq!(MAX_MSG, 16 * 1024 * 1024);
        assert!(write_frame(&mut Vec::new(), &[], true).is_err());
        assert!(write_frame(&mut Vec::new(), &vec![0; MAX_MSG + 1], true).is_err());
    }

    #[test]
    fn parses_presence_family_from_camel_case_wire_message() {
        let msg = parse_native_message(br#"{"presence":true,"browserFamily":"firefox"}"#, None);
        assert!(matches!(
            msg,
            Some(Bridge::Presence(BrowserFamily::Firefox))
        ));
    }

    #[test]
    fn accepts_chromium_family_aliases() {
        let msg = parse_native_message(
            br#"{"presence":true,"browser_family":"chrome-based"}"#,
            None,
        );
        assert!(matches!(
            msg,
            Some(Bridge::Presence(BrowserFamily::Chromium))
        ));
    }

    #[test]
    fn invalid_presence_requires_a_family_before_forwarding() {
        let unknown = br#"{"presence":true,"browserFamily":"unknown"}"#;
        assert!(parse_native_message(unknown, None).is_none());
        assert!(matches!(
            parse_native_message(unknown, Some(BrowserFamily::Firefox)),
            Some(Bridge::Presence(BrowserFamily::Firefox))
        ));
    }

    #[test]
    fn captures_and_pings_from_the_published_connector_still_parse() {
        assert!(matches!(
            parse_native_message(br#"{"ping":true}"#, None),
            Some(Bridge::Ping(None))
        ));
        let capture =
            br#"{"url":"https://example.org/f.iso","kind":"http","browserFamily":"chromium"}"#;
        assert!(matches!(
            parse_native_message(capture, None),
            Some(Bridge::Capture(job, Some(BrowserFamily::Chromium))) if job.url == "https://example.org/f.iso"
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
