use super::*;

mod callbacks;
pub(crate) mod file_types;
pub(crate) mod state;
pub(crate) mod view_models;

use std::sync::atomic::AtomicU64;
use std::sync::MutexGuard;

use callbacks::install_callbacks;
use state::*;
use view_models::*;

/// Where a notice is shown: the page, or the dialog that started the action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Origin {
    Page,
    Links,
    Media,
    Details,
    Settings,
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

/// State the callbacks share. Touched from the UI thread and from Tokio
/// tasks, hence the locks; Slint objects are reached only through `weak` on
/// the UI thread.
pub(crate) struct Ctx {
    pub(crate) client: Arc<ControlClient>,
    pub(crate) runtime: tokio::runtime::Handle,
    pub(crate) weak: slint::Weak<MainWindow>,
    pub(crate) downloads: Mutex<Vec<minidl_app::controller::DownloadView>>,
    pub(crate) filter: AtomicI32,
    pub(crate) catalog: SharedCatalog,
    pub(crate) selection: Mutex<HashSet<i64>>,
    pub(crate) collapsed: Mutex<HashSet<i64>>,
    pub(crate) sync: Mutex<SyncState>,
    pub(crate) engine_restarting: AtomicBool,
    pub(crate) tray_available: AtomicBool,
    pub(crate) notices: Mutex<Notices<Origin>>,
    pub(crate) probe: Mutex<Probe>,
    pub(crate) probe_token: AtomicU64,
    pub(crate) settings: Mutex<SettingsSaver<SettingsDraft>>,
    settings_seen: Mutex<HashMap<String, String>>,
    live_pending: AtomicBool,
    pub(crate) detail_pending: AtomicBool,
}

