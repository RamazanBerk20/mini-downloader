//! yt-dlp download driver. Runs yt-dlp as a subprocess (using aria2c as its
//! downloader for speed), parses progress, and reports into the same event/DB
//! path as aria2 downloads (keyed by app id). Handles HLS/DASH + muxing, which
//! aria2 alone cannot.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use serde_json::json;
use tauri::{async_runtime::JoinHandle, AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use ldm_core::db::Db;
use ldm_core::model::DownloadStatus;
use ldm_core::ytdlp::MediaInfo;

use crate::events::{EV_COMPLETE, EV_ERROR, EV_STATE, EV_TICK};

fn which(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")
        .and_then(|p| std::env::split_paths(&p).map(|d| d.join(name)).find(|c| c.is_file()))
}

fn exe_dir_bin(name: &str) -> Option<PathBuf> {
    let cand = std::env::current_exe().ok()?.parent()?.join(name);
    cand.is_file().then_some(cand)
}

pub struct YtDlp {
    app: AppHandle,
    db: Db,
    download_dir: PathBuf,
    ytdlp: Option<PathBuf>,
    ffmpeg: Option<PathBuf>,
    running: Arc<Mutex<HashMap<i64, JoinHandle<()>>>>,
}

impl YtDlp {
    pub fn resolve(app: AppHandle, db: Db, download_dir: PathBuf) -> Self {
        // Prefer a user-writable yt-dlp copy (self-updatable), then the bundled
        // sidecar next to the app, then PATH.
        let user = ldm_core::paths::bin_dir().join("yt-dlp");
        let ytdlp = if user.is_file() {
            Some(user)
        } else {
            exe_dir_bin("yt-dlp").or_else(|| which("yt-dlp"))
        };
        let ffmpeg = exe_dir_bin("ffmpeg").or_else(|| which("ffmpeg"));
        Self {
            app,
            db,
            download_dir,
            ytdlp,
            ffmpeg,
            running: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    #[allow(dead_code)]
    pub fn is_available(&self) -> bool {
        self.ytdlp.is_some()
    }

    pub async fn probe(&self, url: &str) -> Result<MediaInfo, String> {
        let bin = self.ytdlp.clone().ok_or("yt-dlp not found")?;
        ldm_core::ytdlp::probe(&bin, url).await.map_err(|e| e.to_string())
    }

    /// Start a download for an existing DB row.
    pub fn start(&self, id: i64, url: String, format_id: Option<String>) {
        let Some(bin) = self.ytdlp.clone() else {
            let _ = self.db.set_error(id, None, Some("yt-dlp not available"));
            let _ = self.app.emit(EV_ERROR, json!({ "id": id, "message": "yt-dlp not available" }));
            let _ = self.app.emit(EV_STATE, json!({ "id": id, "status": "error" }));
            return;
        };
        let app = self.app.clone();
        let db = self.db.clone();
        let dir = self.download_dir.clone();
        let ffmpeg = self.ffmpeg.clone();
        let running = self.running.clone();
        let jh = tauri::async_runtime::spawn(async move {
            run(app, db, running.clone(), bin, ffmpeg, dir, id, url, format_id).await;
        });
        self.running.lock().unwrap().insert(id, jh);
    }

    /// Abort a running yt-dlp download (kills the process via kill_on_drop).
    pub fn cancel(&self, id: i64) -> bool {
        if let Some(jh) = self.running.lock().unwrap().remove(&id) {
            jh.abort();
            true
        } else {
            false
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run(
    app: AppHandle,
    db: Db,
    running: Arc<Mutex<HashMap<i64, JoinHandle<()>>>>,
    bin: PathBuf,
    ffmpeg: Option<PathBuf>,
    dir: PathBuf,
    id: i64,
    url: String,
    format_id: Option<String>,
) {
    let name_file = std::env::temp_dir().join(format!("ldm-ytdlp-{id}.name"));
    let _ = std::fs::remove_file(&name_file);
    let out_tmpl = format!("{}/%(title)s.%(ext)s", dir.display());

    let mut cmd = Command::new(&bin);
    cmd.arg("--newline")
        .arg("--no-playlist")
        .arg("--progress-template")
        .arg("dl:%(progress.downloaded_bytes)s|%(progress.total_bytes)s|%(progress.total_bytes_estimate)s|%(progress.speed)s")
        .arg("--print-to-file")
        .arg("after_move:filepath")
        .arg(&name_file)
        .arg("-o")
        .arg(&out_tmpl)
        .arg("-f")
        .arg(format_id.unwrap_or_else(|| "bv*+ba/b".into()));
    if let Some(ff) = &ffmpeg {
        cmd.arg("--ffmpeg-location").arg(ff);
    }
    cmd.arg("--downloader")
        .arg("aria2c")
        .arg("--downloader-args")
        .arg("aria2c:-x16 -s16 -k1M")
        .arg(&url)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            running.lock().unwrap().remove(&id);
            let _ = db.set_error(id, None, Some(&format!("spawn yt-dlp: {e}")));
            let _ = app.emit(EV_ERROR, json!({ "id": id, "message": e.to_string() }));
            let _ = app.emit(EV_STATE, json!({ "id": id, "status": "error" }));
            return;
        }
    };

    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();

    // Drain stderr concurrently (avoid pipe backpressure); keep last few KB.
    let errbuf = Arc::new(Mutex::new(String::new()));
    {
        let errbuf = errbuf.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                let mut b = errbuf.lock().unwrap();
                if b.len() < 4000 {
                    b.push_str(&l);
                    b.push('\n');
                }
            }
        });
    }

    let mut lines = BufReader::new(stdout).lines();
    let mut last_completed = 0i64;
    let mut last_total = 0i64;
    while let Ok(Some(line)) = lines.next_line().await {
        let Some(rest) = line.strip_prefix("dl:") else {
            continue;
        };
        let p: Vec<&str> = rest.split('|').collect();
        let num = |s: &str| s.trim().parse::<f64>().ok().map(|v| v as i64);
        let completed = p.first().copied().and_then(num).unwrap_or(last_completed);
        let total = p
            .get(1)
            .copied()
            .and_then(num)
            .or_else(|| p.get(2).copied().and_then(num))
            .unwrap_or(last_total);
        let speed = p.get(3).copied().and_then(num).unwrap_or(0);
        last_completed = completed;
        last_total = last_total.max(total);
        let _ = db.checkpoint_progress_by_id(id, last_completed, last_total, speed);
        let _ = app.emit(
            EV_TICK,
            json!({ "updates": [{
                "id": id, "gid": "", "name": "",
                "completed": last_completed, "total": last_total,
                "dl_speed": speed, "ul_speed": 0, "connections": 0, "num_seeders": 0,
                "status": "active"
            }] }),
        );
    }

    let status = child.wait().await;
    running.lock().unwrap().remove(&id);

    if status.map(|s| s.success()).unwrap_or(false) {
        let fpath = std::fs::read_to_string(&name_file)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let base = fpath
            .as_ref()
            .and_then(|p| std::path::Path::new(p).file_name())
            .and_then(|n| n.to_str())
            .map(String::from);
        if let Some(b) = &base {
            let _ = db.set_filename(id, b);
        }
        let _ = db.set_status(id, DownloadStatus::Complete);
        let _ = app.emit(EV_COMPLETE, json!({ "id": id, "name": base, "path": fpath }));
        let _ = app.emit(EV_STATE, json!({ "id": id, "status": "complete" }));
    } else {
        let msg = {
            let b = errbuf.lock().unwrap();
            b.lines()
                .rev()
                .find(|l| l.contains("ERROR"))
                .unwrap_or("yt-dlp failed")
                .to_string()
        };
        let _ = db.set_error(id, None, Some(&msg));
        let _ = app.emit(EV_ERROR, json!({ "id": id, "message": msg }));
        let _ = app.emit(EV_STATE, json!({ "id": id, "status": "error" }));
    }
    let _ = std::fs::remove_file(&name_file);
}
