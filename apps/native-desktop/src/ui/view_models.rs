use super::*;

#[derive(serde::Deserialize)]
pub(crate) struct LiveDownloadBatch {
    #[serde(default)]
    pub(crate) updates: Vec<LiveDownloadUpdate>,
}

#[derive(serde::Deserialize)]
pub(crate) struct LiveDownloadUpdate {
    pub(crate) id: i64,
    pub(crate) completed: i64,
    pub(crate) total: i64,
    pub(crate) dl_speed: i64,
    pub(crate) ul_speed: i64,
    pub(crate) connections: i64,
    pub(crate) num_seeders: i64,
}

pub(crate) fn apply_live_downloads(
    downloads: &mut [minidl_app::controller::DownloadView],
    updates: &[LiveDownloadUpdate],
) -> bool {
    let updates = updates
        .iter()
        .map(|update| (update.id, update))
        .collect::<HashMap<_, _>>();
    let mut changed = false;
    for row in downloads {
        let Some(update) = updates.get(&row.id) else {
            continue;
        };
        let values = (
            update.completed.max(0),
            update.total.max(0),
            update.dl_speed.max(0),
            update.ul_speed.max(0),
            update.connections.max(0),
            update.num_seeders.max(0),
        );
        changed |= (
            row.completed_bytes,
            row.total_bytes,
            row.download_speed,
            row.upload_speed,
            row.connections,
            row.num_seeders,
        ) != values;
        row.completed_bytes = values.0;
        row.total_bytes = values.1;
        row.download_speed = values.2;
        row.upload_speed = values.3;
        row.connections = values.4;
        row.num_seeders = values.5;
    }
    changed
}

pub(super) fn system_prefers_dark() -> bool {
    if std::env::var("GTK_THEME")
        .ok()
        .is_some_and(|theme| theme.to_ascii_lowercase().contains("dark"))
    {
        return true;
    }

    #[cfg(target_os = "linux")]
    {
        let config_root = std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(std::path::PathBuf::from)
                    .map(|home| home.join(".config"))
            });
        if let Some(config_root) = config_root {
            for path in [
                config_root.join("kdeglobals"),
                config_root.join("kdedefaults/kdeglobals"),
            ] {
                let Ok(contents) = std::fs::read_to_string(path) else {
                    continue;
                };
                if let Some(value) = contents
                    .lines()
                    .find_map(|line| line.trim().strip_prefix("ColorScheme=").map(str::trim))
                {
                    return value.to_ascii_lowercase().contains("dark");
                }
                if let Some((red, green, blue)) = contents.lines().find_map(|line| {
                    let value = line.trim().strip_prefix("Background=")?;
                    let mut channels = value
                        .split(',')
                        .filter_map(|part| part.trim().parse::<u16>().ok());
                    Some((channels.next()?, channels.next()?, channels.next()?))
                }) {
                    return red + green + blue < 384;
                }
            }
        }
    }

    true
}

