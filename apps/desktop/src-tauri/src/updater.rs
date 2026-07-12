//! GitHub-releases self-updater.
//!
//! The release workflow publishes a `SHA256SUMS` asset after it has uploaded
//! every release file. Before Windows launches an installer, this module gets
//! fresh release metadata, downloads that manifest, and verifies the exact
//! installer bytes against its SHA-256 entry.
//!
//! This is an integrity check, not a publisher signature: an attacker who can
//! modify both the GitHub release and `SHA256SUMS` can still replace both. Add
//! Authenticode signing or a signed manifest with a pinned public key when the
//! project has managed signing-key authority.
//!
//! - Windows: download the NSIS `setup.exe`, launch it, and quit so it can
//!   replace the running binary.
//! - Linux (deb/rpm/AppImage/AUR): the app never self-installs — it only reports
//!   that a newer release exists and opens the release page; the package manager
//!   performs the actual update.

use std::fmt::Display;

#[cfg(windows)]
use std::io::Write;

use serde::{Deserialize, Serialize};
#[cfg(any(windows, test))]
use sha2::{Digest, Sha256};

use crate::errors::{CommandError, ErrorKind};

const REPO: &str = "RamazanBerk20/mini-downloader";
const RELEASE_PAGE_PREFIX: &str = "https://github.com/RamazanBerk20/mini-downloader/releases/";
const RELEASE_ASSET_PREFIX: &str =
    "https://github.com/RamazanBerk20/mini-downloader/releases/download/";
const CHECKSUMS_ASSET: &str = "SHA256SUMS";
#[cfg(windows)]
const MAX_CHECKSUMS_BYTES: u64 = 1024 * 1024;
#[cfg(windows)]
const MAX_INSTALLER_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Deserialize)]
struct Release {
    #[serde(default)]
    tag_name: String,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    assets: Vec<ReleaseAsset>,
}

#[derive(Debug, Deserialize)]
struct ReleaseAsset {
    #[serde(default)]
    name: String,
    #[serde(default)]
    browser_download_url: String,
}

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
    CommandError {
        kind: ErrorKind::Other,
        message: format!("{ctx}: {e}"),
    }
}

fn rejected(message: impl Into<String>) -> CommandError {
    CommandError {
        kind: ErrorKind::Rejected,
        message: message.into(),
    }
}

/// Semver-ish `a > b` over dotted numeric components.
fn version_gt(a: &str, b: &str) -> bool {
    let parse = |s: &str| -> Vec<u64> {
        s.split(['.', '-', '+'])
            .map(|p| p.parse::<u64>().unwrap_or(0))
            .collect()
    };
    let (va, vb) = (parse(a), parse(b));
    for i in 0..va.len().max(vb.len()) {
        let (x, y) = (
            va.get(i).copied().unwrap_or(0),
            vb.get(i).copied().unwrap_or(0),
        );
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

fn is_trusted_release_page(url: &str) -> bool {
    url.starts_with(RELEASE_PAGE_PREFIX)
}

fn is_trusted_release_asset(url: &str) -> bool {
    url.starts_with(RELEASE_ASSET_PREFIX)
}

async fn latest_release() -> Result<Release, CommandError> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let release: Release = client()
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

    if !is_trusted_release_page(&release.html_url) {
        return Err(rejected("update check returned an untrusted release URL"));
    }
    Ok(release)
}

/// The installer asset for the current platform (Windows only; Linux updates via
/// its package manager). Only GitHub's canonical release-download URL is valid.
fn installer_asset(assets: &[ReleaseAsset]) -> Option<&ReleaseAsset> {
    assets.iter().find(|asset| {
        cfg!(windows)
            && asset.name.ends_with("_x64-setup.exe")
            && is_trusted_release_asset(&asset.browser_download_url)
    })
}

fn checksums_asset(assets: &[ReleaseAsset]) -> Option<&ReleaseAsset> {
    assets.iter().find(|asset| {
        asset.name == CHECKSUMS_ASSET && is_trusted_release_asset(&asset.browser_download_url)
    })
}

/// Parse the GNU `sha256sum` format emitted by the release workflow, requiring
/// exactly one entry for the selected installer asset.
#[cfg(any(windows, test))]
fn checksum_for_asset(manifest: &str, asset_name: &str) -> Result<String, CommandError> {
    let mut found = None;
    for line in manifest.lines().map(|line| line.trim_end_matches('\r')) {
        if line.len() < 66 {
            continue;
        }
        let (digest, rest) = line.split_at(64);
        let Some(name) = rest.strip_prefix("  ").or_else(|| rest.strip_prefix(" *")) else {
            continue;
        };
        if name != asset_name {
            continue;
        }
        if !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(rejected("installer checksum entry is malformed"));
        }
        if found.replace(digest.to_ascii_lowercase()).is_some() {
            return Err(rejected("installer checksum entry appears more than once"));
        }
    }
    found.ok_or_else(|| rejected("installer is missing from the release checksum manifest"))
}

#[cfg(windows)]
async fn download_asset(
    asset: &ReleaseAsset,
    max_bytes: u64,
    what: &str,
) -> Result<Vec<u8>, CommandError> {
    if !is_trusted_release_asset(&asset.browser_download_url) {
        return Err(rejected(format!("refusing an untrusted {what} URL")));
    }
    let response = client()
        .get(&asset.browser_download_url)
        .send()
        .await
        .map_err(|e| neterr(&format!("download {what}"), e))?
        .error_for_status()
        .map_err(|e| neterr(&format!("download {what}"), e))?;
    if response
        .content_length()
        .is_some_and(|size| size > max_bytes)
    {
        return Err(rejected(format!("{what} is too large")));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|e| neterr(&format!("download {what}"), e))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max_bytes {
        return Err(rejected(format!("{what} is too large")));
    }
    Ok(bytes.to_vec())
}

