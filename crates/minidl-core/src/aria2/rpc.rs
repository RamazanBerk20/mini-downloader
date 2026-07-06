//! aria2 JSON-RPC over HTTP POST (request/reply). Notifications are handled
//! separately over WebSocket (see `notify.rs`).

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};

/// Lean key set for the 1 Hz progress poll — numbers only. Deliberately omits
/// `files`/`bittorrent`, which for a multi-thousand-file torrent would serialize
/// the entire file array + announce list every second only to be discarded.
pub const POLL_KEYS: &[&str] = &[
    "gid",
    "totalLength",
    "completedLength",
    "uploadLength",
    "downloadSpeed",
    "uploadSpeed",
    "connections",
    "numSeeders",
];

/// The status fields the UI/DB need. Keep this tight — aria2 serializes the
/// full struct otherwise, which is wasteful for large torrents.
pub const STATUS_KEYS: &[&str] = &[
    "gid",
    "status",
    "totalLength",
    "completedLength",
    "uploadLength",
    "downloadSpeed",
    "uploadSpeed",
    "connections",
    "numSeeders",
    "seeder",
    "errorCode",
    "errorMessage",
    "dir",
    "files",
    "following",
    "followedBy",
    "belongsTo",
    "bittorrent",
    "infoHash",
];

/// HTTP JSON-RPC client bound to one aria2 instance.
#[derive(Clone)]
pub struct RpcClient {
    endpoint: String,
    secret: String,
    http: reqwest::Client,
    next_id: std::sync::Arc<AtomicU64>,
}

fn keys_value(keys: &[&str]) -> Value {
    Value::Array(keys.iter().map(|k| json!(k)).collect())
}

