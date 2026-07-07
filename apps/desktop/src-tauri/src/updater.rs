//! GitHub-releases self-updater. No signing key required — it trusts HTTPS +
//! GitHub (the same trust as clicking the download link on the releases page).
//!
//! - Windows: download the NSIS `setup.exe`, launch it, and quit so it can
//!   replace the running binary.
//! - Linux (deb/rpm/AppImage/AUR): the app never self-installs — it only reports
//!   that a newer release exists and opens the release page; the package manager
//!   performs the actual update.

use std::fmt::Display;

use serde::Serialize;
use serde_json::Value;

use crate::errors::{CommandError, ErrorKind};

const REPO: &str = "RamazanBerk20/mini-downloader";

#[derive(Serialize)]
pub struct UpdateInfo {
    pub current: String,
    pub latest: String,
    pub newer: bool,
    /// Release page (browser link).
    pub url: String,
    /// Platform installer download url, when this platform can self-install.
    pub asset_url: Option<String>,
    /// True on Windows with a matching installer asset; false on Linux (defer to
    /// the package manager).
    pub can_install: bool,
    pub notes: String,
}

fn neterr<E: Display>(ctx: &str, e: E) -> CommandError {
    CommandError { kind: ErrorKind::Other, message: format!("{ctx}: {e}") }
}

/// Semver-ish `a > b` over dotted numeric components.
fn version_gt(a: &str, b: &str) -> bool {
    let parse = |s: &str| -> Vec<u64> {
        s.split(['.', '-', '+']).map(|p| p.parse::<u64>().unwrap_or(0)).collect()
    };
    let (va, vb) = (parse(a), parse(b));
    for i in 0..va.len().max(vb.len()) {
        let (x, y) = (va.get(i).copied().unwrap_or(0), vb.get(i).copied().unwrap_or(0));
        if x != y {
            return x > y;
        }
    }
    false
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(60))
        .user_agent(concat!("mini-downloader/", env!("CARGO_PKG_VERSION")))
        .build()
        .unwrap_or_default()
}

/// The installer asset for the current platform (Windows only; Linux updates via
/// its package manager).
fn asset_for_platform(assets: &[Value]) -> Option<String> {
    assets.iter().find_map(|a| {
        let name = a.get("name")?.as_str()?;
        let matches = cfg!(windows) && name.ends_with("_x64-setup.exe");
        if matches {
            a.get("browser_download_url")?.as_str().map(String::from)
        } else {
            None
        }
    })
}

/// Query the latest GitHub release and compare it to the running version.
#[tauri::command]
pub async fn check_update() -> Result<UpdateInfo, CommandError> {
    let current = env!("CARGO_PKG_VERSION");
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let resp: Value = client()
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| neterr("update check failed", e))?
        .error_for_status()
        .map_err(|e| neterr("update check failed", e))?
        .json()
        .await
        .map_err(|e| neterr("update check parse", e))?;

    let latest = resp
        .get("tag_name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim_start_matches('v')
        .to_string();
    let page = resp.get("html_url").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let notes = resp.get("body").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let assets: Vec<Value> =
        resp.get("assets").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let asset_url = asset_for_platform(&assets);
    let newer = !latest.is_empty() && version_gt(&latest, current);

    Ok(UpdateInfo {
        current: current.to_string(),
        latest,
        newer,
        url: page,
        can_install: cfg!(windows) && asset_url.is_some(),
        asset_url,
        notes,
    })
}

/// Windows: download the installer, launch it, and quit so it can replace the
/// running files. Everything else: open the release page in the browser.
#[tauri::command]
pub async fn install_update(
    asset_url: Option<String>,
    page_url: String,
    app: tauri::AppHandle,
) -> Result<(), CommandError> {
    #[cfg(windows)]
    if let Some(url) = asset_url {
        let bytes = client()
            .get(&url)
            .send()
            .await
            .map_err(|e| neterr("download failed", e))?
            .error_for_status()
            .map_err(|e| neterr("download failed", e))?
            .bytes()
            .await
            .map_err(|e| neterr("download failed", e))?;
        let path = std::env::temp_dir().join("MiniDownloaderSetup.exe");
        std::fs::write(&path, &bytes).map_err(|e| neterr("write installer", e))?;
        std::process::Command::new(&path)
            .spawn()
            .map_err(|e| neterr("launch installer", e))?;
        app.exit(0);
        return Ok(());
    }

    // Linux / no installer asset: hand off to the browser + package manager.
    let _ = asset_url; // unused on non-windows
    use tauri_plugin_opener::OpenerExt;
    app.opener().open_url(page_url, None::<&str>).map_err(|e| neterr("open release page", e))
}
