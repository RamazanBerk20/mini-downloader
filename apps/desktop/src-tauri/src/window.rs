//! Shared show/hide logic for the tray.
//!
//! Hiding to tray uses `hide()` (a Wayland client cannot unminimize itself, so
//! minimize is a dead end for restoring). The catch: `hide()`/`show()` unmaps
//! and remaps the surface, and KWin/Wayland then leaves the titlebar buttons
//! (minimize / maximize / close) unresponsive until a geometry/state change
//! re-syncs the decorations — exactly what a manual maximize double-click does.
//! So on restore we nudge the window size by a pixel and back, which sends the
//! configure round-trip that re-wires the decorations, with no lasting change.

use tauri::{AppHandle, Manager, WebviewWindow};

/// Hide the window to the tray.
pub fn hide_to_tray(w: &WebviewWindow) {
    let _ = w.hide();
}

/// Restore + focus the window from the tray, re-waking its titlebar controls.
pub fn show(w: &WebviewWindow) {
    let w = w.clone();
    tauri::async_runtime::spawn(async move {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();

        #[cfg(target_os = "linux")]
        {
            use std::time::Duration;
            tokio::time::sleep(Duration::from_millis(60)).await;
            if let Ok(sz) = w.inner_size() {
                let bigger = tauri::PhysicalSize::new(sz.width + 1, sz.height + 1);
                let _ = w.set_size(bigger);
                tokio::time::sleep(Duration::from_millis(40)).await;
                let _ = w.set_size(sz);
            }
            let _ = w.set_focus();
        }
    });
}

/// True when the window is currently hidden to the tray.
pub fn is_tucked(w: &WebviewWindow) -> bool {
    !w.is_visible().unwrap_or(true)
}

/// Reveal the main window if it exists.
pub fn reveal(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        show(&w);
    }
}

/// Hide the main window to the tray if it exists.
pub fn hide(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        hide_to_tray(&w);
    }
}
