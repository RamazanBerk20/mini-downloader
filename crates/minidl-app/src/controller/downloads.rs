use super::*;

impl Controller {
    pub(super) fn view(&self, id: i64) -> Result<DownloadView, AppError> {
        self.db
            .get(id)?
            .map(Into::into)
            .ok_or_else(|| AppError::classify("download vanished"))
    }

    pub(super) async fn pause(&self, id: i64) -> Result<(), AppError> {
        if let Some(row) = self.db.get(id)? {
            if row.is_ytdlp() {
                self.ytdlp.cancel(id);
            } else if let (Some(gid), Some(engine)) =
                (row.gid.as_deref(), self.engine.current().await)
            {
                let _ = engine.rpc.pause(gid).await;
            }
            self.db
                .transition(id, DownloadStatus::Paused, "user: pause")?;
            self.events
                .emit("downloads:state", json!({"id": id, "status": "paused"}));
        }
        self.arm_engine_if_idle().await;
        Ok(())
    }

    pub(super) async fn resume(self: &Arc<Self>, id: i64) -> Result<(), AppError> {
        self.power_fired.store(false, Ordering::Relaxed);
        let Some(row) = self.db.get(id)? else {
            return Ok(());
        };
        if row.is_ytdlp() {
            self.start_ytdlp(&row, "user: resume, handed to yt-dlp")
                .await?;
        } else {
            let handle = self.ensure_engine().await?;
            let unpaused = match row.gid.as_deref() {
                Some(gid) => handle.engine.rpc.unpause(gid).await.is_ok(),
                None => false,
            };
            if unpaused {
                self.db
                    .transition(id, DownloadStatus::Active, "user: resume")?;
            } else {
                let defaults = self
                    .defaults
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                ingest::reissue(
                    &handle.engine,
                    &self.db,
                    defaults,
                    &row,
                    "user: resume, re-added to aria2",
                )
                .await
                .map_err(AppError::classify)?;
            }
        }
        self.events
            .emit("downloads:state", json!({"id": id, "status": "active"}));
        Ok(())
    }

    pub(super) async fn retry(self: &Arc<Self>, id: i64) -> Result<(), AppError> {
        self.power_fired.store(false, Ordering::Relaxed);
        let row = self
            .db
            .get(id)?
            .ok_or_else(|| AppError::classify("download not found"))?;
        // Checked before touching aria2: the payload of a .torrent/.metalink
        // file is not kept, so nothing could re-add the job afterwards.
        if !retryable(&row.kind) {
            return Err(AppError::classify(
                "a .torrent or .metalink download cannot be retried; add the file again",
            ));
        }
        if let Some(engine) = self.engine.current().await {
            let mut gids = Vec::new();
            if let Some(gid) = row.gid.as_deref() {
                gids.push(gid);
            }
            if let Some(gid) = row.session_gid.as_deref() {
                if !gids.contains(&gid) {
                    gids.push(gid);
                }
            }
            for gid in gids {
                let _ = engine.rpc.force_remove(gid).await;
                let _ = engine.rpc.remove_download_result(gid).await;
            }
        }
        if row.is_ytdlp() {
            self.start_ytdlp(&row, "user: retry, handed to yt-dlp")
                .await?;
        } else {
            let handle = self.ensure_engine().await?;
            let defaults = self
                .defaults
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            ingest::reissue(&handle.engine, &self.db, defaults, &row, "user: retry")
                .await
                .map_err(AppError::classify)?;
        }
        self.events
            .emit("downloads:state", json!({"id": id, "status": "active"}));
        Ok(())
    }

    /// Remove a row. With `delete_files`, delete everything it owns: its
    /// committed file or torrent folder, its partial files and its private
    /// staging folder. Without, a partial left in staging is moved next to
    /// the destination as `<name>.part` so nothing stays hidden.
    pub(super) async fn remove(
        self: &Arc<Self>,
        id: i64,
        delete_files: bool,
    ) -> Result<(), AppError> {
        let Some(row) = self.db.get(id)? else {
            return Ok(());
        };
        self.ytdlp.cancel(id);
        let mut engine = self.engine.current().await;
        // A paused torrent's file list lives only in aria2's session.
        if delete_files
            && engine.is_none()
            && row.gid.is_some()
            && row.stage_dir.is_none()
            && row.status != DownloadStatus::Complete
            && matches!(row.kind.as_str(), "torrent" | "magnet")
        {
            engine = self.ensure_engine().await.ok().map(|handle| handle.engine);
        }
        let mut files = Vec::new();
        if let (true, Some(gid), Some(engine)) = (delete_files, row.gid.as_deref(), &engine) {
            files = engine
                .rpc
                .get_files(gid)
                .await
                .unwrap_or_default()
                .iter()
                .filter_map(|file| file.get("path").and_then(Value::as_str).map(str::to_owned))
                .collect();
        }
        if let Some(engine) = &engine {
            let mut gids = Vec::new();
            if let Some(gid) = row.gid.as_deref() {
                gids.push(gid);
            }
            if let Some(gid) = row.session_gid.as_deref() {
                if !gids.contains(&gid) {
                    gids.push(gid);
                }
            }
            for gid in gids {
                let _ = engine.rpc.force_remove(gid).await;
                let _ = engine.rpc.remove_download_result(gid).await;
            }
        }
        let mut problems = Vec::new();
        if delete_files {
            if let Some(filename) = &row.filename {
                // `dir/filename` is this row's only when it was written in
                // place; a staged row's own file is still in staging.
                let home = row.stage_dir.is_none().then(|| Path::new(&row.dir));
                let stage = row.stage_dir.as_deref().map(Path::new);
                for dir in home.into_iter().chain(stage) {
                    files.push(dir.join(filename).to_string_lossy().into_owned());
                }
            }
            for file in files {
                if let Err(error) = delete_download_file(Path::new(&row.dir), Path::new(&file)) {
                    problems.push(error);
                }
            }
        }
        problems.extend(self.release_stage(&row, delete_files));
        self.db.delete(id)?;
        if let Some(package) = row.package_id {
            let _ = self.db.delete_package_if_empty(package);
        }
        self.events
            .emit("downloads:state", json!({"id": id, "status": "removed"}));
        self.arm_engine_if_idle().await;
        if problems.is_empty() {
            Ok(())
        } else {
            Err(AppError::classify(format!(
                "removed from the list, but: {}",
                problems.join("; ")
            )))
        }
    }

