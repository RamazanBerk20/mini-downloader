use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{Datelike, TimeZone, Timelike};
use interprocess::local_socket::tokio::{prelude::*, Stream};
use interprocess::local_socket::ListenerOptions;
use minidl_ipc::{
    self, BridgeReply, BridgeRequest, ControlCommand, ControlReply, ControlRequest,
    CONTROL_PROTOCOL_VERSION,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::controller::{AppError, Controller};

const MAX_FRAME: usize = 16 * 1024 * 1024;
const MAX_CONNECTIONS: usize = 32;
const MAX_OPEN_BATCHES: usize = 1_024;
const FRAME_TIMEOUT: Duration = Duration::from_secs(15);

/// Held for the controller lifetime to close the simultaneous-first-launch
/// race before either process has had time to bind its control socket.
pub struct InstanceGuard {
    _file: File,
}

pub fn try_acquire_instance() -> std::io::Result<Option<InstanceGuard>> {
    use fs2::FileExt;

    let directory = minidl_ipc::data_dir();
    std::fs::create_dir_all(&directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
    }
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(directory.join("controller.lock"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    match FileExt::try_lock_exclusive(&file) {
        Ok(()) => Ok(Some(InstanceGuard { _file: file })),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
        Err(error) => Err(error),
    }
}

async fn read_frame(stream: &mut Stream) -> std::io::Result<Vec<u8>> {
    let mut length = [0u8; 4];
    stream.read_exact(&mut length).await?;
    let length = u32::from_le_bytes(length) as usize;
    if length == 0 || length > MAX_FRAME {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid frame length",
        ));
    }
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes).await?;
    Ok(bytes)
}

async fn write_frame(stream: &mut Stream, value: &impl serde::Serialize) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    if bytes.len() > MAX_FRAME {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    stream
        .write_all(&(bytes.len() as u32).to_le_bytes())
        .await?;
    stream.write_all(&bytes).await?;
    stream.flush().await
}

fn timed_out() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::TimedOut, "local IPC frame timed out")
}

async fn read_frame_timed(stream: &mut Stream) -> std::io::Result<Vec<u8>> {
    tokio::time::timeout(FRAME_TIMEOUT, read_frame(stream))
        .await
        .map_err(|_| timed_out())?
}

async fn write_frame_timed(
    stream: &mut Stream,
    value: &impl serde::Serialize,
) -> std::io::Result<()> {
    tokio::time::timeout(FRAME_TIMEOUT, write_frame(stream, value))
        .await
        .map_err(|_| timed_out())?
}

#[cfg(unix)]
fn prepare_socket(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
        let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
    }
    let _ = std::fs::remove_file(path);
}

#[cfg(unix)]
fn protect_socket(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

pub async fn serve_control(controller: Arc<Controller>) -> Result<(), AppError> {
    #[cfg(unix)]
    prepare_socket(&minidl_ipc::control_socket_path());
    let listener = ListenerOptions::new()
        .name(minidl_ipc::control_socket_name()?)
        .create_tokio()?;
    #[cfg(unix)]
    protect_socket(&minidl_ipc::control_socket_path());

    let connections = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
    loop {
        let connection = listener.accept().await?;
        let Ok(permit) = connections.clone().try_acquire_owned() else {
            drop(connection);
            continue;
        };
        let controller = controller.clone();
        tokio::spawn(async move {
            let _permit = permit;
            handle_control(connection, controller).await;
        });
    }
}

async fn handle_control(mut stream: Stream, controller: Arc<Controller>) {
    loop {
        let Ok(bytes) = read_frame_timed(&mut stream).await else {
            return;
        };
        let request = match serde_json::from_slice::<ControlRequest>(&bytes) {
            Ok(request) => request,
            Err(error) => {
                let _ = write_frame_timed(
                    &mut stream,
                    &ControlReply::failure(0, "malformed", error.to_string()),
                )
                .await;
                return;
            }
        };
        if request.protocol_version != CONTROL_PROTOCOL_VERSION {
            let _ = write_frame_timed(
                &mut stream,
                &ControlReply::failure(
                    request.request_id,
                    "protocol-mismatch",
                    format!(
                        "unsupported controller protocol {}; expected {}",
                        request.protocol_version, CONTROL_PROTOCOL_VERSION
                    ),
                ),
            )
            .await;
            return;
        }
        if matches!(request.command, ControlCommand::Subscribe) {
            let mut events = controller.events().subscribe();
            if write_frame_timed(&mut stream, &ControlReply::success(request.request_id, ()))
                .await
                .is_err()
            {
                return;
            }
            loop {
                match events.recv().await {
                    Ok(event) if write_frame_timed(&mut stream, &event.0).await.is_ok() => {}
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        controller
                            .events()
                            .emit("state:resync-required", serde_json::json!({}));
                    }
                }
            }
        }
        let reply = match controller.execute(request.command).await {
            Ok(result) => ControlReply::success(request.request_id, result),
            Err(error) => ControlReply::failure(request.request_id, error.kind, error.message),
        };
        if write_frame_timed(&mut stream, &reply).await.is_err() {
            return;
        }
    }
}

