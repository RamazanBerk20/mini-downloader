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
pub async fn probe(ytdlp: &Path, url: &str, headers: &[String], playlist: bool) -> Result<MediaInfo> {
    let mut cmd = tokio::process::Command::new(ytdlp);
    if playlist {
        cmd.args(["-J", "--no-warnings", "--flat-playlist"]);
    } else {
        cmd.args(["-J", "--no-warnings", "--no-playlist"]);
    }
    for h in headers {
        cmd.arg("--add-header").arg(h);
    }
    // `--` terminates option parsing so a URL beginning with `-` (e.g. a pasted
    // `--config-location=...`) is never treated as a yt-dlp flag.
    let out = cmd
        .arg("--")
        .arg(url)
        .output()
        .await
        .context("running yt-dlp -J")?;
    if !out.status.success() {
        anyhow::bail!("yt-dlp probe failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).context("parsing yt-dlp JSON")?;
    Ok(parse_probe_json(&v))
}

fn parse_probe_json(v: &serde_json::Value) -> MediaInfo {
    let title = v.get("title").and_then(|t| t.as_str()).unwrap_or("video").to_string();

    if v.get("_type").and_then(|t| t.as_str()) == Some("playlist") {
        let mut entries = Vec::new();
        if let Some(arr) = v.get("entries").and_then(|e| e.as_array()) {
            for e in arr {
                let s = |k: &str| e.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
                let url = {
                    let u = s("url");
                    if u.is_empty() { s("webpage_url") } else { u }
                };
                if url.is_empty() {
                    continue;
                }
                entries.push(PlaylistEntry {
                    id: s("id"),
                    title: {
                        let t = s("title");
                        if t.is_empty() { "video".into() } else { t }
                    },
                    url,
                    duration: e.get("duration").and_then(|x| x.as_f64()).unwrap_or(0.0) as i64,
                });
            }
        }
        return MediaInfo { title, formats: Vec::new(), kind: "playlist".into(), entries };
    }
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
    MediaInfo { title, formats, kind: "video".into(), entries: Vec::new() }
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
