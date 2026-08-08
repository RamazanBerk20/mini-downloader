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
}

impl From<Download> for DownloadView {
    fn from(row: Download) -> Self {
        Self {
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
        Self::classify(error.to_string())
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
        });
        controller.spawn_ytdlp_completion_handler(ytdlp_completions);
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

    async fn ensure_engine(self: &Arc<Self>) -> Result<EngineHandle, AppError> {
        let _guard = self.engine_init.lock().await;
        let handle = self.engine.get().await?;
        if self.initialized_session.load(Ordering::Acquire) != handle.id {
            crate::reconcile::restore_startup_session(&handle.engine, &self.db).await;
            crate::reconcile::reconcile(&handle.engine, &self.db).await;
            self.initialized_session.store(handle.id, Ordering::Release);
            self.events.emit("downloads:reconciled", json!({}));
            self.spawn_sync(handle.clone());
        }
        Ok(handle)
    }

    fn spawn_sync(self: &Arc<Self>, handle: EngineHandle) {
        let activity = Arc::new(tokio::sync::Notify::new());
        let this = self.clone();
        let notification_handle = handle.clone();
        let notification_activity = activity.clone();
        tokio::spawn(async move {
            let mut stop = notification_handle.stop.clone();
            let mut notifications = notification_handle.engine.subscribe();
            loop {
                tokio::select! {
                    _ = stop.changed() => break,
                    event = notifications.recv() => match event {
                        Ok(event) => {
                            if matches!(event, Aria2Event::Start(_)) {
                                this.power_fired.store(false, Ordering::Relaxed);
                                this.engine.cancel_idle_shutdown();
                            }
                            this.handle_transition(&notification_handle, event.gid()).await;
                            notification_activity.notify_one();
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        });
        let this = self.clone();
        tokio::spawn(async move {
            let mut stop = handle.stop.clone();
            let mut known = HashSet::new();
            let mut checkpoint_tick = 0u32;
            loop {
                let active = handle
                    .engine
                    .rpc
                    .tell_active(POLL_KEYS)
                    .await
                    .unwrap_or_default();
                checkpoint_tick = checkpoint_tick.wrapping_add(1);
                let checkpoint = checkpoint_tick.is_multiple_of(PROGRESS_CHECKPOINT_TICKS);
                let mut current = HashSet::with_capacity(active.len());
                let mut ticks = Vec::with_capacity(active.len());
                for item in &active {
                    let gid = value_string(item, "gid");
                    if gid.is_empty() {
                        continue;
                    }
                    current.insert(gid.clone());
                    let Some(row) = self::lookup(&this.db, &gid) else {
                        continue;
                    };
                    let completed = value_i64(item, "completedLength");
                    let total = value_i64(item, "totalLength");
                    let download_speed = value_i64(item, "downloadSpeed");
                    let upload_speed = value_i64(item, "uploadSpeed");
                    let connections = value_i64(item, "connections");
                    let seeders = value_i64(item, "numSeeders");
                    if checkpoint {
                        let _ = this.db.checkpoint_progress(
                            &gid,
                            completed,
                            total,
                            download_speed,
                            upload_speed,
                            connections,
                            seeders,
                        );
                    }
                    ticks.push(json!({
                        "id": row.id, "gid": gid, "name": "", "completed": completed,
                        "total": total, "dl_speed": download_speed, "ul_speed": upload_speed,
                        "connections": connections, "num_seeders": seeders, "status": "active"
                    }));
                }
                if !ticks.is_empty() {
                    this.events
                        .emit("downloads:tick", json!({"updates": ticks}));
                }
                let transitioned: Vec<_> = known.difference(&current).cloned().collect();
                for gid in transitioned {
                    this.handle_transition(&handle, &gid).await;
                }
                known = current;
                let waiting = handle
                    .engine
                    .rpc
                    .tell_waiting(0, 1, &["gid"])
                    .await
                    .unwrap_or_default();
                if active.is_empty() && waiting.is_empty() {
                    this.engine.arm_idle_shutdown();
                    tokio::select! {
                        _ = stop.changed() => break,
                        _ = activity.notified() => {
                            this.engine.cancel_idle_shutdown();
                            continue;
                        }
                    }
                }
                tokio::select! {
                    _ = stop.changed() => break,
                    _ = tokio::time::sleep(LIVE_POLL_INTERVAL) => {}
                }
            }
        });
    }

    fn spawn_ytdlp_completion_handler(
        self: &Arc<Self>,
        mut completions: mpsc::UnboundedReceiver<YtDlpCompletion>,
    ) {
        let controller = Arc::downgrade(self);
        tokio::spawn(async move {
            while let Some(completion) = completions.recv().await {
                let Some(controller) = controller.upgrade() else {
                    return;
                };
                controller.handle_ytdlp_completion(completion).await;
            }
        });
    }

    async fn handle_ytdlp_completion(&self, completion: YtDlpCompletion) {
        let Some(row) = self.db.get(completion.id).ok().flatten() else {
            return;
        };
        let filename = completion
            .name
            .or_else(|| row.filename.clone())
            .unwrap_or_default();
        let path = completion.path.unwrap_or_else(|| {
            Path::new(&row.dir)
                .join(&filename)
                .to_string_lossy()
                .into_owned()
        });
        crate::completion::set_origin(Path::new(&path), &row.url, row.referrer.as_deref());
        self.events.emit(
            "downloads:complete",
            json!({"id": row.id, "name": filename, "path": path}),
        );
        self.events.emit(
            "downloads:state",
            json!({"id": row.id, "status": "complete"}),
        );
        self.maybe_completion_action().await;
    }

    async fn handle_transition(&self, handle: &EngineHandle, gid: &str) {
        let Ok(status) = handle.engine.rpc.tell_status(gid, STATUS_KEYS).await else {
            return;
        };
        let Some(row) = self.db.find_by_gid(gid).ok().flatten() else {
            let _ = handle.engine.rpc.remove_download_result(gid).await;
            self.arm_engine_if_idle().await;
            return;
        };
        let _ = self.db.checkpoint_progress(
            gid,
            value_i64(&status, "completedLength"),
            value_i64(&status, "totalLength"),
            value_i64(&status, "downloadSpeed"),
            value_i64(&status, "uploadSpeed"),
            value_i64(&status, "connections"),
            value_i64(&status, "numSeeders"),
        );
        let Some(next) = DownloadStatus::from_aria2_str(&value_string(&status, "status")) else {
            return;
        };
        if row.status == DownloadStatus::Scheduled
            && matches!(next, DownloadStatus::Paused | DownloadStatus::Waiting)
        {
            self.arm_engine_if_idle().await;
            return;
        }
        match next {
            DownloadStatus::Complete => {
                if let Some(child) = crate::completion::first_followed_by(&status) {
                    let _ = self.db.set_gid(row.id, &child);
                    if let Some(info_hash) = status
                        .get("infoHash")
                        .and_then(Value::as_str)
                        .filter(|value| !value.is_empty())
                    {
                        let _ = self.db.set_info_hash(row.id, info_hash);
                    }
                    let _ = self.db.set_status(row.id, DownloadStatus::Active);
                    self.events.emit(
                        "downloads:state",
                        json!({"id": row.id, "gid": child, "status": "active"}),
                    );
                    let _ = handle.engine.rpc.remove_download_result(gid).await;
                    return;
                }
                if self.db.set_status_if_changed(row.id, next).unwrap_or(false) {
                    let reported = crate::completion::basename(&status);
                    let filename = if reported.is_empty() {
                        row.filename.clone().unwrap_or_default()
                    } else {
                        reported
                    };
                    let (directory, filename) =
                        crate::completion::organize(&self.db, &row, &filename);
                    if !filename.is_empty() && row.filename.as_deref() != Some(filename.as_str()) {
                        let _ = self.db.set_filename(row.id, &filename);
                    }
                    let path = Path::new(&directory).join(&filename);
                    crate::completion::set_origin(&path, &row.url, row.referrer.as_deref());
                    self.events.emit(
                        "downloads:complete",
                        json!({
                            "id": row.id, "name": filename, "path": path.to_string_lossy()
                        }),
                    );
                    self.maybe_completion_action().await;
                }
            }
            DownloadStatus::Error => {
                if self.db.set_status_if_changed(row.id, next).unwrap_or(false) {
                    let message = status.get("errorMessage").and_then(Value::as_str);
                    let _ = self.db.set_error(
                        row.id,
                        status.get("errorCode").and_then(Value::as_str),
                        message,
                    );
                    self.events
                        .emit("downloads:error", json!({"id": row.id, "message": message}));
                }
            }
            _ => {
                let _ = self.db.set_status(row.id, next);
            }
        }
        self.events.emit(
            "downloads:state",
            json!({"id": row.id, "status": next.as_str()}),
        );
        if next.is_terminal() {
            let _ = handle.engine.rpc.remove_download_result(gid).await;
        }
        if !next.is_running() {
            self.arm_engine_if_idle().await;
        }
    }

    async fn maybe_completion_action(&self) {
        let action = self
            .db
            .get_setting("on_complete_action")
            .ok()
            .flatten()
            .unwrap_or_default();
        if action.is_empty() || action == "none" {
            return;
        }
        if let Some(engine) = self.engine.current().await {
            if let Ok(stats) = engine.rpc.get_global_stat().await {
                if value_i64(&stats, "numActive") + value_i64(&stats, "numWaiting") > 0 {
                    return;
                }
            }
        }
        if self.ytdlp.is_running() {
            return;
        }
        if self.db.has_unfinished_downloads().unwrap_or(true) {
            return;
        }
        if action.starts_with("run:")
            && self
                .db
                .get_setting("on_complete_command_confirmed")
                .ok()
                .flatten()
                .as_deref()
                != Some("true")
        {
            return;
        }
        if self.power_fired.swap(true, Ordering::SeqCst) {
            return;
        }
        match action.as_str() {
            "quit" => {
                self.quit.store(true, Ordering::Relaxed);
                self.ytdlp.cancel_all();
                self.engine.shutdown().await;
                self.events.emit("app:quit", json!({}));
            }
            "sleep" => spawn_power_command("sleep", None),
            "shutdown" => spawn_power_command("shutdown", None),
            value => {
                if let Some(command) = value.strip_prefix("run:") {
                    spawn_power_command("run", Some(command));
                }
            }
        }
    }

    async fn arm_engine_if_idle(&self) {
        if !self
            .db
            .running_rows()
            .unwrap_or_default()
            .iter()
            .any(|row| !row.is_ytdlp() && row.status.is_running())
        {
            self.engine.arm_idle_shutdown();
        }
    }

    pub async fn snapshot(&self) -> Result<AppSnapshot, AppError> {
        let mut settings = HashMap::new();
        for key in SETTINGS_KEYS {
            if let Some(value) = self.db.get_setting(key)? {
                settings.insert((*key).into(), value);
            }
        }
        let packages = self.db.list_packages()?;
        let package_names = packages
            .iter()
            .map(|package| (package.id, package.name.clone()))
            .collect::<HashMap<_, _>>();
        let downloads = self
            .db
            .list(None)?
            .into_iter()
            .map(|download| {
                let mut view = DownloadView::from(download);
                view.package_name = view
                    .package_id
                    .and_then(|id| package_names.get(&id).cloned());
                view
            })
            .collect();
        Ok(AppSnapshot {
            sequence: self.events.sequence(),
            downloads,
            packages,
            categories: self.db.list_categories()?,
            schedules: self.db.list_schedules()?,
            settings,
            engine_running: self.engine.is_running().await,
        })
    }

    pub async fn execute(self: &Arc<Self>, command: ControlCommand) -> Result<Value, AppError> {
        use ControlCommand::*;
        match command {
            Ping => Ok(json!({"version": env!("CARGO_PKG_VERSION")})),
            Show => {
                self.events.emit("ui:show", json!({}));
                Ok(Value::Null)
            }
            Hide => {
                self.events.emit("ui:hide", json!({}));
                Ok(Value::Null)
            }
            Quit => {
                self.quit.store(true, Ordering::Relaxed);
                self.ytdlp.cancel_all();
                self.engine.shutdown().await;
                self.events.emit("app:quit", json!({}));
                Ok(Value::Null)
            }
            Snapshot => value(self.snapshot().await?),
            Subscribe => Err(AppError::classify(
                "subscribe must be the first command on an event connection",
            )),
            IngestUrl { url, checksum } => {
                self.power_fired.store(false, Ordering::Relaxed);
                let url = url.trim().to_owned();
                if url.is_empty() {
                    return Err(AppError::classify("empty URL"));
                }
                let checksum = normalize_checksum(checksum)?;
                let job = ingest::job_from_url(url);
                ingest::validate_capture_job(&self.db, &job)
                    .await
                    .map_err(AppError::classify)?;
                let handle = self.ensure_engine().await?;
                let defaults = self
                    .defaults
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                let outcome = ingest::ingest(
                    Some(&handle.engine),
                    &self.db,
                    &self.ytdlp,
                    &self.download_dir,
                    defaults,
                    job,
                    None,
                    None,
                    checksum,
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
                value(self.view(outcome.id())?)
            }
            IngestFile { path, kind } => {
                self.power_fired.store(false, Ordering::Relaxed);
                if !matches!(kind, DownloadKind::Torrent | DownloadKind::Metalink) {
                    return Err(AppError::classify("unsupported file download type"));
                }
                validate_label("file path", &path, MAX_INPUT_PATH, false)?;
                let metadata = std::fs::metadata(&path)?;
                if !metadata.is_file() {
                    return Err(AppError::classify(
                        "selected torrent or metalink is not a regular file",
                    ));
                }
                if metadata.len() > MAX_METADATA_FILE {
                    return Err(AppError::classify(
                        "torrent or metalink file exceeds 20 MiB",
                    ));
                }
                let mut file = std::fs::File::open(&path)?;
                if !file.metadata()?.is_file() {
                    return Err(AppError::classify(
                        "selected torrent or metalink is not a regular file",
                    ));
                }
                let mut bytes = Vec::new();
                file.by_ref()
                    .take(MAX_METADATA_FILE + 1)
                    .read_to_end(&mut bytes)?;
                if bytes.len() as u64 > MAX_METADATA_FILE {
                    return Err(AppError::classify(
                        "torrent or metalink file exceeds 20 MiB",
                    ));
                }
                let name = Path::new(&path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("download");
                let mut job = ingest::job_from_url(format!("file:{name}"));
                job.kind = kind;
                job.torrent_b64 = Some(base64::engine::general_purpose::STANDARD.encode(bytes));
                ingest::validate_capture_job(&self.db, &job)
                    .await
                    .map_err(AppError::classify)?;
                let handle = self.ensure_engine().await?;
                let defaults = self
                    .defaults
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                let outcome = ingest::ingest(
                    Some(&handle.engine),
                    &self.db,
                    &self.ytdlp,
                    &self.download_dir,
                    defaults,
                    job,
                    None,
                    None,
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
                self.events.emit(
                    "downloads:state",
                    json!({"id": outcome.id(), "status": "active"}),
                );
                value(self.view(outcome.id())?)
            }
            ListDownloads { status } => value(
                self.db
                    .list(status.as_deref())?
                    .into_iter()
                    .map(DownloadView::from)
                    .collect::<Vec<_>>(),
            ),
            Pause { id } => {
                self.pause(id).await?;
                Ok(Value::Null)
            }
            Resume { id } => {
                self.resume(id).await?;
                Ok(Value::Null)
            }
            Retry { id } => {
                self.retry(id).await?;
                value(self.view(id)?)
            }
            Remove { id, delete_files } => {
                self.remove(id, delete_files).await?;
                Ok(Value::Null)
            }
            RemoveCompleted => value(self.remove_status("complete").await?),
            RemoveFailed => value(self.remove_status("error").await?),
            PauseAll => {
                if let Some(engine) = self.engine.current().await {
                    let _ = engine.rpc.pause_all().await;
                }
                for row in self.db.running_rows()? {
                    if row.status.is_running() {
                        let _ = self.db.set_status(row.id, DownloadStatus::Paused);
                    }
                }
                self.events
                    .emit("downloads:state", json!({"batch": "paused"}));
                self.arm_engine_if_idle().await;
                Ok(Value::Null)
            }
            ResumeAll => {
                let ids: Vec<_> = self
                    .db
                    .running_rows()?
                    .into_iter()
                    .filter(|row| row.status == DownloadStatus::Paused)
                    .map(|row| row.id)
                    .collect();
                for id in ids {
                    self.resume(id).await?;
                }
                Ok(Value::Null)
            }
            MoveInQueue { id, direction } => {
                if let Some(row) = self.db.get(id)? {
                    if let (Some(gid), Some(engine)) = (row.gid, self.engine.current().await) {
                        let (position, how) = match direction.as_str() {
                            "top" => (0, "POS_SET"),
                            "up" => (-1, "POS_CUR"),
                            "down" => (1, "POS_CUR"),
                            "bottom" => (1_000_000, "POS_SET"),
                            _ => return Ok(Value::Null),
                        };
                        let _ = engine.rpc.change_position(&gid, position, how).await;
                    }
                }
                Ok(Value::Null)
            }
            SetQueuePosition { id, position } => {
                if let Some(row) = self.db.get(id)? {
                    if let (Some(gid), Some(engine)) = (row.gid, self.engine.current().await) {
                        let _ = engine
                            .rpc
                            .change_position(&gid, position.max(0), "POS_SET")
                            .await;
                    }
                }
                Ok(Value::Null)
            }
            SetGlobalSpeed { download, upload } => {
                let download = download.max(0);
                let upload = upload.max(0);
                let handle = self.ensure_engine().await?;
                handle
                    .engine
                    .rpc
                    .change_global_option(json!({
                        "max-overall-download-limit": download.to_string(),
                        "max-overall-upload-limit": upload.to_string(),
                    }))
                    .await?;
                self.arm_engine_if_idle().await;
                Ok(Value::Null)
            }
            SetDownloadSpeed { id, limit } => {
                let limit = limit.max(0);
                self.db.set_speed_limit(id, (limit > 0).then_some(limit))?;
                if let Some(row) = self.db.get(id)? {
                    if let (Some(gid), Some(engine)) = (row.gid, self.engine.current().await) {
                        engine
                            .rpc
                            .change_option(&gid, json!({"max-download-limit": limit.to_string()}))
                            .await?;
                    }
                }
                Ok(Value::Null)
            }
            OpenFile { id } => {
                let row = self
                    .db
                    .get(id)?
                    .ok_or_else(|| AppError::classify("download not found"))?;
                let path = crate::completion::downloaded_path(&row)
                    .ok_or_else(|| AppError::classify("downloaded file is unavailable"))?;
                open::that(path)?;
                Ok(Value::Null)
            }
            OpenContainingFolder { id } => {
                let row = self
                    .db
                    .get(id)?
                    .ok_or_else(|| AppError::classify("download not found"))?;
                open::that(row.dir)?;
                Ok(Value::Null)
            }
            ProbeMedia { url, playlist } => {
                let url = url.trim().to_owned();
                ingest::validate_media_target(&self.db, &url)
                    .await
                    .map_err(AppError::classify)?;
                value(
                    self.ytdlp
                        .probe(&url, &[], playlist)
                        .await
                        .map_err(AppError::classify)?,
                )
            }
            AddMedia {
                url,
                format_id,
                options,
            } => value(self.add_media(url, format_id, options, None).await?),
            AddPlaylist {
                entries,
                package_name,
                quality,
                options,
            } => {
                if entries.len() > ingest::MAX_BATCH_ITEMS {
                    return Err(AppError::classify("playlist exceeds 5000 entries"));
                }
                validate_label("package name", &package_name, 256, false)?;
                crate::ytdlp::validate_options(quality.as_deref(), options.as_ref())
                    .map_err(AppError::classify)?;
                let package = (!entries.is_empty())
                    .then(|| self.db.insert_package(package_name.trim(), None, None))
                    .transpose()?;
                let mut added = 0usize;
                for entry in entries {
                    if self
                        .add_media(entry.url, quality.clone(), options.clone(), package)
                        .await
                        .is_ok()
                    {
                        added += 1;
                    }
                }
                if let Some(package) = package {
                    let _ = self.db.delete_package_if_empty(package);
                }
                self.events.emit("downloads:state", json!({"batch": added}));
                value(added)
            }
            GrabLinks { text } => {
                if text.len() > MAX_LINK_TEXT {
                    return Err(AppError::classify("link text exceeds 1 MiB"));
                }
                value(minidl_core::grabber::parse_links(&text))
            }
            AddLinks { urls, package_name } => {
                self.power_fired.store(false, Ordering::Relaxed);
                if urls.len() > ingest::MAX_BATCH_ITEMS {
                    return Err(AppError::classify("batch exceeds 5000 URLs"));
                }
                if let Some(name) = package_name.as_deref() {
                    validate_label("package name", name, 256, true)?;
                }
                let mut seen = HashSet::new();
                let mut urls = urls
                    .into_iter()
                    .map(|url| url.trim().to_owned())
                    .filter(|url| !url.is_empty())
                    .filter(|url| seen.insert(url.clone()))
                    .collect::<Vec<_>>();
                for url in &urls {
                    let job = ingest::job_from_url(url.clone());
                    ingest::validate_capture_job(&self.db, &job)
                        .await
                        .map_err(AppError::classify)?;
                }
                if urls.is_empty() {
                    return value(0usize);
                }
                let package = if urls.len() > 1 {
                    let name = package_name
                        .filter(|name| !name.trim().is_empty())
                        .unwrap_or_else(|| minidl_core::grabber::host_of(&urls[0]));
                    Some(self.db.insert_package(
                        if name.is_empty() { "Batch" } else { &name },
                        None,
                        None,
                    )?)
                } else {
                    None
                };
                let handle = self.ensure_engine().await?;
                let defaults = self
                    .defaults
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                let mut added = 0usize;
                for url in urls.drain(..) {
                    if matches!(
                        ingest::ingest(
                            Some(&handle.engine),
                            &self.db,
                            &self.ytdlp,
                            &self.download_dir,
                            defaults.clone(),
                            ingest::job_from_url(url),
                            None,
                            package,
                            None
                        )
                        .await,
                        Ok(IngestOutcome::Added(_))
                    ) {
                        added += 1;
                    }
                }
                if let Some(package) = package {
                    let _ = self.db.delete_package_if_empty(package);
                }
                self.arm_engine_if_idle().await;
                self.events.emit("downloads:state", json!({"batch": added}));
                value(added)
            }
            ListPackages => value(self.db.list_packages()?),
            ListCategories => value(self.db.list_categories()?),
            SaveCategory {
                name,
                dir,
                rules,
                priority,
            } => {
                validate_label("category name", &name, 128, false)?;
                validate_label("category directory", &dir, 4_096, false)?;
                if rules.len() > MAX_CATEGORY_RULES {
                    return Err(AppError::classify("category rules exceed 64 KiB"));
                }
                let rules = normalize_category_rules(&rules)?;
                self.db
                    .upsert_category(name.trim(), dir.trim(), &rules, priority)?;
                self.events.emit("categories:changed", json!({}));
                Ok(Value::Null)
            }
            MoveCategory { id, direction } => {
                self.db.move_category(id, direction)?;
                self.events.emit("categories:changed", json!({}));
                Ok(Value::Null)
            }
            DeleteCategory { id } => {
                self.db.delete_category(id)?;
                self.events.emit("categories:changed", json!({}));
                Ok(Value::Null)
            }
            RestoreDefaultCategories => {
                self.db.seed_default_categories()?;
                self.events.emit("categories:changed", json!({}));
                value(self.db.list_categories()?)
            }
            ResetCategoryDir { id } => {
                self.db.reset_category_dir(id)?;
                self.events.emit("categories:changed", json!({}));
                Ok(Value::Null)
            }
            GetDownloadDetails { id } => value(self.details(id).await?),
            SetTorrentFiles { id, indices } => {
                self.set_torrent_files(id, indices).await?;
                Ok(Value::Null)
            }
            ScheduleDownload { id, start_at } => {
                self.schedule(id, start_at).await?;
                Ok(Value::Null)
            }
            ListSchedules => value(self.db.list_schedules()?),
            SaveSchedule {
                id,
                name,
                action,
                days_mask,
                at_minute,
                speed_limit,
                enabled,
            } => {
                if let Some(name) = name.as_deref() {
                    validate_label("schedule name", name, 128, true)?;
                }
                if !matches!(action.as_str(), "pause_all" | "resume_all" | "set_speed") {
                    return Err(AppError::classify("unsupported schedule action"));
                }
                if !(0..=0x7f).contains(&days_mask) {
                    return Err(AppError::classify("invalid schedule day mask"));
                }
                if !(0..=1_439).contains(&at_minute) {
                    return Err(AppError::classify("invalid schedule time"));
                }
                if speed_limit.is_some_and(|limit| limit < 0) {
                    return Err(AppError::classify("invalid schedule speed limit"));
                }
                self.db.save_schedule(
                    id,
                    name.as_deref(),
                    &action,
                    days_mask,
                    at_minute,
                    speed_limit,
                    enabled,
                )?;
                self.events.emit("scheduler:changed", json!({}));
                Ok(Value::Null)
            }
            DeleteSchedule { id } => {
                self.db.delete_schedule(id)?;
                self.events.emit("scheduler:changed", json!({}));
                Ok(Value::Null)
            }
            GetSetting { key } => value(self.db.get_setting(&key)?),
            SetSetting { key: _, value: _ } => Err(AppError::classify(
                "individual setting writes are unsupported; use apply_settings",
            )),
            GetSystemLocale => value(sys_locale::get_locales().find(|locale| {
                !matches!(
                    locale.trim().to_ascii_lowercase().as_str(),
                    "" | "c" | "posix" | "und"
                )
            })),
            GetConnectorStatus => value(crate::connector::status(&self.connector_presence)),
            SetHandleMagnets { enabled } => {
                crate::platform::set_magnet_handler(enabled).map_err(AppError::classify)?;
                self.db
                    .set_setting("handle_magnets", if enabled { "true" } else { "false" })?;
                Ok(Value::Null)
            }
            SetClipboardWatch { enabled } => {
                self.clipboard_on.store(enabled, Ordering::Relaxed);
                self.db
                    .set_setting("clipboard_watch", if enabled { "true" } else { "false" })?;
                self.events
                    .emit("settings:clipboard", json!({"enabled": enabled}));
                Ok(Value::Null)
            }
            GetEngineDefaults => {
                let defaults = self.defaults.lock().unwrap_or_else(|e| e.into_inner());
                value((defaults.split, defaults.max_connection_per_server))
            }
            SetEngineDefaults { split, connections } => {
                let split = split.clamp(1, 64);
                let connections = connections.clamp(1, 16);
                {
                    let mut defaults = self.defaults.lock().unwrap_or_else(|e| e.into_inner());
                    defaults.split = split;
                    defaults.max_connection_per_server = connections;
                }
                self.db.set_setting("split", &split.to_string())?;
                self.db.set_setting("max_conn", &connections.to_string())?;
                Ok(Value::Null)
            }
            GetMaxConcurrent => value(
                self.db
                    .get_setting("max_concurrent")?
                    .and_then(|value| value.parse::<u32>().ok())
                    .unwrap_or(5),
            ),
            SetMaxConcurrent { value: maximum } => {
                let maximum = maximum.clamp(1, 20);
                self.db
                    .set_setting("max_concurrent", &maximum.to_string())?;
                self.ytdlp.set_max_concurrent(maximum as usize);
                self.engine.update_max_concurrent(maximum).await;
                if let Some(engine) = self.engine.current().await {
                    let _ = engine
                        .rpc
                        .change_global_option(
                            json!({"max-concurrent-downloads": maximum.to_string()}),
                        )
                        .await;
                }
                Ok(Value::Null)
            }
            ApplyProxy { value: proxy } => {
                let proxy = proxy.trim().to_owned();
                validate_label("proxy", &proxy, 4_096, true)?;
                let old_proxy = self.db.get_setting("proxy")?.unwrap_or_default();
                let engine = self.engine.current().await;
                if let Some(engine) = &engine {
                    engine
                        .rpc
                        .change_global_option(json!({"all-proxy": proxy.clone()}))
                        .await?;
                }
                if let Err(error) = self.db.set_setting("proxy", &proxy) {
                    if let Some(engine) = &engine {
                        let _ = engine
                            .rpc
                            .change_global_option(json!({"all-proxy": old_proxy}))
                            .await;
                    }
                    return Err(error.into());
                }
                self.engine
                    .update_proxy((!proxy.is_empty()).then_some(proxy))
                    .await;
                Ok(Value::Null)
            }
            ApplySettings { settings } => {
                self.apply_settings(settings).await?;
                Ok(Value::Null)
            }
            CheckUpdate => value(crate::updater::check().await.map_err(AppError::classify)?),
            InstallUpdate {
                asset_url,
                page_url,
            } => {
                if crate::updater::install(asset_url, page_url)
                    .await
                    .map_err(AppError::classify)?
                {
                    self.quit.store(true, Ordering::Relaxed);
                    self.ytdlp.cancel_all();
                    self.engine.shutdown().await;
                    self.events.emit("app:quit", json!({}));
                }
                Ok(Value::Null)
            }
        }
    }

    async fn apply_settings(&self, mut settings: SettingsUpdate) -> Result<(), AppError> {
        settings.proxy = settings.proxy.trim().to_owned();
        validate_label("proxy", &settings.proxy, 4_096, true)?;
        settings.max_concurrent = settings.max_concurrent.clamp(1, 20);
        settings.split = settings.split.clamp(1, 64);
        settings.connections = settings.connections.clamp(1, 16);
        settings.default_speed_limit = settings.default_speed_limit.max(0);
        if !matches!(settings.theme.as_str(), "system" | "light" | "dark") {
            return Err(AppError::classify("unsupported theme"));
        }
        if !matches!(
            settings.locale.as_str(),
            "system" | "en" | "tr" | "es" | "fr" | "de" | "ru" | "ar" | "zh" | "ja" | "ko"
        ) {
            return Err(AppError::classify("unsupported language"));
        }
        if !matches!(
            settings.on_complete_action.as_str(),
            "none" | "quit" | "sleep" | "shutdown"
        ) && !settings.on_complete_action.starts_with("run:")
        {
            return Err(AppError::classify("unsupported completion action"));
        }
        if settings.on_complete_action == "run:" {
            return Err(AppError::classify("completion command is empty"));
        }
        validate_label(
            "completion action",
            &settings.on_complete_action,
            4_096,
            false,
        )?;

        let old_bool = |key: &str, fallback: bool| -> Result<bool, AppError> {
            Ok(self
                .db
                .get_setting(key)?
                .map(|value| value == "true")
                .unwrap_or(fallback))
        };
        let old_autostart = old_bool("autostart", false)?;
        let old_magnets = old_bool("handle_magnets", true)?;
        let old_proxy = self.db.get_setting("proxy")?.unwrap_or_default();
        let old_maximum = self
            .db
            .get_setting("max_concurrent")?
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(5)
            .clamp(1, 20);
        let autostart_changed = settings.autostart != old_autostart;
        let magnets_changed = settings.handle_magnets != old_magnets;

        if autostart_changed {
            crate::platform::set_autostart(settings.autostart).map_err(AppError::classify)?;
        }
        if magnets_changed {
            if let Err(error) = crate::platform::set_magnet_handler(settings.handle_magnets) {
                if autostart_changed {
                    let _ = crate::platform::set_autostart(old_autostart);
                }
                return Err(AppError::classify(error));
            }
        }

        let engine = self.engine.current().await;
        if let Some(engine) = &engine {
            if let Err(error) = engine
                .rpc
                .change_global_option(json!({
                    "all-proxy": settings.proxy.clone(),
                    "max-concurrent-downloads": settings.max_concurrent.to_string(),
                }))
                .await
            {
                if magnets_changed {
                    let _ = crate::platform::set_magnet_handler(old_magnets);
                }
                if autostart_changed {
                    let _ = crate::platform::set_autostart(old_autostart);
                }
                return Err(error.into());
            }
        }

        let values = vec![
            ("proxy".into(), settings.proxy.clone()),
            ("max_concurrent".into(), settings.max_concurrent.to_string()),
            ("split".into(), settings.split.to_string()),
            ("max_conn".into(), settings.connections.to_string()),
            ("clipboard_watch".into(), settings.clipboard.to_string()),
            ("theme".into(), settings.theme.clone()),
            ("auto_organize".into(), settings.auto_organize.to_string()),
            (
                "prevent_duplicate_downloads".into(),
                settings.prevent_duplicates.to_string(),
            ),
            ("close_to_tray".into(), settings.close_to_tray.to_string()),
            ("autostart".into(), settings.autostart.to_string()),
            ("handle_magnets".into(), settings.handle_magnets.to_string()),
            ("dht_enabled".into(), settings.dht.to_string()),
            ("sandbox_children".into(), settings.sandbox.to_string()),
            (
                "block_private_ips".into(),
                settings.block_private.to_string(),
            ),
            ("locale".into(), settings.locale.clone()),
            (
                "on_complete_action".into(),
                settings.on_complete_action.clone(),
            ),
            (
                "on_complete_command_confirmed".into(),
                settings.on_complete_confirmed.to_string(),
            ),
            (
                "default_speed_limit".into(),
                settings.default_speed_limit.to_string(),
            ),
        ];
        if let Err(error) = self.db.set_settings(&values) {
            if let Some(engine) = &engine {
                let _ = engine
                    .rpc
                    .change_global_option(json!({
                        "all-proxy": old_proxy,
                        "max-concurrent-downloads": old_maximum.to_string(),
                    }))
                    .await;
            }
            if magnets_changed {
                let _ = crate::platform::set_magnet_handler(old_magnets);
            }
            if autostart_changed {
                let _ = crate::platform::set_autostart(old_autostart);
            }
            return Err(error.into());
        }

        {
            let mut defaults = self
                .defaults
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            defaults.split = settings.split;
            defaults.max_connection_per_server = settings.connections;
        }
        self.clipboard_on
            .store(settings.clipboard, Ordering::Relaxed);
        self.ytdlp
            .set_max_concurrent(settings.max_concurrent as usize);
        self.engine
            .update_runtime_options(
                settings.max_concurrent,
                (!settings.proxy.is_empty()).then_some(settings.proxy.clone()),
                settings.dht,
                settings.sandbox,
            )
            .await;
        self.power_fired.store(false, Ordering::Release);
        self.events
            .emit("settings:clipboard", json!({"enabled": settings.clipboard}));
        self.events.emit("settings:changed", json!({}));
        Ok(())
    }

    fn view(&self, id: i64) -> Result<DownloadView, AppError> {
        self.db
            .get(id)?
            .map(Into::into)
            .ok_or_else(|| AppError::classify("download vanished"))
    }

    async fn pause(&self, id: i64) -> Result<(), AppError> {
        if let Some(row) = self.db.get(id)? {
            if row.is_ytdlp() {
                self.ytdlp.cancel(id);
            } else if let (Some(gid), Some(engine)) =
                (row.gid.as_deref(), self.engine.current().await)
            {
                let _ = engine.rpc.pause(gid).await;
            }
            self.db.set_status(id, DownloadStatus::Paused)?;
            self.events
                .emit("downloads:state", json!({"id": id, "status": "paused"}));
        }
        self.arm_engine_if_idle().await;
        Ok(())
    }

    async fn resume(self: &Arc<Self>, id: i64) -> Result<(), AppError> {
        self.power_fired.store(false, Ordering::Relaxed);
        let Some(row) = self.db.get(id)? else {
            return Ok(());
        };
        if row.is_ytdlp() {
            if !self.ytdlp.is_available() {
                return Err(AppError::classify("yt-dlp not available"));
            }
            let job = ingest::job_from_row(&row);
            let options = row
                .media_opts
                .as_deref()
                .and_then(|value| serde_json::from_str(value).ok());
            let target = row.page_url.clone().unwrap_or_else(|| row.url.clone());
            ingest::validate_media_target(&self.db, &target)
                .await
                .map_err(AppError::classify)?;
            crate::ytdlp::validate_options(row.format_id.as_deref(), options.as_ref())
                .map_err(AppError::classify)?;
            self.db.set_status(id, DownloadStatus::Active)?;
            self.ytdlp
                .start(
                    id,
                    target,
                    row.format_id.clone(),
                    job.header_lines(),
                    options,
                )
                .map_err(AppError::classify)?;
        } else {
            let handle = self.ensure_engine().await?;
            if let Some(gid) = row.gid.as_deref() {
                if handle.engine.rpc.unpause(gid).await.is_err() {
                    let defaults = self
                        .defaults
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone();
                    ingest::reissue(&handle.engine, &self.db, defaults, &row)
                        .await
                        .map_err(AppError::classify)?;
                }
            } else {
                let defaults = self
                    .defaults
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                ingest::reissue(&handle.engine, &self.db, defaults, &row)
                    .await
                    .map_err(AppError::classify)?;
            }
            self.db.set_status(id, DownloadStatus::Active)?;
        }
        if row.start_at.is_some() {
            let _ = self.db.set_start_at(id, None);
        }
        self.events
            .emit("downloads:state", json!({"id": id, "status": "active"}));
        Ok(())
    }

    async fn retry(self: &Arc<Self>, id: i64) -> Result<(), AppError> {
        self.power_fired.store(false, Ordering::Relaxed);
        let row = self
            .db
            .get(id)?
            .ok_or_else(|| AppError::classify("download not found"))?;
        if let Some(engine) = self.engine.current().await {
            let mut gids = Vec::new();
            if let Some(gid) = row.gid.as_deref() {
                gids.push(gid);
            }
            if let Some(gid) = row.session_gid.as_deref() {
                if !gids.contains(&gid) {
                    gids.push(gid);
                }
            }
            for gid in gids {
                let _ = engine.rpc.force_remove(gid).await;
                let _ = engine.rpc.remove_download_result(gid).await;
            }
        }
        if row.is_ytdlp() {
            if !self.ytdlp.is_available() {
                return Err(AppError::classify("yt-dlp not available"));
            }
            let job = ingest::job_from_row(&row);
            let options = row
                .media_opts
                .as_deref()
                .and_then(|value| serde_json::from_str(value).ok());
            let target = row.page_url.clone().unwrap_or_else(|| row.url.clone());
            ingest::validate_media_target(&self.db, &target)
                .await
                .map_err(AppError::classify)?;
            crate::ytdlp::validate_options(row.format_id.as_deref(), options.as_ref())
                .map_err(AppError::classify)?;
            self.db.set_status(id, DownloadStatus::Active)?;
            self.ytdlp
                .start(
                    id,
                    target,
                    row.format_id.clone(),
                    job.header_lines(),
                    options,
                )
                .map_err(AppError::classify)?;
        } else {
            let handle = self.ensure_engine().await?;
            let defaults = self
                .defaults
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            ingest::reissue(&handle.engine, &self.db, defaults, &row)
                .await
                .map_err(AppError::classify)?;
            self.db.set_status(id, DownloadStatus::Active)?;
        }
        self.events
            .emit("downloads:state", json!({"id": id, "status": "active"}));
        Ok(())
    }

    async fn remove(&self, id: i64, delete_files: bool) -> Result<(), AppError> {
        let Some(row) = self.db.get(id)? else {
            return Ok(());
        };
        self.ytdlp.cancel(id);
        let mut files = Vec::new();
        if let (Some(gid), Some(engine)) = (row.gid.as_deref(), self.engine.current().await) {
            if delete_files {
                files = engine
                    .rpc
                    .get_files(gid)
                    .await
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|file| file.get("path").and_then(Value::as_str).map(str::to_owned))
                    .collect();
            }
        }
        if let Some(engine) = self.engine.current().await {
            let mut gids = Vec::new();
            if let Some(gid) = row.gid.as_deref() {
                gids.push(gid);
            }
            if let Some(gid) = row.session_gid.as_deref() {
                if !gids.contains(&gid) {
                    gids.push(gid);
                }
            }
            for gid in gids {
                let _ = engine.rpc.force_remove(gid).await;
                let _ = engine.rpc.remove_download_result(gid).await;
            }
        }
        if delete_files {
            if let Some(filename) = &row.filename {
                files.push(
                    Path::new(&row.dir)
                        .join(filename)
                        .to_string_lossy()
                        .into_owned(),
                );
            }
            for file in files {
                if let Some(target) = deletion_target(Path::new(&row.dir), Path::new(&file)) {
                    let _ = std::fs::remove_file(&target);
                    let mut sidecar = target.into_os_string();
                    sidecar.push(".aria2");
                    let _ = std::fs::remove_file(PathBuf::from(sidecar));
                }
            }
        }
        self.db.delete(id)?;
        if let Some(package) = row.package_id {
            let _ = self.db.delete_package_if_empty(package);
        }
        self.events
            .emit("downloads:state", json!({"id": id, "status": "removed"}));
        self.arm_engine_if_idle().await;
        Ok(())
    }

    async fn remove_status(&self, status: &str) -> Result<usize, AppError> {
        let rows = self.db.list(Some(status))?;
        let engine = self.engine.current().await;
        let mut count = 0;
        for row in rows {
            self.ytdlp.cancel(row.id);
            if let Some(engine) = &engine {
                let mut gids = Vec::new();
                if let Some(gid) = row.gid.as_deref() {
                    gids.push(gid);
                }
                if let Some(gid) = row.session_gid.as_deref() {
                    if !gids.contains(&gid) {
                        gids.push(gid);
                    }
                }
                for gid in gids {
                    let _ = engine.rpc.remove_download_result(gid).await;
                }
            }
            if self.db.delete(row.id).is_ok() {
                count += 1;
                if let Some(package) = row.package_id {
                    let _ = self.db.delete_package_if_empty(package);
                }
            }
        }
        self.events
            .emit("downloads:state", json!({"removed": count}));
        self.arm_engine_if_idle().await;
        Ok(count)
    }

    async fn add_media(
        &self,
        url: String,
        format_id: Option<String>,
        options: Option<MediaOptions>,
        package_id: Option<i64>,
    ) -> Result<DownloadView, AppError> {
        self.power_fired.store(false, Ordering::Relaxed);
        if !self.ytdlp.is_available() {
            return Err(AppError::classify("yt-dlp not available"));
        }
        let url = url.trim().to_owned();
        ingest::validate_media_target(&self.db, &url)
            .await
            .map_err(AppError::classify)?;
        crate::ytdlp::validate_options(format_id.as_deref(), options.as_ref())
            .map_err(AppError::classify)?;
        let inserted = self
            .db
            .insert_download_with_duplicate_policy(&NewDownload {
                url: url.clone(),
                dir: self.download_dir.to_string_lossy().into_owned(),
                kind: "video".into(),
                format_id: format_id.clone(),
                package_id,
                media_opts: options
                    .as_ref()
                    .and_then(|options| serde_json::to_string(options).ok()),
                ..Default::default()
            })?;
        let id = inserted.id();
        if matches!(inserted, DownloadInsertResult::Inserted(_)) {
            self.db.set_status(id, DownloadStatus::Active)?;
            self.ytdlp
                .start(id, url, format_id, Vec::new(), options)
                .map_err(AppError::classify)?;
            self.events
                .emit("downloads:state", json!({"id": id, "status": "active"}));
        }
        self.view(id)
    }

    async fn details(&self, id: i64) -> Result<DownloadDetails, AppError> {
        let row = self
            .db
            .get(id)?
            .ok_or_else(|| AppError::classify("download not found"))?;
        let mut details = DownloadDetails {
            id,
            url: row.url.clone(),
            dir: row.dir.clone(),
            kind: row.kind.clone(),
            error_message: row.error_message.clone(),
            num_pieces: 0,
            piece_length: 0,
            verified_length: 0,
            files: stored_detail_file(&row).into_iter().collect(),
            peers: Vec::new(),
            live: false,
        };
        let (Some(gid), Some(engine)) = (row.gid.as_deref(), self.engine.current().await) else {
            return Ok(details);
        };
        let Ok(status) = engine
            .rpc
            .tell_status(gid, minidl_core::aria2::DETAIL_KEYS)
            .await
        else {
            return Ok(details);
        };
        details.live = true;
        details.num_pieces = value_i64(&status, "numPieces");
        details.piece_length = value_i64(&status, "pieceLength");
        details.verified_length = value_i64(&status, "verifiedLength");
        let live_files = status
            .get("files")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|file| DetailFile {
                index: value_i64(file, "index"),
                path: value_string(file, "path"),
                length: value_i64(file, "length"),
                completed_length: value_i64(file, "completedLength"),
                selected: value_string(file, "selected") != "false",
            })
            .collect::<Vec<_>>();
        if !live_files.is_empty() {
            details.files = live_files;
        }
        if matches!(row.kind.as_str(), "torrent" | "magnet") {
            details.peers = engine
                .rpc
                .get_peers(gid)
                .await
                .unwrap_or_default()
                .iter()
                .map(|peer| DetailPeer {
                    ip: value_string(peer, "ip"),
                    down_speed: value_i64(peer, "downloadSpeed"),
                    up_speed: value_i64(peer, "uploadSpeed"),
                    seeder: value_string(peer, "seeder") == "true",
                })
                .collect();
        }
        Ok(details)
    }

    async fn set_torrent_files(
        self: &Arc<Self>,
        id: i64,
        mut indices: Vec<u32>,
    ) -> Result<(), AppError> {
        if indices.is_empty() {
            return Err(AppError::classify("select at least one file"));
        }
        if indices.len() > ingest::MAX_BATCH_ITEMS {
            return Err(AppError::classify("too many torrent files selected"));
        }
        if indices.contains(&0) {
            return Err(AppError::classify("invalid torrent file index"));
        }
        indices.sort_unstable();
        indices.dedup();
        let row = self
            .db
            .get(id)?
            .ok_or_else(|| AppError::classify("download not found"))?;
        if !matches!(row.kind.as_str(), "torrent" | "magnet") {
            return Err(AppError::classify(
                "file selection only applies to torrents",
            ));
        }
        let gid = row
            .gid
            .as_deref()
            .ok_or_else(|| AppError::classify("download has no engine handle yet"))?;
        let handle = self.ensure_engine().await?;
        let options = json!({"select-file": indices.iter().map(u32::to_string).collect::<Vec<_>>().join(",")});
        let result = if row.status == DownloadStatus::Active {
            let _ = handle.engine.rpc.pause(gid).await;
            let result = handle.engine.rpc.change_option(gid, options).await;
            let _ = handle.engine.rpc.unpause(gid).await;
            result
        } else {
            handle.engine.rpc.change_option(gid, options).await
        };
        self.arm_engine_if_idle().await;
        result?;
        Ok(())
    }

    async fn schedule(&self, id: i64, start_at: Option<i64>) -> Result<(), AppError> {
        let row = self
            .db
            .get(id)?
            .ok_or_else(|| AppError::classify("download not found"))?;
        if let Some(start_at) = start_at {
            if row.status.is_running() {
                if row.is_ytdlp() {
                    self.ytdlp.cancel(id);
                } else if let (Some(gid), Some(engine)) =
                    (row.gid.as_deref(), self.engine.current().await)
                {
                    let _ = engine.rpc.pause(gid).await;
                }
            }
            self.db.set_start_at(id, Some(start_at))?;
            self.db.set_status(id, DownloadStatus::Scheduled)?;
            self.events
                .emit("downloads:state", json!({"id": id, "status": "scheduled"}));
        } else {
            self.db.set_start_at(id, None)?;
            if row.status == DownloadStatus::Scheduled {
                self.db.set_status(id, DownloadStatus::Paused)?;
            }
            self.events
                .emit("downloads:state", json!({"id": id, "status": "paused"}));
        }
        self.arm_engine_if_idle().await;
        Ok(())
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_does_not_serialize_replay_secrets() {
        let row = Download {
            id: 1,
            gid: None,
            session_gid: None,
            url: "https://example.org".into(),
            filename: None,
            dir: "/tmp".into(),
            status: DownloadStatus::Paused,
            kind: "http".into(),
            total_bytes: 0,
            completed_bytes: 0,
            download_speed: 0,
            upload_speed: 0,
            connections: 0,
            num_seeders: 0,
            referrer: None,
            info_hash: None,
            error_code: None,
            error_message: None,
            category_id: None,
            created_at: 0,
            finished_at: None,
            user_agent: Some("secret-agent".into()),
            cookie: Some("secret-cookie".into()),
            extra_headers: Some("secret-header".into()),
            page_url: None,
            format_id: None,
            speed_limit: None,
            package_id: None,
            mime: None,
            checksum: None,
            start_at: None,
            media_opts: None,
        };
        let json = serde_json::to_string(&DownloadView::from(row)).unwrap();
        assert!(!json.contains("secret-agent"));
        assert!(!json.contains("secret-cookie"));
        assert!(!json.contains("secret-header"));
    }

    #[test]
    fn checksum_validation_is_strict() {
        assert!(normalize_checksum(Some("f".repeat(64))).is_ok());
        assert!(normalize_checksum(Some("f".repeat(63))).is_err());
    }

    #[test]
    fn simple_category_extensions_become_structured_rules() {
        assert_eq!(
            normalize_category_rules(".zip, PDF").unwrap(),
            r#"[{"match":"ext","values":["zip","pdf"]}]"#
        );
    }

    #[tokio::test]
    async fn ytdlp_completion_uses_the_shared_completion_event_path() {
        let root = tempfile::tempdir().unwrap();
        let downloads = root.path().join("downloads");
        std::fs::create_dir_all(&downloads).unwrap();
        let controller = Controller::open(root.path().join("data"), downloads.clone())
            .await
            .unwrap();
        let id = controller
            .db
            .insert_download(&NewDownload {
                url: "https://example.org/watch?v=1".into(),
                filename: Some("clip.webm".into()),
                dir: downloads.to_string_lossy().into_owned(),
                kind: "video".into(),
                ..Default::default()
            })
            .unwrap();
        controller
            .db
            .set_status(id, DownloadStatus::Complete)
            .unwrap();
        let mut events = controller.events.subscribe();

        controller
            .handle_ytdlp_completion(YtDlpCompletion {
                id,
                name: Some("clip.webm".into()),
                path: Some(downloads.join("clip.webm").to_string_lossy().into_owned()),
            })
            .await;

        let complete = events.recv().await.unwrap().0;
        let state = events.recv().await.unwrap().0;
        assert_eq!(complete.event, "downloads:complete");
        assert_eq!(complete.payload["id"], id);
        assert_eq!(state.event, "downloads:state");
        assert_eq!(state.payload["status"], "complete");
    }

    #[tokio::test]
    async fn stored_download_details_keep_the_completed_file_without_a_live_engine() {
        let root = tempfile::tempdir().unwrap();
        let downloads = root.path().join("downloads");
        std::fs::create_dir_all(&downloads).unwrap();
        let controller = Controller::open(root.path().join("data"), downloads.clone())
            .await
            .unwrap();
        let id = controller
            .db
            .insert_download(&NewDownload {
                url: "https://example.org/archive.zip".into(),
                filename: Some("archive.zip".into()),
                dir: downloads.to_string_lossy().into_owned(),
                kind: "http".into(),
                ..Default::default()
            })
            .unwrap();
        controller
            .db
            .checkpoint_progress_by_id(id, 54_000_000, 54_000_000, 0)
            .unwrap();

        let details = controller.details(id).await.unwrap();

        assert_eq!(details.files.len(), 1);
        assert_eq!(
            details.files[0].path,
            downloads.join("archive.zip").to_string_lossy()
        );
        assert_eq!(details.files[0].completed_length, 54_000_000);
        assert_eq!(details.files[0].length, 54_000_000);
    }
}
