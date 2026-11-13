use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use minidl_core::db::Db;
use minidl_core::model::DownloadStatus;
use minidl_core::ytdlp::MediaInfo;
use minidl_ipc::MediaOptions;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::{mpsc, Notify};
use tokio::task::JoinHandle;

use crate::events::EventBus;

#[cfg(unix)]
struct ProcessGroupKiller(Option<i32>);

#[cfg(unix)]
impl Drop for ProcessGroupKiller {
    fn drop(&mut self) {
        if let Some(pgid) = self.0 {
            // SAFETY: the child created this process group; ESRCH is harmless.
            unsafe { libc::killpg(pgid, libc::SIGKILL) };
        }
    }
}

/// Framework-independent yt-dlp driver. Progress is written to the DB and the
/// same controller event bus used by aria2.
pub struct YtDlp {
    db: Db,
    events: EventBus,
    download_dir: PathBuf,
    binary: Option<PathBuf>,
    aria2c: Option<PathBuf>,
    ffmpeg: Option<PathBuf>,
    completions: mpsc::UnboundedSender<YtDlpCompletion>,
    running: Arc<Mutex<HashMap<i64, RunningTask>>>,
    next_token: AtomicU64,
    slots: Arc<DynamicGate>,
}

struct RunningTask {
    token: u64,
    handle: JoinHandle<()>,
}

struct RunRequest {
    url: String,
    format_id: Option<String>,
    headers: Vec<String>,
    options: Option<MediaOptions>,
}

#[derive(Default)]
struct ProgressState {
    base_done: i64,
    base_total: i64,
    segment_done: i64,
    segment_total: i64,
}

impl ProgressState {
    fn update(&mut self, line: &str) -> Option<(i64, i64, i64)> {
        let progress = line.strip_prefix("dl:")?;
        let fields = progress.split('|').collect::<Vec<_>>();
        let number = |value: &str| {
            value
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|number| number.is_finite() && *number >= 0.0)
                .map(|number| number as i64)
        };
        let done = fields
            .first()
            .and_then(|value| number(value))
            .unwrap_or(self.segment_done);
        let total = fields
            .get(1)
            .and_then(|value| number(value))
            .or_else(|| fields.get(2).and_then(|value| number(value)))
            .unwrap_or(self.segment_total);
        let speed = fields.get(3).and_then(|value| number(value)).unwrap_or(0);
        if done + 1 < self.segment_done {
            self.base_done += self.segment_done;
            self.base_total += self.segment_total.max(self.segment_done);
            self.segment_total = 0;
        }
        self.segment_done = done;
        self.segment_total = self.segment_total.max(total);
        let completed = self.base_done + self.segment_done;
        let total = (self.base_total + self.segment_total).max(completed);
        Some((completed, total, speed))
    }
}

struct DynamicGate {
    active: AtomicUsize,
    limit: AtomicUsize,
    changed: Notify,
}

impl DynamicGate {
    fn new(limit: usize) -> Self {
        Self {
            active: AtomicUsize::new(0),
            limit: AtomicUsize::new(limit.max(1)),
            changed: Notify::new(),
        }
    }

    fn is_full(&self) -> bool {
        self.active.load(Ordering::Acquire) >= self.limit.load(Ordering::Acquire)
    }

    fn set_limit(&self, limit: usize) {
        self.limit.store(limit.max(1), Ordering::Release);
        self.changed.notify_waiters();
    }

    async fn acquire(self: &Arc<Self>) -> DynamicPermit {
        loop {
            let notified = self.changed.notified();
            let active = self.active.load(Ordering::Acquire);
            if active < self.limit.load(Ordering::Acquire) {
                if self
                    .active
                    .compare_exchange(active, active + 1, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    return DynamicPermit { gate: self.clone() };
                }
                continue;
            }
            notified.await;
        }
    }
}

struct DynamicPermit {
    gate: Arc<DynamicGate>,
}