pub(super) fn apply_settings(ui: &MainWindow, snapshot: &AppSnapshot) {
    let setting = |key: &str| snapshot.settings.get(key).map(String::as_str);
    ui.set_proxy_text(setting("proxy").unwrap_or("").into());
    ui.set_max_concurrent(
        setting("max_concurrent")
            .and_then(|v| v.parse().ok())
            .unwrap_or(5),
    );
    ui.set_split_count(setting("split").and_then(|v| v.parse().ok()).unwrap_or(16));
    ui.set_connection_count(
        setting("max_conn")
            .and_then(|v| v.parse().ok())
            .unwrap_or(16),
    );
    ui.set_clipboard_watch(setting("clipboard_watch") == Some("true"));
    ui.set_auto_organize(setting("auto_organize") != Some("false"));
    ui.set_prevent_duplicates(setting("prevent_duplicate_downloads") != Some("false"));
    ui.set_close_to_tray(setting("close_to_tray") != Some("false"));
    ui.set_autostart(setting("autostart") == Some("true"));
    ui.set_handle_magnets(setting("handle_magnets") != Some("false"));
    ui.set_dht_enabled(setting("dht_enabled") != Some("false"));
    ui.set_sandbox_children(setting("sandbox_children") == Some("true"));
    ui.set_block_private_ips(setting("block_private_ips") == Some("true"));
    ui.set_theme_choice(
        match setting("theme") {
            Some("light" | "dark") => setting("theme").unwrap_or("system"),
            _ => "system",
        }
        .into(),
    );
    ui.set_system_dark(system_prefers_dark());
    ui.set_locale_choice(locale_name(setting("locale").unwrap_or("system")).into());
    let on_complete = setting("on_complete_action").unwrap_or("none");
    if let Some(command) = on_complete.strip_prefix("run:") {
        ui.set_on_complete_action("run:".into());
        ui.set_on_complete_command(command.into());
    } else {
        ui.set_on_complete_action(on_complete.into());
    }
    ui.set_on_complete_confirmed(setting("on_complete_command_confirmed") == Some("true"));
    // Mirrors the toolbar's preset cycle in main-window.slint.
    ui.set_global_speed_index(match setting("global_download_limit") {
        Some("500000") => 1,
        Some("1000000") => 2,
        Some("5000000") => 3,
        Some("10000000") => 4,
        _ => 0,
    });
    ui.set_default_speed_mb(
        setting("default_speed_limit")
            .and_then(|value| value.parse::<i64>().ok())
            .map(default_speed_bytes_to_mb)
            .unwrap_or_default(),
    );
}

