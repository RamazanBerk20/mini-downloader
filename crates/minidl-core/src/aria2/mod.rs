//! aria2 engine: process supervision, HTTP JSON-RPC, and WebSocket
//! notifications, wrapped in one [`Engine`] facade.

mod event;
mod notify;
mod options;
mod process;
mod rpc;
#[cfg(target_os = "linux")]
pub(crate) mod sandbox;

pub use event::{Aria2Event, Aria2Status};
pub use options::{build_add_options, EngineDefaults};
pub use process::{Aria2Process, LaunchOptions};
pub use rpc::{RpcClient, DETAIL_KEYS, POLL_KEYS, STATUS_KEYS};

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

/// A running aria2c plus an RPC client and a notification stream. A supervisor
/// task restarts aria2c if it dies unexpectedly.
pub struct Engine {
    pub rpc: RpcClient,
    events: broadcast::Sender<Aria2Event>,
    /// Ownership handle that keeps aria2c alive for the Engine's lifetime (the
    /// watchdog mutates its own clone). Never read directly.
    #[allow(dead_code)]
    process: Arc<Mutex<Aria2Process>>,
    notify_task: Arc<Mutex<JoinHandle<()>>>,
    shutting_down: Arc<AtomicBool>,
    watchdog: JoinHandle<()>,
}

impl Engine {
    /// Spawn aria2c, wait for its RPC, start the notification listener, and a
    /// watchdog that respawns aria2c if it exits unexpectedly.
    pub async fn launch(opts: LaunchOptions) -> Result<Self> {
        let (process, rpc) = Self::spawn_ready(&opts).await?;

        let (events, _rx) = broadcast::channel(256);
        let notify_task = notify::spawn_listener(process.port, events.clone());

        let process = Arc::new(Mutex::new(process));
        let notify_task = Arc::new(Mutex::new(notify_task));
        let shutting_down = Arc::new(AtomicBool::new(false));

        // If aria2c is OOM-killed or crashes, respawn it and retarget the RPC
        // client + notification listener at the new port. Cold startup pauses
        // and cleans its session before exposing controls; in-process recovery
        // keeps that already-clean session so an explicit transfer can recover.
        // Only an *unexpected* exit triggers this — `shutdown()` sets the flag
        // first so a graceful stop isn't fought.
        let watchdog = {
            let process = process.clone();
            let notify_task = notify_task.clone();
            let rpc = rpc.clone();
            let events = events.clone();
            let shutting_down = shutting_down.clone();
            let opts = opts.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    if shutting_down.load(Ordering::Relaxed) {
                        break;
                    }
                    let exited = {
                        let mut p = process.lock().unwrap_or_else(|e| e.into_inner());
                        p.has_exited()
                    };
                    if !exited || shutting_down.load(Ordering::Relaxed) {
                        continue;
                    }
                    if let Ok(newp) = Self::respawn(&opts, &rpc).await {
                        let port = newp.port;
                        {
                            let mut nt = notify_task.lock().unwrap_or_else(|e| e.into_inner());
                            nt.abort();
                            *nt = notify::spawn_listener(port, events.clone());
                        }
                        let mut p = process.lock().unwrap_or_else(|e| e.into_inner());
                        *p = newp; // old Aria2Process drops → its (dead) child reaped
                    }
                }
            })
        };

        Ok(Self {
            rpc,
            events,
            process,
            notify_task,
            shutting_down,
            watchdog,
        })
    }

    /// Spawn aria2c with a port-retry (TOCTOU on the reused `:0` port) and wait
    /// for its RPC to answer. Returns the process + a fresh client.
    async fn spawn_ready(opts: &LaunchOptions) -> Result<(Aria2Process, RpcClient)> {
        let mut last_err = None;
        for _ in 0..3 {
            let proc = Aria2Process::spawn(opts)?;
            let client = RpcClient::new(proc.port, proc.secret.clone());
            match client.wait_ready(50, Duration::from_millis(100)).await {
                Ok(()) => return Ok((proc, client)),
                Err(e) => last_err = Some(e), // proc drops → stuck aria2c killed
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("aria2 RPC did not become ready")))
    }

    /// Respawn aria2c and point the existing (cloned-everywhere) RPC client at it.
    async fn respawn(opts: &LaunchOptions, rpc: &RpcClient) -> Result<Aria2Process> {
        let mut last_err = None;
        for _ in 0..3 {
            let proc = Aria2Process::respawn(opts)?;
            rpc.retarget(proc.port, proc.secret.clone());
            match rpc.wait_ready(50, Duration::from_millis(100)).await {
                Ok(()) => return Ok(proc),
                Err(e) => last_err = Some(e),
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("aria2 RPC did not recover after restart")))
    }

    /// Subscribe to aria2 lifecycle notifications.
    pub fn subscribe(&self) -> broadcast::Receiver<Aria2Event> {
        self.events.subscribe()
    }

    /// Graceful shutdown: stop the watchdog, flush the session, ask aria2 to stop.
    pub async fn shutdown(&self) {
        self.shutting_down.store(true, Ordering::Relaxed);
        let _ = self.rpc.save_session().await;
        let _ = self.rpc.shutdown().await;
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.shutting_down.store(true, Ordering::Relaxed);
        self.watchdog.abort();
        if let Ok(nt) = self.notify_task.lock() {
            nt.abort();
        }
    }
}
