//! Live sync between aria2 and the DB/UI: a notification consumer (prompt state
//! transitions), a 1 Hz progress poller (batched ticks + DB checkpoints + a
//! polling fallback for transitions), and startup reconciliation.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};
use tauri_plugin_notification::NotificationExt;

use minidl_core::aria2::{Aria2Event, Engine, POLL_KEYS, STATUS_KEYS};
use minidl_core::db::Db;
use minidl_core::model::{Download, DownloadStatus};

use crate::events::{Tick, EV_COMPLETE, EV_ERROR, EV_STATE, EV_TICK};

/// Guards the on-complete power action so it fires once per drained queue (reset
/// when a new download starts).
static POWER_FIRED: AtomicBool = AtomicBool::new(false);

pub fn spawn(app: AppHandle, engine: Arc<Engine>, db: Db) {
    // Lets the notification consumer snap the (possibly idle-backed-off) poller
    // back to fast cadence the moment a download starts.
    let wake = Arc::new(tokio::sync::Notify::new());

    // 1. Notification consumer — react to pushed lifecycle events immediately.
    {
        let app = app.clone();
        let engine = engine.clone();
        let db = db.clone();
        let wake = wake.clone();
        let mut rx = engine.subscribe();
        tauri::async_runtime::spawn(async move {
            use tokio::sync::broadcast::error::RecvError;
            loop {
                match rx.recv().await {
                    Ok(ev) => {
                        // A newly started download re-arms the on-complete action.
                        if matches!(ev, Aria2Event::Start(_)) {
                            POWER_FIRED.store(false, Ordering::Relaxed);
                        }
                        wake.notify_one();
                        handle_transition(&app, &engine, &db, ev.gid()).await;
                    }
                    // A burst that overruns the 256-slot ring is recoverable — the
                    // poller still catches the transition. Keep consuming; only a
                    // closed channel (engine gone) ends the task.
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => break,
                }
            }
        });
    }

    // 2. Progress poller — ticks for active items + transition fallback.
    tauri::async_runtime::spawn(async move {
        let mut known: HashSet<String> = HashSet::new();
        let mut gid_id: HashMap<String, i64> = HashMap::new();
        let mut idle_streak = 0u32;
        let mut ckpt_tick = 0u32;
        loop {
            let items = engine.rpc.tell_active(POLL_KEYS).await.unwrap_or_default();

            // Persist a snapshot at most ~every 5s — it only matters across a
            // restart, so 1 write/sec/item of WAL churn is wasteful. Ticks still
            // emit every cycle; the final bytes are checkpointed on transition.
            ckpt_tick = ckpt_tick.wrapping_add(1);
            let do_checkpoint = ckpt_tick % 5 == 0;

            let mut current: HashSet<String> = HashSet::new();
            let mut ticks: Vec<Tick> = Vec::with_capacity(items.len());
            for it in &items {
                let gid = str_field(it, "gid");
                if gid.is_empty() {
                    continue;
                }
                current.insert(gid.clone());
                let id = match gid_id.get(&gid) {
                    Some(i) => *i,
                    None => match db.find_by_gid(&gid) {
                        Ok(Some(d)) => {
                            // One-shot filename backfill (POLL_KEYS omits `files`).
                            if d.filename.is_none() {
                                if let Ok(st) = engine.rpc.tell_status(&gid, &["files"]).await {
                                    let name = basename(&st);
                                    if !name.is_empty() {
                                        let _ = db.set_filename(d.id, &name);
                                    }
                                }
                            }
                            gid_id.insert(gid.clone(), d.id);
                            d.id
                        }
                        _ => 0,
                    },
                };
                let completed = num_field(it, "completedLength");
                let total = num_field(it, "totalLength");
                let dl = num_field(it, "downloadSpeed");
                let ul = num_field(it, "uploadSpeed");
                let conns = num_field(it, "connections");
                let seeders = num_field(it, "numSeeders");
                if do_checkpoint {
                    let _ = db.checkpoint_progress(&gid, completed, total, dl, ul, conns, seeders);
                }
                ticks.push(Tick {
                    id,
                    gid: gid.clone(),
                    name: String::new(),
                    completed,
                    total,
                    dl_speed: dl,
                    ul_speed: ul,
                    connections: conns,
                    num_seeders: seeders,
                    status: "active".into(),
                });
            }

            if !ticks.is_empty() {
                let _ = app.emit(EV_TICK, json!({ "updates": ticks }));
            }

            // gids that left the active set since last tick → transitioned.
            for gid in known.difference(&current) {
                handle_transition(&app, &engine, &db, gid).await;
            }
            gid_id.retain(|g, _| current.contains(g));
            known = current;

            // Idle backoff: with nothing active, grow the poll interval so a
            // tray-hidden app doesn't spin a CPU. A WS `Start` notify snaps it
            // straight back to 1 Hz.
            let dur = if items.is_empty() {
                idle_streak = idle_streak.saturating_add(1);
                if idle_streak > 3 {
                    Duration::from_secs(5)
                } else {
                    Duration::from_secs(1)
                }
            } else {
                idle_streak = 0;
                Duration::from_secs(1)
            };
            tokio::select! {
                _ = tokio::time::sleep(dur) => {}
                _ = wake.notified() => {}
            }
        }
    });
}

