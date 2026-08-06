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

mod locale;

type SharedCatalog = Arc<RwLock<locale::Catalog>>;

slint::include_modules!();

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments
        .iter()
        .any(|argument| argument == "--version" || argument == "-V")
    {
        println!("mini-downloader {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        // aria2/yt-dlp do the heavy work in child processes. One async worker
        // is enough for sockets, timers and UI snapshots and saves an idle OS
        // thread in both controller and renderer processes.
        .worker_threads(1)
        .enable_all()
        .thread_name("minidl-worker")
        .build()
        .expect("create runtime");
    if let Some(command) = arguments
        .iter()
        .find_map(|argument| match argument.as_str() {
            "--quit" => Some(ControlCommand::Quit),
            "--hide" => Some(ControlCommand::Hide),
            "--show" => Some(ControlCommand::Show),
            _ => None,
        })
    {
        match runtime.block_on(server::send_one(command.clone())) {
            Ok(_) => return,
            Err(error) if matches!(command, ControlCommand::Show) => {
                eprintln!("no running controller; starting one: {error}");
            }
            Err(error) => {
                eprintln!("controller unavailable: {error}");
                return;
            }
        }
    }
    let ui_client = arguments.iter().any(|argument| argument == "--ui-client");
    if ui_client {
        // Slint's Linux accessibility bridge uses zbus, which discovers the
        // active Tokio reactor while constructing the window.
        let _runtime_guard = runtime.enter();
        #[cfg(target_os = "linux")]
        if let Err(error) = select_linux_ui_backend() {
            eprintln!("native UI backend: {error}");
            return;
        }
        if let Err(error) = run_ui(&runtime) {
            eprintln!("native UI: {error}");
        }
    } else {
        let launch_commands = arguments
            .iter()
            .filter(|argument| !argument.starts_with("--"))
            .filter_map(|argument| command_for_argument(argument))
            .collect();
        if let Err(error) = runtime.block_on(run_controller(launch_commands)) {
            eprintln!("controller: {error}");
        }
    }
}

#[cfg(target_os = "linux")]
fn select_linux_ui_backend() -> Result<(), String> {
    use slint::winit_030::winit::platform::wayland::WindowAttributesExtWayland as WaylandAttributes;
    use slint::winit_030::winit::platform::x11::WindowAttributesExtX11 as X11Attributes;

    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some()
        && std::env::var("WINIT_UNIX_BACKEND").as_deref() != Ok("x11");
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .with_winit_window_attributes_hook(move |attributes| {
            if wayland {
                WaylandAttributes::with_name(attributes, "io.minidownloader.app", "mini-downloader")
            } else {
                X11Attributes::with_name(attributes, "mini-downloader", "mini-downloader")
            }
        })
        .select()
        .map_err(|error| error.to_string())
}

fn command_for_path(path: &std::path::Path) -> Option<ControlCommand> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    let kind = match extension.as_str() {
        "torrent" => minidl_ipc::DownloadKind::Torrent,
        "meta4" | "metalink" => minidl_ipc::DownloadKind::Metalink,
        _ => return None,
    };
    Some(ControlCommand::IngestFile {
        path: path.to_string_lossy().into_owned(),
        kind,
    })
}

fn command_for_argument(argument: &str) -> Option<ControlCommand> {
    if let Ok(url) = url::Url::parse(argument) {
        if url.scheme() == "file" {
            return url
                .to_file_path()
                .ok()
                .as_deref()
                .and_then(command_for_path);
        }
        if matches!(url.scheme(), "http" | "https" | "ftp" | "magnet") {
            return Some(ControlCommand::IngestUrl {
                url: argument.to_owned(),
                checksum: None,
            });
        }
        if url.scheme() == "minidownloader" {
            return url
                .query_pairs()
                .find(|(key, _)| key == "url")
                .and_then(|(_, value)| command_for_argument(&value));
        }
    }
    command_for_path(std::path::Path::new(argument))
}

async fn run_controller(launch_commands: Vec<ControlCommand>) -> Result<(), String> {
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
        let _ = controller.execute(command).await;
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

    let mut tray_available = true;
    let mut events = controller.events().subscribe();
    loop {
        tokio::select! {
            event = events.recv() => match event {
                Ok(event) if event.0.event == "ui:show" => spawn_ui_client(ui_running.clone()),
                Ok(event) if event.0.event == "app:quit" => break,
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            },
            action = tray_actions.recv(), if tray_available => match action {
                Some(TrayAction::Show) => spawn_ui_client(ui_running.clone()),
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
        let _ = server::send_one(command.clone()).await;
    }
    if !background && launch_commands.is_empty() {
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
            let mut child = tokio::process::Command::new(executable)
                .arg("--ui-client")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?;
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
        while let Ok(event) = events.recv().await {
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

#[derive(Clone, Copy)]
enum TrayAction {
    Show,
    PauseAll,
    ResumeAll,
    Quit,
}

#[cfg(any(target_os = "linux", windows))]
#[derive(Clone)]
struct TrayLabels {
    show: String,
    pause_all: String,
    resume_all: String,
    quit: String,
}

#[cfg(any(target_os = "linux", windows))]
impl TrayLabels {
    fn from_controller(controller: &Controller) -> Self {
        let setting = controller.db().get_setting("locale").ok().flatten();
        let catalog = locale::Catalog::new(locale::selected(setting.as_deref()));
        Self {
            show: catalog.get("trayShow").to_owned(),
            pause_all: catalog.get("pauseAll").to_owned(),
            resume_all: catalog.get("resumeAll").to_owned(),
            quit: catalog.get("ocQuit").to_owned(),
        }
    }
}

#[cfg(target_os = "linux")]
#[derive(Clone)]
struct LinuxTray {
    actions: tokio::sync::mpsc::UnboundedSender<TrayAction>,
    labels: TrayLabels,
}

#[cfg(target_os = "linux")]
impl ksni::Tray for LinuxTray {
    fn id(&self) -> String {
        "io.minidownloader.app".into()
    }
    fn title(&self) -> String {
        "Mini Downloader".into()
    }
    fn icon_name(&self) -> String {
        "mini-downloader".into()
    }
    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        vec![canonical_linux_tray_icon()]
    }
    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.actions.send(TrayAction::Show);
    }
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::StandardItem;
        let item = |label: &str, action| {
            let actions = self.actions.clone();
            StandardItem {
                label: label.into(),
                activate: Box::new(move |_| {
                    let _ = actions.send(action);
                }),
                ..Default::default()
            }
            .into()
        };
        vec![
            item(&self.labels.show, TrayAction::Show),
            item(&self.labels.pause_all, TrayAction::PauseAll),
            item(&self.labels.resume_all, TrayAction::ResumeAll),
            ksni::MenuItem::Separator,
            item(&self.labels.quit, TrayAction::Quit),
        ]
    }
}

#[cfg(target_os = "linux")]
fn canonical_linux_tray_icon() -> ksni::Icon {
    ksni::Icon {
        width: 32,
        height: 32,
        data: include_bytes!(concat!(env!("OUT_DIR"), "/app-icon.argb")).to_vec(),
    }
}

#[cfg(target_os = "linux")]
async fn linux_tray(
    labels: TrayLabels,
) -> Result<
    (
        ksni::Handle<LinuxTray>,
        tokio::sync::mpsc::UnboundedReceiver<TrayAction>,
    ),
    String,
> {
    use ksni::TrayMethods;
    let (actions, receiver) = tokio::sync::mpsc::unbounded_channel();
    let handle = LinuxTray { actions, labels }
        .spawn()
        .await
        .map_err(|error| error.to_string())?;
    Ok((handle, receiver))
}

#[cfg(windows)]
fn windows_tray(
    labels: TrayLabels,
) -> Result<
    (
        std::thread::JoinHandle<()>,
        tokio::sync::mpsc::UnboundedReceiver<TrayAction>,
    ),
    String,
> {
    use tray_icon::menu::{Menu, MenuEvent, MenuItem};
    use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, TranslateMessage, MSG,
    };

    let (actions, receiver) = tokio::sync::mpsc::unbounded_channel();
    let (ready, ready_receiver) = std::sync::mpsc::sync_channel(1);
    let thread = std::thread::Builder::new()
        .name("minidl-tray".into())
        .spawn(move || {
            let show = MenuItem::with_id("show", &labels.show, true, None);
            let pause = MenuItem::with_id("pause-all", &labels.pause_all, true, None);
            let resume = MenuItem::with_id("resume-all", &labels.resume_all, true, None);
            let quit = MenuItem::with_id("quit", &labels.quit, true, None);
            let menu = match Menu::with_items(&[&show, &pause, &resume, &quit]) {
                Ok(menu) => menu,
                Err(error) => {
                    let _ = ready.send(Err(error.to_string()));
                    return;
                }
            };

            let menu_actions = actions.clone();
            MenuEvent::set_event_handler(Some(move |event| {
                let action = match event.id.0.as_str() {
                    "show" => Some(TrayAction::Show),
                    "pause-all" => Some(TrayAction::PauseAll),
                    "resume-all" => Some(TrayAction::ResumeAll),
                    "quit" => Some(TrayAction::Quit),
                    _ => None,
                };
                if let Some(action) = action {
                    let _ = menu_actions.send(action);
                }
            }));
            let click_actions = actions.clone();
            TrayIconEvent::set_event_handler(Some(move |event| {
                if matches!(
                    event,
                    TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } | TrayIconEvent::DoubleClick {
                        button: MouseButton::Left,
                        ..
                    }
                ) {
                    let _ = click_actions.send(TrayAction::Show);
                }
            }));
            let icon = match Icon::from_rgba(tray_pixels(), 32, 32) {
                Ok(icon) => icon,
                Err(error) => {
                    let _ = ready.send(Err(error.to_string()));
                    return;
                }
            };
            let _tray = match TrayIconBuilder::new()
                .with_menu(Box::new(menu))
                .with_menu_on_left_click(false)
                .with_tooltip("Mini Downloader")
                .with_icon(icon)
                .build()
            {
                Ok(tray) => tray,
                Err(error) => {
                    let _ = ready.send(Err(error.to_string()));
                    return;
                }
            };
            let _ = ready.send(Ok(()));
            let mut message = MSG::default();
            // SAFETY: this is the dedicated thread that owns the tray icon's
            // hidden Win32 window and its ordinary message pump.
            unsafe {
                while GetMessageW(&mut message, None, 0, 0).0 > 0 {
                    let _ = TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
        })
        .map_err(|error| error.to_string())?;
    ready_receiver.recv().map_err(|error| error.to_string())??;
    Ok((thread, receiver))
}

#[cfg(windows)]
fn tray_pixels() -> Vec<u8> {
    include_bytes!(concat!(env!("OUT_DIR"), "/app-icon.rgba")).to_vec()
}

fn run_ui(runtime: &tokio::runtime::Runtime) -> Result<(), String> {
    let client = runtime.block_on(connect_with_retry())?;
    let snapshot: AppSnapshot = runtime
        .block_on(client.request(ControlCommand::Snapshot))
        .and_then(|value| serde_json::from_value(value).map_err(|error| error.to_string()))?;
    let ui = MainWindow::new().map_err(|error| error.to_string())?;
    let locale_code = locale::selected(snapshot.settings.get("locale").map(String::as_str));
    let catalog = Arc::new(RwLock::new(locale::Catalog::new(locale_code)));
    let selection = Arc::new(Mutex::new(HashSet::<i64>::new()));
    let collapsed_packages = Arc::new(Mutex::new(HashSet::<i64>::new()));
    let catalog_guard = catalog.read().unwrap_or_else(|error| error.into_inner());
    let selection_guard = selection.lock().unwrap_or_else(|error| error.into_inner());
    let collapsed_guard = collapsed_packages
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    ui.set_downloads(ModelRc::new(VecModel::from(snapshot_rows(
        &snapshot,
        0,
        "",
        &catalog_guard,
        &selection_guard,
        &collapsed_guard,
    ))));
    ui.set_categories(ModelRc::new(VecModel::from(category_rows(
        &snapshot.categories,
        &snapshot.downloads,
    ))));
    ui.set_schedules(ModelRc::new(VecModel::from(schedule_rows(
        &snapshot.schedules,
        &catalog_guard,
    ))));
    ui.set_grabbed_links(ModelRc::new(VecModel::from(Vec::<LinkChoice>::new())));
    ui.set_media_choices(ModelRc::new(VecModel::from(Vec::<MediaChoice>::new())));
    ui.set_detail_files(ModelRc::new(VecModel::from(Vec::<DetailFileRow>::new())));
    apply_settings(&ui, &snapshot);
    apply_i18n(&ui, &catalog_guard);
    apply_benchmark_start_page(&ui);
    apply_selection_state(&ui, &selection_guard, &catalog_guard);
    drop(collapsed_guard);
    drop(selection_guard);
    drop(catalog_guard);
    apply_download_counts(&ui, &snapshot.downloads);
    if let Ok(status) = runtime.block_on(client.request(ControlCommand::GetConnectorStatus)) {
        apply_connector_status(&ui, &status);
    }
    let filter = Arc::new(AtomicI32::new(0));
    let all_downloads = Arc::new(Mutex::new(snapshot.downloads));
    install_callbacks(
        &ui,
        client.clone(),
        runtime.handle().clone(),
        all_downloads.clone(),
        filter.clone(),
        catalog.clone(),
        selection.clone(),
        collapsed_packages.clone(),
    );
    install_window_events(&ui);
    let quit_controller = Arc::new(AtomicBool::new(false));
    {
        let weak = ui.as_weak();
        let quit_controller = quit_controller.clone();
        ui.window().on_close_requested(move || {
            if weak.upgrade().is_some_and(|ui| !ui.get_close_to_tray()) {
                quit_controller.store(true, Ordering::Release);
            }
            let _ = slint::quit_event_loop();
            slint::CloseRequestResponse::HideWindow
        });
    }

    let weak = ui.as_weak();
    let event_client = client.clone();
    let event_downloads = all_downloads.clone();
    let event_catalog = catalog.clone();
    let event_selection = selection.clone();
    let event_collapsed = collapsed_packages.clone();
    let refresh_in_flight = Arc::new(AtomicBool::new(false));
    runtime.spawn(async move {
        let _ = ControlClient::subscribe(move |event| {
            if matches!(event.event.as_str(), "ui:hide" | "app:quit") {
                let _ = slint::invoke_from_event_loop(|| {
                    let _ = slint::quit_event_loop();
                });
                return;
            }
            if event.event == "connector:status" {
                let weak = weak.clone();
                let status = event.payload;
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        apply_connector_status(&ui, &status);
                    }
                });
                return;
            }
            if !matches!(
                event.event.as_str(),
                "downloads:tick"
                    | "downloads:state"
                    | "downloads:complete"
                    | "downloads:error"
                    | "downloads:reconciled"
                    | "categories:changed"
                    | "scheduler:changed"
                    | "state:resync-required"
            ) {
                return;
            }
            if refresh_in_flight.swap(true, Ordering::AcqRel) {
                return;
            }
            let client = event_client.clone();
            let weak = weak.clone();
            let all_downloads = event_downloads.clone();
            let filter = filter.clone();
            let catalog = event_catalog.clone();
            let selection = event_selection.clone();
            let collapsed_packages = event_collapsed.clone();
            let refresh_in_flight = refresh_in_flight.clone();
            tokio::spawn(async move {
                let Ok(value) = client.request(ControlCommand::Snapshot).await else {
                    refresh_in_flight.store(false, Ordering::Release);
                    return;
                };
                let Ok(snapshot) = serde_json::from_value::<AppSnapshot>(value) else {
                    refresh_in_flight.store(false, Ordering::Release);
                    return;
                };
                let filter_value = filter.load(Ordering::Relaxed);
                if let Ok(mut downloads) = all_downloads.lock() {
                    *downloads = snapshot.downloads.clone();
                }
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = weak.upgrade() else { return };
                    let catalog = catalog.read().unwrap_or_else(|error| error.into_inner());
                    let mut selection = selection.lock().unwrap_or_else(|error| error.into_inner());
                    let collapsed_packages = collapsed_packages
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    selection.retain(|id| snapshot.downloads.iter().any(|row| row.id == *id));
                    let rows = snapshot_rows(
                        &snapshot,
                        filter_value,
                        &ui.get_search_text(),
                        &catalog,
                        &selection,
                        &collapsed_packages,
                    );
                    ui.set_downloads(ModelRc::new(VecModel::from(rows)));
                    ui.set_categories(ModelRc::new(VecModel::from(category_rows(
                        &snapshot.categories,
                        &snapshot.downloads,
                    ))));
                    ui.set_schedules(ModelRc::new(VecModel::from(schedule_rows(
                        &snapshot.schedules,
                        &catalog,
                    ))));
                    apply_download_counts(&ui, &snapshot.downloads);
                    apply_selection_state(&ui, &selection, &catalog);
                });
                refresh_in_flight.store(false, Ordering::Release);
            });
        })
        .await;
    });

    let result = ui.run().map_err(|error| error.to_string());
    if quit_controller.load(Ordering::Acquire) {
        let _ = runtime.block_on(client.request(ControlCommand::Quit));
    }
    result
}

