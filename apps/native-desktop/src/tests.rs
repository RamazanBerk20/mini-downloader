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
        );
        assert_eq!(rows[0].meta_text.as_str(), "25 B / 100 B · 25.0%");
        assert_eq!(rows[0].speed_text.as_str(), "50 B/s");
        assert_eq!(rows[0].eta_text.as_str(), "2s");
    }
}