/// Fetch a GID's final status, persist it, and emit the matching event. Idempotent.
async fn handle_transition(app: &AppHandle, engine: &Engine, db: &Db, gid: &str) {
    let st = match engine.rpc.tell_status(gid, STATUS_KEYS).await {
        Ok(v) => v,
        Err(_) => return, // gid gone (e.g. removed) — nothing to do
    };
    let row = match db.find_by_gid(gid) {
        Ok(Some(d)) => d,
        _ => return,
    };
    // Persist the final byte counts on every transition so a completion/pause
    // snapshot is accurate even though the poller only checkpoints periodically.
    let _ = db.checkpoint_progress(
        gid,
        num_field(&st, "completedLength"),
        num_field(&st, "totalLength"),
        num_field(&st, "downloadSpeed"),
        num_field(&st, "uploadSpeed"),
        num_field(&st, "connections"),
        num_field(&st, "numSeeders"),
    );
    let Some(new_status) = DownloadStatus::from_aria2_str(&str_field(&st, "status")) else {
        return;
    };
    if row.status == new_status {
        return; // already reflected
    }
    // Scheduled is an app-only hold: schedule_download pauses the GID and then
    // writes Scheduled, so aria2's resulting "paused" (or still-queued
    // "waiting") must not overwrite it — the scheduler only starts rows whose
    // status is still 'scheduled'.
    if row.status == DownloadStatus::Scheduled
        && matches!(new_status, DownloadStatus::Paused | DownloadStatus::Waiting)
    {
        return;
    }

    match new_status {
        DownloadStatus::Error => {
            // Atomic gate: the WS consumer and the poller can both observe the
            // same transition — only the first flip fires the notification.
            if !db
                .set_status_if_changed(row.id, DownloadStatus::Error)
                .unwrap_or(false)
            {
                return;
            }
            let code = st.get("errorCode").and_then(|v| v.as_str());
            let message = st.get("errorMessage").and_then(|v| v.as_str());
            let _ = db.set_error(row.id, code, message);
            let _ = app.emit(
                EV_ERROR,
                json!({ "gid": gid, "id": row.id, "code": code, "message": message }),
            );
            let name = row.filename.clone().unwrap_or_else(|| row.url.clone());
            let locale = db.get_setting("locale").ok().flatten().unwrap_or_default();
            let _ = app
                .notification()
                .builder()
                .title(minidl_core::i18n::tr(
                    &locale,
                    minidl_core::i18n::Msg::DownloadFailed,
                ))
                .body(&name)
                .show();
        }
        DownloadStatus::Complete => {
            // A magnet/torrent metadata download "completes" by spawning the real
            // content under a new `followedBy` GID. Rebind this row to that child
            // GID instead of reporting completion — otherwise we notify "complete"
            // while nothing real downloaded and the child GID is never tracked.
            if let Some(child) = first_followed_by(&st) {
                let _ = db.set_gid(row.id, &child);
                if let Some(ih) = st
                    .get("infoHash")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                {
                    let _ = db.set_info_hash(row.id, ih);
                }
                let _ = db.set_status(row.id, DownloadStatus::Active);
                let _ = app.emit(
                    EV_STATE,
                    json!({ "gid": child, "id": row.id, "status": "active" }),
                );
                return;
            }
            // Atomic finalize gate: run organize + notify exactly once even if two
            // tasks race the same completion.
            if !db
                .set_status_if_changed(row.id, DownloadStatus::Complete)
                .unwrap_or(false)
            {
                return;
            }
            let name = basename(&st);
            let final_name = if name.is_empty() {
                row.filename.clone().unwrap_or_default()
            } else {
                name
            };
            // Category auto-organize (single-file HTTP): may move the file.
            let (final_dir, final_name) = organize(db, &row, &final_name);
            // aria2 can derive a different output name (and category collision
            // handling can add a suffix), so keep the displayed path in sync
            // even when no category move occurred.
            if !final_name.is_empty() && row.filename.as_deref() != Some(final_name.as_str()) {
                let _ = db.set_filename(row.id, &final_name);
            }
            let path = std::path::Path::new(&final_dir)
                .join(&final_name)
                .to_string_lossy()
                .to_string();
            // Provenance (mark-of-the-web): record the origin so the file isn't
            // less traceable than a normal browser download.
            #[cfg(unix)]
            set_origin_xattr(
                std::path::Path::new(&path),
                &row.url,
                row.referrer.as_deref(),
            );
            let _ = app.emit(
                EV_COMPLETE,
                json!({ "gid": gid, "id": row.id, "name": final_name, "path": path }),
            );
            let locale = db.get_setting("locale").ok().flatten().unwrap_or_default();
            let _ = app
                .notification()
                .builder()
                .title(minidl_core::i18n::tr(
                    &locale,
                    minidl_core::i18n::Msg::DownloadComplete,
                ))
                .body(&final_name)
                .show();
            maybe_power_action(app, engine, db).await;
        }
        other => {
            let _ = db.set_status(row.id, other);
        }
    }
    let _ = app.emit(
        EV_STATE,
        json!({ "gid": gid, "id": row.id, "status": new_status.as_str() }),
    );
}

