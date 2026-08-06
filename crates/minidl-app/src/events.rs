use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use minidl_ipc::ControlEvent;
use serde::Serialize;
use tokio::sync::broadcast;

#[derive(Debug, Clone)]
pub struct AppEvent(pub ControlEvent);

/// In-process event fan-out. Slow/disconnected UI clients cannot block the
/// engine; they recover by requesting a fresh snapshot on reconnect.
#[derive(Clone)]
pub struct EventBus {
    sequence: Arc<AtomicU64>,
    tx: broadcast::Sender<AppEvent>,
}

impl Default for EventBus {
    fn default() -> Self {
        let (tx, _) = broadcast::channel(512);
        Self {
            sequence: Arc::new(AtomicU64::new(0)),
            tx,
        }
    }
}

impl EventBus {
    pub fn subscribe(&self) -> broadcast::Receiver<AppEvent> {
        self.tx.subscribe()
    }

    pub fn emit(&self, event: impl Into<String>, payload: impl Serialize) {
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed) + 1;
        let payload = serde_json::to_value(payload).unwrap_or(serde_json::Value::Null);
        let _ = self.tx.send(AppEvent(ControlEvent {
            sequence,
            event: event.into(),
            payload,
        }));
    }

    pub fn sequence(&self) -> u64 {
        self.sequence.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn events_are_monotonic() {
        let bus = EventBus::default();
        let mut rx = bus.subscribe();
        bus.emit("one", 1);
        bus.emit("two", 2);
        let one = rx.recv().await.unwrap().0;
        let two = rx.recv().await.unwrap().0;
        assert_eq!(one.sequence + 1, two.sequence);
    }
}
