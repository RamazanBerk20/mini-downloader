//! Post-download work kept outside the renderer and off the idle hot path.

use std::path::{Path, PathBuf};

use minidl_core::db::Db;
use minidl_core::model::{Download, DownloadStatus};
use serde_json::Value;

/// Name of the finished item directly under its download directory. A
/// torrent reports its own name: the file for a single-file torrent, the root
/// folder for a multi-file one (whose first file sits deeper than `dir`).
pub fn basename(status: &Value) -> String {
    if let Some(name) = status
        .pointer("/bittorrent/info/name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
    {
        return name.to_owned();
    }
    status
        .get("files")
        .and_then(Value::as_array)
        .and_then(|files| files.first())
        .and_then(|file| file.get("path"))
        .and_then(Value::as_str)
        .and_then(|path| Path::new(path).file_name())
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_owned()
}

pub fn first_followed_by(status: &Value) -> Option<String> {
    status
        .get("followedBy")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(Value::as_str)
        .filter(|gid| !gid.is_empty())
        .map(str::to_owned)
}

/// Resolve the completed item's persisted location without allowing a stored
/// filename to escape its download directory.
pub fn downloaded_path(row: &Download) -> Option<PathBuf> {
    if row.status != DownloadStatus::Complete {
        return None;
    }
    let filename = row.filename.as_deref()?.trim();
    let filename_path = Path::new(filename);
    if filename_path.file_name()? != filename_path.as_os_str() {
        return None;
    }
    // A finished file whose move into place failed is still in staging.
    let home = row.stage_dir.as_deref().unwrap_or(&row.dir);
    let path = Path::new(home).join(filename_path);
    path.exists().then_some(path)
}

/// Where a staged item sits inside its staging folder, from aria2's reported
/// path (a metalink file may be `sub/file`). Falls back to the bare name.
fn staged_relative(stage: &str, status: &Value) -> Option<PathBuf> {
    let path = status
        .get("files")
        .and_then(Value::as_array)
        .and_then(|files| files.first())
        .and_then(|file| file.get("path"))
        .and_then(Value::as_str)?;
    Path::new(path)
        .strip_prefix(stage)
        .ok()
        .filter(|relative| !relative.as_os_str().is_empty())
        .map(Path::to_path_buf)
}

fn duplicate_prevention(db: &Db) -> bool {
    db.get_setting("prevent_duplicate_downloads")
        .ok()
        .flatten()
        .as_deref()
        != Some("false")
}

/// Outcome of [`complete`].
#[derive(Debug, PartialEq, Eq)]
pub enum Completion {
    /// A metadata job (magnet, remote torrent) finished; the row now follows
    /// its content job with this GID.
    Followed(String),
    /// The row became complete now: final folder, name, and why the file
    /// could not be moved into place, if it could not.
    Finished {
        directory: String,
        filename: String,
        problem: Option<String>,
    },
    /// Duplicate prevention is on and the name is taken: the row failed.
    Refused(String),
    /// Another handler (WebSocket event vs poller vs reconcile) got here first.
    AlreadyDone,
}

/// The one path to `complete` for an aria2 row, used by the live handler and
/// by reconcile: gate on the transition, move a staged file into place, sort
/// it into its category, record its origin.
pub fn complete(db: &Db, row: &Download, status: &Value, cause: &str) -> Completion {
    if let Some(child) = first_followed_by(status) {
        let _ = db.set_gid(row.id, &child);
        if let Some(info_hash) = status
            .get("infoHash")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            let _ = db.set_info_hash(row.id, info_hash);
        }
        let _ = db.note(
            row.id,
            "aria2: metadata done, following content job",
            Some(&child),
        );
        return Completion::Followed(child);
    }
    let reported = basename(status);
    let filename = if reported.is_empty() {
        row.filename.clone().unwrap_or_default()
    } else {
        reported
    };
    let relative = row
        .stage_dir
        .as_deref()
        .and_then(|stage| staged_relative(stage, status))
        .unwrap_or_else(|| PathBuf::from(&filename));
    let dedupe = duplicate_prevention(db);
    if row.stage_dir.is_some() && dedupe {
        let taken = relative
            .iter()
            .map(|part| safe_filename(&part.to_string_lossy()))
            .collect::<Option<PathBuf>>()
            .map(|reduced| Path::new(&row.dir).join(reduced))
            .filter(|target| target.exists());
        if let Some(target) = taken {
            let message = format!(
                "{} already exists and duplicate prevention is on; the download was kept in its staging folder",
                target.display()
            );
            return if db
                .fail(
                    row.id,
                    None,
                    Some(&message),
                    "commit: name taken, duplicate prevention",
                )
                .unwrap_or(false)
            {
                Completion::Refused(message)
            } else {
                Completion::AlreadyDone
            };
        }
    }
    if !db
        .transition(row.id, DownloadStatus::Complete, cause)
        .unwrap_or(false)
    {
        return Completion::AlreadyDone;
    }
    let (directory, filename, problem) = match commit(db, row, &relative, !dedupe) {
        Ok((directory, filename)) => {
            let mut placed = row.clone();
            placed.dir = directory;
            let (directory, filename) = organize(db, &placed, &filename);
            (directory, filename, None)
        }
        Err((directory, message)) => (directory, filename, Some(message)),
    };
    if !filename.is_empty() && row.filename.as_deref() != Some(filename.as_str()) {
        let _ = db.set_filename(row.id, &filename);
    }
    let path = Path::new(&directory).join(&filename);
    set_origin(&path, &row.url, row.referrer.as_deref());
    Completion::Finished {
        directory,
        filename,
        problem,
    }
}

