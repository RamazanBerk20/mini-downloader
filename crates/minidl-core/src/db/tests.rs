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
