//! Post-download work kept outside the renderer and off the idle hot path.

use std::path::{Path, PathBuf};

use minidl_core::db::Db;
use minidl_core::model::{Download, DownloadStatus};
use serde_json::Value;

pub fn basename(status: &Value) -> String {
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
    let path = Path::new(&row.dir).join(filename_path);
    path.exists().then_some(path)
}

fn safe_filename(filename: &str) -> Option<String> {
    let name = filename.rsplit(['/', '\\']).next().unwrap_or("").trim();
    (!name.is_empty() && name != "." && name != "..").then(|| name.to_owned())
}

fn collision_name(filename: &str, suffix: u32) -> String {
    match filename.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() && !extension.is_empty() => {
            format!("{stem} ({suffix}).{extension}")
        }
        _ => format!("{filename} ({suffix})"),
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
        output.set_permissions(metadata.permissions())?;
        let mut times = std::fs::FileTimes::new();
        if let Ok(accessed) = metadata.accessed() {
            times = times.set_accessed(accessed);
        }
        if let Ok(modified) = metadata.modified() {
            times = times.set_modified(modified);
        }
        output.set_times(times)?;
        output.sync_all()?;
        #[cfg(unix)]
        for name in xattr::list(source)? {
            if let Some(value) = xattr::get(source, &name)? {
                xattr::set(destination, &name, &value)?;
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

#[cfg(not(unix))]
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
        };

        assert_eq!(downloaded_path(&row).as_deref(), Some(file.as_path()));
        row.status = DownloadStatus::Paused;
        assert!(downloaded_path(&row).is_none());
        row.status = DownloadStatus::Complete;
        row.filename = Some("../finished.zip".into());
        assert!(downloaded_path(&row).is_none());
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
