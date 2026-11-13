mod commands;
mod downloads;
mod lifecycle;
mod media;
mod settings;

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use minidl_core::aria2::{Aria2Event, EngineDefaults, LaunchOptions, POLL_KEYS, STATUS_KEYS};
use minidl_core::db::{Db, DownloadInsertResult};
use minidl_core::model::{Category, Download, DownloadStatus, NewDownload, Package, Schedule};
use minidl_ipc::{ControlCommand, DownloadKind, MediaOptions, SettingsUpdate};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::{mpsc, Mutex as AsyncMutex};

use crate::engine::{EngineHandle, EngineSupervisor};
use crate::events::EventBus;
use crate::ingest::{self, IngestOutcome};
use crate::ytdlp::{YtDlp, YtDlpCompletion};

const SETTINGS_KEYS: &[&str] = &[
    "auto_organize",
    "prevent_duplicate_downloads",
    "clipboard_watch",
    "close_to_tray",
    "autostart",
    "theme",
    "locale",
    "on_complete_action",
    "on_complete_command_confirmed",
    "proxy",
    "handle_magnets",
    "dht_enabled",
    "sandbox_children",
    "block_private_ips",
    "max_concurrent",
    "split",
    "max_conn",
    "default_speed_limit",
    "global_download_limit",
];

const MAX_METADATA_FILE: u64 = 20 * 1024 * 1024;
const MAX_INPUT_PATH: usize = 32 * 1024;
const MAX_LINK_TEXT: usize = 1024 * 1024;
const MAX_CATEGORY_RULES: usize = 64 * 1024;
const LIVE_POLL_INTERVAL: Duration = Duration::from_millis(500);
const PROGRESS_CHECKPOINT_TICKS: u32 = 10;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadView {
    pub id: i64,
    pub gid: Option<String>,
    pub url: String,
    pub filename: Option<String>,
    pub dir: String,
    pub status: DownloadStatus,
    pub kind: String,
    pub total_bytes: i64,
    pub completed_bytes: i64,
    pub download_speed: i64,
    pub upload_speed: i64,
    pub connections: i64,
    pub num_seeders: i64,
    pub referrer: Option<String>,
    pub info_hash: Option<String>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub category_id: Option<i64>,
    pub created_at: i64,
    pub finished_at: Option<i64>,
    pub page_url: Option<String>,
    pub format_id: Option<String>,
    pub speed_limit: Option<i64>,
    pub package_id: Option<i64>,
    #[serde(default)]
    pub package_name: Option<String>,
    pub mime: Option<String>,
    pub checksum: Option<String>,
    pub start_at: Option<i64>,
    pub media_opts: Option<String>,
    /// The finished file or folder, when it exists on disk.
    #[serde(default)]
    pub path: Option<String>,
    /// `path` is a folder (a multi-file torrent); `entries` counts its items.
    #[serde(default)]
    pub is_dir: bool,
    #[serde(default)]
    pub entries: Option<i64>,
}

