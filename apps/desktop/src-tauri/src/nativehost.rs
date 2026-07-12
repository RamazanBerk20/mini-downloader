//! App-side of the browser bridge: a local-socket listener (Unix domain socket
//! on Linux, named pipe on Windows) the native host forwards captured
//! jobs to, plus native-messaging manifest installation (files on Unix,
//! registry keys on Windows).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use interprocess::local_socket::tokio::{prelude::*, Stream};
use interprocess::local_socket::ListenerOptions;
use serde::Serialize;
use serde_json::json;
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use minidl_core::aria2::{Engine, EngineDefaults};
use minidl_core::db::Db;
use minidl_core::ipc::{self, BridgeReply, BridgeRequest, BrowserFamily};

use crate::events::{EV_CONNECTOR_STATUS, EV_STATE};
use crate::ingest::ingest;

const MAX_MSG: usize = 64 * 1024 * 1024;

/// Session-local timestamps from connector messages that successfully reached
/// the desktop app. They deliberately are not persisted: an old confirmation
/// must not be presented as evidence that an extension remains installed.
#[derive(Debug, Default)]
pub struct ConnectorPresence {
    firefox_last_seen: Option<i64>,
    chromium_last_seen: Option<i64>,
}

/// Browser integration state returned to the frontend. `*Detected` and
/// `*LastSeen` represent a real connector → native-host → running-app
/// confirmation. `*ConnectorInstalled` is a read-only fallback based on an
/// active connector entry in a known browser profile, so older store builds
/// that predate presence heartbeats can still be recognized. `*ProfileDetected`
/// tells the UI which supported browser family has local profile data; it does
/// not claim that an executable is currently running.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectorStatus {
    pub firefox_profile_detected: bool,
    pub chromium_profile_detected: bool,
    pub firefox_detected: bool,
    pub chromium_detected: bool,
    pub firefox_connector_installed: bool,
    pub chromium_connector_installed: bool,
    pub firefox_last_seen: Option<i64>,
    pub chromium_last_seen: Option<i64>,
}

fn unix_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

/// Record a confirmed connector message and return the fresh status snapshot.
pub fn record_connector_presence(
    presence: &Arc<Mutex<ConnectorPresence>>,
    family: BrowserFamily,
) -> ConnectorStatus {
    let now = unix_millis();
    {
        let mut presence = presence
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match family {
            BrowserFamily::Firefox => presence.firefox_last_seen = Some(now),
            BrowserFamily::Chromium => presence.chromium_last_seen = Some(now),
        }
    }
    connector_status(presence)
}

/// Snapshot connector state for the settings page. Profile detection is kept
/// separate from a live confirmation: it avoids a false warning for existing
/// connectors, but never claims that a browser is currently connected.
pub fn connector_status(presence: &Arc<Mutex<ConnectorPresence>>) -> ConnectorStatus {
    let (firefox_last_seen, chromium_last_seen) = {
        let presence = presence
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        (presence.firefox_last_seen, presence.chromium_last_seen)
    };
    let firefox_roots = firefox_profile_roots();
    let chromium_roots = chromium_profile_roots();
    let firefox_profile_detected = profile_root_detected(&firefox_roots);
    let chromium_profile_detected = profile_root_detected(&chromium_roots);
    let (firefox_connector_installed, chromium_connector_installed) =
        installed_connectors_in_profiles(&firefox_roots, &chromium_roots);
    ConnectorStatus {
        firefox_profile_detected,
        chromium_profile_detected,
        firefox_detected: firefox_last_seen.is_some(),
        chromium_detected: chromium_last_seen.is_some(),
        firefox_connector_installed,
        chromium_connector_installed,
        firefox_last_seen,
        chromium_last_seen,
    }
}

/// Return whether a known profile root exists for each browser family. This is
/// deliberately profile-based: it works for Flatpak/Snap layouts and is the
/// same criterion used for connector and native-host discovery. If neither
/// root exists (for example, a newly installed browser not yet launched), the
/// frontend keeps both store choices available.
fn profile_root_detected(roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| root.is_dir())
}

