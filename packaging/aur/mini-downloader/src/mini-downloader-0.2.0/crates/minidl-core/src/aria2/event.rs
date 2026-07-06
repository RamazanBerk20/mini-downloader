//! aria2 push-notification events and status mapping.

use serde::{Deserialize, Serialize};

/// A lifecycle notification aria2 pushed over the WebSocket. The payload is the
/// affected download's GID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Aria2Event {
    Start(String),
    Pause(String),
    Stop(String),
    Complete(String),
    /// Seeding finished (BitTorrent only).
    BtComplete(String),
    Error(String),
}

impl Aria2Event {
    /// Parse a notification `method` + `gid` into an event, or `None` if the
    /// method is not a recognized aria2 notification.
    pub fn from_method(method: &str, gid: String) -> Option<Self> {
        match method {
            "aria2.onDownloadStart" => Some(Self::Start(gid)),
            "aria2.onDownloadPause" => Some(Self::Pause(gid)),
            "aria2.onDownloadStop" => Some(Self::Stop(gid)),
            "aria2.onDownloadComplete" => Some(Self::Complete(gid)),
            "aria2.onBtDownloadComplete" => Some(Self::BtComplete(gid)),
            "aria2.onDownloadError" => Some(Self::Error(gid)),
            _ => None,
        }
    }

    pub fn gid(&self) -> &str {
        match self {
            Self::Start(g)
            | Self::Pause(g)
            | Self::Stop(g)
            | Self::Complete(g)
            | Self::BtComplete(g)
            | Self::Error(g) => g,
        }
    }
}

/// aria2's `status` field for a download. Mirrors the RPC string values 1:1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Aria2Status {
    Active,
    Waiting,
    Paused,
    Error,
    Complete,
    Removed,
}

impl Aria2Status {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "active" => Some(Self::Active),
            "waiting" => Some(Self::Waiting),
            "paused" => Some(Self::Paused),
            "error" => Some(Self::Error),
            "complete" => Some(Self::Complete),
            "removed" => Some(Self::Removed),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Waiting => "waiting",
            Self::Paused => "paused",
            Self::Error => "error",
            Self::Complete => "complete",
            Self::Removed => "removed",
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Complete | Self::Error | Self::Removed)
    }
}