impl Drop for DynamicPermit {
    fn drop(&mut self) {
        self.gate.active.fetch_sub(1, Ordering::AcqRel);
        self.gate.changed.notify_waiters();
    }
}

#[derive(Debug)]
pub(crate) struct YtDlpCompletion {
    pub id: i64,
    pub name: Option<String>,
    pub path: Option<String>,
}

impl YtDlp {
    pub(crate) fn resolve(
        db: Db,
        events: EventBus,
        download_dir: PathBuf,
    ) -> (Arc<Self>, mpsc::UnboundedReceiver<YtDlpCompletion>) {
        let user =
            minidl_core::paths::bin_dir().join(format!("yt-dlp{}", std::env::consts::EXE_SUFFIX));
        let binary = user
            .is_file()
            .then_some(user)
            .or_else(|| minidl_core::paths::resolve_tool("yt-dlp"));
        let max = db
            .get_setting("max_concurrent")
            .ok()
            .flatten()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(5)
            .clamp(1, 20);
        let (completions, completion_rx) = mpsc::unbounded_channel();
        let driver = Arc::new(Self {
            db,
            events,
            download_dir,
            binary,
            aria2c: minidl_core::paths::resolve_tool("aria2c"),
            ffmpeg: minidl_core::paths::resolve_tool("ffmpeg"),
            completions,
            running: Arc::new(Mutex::new(HashMap::new())),
            next_token: AtomicU64::new(1),
            slots: Arc::new(DynamicGate::new(max)),
        });
        (driver, completion_rx)
    }

    pub fn is_available(&self) -> bool {
        self.binary.is_some()
    }

    pub fn is_running(&self) -> bool {
        !self
            .running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
    }

    pub fn set_max_concurrent(&self, maximum: usize) {
        self.slots.set_limit(maximum.clamp(1, 20));
    }

    pub async fn probe(
        &self,
        url: &str,
        headers: &[String],
        playlist: bool,
    ) -> Result<MediaInfo, String> {
        let binary = self.binary.as_deref().ok_or("yt-dlp not found")?;
        minidl_core::ytdlp::probe(binary, url, headers, playlist)
            .await
            .map_err(|e| e.to_string())
    }

    pub fn start(
        self: &Arc<Self>,
        id: i64,
        url: String,
        format_id: Option<String>,
        headers: Vec<String>,
        options: Option<MediaOptions>,
    ) -> Result<(), String> {
        let Some(binary) = self.binary.clone() else {
            let _ = self.db.fail(
                id,
                None,
                Some("yt-dlp not available"),
                "yt-dlp: not installed",
            );
            self.events.emit(
                "downloads:error",
                serde_json::json!({
                    "id": id, "message": "yt-dlp not available"
                }),
            );
            self.events.emit(
                "downloads:state",
                serde_json::json!({"id": id, "status": "error"}),
            );
            return Err("yt-dlp not available".into());
        };

        validate_options(format_id.as_deref(), options.as_ref())?;

        let this = self.clone();
        let token = self.next_token.fetch_add(1, Ordering::Relaxed);
        let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(old) = running.remove(&id) {
            old.handle.abort();
        }
        let running_map = self.running.clone();
        let ready = Arc::new(Notify::new());
        let task_ready = ready.clone();
        let handle = tokio::spawn(async move {
            // Publish the token/handle before the task can finish and attempt
            // conditional cleanup on another runtime worker.
            task_ready.notified().await;
            if this.slots.is_full() {
                let _ = this
                    .db
                    .transition(id, DownloadStatus::Waiting, "yt-dlp: all slots busy");
                this.events.emit(
                    "downloads:state",
                    serde_json::json!({"id": id, "status": "waiting"}),
                );
            }
            let _permit = this.slots.acquire().await;
            // Start only if nobody paused/removed the row while it waited.
            let startable = this
                .db
                .get(id)
                .ok()
                .flatten()
                .is_some_and(|row| row.status == DownloadStatus::Active)
                || this
                    .db
                    .transition_if(
                        id,
                        &[DownloadStatus::Queued, DownloadStatus::Waiting],
                        DownloadStatus::Active,
                        "yt-dlp: slot free, started",
                    )
                    .unwrap_or(false);
            if !startable {
                remove_if_current(&running_map, id, token);
                return;
            }
            this.events.emit(
                "downloads:state",
                serde_json::json!({"id": id, "status": "active"}),
            );
            this.run(
                binary,
                id,
                token,
                RunRequest {
                    url,
                    format_id,
                    headers,
                    options,
                },
            )
            .await;
        });
        running.insert(id, RunningTask { token, handle });
        ready.notify_one();
        Ok(())
    }

