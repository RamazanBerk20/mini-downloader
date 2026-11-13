#[cfg(test)]
mod tests {
    use super::*;
    use super::schema::{MIGRATION_1, MIGRATION_2, MIGRATION_3, SCHEMA_VERSION};
    use crate::model::{DownloadStatus, NewDownload};
    use rusqlite::params;

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

        assert!(db.transition(id, DownloadStatus::Complete, "test").unwrap());
        let d = db.get(id).unwrap().unwrap();
        assert_eq!(d.status, DownloadStatus::Complete);
        assert!(d.finished_at.is_some());

        assert!(db.transition(id, DownloadStatus::Active, "retry").unwrap());
        let d = db.get(id).unwrap().unwrap();
        assert_eq!(d.status, DownloadStatus::Active);
        assert!(d.finished_at.is_none());
    }

    /// A completion or error that lands before the code that started the job
    /// writes `active` must win: the late write is refused, not applied.
    #[test]
    fn late_or_duplicate_events_never_move_a_row_backwards() {
        let db = Db::open_in_memory().unwrap();
        let id = db
            .insert_download(&NewDownload {
                url: "https://x/fast.bin".into(),
                dir: "/dl".into(),
                kind: "http".into(),
                ..Default::default()
            })
            .unwrap();
        assert!(db
            .transition_if(id, &[DownloadStatus::Queued], DownloadStatus::Active, "ingest")
            .unwrap());
        assert!(db.transition(id, DownloadStatus::Complete, "aria2").unwrap());
        // Duplicate completion (WebSocket + poller) fires side effects once.
        assert!(!db.transition(id, DownloadStatus::Complete, "aria2").unwrap());
        // A stale paused/error report after completion is ignored.
        assert!(!db.transition(id, DownloadStatus::Paused, "aria2").unwrap());
        assert!(!db.fail(id, Some("1"), Some("late"), "aria2").unwrap());
        assert!(!db
            .transition_if(id, &[DownloadStatus::Queued], DownloadStatus::Active, "ingest")
            .unwrap());
        let row = db.get(id).unwrap().unwrap();
        assert_eq!(row.status, DownloadStatus::Complete);
        assert!(row.error_message.is_none());

        let journal = db.transitions(id).unwrap();
        let path: Vec<_> = journal.iter().map(|t| t.to.as_str()).collect();
        assert_eq!(path, ["queued", "active", "complete"]);
        assert_eq!(journal[1].from, Some(DownloadStatus::Queued));
        assert_eq!(journal[2].cause, "aria2");
    }

    #[test]
    fn explain_names_the_cause_of_the_current_state() {
        let db = Db::open_in_memory().unwrap();
        let id = db
            .insert_download(&NewDownload {
                url: "https://x/broken.bin".into(),
                dir: "/dl".into(),
                kind: "http".into(),
                ..Default::default()
            })
            .unwrap();
        db.transition(id, DownloadStatus::Active, "ingest: handed to aria2")
            .unwrap();
        db.fail(id, Some("3"), Some("Resource not found"), "aria2: download failed")
            .unwrap();
        let row = db.get(id).unwrap().unwrap();
        let text = crate::model::explain(&row, &db.transitions(id).unwrap(), crate::model::now());
        assert!(text.starts_with(&format!("#{id} is error since 0s ago: aria2: download failed")), "{text}");
        assert!(text.contains("error: Resource not found (aria2 code 3)"));
        assert!(text.contains("queued -> active  ingest: handed to aria2"));
    }

    /// A removed download's id is never handed out again: a reused id would
    /// inherit the old row's staging folder, partial file and control file.
    #[test]
    fn download_ids_are_never_reused() {
        let db = Db::open_in_memory().unwrap();
        let new = || NewDownload {
            url: "https://x/a".into(),
            dir: "/dl".into(),
            kind: "http".into(),
            ..Default::default()
        };
        let first = db.insert_download(&new()).unwrap();
        let second = db.insert_download(&new()).unwrap();
        db.delete(second).unwrap();
        let third = db.insert_download(&new()).unwrap();
        assert!(third > second && second > first, "{first} {second} {third}");
    }

    #[test]
    fn v5_database_keeps_rows_and_journal_through_the_id_migration() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(MIGRATION_1).unwrap();
        conn.execute_batch(MIGRATION_2).unwrap();
        conn.execute_batch(MIGRATION_3).unwrap();
        conn.execute_batch(super::schema::MIGRATION_4).unwrap();
        conn.execute_batch(super::schema::MIGRATION_5).unwrap();
        conn.execute_batch(
            "INSERT INTO downloads (id, url, dir, status, created_at, stage_dir)
                 VALUES (7, 'https://example.org/a', '/dl', 'active', 1, '/dl/.minidl-staging/7');
             INSERT INTO download_events (download_id, at, to_status, cause)
                 VALUES (7, 1, 'active', 'ingest');",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 5).unwrap();

        let db = Db::init(conn).unwrap();
        let row = db.get(7).unwrap().unwrap();
        assert_eq!(row.stage_dir.as_deref(), Some("/dl/.minidl-staging/7"));
        assert_eq!(db.transitions(7).unwrap().len(), 1);
        db.delete(7).unwrap();
        assert!(db.transitions(7).unwrap().is_empty(), "journal cascade still works");
        let next = db
            .insert_download(&NewDownload {
                url: "https://x/b".into(),
                dir: "/dl".into(),
                kind: "http".into(),
                ..Default::default()
            })
            .unwrap();
        assert!(next > 7);
    }

    #[test]
    fn transition_table_allows_only_lifecycle_edges() {
        use DownloadStatus::*;
        let all = [
            Queued, Active, Waiting, Paused, Complete, Error, Removed, Scheduled,
        ];
        for from in all {
            for to in all {
                let expected = from != to
                    && to != Queued
                    && (!from.is_terminal() || matches!(to, Active | Scheduled | Removed));
                assert_eq!(from.can_transition_to(to), expected, "{from:?} -> {to:?}");
            }
        }
    }

    #[test]
    fn journal_is_bounded_per_download() {
        let db = Db::open_in_memory().unwrap();
        let id = db
            .insert_download(&NewDownload {
                url: "https://x/flappy.bin".into(),
                dir: "/dl".into(),
                kind: "http".into(),
                ..Default::default()
            })
            .unwrap();
        for _ in 0..40 {
            db.transition(id, DownloadStatus::Active, "t").unwrap();
            db.transition(id, DownloadStatus::Paused, "t").unwrap();
        }
        let journal = db.transitions(id).unwrap();
        assert_eq!(journal.len(), 50);
        assert_eq!(journal.last().unwrap().to, DownloadStatus::Paused);
        db.delete(id).unwrap();
        assert!(db.transitions(id).unwrap().is_empty());
    }

    #[test]
    fn unreadable_status_is_surfaced_not_restartable() {
        let db = Db::open_in_memory().unwrap();
        let id = db
            .insert_download(&NewDownload {
                url: "https://x/odd.bin".into(),
                dir: "/dl".into(),
                kind: "http".into(),
                ..Default::default()
            })
            .unwrap();
        db.lock()
            .execute("UPDATE downloads SET status='bogus' WHERE id=?1", params![id])
            .unwrap();
        let row = db.get(id).unwrap().unwrap();
        assert_eq!(row.status, DownloadStatus::Error);
        assert!(row.error_message.unwrap().contains("bogus"));
        assert!(!db.transition(id, DownloadStatus::Paused, "t").unwrap());
        assert!(db.transition(id, DownloadStatus::Active, "retry").unwrap());
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
        db.transition(id, DownloadStatus::Complete, "test").unwrap();
        let row = db.get(id).unwrap().unwrap();
        assert!(row.cookie.is_none());
        assert!(row.extra_headers.is_none());
    }

    #[test]
    fn removed_and_long_failed_rows_drop_replay_credentials() {
        let db = Db::open_in_memory().unwrap();
        let new = |url: &str| NewDownload {
            url: url.into(),
            dir: "/dl".into(),
            kind: "http".into(),
            cookie: Some("session=secret".into()),
            extra_headers: Some(r#"[["Authorization","Bearer secret"]]"#.into()),
            ..Default::default()
        };
        let removed = db.insert_download(&new("https://x/removed")).unwrap();
        db.transition(removed, DownloadStatus::Removed, "test").unwrap();
        assert!(db.get(removed).unwrap().unwrap().cookie.is_none());

        let failed = db.insert_download(&new("https://x/failed")).unwrap();
        let stale = db.insert_download(&new("https://x/stale")).unwrap();
        db.fail(failed, None, Some("boom"), "test").unwrap();
        db.fail(stale, None, Some("boom"), "test").unwrap();
        // Retry still has its credentials right after a failure.
        assert!(db.get(failed).unwrap().unwrap().cookie.is_some());
        db.lock()
            .execute(
                "UPDATE downloads SET finished_at=0 WHERE id=?1",
                params![stale],
            )
            .unwrap();
        db.migrate().unwrap();
        assert!(db.get(failed).unwrap().unwrap().cookie.is_some());
        let stale = db.get(stale).unwrap().unwrap();
        assert!(stale.cookie.is_none() && stale.extra_headers.is_none());
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

        db.transition(first_id, DownloadStatus::Complete, "test")
            .unwrap();
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

        db.transition(first_id, DownloadStatus::Active, "retry")
            .unwrap();
        db.fail(first_id, None, Some("network failed"), "test")
            .unwrap();
        let retry = db.insert_download_deduplicated(&download).unwrap();
        assert!(matches!(retry, DownloadInsertResult::Inserted(id) if id != first_id));
        assert_eq!(db.list(None).unwrap().len(), 2);
    }

    #[test]
    fn completed_row_whose_file_was_deleted_does_not_block_a_new_download() {
        let root = tempfile::tempdir().unwrap();
        let db = Db::open_in_memory().unwrap();
        let download = NewDownload {
            url: "https://x/gone.iso".into(),
            filename: Some("gone.iso".into()),
            dir: root.path().to_string_lossy().into_owned(),
            kind: "http".into(),
            ..Default::default()
        };
        let first = db.insert_download_deduplicated(&download).unwrap().id();
        db.transition(first, DownloadStatus::Complete, "test").unwrap();

        std::fs::write(root.path().join("gone.iso"), b"data").unwrap();
        assert_eq!(
            db.insert_download_deduplicated(&download).unwrap(),
            DownloadInsertResult::Existing(first)
        );
        std::fs::remove_file(root.path().join("gone.iso")).unwrap();
        assert!(matches!(
            db.insert_download_deduplicated(&download).unwrap(),
            DownloadInsertResult::Inserted(id) if id != first
        ));
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

        // Identity-bearing fields next to a signature still name the file:
        // `key=A` and `key=B` are two downloads, not one rotated link.
        let mut keyed = first.clone();
        keyed.url = "https://files.example/dl?key=A&token=T&expires=1".into();
        let keyed_id = db
            .insert_download_with_duplicate_policy(&keyed)
            .unwrap()
            .id();
        keyed.url = "https://files.example/dl?key=B&token=T&expires=1".into();
        assert_ne!(
            db.insert_download_with_duplicate_policy(&keyed).unwrap(),
            DownloadInsertResult::Existing(keyed_id)
        );
        keyed.url = "https://files.example/dl?expires=2&token=U&key=A".into();
        assert_eq!(
            db.insert_download_with_duplicate_policy(&keyed).unwrap(),
            DownloadInsertResult::Existing(keyed_id)
        );
        let mut hashed = first.clone();
        hashed.url = "https://files.example/get?h=one&exp=1&sig=s".into();
        let hashed_id = db
            .insert_download_with_duplicate_policy(&hashed)
            .unwrap()
            .id();
        hashed.url = "https://files.example/get?h=two&exp=1&sig=s".into();
        assert_ne!(
            db.insert_download_with_duplicate_policy(&hashed).unwrap(),
            DownloadInsertResult::Existing(hashed_id)
        );

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

        db.transition(id, DownloadStatus::Scheduled, "test").unwrap();
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
        assert!(columns.iter().any(|column| column == "stage_dir"));
    }

    /// A download in flight across the upgrade keeps finishing where it was:
    /// no staging folder, journal starts empty, transitions still work.
    #[test]
    fn v4_rows_in_flight_survive_the_staging_migration() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(MIGRATION_1).unwrap();
        conn.execute_batch(MIGRATION_2).unwrap();
        conn.execute_batch(MIGRATION_3).unwrap();
        conn.execute_batch(super::schema::MIGRATION_4).unwrap();
        conn.execute(
            "INSERT INTO downloads (url, dir, status, gid, session_gid, created_at)
             VALUES ('https://example.org/big.iso', '/dl', 'active', 'g1', 'g1', 1)",
            [],
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 4).unwrap();

        let db = Db::init(conn).unwrap();
        let row = db.list(None).unwrap().pop().unwrap();
        assert_eq!(row.status, DownloadStatus::Active);
        assert!(row.stage_dir.is_none());
        assert!(db.transition(row.id, DownloadStatus::Complete, "aria2").unwrap());
        assert_eq!(db.transitions(row.id).unwrap().len(), 1);
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
        db.transition(id, DownloadStatus::Scheduled, "test").unwrap();

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
