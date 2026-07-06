//! Domain types persisted in the DB and surfaced to the UI.

use serde::{Deserialize, Serialize};

use crate::aria2::Aria2Status;

/// Current unix time in seconds.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// App-level download status. Superset of aria2's statuses with two states aria2
/// never reports: `Queued` (created in the DB, not yet handed to aria2) and
/// `Scheduled` (held by the scheduler).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DownloadStatus {
    Queued,
    Active,
    Waiting,
    Paused,
    Complete,
    Error,
    Removed,
    Scheduled,
}

impl DownloadStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Active => "active",
            Self::Waiting => "waiting",
            Self::Paused => "paused",
            Self::Complete => "complete",
            Self::Error => "error",
            Self::Removed => "removed",
            Self::Scheduled => "scheduled",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "active" => Self::Active,
            "waiting" => Self::Waiting,
            "paused" => Self::Paused,
            "complete" => Self::Complete,
            "error" => Self::Error,
            "removed" => Self::Removed,
            "scheduled" => Self::Scheduled,
            _ => Self::Queued,
        }
    }

    pub fn from_aria2(s: Aria2Status) -> Self {
        match s {
            Aria2Status::Active => Self::Active,
            Aria2Status::Waiting => Self::Waiting,
            Aria2Status::Paused => Self::Paused,
            Aria2Status::Complete => Self::Complete,
            Aria2Status::Error => Self::Error,
            Aria2Status::Removed => Self::Removed,
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Complete | Self::Error | Self::Removed)
    }

    pub fn is_running(&self) -> bool {
        matches!(self, Self::Active | Self::Waiting)
    }
}

/// A durable download record. `id` is stable; `gid` is aria2's ephemeral handle,
/// rebound on startup reconciliation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Download {
    pub id: i64,
    pub gid: Option<String>,
    pub url: String,
    pub filename: Option<String>,
    pub dir: String,
    pub status: DownloadStatus,
    pub kind: String,
    pub total_bytes: i64,
    pub completed_bytes: i64,
    pub download_speed: i64,
    pub upload_speed: i64,
    pub connections: i64,
    pub num_seeders: i64,
    pub referrer: Option<String>,
    pub info_hash: Option<String>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub category_id: Option<i64>,
    pub created_at: i64,
    pub finished_at: Option<i64>,
}

/// Parameters to create a new download row.
#[derive(Debug, Clone, Default)]
pub struct NewDownload {
    pub url: String,
    pub filename: Option<String>,
    pub dir: String,
    pub kind: String,
    pub referrer: Option<String>,
    pub category_id: Option<i64>,
}

/// A category: a destination folder + match rules for auto-organize.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Category {
    pub id: i64,
    pub name: String,
    pub dir: String,
    /// JSON array of rules: `[{ "match": "ext"|"mime"|"host", "values": [...] }]`.
    pub rules: String,
    pub priority: i64,
}

/// A time-based scheduler rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Schedule {
    pub id: i64,
    pub name: Option<String>,
    /// `pause_all` | `resume_all` | `set_speed`.
    pub action: String,
    /// Weekday bitmask, bit 0 = Monday … bit 6 = Sunday.
    pub days_mask: i64,
    /// Minutes since midnight (local time).
    pub at_minute: i64,
    /// Bytes/sec for `set_speed`.
    pub speed_limit: Option<i64>,
    pub enabled: bool,
}