    pub fn cancel(&self, id: i64) -> bool {
        self.running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id)
            .map(|task| task.handle.abort())
            .is_some()
    }

    pub fn cancel_all(&self) {
        for (_, task) in self
            .running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain()
        {
            task.handle.abort();
        }
    }

    async fn run(self: Arc<Self>, binary: PathBuf, id: i64, token: u64, request: RunRequest) {
        let RunRequest {
            url,
            format_id,
            headers,
            options,
        } = request;
        let name_file = match tempfile::Builder::new()
            .prefix("minidl-ytdlp-output-")
            .suffix(".name")
            .tempfile()
        {
            Ok(file) => file,
            Err(error) => {
                self.finish_error(
                    id,
                    token,
                    format!("create private yt-dlp output file: {error}"),
                );
                return;
            }
        };
        // Partial and fragment files go to this row's private staging folder,
        // so Remove (with or without deleting files) can find them by id.
        let stage = match minidl_core::files::ensure_stage_dir(&self.download_dir, id) {
            Ok(stage) => stage,
            Err(error) => {
                self.finish_error(id, token, format!("create staging folder: {error}"));
                return;
            }
        };
        let _ = self.db.set_stage_dir(id, Some(&stage.to_string_lossy()));
        let mut temp_path = std::ffi::OsString::from("temp:");
        temp_path.push(&stage);
        let opts = options.unwrap_or_default();
        let mut command = Command::new(binary);
        minidl_core::child_process::hide_console_window(command.as_std_mut());
        command
            .arg("--newline")
            .arg("--no-playlist")
            .arg("--progress-template")
            .arg("dl:%(progress.downloaded_bytes)s|%(progress.total_bytes)s|%(progress.total_bytes_estimate)s|%(progress.speed)s")
            .arg("--print-to-file")
            .arg("after_move:filepath")
            .arg(name_file.path())
            .arg("--paths")
            .arg(&self.download_dir)
            .arg("--paths")
            .arg(&temp_path)
            .arg("-o")
            .arg("%(title)s.%(ext)s");
        if opts.audio_only {
            command.args(["-f", "ba/b"]);
            if self.ffmpeg.is_some() {
                command.arg("-x");
                if !opts.audio_format.is_empty() {
                    command.args(["--audio-format", &opts.audio_format]);
                }
            }
        } else {
            let default_format = if self.ffmpeg.is_some() {
                "bv*+ba/b"
            } else {
                "b"
            };
            command
                .arg("-f")
                .arg(format_id.unwrap_or_else(|| default_format.into()));
        }
        if opts.write_subs || opts.embed_subs {
            command
                .arg("--write-subs")
                .arg("--sub-langs")
                .arg(if opts.sub_langs.is_empty() {
                    "en"
                } else {
                    &opts.sub_langs
                });
            if opts.embed_subs && self.ffmpeg.is_some() {
                command.arg("--embed-subs");
            }
        }
        if opts.embed_thumbnail && self.ffmpeg.is_some() {
            command.arg("--embed-thumbnail");
        }
        if let Some(ffmpeg) = &self.ffmpeg {
            command.arg("--ffmpeg-location").arg(ffmpeg);
        }
        let proxy = self
            .db
            .get_setting("proxy")
            .ok()
            .flatten()
            .filter(|s| !s.is_empty());
        // Use the exact resolved sidecar path. In particular, the Windows
        // installer keeps aria2c beside the app without modifying PATH.
        if let Some(aria2c) = &self.aria2c {
            command
                .arg("--downloader")
                .arg(aria2c)
                .args(["--downloader-args", "aria2c:-x16 -s16 -k1M"]);
        }
        command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let _private_config = match minidl_core::ytdlp::configure_private_inputs(
            &mut command,
            &headers,
            proxy.as_deref(),
        ) {
            Ok(config) => config,
            Err(error) => {
                self.finish_error(id, token, format!("prepare yt-dlp: {error}"));
                return;
            }
        };
        #[cfg(unix)]
        command.process_group(0);
        #[cfg(unix)]
        unsafe {
            command.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL as libc::c_ulong);
                Ok(())
            });
        }

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                self.finish_error(id, token, format!("spawn yt-dlp: {error}"));
                return;
            }
        };
        #[cfg(unix)]
        let mut group = ProcessGroupKiller(child.id().map(|pid| pid as i32));
        if let Err(error) = minidl_core::ytdlp::write_batch_url(&mut child, &url).await {
            let _ = child.kill().await;
            self.finish_error(id, token, format!("send URL to yt-dlp: {error}"));
            return;
        }
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let error_tail = Arc::new(Mutex::new(String::new()));
        let stderr_task = {
            let error_tail = error_tail.clone();
            tokio::spawn(async move {
                let mut stderr = stderr;
                let mut chunk = [0u8; 8 * 1024];
                while let Ok(read) = stderr.read(&mut chunk).await {
                    if read == 0 {
                        break;
                    }
                    let mut tail = error_tail.lock().unwrap_or_else(|e| e.into_inner());
                    tail.push_str(&String::from_utf8_lossy(&chunk[..read]));
                    if tail.len() > 8192 {
                        let mut cut = tail.len() - 8192;
                        while !tail.is_char_boundary(cut) {
                            cut += 1;
                        }
                        tail.drain(..cut);
                    }
                }
            })
        };

        let mut stdout = stdout;
        let mut chunk = [0u8; 8 * 1024];
        let mut line = Vec::with_capacity(256);
        let mut line_overflow = false;
        let mut progress = ProgressState::default();
        while let Ok(read) = stdout.read(&mut chunk).await {
            if read == 0 {
                break;
            }
            for byte in &chunk[..read] {
                if *byte == b'\n' {
                    if !line_overflow {
                        self.report_progress(id, &mut progress, &line);
                    }
                    line.clear();
                    line_overflow = false;
                } else if line.len() < 4 * 1024 {
                    line.push(*byte);
                } else {
                    line_overflow = true;
                }
            }
        }
        if !line.is_empty() && !line_overflow {
            self.report_progress(id, &mut progress, &line);
        }

        let status = child.wait().await;
        #[cfg(unix)]
        {
            group.0 = None;
        }
        let _ = stderr_task.await;
        if status.map(|s| s.success()).unwrap_or(false) {
            let path = std::fs::read_to_string(name_file.path())
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            let name = path
                .as_deref()
                .and_then(|p| std::path::Path::new(p).file_name())
                .and_then(|n| n.to_str())
                .map(String::from);
            #[cfg(unix)]
            if let Some(path) = &path {
                let _ = xattr::set(path, "user.xdg.origin.url", url.as_bytes());
            }
            let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
            if !running.get(&id).is_some_and(|task| task.token == token) {
                return;
            }
            if let Some(name) = &name {
                let _ = self.db.set_filename(id, name);
            }
            minidl_core::files::remove_stage_dir(&stage);
            if !stage.exists() {
                let _ = self.db.set_stage_dir(id, None);
            }
            let _ = self
                .db
                .transition(id, DownloadStatus::Complete, "yt-dlp: finished");
            // The controller owns queue-wide completion actions. A dedicated
            // channel cannot lose this transition under broadcast backpressure.
            let _ = self.completions.send(YtDlpCompletion { id, name, path });
            running.remove(&id);
        } else {
            let message = error_tail
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .lines()
                .rev()
                .find(|line| line.contains("ERROR"))
                .unwrap_or("yt-dlp failed")
                .to_string();
            self.finish_error(id, token, message);
        }
    }

    fn finish_error(&self, id: i64, token: u64, message: String) {
        let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        if !running.get(&id).is_some_and(|task| task.token == token) {
            return;
        }
        let _ = self.db.fail(id, None, Some(&message), "yt-dlp: failed");
        self.events.emit(
            "downloads:error",
            serde_json::json!({"id": id, "message": message}),
        );
        self.events.emit(
            "downloads:state",
            serde_json::json!({"id": id, "status": "error"}),
        );
        running.remove(&id);
    }

    fn report_progress(&self, id: i64, state: &mut ProgressState, line: &[u8]) {
        let Ok(line) = std::str::from_utf8(line) else {
            return;
        };
        let Some((completed, total, speed)) = state.update(line.trim_end_matches('\r')) else {
            return;
        };
        let _ = self
            .db
            .checkpoint_progress_by_id(id, completed, total, speed);
        self.events.emit(
            "downloads:tick",
            serde_json::json!({"updates": [{
                "id": id, "gid": "", "name": "", "completed": completed,
                "total": total, "dl_speed": speed, "ul_speed": 0,
                "connections": 0, "num_seeders": 0, "status": "active"
            }]}),
        );
    }
}

