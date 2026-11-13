//! Small, dependency-light per-user OS integrations.

use std::path::PathBuf;

#[cfg(unix)]
use std::io::Write as _;

/// Show `file` selected in the file manager where the platform can (the
/// freedesktop `FileManager1.ShowItems` call, `explorer /select,`), else open
/// `dir`. Never waits for the file manager itself.
pub async fn show_in_folder(
    file: Option<&std::path::Path>,
    dir: &std::path::Path,
) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    if let Some(file) = file {
        if show_items_over_dbus(file).await {
            return Ok(());
        }
    }
    #[cfg(windows)]
    if let Some(file) = file {
        use std::os::windows::process::CommandExt as _;
        let mut command = std::process::Command::new("explorer.exe");
        command.raw_arg(explorer_select_argument(file));
        return command.spawn().map(drop).map_err(|error| error.to_string());
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    let _ = file;
    open::that_detached(dir).map_err(|error| error.to_string())
}

/// `dbus-send` instead of a D-Bus crate: zbus is already in the binary with
/// the async-io runtime for the tray and AccessKit, and mixing its Tokio mode
/// in breaks them. A missing tool or file manager falls back to the folder.
#[cfg(target_os = "linux")]
async fn show_items_over_dbus(file: &std::path::Path) -> bool {
    let Some(uri) = file_uri(file) else {
        return false;
    };
    let mut command = tokio::process::Command::new("dbus-send");
    command
        .args([
            "--session",
            "--print-reply",
            "--dest=org.freedesktop.FileManager1",
            "--type=method_call",
            "/org/freedesktop/FileManager1",
            "org.freedesktop.FileManager1.ShowItems",
        ])
        .arg(format!("array:string:{uri}"))
        .arg("string:")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    matches!(
        tokio::time::timeout(std::time::Duration::from_secs(5), command.status()).await,
        Ok(Ok(status)) if status.success()
    )
}

/// A `file://` URI that `dbus-send`'s comma-separated array syntax cannot split.
#[cfg(any(target_os = "linux", test))]
fn file_uri(file: &std::path::Path) -> Option<String> {
    url::Url::from_file_path(file)
        .ok()
        .map(|uri| uri.as_str().replace(',', "%2C"))
}

/// explorer.exe parses its own command line: the path must be quoted after
/// the comma, which `Command::arg` quoting does not produce.
#[cfg(any(windows, test))]
fn explorer_select_argument(file: &std::path::Path) -> String {
    format!("/select,\"{}\"", file.display())
}

#[cfg(unix)]
const DESKTOP_ID: &str = "io.minidownloader.app.desktop";

#[cfg(unix)]
const USER_ICON_PNG: &[u8] = include_bytes!("../../../assets/icons/128x128.png");

pub(crate) fn executable() -> Result<PathBuf, String> {
    let current = std::env::current_exe().map_err(|error| error.to_string())?;
    #[cfg(target_os = "linux")]
    if current
        .to_string_lossy()
        .contains(&format!("{}{}", std::path::MAIN_SEPARATOR, ".mount_"))
    {
        if let Some(appimage) = std::env::var_os("APPIMAGE").map(PathBuf::from) {
            if appimage.is_absolute() && appimage.is_file() {
                return appimage.canonicalize().map_err(|error| error.to_string());
            }
        }
    }
    Ok(current)
}

#[cfg(unix)]
fn desktop_exec_escape(path: &std::path::Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('`', "\\`")
        .replace('$', "\\$")
        // A single percent introduces a Desktop Entry field code, even inside
        // quotes. `%%` is the literal-percent escape.
        .replace('%', "%%")
}

#[cfg(unix)]
fn desktop_entry() -> Result<(PathBuf, String), String> {
    let executable = executable()?;
    let escaped = desktop_exec_escape(&executable);
    let data = dirs::data_local_dir()
        .ok_or("local data directory unavailable")?
        .join("applications")
        .join(DESKTOP_ID);
    let entry = format!(
        "[Desktop Entry]\nType=Application\nName=Mini Downloader\nComment=Fast native download manager\nExec=\"{escaped}\" %U\nIcon=mini-downloader\nTerminal=false\nCategories=Network;FileTransfer;\nMimeType=x-scheme-handler/magnet;x-scheme-handler/minidownloader;application/x-bittorrent;application/metalink4+xml;\nStartupNotify=true\nStartupWMClass=mini-downloader\n"
    );
    Ok((data, entry))
}