fn apply_benchmark_start_page(ui: &MainWindow) {
    if std::env::var_os("MINIDL_BENCHMARK").is_none() {
        return;
    }
    match std::env::var("MINIDL_BENCHMARK_PAGE").as_deref() {
        Ok("settings") => {
            let section = std::env::var("MINIDL_BENCHMARK_SETTINGS_SECTION")
                .ok()
                .and_then(|value| value.parse::<i32>().ok())
                .filter(|value| (0..=7).contains(value))
                .unwrap_or(0);
            ui.set_settings_section(section);
            ui.set_current_page(3);
        }
        Ok("shortcuts") => ui.set_current_page(4),
        _ => {}
    }
}

fn install_window_events(ui: &MainWindow) {
    use slint::winit_030::{winit, EventResult, WinitWindowAccessor};
    use winit::event::{ElementState, WindowEvent};
    use winit::keyboard::{Key, ModifiersState, NamedKey};

    let modifiers = Rc::new(Cell::new(ModifiersState::empty()));
    let weak = ui.as_weak();
    ui.window()
        .on_winit_window_event(move |_window, event| match event {
            WindowEvent::ModifiersChanged(value) => {
                modifiers.set(value.state());
                EventResult::Propagate
            }
            WindowEvent::ThemeChanged(theme) => {
                if let Some(ui) = weak.upgrade() {
                    ui.set_system_dark(matches!(theme, winit::window::Theme::Dark));
                }
                EventResult::Propagate
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed && !event.repeat =>
            {
                let Some(ui) = weak.upgrade() else {
                    return EventResult::Propagate;
                };
                let modifiers = modifiers.get();
                let command = modifiers.control_key() || modifiers.super_key();
                let shift = modifiers.shift_key();
                let plain = !command && !shift && !modifiers.alt_key();

                let handled = match &event.logical_key {
                    Key::Named(NamedKey::Escape) => {
                        if ui.get_detail_visible() {
                            ui.set_detail_visible(false);
                            ui.set_delete_confirm_id(0);
                            true
                        } else if ui.get_current_page() != 0 {
                            ui.set_current_page(0);
                            true
                        } else {
                            false
                        }
                    }
                    Key::Character(key) if command && !shift && key.eq_ignore_ascii_case("n") => {
                        ui.invoke_focus_add_input();
                        true
                    }
                    Key::Character(key) if command && !shift && key.eq_ignore_ascii_case("f") => {
                        ui.invoke_focus_search_input();
                        true
                    }
                    Key::Character(key) if command && !shift && key.as_str() == "," => {
                        ui.set_settings_section(0);
                        ui.set_current_page(3);
                        true
                    }
                    Key::Character(key) if command && shift && key.eq_ignore_ascii_case("p") => {
                        ui.invoke_pause_all();
                        true
                    }
                    Key::Character(key) if command && shift && key.eq_ignore_ascii_case("r") => {
                        ui.invoke_resume_all();
                        true
                    }
                    Key::Character(key) if command && !shift && key.eq_ignore_ascii_case("a") => {
                        ui.invoke_select_visible(true);
                        true
                    }
                    Key::Character(key)
                        if plain && !ui.get_shortcut_blocked() && key.as_str() == "/" =>
                    {
                        ui.invoke_focus_search_input();
                        true
                    }
                    Key::Character(key)
                        if plain && !ui.get_shortcut_blocked() && key.as_str() == "?" =>
                    {
                        ui.set_current_page(4);
                        true
                    }
                    Key::Character(key) if plain && !ui.get_shortcut_blocked() => {
                        match key.as_str() {
                            "1" => ui.invoke_activate_status_filter(0),
                            "2" => ui.invoke_activate_status_filter(1),
                            "3" => ui.invoke_activate_status_filter(2),
                            "4" => ui.invoke_activate_status_filter(3),
                            "5" => ui.invoke_activate_status_filter(4),
                            "6" => ui.invoke_activate_status_filter(5),
                            _ => return EventResult::Propagate,
                        }
                        true
                    }
                    _ => false,
                };

                if handled {
                    EventResult::PreventDefault
                } else {
                    EventResult::Propagate
                }
            }
            _ => EventResult::Propagate,
        });
}

async fn connect_with_retry() -> Result<Arc<ControlClient>, String> {
    let mut last = String::from("controller unavailable");
    for _ in 0..40 {
        match ControlClient::connect().await {
            Ok(client) => return Ok(client),
            Err(error) => last = error.to_string(),
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Err(last)
}

const BYTES_PER_DISPLAY_MEGABYTE: f64 = 1_000_000.0;

fn default_speed_mb_to_bytes(megabytes: f32) -> i64 {
    if !megabytes.is_finite() {
        return 0;
    }
    (f64::from(megabytes.max(0.0)) * BYTES_PER_DISPLAY_MEGABYTE)
        .round()
        .min(i64::MAX as f64) as i64
}

fn default_speed_bytes_to_mb(bytes: i64) -> f32 {
    let megabytes = bytes.max(0) as f64 / BYTES_PER_DISPLAY_MEGABYTE;
    ((megabytes * 100.0).round() / 100.0).min(f32::MAX as f64) as f32
}

#[derive(Clone, PartialEq)]
struct SettingsDraft {
    proxy: String,
    maximum: i32,
    split: i32,
    connections: i32,
    clipboard: bool,
    theme: String,
    auto_organize: bool,
    prevent_duplicates: bool,
    close_to_tray: bool,
    autostart: bool,
    handle_magnets: bool,
    dht: bool,
    sandbox: bool,
    block_private: bool,
    locale_choice: String,
    complete_action: String,
    complete_command: String,
    complete_confirmed: bool,
    default_speed_mb: f32,
}

impl SettingsDraft {
    fn from_ui(ui: &MainWindow) -> Self {
        Self {
            proxy: ui.get_proxy_text().to_string(),
            maximum: ui.get_max_concurrent(),
            split: ui.get_split_count(),
            connections: ui.get_connection_count(),
            clipboard: ui.get_clipboard_watch(),
            theme: ui.get_theme_choice().to_string(),
            auto_organize: ui.get_auto_organize(),
            prevent_duplicates: ui.get_prevent_duplicates(),
            close_to_tray: ui.get_close_to_tray(),
            autostart: ui.get_autostart(),
            handle_magnets: ui.get_handle_magnets(),
            dht: ui.get_dht_enabled(),
            sandbox: ui.get_sandbox_children(),
            block_private: ui.get_block_private_ips(),
            locale_choice: ui.get_locale_choice().to_string(),
            complete_action: ui.get_on_complete_action().to_string(),
            complete_command: ui.get_on_complete_command().to_string(),
            complete_confirmed: ui.get_on_complete_confirmed(),
            default_speed_mb: ui.get_default_speed_mb(),
        }
    }

    fn apply_to_ui(&self, ui: &MainWindow) {
        ui.set_proxy_text(self.proxy.clone().into());
        ui.set_max_concurrent(self.maximum);
        ui.set_split_count(self.split);
        ui.set_connection_count(self.connections);
        ui.set_clipboard_watch(self.clipboard);
        ui.set_theme_choice(self.theme.clone().into());
        ui.set_auto_organize(self.auto_organize);
        ui.set_prevent_duplicates(self.prevent_duplicates);
        ui.set_close_to_tray(self.close_to_tray);
        ui.set_autostart(self.autostart);
        ui.set_handle_magnets(self.handle_magnets);
        ui.set_dht_enabled(self.dht);
        ui.set_sandbox_children(self.sandbox);
        ui.set_block_private_ips(self.block_private);
        ui.set_locale_choice(self.locale_choice.clone().into());
        ui.set_on_complete_action(self.complete_action.clone().into());
        ui.set_on_complete_command(self.complete_command.clone().into());
        ui.set_on_complete_confirmed(self.complete_confirmed);
        ui.set_default_speed_mb(self.default_speed_mb);
    }
}

// The generated Slint callback surface needs these independently shared models;
// grouping them would only move the same wiring behind opaque field accesses.
#[allow(clippy::too_many_arguments)]
fn install_callbacks(
    ui: &MainWindow,
    client: Arc<ControlClient>,
    runtime: tokio::runtime::Handle,
    all_downloads: Arc<Mutex<Vec<minidl_app::controller::DownloadView>>>,
    filter: Arc<AtomicI32>,
    catalog: SharedCatalog,
    selection: Arc<Mutex<HashSet<i64>>>,
    collapsed_packages: Arc<Mutex<HashSet<i64>>>,
) {
    let invoke_client = client.clone();
    let invoke_runtime = runtime.clone();
    let invoke = move |command: ControlCommand, weak: slint::Weak<MainWindow>| {
        let client = invoke_client.clone();
        invoke_runtime.spawn(async move {
            let result = client.request(command).await;
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = weak.upgrade() {
                    match result {
                        Ok(_) => ui.set_status_text("".into()),
                        Err(error) => ui.set_status_text(error.into()),
                    }
                }
            });
        });
    };
    let invoke = Rc::new(invoke);

    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        ui.on_add_url(move |url, checksum| {
            let raw = url.trim();
            let command = command_for_argument(raw).unwrap_or_else(|| ControlCommand::IngestUrl {
                url: raw.to_owned(),
                checksum: (!checksum.trim().is_empty()).then(|| checksum.to_string()),
            });
            invoke(command, weak.clone());
        });
    }
    {
        let client = client.clone();
        let runtime = runtime.clone();
        let weak = ui.as_weak();
        let catalog = catalog.clone();
        ui.on_pick_file(move || {
            let client = client.clone();
            let weak = weak.clone();
            let catalog = catalog.clone();
            runtime.spawn(async move {
                let (title, invalid_file) = {
                    let catalog = catalog.read().unwrap_or_else(|error| error.into_inner());
                    (
                        catalog.get("tipAddFile").to_owned(),
                        catalog.get("invalidDownloadFile").to_owned(),
                    )
                };
                let picked = rfd::AsyncFileDialog::new()
                    .set_title(title)
                    .add_filter("Torrent / Metalink", &["torrent", "meta4", "metalink"])
                    .pick_file()
                    .await;
                let Some(file) = picked else { return };
                let result = match command_for_path(file.path()) {
                    Some(command) => client.request(command).await.map(|_| ()),
                    None => Err(invalid_file),
                };
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        ui.set_status_text(result.err().unwrap_or_default().into());
                    }
                });
            });
        });
    }
    {
        let runtime = runtime.clone();
        let weak = ui.as_weak();
        ui.on_copy_url(move |url| {
            let weak = weak.clone();
            let text = url.to_string();
            runtime.spawn(async move {
                let result = tokio::task::spawn_blocking(move || {
                    arboard::Clipboard::new()
                        .map_err(|error| error.to_string())?
                        .set_text(text)
                        .map_err(|error| error.to_string())
                })
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result);
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        ui.set_status_text(result.err().unwrap_or_default().into());
                    }
                });
            });
        });
    }
    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        ui.on_set_global_speed(move |bytes| {
            invoke(
                ControlCommand::SetGlobalSpeed {
                    download: i64::from(bytes.max(0)),
                    upload: 0,
                },
                weak.clone(),
            );
        });
    }
    macro_rules! id_callback {
        ($setter:ident, $variant:ident) => {{
            let invoke = invoke.clone();
            let weak = ui.as_weak();
            ui.$setter(move |id| invoke(ControlCommand::$variant { id: id as i64 }, weak.clone()));
        }};
    }
    id_callback!(on_pause, Pause);
    id_callback!(on_resume, Resume);
    id_callback!(on_retry, Retry);
    id_callback!(on_open_file, OpenFile);
    id_callback!(on_open_folder, OpenContainingFolder);
    {
        let client = client.clone();
        let runtime = runtime.clone();
        let weak = ui.as_weak();
        ui.on_show_details(move |id| {
            let client = client.clone();
            let weak = weak.clone();
            runtime.spawn(async move {
                let result = client
                    .request(ControlCommand::GetDownloadDetails { id: id as i64 })
                    .await;
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        match result.and_then(|value| {
                            serde_json::from_value::<minidl_app::controller::DownloadDetails>(value)
                                .map_err(|error| error.to_string())
                        }) {
                            Ok(details) => {
                                ui.set_detail_id(details.id.clamp(0, i32::MAX as i64) as i32);
                                ui.set_detail_is_torrent(matches!(
                                    details.kind.as_str(),
                                    "torrent" | "magnet"
                                ));
                                ui.set_detail_text(format_details(&details).into());
                                ui.set_detail_files(ModelRc::new(VecModel::from(
                                    detail_file_rows(&details.files),
                                )));
                                ui.set_detail_visible(true);
                            }
                            Err(error) => ui.set_status_text(error.into()),
                        }
                    }
                });
            });
        });
    }
    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        ui.on_move_in_queue(move |id, direction| {
            invoke(
                ControlCommand::MoveInQueue {
                    id: id as i64,
                    direction: direction.to_string(),
                },
                weak.clone(),
            );
        });
    }
    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        ui.on_set_download_speed(move |id, kilobytes| {
            invoke(
                ControlCommand::SetDownloadSpeed {
                    id: id as i64,
                    limit: i64::from(kilobytes.max(0)) * 1024,
                },
                weak.clone(),
            );
        });
    }
    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        ui.on_schedule_download(move |id, minutes| {
            invoke(
                ControlCommand::ScheduleDownload {
                    id: id as i64,
                    start_at: Some(
                        minidl_core::model::now()
                            .saturating_add(i64::from(minutes.max(1)).saturating_mul(60)),
                    ),
                },
                weak.clone(),
            );
        });
    }
    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        ui.on_unschedule_download(move |id| {
            invoke(
                ControlCommand::ScheduleDownload {
                    id: id as i64,
                    start_at: None,
                },
                weak.clone(),
            );
        });
    }
    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        ui.on_remove(move |id| {
            invoke(
                ControlCommand::Remove {
                    id: id as i64,
                    delete_files: false,
                },
                weak.clone(),
            )
        });
    }
    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        ui.on_remove_delete(move |id| {
            invoke(
                ControlCommand::Remove {
                    id: i64::from(id),
                    delete_files: true,
                },
                weak.clone(),
            )
        });
    }
    {
        let selection = selection.clone();
        let all = all_downloads.clone();
        let filter = filter.clone();
        let catalog = catalog.clone();
        let collapsed_packages = collapsed_packages.clone();
        let weak = ui.as_weak();
        ui.on_toggle_selected(move |id, checked| {
            let Some(ui) = weak.upgrade() else { return };
            let id = i64::from(id);
            let mut selection = selection.lock().unwrap_or_else(|error| error.into_inner());
            if checked {
                selection.insert(id);
            } else {
                selection.remove(&id);
            }
            let downloads = all.lock().unwrap_or_else(|error| error.into_inner());
            let catalog = catalog.read().unwrap_or_else(|error| error.into_inner());
            let collapsed_packages = collapsed_packages
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            ui.set_downloads(ModelRc::new(VecModel::from(rows_from_downloads(
                &downloads,
                filter.load(Ordering::Relaxed),
                &ui.get_search_text(),
                &catalog,
                &selection,
                &collapsed_packages,
            ))));
            apply_selection_state(&ui, &selection, &catalog);
        });
    }
    {
        let selection = selection.clone();
        let all = all_downloads.clone();
        let filter = filter.clone();
        let catalog = catalog.clone();
        let collapsed_packages = collapsed_packages.clone();
        let weak = ui.as_weak();
        ui.on_select_visible(move |checked| {
            let Some(ui) = weak.upgrade() else { return };
            let downloads = all.lock().unwrap_or_else(|error| error.into_inner());
            let catalog = catalog.read().unwrap_or_else(|error| error.into_inner());
            let visible = rows_from_downloads(
                &downloads,
                filter.load(Ordering::Relaxed),
                &ui.get_search_text(),
                &catalog,
                &HashSet::new(),
                &HashSet::new(),
            )
            .into_iter()
            .map(|row| i64::from(row.id))
            .collect::<HashSet<_>>();
            let mut selection = selection.lock().unwrap_or_else(|error| error.into_inner());
            if checked {
                selection.extend(visible);
            } else {
                selection.retain(|id| !visible.contains(id));
            }
            let collapsed_packages = collapsed_packages
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            ui.set_downloads(ModelRc::new(VecModel::from(rows_from_downloads(
                &downloads,
                filter.load(Ordering::Relaxed),
                &ui.get_search_text(),
                &catalog,
                &selection,
                &collapsed_packages,
            ))));
            apply_selection_state(&ui, &selection, &catalog);
        });
    }
    {
        let collapsed_packages = collapsed_packages.clone();
        let all = all_downloads.clone();
        let filter = filter.clone();
        let catalog = catalog.clone();
        let selection = selection.clone();
        let weak = ui.as_weak();
        ui.on_toggle_package(move |package_id| {
            let Some(ui) = weak.upgrade() else { return };
            let package_id = i64::from(package_id);
            let mut collapsed = collapsed_packages
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if !collapsed.insert(package_id) {
                collapsed.remove(&package_id);
            }
            let downloads = all.lock().unwrap_or_else(|error| error.into_inner());
            let selection = selection.lock().unwrap_or_else(|error| error.into_inner());
            let catalog = catalog.read().unwrap_or_else(|error| error.into_inner());
            ui.set_downloads(ModelRc::new(VecModel::from(rows_from_downloads(
                &downloads,
                filter.load(Ordering::Relaxed),
                &ui.get_search_text(),
                &catalog,
                &selection,
                &collapsed,
            ))));
        });
    }
    {
        let client = client.clone();
        let runtime = runtime.clone();
        let all = all_downloads.clone();
        let filter = filter.clone();
        let catalog = catalog.clone();
        let selection = selection.clone();
        let collapsed_packages = collapsed_packages.clone();
        let weak = ui.as_weak();
        ui.on_package_action(move |package_id, action| {
            let package_id = i64::from(package_id);
            let ids = all
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .iter()
                .filter(|download| download.package_id == Some(package_id))
                .map(|download| download.id)
                .collect::<Vec<_>>();
            if ids.is_empty() {
                return;
            }
            let action = action.to_string();
            let client = client.clone();
            let weak = weak.clone();
            let all = all.clone();
            let filter = filter.clone();
            let catalog = catalog.clone();
            let selection = selection.clone();
            let collapsed_packages = collapsed_packages.clone();
            runtime.spawn(async move {
                let mut last_error = None;
                for id in ids {
                    let command = match action.as_str() {
                        "pause" => ControlCommand::Pause { id },
                        "resume" => ControlCommand::Resume { id },
                        "remove" => ControlCommand::Remove {
                            id,
                            delete_files: false,
                        },
                        _ => return,
                    };
                    if let Err(error) = client.request(command).await {
                        last_error = Some(error);
                    }
                }
                refresh_snapshot(
                    client,
                    weak.clone(),
                    all,
                    filter,
                    catalog,
                    selection,
                    collapsed_packages,
                )
                .await;
                if let Some(error) = last_error {
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = weak.upgrade() {
                            ui.set_status_text(error.into());
                        }
                    });
                }
            });
        });
    }
    {
        let client = client.clone();
        let runtime = runtime.clone();
        let selection = selection.clone();
        let all = all_downloads.clone();
        let filter = filter.clone();
        let catalog = catalog.clone();
        let collapsed_packages = collapsed_packages.clone();
        let weak = ui.as_weak();
        ui.on_bulk_action(move |action| {
            let action = action.to_string();
            if action == "clear" {
                selection
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .clear();
                if let Some(ui) = weak.upgrade() {
                    let downloads = all.lock().unwrap_or_else(|error| error.into_inner());
                    let selection = selection.lock().unwrap_or_else(|error| error.into_inner());
                    let catalog = catalog.read().unwrap_or_else(|error| error.into_inner());
                    let collapsed_packages = collapsed_packages
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    ui.set_downloads(ModelRc::new(VecModel::from(rows_from_downloads(
                        &downloads,
                        filter.load(Ordering::Relaxed),
                        &ui.get_search_text(),
                        &catalog,
                        &selection,
                        &collapsed_packages,
                    ))));
                    apply_selection_state(&ui, &selection, &catalog);
                }
                return;
            }
            let ids = selection
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .iter()
                .copied()
                .collect::<Vec<_>>();
            if ids.is_empty() {
                return;
            }
            let client = client.clone();
            let weak = weak.clone();
            let selection = selection.clone();
            let all = all.clone();
            let filter = filter.clone();
            let catalog = catalog.clone();
            let collapsed_packages = collapsed_packages.clone();
            runtime.spawn(async move {
                let mut failed = HashSet::new();
                let mut last_error = None;
                for id in ids {
                    let command = match action.as_str() {
                        "pause" => ControlCommand::Pause { id },
                        "resume" => ControlCommand::Resume { id },
                        "remove" => ControlCommand::Remove {
                            id,
                            delete_files: false,
                        },
                        _ => return,
                    };
                    if let Err(error) = client.request(command).await {
                        failed.insert(id);
                        last_error = Some(error);
                    }
                }
                *selection.lock().unwrap_or_else(|error| error.into_inner()) = failed;
                let snapshot = client
                    .request(ControlCommand::Snapshot)
                    .await
                    .ok()
                    .and_then(|value| serde_json::from_value::<AppSnapshot>(value).ok());
                if let Some(snapshot) = &snapshot {
                    *all.lock().unwrap_or_else(|error| error.into_inner()) =
                        snapshot.downloads.clone();
                }
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = weak.upgrade() else { return };
                    let selection = selection.lock().unwrap_or_else(|error| error.into_inner());
                    let catalog = catalog.read().unwrap_or_else(|error| error.into_inner());
                    let collapsed_packages = collapsed_packages
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    if let Some(snapshot) = snapshot {
                        ui.set_downloads(ModelRc::new(VecModel::from(snapshot_rows(
                            &snapshot,
                            filter.load(Ordering::Relaxed),
                            &ui.get_search_text(),
                            &catalog,
                            &selection,
                            &collapsed_packages,
                        ))));
                    }
                    apply_selection_state(&ui, &selection, &catalog);
                    ui.set_status_text(last_error.unwrap_or_default().into());
                });
            });
        });
    }
    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        ui.on_pause_all(move || invoke(ControlCommand::PauseAll, weak.clone()));
    }
    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        ui.on_resume_all(move || invoke(ControlCommand::ResumeAll, weak.clone()));
    }
    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        ui.on_remove_completed(move || invoke(ControlCommand::RemoveCompleted, weak.clone()));
    }
    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        ui.on_remove_failed(move || invoke(ControlCommand::RemoveFailed, weak.clone()));
    }
    {
        let filter_copy = filter.clone();
        let all = all_downloads.clone();
        let catalog = catalog.clone();
        let selection = selection.clone();
        let collapsed_packages = collapsed_packages.clone();
        let weak = ui.as_weak();
        ui.on_filter_changed(move |value| {
            filter_copy.store(value, Ordering::Relaxed);
            if let Some(ui) = weak.upgrade() {
                if let Ok(downloads) = all.lock() {
                    let catalog = catalog.read().unwrap_or_else(|error| error.into_inner());
                    let selection = selection.lock().unwrap_or_else(|error| error.into_inner());
                    let collapsed_packages = collapsed_packages
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    ui.set_downloads(ModelRc::new(VecModel::from(rows_from_downloads(
                        &downloads,
                        value,
                        &ui.get_search_text(),
                        &catalog,
                        &selection,
                        &collapsed_packages,
                    ))));
                    apply_selection_state(&ui, &selection, &catalog);
                }
            }
        });
    }
    {
        let client = client.clone();
        let runtime = runtime.clone();
        let weak = ui.as_weak();
        let all = all_downloads.clone();
        let filter = filter.clone();
        let catalog = catalog.clone();
        let selection = selection.clone();
        let collapsed_packages = collapsed_packages.clone();
        ui.on_refresh(move || {
            let client = client.clone();
            let weak = weak.clone();
            let all = all.clone();
            let filter = filter.clone();
            let catalog = catalog.clone();
            let selection = selection.clone();
            let collapsed_packages = collapsed_packages.clone();
            runtime.spawn(async move {
                refresh_snapshot(
                    client,
                    weak,
                    all,
                    filter,
                    catalog,
                    selection,
                    collapsed_packages,
                )
                .await;
            });
        });
    }
    {
        let client = client.clone();
        let runtime = runtime.clone();
        let weak = ui.as_weak();
        ui.on_grab_links(move |text| {
            let client = client.clone();
            let weak = weak.clone();
            runtime.spawn(async move {
                let result = client
                    .request(ControlCommand::GrabLinks {
                        text: text.to_string(),
                    })
                    .await
                    .and_then(|value| {
                        serde_json::from_value::<Vec<minidl_core::grabber::ParsedLink>>(value)
                            .map_err(|error| error.to_string())
                    });
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        match result {
                            Ok(links) => {
                                if ui.get_link_package().trim().is_empty() {
                                    ui.set_link_package(
                                        links
                                            .first()
                                            .map(|link| link.host.as_str())
                                            .unwrap_or("")
                                            .into(),
                                    );
                                }
                                ui.set_grabbed_links(ModelRc::new(VecModel::from(
                                    links
                                        .into_iter()
                                        .map(|link| LinkChoice {
                                            url: link.url.into(),
                                            kind: link.kind.into(),
                                            host: link.host.into(),
                                            selected: true,
                                        })
                                        .collect::<Vec<_>>(),
                                )));
                                ui.set_status_text("".into());
                            }
                            Err(error) => ui.set_status_text(error.into()),
                        }
                    }
                });
            });
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_toggle_grabbed(move |index, selected| {
            let Some(ui) = weak.upgrade() else { return };
            let model = ui.get_grabbed_links();
            if let Some(mut row) = usize::try_from(index)
                .ok()
                .and_then(|index| model.row_data(index))
            {
                row.selected = selected;
                model.set_row_data(index as usize, row);
            }
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_select_grabbed(move |selected| {
            let Some(ui) = weak.upgrade() else { return };
            let model = ui.get_grabbed_links();
            for index in 0..model.row_count() {
                if let Some(mut row) = model.row_data(index) {
                    row.selected = selected;
                    model.set_row_data(index, row);
                }
            }
        });
    }
    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        let catalog = catalog.clone();
        ui.on_add_grabbed(move || {
            let Some(ui) = weak.upgrade() else { return };
            let model = ui.get_grabbed_links();
            let urls: Vec<_> = (0..model.row_count())
                .filter_map(|index| model.row_data(index))
                .filter(|row| row.selected)
                .map(|row| row.url.to_string())
                .collect();
            if urls.is_empty() {
                let catalog = catalog.read().unwrap_or_else(|error| error.into_inner());
                ui.set_status_text(catalog.get("selectOneLink").into());
                return;
            }
            invoke(
                ControlCommand::AddLinks {
                    urls,
                    package_name: (!ui.get_link_package().trim().is_empty())
                        .then(|| ui.get_link_package().to_string()),
                },
                weak.clone(),
            );
        });
    }
    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        ui.on_add_media(move |url| {
            let options = weak
                .upgrade()
                .map(|ui| media_options(&ui))
                .unwrap_or_default();
            invoke(
                ControlCommand::AddMedia {
                    url: url.to_string(),
                    format_id: None,
                    options: Some(options),
                },
                weak.clone(),
            );
        });
    }
    {
        let client = client.clone();
        let runtime = runtime.clone();
        let weak = ui.as_weak();
        let catalog = catalog.clone();
        ui.on_probe_media(move |url, playlist| {
            let client = client.clone();
            let weak = weak.clone();
            let catalog = catalog.clone();
            runtime.spawn(async move {
                let result = client
                    .request(ControlCommand::ProbeMedia {
                        url: url.to_string(),
                        playlist,
                    })
                    .await;
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        match result.and_then(|value| {
                            serde_json::from_value::<minidl_core::ytdlp::MediaInfo>(value)
                                .map_err(|error| error.to_string())
                        }) {
                            Ok(info) => {
                                let choice_rows = media_choices(&info, &url);
                                let catalog =
                                    catalog.read().unwrap_or_else(|error| error.into_inner());
                                ui.set_media_title(info.title.as_str().into());
                                ui.set_media_is_playlist(info.kind == "playlist");
                                ui.set_media_entries_text(
                                    catalog
                                        .get("plEntries")
                                        .replace("{n}", &choice_rows.len().to_string())
                                        .into(),
                                );
                                ui.set_media_choices(ModelRc::new(VecModel::from(choice_rows)));
                                ui.set_status_text("".into());
                            }
                            Err(error) => ui.set_status_text(error.into()),
                        }
                    }
                });
            });
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_toggle_media_choice(move |index, selected| {
            let Some(ui) = weak.upgrade() else { return };
            let model = ui.get_media_choices();
            if let Some(mut row) = usize::try_from(index)
                .ok()
                .and_then(|index| model.row_data(index))
            {
                row.selected = selected;
                model.set_row_data(index as usize, row);
            }
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_select_media_choices(move |selected| {
            let Some(ui) = weak.upgrade() else { return };
            let model = ui.get_media_choices();
            for index in 0..model.row_count() {
                if let Some(mut row) = model.row_data(index) {
                    row.selected = selected;
                    model.set_row_data(index, row);
                }
            }
        });
    }
    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        let catalog = catalog.clone();
        ui.on_add_playlist(move || {
            let Some(ui) = weak.upgrade() else { return };
            let model = ui.get_media_choices();
            let entries: Vec<_> = (0..model.row_count())
                .filter_map(|index| model.row_data(index))
                .filter(|row| row.selected)
                .map(|row| PlaylistSelection {
                    url: row.url.to_string(),
                    title: Some(row.title.to_string()),
                })
                .collect();
            if entries.is_empty() {
                let catalog = catalog.read().unwrap_or_else(|error| error.into_inner());
                ui.set_status_text(catalog.get("selectOnePlaylist").into());
                return;
            }
            let quality = match ui.get_media_quality().as_str() {
                "1080p" => Some("bv*[height<=1080]+ba/b".into()),
                "720p" => Some("bv*[height<=720]+ba/b".into()),
                _ => None,
            };
            invoke(
                ControlCommand::AddPlaylist {
                    entries,
                    package_name: ui.get_media_title().to_string(),
                    quality,
                    options: Some(media_options(&ui)),
                },
                weak.clone(),
            );
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_toggle_detail_file(move |index, selected| {
            let Some(ui) = weak.upgrade() else { return };
            let model = ui.get_detail_files();
            if let Some(mut row) = usize::try_from(index)
                .ok()
                .and_then(|index| model.row_data(index))
            {
                row.selected = selected;
                model.set_row_data(index as usize, row);
            }
        });
    }
    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        ui.on_apply_torrent_files(move |id| {
            let Some(ui) = weak.upgrade() else { return };
            let model = ui.get_detail_files();
            let indices = (0..model.row_count())
                .filter_map(|index| model.row_data(index))
                .filter(|row| row.selected)
                .filter_map(|row| u32::try_from(row.index).ok())
                .collect();
            invoke(
                ControlCommand::SetTorrentFiles {
                    id: id as i64,
                    indices,
                },
                weak.clone(),
            );
        });
    }
    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        ui.on_add_media_choice(move |url, format_id| {
            let options = weak
                .upgrade()
                .map(|ui| media_options(&ui))
                .unwrap_or_default();
            invoke(
                ControlCommand::AddMedia {
                    url: url.to_string(),
                    format_id: (!format_id.is_empty()).then(|| format_id.to_string()),
                    options: Some(options),
                },
                weak.clone(),
            );
        });
    }
    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        ui.on_save_category(move |name, dir, rules, priority| {
            invoke(
                ControlCommand::SaveCategory {
                    name: name.to_string(),
                    dir: dir.to_string(),
                    rules: rules.to_string(),
                    priority: priority as i64,
                },
                weak.clone(),
            )
        });
    }
    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        ui.on_move_category(move |id, direction| {
            invoke(
                ControlCommand::MoveCategory {
                    id: i64::from(id),
                    direction,
                },
                weak.clone(),
            )
        });
    }
    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        let catalog = catalog.clone();
        ui.on_save_new_category(move |name, dir, extensions, mimes, hosts| {
            if name.trim().is_empty() || dir.trim().is_empty() {
                if let Some(ui) = weak.upgrade() {
                    let catalog = catalog.read().unwrap_or_else(|error| error.into_inner());
                    ui.set_status_text(catalog.get("categoryRequired").into());
                }
                return;
            }
            let priority = weak
                .upgrade()
                .map(|ui| i64::try_from(ui.get_categories().row_count()).unwrap_or(i64::MAX))
                .unwrap_or_default();
            invoke(
                ControlCommand::SaveCategory {
                    name: name.trim().to_owned(),
                    dir: dir.trim().to_owned(),
                    rules: category_rules(&extensions, &mimes, &hosts),
                    priority,
                },
                weak.clone(),
            );
        });
    }
    {
        let client = client.clone();
        let runtime = runtime.clone();
        let weak = ui.as_weak();
        let catalog = catalog.clone();
        ui.on_browse_category(move |name, rules, priority| {
            let client = client.clone();
            let weak = weak.clone();
            let catalog = catalog.clone();
            runtime.spawn(async move {
                let title = catalog
                    .read()
                    .unwrap_or_else(|error| error.into_inner())
                    .get("browseFolder")
                    .to_owned();
                let picked = rfd::AsyncFileDialog::new()
                    .set_title(title)
                    .pick_folder()
                    .await;
                let Some(folder) = picked else { return };
                let result = client
                    .request(ControlCommand::SaveCategory {
                        name: name.to_string(),
                        dir: folder.path().to_string_lossy().into_owned(),
                        rules: rules.to_string(),
                        priority: i64::from(priority),
                    })
                    .await;
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        ui.set_status_text(result.err().unwrap_or_default().into());
                    }
                });
            });
        });
    }
    {
        let runtime = runtime.clone();
        let weak = ui.as_weak();
        let catalog = catalog.clone();
        ui.on_browse_new_category_dir(move || {
            let weak = weak.clone();
            let catalog = catalog.clone();
            runtime.spawn(async move {
                let title = catalog
                    .read()
                    .unwrap_or_else(|error| error.into_inner())
                    .get("browseFolder")
                    .to_owned();
                let picked = rfd::AsyncFileDialog::new()
                    .set_title(title)
                    .pick_folder()
                    .await;
                let Some(folder) = picked else { return };
                let path = folder.path().to_string_lossy().into_owned();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        ui.set_category_dir(path.into());
                    }
                });
            });
        });
    }
    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        ui.on_delete_category(move |id| {
            invoke(
                ControlCommand::DeleteCategory { id: id as i64 },
                weak.clone(),
            )
        });
    }
    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        ui.on_reset_category(move |id| {
            invoke(
                ControlCommand::ResetCategoryDir { id: id as i64 },
                weak.clone(),
            )
        });
    }
    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        ui.on_restore_categories(move || {
            invoke(ControlCommand::RestoreDefaultCategories, weak.clone())
        });
    }
    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        ui.on_save_schedule(move |id, name, action, days, minute, speed, enabled| {
            invoke(
                ControlCommand::SaveSchedule {
                    id: (id > 0).then_some(id as i64),
                    name: (!name.trim().is_empty()).then(|| name.to_string()),
                    action: action.to_string(),
                    days_mask: days as i64,
                    at_minute: minute as i64,
                    speed_limit: (speed > 0).then_some(speed as i64),
                    enabled,
                },
                weak.clone(),
            )
        });
    }
    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        ui.on_delete_schedule(move |id| {
            invoke(
                ControlCommand::DeleteSchedule { id: id as i64 },
                weak.clone(),
            )
        });
    }
    {
        let client = client.clone();
        let runtime = runtime.clone();
        let weak = ui.as_weak();
        let saved = Arc::new(Mutex::new(SettingsDraft::from_ui(ui)));
        let catalog = catalog.clone();
        let all = all_downloads.clone();
        let filter = filter.clone();
        let selection = selection.clone();
        let collapsed_packages = collapsed_packages.clone();
        ui.on_save_settings(
            move |proxy,
                  maximum,
                  split,
                  connections,
                  clipboard,
                  theme,
                  auto_organize,
                  prevent_duplicates,
                  close_to_tray,
                  autostart,
                  handle_magnets,
                  dht,
                  sandbox,
                  block_private,
                  locale_choice,
                  complete_action,
                  complete_command,
                  complete_confirmed,
                  default_speed_mb| {
                let next = SettingsDraft {
                    proxy: proxy.to_string(),
                    maximum,
                    split,
                    connections,
                    clipboard,
                    theme: theme.to_string(),
                    auto_organize,
                    prevent_duplicates,
                    close_to_tray,
                    autostart,
                    handle_magnets,
                    dht,
                    sandbox,
                    block_private,
                    locale_choice: locale_choice.to_string(),
                    complete_action: complete_action.to_string(),
                    complete_command: complete_command.to_string(),
                    complete_confirmed,
                    default_speed_mb,
                };
                let previous = saved
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .clone();
                if next == previous {
                    return;
                }
                let locale_code = locale_code_from_name(&next.locale_choice).to_owned();
                let command = ControlCommand::ApplySettings {
                    settings: SettingsUpdate {
                        proxy: next.proxy.clone(),
                        max_concurrent: next.maximum.max(1) as u32,
                        split: next.split.max(1) as u32,
                        connections: next.connections.max(1) as u32,
                        clipboard: next.clipboard,
                        theme: match next.theme.as_str() {
                            "light" | "dark" => next.theme.clone(),
                            _ => "system".into(),
                        },
                        auto_organize: next.auto_organize,
                        prevent_duplicates: next.prevent_duplicates,
                        close_to_tray: next.close_to_tray,
                        autostart: next.autostart,
                        handle_magnets: next.handle_magnets,
                        dht: next.dht,
                        sandbox: next.sandbox,
                        block_private: next.block_private,
                        locale: locale_code.clone(),
                        on_complete_action: if next.complete_action == "run:" {
                            format!("run:{}", next.complete_command.trim())
                        } else {
                            next.complete_action.clone()
                        },
                        on_complete_confirmed: next.complete_confirmed,
                        default_speed_limit: default_speed_mb_to_bytes(next.default_speed_mb),
                    },
                };
                let client = client.clone();
                let weak = weak.clone();
                let saved = saved.clone();
                let catalog = catalog.clone();
                let all = all.clone();
                let filter = filter.clone();
                let selection = selection.clone();
                let collapsed_packages = collapsed_packages.clone();
                let previous_for_error = previous.clone();
                runtime.spawn(async move {
                    let result = client.request(command).await;
                    let snapshot = if result.is_ok() {
                        client
                            .request(ControlCommand::Snapshot)
                            .await
                            .ok()
                            .and_then(|value| serde_json::from_value::<AppSnapshot>(value).ok())
                    } else {
                        None
                    };
                    if result.is_ok() {
                        *saved.lock().unwrap_or_else(|error| error.into_inner()) = next;
                        *catalog.write().unwrap_or_else(|error| error.into_inner()) =
                            locale::Catalog::new(locale::selected(Some(&locale_code)));
                        if let Some(snapshot) = &snapshot {
                            *all.lock().unwrap_or_else(|error| error.into_inner()) =
                                snapshot.downloads.clone();
                        }
                    }
                    let _ = slint::invoke_from_event_loop(move || {
                        let Some(ui) = weak.upgrade() else { return };
                        match result {
                            Ok(_) => {
                                let catalog =
                                    catalog.read().unwrap_or_else(|error| error.into_inner());
                                apply_i18n(&ui, &catalog);
                                if let Some(snapshot) = snapshot {
                                    let selection =
                                        selection.lock().unwrap_or_else(|error| error.into_inner());
                                    let collapsed_packages = collapsed_packages
                                        .lock()
                                        .unwrap_or_else(|error| error.into_inner());
                                    ui.set_downloads(ModelRc::new(VecModel::from(snapshot_rows(
                                        &snapshot,
                                        filter.load(Ordering::Relaxed),
                                        &ui.get_search_text(),
                                        &catalog,
                                        &selection,
                                        &collapsed_packages,
                                    ))));
                                    ui.set_schedules(ModelRc::new(VecModel::from(schedule_rows(
                                        &snapshot.schedules,
                                        &catalog,
                                    ))));
                                    apply_selection_state(&ui, &selection, &catalog);
                                }
                                ui.set_status_text("".into());
                            }
                            Err(error) => {
                                previous_for_error.apply_to_ui(&ui);
                                ui.set_status_text(error.into());
                            }
                        }
                    });
                });
            },
        );
    }
    {
        let client = client.clone();
        let runtime = runtime.clone();
        let weak = ui.as_weak();
        let catalog = catalog.clone();
        ui.on_check_update(move || {
            let client = client.clone();
            let weak = weak.clone();
            let catalog = catalog.clone();
            runtime.spawn(async move {
                let result = client.request(ControlCommand::CheckUpdate).await;
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        match result.and_then(|value| {
                            serde_json::from_value::<minidl_app::updater::UpdateInfo>(value)
                                .map_err(|error| error.to_string())
                        }) {
                            Ok(update) => {
                                let catalog =
                                    catalog.read().unwrap_or_else(|error| error.into_inner());
                                ui.set_update_text(if update.newer {
                                    catalog
                                        .get("updateAvailable")
                                        .replace("{v}", &update.latest)
                                        .into()
                                } else {
                                    catalog
                                        .get("updateUpToDate")
                                        .replace("{v}", &update.current)
                                        .into()
                                });
                                ui.set_update_page(if update.newer {
                                    update.url.into()
                                } else {
                                    "".into()
                                });
                                ui.set_update_asset(update.asset_url.unwrap_or_default().into());
                            }
                            Err(error) => ui.set_update_text(error.into()),
                        }
                    }
                });
            });
        });
    }
    {
        let invoke = invoke.clone();
        let weak = ui.as_weak();
        ui.on_install_update(move |asset, page| {
            invoke(
                ControlCommand::InstallUpdate {
                    asset_url: (!asset.is_empty()).then(|| asset.to_string()),
                    page_url: page.to_string(),
                },
                weak.clone(),
            )
        });
    }
    {
        let runtime = runtime.clone();
        let weak = ui.as_weak();
        ui.on_open_external(move |url| {
            let weak = weak.clone();
            let url = url.to_string();
            runtime.spawn(async move {
                let result = tokio::task::spawn_blocking(move || {
                    open::that(url).map_err(|error| error.to_string())
                })
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result);
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        ui.set_status_text(result.err().unwrap_or_default().into());
                    }
                });
            });
        });
    }
}

