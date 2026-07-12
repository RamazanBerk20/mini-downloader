//! SQLite persistence (embedded rusqlite). The DB owns durable metadata + the
//! last-known snapshot; aria2 owns live transfer state. A `Mutex<Connection>`
//! serializes access — contention is negligible for this workload (a poller that
//! writes on transitions + on-demand reads).

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::model::{now, Category, Download, DownloadStatus, NewDownload, Package, Schedule};

#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
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
        std::fs::create_dir_all(data_dir).ok();
        let path = data_dir.join("minidl.db");
        let conn = Connection::open(&path)?;
        let db = Self::init(conn)?;
        // The DB holds every URL/referrer and (post-v2) replayed cookies/headers;
        // keep it unreadable by other local users on a traversable home. The dir
        // gets 0700, the DB + its WAL/SHM sidecars 0600.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(data_dir, std::fs::Permissions::from_mode(0o700));
            for suffix in ["", "-wal", "-shm"] {
                let p = data_dir.join(format!("minidl.db{suffix}"));
                let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
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
            "UPDATE downloads SET status=?1, finished_at=COALESCE(?2, finished_at) WHERE id=?3",
            params![status.as_str(), finished, id],
        )?;
        Ok(())
    }

    /// Flip status only if it actually differs; returns `true` when a row was
    /// changed. Used as an atomic finalize gate so a completion seen by both the
    /// WebSocket consumer and the 1 Hz poller fires side effects (notify /
    /// organize) exactly once — the loser's `UPDATE` matches no row.
    pub fn set_status_if_changed(&self, id: i64, status: DownloadStatus) -> Result<bool> {
        let conn = self.lock();
        let finished = if status.is_terminal() {
            Some(now())
        } else {
            None
        };
        let n = conn.execute(
            "UPDATE downloads SET status=?1, finished_at=COALESCE(?2, finished_at) WHERE id=?3 AND status<>?1",
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
    /// auto-organize. A collision can change the filename (`file (1).zip`), so
    /// persisting only the directory would leave the UI pointing at the wrong
    /// path.
    pub fn set_file_location(
        &self,
        id: i64,
        filename: &str,
        dir: &str,
        category_id: i64,
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
