mod clipboard;
mod commands;
mod events;
mod ingest;
mod nativehost;
mod scheduler;
mod state;
mod sync;
mod tray;
mod ytdlp;

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use tauri::{Emitter, Manager};

use minidl_core::aria2::{Engine, EngineDefaults, LaunchOptions};
use minidl_core::db::Db;
use minidl_core::paths;

use state::AppState;

fn looks_like_link(s: &str) -> bool {
    let l = s.to_ascii_lowercase();
    l.starts_with("http://") || l.starts_with("https://") || l.starts_with("ftp://") || l.starts_with("magnet:")
}

/// Add a URL asynchronously from a non-command context (deep link, second
/// instance argv).
fn ingest_url(app: &tauri::AppHandle, url: String) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        let defaults = state.defaults.lock().unwrap().clone();
        let _ = ingest::ingest(
            &state.engine,
            &state.db,
            &state.ytdlp,
            &state.download_dir,
            defaults,
            ingest::job_from_url(url),
            None,
        )
        .await;
        let _ = app.emit(events::EV_STATE, serde_json::json!({ "deeplink": true }));
    });
}

fn show_main(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // Single-instance MUST be first. A second launch (native host running
        // `--background`, or a magnet click) forwards its argv here.
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            show_main(app);
            for arg in argv.iter().skip(1) {
                if looks_like_link(arg) {
                    ingest_url(app, arg.clone());
                }
            }
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--minimized"]),
        ))
        .setup(|app| {
            let data_dir = paths::data_dir();
            let download_dir = paths::default_download_dir();

            let db = Db::open(&data_dir).map_err(|e| e.to_string())?;

            // Prefer a bundled aria2c sidecar next to the app, else system PATH.
            let aria2c_path = std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.join("aria2c")))
                .filter(|p| p.is_file());

            let engine = tauri::async_runtime::block_on(Engine::launch(LaunchOptions {
                aria2c_path,
                download_dir: download_dir.clone(),
                data_dir: data_dir.clone(),
                max_concurrent: 5,
            }))
            .map_err(|e| e.to_string())?;
            let engine = Arc::new(engine);
            let split = db
                .get_setting("split")
                .ok()
                .flatten()
                .and_then(|s| s.parse().ok())
                .unwrap_or(16);
            let conns = db
                .get_setting("max_conn")
                .ok()
                .flatten()
                .and_then(|s| s.parse().ok())
                .unwrap_or(16);
            let defaults = Arc::new(Mutex::new(EngineDefaults {
                split,
                max_connection_per_server: conns,
                min_split_size: "1M".into(),
            }));
            let ytdlp = Arc::new(ytdlp::YtDlp::resolve(
                app.handle().clone(),
                db.clone(),
                download_dir.clone(),
            ));

            let clip_enabled = db
                .get_setting("clipboard_watch")
                .ok()
                .flatten()
                .map(|v| v == "true")
                .unwrap_or(false);
            let clipboard_on = Arc::new(AtomicBool::new(clip_enabled));

            app.manage(AppState {
                engine: engine.clone(),
                db: db.clone(),
                ytdlp: ytdlp.clone(),
                clipboard_on: clipboard_on.clone(),
                defaults: defaults.clone(),
                download_dir: download_dir.clone(),
                data_dir,
            });

            // Reconcile against the restored aria2 session, then start live sync.
            let handle = app.handle().clone();
            let eng = engine.clone();
            let db_recon = db.clone();
            tauri::async_runtime::spawn(async move {
                sync::reconcile(&eng, &db_recon).await;
                let _ = handle.emit(events::EV_RECONCILED, ());
            });
            sync::spawn(app.handle().clone(), engine.clone(), db.clone());
            scheduler::spawn(app.handle().clone(), engine.clone(), db.clone());
            clipboard::spawn(app.handle().clone(), clipboard_on);

            // Browser bridge: UDS listener + best-effort manifest install.
            nativehost::spawn_listener(
                app.handle().clone(),
                engine,
                db.clone(),
                ytdlp,
                download_dir,
                defaults,
            );
            match nativehost::install_browser_integration() {
                Ok(paths) => eprintln!("browser integration: {} manifest(s) installed", paths.len()),
                Err(e) => eprintln!("browser integration not installed: {e}"),
            }

            // System tray.
            tray::build(app.handle())?;

            // Deep links (magnet:, minidownloader:).
            {
                use tauri_plugin_deep_link::DeepLinkExt;
                let h = app.handle().clone();
                app.deep_link().on_open_url(move |event| {
                    for url in event.urls() {
                        ingest_url(&h, url.to_string());
                    }
                });
                let _ = app.deep_link().register_all();
            }

            // Close-to-tray: hide instead of quitting (unless disabled).
            if let Some(win) = app.get_webview_window("main") {
                let win2 = win.clone();
                let db2 = db.clone();
                win.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        let close_to_tray = db2
                            .get_setting("close_to_tray")
                            .ok()
                            .flatten()
                            .map(|v| v != "false")
                            .unwrap_or(true);
                        if close_to_tray {
                            api.prevent_close();
                            let _ = win2.hide();
                        }
                    }
                });
            }

            // Start hidden to tray when launched at login / by the native host.
            let background = std::env::args().any(|a| a == "--background" || a == "--minimized");
            if background {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.hide();
                }
            }

            // Handle a URL passed on the initial command line (deep link on cold start).
            for arg in std::env::args().skip(1) {
                if looks_like_link(&arg) {
                    ingest_url(app.handle(), arg);
                }
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::add_download,
            commands::list_downloads,
            commands::pause_download,
            commands::resume_download,
            commands::remove_download,
            commands::pause_all,
            commands::resume_all,
            commands::remove_completed,
            commands::set_global_speed,
            commands::set_download_speed,
            commands::open_containing_folder,
            commands::install_browser_integration,
            commands::add_torrent_file,
            commands::add_metalink_file,
            commands::list_categories,
            commands::save_category,
            commands::delete_category,
            commands::get_setting,
            commands::set_setting,
            commands::probe_media,
            commands::add_media_download,
            commands::grab_links,
            commands::add_links_batch,
            commands::list_schedules,
            commands::save_schedule,
            commands::delete_schedule,
            commands::set_clipboard_watch,
            commands::get_engine_defaults,
            commands::set_engine_defaults,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