/// Return `(Firefox-family, Chromium-family)` connector presence from active
/// browser profile metadata. This reads only the extension registry/settings
/// files; it neither launches browsers nor reads browsing history.
fn installed_connectors_in_profiles(
    firefox_roots: &[PathBuf],
    chromium_roots: &[PathBuf],
) -> (bool, bool) {
    (
        firefox_connector_installed_in_roots(firefox_roots),
        chromium_connector_installed_in_roots(chromium_roots),
    )
}

fn firefox_connector_installed_in_roots(roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| {
        profile_dirs(root).into_iter().any(|profile| {
            match firefox_connector_state(&profile) {
                Some(active) => active,
                // If a browser has not created extensions.json yet, the named
                // XPI is the best available fallback. A parsed inactive entry
                // deliberately stays inactive instead of using this fallback.
                None => profile
                    .join("extensions")
                    .join(format!("{}.xpi", ipc::EXTENSION_ID))
                    .is_file(),
            }
        })
    })
}

fn firefox_connector_state(profile: &Path) -> Option<bool> {
    let raw = std::fs::read_to_string(profile.join("extensions.json")).ok()?;
    firefox_connector_state_from_json(&raw)
}

fn firefox_connector_state_from_json(raw: &str) -> Option<bool> {
    let document = serde_json::from_str::<serde_json::Value>(raw).ok()?;
    let addons = document.get("addons")?.as_array()?;
    let addon = addons.iter().find(|addon| {
        addon.get("id").and_then(serde_json::Value::as_str) == Some(ipc::EXTENSION_ID)
    })?;
    Some(
        addon
            .get("active")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or_else(|| {
                !addon
                    .get("userDisabled")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
                    && !addon
                        .get("appDisabled")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false)
            }),
    )
}

fn chromium_connector_installed_in_roots(roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| {
        profile_dirs(root).into_iter().any(|profile| {
            match chromium_connector_state(&profile) {
                Some(active) => active,
                // Chromium keeps extension bundles under the profile. Use this
                // only when Preferences is unavailable/unreadable; a parsed
                // disabled extension must not be reported as installed.
                None => chromium_connector_bundle_exists(&profile),
            }
        })
    })
}

fn chromium_connector_state(profile: &Path) -> Option<bool> {
    let raw = std::fs::read_to_string(profile.join("Preferences")).ok()?;
    chromium_connector_state_from_json(&raw)
}

fn chromium_connector_state_from_json(raw: &str) -> Option<bool> {
    let document = serde_json::from_str::<serde_json::Value>(raw).ok()?;
    let settings = document.pointer("/extensions/settings")?.as_object()?;
    let mut found = false;
    for id in [ipc::CHROME_STORE_EXTENSION_ID, ipc::CHROME_EXTENSION_ID] {
        if let Some(entry) = settings.get(id) {
            found = true;
            let state = entry.get("state");
            if state.and_then(serde_json::Value::as_i64) == Some(1)
                || state.and_then(serde_json::Value::as_str) == Some("1")
            {
                return Some(true);
            }
        }
    }
    found.then_some(false)
}

fn chromium_connector_bundle_exists(profile: &Path) -> bool {
    [ipc::CHROME_STORE_EXTENSION_ID, ipc::CHROME_EXTENSION_ID]
        .iter()
        .any(|id| profile.join("Extensions").join(id).is_dir())
}

/// A browser profile is either the root itself (portable layouts) or an
/// immediate child such as `Default`, `Profile 1`, or Zen's named profile.
fn profile_dirs(root: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if root.is_dir() {
        dirs.push(root.to_path_buf());
        if let Ok(entries) = std::fs::read_dir(root) {
            dirs.extend(
                entries
                    .flatten()
                    .filter_map(|entry| entry.file_type().ok()?.is_dir().then(|| entry.path())),
            );
        }
    }
    dirs
}

#[cfg(unix)]
fn profile_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

