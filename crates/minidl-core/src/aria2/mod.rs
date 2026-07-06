//! aria2 engine: process supervision, HTTP JSON-RPC, and WebSocket
//! notifications, wrapped in one [`Engine`] facade.

mod event;
mod notify;
mod options;
mod process;
mod rpc;

pub use event::{Aria2Event, Aria2Status};
pub use options::{build_add_options, EngineDefaults};
pub use process::{Aria2Process, LaunchOptions};
pub use rpc::{RpcClient, STATUS_KEYS};

use std::time::Duration;

use anyhow::Result;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

/// A running aria2c plus an RPC client and a notification stream.
pub struct Engine {
    pub rpc: RpcClient,
    events: broadcast::Sender<Aria2Event>,
    _process: Aria2Process,
    _notify_task: JoinHandle<()>,
}

impl Engine {
    /// Spawn aria2c, wait for its RPC, and start the notification listener.
    pub async fn launch(opts: LaunchOptions) -> Result<Self> {
        // `free_port()` binds :0 then drops the listener, so another process can
        // steal the port before aria2c binds it (TOCTOU). If RPC never comes up,
        // retry with a fresh port/process a couple of times before giving up.
        let mut last_err = None;
        let mut process = None;
        let mut rpc = None;
        for _ in 0..3 {
            let proc = Aria2Process::spawn(&opts)?;
            let client = RpcClient::new(proc.port, proc.secret.clone());
            match client.wait_ready(50, Duration::from_millis(100)).await {
                Ok(()) => {
                    process = Some(proc);
                    rpc = Some(client);
                    break;
                }
                Err(e) => {
                    last_err = Some(e);
                    // `proc` drops here → the stuck aria2c is killed before retry.
                }
            }
        }
        let process = process.ok_or_else(|| {
            last_err.unwrap_or_else(|| anyhow::anyhow!("aria2 RPC did not become ready"))
        })?;
        let rpc = rpc.expect("rpc set when process is set");

        let (events, _rx) = broadcast::channel(256);
        let notify_task = notify::spawn_listener(process.port, events.clone());

        Ok(Self {
            rpc,
            events,
            _process: process,
            _notify_task: notify_task,
        })
    }

    /// Subscribe to aria2 lifecycle notifications.
    pub fn subscribe(&self) -> broadcast::Receiver<Aria2Event> {
        self.events.subscribe()
    }

    /// Graceful shutdown: flush the session, then ask aria2 to stop.
    pub async fn shutdown(&self) {
        let _ = self.rpc.save_session().await;
        let _ = self.rpc.shutdown().await;
    }
}
