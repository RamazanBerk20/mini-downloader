//! Single entry point for adding a download, from any source (UI command,
//! browser capture, clipboard, link grabber). Routes by kind to the right aria2
//! method, records the DB row, and returns its id.

use std::path::Path;

use serde_json::Value;

use minidl_core::aria2::{build_add_options, Engine, EngineDefaults};
use minidl_core::db::{Db, DownloadInsertResult};
use minidl_core::ipc::{CaptureJob, DownloadKind};
use minidl_core::model::{Download, DownloadStatus, NewDownload};

/// Whether an ingest created engine work or reused an existing download.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestOutcome {
    Added(i64),
    Existing(i64),
}

impl IngestOutcome {
    pub fn id(self) -> i64 {
        match self {
            Self::Added(id) | Self::Existing(id) => id,
        }
    }
}

fn kind_str(k: DownloadKind) -> &'static str {
    match k {
        DownloadKind::Http => "http",
        DownloadKind::Magnet => "magnet",
        DownloadKind::Torrent => "torrent",
        DownloadKind::Metalink => "metalink",
        DownloadKind::Hls => "hls",
        DownloadKind::Dash => "dash",
    }
}

/// Only fetch from schemes we intend to support. Blocks `file:`, `data:`,
/// `javascript:` etc. from being handed to aria2/yt-dlp as a source URL.
fn allowed_source_scheme(url: &str) -> bool {
    let u = url.trim_start().to_ascii_lowercase();
    ["http://", "https://", "ftp://", "ftps://", "sftp://"]
        .iter()
        .any(|p| u.starts_with(p))
        || u.starts_with("magnet:")
}

/// True if the URL's host is a loopback/private/link-local literal IP (or
/// `localhost`) — the SSRF surface an auto-captured job could aim at an intranet
/// service. Only enforced when the user opts in (LAN/NAS downloads are common).
fn host_is_private(url: &str) -> bool {
    use std::net::IpAddr;
    let host = url
        .splitn(2, "://")
        .nth(1)
        .and_then(|r| r.split(['/', '?', '#']).next())
        .map(|h| h.rsplit_once(':').map(|(a, _)| a).unwrap_or(h))
        .unwrap_or("");
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => {
            v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified()
        }
        Ok(IpAddr::V6(v6)) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || (v6.segments()[0] & 0xfe00) == 0xfc00 // unique-local
                || (v6.segments()[0] & 0xffc0) == 0xfe80 // link-local
        }
        Err(_) => false,
    }
}

/// Free bytes on the filesystem backing `dir`, if determinable.
#[cfg(unix)]
fn free_space(dir: &Path) -> Option<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c = CString::new(dir.as_os_str().as_bytes()).ok()?;
    // SAFETY: statvfs into a zeroed struct; pointer valid for the call.
    unsafe {
        let mut s: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c.as_ptr(), &mut s) == 0 {
            Some((s.f_bavail as u64).saturating_mul(s.f_frsize as u64))
        } else {
            None
        }
    }
}
#[cfg(not(unix))]
fn free_space(_dir: &Path) -> Option<u64> {
    None
}

fn extra_headers_json(job: &CaptureJob) -> Option<String> {
    if job.extra_headers.is_empty() {
        None
    } else {
        serde_json::to_string(&job.extra_headers).ok()
    }
}

/// Rebuild a `CaptureJob` from a stored row so retry/resume replay the original
/// request faithfully (cookies/UA/referer/headers/kind) instead of degrading to
/// a bare URL. Torrent/metalink payloads aren't persisted, so those retry as the
/// tracked URL only.
pub fn job_from_row(d: &Download) -> CaptureJob {
    let kind = match d.kind.as_str() {
        "magnet" => DownloadKind::Magnet,
        "torrent" => DownloadKind::Torrent,
        "metalink" => DownloadKind::Metalink,
        "hls" => DownloadKind::Hls,
        "dash" | "video" => DownloadKind::Dash,
        _ => DownloadKind::Http,
    };
    let extra_headers = d
        .extra_headers
        .as_deref()
        .and_then(|s| serde_json::from_str::<Vec<(String, String)>>(s).ok())
        .unwrap_or_default();
    CaptureJob {
        url: d.url.clone(),
        filename: d.filename.clone(),
        referrer: d.referrer.clone(),
        user_agent: d.user_agent.clone(),
        cookie: d.cookie.clone(),
        extra_headers,
        kind,
        mime: None,
        size: None,
        page_url: d.page_url.clone(),
        cookie_store_id: None,
        torrent_b64: None,
        batch_id: None,
        batch_name: None,
    }
}