impl From<Download> for DownloadView {
    fn from(row: Download) -> Self {
        let path = crate::completion::downloaded_path(&row);
        let is_dir = path.as_deref().is_some_and(Path::is_dir);
        let entries = path
            .as_deref()
            .filter(|_| is_dir)
            .and_then(|path| std::fs::read_dir(path).ok())
            .map(|entries| entries.count().min(i64::MAX as usize) as i64);
        Self {
            path: path.map(|path| path.to_string_lossy().into_owned()),
            is_dir,
            entries,
            id: row.id,
            gid: row.gid,
            url: row.url,
            filename: row.filename,
            dir: row.dir,
            status: row.status,
            kind: row.kind,
            total_bytes: row.total_bytes,
            completed_bytes: row.completed_bytes,
            download_speed: row.download_speed,
            upload_speed: row.upload_speed,
            connections: row.connections,
            num_seeders: row.num_seeders,
            referrer: row.referrer,
            info_hash: row.info_hash,
            error_code: row.error_code,
            error_message: row.error_message,
            category_id: row.category_id,
            created_at: row.created_at,
            finished_at: row.finished_at,
            page_url: row.page_url,
            format_id: row.format_id,
            speed_limit: row.speed_limit,
            package_id: row.package_id,
            package_name: None,
            mime: row.mime,
            checksum: row.checksum,
            start_at: row.start_at,
            media_opts: row.media_opts,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSnapshot {
    pub sequence: u64,
    pub downloads: Vec<DownloadView>,
    pub packages: Vec<Package>,
    pub categories: Vec<Category>,
    pub schedules: Vec<Schedule>,
    pub settings: HashMap<String, String>,
    pub engine_running: bool,
    /// aria2's supervisor state (`ready`, `restarting`, …) while it runs.
    #[serde(default)]
    pub engine_state: Option<Value>,
    #[serde(default = "tray_default")]
    pub tray_available: bool,
}

fn tray_default() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetailFile {
    pub index: i64,
    pub path: String,
    pub length: i64,
    pub completed_length: i64,
    pub selected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetailPeer {
    pub ip: String,
    pub down_speed: i64,
    pub up_speed: i64,
    pub seeder: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadDetails {
    pub id: i64,
    pub url: String,
    pub dir: String,
    pub kind: String,
    pub error_message: Option<String>,
    pub num_pieces: i64,
    pub piece_length: i64,
    pub verified_length: i64,
    pub files: Vec<DetailFile>,
    pub peers: Vec<DetailPeer>,
    pub live: bool,
}

#[derive(Debug, Clone)]
pub struct AppError {
    pub kind: &'static str,
    pub message: String,
}

impl AppError {
    fn classify(message: impl Into<String>) -> Self {
        let message = message.into();
        let lower = message.to_ascii_lowercase();
        let kind = if lower.contains("yt-dlp") && lower.contains("not") {
            "yt-dlp-missing"
        } else if lower.contains("disk space") {
            "disk-space"
        } else if lower.contains("not found") || lower.contains("vanished") {
            "not-found"
        } else if lower.contains("unsupported")
            || lower.contains("unsafe")
            || lower.contains("blocked")
        {
            "rejected"
        } else if lower.contains("aria2")
            || lower.contains("engine")
            || lower.contains("request failed")
        {
            "engine-unavailable"
        } else {
            "other"
        };
        Self { kind, message }
    }
}

impl<E: std::fmt::Display> From<E> for AppError {
    fn from(error: E) -> Self {
        // `{:#}` keeps anyhow's cause chain ("failed to spawn aria2c: …")
        // instead of only the outermost context.
        Self::classify(format!("{error:#}"))
    }
}

pub struct Controller {
    db: Db,
    download_dir: PathBuf,
    defaults: Arc<Mutex<EngineDefaults>>,
    engine: Arc<EngineSupervisor>,
    ytdlp: Arc<YtDlp>,
    events: EventBus,
    initialized_session: AtomicU64,
    engine_init: AsyncMutex<()>,
    clipboard_on: AtomicBool,
    connector_presence: Arc<Mutex<crate::connector::ConnectorPresence>>,
    power_fired: AtomicBool,
    quit: AtomicBool,
    tray_available: AtomicBool,
}

impl Controller {
    pub async fn open_default() -> Result<Arc<Self>, AppError> {
        Self::open(
            minidl_core::paths::data_dir(),
            minidl_core::paths::default_download_dir(),
        )
        .await
    }

    pub async fn open(data_dir: PathBuf, download_dir: PathBuf) -> Result<Arc<Self>, AppError> {
        let db = Db::open(&data_dir)?;
        let u32_setting = |key: &str, fallback: u32, minimum: u32, maximum: u32| {
            let value = db
                .get_setting(key)
                .ok()
                .flatten()
                .and_then(|s| s.parse().ok())
                .unwrap_or(fallback)
                .clamp(minimum, maximum);
            let _ = db.set_setting(key, &value.to_string());
            value
        };
        let defaults = Arc::new(Mutex::new(EngineDefaults {
            split: u32_setting("split", 16, 1, 64),
            max_connection_per_server: u32_setting("max_conn", 16, 1, 16),
            min_split_size: "1M".into(),
        }));
        let options = LaunchOptions {
            aria2c_path: std::env::current_exe()
                .ok()
                .and_then(|path| {
                    path.parent()
                        .map(|dir| dir.join(format!("aria2c{}", std::env::consts::EXE_SUFFIX)))
                })
                .filter(|path| path.is_file()),
            download_dir: download_dir.clone(),
            data_dir,
            max_concurrent: u32_setting("max_concurrent", 5, 1, 20),
            proxy: db
                .get_setting("proxy")
                .ok()
                .flatten()
                .filter(|s| !s.is_empty()),
            dht: db.get_setting("dht_enabled").ok().flatten().as_deref() != Some("false"),
            sandbox: db.get_setting("sandbox_children").ok().flatten().as_deref() == Some("true"),
            download_limit: stored_limit(&db, "global_download_limit"),
            upload_limit: stored_limit(&db, "global_upload_limit"),
        };
        let events = EventBus::default();
        let (ytdlp, ytdlp_completions) =
            YtDlp::resolve(db.clone(), events.clone(), download_dir.clone());
        let controller = Arc::new(Self {
            clipboard_on: AtomicBool::new(
                db.get_setting("clipboard_watch").ok().flatten().as_deref() == Some("true"),
            ),
            db,
            download_dir,
            defaults,
            engine: EngineSupervisor::new(options),
            ytdlp,
            events,
            initialized_session: AtomicU64::new(0),
            engine_init: AsyncMutex::new(()),
            connector_presence: Arc::new(
                Mutex::new(crate::connector::ConnectorPresence::default()),
            ),
            power_fired: AtomicBool::new(false),
            quit: AtomicBool::new(false),
            tray_available: AtomicBool::new(true),
        });
        controller.spawn_ytdlp_completion_handler(ytdlp_completions);
        controller.recover_ytdlp_rows();
        // Only pay the startup/reconciliation cost when durable aria2 state
        // exists. A truly empty controller reaches tray idle without aria2.
        if controller
            .db
            .resumable_session_rows()
            .unwrap_or_default()
            .iter()
            .any(|row| !row.is_ytdlp())
        {
            let _ = controller.ensure_engine().await?;
            controller.arm_engine_if_idle().await;
        }
        Ok(controller)
    }

    pub fn db(&self) -> &Db {
        &self.db
    }
    pub fn events(&self) -> &EventBus {
        &self.events
    }
    pub fn should_quit(&self) -> bool {
        self.quit.load(Ordering::Relaxed)
    }
    pub fn clipboard_enabled(&self) -> bool {
        self.clipboard_on.load(Ordering::Relaxed)
    }
    /// Set by the desktop shell: whether a tray icon exists to bring the
    /// window back after it closes to the background.
    pub fn set_tray_available(&self, available: bool) {
        self.tray_available.store(available, Ordering::Relaxed);
    }

    pub fn record_connector_presence(&self, family: minidl_ipc::BrowserFamily) {
        let status = crate::connector::record(&self.connector_presence, family);
        self.events.emit(
            "connector:status",
            serde_json::to_value(status).unwrap_or(Value::Null),
        );
    }

    pub async fn ingest_capture(
        self: &Arc<Self>,
        job: minidl_ipc::CaptureJob,
        package_id: Option<i64>,
    ) -> Result<i64, AppError> {
        self.power_fired.store(false, Ordering::Relaxed);
        ingest::validate_capture_job(&self.db, &job)
            .await
            .map_err(AppError::classify)?;
        if let Some(id) =
            ingest::reuse_existing_file(&self.db, &self.download_dir, &job, package_id)
                .map_err(AppError::classify)?
        {
            self.events.emit(
                "downloads:state",
                json!({"id": id, "status": "complete", "reused": true}),
            );
            return Ok(id);
        }
        let handle = if matches!(job.kind, DownloadKind::Hls | DownloadKind::Dash) {
            None
        } else {
            Some(self.ensure_engine().await?)
        };
        let defaults = self
            .defaults
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let outcome = ingest::ingest(
            handle.as_ref().map(|handle| handle.engine.as_ref()),
            &self.db,
            &self.ytdlp,
            &self.download_dir,
            defaults,
            job,
            None,
            package_id,
            None,
        )
        .await;
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                self.arm_engine_if_idle().await;
                return Err(AppError::classify(error));
            }
        };
        if matches!(outcome, IngestOutcome::Added(_)) {
            self.events.emit(
                "downloads:state",
                json!({"id": outcome.id(), "status": "active"}),
            );
        }
        Ok(outcome.id())
    }
}

fn stored_limit(db: &Db, key: &str) -> i64 {
    db.get_setting(key)
        .ok()
        .flatten()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0)
        .max(0)
}

fn lookup(db: &Db, gid: &str) -> Option<Download> {
    db.find_by_gid(gid).ok().flatten()
}

fn value<T: Serialize>(value: T) -> Result<Value, AppError> {
    Ok(serde_json::to_value(value)?)
}

fn value_string(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

fn value_i64(value: &Value, key: &str) -> i64 {
    value
        .get(key)
        .and_then(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .or_else(|| value.as_i64().map(|n| n.to_string()))
        })
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
}

fn validate_label(
    name: &str,
    value: &str,
    maximum: usize,
    allow_empty: bool,
) -> Result<(), AppError> {
    if (!allow_empty && value.trim().is_empty()) || value.len() > maximum {
        return Err(AppError::classify(format!("invalid {name}")));
    }
    if value.chars().any(char::is_control) {
        return Err(AppError::classify(format!(
            "{name} contains a forbidden control character"
        )));
    }
    Ok(())
}

fn normalize_checksum(checksum: Option<String>) -> Result<Option<String>, AppError> {
    match checksum
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
    {
        Some(value) if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) => {
            Ok(Some(format!("sha-256={value}")))
        }
        Some(_) => Err(AppError::classify(
            "invalid SHA-256 checksum (expected 64 hex characters)",
        )),
        None => Ok(None),
    }
}

fn normalize_category_rules(rules: &str) -> Result<String, AppError> {
    let rules = rules.trim();
    if rules.starts_with('[') {
        let value: Value = serde_json::from_str(rules)
            .map_err(|_| AppError::classify("invalid category rules JSON"))?;
        if value.is_array() {
            return Ok(value.to_string());
        }
        return Err(AppError::classify("category rules must be a JSON array"));
    }
    let values: Vec<_> = rules
        .split(',')
        .map(|value| value.trim().trim_start_matches('.').to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect();
    Ok(json!([{"match": "ext", "values": values}]).to_string())
}

fn deletion_target(dir: &Path, candidate: &Path) -> Option<PathBuf> {
    let root = dir.canonicalize().ok()?;
    let candidate = if candidate.is_absolute() {
        candidate.to_owned()
    } else {
        dir.join(candidate)
    };
    let name = candidate.file_name()?;
    if name == "." || name == ".." {
        return None;
    }
    let parent = candidate.parent()?.canonicalize().ok()?;
    parent.starts_with(root).then(|| parent.join(name))
}

/// Whether Retry can re-add this kind of download. The bytes of a .torrent or
/// .metalink file are not kept after it was added.
pub fn retryable(kind: &str) -> bool {
    !matches!(kind, "torrent" | "metalink")
}

/// `error_code` of a video row whose yt-dlp process did not survive the app.
pub const INTERRUPTED: &str = "interrupted";

/// Delete one downloaded file (or a torrent's folder) and its aria2 control
/// file, refusing anything that resolves outside the download's own
/// directory. Symlinks are removed, never followed.
fn delete_download_file(dir: &Path, file: &Path) -> Result<(), String> {
    let Some(target) = deletion_target(dir, file) else {
        return Ok(());
    };
    let result = match std::fs::symlink_metadata(&target) {
        Ok(metadata) if metadata.is_dir() => std::fs::remove_dir_all(&target),
        Ok(_) => std::fs::remove_file(&target),
        Err(error) => Err(error),
    };
    let mut sidecar = target.clone().into_os_string();
    sidecar.push(".aria2");
    let _ = std::fs::remove_file(PathBuf::from(sidecar));
    match result {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            Err(format!("{}: {error}", target.display()))
        }
        _ => Ok(()),
    }
}

/// A staging folder this row owns, checked to be a real directory inside
/// `dir` (never a symlink out of it).
fn verified_stage(dir: &Path, stage: &Path) -> Option<PathBuf> {
    let metadata = std::fs::symlink_metadata(stage).ok()?;
    let root = dir.canonicalize().ok()?;
    let canonical = stage.canonicalize().ok()?;
    (metadata.is_dir() && canonical != root && canonical.starts_with(&root)).then_some(canonical)
}

fn delete_stage(dir: &Path, stage: &Path) -> Result<(), String> {
    let Some(stage) = verified_stage(dir, stage) else {
        return Ok(());
    };
    // remove_dir_all does not follow symlinks inside the folder.
    std::fs::remove_dir_all(&stage).map_err(|error| format!("{}: {error}", stage.display()))?;
    if let Some(parent) = stage
        .parent()
        .filter(|parent| parent.file_name() == Some(minidl_core::files::STAGING_DIR.as_ref()))
    {
        let _ = std::fs::remove_dir(parent);
    }
    Ok(())
}

/// Move each partial file out of a removed row's staging folder into `dir` as
/// `<name>.part` (never replacing anything) and drop aria2's control files.
fn keep_partials(dir: &Path, stage: &Path) -> Result<(), String> {
    let Some(stage) = verified_stage(dir, stage) else {
        return Ok(());
    };
    let entries = std::fs::read_dir(&stage).map_err(|error| error.to_string())?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let regular = entry.file_type().is_ok_and(|kind| kind.is_file());
        if !regular {
            continue;
        }
        if name.ends_with(".aria2") || entry.metadata().is_ok_and(|meta| meta.len() == 0) {
            let _ = std::fs::remove_file(entry.path());
            continue;
        }
        let wanted = minidl_core::files::safe_out_name(&format!("{name}.part"))
            .unwrap_or_else(|| "download.part".into());
        let moved = (0..1_000u32).any(|suffix| {
            let candidate = if suffix == 0 {
                wanted.clone()
            } else {
                minidl_core::files::collision_name(&wanted, suffix)
            };
            minidl_core::files::rename_noreplace(&entry.path(), &dir.join(candidate)).is_ok()
        });
        if !moved {
            return Err(format!(
                "the partial download was kept in {}",
                stage.display()
            ));
        }
    }
    Ok(())
}