async fn refresh_snapshot(
    client: Arc<ControlClient>,
    weak: slint::Weak<MainWindow>,
    all: Arc<Mutex<Vec<minidl_app::controller::DownloadView>>>,
    filter: Arc<AtomicI32>,
    catalog: SharedCatalog,
    selection: Arc<Mutex<HashSet<i64>>>,
    collapsed_packages: Arc<Mutex<HashSet<i64>>>,
) {
    let Ok(value) = client.request(ControlCommand::Snapshot).await else {
        return;
    };
    let Ok(snapshot) = serde_json::from_value::<AppSnapshot>(value) else {
        return;
    };
    if let Ok(mut downloads) = all.lock() {
        *downloads = snapshot.downloads.clone();
    }
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = weak.upgrade() {
            let catalog = catalog.read().unwrap_or_else(|error| error.into_inner());
            let mut selection = selection.lock().unwrap_or_else(|error| error.into_inner());
            let collapsed_packages = collapsed_packages
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            selection.retain(|id| snapshot.downloads.iter().any(|row| row.id == *id));
            ui.set_downloads(ModelRc::new(VecModel::from(snapshot_rows(
                &snapshot,
                filter.load(Ordering::Relaxed),
                &ui.get_search_text(),
                &catalog,
                &selection,
                &collapsed_packages,
            ))));
            ui.set_categories(ModelRc::new(VecModel::from(category_rows(
                &snapshot.categories,
                &snapshot.downloads,
            ))));
            ui.set_schedules(ModelRc::new(VecModel::from(schedule_rows(
                &snapshot.schedules,
                &catalog,
            ))));
            apply_download_counts(&ui, &snapshot.downloads);
            apply_selection_state(&ui, &selection, &catalog);
        }
    });
}

