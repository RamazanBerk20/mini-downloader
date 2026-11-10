use super::*;

mod callbacks;
pub(crate) mod view_models;

use callbacks::install_callbacks;
use view_models::*;

pub(super) fn run_ui(runtime: &tokio::runtime::Runtime) -> Result<(), String> {
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
    let live_update_pending = Arc::new(AtomicBool::new(false));
    runtime.spawn(async move {
        let _ = ControlClient::subscribe(move |event| {
            if matches!(event.event.as_str(), "ui:hide" | "app:quit") {
                let _ = slint::invoke_from_event_loop(|| {
                    let _ = slint::quit_event_loop();
                });
                return;
            }
            if event.event == "clipboard:detected" {
                let weak = weak.clone();
                let url = event
                    .payload
                    .get("url")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let _ = slint::invoke_from_event_loop(move || {
                    // Offer the copied link without overwriting one being typed.
                    if let Some(ui) = weak.upgrade().filter(|ui| ui.get_url_text().is_empty()) {
                        ui.set_url_text(url.into());
                    }
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
            if event.event == "downloads:tick" {
                let Ok(batch) = serde_json::from_value::<LiveDownloadBatch>(event.payload) else {
                    return;
                };
                let changed = event_downloads
                    .lock()
                    .map(|mut downloads| apply_live_downloads(&mut downloads, &batch.updates))
                    .unwrap_or(false);
                if !changed || live_update_pending.swap(true, Ordering::AcqRel) {
                    return;
                }
                let weak = weak.clone();
                let all_downloads = event_downloads.clone();
                let filter = filter.clone();
                let catalog = event_catalog.clone();
                let selection = event_selection.clone();
                let collapsed_packages = event_collapsed.clone();
                let pending = live_update_pending.clone();
                if slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        let downloads = all_downloads
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .clone();
                        let catalog = catalog.read().unwrap_or_else(|error| error.into_inner());
                        let selection = selection.lock().unwrap_or_else(|error| error.into_inner());
                        let collapsed_packages = collapsed_packages
                            .lock()
                            .unwrap_or_else(|error| error.into_inner());
                        update_download_model(
                            &ui,
                            rows_from_downloads(
                                &downloads,
                                filter.load(Ordering::Relaxed),
                                &ui.get_search_text(),
                                &catalog,
                                &selection,
                                &collapsed_packages,
                            ),
                        );
                    }
                    pending.store(false, Ordering::Release);
                })
                .is_err()
                {
                    live_update_pending.store(false, Ordering::Release);
                }
                return;
            }
            if !matches!(
                event.event.as_str(),
                "downloads:state"
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
                    update_download_model(&ui, rows);
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
        // The tray can shut the controller down before its final app:quit
        // event is flushed. A disconnected renderer cannot function, so make
        // the IPC subscription ending an unconditional exit fallback.
        let _ = slint::invoke_from_event_loop(|| {
            let _ = slint::quit_event_loop();
        });
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

pub(crate) fn default_speed_mb_to_bytes(megabytes: f32) -> i64 {
    if !megabytes.is_finite() {
        return 0;
    }
    (f64::from(megabytes.max(0.0)) * BYTES_PER_DISPLAY_MEGABYTE)
        .round()
        .min(i64::MAX as f64) as i64
}

pub(crate) fn default_speed_bytes_to_mb(bytes: i64) -> f32 {
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
