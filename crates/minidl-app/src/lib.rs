//! Framework-independent application orchestration for Mini Downloader.
//!
//! The controller daemon and every UI implementation use this crate. It owns
//! durable state, child processes and domain commands; UI frameworks are only
//! adapters around its command/event boundary.

mod completion;
pub mod connector;
pub mod controller;
pub mod engine;
pub mod events;
pub mod ingest;
pub mod platform;
pub mod reconcile;
pub mod server;
pub mod updater;
pub mod ytdlp;

pub use controller::{AppSnapshot, Controller};
pub use engine::EngineSupervisor;
pub use events::{AppEvent, EventBus};
