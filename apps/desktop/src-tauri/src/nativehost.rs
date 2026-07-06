//! App-side of the browser bridge: a local-socket listener (Unix domain socket
//! on Linux/macOS, named pipe on Windows) the native host forwards captured
//! jobs to, plus native-messaging manifest installation (files on Unix,
//! registry keys on Windows).

use std::path::PathBuf;
#[cfg(unix)]
use std::path::Path;
use std::sync::{Arc, Mutex};

use interprocess::local_socket::tokio::{prelude::*, Stream};
use interprocess::local_socket::ListenerOptions;
use serde_json::json;
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use minidl_core::aria2::{Engine, EngineDefaults};
use minidl_core::db::Db;
use minidl_core::ipc::{self, BridgeReply, BridgeRequest};

use crate::events::EV_STATE;
use crate::ingest::ingest;

const MAX_MSG: usize = 64 * 1024 * 1024;

#[derive(Clone)]
struct Ctx {
    app: AppHandle,
    engine: Arc<Engine>,
    db: Db,
    ytdlp: Arc<crate::ytdlp::YtDlp>,
    download_dir: PathBuf,
    defaults: Arc<Mutex<EngineDefaults>>,
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
) {
    record_app_path();

    let ctx = Ctx {
        app,
        engine,
        db,
        ytdlp,
        download_dir,
        defaults,
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
            let defaults = ctx.defaults.lock().unwrap().clone();
            match ingest(
                &ctx.engine,
                &ctx.db,
                &ctx.ytdlp,
                &ctx.download_dir,
                defaults,
                req.job,
                None,
            )
            .await
            {
                Ok(id) => {
                    let _ = ctx.app.emit(EV_STATE, json!({ "id": id, "status": "active" }));
                    focus_window(&ctx.app);
                    BridgeReply::accepted(id)
                }
                Err(e) => BridgeReply::rejected(e),
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
    let cand = dir.join(format!("minidl-native-host{}", std::env::consts::EXE_SUFFIX));
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
        "allowed_origins": [format!("chrome-extension://{}/", ipc::CHROME_EXTENSION_ID)],
    })
}

#[cfg(unix)]
fn write_manifest(dir: &Path, manifest: &serde_json::Value) -> Option<String> {
    std::fs::create_dir_all(dir).ok()?;
    let file = dir.join(format!("{}.json", ipc::NATIVE_HOST_NAME));
    std::fs::write(&file, serde_json::to_vec_pretty(manifest).ok()?).ok()?;
    Some(file.to_string_lossy().to_string())
}

/// Install the native-messaging host manifest for every detected browser:
/// Firefox family uses `allowed_extensions`, Chromium family uses
/// `allowed_origins`. On Unix these are JSON files in browser config dirs; on
/// Windows the JSON lives in our data dir and HKCU registry keys point at it.
/// Returns the list of manifest paths written.
#[cfg(unix)]
pub fn install_browser_integration() -> Result<Vec<String>, String> {
    let host = host_binary().ok_or("minidl-native-host binary not found next to the app")?;
    let host_path = host.to_string_lossy().to_string();
    let home = std::env::var_os("HOME").ok_or("HOME not set")?;
    let home = Path::new(&home);
    let mut installed = Vec::new();

    // ~/.mozilla always (Firefox + most forks read it); fork dirs if present.
    let ff = firefox_manifest(&host_path);
    for sub in [".mozilla", ".zen", ".librewolf", ".waterfox"] {
        let base = home.join(sub);
        if sub == ".mozilla" || base.exists() {
            if let Some(p) = write_manifest(&base.join("native-messaging-hosts"), &ff) {
                installed.push(p);
            }
        }
    }

    let cr = chromium_manifest(&host_path);
    for sub in [
        "google-chrome",
        "chromium",
        "BraveSoftware/Brave-Browser",
        "microsoft-edge",
        "vivaldi",
    ] {
        let base = home.join(".config").join(sub);
        if base.exists() {
            if let Some(p) = write_manifest(&base.join("NativeMessagingHosts"), &cr) {
                installed.push(p);
            }
        }
    }

    if installed.is_empty() {
        Err("no browser profile directories found".into())
    } else {
        Ok(installed)
    }
}

/// Windows: write both manifest JSONs into our data dir and register them under
/// HKCU (no admin needed). Browsers resolve the host by reading
/// `HKCU\Software\<vendor>\NativeMessagingHosts\<name>` → default value = path
/// to the manifest file.
#[cfg(windows)]
pub fn install_browser_integration() -> Result<Vec<String>, String> {
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

    // (registry base, manifest file) — Firefox reads Mozilla, Chromium family
    // all read their own vendor key; Chrome's key is also read by Brave/Vivaldi
    // and most forks, but writing each vendor key is harmless and explicit.
    let entries: [(&str, &PathBuf); 4] = [
        ("Software\\Mozilla\\NativeMessagingHosts", &ff_file),
        ("Software\\Google\\Chrome\\NativeMessagingHosts", &cr_file),
        ("Software\\Chromium\\NativeMessagingHosts", &cr_file),
        ("Software\\Microsoft\\Edge\\NativeMessagingHosts", &cr_file),
    ];
    for (base, file) in entries {
        let key_path = format!("{base}\\{}", ipc::NATIVE_HOST_NAME);
        if let Ok((key, _)) = hkcu.create_subkey(&key_path) {
            if key.set_value("", &file.to_string_lossy().to_string()).is_ok() {
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
