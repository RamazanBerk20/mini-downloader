mod commands;
mod events;
mod ingest;
mod nativehost;
mod state;
mod sync;
mod ytdlp;

use std::sync::Arc;

use tauri::{Emitter, Manager};

use ldm_core::aria2::{Engine, EngineDefaults, LaunchOptions};
use ldm_core::db::Db;
use ldm_core::paths;

use state::AppState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // Single-instance MUST be registered first. A second launch (e.g. the
        // native host running `ldm-desktop --background`) focuses this instance
        // instead of starting another.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.unminimize();
                let _ = w.set_focus();
            }
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let data_dir = paths::data_dir();
            let download_dir = paths::default_download_dir();

            let db = Db::open(&data_dir).map_err(|e| e.to_string())?;

            let engine = tauri::async_runtime::block_on(Engine::launch(LaunchOptions {
                aria2c_path: None,
                download_dir: download_dir.clone(),
                data_dir: data_dir.clone(),
                max_concurrent: 5,
            }))
            .map_err(|e| e.to_string())?;
            let engine = Arc::new(engine);
            let defaults = EngineDefaults::default();
            let ytdlp = Arc::new(ytdlp::YtDlp::resolve(
                app.handle().clone(),
                db.clone(),
                download_dir.clone(),
            ));

            app.manage(AppState {
                engine: engine.clone(),
                db: db.clone(),
                ytdlp: ytdlp.clone(),
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

            // Browser bridge: listen on the UDS + best-effort manifest install.
            nativehost::spawn_listener(
                app.handle().clone(),
                engine,
                db,
                ytdlp,
                download_dir,
                defaults,
            );
            if let Err(e) = nativehost::install_firefox_manifest() {
                eprintln!("browser integration not installed: {e}");
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
