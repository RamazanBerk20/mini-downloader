use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn install_callbacks(
    ui: &MainWindow,
    client: Arc<ControlClient>,
    runtime: tokio::runtime::Handle,
    all_downloads: Arc<Mutex<Vec<minidl_app::controller::DownloadView>>>,
    filter: Arc<AtomicI32>,
    catalog: SharedCatalog,
    selection: Arc<Mutex<HashSet<i64>>>,
    collapsed_packages: Arc<Mutex<HashSet<i64>>>,
) {
    let invoke_client = client.clone();
    let invoke_runtime = runtime.clone();
    let invoke = move |command: ControlCommand, weak: slint::Weak<MainWindow>| {
        let client = invoke_client.clone();
        invoke_runtime.spawn(async move {
            let result = client.request(command).await;
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = weak.upgrade() {
                    match result {
                        Ok(_) => ui.set_status_text("".into()),
                        Err(error) => ui.set_status_text(error.into()),
                    }
                }
            });
        });
    };
    let invoke = Rc::new(invoke);

    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        ui.on_add_url(move |url, checksum| {
            let raw = url.trim();
            let command = command_for_argument(raw).unwrap_or_else(|| ControlCommand::IngestUrl {
                url: raw.to_owned(),
                checksum: (!checksum.trim().is_empty()).then(|| checksum.to_string()),
            });
            invoke(command, weak.clone());
        });
    }
    {
        let client = client.clone();
        let runtime = runtime.clone();
        let weak = ui.as_weak();
        let catalog = catalog.clone();
        ui.on_pick_file(move || {
            let client = client.clone();
            let weak = weak.clone();
            let catalog = catalog.clone();
            runtime.spawn(async move {
                let (title, invalid_file) = {
                    let catalog = catalog.read().unwrap_or_else(|error| error.into_inner());
                    (
                        catalog.get("tipAddFile").to_owned(),
                        catalog.get("invalidDownloadFile").to_owned(),
                    )
                };
                let picked = rfd::AsyncFileDialog::new()
                    .set_title(title)
                    .add_filter("Torrent / Metalink", &["torrent", "meta4", "metalink"])
                    .pick_file()
                    .await;
                let Some(file) = picked else { return };
                let result = match command_for_path(file.path()) {
                    Some(command) => client.request(command).await.map(|_| ()),
                    None => Err(invalid_file),
                };
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        ui.set_status_text(result.err().unwrap_or_default().into());
                    }
                });
            });
        });
    }
    {
        let runtime = runtime.clone();
        let weak = ui.as_weak();
        ui.on_copy_url(move |url| {
            let weak = weak.clone();
            let text = url.to_string();
            runtime.spawn(async move {
                let result = tokio::task::spawn_blocking(move || {
                    arboard::Clipboard::new()
                        .map_err(|error| error.to_string())?
                        .set_text(text)
                        .map_err(|error| error.to_string())
                })
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result);
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        ui.set_status_text(result.err().unwrap_or_default().into());
                    }
                });
            });
        });
    }
    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        ui.on_set_global_speed(move |bytes| {
            invoke(
                ControlCommand::SetGlobalSpeed {
                    download: i64::from(bytes.max(0)),
                    upload: 0,
                },
                weak.clone(),
            );
        });
    }
    macro_rules! id_callback {
        ($setter:ident, $variant:ident) => {{
            let invoke = invoke.clone();
            let weak = ui.as_weak();
            ui.$setter(move |id| invoke(ControlCommand::$variant { id: id as i64 }, weak.clone()));
        }};
    }
    id_callback!(on_pause, Pause);
    id_callback!(on_resume, Resume);
    id_callback!(on_retry, Retry);
    id_callback!(on_open_file, OpenFile);
    id_callback!(on_open_folder, OpenContainingFolder);
    {
        let client = client.clone();
        let runtime = runtime.clone();
        let weak = ui.as_weak();
        ui.on_show_details(move |id| {
            let client = client.clone();
            let weak = weak.clone();
            runtime.spawn(async move {
                let result = client
                    .request(ControlCommand::GetDownloadDetails { id: id as i64 })
                    .await;
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        match result.and_then(|value| {
                            serde_json::from_value::<minidl_app::controller::DownloadDetails>(value)
                                .map_err(|error| error.to_string())
                        }) {
                            Ok(details) => {
                                ui.set_detail_id(details.id.clamp(0, i32::MAX as i64) as i32);
                                ui.set_detail_is_torrent(matches!(
                                    details.kind.as_str(),
                                    "torrent" | "magnet"
                                ));
                                ui.set_detail_url(details.url.clone().into());
                                ui.set_detail_dir(details.dir.clone().into());
                                ui.set_detail_error(
                                    details.error_message.clone().unwrap_or_default().into(),
                                );
                                ui.set_detail_peer_count(
                                    details.peers.len().min(i32::MAX as usize) as i32,
                                );
                                ui.set_detail_pieces_text(
                                    if details.num_pieces > 0 {
                                        format!(
                                            "{} × {}",
                                            details.num_pieces,
                                            format_bytes(details.piece_length)
                                        )
                                    } else {
                                        String::new()
                                    }
                                    .into(),
                                );
                                ui.set_detail_files(ModelRc::new(VecModel::from(
                                    detail_file_rows(&details.files),
                                )));
                                ui.set_delete_confirm_id(0);
                                ui.set_detail_visible(true);
                            }
                            Err(error) => ui.set_status_text(error.into()),
                        }
                    }
                });
            });
        });
    }
    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        ui.on_move_in_queue(move |id, direction| {
            invoke(
                ControlCommand::MoveInQueue {
                    id: id as i64,
                    direction: direction.to_string(),
                },
                weak.clone(),
            );
        });
    }
    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        ui.on_set_download_speed(move |id, kilobytes| {
            invoke(
                ControlCommand::SetDownloadSpeed {
                    id: id as i64,
                    limit: i64::from(kilobytes.max(0)) * 1024,
                },
                weak.clone(),
            );
        });
    }
    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        ui.on_schedule_download(move |id, minutes| {
            invoke(
                ControlCommand::ScheduleDownload {
                    id: id as i64,
                    start_at: Some(
                        minidl_core::model::now()
                            .saturating_add(i64::from(minutes.max(1)).saturating_mul(60)),
                    ),
                },
                weak.clone(),
            );
        });
    }
    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        ui.on_unschedule_download(move |id| {
            invoke(
                ControlCommand::ScheduleDownload {
                    id: id as i64,
                    start_at: None,
                },
                weak.clone(),
            );
        });
    }
    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        ui.on_remove(move |id| {
            invoke(
                ControlCommand::Remove {
                    id: id as i64,
                    delete_files: false,
                },
                weak.clone(),
            )
        });
    }
    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        ui.on_remove_delete(move |id| {
            invoke(
                ControlCommand::Remove {
                    id: i64::from(id),
                    delete_files: true,
                },
                weak.clone(),
            )
        });
    }
    {
        let selection = selection.clone();
        let all = all_downloads.clone();
        let filter = filter.clone();
        let catalog = catalog.clone();
        let collapsed_packages = collapsed_packages.clone();
        let weak = ui.as_weak();
        ui.on_toggle_selected(move |id, checked| {
            let Some(ui) = weak.upgrade() else { return };
            let id = i64::from(id);
            let mut selection = selection.lock().unwrap_or_else(|error| error.into_inner());
            if checked {
                selection.insert(id);
            } else {
                selection.remove(&id);
            }
            let downloads = all.lock().unwrap_or_else(|error| error.into_inner());
            let catalog = catalog.read().unwrap_or_else(|error| error.into_inner());
            let collapsed_packages = collapsed_packages
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            ui.set_downloads(ModelRc::new(VecModel::from(rows_from_downloads(
                &downloads,
                filter.load(Ordering::Relaxed),
                &ui.get_search_text(),
                &catalog,
                &selection,
                &collapsed_packages,
            ))));
            apply_selection_state(&ui, &selection, &catalog);
        });
    }
    {
        let selection = selection.clone();
        let all = all_downloads.clone();
        let filter = filter.clone();
        let catalog = catalog.clone();
        let collapsed_packages = collapsed_packages.clone();
        let weak = ui.as_weak();
        ui.on_select_visible(move |checked| {
            let Some(ui) = weak.upgrade() else { return };
            let downloads = all.lock().unwrap_or_else(|error| error.into_inner());
            let catalog = catalog.read().unwrap_or_else(|error| error.into_inner());
            let visible = rows_from_downloads(
                &downloads,
                filter.load(Ordering::Relaxed),
                &ui.get_search_text(),
                &catalog,
                &HashSet::new(),
                &HashSet::new(),
            )
            .into_iter()
            .map(|row| i64::from(row.id))
            .collect::<HashSet<_>>();
            let mut selection = selection.lock().unwrap_or_else(|error| error.into_inner());
            if checked {
                selection.extend(visible);
            } else {
                selection.retain(|id| !visible.contains(id));
            }
            let collapsed_packages = collapsed_packages
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            ui.set_downloads(ModelRc::new(VecModel::from(rows_from_downloads(
                &downloads,
                filter.load(Ordering::Relaxed),
                &ui.get_search_text(),
                &catalog,
                &selection,
                &collapsed_packages,
            ))));
            apply_selection_state(&ui, &selection, &catalog);
        });
    }
    {
        let collapsed_packages = collapsed_packages.clone();
        let all = all_downloads.clone();
        let filter = filter.clone();
        let catalog = catalog.clone();
        let selection = selection.clone();
        let weak = ui.as_weak();
        ui.on_toggle_package(move |package_id| {
            let Some(ui) = weak.upgrade() else { return };
            let package_id = i64::from(package_id);
            let mut collapsed = collapsed_packages
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if !collapsed.insert(package_id) {
                collapsed.remove(&package_id);
            }
            let downloads = all.lock().unwrap_or_else(|error| error.into_inner());
            let selection = selection.lock().unwrap_or_else(|error| error.into_inner());
            let catalog = catalog.read().unwrap_or_else(|error| error.into_inner());
            ui.set_downloads(ModelRc::new(VecModel::from(rows_from_downloads(
                &downloads,
                filter.load(Ordering::Relaxed),
                &ui.get_search_text(),
                &catalog,
                &selection,
                &collapsed,
            ))));
        });
    }
    {
        let client = client.clone();
        let runtime = runtime.clone();
        let all = all_downloads.clone();
        let filter = filter.clone();
        let catalog = catalog.clone();
        let selection = selection.clone();
        let collapsed_packages = collapsed_packages.clone();
        let weak = ui.as_weak();
        ui.on_package_action(move |package_id, action| {
            let package_id = i64::from(package_id);
            let ids = all
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .iter()
                .filter(|download| download.package_id == Some(package_id))
                .map(|download| download.id)
                .collect::<Vec<_>>();
            if ids.is_empty() {
                return;
            }
            let action = action.to_string();
            let client = client.clone();
            let weak = weak.clone();
            let all = all.clone();
            let filter = filter.clone();
            let catalog = catalog.clone();
            let selection = selection.clone();
            let collapsed_packages = collapsed_packages.clone();
            runtime.spawn(async move {
                let mut last_error = None;
                for id in ids {
                    let command = match action.as_str() {
                        "pause" => ControlCommand::Pause { id },
                        "resume" => ControlCommand::Resume { id },
                        "remove" => ControlCommand::Remove {
                            id,
                            delete_files: false,
                        },
                        _ => return,
                    };
                    if let Err(error) = client.request(command).await {
                        last_error = Some(error);
                    }
                }
                refresh_snapshot(
                    client,
                    weak.clone(),
                    all,
                    filter,
                    catalog,
                    selection,
                    collapsed_packages,
                )
                .await;
                if let Some(error) = last_error {
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = weak.upgrade() {
                            ui.set_status_text(error.into());
                        }
                    });
                }
            });
        });
    }
    {
        let client = client.clone();
        let runtime = runtime.clone();
        let selection = selection.clone();
        let all = all_downloads.clone();
        let filter = filter.clone();
        let catalog = catalog.clone();
        let collapsed_packages = collapsed_packages.clone();
        let weak = ui.as_weak();
        ui.on_bulk_action(move |action| {
            let action = action.to_string();
            if action == "clear" {
                selection
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .clear();
                if let Some(ui) = weak.upgrade() {
                    let downloads = all.lock().unwrap_or_else(|error| error.into_inner());
                    let selection = selection.lock().unwrap_or_else(|error| error.into_inner());
                    let catalog = catalog.read().unwrap_or_else(|error| error.into_inner());
                    let collapsed_packages = collapsed_packages
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    ui.set_downloads(ModelRc::new(VecModel::from(rows_from_downloads(
                        &downloads,
                        filter.load(Ordering::Relaxed),
                        &ui.get_search_text(),
                        &catalog,
                        &selection,
                        &collapsed_packages,
                    ))));
                    apply_selection_state(&ui, &selection, &catalog);
                }
                return;
            }
            let ids = selection
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .iter()
                .copied()
                .collect::<Vec<_>>();
            if ids.is_empty() {
                return;
            }
            let client = client.clone();
            let weak = weak.clone();
            let selection = selection.clone();
            let all = all.clone();
            let filter = filter.clone();
            let catalog = catalog.clone();
            let collapsed_packages = collapsed_packages.clone();
            runtime.spawn(async move {
                let mut failed = HashSet::new();
                let mut last_error = None;
                for id in ids {
                    let command = match action.as_str() {
                        "pause" => ControlCommand::Pause { id },
                        "resume" => ControlCommand::Resume { id },
                        "remove" => ControlCommand::Remove {
                            id,
                            delete_files: false,
                        },
                        _ => return,
                    };
                    if let Err(error) = client.request(command).await {
                        failed.insert(id);
                        last_error = Some(error);
                    }
                }
                *selection.lock().unwrap_or_else(|error| error.into_inner()) = failed;
                let snapshot = client
                    .request(ControlCommand::Snapshot)
                    .await
                    .ok()
                    .and_then(|value| serde_json::from_value::<AppSnapshot>(value).ok());
                if let Some(snapshot) = &snapshot {
                    *all.lock().unwrap_or_else(|error| error.into_inner()) =
                        snapshot.downloads.clone();
                }
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = weak.upgrade() else { return };
                    let selection = selection.lock().unwrap_or_else(|error| error.into_inner());
                    let catalog = catalog.read().unwrap_or_else(|error| error.into_inner());
                    let collapsed_packages = collapsed_packages
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    if let Some(snapshot) = snapshot {
                        ui.set_downloads(ModelRc::new(VecModel::from(snapshot_rows(
                            &snapshot,
                            filter.load(Ordering::Relaxed),
                            &ui.get_search_text(),
                            &catalog,
                            &selection,
                            &collapsed_packages,
                        ))));
                    }
                    apply_selection_state(&ui, &selection, &catalog);
                    ui.set_status_text(last_error.unwrap_or_default().into());
                });
            });
        });
    }
    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        ui.on_pause_all(move || invoke(ControlCommand::PauseAll, weak.clone()));
    }
    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        ui.on_resume_all(move || invoke(ControlCommand::ResumeAll, weak.clone()));
    }
    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        ui.on_remove_completed(move || invoke(ControlCommand::RemoveCompleted, weak.clone()));
    }
    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        ui.on_remove_failed(move || invoke(ControlCommand::RemoveFailed, weak.clone()));
    }
    {
        let filter_copy = filter.clone();
        let all = all_downloads.clone();
        let catalog = catalog.clone();
        let selection = selection.clone();
        let collapsed_packages = collapsed_packages.clone();
        let weak = ui.as_weak();
        ui.on_filter_changed(move |value| {
            filter_copy.store(value, Ordering::Relaxed);
            if let Some(ui) = weak.upgrade() {
                if let Ok(downloads) = all.lock() {
                    let catalog = catalog.read().unwrap_or_else(|error| error.into_inner());
                    let selection = selection.lock().unwrap_or_else(|error| error.into_inner());
                    let collapsed_packages = collapsed_packages
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    ui.set_downloads(ModelRc::new(VecModel::from(rows_from_downloads(
                        &downloads,
                        value,
                        &ui.get_search_text(),
                        &catalog,
                        &selection,
                        &collapsed_packages,
                    ))));
                    apply_selection_state(&ui, &selection, &catalog);
                }
            }
        });
    }
    {
        let client = client.clone();
        let runtime = runtime.clone();
        let weak = ui.as_weak();
        let all = all_downloads.clone();
        let filter = filter.clone();
        let catalog = catalog.clone();
        let selection = selection.clone();
        let collapsed_packages = collapsed_packages.clone();
        ui.on_refresh(move || {
            let client = client.clone();
            let weak = weak.clone();
            let all = all.clone();
            let filter = filter.clone();
            let catalog = catalog.clone();
            let selection = selection.clone();
            let collapsed_packages = collapsed_packages.clone();
            runtime.spawn(async move {
                refresh_snapshot(
                    client,
                    weak,
                    all,
                    filter,
                    catalog,
                    selection,
                    collapsed_packages,
                )
                .await;
            });
        });
    }
    {
        let client = client.clone();
        let runtime = runtime.clone();
        let weak = ui.as_weak();
        ui.on_grab_links(move |text| {
            let client = client.clone();
            let weak = weak.clone();
            runtime.spawn(async move {
                let result = client
                    .request(ControlCommand::GrabLinks {
                        text: text.to_string(),
                    })
                    .await
                    .and_then(|value| {
                        serde_json::from_value::<Vec<minidl_core::grabber::ParsedLink>>(value)
                            .map_err(|error| error.to_string())
                    });
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        match result {
                            Ok(links) => {
                                if ui.get_link_package().trim().is_empty() {
                                    ui.set_link_package(
                                        links
                                            .first()
                                            .map(|link| link.host.as_str())
                                            .unwrap_or("")
                                            .into(),
                                    );
                                }
                                ui.set_grabbed_links(ModelRc::new(VecModel::from(
                                    links
                                        .into_iter()
                                        .map(|link| LinkChoice {
                                            url: link.url.into(),
                                            kind: link.kind.into(),
                                            host: link.host.into(),
                                            selected: true,
                                        })
                                        .collect::<Vec<_>>(),
                                )));
                                ui.set_status_text("".into());
                            }
                            Err(error) => ui.set_status_text(error.into()),
                        }
                    }
                });
            });
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_toggle_grabbed(move |index, selected| {
            let Some(ui) = weak.upgrade() else { return };
            let model = ui.get_grabbed_links();
            if let Some(mut row) = usize::try_from(index)
                .ok()
                .and_then(|index| model.row_data(index))
            {
                row.selected = selected;
                model.set_row_data(index as usize, row);
            }
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_select_grabbed(move |selected| {
            let Some(ui) = weak.upgrade() else { return };
            let model = ui.get_grabbed_links();
            for index in 0..model.row_count() {
                if let Some(mut row) = model.row_data(index) {
                    row.selected = selected;
                    model.set_row_data(index, row);
                }
            }
        });
    }
    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        let catalog = catalog.clone();
        ui.on_add_grabbed(move || {
            let Some(ui) = weak.upgrade() else { return };
            let model = ui.get_grabbed_links();
            let urls: Vec<_> = (0..model.row_count())
                .filter_map(|index| model.row_data(index))
                .filter(|row| row.selected)
                .map(|row| row.url.to_string())
                .collect();
            if urls.is_empty() {
                let catalog = catalog.read().unwrap_or_else(|error| error.into_inner());
                ui.set_status_text(catalog.get("selectOneLink").into());
                return;
            }
            invoke(
                ControlCommand::AddLinks {
                    urls,
                    package_name: (!ui.get_link_package().trim().is_empty())
                        .then(|| ui.get_link_package().to_string()),
                },
                weak.clone(),
            );
        });
    }
    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        ui.on_add_media(move |url| {
            let options = weak
                .upgrade()
                .map(|ui| media_options(&ui))
                .unwrap_or_default();
            invoke(
                ControlCommand::AddMedia {
                    url: url.to_string(),
                    format_id: None,
                    options: Some(options),
                },
                weak.clone(),
            );
        });
    }
    {
        let client = client.clone();
        let runtime = runtime.clone();
        let weak = ui.as_weak();
        let catalog = catalog.clone();
        ui.on_probe_media(move |url, playlist| {
            let client = client.clone();
            let weak = weak.clone();
            let catalog = catalog.clone();
            runtime.spawn(async move {
                let result = client
                    .request(ControlCommand::ProbeMedia {
                        url: url.to_string(),
                        playlist,
                    })
                    .await;
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        match result.and_then(|value| {
                            serde_json::from_value::<minidl_core::ytdlp::MediaInfo>(value)
                                .map_err(|error| error.to_string())
                        }) {
                            Ok(info) => {
                                let choice_rows = media_choices(&info, &url);
                                let catalog =
                                    catalog.read().unwrap_or_else(|error| error.into_inner());
                                ui.set_media_title(info.title.as_str().into());
                                ui.set_media_is_playlist(info.kind == "playlist");
                                ui.set_media_entries_text(
                                    catalog
                                        .get("plEntries")
                                        .replace("{n}", &choice_rows.len().to_string())
                                        .into(),
                                );
                                ui.set_media_choices(ModelRc::new(VecModel::from(choice_rows)));
                                ui.set_status_text("".into());
                            }
                            Err(error) => ui.set_status_text(error.into()),
                        }
                    }
                });
            });
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_toggle_media_choice(move |index, selected| {
            let Some(ui) = weak.upgrade() else { return };
            let model = ui.get_media_choices();
            if let Some(mut row) = usize::try_from(index)
                .ok()
                .and_then(|index| model.row_data(index))
            {
                row.selected = selected;
                model.set_row_data(index as usize, row);
            }
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_select_media_choices(move |selected| {
            let Some(ui) = weak.upgrade() else { return };
            let model = ui.get_media_choices();
            for index in 0..model.row_count() {
                if let Some(mut row) = model.row_data(index) {
                    row.selected = selected;
                    model.set_row_data(index, row);
                }
            }
        });
    }
    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        let catalog = catalog.clone();
        ui.on_add_playlist(move || {
            let Some(ui) = weak.upgrade() else { return };
            let model = ui.get_media_choices();
            let entries: Vec<_> = (0..model.row_count())
                .filter_map(|index| model.row_data(index))
                .filter(|row| row.selected)
                .map(|row| PlaylistSelection {
                    url: row.url.to_string(),
                    title: Some(row.title.to_string()),
                })
                .collect();
            if entries.is_empty() {
                let catalog = catalog.read().unwrap_or_else(|error| error.into_inner());
                ui.set_status_text(catalog.get("selectOnePlaylist").into());
                return;
            }
            let quality = match ui.get_media_quality().as_str() {
                "1080p" => Some("bv*[height<=1080]+ba/b".into()),
                "720p" => Some("bv*[height<=720]+ba/b".into()),
                _ => None,
            };
            invoke(
                ControlCommand::AddPlaylist {
                    entries,
                    package_name: ui.get_media_title().to_string(),
                    quality,
                    options: Some(media_options(&ui)),
                },
                weak.clone(),
            );
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_toggle_detail_file(move |index, selected| {
            let Some(ui) = weak.upgrade() else { return };
            let model = ui.get_detail_files();
            if let Some(mut row) = usize::try_from(index)
                .ok()
                .and_then(|index| model.row_data(index))
            {
                row.selected = selected;
                model.set_row_data(index as usize, row);
            }
        });
    }
    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        ui.on_apply_torrent_files(move |id| {
            let Some(ui) = weak.upgrade() else { return };
            let model = ui.get_detail_files();
            let indices = (0..model.row_count())
                .filter_map(|index| model.row_data(index))
                .filter(|row| row.selected)
                .filter_map(|row| u32::try_from(row.index).ok())
                .collect();
            invoke(
                ControlCommand::SetTorrentFiles {
                    id: id as i64,
                    indices,
                },
                weak.clone(),
            );
        });
    }
    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        ui.on_add_media_choice(move |url, format_id| {
            let options = weak
                .upgrade()
                .map(|ui| media_options(&ui))
                .unwrap_or_default();
            invoke(
                ControlCommand::AddMedia {
                    url: url.to_string(),
                    format_id: (!format_id.is_empty()).then(|| format_id.to_string()),
                    options: Some(options),
                },
                weak.clone(),
            );
        });
    }
    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        ui.on_save_category(move |name, dir, rules, priority| {
            invoke(
                ControlCommand::SaveCategory {
                    name: name.to_string(),
                    dir: dir.to_string(),
                    rules: rules.to_string(),
                    priority: priority as i64,
                },
                weak.clone(),
            )
        });
    }
    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        ui.on_move_category(move |id, direction| {
            invoke(
                ControlCommand::MoveCategory {
                    id: i64::from(id),
                    direction,
                },
                weak.clone(),
            )
        });
    }
    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        let catalog = catalog.clone();
        ui.on_save_new_category(move |name, dir, extensions, mimes, hosts| {
            if name.trim().is_empty() || dir.trim().is_empty() {
                if let Some(ui) = weak.upgrade() {
                    let catalog = catalog.read().unwrap_or_else(|error| error.into_inner());
                    ui.set_status_text(catalog.get("categoryRequired").into());
                }
                return;
            }
            let priority = weak
                .upgrade()
                .map(|ui| i64::try_from(ui.get_categories().row_count()).unwrap_or(i64::MAX))
                .unwrap_or_default();
            invoke(
                ControlCommand::SaveCategory {
                    name: name.trim().to_owned(),
                    dir: dir.trim().to_owned(),
                    rules: category_rules(&extensions, &mimes, &hosts),
                    priority,
                },
                weak.clone(),
            );
        });
    }
    {
        let client = client.clone();
        let runtime = runtime.clone();
        let weak = ui.as_weak();
        let catalog = catalog.clone();
        ui.on_browse_category(move |name, rules, priority| {
            let client = client.clone();
            let weak = weak.clone();
            let catalog = catalog.clone();
            runtime.spawn(async move {
                let title = catalog
                    .read()
                    .unwrap_or_else(|error| error.into_inner())
                    .get("browseFolder")
                    .to_owned();
                let picked = rfd::AsyncFileDialog::new()
                    .set_title(title)
                    .pick_folder()
                    .await;
                let Some(folder) = picked else { return };
                let result = client
                    .request(ControlCommand::SaveCategory {
                        name: name.to_string(),
                        dir: folder.path().to_string_lossy().into_owned(),
                        rules: rules.to_string(),
                        priority: i64::from(priority),
                    })
                    .await;
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        ui.set_status_text(result.err().unwrap_or_default().into());
                    }
                });
            });
        });
    }
    {
        let runtime = runtime.clone();
        let weak = ui.as_weak();
        let catalog = catalog.clone();
        ui.on_browse_new_category_dir(move || {
            let weak = weak.clone();
            let catalog = catalog.clone();
            runtime.spawn(async move {
                let title = catalog
                    .read()
                    .unwrap_or_else(|error| error.into_inner())
                    .get("browseFolder")
                    .to_owned();
                let picked = rfd::AsyncFileDialog::new()
                    .set_title(title)
                    .pick_folder()
                    .await;
                let Some(folder) = picked else { return };
                let path = folder.path().to_string_lossy().into_owned();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        ui.set_category_dir(path.into());
                    }
                });
            });
        });
    }
    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        ui.on_delete_category(move |id| {
            invoke(
                ControlCommand::DeleteCategory { id: id as i64 },
                weak.clone(),
            )
        });
    }
    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        ui.on_reset_category(move |id| {
            invoke(
                ControlCommand::ResetCategoryDir { id: id as i64 },
                weak.clone(),
            )
        });
    }
    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        ui.on_restore_categories(move || {
            invoke(ControlCommand::RestoreDefaultCategories, weak.clone())
        });
    }
    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        ui.on_save_schedule(move |id, name, action, days, minute, speed, enabled| {
            invoke(
                ControlCommand::SaveSchedule {
                    id: (id > 0).then_some(id as i64),
                    name: (!name.trim().is_empty()).then(|| name.to_string()),
                    action: action.to_string(),
                    days_mask: days as i64,
                    at_minute: minute as i64,
                    speed_limit: (speed > 0).then_some(speed as i64),
                    enabled,
                },
                weak.clone(),
            )
        });
    }
    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        ui.on_delete_schedule(move |id| {
            invoke(
                ControlCommand::DeleteSchedule { id: id as i64 },
                weak.clone(),
            )
        });
    }
    {
        let client = client.clone();
        let runtime = runtime.clone();
        let weak = ui.as_weak();
        let saved = Arc::new(Mutex::new(SettingsDraft::from_ui(ui)));
        let catalog = catalog.clone();
        let all = all_downloads.clone();
        let filter = filter.clone();
        let selection = selection.clone();
        let collapsed_packages = collapsed_packages.clone();
        ui.on_save_settings(
            move |proxy,
                  maximum,
                  split,
                  connections,
                  clipboard,
                  theme,
                  auto_organize,
                  prevent_duplicates,
                  close_to_tray,
                  autostart,
                  handle_magnets,
                  dht,
                  sandbox,
                  block_private,
                  locale_choice,
                  complete_action,
                  complete_command,
                  complete_confirmed,
                  default_speed_mb| {
                let next = SettingsDraft {
                    proxy: proxy.to_string(),
                    maximum,
                    split,
                    connections,
                    clipboard,
                    theme: theme.to_string(),
                    auto_organize,
                    prevent_duplicates,
                    close_to_tray,
                    autostart,
                    handle_magnets,
                    dht,
                    sandbox,
                    block_private,
                    locale_choice: locale_choice.to_string(),
                    complete_action: complete_action.to_string(),
                    complete_command: complete_command.to_string(),
                    complete_confirmed,
                    default_speed_mb,
                };
                let previous = saved
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .clone();
                if next == previous {
                    return;
                }
                let locale_code = locale_code_from_name(&next.locale_choice).to_owned();
                let command = ControlCommand::ApplySettings {
                    settings: SettingsUpdate {
                        proxy: next.proxy.clone(),
                        max_concurrent: next.maximum.max(1) as u32,
                        split: next.split.max(1) as u32,
                        connections: next.connections.max(1) as u32,
                        clipboard: next.clipboard,
                        theme: match next.theme.as_str() {
                            "light" | "dark" => next.theme.clone(),
                            _ => "system".into(),
                        },
                        auto_organize: next.auto_organize,
                        prevent_duplicates: next.prevent_duplicates,
                        close_to_tray: next.close_to_tray,
                        autostart: next.autostart,
                        handle_magnets: next.handle_magnets,
                        dht: next.dht,
                        sandbox: next.sandbox,
                        block_private: next.block_private,
                        locale: locale_code.clone(),
                        on_complete_action: if next.complete_action == "run:" {
                            format!("run:{}", next.complete_command.trim())
                        } else {
                            next.complete_action.clone()
                        },
                        on_complete_confirmed: next.complete_confirmed,
                        default_speed_limit: default_speed_mb_to_bytes(next.default_speed_mb),
                    },
                };
                let client = client.clone();
                let weak = weak.clone();
                let saved = saved.clone();
                let catalog = catalog.clone();
                let all = all.clone();
                let filter = filter.clone();
                let selection = selection.clone();
                let collapsed_packages = collapsed_packages.clone();
                let previous_for_error = previous.clone();
                runtime.spawn(async move {
                    let result = client.request(command).await;
                    let snapshot = if result.is_ok() {
                        client
                            .request(ControlCommand::Snapshot)
                            .await
                            .ok()
                            .and_then(|value| serde_json::from_value::<AppSnapshot>(value).ok())
                    } else {
                        None
                    };
                    if result.is_ok() {
                        *saved.lock().unwrap_or_else(|error| error.into_inner()) = next;
                        *catalog.write().unwrap_or_else(|error| error.into_inner()) =
                            locale::Catalog::new(locale::selected(Some(&locale_code)));
                        if let Some(snapshot) = &snapshot {
                            *all.lock().unwrap_or_else(|error| error.into_inner()) =
                                snapshot.downloads.clone();
                        }
                    }
                    let _ = slint::invoke_from_event_loop(move || {
                        let Some(ui) = weak.upgrade() else { return };
                        match result {
                            Ok(_) => {
                                let catalog =
                                    catalog.read().unwrap_or_else(|error| error.into_inner());
                                apply_i18n(&ui, &catalog);
                                if let Some(snapshot) = snapshot {
                                    let selection =
                                        selection.lock().unwrap_or_else(|error| error.into_inner());
                                    let collapsed_packages = collapsed_packages
                                        .lock()
                                        .unwrap_or_else(|error| error.into_inner());
                                    ui.set_downloads(ModelRc::new(VecModel::from(snapshot_rows(
                                        &snapshot,
                                        filter.load(Ordering::Relaxed),
                                        &ui.get_search_text(),
                                        &catalog,
                                        &selection,
                                        &collapsed_packages,
                                    ))));
                                    ui.set_schedules(ModelRc::new(VecModel::from(schedule_rows(
                                        &snapshot.schedules,
                                        &catalog,
                                    ))));
                                    apply_selection_state(&ui, &selection, &catalog);
                                }
                                ui.set_status_text("".into());
                            }
                            Err(error) => {
                                previous_for_error.apply_to_ui(&ui);
                                ui.set_status_text(error.into());
                            }
                        }
                    });
                });
            },
        );
    }
    {
        let client = client.clone();
        let runtime = runtime.clone();
        let weak = ui.as_weak();
        let catalog = catalog.clone();
        ui.on_check_update(move || {
            let client = client.clone();
            let weak = weak.clone();
            let catalog = catalog.clone();
            runtime.spawn(async move {
                let result = client.request(ControlCommand::CheckUpdate).await;
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        match result.and_then(|value| {
                            serde_json::from_value::<minidl_app::updater::UpdateInfo>(value)
                                .map_err(|error| error.to_string())
                        }) {
                            Ok(update) => {
                                let catalog =
                                    catalog.read().unwrap_or_else(|error| error.into_inner());
                                ui.set_update_text(if update.newer {
                                    catalog
                                        .get("updateAvailable")
                                        .replace("{v}", &update.latest)
                                        .into()
                                } else {
                                    catalog
                                        .get("updateUpToDate")
                                        .replace("{v}", &update.current)
                                        .into()
                                });
                                ui.set_update_page(if update.newer {
                                    update.url.into()
                                } else {
                                    "".into()
                                });
                                ui.set_update_asset(update.asset_url.unwrap_or_default().into());
                            }
                            Err(error) => ui.set_update_text(error.into()),
                        }
                    }
                });
            });
        });
    }
    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        ui.on_install_update(move |asset, page| {
            invoke(
                ControlCommand::InstallUpdate {
                    asset_url: (!asset.is_empty()).then(|| asset.to_string()),
                    page_url: page.to_string(),
                },
                weak.clone(),
            )
        });
    }
    {
        let runtime = runtime.clone();
        let weak = ui.as_weak();
        ui.on_open_external(move |url| {
            let weak = weak.clone();
            let url = url.to_string();
            runtime.spawn(async move {
                let result = tokio::task::spawn_blocking(move || {
                    open::that(url).map_err(|error| error.to_string())
                })
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result);
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        ui.set_status_text(result.err().unwrap_or_default().into());
                    }
                });
            });
        });
    }
}
