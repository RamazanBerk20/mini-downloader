//! Trusted GitHub-release checks and verified Windows installer handoff.

use serde::{Deserialize, Serialize};

const REPOSITORY: &str = "RamazanBerk20/mini-downloader";
const PAGE_PREFIX: &str = "https://github.com/RamazanBerk20/mini-downloader/releases/";
const ASSET_PREFIX: &str = "https://github.com/RamazanBerk20/mini-downloader/releases/download/";
const CHECKSUMS: &str = "SHA256SUMS";

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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub current: String,
    pub latest: String,
    pub newer: bool,
    pub url: String,
    pub asset_url: Option<String>,
    pub can_install: bool,
    pub notes: String,
}

fn parse_version(version: &str) -> Option<[u64; 3]> {
    let mut parts = version.split('.');
    let mut parsed = [0; 3];
    for item in &mut parsed {
        let part = parts.next()?;
        if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        *item = part.parse().ok()?;
    }
    parts.next().is_none().then_some(parsed)
}

fn version_gt(left: &str, right: &str) -> bool {
    matches!((parse_version(left), parse_version(right)), (Some(left), Some(right)) if left > right)
}

fn client(timeout: std::time::Duration) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(timeout)
        .user_agent(concat!("mini-downloader/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| format!("create update client: {error}"))
}

fn trusted_page(url: &str) -> bool {
    url.starts_with(PAGE_PREFIX)
}
fn trusted_asset(url: &str) -> bool {
    url.starts_with(ASSET_PREFIX)
}

async fn latest_release() -> Result<Release, String> {
    let response = client(std::time::Duration::from_secs(60))?
        .get(format!(
            "https://api.github.com/repos/{REPOSITORY}/releases/latest"
        ))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|error| format!("update check failed: {error}"))?
        .error_for_status()
        .map_err(|error| format!("update check failed: {error}"))?;
    let bytes = read_limited(response, 4 * 1024 * 1024, "update response").await?;
    let release = serde_json::from_slice::<Release>(&bytes)
        .map_err(|error| format!("update response is invalid: {error}"))?;
    trusted_page(&release.html_url)
        .then_some(release)
        .ok_or_else(|| "update check returned an untrusted release URL".into())
}

fn installer(assets: &[ReleaseAsset]) -> Option<&ReleaseAsset> {
    assets.iter().find(|asset| {
        cfg!(windows)
            && asset.name.ends_with("_x64-setup.exe")
            && trusted_asset(&asset.browser_download_url)
    })
}

fn checksums(assets: &[ReleaseAsset]) -> Option<&ReleaseAsset> {
    assets
        .iter()
        .find(|asset| asset.name == CHECKSUMS && trusted_asset(&asset.browser_download_url))
}

fn signing_thumbprint() -> Option<&'static str> {
    #[cfg(windows)]
    {
        option_env!("MINIDL_WINDOWS_CERTIFICATE_THUMBPRINT")
            .filter(|value| value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
    }
    #[cfg(not(windows))]
    {
        None
    }
}

pub async fn check() -> Result<UpdateInfo, String> {
    let current = env!("CARGO_PKG_VERSION");
    let release = latest_release().await?;
    let latest = release.tag_name.trim_start_matches('v').to_owned();
    if parse_version(&latest).is_none() {
        return Err("update check returned an invalid release version".into());
    }
    let newer = version_gt(&latest, current);
    let candidate = installer(&release.assets);
    let can_install = newer
        && cfg!(windows)
        && candidate.is_some()
        && checksums(&release.assets).is_some()
        && signing_thumbprint().is_some();
    Ok(UpdateInfo {
        current: current.into(),
        latest,
        newer,
        url: release.html_url,
        asset_url: if can_install {
            candidate.map(|asset| asset.browser_download_url.clone())
        } else {
            None
        },
        can_install,
        notes: release.body,
    })
}

#[cfg(any(windows, test))]
fn checksum_for_asset(manifest: &str, asset_name: &str) -> Result<String, String> {
    let mut found = None;
    for line in manifest.lines().map(|line| line.trim_end_matches('\r')) {
        if line.len() < 66 {
            continue;
        }
        let (digest, suffix) = line.split_at(64);
        let Some(name) = suffix
            .strip_prefix("  ")
            .or_else(|| suffix.strip_prefix(" *"))
        else {
            continue;
        };
        if name != asset_name {
            continue;
        }
        if !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("installer checksum entry is malformed".into());
        }
        if found.replace(digest.to_ascii_lowercase()).is_some() {
            return Err("installer checksum entry appears more than once".into());
        }
    }
    found.ok_or_else(|| "installer is missing from the release checksum manifest".into())
}

