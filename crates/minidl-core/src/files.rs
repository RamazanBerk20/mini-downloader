//! Untrusted filenames and the staging folder finished downloads are moved
//! out of. Every remote name is reduced by [`safe_out_name`] before it touches
//! the filesystem; every move into a user folder goes through
//! [`commit_staged`], which never replaces an existing file.

use std::io;
use std::path::{Path, PathBuf};

/// Hidden folder inside a destination directory that holds in-progress
/// downloads (one subfolder per download id). Same filesystem as the
/// destination, so the final move is a rename.
pub const STAGING_DIR: &str = ".minidl-staging";

/// Invisible formatting characters (Unicode category Cf) that can disguise a
/// name, e.g. `invoice\u{202E}fdp.exe` displays as `invoiceexe.pdf`.
fn is_format_char(c: char) -> bool {
    matches!(c,
        '\u{00AD}' | '\u{061C}' | '\u{180E}'
        | '\u{200B}'..='\u{200F}'
        | '\u{202A}'..='\u{202E}'
        | '\u{2060}'..='\u{2064}'
        | '\u{2066}'..='\u{206F}'
        | '\u{FEFF}'
        | '\u{FFF9}'..='\u{FFFB}')
}

/// DOS device names Windows opens instead of a file, with or without an
/// extension (`CON`, `nul.txt`, `COM1.zip`, `LPT¹`).
fn is_reserved_device_stem(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or("").trim_end();
    let upper = stem.to_ascii_uppercase();
    match upper.as_str() {
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" => true,
        _ => {
            let mut chars = stem.chars();
            let prefix: String = chars.by_ref().take(3).collect();
            let rest: Vec<char> = chars.collect();
            matches!(prefix.to_ascii_uppercase().as_str(), "COM" | "LPT")
                && rest.len() == 1
                && matches!(rest[0], '0'..='9' | '¹' | '²' | '³')
        }
    }
}

/// Reduce an untrusted filename (from `Content-Disposition`, a URL, a torrent)
/// to one safe path component. The same rules apply on every platform so a
/// name means the same thing everywhere:
/// - only the last `/` or `\` component is kept (no traversal);
/// - control and invisible formatting (bidi) characters are dropped;
/// - `: * ? " < > |` become `_` (`:` would open an NTFS alternate data stream);
/// - trailing dots and spaces are trimmed (Windows drops them silently, which
///   would change the extension category rules saw);
/// - DOS device names get a `_` prefix.
///
/// `None` when nothing usable is left.
pub fn safe_out_name(name: &str) -> Option<String> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control() && !is_format_char(*c))
        .map(|c| match c {
            ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c => c,
        })
        .collect();
    let cleaned = cleaned.trim_start().trim_end_matches(['.', ' ']);
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        return None;
    }
    Some(if is_reserved_device_stem(cleaned) {
        format!("_{cleaned}")
    } else {
        cleaned.to_owned()
    })
}

/// `name (n).ext` — the name a collision is moved to.
pub fn collision_name(filename: &str, suffix: u32) -> String {
    match filename.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() && !extension.is_empty() => {
            format!("{stem} ({suffix}).{extension}")
        }
        _ => format!("{filename} ({suffix})"),
    }
}

/// The private folder download `id` is written into before it lands in `dir`.
pub fn stage_dir(dir: &Path, id: i64) -> PathBuf {
    dir.join(STAGING_DIR).join(id.to_string())
}

/// Create an empty [`stage_dir`] for a new download (owner-only on Unix).
/// Anything already there belongs to no live download and is never trusted:
/// a leftover partial or `.aria2` control file would make aria2 resume a
/// different file.
pub fn create_stage_dir(dir: &Path, id: i64) -> io::Result<PathBuf> {
    let stage = stage_dir(dir, id);
    match std::fs::symlink_metadata(&stage) {
        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(&stage)?,
        Ok(_) => std::fs::remove_file(&stage)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    ensure_stage_dir(dir, id)
}

/// [`stage_dir`] of an existing download, created if missing and keeping its
/// contents (a resumed download continues its own partial files).
pub fn ensure_stage_dir(dir: &Path, id: i64) -> io::Result<PathBuf> {
    let stage = stage_dir(dir, id);
    std::fs::create_dir_all(&stage)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for path in [stage.as_path(), stage.parent().unwrap_or(&stage)] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    Ok(stage)
}

/// Remove a staging folder once it is empty, then the shared parent if that is
/// empty too. Leftovers (a partial file, a control file) keep both.
pub fn remove_stage_dir(stage: &Path) {
    if std::fs::remove_dir(stage).is_ok() {
        if let Some(parent) = stage
            .parent()
            .filter(|parent| parent.file_name() == Some(STAGING_DIR.as_ref()))
        {
            let _ = std::fs::remove_dir(parent);
        }
    }
}

/// Move `from` to `to`, failing with `AlreadyExists` instead of replacing.
#[cfg(target_os = "linux")]
pub fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let source = CString::new(from.as_os_str().as_bytes())?;
    let target = CString::new(to.as_os_str().as_bytes())?;
    // SAFETY: both paths are valid NUL-terminated strings for the call.
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            target.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        // The filesystem cannot do RENAME_NOREPLACE (some FUSE/network mounts).
        Some(libc::EINVAL | libc::ENOSYS | libc::EOPNOTSUPP) => link_then_unlink(from, to),
        _ => Err(error),
    }
}

