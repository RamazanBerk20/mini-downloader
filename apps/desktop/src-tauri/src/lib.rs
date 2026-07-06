mod commands;
mod events;
mod state;
mod sync;

use std::sync::Arc;

use tauri::{Emitter, Manager};

use ldm_core::aria2::{Engine, EngineDefaults, LaunchOptions};
use ldm_core::db::Db;
use ldm_core::paths;

use state::AppState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
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

            app.manage(AppState {
                engine: engine.clone(),
                db: db.clone(),
                defaults: EngineDefaults::default(),
                download_dir,
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
            sync::spawn(app.handle().clone(), engine, db);

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
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
