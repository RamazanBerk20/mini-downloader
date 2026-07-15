//! Tauri command surface. Thin wrappers delegating to the engine + DB.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use base64::Engine;
use serde_json::json;
use tauri::{AppHandle, Emitter, State};

use minidl_core::db::DownloadInsertResult;
use minidl_core::grabber::ParsedLink;
use minidl_core::ipc::{CaptureJob, DownloadKind};
use minidl_core::model::{Category, Download, DownloadStatus, NewDownload, Package, Schedule};
use minidl_core::ytdlp::MediaInfo;

use crate::errors::CommandError;
use crate::events::EV_STATE;
use crate::ingest::{ingest, job_from_url, IngestOutcome};
use crate::state::AppState;

fn err<E: std::fmt::Display>(e: E) -> CommandError {
    CommandError::from(e.to_string())
}

fn base_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("download")
        .to_string()
}

/// `C` and `POSIX` are technical fallback locales, not a user-facing language.
/// Ignore them so a real preference lower in the OS locale list (for example
/// `LANG=tr_TR.UTF-8`) can still be used by the UI.
fn is_generic_locale(locale: &str) -> bool {
    matches!(
        locale.trim().to_ascii_lowercase().as_str(),
        "" | "c" | "posix" | "und"
    )
}

/// Resolve a deletion candidate beneath `dir` without following a raw filename
/// (or aria2-reported path) out of that directory.  A lexical `starts_with`
/// check is not enough: `Downloads/../outside` still starts with `Downloads`
/// before the OS resolves it.  Canonicalizing the parent also rejects a
/// symlinked subdirectory that points elsewhere.  The final path component is
/// intentionally not canonicalized, so deleting a symlink removes the link,
/// never its target.
fn deletion_target_in_dir(dir: &Path, candidate: &Path) -> Option<PathBuf> {
    let root = dir.canonicalize().ok()?;
    let candidate = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        dir.join(candidate)
    };
    let name = candidate.file_name()?;
    if name == "." || name == ".." {
        return None;
    }
    let parent = candidate.parent()?.canonicalize().ok()?;
    if !parent.starts_with(&root) {
        return None;
    }
    Some(parent.join(name))
}

fn aria2_sidecar(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".aria2");
    PathBuf::from(name)
}

/// Normalize an optional user-supplied SHA-256 hex digest into aria2's
/// `checksum` option value, rejecting malformed input early.
fn normalize_checksum(checksum: Option<String>) -> Result<Option<String>, CommandError> {
    match checksum
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
    {
        Some(s) if s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()) => {
            Ok(Some(format!("sha-256={s}")))
        }
        Some(_) => Err("invalid SHA-256 checksum (expected 64 hex characters)".into()),
        None => Ok(None),
    }
}

#[tauri::command]
pub async fn add_download(
    url: String,
    checksum: Option<String>,
    state: State<'_, AppState>,
) -> Result<Download, CommandError> {
    let url = url.trim().to_string();
    if url.is_empty() {
        return Err("empty URL".into());
    }
    let checksum = normalize_checksum(checksum)?;
    let defaults = state.defaults.lock().unwrap().clone();
    let id = ingest(
        &state.engine,
        &state.db,
        &state.ytdlp,
        &state.download_dir,
        defaults,
        job_from_url(url),
        None,
        None,
        checksum,
    )
    .await?
    .id();
    state
        .db
        .get(id)
        .map_err(err)?
        .ok_or_else(|| err("download vanished after insert"))
}

#[tauri::command]
pub async fn list_downloads(
    status: Option<String>,
    state: State<'_, AppState>,
) -> Result<Vec<Download>, CommandError> {
    state.db.list(status.as_deref()).map_err(err)
}

#[tauri::command]
pub async fn pause_download(id: i64, state: State<'_, AppState>) -> Result<(), CommandError> {
    if let Some(d) = state.db.get(id).map_err(err)? {
        if d.is_ytdlp() {
            // yt-dlp download: stop the process (resume restarts it).
            state.ytdlp.cancel(id);
        } else if let Some(gid) = &d.gid {
            let _ = state.engine.rpc.pause(gid).await;
        }
        state
            .db
            .set_status(id, DownloadStatus::Paused)
            .map_err(err)?;
    }
    Ok(())
}

