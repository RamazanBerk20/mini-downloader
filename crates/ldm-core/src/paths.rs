//! XDG-correct application paths (no external crate).

use std::path::PathBuf;

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"))
}

/// `$XDG_DATA_HOME/ldm` or `~/.local/share/ldm`. Holds the DB, aria2 session, logs.
pub fn data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/share"))
        .join("ldm")
}

/// `$XDG_CONFIG_HOME/ldm` or `~/.config/ldm`.
pub fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"))
        .join("ldm")
}

/// Default download directory: `~/Downloads`.
pub fn default_download_dir() -> PathBuf {
    home().join("Downloads")
}

/// A per-user writable bin dir for tools that self-update (yt-dlp).
pub fn bin_dir() -> PathBuf {
    data_dir().join("bin")
}