/// Before the UI can issue Resume/Resume All, pair the force-paused startup
/// session with app-owned rows and purge everything else. `session_gid` is the
/// durable metadata-parent GID aria2 writes for magnets and remote
/// torrent/metalink URLs; `gid` is allowed to move to its followed-by child
/// while the app is running.
pub async fn restore_startup_session(engine: &Engine, db: &Db) {
    // Defense in depth: process.rs adds pause=true before aria2 starts, and
    // this catches any unexpected active item before cleanup.
    let _ = engine.rpc.pause_all().await;

    let mut items: Vec<Value> = Vec::new();
    items.extend(
        engine
            .rpc
            .tell_active(STATUS_KEYS)
            .await
            .unwrap_or_default(),
    );
    items.extend(
        engine
            .rpc
            .tell_waiting(0, 10_000, STATUS_KEYS)
            .await
            .unwrap_or_default(),
    );
    items.extend(
        engine
            .rpc
            .tell_stopped(0, 10_000, STATUS_KEYS)
            .await
            .unwrap_or_default(),
    );

    let rows = db
        .resumable_session_rows()
        .unwrap_or_default()
        .into_iter()
        .filter(|row| !row.is_ytdlp())
        .collect::<Vec<_>>();
    let mut claimed_rows = HashSet::new();
    let mut kept_gids = HashSet::new();

    for item in &items {
        let gid = str_field(item, "gid");
        if gid.is_empty() {
            continue;
        }
        let Some(row) = startup_session_row(item, &rows, &claimed_rows) else {
            continue;
        };
        // Rebind to the restored parent before a user can click Resume. A later
        // followedBy event moves only the live gid back to the content child.
        if db.bind_aria2_job(row.id, &gid).is_ok() {
            claimed_rows.insert(row.id);
            kept_gids.insert(gid);
        }
    }

    for item in &items {
        let gid = str_field(item, "gid");
        if gid.is_empty() || kept_gids.contains(&gid) {
            continue;
        }
        // Stale entries never receive a UI row. Remove them while still paused
        // so Resume All cannot accidentally wake an invisible transfer later.
        match str_field(item, "status").as_str() {
            "active" => {
                let _ = engine.rpc.force_remove(&gid).await;
                let _ = engine.rpc.remove_download_result(&gid).await;
            }
            "complete" | "error" | "removed" => {
                let _ = engine.rpc.remove_download_result(&gid).await;
            }
            _ => {
                let _ = engine.rpc.remove(&gid).await;
                let _ = engine.rpc.remove_download_result(&gid).await;
            }
        }
    }

    // Persist the pruned, still-paused queue now rather than leaving a stale
    // session around until aria2's periodic save or application shutdown.
    let _ = engine.rpc.save_session().await;
}

