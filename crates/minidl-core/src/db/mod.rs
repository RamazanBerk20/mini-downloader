//! SQLite persistence. The database owns durable metadata and the last-known
//! snapshot; aria2 owns live transfer state.

mod categories;
mod dedup;
mod downloads;
mod packages;
mod schedules;
mod schema;
mod settings;

use std::sync::{Arc, Mutex};

use rusqlite::Connection;

pub use dedup::{duplicate_sources_match, DownloadInsertResult};

/// Cloneable handle to the serialized SQLite connection.
#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

impl Db {
    /// Lock the connection, recovering from a poisoned mutex instead of
    /// cascading a prior worker panic into every later database operation.
    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|error| error.into_inner())
    }
}

include!("tests.rs");
