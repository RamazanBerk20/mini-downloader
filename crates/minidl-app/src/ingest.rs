use std::path::Path;
use std::sync::Arc;

use minidl_core::aria2::{build_add_options, Engine, EngineDefaults};
use minidl_core::db::{Db, DownloadInsertResult};
use minidl_core::ipc::{CaptureJob, DownloadKind};
use minidl_core::model::{Download, DownloadStatus, NewDownload};
use serde_json::Value;

use crate::ytdlp::YtDlp;

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

fn kind_name(kind: DownloadKind) -> &'static str {
    match kind {
        DownloadKind::Http => "http",
        DownloadKind::Hls => "hls",
        DownloadKind::Dash => "dash",
        DownloadKind::Torrent => "torrent",
        DownloadKind::Magnet => "magnet",
        DownloadKind::Metalink => "metalink",
    }
}

pub(crate) const MAX_URL_LEN: usize = 16 * 1024;
pub(crate) const MAX_BATCH_ITEMS: usize = 5_000;
const MAX_FILENAME_LEN: usize = 1_024;
const MAX_USER_AGENT_LEN: usize = 4 * 1024;
const MAX_COOKIE_LEN: usize = 256 * 1024;
const MAX_HEADER_COUNT: usize = 256;
const MAX_HEADER_BYTES: usize = 512 * 1024;
const MAX_METADATA_B64: usize = 28 * 1024 * 1024;

fn is_standard_base64(value: &str) -> bool {
    if value.is_empty() || !value.len().is_multiple_of(4) {
        return false;
    }
    let padding = value.bytes().rev().take_while(|byte| *byte == b'=').count();
    if padding > 2 {
        return false;
    }
    let content_len = value.len() - padding;
    let bytes = value.as_bytes();
    bytes[..content_len]
        .iter()
        .copied()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/'))
        && bytes[content_len..].iter().all(|byte| *byte == b'=')
}

fn validate_text(name: &str, value: &str, maximum: usize) -> Result<(), String> {
    if value.len() > maximum {
        return Err(format!("{name} is too long"));
    }
    if value.chars().any(char::is_control) {
        return Err(format!("{name} contains a forbidden control character"));
    }
    Ok(())
}

fn validate_url(url: &str, schemes: &[&str], allow_magnet: bool) -> Result<(), String> {
    validate_text("URL", url, MAX_URL_LEN)?;
    if url.is_empty() || url.trim() != url || url.chars().any(char::is_control) {
        return Err("invalid URL".into());
    }
    let parsed = url::Url::parse(url).map_err(|_| "invalid URL".to_string())?;
    if allow_magnet && parsed.scheme().eq_ignore_ascii_case("magnet") {
        return parsed
            .query()
            .filter(|query| !query.is_empty())
            .map(|_| ())
            .ok_or_else(|| "invalid magnet URL".into());
    }
    if !schemes
        .iter()
        .any(|scheme| parsed.scheme().eq_ignore_ascii_case(scheme))
        || parsed.host().is_none()
    {
        return Err("unsupported or unsafe URL scheme".into());
    }
    Ok(())
}

fn is_non_public_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(ip) => {
            let [a, b, _, _] = ip.octets();
            a == 0
                || a == 10
                || a == 127
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && b == 0)
                || (a == 192 && b == 168)
                || (a == 198 && (b == 18 || b == 19))
                || a >= 224
                || ip.is_documentation()
        }
        std::net::IpAddr::V6(ip) => {
            let segments = ip.segments();
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                || segments[0] & 0xfe00 == 0xfc00 // unique-local
                || segments[0] & 0xffc0 == 0xfe80 // link-local
                || segments[0] & 0xffc0 == 0xfec0 // deprecated site-local
                || (segments[0] == 0x2001 && segments[1] == 0x0db8)
                || ip
                    .to_ipv4()
                    .map(std::net::IpAddr::V4)
                    .is_some_and(is_non_public_ip)
        }
    }
}