#[derive(Clone)]
struct BridgeContext {
    controller: Arc<Controller>,
    batches: Arc<Mutex<HashMap<String, (i64, Instant)>>>,
}

pub async fn serve_bridge(controller: Arc<Controller>) -> Result<(), AppError> {
    record_app_path()?;
    #[cfg(unix)]
    prepare_socket(&minidl_ipc::bridge_socket_path());
    let listener = ListenerOptions::new()
        .name(minidl_ipc::bridge_socket_name()?)
        .create_tokio()?;
    #[cfg(unix)]
    protect_socket(&minidl_ipc::bridge_socket_path());
    let context = BridgeContext {
        controller,
        batches: Arc::new(Mutex::new(HashMap::new())),
    };
    let connections = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
    loop {
        let stream = listener.accept().await?;
        let Ok(permit) = connections.clone().try_acquire_owned() else {
            drop(stream);
            continue;
        };
        let context = context.clone();
        tokio::spawn(async move {
            let _permit = permit;
            handle_bridge(stream, context).await;
        });
    }
}

async fn handle_bridge(mut stream: Stream, context: BridgeContext) {
    let reply = match read_frame_timed(&mut stream)
        .await
        .ok()
        .and_then(|bytes| serde_json::from_slice::<BridgeRequest>(&bytes).ok())
    {
        Some(request) if request.protocol_version == minidl_ipc::PROTOCOL_VERSION => {
            if let Some(family) = request.browser_family {
                context.controller.record_connector_presence(family);
            }
            if request.presence {
                if request.browser_family.is_some() {
                    BridgeReply::acknowledged()
                } else {
                    BridgeReply::rejected("connector presence missing browser family")
                }
            } else if request.ping {
                BridgeReply {
                    ok: true,
                    job_id: None,
                    error: Some(format!(
                        "Mini Downloader (protocol {})",
                        minidl_ipc::PROTOCOL_VERSION
                    )),
                }
            } else {
                let batch_id = request.job.batch_id.clone();
                let batch = batch_package(&context, &request.job);
                match context
                    .controller
                    .ingest_capture(request.job, batch.map(|(id, _)| id))
                    .await
                {
                    Ok(id) => {
                        context
                            .controller
                            .events()
                            .emit("ui:show", serde_json::json!({}));
                        BridgeReply::accepted(id)
                    }
                    Err(error) => {
                        if let (Some((package, true)), Some(batch_id)) = (batch, batch_id) {
                            let _ = context.controller.db().delete_package_if_empty(package);
                            context
                                .batches
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .remove(&batch_id);
                        }
                        BridgeReply::rejected(error.message)
                    }
                }
            }
        }
        Some(_) => BridgeReply::rejected("unsupported protocol version"),
        None => BridgeReply::rejected("malformed bridge request"),
    };
    let _ = write_frame_timed(&mut stream, &reply).await;
}

fn batch_package(context: &BridgeContext, job: &minidl_ipc::CaptureJob) -> Option<(i64, bool)> {
    let id = job.batch_id.as_deref()?;
    let mut batches = context.batches.lock().unwrap_or_else(|e| e.into_inner());
    batches.retain(|_, (_, created)| created.elapsed() < Duration::from_secs(600));
    if let Some((package, _)) = batches.get(id) {
        return Some((*package, false));
    }
    if batches.len() >= MAX_OPEN_BATCHES {
        if let Some(oldest) = batches
            .iter()
            .min_by_key(|(_, (_, created))| *created)
            .map(|(key, _)| key.clone())
        {
            batches.remove(&oldest);
        }
    }
    let name = job
        .batch_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| minidl_core::grabber::host_of(&job.url));
    let package = context
        .controller
        .db()
        .insert_package(if name.is_empty() { "Batch" } else { &name }, None, None)
        .ok()?;
    batches.insert(id.to_owned(), (package, Instant::now()));
    Some((package, true))
}

fn record_app_path() -> Result<(), AppError> {
    let executable = crate::platform::executable()?;
    let directory = minidl_ipc::data_dir();
    std::fs::create_dir_all(&directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
    }
    let mut temporary = tempfile::NamedTempFile::new_in(&directory)?;
    temporary.write_all(executable.to_string_lossy().as_bytes())?;
    temporary.as_file().sync_all()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    temporary
        .persist(minidl_ipc::app_path_file())
        .map_err(|error| error.error)?;
    Ok(())
}

