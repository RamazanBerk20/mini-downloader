//! SQLite persistence (embedded rusqlite). The DB owns durable metadata + the
//! last-known snapshot; aria2 owns live transfer state. A `Mutex<Connection>`
//! serializes access — contention is negligible for this workload (a poller that
//! writes on transitions + on-demand reads).

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::model::{now, Category, Download, DownloadStatus, NewDownload};

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

/// Default categories seeded on first run. Dirs are `~`-relative markers the app
/// resolves at runtime.
const DEFAULT_CATEGORIES: &[(&str, &str, &str)] = &[
    ("Video", "~/Videos", r#"["mkv","mp4","webm","avi","mov","m4v","flv","ts"]"#),
    ("Audio", "~/Music", r#"["mp3","flac","m4a","opus","aac","wav","ogg"]"#),
    ("Documents", "~/Documents", r#"["pdf","epub","docx","odt","txt","xlsx","pptx"]"#),
    ("Archives", "~/Downloads/Archives", r#"["zip","7z","rar","tar","gz","xz","zst","bz2"]"#),
    ("Programs", "~/Downloads/Programs", r#"["AppImage","deb","rpm","exe","msi","sh"]"#),
    ("Images", "~/Pictures", r#"["png","jpg","jpeg","gif","webp","svg"]"#),
];

const COLS: &str = "id, gid, url, filename, dir, status, kind, total_bytes, completed_bytes, \
    download_speed, upload_speed, connections, num_seeders, referrer, info_hash, \
    error_code, error_message, category_id, created_at, finished_at";

fn row_to_download(row: &Row) -> rusqlite::Result<Download> {
    let status: String = row.get(5)?;
    Ok(Download {
        id: row.get(0)?,
        gid: row.get(1)?,
        url: row.get(2)?,
        filename: row.get(3)?,
        dir: row.get(4)?,
        status: DownloadStatus::parse(&status),
        kind: row.get(6)?,
        total_bytes: row.get(7)?,
        completed_bytes: row.get(8)?,
        download_speed: row.get(9)?,
        upload_speed: row.get(10)?,
        connections: row.get(11)?,
        num_seeders: row.get(12)?,
        referrer: row.get(13)?,
        info_hash: row.get(14)?,
        error_code: row.get(15)?,
        error_message: row.get(16)?,
        category_id: row.get(17)?,
        created_at: row.get(18)?,
        finished_at: row.get(19)?,
    })
}

impl Db {
    pub fn open(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir).ok();
        let conn = Connection::open(data_dir.join("ldm.db"))?;
        Self::init(conn)
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
        let db = Self { conn: Arc::new(Mutex::new(conn)) };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version < 1 {
            conn.execute_batch(MIGRATION_1)?;
            for (name, dir, rules) in DEFAULT_CATEGORIES {
                conn.execute(
                    "INSERT OR IGNORE INTO categories (name, dir, rules) VALUES (?1, ?2, ?3)",
                    params![name, dir, rules],
                )?;
            }
            conn.execute_batch("PRAGMA user_version=1;")?;
        }
        Ok(())
    }

    // ---- downloads ----

    pub fn insert_download(&self, d: &NewDownload) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO downloads (url, filename, dir, kind, referrer, category_id, status, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'queued', ?7)",
            params![d.url, d.filename, d.dir, d.kind, d.referrer, d.category_id, now()],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn set_gid(&self, id: i64, gid: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("UPDATE downloads SET gid=?1 WHERE id=?2", params![gid, id])?;
        Ok(())
    }

    pub fn get(&self, id: i64) -> Result<Option<Download>> {
        let conn = self.conn.lock().unwrap();
        let sql = format!("SELECT {COLS} FROM downloads WHERE id=?1");
        Ok(conn.query_row(&sql, params![id], row_to_download).optional()?)
    }

    pub fn find_by_gid(&self, gid: &str) -> Result<Option<Download>> {
        let conn = self.conn.lock().unwrap();
        let sql = format!("SELECT {COLS} FROM downloads WHERE gid=?1");
        Ok(conn.query_row(&sql, params![gid], row_to_download).optional()?)
    }

    /// Match by info_hash (BitTorrent) or URL — used to rebind a moved GID.
    pub fn find_by_infohash_or_url(&self, info_hash: Option<&str>, url: &str) -> Result<Option<Download>> {
        let conn = self.conn.lock().unwrap();
        let sql = format!(
            "SELECT {COLS} FROM downloads WHERE (?1 IS NOT NULL AND info_hash=?1) OR url=?2 ORDER BY id DESC LIMIT 1"
        );
        Ok(conn.query_row(&sql, params![info_hash, url], row_to_download).optional()?)
    }

    pub fn list(&self, status: Option<&str>) -> Result<Vec<Download>> {
        let conn = self.conn.lock().unwrap();
        let (sql, has_filter) = match status {
            Some(_) => (format!("SELECT {COLS} FROM downloads WHERE status=?1 ORDER BY id DESC"), true),
            None => (format!("SELECT {COLS} FROM downloads ORDER BY id DESC"), false),
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
        let conn = self.conn.lock().unwrap();
        let sql = format!(
            "SELECT {COLS} FROM downloads WHERE status IN ('active','waiting','paused','queued')"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map([], row_to_download)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn set_status(&self, id: i64, status: DownloadStatus) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let finished = if status.is_terminal() { Some(now()) } else { None };
        conn.execute(
            "UPDATE downloads SET status=?1, finished_at=COALESCE(?2, finished_at) WHERE id=?3",
            params![status.as_str(), finished, id],
        )?;
        Ok(())
    }

    pub fn set_error(&self, id: i64, code: Option<&str>, message: Option<&str>) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE downloads SET status='error', error_code=?1, error_message=?2, finished_at=?3 WHERE id=?4",
            params![code, message, now(), id],
        )?;
        Ok(())
    }

    pub fn set_filename(&self, id: i64, filename: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("UPDATE downloads SET filename=?1 WHERE id=?2", params![filename, id])?;
        Ok(())
    }

    pub fn set_info_hash(&self, id: i64, info_hash: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("UPDATE downloads SET info_hash=?1 WHERE id=?2", params![info_hash, id])?;
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
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE downloads SET completed_bytes=?1, total_bytes=?2, download_speed=?3,
                upload_speed=?4, connections=?5, num_seeders=?6 WHERE gid=?7",
            params![completed, total, dl_speed, ul_speed, connections, num_seeders, gid],
        )?;
        Ok(())
    }

    pub fn delete(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM downloads WHERE id=?1", params![id])?;
        Ok(())
    }

    // ---- categories ----

    pub fn list_categories(&self) -> Result<Vec<Category>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt =
            conn.prepare("SELECT id, name, dir, rules, priority FROM categories ORDER BY priority, name")?;
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

    pub fn upsert_category(&self, name: &str, dir: &str, rules: &str, priority: i64) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO categories (name, dir, rules, priority) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(name) DO UPDATE SET dir=?2, rules=?3, priority=?4",
            params![name, dir, rules, priority],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn delete_category(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM categories WHERE id=?1", params![id])?;
        Ok(())
    }

    // ---- settings ----

    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row("SELECT value FROM settings WHERE key=?1", params![key], |r| r.get(0))
            .optional()?)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
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
        db.set_gid(id, "abc123").unwrap();
        db.checkpoint_progress("abc123", 50, 100, 1000, 0, 8, 0).unwrap();

        let d = db.find_by_gid("abc123").unwrap().unwrap();
        assert_eq!(d.id, id);
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
    fn settings_roundtrip() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(db.get_setting("x").unwrap(), None);
        db.set_setting("x", "1").unwrap();
        db.set_setting("x", "2").unwrap();
        assert_eq!(db.get_setting("x").unwrap(), Some("2".into()));
    }
}
