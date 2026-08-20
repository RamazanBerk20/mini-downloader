use super::*;

impl Controller {
    pub(super) async fn add_media(
        &self,
        url: String,
        format_id: Option<String>,
        options: Option<MediaOptions>,
        package_id: Option<i64>,
    ) -> Result<DownloadView, AppError> {
        self.power_fired.store(false, Ordering::Relaxed);
        if !self.ytdlp.is_available() {
            return Err(AppError::classify("yt-dlp not available"));
        }
        let url = url.trim().to_owned();
        ingest::validate_media_target(&self.db, &url)
            .await
            .map_err(AppError::classify)?;
        crate::ytdlp::validate_options(format_id.as_deref(), options.as_ref())
            .map_err(AppError::classify)?;
        let inserted = self
            .db
            .insert_download_with_duplicate_policy(&NewDownload {
                url: url.clone(),
                dir: self.download_dir.to_string_lossy().into_owned(),
                kind: "video".into(),
                format_id: format_id.clone(),
                package_id,
                media_opts: options
                    .as_ref()
                    .and_then(|options| serde_json::to_string(options).ok()),
                ..Default::default()
            })?;
        let id = inserted.id();
        if matches!(inserted, DownloadInsertResult::Inserted(_)) {
            self.db.set_status(id, DownloadStatus::Active)?;
            self.ytdlp
                .start(id, url, format_id, Vec::new(), options)
                .map_err(AppError::classify)?;
            self.events
                .emit("downloads:state", json!({"id": id, "status": "active"}));
        }
        self.view(id)
    }

    pub(super) async fn details(&self, id: i64) -> Result<DownloadDetails, AppError> {
        let row = self
            .db
            .get(id)?
            .ok_or_else(|| AppError::classify("download not found"))?;
        let mut details = DownloadDetails {
            id,
            url: row.url.clone(),
            dir: row.dir.clone(),
            kind: row.kind.clone(),
            error_message: row.error_message.clone(),
            num_pieces: 0,
            piece_length: 0,
            verified_length: 0,
            files: stored_detail_file(&row).into_iter().collect(),
            peers: Vec::new(),
            live: false,
        };
        let (Some(gid), Some(engine)) = (row.gid.as_deref(), self.engine.current().await) else {
            return Ok(details);
        };
        let Ok(status) = engine
            .rpc
            .tell_status(gid, minidl_core::aria2::DETAIL_KEYS)
            .await
        else {
            return Ok(details);
        };
        details.live = true;
        details.num_pieces = value_i64(&status, "numPieces");
        details.piece_length = value_i64(&status, "pieceLength");
        details.verified_length = value_i64(&status, "verifiedLength");
        let live_files = status
            .get("files")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|file| DetailFile {
                index: value_i64(file, "index"),
                path: value_string(file, "path"),
                length: value_i64(file, "length"),
                completed_length: value_i64(file, "completedLength"),
                selected: value_string(file, "selected") != "false",
            })
            .collect::<Vec<_>>();
        if !live_files.is_empty() {
            details.files = live_files;
        }
        if matches!(row.kind.as_str(), "torrent" | "magnet") {
            details.peers = engine
                .rpc
                .get_peers(gid)
                .await
                .unwrap_or_default()
                .iter()
                .map(|peer| DetailPeer {
                    ip: value_string(peer, "ip"),
                    down_speed: value_i64(peer, "downloadSpeed"),
                    up_speed: value_i64(peer, "uploadSpeed"),
                    seeder: value_string(peer, "seeder") == "true",
                })
                .collect();
        }
        Ok(details)
    }

    pub(super) async fn set_torrent_files(
        self: &Arc<Self>,
        id: i64,
        mut indices: Vec<u32>,
    ) -> Result<(), AppError> {
        if indices.is_empty() {
            return Err(AppError::classify("select at least one file"));
        }
        if indices.len() > ingest::MAX_BATCH_ITEMS {
            return Err(AppError::classify("too many torrent files selected"));
        }
        if indices.contains(&0) {
            return Err(AppError::classify("invalid torrent file index"));
        }
        indices.sort_unstable();
        indices.dedup();
        let row = self
            .db
            .get(id)?
            .ok_or_else(|| AppError::classify("download not found"))?;
        if !matches!(row.kind.as_str(), "torrent" | "magnet") {
            return Err(AppError::classify(
                "file selection only applies to torrents",
            ));
        }
        let gid = row
            .gid
            .as_deref()
            .ok_or_else(|| AppError::classify("download has no engine handle yet"))?;
        let handle = self.ensure_engine().await?;
        let options = json!({"select-file": indices.iter().map(u32::to_string).collect::<Vec<_>>().join(",")});
        let result = if row.status == DownloadStatus::Active {
            let _ = handle.engine.rpc.pause(gid).await;
            let result = handle.engine.rpc.change_option(gid, options).await;
            let _ = handle.engine.rpc.unpause(gid).await;
            result
        } else {
            handle.engine.rpc.change_option(gid, options).await
        };
        self.arm_engine_if_idle().await;
        result?;
        Ok(())
    }
}
