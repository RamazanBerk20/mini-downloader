//! The UI's state machines, kept free of Slint calls so they can be tested:
//! row presentation, controller sync, the notice queue, the video probe and
//! settings saves.

use minidl_app::controller::{retryable, DownloadView, INTERRUPTED};
use minidl_core::model::DownloadStatus;

use crate::RowState;

/// The one visual state of a row, from the core status plus the engine.
pub(crate) fn row_state(view: &DownloadView, engine_restarting: bool) -> RowState {
    let aria2 = !matches!(view.kind.as_str(), "video" | "hls" | "dash");
    match view.status {
        DownloadStatus::Queued => RowState::Queued,
        DownloadStatus::Active | DownloadStatus::Waiting if aria2 && engine_restarting => {
            RowState::EngineRestarting
        }
        DownloadStatus::Active => RowState::Downloading,
        DownloadStatus::Waiting => RowState::Waiting,
        DownloadStatus::Paused => RowState::Paused,
        DownloadStatus::Scheduled => RowState::Scheduled,
        DownloadStatus::Complete => RowState::Done,
        DownloadStatus::Error if view.error_code.as_deref() == Some(INTERRUPTED) => {
            RowState::Interrupted
        }
        DownloadStatus::Error => RowState::Failed,
        DownloadStatus::Removed => RowState::Removed,
    }
}

/// What a row offers: each state has exactly its own valid actions.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct RowActions {
    pub(crate) pause: bool,
    pub(crate) resume: bool,
    pub(crate) retry: bool,
    pub(crate) open: bool,
}

pub(crate) fn row_actions(state: RowState, kind: &str) -> RowActions {
    match state {
        RowState::Queued
        | RowState::Waiting
        | RowState::Downloading
        | RowState::EngineRestarting => RowActions {
            pause: true,
            ..RowActions::default()
        },
        // An interrupted video continues its partial files on resume.
        RowState::Paused | RowState::Scheduled | RowState::Interrupted => RowActions {
            resume: true,
            ..RowActions::default()
        },
        RowState::Failed => RowActions {
            retry: retryable(kind),
            ..RowActions::default()
        },
        RowState::Done => RowActions {
            open: true,
            ..RowActions::default()
        },
        RowState::Removed => RowActions::default(),
    }
}

pub(crate) fn status_key(state: RowState) -> &'static str {
    match state {
        RowState::Queued => "statusQueued",
        RowState::Waiting => "statusWaiting",
        RowState::Downloading => "statusActive",
        RowState::EngineRestarting => "statusEngineRestarting",
        RowState::Paused => "statusPaused",
        RowState::Scheduled => "statusScheduled",
        RowState::Done => "statusCompleted",
        RowState::Failed => "statusFailed",
        RowState::Interrupted => "statusInterrupted",
        RowState::Removed => "statusRemoved",
    }
}

/// Replace a snapshot's rows, keeping the newer live progress of rows that
/// are still running: the database is only checkpointed every few seconds,
/// and a bar must never slide backwards.
pub(crate) fn merge_snapshot(
    old: &[DownloadView],
    mut new: Vec<DownloadView>,
) -> Vec<DownloadView> {
    let old = old
        .iter()
        .map(|row| (row.id, row))
        .collect::<std::collections::HashMap<_, _>>();
    for row in &mut new {
        let Some(previous) = old.get(&row.id) else {
            continue;
        };
        if row.status.is_running()
            && previous.status.is_running()
            && previous.completed_bytes > row.completed_bytes
        {
            row.completed_bytes = previous.completed_bytes;
            row.total_bytes = row.total_bytes.max(previous.total_bytes);
            row.download_speed = previous.download_speed;
            row.upload_speed = previous.upload_speed;
            row.connections = previous.connections;
            row.num_seeders = previous.num_seeders;
        }
    }
    new
}

/// Neutralise characters that reorder text: a server-chosen
/// `invoice\u{202E}fdp.exe` must not render as `invoiceexe.pdf`.
pub(crate) fn display_safe(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' => {
                '\u{FFFD}'
            }
            c => c,
        })
        .collect()
}

/// UI ↔ controller sync. Every state-changing event asks for one snapshot;
/// events that arrive while one is in flight mark it dirty instead of being
/// dropped, and a gap in event sequence numbers forces a resync.
#[derive(Debug, Default)]
pub(crate) struct SyncState {
    in_flight: bool,
    dirty: bool,
    last_sequence: u64,
}

impl SyncState {
    pub(crate) fn starting_at(sequence: u64) -> Self {
        Self {
            last_sequence: sequence,
            ..Self::default()
        }
    }

    /// Something changed: `true` means start a snapshot fetch now.
    pub(crate) fn request(&mut self) -> bool {
        if self.in_flight {
            self.dirty = true;
            false
        } else {
            self.in_flight = true;
            true
        }
    }

    /// Record an event's sequence number: `true` means events were missed.
    pub(crate) fn observe(&mut self, sequence: u64) -> bool {
        let gap = sequence > self.last_sequence.saturating_add(1);
        self.last_sequence = self.last_sequence.max(sequence);
        gap
    }

