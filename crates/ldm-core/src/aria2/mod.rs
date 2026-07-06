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
        let process = Aria2Process::spawn(&opts)?;
        let rpc = RpcClient::new(process.port, process.secret.clone());
        rpc.wait_ready(50, Duration::from_millis(100)).await?;

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
