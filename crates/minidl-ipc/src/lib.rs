//! Shared IPC contract for Mini Downloader.
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

/// Protocol spoken between the lightweight controller and disposable native
/// UI clients. It is deliberately independent from [`PROTOCOL_VERSION`]: the
/// browser connector and desktop UI can then evolve without forcing each
/// other to update in lockstep.
pub const CONTROL_PROTOCOL_VERSION: u32 = 2;

/// Native-messaging host name — must equal the manifest `name` and the string
/// the extension passes to `runtime.connectNative()` / `sendNativeMessage()`.
pub const NATIVE_HOST_NAME: &str = "com.minidownloader.host";

/// Firefox add-on id — must match `browser_specific_settings.gecko.id` and the
/// host manifest `allowed_extensions`.
pub const EXTENSION_ID: &str = "minidownloader@ramazan.dev";

/// Chromium extension id for an **unpacked** load — derived from the `key` in
/// `manifest.chrome.json` — used in the host manifest `allowed_origins`.
pub const CHROME_EXTENSION_ID: &str = "lkllgjnnglfjifnioojkcbefjlfmfahi";

/// Chromium extension id assigned by the **Chrome Web Store** on publish (the
/// store strips the `key`, so the id differs from the unpacked one). Both are
/// allowed by the native host so store + unpacked installs work.
pub const CHROME_STORE_EXTENSION_ID: &str = "hhaobmkdgijodfieadeeanjmnneckafj";

/// Browser family that established a native-messaging connection. This is
/// supplied by the connector itself: native-messaging manifests do not expose
/// the calling browser to the host process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BrowserFamily {
    Firefox,
    Chromium,
}

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
    /// Bulk-capture grouping: jobs sharing a `batch_id` land in one package on
    /// the app side. Optional and additive — absent for single captures and on
    /// older extensions, so this is not a protocol break.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_id: Option<String>,
    /// Human-readable name for the batch's package (page title or host).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_name: Option<String>,
}

fn default_kind() -> DownloadKind {
    DownloadKind::Http
}

impl CaptureJob {
    /// Build the ordered list of `"Name: value"` header lines to replay this
    /// request — the exact shape aria2's `header` option and yt-dlp's
    /// `--add-header` both consume. aria2-specific wiring lives in `minidl-core`;
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
    /// A connectivity check from the extension — the app replies ok without
    /// ingesting anything. Lets the options page confirm the bridge is wired.
    #[serde(default)]
    pub ping: bool,
    /// Browser family reported by the native host from its invocation origin
    /// (or, for an unknown fork, by the connector message). This annotates
    /// regular captures and manual pings as well as presence heartbeats.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser_family: Option<BrowserFamily>,
    /// A silent connector-presence heartbeat. Unlike `ping`, the native host
    /// must never start the desktop app just to deliver this message.
    #[serde(default)]
    pub presence: bool,
}

impl BridgeRequest {
    pub fn new(job: CaptureJob) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            job,
            ping: false,
            browser_family: None,
            presence: false,
        }
    }

    /// A health-check request carrying a placeholder job.
    pub fn ping() -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            ping: true,
            browser_family: None,
            presence: false,
            job: CaptureJob {
                url: "ping://".into(),
                filename: None,
                referrer: None,
                user_agent: None,
                cookie: None,
                extra_headers: Vec::new(),
                kind: DownloadKind::Http,
                mime: None,
                size: None,
                page_url: None,
                cookie_store_id: None,
                torrent_b64: None,
                batch_id: None,
                batch_name: None,
            },
        }
    }

    /// A connector heartbeat carrying no capture job. The desktop app records
    /// the browser family and returns an acknowledgement without focusing the
    /// window or adding a download.
    pub fn presence(browser_family: BrowserFamily) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            ping: false,
            browser_family: Some(browser_family),
            presence: true,
            job: CaptureJob {
                url: "presence://".into(),
                filename: None,
                referrer: None,
                user_agent: None,
                cookie: None,
                extra_headers: Vec::new(),
                kind: DownloadKind::Http,
                mime: None,
                size: None,
                page_url: None,
                cookie_store_id: None,
                torrent_b64: None,
                batch_id: None,
                batch_name: None,
            },
        }
    }

    /// Annotate a regular capture or manual ping with the browser family that
    /// launched the native host. This is additive, so older app/host pairs
    /// continue to exchange the same capture payload.
    pub fn with_browser_family(mut self, browser_family: Option<BrowserFamily>) -> Self {
        self.browser_family = browser_family;
        self
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
    /// Acknowledge a non-capture bridge message such as a ping or presence
    /// heartbeat.
    pub fn acknowledged() -> Self {
        Self {
            ok: true,
            job_id: None,
            error: None,
        }
    }

    pub fn accepted(job_id: i64) -> Self {
        Self {
            ok: true,
            job_id: Some(job_id),
            error: None,
        }
    }
    pub fn rejected(error: impl Into<String>) -> Self {
        Self {
            ok: false,
            job_id: None,
            error: Some(error.into()),
        }
    }
}