/// Write only when the content differs. This runs on every launch, and each
/// write under `~/.local/share/applications` makes the desktop rebuild its
/// application cache.
#[cfg(unix)]
fn write_if_changed(path: &std::path::Path, contents: &[u8]) -> Result<(), String> {
    if std::fs::read(path).is_ok_and(|current| current == contents) {
        return Ok(());
    }
    std::fs::create_dir_all(path.parent().ok_or("path has no parent")?)
        .map_err(|error| error.to_string())?;
    std::fs::write(path, contents).map_err(|error| error.to_string())
}

#[cfg(unix)]
fn install_user_icon() -> Result<(), String> {
    let path = dirs::data_local_dir()
        .ok_or("local data directory unavailable")?
        .join("icons/hicolor/128x128/apps/mini-downloader.png");
    write_if_changed(&path, USER_ICON_PNG)
}

#[cfg(unix)]
pub fn register_private_scheme() -> Result<(), String> {
    let (path, entry) = desktop_entry()?;
    write_if_changed(&path, entry.as_bytes())?;
    install_user_icon()?;
    set_xdg_default("x-scheme-handler/minidownloader")
}

#[cfg(unix)]
fn set_xdg_default(mime: &str) -> Result<(), String> {
    let path = dirs::config_dir()
        .ok_or("configuration directory unavailable")?
        .join("mimeapps.list");
    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error.to_string()),
    };
    let updated = set_default_association(&contents, mime);
    if updated == contents {
        return Ok(());
    }
    write_atomic(&path, &updated)
}

#[cfg(unix)]
fn set_default_association(contents: &str, mime: &str) -> String {
    let mut output = String::with_capacity(contents.len() + mime.len() + DESKTOP_ID.len() + 32);
    let mut in_defaults = false;
    let mut found_defaults = false;
    let mut wrote_default = false;

    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            if in_defaults && !wrote_default {
                output.push_str(&format!("{mime}={DESKTOP_ID};\n"));
                wrote_default = true;
            }
            in_defaults = trimmed == "[Default Applications]";
            found_defaults |= in_defaults;
        }
        if in_defaults
            && line
                .split_once('=')
                .is_some_and(|(key, _)| key.trim() == mime)
        {
            if !wrote_default {
                output.push_str(&format!("{mime}={DESKTOP_ID};\n"));
                wrote_default = true;
            }
            continue;
        }
        output.push_str(line);
        output.push('\n');
    }

    if in_defaults && !wrote_default {
        output.push_str(&format!("{mime}={DESKTOP_ID};\n"));
    } else if !found_defaults {
        if !output.is_empty() && !output.ends_with("\n\n") {
            output.push('\n');
        }
        output.push_str(&format!("[Default Applications]\n{mime}={DESKTOP_ID};\n"));
    }
    output
}

#[cfg(unix)]
fn write_atomic(path: &std::path::Path, contents: &str) -> Result<(), String> {
    // Replace the file a symlink points at, not the link itself: dotfile
    // managers keep `mimeapps.list` as a symlink into their own tree.
    let path = &std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    let parent = path.parent().ok_or("configuration path has no parent")?;
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let mut staging = tempfile::NamedTempFile::new_in(parent).map_err(|error| error.to_string())?;
    staging
        .write_all(contents.as_bytes())
        .map_err(|error| error.to_string())?;
    staging
        .as_file()
        .sync_all()
        .map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        staging
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|error| error.to_string())?;
    }
    staging
        .persist(path)
        .map_err(|error| error.error.to_string())?;
    Ok(())
}