    /// Empty the row's private staging folder before the row goes away, on
    /// every removal path. With `delete_files` everything in it is deleted;
    /// otherwise nothing stays hidden: a finished file whose move into place
    /// failed is moved now, and partial files land next to the destination as
    /// `<name>.part`. Metalink files share one folder, which is left to the
    /// last row using it. Returns what could not be done.
    pub(super) fn release_stage(&self, row: &Download, delete_files: bool) -> Vec<String> {
        let Some(stage) = row.stage_dir.as_deref().map(Path::new) else {
            return Vec::new();
        };
        let dir = Path::new(&row.dir);
        let shared = self.db.list(None).unwrap_or_default().iter().any(|other| {
            other.id != row.id && other.stage_dir.as_deref().map(Path::new) == Some(stage)
        });
        let mut problems = Vec::new();
        if !shared {
            let finished = (row.status == DownloadStatus::Complete)
                .then_some(row.filename.as_deref())
                .flatten();
            let result = if delete_files {
                delete_stage(dir, stage)
            } else if let Some(name) = finished.filter(|name| stage.join(name).exists()) {
                minidl_core::files::commit_staged(stage, name, dir)
                    .map(|_| ())
                    .map_err(|error| {
                        format!("the finished file was kept in {}: {error}", stage.display())
                    })
            } else {
                Ok(())
            };
            let result = result.and_then(|()| {
                if delete_files {
                    Ok(())
                } else {
                    keep_partials(dir, stage)
                }
            });
            if let Err(error) = result {
                problems.push(error);
            }
        }
        minidl_core::files::remove_stage_dir(stage);
        problems
    }

    /// Hand a video row to yt-dlp (resume, retry, or a restart after the app
    /// stopped). yt-dlp continues its own partial files.
    pub(super) async fn start_ytdlp(
        self: &Arc<Self>,
        row: &Download,
        cause: &str,
    ) -> Result<(), AppError> {
        if !self.ytdlp.is_available() {
            return Err(AppError::classify("yt-dlp not available"));
        }
        let job = ingest::job_from_row(row);
        let options = row
            .media_opts
            .as_deref()
            .and_then(|value| serde_json::from_str(value).ok());
        let target = row.page_url.clone().unwrap_or_else(|| row.url.clone());
        ingest::validate_media_target(&self.db, &target)
            .await
            .map_err(AppError::classify)?;
        crate::ytdlp::validate_options(row.format_id.as_deref(), options.as_ref())
            .map_err(AppError::classify)?;
        self.db.transition(row.id, DownloadStatus::Active, cause)?;
        self.ytdlp
            .start(
                row.id,
                target,
                row.format_id.clone(),
                job.header_lines(),
                options,
            )
            .map_err(AppError::classify)
    }

    pub(super) async fn remove_status(&self, status: &str) -> Result<usize, AppError> {
        let rows = self.db.list(Some(status))?;
        let engine = self.engine.current().await;
        let mut count = 0;
        for row in rows {
            self.ytdlp.cancel(row.id);
            if let Some(engine) = &engine {
                let mut gids = Vec::new();
                if let Some(gid) = row.gid.as_deref() {
                    gids.push(gid);
                }
                if let Some(gid) = row.session_gid.as_deref() {
                    if !gids.contains(&gid) {
                        gids.push(gid);
                    }
                }
                for gid in gids {
                    let _ = engine.rpc.remove_download_result(gid).await;
                }
            }
            // Partials of a cleared row move next to their destination as
            // `.part` instead of staying hidden in `.minidl-staging/<id>`.
            let _ = self.release_stage(&row, false);
            if self.db.delete(row.id).is_ok() {
                count += 1;
                if let Some(package) = row.package_id {
                    let _ = self.db.delete_package_if_empty(package);
                }
            }
        }
        self.events
            .emit("downloads:state", json!({"removed": count}));
        self.arm_engine_if_idle().await;
        Ok(count)
    }

    pub(super) async fn schedule(&self, id: i64, start_at: Option<i64>) -> Result<(), AppError> {
        let row = self
            .db
            .get(id)?
            .ok_or_else(|| AppError::classify("download not found"))?;
        if let Some(start_at) = start_at {
            if row.status.is_running() {
                if row.is_ytdlp() {
                    self.ytdlp.cancel(id);
                } else if let (Some(gid), Some(engine)) =
                    (row.gid.as_deref(), self.engine.current().await)
                {
                    let _ = engine.rpc.pause(gid).await;
                }
            }
            self.db.set_start_at(id, Some(start_at))?;
            self.db
                .transition(id, DownloadStatus::Scheduled, "user: schedule")?;
            self.events
                .emit("downloads:state", json!({"id": id, "status": "scheduled"}));
        } else {
            self.db.set_start_at(id, None)?;
            if row.status == DownloadStatus::Scheduled {
                self.db
                    .transition(id, DownloadStatus::Paused, "user: unschedule")?;
            }
            self.events
                .emit("downloads:state", json!({"id": id, "status": "paused"}));
        }
        self.arm_engine_if_idle().await;
        Ok(())
    }
}
