use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use ldm_core::aria2::{Engine, EngineDefaults};
use ldm_core::db::Db;

/// Shared application state, managed by Tauri and reachable from every command.
pub struct AppState {
    pub engine: Arc<Engine>,
    pub db: Db,
    pub ytdlp: Arc<crate::ytdlp::YtDlp>,
    pub clipboard_on: Arc<AtomicBool>,
    pub defaults: EngineDefaults,
    pub download_dir: PathBuf,
    /// Used by the native-host bridge + packaging (later milestones).
    #[allow(dead_code)]
    pub data_dir: PathBuf,
}