pub(super) fn apply_i18n(ui: &MainWindow, catalog: &locale::Catalog) {
    macro_rules! set {
        ($setter:ident, $key:literal) => {
            ui.$setter(catalog.get($key).into());
        };
    }
    set!(set_nav_downloads, "titleAll");
    set!(set_nav_links, "grabLinksTitle");
    set!(set_nav_media, "grabVideoTitle");
    set!(set_nav_settings, "settings");
    set!(set_label_filters, "navStatus");
    set!(set_label_categories, "sectCategories");
    set!(set_label_all, "statusAll");
    set!(set_label_active, "statusActive");
    set!(set_label_paused, "statusPaused");
    set!(set_label_scheduled, "statusScheduled");
    set!(set_label_completed, "statusCompleted");
    set!(set_label_failed, "statusFailed");
    set!(set_label_pause_all, "pauseAll");
    set!(set_label_resume_all, "resumeAll");
    set!(set_label_clear_completed, "clearCompleted");
    set!(set_label_clear_failed, "clearFailed");
    set!(set_label_search, "searchDownloads");
    set!(set_label_add_placeholder, "addPlaceholder");
    set!(set_label_checksum, "checksumLabel");
    set!(set_label_add, "add");
    set!(set_label_pause, "pause");
    set!(set_label_resume, "resume");
    set!(set_label_retry, "retry");
    set!(set_label_open_file, "openFile");
    set!(set_label_folder, "openFolder");
    set!(set_label_details, "detailTitle");
    set!(set_label_remove, "remove");
    set!(set_label_remove_delete, "removeDelete");
    set!(set_label_confirm_delete, "confirmRemoveDelete");
    set!(set_label_copy_url, "copyUrl");
    set!(set_label_close, "close");
    set!(set_label_empty_title, "emptyTitle");
    set!(set_label_empty_sub, "emptySub");
    set!(set_label_no_match_title, "noMatchTitle");
    set!(set_label_no_match_sub, "noMatchSub");
    set!(set_label_onboard_paste, "obPaste");
    set!(set_label_onboard_video, "obVideo");
    set!(set_label_onboard_links, "obLinks");
    set!(set_label_onboard_browser, "obBrowser");
    set!(set_label_link_title, "grabLinksTitle");
    set!(set_label_link_placeholder, "pasteLinksPlaceholder");
    set!(set_label_extract_links, "extractLinks");
    set!(set_label_media_title, "grabVideoTitle");
    set!(set_label_media_placeholder, "videoUrlPlaceholder");
    set!(set_label_best_quality, "bestQuality");
    set!(set_label_general, "sectGeneral");
    set!(set_label_language, "sectLanguage");
    set!(set_label_extensions, "sectExtensions");
    set!(set_label_scheduler, "sectScheduler");
    set!(set_label_updates, "sectUpdates");
    set!(set_label_clipboard, "optClipboard");
    set!(set_label_auto_organize, "optAutoOrganize");
    set!(set_label_prevent_duplicates, "optPreventDuplicates");
    set!(set_label_close_tray, "optCloseTray");
    set!(set_label_autostart, "optAutostart");
    set!(set_label_handle_magnets, "optHandleMagnets");
    set!(set_label_dht, "optDht");
    set!(set_label_sandbox, "optSandbox");
    set!(set_label_block_private, "optBlockPrivate");
    set!(set_label_theme, "theme");
    set!(set_label_theme_system, "themeSystem");
    set!(set_label_theme_light, "themeLight");
    set!(set_label_theme_dark, "themeDark");
    set!(set_label_on_complete, "onComplete");
    set!(set_label_oc_none, "ocNone");
    set!(set_label_oc_quit, "ocQuit");
    set!(set_label_oc_sleep, "ocSleep");
    set!(set_label_oc_shutdown, "ocShutdown");
    set!(set_label_oc_custom, "ocCustom");
    set!(set_label_oc_placeholder, "ocCustomPlaceholder");
    set!(set_label_oc_warning, "ocCustomWarning");
    set!(set_label_oc_confirm, "ocCustomConfirm");
    set!(set_label_network, "sectNetwork");
    set!(set_label_proxy_title, "optProxy");
    set!(set_label_proxy, "proxyPlaceholder");
    set!(
        set_label_prevent_duplicates_hint,
        "optPreventDuplicatesHint"
    );
    set!(set_label_magnets_hint, "optHandleMagnetsHint");
    set!(set_label_restart_hint, "restartHint");
    set!(set_label_connections, "sectConnections");
    set!(set_label_max_concurrent, "maxConcurrent");
    set!(set_label_segments, "optSegments");
    set!(set_label_conn_server, "optConnPerServer");
    set!(set_label_default_speed, "defaultSpeedLimit");
    set!(set_label_connections_hint, "connHint");
    set!(set_label_extension_hint, "extensionStoreHint");
    set!(set_label_firefox_hint, "firefoxFamilyHint");
    set!(set_label_chromium_hint, "chromiumFamilyHint");
    set!(set_label_extension_note, "extensionStoreOpenNote");
    set!(set_label_browser_checking, "browserStateChecking");
    set!(set_label_browser_connected, "browserStateConnected");
    set!(set_label_browser_installed, "browserStateInstalled");
    set!(
        set_label_browser_needs_extension,
        "browserStateNeedsExtension"
    );
    set!(set_label_browser_not_found, "browserStateNotFound");
    set!(set_label_get_firefox, "getForFirefox");
    set!(set_label_get_chrome, "getForChrome");
    set!(set_label_sched_pause, "schedPauseAll");
    set!(set_label_sched_resume, "schedResumeAll");
    set!(set_label_sched_speed, "schedSetSpeed");
    set!(set_label_sched_add, "schedAddRule");
    set!(set_label_day_mon, "dayMon");
    set!(set_label_day_tue, "dayTue");
    set!(set_label_day_wed, "dayWed");
    set!(set_label_day_thu, "dayThu");
    set!(set_label_day_fri, "dayFri");
    set!(set_label_day_sat, "daySat");
    set!(set_label_day_sun, "daySun");
    set!(set_label_add_category, "addCategory");
    set!(set_label_cat_name, "catName");
    set!(set_label_cat_folder, "catFolder");
    set!(set_label_cat_rules, "catRules");
    set!(set_label_cat_mime, "catMimeRules");
    set!(set_label_cat_host, "catHostRules");
    set!(set_label_move_up, "moveUp");
    set!(set_label_move_down, "moveDown");
    set!(set_label_browse_folder, "browseFolder");
    set!(set_label_restore_defaults, "restoreDefaults");
    set!(set_label_check_updates, "updateCheck");
    set!(set_label_update_install, "updateInstall");
    set!(set_label_update_view, "updateView");
    set!(set_label_sponsor, "sponsor");
    set!(set_label_package_collapse, "pkgCollapse");
    set!(set_label_package_expand, "pkgExpand");
    set!(set_label_add_file, "tipAddFile");
    set!(set_label_shortcuts, "shortcutsTitle");
    set!(set_label_checksum_placeholder, "checksumPlaceholder");
    set!(set_label_onboard_hint, "emptyShortcutHint");
    set!(set_label_link_help, "grabLinksHelp");
    set!(set_label_package_name, "pkgName");
    set!(set_label_select_all, "plSelectAll");
    set!(set_label_select_none, "selectNone");
    set!(set_label_add_selected, "addSelectedPlain");
    set!(set_label_media_help, "grabVideoHelp");
    set!(set_label_probe, "probe");
    set!(set_label_playlist, "plPlaylist");
    set!(set_label_audio_only, "optAudioOnly");
    set!(set_label_subtitles, "optSubs");
    set!(set_label_thumbnail, "optEmbedThumb");
    set!(set_label_embed_subtitles, "optEmbedSubs");
    set!(set_label_add_playlist, "addSelectedPlain");
    set!(set_label_sc_focus_add, "scFocusAdd");
    set!(set_label_sc_search, "scSearch");
    set!(set_label_sc_filter, "scFilter");
    set!(set_label_sc_select_all, "scSelectAll");
    set!(set_label_sc_pause_resume, "scPauseResume");
    set!(set_label_sc_remove, "scRemove");
    set!(set_label_sc_open, "scOpen");
    set!(set_label_sc_close, "scClose");
    set!(set_label_hour, "hour");
    set!(set_label_minute, "minute");
    set!(set_label_detail_files, "detailFiles");
    set!(set_label_detail_peers, "detailPeers");
    set!(set_label_detail_pieces, "detailPieces");
    set!(set_label_detail_folder, "detailFolder");
    set!(set_label_apply_selection, "applySelection");
    set!(set_label_speed_limit, "speedLimit");
    set!(set_label_apply, "apply");
    set!(set_label_start_in_minutes, "startInMinutes");
    ui.set_label_schedule_action(
        catalog
            .get("scheduleAction")
            .trim_end_matches('\u{2026}')
            .into(),
    );
    set!(set_label_cancel_schedule, "scheduleCancel");
    set!(set_label_unlimited, "speedUnlimited");
    set!(set_label_global_speed, "globalSpeed");
    ui.set_rtl(catalog.is_rtl());
    let media_count = ui.get_media_choices().row_count();
    ui.set_media_entries_text(
        catalog
            .get("plEntries")
            .replace("{n}", &media_count.to_string())
            .into(),
    );
    if locale_code_from_name(&ui.get_locale_choice()) == "system" {
        ui.set_locale_choice(catalog.get("langSystem").into());
    }
    ui.set_page_title(catalog.get("titleAll").into());
}

