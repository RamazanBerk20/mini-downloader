//! Browser connector discovery and native-messaging host registration.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use minidl_ipc::{
    BrowserFamily, CHROME_EXTENSION_ID, CHROME_STORE_EXTENSION_ID, EXTENSION_ID, NATIVE_HOST_NAME,
};
use serde::Serialize;
use serde_json::json;

#[derive(Debug, Default)]
pub struct ConnectorPresence {
    firefox_last_seen: Option<i64>,
    chromium_last_seen: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectorStatus {
    pub firefox_profile_detected: bool,
    pub chromium_profile_detected: bool,
    pub firefox_detected: bool,
    pub chromium_detected: bool,
    pub firefox_connector_installed: bool,
    pub chromium_connector_installed: bool,
    pub firefox_last_seen: Option<i64>,
    pub chromium_last_seen: Option<i64>,
}

fn unix_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

pub fn record(presence: &Arc<Mutex<ConnectorPresence>>, family: BrowserFamily) -> ConnectorStatus {
    let mut state = presence.lock().unwrap_or_else(|error| error.into_inner());
    match family {
        BrowserFamily::Firefox => state.firefox_last_seen = Some(unix_millis()),
        BrowserFamily::Chromium => state.chromium_last_seen = Some(unix_millis()),
    }
    drop(state);
    status(presence)
}

pub fn status(presence: &Arc<Mutex<ConnectorPresence>>) -> ConnectorStatus {
    let state = presence.lock().unwrap_or_else(|error| error.into_inner());
    let (firefox_last_seen, chromium_last_seen) =
        (state.firefox_last_seen, state.chromium_last_seen);
    drop(state);
    let firefox_roots = firefox_profile_roots();
    let chromium_roots = chromium_profile_roots();
    ConnectorStatus {
        firefox_profile_detected: roots_exist(&firefox_roots),
        chromium_profile_detected: roots_exist(&chromium_roots),
        firefox_detected: firefox_last_seen.is_some(),
        chromium_detected: chromium_last_seen.is_some(),
        firefox_connector_installed: firefox_roots.iter().any(|root| {
            profile_dirs(root).into_iter().any(|profile| {
                firefox_state(&profile).unwrap_or_else(|| {
                    profile
                        .join("extensions")
                        .join(format!("{EXTENSION_ID}.xpi"))
                        .is_file()
                })
            })
        }),
        chromium_connector_installed: chromium_roots.iter().any(|root| {
            profile_dirs(root).into_iter().any(|profile| {
                chromium_state(&profile).unwrap_or_else(|| {
                    [CHROME_STORE_EXTENSION_ID, CHROME_EXTENSION_ID]
                        .iter()
                        .any(|id| profile.join("Extensions").join(id).is_dir())
                })
            })
        }),
        firefox_last_seen,
        chromium_last_seen,
    }
}

fn roots_exist(roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| root.is_dir())
}

fn profile_dirs(root: &Path) -> Vec<PathBuf> {
    if !root.is_dir() {
        return Vec::new();
    }
    let mut profiles = vec![root.to_owned()];
    if let Ok(entries) = std::fs::read_dir(root) {
        profiles.extend(
            entries
                .flatten()
                .filter_map(|entry| entry.file_type().ok()?.is_dir().then(|| entry.path())),
        );
    }
    profiles
}

fn firefox_state(profile: &Path) -> Option<bool> {
    firefox_state_json(&std::fs::read_to_string(profile.join("extensions.json")).ok()?)
}

fn firefox_state_json(raw: &str) -> Option<bool> {
    let document: serde_json::Value = serde_json::from_str(raw).ok()?;
    let addon =
        document.get("addons")?.as_array()?.iter().find(|addon| {
            addon.get("id").and_then(serde_json::Value::as_str) == Some(EXTENSION_ID)
        })?;
    Some(
        addon
            .get("active")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or_else(|| {
                !addon
                    .get("userDisabled")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
                    && !addon
                        .get("appDisabled")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false)
            }),
    )
}

fn chromium_state(profile: &Path) -> Option<bool> {
    chromium_state_json(&std::fs::read_to_string(profile.join("Preferences")).ok()?)
}

