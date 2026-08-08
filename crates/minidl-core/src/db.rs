//! SQLite persistence (embedded rusqlite). The DB owns durable metadata + the
//! last-known snapshot; aria2 owns live transfer state. A `Mutex<Connection>`
//! serializes access — contention is negligible for this workload (a poller that
//! writes on transitions + on-demand reads).

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{bail, Result};
use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::model::{now, Category, Download, DownloadStatus, NewDownload, Package, Schedule};

#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

/// Result of an idempotent download insert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadInsertResult {
    Inserted(i64),
    Existing(i64),
}

impl DownloadInsertResult {
    pub fn id(self) -> i64 {
        match self {
            Self::Inserted(id) | Self::Existing(id) => id,
        }
    }
}

fn is_strong_transient_query_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.starts_with("x-amz-")
        || key.starts_with("x-goog-")
        || matches!(
            key.as_str(),
            "expires"
                | "expiry"
                | "exp"
                | "signature"
                | "sig"
                | "token"
                | "access_token"
                | "auth"
                | "auth_key"
                | "md5"
                | "policy"
        )
}

fn is_transient_query_key(key: &str) -> bool {
    is_strong_transient_query_key(key)
        || matches!(
            key.to_ascii_lowercase().as_str(),
            // Companion fields used by common CDN signatures. These are only
            // stripped when the URL also contains a strong signature field.
            "h" | "hash"
                | "key"
                | "key-pair-id"
                | "user_id"
                | "sv"
                | "spr"
                | "st"
                | "se"
                | "sr"
                | "sp"
        )
}

/// Return the stable resource URL for a fully signed CDN query. Ordinary query
/// strings are deliberately left alone: `download?id=1` and `download?id=2`
/// can be different files. A query is stripped only when every field belongs
/// to a known signing scheme and at least one high-confidence signature field
/// is present.
fn signed_source_base(source: &str) -> Option<String> {
    let mut parsed = url::Url::parse(source).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    let keys = parsed
        .query_pairs()
        .map(|(key, _)| key.into_owned())
        .collect::<Vec<_>>();
    if keys.is_empty()
        || !keys.iter().any(|key| is_strong_transient_query_key(key))
        || !keys.iter().all(|key| is_transient_query_key(key))
    {
        return None;
    }
    parsed.set_query(None);
    parsed.set_fragment(None);
    Some(parsed.to_string())
}

fn unsigned_source(source: &str) -> Option<String> {
    let mut parsed = url::Url::parse(source).ok()?;
    if parsed.query().is_some() {
        return None;
    }
    parsed.set_fragment(None);
    Some(parsed.to_string())
}

/// Compare download sources while tolerating short-lived CDN signatures.
/// This is public so the desktop ingestion layer can compare a target file's
/// recorded origin URL before deciding that it is already downloaded.
pub fn duplicate_sources_match(left: &str, right: &str) -> bool {
    if left == right {
        return true;
    }
    match (signed_source_base(left), signed_source_base(right)) {
        (Some(left), Some(right)) => left == right,
        (Some(left), None) => unsigned_source(right).is_some_and(|right| left == right),
        (None, Some(right)) => unsigned_source(left).is_some_and(|left| left == right),
        (None, None) => false,
    }
}

const MIGRATION_1: &str = r#"
CREATE TABLE downloads (
    id              INTEGER PRIMARY KEY,
    gid             TEXT UNIQUE,
    package_id      INTEGER,
    category_id     INTEGER,
    url             TEXT NOT NULL,
    filename        TEXT,
    dir             TEXT NOT NULL,
    status          TEXT NOT NULL DEFAULT 'queued',
    kind            TEXT NOT NULL DEFAULT 'http',
    total_bytes     INTEGER NOT NULL DEFAULT 0,
    completed_bytes INTEGER NOT NULL DEFAULT 0,
    download_speed  INTEGER NOT NULL DEFAULT 0,
    upload_speed    INTEGER NOT NULL DEFAULT 0,
    connections     INTEGER NOT NULL DEFAULT 0,
    num_seeders     INTEGER NOT NULL DEFAULT 0,
    referrer        TEXT,
    info_hash       TEXT,
    headers_id      INTEGER,
    error_code      TEXT,
    error_message   TEXT,
    created_at      INTEGER NOT NULL,
    finished_at     INTEGER
);
CREATE INDEX idx_downloads_status ON downloads(status);
CREATE INDEX idx_downloads_gid    ON downloads(gid);
CREATE INDEX idx_downloads_url    ON downloads(url);

CREATE TABLE packages (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL,
    category_id INTEGER,
    dir         TEXT,
    status      TEXT NOT NULL DEFAULT 'collecting',
    created_at  INTEGER NOT NULL
);

