//! Cross-platform application paths via the `dirs` crate. On Linux this
//! resolves *localized* XDG user directories (Downloads may be "İndirilenler"
//! on a Turkish system — `dirs` parses `~/.config/user-dirs.dirs`); on Windows
//! it resolves the Known Folder equivalents (`%APPDATA%`, the user's real
//! Downloads folder).

use std::path::PathBuf;

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"))
}

/// Per-user data dir + our folder: `~/.local/share/minidownloader` on Linux,
/// `%APPDATA%\minidownloader` on Windows. Holds the DB, sanitized aria2
/// session state, logs.
/// Must stay in sync with `minidl_ipc::data_dir()` (the native host reads the
/// app-exec-path file from there).
pub fn data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| home().join(".local/share"))
        .join("minidownloader")
}

/// Per-user config dir + our folder: `~/.config/minidownloader` on Linux,
/// `%APPDATA%\minidownloader` on Windows (same as data there — fine, distinct
/// filenames).
pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| home().join(".config"))
        .join("minidownloader")
}

/// A per-user writable bin dir for tools that self-update (yt-dlp).
pub fn bin_dir() -> PathBuf {
    data_dir().join("bin")
}

/// Resolve an external tool (aria2c, yt-dlp, ffmpeg): prefer a bundled sidecar
/// next to the app executable, else fall back to `PATH`. The single source of
/// truth for "next to exe, else PATH" (previously duplicated per crate).
pub fn resolve_tool(name: &str) -> Option<PathBuf> {
    let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let cand = dir.join(&file);
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).map(|d| d.join(&file)).find(|c| c.is_file())
    })
}

/// Map a standard `~/Folder[/sub]` marker to the *localized* platform directory
/// when the leading folder is a well-known user dir; otherwise a plain `~`
/// expansion. Used by category target dirs, which are stored with `~/` markers
/// so they survive locale and OS changes.
pub fn resolve_home_path(dir: &str) -> PathBuf {
    if let Some(rest) = dir.strip_prefix("~/") {
        let mut parts = rest.splitn(2, '/');
        let first = parts.next().unwrap_or("");
        let tail = parts.next();
        let base = match first {
            "Downloads" => dirs::download_dir(),
            "Videos" => dirs::video_dir(),
            "Music" => dirs::audio_dir(),
            "Pictures" => dirs::picture_dir(),
            "Documents" => dirs::document_dir(),
            "Desktop" => dirs::desktop_dir(),
            _ => None,
        }
        .unwrap_or_else(|| home().join(first));
        return match tail {
            Some(t) => base.join(t),
            None => base,
        };
    }
    PathBuf::from(dir)
}

/// Default download directory: the localized platform Downloads folder, or
/// `~/Downloads` as a last resort.
pub fn default_download_dir() -> PathBuf {
    dirs::download_dir().unwrap_or_else(|| home().join("Downloads"))
}