/// Shared resume logic (command + per-download scheduler): restart yt-dlp with
/// replayed auth/format/options, or unpause the GID with a reissue fallback.
pub(crate) async fn resume_row(state: &AppState, id: i64) -> Result<(), CommandError> {
    if let Some(d) = state.db.get(id).map_err(err)? {
        if d.is_ytdlp() {
            // Flip to Active BEFORE start(): the driver task re-checks the DB
            // status after acquiring its semaphore slot and would self-cancel
            // on a still-Scheduled/Paused row.
            state
                .db
                .set_status(id, DownloadStatus::Active)
                .map_err(err)?;
            // Replay auth + the originally chosen format + media options so
            // quality and post-processing are stable across kill+restart.
            let job = crate::ingest::job_from_row(&d);
            let url = d.page_url.clone().unwrap_or_else(|| d.url.clone());
            let opts = crate::ytdlp::MediaOpts::from_row(d.media_opts.as_deref());
            state
                .ytdlp
                .start(id, url, d.format_id.clone(), job.header_lines(), opts);
        } else if let Some(gid) = &d.gid {
            // Fast path: unpause. If aria2 has forgotten the GID (reconcile
            // orphan after a crash), re-issue the request with the original auth
            // so it resumes instead of 403-ing forever.
            if state.engine.rpc.unpause(gid).await.is_err() {
                let defaults = state.defaults.lock().unwrap().clone();
                crate::ingest::reissue(&state.engine, &state.db, defaults, &d).await?;
            }
        } else {
            // Never handed to aria2 (e.g. scheduled straight from queued) —
            // issue it now.
            let defaults = state.defaults.lock().unwrap().clone();
            crate::ingest::reissue(&state.engine, &state.db, defaults, &d).await?;
        }
        state
            .db
            .set_status(id, DownloadStatus::Active)
            .map_err(err)?;
        // A directly-resumed scheduled row must not fire again later.
        if d.start_at.is_some() {
            let _ = state.db.set_start_at(id, None);
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn resume_download(id: i64, state: State<'_, AppState>) -> Result<(), CommandError> {
    resume_row(&state, id).await
}

/// Defer (or un-defer with `None`) a download to start at `start_at` (unix
/// seconds). A live transfer is stopped first; the 20 s scheduler tick starts
/// due rows.
#[tauri::command]
pub async fn schedule_download(
    id: i64,
    start_at: Option<i64>,
    state: State<'_, AppState>,
) -> Result<(), CommandError> {
    let d = state.db.get(id).map_err(err)?.ok_or("download not found")?;
    match start_at {
        Some(ts) => {
            if matches!(d.status, DownloadStatus::Active | DownloadStatus::Waiting) {
                if d.is_ytdlp() {
                    state.ytdlp.cancel(id);
                } else if let Some(gid) = &d.gid {
                    let _ = state.engine.rpc.pause(gid).await;
                }
            }
            state.db.set_start_at(id, Some(ts)).map_err(err)?;
            state
                .db
                .set_status(id, DownloadStatus::Scheduled)
                .map_err(err)?;
        }
        None => {
            state.db.set_start_at(id, None).map_err(err)?;
            if d.status == DownloadStatus::Scheduled {
                state
                    .db
                    .set_status(id, DownloadStatus::Paused)
                    .map_err(err)?;
            }
        }
    }
    Ok(())
}

/// Persist + live-apply the HTTP/SOCKS proxy (`all-proxy`; empty clears). New
/// aria2 downloads pick it up immediately; yt-dlp reads the setting per run.
#[tauri::command]
pub async fn apply_proxy(value: String, state: State<'_, AppState>) -> Result<(), CommandError> {
    let v = value.trim().to_string();
    state.db.set_setting("proxy", &v).map_err(err)?;
    state
        .engine
        .rpc
        .change_global_option(json!({ "all-proxy": v }))
        .await
        .map_err(err)?;
    Ok(())
}

/// Retry a failed/stalled download in place (same row), re-issuing with the
/// original auth + kind instead of degrading to a bare URL.
#[tauri::command]
pub async fn retry_download(id: i64, state: State<'_, AppState>) -> Result<Download, CommandError> {
    let row = state.db.get(id).map_err(err)?.ok_or("download not found")?;
    if let Some(gid) = &row.gid {
        let _ = state.engine.rpc.remove(gid).await;
        let _ = state.engine.rpc.remove_download_result(gid).await;
    }
    if row.is_ytdlp() {
        let job = crate::ingest::job_from_row(&row);
        let url = row.page_url.clone().unwrap_or_else(|| row.url.clone());
        state
            .db
            .set_status(id, DownloadStatus::Active)
            .map_err(err)?;
        let opts = crate::ytdlp::MediaOpts::from_row(row.media_opts.as_deref());
        state
            .ytdlp
            .start(id, url, row.format_id.clone(), job.header_lines(), opts);
    } else {
        let defaults = state.defaults.lock().unwrap().clone();
        crate::ingest::reissue(&state.engine, &state.db, defaults, &row).await?;
        state
            .db
            .set_status(id, DownloadStatus::Active)
            .map_err(err)?;
    }
    state
        .db
        .get(id)
        .map_err(err)?
        .ok_or_else(|| "download vanished".into())
}

#[tauri::command]
pub async fn remove_download(
    id: i64,
    delete_files: bool,
    state: State<'_, AppState>,
) -> Result<(), CommandError> {
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
                if let Some(t) = deletion_target_in_dir(dir, &t) {
                    let _ = std::fs::remove_file(&t);
                    let _ = std::fs::remove_file(aria2_sidecar(&t));
                }
            }
        }
        state.db.delete(id).map_err(err)?;
        if let Some(pkg) = d.package_id {
            let _ = state.db.delete_package_if_empty(pkg);
        }
    }
    Ok(())
}