#[cfg(windows)]
fn profile_home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE").map(PathBuf::from)
}

#[cfg(not(any(unix, windows)))]
fn profile_home() -> Option<PathBuf> {
    None
}

#[cfg(unix)]
fn xdg_config_home(home: &Path) -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(".config"))
}

#[cfg(unix)]
fn firefox_profile_roots() -> Vec<PathBuf> {
    let Some(home) = profile_home() else {
        return Vec::new();
    };
    vec![
        home.join(".mozilla/firefox"),
        home.join(".zen"),
        home.join(".librewolf"),
        home.join(".waterfox"),
        home.join(".var/app/org.mozilla.firefox/.mozilla/firefox"),
        home.join(".var/app/io.gitlab.librewolf-community/.librewolf"),
        home.join("snap/firefox/common/.mozilla/firefox"),
    ]
}

#[cfg(windows)]
fn firefox_profile_roots() -> Vec<PathBuf> {
    let Some(appdata) = std::env::var_os("APPDATA").map(PathBuf::from) else {
        return Vec::new();
    };
    vec![
        appdata.join("Mozilla/Firefox/Profiles"),
        appdata.join("Zen/Profiles"),
        appdata.join("LibreWolf/Profiles"),
        appdata.join("Waterfox/Profiles"),
    ]
}

#[cfg(not(any(unix, windows)))]
fn firefox_profile_roots() -> Vec<PathBuf> {
    Vec::new()
}

#[cfg(unix)]
fn chromium_profile_roots() -> Vec<PathBuf> {
    let Some(home) = profile_home() else {
        return Vec::new();
    };
    let config = xdg_config_home(&home);
    let mut roots = [
        "google-chrome",
        "google-chrome-beta",
        "google-chrome-unstable",
        "google-chrome-canary",
        "google-chrome-for-testing",
        "chromium",
        "ungoogled-chromium",
        "BraveSoftware/Brave-Browser",
        "BraveSoftware/Brave-Browser-Beta",
        "BraveSoftware/Brave-Browser-Dev",
        "BraveSoftware/Brave-Browser-Nightly",
        "microsoft-edge",
        "microsoft-edge-beta",
        "microsoft-edge-dev",
        "vivaldi",
        "opera",
    ]
    .into_iter()
    .map(|sub| config.join(sub))
    .collect::<Vec<_>>();
    roots.extend([
        home.join(".var/app/com.google.Chrome/config/google-chrome"),
        home.join(".var/app/org.chromium.Chromium/config/chromium"),
        home.join(".var/app/com.brave.Browser/config/BraveSoftware/Brave-Browser"),
    ]);
    roots
}

#[cfg(windows)]
fn chromium_profile_roots() -> Vec<PathBuf> {
    let Some(local) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) else {
        return Vec::new();
    };
    vec![
        local.join("Google/Chrome/User Data"),
        local.join("Chromium/User Data"),
        local.join("BraveSoftware/Brave-Browser/User Data"),
        local.join("Microsoft/Edge/User Data"),
        local.join("Vivaldi/User Data"),
        local.join("Opera Software/Opera Stable"),
    ]
}

#[cfg(not(any(unix, windows)))]
fn chromium_profile_roots() -> Vec<PathBuf> {
    Vec::new()
}

#[derive(Clone)]
struct Ctx {
    app: AppHandle,
    engine: Arc<Engine>,
    db: Db,
    ytdlp: Arc<crate::ytdlp::YtDlp>,
    download_dir: PathBuf,
    defaults: Arc<Mutex<EngineDefaults>>,
    /// Bulk-capture grouping: `batch_id` → package id. Jobs arrive one per
    /// connection, so the mapping lives across connections; entries expire so
    /// a re-used id from a crashed browser session can't join a stale group.
    batches: Arc<Mutex<HashMap<String, (i64, Instant)>>>,
    connector_presence: Arc<Mutex<ConnectorPresence>>,
}

const BATCH_TTL: Duration = Duration::from_secs(600);