fn chromium_state_json(raw: &str) -> Option<bool> {
    let document: serde_json::Value = serde_json::from_str(raw).ok()?;
    let settings = document.pointer("/extensions/settings")?.as_object()?;
    let mut found = false;
    for id in [CHROME_STORE_EXTENSION_ID, CHROME_EXTENSION_ID] {
        if let Some(entry) = settings.get(id) {
            found = true;
            let state = entry.get("state");
            if state.and_then(serde_json::Value::as_i64) == Some(1)
                || state.and_then(serde_json::Value::as_str) == Some("1")
            {
                return Some(true);
            }
        }
    }
    found.then_some(false)
}

#[cfg(unix)]
fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

#[cfg(unix)]
fn config_home(home: &Path) -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(".config"))
}

#[cfg(unix)]
fn firefox_profile_roots_for(home: &Path, config: &Path) -> Vec<PathBuf> {
    vec![
        home.join(".mozilla/firefox"),
        home.join(".zen"),
        config.join("zen"),
        home.join(".librewolf"),
        home.join(".waterfox"),
        home.join(".var/app/org.mozilla.firefox/.mozilla/firefox"),
        home.join(".var/app/io.gitlab.librewolf-community/.librewolf"),
        home.join(".var/app/app.zen_browser.zen/.zen"),
        home.join(".var/app/app.zen_browser.zen/config/zen"),
        home.join("snap/firefox/common/.mozilla/firefox"),
    ]
}

#[cfg(unix)]
fn firefox_profile_roots() -> Vec<PathBuf> {
    let Some(home) = home() else {
        return Vec::new();
    };
    firefox_profile_roots_for(&home, &config_home(&home))
}

#[cfg(windows)]
fn firefox_profile_roots() -> Vec<PathBuf> {
    let Some(appdata) = std::env::var_os("APPDATA").map(PathBuf::from) else {
        return Vec::new();
    };
    [
        "Mozilla/Firefox/Profiles",
        "Zen/Profiles",
        "LibreWolf/Profiles",
        "Waterfox/Profiles",
    ]
    .into_iter()
    .map(|path| appdata.join(path))
    .collect()
}

#[cfg(unix)]
fn chromium_profile_roots() -> Vec<PathBuf> {
    let Some(home) = home() else {
        return Vec::new();
    };
    let config = config_home(&home);
    let mut roots: Vec<_> = chromium_config_subdirs()
        .iter()
        .map(|path| config.join(path))
        .collect();
    roots.extend([
        home.join(".var/app/com.google.Chrome/config/google-chrome"),
        home.join(".var/app/org.chromium.Chromium/config/chromium"),
        home.join(".var/app/com.brave.Browser/config/BraveSoftware/Brave-Browser"),
    ]);
    roots
}

#[cfg(windows)]
fn chromium_profile_roots() -> Vec<PathBuf> {
    let Some(local) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) else {
        return Vec::new();
    };
    [
        "Google/Chrome/User Data",
        "Chromium/User Data",
        "BraveSoftware/Brave-Browser/User Data",
        "Microsoft/Edge/User Data",
        "Vivaldi/User Data",
        "Opera Software/Opera Stable",
    ]
    .into_iter()
    .map(|path| local.join(path))
    .collect()
}

#[cfg(not(any(unix, windows)))]
fn firefox_profile_roots() -> Vec<PathBuf> {
    Vec::new()
}
#[cfg(not(any(unix, windows)))]
fn chromium_profile_roots() -> Vec<PathBuf> {
    Vec::new()
}

#[cfg(unix)]
fn chromium_config_subdirs() -> &'static [&'static str] {
    &[
        "google-chrome",
        "google-chrome-beta",
        "google-chrome-unstable",
        "google-chrome-canary",
        "google-chrome-for-testing",
        "chromium",
        "ungoogled-chromium",
        "BraveSoftware/Brave-Browser",
        "BraveSoftware/Brave-Browser-Beta",
        "BraveSoftware/Brave-Browser-Dev",
        "BraveSoftware/Brave-Browser-Nightly",
        "microsoft-edge",
        "microsoft-edge-beta",
        "microsoft-edge-dev",
        "vivaldi",
        "opera",
    ]
}