use minidl_core::files::{collision_name, safe_out_name as safe_filename};

/// Move a staged download (`row.stage_dir`) into `row.dir` without replacing
/// anything and return its final folder and name. Rows without a staging
/// folder (yt-dlp, BitTorrent, rows from before staging) already sit in
/// `row.dir`. On failure the file stays in staging, the row keeps its
/// destination and its `stage_dir` (where [`downloaded_path`] then finds the
/// file), and the error names the staging folder and the reason.
fn commit(
    db: &Db,
    row: &Download,
    relative: &Path,
    allow_rename: bool,
) -> Result<(String, String), (String, String)> {
    let Some(stage) = row.stage_dir.as_deref() else {
        let name = relative.to_string_lossy().into_owned();
        return Ok((row.dir.clone(), name));
    };
    match minidl_core::files::commit_staged_path(
        Path::new(stage),
        relative,
        Path::new(&row.dir),
        allow_rename,
    ) {
        Ok((path, name)) => {
            let directory = path.parent().map_or_else(
                || row.dir.clone(),
                |parent| parent.to_string_lossy().into_owned(),
            );
            let _ = db.set_file_location(row.id, &name, &directory, row.category_id);
            let _ = db.set_stage_dir(row.id, None);
            if relative
                .file_name()
                .is_some_and(|wanted| wanted != name.as_str())
            {
                let _ = db.note(
                    row.id,
                    "commit: name taken, saved under a new name",
                    Some(&name),
                );
            }
            Ok((directory, name))
        }
        Err(error) => {
            let message = format!("could not move the finished file into place: {error}");
            if let Some(name) = relative.file_name() {
                let _ = db.set_filename(row.id, &name.to_string_lossy());
            }
            let _ = db.note(
                row.id,
                "commit failed, file left in staging",
                Some(&message),
            );
            Err((stage.to_owned(), message))
        }
    }
}

fn copy_new(source: &Path, destination: &Path) -> std::io::Result<()> {
    let metadata = std::fs::metadata(source)?;
    let mut input = std::fs::File::open(source)?;
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    let copied = (|| {
        std::io::copy(&mut input, &mut output)?;
        output.sync_all()?;
        // Metadata is best effort: exFAT/FAT/NTFS targets reject modes, times
        // or xattrs, and that must not undo a byte-for-byte good copy.
        let _ = output.set_permissions(metadata.permissions());
        let mut times = std::fs::FileTimes::new();
        if let Ok(accessed) = metadata.accessed() {
            times = times.set_accessed(accessed);
        }
        if let Ok(modified) = metadata.modified() {
            times = times.set_modified(modified);
        }
        let _ = output.set_times(times);
        #[cfg(unix)]
        for name in xattr::list(source).into_iter().flatten() {
            if let Ok(Some(value)) = xattr::get(source, &name) {
                let _ = xattr::set(destination, &name, &value);
            }
        }
        Ok(())
    })();
    drop(output);
    if copied.is_err() {
        let _ = std::fs::remove_file(destination);
    }
    copied
}