/// Remove every completed download from the list (keeps the files on disk).
#[tauri::command]
pub async fn remove_completed(state: State<'_, AppState>) -> Result<usize, CommandError> {
    remove_by_status(&state, "complete").await
}

/// Remove every failed download from the list.
#[tauri::command]
pub async fn remove_failed(state: State<'_, AppState>) -> Result<usize, CommandError> {
    remove_by_status(&state, "error").await
}

async fn remove_by_status(
    state: &State<'_, AppState>,
    status: &str,
) -> Result<usize, CommandError> {
    let rows = state.db.list(Some(status)).map_err(err)?;
    let mut removed = 0;
    for d in rows {
        if let Some(gid) = &d.gid {
            let _ = state.engine.rpc.remove_download_result(gid).await;
        }
        if state.db.delete(d.id).is_ok() {
            removed += 1;
            if let Some(pkg) = d.package_id {
                let _ = state.db.delete_package_if_empty(pkg);
            }
        }
    }
    Ok(removed)
}

#[tauri::command]
pub async fn pause_all(state: State<'_, AppState>) -> Result<(), CommandError> {
    state.engine.rpc.pause_all().await.map_err(err).map(|_| ())
}

#[tauri::command]
pub async fn resume_all(state: State<'_, AppState>) -> Result<(), CommandError> {
    state
        .engine
        .rpc
        .unpause_all()
        .await
        .map_err(err)
        .map(|_| ())
}

/// Global speed caps in bytes/sec (0 = unlimited).
#[tauri::command]
pub async fn set_global_speed(
    down: i64,
    up: i64,
    state: State<'_, AppState>,
) -> Result<(), CommandError> {
    let opts = json!({
        "max-overall-download-limit": down.to_string(),
        "max-overall-upload-limit": up.to_string(),
    });
    state
        .engine
        .rpc
        .change_global_option(opts)
        .await
        .map_err(err)
        .map(|_| ())
}

