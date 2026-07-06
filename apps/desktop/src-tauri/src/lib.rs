mod engine;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};
use tauri::{Emitter, Manager};

use engine::Aria2;
use ldm_core::aria2::{build_add_options, EngineDefaults};
use ldm_core::ipc::{CaptureJob, DownloadKind};

struct AppState {
    aria2: Arc<Aria2>,
    defaults: EngineDefaults,
    download_dir: PathBuf,
}

/// One progress row pushed to the frontend.
#[derive(Serialize, Clone)]
struct Tick {
    gid: String,
    name: String,
    completed: i64,
    total: i64,
    speed: i64,
    status: String,
}

/// aria2 reports numeric fields as strings — parse defensively.
fn parse_i64(v: &Value, key: &str) -> i64 {
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

#[tauri::command]
async fn add_download(url: String, state: tauri::State<'_, AppState>) -> Result<String, String> {
    let job = CaptureJob {
        url: url.clone(),
        filename: None,
        referrer: None,
        user_agent: None,
        cookie: None,
        extra_headers: vec![],
        kind: DownloadKind::Http,
        mime: None,
        size: None,
        page_url: None,
        cookie_store_id: None,
        torrent_b64: None,
    };
    let opts = build_add_options(&job, &state.download_dir.to_string_lossy(), &state.defaults);
    state
        .aria2
        .add_uri(&url, Value::Object(opts))
        .await
        .map_err(|e| e.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            let download_dir = PathBuf::from(home).join("Downloads");

            let aria2 = Arc::new(Aria2::spawn(&download_dir)?);
            app.manage(AppState {
                aria2: aria2.clone(),
                defaults: EngineDefaults::default(),
                download_dir,
            });

            // 1 Hz progress poller → batched `downloads:tick` events.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                // Wait for aria2's RPC to answer before polling.
                for _ in 0..50 {
                    if aria2.get_version().await.is_ok() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }

                let mut ticker = tokio::time::interval(Duration::from_secs(1));
                loop {
                    ticker.tick().await;
                    if let Ok(items) = aria2.tell_active().await {
                        let updates: Vec<Tick> = items
                            .iter()
                            .map(|it| Tick {
                                gid: it.get("gid").and_then(|g| g.as_str()).unwrap_or("").to_string(),
                                name: basename(it),
                                completed: parse_i64(it, "completedLength"),
                                total: parse_i64(it, "totalLength"),
                                speed: parse_i64(it, "downloadSpeed"),
                                status: it
                                    .get("status")
                                    .and_then(|s| s.as_str())
                                    .unwrap_or("")
                                    .to_string(),
                            })
                            .collect();
                        let _ = handle.emit("downloads:tick", json!({ "updates": updates }));
                    }
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![add_download])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