fn host_binary() -> Result<PathBuf, String> {
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let directory = executable
        .parent()
        .ok_or("app executable has no parent directory")?
        .to_owned();
    let candidate = directory.join(format!(
        "minidl-native-host{}",
        std::env::consts::EXE_SUFFIX
    ));
    if !candidate.is_file() {
        return Err(format!("{} was not found", candidate.display()));
    }
    #[cfg(unix)]
    {
        install_stable_host(&candidate)
    }
    #[cfg(not(unix))]
    {
        candidate.canonicalize().map_err(|error| error.to_string())
    }
}

#[cfg(unix)]
fn install_stable_host(source: &Path) -> Result<PathBuf, String> {
    use std::io::{Read, Write};
    use std::os::unix::fs::PermissionsExt;

    let directory = minidl_ipc::data_dir().join("native-messaging");
    std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| error.to_string())?;
    let target = directory.join("minidl-native-host");
    if source.canonicalize().ok().as_ref() == target.canonicalize().ok().as_ref() {
        return Ok(target);
    }

    let mut input = std::fs::File::open(source).map_err(|error| error.to_string())?;
    let mut temporary = tempfile::NamedTempFile::new_in(&directory)
        .map_err(|error| format!("create native-host staging file: {error}"))?;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = input.read(&mut buffer).map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        temporary
            .write_all(&buffer[..read])
            .map_err(|error| error.to_string())?;
    }
    temporary
        .as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o700))
        .map_err(|error| error.to_string())?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| error.to_string())?;
    temporary
        .persist(&target)
        .map_err(|error| format!("install native host: {}", error.error))?;
    Ok(target)
}

fn firefox_manifest(host: &str) -> serde_json::Value {
    json!({
        "name": NATIVE_HOST_NAME, "description": "Mini Downloader native bridge",
        "path": host, "type": "stdio", "allowed_extensions": [EXTENSION_ID]
    })
}

fn chromium_manifest(host: &str) -> serde_json::Value {
    json!({
        "name": NATIVE_HOST_NAME, "description": "Mini Downloader native bridge",
        "path": host, "type": "stdio", "allowed_origins": [
            format!("chrome-extension://{CHROME_EXTENSION_ID}/"),
            format!("chrome-extension://{CHROME_STORE_EXTENSION_ID}/")
        ]
    })
}

fn write_manifest(directory: &Path, manifest: &serde_json::Value) -> Result<String, String> {
    use std::io::Write as _;

    std::fs::create_dir_all(directory)
        .map_err(|error| format!("create {}: {error}", directory.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("protect {}: {error}", directory.display()))?;
    }
    let path = directory.join(format!("{NATIVE_HOST_NAME}.json"));
    let bytes = serde_json::to_vec_pretty(manifest)
        .map_err(|error| format!("serialize native-host manifest: {error}"))?;
    let mut temporary = tempfile::NamedTempFile::new_in(directory)
        .map_err(|error| format!("create manifest staging file: {error}"))?;
    temporary
        .write_all(&bytes)
        .map_err(|error| format!("write {}: {error}", path.display()))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| format!("flush {}: {error}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("protect {}: {error}", path.display()))?;
    }
    temporary
        .persist(&path)
        .map_err(|error| format!("install {}: {}", path.display(), error.error))?;
    Ok(path.to_string_lossy().into_owned())
}