    /// A fetch ended (with the snapshot's sequence when it succeeded):
    /// `true` means fetch again, because something changed meanwhile.
    pub(crate) fn finished(&mut self, sequence: Option<u64>) -> bool {
        if let Some(sequence) = sequence {
            self.last_sequence = self.last_sequence.max(sequence);
        }
        if std::mem::take(&mut self.dirty) {
            true
        } else {
            self.in_flight = false;
            false
        }
    }
}

/// One message for the user, shown where it came from (the main page or the
/// dialog that started the action).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Notice<O> {
    pub(crate) id: i32,
    pub(crate) origin: O,
    pub(crate) error: bool,
    pub(crate) text: String,
}

/// A small queue instead of one global slot: a success clears only its own
/// origin's notices, errors outrank information, newest first.
#[derive(Debug)]
pub(crate) struct Notices<O> {
    next_id: i32,
    items: Vec<Notice<O>>,
}

impl<O> Default for Notices<O> {
    fn default() -> Self {
        Self {
            next_id: 1,
            items: Vec::new(),
        }
    }
}

const NOTICES_PER_ORIGIN: usize = 4;

impl<O: Copy + PartialEq> Notices<O> {
    pub(crate) fn push(&mut self, origin: O, error: bool, text: impl Into<String>) {
        let text = text.into();
        if text.is_empty() {
            return;
        }
        self.items
            .retain(|notice| !(notice.origin == origin && notice.text == text));
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        self.items.insert(
            0,
            Notice {
                id,
                origin,
                error,
                text,
            },
        );
        let mut kept = 0;
        self.items.retain(|notice| {
            if notice.origin != origin {
                return true;
            }
            kept += 1;
            kept <= NOTICES_PER_ORIGIN
        });
        // Stable: errors first, newest first within each level.
        self.items.sort_by_key(|notice| !notice.error);
    }

    pub(crate) fn clear(&mut self, origin: O) {
        self.items.retain(|notice| notice.origin != origin);
    }

    pub(crate) fn dismiss(&mut self, id: i32) {
        self.items.retain(|notice| notice.id != id);
    }

    pub(crate) fn items(&self) -> &[Notice<O>] {
        &self.items
    }
}

/// The Grab-video probe. Only the latest probe's result is accepted, and
/// choices can be added only while they belong to the URL in the field.
#[derive(Debug, Default, PartialEq)]
pub(crate) enum Probe {
    #[default]
    Idle,
    Probing {
        token: u64,
        url: String,
    },
    Loaded {
        url: String,
    },
    Failed {
        url: String,
    },
}

impl Probe {
    pub(crate) fn start(&mut self, url: &str, token: u64) {
        *self = Probe::Probing {
            token,
            url: url.to_owned(),
        };
    }

    /// Accept a result only for the probe still running.
    pub(crate) fn finish(&mut self, token: u64, ok: bool) -> bool {
        let Probe::Probing {
            token: current,
            url,
        } = self
        else {
            return false;
        };
        if *current != token {
            return false;
        }
        let url = std::mem::take(url);
        *self = if ok {
            Probe::Loaded { url }
        } else {
            Probe::Failed { url }
        };
        true
    }

    pub(crate) fn can_add(&self, url: &str) -> bool {
        matches!(self, Probe::Loaded { url: loaded } if loaded == url)
    }
}

/// Settings saves, one in flight at a time with the latest change winning.
/// A failed save reverts the UI to what the controller last confirmed.
#[derive(Debug)]
pub(crate) struct SettingsSaver<T> {
    saved: T,
    in_flight: bool,
    pending: Option<T>,
}

#[derive(Debug, PartialEq)]
pub(crate) enum SaveNext<T> {
    Idle,
    Send(T),
    Revert(T),
}

impl<T: Clone + PartialEq> SettingsSaver<T> {
    pub(crate) fn new(saved: T) -> Self {
        Self {
            saved,
            in_flight: false,
            pending: None,
        }
    }

    /// The UI changed: returns the draft to send now, if any.
    pub(crate) fn submit(&mut self, next: T) -> Option<T> {
        if self.in_flight {
            self.pending = Some(next);
            None
        } else if next == self.saved {
            None
        } else {
            self.in_flight = true;
            Some(next)
        }
    }

    /// A save of `sent` finished.
    pub(crate) fn finished(&mut self, sent: T, ok: bool) -> SaveNext<T> {
        if ok {
            self.saved = sent;
        }
        if let Some(pending) = self.pending.take() {
            if pending != self.saved {
                return SaveNext::Send(pending);
            }
        }
        self.in_flight = false;
        if ok {
            SaveNext::Idle
        } else {
            SaveNext::Revert(self.saved.clone())
        }
    }

    /// Settings changed elsewhere (`ctl`, another window). Adopted only when
    /// no save of ours is in flight, so they cannot undo a newer local edit.
    pub(crate) fn adopt(&mut self, current: T) -> bool {
        if self.in_flight {
            return false;
        }
        self.saved = current;
        true
    }
}