pub(super) fn snapshot_rows(
    snapshot: &AppSnapshot,
    filter: i32,
    search: &str,
    catalog: &locale::Catalog,
    selection: &HashSet<i64>,
    collapsed_packages: &HashSet<i64>,
) -> Vec<DownloadRow> {
    rows_from_downloads(
        &snapshot.downloads,
        filter,
        search,
        catalog,
        selection,
        collapsed_packages,
    )
}

pub(super) fn update_download_model(ui: &MainWindow, rows: Vec<DownloadRow>) {
    let model = ui.get_downloads();
    let same_identity = model.row_count() == rows.len()
        && rows
            .iter()
            .enumerate()
            .all(|(index, row)| model.row_data(index).is_some_and(|old| old.id == row.id));
    if !same_identity {
        ui.set_downloads(ModelRc::new(VecModel::from(rows)));
        return;
    }
    for (index, row) in rows.into_iter().enumerate() {
        if model.row_data(index).as_ref() != Some(&row) {
            model.set_row_data(index, row);
        }
    }
}

pub(super) fn clamp_id(id: i64) -> i32 {
    id.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

pub(crate) fn rows_from_downloads(
    downloads: &[minidl_app::controller::DownloadView],
    filter: i32,
    search: &str,
    catalog: &locale::Catalog,
    selection: &HashSet<i64>,
    collapsed_packages: &HashSet<i64>,
) -> Vec<DownloadRow> {
    // Unicode-aware: file names are not ASCII in most of the ten UI languages.
    let search = search.to_lowercase();
    let filtered = downloads
        .iter()
        .filter(|row| match filter {
            1 => row.status.is_running(),
            2 => row.status == minidl_core::model::DownloadStatus::Paused,
            3 => row.status == minidl_core::model::DownloadStatus::Scheduled,
            4 => row.status == minidl_core::model::DownloadStatus::Complete,
            5 => row.status == minidl_core::model::DownloadStatus::Error,
            category if category >= 1000 => row.category_id == Some(i64::from(category - 1000)),
            _ => true,
        })
        .filter(|row| {
            search.is_empty()
                || row.url.to_lowercase().contains(&search)
                || row
                    .filename
                    .as_deref()
                    .unwrap_or("")
                    .to_lowercase()
                    .contains(&search)
        })
        .collect::<Vec<_>>();

    #[derive(Default)]
    struct PackageStats {
        count: usize,
        total: i64,
        completed: i64,
        speed: i64,
        running: bool,
        resumable: bool,
        all_complete: bool,
    }

    let mut package_stats = HashMap::<i64, PackageStats>::new();
    for row in &filtered {
        let Some(package_id) = row.package_id else {
            continue;
        };
        let stats = package_stats.entry(package_id).or_insert(PackageStats {
            all_complete: true,
            ..PackageStats::default()
        });
        stats.count += 1;
        stats.total = stats.total.saturating_add(row.total_bytes.max(0));
        stats.completed = stats.completed.saturating_add(row.completed_bytes.max(0));
        if row.status.is_running() {
            stats.speed = stats.speed.saturating_add(row.download_speed.max(0));
            stats.running = true;
        }
        stats.resumable |= matches!(
            row.status,
            minidl_core::model::DownloadStatus::Paused
                | minidl_core::model::DownloadStatus::Scheduled
        );
        stats.all_complete &= row.status == minidl_core::model::DownloadStatus::Complete;
    }

    let mut seen_packages = HashSet::new();
    filtered
        .into_iter()
        .filter_map(|row| {
            let package_first = row
                .package_id
                .is_some_and(|package_id| seen_packages.insert(package_id));
            let package_collapsed = row
                .package_id
                .is_some_and(|package_id| collapsed_packages.contains(&package_id));
            if row.package_id.is_some() && package_collapsed && !package_first {
                return None;
            }
            let progress = if row.total_bytes > 0 {
                row.completed_bytes as f32 / row.total_bytes as f32
            } else if row.status == minidl_core::model::DownloadStatus::Complete {
                1.0
            } else {
                0.0
            }
            .clamp(0.0, 1.0);
            let (
                package_count_text,
                package_progress,
                package_meta_text,
                package_running,
                package_resumable,
            ) = row
                .package_id
                .and_then(|package_id| package_stats.get(&package_id))
                .map(|stats| {
                    let package_progress = if stats.total > 0 {
                        stats.completed as f32 / stats.total as f32
                    } else if stats.all_complete {
                        1.0
                    } else {
                        0.0
                    }
                    .clamp(0.0, 1.0);
                    let mut meta = format!(
                        "{} · {} / {}",
                        format_percentage(package_progress),
                        format_bytes(stats.completed),
                        if stats.total > 0 {
                            format_bytes(stats.total)
                        } else {
                            "—".to_owned()
                        }
                    );
                    if stats.speed > 0 {
                        meta.push_str(" · ");
                        meta.push_str(&format_speed(stats.speed));
                    }
                    (
                        catalog
                            .get("pkgItems")
                            .replace("{n}", &stats.count.to_string()),
                        package_progress,
                        meta,
                        stats.running,
                        stats.resumable,
                    )
                })
                .unwrap_or_default();
            let progress_text = format_percentage(progress);
            let meta_text = format!(
                "{} / {} · {progress_text}",
                format_bytes(row.completed_bytes),
                if row.total_bytes > 0 {
                    format_bytes(row.total_bytes)
                } else {
                    "—".to_owned()
                },
            );
            let eta_text = estimated_time_remaining(
                row.status,
                row.completed_bytes,
                row.total_bytes,
                row.download_speed,
                catalog,
            )
            .unwrap_or_default();
            Some(DownloadRow {
                id: clamp_id(row.id),
                name: row.filename.as_deref().unwrap_or(&row.url).into(),
                url: row.url.as_str().into(),
                kind: row.kind.as_str().into(),
                status: row.status.as_str().into(),
                status_display: status_label(row.status, catalog).into(),
                progress,
                progress_text: progress_text.into(),
                speed_text: format_speed(row.download_speed).into(),
                eta_text: eta_text.into(),
                meta_text: meta_text.into(),
                running: row.status.is_running(),
                resumable: matches!(
                    row.status,
                    minidl_core::model::DownloadStatus::Paused
                        | minidl_core::model::DownloadStatus::Scheduled
                ),
                selected: selection.contains(&row.id),
                package_id: row.package_id.map(clamp_id).unwrap_or_default(),
                package_name: row.package_name.as_deref().unwrap_or_default().into(),
                package_first,
                package_collapsed,
                package_count_text: package_count_text.into(),
                package_progress,
                package_meta_text: package_meta_text.into(),
                package_running,
                package_resumable,
            })
        })
        .collect()
}

pub(crate) fn category_rules(extensions: &str, mimes: &str, hosts: &str) -> String {
    let values = |raw: &str| {
        raw.split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let extensions = values(extensions);
    let mimes = values(mimes);
    let hosts = values(hosts);
    if mimes.is_empty() && hosts.is_empty() {
        return serde_json::to_string(&extensions).unwrap_or_else(|_| "[]".into());
    }

    let mut rules = Vec::new();
    if !extensions.is_empty() {
        rules.push(serde_json::json!({"match": "ext", "values": extensions}));
    }
    if !mimes.is_empty() {
        rules.push(serde_json::json!({"match": "mime", "values": mimes}));
    }
    if !hosts.is_empty() {
        rules.push(serde_json::json!({"match": "host", "values": hosts}));
    }
    serde_json::Value::Array(rules).to_string()
}

pub(super) fn category_rows(
    categories: &[minidl_core::model::Category],
    downloads: &[minidl_app::controller::DownloadView],
) -> Vec<CategoryRow> {
    categories
        .iter()
        .map(|category| CategoryRow {
            id: category.id as i32,
            name: category.name.as_str().into(),
            dir: category.dir.as_str().into(),
            rules: category.rules.as_str().into(),
            priority: category.priority.clamp(i32::MIN as i64, i32::MAX as i64) as i32,
            count: downloads
                .iter()
                .filter(|download| download.category_id == Some(category.id))
                .count()
                .clamp(0, i32::MAX as usize) as i32,
        })
        .collect()
}

pub(super) fn apply_download_counts(
    ui: &MainWindow,
    downloads: &[minidl_app::controller::DownloadView],
) {
    use minidl_core::model::DownloadStatus;

    let count = |predicate: fn(&minidl_app::controller::DownloadView) -> bool| {
        downloads
            .iter()
            .filter(|download| predicate(download))
            .count()
            .clamp(0, i32::MAX as usize) as i32
    };
    ui.set_count_all(downloads.len().clamp(0, i32::MAX as usize) as i32);
    ui.set_count_active(count(|download| download.status.is_running()));
    ui.set_count_paused(count(|download| download.status == DownloadStatus::Paused));
    ui.set_count_scheduled(count(|download| {
        download.status == DownloadStatus::Scheduled
    }));
    ui.set_count_completed(count(|download| {
        download.status == DownloadStatus::Complete
    }));
    ui.set_count_failed(count(|download| download.status == DownloadStatus::Error));
}

pub(super) fn apply_selection_state(
    ui: &MainWindow,
    selection: &HashSet<i64>,
    catalog: &locale::Catalog,
) {
    let count = selection.len().min(i32::MAX as usize) as i32;
    ui.set_selected_count(count);
    ui.set_selected_text(
        catalog
            .get("bulkSelected")
            .replace("{n}", &count.to_string())
            .into(),
    );
}

pub(super) fn schedule_rows(
    schedules: &[minidl_core::model::Schedule],
    catalog: &locale::Catalog,
) -> Vec<ScheduleRow> {
    schedules
        .iter()
        .map(|schedule| {
            let hour = schedule.at_minute.div_euclid(60);
            let minute = schedule.at_minute.rem_euclid(60);
            let speed = schedule
                .speed_limit
                .map(format_speed)
                .filter(|value| !value.is_empty())
                .unwrap_or_default();
            let days = [
                "dayMon", "dayTue", "dayWed", "dayThu", "dayFri", "daySat", "daySun",
            ]
            .into_iter()
            .enumerate()
            .filter(|(index, _)| schedule.days_mask & (1_i64 << index) != 0)
            .map(|(_, key)| catalog.get(key))
            .collect::<Vec<_>>()
            .join(" ");
            ScheduleRow {
                id: schedule.id as i32,
                name: schedule.name.as_deref().unwrap_or_default().into(),
                summary: format!(
                    "{:02}:{:02} · {}{}",
                    hour,
                    minute,
                    days,
                    if speed.is_empty() {
                        String::new()
                    } else {
                        format!(" · {speed}")
                    }
                )
                .into(),
                action: schedule.action.as_str().into(),
                days: schedule.days_mask.clamp(0, 127) as i32,
                minute: schedule.at_minute.clamp(0, 1439) as i32,
                speed: schedule
                    .speed_limit
                    .unwrap_or_default()
                    .clamp(0, i32::MAX as i64) as i32,
                enabled: schedule.enabled,
            }
        })
        .collect()
}

pub(super) fn media_choices(
    info: &minidl_core::ytdlp::MediaInfo,
    source_url: &str,
) -> Vec<MediaChoice> {
    if info.kind == "playlist" {
        return info
            .entries
            .iter()
            .map(|entry| MediaChoice {
                url: entry.url.as_str().into(),
                format_id: "".into(),
                title: entry.title.as_str().into(),
                label: if entry.duration > 0 {
                    format!(
                        "{}  ·  {}:{:02}",
                        entry.title,
                        entry.duration / 60,
                        entry.duration % 60
                    )
                    .into()
                } else {
                    entry.title.as_str().into()
                },
                selected: true,
                playlist: true,
            })
            .collect();
    }
    let mut formats = info.formats.clone();
    formats.sort_by_key(|format| std::cmp::Reverse((format.height, format.filesize)));
    formats
        .into_iter()
        .filter(|format| !format.format_id.is_empty())
        .map(|format| {
            let codecs = match (format.vcodec.as_str(), format.acodec.as_str()) {
                ("none", audio) => format!("audio {audio}"),
                (video, "none") => format!("video {video}"),
                (video, audio) => format!("{video} + {audio}"),
            };
            MediaChoice {
                url: source_url.into(),
                format_id: format.format_id.into(),
                title: info.title.as_str().into(),
                label: format!(
                    "{} · {} · {} · {}",
                    format.resolution,
                    format.ext,
                    codecs,
                    format_bytes(format.filesize)
                )
                .into(),
                selected: false,
                playlist: false,
            }
        })
        .collect()
}

pub(super) fn media_options(ui: &MainWindow) -> MediaOptions {
    MediaOptions {
        write_subs: ui.get_media_subtitles(),
        sub_langs: ui.get_media_sub_langs().trim().to_owned(),
        embed_subs: ui.get_media_embed_subtitles(),
        audio_only: ui.get_media_audio_only(),
        audio_format: ui.get_media_audio_format().to_string(),
        embed_thumbnail: ui.get_media_thumbnail(),
    }
}

pub(super) fn detail_file_rows(files: &[minidl_app::controller::DetailFile]) -> Vec<DetailFileRow> {
    files
        .iter()
        .map(|file| DetailFileRow {
            index: file.index.clamp(0, i32::MAX as i64) as i32,
            name: file
                .path
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(&file.path)
                .into(),
            progress: if file.length > 0 {
                (file.completed_length as f32 / file.length as f32).clamp(0.0, 1.0)
            } else {
                0.0
            },
            meta: format!(
                "{} / {}",
                format_bytes(file.completed_length),
                format_bytes(file.length)
            )
            .into(),
            selected: file.selected,
        })
        .collect()
}

pub(super) fn locale_code_from_name(name: &str) -> &'static str {
    match name {
        "English" => "en",
        "Türkçe" => "tr",
        "Español" => "es",
        "Français" => "fr",
        "Deutsch" => "de",
        "Русский" => "ru",
        "العربية" => "ar",
        "中文" => "zh",
        "日本語" => "ja",
        "한국어" => "ko",
        _ => "system",
    }
}

pub(super) fn locale_name(code: &str) -> &'static str {
    match locale::normalize(code) {
        "en" if code != "system" && !code.is_empty() => "English",
        "tr" => "Türkçe",
        "es" => "Español",
        "fr" => "Français",
        "de" => "Deutsch",
        "ru" => "Русский",
        "ar" => "العربية",
        "zh" => "中文",
        "ja" => "日本語",
        "ko" => "한국어",
        _ => "System Default",
    }
}

