//! Tauri command surface. Thin wrappers delegating to the engine + DB.

use base64::Engine;
use serde_json::json;
use tauri::State;

use ldm_core::ipc::{CaptureJob, DownloadKind};
use ldm_core::model::{Category, Download, DownloadStatus, NewDownload};
use ldm_core::ytdlp::MediaInfo;

use crate::ingest::{ingest, job_from_url};
use crate::state::AppState;

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

fn base_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("download")
        .to_string()
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
        &state.ytdlp,
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
        } else {
            // yt-dlp download: stop the process (resume restarts it).
            state.ytdlp.cancel(id);
        }
        state.db.set_status(id, DownloadStatus::Paused).map_err(err)?;
    }
    Ok(())
}

#[tauri::command]
pub async fn resume_download(id: i64, state: State<'_, AppState>) -> Result<(), String> {
    if let Some(d) = state.db.get(id).map_err(err)? {
        if let Some(gid) = &d.gid {
            let _ = state.engine.rpc.unpause(gid).await;
        } else if matches!(d.kind.as_str(), "video" | "hls" | "dash") {
            state.ytdlp.start(id, d.url.clone(), None);
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
        state.ytdlp.cancel(id); // no-op for aria2 downloads
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

async fn add_file_job(
    path: String,
    kind: DownloadKind,
    state: &State<'_, AppState>,
) -> Result<Download, String> {
    let bytes = std::fs::read(&path).map_err(err)?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let name = base_name(&path);
    let job = CaptureJob {
        url: format!("file:{name}"),
        filename: None,
        referrer: None,
        user_agent: None,
        cookie: None,
        extra_headers: vec![],
        kind,
        mime: None,
        size: None,
        page_url: None,
        cookie_store_id: None,
        torrent_b64: Some(b64),
    };
    let id = ingest(
        &state.engine,
        &state.db,
        &state.ytdlp,
        &state.download_dir,
        &state.defaults,
        job,
        None,
    )
    .await?;
    state.db.get(id).map_err(err)?.ok_or_else(|| "download vanished".to_string())
}

/// Probe a page/URL for downloadable video formats (yt-dlp).
#[tauri::command]
pub async fn probe_media(url: String, state: State<'_, AppState>) -> Result<MediaInfo, String> {
    state.ytdlp.probe(&url).await
}

/// Start a yt-dlp download of a chosen format (or best when None).
#[tauri::command]
pub async fn add_media_download(
    url: String,
    format_id: Option<String>,
    state: State<'_, AppState>,
) -> Result<Download, String> {
    let dir = state.download_dir.to_string_lossy().to_string();
    let id = state
        .db
        .insert_download(&NewDownload {
            url: url.clone(),
            dir,
            kind: "video".into(),
            ..Default::default()
        })
        .map_err(err)?;
    state.db.set_status(id, DownloadStatus::Active).map_err(err)?;
    state.ytdlp.start(id, url, format_id);
    state.db.get(id).map_err(err)?.ok_or_else(|| "download vanished".to_string())
}

#[tauri::command]
pub async fn add_torrent_file(path: String, state: State<'_, AppState>) -> Result<Download, String> {
    add_file_job(path, DownloadKind::Torrent, &state).await
}

#[tauri::command]
pub async fn add_metalink_file(path: String, state: State<'_, AppState>) -> Result<Download, String> {
    add_file_job(path, DownloadKind::Metalink, &state).await
}

#[tauri::command]
pub async fn list_categories(state: State<'_, AppState>) -> Result<Vec<Category>, String> {
    state.db.list_categories().map_err(err)
}

#[tauri::command]
pub async fn save_category(
    name: String,
    dir: String,
    rules: String,
    priority: i64,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state.db.upsert_category(&name, &dir, &rules, priority).map_err(err).map(|_| ())
}

#[tauri::command]
pub async fn delete_category(id: i64, state: State<'_, AppState>) -> Result<(), String> {
    state.db.delete_category(id).map_err(err)
}

#[tauri::command]
pub async fn get_setting(key: String, state: State<'_, AppState>) -> Result<Option<String>, String> {
    state.db.get_setting(&key).map_err(err)
}

#[tauri::command]
pub async fn set_setting(key: String, value: String, state: State<'_, AppState>) -> Result<(), String> {
    state.db.set_setting(&key, &value).map_err(err)
}