#[cfg(test)]
fn verify_sha256(bytes: &[u8], expected: &str) -> Result<(), String> {
    use sha2::{Digest, Sha256};
    let actual = format!("{:x}", Sha256::digest(bytes));
    actual
        .eq_ignore_ascii_case(expected)
        .then_some(())
        .ok_or_else(|| "installer checksum verification failed".into())
}

#[cfg(windows)]
fn verify_authenticode(path: &std::path::Path, expected_thumbprint: &str) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Security::Cryptography::{
        CertGetCertificateContextProperty, CERT_SHA1_HASH_PROP_ID,
    };
    use windows_sys::Win32::Security::WinTrust::{
        WTHelperGetProvCertFromChain, WTHelperGetProvSignerFromChain,
        WTHelperProvDataFromStateData, WinVerifyTrust, WINTRUST_ACTION_GENERIC_VERIFY_V2,
        WINTRUST_DATA, WINTRUST_FILE_INFO, WTD_CHOICE_FILE,
        WTD_REVOCATION_CHECK_CHAIN_EXCLUDE_ROOT, WTD_REVOKE_WHOLECHAIN, WTD_STATEACTION_CLOSE,
        WTD_STATEACTION_VERIFY, WTD_UI_NONE,
    };

    let wide = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut file = WINTRUST_FILE_INFO {
        cbStruct: std::mem::size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: wide.as_ptr(),
        ..Default::default()
    };
    let mut data = WINTRUST_DATA {
        cbStruct: std::mem::size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_WHOLECHAIN,
        dwUnionChoice: WTD_CHOICE_FILE,
        Anonymous: windows_sys::Win32::Security::WinTrust::WINTRUST_DATA_0 { pFile: &mut file },
        dwStateAction: WTD_STATEACTION_VERIFY,
        dwProvFlags: WTD_REVOCATION_CHECK_CHAIN_EXCLUDE_ROOT,
        ..Default::default()
    };
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;

    // SAFETY: every WinTrust structure points to live storage for the duration
    // of the calls, and the provider pointers are checked before dereference.
    let result = unsafe {
        (|| -> Result<(), String> {
            let status = WinVerifyTrust(
                std::ptr::null_mut(),
                &mut action,
                &mut data as *mut _ as *mut core::ffi::c_void,
            );
            if status != 0 {
                Err(format!(
                    "installer Authenticode validation failed (0x{status:08x})"
                ))
            } else {
                let provider = WTHelperProvDataFromStateData(data.hWVTStateData);
                let signer = (!provider.is_null())
                    .then(|| WTHelperGetProvSignerFromChain(provider, 0, 0, 0))
                    .filter(|pointer| !pointer.is_null())
                    .ok_or_else(|| "installer signature has no signer".to_string())?;
                let certificate = WTHelperGetProvCertFromChain(signer, 0);
                if certificate.is_null() || (*certificate).pCert.is_null() {
                    Err("installer signature has no signing certificate".into())
                } else {
                    let mut digest = [0u8; 20];
                    let mut length = digest.len() as u32;
                    let ok = CertGetCertificateContextProperty(
                        (*certificate).pCert,
                        CERT_SHA1_HASH_PROP_ID,
                        digest.as_mut_ptr().cast(),
                        &mut length,
                    );
                    if ok == 0 || length as usize != digest.len() {
                        Err("could not read installer signing identity".into())
                    } else {
                        let actual = digest
                            .iter()
                            .map(|byte| format!("{byte:02X}"))
                            .collect::<String>();
                        actual
                            .eq_ignore_ascii_case(expected_thumbprint)
                            .then_some(())
                            .ok_or_else(|| "installer was signed by an unexpected publisher".into())
                    }
                }
            }
        })()
    };

    data.dwStateAction = WTD_STATEACTION_CLOSE;
    unsafe {
        WinVerifyTrust(
            std::ptr::null_mut(),
            &mut action,
            &mut data as *mut _ as *mut core::ffi::c_void,
        );
    }
    result
}

#[cfg(windows)]
async fn download(asset: &ReleaseAsset, maximum: u64, label: &str) -> Result<Vec<u8>, String> {
    if !trusted_asset(&asset.browser_download_url) {
        return Err(format!("refusing an untrusted {label} URL"));
    }
    let response = client(std::time::Duration::from_secs(60))?
        .get(&asset.browser_download_url)
        .send()
        .await
        .map_err(|error| format!("download {label}: {error}"))?
        .error_for_status()
        .map_err(|error| format!("download {label}: {error}"))?;
    read_limited(response, maximum, label).await
}

