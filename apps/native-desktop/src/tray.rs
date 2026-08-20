use super::*;

#[derive(Clone, Copy)]
pub(super) enum TrayAction {
    Show,
    PauseAll,
    ResumeAll,
    Quit,
}

#[cfg(any(target_os = "linux", windows))]
#[derive(Clone)]
pub(super) struct TrayLabels {
    show: String,
    pause_all: String,
    resume_all: String,
    quit: String,
}

#[cfg(any(target_os = "linux", windows))]
impl TrayLabels {
    pub(super) fn from_controller(controller: &Controller) -> Self {
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
pub(super) struct LinuxTray {
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
pub(crate) fn canonical_linux_tray_icon() -> ksni::Icon {
    ksni::Icon {
        width: 32,
        height: 32,
        data: include_bytes!(concat!(env!("OUT_DIR"), "/app-icon.argb")).to_vec(),
    }
}

#[cfg(target_os = "linux")]
pub(super) async fn linux_tray(
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
pub(super) fn windows_tray(
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
            MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
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