/// Persisted yt-dlp choices carried over the controller boundary.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MediaOptions {
    pub write_subs: bool,
    pub sub_langs: String,
    pub embed_subs: bool,
    pub audio_only: bool,
    pub audio_format: String,
    pub embed_thumbnail: bool,
}

/// A shallow playlist entry selected by the user after a yt-dlp probe.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaylistSelection {
    pub url: String,
    pub title: Option<String>,
}

/// One validated, atomic settings update from a native UI. Keeping the full
/// draft in one request prevents a failed middle command from leaving half of
/// the Settings drawer persisted.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SettingsUpdate {
    pub proxy: String,
    pub max_concurrent: u32,
    pub split: u32,
    pub connections: u32,
    pub clipboard: bool,
    pub theme: String,
    pub auto_organize: bool,
    pub prevent_duplicates: bool,
    pub close_to_tray: bool,
    pub autostart: bool,
    pub handle_magnets: bool,
    pub dht: bool,
    pub sandbox: bool,
    pub block_private: bool,
    pub locale: String,
    pub on_complete_action: String,
    pub on_complete_confirmed: bool,
    pub default_speed_limit: i64,
}

impl Default for SettingsUpdate {
    fn default() -> Self {
        Self {
            proxy: String::new(),
            max_concurrent: 5,
            split: 16,
            connections: 16,
            clipboard: false,
            theme: "system".into(),
            auto_organize: true,
            prevent_duplicates: true,
            close_to_tray: true,
            autostart: false,
            handle_magnets: true,
            dht: true,
            sandbox: false,
            block_private: false,
            locale: "system".into(),
            on_complete_action: "none".into(),
            on_complete_confirmed: false,
            default_speed_limit: 0,
        }
    }
}

