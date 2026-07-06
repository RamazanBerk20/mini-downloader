//! Single entry point for adding a download, from any source (UI command,
//! browser capture, clipboard, link grabber). Routes by kind to the right aria2
//! method, records the DB row, and returns its id.

use std::path::Path;

use serde_json::Value;

use ldm_core::aria2::{build_add_options, Engine, EngineDefaults};
use ldm_core::db::Db;
use ldm_core::ipc::{CaptureJob, DownloadKind};
use ldm_core::model::{DownloadStatus, NewDownload};

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

/// Add a captured job. Returns the stable DB id on success. aria2 handles
/// http/magnet/torrent/metalink; yt-dlp handles HLS/DASH.
pub async fn ingest(
    engine: &Engine,
    db: &Db,
    ytdlp: &crate::ytdlp::YtDlp,
    download_dir: &Path,
    defaults: &EngineDefaults,
    job: CaptureJob,
    category_id: Option<i64>,
) -> Result<i64, String> {
    let dir = download_dir.to_string_lossy().to_string();
    // For streaming media the "url" we track/resume from is the page URL.
    let is_stream = matches!(job.kind, DownloadKind::Hls | DownloadKind::Dash);
    let target = if is_stream {
        job.page_url.clone().unwrap_or_else(|| job.url.clone())
    } else {
        job.url.clone()
    };

    let id = db
        .insert_download(&NewDownload {
            url: target.clone(),
            filename: job.filename.clone(),
            dir: dir.clone(),
            kind: kind_str(job.kind).into(),
            referrer: job.referrer.clone(),
            category_id,
        })
        .map_err(|e| e.to_string())?;

    if is_stream {
        db.set_status(id, DownloadStatus::Active).map_err(|e| e.to_string())?;
        ytdlp.start(id, target, None);
        return Ok(id);
    }

    let opts = Value::Object(build_add_options(&job, &dir, defaults));
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
            if let Some(g) = gids.first() {
                db.set_gid(id, g).map_err(|e| e.to_string())?;
            }
            db.set_status(id, DownloadStatus::Active).map_err(|e| e.to_string())?;
            Ok(id)
        }
        Err(e) => {
            let _ = db.set_error(id, None, Some(&e.to_string()));
            Err(e.to_string())
        }
    }
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
    }
}