fn remove_if_current(running: &Mutex<HashMap<i64, RunningTask>>, id: i64, token: u64) -> bool {
    let mut running = running.lock().unwrap_or_else(|error| error.into_inner());
    if running.get(&id).is_some_and(|task| task.token == token) {
        running.remove(&id);
        true
    } else {
        false
    }
}

fn validate_option_value(name: &str, value: &str, maximum: usize) -> Result<(), String> {
    if value.len() > maximum || value.starts_with('-') || value.chars().any(char::is_control) {
        return Err(format!("invalid yt-dlp {name}"));
    }
    Ok(())
}

pub(crate) fn validate_options(
    format_id: Option<&str>,
    options: Option<&MediaOptions>,
) -> Result<(), String> {
    if let Some(format_id) = format_id.filter(|value| !value.is_empty()) {
        validate_option_value("format", format_id, 256)?;
    }
    let Some(options) = options else {
        return Ok(());
    };
    validate_option_value("subtitle languages", &options.sub_langs, 256)?;
    if !options.audio_format.is_empty()
        && !matches!(options.audio_format.as_str(), "mp3" | "m4a" | "opus")
    {
        return Err("unsupported yt-dlp audio format".into());
    }
    Ok(())
}

impl Drop for YtDlp {
    fn drop(&mut self) {
        self.cancel_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_option_injection_and_unknown_audio_formats() {
        assert!(validate_options(Some("--proxy"), None).is_err());
        assert!(validate_options(
            None,
            Some(&MediaOptions {
                audio_only: true,
                audio_format: "wav".into(),
                ..Default::default()
            })
        )
        .is_err());
        assert!(validate_options(
            Some("137+140"),
            Some(&MediaOptions {
                audio_format: "opus".into(),
                sub_langs: "en.*,tr".into(),
                ..Default::default()
            })
        )
        .is_ok());
    }

    #[test]
    fn progress_accumulates_sequential_fragments() {
        let mut state = ProgressState::default();
        assert_eq!(state.update("dl:50|100|0|20"), Some((50, 100, 20)));
        assert_eq!(state.update("dl:100|100|0|10"), Some((100, 100, 10)));
        assert_eq!(state.update("dl:1|40|0|5"), Some((101, 140, 5)));
        assert_eq!(state.update("unrelated"), None);
    }
}
