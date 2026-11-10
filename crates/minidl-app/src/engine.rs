use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::Result;
use minidl_core::aria2::{Engine, LaunchOptions};
use tokio::sync::{watch, Mutex};

struct Session {
    id: u64,
    engine: Arc<Engine>,
    stop: watch::Sender<bool>,
}

#[derive(Clone)]
pub struct EngineHandle {
    pub id: u64,
    pub engine: Arc<Engine>,
    pub stop: watch::Receiver<bool>,
}

/// Lazily owns aria2 and tears it down after an explicit idle arm. A generation
/// counter makes stale timers harmless when new work arrives during the grace
/// period.
pub struct EngineSupervisor {
    options: RwLock<LaunchOptions>,
    engine: Mutex<Option<Session>>,
    generation: AtomicU64,
    idle_delay: Duration,
}

impl EngineSupervisor {
    pub fn new(options: LaunchOptions) -> Arc<Self> {
        Self::with_idle_delay(options, Duration::from_secs(30))
    }

    pub fn with_idle_delay(options: LaunchOptions, idle_delay: Duration) -> Arc<Self> {
        Arc::new(Self {
            options: RwLock::new(options),
            engine: Mutex::new(None),
            generation: AtomicU64::new(0),
            idle_delay,
        })
    }

    pub async fn get(self: &Arc<Self>) -> Result<EngineHandle> {
        self.cancel_idle_shutdown();
        let mut slot = self.engine.lock().await;
        if let Some(session) = slot.as_ref() {
            return Ok(EngineHandle {
                id: session.id,
                engine: session.engine.clone(),
                stop: session.stop.subscribe(),
            });
        }
        let options = self
            .options
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let engine = Arc::new(Engine::launch(options).await?);
        let id = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let (stop, stop_rx) = watch::channel(false);
        *slot = Some(Session {
            id,
            engine: engine.clone(),
            stop,
        });
        Ok(EngineHandle {
            id,
            engine,
            stop: stop_rx,
        })
    }

    pub async fn current(&self) -> Option<Arc<Engine>> {
        self.engine
            .lock()
            .await
            .as_ref()
            .map(|session| session.engine.clone())
    }

    pub async fn is_running(&self) -> bool {
        self.engine.lock().await.is_some()
    }

    pub fn cancel_idle_shutdown(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
    }

    /// Schedule a graceful stop. Calling this again replaces the previous arm;
    /// calling [`Self::get`] invalidates it immediately.
    pub fn arm_idle_shutdown(self: &Arc<Self>) {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let this = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(this.idle_delay).await;
            let session = {
                let mut slot = this.engine.lock().await;
                // Recheck while owning the slot. `get()` may invalidate this
                // timer after the first check but before the mutex is acquired.
                if this.generation.load(Ordering::SeqCst) != generation {
                    return;
                }
                slot.take()
            };
            if let Some(session) = session {
                let _ = session.stop.send(true);
                session.engine.shutdown().await;
            }
        });
    }

    async fn publish_options(&self, options: LaunchOptions) {
        *self
            .options
            .write()
            .unwrap_or_else(|error| error.into_inner()) = options.clone();
        if let Some(engine) = self.current().await {
            engine.update_launch_options(options);
        }
    }

    pub async fn update_runtime_options(
        &self,
        max_concurrent: u32,
        proxy: Option<String>,
        dht: bool,
        sandbox: bool,
    ) {
        let mut options = self
            .options
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        options.max_concurrent = max_concurrent;
        options.proxy = proxy;
        options.dht = dht;
        options.sandbox = sandbox;
        self.publish_options(options).await;
    }

    pub async fn update_max_concurrent(&self, maximum: u32) {
        let mut options = self
            .options
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        options.max_concurrent = maximum;
        self.publish_options(options).await;
    }

    pub async fn update_speed_limits(&self, download: i64, upload: i64) {
        let mut options = self
            .options
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        options.download_limit = download;
        options.upload_limit = upload;
        self.publish_options(options).await;
    }

    pub async fn update_proxy(&self, proxy: Option<String>) {
        let mut options = self
            .options
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        options.proxy = proxy;
        self.publish_options(options).await;
    }

    pub async fn shutdown(&self) {
        self.cancel_idle_shutdown();
        if let Some(session) = self.engine.lock().await.take() {
            let _ = session.stop.send(true);
            session.engine.shutdown().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_advances_generation() {
        let supervisor =
            EngineSupervisor::with_idle_delay(LaunchOptions::default(), Duration::from_millis(1));
        let before = supervisor.generation.load(Ordering::SeqCst);
        supervisor.cancel_idle_shutdown();
        assert!(supervisor.generation.load(Ordering::SeqCst) > before);
    }

    #[tokio::test]
    async fn runtime_options_survive_future_lazy_launches() {
        let supervisor =
            EngineSupervisor::with_idle_delay(LaunchOptions::default(), Duration::from_millis(1));
        supervisor
            .update_runtime_options(9, Some("http://proxy.example:8080".into()), false, true)
            .await;
        let options = supervisor
            .options
            .read()
            .unwrap_or_else(|error| error.into_inner());
        assert_eq!(options.max_concurrent, 9);
        assert_eq!(options.proxy.as_deref(), Some("http://proxy.example:8080"));
        assert!(!options.dht);
        assert!(options.sandbox);
    }
}