impl RpcClient {
    pub fn new(port: u16, secret: impl Into<String>) -> Self {
        // Bound every call: an aria2 that is alive but wedged (SIGSTOP, stalled
        // disk/NFS) must not hang the poll loop forever — without a timeout the
        // request never returns and the `Err(_) => continue` fallback never fires.
        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            endpoint: format!("http://127.0.0.1:{port}/jsonrpc"),
            secret: secret.into(),
            http,
            next_id: std::sync::Arc::new(AtomicU64::new(1)),
        }
    }

    /// One JSON-RPC call. `params` are the args *after* the secret token.
    pub async fn call(&self, method: &str, params: Vec<Value>) -> Result<Value> {
        let mut arr = Vec::with_capacity(params.len() + 1);
        arr.push(json!(format!("token:{}", self.secret)));
        arr.extend(params);

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let body = json!({ "jsonrpc": "2.0", "id": id.to_string(), "method": method, "params": arr });

        let resp: Value = self
            .http
            .post(&self.endpoint)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("aria2 {method} request failed"))?
            .json()
            .await
            .with_context(|| format!("aria2 {method} response not JSON"))?;

        if let Some(err) = resp.get("error") {
            let msg = err.get("message").and_then(|m| m.as_str()).unwrap_or("unknown");
            return Err(anyhow!("aria2 {method} error: {msg}"));
        }
        Ok(resp.get("result").cloned().unwrap_or(Value::Null))
    }

    fn as_str(v: &Value) -> Result<String> {
        v.as_str().map(String::from).ok_or_else(|| anyhow!("expected string result, got {v}"))
    }

    fn as_array(v: Value) -> Vec<Value> {
        match v {
            Value::Array(a) => a,
            _ => Vec::new(),
        }
    }

    // ---- add ----

    pub async fn add_uri(&self, uris: &[String], options: Value) -> Result<String> {
        let res = self
            .call("aria2.addUri", vec![json!(uris), options])
            .await?;
        Self::as_str(&res)
    }

    pub async fn add_torrent(&self, torrent_b64: &str, uris: &[String], options: Value) -> Result<String> {
        let res = self
            .call("aria2.addTorrent", vec![json!(torrent_b64), json!(uris), options])
            .await?;
        Self::as_str(&res)
    }

    pub async fn add_metalink(&self, metalink_b64: &str, options: Value) -> Result<Vec<String>> {
        let res = self
            .call("aria2.addMetalink", vec![json!(metalink_b64), options])
            .await?;
        Ok(Self::as_array(res)
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect())
    }

    // ---- control ----

    pub async fn remove(&self, gid: &str) -> Result<Value> {
        self.call("aria2.remove", vec![json!(gid)]).await
    }
    pub async fn force_remove(&self, gid: &str) -> Result<Value> {
        self.call("aria2.forceRemove", vec![json!(gid)]).await
    }
    pub async fn remove_download_result(&self, gid: &str) -> Result<Value> {
        self.call("aria2.removeDownloadResult", vec![json!(gid)]).await
    }
    pub async fn pause(&self, gid: &str) -> Result<Value> {
        self.call("aria2.pause", vec![json!(gid)]).await
    }
    pub async fn unpause(&self, gid: &str) -> Result<Value> {
        self.call("aria2.unpause", vec![json!(gid)]).await
    }
    pub async fn pause_all(&self) -> Result<Value> {
        self.call("aria2.pauseAll", vec![]).await
    }
    pub async fn unpause_all(&self) -> Result<Value> {
        self.call("aria2.unpauseAll", vec![]).await
    }
    /// Reorder a queued download. `how` is `POS_SET`/`POS_CUR`/`POS_END`.
    pub async fn change_position(&self, gid: &str, pos: i64, how: &str) -> Result<Value> {
        self.call("aria2.changePosition", vec![json!(gid), json!(pos), json!(how)]).await
    }

    // ---- query ----

    pub async fn tell_status(&self, gid: &str, keys: &[&str]) -> Result<Value> {
        self.call("aria2.tellStatus", vec![json!(gid), keys_value(keys)]).await
    }
    pub async fn tell_active(&self, keys: &[&str]) -> Result<Vec<Value>> {
        let res = self.call("aria2.tellActive", vec![keys_value(keys)]).await?;
        Ok(Self::as_array(res))
    }
    pub async fn tell_waiting(&self, offset: i64, num: i64, keys: &[&str]) -> Result<Vec<Value>> {
        let res = self
            .call("aria2.tellWaiting", vec![json!(offset), json!(num), keys_value(keys)])
            .await?;
        Ok(Self::as_array(res))
    }
    pub async fn tell_stopped(&self, offset: i64, num: i64, keys: &[&str]) -> Result<Vec<Value>> {
        let res = self
            .call("aria2.tellStopped", vec![json!(offset), json!(num), keys_value(keys)])
            .await?;
        Ok(Self::as_array(res))
    }
    pub async fn get_global_stat(&self) -> Result<Value> {
        self.call("aria2.getGlobalStat", vec![]).await
    }
    pub async fn get_files(&self, gid: &str) -> Result<Vec<Value>> {
        let res = self.call("aria2.getFiles", vec![json!(gid)]).await?;
        Ok(Self::as_array(res))
    }

    // ---- options ----

    pub async fn change_option(&self, gid: &str, options: Value) -> Result<Value> {
        self.call("aria2.changeOption", vec![json!(gid), options]).await
    }
    pub async fn change_global_option(&self, options: Value) -> Result<Value> {
        self.call("aria2.changeGlobalOption", vec![options]).await
    }

    // ---- lifecycle ----

    pub async fn get_version(&self) -> Result<String> {
        let res = self.call("aria2.getVersion", vec![]).await?;
        Ok(res.get("version").and_then(|v| v.as_str()).unwrap_or("unknown").to_string())
    }
    pub async fn save_session(&self) -> Result<Value> {
        self.call("aria2.saveSession", vec![]).await
    }
    pub async fn shutdown(&self) -> Result<Value> {
        self.call("aria2.shutdown", vec![]).await
    }

    /// Poll `getVersion` until aria2's RPC answers or the attempts run out.
    pub async fn wait_ready(&self, attempts: u32, delay: std::time::Duration) -> Result<()> {
        for _ in 0..attempts {
            if self.get_version().await.is_ok() {
                return Ok(());
            }
            tokio::time::sleep(delay).await;
        }
        Err(anyhow!("aria2 RPC did not become ready"))
    }
}
