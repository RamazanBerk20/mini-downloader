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
            self.db.set_status(id, DownloadStatus::Paused)?;
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
            if !self.ytdlp.is_available() {
                return Err(AppError::classify("yt-dlp not available"));
            }
            let job = ingest::job_from_row(&row);
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
            self.db.set_status(id, DownloadStatus::Active)?;
            self.ytdlp
                .start(
                    id,
                    target,
                    row.format_id.clone(),
                    job.header_lines(),
                    options,
                )
                .map_err(AppError::classify)?;
        } else {
            let handle = self.ensure_engine().await?;
            if let Some(gid) = row.gid.as_deref() {
                if handle.engine.rpc.unpause(gid).await.is_err() {
                    let defaults = self
                        .defaults
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone();
                    ingest::reissue(&handle.engine, &self.db, defaults, &row)
                        .await
                        .map_err(AppError::classify)?;
                }
            } else {
                let defaults = self
                    .defaults
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                ingest::reissue(&handle.engine, &self.db, defaults, &row)
                    .await
                    .map_err(AppError::classify)?;
            }
            self.db.set_status(id, DownloadStatus::Active)?;
        }
        if row.start_at.is_some() {
            let _ = self.db.set_start_at(id, None);
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
            if !self.ytdlp.is_available() {
                return Err(AppError::classify("yt-dlp not available"));
            }
            let job = ingest::job_from_row(&row);
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
            self.db.set_status(id, DownloadStatus::Active)?;
            self.ytdlp
                .start(
                    id,
                    target,
                    row.format_id.clone(),
                    job.header_lines(),
                    options,
                )
                .map_err(AppError::classify)?;
        } else {
            let handle = self.ensure_engine().await?;
            let defaults = self
                .defaults
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            ingest::reissue(&handle.engine, &self.db, defaults, &row)
                .await
                .map_err(AppError::classify)?;
            self.db.set_status(id, DownloadStatus::Active)?;
        }
        self.events
            .emit("downloads:state", json!({"id": id, "status": "active"}));
        Ok(())
    }

    pub(super) async fn remove(&self, id: i64, delete_files: bool) -> Result<(), AppError> {
        let Some(row) = self.db.get(id)? else {
            return Ok(());
        };
        self.ytdlp.cancel(id);
        let mut files = Vec::new();
        if let (Some(gid), Some(engine)) = (row.gid.as_deref(), self.engine.current().await) {
            if delete_files {
                files = engine
                    .rpc
                    .get_files(gid)
                    .await
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|file| file.get("path").and_then(Value::as_str).map(str::to_owned))
                    .collect();
            }
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
        if delete_files {
            if let Some(filename) = &row.filename {
                files.push(
                    Path::new(&row.dir)
                        .join(filename)
                        .to_string_lossy()
                        .into_owned(),
                );
            }
            for file in files {
                if let Some(target) = deletion_target(Path::new(&row.dir), Path::new(&file)) {
                    let _ = std::fs::remove_file(&target);
                    let mut sidecar = target.into_os_string();
                    sidecar.push(".aria2");
                    let _ = std::fs::remove_file(PathBuf::from(sidecar));
                }
            }
        }
        self.db.delete(id)?;
        if let Some(package) = row.package_id {
            let _ = self.db.delete_package_if_empty(package);
        }
        self.events
            .emit("downloads:state", json!({"id": id, "status": "removed"}));
        self.arm_engine_if_idle().await;
        Ok(())
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
            self.db.set_status(id, DownloadStatus::Scheduled)?;
            self.events
                .emit("downloads:state", json!({"id": id, "status": "scheduled"}));
        } else {
            self.db.set_start_at(id, None)?;
            if row.status == DownloadStatus::Scheduled {
                self.db.set_status(id, DownloadStatus::Paused)?;
            }
            self.events
                .emit("downloads:state", json!({"id": id, "status": "paused"}));
        }
        self.arm_engine_if_idle().await;
        Ok(())
    }
}
