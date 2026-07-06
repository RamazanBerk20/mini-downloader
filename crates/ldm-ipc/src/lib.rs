//! Shared IPC contract for Linux Download Manager.
//!
//! This is the single canonical definition of the message a captured download
//! travels in, from the Firefox extension all the way to the aria2 engine. The
//! extension serializes exactly [`CaptureJob`]; the native-host binary forwards
//! a [`BridgeRequest`] over the Unix domain socket; the app replies with a
//! [`BridgeReply`]. Both ends MUST agree on this crate — do not redefine these
//! shapes anywhere else.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Wire protocol version. Bump on any breaking change to the structs below.
/// The app rejects a [`BridgeRequest`] whose `protocol_version` it does not
/// understand, so the extension and app can update out of lockstep safely.
pub const PROTOCOL_VERSION: u32 = 1;

/// Native-messaging host name — must equal the manifest `name` and the string
/// the extension passes to `runtime.connectNative()` / `sendNativeMessage()`.
pub const NATIVE_HOST_NAME: &str = "com.ldm.host";

/// Firefox add-on id — must match `browser_specific_settings.gecko.id` and the
/// host manifest `allowed_extensions`.
pub const EXTENSION_ID: &str = "ldm@ramazan.dev";

/// Chromium extension id — derived from the `key` in `manifest.chrome.json`,
/// used in the host manifest `allowed_origins` (`chrome-extension://<id>/`).
pub const CHROME_EXTENSION_ID: &str = "lkllgjnnglfjifnioojkcbefjlfmfahi";

/// What kind of source a captured job points at. The app routes each kind to
/// the right aria2 method or to yt-dlp.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DownloadKind {
    /// Plain HTTP(S) file → aria2 `addUri`.
    Http,
    /// HLS manifest (`.m3u8`) → yt-dlp drives.
    Hls,
    /// DASH manifest (`.mpd`) → yt-dlp drives.
    Dash,
    /// `.torrent` file (base64 payload in `torrent_b64`) → aria2 `addTorrent`.
    Torrent,
    /// `magnet:` URI → aria2 `addUri`.
    Magnet,
    /// Metalink (`.meta4`, base64 in `torrent_b64`) → aria2 `addMetalink`.
    Metalink,
}

/// A single captured download, with everything aria2/yt-dlp need to replay the
/// exact request the browser would have made (cookies/headers are "the whole
/// ballgame" for authenticated downloads).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureJob {
    pub url: String,
    /// Suggested output filename (from `Content-Disposition` or the URL).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub referrer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
    /// Raw `Cookie:` header value (`k=v; k2=v2`), captured from the request or
    /// reconstructed from the cookie jar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cookie: Option<String>,
    /// Any other request headers worth replaying (Authorization, Origin, …).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_headers: Vec<(String, String)>,
    #[serde(default = "default_kind")]
    pub kind: DownloadKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    /// Content length in bytes, or `-1`/`None` when unknown (chunked).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<i64>,
    /// Originating page URL — hand this (not a `blob:`/MSE `url`) to yt-dlp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_url: Option<String>,
    /// Firefox container / cookie-store identity, so the right jar is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cookie_store_id: Option<String>,
    /// base64 payload for `.torrent` / `.meta4` kinds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub torrent_b64: Option<String>,
}

fn default_kind() -> DownloadKind {
    DownloadKind::Http
}

impl CaptureJob {
    /// Build the ordered list of `"Name: value"` header lines to replay this
    /// request — the exact shape aria2's `header` option and yt-dlp's
    /// `--add-header` both consume. aria2-specific wiring lives in `ldm-core`;
    /// this stays dependency-free.
    pub fn header_lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(r) = &self.referrer {
            out.push(format!("Referer: {r}"));
        }
        if let Some(ua) = &self.user_agent {
            out.push(format!("User-Agent: {ua}"));
        }
        if let Some(c) = &self.cookie {
            out.push(format!("Cookie: {c}"));
        }
        for (name, value) in &self.extra_headers {
            out.push(format!("{name}: {value}"));
        }
        out
    }
}

/// Envelope the native host forwards to the app over the UDS.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeRequest {
    pub protocol_version: u32,
    pub job: CaptureJob,
}

impl BridgeRequest {
    pub fn new(job: CaptureJob) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            job,
        }
    }
}

/// The app's reply, relayed by the host back to the extension.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeReply {
    pub ok: bool,
    /// App-side stable download id, when accepted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl BridgeReply {
    pub fn accepted(job_id: i64) -> Self {
        Self { ok: true, job_id: Some(job_id), error: None }
    }
    pub fn rejected(error: impl Into<String>) -> Self {
        Self { ok: false, job_id: None, error: Some(error.into()) }
    }
}

/// Path of the Unix domain socket the app listens on and the native host
/// connects to. Lives under `$XDG_RUNTIME_DIR` (user-private, wiped on logout);
/// falls back to `/tmp` when the runtime dir is unset.
pub fn bridge_socket_path() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join("ldm").join("bridge.sock")
}

/// `$XDG_DATA_HOME/ldm` or `~/.local/share/ldm`. Persistent; shared by the app
/// and the native host so the host can find where the app binary lives.
pub fn data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join(".local/share")
        })
        .join("ldm")
}

/// File the app writes with the absolute path to its own executable, so the
/// native host can launch the app when it is not already running.
pub fn app_path_file() -> PathBuf {
    data_dir().join("app-exec-path")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_lines_order_and_content() {
        let job = CaptureJob {
            url: "https://example.org/f.iso".into(),
            filename: Some("f.iso".into()),
            referrer: Some("https://example.org/page".into()),
            user_agent: Some("Mozilla/5.0".into()),
            cookie: Some("sid=abc; t=1".into()),
            extra_headers: vec![("Authorization".into(), "Bearer x".into())],
            kind: DownloadKind::Http,
            mime: None,
            size: Some(-1),
            page_url: None,
            cookie_store_id: None,
            torrent_b64: None,
        };
        assert_eq!(
            job.header_lines(),
            vec![
                "Referer: https://example.org/page",
                "User-Agent: Mozilla/5.0",
                "Cookie: sid=abc; t=1",
                "Authorization: Bearer x",
            ]
        );
    }

    #[test]
    fn bridge_socket_under_runtime_dir() {
        // Just assert the tail; the base varies by environment.
        let p = bridge_socket_path();
        assert!(p.ends_with("ldm/bridge.sock"));
    }
}