async fn ensure_public_target(raw: &str) -> Result<(), String> {
    use url::Host;

    let parsed = url::Url::parse(raw).map_err(|_| "invalid target URL".to_string())?;
    let host = parsed
        .host()
        .ok_or_else(|| "target URL has no host".to_string())?;
    let blocked = match host {
        Host::Ipv4(ip) => is_non_public_ip(ip.into()),
        Host::Ipv6(ip) => is_non_public_ip(ip.into()),
        Host::Domain(domain) => {
            let domain = domain.trim_end_matches('.');
            if domain.eq_ignore_ascii_case("localhost") || domain.ends_with(".localhost") {
                true
            } else {
                let port = parsed.port_or_known_default().unwrap_or(443);
                let resolved = tokio::net::lookup_host((domain, port))
                    .await
                    .map_err(|error| format!("could not resolve target host: {error}"))?
                    .collect::<Vec<_>>();
                if resolved.is_empty() {
                    return Err("target host resolved to no addresses".into());
                }
                resolved
                    .into_iter()
                    .any(|address| is_non_public_ip(address.ip()))
            }
        }
    };
    if blocked {
        Err(
            "target host resolves to a private, local, or reserved address (blocked in settings)"
                .into(),
        )
    } else {
        Ok(())
    }
}

pub(crate) async fn validate_media_target(db: &Db, url: &str) -> Result<(), String> {
    validate_url(url, &["http", "https"], false)?;
    if db
        .get_setting("block_private_ips")
        .ok()
        .flatten()
        .as_deref()
        == Some("true")
    {
        ensure_public_target(url).await?;
    }
    Ok(())
}

