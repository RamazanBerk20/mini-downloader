//! yt-dlp probing. The download driver (process management + progress) lives in
//! the app, since it needs the event bus; probing is pure and lives here.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaInfo {
    pub title: String,
    pub formats: Vec<Format>,
}

/// Run `yt-dlp -J` and extract the title + selectable formats.
pub async fn probe(ytdlp: &Path, url: &str) -> Result<MediaInfo> {
    let out = tokio::process::Command::new(ytdlp)
        // `--` terminates option parsing so a URL beginning with `-` (e.g. a
        // pasted `--config-location=...`) is never treated as a yt-dlp flag.
        .args(["-J", "--no-warnings", "--no-playlist", "--", url])
        .output()
        .await
        .context("running yt-dlp -J")?;
    if !out.status.success() {
        anyhow::bail!("yt-dlp probe failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).context("parsing yt-dlp JSON")?;

    let title = v.get("title").and_then(|t| t.as_str()).unwrap_or("video").to_string();
    let mut formats = Vec::new();
    if let Some(arr) = v.get("formats").and_then(|f| f.as_array()) {
        for f in arr {
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
    Ok(MediaInfo { title, formats })
}
