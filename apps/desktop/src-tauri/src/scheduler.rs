//! App-level scheduler: applies time-window policy (pause/resume all, global
//! speed cap) on top of aria2, which has no wall-clock scheduling of its own.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{Datelike, Timelike};
use serde_json::json;
use tauri::{AppHandle, Emitter, Manager};

use minidl_core::aria2::Engine;
use minidl_core::db::Db;
use minidl_core::model::Schedule;

use crate::events::EV_STATE;
use crate::state::AppState;

pub fn spawn(app: AppHandle, engine: Arc<Engine>, db: Db) {
    tauri::async_runtime::spawn(async move {
        // Fire any rule whose minute falls in the window since the last check,
        // not only an exact `== minute` sample. A suspended laptop can skip the
        // exact minute entirely; on wake the gap is large and we still fire the
        // rules that were due. `fired` (per rule id → ordinal date) prevents a
        // double-fire within the same day.
        let mut last_min: i64 = -1;
        let mut last_day: i32 = 0;
        let mut fired: HashMap<i64, i32> = HashMap::new();
        loop {
            tokio::time::sleep(Duration::from_secs(20)).await;
            let now = chrono::Local::now();
            let now_min = now.hour() as i64 * 60 + now.minute() as i64;
            let epoch_day = now.num_days_from_ce();
            let day_bit = 1i64 << now.weekday().num_days_from_monday();

            if last_min < 0 {
                // First tick: establish a baseline, don't retroactively fire.
                last_min = now_min;
                last_day = epoch_day;
                continue;
            }

            let same_day = epoch_day == last_day && now_min >= last_min;
            for s in db.list_schedules().unwrap_or_default() {
                if !s.enabled || (s.days_mask & day_bit) == 0 {
                    continue;
                }
                let am = s.at_minute;
                // Minute fell inside (last_min, now_min], handling a midnight/day
                // wrap (or a long suspend across midnight) as a split window.
                let passed = if same_day {
                    am > last_min && am <= now_min
                } else {
                    am > last_min || am <= now_min
                };
                if passed && fired.get(&s.id) != Some(&epoch_day) {
                    apply(&app, &engine, &s).await;
                    fired.insert(s.id, epoch_day);
                }
            }
            // Keep the guard map from growing: only today's fires matter.
            fired.retain(|_, d| *d == epoch_day);
            last_min = now_min;
            last_day = epoch_day;

            // Per-download scheduled starts: `start_at <= now` fires even after
            // a long suspend. Clear start_at first so a failing resume doesn't
            // re-fire every tick.
            let due = db.due_scheduled(minidl_core::model::now()).unwrap_or_default();
            if !due.is_empty() {
                if let Some(state) = app.try_state::<AppState>() {
                    for row in due {
                        let _ = db.set_start_at(row.id, None);
                        match crate::commands::resume_row(&state, row.id).await {
                            Ok(()) => {
                                let _ = app.emit(EV_STATE, json!({ "id": row.id, "status": "active" }));
                            }
                            Err(e) => {
                                let _ = db.set_error(row.id, None, Some(&e.to_string()));
                                let _ = app.emit(EV_STATE, json!({ "id": row.id, "status": "error" }));
                            }
                        }
                    }
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
