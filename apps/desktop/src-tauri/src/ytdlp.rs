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

use minidl_core::db::Db;
use minidl_core::model::DownloadStatus;
use minidl_core::ytdlp::MediaInfo;

use crate::events::{EV_COMPLETE, EV_ERROR, EV_STATE, EV_TICK};

/// RAII guard that kills the yt-dlp process group on drop. Aborting the driver
/// task unwinds its stack, dropping this guard, so a cancelled download takes its
/// aria2c grandchild down with it instead of orphaning it to keep downloading.
#[cfg(unix)]
struct ProcessGroupKiller {
    pgid: Option<i32>,
}

#[cfg(unix)]
impl ProcessGroupKiller {
    fn disarm(&mut self) {
        self.pgid = None;
    }
}

#[cfg(unix)]
impl Drop for ProcessGroupKiller {
    fn drop(&mut self) {
        if let Some(pgid) = self.pgid {
            // SAFETY: killpg with a valid pgid; ESRCH (already gone) is harmless.
            unsafe {
                libc::killpg(pgid, libc::SIGKILL);
            }
        }
    }
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
        // sidecar next to the app, then PATH (via the shared resolver).
        let user = minidl_core::paths::bin_dir().join("yt-dlp");
        let ytdlp = if user.is_file() {
            Some(user)
        } else {
            minidl_core::paths::resolve_tool("yt-dlp")
        };
        let ffmpeg = minidl_core::paths::resolve_tool("ffmpeg");
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

    pub async fn probe(&self, url: &str, headers: &[String]) -> Result<MediaInfo, String> {
        let bin = self.ytdlp.clone().ok_or("yt-dlp not found")?;
        minidl_core::ytdlp::probe(&bin, url, headers).await.map_err(|e| e.to_string())
    }

    /// Start a download for an existing DB row. `headers` are `"Name: value"`
    /// replay lines (cookies/UA/referer) so authed streams keep working across
    /// resume/restart; `format_id` pins the chosen quality.
    pub fn start(&self, id: i64, url: String, format_id: Option<String>, headers: Vec<String>) {
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
        // Hold the lock across abort-old + spawn + insert. This (a) aborts any
        // task already running for this id — double-clicking Resume must not
        // launch two yt-dlp/aria2c processes on the same output file — and (b)
        // inserts the handle before the task can remove itself (the task takes
        // the same lock), closing the insert-after-spawn leak race.
        let mut guard = self.running.lock().unwrap();
        if let Some(old) = guard.remove(&id) {
            old.abort();
        }
        let jh = tauri::async_runtime::spawn(async move {
            run(app, db, running.clone(), bin, ffmpeg, dir, id, url, format_id, headers).await;
        });
        guard.insert(id, jh);
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

    /// Abort every running yt-dlp download — called on app shutdown so no
    /// yt-dlp/aria2c child is left orphaned.
    pub fn cancel_all(&self) {
        let mut map = self.running.lock().unwrap();
        for (_, jh) in map.drain() {
            jh.abort();
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
    headers: Vec<String>,
) {
    let name_file = std::env::temp_dir().join(format!("minidl-ytdlp-{id}.name"));
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
    // Replay captured cookies/UA/referer so authed or members-only streams keep
    // working across resume/restart.
    for h in &headers {
        cmd.arg("--add-header").arg(h);
    }
    // Route through the configured proxy, if any (mirrors aria2's all-proxy).
    if let Some(proxy) = db.get_setting("proxy").ok().flatten().filter(|s| !s.is_empty()) {
        cmd.arg("--proxy").arg(proxy);
    }
    cmd.arg("--downloader")
        .arg("aria2c")
        .arg("--downloader-args")
        .arg("aria2c:-x16 -s16 -k1M")
        // `--` so a URL beginning with `-` can't be parsed as a yt-dlp option.
        .arg("--")
        .arg(&url)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // Own process group so cancel kills the whole tree (yt-dlp + its aria2c
    // grandchild), not just the yt-dlp parent.
    #[cfg(unix)]
    cmd.process_group(0);
    // Also die if the app dies abruptly (e.g. std::process::exit, where Drop
    // guards never run): the kernel sends SIGKILL when our parent exits.
    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL as libc::c_ulong);
            Ok(())
        });
    }

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

    // With `process_group(0)` the child's pgid equals its pid. If this task is
    // aborted (cancel/pause), the guard's Drop kills the whole group.
    #[cfg(unix)]
    let mut group_killer = ProcessGroupKiller { pgid: child.id().map(|p| p as i32) };

    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();

    // Drain stderr concurrently (avoid pipe backpressure); keep the LAST ~8 KB so
    // the trailing `ERROR:` line (the real cause) is not lost once the head fills.
    let errbuf = Arc::new(Mutex::new(String::new()));
    let stderr_task = {
        let errbuf = errbuf.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                let mut b = errbuf.lock().unwrap();
                b.push_str(&l);
                b.push('\n');
                if b.len() > 8192 {
                    let mut cut = b.len() - 8192;
                    while cut < b.len() && !b.is_char_boundary(cut) {
                        cut += 1;
                    }
                    b.drain(..cut);
                }
            }
        })
    };

    let mut lines = BufReader::new(stdout).lines();
    // `bv*+ba` downloads streams sequentially, each reporting its own progress
    // from ~0. Bank each finished stream's bytes into a base so the reported
    // progress accumulates instead of jumping backward when the next stream
    // starts. Totals are summed as they become known.
    let mut base_completed = 0i64;
    let mut base_total = 0i64;
    let mut seg_completed = 0i64;
    let mut seg_total = 0i64;
    while let Ok(Some(line)) = lines.next_line().await {
        let Some(rest) = line.strip_prefix("dl:") else {
            continue;
        };
        let p: Vec<&str> = rest.split('|').collect();
        let num = |s: &str| s.trim().parse::<f64>().ok().map(|v| v as i64);
        let completed = p.first().copied().and_then(num).unwrap_or(seg_completed);
        let total = p
            .get(1)
            .copied()
            .and_then(num)
            .or_else(|| p.get(2).copied().and_then(num))
            .unwrap_or(seg_total);
        let speed = p.get(3).copied().and_then(num).unwrap_or(0);
        // A large backward jump means the previous stream finished and a new one
        // began reporting from ~0 — bank the finished stream.
        if completed + 1 < seg_completed {
            base_completed += seg_completed;
            base_total += seg_total.max(seg_completed);
            seg_total = 0;
        }
        seg_completed = completed;
        seg_total = seg_total.max(total);
        let cum_completed = base_completed + seg_completed;
        let cum_total = (base_total + seg_total).max(cum_completed);
        let _ = db.checkpoint_progress_by_id(id, cum_completed, cum_total, speed);
        let _ = app.emit(
            EV_TICK,
            json!({ "updates": [{
                "id": id, "gid": "", "name": "",
                "completed": cum_completed, "total": cum_total,
                "dl_speed": speed, "ul_speed": 0, "connections": 0, "num_seeders": 0,
                "status": "active"
            }] }),
        );
    }

    let status = child.wait().await;
    // Join the stderr drain so the final ERROR line is present before we read it.
    let _ = stderr_task.await;
    // Child has exited — don't let the guard kill a possibly-reused pgid.
    #[cfg(unix)]
    group_killer.disarm();
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
        // Mark-of-the-web: record where the media came from.
        #[cfg(unix)]
        if let Some(p) = &fpath {
            let _ = xattr::set(p, "user.xdg.origin.url", url.as_bytes());
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