/// Resolve the package for a job's `batch_id`: first job of a batch creates the
/// package (named from `batch_name`, falling back to the URL host), the rest
/// reuse it. Returns `(package_id, created_now)` so a rejected first job can
/// clean up the package it just created.
fn batch_package(ctx: &Ctx, job: &minidl_core::ipc::CaptureJob) -> Option<(i64, bool)> {
    let bid = job.batch_id.as_deref()?;
    let mut map = ctx.batches.lock().unwrap_or_else(|e| e.into_inner());
    map.retain(|_, (_, at)| at.elapsed() < BATCH_TTL);
    if let Some((pkg, _)) = map.get(bid) {
        return Some((*pkg, false));
    }
    let name = job
        .batch_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .unwrap_or_else(|| minidl_core::grabber::host_of(&job.url));
    let name = if name.is_empty() {
        "Batch".to_string()
    } else {
        name
    };
    let pkg = ctx.db.insert_package(&name, None, None).ok()?;
    map.insert(bid.to_string(), (pkg, Instant::now()));
    Some((pkg, true))
}

/// Bind the bridge socket and serve forwarded jobs. Also records this app's
/// executable path so the host can launch it on demand.
pub fn spawn_listener(
    app: AppHandle,
    engine: Arc<Engine>,
    db: Db,
    ytdlp: Arc<crate::ytdlp::YtDlp>,
    download_dir: PathBuf,
    defaults: Arc<Mutex<EngineDefaults>>,
    connector_presence: Arc<Mutex<ConnectorPresence>>,
) {
    record_app_path();

    let ctx = Ctx {
        app,
        engine,
        db,
        ytdlp,
        download_dir,
        defaults,
        batches: Arc::new(Mutex::new(HashMap::new())),
        connector_presence,
    };

    tauri::async_runtime::spawn(async move {
        // On Unix the socket is a filesystem path: pre-create its dir with
        // user-only permissions and clear any stale socket file.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let path = ipc::bridge_socket_path();
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
                let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
            }
            let _ = std::fs::remove_file(&path);
        }

        let name = match ipc::bridge_socket_name() {
            Ok(n) => n,
            Err(e) => {
                eprintln!("bridge: bad socket name: {e}");
                return;
            }
        };
        let listener = match ListenerOptions::new().name(name).create_tokio() {
            Ok(l) => l,
            Err(e) => {
                eprintln!("bridge: failed to bind: {e}");
                return;
            }
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(
                ipc::bridge_socket_path(),
                std::fs::Permissions::from_mode(0o600),
            );
        }

        loop {
            match listener.accept().await {
                Ok(conn) => {
                    let ctx = ctx.clone();
                    tauri::async_runtime::spawn(async move {
                        handle_conn(conn, ctx).await;
                    });
                }
                Err(_) => continue,
            }
        }
    });
}

