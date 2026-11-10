use super::*;

impl Controller {
    pub async fn snapshot(&self) -> Result<AppSnapshot, AppError> {
        let mut settings = HashMap::new();
        for key in SETTINGS_KEYS {
            if let Some(value) = self.db.get_setting(key)? {
                settings.insert((*key).into(), value);
            }
        }
        let packages = self.db.list_packages()?;
        let package_names = packages
            .iter()
            .map(|package| (package.id, package.name.clone()))
            .collect::<HashMap<_, _>>();
        let downloads = self
            .db
            .list(None)?
            .into_iter()
            .map(|download| {
                let mut view = DownloadView::from(download);
                view.package_name = view
                    .package_id
                    .and_then(|id| package_names.get(&id).cloned());
                view
            })
            .collect();
        Ok(AppSnapshot {
            sequence: self.events.sequence(),
            downloads,
            packages,
            categories: self.db.list_categories()?,
            schedules: self.db.list_schedules()?,
            settings,
            engine_running: self.engine.is_running().await,
        })
    }

    pub async fn execute(self: &Arc<Self>, command: ControlCommand) -> Result<Value, AppError> {
        use ControlCommand::*;
        match command {
            Ping => Ok(json!({"version": env!("CARGO_PKG_VERSION")})),
            Show => {
                self.events.emit("ui:show", json!({}));
                Ok(Value::Null)
            }
            Hide => {
                self.events.emit("ui:hide", json!({}));
                Ok(Value::Null)
            }
            Quit => {
                self.quit.store(true, Ordering::Relaxed);
                self.ytdlp.cancel_all();
                self.engine.shutdown().await;
                self.events.emit("app:quit", json!({}));
                Ok(Value::Null)
            }
            Snapshot => value(self.snapshot().await?),
            Subscribe => Err(AppError::classify(
                "subscribe must be the first command on an event connection",
            )),
            IngestUrl { url, checksum } => {
                self.power_fired.store(false, Ordering::Relaxed);
                let url = url.trim().to_owned();
                if url.is_empty() {
                    return Err(AppError::classify("empty URL"));
                }
                let checksum = normalize_checksum(checksum)?;
                let job = ingest::job_from_url(url);
                ingest::validate_capture_job(&self.db, &job)
                    .await
                    .map_err(AppError::classify)?;
                let handle = self.ensure_engine().await?;
                let defaults = self
                    .defaults
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                let outcome = ingest::ingest(
                    Some(&handle.engine),
                    &self.db,
                    &self.ytdlp,
                    &self.download_dir,
                    defaults,
                    job,
                    None,
                    None,
                    checksum,
                )
                .await;
                let outcome = match outcome {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        self.arm_engine_if_idle().await;
                        return Err(AppError::classify(error));
                    }
                };
                if matches!(outcome, IngestOutcome::Added(_)) {
                    self.events.emit(
                        "downloads:state",
                        json!({"id": outcome.id(), "status": "active"}),
                    );
                }
                value(self.view(outcome.id())?)
            }
            IngestFile { path, kind } => {
                self.power_fired.store(false, Ordering::Relaxed);
                if !matches!(kind, DownloadKind::Torrent | DownloadKind::Metalink) {
                    return Err(AppError::classify("unsupported file download type"));
                }
                validate_label("file path", &path, MAX_INPUT_PATH, false)?;
                let metadata = std::fs::metadata(&path)?;
                if !metadata.is_file() {
                    return Err(AppError::classify(
                        "selected torrent or metalink is not a regular file",
                    ));
                }
                if metadata.len() > MAX_METADATA_FILE {
                    return Err(AppError::classify(
                        "torrent or metalink file exceeds 20 MiB",
                    ));
                }
                let mut file = std::fs::File::open(&path)?;
                if !file.metadata()?.is_file() {
                    return Err(AppError::classify(
                        "selected torrent or metalink is not a regular file",
                    ));
                }
                let mut bytes = Vec::new();
                file.by_ref()
                    .take(MAX_METADATA_FILE + 1)
                    .read_to_end(&mut bytes)?;
                if bytes.len() as u64 > MAX_METADATA_FILE {
                    return Err(AppError::classify(
                        "torrent or metalink file exceeds 20 MiB",
                    ));
                }
                let name = Path::new(&path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("download");
                let mut job = ingest::job_from_url(format!("file:{name}"));
                job.kind = kind;
                job.torrent_b64 = Some(base64::engine::general_purpose::STANDARD.encode(bytes));
                ingest::validate_capture_job(&self.db, &job)
                    .await
                    .map_err(AppError::classify)?;
                let handle = self.ensure_engine().await?;
                let defaults = self
                    .defaults
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                let outcome = ingest::ingest(
                    Some(&handle.engine),
                    &self.db,
                    &self.ytdlp,
                    &self.download_dir,
                    defaults,
                    job,
                    None,
                    None,
                    None,
                )
                .await;
                let outcome = match outcome {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        self.arm_engine_if_idle().await;
                        return Err(AppError::classify(error));
                    }
                };
                self.events.emit(
                    "downloads:state",
                    json!({"id": outcome.id(), "status": "active"}),
                );
                value(self.view(outcome.id())?)
            }
            ListDownloads { status } => value(
                self.db
                    .list(status.as_deref())?
                    .into_iter()
                    .map(DownloadView::from)
                    .collect::<Vec<_>>(),
            ),
            Pause { id } => {
                self.pause(id).await?;
                Ok(Value::Null)
            }
            Resume { id } => {
                self.resume(id).await?;
                Ok(Value::Null)
            }
            Retry { id } => {
                self.retry(id).await?;
                value(self.view(id)?)
            }
            Remove { id, delete_files } => {
                self.remove(id, delete_files).await?;
                Ok(Value::Null)
            }
            RemoveCompleted => value(self.remove_status("complete").await?),
            RemoveFailed => value(self.remove_status("error").await?),
            PauseAll => {
                if let Some(engine) = self.engine.current().await {
                    let _ = engine.rpc.pause_all().await;
                }
                for row in self.db.running_rows()? {
                    if row.status.is_running() {
                        // aria2's pauseAll does not reach yt-dlp children.
                        if row.is_ytdlp() {
                            self.ytdlp.cancel(row.id);
                        }
                        let _ = self.db.set_status(row.id, DownloadStatus::Paused);
                    }
                }
                self.events
                    .emit("downloads:state", json!({"batch": "paused"}));
                self.arm_engine_if_idle().await;
                Ok(Value::Null)
            }
            ResumeAll => {
                let ids: Vec<_> = self
                    .db
                    .running_rows()?
                    .into_iter()
                    .filter(|row| row.status == DownloadStatus::Paused)
                    .map(|row| row.id)
                    .collect();
                // One item that cannot resume must not strand the rest paused.
                let mut first_error = None;
                for id in ids {
                    if let Err(error) = self.resume(id).await {
                        first_error.get_or_insert(error);
                    }
                }
                first_error.map_or(Ok(Value::Null), Err)
            }
            MoveInQueue { id, direction } => {
                if let Some(row) = self.db.get(id)? {
                    if let (Some(gid), Some(engine)) = (row.gid, self.engine.current().await) {
                        let (position, how) = match direction.as_str() {
                            "top" => (0, "POS_SET"),
                            "up" => (-1, "POS_CUR"),
                            "down" => (1, "POS_CUR"),
                            "bottom" => (1_000_000, "POS_SET"),
                            _ => return Ok(Value::Null),
                        };
                        let _ = engine.rpc.change_position(&gid, position, how).await;
                    }
                }
                Ok(Value::Null)
            }
            SetQueuePosition { id, position } => {
                if let Some(row) = self.db.get(id)? {
                    if let (Some(gid), Some(engine)) = (row.gid, self.engine.current().await) {
                        let _ = engine
                            .rpc
                            .change_position(&gid, position.max(0), "POS_SET")
                            .await;
                    }
                }
                Ok(Value::Null)
            }
            SetGlobalSpeed { download, upload } => {
                let download = download.max(0);
                let upload = upload.max(0);
                // Persist first: aria2 is stopped whenever it is idle, so a cap
                // held only by the live process is gone by the next download.
                self.db.set_settings(&[
                    ("global_download_limit".into(), download.to_string()),
                    ("global_upload_limit".into(), upload.to_string()),
                ])?;
                self.engine.update_speed_limits(download, upload).await;
                if let Some(engine) = self.engine.current().await {
                    engine
                        .rpc
                        .change_global_option(json!({
                            "max-overall-download-limit": download.to_string(),
                            "max-overall-upload-limit": upload.to_string(),
                        }))
                        .await?;
                }
                Ok(Value::Null)
            }
            SetDownloadSpeed { id, limit } => {
                let limit = limit.max(0);
                self.db.set_speed_limit(id, (limit > 0).then_some(limit))?;
                if let Some(row) = self.db.get(id)? {
                    if let (Some(gid), Some(engine)) = (row.gid, self.engine.current().await) {
                        engine
                            .rpc
                            .change_option(&gid, json!({"max-download-limit": limit.to_string()}))
                            .await?;
                    }
                }
                Ok(Value::Null)
            }
            OpenFile { id } => {
                let row = self
                    .db
                    .get(id)?
                    .ok_or_else(|| AppError::classify("download not found"))?;
                let path = crate::completion::downloaded_path(&row)
                    .ok_or_else(|| AppError::classify("downloaded file is unavailable"))?;
                open::that(path)?;
                Ok(Value::Null)
            }
            OpenContainingFolder { id } => {
                let row = self
                    .db
                    .get(id)?
                    .ok_or_else(|| AppError::classify("download not found"))?;
                open::that(row.dir)?;
                Ok(Value::Null)
            }
            ProbeMedia { url, playlist } => {
                let url = url.trim().to_owned();
                ingest::validate_media_target(&self.db, &url)
                    .await
                    .map_err(AppError::classify)?;
                value(
                    self.ytdlp
                        .probe(&url, &[], playlist)
                        .await
                        .map_err(AppError::classify)?,
                )
            }
            AddMedia {
                url,
                format_id,
                options,
            } => value(self.add_media(url, format_id, options, None).await?),
            AddPlaylist {
                entries,
                package_name,
                quality,
                options,
            } => {
                if entries.len() > ingest::MAX_BATCH_ITEMS {
                    return Err(AppError::classify("playlist exceeds 5000 entries"));
                }
                validate_label("package name", &package_name, 256, false)?;
                crate::ytdlp::validate_options(quality.as_deref(), options.as_ref())
                    .map_err(AppError::classify)?;
                let package = (!entries.is_empty())
                    .then(|| self.db.insert_package(package_name.trim(), None, None))
                    .transpose()?;
                let mut added = 0usize;
                for entry in entries {
                    if self
                        .add_media(entry.url, quality.clone(), options.clone(), package)
                        .await
                        .is_ok()
                    {
                        added += 1;
                    }
                }
                if let Some(package) = package {
                    let _ = self.db.delete_package_if_empty(package);
                }
                self.events.emit("downloads:state", json!({"batch": added}));
                value(added)
            }
            GrabLinks { text } => {
                if text.len() > MAX_LINK_TEXT {
                    return Err(AppError::classify("link text exceeds 1 MiB"));
                }
                value(minidl_core::grabber::parse_links(&text))
            }
            AddLinks { urls, package_name } => {
                self.power_fired.store(false, Ordering::Relaxed);
                if urls.len() > ingest::MAX_BATCH_ITEMS {
                    return Err(AppError::classify("batch exceeds 5000 URLs"));
                }
                if let Some(name) = package_name.as_deref() {
                    validate_label("package name", name, 256, true)?;
                }
                let mut seen = HashSet::new();
                let mut urls = urls
                    .into_iter()
                    .map(|url| url.trim().to_owned())
                    .filter(|url| !url.is_empty())
                    .filter(|url| seen.insert(url.clone()))
                    .collect::<Vec<_>>();
                for url in &urls {
                    let job = ingest::job_from_url(url.clone());
                    ingest::validate_capture_job(&self.db, &job)
                        .await
                        .map_err(AppError::classify)?;
                }
                if urls.is_empty() {
                    return value(0usize);
                }
                let package = if urls.len() > 1 {
                    let name = package_name
                        .filter(|name| !name.trim().is_empty())
                        .unwrap_or_else(|| minidl_core::grabber::host_of(&urls[0]));
                    Some(self.db.insert_package(
                        if name.is_empty() { "Batch" } else { &name },
                        None,
                        None,
                    )?)
                } else {
                    None
                };
                let handle = self.ensure_engine().await?;
                let defaults = self
                    .defaults
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                let mut added = 0usize;
                for url in urls.drain(..) {
                    if matches!(
                        ingest::ingest(
                            Some(&handle.engine),
                            &self.db,
                            &self.ytdlp,
                            &self.download_dir,
                            defaults.clone(),
                            ingest::job_from_url(url),
                            None,
                            package,
                            None
                        )
                        .await,
                        Ok(IngestOutcome::Added(_))
                    ) {
                        added += 1;
                    }
                }
                if let Some(package) = package {
                    let _ = self.db.delete_package_if_empty(package);
                }
                self.arm_engine_if_idle().await;
                self.events.emit("downloads:state", json!({"batch": added}));
                value(added)
            }
            ListPackages => value(self.db.list_packages()?),
            ListCategories => value(self.db.list_categories()?),
            SaveCategory {
                name,
                dir,
                rules,
                priority,
            } => {
                validate_label("category name", &name, 128, false)?;
                validate_label("category directory", &dir, 4_096, false)?;
                if rules.len() > MAX_CATEGORY_RULES {
                    return Err(AppError::classify("category rules exceed 64 KiB"));
                }
                let rules = normalize_category_rules(&rules)?;
                self.db
                    .upsert_category(name.trim(), dir.trim(), &rules, priority)?;
                self.events.emit("categories:changed", json!({}));
                Ok(Value::Null)
            }
            MoveCategory { id, direction } => {
                self.db.move_category(id, direction)?;
                self.events.emit("categories:changed", json!({}));
                Ok(Value::Null)
            }
            DeleteCategory { id } => {
                self.db.delete_category(id)?;
                self.events.emit("categories:changed", json!({}));
                Ok(Value::Null)
            }
            RestoreDefaultCategories => {
                self.db.seed_default_categories()?;
                self.events.emit("categories:changed", json!({}));
                value(self.db.list_categories()?)
            }
            ResetCategoryDir { id } => {
                self.db.reset_category_dir(id)?;
                self.events.emit("categories:changed", json!({}));
                Ok(Value::Null)
            }
            GetDownloadDetails { id } => value(self.details(id).await?),
            SetTorrentFiles { id, indices } => {
                self.set_torrent_files(id, indices).await?;
                Ok(Value::Null)
            }
            ScheduleDownload { id, start_at } => {
                self.schedule(id, start_at).await?;
                Ok(Value::Null)
            }
            ListSchedules => value(self.db.list_schedules()?),
            SaveSchedule {
                id,
                name,
                action,
                days_mask,
                at_minute,
                speed_limit,
                enabled,
            } => {
                if let Some(name) = name.as_deref() {
                    validate_label("schedule name", name, 128, true)?;
                }
                if !matches!(action.as_str(), "pause_all" | "resume_all" | "set_speed") {
                    return Err(AppError::classify("unsupported schedule action"));
                }
                if !(0..=0x7f).contains(&days_mask) {
                    return Err(AppError::classify("invalid schedule day mask"));
                }
                if !(0..=1_439).contains(&at_minute) {
                    return Err(AppError::classify("invalid schedule time"));
                }
                if speed_limit.is_some_and(|limit| limit < 0) {
                    return Err(AppError::classify("invalid schedule speed limit"));
                }
                self.db.save_schedule(
                    id,
                    name.as_deref(),
                    &action,
                    days_mask,
                    at_minute,
                    speed_limit,
                    enabled,
                )?;
                self.events.emit("scheduler:changed", json!({}));
                Ok(Value::Null)
            }
            DeleteSchedule { id } => {
                self.db.delete_schedule(id)?;
                self.events.emit("scheduler:changed", json!({}));
                Ok(Value::Null)
            }
            GetSetting { key } => value(self.db.get_setting(&key)?),
            SetSetting { key: _, value: _ } => Err(AppError::classify(
                "individual setting writes are unsupported; use apply_settings",
            )),
            GetSystemLocale => value(sys_locale::get_locales().find(|locale| {
                !matches!(
                    locale.trim().to_ascii_lowercase().as_str(),
                    "" | "c" | "posix" | "und"
                )
            })),
            GetConnectorStatus => value(crate::connector::status(&self.connector_presence)),
            SetHandleMagnets { enabled } => {
                crate::platform::set_magnet_handler(enabled).map_err(AppError::classify)?;
                self.db
                    .set_setting("handle_magnets", if enabled { "true" } else { "false" })?;
                Ok(Value::Null)
            }
            SetClipboardWatch { enabled } => {
                self.clipboard_on.store(enabled, Ordering::Relaxed);
                self.db
                    .set_setting("clipboard_watch", if enabled { "true" } else { "false" })?;
                self.events
                    .emit("settings:clipboard", json!({"enabled": enabled}));
                Ok(Value::Null)
            }
            GetEngineDefaults => {
                let defaults = self.defaults.lock().unwrap_or_else(|e| e.into_inner());
                value((defaults.split, defaults.max_connection_per_server))
            }
            SetEngineDefaults { split, connections } => {
                let split = split.clamp(1, 64);
                let connections = connections.clamp(1, 16);
                {
                    let mut defaults = self.defaults.lock().unwrap_or_else(|e| e.into_inner());
                    defaults.split = split;
                    defaults.max_connection_per_server = connections;
                }
                self.db.set_setting("split", &split.to_string())?;
                self.db.set_setting("max_conn", &connections.to_string())?;
                Ok(Value::Null)
            }
            GetMaxConcurrent => value(
                self.db
                    .get_setting("max_concurrent")?
                    .and_then(|value| value.parse::<u32>().ok())
                    .unwrap_or(5),
            ),
            SetMaxConcurrent { value: maximum } => {
                let maximum = maximum.clamp(1, 20);
                self.db
                    .set_setting("max_concurrent", &maximum.to_string())?;
                self.ytdlp.set_max_concurrent(maximum as usize);
                self.engine.update_max_concurrent(maximum).await;
                if let Some(engine) = self.engine.current().await {
                    let _ = engine
                        .rpc
                        .change_global_option(
                            json!({"max-concurrent-downloads": maximum.to_string()}),
                        )
                        .await;
                }
                Ok(Value::Null)
            }
            ApplyProxy { value: proxy } => {
                let proxy = proxy.trim().to_owned();
                validate_label("proxy", &proxy, 4_096, true)?;
                let old_proxy = self.db.get_setting("proxy")?.unwrap_or_default();
                let engine = self.engine.current().await;
                if let Some(engine) = &engine {
                    engine
                        .rpc
                        .change_global_option(json!({"all-proxy": proxy.clone()}))
                        .await?;
                }
                if let Err(error) = self.db.set_setting("proxy", &proxy) {
                    if let Some(engine) = &engine {
                        let _ = engine
                            .rpc
                            .change_global_option(json!({"all-proxy": old_proxy}))
                            .await;
                    }
                    return Err(error.into());
                }
                self.engine
                    .update_proxy((!proxy.is_empty()).then_some(proxy))
                    .await;
                Ok(Value::Null)
            }
            ApplySettings { settings } => {
                self.apply_settings(settings).await?;
                Ok(Value::Null)
            }
            CheckUpdate => value(crate::updater::check().await.map_err(AppError::classify)?),
            InstallUpdate {
                asset_url,
                page_url,
            } => {
                if crate::updater::install(asset_url, page_url)
                    .await
                    .map_err(AppError::classify)?
                {
                    self.quit.store(true, Ordering::Relaxed);
                    self.ytdlp.cancel_all();
                    self.engine.shutdown().await;
                    self.events.emit("app:quit", json!({}));
                }
                Ok(Value::Null)
            }
        }
    }
}