pub(super) fn apply_connector_status(ui: &MainWindow, status: &serde_json::Value) {
    let flag = |key: &str| {
        status
            .get(key)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    };
    let state = |live: &str, installed: &str, profile: &str| {
        if flag(live) {
            1
        } else if flag(installed) {
            2
        } else if flag(profile) {
            3
        } else {
            4
        }
    };
    ui.set_firefox_state(state(
        "firefoxDetected",
        "firefoxConnectorInstalled",
        "firefoxProfileDetected",
    ));
    ui.set_chromium_state(state(
        "chromiumDetected",
        "chromiumConnectorInstalled",
        "chromiumProfileDetected",
    ));
}

pub(super) fn status_label(
    status: minidl_core::model::DownloadStatus,
    catalog: &locale::Catalog,
) -> &str {
    let key = match status {
        minidl_core::model::DownloadStatus::Queued => "statusQueued",
        minidl_core::model::DownloadStatus::Active => "statusActive",
        minidl_core::model::DownloadStatus::Waiting => "statusWaiting",
        minidl_core::model::DownloadStatus::Paused => "statusPaused",
        minidl_core::model::DownloadStatus::Complete => "statusCompleted",
        minidl_core::model::DownloadStatus::Error => "statusFailed",
        minidl_core::model::DownloadStatus::Scheduled => "statusScheduled",
        minidl_core::model::DownloadStatus::Removed => "remove",
    };
    catalog.get(key)
}

