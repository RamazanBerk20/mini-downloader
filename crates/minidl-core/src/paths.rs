//! XDG-correct application paths (no external crate). Resolves *localized* user
//! directories (Downloads/Videos/… may be named e.g. "İndirilenler" on a
//! non-English system) via `$XDG_*_DIR` env vars and `~/.config/user-dirs.dirs`.

use std::path::PathBuf;

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"))
}

fn config_home() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"))
}

/// `$XDG_DATA_HOME/minidownloader` or `~/.local/share/minidownloader`. Holds the DB, aria2 session, logs.
pub fn data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/share"))
        .join("minidownloader")
}

/// `$XDG_CONFIG_HOME/minidownloader` or `~/.config/minidownloader`.
pub fn config_dir() -> PathBuf {
    config_home().join("minidownloader")
}

/// A per-user writable bin dir for tools that self-update (yt-dlp).
pub fn bin_dir() -> PathBuf {
    data_dir().join("bin")
}

/// Resolve a standard XDG user directory (localized), e.g. `xdg_user_dir("DOWNLOAD")`.
/// Checks the `$XDG_<KEY>_DIR` env var first, then `~/.config/user-dirs.dirs`.
pub fn xdg_user_dir(key: &str) -> Option<PathBuf> {
    let env_key = format!("XDG_{key}_DIR");
    if let Some(v) = std::env::var_os(&env_key) {
        let p = PathBuf::from(v);
        if !p.as_os_str().is_empty() {
            return Some(p);
        }
    }
    let content = std::fs::read_to_string(config_home().join("user-dirs.dirs")).ok()?;
    let prefix = format!("{env_key}=");
    for line in content.lines() {
        let l = line.trim();
        if l.starts_with('#') {
            continue;
        }
        if let Some(rest) = l.strip_prefix(&prefix) {
            let val = rest.trim().trim_matches('"');
            let path = if let Some(r) = val.strip_prefix("$HOME/") {
                home().join(r)
            } else if val == "$HOME" {
                home()
            } else {
                PathBuf::from(val)
            };
            return Some(path);
        }
    }
    None
}

/// Map a standard `~/Folder[/sub]` marker to the *localized* XDG directory when
/// the leading folder is a known user dir; otherwise a plain `~` expansion.
pub fn resolve_home_path(dir: &str) -> PathBuf {
    if let Some(rest) = dir.strip_prefix("~/") {
        let mut parts = rest.splitn(2, '/');
        let first = parts.next().unwrap_or("");
        let tail = parts.next();
        let base = match first {
            "Downloads" => xdg_user_dir("DOWNLOAD"),
            "Videos" => xdg_user_dir("VIDEOS"),
            "Music" => xdg_user_dir("MUSIC"),
            "Pictures" => xdg_user_dir("PICTURES"),
            "Documents" => xdg_user_dir("DOCUMENTS"),
            "Desktop" => xdg_user_dir("DESKTOP"),
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

/// Default download directory: the localized XDG Downloads folder, or `~/Downloads`.
pub fn default_download_dir() -> PathBuf {
    xdg_user_dir("DOWNLOAD").unwrap_or_else(|| home().join("Downloads"))
}
