use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{bail, Result};
use rusqlite::{params, Connection};

use super::Db;

pub(super) const MIGRATION_1: &str = r#"
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
pub(super) const MIGRATION_2: &str = r#"
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
pub(super) const MIGRATION_3: &str = r#"
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

pub(super) const SCHEMA_VERSION: i64 = 4;

/// Default categories seeded on first run. Dirs are `~`-relative markers the app
/// resolves at runtime.
pub(super) const DEFAULT_CATEGORIES: &[(&str, &str, &str)] = &[
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

impl Db {
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

    pub(super) fn init(conn: Connection) -> Result<Self> {
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
}
