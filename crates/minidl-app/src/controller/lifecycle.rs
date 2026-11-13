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
            let mut engine_state = notification_handle.engine.state();
            loop {
                tokio::select! {
                    _ = stop.changed() => break,
                    changed = engine_state.changed() => {
                        if changed.is_err() {
                            break;
                        }
                        let state = engine_state.borrow_and_update().clone();
                        this.events.emit("engine:state", json!(state));
                        // A respawned aria2 restored only its last saved
                        // session: put every running row back in line with it.
                        if matches!(state, minidl_core::aria2::EngineState::Ready { .. }) {
                            let defaults = this.defaults.lock().unwrap_or_else(|e| e.into_inner()).clone();
                            for id in crate::reconcile::recover_lost_jobs(
                                &notification_handle.engine, &this.db, &defaults, None,
                            ).await {
                                this.emit_row_state(id);
                            }
                            this.events.emit("downloads:reconciled", json!({}));
                        }
                    }
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

    /// No yt-dlp process outlives the controller. At startup a video row
    /// that was downloading is shown as interrupted (Retry continues its
    /// partial files) and rows that were waiting for a slot start again.
    pub(super) fn recover_ytdlp_rows(self: &Arc<Self>) {
        let mut restart = Vec::new();
        for row in self.db.running_rows().unwrap_or_default() {
            if !row.is_ytdlp() {
                continue;
            }
            match row.status {
                DownloadStatus::Active => {
                    let _ = self.db.fail(
                        row.id,
                        Some(INTERRUPTED),
                        Some("interrupted: the app stopped while this was downloading"),
                        "startup: no yt-dlp process survived the restart",
                    );
                }
                DownloadStatus::Queued | DownloadStatus::Waiting => restart.push(row),
                _ => {}
            }
        }
        if restart.is_empty() {
            return;
        }
        // Starting validates the target over DNS: keep it off the open path.
        let this = self.clone();
        tokio::spawn(async move {
            for row in restart {
                if let Err(error) = this
                    .start_ytdlp(&row, "startup: queued video started")
                    .await
                {
                    let _ = this.db.fail(
                        row.id,
                        None,
                        Some(&error.message),
                        "startup: queued video could not start",
                    );
                }
                this.emit_row_state(row.id);
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

    pub(super) fn emit_row_state(&self, id: i64) {
        if let Some(row) = self.db.get(id).ok().flatten() {
            self.events.emit(
                "downloads:state",
                json!({"id": id, "status": row.status.as_str()}),
            );
        }
    }

    async fn handle_transition(&self, handle: &EngineHandle, gid: &str) {
        let status = match handle.engine.rpc.tell_status(gid, STATUS_KEYS).await {
            Ok(status) => status,
            Err(error) => {
                // aria2 is up but no longer knows a job a running row is bound
                // to: re-add it or fail it, never leave it "active" forever.
                if error.to_string().contains("is not found") {
                    if let Some(row) = self.db.find_by_gid(gid).ok().flatten() {
                        let defaults = self
                            .defaults
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .clone();
                        crate::reconcile::recover_lost_jobs(
                            &handle.engine,
                            &self.db,
                            &defaults,
                            Some(row.id),
                        )
                        .await;
                        self.emit_row_state(row.id);
                    }
                }
                return;
            }
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
                match crate::completion::complete(
                    &self.db,
                    &row,
                    &status,
                    "aria2: download complete",
                ) {
                    crate::completion::Completion::Followed(child) => {
                        self.events.emit(
                            "downloads:state",
                            json!({"id": row.id, "gid": child, "status": "active"}),
                        );
                        let _ = handle.engine.rpc.remove_download_result(gid).await;
                        return;
                    }
                    crate::completion::Completion::Finished {
                        directory,
                        filename,
                        problem,
                    } => {
                        if let Some(message) = problem {
                            self.events
                                .emit("downloads:error", json!({"id": row.id, "message": message}));
                        }
                        let path = Path::new(&directory).join(&filename);
                        self.events.emit(
                            "downloads:complete",
                            json!({
                                "id": row.id, "name": filename, "path": path.to_string_lossy()
                            }),
                        );
                        self.maybe_completion_action().await;
                    }
                    crate::completion::Completion::Refused(message) => {
                        self.events
                            .emit("downloads:error", json!({"id": row.id, "message": message}));
                        self.events
                            .emit("downloads:state", json!({"id": row.id, "status": "error"}));
                        let _ = handle.engine.rpc.remove_download_result(gid).await;
                        self.arm_engine_if_idle().await;
                        return;
                    }
                    crate::completion::Completion::AlreadyDone => {}
                }
            }
            DownloadStatus::Error => {
                let code = status.get("errorCode").and_then(Value::as_str);
                let message = crate::reconcile::error_text(&status);
                if self
                    .db
                    .fail(row.id, code, message.as_deref(), "aria2: download failed")
                    .unwrap_or(false)
                {
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
                            let _ = delete_download_file(Path::new(&row.dir), Path::new(file));
                        }
                    }
                    self.events
                        .emit("downloads:error", json!({"id": row.id, "message": message}));
                }
            }
            _ => {
                let _ = self
                    .db
                    .transition(row.id, next, &format!("aria2: {}", next.as_str()));
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