/// Move `from` to `to`, failing with `AlreadyExists` instead of replacing.
#[cfg(windows)]
pub fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
    }
    let wide = |path: &Path| {
        path.as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<u16>>()
    };
    let (source, target) = (wide(from), wide(to));
    // Flags 0: no MOVEFILE_REPLACE_EXISTING, so an existing target fails with
    // ERROR_ALREADY_EXISTS (io::ErrorKind::AlreadyExists).
    // SAFETY: both buffers are NUL-terminated UTF-16 paths alive for the call.
    if unsafe { MoveFileExW(source.as_ptr(), target.as_ptr(), 0) } != 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Move `from` to `to`, failing with `AlreadyExists` instead of replacing.
#[cfg(not(any(target_os = "linux", windows)))]
pub fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    link_then_unlink(from, to)
}

// shortcut: a hard link is atomic and no-replace but works for files only, so
// a staged torrent folder on such a filesystem stays in staging with an error;
// upgrade if those filesystems matter.
#[cfg(not(windows))]
fn link_then_unlink(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::hard_link(from, to)?;
    std::fs::remove_file(from)
}

/// Move the finished item `name` out of `stage` into `dir` under a safe name,
/// never replacing an existing file: a collision takes `name (1).ext`, … The
/// staging folder is removed when it ends up empty. Returns the final path and
/// name.
pub fn commit_staged(stage: &Path, name: &str, dir: &Path) -> io::Result<(PathBuf, String)> {
    if Path::new(name).file_name() != Some(name.as_ref()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "staged item is not a single path component",
        ));
    }
    commit_staged_path(stage, Path::new(name), dir, true)
}

