//! yt-dlp probing. The download driver (process management + progress) lives in
//! the app, since it needs the event bus; probing is pure and lives here.

use std::io::Write;
use std::path::Path;
use std::process::Stdio;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
const MAX_PROBE_STDOUT: usize = 32 * 1024 * 1024;
const MAX_PROBE_STDERR: usize = 1024 * 1024;
const MAX_PROBE_ITEMS: usize = 5_000;

/// Keeps a 0600 yt-dlp configuration file alive until the child has parsed it.
/// Credentials belong here rather than in argv, which is commonly readable via
/// `/proc/<pid>/cmdline` and process-listing tools.
pub struct PrivateConfig {
    _file: tempfile::NamedTempFile,
}

fn quoted_config_value(value: &str) -> Result<String> {
    if value.contains(['\r', '\n', '\0']) {
        anyhow::bail!("yt-dlp option contains a forbidden control character");
    }
    Ok(format!(
        "\"{}\"",
        value.replace('\\', "\\\\").replace('\"', "\\\"")
    ))
}

/// Configure secret options through a private file and URLs through stdin.
/// The returned guard must remain alive until the child exits.
pub fn configure_private_inputs(
    command: &mut tokio::process::Command,
    headers: &[String],
    proxy: Option<&str>,
) -> Result<PrivateConfig> {
    let mut file = tempfile::Builder::new()
        .prefix("minidl-ytdlp-")
        .suffix(".conf")
        .tempfile()
        .context("create private yt-dlp config")?;
    for header in headers {
        writeln!(file, "--add-headers {}", quoted_config_value(header)?)
            .context("write private yt-dlp headers")?;
    }
    if let Some(proxy) = proxy.filter(|value| !value.is_empty()) {
        writeln!(file, "--proxy {}", quoted_config_value(proxy)?)
            .context("write private yt-dlp proxy")?;
    }
    file.flush().context("flush private yt-dlp config")?;
    file.as_file()
        .sync_all()
        .context("sync private yt-dlp config")?;

    command
        .arg("--ignore-config")
        .arg("--config-locations")
        .arg(file.path())
        .arg("--batch-file")
        .arg("-")
        .stdin(Stdio::piped());
    Ok(PrivateConfig { _file: file })
}

