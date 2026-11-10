use super::*;

impl Controller {
    pub(super) async fn ensure_engine(self: &Arc<Self>) -> Result<EngineHandle, AppError> {
        let _guard = self.engine_init.lock().await;
        let handle = self.engine.get().await?;
        if self.initialized_session.load(Ordering::Acquire) != handle.id {
            crate::reconcile::restore_startup_session(&handle.engine, &self.db).await;
            crate::reconcile::reconcile(&handle.engine, &self.db).await;
            self.initialized_session.store(handle.id, Ordering::Release);
            self.events.emit("downloads:reconciled", json!({}));
            self.spawn_sync(handle.clone());
        }
        Ok(handle)
    }

    pub(super) fn spawn_sync(self: &Arc<Self>, handle: EngineHandle) {
        let activity = Arc::new(tokio::sync::Notify::new());
        let this = self.clone();
        let notification_handle = handle.clone();
        let notification_activity = activity.clone();
        tokio::spawn(async move {
            let mut stop = notification_handle.stop.clone();
            let mut notifications = notification_handle.engine.subscribe();
            loop {
                tokio::select! {
                    _ = stop.changed() => break,
                    event = notifications.recv() => match event {
                        Ok(event) => {
                            if matches!(event, Aria2Event::Start(_)) {
                                this.power_fired.store(false, Ordering::Relaxed);
                                this.engine.cancel_idle_shutdown();
                            }
                            this.handle_transition(&notification_handle, event.gid()).await;
                            notification_activity.notify_one();
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        });
        let this = self.clone();
        tokio::spawn(async move {
            let mut stop = handle.stop.clone();
            let mut known = HashSet::new();
            let mut checkpoint_tick = 0u32;
            loop {
                let active = handle
                    .engine
                    .rpc
                    .tell_active(POLL_KEYS)
                    .await
                    .unwrap_or_default();
                checkpoint_tick = checkpoint_tick.wrapping_add(1);
                let checkpoint = checkpoint_tick.is_multiple_of(PROGRESS_CHECKPOINT_TICKS);
                let mut current = HashSet::with_capacity(active.len());
                let mut ticks = Vec::with_capacity(active.len());
                for item in &active {
                    let gid = value_string(item, "gid");
                    if gid.is_empty() {
                        continue;
                    }
                    current.insert(gid.clone());
                    let Some(row) = self::lookup(&this.db, &gid) else {
                        continue;
                    };
                    let completed = value_i64(item, "completedLength");
                    let total = value_i64(item, "totalLength");
                    let download_speed = value_i64(item, "downloadSpeed");
                    let upload_speed = value_i64(item, "uploadSpeed");
                    let connections = value_i64(item, "connections");
                    let seeders = value_i64(item, "numSeeders");
                    if checkpoint {
                        let _ = this.db.checkpoint_progress(
                            &gid,
                            completed,
                            total,
                            download_speed,
                            upload_speed,
                            connections,
                            seeders,
                        );
                    }
                    ticks.push(json!({
                        "id": row.id, "gid": gid, "name": "", "completed": completed,
                        "total": total, "dl_speed": download_speed, "ul_speed": upload_speed,
                        "connections": connections, "num_seeders": seeders, "status": "active"
                    }));
                }
                if !ticks.is_empty() {
                    this.events
                        .emit("downloads:tick", json!({"updates": ticks}));
                }
                let transitioned: Vec<_> = known.difference(&current).cloned().collect();
                for gid in transitioned {
                    this.handle_transition(&handle, &gid).await;
                }
                known = current;
                let waiting = handle
                    .engine
                    .rpc
                    .tell_waiting(0, 1, &["gid"])
                    .await
                    .unwrap_or_default();
                if active.is_empty() && waiting.is_empty() {
                    this.engine.arm_idle_shutdown();
                    tokio::select! {
                        _ = stop.changed() => break,
                        _ = activity.notified() => {
                            this.engine.cancel_idle_shutdown();
                            continue;
                        }
                    }
                }
                tokio::select! {
                    _ = stop.changed() => break,
                    _ = tokio::time::sleep(LIVE_POLL_INTERVAL) => {}
                }
            }
        });
    }

    pub(super) fn spawn_ytdlp_completion_handler(
        self: &Arc<Self>,
        mut completions: mpsc::UnboundedReceiver<YtDlpCompletion>,
    ) {
        let controller = Arc::downgrade(self);
        tokio::spawn(async move {
            while let Some(completion) = completions.recv().await {
                let Some(controller) = controller.upgrade() else {
                    return;
                };
                controller.handle_ytdlp_completion(completion).await;
            }
        });
    }

    pub(super) async fn handle_ytdlp_completion(&self, completion: YtDlpCompletion) {
        let Some(row) = self.db.get(completion.id).ok().flatten() else {
            return;
        };
        let filename = completion
            .name
            .or_else(|| row.filename.clone())
            .unwrap_or_default();
        let path = completion.path.unwrap_or_else(|| {
            Path::new(&row.dir)
                .join(&filename)
                .to_string_lossy()
                .into_owned()
        });
        crate::completion::set_origin(Path::new(&path), &row.url, row.referrer.as_deref());
        self.events.emit(
            "downloads:complete",
            json!({"id": row.id, "name": filename, "path": path}),
        );
        self.events.emit(
            "downloads:state",
            json!({"id": row.id, "status": "complete"}),
        );
        self.maybe_completion_action().await;
    }

    async fn handle_transition(&self, handle: &EngineHandle, gid: &str) {
        let Ok(status) = handle.engine.rpc.tell_status(gid, STATUS_KEYS).await else {
            return;
        };
        let Some(row) = self.db.find_by_gid(gid).ok().flatten() else {
            let _ = handle.engine.rpc.remove_download_result(gid).await;
            self.arm_engine_if_idle().await;
            return;
        };
        let _ = self.db.checkpoint_progress(
            gid,
            value_i64(&status, "completedLength"),
            value_i64(&status, "totalLength"),
            value_i64(&status, "downloadSpeed"),
            value_i64(&status, "uploadSpeed"),
            value_i64(&status, "connections"),
            value_i64(&status, "numSeeders"),
        );
        let Some(next) = DownloadStatus::from_aria2_str(&value_string(&status, "status")) else {
            return;
        };
        if row.status == DownloadStatus::Scheduled
            && matches!(next, DownloadStatus::Paused | DownloadStatus::Waiting)
        {
            self.arm_engine_if_idle().await;
            return;
        }
        match next {
            DownloadStatus::Complete => {
                if let Some(child) = crate::completion::first_followed_by(&status) {
                    let _ = self.db.set_gid(row.id, &child);
                    if let Some(info_hash) = status
                        .get("infoHash")
                        .and_then(Value::as_str)
                        .filter(|value| !value.is_empty())
                    {
                        let _ = self.db.set_info_hash(row.id, info_hash);
                    }
                    let _ = self.db.set_status(row.id, DownloadStatus::Active);
                    self.events.emit(
                        "downloads:state",
                        json!({"id": row.id, "gid": child, "status": "active"}),
                    );
                    let _ = handle.engine.rpc.remove_download_result(gid).await;
                    return;
                }
                if self.db.set_status_if_changed(row.id, next).unwrap_or(false) {
                    let reported = crate::completion::basename(&status);
                    let filename = if reported.is_empty() {
                        row.filename.clone().unwrap_or_default()
                    } else {
                        reported
                    };
                    let (directory, filename) =
                        crate::completion::organize(&self.db, &row, &filename);
                    if !filename.is_empty() && row.filename.as_deref() != Some(filename.as_str()) {
                        let _ = self.db.set_filename(row.id, &filename);
                    }
                    let path = Path::new(&directory).join(&filename);
                    crate::completion::set_origin(&path, &row.url, row.referrer.as_deref());
                    self.events.emit(
                        "downloads:complete",
                        json!({
                            "id": row.id, "name": filename, "path": path.to_string_lossy()
                        }),
                    );
                    self.maybe_completion_action().await;
                }
            }
            DownloadStatus::Error => {
                if self.db.set_status_if_changed(row.id, next).unwrap_or(false) {
                    let code = status.get("errorCode").and_then(Value::as_str);
                    let message = crate::reconcile::error_text(&status);
                    if code == Some("32") {
                        // Bytes that failed verification must not sit in the
                        // folder looking finished, and aria2 would only
                        // re-verify the same bytes on every retry.
                        for file in status
                            .get("files")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                            .filter_map(|file| file.get("path").and_then(Value::as_str))
                        {
                            delete_download_file(Path::new(&row.dir), Path::new(file));
                        }
                    }
                    let _ = self.db.set_error(row.id, code, message.as_deref());
                    self.events
                        .emit("downloads:error", json!({"id": row.id, "message": message}));
                }
            }
            _ => {
                let _ = self.db.set_status(row.id, next);
            }
        }
        self.events.emit(
            "downloads:state",
            json!({"id": row.id, "status": next.as_str()}),
        );
        if next.is_terminal() {
            let _ = handle.engine.rpc.remove_download_result(gid).await;
        }
        if !next.is_running() {
            self.arm_engine_if_idle().await;
        }
    }

    async fn maybe_completion_action(&self) {
        let action = self
            .db
            .get_setting("on_complete_action")
            .ok()
            .flatten()
            .unwrap_or_default();
        if action.is_empty() || action == "none" {
            return;
        }
        if let Some(engine) = self.engine.current().await {
            if let Ok(stats) = engine.rpc.get_global_stat().await {
                if value_i64(&stats, "numActive") + value_i64(&stats, "numWaiting") > 0 {
                    return;
                }
            }
        }
        if self.ytdlp.is_running() {
            return;
        }
        if self.db.has_unfinished_downloads().unwrap_or(true) {
            return;
        }
        if action.starts_with("run:")
            && self
                .db
                .get_setting("on_complete_command_confirmed")
                .ok()
                .flatten()
                .as_deref()
                != Some("true")
        {
            return;
        }
        if self.power_fired.swap(true, Ordering::SeqCst) {
            return;
        }
        match action.as_str() {
            "quit" => {
                self.quit.store(true, Ordering::Relaxed);
                self.ytdlp.cancel_all();
                self.engine.shutdown().await;
                self.events.emit("app:quit", json!({}));
            }
            "sleep" => spawn_power_command("sleep", None),
            "shutdown" => spawn_power_command("shutdown", None),
            value => {
                if let Some(command) = value.strip_prefix("run:") {
                    spawn_power_command("run", Some(command));
                }
            }
        }
    }

    pub(super) async fn arm_engine_if_idle(&self) {
        if !self
            .db
            .running_rows()
            .unwrap_or_default()
            .iter()
            .any(|row| !row.is_ytdlp() && row.status.is_running())
        {
            self.engine.arm_idle_shutdown();
        }
    }
}
