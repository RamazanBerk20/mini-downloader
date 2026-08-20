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
            .set_status(id, DownloadStatus::Complete)
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
}