async fn handle_conn(mut conn: Stream, ctx: Ctx) {
    let mut len = [0u8; 4];
    if conn.read_exact(&mut len).await.is_err() {
        return;
    }
    let n = u32::from_le_bytes(len) as usize;
    if n == 0 || n > MAX_MSG {
        return;
    }
    let mut buf = vec![0u8; n];
    if conn.read_exact(&mut buf).await.is_err() {
        return;
    }

    let reply = match serde_json::from_slice::<BridgeRequest>(&buf) {
        Ok(req) if req.protocol_version == ipc::PROTOCOL_VERSION => {
            // A family annotation is attached to all modern connector messages
            // (presence, capture, and manual ping). Only update session state
            // after the message has reached the running app.
            if let Some(family) = req.browser_family {
                let status = record_connector_presence(&ctx.connector_presence, family);
                let _ = ctx.app.emit(EV_CONNECTOR_STATUS, status);
            }

            if req.presence {
                if req.browser_family.is_some() {
                    // Silent heartbeat: acknowledge without an ingest, a
                    // notification, or bringing the desktop window forward.
                    BridgeReply::acknowledged()
                } else {
                    BridgeReply::rejected("connector presence missing browser family")
                }
            } else if req.ping {
                // Preserve the options page's historical health-check text.
                BridgeReply {
                    ok: true,
                    job_id: None,
                    error: Some(format!(
                        "Mini Downloader (protocol {})",
                        ipc::PROTOCOL_VERSION
                    )),
                }
            } else {
                let defaults = ctx.defaults.lock().unwrap().clone();
                let batch_id = req.job.batch_id.clone();
                let batch = batch_package(&ctx, &req.job);
                match ingest(
                    &ctx.engine,
                    &ctx.db,
                    &ctx.ytdlp,
                    &ctx.download_dir,
                    defaults,
                    req.job,
                    None,
                    batch.map(|(pkg, _)| pkg),
                    None,
                )
                .await
                {
                    Ok(id) => {
                        let _ = ctx
                            .app
                            .emit(EV_STATE, json!({ "id": id, "status": "active" }));
                        focus_window(&ctx.app);
                        BridgeReply::accepted(id)
                    }
                    Err(e) => {
                        // A batch's first job created the package just above — if
                        // that job is rejected, drop the empty package (and the
                        // mapping) so an all-rejected harvest leaves no dead row.
                        if let (Some((pkg, true)), Some(bid)) = (batch, batch_id) {
                            let _ = ctx.db.delete_package_if_empty(pkg);
                            ctx.batches
                                .lock()
                                .unwrap_or_else(|p| p.into_inner())
                                .remove(&bid);
                        }
                        BridgeReply::rejected(e)
                    }
                }
            }
        }
        Ok(_) => BridgeReply::rejected("unsupported protocol version"),
        Err(_) => BridgeReply::rejected("malformed bridge request"),
    };

    let bytes = serde_json::to_vec(&reply).unwrap_or_default();
    let _ = conn.write_all(&(bytes.len() as u32).to_le_bytes()).await;
    let _ = conn.write_all(&bytes).await;
    let _ = conn.flush().await;
}

fn focus_window(app: &AppHandle) {
    crate::window::reveal(app);
}

fn record_app_path() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::create_dir_all(ipc::data_dir());
        let _ = std::fs::write(ipc::app_path_file(), exe.to_string_lossy().as_bytes());
    }
}

/// Locate the `minidl-native-host` binary shipped next to the app executable.
fn host_binary() -> Option<PathBuf> {
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let cand = dir.join(format!(
        "minidl-native-host{}",
        std::env::consts::EXE_SUFFIX
    ));
    if cand.is_file() {
        Some(cand)
    } else {
        None
    }
}

fn firefox_manifest(host_path: &str) -> serde_json::Value {
    json!({
        "name": ipc::NATIVE_HOST_NAME,
        "description": "Mini Downloader native bridge",
        "path": host_path,
        "type": "stdio",
        "allowed_extensions": [ipc::EXTENSION_ID],
    })
}

fn chromium_manifest(host_path: &str) -> serde_json::Value {
    json!({
        "name": ipc::NATIVE_HOST_NAME,
        "description": "Mini Downloader native bridge",
        "path": host_path,
        "type": "stdio",
        "allowed_origins": [
            format!("chrome-extension://{}/", ipc::CHROME_EXTENSION_ID),
            format!("chrome-extension://{}/", ipc::CHROME_STORE_EXTENSION_ID),
        ],
    })
}

#[cfg(unix)]
fn write_manifest(dir: &Path, manifest: &serde_json::Value) -> Option<String> {
    std::fs::create_dir_all(dir).ok()?;
    let file = dir.join(format!("{}.json", ipc::NATIVE_HOST_NAME));
    std::fs::write(&file, serde_json::to_vec_pretty(manifest).ok()?).ok()?;
    Some(file.to_string_lossy().to_string())
}

