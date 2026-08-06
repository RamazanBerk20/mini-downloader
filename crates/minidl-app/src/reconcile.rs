//! Safe startup ownership reconciliation shared by every desktop shell.

use std::collections::{HashMap, HashSet};

use minidl_core::aria2::{Engine, STATUS_KEYS};
use minidl_core::db::Db;
use minidl_core::model::{Download, DownloadStatus};
use serde_json::Value;

fn string(item: &Value, key: &str) -> String {
    item.get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn number(item: &Value, key: &str) -> i64 {
    item.get(key)
        .and_then(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .or_else(|| value.as_i64().map(|n| n.to_string()))
        })
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
}

async fn session_items(engine: &Engine) -> Vec<Value> {
    let mut items = engine
        .rpc
        .tell_active(STATUS_KEYS)
        .await
        .unwrap_or_default();
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
    items
}

fn source_uris(item: &Value) -> Vec<String> {
    item.get("files")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|file| file.get("uris").and_then(Value::as_array))
        .flatten()
        .filter_map(|uri| uri.get("uri").and_then(Value::as_str))
        .filter(|uri| !uri.is_empty())
        .map(str::to_owned)
        .collect()
}

fn unique_unclaimed<'a>(
    rows: impl Iterator<Item = &'a Download>,
    claimed: &HashSet<i64>,
) -> Option<&'a Download> {
    let mut matches = rows.filter(|row| !claimed.contains(&row.id));
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

fn owner<'a>(item: &Value, rows: &'a [Download], claimed: &HashSet<i64>) -> Option<&'a Download> {
    let gid = string(item, "gid");
    if gid.is_empty() {
        return None;
    }
    if let Some(row) = unique_unclaimed(
        rows.iter()
            .filter(|row| row.session_gid.as_deref() == Some(&gid)),
        claimed,
    ) {
        return Some(row);
    }
    if let Some(row) = unique_unclaimed(
        rows.iter()
            .filter(|row| row.session_gid.is_none() && row.gid.as_deref() == Some(&gid)),
        claimed,
    ) {
        return Some(row);
    }
    let dir = string(item, "dir");
    let uris = source_uris(item);
    if !uris.is_empty() {
        if let Some(row) = unique_unclaimed(
            rows.iter().filter(|row| {
                row.session_gid.is_none()
                    && row.gid.is_some()
                    && (dir.is_empty() || row.dir == dir)
                    && uris.iter().any(|uri| uri == &row.url)
            }),
            claimed,
        ) {
            return Some(row);
        }
    }
    let info_hash = string(item, "infoHash");
    (!info_hash.is_empty())
        .then(|| {
            unique_unclaimed(
                rows.iter().filter(|row| {
                    row.session_gid.is_none()
                        && row.gid.is_some()
                        && row.info_hash.as_deref() == Some(&info_hash)
                }),
                claimed,
            )
        })
        .flatten()
}

/// Claim only aria2 session entries represented by durable DB rows and remove
/// every invisible leftover while all entries are force-paused.
pub async fn restore_startup_session(engine: &Engine, db: &Db) {
    let _ = engine.rpc.pause_all().await;
    let items = session_items(engine).await;
    let rows: Vec<_> = db
        .resumable_session_rows()
        .unwrap_or_default()
        .into_iter()
        .filter(|row| !row.is_ytdlp())
        .collect();
    let mut claimed = HashSet::new();
    let mut kept = HashSet::new();
    for item in &items {
        let gid = string(item, "gid");
        if let Some(row) = owner(item, &rows, &claimed) {
            if db.bind_aria2_job(row.id, &gid).is_ok() {
                claimed.insert(row.id);
                kept.insert(gid);
            }
        }
    }
    for item in &items {
        let gid = string(item, "gid");
        if gid.is_empty() || kept.contains(&gid) {
            continue;
        }
        match string(item, "status").as_str() {
            "active" => {
                let _ = engine.rpc.force_remove(&gid).await;
            }
            "complete" | "error" | "removed" => {}
            _ => {
                let _ = engine.rpc.remove(&gid).await;
            }
        }
        let _ = engine.rpc.remove_download_result(&gid).await;
    }
    let _ = engine.rpc.save_session().await;
}

pub async fn reconcile(engine: &Engine, db: &Db) {
    let all = session_items(engine).await;
    let live: HashMap<_, _> = all
        .iter()
        .filter_map(|item| {
            let gid = string(item, "gid");
            (!gid.is_empty()).then_some((gid, item))
        })
        .collect();
    for row in db.running_rows().unwrap_or_default() {
        let Some(gid) = row.gid.as_deref() else {
            if row.status.is_running() {
                let _ = db.set_status(row.id, DownloadStatus::Paused);
            }
            continue;
        };
        match live.get(gid) {
            Some(item) => {
                let _ = db.checkpoint_progress(
                    gid,
                    number(item, "completedLength"),
                    number(item, "totalLength"),
                    0,
                    0,
                    0,
                    number(item, "numSeeders"),
                );
                if let Some(status) = DownloadStatus::from_aria2_str(&string(item, "status")) {
                    if status == DownloadStatus::Error {
                        let _ = db.set_error(
                            row.id,
                            item.get("errorCode").and_then(Value::as_str),
                            item.get("errorMessage").and_then(Value::as_str),
                        );
                    } else {
                        let _ = db.set_status(row.id, status);
                    }
                }
            }
            None => {
                let _ = db.set_status(row.id, DownloadStatus::Paused);
            }
        }
    }
}