/// Add a captured job, or return the matching existing row when duplicate
/// prevention is enabled. aria2 handles http/magnet/torrent/metalink; yt-dlp
/// handles HLS/DASH. `checksum` is a pre-normalized aria2 checksum value
/// (`sha-256=<hex>`), honored for HTTP only.
#[allow(clippy::too_many_arguments)]
pub async fn ingest(
    engine: &Engine,
    db: &Db,
    ytdlp: &crate::ytdlp::YtDlp,
    download_dir: &Path,
    defaults: EngineDefaults,
    job: CaptureJob,
    category_id: Option<i64>,
    package_id: Option<i64>,
    checksum: Option<String>,
) -> Result<IngestOutcome, String> {
    let dir = download_dir.to_string_lossy().to_string();
    // For streaming media the "url" we track/resume from is the page URL.
    let is_stream = matches!(job.kind, DownloadKind::Hls | DownloadKind::Dash);
    let target = if is_stream {
        job.page_url.clone().unwrap_or_else(|| job.url.clone())
    } else {
        job.url.clone()
    };

    // Fetch-target kinds must use an allowed scheme (torrent/metalink carry the
    // payload in `torrent_b64`, so their synthetic `file:` url is exempt).
    let fetches_url = matches!(
        job.kind,
        DownloadKind::Http | DownloadKind::Magnet | DownloadKind::Hls | DownloadKind::Dash
    );
    if fetches_url && !allowed_source_scheme(&target) {
        return Err("unsupported or unsafe URL scheme".to_string());
    }

    // Opt-in SSRF guard: block private/loopback targets (off by default so LAN
    // and NAS downloads keep working).
    if fetches_url && !target.starts_with("magnet:") {
        let block_private = db
            .get_setting("block_private_ips")
            .ok()
            .flatten()
            .map(|v| v == "true")
            .unwrap_or(false);
        if block_private && host_is_private(&target) {
            return Err("target host is a private/loopback address (blocked in settings)".to_string());
        }
    }

    // Fail fast when the known content size won't fit (plus a 64 MiB margin),
    // instead of erroring deep into the transfer with a half-written file.
    if let Some(size) = job.size {
        if size > 0 {
            if let Some(free) = free_space(download_dir) {
                if (size as u64).saturating_add(64 * 1024 * 1024) > free {
                    return Err(format!(
                        "insufficient disk space (need {} MB, have {} MB free)",
                        size / 1_000_000,
                        free / 1_000_000
                    ));
                }
            }
        }
    }

    let checksum = checksum.filter(|_| matches!(job.kind, DownloadKind::Http));
    let new_download = NewDownload {
        url: target.clone(),
        filename: job.filename.clone(),
        dir: dir.clone(),
        kind: kind_str(job.kind).into(),
        referrer: job.referrer.clone(),
        category_id,
        user_agent: job.user_agent.clone(),
        cookie: job.cookie.clone(),
        extra_headers: extra_headers_json(&job),
        page_url: job.page_url.clone(),
        format_id: None,
        package_id,
        mime: job.mime.clone(),
        checksum: checksum.clone(),
        media_opts: None,
    };
    let inserted = if fetches_url {
        db.insert_download_with_duplicate_policy(&new_download)
            .map_err(|e| e.to_string())?
    } else {
        DownloadInsertResult::Inserted(
            db.insert_download(&new_download)
                .map_err(|e| e.to_string())?,
        )
    };
    let id = inserted.id();
    if matches!(inserted, DownloadInsertResult::Existing(_)) {
        return Ok(IngestOutcome::Existing(id));
    }

    if is_stream {
        db.set_status(id, DownloadStatus::Active).map_err(|e| e.to_string())?;
        ytdlp.start(id, target, None, job.header_lines(), None);
        return Ok(IngestOutcome::Added(id));
    }

    let mut opts_map = build_add_options(&job, &dir, &defaults);
    // Apply the global default per-download speed cap (bytes/sec) if set.
    if let Some(limit) = db
        .get_setting("default_speed_limit")
        .ok()
        .flatten()
        .and_then(|s| s.parse::<i64>().ok())
        .filter(|n| *n > 0)
    {
        opts_map.insert("max-download-limit".into(), Value::String(limit.to_string()));
    }
    // aria2 verifies the finished file against this and fails the download
    // (error 32) on mismatch.
    if let Some(sum) = &checksum {
        opts_map.insert("checksum".into(), Value::String(sum.clone()));
    }
    let opts = Value::Object(opts_map);
    let result: anyhow::Result<Vec<String>> = match job.kind {
        DownloadKind::Http | DownloadKind::Magnet => {
            engine.rpc.add_uri(&[job.url.clone()], opts).await.map(|g| vec![g])
        }
        DownloadKind::Torrent => match &job.torrent_b64 {
            Some(b64) => engine.rpc.add_torrent(b64, &[], opts).await.map(|g| vec![g]),
            None => Err(anyhow::anyhow!("torrent job without payload")),
        },
        DownloadKind::Metalink => match &job.torrent_b64 {
            Some(b64) => engine.rpc.add_metalink(b64, opts).await,
            None => Err(anyhow::anyhow!("metalink job without payload")),
        },
        DownloadKind::Hls | DownloadKind::Dash => unreachable!(),
    };

    match result {
        Ok(gids) => {
            // A metalink can expand to several files, each with its own GID. The
            // row we already inserted tracks the first; give every *additional*
            // GID its own row so its progress is tracked, it can be paused, and it
            // survives reconcile. An empty GID list means aria2 accepted nothing —
            // fail the row instead of leaving it Active-with-no-GID (a permanent
            // zombie the poller can never map).
            let Some((first, rest)) = gids.split_first() else {
                let _ = db.set_error(id, None, Some("engine returned no download id"));
                return Err("engine returned no download id".into());
            };
            db.bind_aria2_job(id, first).map_err(|e| e.to_string())?;
            db.set_status(id, DownloadStatus::Active).map_err(|e| e.to_string())?;
            for g in rest {
                if let Ok(child_id) = db.insert_download(&NewDownload {
                    url: target.clone(),
                    dir: dir.clone(),
                    kind: kind_str(job.kind).into(),
                    referrer: job.referrer.clone(),
                    category_id,
                    package_id,
                    ..Default::default()
                }) {
                    let _ = db.bind_aria2_job(child_id, g);
                    let _ = db.set_status(child_id, DownloadStatus::Active);
                }
            }
            Ok(IngestOutcome::Added(id))
        }
        Err(e) => {
            let _ = db.set_error(id, None, Some(&e.to_string()));
            Err(e.to_string())
        }
    }
}