fn startup_session_row<'a>(
    item: &Value,
    rows: &'a [Download],
    claimed_rows: &HashSet<i64>,
) -> Option<&'a Download> {
    let gid = str_field(item, "gid");
    if gid.is_empty() {
        return None;
    }

    // session_gid is authoritative for newly-created rows. Do not let a stale
    // followed-by child claim the row before its saved metadata parent arrives.
    if let Some(row) = unique_unclaimed(
        rows.iter()
            .filter(|row| row.session_gid.as_deref() == Some(gid.as_str())),
        claimed_rows,
    ) {
        return Some(row);
    }

    // Migration path for releases before session_gid existed: a direct session
    // GID can safely seed it. For metadata parents, use a conservative unique
    // source URL + destination match (or info hash) once, then persist it.
    if let Some(row) = unique_unclaimed(
        rows.iter()
            .filter(|row| row.session_gid.is_none() && row.gid.as_deref() == Some(gid.as_str())),
        claimed_rows,
    ) {
        return Some(row);
    }

    let session_dir = str_field(item, "dir");
    let uris = session_source_uris(item);
    if !uris.is_empty() {
        if let Some(row) = unique_unclaimed(
            rows.iter().filter(|row| {
                row.session_gid.is_none()
                    && row.gid.is_some()
                    && (session_dir.is_empty() || row.dir == session_dir)
                    && uris.iter().any(|uri| uri == &row.url)
            }),
            claimed_rows,
        ) {
            return Some(row);
        }
    }

    let info_hash = str_field(item, "infoHash");
    if !info_hash.is_empty() {
        return unique_unclaimed(
            rows.iter().filter(|row| {
                row.session_gid.is_none()
                    && row.gid.is_some()
                    && row.info_hash.as_deref() == Some(info_hash.as_str())
            }),
            claimed_rows,
        );
    }
    None
}

fn unique_unclaimed<'a>(
    rows: impl Iterator<Item = &'a Download>,
    claimed_rows: &HashSet<i64>,
) -> Option<&'a Download> {
    let mut found = None;
    for row in rows {
        if claimed_rows.contains(&row.id) {
            continue;
        }
        if found.is_some() {
            return None;
        }
        found = Some(row);
    }
    found
}

fn session_source_uris(item: &Value) -> Vec<String> {
    let mut uris = Vec::new();
    let Some(files) = item.get("files").and_then(|files| files.as_array()) else {
        return uris;
    };
    for file in files {
        let Some(file_uris) = file.get("uris").and_then(|uris| uris.as_array()) else {
            continue;
        };
        for uri in file_uris {
            if let Some(uri) = uri
                .get("uri")
                .and_then(|uri| uri.as_str())
                .filter(|uri| !uri.is_empty())
            {
                uris.push(uri.to_string());
            }
        }
    }
    uris
}

/// Startup reconciliation: after ownership cleanup, refresh each DB row from
/// aria2's force-paused live view; rows whose GID aria2 no longer knows are
/// marked paused (interrupted) so the user can resume/re-add.
pub async fn reconcile(engine: &Engine, db: &Db) {
    let mut all: Vec<Value> = Vec::new();
    all.extend(
        engine
            .rpc
            .tell_active(STATUS_KEYS)
            .await
            .unwrap_or_default(),
    );
    all.extend(
        engine
            .rpc
            .tell_waiting(0, 10_000, STATUS_KEYS)
            .await
            .unwrap_or_default(),
    );
    all.extend(
        engine
            .rpc
            .tell_stopped(0, 10_000, STATUS_KEYS)
            .await
            .unwrap_or_default(),
    );

    let live: HashMap<String, &Value> = all
        .iter()
        .filter_map(|it| {
            let g = str_field(it, "gid");
            if g.is_empty() {
                None
            } else {
                Some((g, it))
            }
        })
        .collect();

    for row in db.running_rows().unwrap_or_default() {
        let Some(gid) = &row.gid else {
            // yt-dlp rows carry no GID; their subprocess died with the app. Mark
            // still-running ones Paused (interrupted) so they don't linger as
            // un-resumable "active" zombies with frozen progress.
            if matches!(row.status, DownloadStatus::Active | DownloadStatus::Waiting) {
                let _ = db.set_status(row.id, DownloadStatus::Paused);
            }
            continue;
        };
        match live.get(gid) {
            Some(item) => {
                let completed = num_field(item, "completedLength");
                let total = num_field(item, "totalLength");
                let _ = db.checkpoint_progress(
                    gid,
                    completed,
                    total,
                    0,
                    0,
                    0,
                    num_field(item, "numSeeders"),
                );
                let Some(mapped) = DownloadStatus::from_aria2_str(&str_field(item, "status"))
                else {
                    continue;
                };
                if mapped == DownloadStatus::Error {
                    let _ = db.set_error(
                        row.id,
                        item.get("errorCode").and_then(|v| v.as_str()),
                        item.get("errorMessage").and_then(|v| v.as_str()),
                    );
                } else {
                    let _ = db.set_status(row.id, mapped);
                }
            }
            None => {
                // aria2 forgot this GID (crash before checkpoint) — interrupted.
                let _ = db.set_status(row.id, DownloadStatus::Paused);
            }
        }
    }
}