impl Ctx {
    pub(crate) fn catalog(&self) -> std::sync::RwLockReadGuard<'_, locale::Catalog> {
        self.catalog
            .read()
            .unwrap_or_else(|error| error.into_inner())
    }

    /// The rows the list shows under the current filter and search.
    pub(crate) fn rows(&self, ui: &MainWindow) -> Vec<DownloadRow> {
        rows_from_downloads(
            &lock(&self.downloads),
            self.filter.load(Ordering::Relaxed),
            &ui.get_search_text(),
            &self.catalog(),
            &lock(&self.selection),
            &lock(&self.collapsed),
            self.engine_restarting.load(Ordering::Relaxed),
        )
    }

    /// The one way the list is redrawn: rows diffed into the model (no card
    /// is rebuilt unless the row set changed), counts and selection bar.
    pub(crate) fn render(&self, ui: &MainWindow) {
        update_download_model(ui, self.rows(ui));
        apply_download_counts(ui, &lock(&self.downloads));
        apply_selection_state(ui, &lock(&self.selection), &self.catalog());
    }

    fn render_later(self: &Arc<Self>) {
        let this = self.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(ui) = this.weak.upgrade() {
                this.render(&ui);
            }
        });
    }

    /// Ask for a fresh snapshot; see [`SyncState`].
    pub(crate) fn refresh(self: &Arc<Self>) {
        if lock(&self.sync).request() {
            self.fetch();
        }
    }

    fn fetch(self: &Arc<Self>) {
        let this = self.clone();
        self.runtime.spawn(async move {
            let snapshot = this
                .client
                .request(ControlCommand::Snapshot)
                .await
                .ok()
                .and_then(|value| serde_json::from_value::<AppSnapshot>(value).ok());
            let sequence = snapshot.as_ref().map(|snapshot| snapshot.sequence);
            if let Some(snapshot) = snapshot {
                let ctx = this.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ctx.weak.upgrade() {
                        ctx.apply_snapshot(&ui, snapshot);
                    }
                });
            }
            if lock(&this.sync).finished(sequence) {
                this.fetch();
            }
        });
    }

    pub(crate) fn apply_snapshot(&self, ui: &MainWindow, snapshot: AppSnapshot) {
        {
            let mut downloads = lock(&self.downloads);
            *downloads = merge_snapshot(&downloads, snapshot.downloads.clone());
            lock(&self.selection).retain(|id| downloads.iter().any(|row| row.id == *id));
        }
        self.engine_restarting.store(
            engine_restarting(snapshot.engine_state.as_ref()),
            Ordering::Relaxed,
        );
        self.tray_available
            .store(snapshot.tray_available, Ordering::Relaxed);
        let settings_changed = {
            let mut seen = lock(&self.settings_seen);
            let changed = *seen != snapshot.settings;
            if changed {
                seen.clone_from(&snapshot.settings);
            }
            changed
        };
        if settings_changed {
            self.adopt_settings(ui, &snapshot);
        }
        ui.set_categories(ModelRc::new(VecModel::from(category_rows(
            &snapshot.categories,
            &snapshot.downloads,
        ))));
        ui.set_schedules(ModelRc::new(VecModel::from(schedule_rows(
            &snapshot.schedules,
            &self.catalog(),
        ))));
        self.render(ui);
    }

    /// Settings changed outside this window's own save (or by it): show the
    /// stored values unless a save of ours is still in flight.
    fn adopt_settings(&self, ui: &MainWindow, snapshot: &AppSnapshot) {
        let mut saver = lock(&self.settings);
        let previous = SettingsDraft::from_ui(ui);
        apply_settings(ui, snapshot);
        if !saver.adopt(SettingsDraft::from_ui(ui)) {
            previous.apply_to_ui(ui);
            return;
        }
        drop(saver);
        if ui.get_locale_choice() != previous.locale_choice {
            let code = locale::selected(snapshot.settings.get("locale").map(String::as_str));
            *self
                .catalog
                .write()
                .unwrap_or_else(|error| error.into_inner()) = locale::Catalog::new(code);
            apply_i18n(ui, &self.catalog());
        }
    }

    pub(crate) fn notice(&self, ui: &MainWindow, origin: Origin, error: bool, text: &str) {
        lock(&self.notices).push(origin, error, text);
        self.render_notices(ui);
    }

    /// An action from `origin` finished: a success clears that origin's
    /// notices (only), a failure adds one.
    pub(crate) fn report(&self, ui: &MainWindow, origin: Origin, result: Result<(), String>) {
        {
            let mut notices = lock(&self.notices);
            match result {
                Ok(()) => notices.clear(origin),
                Err(error) => notices.push(origin, true, error),
            }
        }
        self.render_notices(ui);
    }

    pub(crate) fn render_notices(&self, ui: &MainWindow) {
        let notices = lock(&self.notices);
        let rows = |origin: Origin| {
            ModelRc::new(VecModel::from(
                notices
                    .items()
                    .iter()
                    .filter(|notice| notice.origin == origin)
                    .map(|notice| NoticeRow {
                        id: notice.id,
                        error: notice.error,
                        text: display_safe(&notice.text).into(),
                    })
                    .collect::<Vec<_>>(),
            ))
        };
        ui.set_page_notices(rows(Origin::Page));
        ui.set_links_notices(rows(Origin::Links));
        ui.set_media_notices(rows(Origin::Media));
        ui.set_detail_notices(rows(Origin::Details));
        ui.set_settings_notices(rows(Origin::Settings));
    }

    /// Send one command and report its outcome where it came from.
    pub(crate) fn command(self: &Arc<Self>, origin: Origin, command: ControlCommand) {
        let this = self.clone();
        self.runtime.spawn(async move {
            let result = this.client.request(command).await.map(drop);
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = this.weak.upgrade() {
                    this.report(&ui, origin, result);
                }
            });
        });
    }

    fn on_event(self: &Arc<Self>, event: minidl_ipc::ControlEvent) {
        if lock(&self.sync).observe(event.sequence) {
            self.refresh();
        }
        match event.event.as_str() {
            "ui:hide" | "app:quit" => {
                let _ = slint::invoke_from_event_loop(|| {
                    let _ = slint::quit_event_loop();
                });
            }
            "ui:show" => {
                let weak = self.weak.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        raise_window(&ui);
                    }
                });
            }
            "clipboard:detected" => {
                let weak = self.weak.clone();
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
            }
            "connector:status" => {
                let weak = self.weak.clone();
                let status = event.payload;
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        apply_connector_status(&ui, &status);
                    }
                });
            }
            "engine:state" => {
                let restarting = engine_restarting(Some(&event.payload));
                if self.engine_restarting.swap(restarting, Ordering::Relaxed) != restarting {
                    self.render_later();
                }
            }
            "downloads:tick" => self.on_tick(event.payload),
            "downloads:state"
            | "downloads:complete"
            | "downloads:error"
            | "downloads:reconciled"
            | "categories:changed"
            | "scheduler:changed"
            | "settings:changed"
            | "state:resync-required" => self.refresh(),
            _ => {}
        }
    }

    /// Live progress: merged into the cache, rendered at most once per
    /// event-loop turn, and the open detail dialog follows along.
    fn on_tick(self: &Arc<Self>, payload: serde_json::Value) {
        let Ok(batch) = serde_json::from_value::<LiveDownloadBatch>(payload) else {
            return;
        };
        let changed = apply_live_downloads(&mut lock(&self.downloads), &batch.updates);
        if !changed || self.live_pending.swap(true, Ordering::AcqRel) {
            return;
        }
        let ids = batch
            .updates
            .iter()
            .map(|update| update.id)
            .collect::<Vec<_>>();
        let this = self.clone();
        if slint::invoke_from_event_loop(move || {
            this.live_pending.store(false, Ordering::Release);
            let Some(ui) = this.weak.upgrade() else {
                return;
            };
            this.render(&ui);
            let detail = i64::from(ui.get_detail_id());
            if ui.get_overlay() == Overlay::Details && ids.contains(&detail) {
                this.load_details(detail, false);
            }
        })
        .is_err()
        {
            self.live_pending.store(false, Ordering::Release);
        }
    }

    /// Fetch a download's details into the dialog; `open` shows it and loads
    /// the per-download fields, a refresh only updates the live parts.
    pub(crate) fn load_details(self: &Arc<Self>, id: i64, open: bool) {
        if !open && self.detail_pending.swap(true, Ordering::AcqRel) {
            return;
        }
        let this = self.clone();
        self.runtime.spawn(async move {
            let result = this
                .client
                .request(ControlCommand::GetDownloadDetails { id })
                .await
                .and_then(|value| {
                    serde_json::from_value::<minidl_app::controller::DownloadDetails>(value)
                        .map_err(|error| error.to_string())
                });
            let _ = slint::invoke_from_event_loop(move || {
                this.detail_pending.store(false, Ordering::Release);
                let Some(ui) = this.weak.upgrade() else {
                    return;
                };
                let details = match result {
                    Ok(details) => details,
                    Err(error) => {
                        if open {
                            this.notice(&ui, Origin::Page, true, &error);
                        }
                        return;
                    }
                };
                if !open
                    && (ui.get_overlay() != Overlay::Details || i64::from(ui.get_detail_id()) != id)
                {
                    return;
                }
                if open {
                    ui.set_detail_files(ModelRc::new(VecModel::from(Vec::<DetailFileRow>::new())));
                }
                apply_details(&ui, &details);
                if open {
                    let speed_limit = lock(&this.downloads)
                        .iter()
                        .find(|row| row.id == id)
                        .and_then(|row| row.speed_limit)
                        .unwrap_or(0);
                    ui.set_detail_speed_kb(
                        (speed_limit / 1_000).clamp(0, i64::from(i32::MAX)) as i32
                    );
                    lock(&this.notices).clear(Origin::Details);
                    this.render_notices(&ui);
                    ui.set_overlay(Overlay::Details);
                }
            });
        });
    }
}