fn system_prefers_dark() -> bool {
    if std::env::var("GTK_THEME")
        .ok()
        .is_some_and(|theme| theme.to_ascii_lowercase().contains("dark"))
    {
        return true;
    }

    #[cfg(target_os = "linux")]
    {
        let config_root = std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(std::path::PathBuf::from)
                    .map(|home| home.join(".config"))
            });
        if let Some(config_root) = config_root {
            for path in [
                config_root.join("kdeglobals"),
                config_root.join("kdedefaults/kdeglobals"),
            ] {
                let Ok(contents) = std::fs::read_to_string(path) else {
                    continue;
                };
                if let Some(value) = contents
                    .lines()
                    .find_map(|line| line.trim().strip_prefix("ColorScheme=").map(str::trim))
                {
                    return value.to_ascii_lowercase().contains("dark");
                }
                if let Some((red, green, blue)) = contents.lines().find_map(|line| {
                    let value = line.trim().strip_prefix("Background=")?;
                    let mut channels = value
                        .split(',')
                        .filter_map(|part| part.trim().parse::<u16>().ok());
                    Some((channels.next()?, channels.next()?, channels.next()?))
                }) {
                    return red + green + blue < 384;
                }
            }
        }
    }

    true
}

fn apply_settings(ui: &MainWindow, snapshot: &AppSnapshot) {
    let setting = |key: &str| snapshot.settings.get(key).map(String::as_str);
    ui.set_proxy_text(setting("proxy").unwrap_or("").into());
    ui.set_max_concurrent(
        setting("max_concurrent")
            .and_then(|v| v.parse().ok())
            .unwrap_or(5),
    );
    ui.set_split_count(setting("split").and_then(|v| v.parse().ok()).unwrap_or(16));
    ui.set_connection_count(
        setting("max_conn")
            .and_then(|v| v.parse().ok())
            .unwrap_or(16),
    );
    ui.set_clipboard_watch(setting("clipboard_watch") == Some("true"));
    ui.set_auto_organize(setting("auto_organize") != Some("false"));
    ui.set_prevent_duplicates(setting("prevent_duplicate_downloads") != Some("false"));
    ui.set_close_to_tray(setting("close_to_tray") != Some("false"));
    ui.set_autostart(setting("autostart") == Some("true"));
    ui.set_handle_magnets(setting("handle_magnets") != Some("false"));
    ui.set_dht_enabled(setting("dht_enabled") != Some("false"));
    ui.set_sandbox_children(setting("sandbox_children") == Some("true"));
    ui.set_block_private_ips(setting("block_private_ips") == Some("true"));
    ui.set_theme_choice(
        match setting("theme") {
            Some("light" | "dark") => setting("theme").unwrap_or("system"),
            _ => "system",
        }
        .into(),
    );
    ui.set_system_dark(system_prefers_dark());
    ui.set_locale_choice(locale_name(setting("locale").unwrap_or("system")).into());
    let on_complete = setting("on_complete_action").unwrap_or("none");
    if let Some(command) = on_complete.strip_prefix("run:") {
        ui.set_on_complete_action("run:".into());
        ui.set_on_complete_command(command.into());
    } else {
        ui.set_on_complete_action(on_complete.into());
    }
    ui.set_on_complete_confirmed(setting("on_complete_command_confirmed") == Some("true"));
    ui.set_default_speed_mb(
        setting("default_speed_limit")
            .and_then(|value| value.parse::<i64>().ok())
            .map(default_speed_bytes_to_mb)
            .unwrap_or_default(),
    );
}

