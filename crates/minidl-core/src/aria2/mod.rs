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
pub use process::{Aria2Process, LaunchOptions, SessionMode};
pub use rpc::{new_gid, RpcClient, DETAIL_KEYS, POLL_KEYS, STATUS_KEYS};

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use anyhow::Result;
use serde::Serialize;
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;

/// Lifecycle of the aria2 process behind an [`Engine`]. The RPC client only
/// talks to aria2 while the state is `Ready`.
///
/// ```text
/// launch ─► Ready ─(child exits)─► Restarting{n} ─► Ready
///                                        └─(3 spawns fail)─► Failed ─(next tick)─► Restarting{n+1}
/// any ─(shutdown)─► Stopping
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum EngineState {
    Ready { port: u16 },
    Restarting { attempt: u32 },
    Failed { reason: String },
    Stopping,
}

/// A running aria2c plus an RPC client and a notification stream. A supervisor
/// task restarts aria2c if it dies unexpectedly.
pub struct Engine {
    pub rpc: RpcClient,
    events: broadcast::Sender<Aria2Event>,
    /// Ownership handle that keeps aria2c alive for the Engine's lifetime (the
    /// watchdog swaps in a respawned child).
    process: Arc<Mutex<Aria2Process>>,
    notify_task: Arc<Mutex<JoinHandle<()>>>,
    shutting_down: Arc<AtomicBool>,
    launch_options: Arc<RwLock<LaunchOptions>>,
    state: watch::Sender<EngineState>,
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
        let launch_options = Arc::new(RwLock::new(opts));
        let (state, _) = watch::channel(EngineState::Ready {
            port: process.lock().unwrap_or_else(|e| e.into_inner()).port,
        });

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
            let launch_options = launch_options.clone();
            let state = state.clone();
            tokio::spawn(async move {
                let mut attempt = 0u32;
                loop {
                    // shortcut: exit is noticed by polling, so calls in the
                    // first second after a crash still go to the old port;
                    // upgrade to a child-exit wait if that window matters.
                    tokio::time::sleep(Duration::from_secs(1)).await;
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
                    rpc.invalidate();
                    attempt += 1;
                    state.send_replace(EngineState::Restarting { attempt });
                    let options = launch_options
                        .read()
                        .unwrap_or_else(|error| error.into_inner())
                        .clone();
                    match Self::respawn(&options).await {
                        Ok(newp) if !shutting_down.load(Ordering::Relaxed) => {
                            let port = newp.port;
                            rpc.retarget(port, newp.secret.clone());
                            {
                                let mut nt = notify_task.lock().unwrap_or_else(|e| e.into_inner());
                                nt.abort();
                                *nt = notify::spawn_listener(port, events.clone());
                            }
                            *process.lock().unwrap_or_else(|e| e.into_inner()) = newp; // old child reaped
                            attempt = 0;
                            state.send_replace(EngineState::Ready { port });
                        }
                        Ok(_) => break,
                        Err(error) => {
                            state.send_replace(EngineState::Failed {
                                reason: format!("{error:#}"),
                            });
                        }
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
            launch_options,
            state,
            watchdog,
        })
    }

    /// Spawn aria2c with a port-retry (TOCTOU on the reused `:0` port) and wait
    /// for its RPC to answer. Returns the process + a fresh client.
    async fn spawn_ready(opts: &LaunchOptions) -> Result<(Aria2Process, RpcClient)> {
        let mut last_err = None;
        for _ in 0..3 {
            let mut proc = Aria2Process::spawn(opts)?;
            let client = RpcClient::new(proc.port, proc.secret.clone())?;
            match Self::wait_ready(&mut proc, &client).await {
                Ok(()) => return Ok((proc, client)),
                Err(e) => last_err = Some(e), // proc drops → stuck aria2c killed
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("aria2 RPC did not become ready")))
    }

    /// Wait for our own child's RPC. A dead child fails immediately instead of
    /// after the full timeout, and a reply on the port while the child is dead
    /// came from some other process, so it is never trusted with jobs.
    async fn wait_ready(proc: &mut Aria2Process, rpc: &RpcClient) -> Result<()> {
        for _ in 0..50 {
            let answered = rpc.get_version().await.is_ok();
            if proc.has_exited() {
                anyhow::bail!("aria2c exited during startup");
            }
            if answered {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        anyhow::bail!("aria2 RPC did not become ready")
    }

    /// Respawn aria2c and wait for it on a private client; the caller points
    /// the shared (cloned-everywhere) client at it once it is ready.
    async fn respawn(opts: &LaunchOptions) -> Result<Aria2Process> {
        let mut last_err = None;
        for _ in 0..3 {
            let mut proc = Aria2Process::respawn(opts)?;
            let rpc = RpcClient::new(proc.port, proc.secret.clone())?;
            match Self::wait_ready(&mut proc, &rpc).await {
                Ok(()) => return Ok(proc),
                Err(e) => last_err = Some(e),
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("aria2 RPC did not recover after restart")))
    }

    /// OS process id of the current aria2c child.
    pub fn pid(&self) -> u32 {
        self.process.lock().unwrap_or_else(|e| e.into_inner()).pid()
    }

    /// Watch the engine process lifecycle. The controller reconciles its rows
    /// against aria2 on every `Restarting → Ready`.
    pub fn state(&self) -> watch::Receiver<EngineState> {
        self.state.subscribe()
    }

    /// Subscribe to aria2 lifecycle notifications.
    pub fn subscribe(&self) -> broadcast::Receiver<Aria2Event> {
        self.events.subscribe()
    }

    /// Replace the options used by the crash watchdog on its next respawn.
    /// Options already applied over RPC take effect immediately; process-only
    /// options (DHT/sandbox) take effect on the next clean or watchdog launch.
    pub fn update_launch_options(&self, options: LaunchOptions) {
        *self
            .launch_options
            .write()
            .unwrap_or_else(|error| error.into_inner()) = options;
    }

    /// Graceful shutdown: stop the watchdog, flush the session, ask aria2 to stop.
    pub async fn shutdown(&self) {
        self.shutting_down.store(true, Ordering::Relaxed);
        self.state.send_replace(EngineState::Stopping);
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