/// Complete command surface for the native UI. Arguments stay strongly typed;
/// successful results are serialized domain objects because those types live
/// in `minidl-core`, which already depends on this small IPC crate.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command", content = "args", rename_all = "snake_case")]
pub enum ControlCommand {
    Ping,
    Show,
    Hide,
    Quit,
    Snapshot,
    Subscribe,
    IngestUrl {
        url: String,
        checksum: Option<String>,
    },
    IngestFile {
        path: String,
        kind: DownloadKind,
    },
    ListDownloads {
        status: Option<String>,
    },
    Pause {
        id: i64,
    },
    Resume {
        id: i64,
    },
    Retry {
        id: i64,
    },
    Remove {
        id: i64,
        delete_files: bool,
    },
    RemoveCompleted,
    RemoveFailed,
    PauseAll,
    ResumeAll,
    MoveInQueue {
        id: i64,
        direction: String,
    },
    SetQueuePosition {
        id: i64,
        position: i64,
    },
    SetGlobalSpeed {
        download: i64,
        upload: i64,
    },
    SetDownloadSpeed {
        id: i64,
        limit: i64,
    },
    OpenFile {
        id: i64,
    },
    OpenContainingFolder {
        id: i64,
    },
    ProbeMedia {
        url: String,
        playlist: bool,
    },
    AddMedia {
        url: String,
        format_id: Option<String>,
        options: Option<MediaOptions>,
    },
    AddPlaylist {
        entries: Vec<PlaylistSelection>,
        package_name: String,
        quality: Option<String>,
        options: Option<MediaOptions>,
    },
    GrabLinks {
        text: String,
    },
    AddLinks {
        urls: Vec<String>,
        package_name: Option<String>,
    },
    ListPackages,
    ListCategories,
    SaveCategory {
        name: String,
        dir: String,
        rules: String,
        priority: i64,
    },
    MoveCategory {
        id: i64,
        direction: i32,
    },
    DeleteCategory {
        id: i64,
    },
    RestoreDefaultCategories,
    ResetCategoryDir {
        id: i64,
    },
    GetDownloadDetails {
        id: i64,
    },
    SetTorrentFiles {
        id: i64,
        indices: Vec<u32>,
    },
    ScheduleDownload {
        id: i64,
        start_at: Option<i64>,
    },
    ListSchedules,
    SaveSchedule {
        id: Option<i64>,
        name: Option<String>,
        action: String,
        days_mask: i64,
        at_minute: i64,
        speed_limit: Option<i64>,
        enabled: bool,
    },
    DeleteSchedule {
        id: i64,
    },
    GetSetting {
        key: String,
    },
    SetSetting {
        key: String,
        value: String,
    },
    GetSystemLocale,
    GetConnectorStatus,
    SetHandleMagnets {
        enabled: bool,
    },
    SetClipboardWatch {
        enabled: bool,
    },
    GetEngineDefaults,
    SetEngineDefaults {
        split: u32,
        connections: u32,
    },
    GetMaxConcurrent,
    SetMaxConcurrent {
        value: u32,
    },
    ApplyProxy {
        value: String,
    },
    ApplySettings {
        settings: SettingsUpdate,
    },
    CheckUpdate,
    InstallUpdate {
        asset_url: Option<String>,
        page_url: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlRequest {
    pub protocol_version: u32,
    pub request_id: u64,
    pub command: ControlCommand,
}

impl ControlRequest {
    pub fn new(request_id: u64, command: ControlCommand) -> Self {
        Self {
            protocol_version: CONTROL_PROTOCOL_VERSION,
            request_id,
            command,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlError {
    pub kind: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlReply {
    pub request_id: u64,
    pub result: Option<serde_json::Value>,
    pub error: Option<ControlError>,
}

impl ControlReply {
    pub fn success(request_id: u64, result: impl Serialize) -> Self {
        Self {
            request_id,
            result: serde_json::to_value(result).ok(),
            error: None,
        }
    }

    pub fn failure(request_id: u64, kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            request_id,
            result: None,
            error: Some(ControlError {
                kind: kind.into(),
                message: message.into(),
            }),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlEvent {
    pub sequence: u64,
    pub event: String,
    pub payload: serde_json::Value,
}

/// Path of the Unix domain socket the app listens on and the native host
/// connects to. Lives under `$XDG_RUNTIME_DIR` (user-private tmpfs, wiped on
/// logout). When the runtime dir is unset it falls back to the per-user data dir
/// — deliberately **not** `/tmp`: a world-writable `/tmp/minidownloader` would
/// let another local user pre-create the directory and squat the socket,
/// capturing the cookies/headers the native host forwards. The data dir lives
/// under the user's home, which other users cannot write into. Unix only —
/// Windows uses a named pipe (see [`bridge_socket_name`]).
#[cfg(unix)]
pub fn bridge_socket_path() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(rt) => PathBuf::from(rt).join("minidownloader").join("bridge.sock"),
        None => data_dir().join("bridge.sock"),
    }
}

/// Build a stable, opaque namespace for Windows pipes from the per-user data
/// directory. Named-pipe names are machine-global, so a fixed name prevents
/// two simultaneously signed-in users from running the app. The native host
/// and controller independently resolve the same Known Folder path.
#[cfg(windows)]
fn windows_pipe_name(kind: &str) -> String {
    // FNV-1a is intentionally implemented here instead of using
    // `DefaultHasher`, whose output is not a stability guarantee. This is a
    // namespace discriminator, not a secret or a cryptographic credential.
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in data_dir().to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("minidownloader-{kind}-{hash:016x}")
}

/// Cross-platform local-socket name for the bridge: a filesystem Unix domain
/// socket on Unix (user-private and mode 0600), or a per-user namespaced Windows
/// named pipe. Windows clients open the duplex pipe read/write and remote pipe
/// clients are rejected by the transport.
pub fn bridge_socket_name() -> std::io::Result<interprocess::local_socket::Name<'static>> {
    #[cfg(unix)]
    {
        use interprocess::local_socket::{GenericFilePath, ToFsName};
        bridge_socket_path().to_fs_name::<GenericFilePath>()
    }
    #[cfg(windows)]
    {
        use interprocess::local_socket::{GenericNamespaced, ToNsName};
        windows_pipe_name("bridge").to_ns_name::<GenericNamespaced>()
    }
}

/// Private socket used by native UI clients and later invocations. It is kept
/// separate from the browser bridge so the stable extension protocol remains
/// byte-for-byte compatible.
#[cfg(unix)]
pub fn control_socket_path() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(rt) => PathBuf::from(rt)
            .join("minidownloader")
            .join("control.sock"),
        None => data_dir().join("control.sock"),
    }
}

pub fn control_socket_name() -> std::io::Result<interprocess::local_socket::Name<'static>> {
    #[cfg(unix)]
    {
        use interprocess::local_socket::{GenericFilePath, ToFsName};
        control_socket_path().to_fs_name::<GenericFilePath>()
    }
    #[cfg(windows)]
    {
        use interprocess::local_socket::{GenericNamespaced, ToNsName};
        windows_pipe_name("control").to_ns_name::<GenericNamespaced>()
    }
}

/// Per-user persistent data dir (`~/.local/share/minidownloader` on Linux,
/// `%APPDATA%\minidownloader` on Windows). Shared by the app and the native
/// host so the host can find where the app binary lives.
pub fn data_dir() -> PathBuf {
    if let Some(directory) = dirs::data_dir() {
        return directory.join("minidownloader");
    }
    if let Some(home) = dirs::home_dir() {
        return home.join(".local/share/minidownloader");
    }
    #[cfg(unix)]
    {
        // A stable UID-specific fallback lets the app and native host rendezvous
        // without sharing a predictable world-writable `/tmp/minidownloader`.
        // Callers create it as 0700 and fail if another user pre-created it.
        let uid = unsafe { libc::geteuid() };
        std::env::temp_dir().join(format!("minidownloader-{uid}"))
    }
    #[cfg(not(unix))]
    {
        // Known Folders are available on supported Windows versions. Retain a
        // stable last resort for restricted test/service environments.
        let user = std::env::var("USERNAME")
            .unwrap_or_else(|_| "current-user".into())
            .chars()
            .filter(|character| character.is_ascii_alphanumeric() || *character == '-')
            .collect::<String>();
        std::env::temp_dir().join(format!("minidownloader-{user}"))
    }
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
            batch_id: None,
            batch_name: None,
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

    #[cfg(unix)]
    #[test]
    fn bridge_socket_under_runtime_dir() {
        // Just assert the tail; the base varies by environment. `Path::ends_with`
        // matches whole components, so this checks the last two path segments.
        let p = bridge_socket_path();
        assert!(p.ends_with("minidownloader/bridge.sock"));
    }

    #[test]
    fn presence_request_is_distinct_from_a_regular_capture() {
        let req = BridgeRequest::presence(BrowserFamily::Chromium);
        assert!(req.presence);
        assert_eq!(req.browser_family, Some(BrowserFamily::Chromium));
        assert!(!req.ping);
    }

    #[test]
    fn control_request_round_trips_with_typed_arguments() {
        let request = ControlRequest::new(
            42,
            ControlCommand::SetDownloadSpeed {
                id: 9,
                limit: 1_000_000,
            },
        );
        let bytes = serde_json::to_vec(&request).unwrap();
        let decoded: ControlRequest = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(decoded.protocol_version, CONTROL_PROTOCOL_VERSION);
        assert_eq!(decoded.request_id, 42);
        assert!(matches!(
            decoded.command,
            ControlCommand::SetDownloadSpeed {
                id: 9,
                limit: 1_000_000
            }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn control_socket_is_distinct_and_private_by_location() {
        assert!(control_socket_path().ends_with("minidownloader/control.sock"));
        assert_ne!(control_socket_path(), bridge_socket_path());
    }

    #[cfg(windows)]
    #[test]
    fn windows_pipe_names_are_stable_distinct_and_user_namespaced() {
        let bridge = windows_pipe_name("bridge");
        let control = windows_pipe_name("control");
        assert_eq!(bridge, windows_pipe_name("bridge"));
        assert_ne!(bridge, control);
        assert!(bridge.starts_with("minidownloader-bridge-"));
        assert_eq!(bridge.len(), "minidownloader-bridge-".len() + 16);
    }
}
