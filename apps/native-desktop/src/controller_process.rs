use super::*;

pub(super) async fn run_controller(launch_commands: Vec<ControlCommand>) -> Result<(), String> {
    let background = std::env::args().any(|arg| arg == "--background" || arg == "--minimized");
    if server::send_one(ControlCommand::Ping).await.is_ok() {
        forward_to_controller(&launch_commands, background).await;
        return Ok(());
    }
    let _instance_guard = match server::try_acquire_instance().map_err(|error| error.to_string())? {
        Some(guard) => guard,
        None => {
            // The winner may still be opening SQLite and binding its socket.
            for _ in 0..40 {
                if server::send_one(ControlCommand::Ping).await.is_ok() {
                    forward_to_controller(&launch_commands, background).await;
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            return Err(
                "another controller owns the instance lock but did not become ready".into(),
            );
        }
    };

    let controller = Controller::open_default()
        .await
        .map_err(|error| error.message)?;
    let benchmark = std::env::var_os("MINIDL_BENCHMARK").is_some();
    if !benchmark {
        if let Err(error) = minidl_app::platform::register_private_scheme() {
            eprintln!("URL scheme registration failed: {error}");
        }
        let handle_magnets = controller
            .db()
            .get_setting("handle_magnets")
            .ok()
            .flatten()
            .as_deref()
            != Some("false");
        if let Err(error) = minidl_app::platform::set_magnet_handler(handle_magnets) {
            eprintln!("magnet handler registration failed: {error}");
        }
        if let Some(enabled) = controller.db().get_setting("autostart").ok().flatten() {
            if let Err(error) = minidl_app::platform::set_autostart(enabled == "true") {
                eprintln!("autostart registration failed: {error}");
            }
        }
        if let Err(error) = minidl_app::connector::register_native_host_manifests() {
            eprintln!("browser bridge registration skipped: {error}");
        }
    }
    // The UI child's lifecycle: closed or open. "Show" (tray, --show, a
    // forwarded link) is one event: it spawns a closed UI, and an open UI
    // raises its own window when it sees the same event.
    let ui_running = Arc::new(AtomicBool::new(false));
    let mut control = tokio::spawn(server::serve_control(controller.clone()));
    let mut bridge = tokio::spawn(server::serve_bridge(controller.clone()));
    let mut scheduler = tokio::spawn(server::run_scheduler(controller.clone()));
    server::run_clipboard_watcher(controller.clone());
    spawn_notification_adapter(controller.clone());

    // Let the socket listener bind before the first UI client connects.
    tokio::time::sleep(Duration::from_millis(40)).await;
    if control.is_finished() || bridge.is_finished() || scheduler.is_finished() {
        return Err("a controller background service stopped during startup".into());
    }
    if !background {
        spawn_ui_client(ui_running.clone());
    }
    for command in launch_commands {
        if let Err(error) = controller.execute(command).await {
            eprintln!("{}", error.message);
        }
    }

    #[cfg(any(target_os = "linux", windows))]
    let tray_labels = TrayLabels::from_controller(&controller);

    #[cfg(target_os = "linux")]
    let (mut _tray_handle, mut tray_actions) = match linux_tray(tray_labels).await {
        Ok((handle, actions)) => (Some(handle), actions),
        Err(error) => {
            eprintln!("system tray unavailable: {error}");
            let (_sender, actions) = tokio::sync::mpsc::unbounded_channel();
            (None, actions)
        }
    };
    #[cfg(windows)]
    let (mut _tray_thread, mut tray_actions) = match windows_tray(tray_labels) {
        Ok((thread, actions)) => (Some(thread), actions),
        Err(error) => {
            eprintln!("system tray unavailable: {error}");
            let (_sender, actions) = tokio::sync::mpsc::unbounded_channel();
            (None, actions)
        }
    };
    #[cfg(not(any(target_os = "linux", windows)))]
    let mut tray_actions = {
        let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
        rx
    };

    #[cfg(target_os = "linux")]
    controller.set_tray_available(_tray_handle.is_some());
    #[cfg(windows)]
    controller.set_tray_available(_tray_thread.is_some());
    #[cfg(not(any(target_os = "linux", windows)))]
    controller.set_tray_available(false);

    let mut tray_available = true;
    let mut events = controller.events().subscribe();
    loop {
        tokio::select! {
            event = events.recv() => match event {
                Ok(event) if event.0.event == "ui:show" => spawn_ui_client(ui_running.clone()),
                Ok(event) if event.0.event == "app:quit" => break,
                #[cfg(target_os = "linux")]
                Ok(event) if event.0.event == "settings:changed" => {
                    if let Some(handle) = &_tray_handle {
                        relabel_linux_tray(handle, &controller).await;
                    }
                }
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            },
            action = tray_actions.recv(), if tray_available => match action {
                Some(TrayAction::Show) => { let _ = controller.execute(ControlCommand::Show).await; }
                Some(TrayAction::PauseAll) => { let _ = controller.execute(ControlCommand::PauseAll).await; }
                Some(TrayAction::ResumeAll) => { let _ = controller.execute(ControlCommand::ResumeAll).await; }
                Some(TrayAction::Quit) => { let _ = controller.execute(ControlCommand::Quit).await; break; }
                None => tray_available = false,
            },
            _ = tokio::signal::ctrl_c() => {
                let _ = controller.execute(ControlCommand::Quit).await;
                break;
            },
            result = &mut control => {
                return Err(service_exit("control socket", result));
            },
            result = &mut bridge => {
                return Err(service_exit("browser bridge", result));
            },
            result = &mut scheduler => {
                if controller.should_quit() {
                    break;
                }
                return Err(match result {
                    Ok(()) => "scheduler stopped unexpectedly".into(),
                    Err(error) => format!("scheduler task failed: {error}"),
                });
            },
        }
    }
    control.abort();
    bridge.abort();
    scheduler.abort();
    Ok(())
}

fn service_exit(
    name: &str,
    result: Result<Result<(), minidl_app::controller::AppError>, tokio::task::JoinError>,
) -> String {
    match result {
        Ok(Ok(())) => format!("{name} stopped unexpectedly"),
        Ok(Err(error)) => format!("{name} failed: {}", error.message),
        Err(error) => format!("{name} task failed: {error}"),
    }
}

async fn forward_to_controller(launch_commands: &[ControlCommand], background: bool) {
    for command in launch_commands {
        if let Err(error) = server::send_one(command.clone()).await {
            eprintln!("{error}");
        }
    }
    // Also after adding: a magnet or link opened from the browser while the
    // app sits in the tray would otherwise give no sign that anything happened.
    if !background {
        let _ = server::send_one(ControlCommand::Show).await;
    }
}

fn spawn_ui_client(running: Arc<AtomicBool>) {
    if running.swap(true, Ordering::SeqCst) {
        return;
    }
    tokio::spawn(async move {
        let result = async {
            let executable = std::env::current_exe()?;
            let mut command = tokio::process::Command::new(executable);
            command
                .arg("--ui-client")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            // XWayland for the window on request (winit 0.30 picks Wayland
            // whenever WAYLAND_DISPLAY is set).
            if std::env::var_os("MINIDL_UI_X11").is_some_and(|value| value == "1") {
                command.env_remove("WAYLAND_DISPLAY");
            }
            let mut child = command.spawn()?;
            child.wait().await
        }
        .await;
        if let Err(error) = result {
            eprintln!("launch UI: {error}");
        }
        running.store(false, Ordering::SeqCst);
    });
}

fn spawn_notification_adapter(controller: Arc<Controller>) {
    tokio::spawn(async move {
        let mut events = controller.events().subscribe();
        loop {
            let event = match events.recv().await {
                Ok(event) => event,
                // Missing a few notifications must not stop all later ones.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            };
            let locale = controller
                .db()
                .get_setting("locale")
                .ok()
                .flatten()
                .unwrap_or_default();
            let (title, body) = match event.0.event.as_str() {
                "downloads:complete" => (
                    minidl_core::i18n::tr(&locale, minidl_core::i18n::Msg::DownloadComplete)
                        .to_owned(),
                    event
                        .0
                        .payload
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("Download")
                        .to_owned(),
                ),
                "downloads:error" => (
                    minidl_core::i18n::tr(&locale, minidl_core::i18n::Msg::DownloadFailed)
                        .to_owned(),
                    event
                        .0
                        .payload
                        .get("message")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("Unknown error")
                        .to_owned(),
                ),
                _ => continue,
            };
            let body = notification_text(&body);
            tokio::task::spawn_blocking(move || {
                let _ = notify_rust::Notification::new()
                    .appname("Mini Downloader")
                    .summary(&title)
                    .body(&body)
                    .show();
            });
        }
    });
}

/// Notification servers that support `body-markup` interpret `<b>`, `<a>` and
/// `<img>`; a server-chosen file name must stay plain text and must not
/// reorder itself with bidi controls.
pub(crate) fn notification_text(text: &str) -> String {
    ui::state::display_safe(text)
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
