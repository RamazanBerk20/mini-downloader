//! Spawn and own a private `aria2c` daemon on a random loopback port with a
//! per-launch RPC secret.

use std::io::Read;
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

/// 16 random bytes as hex, fresh per launch, memory-only.
fn random_secret() -> Result<String> {
    let mut b = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut b))
        .context("reading /dev/urandom")?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

fn resolve_on_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|d| d.join(name))
            .find(|c| c.is_file())
    })
}

impl Aria2Process {
    pub fn spawn(opts: &LaunchOptions) -> Result<Self> {
        let port = free_port()?;
        let secret = random_secret()?;
        let bin = opts
            .aria2c_path
            .clone()
            .or_else(|| resolve_on_path("aria2c"))
            .ok_or_else(|| anyhow!("aria2c not found (no sidecar and not on PATH)"))?;

        std::fs::create_dir_all(&opts.download_dir).ok();
        std::fs::create_dir_all(&opts.data_dir).ok();
        let session_path = opts.data_dir.join("aria2.session");
        // aria2 warns if --input-file is missing; ensure it exists (may be empty).
        if !session_path.exists() {
            std::fs::write(&session_path, b"").ok();
        }
        let dht_path = opts.data_dir.join("dht.dat");

        let child = Command::new(&bin)
            .arg("--enable-rpc=true")
            .arg("--rpc-listen-all=false")
            .arg(format!("--rpc-listen-port={port}"))
            .arg(format!("--rpc-secret={secret}"))
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
            .arg("--quiet=true")
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
        explicit.clone().or_else(|| resolve_on_path("aria2c"))
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
