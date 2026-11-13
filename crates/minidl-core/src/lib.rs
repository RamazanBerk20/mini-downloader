//! Core engine for Mini Downloader (GUI-agnostic).
//!
//! - [`aria2`]: process supervision + JSON-RPC + notifications.
//! - [`db`]: SQLite persistence.
//! - [`model`]: domain types.
//! - [`paths`]: XDG paths.

pub mod aria2;
pub mod categories;
pub mod child_process;
pub mod db;
pub mod files;
pub mod grabber;
pub mod i18n;
pub mod model;
pub mod paths;
pub mod redirect;
pub mod ytdlp;

pub use minidl_ipc as ipc;
