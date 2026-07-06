//! Walking-skeleton aria2 engine: spawn a private aria2c on a random loopback
//! port with a per-launch secret, and talk JSON-RPC to it over HTTP POST.
//!
//! This deliberately uses the leaner HTTP transport (not WebSocket) — the risk
//! being de-risked here is "does the bundled/aria2 spawn + RPC handshake work,"
//! not push latency. WebSocket notifications land in M0.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::Mutex;

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

/// Owns the aria2c child process and an HTTP JSON-RPC client to it.
pub struct Aria2 {
    endpoint: String,
    secret: String,
    http: reqwest::Client,
    /// Kept alive for the app's lifetime; `--stop-with-process` also reaps it if
    /// we die uncleanly.
    _child: Mutex<Child>,
}

/// Bind :0 to let the OS pick a free port, then drop the listener and reuse the
/// number. aria2 has no ephemeral-port mode, so we choose one ourselves.
fn free_port() -> Result<u16> {
    let l = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(l.local_addr()?.port())
}

/// 16 random bytes as hex, fresh per launch, memory-only. Linux-only urandom
/// read keeps this dependency-free.
fn random_secret() -> Result<String> {
    let mut b = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut b))
        .context("reading /dev/urandom")?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

/// Resolve the aria2c binary. TODO(M0+): prefer the bundled Tauri sidecar; for
/// now use the system binary on PATH.
fn resolve_aria2c() -> Result<PathBuf> {
    // Minimal PATH search without pulling the `which` crate.
    if let Ok(path) = std::env::var("PATH") {
        for dir in path.split(':') {
            let cand = Path::new(dir).join("aria2c");
            if cand.is_file() {
                return Ok(cand);
            }
        }
    }
    Err(anyhow!("aria2c not found on PATH — install aria2"))
}

impl Aria2 {
    pub fn spawn(download_dir: &Path) -> Result<Self> {
        let port = free_port()?;
        let secret = random_secret()?;
        let bin = resolve_aria2c()?;
        std::fs::create_dir_all(download_dir).ok();

        let child = Command::new(&bin)
            .arg("--enable-rpc=true")
            .arg("--rpc-listen-all=false")
            .arg(format!("--rpc-listen-port={port}"))
            .arg(format!("--rpc-secret={secret}"))
            .arg("--continue=true")
            .arg(format!("--dir={}", download_dir.display()))
            .arg(format!("--stop-with-process={}", std::process::id()))
            .spawn()
            .with_context(|| format!("failed to spawn {}", bin.display()))?;

        Ok(Self {
            endpoint: format!("http://127.0.0.1:{port}/jsonrpc"),
            secret,
            http: reqwest::Client::new(),
            _child: Mutex::new(child),
        })
    }

    /// One JSON-RPC call. `params` is the array *after* the secret; the token is
    /// spliced in as `params[0]`.
    async fn call(&self, method: &str, params: Value) -> Result<Value> {
        let mut arr = match params {
            Value::Array(a) => a,
            other => vec![other],
        };
        arr.insert(0, json!(format!("token:{}", self.secret)));

        let body = json!({ "jsonrpc": "2.0", "id": "ldm", "method": method, "params": arr });
        let resp: Value = self
            .http
            .post(&self.endpoint)
            .json(&body)
            .send()
            .await?
            .json()
            .await
            .context("aria2 RPC response was not JSON")?;

        if let Some(err) = resp.get("error") {
            return Err(anyhow!("aria2 error: {err}"));
        }
        Ok(resp.get("result").cloned().unwrap_or(Value::Null))
    }

    /// `aria2.addUri([url], options)` → GID.
    pub async fn add_uri(&self, url: &str, options: Value) -> Result<String> {
        let res = self.call("aria2.addUri", json!([[url], options])).await?;
        res.as_str()
            .map(String::from)
            .ok_or_else(|| anyhow!("addUri returned no GID"))
    }

    /// `aria2.tellActive(keys)` → array of status structs.
    pub async fn tell_active(&self) -> Result<Vec<Value>> {
        let keys = json!([
            "gid",
            "status",
            "totalLength",
            "completedLength",
            "downloadSpeed",
            "files"
        ]);
        let res = self.call("aria2.tellActive", json!([keys])).await?;
        Ok(res.as_array().cloned().unwrap_or_default())
    }

    /// Readiness probe.
    pub async fn get_version(&self) -> Result<String> {
        let res = self.call("aria2.getVersion", json!([])).await?;
        Ok(res
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string())
    }
}
