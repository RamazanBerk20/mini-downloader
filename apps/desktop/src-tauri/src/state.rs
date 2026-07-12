use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use minidl_core::aria2::{Engine, EngineDefaults};
use minidl_core::db::Db;

/// Shared application state, managed by Tauri and reachable from every command.
pub struct AppState {
    pub engine: Arc<Engine>,
    pub db: Db,
    pub ytdlp: Arc<crate::ytdlp::YtDlp>,
    pub clipboard_on: Arc<AtomicBool>,
    /// Segment/connection defaults — adjustable at runtime from Settings.
    pub defaults: Arc<Mutex<EngineDefaults>>,
    pub download_dir: PathBuf,
    /// Session-local confirmations received from browser connectors through
    /// the native-messaging bridge.
    pub connector_presence: Arc<Mutex<crate::nativehost::ConnectorPresence>>,
    /// Used by the native-host bridge + packaging (later milestones).
    #[allow(dead_code)]
    pub data_dir: PathBuf,
}
