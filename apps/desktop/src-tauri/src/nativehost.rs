//! App-side of the browser bridge: a Unix-domain-socket listener the native host
//! forwards captured jobs to, plus native-messaging manifest installation.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::json;
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

use ldm_core::aria2::{Engine, EngineDefaults};
use ldm_core::db::Db;
use ldm_core::ipc::{self, BridgeReply, BridgeRequest};

use crate::events::EV_STATE;
use crate::ingest::ingest;

const MAX_MSG: usize = 64 * 1024 * 1024;

#[derive(Clone)]
struct Ctx {
    app: AppHandle,
    engine: Arc<Engine>,
    db: Db,
    download_dir: PathBuf,
    defaults: EngineDefaults,
}

/// Bind the bridge socket and serve forwarded jobs. Also records this app's
/// executable path so the host can launch it on demand.
pub fn spawn_listener(
    app: AppHandle,
    engine: Arc<Engine>,
    db: Db,
    download_dir: PathBuf,
    defaults: EngineDefaults,
) {
    record_app_path();

    let ctx = Ctx {
        app,
        engine,
        db,
        download_dir,
        defaults,
    };

    tauri::async_runtime::spawn(async move {
        let path = ipc::bridge_socket_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
            let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
        }
        let _ = std::fs::remove_file(&path);

        let listener = match UnixListener::bind(&path) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("bridge: failed to bind {}: {e}", path.display());
                return;
            }
        };
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));

        loop {
            match listener.accept().await {
                Ok((conn, _)) => {
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

async fn handle_conn(mut conn: UnixStream, ctx: Ctx) {
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
            match ingest(
                &ctx.engine,
                &ctx.db,
                &ctx.download_dir,
                &ctx.defaults,
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
    use tauri::Manager;
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

fn record_app_path() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::create_dir_all(ipc::data_dir());
        let _ = std::fs::write(ipc::app_path_file(), exe.to_string_lossy().as_bytes());
    }
}

/// Locate the `ldm-native-host` binary shipped next to the app executable.
fn host_binary() -> Option<PathBuf> {
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let cand = dir.join("ldm-native-host");
    if cand.is_file() {
        Some(cand)
    } else {
        None
    }
}

/// Write the Firefox native-messaging manifest pointing at the host binary.
/// Per-user location; idempotent.
pub fn install_firefox_manifest() -> Result<PathBuf, String> {
    let host = host_binary().ok_or("ldm-native-host binary not found next to the app")?;
    let home = std::env::var_os("HOME").ok_or("HOME not set")?;
    let dir = Path::new(&home).join(".mozilla/native-messaging-hosts");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    let manifest = json!({
        "name": ipc::NATIVE_HOST_NAME,
        "description": "Linux Download Manager native bridge",
        "path": host.to_string_lossy(),
        "type": "stdio",
        "allowed_extensions": [ipc::EXTENSION_ID],
    });

    let file = dir.join(format!("{}.json", ipc::NATIVE_HOST_NAME));
    std::fs::write(&file, serde_json::to_vec_pretty(&manifest).unwrap())
        .map_err(|e| e.to_string())?;
    Ok(file)
}
