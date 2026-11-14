#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_arguments_cover_links_files_and_private_deep_links() {
        assert!(matches!(
            command_for_argument("magnet:?xt=urn:btih:abcdef"),
            Some(ControlCommand::IngestUrl { .. })
        ));
        assert!(matches!(
            command_for_argument("/tmp/example.torrent"),
            Some(ControlCommand::IngestFile {
                kind: minidl_ipc::DownloadKind::Torrent,
                ..
            })
        ));
        assert!(matches!(
            command_for_argument(
                "minidownloader://add?url=https%3A%2F%2Fexample.org%2Farchive.zip"
            ),
            Some(ControlCommand::IngestUrl { .. })
        ));
    }

    #[test]
    fn add_field_keeps_the_typed_checksum_for_ordinary_links() {
        let digest = "a".repeat(64);
        assert!(matches!(
            command_for_add_field(" https://example.org/file.iso ", &digest),
            ControlCommand::IngestUrl { url, checksum: Some(checksum) }
                if url == "https://example.org/file.iso" && checksum == digest
        ));
        assert!(matches!(
            command_for_add_field("https://example.org/file.iso", "  "),
            ControlCommand::IngestUrl { checksum: None, .. }
        ));
        assert!(matches!(
            command_for_add_field("/tmp/example.torrent", &digest),
            ControlCommand::IngestFile { .. }
        ));
    }

    #[test]
    fn search_matches_non_ascii_names_case_insensitively() {
        let catalog = locale::Catalog::new("en");
        let mut row: minidl_app::controller::DownloadView = serde_json::from_value(
            serde_json::json!({
                "id": 1, "gid": null, "url": "https://example.org/a", "filename": "ÖDEV Çalışması.pdf",
                "dir": "/dl", "status": "complete", "kind": "http", "total_bytes": 0,
                "completed_bytes": 0, "download_speed": 0, "upload_speed": 0, "connections": 0,
                "num_seeders": 0, "referrer": null, "info_hash": null, "error_code": null,
                "error_message": null, "category_id": null, "created_at": 0, "finished_at": null,
                "page_url": null, "format_id": null, "speed_limit": null, "package_id": null,
                "mime": null, "checksum": null, "start_at": null, "media_opts": null
            }),
        )
        .unwrap();
        let found = |search: &str, row: &minidl_app::controller::DownloadView| {
            rows_from_downloads(
                std::slice::from_ref(row),
                0,
                search,
                &catalog,
                &HashSet::new(),
                &HashSet::new(),
                false,
            )
            .len()
        };
        assert_eq!(found("ödev çalış", &row), 1);
        row.filename = Some("other.pdf".into());
        assert_eq!(found("ödev", &row), 0);
    }

    #[test]
    fn category_editor_preserves_legacy_and_structured_rule_shapes() {
        assert_eq!(category_rules("zip, pdf", "", ""), r#"["zip","pdf"]"#);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&category_rules(
                "mp4",
                "video/",
                ".example.com"
            ))
            .unwrap(),
            serde_json::json!([
                {"match": "ext", "values": ["mp4"]},
                {"match": "mime", "values": ["video/"]},
                {"match": "host", "values": [".example.com"]}
            ])
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn tray_icon_uses_canonical_embedded_app_asset() {
        let icon = canonical_linux_tray_icon();
        assert_eq!(icon.width, 32);
        assert_eq!(icon.height, 32);
        assert_eq!(icon.data.len(), 32 * 32 * 4);
        assert!(icon.data.chunks_exact(4).any(|pixel| pixel[0] != 0));
    }

    #[test]
    fn default_speed_round_trips_as_megabytes_per_second() {
        assert_eq!(default_speed_mb_to_bytes(0.0), 0);
        assert_eq!(default_speed_mb_to_bytes(0.5), 500_000);
        assert_eq!(default_speed_mb_to_bytes(1.0), 1_000_000);
        assert_eq!(default_speed_bytes_to_mb(500_000), 0.5);
        assert_eq!(default_speed_bytes_to_mb(1_500_000), 1.5);
    }

    #[test]
    fn transfer_values_use_the_requested_precision() {
        assert_eq!(format_bytes(1_760_000_000), "1.76 GB");
        assert_eq!(format_bytes(2_340_000_000), "2.34 GB");
        assert_eq!(format_speed(2_345_678), "2.35 MB/s");
        assert_eq!(format_percentage(0.752), "75.2%");
        assert_eq!(format_percentage(1.5), "100.0%");
    }

    #[test]
    fn eta_is_localized_rounded_up_and_only_shown_for_active_transfers() {
        use minidl_core::model::DownloadStatus;

        let english = locale::Catalog::new("en");
        let turkish = locale::Catalog::new("tr");
        assert_eq!(
            estimated_time_remaining(DownloadStatus::Active, 0, 252, 1, &english).as_deref(),
            Some("4m 12s")
        );
        assert_eq!(format_duration(42, &english), "42s");
        assert_eq!(format_duration(7_440, &english), "2h 04m");
        assert_eq!(format_duration(266_400, &english), "3d 02h");
        assert_eq!(format_duration(257, &turkish), "4d 17s");
        assert_eq!(format_duration(7_440, &turkish), "2sa 04d");
        assert_eq!(format_duration(266_400, &turkish), "3g 02sa");
        assert_eq!(
            estimated_time_remaining(DownloadStatus::Active, 0, 101, 100, &english).as_deref(),
            Some("2s")
        );
        assert!(
            estimated_time_remaining(DownloadStatus::Paused, 0, 100, 10, &english).is_none()
        );
        assert!(estimated_time_remaining(DownloadStatus::Active, 0, 0, 10, &english).is_none());
        assert!(estimated_time_remaining(DownloadStatus::Active, 0, 100, 0, &english).is_none());
    }

    #[test]
    fn live_ticks_update_the_ui_cache_without_a_database_snapshot() {
        let mut downloads = vec![minidl_app::controller::DownloadView {
            id: 7,
            gid: Some("gid".into()),
            url: "https://example.org/file".into(),
            filename: Some("file".into()),
            dir: "/tmp".into(),
            status: minidl_core::model::DownloadStatus::Active,
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
            page_url: None,
            format_id: None,
            speed_limit: None,
            package_id: None,
            package_name: None,
            mime: None,
            checksum: None,
            start_at: None,
            media_opts: None,
            path: None,
            is_dir: false,
            entries: None,
        }];
        let update = LiveDownloadUpdate {
            id: 7,
            completed: 25,
            total: 100,
            dl_speed: 50,
            ul_speed: 2,
            connections: 4,
            num_seeders: 3,
        };

        assert!(apply_live_downloads(&mut downloads, &[update]));
        assert_eq!(downloads[0].completed_bytes, 25);
        assert_eq!(downloads[0].total_bytes, 100);
        assert_eq!(downloads[0].download_speed, 50);
        assert_eq!(downloads[0].connections, 4);

        let rows = rows_from_downloads(
            &downloads,
            0,
            "",
            &locale::Catalog::new("en"),
            &HashSet::new(),
            &HashSet::new(),
            false,
        );
        assert_eq!(rows[0].meta_text.as_str(), "25 B / 100 B · 25.0%");
        assert_eq!(rows[0].speed_text.as_str(), "50 B/s");
        assert_eq!(rows[0].eta_text.as_str(), "2s");

        // A paused row keeps its last speed in the cache but never shows it.
        downloads[0].status = minidl_core::model::DownloadStatus::Paused;
        let rows = rows_from_downloads(
            &downloads,
            0,
            "",
            &locale::Catalog::new("en"),
            &HashSet::new(),
            &HashSet::new(),
            false,
        );
        assert_eq!(rows[0].speed_text.as_str(), "");
        assert_eq!(rows[0].eta_text.as_str(), "");
        assert!(rows[0].can_resume && !rows[0].can_pause);
    }

    #[test]
    fn the_title_bar_rate_counts_only_running_transfers() {
        let mut rows = vec![
            sample("http", "active"),
            sample("http", "active"),
            sample("http", "paused"),
            sample("http", "complete"),
        ];
        rows[0].download_speed = 2_000_000;
        rows[1].download_speed = 345_678;
        rows[2].download_speed = 9_000_000;
        assert_eq!(total_download_speed(&rows), 2_345_678);
        assert_eq!(format_speed(total_download_speed(&rows)), "2.35 MB/s");
        rows[1].download_speed = i64::MAX;
        assert_eq!(total_download_speed(&rows), i64::MAX);
        // Idle: nothing to show, the bar stays still.
        assert_eq!(format_speed(total_download_speed(&rows[2..])), "");
    }

    fn sample(kind: &str, status: &str) -> minidl_app::controller::DownloadView {
        serde_json::from_value(serde_json::json!({
            "id": 1, "gid": null, "url": "https://example.org/a.iso", "filename": null,
            "dir": "/dl", "status": status, "kind": kind, "total_bytes": 100,
            "completed_bytes": 10, "download_speed": 5, "upload_speed": 0, "connections": 0,
            "num_seeders": 0, "referrer": null, "info_hash": null, "error_code": null,
            "error_message": null, "category_id": null, "created_at": 0, "finished_at": null,
            "page_url": null, "format_id": null, "speed_limit": null, "package_id": null,
            "mime": null, "checksum": null, "start_at": null, "media_opts": null
        }))
        .unwrap()
    }

    #[test]
    fn every_status_has_one_row_state_with_exactly_its_actions() {
        use ui::state::{row_actions, row_state, status_key, RowActions};

        let state = |kind: &str, status: &str, restarting| row_state(&sample(kind, status), restarting);
        assert_eq!(state("http", "active", false), RowState::Downloading);
        assert_eq!(state("http", "active", true), RowState::EngineRestarting);
        assert_eq!(state("video", "active", true), RowState::Downloading);
        assert_eq!(state("http", "waiting", false), RowState::Waiting);
        assert_eq!(state("http", "queued", false), RowState::Queued);
        assert_eq!(state("http", "scheduled", false), RowState::Scheduled);
        assert_eq!(state("http", "complete", false), RowState::Done);
        assert_eq!(state("http", "error", false), RowState::Failed);
        let mut interrupted = sample("video", "error");
        interrupted.error_code = Some(minidl_app::controller::INTERRUPTED.into());
        assert_eq!(row_state(&interrupted, false), RowState::Interrupted);

        assert_eq!(
            row_actions(RowState::Queued, "http"),
            RowActions { pause: true, ..RowActions::default() }
        );
        assert!(row_actions(RowState::Interrupted, "video").resume);
        assert!(row_actions(RowState::Failed, "http").retry);
        assert!(!row_actions(RowState::Failed, "torrent").retry);
        assert!(!row_actions(RowState::Failed, "metalink").retry);
        assert_eq!(
            row_actions(RowState::Done, "http"),
            RowActions { open: true, ..RowActions::default() }
        );
        assert_eq!(row_actions(RowState::Removed, "http"), RowActions::default());
        let english = locale::Catalog::new("en");
        for state in [
            RowState::Queued, RowState::Waiting, RowState::Downloading,
            RowState::EngineRestarting, RowState::Paused, RowState::Scheduled,
            RowState::Done, RowState::Failed, RowState::Interrupted, RowState::Removed,
        ] {
            assert_ne!(english.get(status_key(state)), status_key(state));
        }
    }

    #[test]
    fn a_snapshot_never_moves_a_running_bar_backwards() {
        let mut live = sample("http", "active");
        live.completed_bytes = 60;
        live.download_speed = 9;
        let mut stale = sample("http", "active");
        stale.completed_bytes = 40;
        let merged = ui::state::merge_snapshot(&[live.clone()], vec![stale.clone()]);
        assert_eq!(merged[0].completed_bytes, 60);
        assert_eq!(merged[0].download_speed, 9);
        stale.status = minidl_core::model::DownloadStatus::Paused;
        let merged = ui::state::merge_snapshot(&[live], vec![stale]);
        assert_eq!(merged[0].completed_bytes, 40);
    }

    #[test]
    fn names_cannot_reorder_themselves_on_screen() {
        let mut row = sample("http", "complete");
        row.filename = Some("invoice\u{202E}fdp.exe".into());
        let rows = rows_from_downloads(
            &[row],
            0,
            "",
            &locale::Catalog::new("en"),
            &HashSet::new(),
            &HashSet::new(),
            false,
        );
        assert_eq!(rows[0].name.as_str(), "invoice\u{FFFD}fdp.exe");
        assert_eq!(rows[0].file_type.as_str(), "executable");
        assert_eq!(rows[0].type_label.as_str(), "Program");
    }

    #[test]
    fn sync_never_drops_a_change_and_resyncs_on_gaps() {
        let mut sync = ui::state::SyncState::starting_at(10);
        assert!(!sync.observe(11));
        assert!(sync.request());
        assert!(!sync.request()); // in flight: remembered, not dropped
        assert!(sync.finished(Some(12))); // so fetch once more
        assert!(!sync.finished(Some(13)));
        assert!(sync.request());
        assert!(!sync.finished(None));
        assert!(sync.observe(20)); // events 14..19 were missed
        assert!(!sync.observe(21));
    }

    #[test]
    fn notices_queue_by_origin_with_errors_first() {
        use ui::Origin;
        let mut notices = ui::state::Notices::default();
        notices.push(Origin::Page, false, "info");
        notices.push(Origin::Page, true, "boom");
        notices.push(Origin::Media, true, "probe failed");
        notices.push(Origin::Page, true, "boom"); // duplicate moves up, no copy
        let page = |n: &ui::state::Notices<Origin>| {
            n.items()
                .iter()
                .filter(|x| x.origin == Origin::Page)
                .map(|x| x.text.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(page(&notices), ["boom", "info"]);
        notices.clear(Origin::Media); // a media success leaves the page alone
        assert_eq!(page(&notices), ["boom", "info"]);
        assert_eq!(notices.items().len(), 2);
        for n in 0..10 {
            notices.push(Origin::Page, true, format!("e{n}"));
        }
        assert_eq!(page(&notices).len(), 4);
        let id = notices.items()[0].id;
        notices.dismiss(id);
        assert_eq!(page(&notices).len(), 3);
    }

    #[test]
    fn only_the_latest_probe_can_be_added() {
        let mut probe = ui::state::Probe::default();
        probe.start("https://a", 1);
        probe.start("https://b", 2);
        assert!(!probe.finish(1, true)); // stale result ignored
        assert!(!probe.can_add("https://b"));
        assert!(probe.finish(2, true));
        assert!(probe.can_add("https://b"));
        assert!(!probe.can_add("https://a"));
        probe.start("https://c", 3);
        assert!(probe.finish(3, false));
        assert!(!probe.can_add("https://c"));
    }

    #[test]
    fn settings_saves_are_serialised_and_the_latest_wins() {
        use ui::state::{SaveNext, SettingsSaver};
        let mut saver = SettingsSaver::new(0);
        assert_eq!(saver.submit(1), Some(1));
        assert_eq!(saver.submit(2), None); // queued behind the first
        assert_eq!(saver.submit(3), None); // replaces it
        assert!(!saver.adopt(9)); // no outside value while ours is in flight
        assert_eq!(saver.finished(1, true), SaveNext::Send(3));
        assert_eq!(saver.finished(3, false), SaveNext::Revert(1));
        assert_eq!(saver.submit(1), None); // equal to what is saved
        assert!(saver.adopt(5));
        assert_eq!(saver.submit(5), None);
    }

    #[test]
    fn notification_bodies_are_plain_text() {
        assert_eq!(
            controller_process::notification_text("<b>a</b> & \u{202E}x"),
            "&lt;b&gt;a&lt;/b&gt; &amp; \u{FFFD}x"
        );
    }
}