fn apply_i18n(ui: &MainWindow, catalog: &locale::Catalog) {
    macro_rules! set {
        ($setter:ident, $key:literal) => {
            ui.$setter(catalog.get($key).into());
        };
    }
    set!(set_nav_downloads, "titleAll");
    set!(set_nav_links, "grabLinksTitle");
    set!(set_nav_media, "grabVideoTitle");
    set!(set_nav_settings, "settings");
    set!(set_label_filters, "navStatus");
    set!(set_label_categories, "sectCategories");
    set!(set_label_all, "statusAll");
    set!(set_label_active, "statusActive");
    set!(set_label_paused, "statusPaused");
    set!(set_label_scheduled, "statusScheduled");
    set!(set_label_completed, "statusCompleted");
    set!(set_label_failed, "statusFailed");
    set!(set_label_pause_all, "pauseAll");
    set!(set_label_resume_all, "resumeAll");
    set!(set_label_clear_completed, "clearCompleted");
    set!(set_label_clear_failed, "clearFailed");
    set!(set_label_search, "searchDownloads");
    set!(set_label_add_placeholder, "addPlaceholder");
    set!(set_label_checksum, "checksumLabel");
    set!(set_label_add, "add");
    set!(set_label_pause, "pause");
    set!(set_label_resume, "resume");
    set!(set_label_retry, "retry");
    set!(set_label_open_file, "openFile");
    set!(set_label_folder, "openFolder");
    set!(set_label_details, "detailTitle");
    set!(set_label_remove, "remove");
    set!(set_label_remove_delete, "removeDelete");
    set!(set_label_confirm_delete, "confirmRemoveDelete");
    set!(set_label_copy_url, "copyUrl");
    set!(set_label_close, "close");
    set!(set_label_empty_title, "emptyTitle");
    set!(set_label_empty_sub, "emptySub");
    set!(set_label_no_match_title, "noMatchTitle");
    set!(set_label_no_match_sub, "noMatchSub");
    set!(set_label_onboard_paste, "obPaste");
    set!(set_label_onboard_video, "obVideo");
    set!(set_label_onboard_links, "obLinks");
    set!(set_label_onboard_browser, "obBrowser");
    set!(set_label_link_title, "grabLinksTitle");
    set!(set_label_link_placeholder, "pasteLinksPlaceholder");
    set!(set_label_extract_links, "extractLinks");
    set!(set_label_media_title, "grabVideoTitle");
    set!(set_label_media_placeholder, "videoUrlPlaceholder");
    set!(set_label_best_quality, "bestQuality");
    set!(set_label_general, "sectGeneral");
    set!(set_label_language, "sectLanguage");
    set!(set_label_extensions, "sectExtensions");
    set!(set_label_scheduler, "sectScheduler");
    set!(set_label_updates, "sectUpdates");
    set!(set_label_clipboard, "optClipboard");
    set!(set_label_auto_organize, "optAutoOrganize");
    set!(set_label_prevent_duplicates, "optPreventDuplicates");
    set!(set_label_close_tray, "optCloseTray");
    set!(set_label_autostart, "optAutostart");
    set!(set_label_handle_magnets, "optHandleMagnets");
    set!(set_label_dht, "optDht");
    set!(set_label_sandbox, "optSandbox");
    set!(set_label_block_private, "optBlockPrivate");
    set!(set_label_theme, "theme");
    set!(set_label_theme_system, "themeSystem");
    set!(set_label_theme_light, "themeLight");
    set!(set_label_theme_dark, "themeDark");
    set!(set_label_on_complete, "onComplete");
    set!(set_label_oc_none, "ocNone");
    set!(set_label_oc_quit, "ocQuit");
    set!(set_label_oc_sleep, "ocSleep");
    set!(set_label_oc_shutdown, "ocShutdown");
    set!(set_label_oc_custom, "ocCustom");
    set!(set_label_oc_placeholder, "ocCustomPlaceholder");
    set!(set_label_oc_warning, "ocCustomWarning");
    set!(set_label_oc_confirm, "ocCustomConfirm");
    set!(set_label_network, "sectNetwork");
    set!(set_label_proxy_title, "optProxy");
    set!(set_label_proxy, "proxyPlaceholder");
    set!(
        set_label_prevent_duplicates_hint,
        "optPreventDuplicatesHint"
    );
    set!(set_label_magnets_hint, "optHandleMagnetsHint");
    set!(set_label_restart_hint, "restartHint");
    set!(set_label_connections, "sectConnections");
    set!(set_label_max_concurrent, "maxConcurrent");
    set!(set_label_segments, "optSegments");
    set!(set_label_conn_server, "optConnPerServer");
    set!(set_label_default_speed, "defaultSpeedLimit");
    set!(set_label_connections_hint, "connHint");
    set!(set_label_extension_hint, "extensionStoreHint");
    set!(set_label_firefox_hint, "firefoxFamilyHint");
    set!(set_label_chromium_hint, "chromiumFamilyHint");
    set!(set_label_extension_note, "extensionStoreOpenNote");
    set!(set_label_browser_checking, "browserStateChecking");
    set!(set_label_browser_connected, "browserStateConnected");
    set!(set_label_browser_installed, "browserStateInstalled");
    set!(
        set_label_browser_needs_extension,
        "browserStateNeedsExtension"
    );
    set!(set_label_browser_not_found, "browserStateNotFound");
    set!(set_label_get_firefox, "getForFirefox");
    set!(set_label_get_chrome, "getForChrome");
    set!(set_label_sched_pause, "schedPauseAll");
    set!(set_label_sched_resume, "schedResumeAll");
    set!(set_label_sched_speed, "schedSetSpeed");
    set!(set_label_sched_add, "schedAddRule");
    set!(set_label_day_mon, "dayMon");
    set!(set_label_day_tue, "dayTue");
    set!(set_label_day_wed, "dayWed");
    set!(set_label_day_thu, "dayThu");
    set!(set_label_day_fri, "dayFri");
    set!(set_label_day_sat, "daySat");
    set!(set_label_day_sun, "daySun");
    set!(set_label_add_category, "addCategory");
    set!(set_label_cat_name, "catName");
    set!(set_label_cat_folder, "catFolder");
    set!(set_label_cat_rules, "catRules");
    set!(set_label_cat_mime, "catMimeRules");
    set!(set_label_cat_host, "catHostRules");
    set!(set_label_move_up, "moveUp");
    set!(set_label_move_down, "moveDown");
    set!(set_label_browse_folder, "browseFolder");
    set!(set_label_restore_defaults, "restoreDefaults");
    set!(set_label_check_updates, "updateCheck");
    set!(set_label_update_install, "updateInstall");
    set!(set_label_update_view, "updateView");
    set!(set_label_sponsor, "sponsor");
    set!(set_label_package_collapse, "pkgCollapse");
    set!(set_label_package_expand, "pkgExpand");
    set!(set_label_add_file, "tipAddFile");
    set!(set_label_shortcuts, "shortcutsTitle");
    set!(set_label_checksum_placeholder, "checksumPlaceholder");
    set!(set_label_onboard_hint, "emptyShortcutHint");
    set!(set_label_link_help, "grabLinksHelp");
    set!(set_label_package_name, "pkgName");
    set!(set_label_select_all, "plSelectAll");
    set!(set_label_select_none, "selectNone");
    set!(set_label_add_selected, "addSelectedPlain");
    set!(set_label_media_help, "grabVideoHelp");
    set!(set_label_probe, "probe");
    set!(set_label_playlist, "plPlaylist");
    set!(set_label_audio_only, "optAudioOnly");
    set!(set_label_subtitles, "optSubs");
    set!(set_label_thumbnail, "optEmbedThumb");
    set!(set_label_embed_subtitles, "optEmbedSubs");
    set!(set_label_add_playlist, "addSelectedPlain");
    set!(set_label_sc_focus_add, "scFocusAdd");
    set!(set_label_sc_search, "scSearch");
    set!(set_label_sc_filter, "scFilter");
    set!(set_label_sc_select_all, "scSelectAll");
    set!(set_label_sc_pause_resume, "scPauseResume");
    set!(set_label_sc_remove, "scRemove");
    set!(set_label_sc_open, "scOpen");
    set!(set_label_sc_close, "scClose");
    set!(set_label_hour, "hour");
    set!(set_label_minute, "minute");
    set!(set_label_apply_selection, "applySelection");
    set!(set_label_speed_limit, "speedLimit");
    set!(set_label_apply, "apply");
    set!(set_label_start_in_minutes, "startInMinutes");
    set!(set_label_schedule_action, "scheduleAction");
    set!(set_label_cancel_schedule, "scheduleCancel");
    set!(set_label_unlimited, "speedUnlimited");
    set!(set_label_global_speed, "globalSpeed");
    ui.set_rtl(catalog.is_rtl());
    let media_count = ui.get_media_choices().row_count();
    ui.set_media_entries_text(
        catalog
            .get("plEntries")
            .replace("{n}", &media_count.to_string())
            .into(),
    );
    if locale_code_from_name(&ui.get_locale_choice()) == "system" {
        ui.set_locale_choice(catalog.get("langSystem").into());
    }
    ui.set_page_title(catalog.get("titleAll").into());
}

