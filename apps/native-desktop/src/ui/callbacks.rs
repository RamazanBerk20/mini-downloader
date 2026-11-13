use super::*;

thread_local! {
    // One clipboard owner for the UI's lifetime: on X11 the copied data is
    // served by this object, and a dropped one leaves nothing to paste on a
    // window manager without a clipboard manager.
    static CLIPBOARD: std::cell::RefCell<Option<arboard::Clipboard>> =
        const { std::cell::RefCell::new(None) };
}

fn with_clipboard(
    action: impl FnOnce(&mut arboard::Clipboard) -> Result<(), arboard::Error>,
) -> Result<(), String> {
    CLIPBOARD.with(|cell| {
        let mut cell = cell.borrow_mut();
        if cell.is_none() {
            *cell = Some(arboard::Clipboard::new().map_err(|error| error.to_string())?);
        }
        let clipboard = cell.as_mut().expect("initialised above");
        action(clipboard).map_err(|error| error.to_string())
    })
}

/// Ctrl+C on the list: the selected finished files go on the clipboard as
/// files (a file manager or chat app pastes them). Returns whether any did.
pub(super) fn copy_selected_files(ctx: &Arc<Ctx>, ui: &MainWindow) -> bool {
    let paths = selected_paths(ctx);
    if paths.is_empty() {
        return false;
    }
    // Native Wayland: our own data device (arboard only reaches XWayland).
    let result = if crate::dragout::copy(&paths) {
        Ok(())
    } else {
        with_clipboard(|clipboard| clipboard.set().file_list(&paths))
    };
    match result {
        Ok(()) => {
            let text = ctx
                .catalog()
                .get("filesCopied")
                .replace("{n}", &paths.len().to_string());
            ctx.notice(ui, Origin::Page, false, &text);
        }
        Err(error) => ctx.notice(ui, Origin::Page, true, &error),
    }
    true
}

/// Finished files of the selected rows, in list order. The controller sets
/// `path` only for a completed row whose file or folder exists.
pub(crate) fn selected_paths(ctx: &Ctx) -> Vec<std::path::PathBuf> {
    let selection = lock(&ctx.selection);
    lock(&ctx.downloads)
        .iter()
        .filter(|row| selection.contains(&row.id))
        .filter_map(|row| row.path.as_deref().map(std::path::PathBuf::from))
        .collect()
}

/// The drag image: the hidden ghost (file tile, name, count) shown for one
/// snapshot of the window and cut out of it.
fn drag_icon(ui: &MainWindow, id: i32, count: usize) -> Option<crate::dragout::Icon> {
    let row = ui.get_downloads().iter().find(|row| row.id == id)?;
    ui.set_drag_ghost_name(row.name.clone());
    ui.set_drag_ghost_type(row.file_type.clone());
    ui.set_drag_ghost_type_label(row.type_label.clone());
    ui.set_drag_ghost_count(if count > 1 {
        count.to_string().into()
    } else {
        row.file_count.clone()
    });
    ui.set_drag_ghost_visible(true);
    let snapshot = ui.window().take_snapshot();
    ui.set_drag_ghost_visible(false);
    let snapshot = snapshot.ok()?;
    let scale = ui.window().scale_factor();
    let physical = |length: f32, limit: u32| ((length * scale).round().max(1.0) as u32).min(limit);
    let width = physical(ui.get_drag_ghost_width(), snapshot.width());
    let height = physical(ui.get_drag_ghost_height(), snapshot.height());
    let rgba = snapshot
        .as_slice()
        .chunks(snapshot.width() as usize)
        .take(height as usize)
        .flat_map(|line| {
            line[..width as usize]
                .iter()
                .flat_map(|pixel| [pixel.r, pixel.g, pixel.b, pixel.a])
        })
        .collect();
    Some(crate::dragout::Icon {
        width,
        height,
        scale,
        rgba,
    })
}

pub(super) fn add_dropped_file(ctx: &Arc<Ctx>, path: &std::path::Path) {
    match command_for_path(path) {
        Some(command) => ctx.command(Origin::Page, command),
        None => {
            if let Some(ui) = ctx.weak.upgrade() {
                let text = ctx.catalog().get("invalidDownloadFile").to_owned();
                ctx.notice(&ui, Origin::Page, true, &text);
            }
        }
    }
}

