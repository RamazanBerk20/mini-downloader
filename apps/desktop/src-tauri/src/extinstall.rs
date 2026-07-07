//! IDM-style browser-extension auto-install. Like IDM on Windows, once the
//! extension is *published + signed* the app writes the browser's own
//! auto-install pointer so the extension appears on next browser launch — no
//! about:debugging, no store visit.
//!
//! - Firefox (all OSes, no root): drop the AMO-signed `.xpi` into every profile's
//!   `extensions/<addon-id>.xpi`. Works the moment `AMO_XPI_URL` is set.
//! - Chromium: an "external extension" pointer to the Chrome Web Store update
//!   URL (Linux JSON file / Windows registry). Requires a CWS listing, so it
//!   stays off until `CWS_EXT_ID` is set.
//!
//! Both are no-ops while the constants below are empty (nothing published yet).

use std::path::PathBuf;

use crate::errors::{CommandError, ErrorKind};

/// AMO "download latest signed .xpi" URL — fill after the add-on is listed, e.g.
/// `https://addons.mozilla.org/firefox/downloads/latest/mini-downloader/latest.xpi`.
const AMO_XPI_URL: &str = "";
/// Chrome Web Store extension id (assigned on publish).
const CWS_EXT_ID: &str = "hhaobmkdgijodfieadeeanjmnneckafj";
const CWS_UPDATE_URL: &str = "https://clients2.google.com/service/update2/crx";

/// Firefox profile parent dirs across install types (native / forks / sandboxed).
fn firefox_profile_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = dirs::home_dir() {
        for rel in [
            ".mozilla/firefox",
            ".zen",
            ".librewolf",
            ".waterfox",
            ".var/app/org.mozilla.firefox/.mozilla/firefox",
            "snap/firefox/common/.mozilla/firefox",
        ] {
            roots.push(home.join(rel));
        }
    }
    // Windows: %APPDATA%\Mozilla\Firefox\Profiles
    if let Some(data) = dirs::data_dir() {
        roots.push(data.join("Mozilla").join("Firefox").join("Profiles"));
    }
    roots.into_iter().filter(|p| p.is_dir()).collect()
}

/// Every actual profile dir (contains prefs.js/times.json) under those roots.
fn firefox_profiles() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for root in firefox_profile_roots() {
        if let Ok(entries) = std::fs::read_dir(&root) {
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() && (p.join("prefs.js").exists() || p.join("times.json").exists()) {
                    out.push(p);
                }
            }
        }
    }
    out
}

/// Download the AMO-signed .xpi and drop it into every Firefox profile.
async fn install_firefox(report: &mut Vec<String>) -> bool {
    if AMO_XPI_URL.is_empty() {
        return false;
    }
    let bytes = match reqwest::get(AMO_XPI_URL).await {
        Ok(r) => match r.error_for_status() {
            Ok(r) => match r.bytes().await {
                Ok(b) => b,
                Err(_) => return false,
            },
            Err(_) => return false,
        },
        Err(_) => return false,
    };
    let id = minidl_core::ipc::EXTENSION_ID;
    let mut any = false;
    for prof in firefox_profiles() {
        let dir = prof.join("extensions");
        if std::fs::create_dir_all(&dir).is_err() {
            continue;
        }
        let xpi = dir.join(format!("{id}.xpi"));
        if std::fs::write(&xpi, &bytes).is_ok() {
            report.push(xpi.to_string_lossy().into_owned());
            any = true;
        }
    }
    any
}

/// Chromium "external extension" pointer to the Web Store (Linux: JSON files).
#[cfg(unix)]
fn install_chromium(report: &mut Vec<String>) -> bool {
    if CWS_EXT_ID.is_empty() {
        return false;
    }
    let Some(home) = dirs::home_dir() else { return false };
    let json = serde_json::json!({ "external_update_url": CWS_UPDATE_URL });
    let payload = serde_json::to_vec_pretty(&json).unwrap_or_default();
    let mut any = false;
    for sub in [
        "google-chrome",
        "google-chrome-beta",
        "chromium",
        "ungoogled-chromium",
        "BraveSoftware/Brave-Browser",
        "microsoft-edge",
        "vivaldi",
        "opera",
    ] {
        let base = home.join(".config").join(sub);
        if !base.is_dir() {
            continue;
        }
        let dir = base.join("External Extensions");
        if std::fs::create_dir_all(&dir).is_err() {
            continue;
        }
        let f = dir.join(format!("{CWS_EXT_ID}.json"));
        if std::fs::write(&f, &payload).is_ok() {
            report.push(f.to_string_lossy().into_owned());
            any = true;
        }
    }
    any
}

/// Windows: register the Web Store extension under HKCU so Chrome auto-installs it.
#[cfg(windows)]
fn install_chromium(report: &mut Vec<String>) -> bool {
    if CWS_EXT_ID.is_empty() {
        return false;
    }
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let mut any = false;
    for base in [
        "Software\\Google\\Chrome\\Extensions",
        "Software\\Chromium\\Extensions",
        "Software\\Microsoft\\Edge\\Extensions",
    ] {
        let path = format!("{base}\\{CWS_EXT_ID}");
        if let Ok((key, _)) = hkcu.create_subkey(&path) {
            if key.set_value("update_url", &CWS_UPDATE_URL.to_string()).is_ok() {
                report.push(format!("HKCU\\{path}"));
                any = true;
            }
        }
    }
    any
}

/// Auto-install the extension into every detected browser. Returns the list of
/// locations written (empty if nothing is published yet).
pub async fn auto_install() -> Vec<String> {
    let mut report = Vec::new();
    let _ = install_firefox(&mut report).await;
    let _ = install_chromium(&mut report);
    report
}

/// Command surface: report what was installed, or an actionable "not published".
#[tauri::command]
pub async fn auto_install_extension() -> Result<String, CommandError> {
    let report = auto_install().await;
    if report.is_empty() {
        return Err(CommandError {
            kind: ErrorKind::Other,
            message: "Extension not published to the stores yet (see scripts/EXTENSION-PUBLISHING.md)."
                .into(),
        });
    }
    Ok(format!("Installed the extension into {} location(s):\n{}", report.len(), report.join("\n")))
}
