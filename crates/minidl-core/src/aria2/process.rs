//! Spawn and own a private `aria2c` daemon on a random loopback port with a
//! per-launch RPC secret.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};

use anyhow::{anyhow, Context, Result};

#[derive(Debug, Clone)]
pub struct LaunchOptions {
    /// Explicit aria2c path (for example a bundled executable). Falls back to PATH.
    pub aria2c_path: Option<PathBuf>,
    pub download_dir: PathBuf,
    /// App data dir; holds the session + DHT files.
    pub data_dir: PathBuf,
    pub max_concurrent: u32,
    /// Optional `all-proxy` value (http/https/socks) for aria2.
    pub proxy: Option<String>,
    /// BitTorrent DHT / PEX / LPD (announce to the swarm). Off = more private.
    pub dht: bool,
    /// Confine the aria2c child to write only under download_dir/data_dir via
    /// Landlock (Linux ≥5.13; no-op elsewhere). Opt-in.
    pub sandbox: bool,
    /// Global download/upload caps in bytes/sec (0 = unlimited). Launch
    /// options so the cap survives idle shutdown and crash respawn.
    pub download_limit: i64,
    pub upload_limit: i64,
}

impl Default for LaunchOptions {
    fn default() -> Self {
        Self {
            aria2c_path: None,
            download_dir: PathBuf::new(),
            data_dir: PathBuf::new(),
            max_concurrent: 5,
            proxy: None,
            dht: true,
            sandbox: false,
            download_limit: 0,
            upload_limit: 0,
        }
    }
}

/// How a launch treats aria2's session file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionMode {
    /// Cold application start: every saved job was rewritten as paused first.
    ColdPaused,
    /// Crash respawn: the app-owned session is restored as it was saved.
    Warm,
    /// The saved session could not be prepared, so it is not restored; new
    /// jobs are still saved to it.
    Disabled(String),
}

/// A running aria2c child plus the connection parameters to reach it.
pub struct Aria2Process {
    child: Child,
    pub port: u16,
    pub secret: String,
    pub session_path: PathBuf,
    pub session_mode: SessionMode,
}

/// Bind :0, read the assigned port, drop the listener, reuse the number.
fn free_port() -> Result<u16> {
    let l = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(l.local_addr()?.port())
}

/// 16 random bytes as hex, fresh per launch. On unix it is handed to aria2 via a
/// 0600 conf file (never argv — `/proc/<pid>/cmdline` is world-readable).
fn random_secret() -> Result<String> {
    let mut b = [0u8; 16];
    getrandom::getrandom(&mut b).context("getrandom")?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

/// Rewrite every valid saved entry as paused before aria2 reads it. Ownership is
/// checked through RPC immediately after launch, while this first barrier makes
/// it impossible for an old session entry to transfer bytes during startup.
fn pause_startup_session(raw: &str) -> String {
    let mut paused = String::new();
    let mut entry = Vec::new();

    for line in raw.lines() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        let is_option = line.starts_with(' ') || line.starts_with('\t');
        if !is_option && !entry.is_empty() {
            append_paused_session_entry(&mut paused, &entry);
            entry.clear();
        }
        entry.push(line);
    }
    append_paused_session_entry(&mut paused, &entry);
    paused
}

fn append_paused_session_entry(output: &mut String, entry: &[&str]) {
    // A GID is mandatory for saved aria2 sessions. Dropping malformed blocks is
    // safer than allowing arbitrary legacy input to run at startup.
    if !entry
        .iter()
        .any(|line| line.trim_start().starts_with("gid="))
    {
        return;
    }
    for line in entry {
        if line.trim_start().starts_with("pause=") {
            continue;
        }
        output.push_str(line);
        output.push('\n');
    }
    output.push_str(" pause=true\n");
}

/// Replace a session file only after its complete paused form is durably staged
/// beside it. A direct `write(path, ...)` truncates the only resume record
/// before the write finishes; a crash or I/O error there would lose partial
/// download state. Same-directory rename makes the visible replacement atomic.
fn write_file_atomically(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("session path has no parent"))?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("aria2.session");

    for attempt in 0..32u32 {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let temp = parent.join(format!(
            ".{name}.{}.{}.tmp",
            std::process::id(),
            stamp + attempt as u128
        ));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = match options.open(&temp) {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err).context("create staged aria2 session"),
        };
        if let Err(err) = file.write_all(contents).and_then(|_| file.sync_all()) {
            drop(file);
            let _ = std::fs::remove_file(&temp);
            return Err(err).context("write staged aria2 session");
        }
        drop(file);
        if let Err(err) = std::fs::rename(&temp, path) {
            let _ = std::fs::remove_file(&temp);
            return Err(err).context("replace aria2 session");
        }
        #[cfg(unix)]
        if let Ok(dir) = std::fs::File::open(parent) {
            // Best-effort directory fsync makes the completed rename durable
            // across a power loss; the replacement is already safe either way.
            let _ = dir.sync_all();
        }
        return Ok(());
    }
    Err(anyhow!("could not allocate a staged aria2 session file"))
}

