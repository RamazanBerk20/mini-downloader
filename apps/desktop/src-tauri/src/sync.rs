//! Live sync between aria2 and the DB/UI: a notification consumer (prompt state
//! transitions), a 1 Hz progress poller (batched ticks + DB checkpoints + a
//! polling fallback for transitions), and startup reconciliation.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

use ldm_core::aria2::{Engine, STATUS_KEYS};
use ldm_core::db::Db;
use ldm_core::model::DownloadStatus;

use crate::events::{Tick, EV_COMPLETE, EV_ERROR, EV_STATE, EV_TICK};

pub fn spawn(app: AppHandle, engine: Arc<Engine>, db: Db) {
    // 1. Notification consumer — react to pushed lifecycle events immediately.
    {
        let app = app.clone();
        let engine = engine.clone();
        let db = db.clone();
        let mut rx = engine.subscribe();
        tauri::async_runtime::spawn(async move {
            while let Ok(ev) = rx.recv().await {
                handle_transition(&app, &engine, &db, ev.gid()).await;
            }
        });
    }

    // 2. Progress poller — ticks for active items + transition fallback.
    tauri::async_runtime::spawn(async move {
        let mut known: HashSet<String> = HashSet::new();
        let mut gid_id: HashMap<String, i64> = HashMap::new();
        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        loop {
            ticker.tick().await;
            let items = match engine.rpc.tell_active(STATUS_KEYS).await {
                Ok(v) => v,
                Err(_) => continue,
            };

            let mut current: HashSet<String> = HashSet::new();
            let mut ticks: Vec<Tick> = Vec::with_capacity(items.len());
            for it in &items {
                let gid = str_field(it, "gid");
                if gid.is_empty() {
                    continue;
                }
                current.insert(gid.clone());
                let name = basename(it);
                let id = match gid_id.get(&gid) {
                    Some(i) => *i,
                    None => match db.find_by_gid(&gid) {
                        Ok(Some(d)) => {
                            if !name.is_empty() && d.filename.is_none() {
                                let _ = db.set_filename(d.id, &name);
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
                let _ = db.checkpoint_progress(&gid, completed, total, dl, ul, conns, seeders);
                ticks.push(Tick {
                    id,
                    gid: gid.clone(),
                    name,
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
            known = current;
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
    let new_status = match str_field(&st, "status").as_str() {
        "complete" => DownloadStatus::Complete,
        "error" => DownloadStatus::Error,
        "paused" => DownloadStatus::Paused,
        "active" => DownloadStatus::Active,
        "waiting" => DownloadStatus::Waiting,
        "removed" => DownloadStatus::Removed,
        _ => return,
    };
    if row.status == new_status {
        return; // already reflected
    }

    match new_status {
        DownloadStatus::Error => {
            let code = st.get("errorCode").and_then(|v| v.as_str());
            let message = st.get("errorMessage").and_then(|v| v.as_str());
            let _ = db.set_error(row.id, code, message);
            let _ = app.emit(EV_ERROR, json!({ "gid": gid, "id": row.id, "code": code, "message": message }));
        }
        DownloadStatus::Complete => {
            let name = basename(&st);
            let final_name = if name.is_empty() {
                row.filename.clone().unwrap_or_default()
            } else {
                if row.filename.is_none() {
                    let _ = db.set_filename(row.id, &name);
                }
                name
            };
            let _ = db.set_status(row.id, DownloadStatus::Complete);
            // Category auto-organize (single-file HTTP): may move the file.
            let final_dir = organize(db, &row, &final_name);
            let path = format!("{final_dir}/{final_name}");
            let _ = app.emit(EV_COMPLETE, json!({ "gid": gid, "id": row.id, "name": final_name, "path": path }));
        }
        other => {
            let _ = db.set_status(row.id, other);
        }
    }
    let _ = app.emit(EV_STATE, json!({ "gid": gid, "id": row.id, "status": new_status.as_str() }));
}

/// Startup reconciliation: aria2 restored its session (GIDs preserved), so refresh
/// each DB row from aria2's live view; rows whose GID aria2 no longer knows are
/// marked paused (interrupted) so the user can resume/re-add.
pub async fn reconcile(engine: &Engine, db: &Db) {
    let mut all: Vec<Value> = Vec::new();
    all.extend(engine.rpc.tell_active(STATUS_KEYS).await.unwrap_or_default());
    all.extend(engine.rpc.tell_waiting(0, 10_000, STATUS_KEYS).await.unwrap_or_default());
    all.extend(engine.rpc.tell_stopped(0, 10_000, STATUS_KEYS).await.unwrap_or_default());

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
                let mapped = match str_field(item, "status").as_str() {
                    "complete" => DownloadStatus::Complete,
                    "error" => DownloadStatus::Error,
                    "paused" => DownloadStatus::Paused,
                    "active" => DownloadStatus::Active,
                    "waiting" => DownloadStatus::Waiting,
                    "removed" => DownloadStatus::Removed,
                    _ => continue,
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

/// Move a finished single-file HTTP download into its category folder. Returns
/// the (possibly new) directory.
fn organize(db: &Db, row: &ldm_core::model::Download, filename: &str) -> String {
    let auto = db
        .get_setting("auto_organize")
        .ok()
        .flatten()
        .map(|v| v != "false")
        .unwrap_or(true);
    if !auto || filename.is_empty() || row.kind != "http" {
        return row.dir.clone();
    }
    let cats = db.list_categories().unwrap_or_default();
    let Some(cat) = ldm_core::categories::classify(filename, &cats) else {
        return row.dir.clone();
    };
    let target = ldm_core::categories::expand(&cat.dir);
    let target_str = target.to_string_lossy().to_string();
    if target_str == row.dir {
        return row.dir.clone();
    }
    let src = std::path::Path::new(&row.dir).join(filename);
    if !src.exists() {
        return row.dir.clone();
    }
    let _ = std::fs::create_dir_all(&target);
    let dst = target.join(filename);
    let moved = std::fs::rename(&src, &dst).is_ok()
        || (std::fs::copy(&src, &dst).is_ok() && std::fs::remove_file(&src).is_ok());
    if moved {
        let _ = db.set_dir_and_category(row.id, &target_str, cat.id);
        target_str
    } else {
        row.dir.clone()
    }
}

fn str_field(v: &Value, key: &str) -> String {
    v.get(key).and_then(|x| x.as_str()).unwrap_or("").to_string()
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