fn stored_detail_file(row: &Download) -> Option<DetailFile> {
    if matches!(row.kind.as_str(), "torrent" | "magnet") {
        return None;
    }
    let filename = row
        .filename
        .as_deref()
        .map(str::trim)
        .filter(|filename| !filename.is_empty())?;
    let filename = Path::new(filename);
    let path = if filename.is_absolute() {
        filename.to_owned()
    } else {
        Path::new(&row.dir).join(filename)
    };
    Some(DetailFile {
        index: 1,
        path: path.to_string_lossy().into_owned(),
        length: row.total_bytes.max(0),
        completed_length: row.completed_bytes.max(0),
        selected: true,
    })
}

fn spawn_power_command(action: &str, command: Option<&str>) {
    #[cfg(unix)]
    let mut process = match action {
        "sleep" => std::process::Command::new("systemctl"),
        "shutdown" => std::process::Command::new("systemctl"),
        _ => std::process::Command::new("sh"),
    };
    #[cfg(unix)]
    match action {
        "sleep" => {
            process.arg("suspend");
        }
        "shutdown" => {
            process.arg("poweroff");
        }
        _ => {
            process.arg("-c").arg(command.unwrap_or_default());
        }
    }

    #[cfg(windows)]
    let mut process = match action {
        "sleep" => {
            let mut process = std::process::Command::new("rundll32.exe");
            process
                .arg("powrprof.dll,SetSuspendState")
                .args(["0", "1", "0"]);
            process
        }
        "shutdown" => {
            let mut process = std::process::Command::new("shutdown.exe");
            process.args(["/s", "/t", "0"]);
            process
        }
        _ => {
            let mut process = std::process::Command::new("cmd.exe");
            process.arg("/C").arg(command.unwrap_or_default());
            process
        }
    };
    minidl_core::child_process::hide_console_window(&mut process);
    let _ = process.spawn();
}

include!("tests.rs");