#[tauri::command]
pub async fn set_download_speed(
    id: i64,
    limit: i64,
    state: State<'_, AppState>,
) -> Result<(), CommandError> {
    // Persist so the cap is re-applied on resume/reissue, then apply live.
    state
        .db
        .set_speed_limit(id, if limit > 0 { Some(limit) } else { None })
        .map_err(err)?;
    if let Some(d) = state.db.get(id).map_err(err)? {
        if let Some(gid) = d.gid {
            let opts = json!({ "max-download-limit": limit.to_string() });
            state
                .engine
                .rpc
                .change_option(&gid, opts)
                .await
                .map_err(err)?;
        }
    }
    Ok(())
}

/// Reorder a waiting download in the aria2 queue: "top" | "up" | "down" | "bottom".
#[tauri::command]
pub async fn move_in_queue(
    id: i64,
    direction: String,
    state: State<'_, AppState>,
) -> Result<(), CommandError> {
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

/// Move a download to an absolute queue position (drag-reorder). aria2 clamps
/// and only reorders items still in its waiting queue.
#[tauri::command]
pub async fn set_queue_position(
    id: i64,
    pos: i64,
    state: State<'_, AppState>,
) -> Result<(), CommandError> {
    if let Some(d) = state.db.get(id).map_err(err)? {
        if let Some(gid) = d.gid {
            let _ = state
                .engine
                .rpc
                .change_position(&gid, pos.max(0), "POS_SET")
                .await;
        }
    }
    Ok(())
}

/// Max simultaneous downloads (persisted; applied live + read on next launch).
#[tauri::command]
pub async fn set_max_concurrent(n: u32, state: State<'_, AppState>) -> Result<(), CommandError> {
    let n = n.clamp(1, 20);
    state
        .db
        .set_setting("max_concurrent", &n.to_string())
        .map_err(err)?;
    let _ = state
        .engine
        .rpc
        .change_global_option(json!({ "max-concurrent-downloads": n.to_string() }))
        .await;
    Ok(())
}

#[tauri::command]
pub async fn get_max_concurrent(state: State<'_, AppState>) -> Result<u32, CommandError> {
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
) -> Result<(), CommandError> {
    use tauri_plugin_opener::OpenerExt;
    if let Some(d) = state.db.get(id).map_err(err)? {
        app.opener().open_path(d.dir, None::<&str>).map_err(err)?;
    }
    Ok(())
}

/// Return connector status from live bridge messages plus a read-only active
/// extension-profile fallback for store builds that predate the heartbeat.
#[tauri::command]
pub async fn get_connector_status(
    state: State<'_, AppState>,
) -> Result<crate::nativehost::ConnectorStatus, CommandError> {
    Ok(crate::nativehost::connector_status(
        &state.connector_presence,
    ))
}

async fn add_file_job(
    path: String,
    kind: DownloadKind,
    state: &State<'_, AppState>,
) -> Result<Download, CommandError> {
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
        batch_id: None,
        batch_name: None,
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
        None,
        None,
    )
    .await?
    .id();
    state
        .db
        .get(id)
        .map_err(err)?
        .ok_or_else(|| err("download vanished"))
}

/// Probe a page/URL for downloadable video formats (yt-dlp). With `playlist`
/// the URL is probed as a flat playlist (entry list instead of formats).
#[tauri::command]
pub async fn probe_media(
    url: String,
    playlist: bool,
    state: State<'_, AppState>,
) -> Result<MediaInfo, CommandError> {
    state.ytdlp.probe(&url, &[], playlist).await.map_err(err)
}

