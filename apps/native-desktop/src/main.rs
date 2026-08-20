#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::process::Stdio;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use minidl_app::server::{self, ControlClient};
use minidl_app::{AppSnapshot, Controller};
use minidl_ipc::{ControlCommand, MediaOptions, PlaylistSelection, SettingsUpdate};
use slint::{ComponentHandle, Model, ModelRc, VecModel};

mod controller_process;
mod launch;
mod locale;
mod tray;
mod ui;

use controller_process::run_controller;
use launch::{command_for_argument, command_for_path};
use tray::*;
use ui::run_ui;

type SharedCatalog = Arc<RwLock<locale::Catalog>>;

slint::include_modules!();

fn main() {
    launch::run();
}

#[cfg(test)]
use ui::view_models::*;
#[cfg(test)]
use ui::*;

include!("tests.rs");