fn snapshot_rows(
    snapshot: &AppSnapshot,
    filter: i32,
    search: &str,
    catalog: &locale::Catalog,
    selection: &HashSet<i64>,
    collapsed_packages: &HashSet<i64>,
) -> Vec<DownloadRow> {
    rows_from_downloads(
        &snapshot.downloads,
        filter,
        search,
        catalog,
        selection,
        collapsed_packages,
    )
}

fn clamp_id(id: i64) -> i32 {
    id.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

fn rows_from_downloads(
    downloads: &[minidl_app::controller::DownloadView],
    filter: i32,
    search: &str,
    catalog: &locale::Catalog,
    selection: &HashSet<i64>,
    collapsed_packages: &HashSet<i64>,
) -> Vec<DownloadRow> {
    let search = search.to_ascii_lowercase();
    let filtered = downloads
        .iter()
        .filter(|row| match filter {
            1 => row.status.is_running(),
            2 => row.status == minidl_core::model::DownloadStatus::Paused,
            3 => row.status == minidl_core::model::DownloadStatus::Scheduled,
            4 => row.status == minidl_core::model::DownloadStatus::Complete,
            5 => row.status == minidl_core::model::DownloadStatus::Error,
            category if category >= 1000 => row.category_id == Some(i64::from(category - 1000)),
            _ => true,
        })
        .filter(|row| {
            search.is_empty()
                || row.url.to_ascii_lowercase().contains(&search)
                || row
                    .filename
                    .as_deref()
                    .unwrap_or("")
                    .to_ascii_lowercase()
                    .contains(&search)
        })
        .collect::<Vec<_>>();

    #[derive(Default)]
    struct PackageStats {
        count: usize,
        total: i64,
        completed: i64,
        speed: i64,
        running: bool,
        resumable: bool,
        all_complete: bool,
    }

    let mut package_stats = HashMap::<i64, PackageStats>::new();
    for row in &filtered {
        let Some(package_id) = row.package_id else {
            continue;
        };
        let stats = package_stats.entry(package_id).or_insert(PackageStats {
            all_complete: true,
            ..PackageStats::default()
        });
        stats.count += 1;
        stats.total = stats.total.saturating_add(row.total_bytes.max(0));
        stats.completed = stats.completed.saturating_add(row.completed_bytes.max(0));
        if row.status.is_running() {
            stats.speed = stats.speed.saturating_add(row.download_speed.max(0));
            stats.running = true;
        }
        stats.resumable |= matches!(
            row.status,
            minidl_core::model::DownloadStatus::Paused
                | minidl_core::model::DownloadStatus::Scheduled
        );
        stats.all_complete &= row.status == minidl_core::model::DownloadStatus::Complete;
    }

    let mut seen_packages = HashSet::new();
    filtered
        .into_iter()
        .filter_map(|row| {
            let package_first = row
                .package_id
                .is_some_and(|package_id| seen_packages.insert(package_id));
            let package_collapsed = row
                .package_id
                .is_some_and(|package_id| collapsed_packages.contains(&package_id));
            if row.package_id.is_some() && package_collapsed && !package_first {
                return None;
            }
            let progress = if row.total_bytes > 0 {
                row.completed_bytes as f32 / row.total_bytes as f32
            } else if row.status == minidl_core::model::DownloadStatus::Complete {
                1.0
            } else {
                0.0
            };
            let (
                package_count_text,
                package_progress,
                package_meta_text,
                package_running,
                package_resumable,
            ) = row
                .package_id
                .and_then(|package_id| package_stats.get(&package_id))
                .map(|stats| {
                    let package_progress = if stats.total > 0 {
                        stats.completed as f32 / stats.total as f32
                    } else if stats.all_complete {
                        1.0
                    } else {
                        0.0
                    };
                    let mut meta = format!(
                        "{:.0}% · {} / {}",
                        package_progress * 100.0,
                        format_bytes(stats.completed),
                        if stats.total > 0 {
                            format_bytes(stats.total)
                        } else {
                            "—".to_owned()
                        }
                    );
                    if stats.speed > 0 {
                        meta.push_str(" · ");
                        meta.push_str(&format_speed(stats.speed));
                    }
                    (
                        catalog
                            .get("pkgItems")
                            .replace("{n}", &stats.count.to_string()),
                        package_progress,
                        meta,
                        stats.running,
                        stats.resumable,
                    )
                })
                .unwrap_or_default();
            Some(DownloadRow {
                id: clamp_id(row.id),
                name: row.filename.as_deref().unwrap_or(&row.url).into(),
                url: row.url.as_str().into(),
                kind: row.kind.as_str().into(),
                status: row.status.as_str().into(),
                status_display: status_label(row.status, catalog).into(),
                progress,
                progress_text: format!("{:.0}%", progress * 100.0).into(),
                speed_text: format_speed(row.download_speed).into(),
                meta_text: format!(
                    "{} / {} · {:.0}%",
                    format_bytes(row.completed_bytes),
                    if row.total_bytes > 0 {
                        format_bytes(row.total_bytes)
                    } else {
                        "—".to_owned()
                    },
                    progress * 100.0,
                )
                .into(),
                running: row.status.is_running(),
                resumable: matches!(
                    row.status,
                    minidl_core::model::DownloadStatus::Paused
                        | minidl_core::model::DownloadStatus::Scheduled
                ),
                selected: selection.contains(&row.id),
                package_id: row.package_id.map(clamp_id).unwrap_or_default(),
                package_name: row.package_name.as_deref().unwrap_or_default().into(),
                package_first,
                package_collapsed,
                package_count_text: package_count_text.into(),
                package_progress,
                package_meta_text: package_meta_text.into(),
                package_running,
                package_resumable,
            })
        })
        .collect()
}

fn category_rules(extensions: &str, mimes: &str, hosts: &str) -> String {
    let values = |raw: &str| {
        raw.split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let extensions = values(extensions);
    let mimes = values(mimes);
    let hosts = values(hosts);
    if mimes.is_empty() && hosts.is_empty() {
        return serde_json::to_string(&extensions).unwrap_or_else(|_| "[]".into());
    }

    let mut rules = Vec::new();
    if !extensions.is_empty() {
        rules.push(serde_json::json!({"match": "ext", "values": extensions}));
    }
    if !mimes.is_empty() {
        rules.push(serde_json::json!({"match": "mime", "values": mimes}));
    }
    if !hosts.is_empty() {
        rules.push(serde_json::json!({"match": "host", "values": hosts}));
    }
    serde_json::Value::Array(rules).to_string()
}

fn category_rows(
    categories: &[minidl_core::model::Category],
    downloads: &[minidl_app::controller::DownloadView],
) -> Vec<CategoryRow> {
    categories
        .iter()
        .map(|category| CategoryRow {
            id: category.id as i32,
            name: category.name.as_str().into(),
            dir: category.dir.as_str().into(),
            rules: category.rules.as_str().into(),
            priority: category.priority.clamp(i32::MIN as i64, i32::MAX as i64) as i32,
            count: downloads
                .iter()
                .filter(|download| download.category_id == Some(category.id))
                .count()
                .clamp(0, i32::MAX as usize) as i32,
        })
        .collect()
}

fn apply_download_counts(ui: &MainWindow, downloads: &[minidl_app::controller::DownloadView]) {
    use minidl_core::model::DownloadStatus;

    let count = |predicate: fn(&minidl_app::controller::DownloadView) -> bool| {
        downloads
            .iter()
            .filter(|download| predicate(download))
            .count()
            .clamp(0, i32::MAX as usize) as i32
    };
    ui.set_count_all(downloads.len().clamp(0, i32::MAX as usize) as i32);
    ui.set_count_active(count(|download| download.status.is_running()));
    ui.set_count_paused(count(|download| download.status == DownloadStatus::Paused));
    ui.set_count_scheduled(count(|download| {
        download.status == DownloadStatus::Scheduled
    }));
    ui.set_count_completed(count(|download| {
        download.status == DownloadStatus::Complete
    }));
    ui.set_count_failed(count(|download| download.status == DownloadStatus::Error));
}

fn apply_selection_state(ui: &MainWindow, selection: &HashSet<i64>, catalog: &locale::Catalog) {
    let count = selection.len().min(i32::MAX as usize) as i32;
    ui.set_selected_count(count);
    ui.set_selected_text(
        catalog
            .get("bulkSelected")
            .replace("{n}", &count.to_string())
            .into(),
    );
}

fn schedule_rows(
    schedules: &[minidl_core::model::Schedule],
    catalog: &locale::Catalog,
) -> Vec<ScheduleRow> {
    schedules
        .iter()
        .map(|schedule| {
            let hour = schedule.at_minute.div_euclid(60);
            let minute = schedule.at_minute.rem_euclid(60);
            let speed = schedule
                .speed_limit
                .map(format_speed)
                .filter(|value| !value.is_empty())
                .unwrap_or_default();
            let days = [
                "dayMon", "dayTue", "dayWed", "dayThu", "dayFri", "daySat", "daySun",
            ]
            .into_iter()
            .enumerate()
            .filter(|(index, _)| schedule.days_mask & (1_i64 << index) != 0)
            .map(|(_, key)| catalog.get(key))
            .collect::<Vec<_>>()
            .join(" ");
            ScheduleRow {
                id: schedule.id as i32,
                name: schedule.name.as_deref().unwrap_or_default().into(),
                summary: format!(
                    "{:02}:{:02} · {}{}",
                    hour,
                    minute,
                    days,
                    if speed.is_empty() {
                        String::new()
                    } else {
                        format!(" · {speed}")
                    }
                )
                .into(),
                action: schedule.action.as_str().into(),
                days: schedule.days_mask.clamp(0, 127) as i32,
                minute: schedule.at_minute.clamp(0, 1439) as i32,
                speed: schedule
                    .speed_limit
                    .unwrap_or_default()
                    .clamp(0, i32::MAX as i64) as i32,
                enabled: schedule.enabled,
            }
        })
        .collect()
}

fn media_choices(info: &minidl_core::ytdlp::MediaInfo, source_url: &str) -> Vec<MediaChoice> {
    if info.kind == "playlist" {
        return info
            .entries
            .iter()
            .map(|entry| MediaChoice {
                url: entry.url.as_str().into(),
                format_id: "".into(),
                title: entry.title.as_str().into(),
                label: if entry.duration > 0 {
                    format!(
                        "{}  ·  {}:{:02}",
                        entry.title,
                        entry.duration / 60,
                        entry.duration % 60
                    )
                    .into()
                } else {
                    entry.title.as_str().into()
                },
                selected: true,
                playlist: true,
            })
            .collect();
    }
    let mut formats = info.formats.clone();
    formats.sort_by_key(|format| std::cmp::Reverse((format.height, format.filesize)));
    formats
        .into_iter()
        .filter(|format| !format.format_id.is_empty())
        .map(|format| {
            let codecs = match (format.vcodec.as_str(), format.acodec.as_str()) {
                ("none", audio) => format!("audio {audio}"),
                (video, "none") => format!("video {video}"),
                (video, audio) => format!("{video} + {audio}"),
            };
            MediaChoice {
                url: source_url.into(),
                format_id: format.format_id.into(),
                title: info.title.as_str().into(),
                label: format!(
                    "{} · {} · {} · {}",
                    format.resolution,
                    format.ext,
                    codecs,
                    format_bytes(format.filesize)
                )
                .into(),
                selected: false,
                playlist: false,
            }
        })
        .collect()
}

fn media_options(ui: &MainWindow) -> MediaOptions {
    MediaOptions {
        write_subs: ui.get_media_subtitles(),
        sub_langs: ui.get_media_sub_langs().trim().to_owned(),
        embed_subs: ui.get_media_embed_subtitles(),
        audio_only: ui.get_media_audio_only(),
        audio_format: ui.get_media_audio_format().to_string(),
        embed_thumbnail: ui.get_media_thumbnail(),
    }
}

fn detail_file_rows(files: &[minidl_app::controller::DetailFile]) -> Vec<DetailFileRow> {
    files
        .iter()
        .map(|file| DetailFileRow {
            index: file.index.clamp(0, i32::MAX as i64) as i32,
            name: file
                .path
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(&file.path)
                .into(),
            progress: if file.length > 0 {
                (file.completed_length as f32 / file.length as f32).clamp(0.0, 1.0)
            } else {
                0.0
            },
            meta: format!(
                "{} / {}",
                format_bytes(file.completed_length),
                format_bytes(file.length)
            )
            .into(),
            selected: file.selected,
        })
        .collect()
}

fn format_details(details: &minidl_app::controller::DownloadDetails) -> String {
    let mut output = format!(
        "{}\n{}\n{} files · {} peers · {} pieces",
        details.url,
        details.dir,
        details.files.len(),
        details.peers.len(),
        details.num_pieces
    );
    if let Some(error) = details
        .error_message
        .as_deref()
        .filter(|error| !error.is_empty())
    {
        output.push('\n');
        output.push_str(error);
    }
    output
}

fn locale_code_from_name(name: &str) -> &'static str {
    match name {
        "English" => "en",
        "Türkçe" => "tr",
        "Español" => "es",
        "Français" => "fr",
        "Deutsch" => "de",
        "Русский" => "ru",
        "العربية" => "ar",
        "中文" => "zh",
        "日本語" => "ja",
        "한국어" => "ko",
        _ => "system",
    }
}

fn locale_name(code: &str) -> &'static str {
    match locale::normalize(code) {
        "en" if code != "system" && !code.is_empty() => "English",
        "tr" => "Türkçe",
        "es" => "Español",
        "fr" => "Français",
        "de" => "Deutsch",
        "ru" => "Русский",
        "ar" => "العربية",
        "zh" => "中文",
        "ja" => "日本語",
        "ko" => "한국어",
        _ => "System Default",
    }
}

