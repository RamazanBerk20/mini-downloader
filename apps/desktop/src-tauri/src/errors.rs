//! Typed error surfaced to the frontend. Serializes to `{ kind, message }` so
//! the UI can react to actionable failures (e.g. offer to install yt-dlp)
//! instead of only showing a flat string.

use serde::Serialize;

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorKind {
    YtDlpMissing,
    EngineUnavailable,
    NotFound,
    DiskSpace,
    Rejected,
    Other,
}

#[derive(Debug, Clone, Serialize)]
pub struct CommandError {
    pub kind: ErrorKind,
    pub message: String,
}

impl CommandError {
    /// Best-effort classification of a string error into a kind, so existing
    /// `?`/`.map_err` sites keep working while still tagging the common
    /// actionable cases the UI can react to.
    fn classify(message: String) -> Self {
        let m = message.to_ascii_lowercase();
        let kind = if m.contains("yt-dlp") && (m.contains("not available") || m.contains("not found")) {
            ErrorKind::YtDlpMissing
        } else if m.contains("disk space") {
            ErrorKind::DiskSpace
        } else if m.contains("request failed") || m.contains("did not become ready") || m.contains("recover") {
            ErrorKind::EngineUnavailable
        } else if m.contains("not found") || m.contains("vanished") {
            ErrorKind::NotFound
        } else if m.contains("unsupported") || m.contains("unsafe") || m.contains("blocked") {
            ErrorKind::Rejected
        } else {
            ErrorKind::Other
        };
        Self { kind, message }
    }
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for CommandError {}

impl From<String> for CommandError {
    fn from(s: String) -> Self {
        Self::classify(s)
    }
}
impl From<&str> for CommandError {
    fn from(s: &str) -> Self {
        Self::classify(s.to_string())
    }
}
impl From<anyhow::Error> for CommandError {
    fn from(e: anyhow::Error) -> Self {
        Self::classify(e.to_string())
    }
}