fn pause_startup_session_file(session_path: &Path) -> Result<()> {
    let raw = match std::fs::read_to_string(session_path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err).context("read saved aria2 session"),
    };
    write_file_atomically(session_path, pause_startup_session(&raw).as_bytes())
}

impl Aria2Process {
    /// Spawn for a cold application launch. Every restored job is paused before
    /// aria2 starts; the desktop layer then removes entries not owned by a
    /// current database row before exposing any Resume controls.
    pub fn spawn(opts: &LaunchOptions) -> Result<Self> {
        Self::spawn_with_session(opts, true)
    }

    /// Respawn after aria2 crashes while the application remains alive. The
    /// cold-start cleanup already removed stale entries, so preserve the current
    /// app-owned session for an explicitly-started transfer.
    pub(crate) fn respawn(opts: &LaunchOptions) -> Result<Self> {
        Self::spawn_with_session(opts, false)
    }

    fn spawn_with_session(opts: &LaunchOptions, pause_session: bool) -> Result<Self> {
        let port = free_port()?;
        let secret = random_secret()?;
        let bin = opts
            .aria2c_path
            .clone()
            .or_else(|| crate::paths::resolve_tool("aria2c"))
            .ok_or_else(|| anyhow!("aria2c not found (no sidecar and not on PATH)"))?;

        std::fs::create_dir_all(&opts.download_dir).context("create download directory")?;
        std::fs::create_dir_all(&opts.data_dir).context("create private app data directory")?;
        // Keep the data dir private: the session file below holds replayed
        // `Cookie:`/`Authorization:` header lines and signed query tokens.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&opts.data_dir, std::fs::Permissions::from_mode(0o700))
                .context("protect app data directory")?;
        }
        let session_path = opts.data_dir.join("aria2.session");
        // aria2 warns if --input-file is missing. A cold launch rewrites every
        // saved job with pause=true; if that write fails, don't restore it,
        // but keep saving so jobs added in this run still survive a restart.
        let session_mode = if pause_session {
            match pause_startup_session_file(&session_path) {
                Ok(()) => SessionMode::ColdPaused,
                Err(error) => SessionMode::Disabled(format!("{error:#}")),
            }
        } else if !session_path.exists() {
            match std::fs::write(&session_path, b"") {
                Ok(()) => SessionMode::Warm,
                Err(error) => SessionMode::Disabled(error.to_string()),
            }
        } else {
            SessionMode::Warm
        };
        if let SessionMode::Disabled(reason) = &session_mode {
            eprintln!("aria2 session not restored: {reason}");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&session_path, std::fs::Permissions::from_mode(0o600));
        }
        let dht_path = opts.data_dir.join("dht.dat");

        // Keep RPC credentials and proxy credentials off argv. The app data
        // directory is private on Unix and inherits the user's private AppData
        // ACL on Windows.
        let conf_path = opts.data_dir.join("aria2.conf");
        let mut conf_options = std::fs::OpenOptions::new();
        conf_options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            conf_options.mode(0o600);
        }
        let mut conf = conf_options
            .open(&conf_path)
            .context("create private aria2 config")?;
        writeln!(conf, "rpc-secret={secret}").context("write aria2 RPC secret")?;
        if let Some(proxy) = opts.proxy.as_deref().filter(|value| !value.is_empty()) {
            if proxy.contains(['\r', '\n', '\0']) {
                return Err(anyhow!("proxy contains a forbidden control character"));
            }
            writeln!(conf, "all-proxy={proxy}").context("write aria2 proxy")?;
        }
        conf.flush().context("flush private aria2 config")?;
        conf.sync_all().context("sync private aria2 config")?;
        drop(conf);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&conf_path, std::fs::Permissions::from_mode(0o600))
                .context("protect private aria2 config")?;
        }
        let conf_arg = format!("--conf-path={}", conf_path.display());

        let mut cmd = Command::new(&bin);
        crate::child_process::hide_console_window(&mut cmd);
        cmd.arg("--enable-rpc=true")
            .arg("--rpc-listen-all=false")
            .arg(format!("--rpc-listen-port={port}"))
            .arg(&conf_arg)
            // Large torrent/metalink payloads arrive base64 over RPC.
            .arg("--rpc-max-request-size=32M")
            // No --continue: it makes aria2 append to or overwrite a same-named
            // file that has no control file. Resume uses the control file.
            .arg("--always-resume=true")
            .arg(format!("--dir={}", opts.download_dir.display()))
            .arg(format!("--stop-with-process={}", std::process::id()))
            .arg(format!(
                "--max-concurrent-downloads={}",
                opts.max_concurrent
            ))
            .arg(format!(
                "--max-overall-download-limit={}",
                opts.download_limit.max(0)
            ))
            .arg(format!(
                "--max-overall-upload-limit={}",
                opts.upload_limit.max(0)
            ))
            .arg("--bt-save-metadata=true")
            // Required for addTorrent/addMetalink jobs to survive --save-session.
            .arg("--rpc-save-upload-metadata=true")
            .arg(format!("--dht-file-path={}", dht_path.display()))
            .arg("--check-certificate=true")
            // Quieter stdout; RPC is the interface.
            .arg("--quiet=true");

        // BitTorrent swarm participation — DHT/PEX/LPD announce our IP; off is
        // more private (opt-out for privacy-conscious users).
        let dht = if opts.dht { "true" } else { "false" };
        cmd.arg(format!("--enable-dht={dht}"))
            .arg(format!("--enable-dht6={dht}"))
            .arg(format!("--bt-enable-lpd={dht}"))
            .arg(format!("--enable-peer-exchange={dht}"));
        cmd.arg(format!("--save-session={}", session_path.display()))
            .arg("--save-session-interval=30")
            // Retain partial-file state while a user-started transfer is active.
            .arg("--auto-save-interval=20");
        if !matches!(session_mode, SessionMode::Disabled(_)) {
            cmd.arg(format!("--input-file={}", session_path.display()));
        }

        // Optional Landlock confinement of the child (Linux ≥ 5.13). Off by
        // default; when on, the child may write only under download_dir/data_dir/tmp.
        #[cfg(target_os = "linux")]
        if opts.sandbox {
            let write_dirs = vec![
                opts.download_dir.clone(),
                opts.data_dir.clone(),
                std::path::PathBuf::from("/tmp"),
            ];
            let mut ruleset = Some(
                crate::aria2::sandbox::prepare(&write_dirs)
                    .map_err(|error| anyhow!("prepare aria2 sandbox: {error}"))?,
            );
            // SAFETY: the closure only performs syscalls (no allocation, no
            // locks), as required between fork and exec.
            unsafe {
                std::os::unix::process::CommandExt::pre_exec(&mut cmd, move || {
                    crate::aria2::sandbox::enforce(ruleset.take())
                });
            }
        }

        let child = cmd
            .spawn()
            .with_context(|| format!("failed to spawn {}", bin.display()))?;

        Ok(Self {
            child,
            port,
            secret,
            session_path,
            session_mode,
        })
    }

    pub fn resolve_aria2c(explicit: &Option<PathBuf>) -> Option<PathBuf> {
        explicit
            .clone()
            .or_else(|| crate::paths::resolve_tool("aria2c"))
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// True once the child has exited — the watchdog's restart trigger.
    pub fn has_exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }

    pub fn data_session_path(data_dir: &Path) -> PathBuf {
        data_dir.join("aria2.session")
    }
}

