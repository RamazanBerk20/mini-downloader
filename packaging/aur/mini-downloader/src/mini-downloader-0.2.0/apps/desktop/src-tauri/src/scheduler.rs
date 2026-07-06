//! App-level scheduler: applies time-window policy (pause/resume all, global
//! speed cap) on top of aria2, which has no wall-clock scheduling of its own.

use std::sync::Arc;
use std::time::Duration;

use chrono::{Datelike, Timelike};
use serde_json::json;
use tauri::{AppHandle, Emitter};

use minidl_core::aria2::Engine;
use minidl_core::db::Db;
use minidl_core::model::Schedule;

pub fn spawn(app: AppHandle, engine: Arc<Engine>, db: Db) {
    tauri::async_runtime::spawn(async move {
        let mut last_minute: i64 = -1;
        loop {
            tokio::time::sleep(Duration::from_secs(20)).await;
            let now = chrono::Local::now();
            let minute = now.hour() as i64 * 60 + now.minute() as i64;
            if minute == last_minute {
                continue; // fire each rule at most once per minute
            }
            last_minute = minute;
            let day_bit = 1i64 << now.weekday().num_days_from_monday();

            for s in db.list_schedules().unwrap_or_default() {
                if s.enabled && (s.days_mask & day_bit) != 0 && s.at_minute == minute {
                    apply(&app, &engine, &s).await;
                }
            }
        }
    });
}

async fn apply(app: &AppHandle, engine: &Engine, s: &Schedule) {
    match s.action.as_str() {
        "pause_all" => {
            let _ = engine.rpc.pause_all().await;
        }
        "resume_all" => {
            let _ = engine.rpc.unpause_all().await;
        }
        "set_speed" => {
            let limit = s.speed_limit.unwrap_or(0);
            let _ = engine
                .rpc
                .change_global_option(json!({ "max-overall-download-limit": limit.to_string() }))
                .await;
        }
        _ => {}
    }
    let _ = app.emit("scheduler:changed", json!({ "action": s.action, "name": s.name }));
}
