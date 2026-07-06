//! Core engine for Linux Download Manager (GUI-agnostic).
//!
//! Modules are filled in across milestones (see the plan). Today this holds the
//! aria2 option mapping — pure, testable logic with no I/O.

pub mod aria2;

pub use ldm_ipc as ipc;