#[cfg(unix)]
pub fn register_native_host_manifests() -> Result<Vec<String>, String> {
    let host = host_binary()?;
    let host = host.to_string_lossy().into_owned();
    let home = home().ok_or("HOME not set")?;
    let config = config_home(&home);
    let mut installed = Vec::new();
    let firefox = firefox_manifest(&host);
    for base in [".mozilla", ".zen", ".librewolf", ".waterfox"] {
        let base = home.join(base);
        if base == home.join(".mozilla") || base.exists() {
            if let Ok(path) = write_manifest(&base.join("native-messaging-hosts"), &firefox) {
                installed.push(path);
            }
        }
    }
    for base in [
        ".var/app/org.mozilla.firefox/.mozilla",
        ".var/app/io.gitlab.librewolf-community/.librewolf",
        ".var/app/app.zen_browser.zen/.zen",
        ".var/app/app.zen_browser.zen/config/zen",
        "snap/firefox/common/.mozilla",
    ] {
        let base = home.join(base);
        if base.exists() {
            if let Ok(path) = write_manifest(&base.join("native-messaging-hosts"), &firefox) {
                installed.push(path);
            }
        }
    }
    let zen_config = config.join("zen");
    if zen_config.exists() {
        if let Ok(path) = write_manifest(&zen_config.join("native-messaging-hosts"), &firefox) {
            installed.push(path);
        }
    }
    let chromium = chromium_manifest(&host);
    for subdirectory in chromium_config_subdirs() {
        let base = config.join(subdirectory);
        if base.exists() {
            if let Ok(path) = write_manifest(&base.join("NativeMessagingHosts"), &chromium) {
                installed.push(path);
            }
        }
    }
    if installed.is_empty() {
        Err("no browser profile directories found".into())
    } else {
        Ok(installed)
    }
}

#[cfg(windows)]
pub fn register_native_host_manifests() -> Result<Vec<String>, String> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let host = host_binary()?;
    let host = host.to_string_lossy().into_owned();
    let directory = minidl_ipc::data_dir().join("native-messaging");
    std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    let firefox_file = PathBuf::from(write_manifest(&directory, &firefox_manifest(&host))?);
    let chromium_directory = directory.join("chromium");
    let chromium_file = PathBuf::from(write_manifest(
        &chromium_directory,
        &chromium_manifest(&host),
    )?);
    let root = RegKey::predef(HKEY_CURRENT_USER);
    let mut installed = Vec::new();
    for (base, manifest) in [
        ("Software\\Mozilla\\NativeMessagingHosts", &firefox_file),
        (
            "Software\\Google\\Chrome\\NativeMessagingHosts",
            &chromium_file,
        ),
        ("Software\\Chromium\\NativeMessagingHosts", &chromium_file),
        (
            "Software\\Microsoft\\Edge\\NativeMessagingHosts",
            &chromium_file,
        ),
        (
            "Software\\BraveSoftware\\Brave-Browser\\NativeMessagingHosts",
            &chromium_file,
        ),
        ("Software\\Vivaldi\\NativeMessagingHosts", &chromium_file),
    ] {
        let key = format!("{base}\\{NATIVE_HOST_NAME}");
        if let Ok((registry, _)) = root.create_subkey(&key) {
            if registry
                .set_value("", &manifest.to_string_lossy().to_string())
                .is_ok()
            {
                installed.push(format!("HKCU\\{key}"));
            }
        }
    }
    if installed.is_empty() {
        Err("failed to write native-messaging registry keys".into())
    } else {
        Ok(installed)
    }
}

#[cfg(not(any(unix, windows)))]
pub fn register_native_host_manifests() -> Result<Vec<String>, String> {
    Err("unsupported platform".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_json_requires_an_enabled_connector() {
        let firefox = r#"{"addons":[{"id":"minidownloader@ramazan.dev","active":true}]}"#;
        let chromium =
            r#"{"extensions":{"settings":{"hhaobmkdgijodfieadeeanjmnneckafj":{"state":1}}}}"#;
        assert_eq!(firefox_state_json(firefox), Some(true));
        assert_eq!(chromium_state_json(chromium), Some(true));
    }

    #[test]
    fn live_detection_requires_a_session_message() {
        let presence = Arc::new(Mutex::new(ConnectorPresence::default()));
        assert!(!status(&presence).firefox_detected);
        assert!(record(&presence, BrowserFamily::Firefox).firefox_detected);
    }

    #[cfg(unix)]
    #[test]
    fn firefox_family_roots_include_xdg_and_flatpak_zen_profiles() {
        let home = Path::new("/home/tester");
        let config = Path::new("/mnt/config");
        let roots = firefox_profile_roots_for(home, config);
        assert!(roots.contains(&config.join("zen")));
        assert!(roots.contains(&home.join(".var/app/app.zen_browser.zen/config/zen")));
    }
}
