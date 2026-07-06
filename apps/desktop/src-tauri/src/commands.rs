//! Tauri command surface. Thin wrappers delegating to the engine + DB.

use std::sync::atomic::Ordering;

use base64::Engine;
use serde_json::json;
use tauri::{AppHandle, Emitter, State};

use minidl_core::grabber::ParsedLink;
use minidl_core::ipc::{CaptureJob, DownloadKind};
use minidl_core::model::{Category, Download, DownloadStatus, NewDownload, Schedule};
use minidl_core::ytdlp::MediaInfo;

use crate::events::EV_STATE;
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
    let defaults = state.defaults.lock().unwrap().clone();
    let id = ingest(
        &state.engine,
        &state.db,
        &state.ytdlp,
        &state.download_dir,
        defaults,
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
        if d.is_ytdlp() {
            // yt-dlp download: stop the process (resume restarts it).
            state.ytdlp.cancel(id);
        } else if let Some(gid) = &d.gid {
            let _ = state.engine.rpc.pause(gid).await;
        }
        state.db.set_status(id, DownloadStatus::Paused).map_err(err)?;
    }
    Ok(())
}

#[tauri::command]
pub async fn resume_download(id: i64, state: State<'_, AppState>) -> Result<(), String> {
    if let Some(d) = state.db.get(id).map_err(err)? {
        if d.is_ytdlp() {
            // Replay auth + the originally chosen format so quality is stable.
            let job = crate::ingest::job_from_row(&d);
            let url = d.page_url.clone().unwrap_or_else(|| d.url.clone());
            state.ytdlp.start(id, url, d.format_id.clone(), job.header_lines());
        } else if let Some(gid) = &d.gid {
            // Fast path: unpause. If aria2 has forgotten the GID (reconcile
            // orphan after a crash), re-issue the request with the original auth
            // so it resumes instead of 403-ing forever.
            if state.engine.rpc.unpause(gid).await.is_err() {
                let defaults = state.defaults.lock().unwrap().clone();
                crate::ingest::reissue(&state.engine, &state.db, defaults, &d).await?;
            }
        }
        state.db.set_status(id, DownloadStatus::Active).map_err(err)?;
    }
    Ok(())
}

/// Retry a failed/stalled download in place (same row), re-issuing with the
/// original auth + kind instead of degrading to a bare URL.
#[tauri::command]
pub async fn retry_download(id: i64, state: State<'_, AppState>) -> Result<Download, String> {
    let row = state.db.get(id).map_err(err)?.ok_or("download not found")?;
    if let Some(gid) = &row.gid {
        let _ = state.engine.rpc.remove(gid).await;
        let _ = state.engine.rpc.remove_download_result(gid).await;
    }
    if row.is_ytdlp() {
        let job = crate::ingest::job_from_row(&row);
        let url = row.page_url.clone().unwrap_or_else(|| row.url.clone());
        state.db.set_status(id, DownloadStatus::Active).map_err(err)?;
        state.ytdlp.start(id, url, row.format_id.clone(), job.header_lines());
    } else {
        let defaults = state.defaults.lock().unwrap().clone();
        crate::ingest::reissue(&state.engine, &state.db, defaults, &row).await?;
        state.db.set_status(id, DownloadStatus::Active).map_err(err)?;
    }
    state.db.get(id).map_err(err)?.ok_or_else(|| "download vanished".into())
}