impl Drop for Aria2Process {
    fn drop(&mut self) {
        // Best-effort reap; --stop-with-process also handles the crash case.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_session_forces_every_entry_paused() {
        let raw = concat!(
            "https://known.example/file\n",
            " gid=known-gid\n",
            " pause=false\n",
            " out=file\n",
            "https://stale.example/file\n",
            " gid=stale-gid\n",
            " out=stale\n",
        );

        let paused = pause_startup_session(raw);

        assert!(paused.contains("https://known.example/file"));
        assert!(paused.contains("https://stale.example/file"));
        assert!(!paused.contains("pause=false"));
        assert_eq!(paused.matches(" pause=true\n").count(), 2);
    }

    #[test]
    fn startup_session_file_is_staged_then_replaced() {
        let root = std::env::temp_dir().join(format!(
            "minidl-session-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let session = root.join("aria2.session");
        std::fs::write(
            &session,
            "https://example.invalid/file\n gid=known-gid\n pause=false\n",
        )
        .unwrap();

        pause_startup_session_file(&session).unwrap();

        let rewritten = std::fs::read_to_string(&session).unwrap();
        assert!(rewritten.contains("gid=known-gid"));
        assert!(rewritten.contains("pause=true"));
        assert!(!rewritten.contains("pause=false"));
        assert!(std::fs::read_dir(&root).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")));
        let _ = std::fs::remove_dir_all(root);
    }

    /// An unwritable session must not silently turn persistence off: aria2
    /// still saves the jobs of this run.
    #[cfg(unix)]
    #[test]
    fn unpreparable_session_still_saves_new_jobs() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        // A directory where the session file should be makes the rewrite fail.
        std::fs::create_dir(data.join("aria2.session")).unwrap();
        let fake = root.path().join("aria2c");
        std::fs::write(
            &fake,
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$(dirname \"$0\")/argv\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut process = Aria2Process::spawn(&LaunchOptions {
            aria2c_path: Some(fake),
            download_dir: root.path().join("dl"),
            data_dir: data.clone(),
            ..Default::default()
        })
        .unwrap();
        assert!(matches!(process.session_mode, SessionMode::Disabled(_)));
        let _ = process.child.wait();
        let argv = std::fs::read_to_string(root.path().join("argv")).unwrap();
        assert!(argv.contains("--save-session="));
        assert!(!argv.contains("--input-file="));
        assert!(!argv.contains("--continue"));
    }

    #[test]
    fn malformed_session_blocks_are_dropped() {
        let raw = "https://example.invalid/no-gid\n out=file\n";

        assert!(pause_startup_session(raw).is_empty());
    }
}