#[cfg(windows)]
async fn download_checksums(asset: &ReleaseAsset) -> Result<String, CommandError> {
    let bytes = download_asset(asset, MAX_CHECKSUMS_BYTES, "checksum manifest").await?;
    std::str::from_utf8(&bytes)
        .map(str::to_owned)
        .map_err(|e| rejected(format!("checksum manifest is not UTF-8: {e}")))
}

#[cfg(any(windows, test))]
fn verify_sha256(bytes: &[u8], expected: &str) -> Result<(), CommandError> {
    let actual = format!("{:x}", Sha256::digest(bytes));
    if actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(rejected("installer checksum verification failed"))
    }
}

#[cfg(windows)]
fn write_verified_installer(bytes: &[u8]) -> Result<std::path::PathBuf, CommandError> {
    // `NamedTempFile` creates the file atomically with a random name, avoiding
    // the predictable temp path that the old updater could overwrite or follow.
    let mut file = tempfile::Builder::new()
        .prefix("mini-downloader-update-")
        .suffix(".exe")
        .tempfile()
        .map_err(|e| neterr("create installer file", e))?;
    file.write_all(bytes)
        .map_err(|e| neterr("write installer", e))?;
    file.as_file()
        .sync_all()
        .map_err(|e| neterr("flush installer", e))?;
    let (_file, path) = file
        .keep()
        .map_err(|e| neterr("keep installer file", e.error))?;
    Ok(path)
}

/// Query the latest GitHub release and compare it to the running version.
#[tauri::command]
pub async fn check_update() -> Result<UpdateInfo, CommandError> {
    let current = env!("CARGO_PKG_VERSION");
    let release = latest_release().await?;
    let latest = release.tag_name.trim_start_matches('v').to_string();
    let newer = !latest.is_empty() && version_gt(&latest, current);
    let installer = installer_asset(&release.assets);
    let can_install =
        newer && cfg!(windows) && installer.is_some() && checksums_asset(&release.assets).is_some();
    // Do not hand an installer URL to the renderer unless the corresponding
    // release includes the manifest required for verified installation.
    let asset_url = if can_install {
        installer.map(|asset| asset.browser_download_url.clone())
    } else {
        None
    };

    Ok(UpdateInfo {
        current: current.to_string(),
        latest,
        newer,
        url: release.html_url,
        can_install,
        asset_url,
        notes: release.body,
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
    // Values crossing the webview boundary are never used as an update source.
    // `asset_url` only records whether the UI requested self-install; the
    // release and its assets are fetched again from GitHub below.
    let install_requested = asset_url.is_some();
    let _ = page_url;
    let release = latest_release().await?;

    #[cfg(windows)]
    if install_requested {
        let latest = release.tag_name.trim_start_matches('v');
        if latest.is_empty() || !version_gt(latest, env!("CARGO_PKG_VERSION")) {
            return Err(rejected(
                "the selected release is no longer newer than this app",
            ));
        }
        let installer = installer_asset(&release.assets)
            .ok_or_else(|| rejected("no trusted Windows installer was found in this release"))?;
        let manifest_asset = checksums_asset(&release.assets).ok_or_else(|| {
            rejected("this release has no checksum manifest; refusing self-install")
        })?;
        let manifest = download_checksums(manifest_asset).await?;
        let expected = checksum_for_asset(&manifest, &installer.name)?;
        let bytes = download_asset(installer, MAX_INSTALLER_BYTES, "installer").await?;
        verify_sha256(&bytes, &expected)?;
        let path = write_verified_installer(&bytes)?;
        std::process::Command::new(&path)
            .spawn()
            .map_err(|e| neterr("launch installer", e))?;
        app.exit(0);
        return Ok(());
    }

    // Linux / no verified installer asset: hand off to the trusted release page
    // and let the package manager perform the update.
    let _ = install_requested; // unused on non-Windows targets
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_url(release.html_url, None::<&str>)
        .map_err(|e| neterr("open release page", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_manifest_accepts_the_selected_asset_only() {
        let installer = "Mini.Downloader_2.1.0_x64-setup.exe";
        let hash = "a3f1d9e7c5b4a2918070605040302010fedcba98765432100123456789abcdef";
        let manifest = format!("{hash}  {installer}\n{}  other.exe\n", "f".repeat(64));

        assert_eq!(checksum_for_asset(&manifest, installer).unwrap(), hash);
        assert!(checksum_for_asset(&manifest, "missing.exe").is_err());
    }

    #[test]
    fn checksum_manifest_rejects_malformed_or_duplicate_entries() {
        let installer = "Mini.Downloader_2.1.0_x64-setup.exe";
        let malformed = format!("{}  {installer}\n", "z".repeat(64));
        assert!(checksum_for_asset(&malformed, installer).is_err());

        let duplicate = format!(
            "{}  {installer}\n{}  {installer}\n",
            "a".repeat(64),
            "b".repeat(64)
        );
        assert!(checksum_for_asset(&duplicate, installer).is_err());
    }

    #[test]
    fn sha256_verification_rejects_changed_bytes() {
        let hash = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
        assert!(verify_sha256(b"hello world", hash).is_ok());
        assert!(verify_sha256(b"hello world!", hash).is_err());
    }

    #[test]
    fn only_canonical_github_release_urls_are_trusted() {
        assert!(is_trusted_release_page(
            "https://github.com/RamazanBerk20/mini-downloader/releases/tag/v2.1.0"
        ));
        assert!(is_trusted_release_asset(
            "https://github.com/RamazanBerk20/mini-downloader/releases/download/v2.1.0/setup.exe"
        ));
        assert!(!is_trusted_release_asset(
            "https://example.invalid/setup.exe"
        ));
    }
}