/// Reduce an aria2-reported filename to one path component before building a
/// source or category destination. Captured filenames are untrusted and may use
/// either platform's separator even when the app currently runs on only one.
fn safe_filename(filename: &str) -> Option<String> {
    let name = filename.rsplit(['/', '\\']).next().unwrap_or("").trim();
    if name.is_empty() || name == "." || name == ".." {
        None
    } else {
        Some(name.to_string())
    }
}

fn collision_name(filename: &str, suffix: u32) -> String {
    match filename.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() => {
            format!("{stem} ({suffix}).{ext}")
        }
        _ => format!("{filename} ({suffix})"),
    }
}

/// Copy to a new destination without ever replacing a pre-existing file. The
/// `create_new` open is the actual collision guard; a preceding `exists` check
/// would race another completion and let a normal `rename` overwrite its file.
fn copy_file_new(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    let mut input = std::fs::File::open(src)?;
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dst)?;
    let copied = std::io::copy(&mut input, &mut output).map(|_| ());
    drop(output);
    if copied.is_err() {
        // This process created the destination, so removing a partial copy
        // cannot touch an existing user file.
        let _ = std::fs::remove_file(dst);
    }
    copied
}

/// Move `src` to the first available deterministic name in `target`. Copying
/// through an exclusive destination works both across filesystems and under
/// concurrent completion events; the source is removed only after the full copy
/// exists.
fn move_to_unique_target(
    src: &std::path::Path,
    target: &std::path::Path,
    filename: &str,
) -> std::io::Result<(std::path::PathBuf, String)> {
    for suffix in 0..=u32::MAX {
        let candidate_name = if suffix == 0 {
            filename.to_string()
        } else {
            collision_name(filename, suffix)
        };
        let dst = target.join(&candidate_name);
        match copy_file_new(src, &dst) {
            Ok(()) => {
                if let Err(err) = std::fs::remove_file(src) {
                    // Avoid leaving a duplicate file behind if the source can no
                    // longer be removed after a successful copy.
                    let _ = std::fs::remove_file(&dst);
                    return Err(err);
                }
                return Ok((dst, candidate_name));
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "no available filename in category directory",
    ))
}

fn same_directory(left: &std::path::Path, right: &std::path::Path) -> bool {
    left == right
        || matches!(
            (left.canonicalize(), right.canonicalize()),
            (Ok(left), Ok(right)) if left == right
        )
}

/// Move a finished single-file HTTP download into its category folder. Returns
/// its final directory and filename. A collision never replaces a pre-existing
/// category file; the moved row is updated atomically with its unique name.
fn organize(db: &Db, row: &minidl_core::model::Download, filename: &str) -> (String, String) {
    let Some(filename) = safe_filename(filename) else {
        return (row.dir.clone(), filename.to_string());
    };
    let auto = db
        .get_setting("auto_organize")
        .ok()
        .flatten()
        .map(|v| v != "false")
        .unwrap_or(true);
    if !auto || filename.is_empty() || row.kind != "http" {
        return (row.dir.clone(), filename);
    }
    let cats = db.list_categories().unwrap_or_default();
    let host = minidl_core::grabber::host_of(&row.url);
    let host = if host.is_empty() {
        None
    } else {
        Some(host.as_str())
    };
    let Some(cat) = minidl_core::categories::classify(&filename, row.mime.as_deref(), host, &cats)
    else {
        return (row.dir.clone(), filename);
    };
    let target = minidl_core::categories::expand(&cat.dir);
    let target_str = target.to_string_lossy().to_string();
    let src = std::path::Path::new(&row.dir).join(&filename);
    if !src.exists() {
        return (row.dir.clone(), filename);
    }
    if std::fs::create_dir_all(&target).is_err()
        || same_directory(&target, std::path::Path::new(&row.dir))
    {
        return (row.dir.clone(), filename);
    }
    if let Ok((_dst, moved_name)) = move_to_unique_target(&src, &target, &filename) {
        // Drop the aria2 control file left in the source dir, if any.
        let mut sidecar = src.into_os_string();
        sidecar.push(".aria2");
        let _ = std::fs::remove_file(std::path::PathBuf::from(sidecar));
        let _ = db.set_file_location(row.id, &moved_name, &target_str, cat.id);
        (target_str, moved_name)
    } else {
        (row.dir.clone(), filename)
    }
}

fn custom_command_confirmed(db: &Db) -> bool {
    db.get_setting("on_complete_command_confirmed")
        .ok()
        .flatten()
        .as_deref()
        == Some("true")
}

/// After a completion, if the whole queue has drained, run the configured
/// on-complete power action exactly once (unattended overnight downloads).
async fn maybe_power_action(app: &AppHandle, engine: &Engine, db: &Db) {
    let action = db
        .get_setting("on_complete_action")
        .ok()
        .flatten()
        .unwrap_or_default();
    if action.is_empty() || action == "none" {
        return;
    }
    // aria2 still has active/waiting work?
    if let Ok(stat) = engine.rpc.get_global_stat().await {
        let n = |k: &str| {
            stat.get(k)
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(0)
        };
        if n("numActive") + n("numWaiting") > 0 {
            return;
        }
    }
    // yt-dlp (GID-less) rows still running?
    if db
        .running_rows()
        .map(|r| r.iter().any(|d| d.status.is_running()))
        .unwrap_or(false)
    {
        return;
    }
    // Do not consume the drained-queue one-shot for a command that has not
    // been explicitly acknowledged. This check is intentionally before the
    // atomic gate below.
    if action.starts_with("run:") && !custom_command_confirmed(db) {
        return;
    }
    // Fire once per drained queue (re-armed when a new download starts).
    if POWER_FIRED.swap(true, Ordering::SeqCst) {
        return;
    }
    match action.as_str() {
        "quit" => app.exit(0),
        "sleep" => {
            let _ = std::process::Command::new("systemctl")
                .arg("suspend")
                .spawn();
        }
        "shutdown" => {
            let _ = std::process::Command::new("systemctl")
                .arg("poweroff")
                .spawn();
        }
        other => {
            if let Some(cmd) = other.strip_prefix("run:") {
                // A command is deliberately opt-in twice: the setting must
                // contain it and the UI must have recorded a fresh explicit
                // acknowledgement. This makes pre-2.1 stored commands inert
                // until their owner reviews them once.
                let _ = std::process::Command::new("sh").arg("-c").arg(cmd).spawn();
            }
        }
    }
}

/// Tag a finished file with its origin URL/referrer (freedesktop mark-of-the-web).
#[cfg(unix)]
fn set_origin_xattr(path: &std::path::Path, url: &str, referrer: Option<&str>) {
    let _ = xattr::set(path, "user.xdg.origin.url", url.as_bytes());
    if let Some(r) = referrer {
        let _ = xattr::set(path, "user.xdg.referrer.url", r.as_bytes());
    }
}

/// First `followedBy` GID of a status object, if any — present on a magnet /
/// torrent metadata download that has spawned the real content download.
fn first_followed_by(st: &Value) -> Option<String> {
    st.get("followedBy")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|v| v.as_str())
        .map(String::from)
        .filter(|s| !s.is_empty())
}

