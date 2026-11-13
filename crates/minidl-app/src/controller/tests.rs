#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_does_not_serialize_replay_secrets() {
        let row = Download {
            id: 1,
            gid: None,
            session_gid: None,
            url: "https://example.org".into(),
            filename: None,
            dir: "/tmp".into(),
            status: DownloadStatus::Paused,
            kind: "http".into(),
            total_bytes: 0,
            completed_bytes: 0,
            download_speed: 0,
            upload_speed: 0,
            connections: 0,
            num_seeders: 0,
            referrer: None,
            info_hash: None,
            error_code: None,
            error_message: None,
            category_id: None,
            created_at: 0,
            finished_at: None,
            user_agent: Some("secret-agent".into()),
            cookie: Some("secret-cookie".into()),
            extra_headers: Some("secret-header".into()),
            page_url: None,
            format_id: None,
            speed_limit: None,
            package_id: None,
            mime: None,
            checksum: None,
            start_at: None,
            media_opts: None,
            stage_dir: None,
        };
        let json = serde_json::to_string(&DownloadView::from(row)).unwrap();
        assert!(!json.contains("secret-agent"));
        assert!(!json.contains("secret-cookie"));
        assert!(!json.contains("secret-header"));
    }

    #[test]
    fn checksum_validation_is_strict() {
        assert!(normalize_checksum(Some("f".repeat(64))).is_ok());
        assert!(normalize_checksum(Some("f".repeat(63))).is_err());
    }

    #[test]
    fn simple_category_extensions_become_structured_rules() {
        assert_eq!(
            normalize_category_rules(".zip, PDF").unwrap(),
            r#"[{"match":"ext","values":["zip","pdf"]}]"#
        );
    }

    #[tokio::test]
    async fn ytdlp_completion_uses_the_shared_completion_event_path() {
        let root = tempfile::tempdir().unwrap();
        let downloads = root.path().join("downloads");
        std::fs::create_dir_all(&downloads).unwrap();
        let controller = Controller::open(root.path().join("data"), downloads.clone())
            .await
            .unwrap();
        let id = controller
            .db
            .insert_download(&NewDownload {
                url: "https://example.org/watch?v=1".into(),
                filename: Some("clip.webm".into()),
                dir: downloads.to_string_lossy().into_owned(),
                kind: "video".into(),
                ..Default::default()
            })
            .unwrap();
        controller
            .db
            .transition(id, DownloadStatus::Complete, "test")
            .unwrap();
        let mut events = controller.events.subscribe();

        controller
            .handle_ytdlp_completion(YtDlpCompletion {
                id,
                name: Some("clip.webm".into()),
                path: Some(downloads.join("clip.webm").to_string_lossy().into_owned()),
            })
            .await;

        let complete = events.recv().await.unwrap().0;
        let state = events.recv().await.unwrap().0;
        assert_eq!(complete.event, "downloads:complete");
        assert_eq!(complete.payload["id"], id);
        assert_eq!(state.event, "downloads:state");
        assert_eq!(state.payload["status"], "complete");
    }

    #[tokio::test]
    async fn stored_download_details_keep_the_completed_file_without_a_live_engine() {
        let root = tempfile::tempdir().unwrap();
        let downloads = root.path().join("downloads");
        std::fs::create_dir_all(&downloads).unwrap();
        let controller = Controller::open(root.path().join("data"), downloads.clone())
            .await
            .unwrap();
        let id = controller
            .db
            .insert_download(&NewDownload {
                url: "https://example.org/archive.zip".into(),
                filename: Some("archive.zip".into()),
                dir: downloads.to_string_lossy().into_owned(),
                kind: "http".into(),
                ..Default::default()
            })
            .unwrap();
        controller
            .db
            .checkpoint_progress_by_id(id, 54_000_000, 54_000_000, 0)
            .unwrap();

        let details = controller.details(id).await.unwrap();

        assert_eq!(details.files.len(), 1);
        assert_eq!(
            details.files[0].path,
            downloads.join("archive.zip").to_string_lossy()
        );
        assert_eq!(details.files[0].completed_length, 54_000_000);
        assert_eq!(details.files[0].length, 54_000_000);
    }

    async fn open_controller(root: &Path) -> (Arc<Controller>, PathBuf) {
        let downloads = root.join("downloads");
        std::fs::create_dir_all(&downloads).unwrap();
        let controller = Controller::open(root.join("data"), downloads.clone())
            .await
            .unwrap();
        (controller, downloads)
    }

    fn insert(controller: &Controller, downloads: &Path, kind: &str, name: Option<&str>) -> i64 {
        controller
            .db
            .insert_download(&NewDownload {
                url: "https://example.org/x".into(),
                filename: name.map(str::to_owned),
                dir: downloads.to_string_lossy().into_owned(),
                kind: kind.into(),
                ..Default::default()
            })
            .unwrap()
    }

    #[tokio::test]
    async fn delete_files_removes_a_finished_torrent_folder_but_never_follows_links() {
        let root = tempfile::tempdir().unwrap();
        let (controller, downloads) = open_controller(root.path()).await;
        let outside = root.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("keep.txt"), b"x").unwrap();
        std::fs::create_dir_all(downloads.join("Album")).unwrap();
        std::fs::write(downloads.join("Album/01.flac"), b"x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, downloads.join("Album/link")).unwrap();
        let id = insert(&controller, &downloads, "torrent", Some("Album"));
        controller
            .db
            .transition(id, DownloadStatus::Complete, "test")
            .unwrap();

        controller.remove(id, true).await.unwrap();

        assert!(!downloads.join("Album").exists());
        assert!(outside.join("keep.txt").exists());
        assert!(controller.db.get(id).unwrap().is_none());
    }

    /// "Clear failed" goes through the same staging cleanup as Remove: the
    /// partial is not left hidden in `.minidl-staging/<id>`.
    #[tokio::test]
    async fn clearing_failed_rows_releases_their_staging_folders() {
        let root = tempfile::tempdir().unwrap();
        let (controller, downloads) = open_controller(root.path()).await;
        let id = insert(&controller, &downloads, "http", Some("big.iso"));
        let stage = minidl_core::files::create_stage_dir(&downloads, id).unwrap();
        std::fs::write(stage.join("big.iso"), b"partial").unwrap();
        std::fs::write(stage.join("big.iso.aria2"), b"control").unwrap();
        controller
            .db
            .set_stage_dir(id, Some(&stage.to_string_lossy()))
            .unwrap();
        controller
            .db
            .fail(id, None, Some("network"), "test")
            .unwrap();

        assert_eq!(controller.remove_status("error").await.unwrap(), 1);
        assert!(!downloads.join(minidl_core::files::STAGING_DIR).exists());
        assert_eq!(
            std::fs::read(downloads.join("big.iso.part")).unwrap(),
            b"partial"
        );
    }

    /// A finished file whose move into place failed is moved by a Remove that
    /// keeps files, never renamed to `.part`.
    #[tokio::test]
    async fn removing_a_complete_row_left_in_staging_moves_its_file_into_place() {
        let root = tempfile::tempdir().unwrap();
        let (controller, downloads) = open_controller(root.path()).await;
        let id = insert(&controller, &downloads, "http", Some("done.iso"));
        let stage = minidl_core::files::create_stage_dir(&downloads, id).unwrap();
        std::fs::write(stage.join("done.iso"), b"finished").unwrap();
        controller
            .db
            .set_stage_dir(id, Some(&stage.to_string_lossy()))
            .unwrap();
        controller
            .db
            .transition(id, DownloadStatus::Complete, "test")
            .unwrap();
        let row = controller.db.get(id).unwrap().unwrap();
        assert_eq!(
            crate::completion::downloaded_path(&row).as_deref(),
            Some(stage.join("done.iso").as_path())
        );

        controller.remove(id, false).await.unwrap();
        assert_eq!(std::fs::read(downloads.join("done.iso")).unwrap(), b"finished");
        assert!(!downloads.join("done.iso.part").exists());
    }

    #[tokio::test]
    async fn removing_a_staged_download_never_leaves_a_hidden_partial() {
        let root = tempfile::tempdir().unwrap();
        let (controller, downloads) = open_controller(root.path()).await;
        let mut ids = Vec::new();
        for _ in 0..2 {
            let id = insert(&controller, &downloads, "http", Some("disk.iso"));
            let stage = minidl_core::files::create_stage_dir(&downloads, id).unwrap();
            std::fs::write(stage.join("disk.iso"), b"partial").unwrap();
            std::fs::write(stage.join("disk.iso.aria2"), b"control").unwrap();
            controller
                .db
                .set_stage_dir(id, Some(&stage.to_string_lossy()))
                .unwrap();
            controller
                .db
                .transition(id, DownloadStatus::Paused, "test")
                .unwrap();
            ids.push(id);
        }
        // An unrelated file with the same name is not the row's to delete.
        std::fs::write(downloads.join("disk.iso"), b"someone else's").unwrap();

        controller.remove(ids[0], true).await.unwrap();
        assert!(!minidl_core::files::stage_dir(&downloads, ids[0]).exists());
        assert!(downloads.join("disk.iso").exists());

        controller.remove(ids[1], false).await.unwrap();
        assert_eq!(
            std::fs::read(downloads.join("disk.iso.part")).unwrap(),
            b"partial"
        );
        assert!(!downloads.join(minidl_core::files::STAGING_DIR).exists());
    }

    #[tokio::test]
    async fn a_video_row_without_its_process_is_interrupted_after_a_restart() {
        let root = tempfile::tempdir().unwrap();
        let id = {
            let (controller, downloads) = open_controller(root.path()).await;
            let id = insert(&controller, &downloads, "video", None);
            controller
                .db
                .transition(id, DownloadStatus::Active, "test")
                .unwrap();
            id
        };
        let (controller, _) = open_controller(root.path()).await;
        let row = controller.db.get(id).unwrap().unwrap();
        assert_eq!(row.status, DownloadStatus::Error);
        assert_eq!(row.error_code.as_deref(), Some(INTERRUPTED));
        assert!(retryable(&row.kind));
        assert!(!retryable("torrent"));
    }
}
