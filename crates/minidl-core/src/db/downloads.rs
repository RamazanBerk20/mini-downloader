use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::model::{now, Download, DownloadStatus, NewDownload, Transition};

use super::dedup::signed_source_path;
use super::{duplicate_sources_match, Db, DownloadInsertResult};

const COLS: &str =
    "id, gid, session_gid, url, filename, dir, status, kind, total_bytes, completed_bytes, \
    download_speed, upload_speed, connections, num_seeders, referrer, info_hash, \
    error_code, error_message, category_id, created_at, finished_at, \
    user_agent, cookie, extra_headers, page_url, format_id, speed_limit, \
    package_id, mime, checksum, start_at, media_opts, stage_dir";

/// Journal rows kept per download; older ones are pruned on every write.
const JOURNAL_PER_DOWNLOAD: i64 = 50;

fn row_to_download(row: &Row) -> rusqlite::Result<Download> {
    let raw_status: String = row.get(6)?;
    // A status this build cannot read is surfaced as a failure, never treated
    // as a restartable `queued` row.
    let status = DownloadStatus::parse(&raw_status);
    let error_message: Option<String> = row.get(17)?;
    Ok(Download {
        id: row.get(0)?,
        gid: row.get(1)?,
        session_gid: row.get(2)?,
        url: row.get(3)?,
        filename: row.get(4)?,
        dir: row.get(5)?,
        status: status.unwrap_or(DownloadStatus::Error),
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
        error_message: match status {
            Some(_) => error_message,
            None => Some(format!("unreadable download status {raw_status:?}")),
        },
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
        stage_dir: row.get(32)?,
    })
}

impl Db {
    pub fn insert_download(&self, download: &NewDownload) -> Result<i64> {
        let conn = self.lock();
        Self::insert_download_locked(&conn, download)
    }

    /// Insert `download` unless the same source is already represented by a
    /// usable row. The lookup and insert share one DB lock, so simultaneous
    /// browser handoffs cannot both pass the duplicate check.
    pub fn insert_download_deduplicated(
        &self,
        download: &NewDownload,
    ) -> Result<DownloadInsertResult> {
        let conn = self.lock();
        Self::insert_download_deduplicated_locked(&conn, download)
    }

