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

    /// `None` for a value this build does not know (a corrupt or newer row).
    /// Callers surface it instead of treating it as restartable `Queued`.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "queued" => Self::Queued,
            "active" => Self::Active,
            "waiting" => Self::Waiting,
            "paused" => Self::Paused,
            "complete" => Self::Complete,
            "error" => Self::Error,
            "removed" => Self::Removed,
            "scheduled" => Self::Scheduled,
            _ => return None,
        })
    }

    /// The download lifecycle. Every status write goes through
    /// [`crate::db::Db::transition`], which refuses an edge this table does
    /// not allow, so a late or duplicate engine event cannot move a row
    /// backwards (for example `complete` → `active`, or `complete` → `error`).
    ///
    /// ```text
    /// queued ──► active ⇄ waiting ⇄ paused ──► complete | error | removed
    ///   │          ▲  └────────► scheduled ◄────┘          │
    ///   └──────────┴──────── user retry / resume ◄─────────┘
    /// ```
    pub fn can_transition_to(self, next: Self) -> bool {
        use DownloadStatus::*;
        match (self, next) {
            (from, to) if from == to => false,
            // Nothing ever returns to `queued`: it means "not yet handed to an engine".
            (_, Queued) => false,
            (Queued | Active | Waiting | Paused | Scheduled, _) => true,
            // Terminal rows restart only through an explicit retry/resume, which
            // makes them active (or scheduled/removed by the user).
            (Complete | Error | Removed, Active | Scheduled | Removed) => true,
            (Complete | Error | Removed, _) => false,
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

    /// Map an aria2 status string to an app status, or `None` for an
    /// unrecognized value. The one place this mapping lives (previously
    /// open-coded in several call sites that could drift).
    pub fn from_aria2_str(s: &str) -> Option<Self> {
        Aria2Status::parse(s).map(Self::from_aria2)
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
    /// The aria2 GID currently used for live control and progress. Metadata
    /// downloads can replace this with a followed-by child GID.
    pub gid: Option<String>,
    /// The durable GID written by aria2's session file. For magnets and remote
    /// torrent/metalink URLs this remains the metadata-parent GID even when the
    /// live `gid` has moved to a child transfer.
    pub session_gid: Option<String>,
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
    /// Replay context captured from the browser, persisted so retry/resume/
    /// reconcile don't drop authentication (v2 columns).
    pub user_agent: Option<String>,
    pub cookie: Option<String>,
    /// JSON-encoded `Vec<(String, String)>` of extra headers.
    pub extra_headers: Option<String>,
    /// Originating page URL — handed to yt-dlp for stream downloads.
    pub page_url: Option<String>,
    /// yt-dlp chosen format id, replayed on resume so quality is stable.
    pub format_id: Option<String>,
    /// Per-download speed cap in bytes/sec (aria2 `max-download-limit`).
    pub speed_limit: Option<i64>,
    /// Grouping handle into `packages` (batch adds, playlists).
    pub package_id: Option<i64>,
    /// Content-Type captured at ingest — category mime rules match on it.
    pub mime: Option<String>,
    /// aria2 checksum option value (`sha-256=<hex>`), verified on completion.
    pub checksum: Option<String>,
    /// Deferred start time (unix secs) while status is `Scheduled`.
    pub start_at: Option<i64>,
    /// JSON-encoded yt-dlp media options (subs/audio-extract/thumbnail),
    /// replayed on resume/retry.
    pub media_opts: Option<String>,
    /// Private staging directory aria2 writes into (a hidden subfolder of
    /// `dir`); the finished file is moved into `dir` without replacing
    /// anything. `None` for yt-dlp rows and rows created before staging.
    pub stage_dir: Option<String>,
}

impl Download {
    /// True when this row is driven by yt-dlp (no aria2 GID). yt-dlp rows never
    /// receive a GID, so both the kind and the absent GID agree — use this one
    /// predicate everywhere instead of branching on GID/kind independently.
    pub fn is_ytdlp(&self) -> bool {
        matches!(self.kind.as_str(), "video" | "hls" | "dash")
    }
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
    pub user_agent: Option<String>,
    pub cookie: Option<String>,
    pub extra_headers: Option<String>,
    pub page_url: Option<String>,
    pub format_id: Option<String>,
    pub package_id: Option<i64>,
    pub mime: Option<String>,
    pub checksum: Option<String>,
    pub media_opts: Option<String>,
}

/// One recorded status change of a download: what it was, what it became, and
/// why. Read back by `ctl why`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transition {
    pub at: i64,
    pub from: Option<DownloadStatus>,
    pub to: DownloadStatus,
    pub cause: String,
    pub detail: Option<String>,
}

fn ago(at: i64, now: i64) -> String {
    let seconds = (now - at).max(0);
    match seconds {
        0..=89 => format!("{seconds}s ago"),
        90..=5399 => format!("{}m ago", seconds / 60),
        5400..=172_799 => format!("{}h ago", seconds / 3600),
        _ => format!("{}d ago", seconds / 86_400),
    }
}

/// Plain-text answer to "why is this download in its current state?" from
/// the row and its journal ([`crate::db::Db::transitions`]).
pub fn explain(row: &Download, journal: &[Transition], now: i64) -> String {
    let mut text = format!("#{} is {}", row.id, row.status.as_str());
    match journal
        .iter()
        .rev()
        .find(|entry| entry.to == row.status && entry.from != Some(entry.to))
    {
        Some(entry) => {
            text.push_str(&format!(" since {}: {}", ago(entry.at, now), entry.cause));
            if let Some(detail) = &entry.detail {
                text.push_str(&format!(" ({detail})"));
            }
        }
        None => text.push_str(" (no recorded transition into this state)"),
    }
    text.push('\n');
    if row.status == DownloadStatus::Error {
        if let Some(message) = &row.error_message {
            text.push_str(&format!("error: {message}"));
            if let Some(code) = &row.error_code {
                text.push_str(&format!(" (aria2 code {code})"));
            }
            text.push('\n');
        }
    }
    if !journal.is_empty() {
        text.push_str("history:\n");
        for entry in journal {
            let from = entry.from.map_or("-", |status| status.as_str());
            text.push_str(&format!(
                "  {:>8}  {from} -> {}  {}",
                ago(entry.at, now),
                entry.to.as_str(),
                entry.cause
            ));
            if let Some(detail) = &entry.detail {
                text.push_str(&format!(" ({detail})"));
            }
            text.push('\n');
        }
    }
    text
}

/// A group of downloads added together (batch paste, playlist, bulk capture).
/// Pure grouping — status/progress are derived from members in the UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Package {
    pub id: i64,
    pub name: String,
    pub category_id: Option<i64>,
    pub dir: Option<String>,
    pub status: String,
    pub created_at: i64,
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