pub(crate) async fn validate_capture_job(db: &Db, job: &CaptureJob) -> Result<(), String> {
    validate_text("URL", &job.url, MAX_URL_LEN)?;
    if let Some(value) = &job.filename {
        validate_text("filename", value, MAX_FILENAME_LEN)?;
    }
    if let Some(value) = &job.referrer {
        validate_text("referrer", value, MAX_URL_LEN)?;
    }
    if let Some(value) = &job.user_agent {
        validate_text("user agent", value, MAX_USER_AGENT_LEN)?;
    }
    if let Some(value) = &job.cookie {
        validate_text("cookie header", value, MAX_COOKIE_LEN)?;
    }
    if let Some(value) = &job.page_url {
        validate_text("page URL", value, MAX_URL_LEN)?;
    }
    if let Some(value) = &job.cookie_store_id {
        validate_text("cookie store id", value, 256)?;
    }
    if let Some(value) = &job.mime {
        validate_text("content type", value, 1_024)?;
    }
    if let Some(value) = &job.batch_id {
        validate_text("batch id", value, 128)?;
    }
    if let Some(value) = &job.batch_name {
        validate_text("batch name", value, 256)?;
    }
    if job.extra_headers.len() > MAX_HEADER_COUNT {
        return Err("too many request headers".into());
    }
    let mut header_bytes = 0usize;
    for (name, value) in &job.extra_headers {
        header_bytes = header_bytes
            .saturating_add(name.len())
            .saturating_add(value.len());
        if name.is_empty()
            || !name.bytes().all(|byte| {
                byte.is_ascii_alphanumeric()
                    || matches!(
                        byte,
                        b'!' | b'#'
                            | b'$'
                            | b'%'
                            | b'&'
                            | b'\''
                            | b'*'
                            | b'+'
                            | b'-'
                            | b'.'
                            | b'^'
                            | b'_'
                            | b'`'
                            | b'|'
                            | b'~'
                    )
            })
            || value.chars().any(char::is_control)
        {
            return Err("invalid request header".into());
        }
    }
    if header_bytes > MAX_HEADER_BYTES {
        return Err("request headers are too large".into());
    }
    if job.size.is_some_and(|size| size < -1) {
        return Err("invalid content length".into());
    }
    if let Some(payload) = &job.torrent_b64 {
        if payload.len() > MAX_METADATA_B64 {
            return Err("torrent or metalink payload exceeds 28 MiB".into());
        }
        if !is_standard_base64(payload) {
            return Err("invalid torrent or metalink base64 payload".into());
        }
    }

    match job.kind {
        DownloadKind::Http => {
            validate_url(&job.url, &["http", "https", "ftp", "ftps", "sftp"], false)?;
            if db
                .get_setting("block_private_ips")
                .ok()
                .flatten()
                .as_deref()
                == Some("true")
            {
                ensure_public_target(&job.url).await?;
            }
        }
        DownloadKind::Magnet => validate_url(&job.url, &[], true)?,
        DownloadKind::Hls | DownloadKind::Dash => {
            validate_media_target(db, job.page_url.as_deref().unwrap_or(&job.url)).await?;
        }
        DownloadKind::Torrent | DownloadKind::Metalink => {
            if job.torrent_b64.is_none() {
                return Err("torrent or metalink job has no payload".into());
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
fn free_space(dir: &Path) -> Option<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let dir = CString::new(dir.as_os_str().as_bytes()).ok()?;
    // SAFETY: `statvfs` receives a valid C path and output pointer.
    unsafe {
        let mut value: libc::statvfs = std::mem::zeroed();
        (libc::statvfs(dir.as_ptr(), &mut value) == 0)
            .then(|| (value.f_bavail as u64).saturating_mul(value.f_frsize as u64))
    }
}

#[cfg(not(unix))]
fn free_space(_: &Path) -> Option<u64> {
    None
}

pub fn job_from_url(url: String) -> CaptureJob {
    CaptureJob {
        kind: if url.starts_with("magnet:") {
            DownloadKind::Magnet
        } else {
            DownloadKind::Http
        },
        url,
        filename: None,
        referrer: None,
        user_agent: None,
        cookie: None,
        extra_headers: Vec::new(),
        mime: None,
        size: None,
        page_url: None,
        cookie_store_id: None,
        torrent_b64: None,
        batch_id: None,
        batch_name: None,
    }
}

pub fn job_from_row(row: &Download) -> CaptureJob {
    let kind = match row.kind.as_str() {
        "magnet" => DownloadKind::Magnet,
        "torrent" => DownloadKind::Torrent,
        "metalink" => DownloadKind::Metalink,
        "hls" => DownloadKind::Hls,
        "dash" | "video" => DownloadKind::Dash,
        _ => DownloadKind::Http,
    };
    CaptureJob {
        url: row.url.clone(),
        filename: row.filename.clone(),
        referrer: row.referrer.clone(),
        user_agent: row.user_agent.clone(),
        cookie: row.cookie.clone(),
        extra_headers: row
            .extra_headers
            .as_deref()
            .and_then(|value| serde_json::from_str(value).ok())
            .unwrap_or_default(),
        kind,
        mime: row.mime.clone(),
        size: None,
        page_url: row.page_url.clone(),
        cookie_store_id: None,
        torrent_b64: None,
        batch_id: None,
        batch_name: None,
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn ingest(
    engine: Option<&Engine>,
    db: &Db,
    ytdlp: &Arc<YtDlp>,
    download_dir: &Path,
    defaults: EngineDefaults,
    job: CaptureJob,
    category_id: Option<i64>,
    package_id: Option<i64>,
    checksum: Option<String>,
) -> Result<IngestOutcome, String> {
    let stream = matches!(job.kind, DownloadKind::Hls | DownloadKind::Dash);
    if !stream && engine.is_none() {
        return Err("aria2 engine is required for this download type".into());
    }
    if stream && !ytdlp.is_available() {
        return Err("yt-dlp not available".into());
    }
    let target = if stream {
        job.page_url.clone().unwrap_or_else(|| job.url.clone())
    } else {
        job.url.clone()
    };
    let fetches = matches!(
        job.kind,
        DownloadKind::Http | DownloadKind::Magnet | DownloadKind::Hls | DownloadKind::Dash
    );
    if let (Some(size), Some(free)) = (job.size.filter(|size| *size > 0), free_space(download_dir))
    {
        if (size as u64).saturating_add(64 * 1024 * 1024) > free {
            return Err(format!(
                "insufficient disk space (need {} MB, have {} MB free)",
                size / 1_000_000,
                free / 1_000_000
            ));
        }
    }
    let checksum = checksum.filter(|_| matches!(job.kind, DownloadKind::Http));
    let dir = download_dir.to_string_lossy().to_string();
    let new_row = NewDownload {
        url: target.clone(),
        filename: job.filename.clone(),
        dir: dir.clone(),
        kind: kind_name(job.kind).into(),
        referrer: job.referrer.clone(),
        category_id,
        user_agent: job.user_agent.clone(),
        cookie: job.cookie.clone(),
        extra_headers: (!job.extra_headers.is_empty())
            .then(|| serde_json::to_string(&job.extra_headers).unwrap_or_default()),
        page_url: job.page_url.clone(),
        package_id,
        mime: job.mime.clone(),
        checksum: checksum.clone(),
        ..Default::default()
    };
    let inserted = if fetches {
        db.insert_download_with_duplicate_policy(&new_row)
    } else {
        db.insert_download(&new_row)
            .map(DownloadInsertResult::Inserted)
    }
    .map_err(|e| e.to_string())?;
    let id = inserted.id();
    if matches!(inserted, DownloadInsertResult::Existing(_)) {
        return Ok(IngestOutcome::Existing(id));
    }
    if stream {
        db.set_status(id, DownloadStatus::Active)
            .map_err(|e| e.to_string())?;
        ytdlp.start(id, target, None, job.header_lines(), None)?;
        return Ok(IngestOutcome::Added(id));
    }
    let engine = engine.expect("non-stream downloads were checked above");

    let mut options = build_add_options(&job, &dir, &defaults);
    if let Some(limit) = db
        .get_setting("default_speed_limit")
        .ok()
        .flatten()
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value > 0)
    {
        options.insert(
            "max-download-limit".into(),
            Value::String(limit.to_string()),
        );
    }
    if let Some(checksum) = checksum {
        options.insert("checksum".into(), Value::String(checksum));
    }
    if fetches
        && !target.to_ascii_lowercase().starts_with("magnet:")
        && db
            .get_setting("block_private_ips")
            .ok()
            .flatten()
            .as_deref()
            == Some("true")
    {
        // DNS is checked before insertion. Prevent aria2 from following a
        // public initial URL to a private destination afterward.
        options.insert("max-redirect".into(), Value::String("0".into()));
    }
    let options = Value::Object(options);
    let gids = match job.kind {
        DownloadKind::Http | DownloadKind::Magnet => engine
            .rpc
            .add_uri(std::slice::from_ref(&job.url), options)
            .await
            .map(|gid| vec![gid]),
        DownloadKind::Torrent => match job.torrent_b64.as_deref() {
            Some(payload) => engine
                .rpc
                .add_torrent(payload, &[], options)
                .await
                .map(|gid| vec![gid]),
            None => Err(anyhow::anyhow!("torrent job without payload")),
        },
        DownloadKind::Metalink => match job.torrent_b64.as_deref() {
            Some(payload) => engine.rpc.add_metalink(payload, options).await,
            None => Err(anyhow::anyhow!("metalink job without payload")),
        },
        DownloadKind::Hls | DownloadKind::Dash => unreachable!(),
    };
    match gids {
        Ok(gids) => {
            let Some((first, rest)) = gids.split_first() else {
                let _ = db.set_error(id, None, Some("engine returned no download id"));
                return Err("engine returned no download id".into());
            };
            db.bind_aria2_job(id, first).map_err(|e| e.to_string())?;
            db.set_status(id, DownloadStatus::Active)
                .map_err(|e| e.to_string())?;
            for gid in rest {
                if let Ok(child) = db.insert_download(&NewDownload {
                    url: target.clone(),
                    dir: dir.clone(),
                    kind: kind_name(job.kind).into(),
                    referrer: job.referrer.clone(),
                    category_id,
                    package_id,
                    ..Default::default()
                }) {
                    let _ = db.bind_aria2_job(child, gid);
                    let _ = db.set_status(child, DownloadStatus::Active);
                }
            }
            Ok(IngestOutcome::Added(id))
        }
        Err(error) => {
            let _ = db.set_error(id, None, Some(&error.to_string()));
            Err(error.to_string())
        }
    }
}

pub async fn reissue(
    engine: &Engine,
    db: &Db,
    defaults: EngineDefaults,
    row: &Download,
) -> Result<(), String> {
    let job = job_from_row(row);
    if !matches!(job.kind, DownloadKind::Http | DownloadKind::Magnet) {
        return Err("cannot reissue this download type".into());
    }
    validate_capture_job(db, &job).await?;
    let mut options = build_add_options(&job, &row.dir, &defaults);
    if matches!(job.kind, DownloadKind::Http) {
        if let Some(checksum) = &row.checksum {
            options.insert("checksum".into(), Value::String(checksum.clone()));
        }
        if db
            .get_setting("block_private_ips")
            .ok()
            .flatten()
            .as_deref()
            == Some("true")
        {
            options.insert("max-redirect".into(), Value::String("0".into()));
        }
    }
    let gid = engine
        .rpc
        .add_uri(std::slice::from_ref(&job.url), Value::Object(options))
        .await
        .map_err(|e| e.to_string())?;
    db.bind_aria2_job(row.id, &gid).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unsafe_schemes() {
        assert!(validate_url("file:///etc/passwd", &["http", "https"], false).is_err());
        assert!(validate_url("javascript:alert(1)", &["http", "https"], false).is_err());
        assert!(validate_url("https://example.org/file", &["http", "https"], false).is_ok());
        assert!(validate_url("magnet:?xt=urn:btih:abc", &[], true).is_ok());
        assert!(
            validate_url("https://example.org/\n--proxy=x", &["http", "https"], false).is_err()
        );
    }

    #[test]
    fn accepts_only_well_formed_standard_base64() {
        assert!(is_standard_base64("dGVzdA=="));
        assert!(is_standard_base64("YWJj"));
        assert!(!is_standard_base64(""));
        assert!(!is_standard_base64("QU=J"));
        assert!(!is_standard_base64("===="));
        assert!(!is_standard_base64("YWJj-_=="));
    }

    #[tokio::test]
    async fn detects_private_literal_hosts_and_userinfo_bypasses() {
        assert!(ensure_public_target("http://127.0.0.1/a").await.is_err());
        assert!(ensure_public_target("http://[::1]/a").await.is_err());
        assert!(ensure_public_target("http://user@127.0.0.1/a")
            .await
            .is_err());
        assert!(ensure_public_target("http://localhost./a").await.is_err());
        assert!(ensure_public_target("https://93.184.216.34/a")
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn validates_capture_shape_before_process_launch() {
        let db = Db::open_in_memory().unwrap();
        let mut job = job_from_url("https://example.org/file".into());
        assert!(validate_capture_job(&db, &job).await.is_ok());
        job.extra_headers = vec![("X-Test".into(), "ok\r\nInjected: yes".into())];
        assert!(validate_capture_job(&db, &job).await.is_err());
        job.extra_headers.clear();
        job.kind = DownloadKind::Magnet;
        assert!(validate_capture_job(&db, &job).await.is_err());
    }
}