const MAX_SCHEDULE_CATCHUP_MINUTES: i64 = 7 * 24 * 60;

fn schedule_minutes(last: Option<i64>, now: i64) -> Vec<i64> {
    let now = now.div_euclid(60);
    match last {
        None => vec![now],
        Some(last) if now == last => Vec::new(),
        Some(last) if now < last => vec![now],
        Some(last) => {
            let first = (last + 1).max(now - MAX_SCHEDULE_CATCHUP_MINUTES + 1);
            (first..=now).collect()
        }
    }
}

fn schedule_matches(schedule: &minidl_core::model::Schedule, weekday: u32, minute: i64) -> bool {
    schedule.enabled && schedule.days_mask & (1i64 << weekday) != 0 && schedule.at_minute == minute
}

/// Event-driven scheduler: evaluates the current minute on startup and each
/// elapsed wall-clock minute thereafter. Iterating UTC instants gives correct
/// local dates across midnight/DST; the local-minute key prevents a fall-back
/// hour from firing the same rule twice.
pub async fn run_scheduler(controller: Arc<Controller>) {
    let mut events = controller.events().subscribe();
    let mut last_utc_minute = None;
    let mut fired = HashSet::<(i64, chrono::NaiveDate, i64)>::new();
    loop {
        let now = chrono::Local::now();
        let now_epoch = now.timestamp();
        let minutes = schedule_minutes(last_utc_minute, now_epoch);
        last_utc_minute = Some(now_epoch.div_euclid(60));
        let schedules = controller.db().list_schedules().unwrap_or_default();
        for epoch_minute in minutes {
            let Some(utc) = chrono::Utc
                .timestamp_opt(epoch_minute.saturating_mul(60), 0)
                .single()
            else {
                continue;
            };
            let local = utc.with_timezone(&chrono::Local);
            let minute = local.hour() as i64 * 60 + local.minute() as i64;
            let weekday = local.weekday().num_days_from_monday();
            for schedule in &schedules {
                let key = (schedule.id, local.date_naive(), minute);
                if schedule_matches(schedule, weekday, minute) && fired.insert(key) {
                    let command = match schedule.action.as_str() {
                        "pause_all" => Some(ControlCommand::PauseAll),
                        "resume_all" => Some(ControlCommand::ResumeAll),
                        "set_speed" => Some(ControlCommand::SetGlobalSpeed {
                            download: schedule.speed_limit.unwrap_or(0),
                            upload: 0,
                        }),
                        _ => None,
                    };
                    if let Some(command) = command {
                        let _ = controller.execute(command).await;
                    }
                }
            }
        }
        let retain_after = now.date_naive() - chrono::Duration::days(8);
        fired.retain(|(_, date, _)| *date >= retain_after);
        for row in controller
            .db()
            .due_scheduled(minidl_core::model::now())
            .unwrap_or_default()
        {
            if let Err(error) = controller
                .execute(ControlCommand::Resume { id: row.id })
                .await
            {
                let retry_at = minidl_core::model::now().saturating_add(60);
                let _ = controller
                    .db()
                    .set_status(row.id, minidl_core::model::DownloadStatus::Scheduled);
                let _ = controller.db().set_start_at(row.id, Some(retry_at));
                controller.events().emit(
                    "downloads:error",
                    serde_json::json!({"id": row.id, "message": error.message}),
                );
                controller.events().emit(
                    "downloads:state",
                    serde_json::json!({"id": row.id, "status": "scheduled"}),
                );
                controller
                    .events()
                    .emit("scheduler:changed", serde_json::json!({}));
            }
        }

        let until_minute = 60 - now.second() as u64;
        let until_download = controller
            .db()
            .list(Some("scheduled"))
            .unwrap_or_default()
            .into_iter()
            .filter_map(|row| row.start_at)
            .map(|at| at.saturating_sub(minidl_core::model::now()).max(1) as u64)
            .min();
        let delay = Duration::from_secs(
            until_download
                .map(|due| due.min(until_minute))
                .unwrap_or(until_minute)
                .max(1),
        );
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            event = events.recv() => match event {
                Ok(event) if event.0.event == "scheduler:changed" => {}
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
        if controller.should_quit() {
            return;
        }
    }
}

pub fn run_clipboard_watcher(controller: Arc<Controller>) {
    let running = Arc::new(AtomicBool::new(false));
    launch_clipboard_worker(controller.clone(), running.clone());
    tokio::spawn(async move {
        let mut events = controller.events().subscribe();
        while let Ok(event) = events.recv().await {
            if event.0.event == "settings:clipboard" && controller.clipboard_enabled() {
                launch_clipboard_worker(controller.clone(), running.clone());
            }
            if controller.should_quit() {
                break;
            }
        }
    });
}

fn launch_clipboard_worker(controller: Arc<Controller>, running: Arc<AtomicBool>) {
    if !controller.clipboard_enabled() || running.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(move || {
        let mut clipboard = arboard::Clipboard::new().ok();
        let mut previous = String::new();
        while controller.clipboard_enabled() && !controller.should_quit() {
            if let Some(text) = clipboard
                .as_mut()
                .and_then(|clipboard| clipboard.get_text().ok())
            {
                let text = text.trim();
                let link = text.starts_with("http://")
                    || text.starts_with("https://")
                    || text.starts_with("ftp://")
                    || text.starts_with("magnet:");
                if link && text.len() <= crate::ingest::MAX_URL_LEN && text != previous {
                    previous = text.to_owned();
                    controller
                        .events()
                        .emit("clipboard:detected", serde_json::json!({"url": text}));
                }
            }
            std::thread::sleep(Duration::from_millis(750));
        }
        running.store(false, Ordering::SeqCst);
        if controller.clipboard_enabled() && !controller.should_quit() {
            launch_clipboard_worker(controller, running);
        }
    });
}

pub struct ControlClient {
    counter: std::sync::atomic::AtomicU64,
}

impl ControlClient {
    pub async fn connect() -> std::io::Result<Arc<Self>> {
        // Probe eagerly so callers learn that the controller is unavailable at
        // connect time. Requests use independent streams to avoid a long media
        // probe serializing every unrelated UI action behind one mutex.
        let _probe = Stream::connect(minidl_ipc::control_socket_name()?).await?;
        Ok(Arc::new(Self {
            counter: std::sync::atomic::AtomicU64::new(1),
        }))
    }

    pub async fn request(&self, command: ControlCommand) -> Result<serde_json::Value, String> {
        let request_id = self.counter.fetch_add(1, Ordering::Relaxed);
        let request = ControlRequest::new(request_id, command);
        let mut stream =
            Stream::connect(minidl_ipc::control_socket_name().map_err(|error| error.to_string())?)
                .await
                .map_err(|error| error.to_string())?;
        write_frame_timed(&mut stream, &request)
            .await
            .map_err(|error| error.to_string())?;
        let bytes = read_frame_timed(&mut stream)
            .await
            .map_err(|error| error.to_string())?;
        let reply: ControlReply =
            serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
        if let Some(error) = reply.error {
            Err(error.message)
        } else {
            Ok(reply.result.unwrap_or(serde_json::Value::Null))
        }
    }

    pub async fn subscribe(
        mut on_event: impl FnMut(minidl_ipc::ControlEvent) + Send + 'static,
    ) -> Result<(), String> {
        let mut stream =
            Stream::connect(minidl_ipc::control_socket_name().map_err(|e| e.to_string())?)
                .await
                .map_err(|e| e.to_string())?;
        write_frame_timed(
            &mut stream,
            &ControlRequest::new(1, ControlCommand::Subscribe),
        )
        .await
        .map_err(|e| e.to_string())?;
        let _ack: ControlReply = serde_json::from_slice(
            &read_frame_timed(&mut stream)
                .await
                .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        loop {
            let event =
                serde_json::from_slice(&read_frame(&mut stream).await.map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            on_event(event);
        }
    }
}

pub async fn send_one(command: ControlCommand) -> Result<serde_json::Value, String> {
    ControlClient::connect()
        .await
        .map_err(|e| e.to_string())?
        .request(command)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_limit_rejects_zero_and_oversize() {
        assert_eq!(MAX_FRAME, 16 * 1024 * 1024);
        assert_eq!(MAX_CONNECTIONS, 32);
    }

    #[test]
    fn scheduler_checks_startup_elapsed_and_backward_clock_minutes() {
        assert_eq!(schedule_minutes(None, 600), vec![10]);
        assert!(schedule_minutes(Some(10), 659).is_empty());
        assert_eq!(schedule_minutes(Some(10), 720), vec![11, 12]);
        assert_eq!(schedule_minutes(Some(12), 600), vec![10]);
    }

    #[test]
    fn scheduler_uses_each_candidates_own_weekday() {
        let schedule = minidl_core::model::Schedule {
            id: 1,
            name: None,
            action: "pause_all".into(),
            days_mask: 1 << 1, // Tuesday
            at_minute: 5,
            speed_limit: None,
            enabled: true,
        };
        assert!(!schedule_matches(&schedule, 0, 5));
        assert!(schedule_matches(&schedule, 1, 5));
        assert!(!schedule_matches(&schedule, 1, 6));
    }
}