#[cfg(unix)]
pub fn set_magnet_handler(enabled: bool) -> Result<(), String> {
    let (path, entry) = desktop_entry()?;
    write_if_changed(&path, entry.as_bytes())?;
    install_user_icon()?;
    if enabled {
        set_xdg_default("x-scheme-handler/magnet")
    } else {
        remove_mime_association("x-scheme-handler/magnet")
    }
}

#[cfg(unix)]
fn remove_mime_association(mime: &str) -> Result<(), String> {
    let mut first_error = None;
    let mut files = Vec::new();
    if let Some(config) = dirs::config_dir() {
        files.push(config.join("mimeapps.list"));
    }
    if let Some(data) = dirs::data_local_dir() {
        files.push(data.join("applications/mimeapps.list"));
    }
    for path in files {
        let Ok(contents) = std::fs::read_to_string(&path) else {
            continue;
        };
        let mut changed = false;
        let mut output = String::with_capacity(contents.len());
        for line in contents.lines() {
            if let Some((key, values)) = line.split_once('=') {
                if key.trim() == mime {
                    let kept: Vec<_> = values
                        .split(';')
                        .filter(|value| !value.is_empty() && *value != DESKTOP_ID)
                        .collect();
                    if kept.len() != values.split(';').filter(|value| !value.is_empty()).count() {
                        changed = true;
                        if !kept.is_empty() {
                            output.push_str(&format!("{key}={};\n", kept.join(";")));
                        }
                        continue;
                    }
                }
            }
            output.push_str(line);
            output.push('\n');
        }
        if changed {
            if let Err(error) = std::fs::write(&path, output) {
                first_error.get_or_insert(error.to_string());
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}

#[cfg(unix)]
pub fn set_autostart(enabled: bool) -> Result<(), String> {
    let path = dirs::config_dir()
        .ok_or("configuration directory unavailable")?
        .join("autostart")
        .join(DESKTOP_ID);
    if !enabled {
        return match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.to_string()),
        };
    }
    let executable = desktop_exec_escape(&executable()?);
    let entry = format!(
        "[Desktop Entry]\nType=Application\nName=Mini Downloader\nExec=\"{executable}\" --background\nIcon=mini-downloader\nTerminal=false\nX-GNOME-Autostart-enabled=true\n"
    );
    write_if_changed(&path, entry.as_bytes())
}

#[cfg(windows)]
fn quoted_command(argument: &str) -> Result<String, String> {
    Ok(format!("\"{}\" {argument}", executable()?.display()))
}

#[cfg(windows)]
pub fn register_private_scheme() -> Result<(), String> {
    register_windows_scheme("minidownloader")
}

#[cfg(windows)]
fn register_windows_scheme(scheme: &str) -> Result<(), String> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;
    let root = RegKey::predef(HKEY_CURRENT_USER);
    let path = format!("Software\\Classes\\{scheme}");
    let (key, _) = root
        .create_subkey(&path)
        .map_err(|error| error.to_string())?;
    key.set_value("", &format!("URL:{scheme} Protocol"))
        .map_err(|error| error.to_string())?;
    key.set_value("URL Protocol", &"")
        .map_err(|error| error.to_string())?;
    let (command, _) = key
        .create_subkey("shell\\open\\command")
        .map_err(|error| error.to_string())?;
    command
        .set_value("", &quoted_command("\"%1\"")?)
        .map_err(|error| error.to_string())
}