/// Re-add an existing row to aria2 (same DB id) with its original auth, binding
/// the new GID back to the row. Used by resume/retry when aria2 has forgotten
/// the GID. Only http/magnet can be reissued — torrent/metalink payloads aren't
/// persisted.
pub async fn reissue(
    engine: &Engine,
    db: &Db,
    defaults: EngineDefaults,
    row: &Download,
) -> Result<(), String> {
    let job = job_from_row(row);
    if !matches!(job.kind, DownloadKind::Http | DownloadKind::Magnet) {
        return Err("cannot reissue this download type".to_string());
    }
    let mut opts_map = build_add_options(&job, &row.dir, &defaults);
    if let (Some(sum), DownloadKind::Http) = (&row.checksum, job.kind) {
        opts_map.insert("checksum".into(), Value::String(sum.clone()));
    }
    let opts = Value::Object(opts_map);
    let gid = engine
        .rpc
        .add_uri(&[job.url.clone()], opts)
        .await
        .map_err(|e| e.to_string())?;
    db.bind_aria2_job(row.id, &gid).map_err(|e| e.to_string())?;
    Ok(())
}

/// Build an HTTP/magnet job from a bare URL (UI + clipboard path).
pub fn job_from_url(url: String) -> CaptureJob {
    let kind = if url.starts_with("magnet:") {
        DownloadKind::Magnet
    } else {
        DownloadKind::Http
    };
    CaptureJob {
        url,
        filename: None,
        referrer: None,
        user_agent: None,
        cookie: None,
        extra_headers: vec![],
        kind,
        mime: None,
        size: None,
        page_url: None,
        cookie_store_id: None,
        torrent_b64: None,
        batch_id: None,
        batch_name: None,
    }
}