/// Register the native-messaging host manifest for every detected browser:
/// Firefox family uses `allowed_extensions`, Chromium family uses
/// `allowed_origins`. On Unix these are JSON files in browser config dirs; on
/// Windows the JSON lives in our data dir and HKCU registry keys point at it.
/// Returns the list of manifest paths written.
#[cfg(unix)]
pub fn register_native_host_manifests() -> Result<Vec<String>, String> {
    let host = host_binary().ok_or("minidl-native-host binary not found next to the app")?;
    let host_path = host.to_string_lossy().to_string();
    let home = std::env::var_os("HOME").ok_or("HOME not set")?;
    let home = Path::new(&home);
    let mut installed = Vec::new();

    // ~/.mozilla always (Firefox + most forks read it); fork + Flatpak/Snap dirs
    // if present (a growing share of Linux users run sandboxed browsers).
    let ff = firefox_manifest(&host_path);
    let ff_native = "native-messaging-hosts";
    let mut ff_dirs: Vec<PathBuf> = vec![home.join(".mozilla").join(ff_native)];
    for sub in [".zen", ".librewolf", ".waterfox"] {
        let base = home.join(sub);
        if base.exists() {
            ff_dirs.push(base.join(ff_native));
        }
    }
    // Flatpak / Snap Firefox keep their profile inside the sandbox home.
    for rel in [
        ".var/app/org.mozilla.firefox/.mozilla",
        ".var/app/io.gitlab.librewolf-community/.librewolf",
        "snap/firefox/common/.mozilla",
    ] {
        let base = home.join(rel);
        if base.exists() {
            ff_dirs.push(base.join(ff_native));
        }
    }
    for dir in ff_dirs {
        if let Some(p) = write_manifest(&dir, &ff) {
            installed.push(p);
        }
    }

    // Respect XDG_CONFIG_HOME when set. A browser launched with a custom
    // --user-data-dir cannot be discovered reliably, so it remains the
    // browser's responsibility to expose a supported profile location.
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(".config"));

    let cr = chromium_manifest(&host_path);
    let cr_native = "NativeMessagingHosts";
    for sub in [
        "google-chrome",
        "google-chrome-beta",
        "google-chrome-unstable",
        "google-chrome-canary",
        "google-chrome-for-testing",
        "chromium",
        "ungoogled-chromium",
        "BraveSoftware/Brave-Browser",
        "BraveSoftware/Brave-Browser-Beta",
        "BraveSoftware/Brave-Browser-Dev",
        "BraveSoftware/Brave-Browser-Nightly",
        "microsoft-edge",
        "microsoft-edge-beta",
        "microsoft-edge-dev",
        "vivaldi",
        "opera",
    ] {
        let base = config_home.join(sub);
        if base.exists() {
            if let Some(p) = write_manifest(&base.join(cr_native), &cr) {
                installed.push(p);
            }
        }
    }
    // Flatpak Chromium-family browsers.
    for id in [
        "com.google.Chrome",
        "org.chromium.Chromium",
        "com.brave.Browser",
    ] {
        let base = home.join(format!(".var/app/{id}/config"));
        if base.exists() {
            // The per-browser subdir name matches the ~/.config layout.
            for sub in ["google-chrome", "chromium", "BraveSoftware/Brave-Browser"] {
                let d = base.join(sub);
                if d.exists() {
                    if let Some(p) = write_manifest(&d.join(cr_native), &cr) {
                        installed.push(p);
                    }
                }
            }
        }
    }

    if installed.is_empty() {
        Err("no browser profile directories found".into())
    } else {
        Ok(installed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connector_status_requires_a_session_confirmation() {
        let presence = Arc::new(Mutex::new(ConnectorPresence::default()));
        let initial = connector_status(&presence);
        assert!(!initial.firefox_detected);
        assert!(!initial.chromium_detected);
        assert_eq!(initial.firefox_last_seen, None);

        let confirmed = record_connector_presence(&presence, BrowserFamily::Firefox);
        assert!(confirmed.firefox_detected);
        assert!(!confirmed.chromium_detected);
        assert!(confirmed.firefox_last_seen.is_some());
        assert_eq!(confirmed.chromium_last_seen, None);
    }

    #[test]
    fn profile_detection_only_counts_existing_profile_roots() {
        let missing = std::env::temp_dir().join(format!(
            "minidl-profile-root-missing-{}-{}",
            std::process::id(),
            unix_millis()
        ));
        assert!(!profile_root_detected(&[missing.clone()]));

        std::fs::create_dir_all(&missing).unwrap();
        assert!(profile_root_detected(&[missing.clone()]));
        let _ = std::fs::remove_dir_all(missing);
    }

    #[test]
    fn firefox_profile_detection_requires_an_active_connector() {
        let active = r#"{
          "addons": [{
            "id": "minidownloader@ramazan.dev",
            "active": true,
            "userDisabled": false,
            "appDisabled": false
          }]
        }"#;
        let disabled = r#"{
          "addons": [{
            "id": "minidownloader@ramazan.dev",
            "active": false,
            "userDisabled": true
          }]
        }"#;
        assert_eq!(firefox_connector_state_from_json(active), Some(true));
        assert_eq!(firefox_connector_state_from_json(disabled), Some(false));
    }

    #[test]
    fn chromium_profile_detection_requires_an_enabled_connector() {
        let enabled = r#"{
          "extensions": {"settings": {
            "hhaobmkdgijodfieadeeanjmnneckafj": {"state": 1}
          }}
        }"#;
        let disabled = r#"{
          "extensions": {"settings": {
            "hhaobmkdgijodfieadeeanjmnneckafj": {"state": 0}
          }}
        }"#;
        assert_eq!(chromium_connector_state_from_json(enabled), Some(true));
        assert_eq!(chromium_connector_state_from_json(disabled), Some(false));
    }
}

