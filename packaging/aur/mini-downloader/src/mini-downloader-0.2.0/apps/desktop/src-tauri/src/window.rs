//! Shared show/hide logic for the tray.
//!
//! Known upstream bug: on Wayland, hiding then showing a window leaves the
//! titlebar buttons (minimize / maximize / close) unresponsive —
//! <https://github.com/tauri-apps/tauri/issues/11856> /
//! <https://github.com/tauri-apps/tao/issues/1046>. Fixed in tao by
//! <https://github.com/tauri-apps/tao/pull/1218> (unreleased as of tao 0.35).
//! Until that ships, the endorsed workaround is to toggle the `resizable`
//! property whenever the window gains focus, which re-syncs the decorations
//! invisibly. See `install_decoration_fix`.

use tauri::{AppHandle, Manager, WebviewWindow};

/// Hide the window to the tray.
pub fn hide_to_tray(w: &WebviewWindow) {
    let _ = w.hide();
}

/// Restore + focus the window from the tray.
pub fn show(w: &WebviewWindow) {
    let _ = w.unminimize();
    let _ = w.show();
    let _ = w.set_focus();
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


/// Wayland decoration workaround (see module docs): every time the window is
/// focused, toggle `resizable` off/on to re-wire the titlebar buttons after a
/// hide/show remap. Harmless no-op elsewhere; remove once tao 0.36 ships.
pub fn install_decoration_fix(w: &WebviewWindow) {
    #[cfg(target_os = "linux")]
    {
        let win = w.clone();
        w.on_window_event(move |event| {
            if let tauri::WindowEvent::Focused(true) = event {
                let _ = win.set_resizable(false);
                let _ = win.set_resizable(true);
            }
        });
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = w;
    }
}