/// Run `command` and report on the UI thread with `done`.
fn request(
    ctx: &Arc<Ctx>,
    command: ControlCommand,
    done: impl FnOnce(&Arc<Ctx>, &MainWindow, Result<serde_json::Value, String>) + Send + 'static,
) {
    let ctx = ctx.clone();
    ctx.runtime.clone().spawn(async move {
        let result = ctx.client.request(command).await;
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(ui) = ctx.weak.upgrade() {
                done(&ctx, &ui, result);
            }
        });
    });
}

pub(super) fn install_callbacks(ui: &MainWindow, ctx: &Arc<Ctx>) {
    {
        let ctx = ctx.clone();
        ui.on_add_url(move |url, checksum| {
            let command = command_for_add_field(&url, &checksum);
            let known = lock(&ctx.downloads)
                .iter()
                .map(|row| row.id)
                .collect::<HashSet<_>>();
            request(&ctx, command, move |ctx, ui, result| match result {
                Ok(value) => {
                    // Clear only what was submitted, not a link typed while
                    // the request was in flight.
                    if ui.get_url_text() == url {
                        ui.set_url_text("".into());
                        ui.set_checksum_text("".into());
                    }
                    ctx.report(ui, Origin::Page, Ok(()));
                    // "Prevent duplicates" handed back a row that was there.
                    let existing = value
                        .get("id")
                        .and_then(serde_json::Value::as_i64)
                        .is_some_and(|id| known.contains(&id));
                    if existing {
                        let text = ctx.catalog().get("alreadyInList").to_owned();
                        ctx.notice(ui, Origin::Page, false, &text);
                    }
                }
                Err(error) => ctx.report(ui, Origin::Page, Err(error)),
            });
        });
    }
    {
        let ctx = ctx.clone();
        ui.on_pick_file(move || {
            let ctx = ctx.clone();
            ctx.runtime.clone().spawn(async move {
                let (title, invalid_file) = {
                    let catalog = ctx.catalog();
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
                    Some(command) => ctx.client.request(command).await.map(drop),
                    None => Err(invalid_file),
                };
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ctx.weak.upgrade() {
                        ctx.report(&ui, Origin::Page, result);
                    }
                });
            });
        });
    }
    {
        let ctx = ctx.clone();
        ui.on_copy_url(move |url| {
            let Some(ui) = ctx.weak.upgrade() else { return };
            let text = url.to_string();
            let result = with_clipboard(|clipboard| clipboard.set_text(text));
            ctx.report(&ui, Origin::Page, result);
        });
    }
    {
        let ctx = ctx.clone();
        ui.on_set_global_speed(move |bytes| {
            ctx.command(
                Origin::Page,
                ControlCommand::SetGlobalSpeed {
                    download: i64::from(bytes.max(0)),
                    upload: 0,
                },
            );
        });
    }
    macro_rules! id_callback {
        ($setter:ident, $variant:ident) => {{
            let ctx = ctx.clone();
            ui.$setter(move |id| {
                ctx.command(Origin::Page, ControlCommand::$variant { id: i64::from(id) })
            });
        }};
    }
    id_callback!(on_pause, Pause);
    id_callback!(on_resume, Resume);
    id_callback!(on_retry, Retry);
    id_callback!(on_open_file, OpenFile);
    id_callback!(on_open_folder, OpenContainingFolder);
    {
        let ctx = ctx.clone();
        ui.on_show_details(move |id| ctx.load_details(i64::from(id), true));
    }
    macro_rules! detail_command {
        ($setter:ident, |$($arg:ident),*| $command:expr) => {{
            let ctx = ctx.clone();
            ui.$setter(move |$($arg),*| ctx.command(Origin::Details, $command));
        }};
    }
    detail_command!(on_move_in_queue, |id, direction| {
        ControlCommand::MoveInQueue {
            id: i64::from(id),
            direction: direction.to_string(),
        }
    });
    // Decimal kilobytes, like every other figure the app shows.
    detail_command!(on_set_download_speed, |id, kilobytes| {
        ControlCommand::SetDownloadSpeed {
            id: i64::from(id),
            limit: i64::from(kilobytes.max(0)) * 1_000,
        }
    });
    detail_command!(on_schedule_download, |id, minutes| {
        ControlCommand::ScheduleDownload {
            id: i64::from(id),
            start_at: Some(
                minidl_core::model::now()
                    .saturating_add(i64::from(minutes.max(1)).saturating_mul(60)),
            ),
        }
    });
    detail_command!(on_unschedule_download, |id| {
        ControlCommand::ScheduleDownload {
            id: i64::from(id),
            start_at: None,
        }
    });
    {
        let ctx = ctx.clone();
        ui.on_remove(move |id| {
            ctx.command(
                Origin::Page,
                ControlCommand::Remove {
                    id: i64::from(id),
                    delete_files: false,
                },
            )
        });
    }
    {
        // The dialog closes with the click; its outcome shows on the page.
        let ctx = ctx.clone();
        ui.on_remove_delete(move |id| {
            ctx.command(
                Origin::Page,
                ControlCommand::Remove {
                    id: i64::from(id),
                    delete_files: true,
                },
            )
        });
    }
    {
        let ctx = ctx.clone();
        ui.on_toggle_selected(move |id, checked| {
            let Some(ui) = ctx.weak.upgrade() else { return };
            {
                let mut selection = lock(&ctx.selection);
                if checked {
                    selection.insert(i64::from(id));
                } else {
                    selection.remove(&i64::from(id));
                }
            }
            ctx.render(&ui);
        });
    }
    {
        let ctx = ctx.clone();
        ui.on_row_clicked(move |id, toggle| {
            let Some(ui) = ctx.weak.upgrade() else { return };
            let id = i64::from(id);
            {
                let mut selection = lock(&ctx.selection);
                if toggle {
                    if !selection.remove(&id) {
                        selection.insert(id);
                    }
                } else {
                    selection.clear();
                    selection.insert(id);
                }
            }
            ctx.render(&ui);
        });
    }
    {
        // Press-and-move on a finished row drags its file(s) to other apps.
        let gesture = Rc::new(std::cell::RefCell::new(crate::dragout::Gesture::default()));
        let pressed = gesture.clone();
        ui.on_drag_pressed(move |id| pressed.borrow_mut().press(id));
        let ctx = ctx.clone();
        ui.on_drag_moved(move |id, dx, dy| {
            if !gesture.borrow_mut().moved(id, dx, dy) {
                return false;
            }
            let Some(ui) = ctx.weak.upgrade() else {
                return false;
            };
            let paths = crate::dragout::drag_paths(
                &lock(&ctx.downloads),
                &lock(&ctx.selection),
                i64::from(id),
                std::path::Path::exists,
            );
            if paths.is_empty() {
                return false;
            }
            let count = paths.len();
            crate::dragout::start(&ui, paths, || drag_icon(&ui, id, count))
        });
    }
    {
        let ctx = ctx.clone();
        ui.on_copy_selected_files(move || {
            if let Some(ui) = ctx.weak.upgrade() {
                copy_selected_files(&ctx, &ui);
            }
        });
    }
    {
        let ctx = ctx.clone();
        ui.on_dismiss_notice(move |id| {
            let Some(ui) = ctx.weak.upgrade() else { return };
            lock(&ctx.notices).dismiss(id);
            ctx.render_notices(&ui);
        });
    }
    {
        let ctx = ctx.clone();
        ui.on_select_visible(move |checked| {
            let Some(ui) = ctx.weak.upgrade() else { return };
            let visible = rows_from_downloads(
                &lock(&ctx.downloads),
                ctx.filter.load(Ordering::Relaxed),
                &ui.get_search_text(),
                &ctx.catalog(),
                &HashSet::new(),
                &HashSet::new(),
                false,
            )
            .into_iter()
            .map(|row| i64::from(row.id))
            .collect::<HashSet<_>>();
            {
                let mut selection = lock(&ctx.selection);
                if checked {
                    selection.extend(visible);
                } else {
                    selection.retain(|id| !visible.contains(id));
                }
            }
            ctx.render(&ui);
        });
    }
    {
        let ctx = ctx.clone();
        ui.on_toggle_package(move |package_id| {
            let Some(ui) = ctx.weak.upgrade() else { return };
            {
                let mut collapsed = lock(&ctx.collapsed);
                if !collapsed.insert(i64::from(package_id)) {
                    collapsed.remove(&i64::from(package_id));
                }
            }
            ctx.render(&ui);
        });
    }
    {
        let ctx = ctx.clone();
        ui.on_package_action(move |package_id, action| {
            let Some(ui) = ctx.weak.upgrade() else { return };
            // Only the members the header counts: those visible under the
            // current filter and search.
            let ids = rows_from_downloads(
                &lock(&ctx.downloads),
                ctx.filter.load(Ordering::Relaxed),
                &ui.get_search_text(),
                &ctx.catalog(),
                &HashSet::new(),
                &HashSet::new(),
                false,
            )
            .into_iter()
            .filter(|row| row.package_id == package_id)
            .map(|row| i64::from(row.id))
            .collect::<Vec<_>>();
            run_batch(&ctx, ids, action.to_string(), false);
        });
    }
    {
        let ctx = ctx.clone();
        ui.on_bulk_action(move |action| {
            let Some(ui) = ctx.weak.upgrade() else { return };
            if action == "clear" {
                lock(&ctx.selection).clear();
                ctx.render(&ui);
                return;
            }
            let ids = lock(&ctx.selection).iter().copied().collect::<Vec<_>>();
            run_batch(&ctx, ids, action.to_string(), true);
        });
    }
    {
        let ctx = ctx.clone();
        ui.on_pause_all(move || ctx.command(Origin::Page, ControlCommand::PauseAll));
    }
    {
        let ctx = ctx.clone();
        ui.on_resume_all(move || ctx.command(Origin::Page, ControlCommand::ResumeAll));
    }
    {
        let ctx = ctx.clone();
        ui.on_remove_completed(move || ctx.command(Origin::Page, ControlCommand::RemoveCompleted));
    }
    {
        let ctx = ctx.clone();
        ui.on_remove_failed(move || ctx.command(Origin::Page, ControlCommand::RemoveFailed));
    }
    {
        let ctx = ctx.clone();
        ui.on_filter_changed(move |value| {
            ctx.filter.store(value, Ordering::Relaxed);
            if let Some(ui) = ctx.weak.upgrade() {
                ctx.render(&ui);
            }
        });
    }
    {
        let ctx = ctx.clone();
        ui.on_refresh(move || ctx.refresh());
    }
    {
        let ctx = ctx.clone();
        ui.on_grab_links(move |text| {
            let Some(ui) = ctx.weak.upgrade() else { return };
            ui.set_links_busy(true);
            let command = ControlCommand::GrabLinks {
                text: text.to_string(),
            };
            request(&ctx, command, |ctx, ui, result| {
                ui.set_links_busy(false);
                let links = match result.and_then(|value| {
                    serde_json::from_value::<Vec<minidl_core::grabber::ParsedLink>>(value)
                        .map_err(|error| error.to_string())
                }) {
                    Ok(links) => links,
                    Err(error) => return ctx.report(ui, Origin::Links, Err(error)),
                };
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
                ctx.report(ui, Origin::Links, Ok(()));
            });
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_toggle_grabbed(move |index, selected| {
            let Some(ui) = weak.upgrade() else { return };
            set_choice(&ui.get_grabbed_links(), index, |row| {
                row.selected = selected
            });
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_select_grabbed(move |selected| {
            let Some(ui) = weak.upgrade() else { return };
            let model = ui.get_grabbed_links();
            for index in 0..model.row_count() {
                set_choice(&model, index as i32, |row| row.selected = selected);
            }
        });
    }
    {
        let ctx = ctx.clone();
        ui.on_add_grabbed(move || {
            let Some(ui) = ctx.weak.upgrade() else { return };
            let model = ui.get_grabbed_links();
            let urls: Vec<_> = (0..model.row_count())
                .filter_map(|index| model.row_data(index))
                .filter(|row| row.selected)
                .map(|row| row.url.to_string())
                .collect();
            if urls.is_empty() {
                let text = ctx.catalog().get("selectOneLink").to_owned();
                ctx.notice(&ui, Origin::Links, true, &text);
                return;
            }
            ctx.command(
                Origin::Links,
                ControlCommand::AddLinks {
                    urls,
                    package_name: (!ui.get_link_package().trim().is_empty())
                        .then(|| ui.get_link_package().to_string()),
                },
            );
        });
    }
    {
        let ctx = ctx.clone();
        ui.on_add_media(move |url| {
            let Some(ui) = ctx.weak.upgrade() else { return };
            ctx.command(
                Origin::Media,
                ControlCommand::AddMedia {
                    url: url.to_string(),
                    format_id: None,
                    options: Some(media_options(&ui)),
                },
            );
        });
    }
    {
        let ctx = ctx.clone();
        ui.on_probe_media(move |url, playlist| {
            let Some(ui) = ctx.weak.upgrade() else { return };
            let url = url.to_string();
            let token = ctx.probe_token.fetch_add(1, Ordering::Relaxed) + 1;
            lock(&ctx.probe).start(&url, token);
            // Nothing from an earlier probe stays on screen or addable.
            ui.set_media_busy(true);
            ui.set_media_loaded_url("".into());
            ui.set_media_title("".into());
            ui.set_media_is_playlist(false);
            ui.set_media_choices(ModelRc::new(VecModel::from(Vec::<MediaChoice>::new())));
            lock(&ctx.notices).clear(Origin::Media);
            ctx.render_notices(&ui);
            let command = ControlCommand::ProbeMedia {
                url: url.clone(),
                playlist,
            };
            request(&ctx, command, move |ctx, ui, result| {
                let info = result.and_then(|value| {
                    serde_json::from_value::<minidl_core::ytdlp::MediaInfo>(value)
                        .map_err(|error| error.to_string())
                });
                if !lock(&ctx.probe).finish(token, info.is_ok()) {
                    return; // a newer probe owns the dialog
                }
                ui.set_media_busy(false);
                let info = match info {
                    Ok(info) => info,
                    Err(error) => return ctx.report(ui, Origin::Media, Err(error)),
                };
                let choice_rows = media_choices(&info, &url);
                ui.set_media_title(info.title.as_str().into());
                ui.set_media_is_playlist(info.kind == "playlist");
                ui.set_media_entries_text(
                    ctx.catalog()
                        .get("plEntries")
                        .replace("{n}", &choice_rows.len().to_string())
                        .into(),
                );
                ui.set_media_choices(ModelRc::new(VecModel::from(choice_rows)));
                ui.set_media_loaded_url(url.as_str().into());
            });
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_toggle_media_choice(move |index, selected| {
            let Some(ui) = weak.upgrade() else { return };
            set_choice(&ui.get_media_choices(), index, |row| {
                row.selected = selected
            });
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_select_media_choices(move |selected| {
            let Some(ui) = weak.upgrade() else { return };
            let model = ui.get_media_choices();
            for index in 0..model.row_count() {
                set_choice(&model, index as i32, |row| row.selected = selected);
            }
        });
    }
    {
        let ctx = ctx.clone();
        ui.on_add_playlist(move || {
            let Some(ui) = ctx.weak.upgrade() else { return };
            if !lock(&ctx.probe).can_add(&ui.get_media_url()) {
                return;
            }
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
                let text = ctx.catalog().get("selectOnePlaylist").to_owned();
                ctx.notice(&ui, Origin::Media, true, &text);
                return;
            }
            let quality = match ui.get_media_quality().as_str() {
                "1080p" => Some("bv*[height<=1080]+ba/b".into()),
                "720p" => Some("bv*[height<=720]+ba/b".into()),
                _ => None,
            };
            ctx.command(
                Origin::Media,
                ControlCommand::AddPlaylist {
                    entries,
                    package_name: ui.get_media_title().to_string(),
                    quality,
                    options: Some(media_options(&ui)),
                },
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
        let ctx = ctx.clone();
        ui.on_apply_torrent_files(move |id| {
            let Some(ui) = ctx.weak.upgrade() else { return };
            let model = ui.get_detail_files();
            let indices = (0..model.row_count())
                .filter_map(|index| model.row_data(index))
                .filter(|row| row.selected)
                .filter_map(|row| u32::try_from(row.index).ok())
                .collect();
            ctx.command(
                Origin::Details,
                ControlCommand::SetTorrentFiles {
                    id: i64::from(id),
                    indices,
                },
            );
        });
    }
    {
        let ctx = ctx.clone();
        ui.on_add_media_choice(move |url, format_id| {
            let Some(ui) = ctx.weak.upgrade() else { return };
            // A format row belongs to the probed URL; refuse it once the
            // field shows another one (the buttons are disabled then too).
            if !lock(&ctx.probe).can_add(&ui.get_media_url()) || url != ui.get_media_url() {
                return;
            }
            ctx.command(
                Origin::Media,
                ControlCommand::AddMedia {
                    url: url.to_string(),
                    format_id: (!format_id.is_empty()).then(|| format_id.to_string()),
                    options: Some(media_options(&ui)),
                },
            );
        });
    }
    {
        let ctx = ctx.clone();
        ui.on_save_category(move |name, dir, rules, priority| {
            ctx.command(
                Origin::Settings,
                ControlCommand::SaveCategory {
                    name: name.to_string(),
                    dir: dir.to_string(),
                    rules: rules.to_string(),
                    priority: i64::from(priority),
                },
            )
        });
    }
    {
        let ctx = ctx.clone();
        ui.on_move_category(move |id, direction| {
            ctx.command(
                Origin::Settings,
                ControlCommand::MoveCategory {
                    id: i64::from(id),
                    direction,
                },
            )
        });
    }
    {
        let ctx = ctx.clone();
        ui.on_save_new_category(move |name, dir, extensions, mimes, hosts| {
            let Some(ui) = ctx.weak.upgrade() else { return };
            if name.trim().is_empty() || dir.trim().is_empty() {
                let text = ctx.catalog().get("categoryRequired").to_owned();
                ctx.notice(&ui, Origin::Settings, true, &text);
                return;
            }
            let priority = i64::try_from(ui.get_categories().row_count()).unwrap_or(i64::MAX);
            ctx.command(
                Origin::Settings,
                ControlCommand::SaveCategory {
                    name: name.trim().to_owned(),
                    dir: dir.trim().to_owned(),
                    rules: category_rules(&extensions, &mimes, &hosts),
                    priority,
                },
            );
        });
    }
    {
        let ctx = ctx.clone();
        ui.on_browse_category(move |name, rules, priority| {
            let ctx = ctx.clone();
            ctx.runtime.clone().spawn(async move {
                let title = ctx.catalog().get("browseFolder").to_owned();
                let picked = rfd::AsyncFileDialog::new()
                    .set_title(title)
                    .pick_folder()
                    .await;
                let Some(folder) = picked else { return };
                ctx.command(
                    Origin::Settings,
                    ControlCommand::SaveCategory {
                        name: name.to_string(),
                        dir: folder.path().to_string_lossy().into_owned(),
                        rules: rules.to_string(),
                        priority: i64::from(priority),
                    },
                );
            });
        });
    }
    {
        let ctx = ctx.clone();
        ui.on_browse_new_category_dir(move || {
            let ctx = ctx.clone();
            ctx.runtime.clone().spawn(async move {
                let title = ctx.catalog().get("browseFolder").to_owned();
                let picked = rfd::AsyncFileDialog::new()
                    .set_title(title)
                    .pick_folder()
                    .await;
                let Some(folder) = picked else { return };
                let path = folder.path().to_string_lossy().into_owned();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ctx.weak.upgrade() {
                        ui.set_category_dir(path.into());
                    }
                });
            });
        });
    }
    {
        let ctx = ctx.clone();
        ui.on_delete_category(move |id| {
            ctx.command(
                Origin::Settings,
                ControlCommand::DeleteCategory { id: i64::from(id) },
            )
        });
    }
    {
        let ctx = ctx.clone();
        ui.on_reset_category(move |id| {
            ctx.command(
                Origin::Settings,
                ControlCommand::ResetCategoryDir { id: i64::from(id) },
            )
        });
    }
    {
        let ctx = ctx.clone();
        ui.on_restore_categories(move || {
            ctx.command(Origin::Settings, ControlCommand::RestoreDefaultCategories)
        });
    }
    {
        let ctx = ctx.clone();
        ui.on_save_schedule(move |id, name, action, days, minute, speed, enabled| {
            ctx.command(
                Origin::Settings,
                ControlCommand::SaveSchedule {
                    id: (id > 0).then_some(i64::from(id)),
                    name: (!name.trim().is_empty()).then(|| name.to_string()),
                    action: action.to_string(),
                    days_mask: i64::from(days),
                    at_minute: i64::from(minute),
                    speed_limit: (speed > 0).then_some(i64::from(speed)),
                    enabled,
                },
            )
        });
    }
    {
        let ctx = ctx.clone();
        ui.on_delete_schedule(move |id| {
            ctx.command(
                Origin::Settings,
                ControlCommand::DeleteSchedule { id: i64::from(id) },
            )
        });
    }
    {
        let ctx = ctx.clone();
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
                let next = lock(&ctx.settings).submit(next);
                if let Some(draft) = next {
                    save_settings(&ctx, draft);
                }
            },
        );
    }
    {
        let ctx = ctx.clone();
        ui.on_check_update(move || {
            request(
                &ctx,
                ControlCommand::CheckUpdate,
                |ctx, ui, result| match result.and_then(|value| {
                    serde_json::from_value::<minidl_app::updater::UpdateInfo>(value)
                        .map_err(|error| error.to_string())
                }) {
                    Ok(update) => {
                        let catalog = ctx.catalog();
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
                },
            );
        });
    }
    {
        let ctx = ctx.clone();
        ui.on_install_update(move |asset, page| {
            ctx.command(
                Origin::Settings,
                ControlCommand::InstallUpdate {
                    asset_url: (!asset.is_empty()).then(|| asset.to_string()),
                    page_url: page.to_string(),
                },
            )
        });
    }
    {
        let ctx = ctx.clone();
        ui.on_open_external(move |url| {
            let Some(ui) = ctx.weak.upgrade() else { return };
            // Detached: never wait for the browser to exit.
            let result = open::that_detached(url.as_str()).map_err(|error| error.to_string());
            ctx.report(&ui, Origin::Settings, result);
        });
    }
}

fn set_choice<T: Clone + 'static>(model: &ModelRc<T>, index: i32, change: impl FnOnce(&mut T)) {
    if let Some(mut row) = usize::try_from(index)
        .ok()
        .and_then(|index| model.row_data(index))
    {
        change(&mut row);
        model.set_row_data(index as usize, row);
    }
}

/// Pause, resume or remove several rows; with `from_selection`, the failed
/// ones stay selected.
fn run_batch(ctx: &Arc<Ctx>, ids: Vec<i64>, action: String, from_selection: bool) {
    if ids.is_empty() {
        return;
    }
    let ctx = ctx.clone();
    ctx.runtime.clone().spawn(async move {
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
            if let Err(error) = ctx.client.request(command).await {
                failed.insert(id);
                last_error = Some(error);
            }
        }
        if from_selection {
            *lock(&ctx.selection) = failed;
        }
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(ui) = ctx.weak.upgrade() {
                ctx.render(&ui);
                ctx.report(&ui, Origin::Page, last_error.map_or(Ok(()), Err));
            }
        });
    });
}

/// Send one settings draft; when it finishes, send the newest pending one or
/// revert to the last confirmed values on failure.
fn save_settings(ctx: &Arc<Ctx>, draft: SettingsDraft) {
    let locale_code = locale_code_from_name(&draft.locale_choice).to_owned();
    let command = ControlCommand::ApplySettings {
        settings: SettingsUpdate {
            proxy: draft.proxy.clone(),
            max_concurrent: draft.maximum.max(1) as u32,
            split: draft.split.max(1) as u32,
            connections: draft.connections.max(1) as u32,
            clipboard: draft.clipboard,
            theme: match draft.theme.as_str() {
                "light" | "dark" => draft.theme.clone(),
                _ => "system".into(),
            },
            auto_organize: draft.auto_organize,
            prevent_duplicates: draft.prevent_duplicates,
            close_to_tray: draft.close_to_tray,
            autostart: draft.autostart,
            handle_magnets: draft.handle_magnets,
            dht: draft.dht,
            sandbox: draft.sandbox,
            block_private: draft.block_private,
            locale: locale_code.clone(),
            on_complete_action: if draft.complete_action == "run:" {
                format!("run:{}", draft.complete_command.trim())
            } else {
                draft.complete_action.clone()
            },
            on_complete_confirmed: draft.complete_confirmed,
            default_speed_limit: default_speed_mb_to_bytes(draft.default_speed_mb),
        },
    };
    request(ctx, command, move |ctx, ui, result| {
        let ok = result.is_ok();
        if ok {
            *ctx.catalog
                .write()
                .unwrap_or_else(|error| error.into_inner()) =
                locale::Catalog::new(locale::selected(Some(&locale_code)));
            apply_i18n(ui, &ctx.catalog());
            ctx.render(ui);
        }
        ctx.report(ui, Origin::Settings, result.map(drop));
        let next = lock(&ctx.settings).finished(draft, ok);
        match next {
            SaveNext::Idle => {}
            SaveNext::Send(pending) => save_settings(ctx, pending),
            SaveNext::Revert(saved) => saved.apply_to_ui(ui),
        }
    });
}