#[cfg(windows)]
pub fn set_magnet_handler(enabled: bool) -> Result<(), String> {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};
    use winreg::RegKey;
    if enabled {
        return register_windows_scheme("magnet");
    }
    let root = RegKey::predef(HKEY_CURRENT_USER);
    let path = "Software\\Classes\\magnet";
    let ours = root
        .open_subkey_with_flags(format!("{path}\\shell\\open\\command"), KEY_READ)
        .ok()
        .and_then(|key| key.get_value::<String, _>("").ok())
        .is_some_and(|command| {
            executable()
                .ok()
                .is_some_and(|exe| command.contains(&exe.to_string_lossy().to_string()))
        });
    if ours {
        root.delete_subkey_all(path)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[cfg(windows)]
pub fn set_autostart(enabled: bool) -> Result<(), String> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;
    let root = RegKey::predef(HKEY_CURRENT_USER);
    let (run, _) = root
        .create_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Run")
        .map_err(|error| error.to_string())?;
    if enabled {
        run.set_value("MiniDownloader", &quoted_command("--background")?)
            .map_err(|error| error.to_string())
    } else {
        match run.delete_value("MiniDownloader") {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }
}

#[cfg(not(any(unix, windows)))]
pub fn register_private_scheme() -> Result<(), String> {
    Err("unsupported platform".into())
}
#[cfg(not(any(unix, windows)))]
pub fn set_magnet_handler(_enabled: bool) -> Result<(), String> {
    Err("unsupported platform".into())
}
#[cfg(not(any(unix, windows)))]
pub fn set_autostart(_enabled: bool) -> Result<(), String> {
    Err("unsupported platform".into())
}

#[cfg(test)]
mod reveal_tests {
    use super::*;

    #[test]
    fn reveal_arguments_select_the_file() {
        assert_eq!(
            explorer_select_argument(std::path::Path::new(r"C:\Users\a b\x,y.iso")),
            r#"/select,"C:\Users\a b\x,y.iso""#
        );
        #[cfg(unix)]
        assert_eq!(
            file_uri(std::path::Path::new("/home/u/a b,c.iso")).as_deref(),
            Some("file:///home/u/a%20b%2Cc.iso")
        );
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn desktop_entry_quotes_the_executable() {
        let (_, entry) = desktop_entry().unwrap();
        assert!(entry.contains("Exec=\""));
        assert!(entry.contains("x-scheme-handler/magnet"));
        assert!(entry.contains("StartupWMClass=mini-downloader"));
        assert_eq!(
            desktop_exec_escape(std::path::Path::new("/opt/100%/$app`name")),
            "/opt/100%%/\\$app\\`name"
        );
    }

    #[test]
    fn default_mime_association_is_inserted_and_replaced_without_duplicates() {
        let initial = "[Added Associations]\ntext/plain=editor.desktop;\n";
        let inserted = set_default_association(initial, "x-scheme-handler/minidownloader");
        assert!(inserted.contains(
            "[Default Applications]\nx-scheme-handler/minidownloader=io.minidownloader.app.desktop;\n"
        ));

        let existing = "[Default Applications]\nx-scheme-handler/minidownloader=old.desktop;\ntext/plain=editor.desktop;\n\n[Added Associations]\nx-scheme-handler/minidownloader=old.desktop;\n".to_owned();
        let replaced = set_default_association(&existing, "x-scheme-handler/minidownloader");
        assert_eq!(
            replaced
                .matches("x-scheme-handler/minidownloader=io.minidownloader.app.desktop;")
                .count(),
            1
        );
        assert!(!replaced.contains("x-scheme-handler/minidownloader=old.desktop;\ntext/plain"));
        assert!(
            replaced.contains("[Added Associations]\nx-scheme-handler/minidownloader=old.desktop;")
        );
    }

    #[test]
    fn atomic_write_keeps_a_symlinked_configuration_file_a_symlink() {
        let root = tempfile::tempdir().unwrap();
        let real = root.path().join("dotfiles/mimeapps.list");
        std::fs::create_dir_all(real.parent().unwrap()).unwrap();
        std::fs::write(&real, "old").unwrap();
        let link = root.path().join("mimeapps.list");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        write_atomic(&link, "new").unwrap();

        assert!(std::fs::symlink_metadata(&link).unwrap().is_symlink());
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "new");
    }

    #[test]
    fn default_association_is_stable_once_set() {
        let once = set_default_association("", "x-scheme-handler/magnet");
        assert_eq!(
            set_default_association(&once, "x-scheme-handler/magnet"),
            once
        );
    }

    #[test]
    fn atomic_user_configuration_write_is_private() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config/mimeapps.list");
        write_atomic(&path, "[Default Applications]\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[Default Applications]\n"
        );
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
