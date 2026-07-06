//! Core engine for Linux Download Manager (GUI-agnostic).
//!
//! - [`aria2`]: process supervision + JSON-RPC + notifications.
//! - [`db`]: SQLite persistence.
//! - [`model`]: domain types.
//! - [`paths`]: XDG paths.

pub mod aria2;
pub mod db;
pub mod model;
pub mod paths;

pub use ldm_ipc as ipc;
