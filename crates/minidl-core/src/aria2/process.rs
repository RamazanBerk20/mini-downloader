//! Spawn and own a private `aria2c` daemon on a random loopback port with a
//! per-launch RPC secret.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};

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

impl Aria2Process {
    pub fn spawn(opts: &LaunchOptions) -> Result<Self> {
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
        // aria2 warns if --input-file is missing; ensure it exists (may be empty).
        if !session_path.exists() {
            std::fs::write(&session_path, b"").ok();
        }
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
            use std::io::Write;
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
            .arg(format!("--save-session={}", session_path.display()))
            .arg("--save-session-interval=30")
            .arg("--force-save=true")
            .arg(format!("--input-file={}", session_path.display()))
            .arg("--auto-save-interval=20")
            .arg(format!("--max-concurrent-downloads={}", opts.max_concurrent))
            .arg("--bt-save-metadata=true")
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