fn apply_connector_status(ui: &MainWindow, status: &serde_json::Value) {
    let flag = |key: &str| {
        status
            .get(key)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    };
    let state = |live: &str, installed: &str, profile: &str| {
        if flag(live) {
            1
        } else if flag(installed) {
            2
        } else if flag(profile) {
            3
        } else {
            4
        }
    };
    ui.set_firefox_state(state(
        "firefoxDetected",
        "firefoxConnectorInstalled",
        "firefoxProfileDetected",
    ));
    ui.set_chromium_state(state(
        "chromiumDetected",
        "chromiumConnectorInstalled",
        "chromiumProfileDetected",
    ));
}

fn status_label(status: minidl_core::model::DownloadStatus, catalog: &locale::Catalog) -> &str {
    let key = match status {
        minidl_core::model::DownloadStatus::Queued => "statusQueued",
        minidl_core::model::DownloadStatus::Active => "statusActive",
        minidl_core::model::DownloadStatus::Waiting => "statusWaiting",
        minidl_core::model::DownloadStatus::Paused => "statusPaused",
        minidl_core::model::DownloadStatus::Complete => "statusCompleted",
        minidl_core::model::DownloadStatus::Error => "statusFailed",
        minidl_core::model::DownloadStatus::Scheduled => "statusScheduled",
        minidl_core::model::DownloadStatus::Removed => "remove",
    };
    catalog.get(key)
}