/// [`commit_staged`] for an item at `relative` inside `stage`, which may sit
/// in subfolders (a metalink file named `sub/file.iso`). The subfolders are
/// recreated inside `dir`, each name reduced by [`safe_out_name`], and the
/// result is refused if it would resolve outside `dir` (a symlinked
/// subfolder). With `allow_rename` false a taken name is an `AlreadyExists`
/// error instead of `name (1).ext`. Returns the item's final path and name.
pub fn commit_staged_path(
    stage: &Path,
    relative: &Path,
    dir: &Path,
    allow_rename: bool,
) -> io::Result<(PathBuf, String)> {
    let invalid = |what: &str| io::Error::new(io::ErrorKind::InvalidInput, what.to_owned());
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            std::path::Component::Normal(part) => parts.push(
                part.to_str()
                    .ok_or_else(|| invalid("staged name is not UTF-8"))?
                    .to_owned(),
            ),
            _ => return Err(invalid("staged item is not a relative path inside staging")),
        }
    }
    let name = parts.pop().ok_or_else(|| invalid("empty staged name"))?;
    let wanted = safe_out_name(&name).ok_or_else(|| invalid("unusable download filename"))?;
    let mut source_dir = stage.to_path_buf();
    let mut target = dir.to_path_buf();
    for part in &parts {
        source_dir.push(part);
        target.push(safe_out_name(part).ok_or_else(|| invalid("unusable folder name"))?);
    }
    if !parts.is_empty() {
        std::fs::create_dir_all(&target)?;
        if !target.canonicalize()?.starts_with(dir.canonicalize()?) {
            return Err(invalid(
                "destination folder resolves outside the download folder",
            ));
        }
    }
    let source = source_dir.join(&name);
    let limit = if allow_rename { 10_000u32 } else { 1 };
    for suffix in 0..limit {
        let candidate = if suffix == 0 {
            wanted.clone()
        } else {
            collision_name(&wanted, suffix)
        };
        let destination = target.join(&candidate);
        match rename_noreplace(&source, &destination) {
            Ok(()) => {
                // Drop the now-empty subfolders, then the staging folder.
                let mut empty = source_dir;
                while empty != stage && std::fs::remove_dir(&empty).is_ok() {
                    empty.pop();
                }
                remove_stage_dir(stage);
                return Ok((destination, candidate));
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("{} already exists", target.join(&wanted).display()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_reduced_to_one_safe_component() {
        let reduce = |name| safe_out_name(name);
        assert_eq!(
            reduce("../../.config/autostart/x.desktop").as_deref(),
            Some("x.desktop")
        );
        assert_eq!(reduce("..\\..\\evil.exe").as_deref(), Some("evil.exe"));
        assert_eq!(reduce("../../"), None);
        assert_eq!(reduce(" .. "), None);
        assert_eq!(
            reduce("notes.txt:payload").as_deref(),
            Some("notes.txt_payload")
        );
        assert_eq!(reduce("a*b?c\"d<e>f|g").as_deref(), Some("a_b_c_d_e_f_g"));
        assert_eq!(reduce("evil.exe.").as_deref(), Some("evil.exe"));
        assert_eq!(reduce("evil.exe . . ").as_deref(), Some("evil.exe"));
        assert_eq!(
            reduce("invoice\u{202E}fdp.exe").as_deref(),
            Some("invoicefdp.exe")
        );
        assert_eq!(
            reduce("zero\u{200B}width\u{FEFF}.pdf").as_deref(),
            Some("zerowidth.pdf")
        );
        assert_eq!(reduce("tab\there.txt").as_deref(), Some("tabhere.txt"));
        assert_eq!(
            reduce("Bob's report.pdf").as_deref(),
            Some("Bob's report.pdf")
        );
        assert_eq!(
            reduce("İndirilenler.zip").as_deref(),
            Some("İndirilenler.zip")
        );
    }

    #[test]
    fn dos_device_names_are_defused() {
        for name in [
            "CON", "nul", "Aux.txt", "COM1.zip", "lpt9", "COM¹", "con .txt",
        ] {
            assert!(
                safe_out_name(name).unwrap().starts_with('_'),
                "{name} kept its device name"
            );
        }
        for name in ["CONTACTS.txt", "COM10", "nullable.rs", "com.zip", "LPT"] {
            assert_eq!(safe_out_name(name).as_deref(), Some(name));
        }
    }

    #[test]
    fn commit_never_replaces_and_cleans_up_staging() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path();
        std::fs::write(dir.join("report.pdf"), b"user's own file").unwrap();
        let stage = create_stage_dir(dir, 7).unwrap();
        std::fs::write(stage.join("report.pdf"), b"downloaded").unwrap();

        let (path, name) = commit_staged(&stage, "report.pdf", dir).unwrap();
        assert_eq!(name, "report (1).pdf");
        assert_eq!(path, dir.join("report (1).pdf"));
        assert_eq!(
            std::fs::read(dir.join("report.pdf")).unwrap(),
            b"user's own file"
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"downloaded");
        assert!(!dir.join(STAGING_DIR).exists());

        // A folder (multi-file torrent root) moves the same way.
        let stage = create_stage_dir(dir, 8).unwrap();
        std::fs::create_dir(stage.join("Album")).unwrap();
        std::fs::write(stage.join("Album/01.flac"), b"a").unwrap();
        let (path, name) = commit_staged(&stage, "Album", dir).unwrap();
        assert_eq!(name, "Album");
        assert!(path.join("01.flac").is_file());
    }

    #[test]
    fn staged_subfolders_are_recreated_inside_the_destination_only() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("dl");
        std::fs::create_dir_all(&dir).unwrap();
        let stage = create_stage_dir(&dir, 1).unwrap();
        std::fs::create_dir_all(stage.join("disc 1")).unwrap();
        std::fs::write(stage.join("disc 1/track.flac"), b"a").unwrap();
        let (path, name) =
            commit_staged_path(&stage, Path::new("disc 1/track.flac"), &dir, true).unwrap();
        assert_eq!(path, dir.join("disc 1/track.flac"));
        assert_eq!(name, "track.flac");
        assert!(!dir.join(STAGING_DIR).exists());

        let stage = create_stage_dir(&dir, 2).unwrap();
        for bad in ["../x", "/etc/x", "a/../../x"] {
            assert!(
                commit_staged_path(&stage, Path::new(bad), &dir, true).is_err(),
                "{bad}"
            );
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.path(), dir.join("out")).unwrap();
            std::fs::create_dir_all(stage.join("out")).unwrap();
            std::fs::write(stage.join("out/x"), b"a").unwrap();
            assert!(commit_staged_path(&stage, Path::new("out/x"), &dir, true).is_err());
            assert!(!root.path().join("x").exists());
        }
    }

    #[test]
    fn without_renaming_a_taken_name_is_refused() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a.bin"), b"old").unwrap();
        let stage = create_stage_dir(root.path(), 1).unwrap();
        std::fs::write(stage.join("a.bin"), b"new").unwrap();
        let error = commit_staged_path(&stage, Path::new("a.bin"), root.path(), false).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(root.path().join("a.bin")).unwrap(), b"old");
        assert!(stage.join("a.bin").exists());
    }

    #[test]
    fn a_new_stage_never_inherits_leftovers() {
        let root = tempfile::tempdir().unwrap();
        let stale = stage_dir(root.path(), 3);
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::write(stale.join("file.iso"), b"someone else's bytes").unwrap();
        std::fs::write(stale.join("file.iso.aria2"), b"control").unwrap();
        let stage = create_stage_dir(root.path(), 3).unwrap();
        assert_eq!(std::fs::read_dir(stage).unwrap().count(), 0);
    }

    #[test]
    fn rename_noreplace_refuses_an_existing_target() {
        let root = tempfile::tempdir().unwrap();
        let (from, to) = (root.path().join("a"), root.path().join("b"));
        std::fs::write(&from, b"new").unwrap();
        std::fs::write(&to, b"old").unwrap();
        let error = rename_noreplace(&from, &to).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&to).unwrap(), b"old");
        assert!(from.exists());
    }
}