fn move_unique(source: &Path, target: &Path, filename: &str) -> std::io::Result<(PathBuf, String)> {
    for suffix in 0..=u32::MAX {
        let candidate = if suffix == 0 {
            filename.to_owned()
        } else {
            collision_name(filename, suffix)
        };
        let destination = target.join(&candidate);
        // Same-filesystem hard links provide an atomic no-replace move while
        // preserving permissions, timestamps and extended attributes exactly.
        // Cross-filesystem moves fall back to an exclusive metadata-preserving
        // copy.
        let moved = match std::fs::hard_link(source, &destination) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                continue;
            }
            Err(_) => copy_new(source, &destination),
        };
        match moved {
            Ok(()) => {
                if let Err(error) = std::fs::remove_file(source) {
                    let _ = std::fs::remove_file(&destination);
                    return Err(error);
                }
                return Ok((destination, candidate));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "no free destination filename",
    ))
}

fn same_directory(left: &Path, right: &Path) -> bool {
    left == right
        || matches!((left.canonicalize(), right.canonicalize()), (Ok(left), Ok(right)) if left == right)
}

/// Move a completed single-file HTTP download into its matching category.
/// The exclusive destination create is the collision/race guard.
pub fn organize(db: &Db, row: &Download, filename: &str) -> (String, String) {
    let Some(filename) = safe_filename(filename) else {
        return (row.dir.clone(), filename.to_owned());
    };
    let enabled = db
        .get_setting("auto_organize")
        .ok()
        .flatten()
        .map(|value| value != "false")
        .unwrap_or(true);
    if !enabled || row.kind != "http" {
        return (row.dir.clone(), filename);
    }

    let host = minidl_core::grabber::host_of(&row.url);
    let categories = db.list_categories().unwrap_or_default();
    let Some(category) = minidl_core::categories::classify(
        &filename,
        row.mime.as_deref(),
        (!host.is_empty()).then_some(host.as_str()),
        &categories,
    ) else {
        return (row.dir.clone(), filename);
    };
    let target = minidl_core::categories::expand(&category.dir);
    let source = Path::new(&row.dir).join(&filename);
    if !source.exists()
        || std::fs::create_dir_all(&target).is_err()
        || same_directory(&target, Path::new(&row.dir))
    {
        return (row.dir.clone(), filename);
    }
    match move_unique(&source, &target, &filename) {
        Ok((_path, moved_name)) => {
            let mut sidecar = source.into_os_string();
            sidecar.push(".aria2");
            let _ = std::fs::remove_file(PathBuf::from(sidecar));
            let target = target.to_string_lossy().into_owned();
            let _ = db.set_file_location(row.id, &moved_name, &target, Some(category.id));
            (target, moved_name)
        }
        Err(_) => (row.dir.clone(), filename),
    }
}

#[cfg(unix)]
pub fn set_origin(path: &Path, url: &str, referrer: Option<&str>) {
    let _ = xattr::set(path, "user.xdg.origin.url", url.as_bytes());
    if let Some(referrer) = referrer {
        let _ = xattr::set(path, "user.xdg.referrer.url", referrer.as_bytes());
    }
}

/// Mark-of-the-Web body, as browsers write it. Without it Windows treats a
/// downloaded file as locally created: no SmartScreen, no Office Protected View.
#[cfg(any(windows, test))]
fn zone_identifier(url: &str, referrer: Option<&str>) -> String {
    // The stream is an INI file; a line break in a value would forge a key.
    let clean = |value: &str| value.replace(['\r', '\n'], "");
    let mut body = String::from("[ZoneTransfer]\r\nZoneId=3\r\n");
    if let Some(referrer) = referrer {
        body.push_str(&format!("ReferrerUrl={}\r\n", clean(referrer)));
    }
    body.push_str(&format!("HostUrl={}\r\n", clean(url)));
    body
}

#[cfg(windows)]
pub fn set_origin(path: &Path, url: &str, referrer: Option<&str>) {
    let mut stream = path.as_os_str().to_owned();
    stream.push(":Zone.Identifier");
    let _ = std::fs::write(stream, zone_identifier(url, referrer));
}