CREATE TABLE categories (
    id       INTEGER PRIMARY KEY,
    name     TEXT UNIQUE NOT NULL,
    dir      TEXT NOT NULL,
    rules    TEXT NOT NULL DEFAULT '[]',
    priority INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE schedules (
    id          INTEGER PRIMARY KEY,
    name        TEXT,
    action      TEXT NOT NULL,
    days_mask   INTEGER NOT NULL DEFAULT 127,
    at_minute   INTEGER NOT NULL DEFAULT 0,
    speed_limit INTEGER,
    enabled     INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE secrets (
    id         INTEGER PRIMARY KEY,
    kind       TEXT NOT NULL,
    data       BLOB NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
"#;

/// v2: durable replay context (auth survives retry/resume/restart) + yt-dlp
/// format + per-download speed cap. `ALTER TABLE ADD COLUMN` is safe/idempotent
/// under the version gate.
const MIGRATION_2: &str = r#"
ALTER TABLE downloads ADD COLUMN user_agent    TEXT;
ALTER TABLE downloads ADD COLUMN cookie        TEXT;
ALTER TABLE downloads ADD COLUMN extra_headers TEXT;
ALTER TABLE downloads ADD COLUMN page_url      TEXT;
ALTER TABLE downloads ADD COLUMN format_id     TEXT;
ALTER TABLE downloads ADD COLUMN speed_limit   INTEGER;
"#;

/// v3: package grouping surfaced (index; the table itself is v1), category
/// mime/host rules (`mime`), aria2 checksum verification (`checksum`, stored as
/// `sha-256=<hex>`), per-download scheduling (`start_at`, unix secs), and yt-dlp
/// media options (`media_opts`, JSON).
const MIGRATION_3: &str = r#"
ALTER TABLE downloads ADD COLUMN mime       TEXT;
ALTER TABLE downloads ADD COLUMN checksum   TEXT;
ALTER TABLE downloads ADD COLUMN start_at   INTEGER;
ALTER TABLE downloads ADD COLUMN media_opts TEXT;
CREATE INDEX idx_downloads_package ON downloads(package_id);
"#;

/// v4: aria2 serializes the metadata-parent GID for magnets and remote
/// torrent/metalink URLs, while the live row follows its child GID. Keep both
/// identities so a paused startup session can resume the same partial job.
const MIGRATION_4: &str = r#"
ALTER TABLE downloads ADD COLUMN session_gid TEXT;
CREATE INDEX idx_downloads_session_gid ON downloads(session_gid);
"#;

const SCHEMA_VERSION: i64 = 4;

/// Default categories seeded on first run. Dirs are `~`-relative markers the app
/// resolves at runtime.
const DEFAULT_CATEGORIES: &[(&str, &str, &str)] = &[
    (
        "Video",
        "~/Videos",
        r#"["mkv","mp4","webm","avi","mov","m4v","flv","ts"]"#,
    ),
    (
        "Audio",
        "~/Music",
        r#"["mp3","flac","m4a","opus","aac","wav","ogg"]"#,
    ),
    (
        "Documents",
        "~/Documents",
        r#"["pdf","epub","docx","odt","txt","xlsx","pptx"]"#,
    ),
    (
        "Archives",
        "~/Downloads/Archives",
        r#"["zip","7z","rar","tar","gz","xz","zst","bz2"]"#,
    ),
    (
        "Programs",
        "~/Downloads/Programs",
        r#"["AppImage","deb","rpm","exe","msi","sh"]"#,
    ),
    (
        "Images",
        "~/Pictures",
        r#"["png","jpg","jpeg","gif","webp","svg"]"#,
    ),
];

const COLS: &str =
    "id, gid, session_gid, url, filename, dir, status, kind, total_bytes, completed_bytes, \
    download_speed, upload_speed, connections, num_seeders, referrer, info_hash, \
    error_code, error_message, category_id, created_at, finished_at, \
    user_agent, cookie, extra_headers, page_url, format_id, speed_limit, \
    package_id, mime, checksum, start_at, media_opts";

fn row_to_download(row: &Row) -> rusqlite::Result<Download> {
    let status: String = row.get(6)?;
    Ok(Download {
        id: row.get(0)?,
        gid: row.get(1)?,
        session_gid: row.get(2)?,
        url: row.get(3)?,
        filename: row.get(4)?,
        dir: row.get(5)?,
        status: DownloadStatus::parse(&status),
        kind: row.get(7)?,
        total_bytes: row.get(8)?,
        completed_bytes: row.get(9)?,
        download_speed: row.get(10)?,
        upload_speed: row.get(11)?,
        connections: row.get(12)?,
        num_seeders: row.get(13)?,
        referrer: row.get(14)?,
        info_hash: row.get(15)?,
        error_code: row.get(16)?,
        error_message: row.get(17)?,
        category_id: row.get(18)?,
        created_at: row.get(19)?,
        finished_at: row.get(20)?,
        user_agent: row.get(21)?,
        cookie: row.get(22)?,
        extra_headers: row.get(23)?,
        page_url: row.get(24)?,
        format_id: row.get(25)?,
        speed_limit: row.get(26)?,
        package_id: row.get(27)?,
        mime: row.get(28)?,
        checksum: row.get(29)?,
        start_at: row.get(30)?,
        media_opts: row.get(31)?,
    })
}

impl Db {
    /// Lock the connection, recovering from a poisoned mutex instead of
    /// panicking. A panic while another thread held the lock would otherwise
    /// poison it and turn every subsequent DB call into a cascading crash; the
    /// connection itself is still usable.
    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn open(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(data_dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let path = data_dir.join("minidl.db");
        let conn = Connection::open(&path)?;
        let db = Self::init(conn)?;
        // The DB holds every URL/referrer and (post-v2) replayed cookies/headers;
        // keep it unreadable by other local users on a traversable home. The dir
        // gets 0700, the DB + its WAL/SHM sidecars 0600.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for suffix in ["", "-wal", "-shm"] {
                let p = data_dir.join(format!("minidl.db{suffix}"));
                match std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
        Ok(db)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA foreign_keys=ON;
             PRAGMA busy_timeout=5000;",
        )?;
        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<()> {
        let mut conn = self.lock();
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > SCHEMA_VERSION {
            bail!(
                "database schema version {version} is newer than supported version {SCHEMA_VERSION}"
            );
        }
        // Each step runs in its own transaction (atomic — a crash mid-step rolls
        // back including the `user_version` bump, so re-running is safe).
        if version < 1 {
            let tx = conn.transaction()?;
            tx.execute_batch(MIGRATION_1)?;
            for (name, dir, rules) in DEFAULT_CATEGORIES {
                tx.execute(
                    "INSERT OR IGNORE INTO categories (name, dir, rules) VALUES (?1, ?2, ?3)",
                    params![name, dir, rules],
                )?;
            }
            tx.execute_batch("PRAGMA user_version=1;")?;
            tx.commit()?;
        }
        if version < 2 {
            let tx = conn.transaction()?;
            tx.execute_batch(MIGRATION_2)?;
            tx.execute_batch("PRAGMA user_version=2;")?;
            tx.commit()?;
        }
        if version < 3 {
            let tx = conn.transaction()?;
            tx.execute_batch(MIGRATION_3)?;
            tx.execute_batch("PRAGMA user_version=3;")?;
            tx.commit()?;
        }
        if version < 4 {
            let tx = conn.transaction()?;
            tx.execute_batch(MIGRATION_4)?;
            tx.execute_batch("PRAGMA user_version=4;")?;
            tx.commit()?;
        }
        Ok(())
    }

    // ---- downloads ----

    pub fn insert_download(&self, d: &NewDownload) -> Result<i64> {
        let conn = self.lock();
        Self::insert_download_locked(&conn, d)
    }

    /// Insert `d` unless the same source is already represented by a usable
    /// row. The lookup and insert share one DB lock, so simultaneous browser
    /// handoffs cannot both pass the duplicate check.
    ///
    /// Failed/removed rows deliberately do not block a fresh attempt. A
    /// completed row does: users can remove it or turn duplicate prevention off
    /// when they intentionally want to fetch the same stable URL again.
    pub fn insert_download_deduplicated(&self, d: &NewDownload) -> Result<DownloadInsertResult> {
        let conn = self.lock();
        Self::insert_download_deduplicated_locked(&conn, d)
    }

    /// Apply the user's duplicate-prevention setting while inserting. Reading
    /// the setting, checking for a match, and inserting all happen under one
    /// lock so a concurrent setting write or handoff cannot split the decision.
    pub fn insert_download_with_duplicate_policy(
        &self,
        d: &NewDownload,
    ) -> Result<DownloadInsertResult> {
        let conn = self.lock();
        let enabled = conn
            .query_row(
                "SELECT value FROM settings WHERE key='prevent_duplicate_downloads'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .as_deref()
            != Some("false");
        if enabled {
            Self::insert_download_deduplicated_locked(&conn, d)
        } else {
            Ok(DownloadInsertResult::Inserted(
                Self::insert_download_locked(&conn, d)?,
            ))
        }
    }

    fn insert_download_deduplicated_locked(
        conn: &Connection,
        d: &NewDownload,
    ) -> Result<DownloadInsertResult> {
        let signed_base = signed_source_base(&d.url);
        let mut statement = conn.prepare(
            "SELECT id, url FROM downloads
             WHERE kind=?2
               AND format_id IS ?3 AND media_opts IS ?4 AND checksum IS ?5
               AND status IN ('queued','active','waiting','paused','complete','scheduled')
               AND (
                    url=?1
                    OR (?6 IS NOT NULL AND (
                        url=?6
                        OR (instr(url, '?') > 0
                            AND substr(url, 1, instr(url, '?') - 1)=?6)
                    ))
               )
             ORDER BY id DESC",
        )?;
        let candidates = statement.query_map(
            params![
                d.url,
                d.kind,
                d.format_id,
                d.media_opts,
                d.checksum,
                signed_base
            ],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )?;
        for candidate in candidates {
            let (id, source) = candidate?;
            if duplicate_sources_match(&d.url, &source) {
                return Ok(DownloadInsertResult::Existing(id));
            }
        }

        Ok(DownloadInsertResult::Inserted(
            Self::insert_download_locked(conn, d)?,
        ))
    }

    fn insert_download_locked(conn: &Connection, d: &NewDownload) -> Result<i64> {
        conn.execute(
            "INSERT INTO downloads
                (url, filename, dir, kind, referrer, category_id, status, created_at,
                 user_agent, cookie, extra_headers, page_url, format_id,
                 package_id, mime, checksum, media_opts)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'queued', ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                d.url, d.filename, d.dir, d.kind, d.referrer, d.category_id, now(),
                d.user_agent, d.cookie, d.extra_headers, d.page_url, d.format_id,
                d.package_id, d.mime, d.checksum, d.media_opts
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Persist a per-download speed cap (bytes/sec; `None` clears it).
    pub fn set_speed_limit(&self, id: i64, limit: Option<i64>) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE downloads SET speed_limit=?1 WHERE id=?2",
            params![limit, id],
        )?;
        Ok(())
    }

    pub fn set_gid(&self, id: i64, gid: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute("UPDATE downloads SET gid=?1 WHERE id=?2", params![gid, id])?;
        Ok(())
    }

    /// Bind a newly-issued aria2 job. Its initial GID is also the GID aria2
    /// persists in its session file; later metadata transitions may update only
    /// the live `gid` through [`Self::set_gid`].
    pub fn bind_aria2_job(&self, id: i64, gid: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE downloads SET gid=?1, session_gid=?1 WHERE id=?2",
            params![gid, id],
        )?;
        Ok(())
    }

    pub fn get(&self, id: i64) -> Result<Option<Download>> {
        let conn = self.lock();
        let sql = format!("SELECT {COLS} FROM downloads WHERE id=?1");
        Ok(conn
            .query_row(&sql, params![id], row_to_download)
            .optional()?)
    }

    pub fn find_by_gid(&self, gid: &str) -> Result<Option<Download>> {
        let conn = self.lock();
        let sql = format!("SELECT {COLS} FROM downloads WHERE gid=?1");
        Ok(conn
            .query_row(&sql, params![gid], row_to_download)
            .optional()?)
    }

    /// Match by info_hash (BitTorrent) or URL — used to rebind a moved GID.
    pub fn find_by_infohash_or_url(
        &self,
        info_hash: Option<&str>,
        url: &str,
    ) -> Result<Option<Download>> {
        let conn = self.lock();
        let sql = format!(
            "SELECT {COLS} FROM downloads WHERE (?1 IS NOT NULL AND info_hash=?1) OR url=?2 ORDER BY id DESC LIMIT 1"
        );
        Ok(conn
            .query_row(&sql, params![info_hash, url], row_to_download)
            .optional()?)
    }

    pub fn list(&self, status: Option<&str>) -> Result<Vec<Download>> {
        let conn = self.lock();
        let (sql, has_filter) = match status {
            Some(_) => (
                format!("SELECT {COLS} FROM downloads WHERE status=?1 ORDER BY id DESC"),
                true,
            ),
            None => (
                format!("SELECT {COLS} FROM downloads ORDER BY id DESC"),
                false,
            ),
        };
        let mut stmt = conn.prepare(&sql)?;
        let rows = if has_filter {
            stmt.query_map(params![status.unwrap()], row_to_download)?
                .collect::<rusqlite::Result<Vec<_>>>()?
        } else {
            stmt.query_map([], row_to_download)?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        Ok(rows)
    }

    /// Rows the app thinks are still running (active/waiting/paused/queued) — the
    /// reconciliation candidates on startup.
    pub fn running_rows(&self) -> Result<Vec<Download>> {
        let conn = self.lock();
        let sql = format!(
            "SELECT {COLS} FROM downloads WHERE status IN ('active','waiting','paused','queued')"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map([], row_to_download)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Whether any queued work remains, including deliberately paused and
    /// scheduled rows. Completion actions must not fire while these exist.
    pub fn has_unfinished_downloads(&self) -> Result<bool> {
        let conn = self.lock();
        conn.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM downloads
                WHERE status IN ('queued','active','waiting','paused','scheduled')
            )",
            [],
            |row| row.get(0),
        )
        .map_err(Into::into)
    }

    /// Nonterminal aria2 rows whose saved session may be restored. Scheduled
    /// rows are included so their GID survives restart, but remain excluded from
    /// [`Self::running_rows`] so startup reconciliation does not unschedule them.
    pub fn resumable_session_rows(&self) -> Result<Vec<Download>> {
        let conn = self.lock();
        let sql = format!(
            "SELECT {COLS} FROM downloads WHERE status IN ('active','waiting','paused','queued','scheduled')"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map([], row_to_download)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn set_status(&self, id: i64, status: DownloadStatus) -> Result<()> {
        let conn = self.lock();
        let finished = if status.is_terminal() {
            Some(now())
        } else {
            None
        };
        conn.execute(
            "UPDATE downloads
             SET status=?1,
                 finished_at=?2,
                 cookie=CASE WHEN ?1='complete' THEN NULL ELSE cookie END,
                 extra_headers=CASE WHEN ?1='complete' THEN NULL ELSE extra_headers END
             WHERE id=?3",
            params![status.as_str(), finished, id],
        )?;
        Ok(())
    }

    /// Flip status only if it actually differs; returns `true` when a row was
    /// changed. Used as an atomic finalize gate so a completion seen by both the
    /// WebSocket consumer and the live poller fires side effects (notify /
    /// organize) exactly once — the loser's `UPDATE` matches no row.
    pub fn set_status_if_changed(&self, id: i64, status: DownloadStatus) -> Result<bool> {
        let conn = self.lock();
        let finished = if status.is_terminal() {
            Some(now())
        } else {
            None
        };
        let n = conn.execute(
            "UPDATE downloads
             SET status=?1,
                 finished_at=?2,
                 cookie=CASE WHEN ?1='complete' THEN NULL ELSE cookie END,
                 extra_headers=CASE WHEN ?1='complete' THEN NULL ELSE extra_headers END
             WHERE id=?3 AND status<>?1",
            params![status.as_str(), finished, id],
        )?;
        Ok(n > 0)
    }

    pub fn set_error(&self, id: i64, code: Option<&str>, message: Option<&str>) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE downloads SET status='error', error_code=?1, error_message=?2, finished_at=?3 WHERE id=?4",
            params![code, message, now(), id],
        )?;
        Ok(())
    }

    pub fn set_filename(&self, id: i64, filename: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE downloads SET filename=?1 WHERE id=?2",
            params![filename, id],
        )?;
        Ok(())
    }

    pub fn set_info_hash(&self, id: i64, info_hash: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE downloads SET info_hash=?1 WHERE id=?2",
            params![info_hash, id],
        )?;
        Ok(())
    }

    /// Record where a finished file was moved to (category auto-organize).
    pub fn set_dir_and_category(&self, id: i64, dir: &str, category_id: i64) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE downloads SET dir=?1, category_id=?2 WHERE id=?3",
            params![dir, category_id, id],
        )?;
        Ok(())
    }

    /// Atomically record the final filename and destination selected by category
    /// auto-organize, or the location of an existing file adopted by duplicate
    /// prevention. The category is absent when the file remains in the default
    /// download directory.
    pub fn set_file_location(
        &self,
        id: i64,
        filename: &str,
        dir: &str,
        category_id: Option<i64>,
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE downloads SET filename=?1, dir=?2, category_id=?3 WHERE id=?4",
            params![filename, dir, category_id, id],
        )?;
        Ok(())
    }

    /// Persist a progress snapshot (used on transitions + checkpoints, not every tick).
    #[allow(clippy::too_many_arguments)]
    pub fn checkpoint_progress(
        &self,
        gid: &str,
        completed: i64,
        total: i64,
        dl_speed: i64,
        ul_speed: i64,
        connections: i64,
        num_seeders: i64,
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE downloads SET completed_bytes=?1, total_bytes=?2, download_speed=?3,
                upload_speed=?4, connections=?5, num_seeders=?6 WHERE gid=?7",
            params![
                completed,
                total,
                dl_speed,
                ul_speed,
                connections,
                num_seeders,
                gid
            ],
        )?;
        Ok(())
    }

    /// Progress update keyed by app id (used by the yt-dlp driver, which has no GID).
    pub fn checkpoint_progress_by_id(
        &self,
        id: i64,
        completed: i64,
        total: i64,
        dl_speed: i64,
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE downloads SET completed_bytes=?1, total_bytes=?2, download_speed=?3 WHERE id=?4",
            params![completed, total, dl_speed, id],
        )?;
        Ok(())
    }

    pub fn delete(&self, id: i64) -> Result<()> {
        let conn = self.lock();
        conn.execute("DELETE FROM downloads WHERE id=?1", params![id])?;
        Ok(())
    }

    /// Set (or clear) a deferred start time. Status is managed by the caller.
    pub fn set_start_at(&self, id: i64, start_at: Option<i64>) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE downloads SET start_at=?1 WHERE id=?2",
            params![start_at, id],
        )?;
        Ok(())
    }

    /// Scheduled rows whose start time has passed — the scheduler starts these.
    pub fn due_scheduled(&self, now: i64) -> Result<Vec<Download>> {
        let conn = self.lock();
        let sql = format!(
            "SELECT {COLS} FROM downloads WHERE status='scheduled' AND start_at IS NOT NULL AND start_at<=?1"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params![now], row_to_download)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    // ---- packages ----

    pub fn insert_package(
        &self,
        name: &str,
        category_id: Option<i64>,
        dir: Option<&str>,
    ) -> Result<i64> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO packages (name, category_id, dir, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![name, category_id, dir, now()],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn list_packages(&self) -> Result<Vec<Package>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, name, category_id, dir, status, created_at FROM packages ORDER BY id DESC",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(Package {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    category_id: r.get(2)?,
                    dir: r.get(3)?,
                    status: r.get(4)?,
                    created_at: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Drop a package once its last member is removed (keeps the list free of
    /// empty group headers). No-op while any download still references it.
    pub fn delete_package_if_empty(&self, package_id: i64) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "DELETE FROM packages WHERE id=?1
             AND NOT EXISTS (SELECT 1 FROM downloads WHERE package_id=?1)",
            params![package_id],
        )?;
        Ok(())
    }

    // ---- categories ----

    pub fn list_categories(&self) -> Result<Vec<Category>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, name, dir, rules, priority FROM categories ORDER BY priority, name",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(Category {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    dir: r.get(2)?,
                    rules: r.get(3)?,
                    priority: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn upsert_category(
        &self,
        name: &str,
        dir: &str,
        rules: &str,
        priority: i64,
    ) -> Result<i64> {
        let conn = self.lock();
        // `RETURNING id` gives the row's id on both the INSERT and the DO UPDATE
        // path. `last_insert_rowid()` would return a stale/unrelated rowid when
        // the ON CONFLICT branch runs (no insert happens).
        let id = conn.query_row(
            "INSERT INTO categories (name, dir, rules, priority) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(name) DO UPDATE SET dir=?2, rules=?3, priority=?4
             RETURNING id",
            params![name, dir, rules, priority],
            |r| r.get(0),
        )?;
        Ok(id)
    }

    pub fn move_category(&self, id: i64, direction: i32) -> Result<()> {
        if direction == 0 {
            return Ok(());
        }

        let mut conn = self.lock();
        let mut ids = {
            let mut stmt = conn.prepare("SELECT id FROM categories ORDER BY priority, name")?;
            let rows = stmt
                .query_map([], |row| row.get::<_, i64>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        let Some(index) = ids.iter().position(|candidate| *candidate == id) else {
            return Ok(());
        };
        let target = if direction < 0 {
            index.checked_sub(1)
        } else {
            index.checked_add(1).filter(|target| *target < ids.len())
        };
        let Some(target) = target else {
            return Ok(());
        };
        ids.swap(index, target);

        let transaction = conn.transaction()?;
        for (priority, category_id) in ids.into_iter().enumerate() {
            transaction.execute(
                "UPDATE categories SET priority=?1 WHERE id=?2",
                params![priority as i64, category_id],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn delete_category(&self, id: i64) -> Result<()> {
        let conn = self.lock();
        conn.execute("DELETE FROM categories WHERE id=?1", params![id])?;
        Ok(())
    }

    /// Re-add any missing built-in categories (the "restore defaults" action).
    /// Existing categories with the same name are left untouched.
    pub fn seed_default_categories(&self) -> Result<()> {
        let conn = self.lock();
        for (name, dir, rules) in DEFAULT_CATEGORIES {
            conn.execute(
                "INSERT OR IGNORE INTO categories (name, dir, rules) VALUES (?1, ?2, ?3)",
                params![name, dir, rules],
            )?;
        }
        Ok(())
    }

    /// Reset a category's folder to its built-in default marker (no-op if the
    /// category isn't one of the built-ins).
    pub fn reset_category_dir(&self, id: i64) -> Result<()> {
        let conn = self.lock();
        let name: Option<String> = conn
            .query_row(
                "SELECT name FROM categories WHERE id=?1",
                params![id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(name) = name {
            if let Some(def) = DEFAULT_CATEGORIES
                .iter()
                .find(|(n, _, _)| *n == name)
                .map(|(_, d, _)| *d)
            {
                conn.execute("UPDATE categories SET dir=?1 WHERE id=?2", params![def, id])?;
            }
        }
        Ok(())
    }

    // ---- schedules ----

    pub fn list_schedules(&self) -> Result<Vec<Schedule>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, name, action, days_mask, at_minute, speed_limit, enabled FROM schedules ORDER BY at_minute",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(Schedule {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    action: r.get(2)?,
                    days_mask: r.get(3)?,
                    at_minute: r.get(4)?,
                    speed_limit: r.get(5)?,
                    enabled: r.get::<_, i64>(6)? != 0,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn save_schedule(
        &self,
        id: Option<i64>,
        name: Option<&str>,
        action: &str,
        days_mask: i64,
        at_minute: i64,
        speed_limit: Option<i64>,
        enabled: bool,
    ) -> Result<i64> {
        let conn = self.lock();
        match id {
            Some(id) => {
                conn.execute(
                    "UPDATE schedules SET name=?1, action=?2, days_mask=?3, at_minute=?4, speed_limit=?5, enabled=?6 WHERE id=?7",
                    params![name, action, days_mask, at_minute, speed_limit, enabled as i64, id],
                )?;
                Ok(id)
            }
            None => {
                conn.execute(
                    "INSERT INTO schedules (name, action, days_mask, at_minute, speed_limit, enabled) VALUES (?1,?2,?3,?4,?5,?6)",
                    params![name, action, days_mask, at_minute, speed_limit, enabled as i64],
                )?;
                Ok(conn.last_insert_rowid())
            }
        }
    }

    pub fn delete_schedule(&self, id: i64) -> Result<()> {
        let conn = self.lock();
        conn.execute("DELETE FROM schedules WHERE id=?1", params![id])?;
        Ok(())
    }

    // ---- settings ----

    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        let conn = self.lock();
        Ok(conn
            .query_row(
                "SELECT value FROM settings WHERE key=?1",
                params![key],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value=?2",
            params![key, value],
        )?;
        Ok(())
    }

    /// Persist a complete settings draft in one SQLite transaction.
    pub fn set_settings(&self, values: &[(String, String)]) -> Result<()> {
        let mut conn = self.lock();
        let transaction = conn.transaction()?;
        {
            let mut statement = transaction.prepare(
                "INSERT INTO settings (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value=?2",
            )?;
            for (key, value) in values {
                statement.execute(params![key, value])?;
            }
        }
        transaction.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_list_and_status() {
        let db = Db::open_in_memory().unwrap();
        let id = db
            .insert_download(&NewDownload {
                url: "https://x/f.iso".into(),
                dir: "/dl".into(),
                kind: "http".into(),
                ..Default::default()
            })
            .unwrap();
        db.bind_aria2_job(id, "abc123").unwrap();
        db.checkpoint_progress("abc123", 50, 100, 1000, 0, 8, 0)
            .unwrap();

        let d = db.find_by_gid("abc123").unwrap().unwrap();
        assert_eq!(d.id, id);
        assert_eq!(d.session_gid.as_deref(), Some("abc123"));
        assert_eq!(d.completed_bytes, 50);
        assert_eq!(d.connections, 8);

        db.set_status(id, DownloadStatus::Complete).unwrap();
        let d = db.get(id).unwrap().unwrap();
        assert_eq!(d.status, DownloadStatus::Complete);
        assert!(d.finished_at.is_some());

        db.set_status(id, DownloadStatus::Paused).unwrap();
        let d = db.get(id).unwrap().unwrap();
        assert_eq!(d.status, DownloadStatus::Paused);
        assert!(d.finished_at.is_none());
    }

    #[test]
    fn completion_discards_replay_credentials() {
        let db = Db::open_in_memory().unwrap();
        let id = db
            .insert_download(&NewDownload {
                url: "https://example.org/private.zip".into(),
                dir: "/dl".into(),
                kind: "http".into(),
                cookie: Some("session=secret".into()),
                extra_headers: Some(r#"[["Authorization","Bearer secret"]]"#.into()),
                ..Default::default()
            })
            .unwrap();
        db.set_status(id, DownloadStatus::Complete).unwrap();
        let row = db.get(id).unwrap().unwrap();
        assert!(row.cookie.is_none());
        assert!(row.extra_headers.is_none());
    }

    #[test]
    fn deduplicated_insert_reuses_live_and_completed_rows_but_not_failures() {
        let db = Db::open_in_memory().unwrap();
        let download = NewDownload {
            url: "https://x/f.iso".into(),
            dir: "/dl".into(),
            kind: "http".into(),
            ..Default::default()
        };

        let first = db.insert_download_deduplicated(&download).unwrap();
        let first_id = first.id();
        assert_eq!(first, DownloadInsertResult::Inserted(first_id));
        assert_eq!(
            db.insert_download_deduplicated(&download).unwrap(),
            DownloadInsertResult::Existing(first_id)
        );

        db.set_status(first_id, DownloadStatus::Complete).unwrap();
        assert_eq!(
            db.insert_download_deduplicated(&download).unwrap(),
            DownloadInsertResult::Existing(first_id)
        );
        let mut after_auto_organize = download.clone();
        after_auto_organize.dir = "/organized/archives".into();
        assert_eq!(
            db.insert_download_deduplicated(&after_auto_organize)
                .unwrap(),
            DownloadInsertResult::Existing(first_id)
        );

        db.set_error(first_id, None, Some("network failed"))
            .unwrap();
        let retry = db.insert_download_deduplicated(&download).unwrap();
        assert!(matches!(retry, DownloadInsertResult::Inserted(id) if id != first_id));
        assert_eq!(db.list(None).unwrap().len(), 2);
    }

    #[test]
    fn deduplicated_insert_recognizes_rotated_cdn_signatures_only() {
        let db = Db::open_in_memory().unwrap();
        let first = NewDownload {
            url: "https://cdn.example/file.zip?md5=old&expires=100&user_id=7&h=stable".into(),
            filename: Some("file.zip".into()),
            dir: "/dl".into(),
            kind: "http".into(),
            ..Default::default()
        };
        let first_id = db
            .insert_download_with_duplicate_policy(&first)
            .unwrap()
            .id();
        let mut rotated = first.clone();
        rotated.url = "https://cdn.example/file.zip?md5=new&expires=200&user_id=7&h=stable".into();
        assert_eq!(
            db.insert_download_with_duplicate_policy(&rotated).unwrap(),
            DownloadInsertResult::Existing(first_id)
        );

        let mut semantic = first.clone();
        semantic.url = "https://cdn.example/download?id=1".into();
        assert!(matches!(
            db.insert_download_with_duplicate_policy(&semantic).unwrap(),
            DownloadInsertResult::Inserted(_)
        ));
        semantic.url = "https://cdn.example/download?id=2".into();
        assert!(matches!(
            db.insert_download_with_duplicate_policy(&semantic).unwrap(),
            DownloadInsertResult::Inserted(_)
        ));

        let mut mixed = first;
        mixed.url = "https://cdn.example/download?id=1&token=old".into();
        let mixed_id = db
            .insert_download_with_duplicate_policy(&mixed)
            .unwrap()
            .id();
        mixed.url = "https://cdn.example/download?id=2&token=new".into();
        assert_ne!(
            db.insert_download_with_duplicate_policy(&mixed).unwrap(),
            DownloadInsertResult::Existing(mixed_id)
        );
    }

    #[test]
    fn deduplicated_insert_is_atomic_across_simultaneous_handoffs() {
        use std::sync::{Arc, Barrier};
        use std::thread;

        let db = Db::open_in_memory().unwrap();
        let barrier = Arc::new(Barrier::new(8));
        let mut workers = Vec::new();
        for _ in 0..8 {
            let db = db.clone();
            let barrier = barrier.clone();
            workers.push(thread::spawn(move || {
                let download = NewDownload {
                    url: "https://x/same.iso".into(),
                    dir: "/dl".into(),
                    kind: "http".into(),
                    ..Default::default()
                };
                barrier.wait();
                db.insert_download_deduplicated(&download).unwrap()
            }));
        }

        let results: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
        assert_eq!(
            results
                .iter()
                .filter(|r| matches!(r, DownloadInsertResult::Inserted(_)))
                .count(),
            1
        );
        let id = results[0].id();
        assert!(results.iter().all(|r| r.id() == id));
        assert_eq!(db.list(None).unwrap().len(), 1);
    }

    #[test]
    fn duplicate_policy_defaults_on_can_be_disabled_and_keeps_media_variants() {
        let db = Db::open_in_memory().unwrap();
        let video = NewDownload {
            url: "https://video.example/watch/1".into(),
            dir: "/dl".into(),
            kind: "video".into(),
            format_id: Some("1080p".into()),
            media_opts: Some(r#"{"write_subs":true}"#.into()),
            ..Default::default()
        };

        let first = db.insert_download_with_duplicate_policy(&video).unwrap();
        assert!(matches!(first, DownloadInsertResult::Inserted(_)));
        assert_eq!(
            db.insert_download_with_duplicate_policy(&video).unwrap(),
            DownloadInsertResult::Existing(first.id())
        );

        let mut other_quality = video.clone();
        other_quality.format_id = Some("720p".into());
        assert!(matches!(
            db.insert_download_with_duplicate_policy(&other_quality)
                .unwrap(),
            DownloadInsertResult::Inserted(_)
        ));

        db.set_setting("prevent_duplicate_downloads", "false")
            .unwrap();
        assert!(matches!(
            db.insert_download_with_duplicate_policy(&video).unwrap(),
            DownloadInsertResult::Inserted(_)
        ));
        assert_eq!(db.list(None).unwrap().len(), 3);
    }

    #[test]
    fn default_categories_seeded() {
        let db = Db::open_in_memory().unwrap();
        let cats = db.list_categories().unwrap();
        assert!(cats.iter().any(|c| c.name == "Video"));
        assert!(cats.len() >= 6);
    }

    #[test]
    fn upsert_category_returns_stable_id_on_update() {
        let db = Db::open_in_memory().unwrap();
        let id1 = db.upsert_category("Custom", "~/a", "[]", 5).unwrap();
        // Second call hits ON CONFLICT DO UPDATE — must return the same id, not a
        // bogus last_insert_rowid().
        let id2 = db.upsert_category("Custom", "~/b", "[]", 9).unwrap();
        assert_eq!(id1, id2);
        let cat = db
            .list_categories()
            .unwrap()
            .into_iter()
            .find(|c| c.name == "Custom")
            .unwrap();
        assert_eq!(cat.id, id1);
        assert_eq!(cat.dir, "~/b");
    }

    #[test]
    fn category_order_moves_one_position_at_a_time() {
        let db = Db::open_in_memory().unwrap();
        let before = db.list_categories().unwrap();
        let first = before[0].id;
        let second = before[1].id;

        db.move_category(second, -1).unwrap();
        let moved = db.list_categories().unwrap();
        assert_eq!(moved[0].id, second);
        assert_eq!(moved[1].id, first);

        db.move_category(second, -1).unwrap();
        assert_eq!(db.list_categories().unwrap()[0].id, second);
        db.move_category(second, 1).unwrap();
        assert_eq!(db.list_categories().unwrap()[0].id, first);
    }

    #[test]
    fn v3_columns_roundtrip() {
        let db = Db::open_in_memory().unwrap();
        let pkg = db.insert_package("My batch", None, Some("/dl")).unwrap();
        let id = db
            .insert_download(&NewDownload {
                url: "https://x/f.iso".into(),
                dir: "/dl".into(),
                kind: "http".into(),
                package_id: Some(pkg),
                mime: Some("application/x-iso9660-image".into()),
                checksum: Some("sha-256=ab".into()),
                media_opts: Some(r#"{"audio_only":true}"#.into()),
                ..Default::default()
            })
            .unwrap();
        db.set_start_at(id, Some(123)).unwrap();

        let d = db.get(id).unwrap().unwrap();
        assert_eq!(d.package_id, Some(pkg));
        assert_eq!(d.mime.as_deref(), Some("application/x-iso9660-image"));
        assert_eq!(d.checksum.as_deref(), Some("sha-256=ab"));
        assert_eq!(d.start_at, Some(123));
        assert_eq!(d.media_opts.as_deref(), Some(r#"{"audio_only":true}"#));

        assert_eq!(db.list_packages().unwrap().len(), 1);

        db.set_status(id, DownloadStatus::Scheduled).unwrap();
        assert!(db.due_scheduled(122).unwrap().is_empty());
        assert_eq!(db.due_scheduled(123).unwrap().len(), 1);

        // Non-empty package survives; emptying it deletes it.
        db.delete_package_if_empty(pkg).unwrap();
        assert_eq!(db.list_packages().unwrap().len(), 1);
        db.delete(id).unwrap();
        db.delete_package_if_empty(pkg).unwrap();
        assert!(db.list_packages().unwrap().is_empty());
    }

    #[test]
    fn legacy_v1_database_migrates_without_losing_rows() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(MIGRATION_1).unwrap();
        conn.execute(
            "INSERT INTO downloads (url, dir, created_at) VALUES (?1, ?2, ?3)",
            params!["https://example.org/legacy.iso", "/downloads", 123_i64],
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();

        let db = Db::init(conn).unwrap();
        let row = db.list(None).unwrap().pop().unwrap();
        assert_eq!(row.url, "https://example.org/legacy.iso");
        assert!(row.session_gid.is_none());
        let version: i64 = db
            .lock()
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn legacy_v3_database_receives_session_identity_migration() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(MIGRATION_1).unwrap();
        conn.execute_batch(MIGRATION_2).unwrap();
        conn.execute_batch(MIGRATION_3).unwrap();
        conn.pragma_update(None, "user_version", 3).unwrap();

        let db = Db::init(conn).unwrap();
        let columns: Vec<String> = {
            let conn = db.lock();
            let mut statement = conn.prepare("PRAGMA table_info(downloads)").unwrap();
            statement
                .query_map([], |row| row.get(1))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        };
        assert!(columns.iter().any(|column| column == "session_gid"));
    }

    #[test]
    fn future_database_schema_is_rejected() {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();
        let error = Db::init(conn).err().unwrap().to_string();
        assert!(error.contains("newer than supported"));
    }

    #[test]
    fn session_gid_survives_live_metadata_rebind() {
        let db = Db::open_in_memory().unwrap();
        let id = db
            .insert_download(&NewDownload {
                url: "magnet:?xt=urn:btih:abc".into(),
                dir: "/dl".into(),
                kind: "magnet".into(),
                ..Default::default()
            })
            .unwrap();
        db.bind_aria2_job(id, "metadata-parent").unwrap();
        db.set_gid(id, "content-child").unwrap();

        let row = db.get(id).unwrap().unwrap();
        assert_eq!(row.gid.as_deref(), Some("content-child"));
        assert_eq!(row.session_gid.as_deref(), Some("metadata-parent"));
    }

    #[test]
    fn resumable_session_rows_include_scheduled_without_changing_reconcile_rows() {
        let db = Db::open_in_memory().unwrap();
        let id = db
            .insert_download(&NewDownload {
                url: "https://example.invalid/file".into(),
                dir: "/dl".into(),
                kind: "http".into(),
                ..Default::default()
            })
            .unwrap();
        db.bind_aria2_job(id, "scheduled-gid").unwrap();
        db.set_status(id, DownloadStatus::Scheduled).unwrap();

        assert!(db.running_rows().unwrap().is_empty());
        assert_eq!(db.resumable_session_rows().unwrap().len(), 1);
    }

    #[test]
    fn settings_roundtrip() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(db.get_setting("x").unwrap(), None);
        db.set_setting("x", "1").unwrap();
        db.set_setting("x", "2").unwrap();
        assert_eq!(db.get_setting("x").unwrap(), Some("2".into()));
    }
}
