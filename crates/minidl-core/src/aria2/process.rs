//! Spawn and own a private `aria2c` daemon on a random loopback port with a
//! per-launch RPC secret.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::io::Write;

use anyhow::{anyhow, Context, Result};

#[derive(Debug, Clone)]
pub struct LaunchOptions {
    /// Explicit aria2c path (e.g. a resolved Tauri sidecar). Falls back to PATH.
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
        }
    }
}

/// A running aria2c child plus the connection parameters to reach it.
pub struct Aria2Process {
    child: Child,
    pub port: u16,
    pub secret: String,
    pub session_path: PathBuf,
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
    if !entry.iter().any(|line| line.trim_start().starts_with("gid=")) {
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
        let temp = parent.join(format!(".{name}.{}.{}.tmp", std::process::id(), stamp + attempt as u128));
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

        std::fs::create_dir_all(&opts.download_dir).ok();
        std::fs::create_dir_all(&opts.data_dir).ok();
        // Keep the data dir private: the session file below holds replayed
        // `Cookie:`/`Authorization:` header lines and signed query tokens.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&opts.data_dir, std::fs::Permissions::from_mode(0o700));
        }
        let session_path = opts.data_dir.join("aria2.session");
        // aria2 warns if --input-file is missing. A cold launch rewrites every
        // saved job with pause=true; if that write fails, don't restore it.
        let use_session = if pause_session {
            pause_startup_session_file(&session_path).is_ok()
        } else if !session_path.exists() {
            std::fs::write(&session_path, b"").is_ok()
        } else {
            true
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&session_path, std::fs::Permissions::from_mode(0o600));
        }
        let dht_path = opts.data_dir.join("dht.dat");

        // Keep the RPC secret off argv: a second local user could otherwise read
        // it from `/proc/<pid>/cmdline` (or `ps`) and drive our aria2 (arbitrary
        // file write via addUri `dir`/`out`). On unix pass it through a 0600 conf
        // file; Windows has no `/proc` and no unix perms API, so fall back to argv.
        #[cfg(unix)]
        let secret_arg = {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            let conf_path = opts.data_dir.join("aria2.conf");
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&conf_path)
                .context("create aria2 rpc-secret conf")?;
            writeln!(f, "rpc-secret={secret}").context("write aria2 rpc-secret conf")?;
            drop(f);
            // `.mode()` only applies on creation; enforce 0600 if the file pre-existed.
            std::fs::set_permissions(&conf_path, std::fs::Permissions::from_mode(0o600)).ok();
            format!("--conf-path={}", conf_path.display())
        };
        #[cfg(not(unix))]
        let secret_arg = format!("--rpc-secret={secret}");

        let mut cmd = Command::new(&bin);
        cmd.arg("--enable-rpc=true")
            .arg("--rpc-listen-all=false")
            .arg(format!("--rpc-listen-port={port}"))
            .arg(&secret_arg)
            // Large torrent/metalink payloads arrive base64 over RPC.
            .arg("--rpc-max-request-size=32M")
            .arg("--continue=true")
            .arg("--always-resume=true")
            .arg(format!("--dir={}", opts.download_dir.display()))
            .arg(format!("--stop-with-process={}", std::process::id()))
            .arg(format!("--max-concurrent-downloads={}", opts.max_concurrent))
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
        if let Some(proxy) = opts.proxy.as_deref().filter(|p| !p.is_empty()) {
            cmd.arg(format!("--all-proxy={proxy}"));
        }
        if use_session {
            cmd.arg(format!("--save-session={}", session_path.display()))
                .arg("--save-session-interval=30")
                .arg(format!("--input-file={}", session_path.display()))
                // Retain partial-file state while a user-started transfer is active.
                .arg("--auto-save-interval=20");
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
            unsafe {
                std::os::unix::process::CommandExt::pre_exec(&mut cmd, move || {
                    crate::aria2::sandbox::restrict(&write_dirs);
                    Ok(())
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
        })
    }

    pub fn resolve_aria2c(explicit: &Option<PathBuf>) -> Option<PathBuf> {
        explicit.clone().or_else(|| crate::paths::resolve_tool("aria2c"))
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
        assert!(std::fs::read_dir(&root)
            .unwrap()
            .all(|entry| !entry.unwrap().file_name().to_string_lossy().ends_with(".tmp")));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn malformed_session_blocks_are_dropped() {
        let raw = "https://example.invalid/no-gid\n out=file\n";

        assert!(pause_startup_session(raw).is_empty());
    }
}