#[cfg(not(any(unix, windows)))]
pub fn set_origin(_path: &Path, _url: &str, _referrer: Option<&str>) {}

#[cfg(test)]
mod tests {
    use super::*;
    use minidl_core::model::NewDownload;

    #[test]
    fn downloaded_path_accepts_only_existing_completed_items() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("finished.zip");
        std::fs::write(&file, b"done").unwrap();
        let mut row = Download {
            id: 1,
            gid: None,
            session_gid: None,
            url: "https://example.invalid/finished.zip".into(),
            filename: Some("finished.zip".into()),
            dir: root.path().to_string_lossy().into_owned(),
            status: DownloadStatus::Complete,
            kind: "http".into(),
            total_bytes: 4,
            completed_bytes: 4,
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
            finished_at: Some(0),
            user_agent: None,
            cookie: None,
            extra_headers: None,
            page_url: None,
            format_id: None,
            speed_limit: None,
            package_id: None,
            mime: None,
            checksum: None,
            start_at: None,
            media_opts: None,
            stage_dir: None,
        };

        assert_eq!(downloaded_path(&row).as_deref(), Some(file.as_path()));
        row.status = DownloadStatus::Paused;
        assert!(downloaded_path(&row).is_none());
        row.status = DownloadStatus::Complete;
        row.filename = Some("../finished.zip".into());
        assert!(downloaded_path(&row).is_none());
    }

    #[test]
    fn torrents_are_named_by_their_root_not_their_first_file() {
        let multi = serde_json::json!({
            "dir": "/dl",
            "files": [{"path": "/dl/Album/01 - intro.flac"}],
            "bittorrent": {"info": {"name": "Album"}}
        });
        assert_eq!(basename(&multi), "Album");
        let http = serde_json::json!({"files": [{"path": "/dl/file.zip"}]});
        assert_eq!(basename(&http), "file.zip");
    }

    #[test]
    fn zone_identifier_marks_the_internet_zone_without_line_injection() {
        assert_eq!(
            zone_identifier("https://a/b\r\nZoneId=0", Some("https://ref/")),
            "[ZoneTransfer]\r\nZoneId=3\r\nReferrerUrl=https://ref/\r\nHostUrl=https://a/bZoneId=0\r\n"
        );
    }

    #[test]
    fn organize_never_overwrites_a_collision() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let target = root.path().join("target");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(source.join("report.bin"), b"new").unwrap();
        std::fs::write(target.join("report.bin"), b"old").unwrap();

        let db = Db::open_in_memory().unwrap();
        db.upsert_category("Binary", &target.to_string_lossy(), r#"["bin"]"#, -1)
            .unwrap();
        let id = db
            .insert_download(&NewDownload {
                url: "https://example.invalid/report.bin".into(),
                filename: Some("report.bin".into()),
                dir: source.to_string_lossy().into_owned(),
                kind: "http".into(),
                ..Default::default()
            })
            .unwrap();
        let row = db.get(id).unwrap().unwrap();

        let (_, filename) = organize(&db, &row, "report.bin");
        assert_eq!(filename, "report (1).bin");
        assert_eq!(std::fs::read(target.join("report.bin")).unwrap(), b"old");
        assert_eq!(
            std::fs::read(target.join("report (1).bin")).unwrap(),
            b"new"
        );
    }

    #[cfg(unix)]
    #[test]
    fn move_unique_preserves_mode_and_extended_attributes() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let root = tempfile::tempdir().unwrap();
        let source_dir = root.path().join("source");
        let target = root.path().join("target");
        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        let source = source_dir.join("artifact.bin");
        std::fs::write(&source, b"content").unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o640)).unwrap();
        xattr::set(&source, "user.minidl-test", b"preserved").unwrap();

        let (destination, _) = move_unique(&source, &target, "artifact.bin").unwrap();
        assert!(!source.exists());
        assert_eq!(
            std::fs::metadata(&destination).unwrap().mode() & 0o777,
            0o640
        );
        assert_eq!(
            xattr::get(&destination, "user.minidl-test").unwrap(),
            Some(b"preserved".to_vec())
        );
    }
}
