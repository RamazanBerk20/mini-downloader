//! Safe startup ownership reconciliation shared by every desktop shell.

use std::collections::{HashMap, HashSet};

use minidl_core::aria2::{Engine, EngineDefaults, STATUS_KEYS};
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

/// Text for a failed aria2 job. aria2 reports some failures, a checksum
/// mismatch among them, with a code and an empty message.
pub(crate) fn error_text(item: &Value) -> Option<String> {
    let message = string(item, "errorMessage");
    if !message.trim().is_empty() {
        return Some(message);
    }
    match string(item, "errorCode").as_str() {
        "" => None,
        "32" => Some("checksum verification failed".into()),
        code => Some(format!("aria2 error {code}")),
    }
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

fn live_items(all: &[Value]) -> HashMap<String, &Value> {
    all.iter()
        .filter_map(|item| {
            let gid = string(item, "gid");
            (!gid.is_empty()).then_some((gid, item))
        })
        .collect()
}

/// Bring running rows in line with what aria2 reports after a cold start: a
/// row whose job aria2 does not have is paused (Resume re-adds it).
pub async fn reconcile(engine: &Engine, db: &Db) {
    let all = session_items(engine).await;
    let live = live_items(&all);
    for row in db.running_rows().unwrap_or_default() {
        // yt-dlp jobs have their own lifecycle (`Controller::open`, `YtDlp`):
        // aria2 knowing nothing about them says nothing about their state.
        if row.is_ytdlp() {
            continue;
        }
        let Some(gid) = row.gid.as_deref() else {
            if row.status.is_running() {
                let _ = db.transition(
                    row.id,
                    DownloadStatus::Paused,
                    "reconcile: no engine job was ever bound",
                );
            }
            continue;
        };
        match live.get(gid) {
            Some(item) => sync_row(db, &row, gid, item),
            None => {
                let _ = db.transition(
                    row.id,
                    DownloadStatus::Paused,
                    "reconcile: not in the restored aria2 session",
                );
            }
        }
    }
}

fn sync_row(db: &Db, row: &Download, gid: &str, item: &Value) {
    let _ = db.checkpoint_progress(
        gid,
        number(item, "completedLength"),
        number(item, "totalLength"),
        0,
        0,
        0,
        number(item, "numSeeders"),
    );
    match DownloadStatus::from_aria2_str(&string(item, "status")) {
        Some(DownloadStatus::Error) => {
            let _ = db.fail(
                row.id,
                item.get("errorCode").and_then(Value::as_str),
                error_text(item).as_deref(),
                "reconcile: aria2 reports an error",
            );
        }
        // Finished while nobody was watching: the same completion as live,
        // so a staged file is moved into place.
        Some(DownloadStatus::Complete) => {
            let _ = crate::completion::complete(db, row, item, "reconcile: aria2 reports complete");
        }
        Some(status) => {
            let _ = db.transition(row.id, status, "reconcile: aria2 status");
        }
        None => {}
    }
}

/// After aria2 crashed and was respawned from its last saved session: every
/// row that was running but whose job the new instance does not have is
/// re-added (same row, resuming from its control file), or failed with the
/// reason when it cannot be (torrent and metalink payloads are not kept).
/// Returns the ids whose state changed.
/// `only` limits the pass to one row (a single job aria2 reported missing).
pub async fn recover_lost_jobs(
    engine: &Engine,
    db: &Db,
    defaults: &EngineDefaults,
    only: Option<i64>,
) -> Vec<i64> {
    let all = session_items(engine).await;
    let live = live_items(&all);
    let mut changed = Vec::new();
    for row in db.running_rows().unwrap_or_default() {
        if row.is_ytdlp() || !row.status.is_running() || only.is_some_and(|id| id != row.id) {
            continue;
        }
        if let Some(item) = row.gid.as_deref().and_then(|gid| live.get(gid)) {
            sync_row(db, &row, row.gid.as_deref().unwrap_or_default(), item);
            changed.push(row.id);
            continue;
        }
        let cause = "recover: aria2 restarted without this job";
        if crate::ingest::reissue(engine, db, defaults.clone(), &row, cause)
            .await
            .is_err()
        {
            let _ = db.fail(
                row.id,
                None,
                Some("the download engine restarted and lost this download; retry it"),
                cause,
            );
        }
        changed.push(row.id);
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_without_a_message_still_get_text() {
        let text = |item| error_text(&item);
        assert_eq!(
            text(serde_json::json!({"errorCode": "32", "errorMessage": ""})).as_deref(),
            Some("checksum verification failed")
        );
        assert_eq!(
            text(serde_json::json!({"errorCode": "3", "errorMessage": "Resource not found"}))
                .as_deref(),
            Some("Resource not found")
        );
        assert_eq!(
            text(serde_json::json!({"errorCode": "19"})).as_deref(),
            Some("aria2 error 19")
        );
        assert_eq!(text(serde_json::json!({})), None);
    }
}
