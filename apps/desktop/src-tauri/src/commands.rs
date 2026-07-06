//! Tauri command surface. Thin wrappers delegating to the engine + DB.

use serde_json::json;
use tauri::State;

use ldm_core::model::{Download, DownloadStatus};

use crate::ingest::{ingest, job_from_url};
use crate::state::AppState;

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

#[tauri::command]
pub async fn add_download(url: String, state: State<'_, AppState>) -> Result<Download, String> {
    let url = url.trim().to_string();
    if url.is_empty() {
        return Err("empty URL".into());
    }
    let id = ingest(
        &state.engine,
        &state.db,
        &state.download_dir,
        &state.defaults,
        job_from_url(url),
        None,
    )
    .await?;
    state
        .db
        .get(id)
        .map_err(err)?
        .ok_or_else(|| "download vanished after insert".to_string())
}

#[tauri::command]
pub async fn list_downloads(
    status: Option<String>,
    state: State<'_, AppState>,
) -> Result<Vec<Download>, String> {
    state.db.list(status.as_deref()).map_err(err)
}

#[tauri::command]
pub async fn pause_download(id: i64, state: State<'_, AppState>) -> Result<(), String> {
    if let Some(d) = state.db.get(id).map_err(err)? {
        if let Some(gid) = d.gid {
            let _ = state.engine.rpc.pause(&gid).await;
        }
        state.db.set_status(id, DownloadStatus::Paused).map_err(err)?;
    }
    Ok(())
}

#[tauri::command]
pub async fn resume_download(id: i64, state: State<'_, AppState>) -> Result<(), String> {
    if let Some(d) = state.db.get(id).map_err(err)? {
        if let Some(gid) = d.gid {
            let _ = state.engine.rpc.unpause(&gid).await;
        }
        state.db.set_status(id, DownloadStatus::Active).map_err(err)?;
    }
    Ok(())
}

#[tauri::command]
pub async fn remove_download(
    id: i64,
    delete_files: bool,
    state: State<'_, AppState>,
) -> Result<(), String> {
    if let Some(d) = state.db.get(id).map_err(err)? {
        if let Some(gid) = &d.gid {
            let _ = state.engine.rpc.remove(gid).await;
            let _ = state.engine.rpc.remove_download_result(gid).await;
        }
        if delete_files {
            if let Some(name) = &d.filename {
                let base = std::path::Path::new(&d.dir).join(name);
                let _ = std::fs::remove_file(&base);
                let _ = std::fs::remove_file(std::path::Path::new(&d.dir).join(format!("{name}.aria2")));
            }
        }
        state.db.delete(id).map_err(err)?;
    }
    Ok(())
}

#[tauri::command]
pub async fn pause_all(state: State<'_, AppState>) -> Result<(), String> {
    state.engine.rpc.pause_all().await.map_err(err).map(|_| ())
}

#[tauri::command]
pub async fn resume_all(state: State<'_, AppState>) -> Result<(), String> {
    state.engine.rpc.unpause_all().await.map_err(err).map(|_| ())
}

/// Global speed caps in bytes/sec (0 = unlimited).
#[tauri::command]
pub async fn set_global_speed(down: i64, up: i64, state: State<'_, AppState>) -> Result<(), String> {
    let opts = json!({
        "max-overall-download-limit": down.to_string(),
        "max-overall-upload-limit": up.to_string(),
    });
    state.engine.rpc.change_global_option(opts).await.map_err(err).map(|_| ())
}

#[tauri::command]
pub async fn set_download_speed(id: i64, limit: i64, state: State<'_, AppState>) -> Result<(), String> {
    if let Some(d) = state.db.get(id).map_err(err)? {
        if let Some(gid) = d.gid {
            let opts = json!({ "max-download-limit": limit.to_string() });
            state.engine.rpc.change_option(&gid, opts).await.map_err(err)?;
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn open_containing_folder(
    id: i64,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    if let Some(d) = state.db.get(id).map_err(err)? {
        app.opener().open_path(d.dir, None::<&str>).map_err(err)?;
    }
    Ok(())
}

/// Install the Firefox native-messaging manifest so the extension can reach the
/// app. Returns the manifest path.
#[tauri::command]
pub async fn install_browser_integration() -> Result<String, String> {
    crate::nativehost::install_firefox_manifest().map(|p| p.to_string_lossy().to_string())
}