fn engine_restarting(state: Option<&serde_json::Value>) -> bool {
    state
        .and_then(|state| state.get("state"))
        .and_then(serde_json::Value::as_str)
        == Some("restarting")
}

/// "Show": bring an open window to the front, also from minimised.
fn raise_window(ui: &MainWindow) {
    use slint::winit_030::WinitWindowAccessor;
    let _ = ui.show();
    ui.window().with_winit_window(|window| {
        window.set_minimized(false);
        window.focus_window();
    });
}

pub(super) fn run_ui(runtime: &tokio::runtime::Runtime) -> Result<(), String> {
    let client = runtime.block_on(connect_with_retry())?;
    let snapshot: AppSnapshot = runtime
        .block_on(client.request(ControlCommand::Snapshot))
        .and_then(|value| serde_json::from_value(value).map_err(|error| error.to_string()))?;
    let ui = MainWindow::new().map_err(|error| error.to_string())?;
    let locale_code = locale::selected(snapshot.settings.get("locale").map(String::as_str));
    let catalog = Arc::new(RwLock::new(locale::Catalog::new(locale_code)));
    ui.set_grabbed_links(ModelRc::new(VecModel::from(Vec::<LinkChoice>::new())));
    ui.set_media_choices(ModelRc::new(VecModel::from(Vec::<MediaChoice>::new())));
    ui.set_detail_files(ModelRc::new(VecModel::from(Vec::<DetailFileRow>::new())));
    apply_settings(&ui, &snapshot);
    apply_i18n(
        &ui,
        &catalog.read().unwrap_or_else(|error| error.into_inner()),
    );
    apply_benchmark_start_page(&ui);
    if let Ok(status) = runtime.block_on(client.request(ControlCommand::GetConnectorStatus)) {
        apply_connector_status(&ui, &status);
    }
    let ctx = Arc::new(Ctx {
        client: client.clone(),
        runtime: runtime.handle().clone(),
        weak: ui.as_weak(),
        downloads: Mutex::new(Vec::new()),
        filter: AtomicI32::new(0),
        catalog,
        selection: Mutex::new(HashSet::new()),
        collapsed: Mutex::new(HashSet::new()),
        sync: Mutex::new(SyncState::starting_at(snapshot.sequence)),
        engine_restarting: AtomicBool::new(false),
        tray_available: AtomicBool::new(true),
        notices: Mutex::new(Notices::default()),
        probe: Mutex::new(Probe::default()),
        probe_token: AtomicU64::new(0),
        settings: Mutex::new(SettingsSaver::new(SettingsDraft::from_ui(&ui))),
        settings_seen: Mutex::new(snapshot.settings.clone()),
        live_pending: AtomicBool::new(false),
        detail_pending: AtomicBool::new(false),
    });
    ctx.apply_snapshot(&ui, snapshot);
    ctx.render_notices(&ui);
    install_callbacks(&ui, &ctx);
    install_window_events(&ui, &ctx);
    install_title_bar(&ui);
    let quit_controller = Arc::new(AtomicBool::new(false));
    {
        let weak = ui.as_weak();
        let ctx = ctx.clone();
        let quit_controller = quit_controller.clone();
        ui.window().on_close_requested(move || {
            // Without a tray icon nothing could bring the window back, so
            // closing quits like with "close to tray" off.
            let to_tray = weak.upgrade().is_some_and(|ui| ui.get_close_to_tray())
                && ctx.tray_available.load(Ordering::Relaxed);
            if !to_tray {
                quit_controller.store(true, Ordering::Release);
            }
            let _ = slint::quit_event_loop();
            slint::CloseRequestResponse::HideWindow
        });
    }
    {
        // The platform's own scheme once the window exists (winit reads it
        // from the system); the file-based guess is only the fallback.
        let weak = ui.as_weak();
        slint::Timer::single_shot(Duration::ZERO, move || {
            use slint::winit_030::WinitWindowAccessor;
            if let Some(ui) = weak.upgrade() {
                crate::dragout::init(&ui);
                if let Some(theme) = ui
                    .window()
                    .with_winit_window(|window| window.theme())
                    .flatten()
                {
                    ui.set_system_dark(matches!(
                        theme,
                        slint::winit_030::winit::window::Theme::Dark
                    ));
                }
            }
        });
    }

    let event_ctx = ctx.clone();
    runtime.spawn(async move {
        let handler = event_ctx.clone();
        let _ = ControlClient::subscribe(move |event| handler.on_event(event)).await;
        // The tray can shut the controller down before its final app:quit
        // event is flushed. A disconnected renderer cannot function, so make
        // the IPC subscription ending an unconditional exit fallback.
        let _ = slint::invoke_from_event_loop(|| {
            let _ = slint::quit_event_loop();
        });
    });

    let result = ui.run().map_err(|error| error.to_string());
    crate::dragout::shutdown();
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
            ui.set_overlay(Overlay::Settings);
        }
        Ok("shortcuts") => ui.set_overlay(Overlay::Shortcuts),
        _ => {}
    }
}