#[tauri::command]
pub async fn remove_download(
    id: i64,
    delete_files: bool,
    state: State<'_, AppState>,
) -> Result<(), String> {
    if let Some(d) = state.db.get(id).map_err(err)? {
        state.ytdlp.cancel(id); // no-op for aria2 downloads
        // Capture the file list *before* removing the download result (aria2
        // forgets it afterwards) so a multi-file torrent's files can be deleted.
        let files: Vec<String> = if delete_files {
            match &d.gid {
                Some(gid) => state
                    .engine
                    .rpc
                    .get_files(gid)
                    .await
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|f| f.get("path").and_then(|p| p.as_str()).map(String::from))
                    .filter(|p| !p.is_empty())
                    .collect(),
                None => Vec::new(),
            }
        } else {
            Vec::new()
        };
        if let Some(gid) = &d.gid {
            let _ = state.engine.rpc.remove(gid).await;
            let _ = state.engine.rpc.remove_download_result(gid).await;
        }
        if delete_files {
            let dir = std::path::Path::new(&d.dir);
            // Prefer aria2's own file list (covers every file of a torrent); fall
            // back to the tracked filename. Only ever delete inside the download
            // dir. Also drop the `.aria2` control sidecar.
            let mut targets: Vec<std::path::PathBuf> =
                files.iter().map(std::path::PathBuf::from).collect();
            if let Some(name) = &d.filename {
                targets.push(dir.join(name));
            }
            for t in targets {
                if t.starts_with(dir) {
                    let _ = std::fs::remove_file(&t);
                    let _ = std::fs::remove_file(std::path::PathBuf::from(format!("{}.aria2", t.display())));
                }
            }
        }
        state.db.delete(id).map_err(err)?;
    }
    Ok(())
}

/// Remove every completed download from the list (keeps the files on disk).
#[tauri::command]
pub async fn remove_completed(state: State<'_, AppState>) -> Result<usize, String> {
    let rows = state.db.list(Some("complete")).map_err(err)?;
    let mut removed = 0;
    for d in rows {
        if let Some(gid) = &d.gid {
            let _ = state.engine.rpc.remove_download_result(gid).await;
        }
        if state.db.delete(d.id).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
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
    // Persist so the cap is re-applied on resume/reissue, then apply live.
    state.db.set_speed_limit(id, if limit > 0 { Some(limit) } else { None }).map_err(err)?;
    if let Some(d) = state.db.get(id).map_err(err)? {
        if let Some(gid) = d.gid {
            let opts = json!({ "max-download-limit": limit.to_string() });
            state.engine.rpc.change_option(&gid, opts).await.map_err(err)?;
        }
    }
    Ok(())
}

/// Reorder a waiting download in the aria2 queue: "top" | "up" | "down" | "bottom".
#[tauri::command]
pub async fn move_in_queue(id: i64, direction: String, state: State<'_, AppState>) -> Result<(), String> {
    if let Some(d) = state.db.get(id).map_err(err)? {
        if let Some(gid) = d.gid {
            let (pos, how) = match direction.as_str() {
                "top" => (0, "POS_SET"),
                "up" => (-1, "POS_CUR"),
                "down" => (1, "POS_CUR"),
                "bottom" => (1_000_000, "POS_SET"),
                _ => return Ok(()),
            };
            let _ = state.engine.rpc.change_position(&gid, pos, how).await;
        }
    }
    Ok(())
}

/// Max simultaneous downloads (persisted; applied live + read on next launch).
#[tauri::command]
pub async fn set_max_concurrent(n: u32, state: State<'_, AppState>) -> Result<(), String> {
    let n = n.clamp(1, 20);
    state.db.set_setting("max_concurrent", &n.to_string()).map_err(err)?;
    let _ = state
        .engine
        .rpc
        .change_global_option(json!({ "max-concurrent-downloads": n.to_string() }))
        .await;
    Ok(())
}

#[tauri::command]
pub async fn get_max_concurrent(state: State<'_, AppState>) -> Result<u32, String> {
    Ok(state
        .db
        .get_setting("max_concurrent")
        .map_err(err)?
        .and_then(|s| s.parse().ok())
        .unwrap_or(5))
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

/// Install the native-messaging host manifest for every detected browser
/// (Firefox family + Chromium family). Returns a summary.
#[tauri::command]
pub async fn install_browser_integration() -> Result<String, String> {
    crate::nativehost::install_browser_integration()
        .map(|paths| format!("Installed {} manifest(s):\n{}", paths.len(), paths.join("\n")))
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
    let defaults = state.defaults.lock().unwrap().clone();
    let id = ingest(
        &state.engine,
        &state.db,
        &state.ytdlp,
        &state.download_dir,
        defaults,
        job,
        None,
    )
    .await?;
    state.db.get(id).map_err(err)?.ok_or_else(|| "download vanished".to_string())
}

/// Probe a page/URL for downloadable video formats (yt-dlp).
#[tauri::command]
pub async fn probe_media(url: String, state: State<'_, AppState>) -> Result<MediaInfo, String> {
    state.ytdlp.probe(&url, &[]).await
}

/// Start a yt-dlp download of a chosen format (or best when None). Persists the
/// format so resume keeps the same quality.
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
            format_id: format_id.clone(),
            ..Default::default()
        })
        .map_err(err)?;
    state.db.set_status(id, DownloadStatus::Active).map_err(err)?;
    state.ytdlp.start(id, url, format_id, vec![]);
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