#[cfg(windows)]
async fn download_installer(
    asset: &ReleaseAsset,
    maximum: u64,
) -> Result<(tempfile::NamedTempFile, String), String> {
    use sha2::{Digest, Sha256};
    use std::io::Write;

    if !trusted_asset(&asset.browser_download_url) {
        return Err("refusing an untrusted installer URL".into());
    }
    let mut response = client(std::time::Duration::from_secs(30 * 60))?
        .get(&asset.browser_download_url)
        .send()
        .await
        .map_err(|error| format!("download installer: {error}"))?
        .error_for_status()
        .map_err(|error| format!("download installer: {error}"))?;
    if response
        .content_length()
        .is_some_and(|length| length > maximum)
    {
        return Err("installer is too large".into());
    }
    let mut file = tempfile::Builder::new()
        .prefix("mini-downloader-update-")
        .suffix(".exe")
        .tempfile()
        .map_err(|error| format!("create installer file: {error}"))?;
    let mut digest = Sha256::new();
    let mut received = 0u64;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("download installer: {error}"))?
    {
        received = received.saturating_add(chunk.len() as u64);
        if received > maximum {
            return Err("installer is too large".into());
        }
        digest.update(&chunk);
        file.write_all(&chunk)
            .map_err(|error| format!("write installer: {error}"))?;
    }
    file.as_file()
        .sync_all()
        .map_err(|error| format!("flush installer: {error}"))?;
    Ok((file, format!("{:x}", digest.finalize())))
}

async fn read_limited(
    mut response: reqwest::Response,
    maximum: u64,
    label: &str,
) -> Result<Vec<u8>, String> {
    if response
        .content_length()
        .is_some_and(|length| length > maximum)
    {
        return Err(format!("{label} is too large"));
    }
    let capacity = response.content_length().unwrap_or_default().min(maximum) as usize;
    let mut bytes = Vec::with_capacity(capacity);
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("download {label}: {error}"))?
    {
        if bytes.len().saturating_add(chunk.len()) > maximum as usize {
            return Err(format!("{label} is too large"));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// Returns true when the caller must terminate for a launched Windows installer.
pub async fn install(asset_url: Option<String>, _page_url: String) -> Result<bool, String> {
    let install_requested = asset_url.is_some();
    let release = latest_release().await?;

    #[cfg(windows)]
    if install_requested {
        let latest = release.tag_name.trim_start_matches('v');
        if latest.is_empty() || !version_gt(latest, env!("CARGO_PKG_VERSION")) {
            return Err("the selected release is no longer newer than this app".into());
        }
        let installer =
            installer(&release.assets).ok_or("no trusted Windows installer was found")?;
        let sums = checksums(&release.assets).ok_or("release has no checksum manifest")?;
        let manifest_bytes = download(sums, 1024 * 1024, "checksum manifest").await?;
        let manifest = std::str::from_utf8(&manifest_bytes)
            .map_err(|error| format!("checksum manifest is not UTF-8: {error}"))?;
        let expected = checksum_for_asset(manifest, &installer.name)?;
        let (file, actual) = download_installer(installer, 512 * 1024 * 1024).await?;
        if !actual.eq_ignore_ascii_case(&expected) {
            return Err("installer checksum verification failed".into());
        }
        verify_authenticode(
            file.path(),
            signing_thumbprint().ok_or("this build has no trusted signing identity")?,
        )?;
        let (_, path) = file
            .keep()
            .map_err(|error| format!("keep installer: {}", error.error))?;
        let mut command = std::process::Command::new(path);
        minidl_core::child_process::hide_console_window(&mut command);
        command
            .spawn()
            .map_err(|error| format!("launch installer: {error}"))?;
        return Ok(true);
    }

    let _ = install_requested;
    open::that(&release.html_url).map_err(|error| format!("open release page: {error}"))?;
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert!(version_gt("2.10.0", "2.9.9"));
        assert!(!version_gt("2.1.2", "2.1.2"));
        assert!(!version_gt("2.2.0-beta", "2.1.2"));
        assert_eq!(parse_version("3.0.0"), Some([3, 0, 0]));
        assert_eq!(parse_version("2.1"), None);
    }

    #[test]
    fn checksum_selection_is_exact() {
        let name = "Mini.Downloader_2.1.0_x64-setup.exe";
        let digest = "a3f1d9e7c5b4a2918070605040302010fedcba98765432100123456789abcdef";
        assert_eq!(
            checksum_for_asset(&format!("{digest}  {name}\n"), name).unwrap(),
            digest
        );
        assert!(verify_sha256(
            b"hello world",
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        )
        .is_ok());
    }

    #[test]
    fn only_canonical_release_urls_are_trusted() {
        assert!(trusted_page(
            "https://github.com/RamazanBerk20/mini-downloader/releases/tag/v2.1.2"
        ));
        assert!(!trusted_asset("https://example.invalid/setup.exe"));
    }
}