/// Start a yt-dlp download of a chosen format (or best when None). Persists the
/// format + media options so resume keeps the same quality and post-processing.
#[tauri::command]
pub async fn add_media_download(
    url: String,
    format_id: Option<String>,
    opts: Option<crate::ytdlp::MediaOpts>,
    state: State<'_, AppState>,
) -> Result<Download, CommandError> {
    let dir = state.download_dir.to_string_lossy().to_string();
    let inserted = state
        .db
        .insert_download_with_duplicate_policy(&NewDownload {
            url: url.clone(),
            dir,
            kind: "video".into(),
            format_id: format_id.clone(),
            media_opts: opts.as_ref().and_then(|o| o.to_json()),
            ..Default::default()
        })
        .map_err(err)?;
    let id = inserted.id();
    if matches!(inserted, DownloadInsertResult::Existing(_)) {
        return state
            .db
            .get(id)
            .map_err(err)?
            .ok_or_else(|| err("download vanished"));
    }
    state
        .db
        .set_status(id, DownloadStatus::Active)
        .map_err(err)?;
    state.ytdlp.start(id, url, format_id, vec![], opts);
    state
        .db
        .get(id)
        .map_err(err)?
        .ok_or_else(|| err("download vanished"))
}

#[derive(serde::Deserialize)]
pub struct PlaylistEntryIn {
    pub url: String,
    pub title: Option<String>,
}

