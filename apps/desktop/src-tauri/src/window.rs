//! Shared window reveal logic.
//!
//! Restoring a window that was hidden to the tray is surprisingly fragile on
//! Linux: KWin/Wayland (and some GTK versions) leave a re-mapped window in a
//! half-managed state where the server-side titlebar buttons (minimize /
//! maximize / close) stop responding until the window is re-evaluated. Doing the
//! full unminimize → show → focus sequence and then briefly toggling
//! always-on-top forces the compositor to re-decorate and re-manage the window,
//! which restores the titlebar controls.

use tauri::{AppHandle, Manager, WebviewWindow};

/// Reveal + focus the given window, working around the Linux hide/show
/// decoration bug.
pub fn show(w: &WebviewWindow) {
    let _ = w.unminimize();
    let _ = w.show();
    let _ = w.set_focus();
    // Nudge the compositor to re-manage the freshly-mapped window so its
    // titlebar buttons respond again.
    let _ = w.set_always_on_top(true);
    let _ = w.set_always_on_top(false);
}

/// Reveal the main window if it exists.
pub fn reveal(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        show(&w);
    }
}

/// Hide the main window to the tray.
pub fn hide(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.hide();
    }
}