pub(crate) fn format_bytes(value: i64) -> String {
    let value = value.max(0) as f64;
    for (unit, divisor) in [
        ("GB", 1_000_000_000.0),
        ("MB", 1_000_000.0),
        ("KB", 1_000.0),
    ] {
        if value >= divisor {
            return format!("{:.2} {unit}", value / divisor);
        }
    }
    format!("{} B", value as i64)
}

pub(crate) fn format_percentage(progress: f32) -> String {
    format!("{:.1}%", progress.clamp(0.0, 1.0) * 100.0)
}

pub(crate) fn estimated_time_remaining(
    status: minidl_core::model::DownloadStatus,
    completed: i64,
    total: i64,
    speed: i64,
    catalog: &locale::Catalog,
) -> Option<String> {
    if status != minidl_core::model::DownloadStatus::Active || total <= 0 || speed <= 0 {
        return None;
    }
    let remaining = total.saturating_sub(completed.max(0));
    if remaining <= 0 {
        return None;
    }
    let seconds = remaining.saturating_add(speed - 1) / speed;
    Some(format_duration(seconds, catalog))
}

pub(crate) fn format_duration(seconds: i64, catalog: &locale::Catalog) -> String {
    if seconds < 60 {
        format!("{seconds}{}", catalog.get("durationSecondShort"))
    } else if seconds < 3_600 {
        format!(
            "{}{} {:02}{}",
            seconds / 60,
            catalog.get("durationMinuteShort"),
            seconds % 60,
            catalog.get("durationSecondShort")
        )
    } else if seconds < 86_400 {
        format!(
            "{}{} {:02}{}",
            seconds / 3_600,
            catalog.get("durationHourShort"),
            (seconds % 3_600) / 60,
            catalog.get("durationMinuteShort")
        )
    } else {
        format!(
            "{}{} {:02}{}",
            seconds / 86_400,
            catalog.get("durationDayShort"),
            (seconds % 86_400) / 3_600,
            catalog.get("durationHourShort")
        )
    }
}

pub(crate) fn format_speed(value: i64) -> String {
    if value <= 0 {
        String::new()
    } else {
        format!("{}/s", format_bytes(value))
    }
}