    /// Apply the user's duplicate-prevention setting while inserting. Reading
    /// the setting, checking for a match, and inserting all happen under one
    /// lock so a concurrent setting write or handoff cannot split the decision.
    pub fn insert_download_with_duplicate_policy(
        &self,
        download: &NewDownload,
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
            Self::insert_download_deduplicated_locked(&conn, download)
        } else {
            Ok(DownloadInsertResult::Inserted(
                Self::insert_download_locked(&conn, download)?,
            ))
        }
    }

    fn insert_download_deduplicated_locked(
        conn: &Connection,
        download: &NewDownload,
    ) -> Result<DownloadInsertResult> {
        let signed_base = signed_source_path(&download.url);
        let mut statement = conn.prepare(
            "SELECT id, url, status, dir, filename FROM downloads
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
                download.url,
                download.kind,
                download.format_id,
                download.media_opts,
                download.checksum,
                signed_base
            ],
            |row| {
                let gone = row.get::<_, String>(2)? == "complete"
                    && row.get::<_, Option<String>>(4)?.is_some_and(|filename| {
                        row.get::<_, String>(3)
                            .is_ok_and(|dir| !std::path::Path::new(&dir).join(filename).exists())
                    });
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, gone))
            },
        )?;
        for candidate in candidates {
            let (id, source, gone) = candidate?;
            // The user deleted the finished file: asking again means "download
            // it again", not "show me the entry for a file that is not there".
            if gone {
                continue;
            }
            if duplicate_sources_match(&download.url, &source) {
                return Ok(DownloadInsertResult::Existing(id));
            }
        }

        Ok(DownloadInsertResult::Inserted(
            Self::insert_download_locked(conn, download)?,
        ))
    }

    fn insert_download_locked(conn: &Connection, download: &NewDownload) -> Result<i64> {
        conn.execute(
            "INSERT INTO downloads
                (url, filename, dir, kind, referrer, category_id, status, created_at,
                 user_agent, cookie, extra_headers, page_url, format_id,
                 package_id, mime, checksum, media_opts)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'queued', ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                download.url,
                download.filename,
                download.dir,
                download.kind,
                download.referrer,
                download.category_id,
                now(),
                download.user_agent,
                download.cookie,
                download.extra_headers,
                download.page_url,
                download.format_id,
                download.package_id,
                download.mime,
                download.checksum,
                download.media_opts
            ],
        )?;
        let id = conn.last_insert_rowid();
        record_transition(conn, id, None, DownloadStatus::Queued, "created", None)?;
        Ok(id)
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
        let mut statement = conn.prepare(&sql)?;
        let rows = if has_filter {
            statement
                .query_map(params![status.unwrap()], row_to_download)?
                .collect::<rusqlite::Result<Vec<_>>>()?
        } else {
            statement
                .query_map([], row_to_download)?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        Ok(rows)
    }

    /// Rows the app thinks are still running (active/waiting/paused/queued) —
    /// the reconciliation candidates on startup.
    pub fn running_rows(&self) -> Result<Vec<Download>> {
        let conn = self.lock();
        let sql = format!(
            "SELECT {COLS} FROM downloads WHERE status IN ('active','waiting','paused','queued')"
        );
        let mut statement = conn.prepare(&sql)?;
        let rows = statement
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
        let mut statement = conn.prepare(&sql)?;
        let rows = statement
            .query_map([], row_to_download)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Record where aria2 writes this job before it is moved into `dir`.
    pub fn set_stage_dir(&self, id: i64, stage_dir: Option<&str>) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "UPDATE downloads SET stage_dir=?1 WHERE id=?2",
            params![stage_dir, id],
        )?;
        Ok(())
    }

    /// Move a row to `to` if the lifecycle allows it from its current status
    /// ([`DownloadStatus::can_transition_to`]). The write is a compare-and-set
    /// on the status that was read, clears credentials on `complete`/`removed`,
    /// keeps `finished_at`/`start_at`/error fields consistent with the new
    /// status, and journals `cause`. Returns `false` (and changes nothing) for
    /// a refused or no-op transition.
    pub fn transition(&self, id: i64, to: DownloadStatus, cause: &str) -> Result<bool> {
        let conn = self.lock();
        transition_locked(&conn, id, None, to, cause, None)
    }

    /// [`Self::transition`], but only out of one of the `from` statuses.
    pub fn transition_if(
        &self,
        id: i64,
        from: &[DownloadStatus],
        to: DownloadStatus,
        cause: &str,
    ) -> Result<bool> {
        let conn = self.lock();
        transition_locked(&conn, id, Some(from), to, cause, None)
    }

    /// Transition to `error`, recording the engine's code and message.
    pub fn fail(
        &self,
        id: i64,
        code: Option<&str>,
        message: Option<&str>,
        cause: &str,
    ) -> Result<bool> {
        let conn = self.lock();
        transition_locked(
            &conn,
            id,
            None,
            DownloadStatus::Error,
            cause,
            Some((code, message)),
        )
    }

    /// Journal an event that does not change the status (the status is kept as
    /// both `from` and `to`).
    pub fn note(&self, id: i64, cause: &str, detail: Option<&str>) -> Result<()> {
        let conn = self.lock();
        let Some(raw) = conn
            .query_row(
                "SELECT status FROM downloads WHERE id=?1",
                params![id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        else {
            return Ok(());
        };
        let status = DownloadStatus::parse(&raw).unwrap_or(DownloadStatus::Error);
        record_transition(&conn, id, Some(status), status, cause, detail)?;
        Ok(())
    }

    /// The journal of a download, oldest first.
    pub fn transitions(&self, id: i64) -> Result<Vec<Transition>> {
        let conn = self.lock();
        let mut statement = conn.prepare(
            "SELECT at, from_status, to_status, cause, detail FROM download_events
             WHERE download_id=?1 ORDER BY id",
        )?;
        let rows = statement
            .query_map(params![id], |row| {
                let from: Option<String> = row.get(1)?;
                let to: String = row.get(2)?;
                Ok(Transition {
                    at: row.get(0)?,
                    from: from.as_deref().and_then(DownloadStatus::parse),
                    to: DownloadStatus::parse(&to).unwrap_or(DownloadStatus::Error),
                    cause: row.get(3)?,
                    detail: row.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
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
    /// prevention.
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
        let mut statement = conn.prepare(&sql)?;
        let rows = statement
            .query_map(params![now], row_to_download)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}

fn record_transition(
    conn: &Connection,
    id: i64,
    from: Option<DownloadStatus>,
    to: DownloadStatus,
    cause: &str,
    detail: Option<&str>,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO download_events (download_id, at, from_status, to_status, cause, detail)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            id,
            now(),
            from.map(|status| status.as_str()),
            to.as_str(),
            cause,
            detail
        ],
    )?;
    conn.execute(
        "DELETE FROM download_events WHERE download_id=?1 AND id NOT IN (
            SELECT id FROM download_events WHERE download_id=?1 ORDER BY id DESC LIMIT ?2)",
        params![id, JOURNAL_PER_DOWNLOAD],
    )?;
    Ok(())
}

fn transition_locked(
    conn: &Connection,
    id: i64,
    from: Option<&[DownloadStatus]>,
    to: DownloadStatus,
    cause: &str,
    error: Option<(Option<&str>, Option<&str>)>,
) -> Result<bool> {
    let Some(raw) = conn
        .query_row(
            "SELECT status FROM downloads WHERE id=?1",
            params![id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    else {
        return Ok(false);
    };
    let current = DownloadStatus::parse(&raw);
    // An unreadable status may only be replaced by an explicit restart.
    let allowed = match current {
        Some(current) => {
            current.can_transition_to(to) && from.is_none_or(|from| from.contains(&current))
        }
        None => from.is_none() && to == DownloadStatus::Active,
    };
    if !allowed {
        return Ok(false);
    }
    let (code, message) = error.unwrap_or((None, None));
    let tx = conn.unchecked_transaction()?;
    let changed = tx.execute(
        "UPDATE downloads
         SET status=?1,
             finished_at=?2,
             start_at=CASE WHEN ?1='scheduled' THEN start_at ELSE NULL END,
             error_code=?3,
             error_message=?4,
             cookie=CASE WHEN ?1 IN ('complete','removed') THEN NULL ELSE cookie END,
             extra_headers=CASE WHEN ?1 IN ('complete','removed') THEN NULL ELSE extra_headers END
         WHERE id=?5 AND status=?6",
        params![
            to.as_str(),
            to.is_terminal().then(now),
            code,
            message,
            id,
            raw
        ],
    )?;
    if changed == 0 {
        return Ok(false);
    }
    record_transition(&tx, id, current, to, cause, message)?;
    tx.commit()?;
    Ok(true)
}
