//! Clipboard monitor: polls the clipboard (there is no change event) behind a
//! toggle and offers to grab detected URLs/magnets. Runs in Rust so it works
//! while the window is hidden to tray.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_clipboard_manager::ClipboardExt;

fn is_link(t: &str) -> bool {
    let l = t.to_ascii_lowercase();
    !t.chars().any(char::is_whitespace)
        && (l.starts_with("http://")
            || l.starts_with("https://")
            || l.starts_with("ftp://")
            || l.starts_with("magnet:?"))
}

pub fn spawn(app: AppHandle, on: Arc<AtomicBool>) {
    tauri::async_runtime::spawn(async move {
        let mut last = String::new();
        loop {
            tokio::time::sleep(Duration::from_millis(800)).await;
            if !on.load(Ordering::Relaxed) {
                continue;
            }
            let Ok(text) = app.clipboard().read_text() else {
                continue;
            };
            let t = text.trim().to_string();
            if t.is_empty() || t == last {
                continue;
            }
            last = t.clone();
            if is_link(&t) {
                // Magnet handling off → don't even offer clipboard magnets.
                if t.to_ascii_lowercase().starts_with("magnet:") {
                    let skip = app
                        .try_state::<crate::state::AppState>()
                        .map(|s| {
                            s.db.get_setting("handle_magnets")
                                .ok()
                                .flatten()
                                .map(|v| v == "false")
                                .unwrap_or(false)
                        })
                        .unwrap_or(false);
                    if skip {
                        continue;
                    }
                }
                let _ = app.emit("clipboard:detected", json!({ "url": t }));
            }
        }
    });
}