/// Windows: write both manifest JSONs into our data dir and register them under
/// HKCU (no admin needed). Browsers resolve the host by reading
/// `HKCU\Software\<vendor>\NativeMessagingHosts\<name>` → default value = path
/// to the manifest file.
#[cfg(windows)]
pub fn register_native_host_manifests() -> Result<Vec<String>, String> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let host = host_binary().ok_or("minidl-native-host.exe not found next to the app")?;
    let host_path = host.to_string_lossy().to_string();

    let dir = ipc::data_dir().join("native-messaging");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    let ff_file = dir.join(format!("{}.firefox.json", ipc::NATIVE_HOST_NAME));
    let cr_file = dir.join(format!("{}.chrome.json", ipc::NATIVE_HOST_NAME));
    std::fs::write(
        &ff_file,
        serde_json::to_vec_pretty(&firefox_manifest(&host_path)).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    std::fs::write(
        &cr_file,
        serde_json::to_vec_pretty(&chromium_manifest(&host_path)).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let mut installed = Vec::new();

    // Standard per-user lookup locations plus Brave's stable per-user path.
    // Chromium-derived browsers may inherit Chromium/Chrome lookup, but avoid
    // creating registry entries for every unverified browser fork/channel.
    let entries: [(&str, &PathBuf); 5] = [
        ("Software\\Mozilla\\NativeMessagingHosts", &ff_file),
        ("Software\\Google\\Chrome\\NativeMessagingHosts", &cr_file),
        ("Software\\Chromium\\NativeMessagingHosts", &cr_file),
        ("Software\\Microsoft\\Edge\\NativeMessagingHosts", &cr_file),
        (
            "Software\\BraveSoftware\\Brave-Browser\\NativeMessagingHosts",
            &cr_file,
        ),
    ];
    for (base, file) in entries {
        let key_path = format!("{base}\\{}", ipc::NATIVE_HOST_NAME);
        if let Ok((key, _)) = hkcu.create_subkey(&key_path) {
            if key
                .set_value("", &file.to_string_lossy().to_string())
                .is_ok()
            {
                installed.push(format!("HKCU\\{key_path}"));
            }
        }
    }

    if installed.is_empty() {
        Err("failed to write any registry keys".into())
    } else {
        Ok(installed)
    }
}