/// Add the selected playlist entries as one package: one yt-dlp row per entry,
/// all with the same quality preset + media options. The yt-dlp driver's
/// internal semaphore keeps the process count bounded.
#[tauri::command]
pub async fn add_playlist_batch(
    entries: Vec<PlaylistEntryIn>,
    package_name: String,
    quality: Option<String>,
    opts: Option<crate::ytdlp::MediaOpts>,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<usize, CommandError> {
    if entries.is_empty() {
        return Ok(0);
    }
    let package_id = if entries.len() >= 2 {
        let name = {
            let n = package_name.trim();
            if n.is_empty() {
                "Playlist"
            } else {
                n
            }
        };
        state.db.insert_package(name, None, None).ok()
    } else {
        None
    };
    let dir = state.download_dir.to_string_lossy().to_string();
    let opts_json = opts.as_ref().and_then(|o| o.to_json());
    let mut added = 0;
    for e in entries {
        let inserted = state
            .db
            .insert_download_with_duplicate_policy(&NewDownload {
                url: e.url.clone(),
                // Display name until yt-dlp reports the real file on completion.
                filename: e.title.clone(),
                dir: dir.clone(),
                kind: "video".into(),
                format_id: quality.clone(),
                package_id,
                media_opts: opts_json.clone(),
                ..Default::default()
            })
            .map_err(err)?;
        let id = inserted.id();
        if matches!(inserted, DownloadInsertResult::Existing(_)) {
            continue;
        }
        state
            .db
            .set_status(id, DownloadStatus::Active)
            .map_err(err)?;
        state
            .ytdlp
            .start(id, e.url, quality.clone(), vec![], opts.clone());
        added += 1;
    }
    if let Some(pkg) = package_id {
        let _ = state.db.delete_package_if_empty(pkg);
    }
    let _ = app.emit(EV_STATE, json!({ "batch": added }));
    Ok(added)
}

#[tauri::command]
pub async fn add_torrent_file(
    path: String,
    state: State<'_, AppState>,
) -> Result<Download, CommandError> {
    add_file_job(path, DownloadKind::Torrent, &state).await
}

#[tauri::command]
pub async fn add_metalink_file(
    path: String,
    state: State<'_, AppState>,
) -> Result<Download, CommandError> {
    add_file_job(path, DownloadKind::Metalink, &state).await
}

#[tauri::command]
pub async fn list_categories(state: State<'_, AppState>) -> Result<Vec<Category>, CommandError> {
    state.db.list_categories().map_err(err)
}

#[tauri::command]
pub async fn save_category(
    name: String,
    dir: String,
    rules: String,
    priority: i64,
    state: State<'_, AppState>,
) -> Result<(), CommandError> {
    state
        .db
        .upsert_category(&name, &dir, &rules, priority)
        .map_err(err)
        .map(|_| ())
}

#[tauri::command]
pub async fn delete_category(id: i64, state: State<'_, AppState>) -> Result<(), CommandError> {
    state.db.delete_category(id).map_err(err)
}

/// Re-add the built-in default categories (restore after edits/deletes).
#[tauri::command]
pub async fn restore_default_categories(
    state: State<'_, AppState>,
) -> Result<Vec<Category>, CommandError> {
    state.db.seed_default_categories().map_err(err)?;
    state.db.list_categories().map_err(err)
}

/// Reset a category's folder back to its built-in default.
#[tauri::command]
pub async fn reset_category_dir(id: i64, state: State<'_, AppState>) -> Result<(), CommandError> {
    state.db.reset_category_dir(id).map_err(err)
}

#[tauri::command]
pub async fn get_setting(
    key: String,
    state: State<'_, AppState>,
) -> Result<Option<String>, CommandError> {
    state.db.get_setting(&key).map_err(err)
}

/// Preferred OS UI locale, independent from the embedded WebView's locale.
/// Some Linux WebViews report English even when the desktop session is Turkish.
#[tauri::command]
pub fn get_system_locale() -> Option<String> {
    sys_locale::get_locales().find(|locale| !is_generic_locale(locale))
}

#[tauri::command]
pub async fn set_setting(
    key: String,
    value: String,
    state: State<'_, AppState>,
) -> Result<(), CommandError> {
    state.db.set_setting(&key, &value).map_err(err)
}

/// Claim or release the OS `magnet:` association immediately, then persist the
/// preference. If persistence fails, best-effort restore the previous handler
/// state so the switch and the operating system do not silently disagree.
#[tauri::command]
pub async fn set_handle_magnets(
    enabled: bool,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), CommandError> {
    let previous = state
        .db
        .get_setting("handle_magnets")
        .map_err(err)?
        .map(|value| value != "false")
        .unwrap_or(true);

    crate::configure_magnet_handler(&app, enabled).map_err(err)?;
    if let Err(e) = state
        .db
        .set_setting("handle_magnets", if enabled { "true" } else { "false" })
    {
        let _ = crate::configure_magnet_handler(&app, previous);
        return Err(err(e));
    }
    Ok(())
}

// ---- download details (expandable panel) ----

#[derive(serde::Serialize)]
pub struct DetailFile {
    pub index: i64,
    pub path: String,
    pub length: i64,
    pub completed_length: i64,
    pub selected: bool,
}

#[derive(serde::Serialize)]
pub struct DetailPeer {
    pub ip: String,
    pub down_speed: i64,
    pub up_speed: i64,
    pub seeder: bool,
}

#[derive(serde::Serialize)]
pub struct DownloadDetails {
    pub id: i64,
    pub url: String,
    pub dir: String,
    pub kind: String,
    pub error_message: Option<String>,
    pub num_pieces: i64,
    pub piece_length: i64,
    pub verified_length: i64,
    pub files: Vec<DetailFile>,
    pub peers: Vec<DetailPeer>,
    /// False when aria2 no longer knows the GID (or the row is yt-dlp) — the
    /// DB-only fields above are still valid.
    pub live: bool,
}

fn v_str(v: &serde_json::Value, k: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
}
/// aria2 serializes numbers as strings — parse defensively.
fn v_i64(v: &serde_json::Value, k: &str) -> i64 {
    v.get(k)
        .and_then(|x| {
            x.as_str()
                .map(String::from)
                .or_else(|| x.as_i64().map(|n| n.to_string()))
        })
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

/// Everything the expandable row panel shows: per-file progress + selection,
/// peers (torrents), piece info. yt-dlp rows and forgotten GIDs degrade to the
/// DB-only subset.
#[tauri::command]
pub async fn get_download_details(
    id: i64,
    state: State<'_, AppState>,
) -> Result<DownloadDetails, CommandError> {
    let d = state.db.get(id).map_err(err)?.ok_or("download not found")?;
    let mut det = DownloadDetails {
        id: d.id,
        url: d.url.clone(),
        dir: d.dir.clone(),
        kind: d.kind.clone(),
        error_message: d.error_message.clone(),
        num_pieces: 0,
        piece_length: 0,
        verified_length: 0,
        files: Vec::new(),
        peers: Vec::new(),
        live: false,
    };
    if d.is_ytdlp() {
        return Ok(det);
    }
    let Some(gid) = &d.gid else { return Ok(det) };
    let Ok(st) = state
        .engine
        .rpc
        .tell_status(gid, minidl_core::aria2::DETAIL_KEYS)
        .await
    else {
        return Ok(det);
    };
    det.live = true;
    det.num_pieces = v_i64(&st, "numPieces");
    det.piece_length = v_i64(&st, "pieceLength");
    det.verified_length = v_i64(&st, "verifiedLength");
    if let Some(files) = st.get("files").and_then(|f| f.as_array()) {
        det.files = files
            .iter()
            .map(|f| DetailFile {
                index: v_i64(f, "index"),
                path: v_str(f, "path"),
                length: v_i64(f, "length"),
                completed_length: v_i64(f, "completedLength"),
                selected: v_str(f, "selected") != "false",
            })
            .collect();
    }
    if matches!(d.kind.as_str(), "torrent" | "magnet") {
        det.peers = state
            .engine
            .rpc
            .get_peers(gid)
            .await
            .unwrap_or_default()
            .iter()
            .map(|p| DetailPeer {
                ip: v_str(p, "ip"),
                down_speed: v_i64(p, "downloadSpeed"),
                up_speed: v_i64(p, "uploadSpeed"),
                seeder: v_str(p, "seeder") == "true",
            })
            .collect();
    }
    Ok(det)
}

/// Restrict a torrent to the given file indices (1-based, aria2 `select-file`).
/// aria2 only honors the option for waiting/paused GIDs, so an active download
/// is paused around the change.
#[tauri::command]
pub async fn set_torrent_files(
    id: i64,
    indices: Vec<u32>,
    state: State<'_, AppState>,
) -> Result<(), CommandError> {
    if indices.is_empty() {
        return Err("select at least one file".into());
    }
    let d = state.db.get(id).map_err(err)?.ok_or("download not found")?;
    if !matches!(d.kind.as_str(), "torrent" | "magnet") {
        return Err("file selection only applies to torrents".into());
    }
    let gid = d
        .gid
        .as_deref()
        .ok_or("download has no engine handle yet")?;
    let sel = indices
        .iter()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let opts = json!({ "select-file": sel });
    if d.status == DownloadStatus::Active {
        let _ = state.engine.rpc.pause(gid).await;
        let res = state.engine.rpc.change_option(gid, opts).await;
        let _ = state.engine.rpc.unpause(gid).await;
        res.map_err(err)?;
    } else {
        state
            .engine
            .rpc
            .change_option(gid, opts)
            .await
            .map_err(err)?;
    }
    Ok(())
}

// ---- link grabber ----

#[tauri::command]
pub async fn grab_links(text: String) -> Result<Vec<ParsedLink>, CommandError> {
    Ok(minidl_core::grabber::parse_links(&text))
}

#[tauri::command]
pub async fn add_links_batch(
    urls: Vec<String>,
    package_name: Option<String>,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<usize, CommandError> {
    let urls: Vec<String> = urls
        .into_iter()
        .map(|u| u.trim().to_string())
        .filter(|u| !u.is_empty())
        .collect();
    // Group multi-link batches into a package; a single link stays ungrouped.
    let package_id = if urls.len() >= 2 {
        let name = package_name
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| {
                let host = minidl_core::grabber::host_of(&urls[0]);
                if host.is_empty() {
                    "Batch".to_string()
                } else {
                    host
                }
            });
        state.db.insert_package(&name, None, None).ok()
    } else {
        None
    };
    let mut added = 0;
    let defaults = state.defaults.lock().unwrap().clone();
    for u in urls {
        if matches!(
            ingest(
                &state.engine,
                &state.db,
                &state.ytdlp,
                &state.download_dir,
                defaults.clone(),
                job_from_url(u),
                None,
                package_id,
                None,
            )
            .await,
            Ok(IngestOutcome::Added(_))
        ) {
            added += 1;
        }
    }
    // An all-failed batch would otherwise leave an empty group header behind.
    if let Some(pkg) = package_id {
        let _ = state.db.delete_package_if_empty(pkg);
    }
    let _ = app.emit(EV_STATE, json!({ "batch": added }));
    Ok(added)
}

#[tauri::command]
pub async fn list_packages(state: State<'_, AppState>) -> Result<Vec<Package>, CommandError> {
    state.db.list_packages().map_err(err)
}

// ---- scheduler ----

#[tauri::command]
pub async fn list_schedules(state: State<'_, AppState>) -> Result<Vec<Schedule>, CommandError> {
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
) -> Result<(), CommandError> {
    state
        .db
        .save_schedule(
            id,
            name.as_deref(),
            &action,
            days_mask,
            at_minute,
            speed_limit,
            enabled,
        )
        .map_err(err)
        .map(|_| ())
}

#[tauri::command]
pub async fn delete_schedule(id: i64, state: State<'_, AppState>) -> Result<(), CommandError> {
    state.db.delete_schedule(id).map_err(err)
}

// ---- clipboard ----

#[tauri::command]
pub async fn set_clipboard_watch(
    enabled: bool,
    state: State<'_, AppState>,
) -> Result<(), CommandError> {
    state.clipboard_on.store(enabled, Ordering::Relaxed);
    state
        .db
        .set_setting("clipboard_watch", if enabled { "true" } else { "false" })
        .map_err(err)
}

// ---- engine tuning (segments / connections) ----

/// Returns `[split, connections]`.
#[tauri::command]
pub async fn get_engine_defaults(state: State<'_, AppState>) -> Result<(u32, u32), CommandError> {
    let d = state.defaults.lock().unwrap();
    Ok((d.split, d.max_connection_per_server))
}

#[tauri::command]
pub async fn set_engine_defaults(
    split: u32,
    connections: u32,
    state: State<'_, AppState>,
) -> Result<(), CommandError> {
    let split = split.clamp(1, 64);
    let connections = connections.clamp(1, 16);
    {
        let mut d = state.defaults.lock().unwrap();
        d.split = split;
        d.max_connection_per_server = connections;
    }
    state
        .db
        .set_setting("split", &split.to_string())
        .map_err(err)?;
    state
        .db
        .set_setting("max_conn", &connections.to_string())
        .map_err(err)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(label: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("minidl-{label}-{}-{stamp}", std::process::id()))
    }

    #[test]
    fn deletion_target_rejects_parent_traversal_and_keeps_nested_files() {
        let root = temp_dir("delete-target");
        let downloads = root.join("downloads");
        let nested = downloads.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        let outside = root.join("outside-file");
        std::fs::write(&outside, b"do not delete").unwrap();

        assert!(deletion_target_in_dir(&downloads, &downloads.join("../outside-file")).is_none());
        assert_eq!(std::fs::read(&outside).unwrap(), b"do not delete");

        let valid = nested.join("inside-file");
        assert_eq!(
            deletion_target_in_dir(&downloads, &valid).as_deref(),
            Some(valid.as_path())
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn generic_posix_locales_are_not_treated_as_a_language_preference() {
        assert!(is_generic_locale("C"));
        assert!(is_generic_locale(" POSIX "));
        assert!(is_generic_locale("und"));
        assert!(!is_generic_locale("tr-TR"));
        assert!(!is_generic_locale("en-US"));
    }

    #[cfg(unix)]
    #[test]
    fn deletion_target_rejects_symlinked_parent_outside_download_dir() {
        use std::os::unix::fs::symlink;

        let root = temp_dir("delete-symlink");
        let downloads = root.join("downloads");
        let outside = root.join("outside");
        std::fs::create_dir_all(&downloads).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        symlink(&outside, downloads.join("linked")).unwrap();

        assert!(deletion_target_in_dir(&downloads, &downloads.join("linked/file")).is_none());

        let _ = std::fs::remove_dir_all(root);
    }
}