/// The title bar's drag, minimise and close; maximise is the Window's property.
fn install_title_bar(ui: &MainWindow) {
    use slint::winit_030::WinitWindowAccessor;

    let weak = ui.as_weak();
    ui.on_window_drag(move || {
        if let Some(ui) = weak.upgrade() {
            ui.window().with_winit_window(|window| {
                let _ = window.drag_window();
            });
        }
    });
    // Through winit, not the Window's `minimized`: Wayland never reports
    // un-minimising, so that property would stay set (and freeze `maximized`).
    let weak = ui.as_weak();
    ui.on_window_minimize(move || {
        if let Some(ui) = weak.upgrade() {
            ui.window()
                .with_winit_window(|window| window.set_minimized(true));
        }
    });
    let weak = ui.as_weak();
    ui.on_window_close(move || {
        // Exactly the system close button: on_close_requested decides.
        if let Some(ui) = weak.upgrade() {
            ui.window()
                .dispatch_event(slint::platform::WindowEvent::CloseRequested);
        }
    });
}

fn install_window_events(ui: &MainWindow, ctx: &Arc<Ctx>) {
    use slint::winit_030::{winit, EventResult, WinitWindowAccessor};
    use winit::event::{ElementState, WindowEvent};
    use winit::keyboard::{Key, ModifiersState, NamedKey};

    let modifiers = Rc::new(Cell::new(ModifiersState::empty()));
    let weak = ui.as_weak();
    let ctx = ctx.clone();
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
            // winit delivers dropped files on X11 and Windows (not on Wayland,
            // and never dropped URLs): add .torrent/.metalink files. Our own
            // drag-out landing back on the window is not one.
            WindowEvent::DroppedFile(path) => {
                if !crate::dragout::is_own_drop(path) {
                    callbacks::add_dropped_file(&ctx, path);
                }
                EventResult::PreventDefault
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
                let free = !ui.get_shortcut_blocked();

                let handled = match &event.logical_key {
                    Key::Named(NamedKey::Escape) => {
                        let open = ui.get_overlay() != Overlay::None;
                        ui.set_overlay(Overlay::None);
                        open
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
                        ui.set_overlay(Overlay::Settings);
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
                    Key::Character(key)
                        if command && !shift && free && key.eq_ignore_ascii_case("a") =>
                    {
                        ui.invoke_select_visible(true);
                        true
                    }
                    Key::Character(key)
                        if command && !shift && free && key.eq_ignore_ascii_case("c") =>
                    {
                        callbacks::copy_selected_files(&ctx, &ui)
                    }
                    Key::Character(key) if plain && free && key.as_str() == "/" => {
                        ui.invoke_focus_search_input();
                        true
                    }
                    Key::Character(key) if plain && free && key.as_str() == "?" => {
                        ui.set_overlay(Overlay::Shortcuts);
                        true
                    }
                    Key::Character(key) if plain && free => {
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

#[derive(Clone, PartialEq, Debug)]
pub(crate) struct SettingsDraft {
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
    pub(crate) fn from_ui(ui: &MainWindow) -> Self {
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

    pub(crate) fn apply_to_ui(&self, ui: &MainWindow) {
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