fn str_field(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}

fn num_field(v: &Value, key: &str) -> i64 {
    v.get(key)
        .and_then(|x| x.as_str())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

fn basename(item: &Value) -> String {
    item.get("files")
        .and_then(|f| f.get(0))
        .and_then(|f0| f0.get("path"))
        .and_then(|p| p.as_str())
        .map(|p| p.rsplit('/').next().unwrap_or(p).to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidl_core::model::NewDownload;

    fn temp_dir(label: &str) -> std::path::PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("minidl-{label}-{}-{stamp}", std::process::id()))
    }

    #[test]
    fn custom_command_needs_explicit_confirmation() {
        let db = Db::open_in_memory().unwrap();
        assert!(!custom_command_confirmed(&db));
        db.set_setting("on_complete_command_confirmed", "false")
            .unwrap();
        assert!(!custom_command_confirmed(&db));
        db.set_setting("on_complete_command_confirmed", "true")
            .unwrap();
        assert!(custom_command_confirmed(&db));
    }

    #[test]
    fn startup_match_uses_metadata_parent_session_gid() {
        let db = Db::open_in_memory().unwrap();
        let id = db
            .insert_download(&NewDownload {
                url: "magnet:?xt=urn:btih:abc".into(),
                dir: "/dl".into(),
                kind: "magnet".into(),
                ..Default::default()
            })
            .unwrap();
        db.bind_aria2_job(id, "metadata-parent").unwrap();
        db.set_gid(id, "content-child").unwrap();
        let rows = db.resumable_session_rows().unwrap();

        let item = json!({ "gid": "metadata-parent", "status": "paused" });
        let row = startup_session_row(&item, &rows, &HashSet::new()).unwrap();

        assert_eq!(row.id, id);
        assert_eq!(row.gid.as_deref(), Some("content-child"));
        assert_eq!(row.session_gid.as_deref(), Some("metadata-parent"));
    }

    #[test]
    fn legacy_metadata_session_can_match_unique_source_and_directory() {
        let db = Db::open_in_memory().unwrap();
        let id = db
            .insert_download(&NewDownload {
                url: "https://example.invalid/file.torrent".into(),
                dir: "/dl".into(),
                kind: "http".into(),
                ..Default::default()
            })
            .unwrap();
        db.set_gid(id, "old-content-child").unwrap();
        let rows = db.resumable_session_rows().unwrap();
        let item = json!({
            "gid": "legacy-parent",
            "status": "paused",
            "dir": "/dl",
            "files": [{"uris": [{"uri": "https://example.invalid/file.torrent"}]}],
        });

        let row = startup_session_row(&item, &rows, &HashSet::new()).unwrap();

        assert_eq!(row.id, id);
    }

    #[test]
    fn organize_uses_next_available_name_without_overwriting_and_updates_db() {
        let root = temp_dir("organize-collision");
        let source_dir = root.join("downloads");
        let category_dir = root.join("category");
        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::create_dir_all(&category_dir).unwrap();
        std::fs::write(source_dir.join("report.bin"), b"new download").unwrap();
        std::fs::write(category_dir.join("report.bin"), b"existing original").unwrap();
        std::fs::write(category_dir.join("report (1).bin"), b"existing first copy").unwrap();

        let db = Db::open_in_memory().unwrap();
        let category_id = db
            .upsert_category(
                "Collision test",
                &category_dir.to_string_lossy(),
                r#"["bin"]"#,
                -1,
            )
            .unwrap();
        let id = db
            .insert_download(&NewDownload {
                url: "https://example.invalid/report.bin".into(),
                filename: Some("report.bin".into()),
                dir: source_dir.to_string_lossy().to_string(),
                kind: "http".into(),
                ..Default::default()
            })
            .unwrap();
        let row = db.get(id).unwrap().unwrap();

        let (final_dir, final_name) = organize(&db, &row, "report.bin");

        assert_eq!(final_dir, category_dir.to_string_lossy());
        assert_eq!(final_name, "report (2).bin");
        assert_eq!(
            std::fs::read(category_dir.join("report.bin")).unwrap(),
            b"existing original"
        );
        assert_eq!(
            std::fs::read(category_dir.join("report (1).bin")).unwrap(),
            b"existing first copy"
        );
        assert_eq!(
            std::fs::read(category_dir.join("report (2).bin")).unwrap(),
            b"new download"
        );
        assert!(!source_dir.join("report.bin").exists());

        let stored = db.get(id).unwrap().unwrap();
        assert_eq!(stored.filename.as_deref(), Some("report (2).bin"));
        assert_eq!(stored.dir, category_dir.to_string_lossy());
        assert_eq!(stored.category_id, Some(category_id));

        let _ = std::fs::remove_dir_all(root);
    }
}