/// Write exactly one URL to yt-dlp's stdin-backed batch file.
pub async fn write_batch_url(child: &mut tokio::process::Child, url: &str) -> Result<()> {
    if url.contains(['\r', '\n', '\0']) {
        anyhow::bail!("URL contains a forbidden control character");
    }
    let mut stdin = child.stdin.take().context("yt-dlp stdin unavailable")?;
    stdin
        .write_all(url.as_bytes())
        .await
        .context("write URL to yt-dlp")?;
    stdin.write_all(b"\n").await.context("finish yt-dlp URL")?;
    stdin.shutdown().await.context("close yt-dlp stdin")?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Format {
    pub format_id: String,
    pub ext: String,
    pub resolution: String,
    pub vcodec: String,
    pub acodec: String,
    pub filesize: i64,
    pub protocol: String,
    pub note: String,
    pub height: i64,
}

/// One entry of a probed playlist (from `--flat-playlist`, so only the shallow
/// metadata is known — formats are resolved when the entry is downloaded).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaylistEntry {
    pub id: String,
    pub title: String,
    pub url: String,
    /// Seconds, 0 when unknown.
    pub duration: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaInfo {
    pub title: String,
    pub formats: Vec<Format>,
    /// `"video"` or `"playlist"` (yt-dlp's `_type`).
    #[serde(default = "default_media_kind")]
    pub kind: String,
    /// Populated only when `kind == "playlist"`.
    #[serde(default)]
    pub entries: Vec<PlaylistEntry>,
}

fn default_media_kind() -> String {
    "video".into()
}

/// Run `yt-dlp -J` and extract the title + selectable formats. `headers` are
/// `"Name: value"` lines replayed via `--add-header` so members-only/authed
/// pages probe correctly (empty for a plain user-entered URL). With `playlist`
/// the URL is probed as a flat playlist (fast, no per-entry format resolution);
/// a plain video probed that way still returns a normal format list.
pub async fn probe(
    ytdlp: &Path,
    url: &str,
    headers: &[String],
    playlist: bool,
) -> Result<MediaInfo> {
    let mut cmd = tokio::process::Command::new(ytdlp);
    crate::child_process::hide_console_window(cmd.as_std_mut());
    if playlist {
        cmd.args(["-J", "--no-warnings", "--flat-playlist"]);
    } else {
        cmd.args(["-J", "--no-warnings", "--no-playlist"]);
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let _private_config = configure_private_inputs(&mut cmd, headers, None)?;
    cmd.kill_on_drop(true);
    let mut child = cmd.spawn().context("running yt-dlp -J")?;
    if let Err(error) = write_batch_url(&mut child, url).await {
        let _ = child.kill().await;
        return Err(error);
    }
    let stdout = child.stdout.take().context("yt-dlp stdout unavailable")?;
    let stderr = child.stderr.take().context("yt-dlp stderr unavailable")?;
    let stdout_task = tokio::spawn(read_bounded(stdout, MAX_PROBE_STDOUT));
    let stderr_task = tokio::spawn(read_bounded(stderr, MAX_PROBE_STDERR));
    let status = match tokio::time::timeout(PROBE_TIMEOUT, child.wait()).await {
        Ok(status) => status.context("waiting for yt-dlp -J")?,
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            let _ = stdout_task.await;
            let _ = stderr_task.await;
            anyhow::bail!("yt-dlp probe timed out after 120 seconds");
        }
    };
    let (stdout, stdout_overflow) = stdout_task.await.context("join yt-dlp stdout reader")??;
    let (stderr, stderr_overflow) = stderr_task.await.context("join yt-dlp stderr reader")??;
    if stdout_overflow {
        anyhow::bail!("yt-dlp probe output exceeded 32 MiB");
    }
    if !status.success() {
        anyhow::bail!("yt-dlp probe failed: {}", String::from_utf8_lossy(&stderr));
    }
    if stderr_overflow {
        anyhow::bail!("yt-dlp probe diagnostics exceeded 1 MiB");
    }
    let v: serde_json::Value = serde_json::from_slice(&stdout).context("parsing yt-dlp JSON")?;
    Ok(parse_probe_json(&v))
}

/// Drain a child pipe completely so the child cannot deadlock on a full pipe,
/// while retaining at most `limit` bytes in memory.
async fn read_bounded(
    mut reader: impl AsyncRead + Unpin + Send + 'static,
    limit: usize,
) -> Result<(Vec<u8>, bool)> {
    let mut retained = Vec::new();
    let mut overflow = false;
    let mut chunk = [0u8; 16 * 1024];
    loop {
        let read = reader
            .read(&mut chunk)
            .await
            .context("read yt-dlp output")?;
        if read == 0 {
            break;
        }
        let available = limit.saturating_sub(retained.len());
        let keep = available.min(read);
        retained.extend_from_slice(&chunk[..keep]);
        overflow |= keep < read;
    }
    Ok((retained, overflow))
}

fn parse_probe_json(v: &serde_json::Value) -> MediaInfo {
    let title = v
        .get("title")
        .and_then(|t| t.as_str())
        .unwrap_or("video")
        .to_string();

    if v.get("_type").and_then(|t| t.as_str()) == Some("playlist") {
        let mut entries = Vec::new();
        if let Some(arr) = v.get("entries").and_then(|e| e.as_array()) {
            for e in arr.iter().take(MAX_PROBE_ITEMS) {
                let s = |k: &str| e.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
                let url = {
                    let u = s("url");
                    if u.is_empty() {
                        s("webpage_url")
                    } else {
                        u
                    }
                };
                if url.is_empty() {
                    continue;
                }
                entries.push(PlaylistEntry {
                    id: s("id"),
                    title: {
                        let t = s("title");
                        if t.is_empty() {
                            "video".into()
                        } else {
                            t
                        }
                    },
                    url,
                    duration: e.get("duration").and_then(|x| x.as_f64()).unwrap_or(0.0) as i64,
                });
            }
        }
        return MediaInfo {
            title,
            formats: Vec::new(),
            kind: "playlist".into(),
            entries,
        };
    }
    let mut formats = Vec::new();
    if let Some(arr) = v.get("formats").and_then(|f| f.as_array()) {
        for f in arr.iter().take(MAX_PROBE_ITEMS) {
            let s = |k: &str| f.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
            let height = f.get("height").and_then(|x| x.as_i64()).unwrap_or(0);
            let filesize = f
                .get("filesize")
                .and_then(|x| x.as_i64())
                .or_else(|| f.get("filesize_approx").and_then(|x| x.as_i64()))
                .unwrap_or(0);
            let resolution = {
                let r = s("resolution");
                if !r.is_empty() {
                    r
                } else if height > 0 {
                    format!("{height}p")
                } else {
                    "audio".into()
                }
            };
            formats.push(Format {
                format_id: s("format_id"),
                ext: s("ext"),
                resolution,
                vcodec: s("vcodec"),
                acodec: s("acodec"),
                filesize,
                protocol: s("protocol"),
                note: s("format_note"),
                height,
            });
        }
    }
    MediaInfo {
        title,
        formats,
        kind: "video".into(),
        entries: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_flat_playlist_json() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{
                "_type": "playlist",
                "title": "My list",
                "entries": [
                    {"id": "a1", "title": "First", "url": "https://y/watch?v=a1", "duration": 61.5},
                    {"id": "a2", "title": "", "webpage_url": "https://y/watch?v=a2"},
                    {"id": "a3"}
                ]
            }"#,
        )
        .unwrap();
        let info = parse_probe_json(&v);
        assert_eq!(info.kind, "playlist");
        assert_eq!(info.title, "My list");
        assert!(info.formats.is_empty());
        // The entry with no URL at all is dropped.
        assert_eq!(info.entries.len(), 2);
        assert_eq!(info.entries[0].url, "https://y/watch?v=a1");
        assert_eq!(info.entries[0].duration, 61);
        assert_eq!(info.entries[1].title, "video");
        assert_eq!(info.entries[1].url, "https://y/watch?v=a2");
    }

    #[test]
    fn parses_plain_video_json() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{
                "title": "Clip",
                "formats": [
                    {"format_id": "22", "ext": "mp4", "height": 720, "filesize": 1000,
                     "vcodec": "avc1", "acodec": "mp4a", "protocol": "https", "format_note": "hd"}
                ]
            }"#,
        )
        .unwrap();
        let info = parse_probe_json(&v);
        assert_eq!(info.kind, "video");
        assert!(info.entries.is_empty());
        assert_eq!(info.formats.len(), 1);
        assert_eq!(info.formats[0].resolution, "720p");
    }
}