fn format_bytes(value: i64) -> String {
    let value = value.max(0) as f64;
    for (unit, divisor) in [
        ("GB", 1_000_000_000.0),
        ("MB", 1_000_000.0),
        ("KB", 1_000.0),
    ] {
        if value >= divisor {
            return format!("{:.1} {unit}", value / divisor);
        }
    }
    format!("{} B", value as i64)
}

fn format_speed(value: i64) -> String {
    if value <= 0 {
        String::new()
    } else {
        format!("{}/s", format_bytes(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_arguments_cover_links_files_and_private_deep_links() {
        assert!(matches!(
            command_for_argument("magnet:?xt=urn:btih:abcdef"),
            Some(ControlCommand::IngestUrl { .. })
        ));
        assert!(matches!(
            command_for_argument("/tmp/example.torrent"),
            Some(ControlCommand::IngestFile {
                kind: minidl_ipc::DownloadKind::Torrent,
                ..
            })
        ));
        assert!(matches!(
            command_for_argument(
                "minidownloader://add?url=https%3A%2F%2Fexample.org%2Farchive.zip"
            ),
            Some(ControlCommand::IngestUrl { .. })
        ));
    }

    #[test]
    fn category_editor_preserves_legacy_and_structured_rule_shapes() {
        assert_eq!(category_rules("zip, pdf", "", ""), r#"["zip","pdf"]"#);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&category_rules(
                "mp4",
                "video/",
                ".example.com"
            ))
            .unwrap(),
            serde_json::json!([
                {"match": "ext", "values": ["mp4"]},
                {"match": "mime", "values": ["video/"]},
                {"match": "host", "values": [".example.com"]}
            ])
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn tray_icon_uses_canonical_embedded_app_asset() {
        let icon = canonical_linux_tray_icon();
        assert_eq!(icon.width, 32);
        assert_eq!(icon.height, 32);
        assert_eq!(icon.data.len(), 32 * 32 * 4);
        assert!(icon.data.chunks_exact(4).any(|pixel| pixel[0] != 0));
    }

    #[test]
    fn default_speed_round_trips_as_megabytes_per_second() {
        assert_eq!(default_speed_mb_to_bytes(0.0), 0);
        assert_eq!(default_speed_mb_to_bytes(0.5), 500_000);
        assert_eq!(default_speed_mb_to_bytes(1.0), 1_000_000);
        assert_eq!(default_speed_bytes_to_mb(500_000), 0.5);
        assert_eq!(default_speed_bytes_to_mb(1_500_000), 1.5);
    }
}