// ---- link grabber ----

#[tauri::command]
pub async fn grab_links(text: String) -> Result<Vec<ParsedLink>, String> {
    Ok(minidl_core::grabber::parse_links(&text))
}

#[tauri::command]
pub async fn add_links_batch(
    urls: Vec<String>,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<usize, String> {
    let mut added = 0;
    let defaults = state.defaults.lock().unwrap().clone();
    for u in urls {
        let u = u.trim().to_string();
        if u.is_empty() {
            continue;
        }
        if ingest(
            &state.engine,
            &state.db,
            &state.ytdlp,
            &state.download_dir,
            defaults.clone(),
            job_from_url(u),
            None,
        )
        .await
        .is_ok()
        {
            added += 1;
        }
    }
    let _ = app.emit(EV_STATE, json!({ "batch": added }));
    Ok(added)
}

// ---- scheduler ----

#[tauri::command]
pub async fn list_schedules(state: State<'_, AppState>) -> Result<Vec<Schedule>, String> {
    state.db.list_schedules().map_err(err)
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn save_schedule(
    id: Option<i64>,
    name: Option<String>,
    action: String,
    days_mask: i64,
    at_minute: i64,
    speed_limit: Option<i64>,
    enabled: bool,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state
        .db
        .save_schedule(id, name.as_deref(), &action, days_mask, at_minute, speed_limit, enabled)
        .map_err(err)
        .map(|_| ())
}

#[tauri::command]
pub async fn delete_schedule(id: i64, state: State<'_, AppState>) -> Result<(), String> {
    state.db.delete_schedule(id).map_err(err)
}

// ---- clipboard ----

#[tauri::command]
pub async fn set_clipboard_watch(enabled: bool, state: State<'_, AppState>) -> Result<(), String> {
    state.clipboard_on.store(enabled, Ordering::Relaxed);
    state
        .db
        .set_setting("clipboard_watch", if enabled { "true" } else { "false" })
        .map_err(err)
}

// ---- engine tuning (segments / connections) ----

/// Returns `[split, connections]`.
#[tauri::command]
pub async fn get_engine_defaults(state: State<'_, AppState>) -> Result<(u32, u32), String> {
    let d = state.defaults.lock().unwrap();
    Ok((d.split, d.max_connection_per_server))
}

#[tauri::command]
pub async fn set_engine_defaults(
    split: u32,
    connections: u32,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let split = split.clamp(1, 64);
    let connections = connections.clamp(1, 16);
    {
        let mut d = state.defaults.lock().unwrap();
        d.split = split;
        d.max_connection_per_server = connections;
    }
    state.db.set_setting("split", &split.to_string()).map_err(err)?;
    state.db.set_setting("max_conn", &connections.to_string()).map_err(err)?;
    Ok(())
}
